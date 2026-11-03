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
candidate_sha="${1:-}"
if [[ -z "$candidate_sha" || "$candidate_sha" == -* ]]; then
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
      and length >= 1
      and any(.[]; . == {name: "v*", type: "tag"})
      and all(.[]; (.name | type == "string") and (.type | type == "string")))
  then sort_by(.name, .type)
  else error("expected a deployment policy array including at least one v* tag entry")
  end
' "$branch_policies_file" 2>/dev/null)" \
  || fail "committed deployment policy source is malformed: $branch_policies_file"
pass "committed deployment policy source is valid (includes v* tag policy)"

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

check_runs_json="$(gh api "$repo_api/commits/${candidate_sha}/check-runs" --paginate --slurp 2>/dev/null)" \
  || fail "could not retrieve check runs for SHA $candidate_sha"

check_status() {
  local check_name="$1"
  jq -r --arg check_name "$check_name" --arg candidate_sha "$candidate_sha" '
    [ .[]
      | if type == "array" then .[]?
        else (.check_runs // [])[]?
        end
      | select(.name == $check_name or (.name | endswith("/ " + $check_name)))
      | select(.head_sha == $candidate_sha)
      | select((.app.slug // "") == "github-actions")
      | select((.app.id // 0) == 15368)
    ] as $runs
    | if ($runs | length) == 0 then "not_found"
      else
        ($runs | sort_by(.completed_at // .started_at // "") | last)
        | if (.conclusion // "") == "" then "not_completed" else .conclusion end
      end
  ' <<< "$check_runs_json" 2>/dev/null || echo "error"
}

main_gate_status="$(check_status main-gate)"
security_gate_status="$(check_status security-gate)"

if [[ "$main_gate_status" != "success" ]]; then
  fail "Mainline Validation gate did not pass for SHA $candidate_sha (status: $main_gate_status)"
fi
if [[ "$security_gate_status" != "success" ]]; then
  fail "Security Gate check did not pass for SHA $candidate_sha (status: $security_gate_status)"
fi
pass "release-candidate gates passed for exact SHA $candidate_sha from GitHub Actions"

env_json="$(gh api "$repo_api/environments/release" 2>/dev/null)" \
  || fail "release environment not found; create a GitHub environment named 'release'"
pass "release environment exists"

reviewers_file="${RELEASE_REVIEWERS_FILE:-$repo_root/.release_reviewers}"
[[ -f "$reviewers_file" ]] \
  || fail "committed reviewer source is missing: $reviewers_file"

expected_reviewers="$(jq -c '
  if type == "array" and (all(.[]; type == "string"))
  then sort
  else error("expected a JSON array of reviewer login strings")
  end
' "$reviewers_file" 2>/dev/null)" \
  || fail "committed reviewer source is malformed: $reviewers_file"
pass "committed reviewer source is a valid login array"

required_reviewers="$(jq -c '
  [
    .protection_rules[]?
    | select(.type == "required_reviewers")
    | if (.reviewers | type) != "array" then error("reviewers must be an array")
      else .reviewers[]
        | if type != "object" then error("reviewer must be an object")
          elif ((.reviewer | type) == "object" and (.reviewer.login | type) == "string")
          then .reviewer.login
          elif (.login | type) == "string" then .login
          else error("reviewer login is missing")
          end
      end
  ] | sort
' <<< "$env_json" 2>/dev/null || echo "false")"

if [[ "$required_reviewers" == "false" ]]; then
  fail "release environment protection rules are malformed"
fi
[[ "$required_reviewers" == "$expected_reviewers" ]] \
  || fail "release environment reviewers ($required_reviewers) must match committed source ($expected_reviewers)"
pass "release environment reviewers match the committed source"

jq -e 'has("can_admins_bypass") and (.can_admins_bypass == false)' <<< "$env_json" >/dev/null 2>&1 \
  || fail "release environment allows administrator bypass"
pass "release environment administrator bypass is disabled"

jq -e '
  ((.deployment_branch_policy // {}).protected_branches == false)
  and ((.deployment_branch_policy // {}).custom_branch_policies == true)
' <<< "$env_json" >/dev/null 2>&1 \
  || fail "release environment must use selected custom deployment policies, not protected branches"

deployment_policies="$(gh api "$repo_api/environments/release/deployment-branch-policies" 2>/dev/null)" \
  || fail "could not retrieve release environment deployment policies"

# The API returns deployment policies in .branch_policies. Compare its complete
# name/type set to the committed source, rather than merely finding one entry.
jq -e --argjson expected "$committed_policies" '
  ((.branch_policies // []) | type == "array")
  and ((.branch_policies // []) | all(.[];
      type == "object"
      and (.name | type == "string")
      and (.type | type == "string")
    ))
  and (([.branch_policies[]?
      | {name: .name, type: .type}
    ] | sort_by(.name, .type)) == $expected)
' <<< "$deployment_policies" >/dev/null 2>&1 \
  || fail "release environment deployment policies must match committed .branch_policies exactly"
pass "release environment deployment policy is exactly v* tags"

echo "Release policy checks passed."
