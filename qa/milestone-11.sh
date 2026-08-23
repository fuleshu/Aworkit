#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repository_root"

cargo fmt --check \
  -p aworkit-bootstrap-helper \
  -p aworkit-trusted-core
bash qa/check-boundaries.sh

# Exercise the complete hermetic helper matrix: enrollment, journal recovery,
# protocol fences, immutable slots, selector drift, process watchdog faults,
# verified activation, deterministic rollback, and manual recovery.
RUSTFLAGS="-D warnings" cargo test \
  -p aworkit-bootstrap-helper \
  --all-targets \
  --no-fail-fast

# Receipt import and commit-before-resuming-the-same-Management-Chat remain on
# the trusted-core side of the bootstrap boundary.
RUSTFLAGS="-D warnings" cargo test \
  -p aworkit-trusted-core \
  --test milestone_10_repair \
  --no-fail-fast

RUSTFLAGS="-D warnings" cargo check \
  -p aworkit-bootstrap-helper \
  -p aworkit-trusted-core \
  --all-targets
