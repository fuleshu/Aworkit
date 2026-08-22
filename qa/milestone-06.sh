#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo fmt --check \
  -p aworkit-protocol \
  -p aworkit-portable-store \
  -p aworkit-local-store \
  -p aworkit-trusted-core
bash qa/check-boundaries.sh
cargo test \
  -p aworkit-protocol \
  -p aworkit-portable-store \
  -p aworkit-local-store \
  -p aworkit-trusted-core \
  --all-targets
if cargo clippy --version >/dev/null 2>&1; then
  cargo clippy \
    -p aworkit-protocol \
    -p aworkit-portable-store \
    -p aworkit-local-store \
    -p aworkit-trusted-core \
    --all-targets \
    -- -D warnings
else
  RUSTFLAGS="-D warnings" cargo check \
    -p aworkit-protocol \
    -p aworkit-portable-store \
    -p aworkit-local-store \
    -p aworkit-trusted-core \
    --all-targets
fi
