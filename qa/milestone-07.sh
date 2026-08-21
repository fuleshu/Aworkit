#!/usr/bin/env bash
set -euo pipefail
cd desktop
./node_modules/.bin/vitest run
./node_modules/.bin/tsc --noEmit
./node_modules/.bin/vite build
