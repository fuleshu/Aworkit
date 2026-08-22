#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repository_root"

cargo fmt --check \
  -p aworkit-protocol \
  -p aworkit-local-store \
  -p aworkit-trusted-core \
  -p aworkit-capability-host \
  -p aworkit-workflow-worker
bash qa/check-boundaries.sh
cargo test \
  -p aworkit-protocol \
  -p aworkit-local-store \
  -p aworkit-trusted-core \
  -p aworkit-capability-host \
  -p aworkit-workflow-worker \
  --all-targets

if cargo clippy --version >/dev/null 2>&1; then
  cargo clippy \
    -p aworkit-protocol \
    -p aworkit-local-store \
    -p aworkit-trusted-core \
    -p aworkit-capability-host \
    -p aworkit-workflow-worker \
    --all-targets \
    -- -D warnings
else
  RUSTFLAGS="-D warnings" cargo check \
    -p aworkit-protocol \
    -p aworkit-local-store \
    -p aworkit-trusted-core \
    -p aworkit-capability-host \
    -p aworkit-workflow-worker \
    --all-targets
fi

cd "$repository_root/desktop"
./node_modules/.bin/vitest run src/workbench/milestone09.test.ts
./node_modules/.bin/tsc --noEmit
