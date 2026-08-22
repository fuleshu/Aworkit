#!/usr/bin/env bash
# Complete Milestone 02 canonical-local-persistence and recovery gate.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

cargo fmt -p aworkit-local-store -p aworkit-protocol -p aworkit-process --check
cargo test -p aworkit-local-store --all-targets
RUSTFLAGS="-D warnings" cargo check -p aworkit-local-store --all-targets
./qa/check-boundaries.sh
