#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if command -v flock >/dev/null 2>&1; then
  exec 9>"${TMPDIR:-/tmp}/aworkit-desktop-qa.lock"
  flock 9
fi
cd "$repository_root"
cargo test -p aworkit-trusted-core
cargo test --manifest-path desktop/src-tauri/Cargo.toml --lib
./qa/rescue-simple-chat-e2e.sh
cd "$repository_root/desktop"
./node_modules/.bin/vitest run src/chat src/app.integration.test.tsx
./node_modules/.bin/tsc --noEmit
./node_modules/.bin/vite build
cd "$repository_root"
./qa/desktop-browser-visual.sh
./qa/desktop-native-smoke.sh
./qa/desktop-native-simple-chat-e2e.sh
