#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Fixture-backed read-only replacement for the gh CLI used by release tests.

set -euo pipefail

[[ "${1:-}" == "api" ]] || { echo "unsupported fake gh command" >&2; exit 2; }
endpoint="${2:-}"
fixture="${GH_API_FIXTURE_FILE:?GH_API_FIXTURE_FILE is required}"
case_name="${GH_API_CASE:?GH_API_CASE is required}"

case "$endpoint" in
  repos/*/rulesets)
    jq -c '.base.rulesets' "$fixture"
    ;;
  repos/*/rulesets/*)
    jq -c --arg case "$case_name" '
       .base.ruleset_detail as $detail
       | .cases[$case] as $config
       | if ($config.invalid_ruleset // false)
         then ($detail | .rules |= map(select(.type != "update")))
         elif ($config.extra_ruleset_rule // false)
         then ($detail | .rules += [{"type": "creation"}])
         elif ($config.ruleset_bypass // false)
         then ($detail | .bypass_actors = [{"actor_id": 7}])
         elif ($config.permissive_update_rule // false)
         then ($detail | .rules |= map(if .type == "update"
              then (.parameters.update_allows_fetch_and_merge = true)
              else . end))
         else $detail
         end
    ' "$fixture"
    ;;
  repos/*/actions/workflows/*/runs\?*)
    workflow="${endpoint#*/actions/workflows/}"
    workflow="${workflow%%/*}"
    if [[ "$workflow" == main-ci.yml ]]; then id=101; else id=102; fi
    jq -nc --argjson id "$id" --arg sha "$(jq -r '.candidate_sha' "$fixture")" \
      --arg case "$case_name" '{workflow_runs: [{id: $id, run_attempt: 1, head_sha: (if $case == "wrong-sha" then "2222222222222222222222222222222222222222" else $sha end)}]} | [.]'
    ;;
  repos/*/actions/runs/*/attempts/*/jobs\?*)
    if [[ "$endpoint" == *'/runs/101/'* ]]; then id=101; gate=main-gate; else id=102; gate=security-gate; fi
    jq -nc --argjson id "$id" --arg gate "$gate" '{jobs: [{name: $gate, run_id: $id, status: "completed", conclusion: "success", check_run_url: ("https://api.github.com/repos/fixture-owner/fixture-repo/check-runs/" + ($id|tostring))}]} | [.]'
    ;;
  repos/*/actions/runs/*/attempts/*)
    if [[ "$endpoint" == *'/runs/101/'* ]]; then id=101; workflow=main-ci.yml; else id=102; workflow=security.yml; fi
    jq -nc --argjson id "$id" --arg workflow "$workflow" --arg case "$case_name" \
      --arg sha "$(jq -r '.candidate_sha' "$fixture")" \
      '{id: $id, run_attempt: 1, head_sha: $sha, path: (".github/workflows/" + (if $case == "wrong-workflow" then "other.yml" else $workflow end)),
        head_branch: "main", event: (if $id == 101 and $case == "mainline-dispatch" then "workflow_dispatch" else "push" end), repository: {id: 5}, head_repository: {id: 5},
        run_started_at: "2026-08-10T11:00:00Z",
        status: (if $id == 101 and $case == "latest-incomplete" then "in_progress" else "completed" end),
        conclusion: (if $id == 101 and $case == "latest-failure" then "failure" elif $id == 101 and $case == "latest-incomplete" then null else "success" end)}'
    ;;
  repos/*/check-runs/*)
    id="${endpoint##*/}"
    if [[ "$id" == 101 ]]; then gate=main-gate; else gate=security-gate; fi
    jq -nc --argjson id "$id" --arg gate "$gate" --arg case "$case_name" \
      --arg sha "$(jq -r '.candidate_sha' "$fixture")" \
      '{id: $id, name: $gate, head_sha: $sha, status: "completed", conclusion: "success",
        app: (if $case == "wrong-provider" then {id: 999, slug: "other-provider"} else {id: 15368, slug: "github-actions"} end)}'
    ;;
  repos/*/commits/*/check-runs)
    jq -c --arg case "$case_name" '
      .base.check_runs as $runs
      | .cases[$case] as $config
      | if ($config.wrong_sha // false)
        then $runs | map(map(.head_sha = "2222222222222222222222222222222222222222"))
        elif ($config.wrong_provider // false)
        then $runs | map(map(.app = {"id": 999, "slug": "other-provider"}))
        elif ($config.latest_main == "failure")
        then $runs | .[1] |= map(if .name == "main-gate" then .conclusion = "failure" else . end)
        elif ($config.latest_main == "incomplete")
        then $runs | .[1] |= map(if .name == "main-gate" then (.conclusion = null | .completed_at = null | .started_at = "2026-08-10T12:00:00Z") else . end)
        else $runs
        end
    ' "$fixture"
    ;;
  repos/*/environments/release-build|repos/*/environments/release-publish)
    jq -c --arg endpoint "$endpoint" --arg case "$case_name" '
       .base.environment as $environment
       | .cases[$case] as $config
        | if ($endpoint | endswith("release-build")) then $environment | .protection_rules |= map(select(.type == "branch_policy"))
          elif ($config.wrong_reviewer // false)
         then $environment | .protection_rules[0].reviewers[0].reviewer.login = "someone-else"
         elif ($config.extra_reviewer // false)
         then $environment | .protection_rules[0].reviewers += [{"reviewer": {"login": "another-reviewer"}}]
         elif ($config.admin_bypass // false)
         then $environment | .can_admins_bypass = true
        else $environment
        end
    ' "$fixture"
    ;;
  repos/*/environments/release-*/deployment-branch-policies\?*)
    jq -c --arg case "$case_name" '
      .base.branch_policies as $policies
      | .cases[$case] as $config
      | if ($config.extra_policy // false)
         then $policies | .branch_policies += [{"id": 8, "name": "main", "type": "branch"}]
         elif ($config.wrong_tag_type // false)
          then $policies | .branch_policies[0].type = "tag"
         elif ($config.empty_policy // false)
         then $policies | .branch_policies = []
         else $policies
         end | [.]
    ' "$fixture"
    ;;
  repos/fixture-owner/fixture-repo)
    jq -nc '{id: 5}'
    ;;
  *)
    echo "fixture has no response for endpoint: $endpoint" >&2
    exit 2
    ;;
esac
