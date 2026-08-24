#!/usr/bin/env bash
# Release-blocking rescue slice: use the native desktop runtime composition
# twice against one deterministic OpenAI-compatible endpoint and one data root.
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
desktop_manifest="$repository_root/desktop/src-tauri/Cargo.toml"
fixture_script="$repository_root/qa/fixtures/openai-compatible-fixture.mjs"
check_script="$repository_root/qa/check-rescue-simple-chat.mjs"

for command in node cargo; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "rescue Simple Chat E2E requires $command" >&2
    exit 1
  fi
done

temporary_directory=$(mktemp -d)
fixture_pid=""
report_rescue_failure() {
  local status=$?
  trap - ERR
  echo "rescue Simple Chat E2E failed (exit $status)" >&2
  for diagnostic in fixture.stderr fixture-requests.jsonl first-result.json reopen-result.json; do
    if [[ -s "$temporary_directory/$diagnostic" ]]; then
      echo "--- $diagnostic ---" >&2
      sed -n '1,200p' "$temporary_directory/$diagnostic" >&2
    fi
  done
  exit "$status"
}
cleanup_rescue_e2e() {
  if [[ -n "$fixture_pid" ]] && kill -0 "$fixture_pid" 2>/dev/null; then
    kill "$fixture_pid" 2>/dev/null || true
    wait "$fixture_pid" 2>/dev/null || true
  fi
  rm -r -- "$temporary_directory"
}
trap report_rescue_failure ERR
trap cleanup_rescue_e2e EXIT

ready_file="$temporary_directory/fixture-ready.json"
request_log="$temporary_directory/fixture-requests.jsonl"
node "$fixture_script" \
  --ready-file "$ready_file" \
  --request-log "$request_log" \
  >"$temporary_directory/fixture.stdout" \
  2>"$temporary_directory/fixture.stderr" &
fixture_pid=$!

for _ in {1..100}; do
  [[ -s "$ready_file" ]] && break
  if ! kill -0 "$fixture_pid" 2>/dev/null; then
    sed -n '1,160p' "$temporary_directory/fixture.stderr" >&2
    echo "OpenAI-compatible rescue fixture exited before becoming ready" >&2
    exit 1
  fi
  sleep 0.05
done
if [[ ! -s "$ready_file" ]]; then
  sed -n '1,160p' "$temporary_directory/fixture.stderr" >&2
  echo "OpenAI-compatible rescue fixture did not become ready" >&2
  exit 1
fi

base_url=$(node -e '
  const fs = require("node:fs");
  const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (value.schemaVersion !== 1 || typeof value.baseUrl !== "string") process.exit(2);
  process.stdout.write(value.baseUrl);
' "$ready_file")

if [[ -n "${AWORKIT_RESCUE_E2E_BIN:-}" ]]; then
  rescue_binary=$AWORKIT_RESCUE_E2E_BIN
else
  cargo build --locked --manifest-path "$desktop_manifest" --bin aworkit-rescue-e2e
  rescue_binary="$repository_root/desktop/src-tauri/target/debug/aworkit-rescue-e2e"
  if [[ ! -x "$rescue_binary" && -x "$rescue_binary.exe" ]]; then
    rescue_binary="$rescue_binary.exe"
  fi
fi
test -x "$rescue_binary"

data_root="$temporary_directory/fresh-profile"
test ! -e "$data_root"
first_result="$temporary_directory/first-result.json"
reopen_result="$temporary_directory/reopen-result.json"

AWORKIT_RESCUE_E2E_API_KEY=aworkit-rescue-key \
AWORKIT_RESCUE_E2E_MODEL=aworkit-rescue-model \
  "$rescue_binary" "$data_root" "$base_url" first >"$first_result"
test -d "$data_root"

# A separate process opening the same data root is the persistence proof. The
# runner restores the hermetic store behind the already-persisted opaque
# credential reference; it must not recommit Settings or replay the first
# provider effect while rebuilding Chat state.
AWORKIT_RESCUE_E2E_API_KEY=aworkit-rescue-key \
AWORKIT_RESCUE_E2E_MODEL=aworkit-rescue-model \
  "$rescue_binary" "$data_root" "$base_url" reopen >"$reopen_result"

node "$check_script" "$first_result" "$reopen_result" "$request_log"
