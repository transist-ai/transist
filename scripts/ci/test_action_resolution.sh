#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# test_action_resolution.sh — Verify every pinned GitHub Action SHA in the
# release workflow resolves to an existing commit on GitHub.
#
# This catches regressions where the release workflow references a nonexistent
# action commit SHA (404), which would prevent the job from starting. The test
# uses the GitHub API (requires network access and gh or curl) and is a hosted
# CI fixture.
#
# Exit status: 0 (all actions resolve) or 1 (any action fails to resolve).

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

failures=0

# Collect every `uses: <action>@<sha>` reference from the release workflow.
declare -a action_refs=()
while IFS= read -r line; do
  ref="$(echo "$line" | grep -oE '[A-Za-z0-9._/-]+@[0-9a-f]{40}')"
  [[ -n "$ref" ]] && action_refs+=("$ref")
done < <(grep -hoE 'uses:[[:space:]]*[^[:space:]]+@[0-9a-f]{40}' .github/workflows/release.yml 2>/dev/null)

if [[ "${#action_refs[@]}" -eq 0 ]]; then
  echo "FAIL: no pinned action references found in workflow files" >&2
  exit 1
fi

echo "Checking ${#action_refs[@]} pinned action reference(s)..."

for ref in "${action_refs[@]}"; do
  action="${ref%@*}"
  sha="${ref##*@}"
  resolved=false
  # Use gh api if available (authenticated, higher rate limit).
  if command -v gh >/dev/null 2>&1 && [[ -n "${GITHUB_TOKEN:-${GH_TOKEN:-}}" ]]; then
    if gh api "repos/$action/commits/$sha" --jq '.sha' >/dev/null 2>&1; then
      resolved=true
    fi
  fi
  # Fallback: check the web URL (not subject to API rate limits).
  if ! $resolved; then
    http_code="$(curl -sI -o /dev/null -w '%{http_code}' \
      "https://github.com/$action/commit/$sha" 2>/dev/null || echo "000")"
    if [[ "$http_code" == "200" ]]; then
      resolved=true
    fi
  fi
  if $resolved; then
    echo "  OK  $action@$sha"
  else
    echo "  FAIL  $action@$sha does not resolve" >&2
    failures=$((failures + 1))
  fi
done

echo ""
echo "============================================================"
if [[ "$failures" -eq 0 ]]; then
  echo "  RESULT: All action references resolve"
else
  echo "  RESULT: $failures action reference(s) failed to resolve"
fi
echo "============================================================"
exit "$failures"
