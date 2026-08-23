#!/usr/bin/env bash
# Milestone 12 release-candidate aggregate: retain every stable M01-M11 gate,
# then add only the native cross-system and packaging boundaries introduced here.
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$repository_root"

for milestone in 01 02 03 04 05 06 07 08 09 10 11; do
  printf '== Aworkit release matrix: milestone %s ==\n' "$milestone"
  bash "qa/milestone-$milestone.sh"
done

printf '== Aworkit release matrix: milestone 12 native conformance ==\n'
bash qa/milestone-12-native.sh
