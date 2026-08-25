#!/usr/bin/env bash
# Standard Agent functional QA gate (W8): the complete user story end to end.
#
# (1) Rust formatting and warnings-denied tests across the library crates and
#     the desktop runtime, whose suite contains the functional proof of the
#     standard agent workflow: graph pass plan->agent->output->wait, per-node
#     lifecycle evidence, approval suspend/resume (accept + reject), the full
#     built-in tool matrix, subagent delegation, and MCP tool execution.
# (2) Frontend typecheck, Vitest, and the production Vite build.
# (3) The stable M01-M12 release matrix.
# (4) Native smoke: the real Tauri application boots, renders the workbench,
#     and executes the packaged Simple Chat and project read-tool workflows.
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$repository_root"

printf '== Standard Agent gate: formatting ==\n'
cargo fmt --check
cargo fmt --check --manifest-path desktop/src-tauri/Cargo.toml

printf '== Standard Agent gate: warnings-denied library tests ==\n'
RUSTFLAGS="-D warnings" cargo test --workspace --all-targets --no-fail-fast

printf '== Standard Agent gate: warnings-denied desktop tests ==\n'
RUSTFLAGS="-D warnings" cargo test \
  --manifest-path desktop/src-tauri/Cargo.toml \
  --all-targets \
  --no-fail-fast

printf '== Standard Agent gate: frontend typecheck, tests, and build ==\n'
(
  cd desktop
  ./node_modules/.bin/tsc --noEmit
  ./node_modules/.bin/vitest run
  ./node_modules/.bin/vite build
)

printf '== Standard Agent gate: stable release matrix ==\n'
bash qa/milestone-12.sh

printf '== Standard Agent gate: boundary and process smoke ==\n'
bash qa/check-boundaries.sh
bash qa/smoke-processes.sh

printf '== Standard Agent gate: desktop startup probe ==\n'
bash qa/desktop-native-launch-probe.sh

printf '== Standard Agent gate: native application smoke ==\n'
bash qa/desktop-native-smoke.sh

printf '== Standard Agent gate: packaged workflow end-to-end runs ==\n'
if [[ -n "${AWORKIT_SKIP_NATIVE_BUILD:-}" ]]; then
  export AWORKIT_SKIP_NATIVE_BUILD=1
fi
bash qa/desktop-native-project-read-tool-e2e.sh
bash qa/desktop-native-simple-chat-e2e.sh

printf '== Standard Agent gate: PASS ==\n'
