#!/usr/bin/env bash
# Starts every standalone process and verifies exact bounded handshake behavior.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

temporary_dir="$(mktemp -d)"
trap 'rm -rf -- "$temporary_dir"' EXIT

run_with_timeout() {
  node qa/run-with-timeout.mjs 20000 "$@"
}

processes=(
  'aworkit-trusted-core:trusted-core'
  'aworkit-workflow-worker:workflow-worker'
  'aworkit-capability-host:capability-host'
  'aworkit-local-store:local-store'
  'aworkit-portable-store:portable-store'
  'aworkit-bootstrap-helper:bootstrap-helper'
)

for specification in "${processes[@]}"; do
  package="${specification%%:*}"
  process_name="${specification#*:}"
  output="$(run_with_timeout cargo run --quiet -p "$package" --bin "$package" -- --smoke --generation 1)"
  expected="aworkit-smoke process=$process_name generation=1 status=ready shutdown=bounded"
  if [[ "$output" != "$expected" ]]; then
    echo "unexpected smoke handshake for $package: $output" >&2
    exit 1
  fi

  if run_with_timeout cargo run --quiet -p "$package" --bin "$package" -- --smoke --unknown \
      >"$temporary_dir/unknown.out" 2>"$temporary_dir/unknown.err"; then
    echo "$package accepted an unknown argument" >&2
    exit 1
  fi
  if [[ -s "$temporary_dir/unknown.out" ]] \
      || ! rg -q "^$process_name: unknown argument: --unknown$" "$temporary_dir/unknown.err"; then
    echo "$package did not reject an unknown argument deterministically" >&2
    exit 1
  fi

  if run_with_timeout cargo run --quiet -p "$package" --bin "$package" -- --smoke \
      --generation 9007199254740992 \
      >"$temporary_dir/generation.out" 2>"$temporary_dir/generation.err"; then
    echo "$package accepted an inexact cross-language generation" >&2
    exit 1
  fi
  if [[ -s "$temporary_dir/generation.out" ]] \
      || ! rg -q "^$process_name: --generation must not exceed 9007199254740991$" \
          "$temporary_dir/generation.err"; then
    echo "$package did not enforce the exact generation bound" >&2
    exit 1
  fi
done
