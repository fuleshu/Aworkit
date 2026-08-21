#!/usr/bin/env bash
# Verifies that the milestone scaffold has no sideways Rust process dependencies.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

dependency_lines() {
  awk '/^\[dependencies\]/{inside=1; next} /^\[/{inside=0} inside && NF {print}' "$1"
}

for crate in trusted-core workflow-worker capability-host local-store portable-store bootstrap-helper; do
  manifest="crates/aworkit-$crate/Cargo.toml"
  if dependency_lines "$manifest" | rg -q 'aworkit-(trusted-core|workflow-worker|capability-host|local-store|portable-store|bootstrap-helper)'; then
    echo "sideways process dependency in $manifest" >&2
    exit 1
  fi
done

if dependency_lines crates/aworkit-trusted-core/Cargo.toml | rg -qv '^aworkit-process ='; then
  echo "trusted core must not depend on implementation processes" >&2
  exit 1
fi

test "$(dependency_lines crates/aworkit-process/Cargo.toml)" = 'aworkit-protocol = { path = "../aworkit-protocol" }'
forbidden='aworkit-(trusted-core|workflow-worker|capability-host|local-store|portable-store|bootstrap-helper)|tauri|rig'
if dependency_lines crates/aworkit-protocol/Cargo.toml | rg -q "$forbidden"; then
  echo "protocol layer imports a process, framework, or provider dependency" >&2
  exit 1
fi
if rg -n 'from "(react|@tauri-apps)' desktop/src/protocol; then
  echo "protocol runtime parser imports UI framework types" >&2
  exit 1
fi
