#!/usr/bin/env node

import { readFileSync } from "node:fs";

const [firstPath, reopenPath, requestLogPath] = process.argv.slice(2);
if (firstPath === undefined || reopenPath === undefined || requestLogPath === undefined) {
  throw new Error(
    "usage: check-rescue-simple-chat.mjs <first-result.json> <reopen-result.json> <fixture-requests.jsonl>",
  );
}

const first = readSingleJson(firstPath, "first CLI result");
const reopen = readSingleJson(reopenPath, "reopen CLI result");
const requests = readJsonLines(requestLogPath);

assert(first.schemaVersion === 1, "first result schemaVersion must be 1");
assert(first.phase === "first", "first result phase must be first");
assert(first.providerTested === true, "first phase must prove provider testing succeeded");
assert(
  Number.isSafeInteger(first.settingsVersion) && first.settingsVersion > 0,
  "first phase must report the committed Settings version",
);
assert(typeof first.chatId === "string" && first.chatId !== "", "first phase needs a Chat ID");
assert(
  first.assistantReply === "AWORKIT_FIXTURE_REPLY_1: hello",
  "first phase must expose the exact assistant reply",
);

assert(reopen.schemaVersion === 1, "reopen result schemaVersion must be 1");
assert(reopen.phase === "reopen", "reopen result phase must be reopen");
assert(reopen.providerTested === true, "reopen phase must retain a tested provider binding");
assert(reopen.chatId === first.chatId, "reopen must continue the original Chat ID");
assert(
  reopen.settingsVersion === first.settingsVersion,
  "reopen must reuse the saved Settings version without recommitting it",
);
assert(
  reopen.priorAssistantReply === first.assistantReply,
  "reopen must load the first committed assistant reply before sending again",
);
assert(
  reopen.assistantReply === "AWORKIT_FIXTURE_REPLY_2: again",
  "reopen must expose the exact second assistant reply",
);

const modelQueries = requests.filter(({ kind }) => kind === "models");
const completions = requests.filter(({ kind }) => kind === "chat.completion");
assert(modelQueries.length >= 1, "provider Test Connection must query the fixture");
assert(
  completions.length === 2,
  `expected exactly two provider completions without restart replay; received ${completions.length}`,
);
assert(completions[0].model === "aworkit-rescue-model", "first completion must use configured model");
assert(completions[1].model === "aworkit-rescue-model", "second completion must use configured model");
assert(lastUserText(completions[0].messages) === "hello", "first provider prompt must be hello");
assert(lastUserText(completions[1].messages) === "again", "second provider prompt must be again");
assert(
  completions[1].messages.some(
    ({ role, content }) =>
      role === "assistant" && content === "AWORKIT_FIXTURE_REPLY_1: hello",
  ),
  "continued provider request must include the persisted assistant reply",
);

process.stdout.write(
  `rescue Simple Chat E2E passed: chat=${first.chatId}, providerTests=${modelQueries.length}, completions=2\n`,
);

function readSingleJson(path, label) {
  const source = readFileSync(path, "utf8").trim();
  assert(source !== "", `${label} is empty`);
  assert(!source.includes("\n"), `${label} must contain exactly one JSON line`);
  try {
    return JSON.parse(source);
  } catch (error) {
    throw new Error(`${label} is not valid JSON: ${error.message}`);
  }
}

function readJsonLines(path) {
  const source = readFileSync(path, "utf8").trim();
  if (source === "") return [];
  return source.split("\n").map((line, index) => {
    try {
      return JSON.parse(line);
    } catch (error) {
      throw new Error(`fixture request log line ${index + 1} is invalid JSON: ${error.message}`);
    }
  });
}

function lastUserText(messages) {
  if (!Array.isArray(messages)) return undefined;
  return [...messages].reverse().find(({ role }) => role === "user")?.content;
}

function assert(condition, message) {
  if (!condition) throw new Error(`rescue Simple Chat E2E failed: ${message}`);
}
