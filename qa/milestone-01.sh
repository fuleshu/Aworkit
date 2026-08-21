#!/usr/bin/env bash
# Runs all Milestone 01 checks without requiring product features or a display.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

cargo fmt --all --check
cargo test --workspace
./qa/check-boundaries.sh
./qa/smoke-processes.sh
(cd desktop && npx --yes pnpm@10.16.1 generate:protocol && npx --yes pnpm@10.16.1 test && npx --yes pnpm@10.16.1 build && npx --yes pnpm@10.16.1 exec tauri build --debug --no-bundle)
