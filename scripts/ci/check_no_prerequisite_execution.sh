#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

scope="crates/transist-prerequisites"
patterns=(
  'std::process::'
  'Command::new'
  'wait-timeout'
  'duct::'
  'subprocess::'
  'libc::(exec|spawn|posix_spawn)'
)

for pattern in "${patterns[@]}"; do
  if rg -n --hidden -g '!target/**' -e "$pattern" "$scope"; then
    echo "Prerequisite execution guard failed: forbidden process API '$pattern' remains in $scope." >&2
    exit 1
  fi
done

echo "No executable prerequisite process APIs found."
