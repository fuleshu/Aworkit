#!/usr/bin/env bash
# Runs deterministic worker-runtime behavior without a desktop or live providers.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

cargo fmt --all --check
cargo test -p aworkit-workflow-worker
./qa/check-boundaries.sh
