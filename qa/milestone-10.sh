#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repository_root"

cargo fmt --check \
  -p aworkit-local-store \
  -p aworkit-trusted-core
cargo fmt --check --manifest-path desktop/src-tauri/Cargo.toml
bash qa/check-boundaries.sh
cargo test \
  -p aworkit-local-store \
  -p aworkit-trusted-core \
  --all-targets
cargo test --manifest-path desktop/src-tauri/Cargo.toml --all-targets

# Rust 1.97's expanded pedantic Clippy set reports pre-existing warnings in
# earlier milestones. Keep Milestone 10 gated on every compiler warning while
# that repository-wide lint debt remains independently visible.
RUSTFLAGS="-D warnings" cargo check \
  -p aworkit-local-store \
  -p aworkit-trusted-core \
  --all-targets
RUSTFLAGS="-D warnings" cargo check \
  --manifest-path desktop/src-tauri/Cargo.toml \
  --all-targets

cd "$repository_root/desktop"
./node_modules/.bin/vitest run
./node_modules/.bin/tsc --noEmit
./node_modules/.bin/vite build
