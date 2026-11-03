#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Check every commit in BASE..HEAD (default HEAD) for the hosted DCO sign-off.
# No fetch, installation, identity exception, or repository mutation is performed.
set -euo pipefail
if [[ $# -lt 1 || $# -gt 2 || -z "$1" ]]; then
  echo "Usage: $0 BASE [HEAD]" >&2
  exit 2
fi
base="$(git rev-parse --verify --end-of-options "$1^{commit}")"
head="$(git rev-parse --verify --end-of-options "${2:-HEAD}^{commit}")"
if [[ "$(git rev-parse --is-shallow-repository)" != false ]]; then
  echo "FAIL: DCO requires complete local history; fetch it explicitly first." >&2
  exit 1
fi
git merge-base "$base" "$head" >/dev/null || {
  echo "FAIL: BASE and HEAD have no available common ancestor." >&2
  exit 1
}
# Capture first: a failed traversal must not become an empty passing loop.
commits="$(git rev-list "$base..$head")"
failed=0
while IFS= read -r sha; do
  [[ -n "$sha" ]] || continue
  body="$(git log -1 --format=%B "$sha")"
  if ! grep -qE '^Signed-off-by: .+ <.+@.+>' <<<"$body"; then
    echo "FAIL: commit $sha is missing Signed-off-by" >&2
    git log -1 --format='  %h %s' "$sha" >&2
    failed=1
  fi
done <<<"$commits"
if [[ "$failed" -ne 0 ]]; then
  echo "All commits must include 'Signed-off-by: Name <email>' (git commit -s)." >&2
  exit 1
fi
echo "DCO check passed."
