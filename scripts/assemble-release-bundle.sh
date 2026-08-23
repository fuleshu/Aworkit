#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
output=${1:-"$workspace_root/release/aworkit-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"}
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$workspace_root" log -1 --format=%ct)}

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) executable_suffix=.exe ;;
  *) executable_suffix= ;;
esac

export CARGO_INCREMENTAL=0
export SOURCE_DATE_EPOCH="$source_date_epoch"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$@"
  else
    echo "a SHA-256 utility (sha256sum or shasum) is required" >&2
    return 127
  fi
}

cargo build --manifest-path "$workspace_root/Cargo.toml" --release --locked \
  -p aworkit-trusted-core \
  -p aworkit-workflow-worker \
  -p aworkit-capability-host \
  -p aworkit-bootstrap-helper
npm --prefix "$workspace_root/desktop" run build
cargo build --manifest-path "$workspace_root/desktop/src-tauri/Cargo.toml" --release --locked

source_tree_hash=$(
  cd "$workspace_root"
  git ls-files -co --exclude-standard -z \
    | while IFS= read -r -d '' path; do
        case "$path" in
          .adashi/*|desktop/dist/*|desktop/src-tauri/gen/*) continue ;;
        esac
        if [[ -f "$path" ]]; then
          digest=$(sha256 "$path" | cut -d ' ' -f 1)
          printf '%s  %s\n' "$digest" "$path"
        fi
      done \
    | LC_ALL=C sort \
    | sha256 \
    | cut -d ' ' -f 1
)
workspace_identity_hash=$(
  cd "$workspace_root"
  sha256 Cargo.toml Cargo.lock desktop/src-tauri/Cargo.toml desktop/src-tauri/Cargo.lock desktop/pnpm-lock.yaml \
    | sha256 \
    | cut -d ' ' -f 1
)
toolchain_hash=$(
  {
    rustc -Vv
    cargo -V
    node --version
    npm --version
  } | sha256 | cut -d ' ' -f 1
)
source_revision=$(git -C "$workspace_root" rev-parse HEAD)

"$workspace_root/target/release/aworkit-release-assembler$executable_suffix" \
  --output "$output" \
  --desktop "$workspace_root/desktop/src-tauri/target/release/aworkit-desktop$executable_suffix" \
  --trusted-core "$workspace_root/target/release/aworkit-trusted-core$executable_suffix" \
  --workflow-worker "$workspace_root/target/release/aworkit-workflow-worker$executable_suffix" \
  --capability-host "$workspace_root/target/release/aworkit-capability-host$executable_suffix" \
  --bootstrap-helper "$workspace_root/target/release/aworkit-bootstrap-helper$executable_suffix" \
  --ui-dist "$workspace_root/desktop/dist" \
  --source-revision "$source_revision" \
  --source-tree-hash "$source_tree_hash" \
  --workspace-identity-hash "$workspace_identity_hash" \
  --toolchain-hash "$toolchain_hash" \
  --source-date-epoch "$source_date_epoch"

printf 'Aworkit whole-application bundle: %s\n' "$output"
