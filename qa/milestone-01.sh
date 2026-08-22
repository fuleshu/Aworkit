#!/usr/bin/env bash
# Complete Milestone 01 workspace, boundary, protocol, and native-shell gate.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

test -x desktop/node_modules/.bin/vitest
test -x desktop/node_modules/.bin/tsc
test -x desktop/node_modules/.bin/vite
test -x desktop/node_modules/.bin/tauri

cargo fmt --all --check
cargo test --workspace --all-targets
node protocol/generate-runtime-schema.mjs --check
./qa/check-boundaries.sh
./qa/smoke-processes.sh

(
  cd desktop
  ./node_modules/.bin/vitest run src/protocol/schema.test.ts
  ./node_modules/.bin/tsc --noEmit
  ./node_modules/.bin/vite build
)
cargo test --manifest-path desktop/src-tauri/Cargo.toml --lib
./qa/desktop-native-smoke.sh
