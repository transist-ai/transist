#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

fail=0

while IFS= read -r file; do
  [[ -z "$file" ]] && continue

  expected='// SPDX-License-Identifier: MPL-2.0'

  first_line="$(sed -n '1p' "$file")"
  if [[ "$first_line" != "$expected" ]]; then
    printf 'SPDX header mismatch in %s\n  expected: %s\n  actual:   %s\n' "$file" "$expected" "$first_line" >&2
    fail=1
  fi
done < <(rg --files src tests crates -g '*.rs' -g 'build.rs')

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "SPDX header check passed."
