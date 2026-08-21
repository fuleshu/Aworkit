#!/usr/bin/env bash
set -euo pipefail

# Validate the desktop Chat vertical slice alongside its local trusted-core contracts.
cargo test -p aworkit-trusted-core
cd desktop
./node_modules/.bin/vitest run src/chat/chat.test.ts
./node_modules/.bin/tsc --noEmit
./node_modules/.bin/vite build
