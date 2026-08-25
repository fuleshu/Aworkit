#!/usr/bin/env bash
# Cross-platform desktop startup probe: builds the debug desktop binary and
# verifies it boots the full Tauri setup hook (native runtime, stored-profile
# reconciliation, seeding) and keeps its window open. A startup panic exits
# immediately and fails this gate.
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
desktop_root="$repository_root/desktop"
binary_path="$desktop_root/src-tauri/target/debug/aworkit-desktop"

if [[ "${AWORKIT_SKIP_NATIVE_BUILD:-0}" != "1" ]]; then
  cargo build --manifest-path "$desktop_root/src-tauri/Cargo.toml" \
    --bin aworkit-desktop
fi
test -x "$binary_path" || {
  echo "desktop binary is not built: $binary_path" >&2
  exit 1
}

temporary_directory=$(mktemp -d)
application_pid=""
cleanup() {
  if [[ -n "$application_pid" ]] && kill -0 "$application_pid" 2>/dev/null; then
    kill "$application_pid" 2>/dev/null || true
    wait "$application_pid" 2>/dev/null || true
  fi
  rm -rf -- "$temporary_directory"
}
trap cleanup EXIT

(
  cd "$desktop_root"
  exec "$binary_path"
) >"$temporary_directory/stdout.log" 2>"$temporary_directory/stderr.log" &
application_pid=$!

for _ in $(seq 1 30); do
  if ! kill -0 "$application_pid" 2>/dev/null; then
    set +e
    wait "$application_pid"
    status=$?
    set -e
    echo "desktop app exited during startup (status $status)" >&2
    sed -n '1,40p' "$temporary_directory/stderr.log" >&2 || true
    exit 1
  fi
  sleep 1
done

echo "desktop app stayed alive for 30s after startup"
