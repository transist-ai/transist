#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Check the commits introduced by a forward push to main.
set -euo pipefail

if [[ $# -ne 2 || ! "$1" =~ ^[0-9a-fA-F]{40}$ || ! "$2" =~ ^[0-9a-fA-F]{40}$ || "$1" =~ ^0{40}$ || "$2" =~ ^0{40}$ ]]; then
  echo "Usage: $0 BEFORE_SHA AFTER_SHA (nonzero full SHA-1 commit IDs)" >&2
  exit 2
fi
before="$1"
after="$2"
for sha in "$before" "$after"; do
  type="$(git cat-file -t "$sha")"
  if [[ "$type" != commit ]]; then
    echo "FAIL: $sha is not a commit." >&2
    exit 1
  fi
done
if [[ "$(git rev-parse --is-shallow-repository)" != false ]]; then
  echo "FAIL: DCO requires complete local history." >&2
  exit 1
fi
before="$(git rev-parse --verify "$before^{commit}")"
after="$(git rev-parse --verify "$after^{commit}")"
if [[ "$before" == "$after" ]] || ! git merge-base --is-ancestor "$before" "$after"; then
  echo "FAIL: before must be a strict ancestor of after." >&2
  exit 1
fi

commits="$(git rev-list "$before..$after")"
failed=0
while IFS= read -r sha; do
  [[ -n "$sha" ]] || continue
  message="$(git show -s --format=%B "$sha")"
  trailers="$(git interpret-trailers --parse --no-divider <<<"$message")"
  if ! grep -qE '^Signed-off-by:[[:space:]]*[^<>]*[^<>[:space:]][^<>]* <[^@<>[:space:]]+@[^@<>[:space:]]+>[[:space:]]*$' <<<"$trailers"; then
    echo "FAIL: commit $sha is missing a valid Signed-off-by trailer." >&2
    failed=1
  fi
done <<<"$commits"
if [[ "$failed" -ne 0 ]]; then
  exit 1
fi
echo "DCO check passed."
