#!/usr/bin/env node
// Per-turn prompt-cache and usage report for one Aworkit Run.
//
// The cache-miss investigation in task 128 needed this table four times by hand,
// and every hand-built query risked a different pairing of a request with its
// usage. This reads the canonical store directly: usage events are paired to the
// request surface by spanId, and each turn is checked against two invariants.
//
//   node qa/cache-usage-report.mjs [--chat <chat-id>] [--store <documents dir>]
//                                  [--dsh <session cache json>]
//
// The Aworkit numbers come from semantic events, so they are only as good as the
// provider's own usage object. sentBytes and totalTokens are recorded per call by
// the harness; a missing sentBytes means the run predates that instrumentation.
//
// Invariants per turn, both hard:
//   tokens never exceed bytes       inputTokens <= sentBytes
//   a token covers at least a byte  cacheMissInputTokens <= sentBytes
// The stronger reading compares the hit rate with prefixShare, the share of the
// request that repeats the previous one: a gap means a prefix loss on the
// provider side, which is what the recorded benchmark shows on eight turns.
// A violation means the provider's counters, not the request, need explaining.
// Falling inside the bounds does not prove the split: a cache boundary can sit
// anywhere in the request, so the table is read together with sentBytes.

import { DatabaseSync } from "node:sqlite";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/** Default project-local store written by the desktop application. */
const DEFAULT_STORE = join(
  homedir(),
  ".local/share/com.aworkit.desktop/runtime/history/aworkit.sqlite3",
);

function parseArguments(argv) {
  const options = { store: DEFAULT_STORE, chat: null, dsh: null };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (flag === "--chat") options.chat = value;
    else if (flag === "--store") options.store = value;
    else if (flag === "--dsh") options.dsh = value;
    else if (flag.startsWith("--")) throw new Error(`unknown option ${flag}`);
  }
  return options;
}

/** Newest chat that has model usage, so a benchmark run needs no identifier. */
function newestMeasuredChat(database) {
  const row = database
    .prepare(
      `select chat_id, count(*) as turns from semantic_events
       where kind = 'span.usage' group by chat_id order by turns desc limit 1`,
    )
    .get();
  if (row === undefined) throw new Error("no chat recorded model usage");
  return row.chat_id;
}

/** Every model turn of one chat, oldest first. */
/** Share of the request that repeats the previous one, as a percentage. */
function prefixShare(sent, consumed) {
  return sent === null || consumed === null || sent === 0
    ? null
    : (consumed / sent) * 100;
}

function turns(database, chatId) {
  const rows = database
    .prepare(
      `select sequence, payload from semantic_events
       where chat_id = ? and kind in ('span.started', 'span.usage') order by sequence`,
    )
    .all(chatId);
  const requests = new Map();
  const measured = [];
  for (const row of rows) {
    const payload = JSON.parse(row.payload);
    if (payload.input !== undefined) {
      requests.set(payload.spanId, payload.input);
      continue;
    }
    if (payload.cache === undefined && payload.inputTokens === undefined)
      continue;
    const cache = payload.cache ?? {};
    measured.push({
      sequence: row.sequence,
      spanId: payload.spanId,
      inputTokens: payload.inputTokens ?? 0,
      outputTokens: payload.outputTokens ?? 0,
      cached: cache.cachedInputTokens ?? null,
      miss: cache.cacheMissInputTokens ?? null,
      total: cache.totalTokens ?? null,
      sentBytes: cache.sentBytes ?? null,
      prefixBytes: cache.commonPrefixBytes ?? null,
      request: requests.get(payload.spanId) ?? null,
    });
  }
  return measured;
}

/** Characters of the request surface, the only size measure older runs have. */
function requestChars(request) {
  return request === null ? null : JSON.stringify(request).length;
}

function pad(value, width) {
  return String(value).padStart(width);
}

/** One line per turn plus the flags that make a turn worth investigating. */
function reportAworkit(chatId, rows) {
  console.log(`chat ${chatId}: ${rows.length} model turns`);
  console.log(
    "  seq  inputTokens  cached    miss  cacheHit  prefixShare  sentBytes  typedChars  flags",
  );
  let miss = 0;
  let cached = 0;
  let input = 0;
  let flagged = 0;
  let unmeasured = 0;
  for (const turn of rows) {
    miss += turn.miss ?? 0;
    cached += turn.cached ?? 0;
    input += turn.inputTokens;
    const chars = requestChars(turn.request);
    const share = prefixShare(turn.sentBytes, turn.prefixBytes);
    const hitRate =
      turn.cached === null || turn.inputTokens === 0
        ? null
        : (turn.cached / turn.inputTokens) * 100;
    const flags = [];
    if (turn.sentBytes !== null && turn.inputTokens > turn.sentBytes) {
      flags.push("tokens>bytes");
    }
    if (turn.sentBytes !== null && (turn.miss ?? 0) > turn.sentBytes) {
      flags.push("miss>bytes");
    }
    // A hit far below the repeated prefix means the provider dropped a prefix we
    // kept identical. The ten-point margin is a reading aid, not a bound.
    if (share !== null && hitRate !== null && share - hitRate > 10) {
      flags.push("hit<prefix");
    }
    if (turn.sentBytes === null) unmeasured += 1;
    if (flags.length > 0) flagged += 1;
    console.log(
      `  ${pad(turn.sequence ?? "", 5)} ${pad(turn.inputTokens, 11)} ` +
        `${pad(turn.cached ?? "-", 7)} ${pad(turn.miss ?? "-", 7)} ` +
        `${pad(hitRate === null ? "-" : `${hitRate.toFixed(1)}%`, 8)} ` +
        `${pad(share === null ? "-" : `${share.toFixed(1)}%`, 11)} ` +
        `${pad(turn.sentBytes ?? "-", 9)} ${pad(chars ?? "-", 10)}  ${flags.join(",")}`,
    );
  }
  const share = input === 0 ? 0 : (miss / input) * 100;
  console.log(
    `  total input ${input.toLocaleString()}, cached ${cached.toLocaleString()}, ` +
      `uncached ${miss.toLocaleString()} (${share.toFixed(1)}% of input)`,
  );
  console.log(`  turns violating a byte bound: ${flagged} of ${rows.length}`);
  if (unmeasured > 0) {
    console.log(
      `  turns without a recorded sentBytes: ${unmeasured} (run predates the instrumentation)`,
    );
  }
  return { input, cached, miss, flagged };
}

/** The DSH side of the same benchmark, when its session cache is available. */
function reportDsh(path) {
  const record = JSON.parse(readFileSync(path, "utf8")).record;
  const totals = record.rows.tokenUsage.val.totals;
  const input = totals.uncachedInputTokens + totals.cacheReadTokens;
  const miss = totals.uncachedInputTokens;
  console.log(`DSH ${path}`);
  console.log(
    `  total input ${input.toLocaleString()}, uncached ${miss.toLocaleString()} ` +
      `(${((miss / input) * 100).toFixed(1)}% of input), output ${totals.outputTokens.toLocaleString()}`,
  );
  const pressure = record.rows.contextPressure?.val;
  if (pressure !== undefined) {
    console.log(
      `  peak context ${pressure.pressureTokens.toLocaleString()} of ${pressure.contextWindow.toLocaleString()}`,
    );
  }
  return { input, miss };
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const database = new DatabaseSync(options.store, { readOnly: true });
  try {
    const chatId = options.chat ?? newestMeasuredChat(database);
    const rows = turns(database, chatId);
    const aworkit = reportAworkit(chatId, rows);
    if (options.dsh !== null) {
      const dsh = reportDsh(options.dsh);
      console.log(
        `  ratio uncached Aworkit/DSH: ${(aworkit.miss / dsh.miss).toFixed(2)}x`,
      );
    }
    // A flagged turn is the point of the report, so exit nonzero for scripting.
    process.exitCode = aworkit.flagged === 0 ? 0 : 1;
  } finally {
    database.close();
  }
}

main();
