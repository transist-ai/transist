#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_release_policy.sh - Read-only verification of the live release policy.
#
# Usage:
#   check_release_policy.sh <candidate-commit-sha>
#
# The GitHub API calls are deliberately scoped to the supplied commit SHA. No
# repository, environment, ruleset, release, tag, or workflow mutation occurs.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ "${1:-}" == --candidate ]]; then
  shift
fi
candidate_sha="${1:-}"
if [[ $# -ne 1 || ! "$candidate_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Usage: $0 <candidate-commit-sha>" >&2
  exit 1
fi

fail() {
  echo "FAIL  $1" >&2
  exit 1
}

pass() {
  echo "PASS  $1"
}

command -v gh >/dev/null 2>&1 || fail "gh CLI is required for live release policy verification"
command -v jq >/dev/null 2>&1 || fail "jq is required for live release policy verification"

branch_policies_file="$repo_root/.branch_policies"
[[ -f "$branch_policies_file" ]] \
  || fail "committed deployment policy source is missing: $branch_policies_file"

committed_policies="$(jq -c '
  if (type == "array"
      and . == [{name: "main", type: "branch"}])
  then sort_by(.name, .type)
  else error("expected exactly the main branch deployment policy")
  end
' "$branch_policies_file" 2>/dev/null)" \
  || fail "committed deployment policy source is malformed: $branch_policies_file"
pass "committed deployment policy source is valid (main branch only)"

repo_api="repos/${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

rulesets_json="$(gh api "$repo_api/rulesets" --paginate --slurp 2>/dev/null)" \
  || fail "could not retrieve repository rulesets"

tag_ruleset_ids="$(jq -r '
  .[][]?
  | select(
      .name == "v-tag-protection"
      and .target == "tag"
      and .enforcement == "active"
    )
  | .id
' <<< "$rulesets_json")" || fail "repository rulesets response is malformed"

tag_ruleset_valid=0
while IFS= read -r ruleset_id; do
  [[ -n "$ruleset_id" ]] || continue
  ruleset_json="$(gh api "$repo_api/rulesets/$ruleset_id" 2>/dev/null)" \
    || continue
  if jq -e '
     .name == "v-tag-protection"
     and .target == "tag"
     and .enforcement == "active"
     and ((.conditions.ref_name.include // []) | type == "array" and . == ["refs/tags/v*"])
     and ((.conditions.ref_name.exclude // []) | type == "array" and length == 0)
      and (.rules | type == "array")
      and ([.rules[].type] | sort == ["deletion", "non_fast_forward", "update"])
      and ((.rules[] | select(.type == "update") | .parameters // {}) as $p
           | (($p.allow_all_users // false) == false)
             and (($p.allow_force_pushes // false) == false)
             and (($p.allow_soft_force_pushes // false) == false)
             and (($p.update_allows_fetch_and_merge // false) == false))
      and ((.bypass_actors // []) | type == "array" and length == 0)
   ' <<< "$ruleset_json" >/dev/null 2>&1; then
    tag_ruleset_valid=1
    break
  fi
done <<< "$tag_ruleset_ids"

if [[ "$tag_ruleset_valid" -ne 1 ]]; then
  fail "active v-tag-protection ruleset must cover refs/tags/v*, block deletion/update/non-fast-forward with no bypass actors, and not permit force pushes or fetch-and-merge updates"
fi
pass "live v* rules block tag update and deletion with no bypass and no force-push allowance"

python3 "$repo_root/scripts/ci/verify_candidate_policy.py" "$candidate_sha"

echo "Release policy checks passed."
