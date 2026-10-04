/** Project optional provider evidence without converting missing usage to zero.
 * Only canonical per-request usage events count; embedded history never does.
 *
 * A provider that reports cached input without an uncached counter does not make
 * the split unknown: both APIs count cached input inside the prompt, so the
 * remainder is exactly `input − cached`. That total is published as derived
 * arithmetic and labelled, never as a reported counter.
 *
 * `scoped` marks figures computed from a loaded window of events rather than the
 * whole Run. The client pages history, so a Run whose events are not all loaded
 * must never present a per-span sum as a Run total. */
import type { RuntimeEvent } from "./corePort";
import type { RunDetailField } from "./runDetails";

type RecordValue = Record<string, unknown>;
function record(value: unknown): RecordValue {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as RecordValue : {};
}
function count(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined;
}

export function cacheUsageFields(
  events: readonly RuntimeEvent[],
  reviews = false,
  scoped = false,
): RunDetailField[] {
  const requests = new Map<string, RecordValue>();
  for (const event of events) {
    const p = record(event.payload);
    if (reviews ? event.kind !== "approval.reviewed"
      : event.kind !== "span.usage" && event.kind !== "context.compaction-ended") continue;
    const key = reviews ? event.eventId
      : String(p.spanId ?? event.eventId);
    requests.set(key, event.kind === "context.compaction-ended" ? record(p.auxiliary) : p);
  }
  if (requests.size === 0) return [];
  let hits = 0, misses = 0, hitReports = 0, missReports = 0, pairedHits = 0, pairedInput = 0;
  const derivedMisses: number[] = [];
  for (const p of requests.values()) {
    const cache = record(p.cache);
    const hit = count(cache.cachedInputTokens), miss = count(cache.cacheMissInputTokens);
    if (hit !== undefined) { hits += hit; hitReports++; }
    if (miss !== undefined) { misses += miss; missReports++; }
    const input = count(p.inputTokens);
    // The denominator belongs to exactly the calls that reported hits.
    if (hit !== undefined && input !== undefined && hit <= input) {
      pairedHits += hit; pairedInput += input;
      // Both APIs count cached input inside the prompt, so a call that reported
      // only a hit counter still fixes the remainder exactly. Sum it separately
      // and label it derived: it is arithmetic, never a provider counter.
      if (miss === undefined) derivedMisses.push(input - hit);
    }
  }
  const calls = (reports: number) => `${reports} ${reports === 1 ? "call" : "calls"}`;
  const value = (total: number, reports: number) => reports === 0 ? "Not reported"
    : scoped ? `${total.toLocaleString()} (${calls(reports)} loaded)`
    : `${total.toLocaleString()}${reports < requests.size ? ` (${reports}/${requests.size} calls reported)` : ""}`;
  const suffix = scoped ? " in loaded activity" : "";
  const derived = derivedMisses.reduce((sum, value) => sum + value, 0);
  const coverage = derivedMisses.length < requests.size
    ? ` (${derivedMisses.length}/${requests.size} calls)` : "";
  // A provider that reports cached input but no uncached counter (MiMo, Qwen)
  // still fixes the remainder exactly, because both count cached input inside
  // the prompt. Summing "input − cached" is published as derived arithmetic, so
  // it is never mistaken for a provider counter.
  const derivedValue = derivedMisses.length === 0 ? "Not reported"
    : scoped ? `${derived.toLocaleString()} (${calls(derivedMisses.length)} loaded, derived)`
    : `${derived.toLocaleString()}${coverage} (input − cached; no provider counter)`;
  const rateSuffix = scoped ? " of loaded activity"
    : hitReports < requests.size ? " of reported calls" : "";
  return [
    { label: `Cached input tokens${suffix}`, value: value(hits, hitReports) },
    { label: `Uncached input tokens${suffix}`, value: missReports > 0 ? value(misses, missReports)
      : derivedMisses.length > 0 ? derivedValue : "Not reported" },
    { label: "Cache hit rate", value: pairedInput === 0 ? "Not available"
      : `${(100 * pairedHits / pairedInput).toFixed(1)}%${rateSuffix}` },
  ];
}

/** Complete cache units a Run summary event reported, when it carries them. */
export function reportedCacheUnits(
  events: readonly RuntimeEvent[],
): { readonly cached: number; readonly uncached: number } | undefined {
  let cached = 0, uncached = 0, reports = 0;
  for (const event of events) {
    if (event.kind !== "message.assistant" && event.kind !== "context.manual-completed"
      && event.kind !== "context.manual-failed" && event.kind !== "execution.failed") continue;
    const payload = record(event.payload);
    const hits = count(payload.cachedInputUnits), misses = count(payload.uncachedInputUnits);
    if (hits === undefined || misses === undefined) continue;
    cached += hits;
    uncached += misses;
    reports++;
  }
  return reports === 0 ? undefined : { cached, uncached };
}

/** Whole-Run cache fields from one complete aggregate, never from a window. */
export function reportedCacheFields(
  units: { readonly cached: number; readonly uncached: number },
): RunDetailField[] {
  const total = units.cached + units.uncached;
  return [
    { label: "Cached input tokens", value: units.cached.toLocaleString() },
    { label: "Uncached input tokens", value: units.uncached.toLocaleString() },
    { label: "Cache hit rate", value: total === 0 ? "Not available"
      : `${(100 * units.cached / total).toFixed(1)}%` },
  ];
}
