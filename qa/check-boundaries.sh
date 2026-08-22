#!/usr/bin/env bash
# Verifies dependency-kind-aware process isolation and protocol purity.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

node qa/check-boundaries.mjs --self-test
cargo metadata --format-version 1 --all-features | node qa/check-boundaries.mjs --metadata-stdin

if rg -n --glob '*.ts' --glob '*.tsx' \
    "from [\"'](react|react-dom|@tauri-apps|@mantine|@xyflow)" desktop/src/protocol; then
  echo "protocol runtime parser imports a UI or desktop framework" >&2
  exit 1
fi
