#!/usr/bin/env bash
# Runs the canonical local-persistence checks without needing a display server.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

cargo fmt --all --check
cargo test -p aworkit-local-store
./qa/check-boundaries.sh
