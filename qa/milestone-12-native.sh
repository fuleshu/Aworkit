#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$repository_root"

cargo fmt --check \
  -p aworkit-process \
  -p aworkit-bootstrap-helper \
  -p aworkit-trusted-core
cargo fmt --check --manifest-path desktop/src-tauri/Cargo.toml
bash qa/check-boundaries.sh
bash qa/smoke-processes.sh
node qa/check-milestone-12.mjs

# Native fault boundaries and supported/degraded capability reports. These
# compile from the same cfg-gated ports on all three release runners.
RUSTFLAGS="-D warnings" cargo test -p aworkit-process --all-targets --no-fail-fast
RUSTFLAGS="-D warnings" cargo test -p aworkit-bootstrap-helper --all-targets --no-fail-fast
RUSTFLAGS="-D warnings" cargo test \
  -p aworkit-trusted-core \
  --test milestone_12_native \
  --no-fail-fast
RUSTFLAGS="-D warnings" cargo test \
  --manifest-path desktop/src-tauri/Cargo.toml \
  --all-targets \
  --no-fail-fast

(
  cd desktop
  ./node_modules/.bin/tsc --noEmit
  ./node_modules/.bin/vitest run
  ./node_modules/.bin/vite build
)

# Cross-system packaging smoke: create a real bundle on the current runner and
# validate the complete role closure, compatibility metadata, and provenance.
release_fixture=$(mktemp -d "${TMPDIR:-/tmp}/aworkit-m12-qa.XXXXXX")
trap 'rm -rf -- "$release_fixture"' EXIT
SOURCE_DATE_EPOCH=1700000000 \
  scripts/assemble-release-bundle.sh "$release_fixture/bundle"
node qa/check-milestone-12.mjs \
  qa/fixtures/milestone-12-platform-matrix.json \
  "$release_fixture/bundle/WholeApplicationBundleV1.json"
