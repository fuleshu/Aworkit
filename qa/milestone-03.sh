#!/usr/bin/env bash
# Complete deterministic workflow-worker contract, runtime, and process QA.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

cargo fmt --check -p aworkit-protocol -p aworkit-workflow-worker
if cargo clippy -V >/dev/null 2>&1; then
  cargo clippy -p aworkit-workflow-worker --all-targets -- -D warnings
else
  echo "clippy component unavailable; enforcing rustc warnings as errors" >&2
  cargo rustc -p aworkit-workflow-worker --lib -- -D warnings
  cargo rustc -p aworkit-workflow-worker --bin aworkit-workflow-worker -- -D warnings
  cargo rustc -p aworkit-workflow-worker --test milestone_03 -- -D warnings
  cargo rustc -p aworkit-workflow-worker --test milestone_03_runtime -- -D warnings
fi
cargo test -p aworkit-workflow-worker

# Normal dependencies may include only the shared protocol/process foundations,
# never another implementation process or a provider/UI framework.
if cargo tree -p aworkit-workflow-worker --edges normal --prefix none \
    | rg -q '^aworkit-(trusted-core|capability-host|local-store|portable-store|bootstrap-helper) '; then
  echo "workflow worker imports another implementation process" >&2
  exit 1
fi

./qa/check-boundaries.sh
