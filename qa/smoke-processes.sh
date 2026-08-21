#!/usr/bin/env bash
# Starts every standalone process through its bounded smoke entry point.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

for package in aworkit-trusted-core aworkit-workflow-worker aworkit-capability-host aworkit-local-store aworkit-portable-store aworkit-bootstrap-helper; do
  cargo run --quiet -p "$package" -- --smoke --generation 1 | rg '^aworkit-smoke process=.* generation=1 status=ready shutdown=bounded$'
done
