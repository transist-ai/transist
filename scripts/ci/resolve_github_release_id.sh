#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# Resolve exactly one GitHub release by its tag_name field, including drafts
# whose URL has changed to an `untagged-*` slug. GitHub's release-by-tag
# endpoint returns 404 for that state, while the authenticated release list
# still exposes the original tag_name.
#
# Usage: resolve_github_release_id.sh <owner/repo> <tag>

set -euo pipefail

if [[ "$#" -ne 2 || -z "$1" || -z "$2" ]]; then
  echo "Usage: $0 <owner/repo> <tag>" >&2
  exit 1
fi

repository="$1"
release_tag="$2"
release_id=""
match_count=0

while IFS=$'\t' read -r candidate_id candidate_tag; do
  if [[ "$candidate_tag" == "$release_tag" ]]; then
    release_id="$candidate_id"
    match_count=$((match_count + 1))
  fi
done < <(
  gh api --paginate "repos/$repository/releases?per_page=100" \
    --jq '.[] | [.id, .tag_name] | @tsv'
)

if [[ "$match_count" -ne 1 || ! "$release_id" =~ ^[0-9]+$ ]]; then
  echo "FAIL: expected exactly one GitHub release with tag_name '$release_tag'; found $match_count" >&2
  exit 1
fi

printf '%s\n' "$release_id"
