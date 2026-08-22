#!/usr/bin/env bash
# Complete trusted-core lifecycle, authority, supervision, and recovery QA.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

cargo fmt --check -p aworkit-trusted-core
if cargo clippy -V >/dev/null 2>&1; then
  cargo clippy -p aworkit-trusted-core --lib --bin aworkit-trusted-core --test milestone_04 -- -D warnings
else
  echo "clippy component unavailable; enforcing rustc warnings as errors" >&2
  cargo rustc -p aworkit-trusted-core --lib -- -D warnings
  cargo rustc -p aworkit-trusted-core --bin aworkit-trusted-core -- -D warnings
  cargo rustc -p aworkit-trusted-core --test milestone_04 -- -D warnings
fi
cargo build -p aworkit-workflow-worker
AWORKIT_WORKER_BIN="$root_dir/target/debug/aworkit-workflow-worker" \
  cargo test -p aworkit-trusted-core --lib --test milestone_04

# The trusted core must consume process-neutral ports, never concrete runtime or
# persistence implementations in its normal dependency graph.
if cargo tree -p aworkit-trusted-core --edges normal --prefix none \
    | rg -q '^aworkit-(workflow-worker|capability-host|local-store|portable-store|bootstrap-helper) '; then
  echo "trusted core imports a concrete implementation process" >&2
  exit 1
fi

./qa/check-boundaries.sh
