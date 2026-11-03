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
  repos/*/environments/release)
    jq -c --arg case "$case_name" '
       .base.environment as $environment
       | .cases[$case] as $config
       | if ($config.wrong_reviewer // false)
         then $environment | .protection_rules[0].reviewers[0].reviewer.login = "someone-else"
         elif ($config.extra_reviewer // false)
         then $environment | .protection_rules[0].reviewers += [{"reviewer": {"login": "another-reviewer"}}]
         elif ($config.admin_bypass // false)
         then $environment | .can_admins_bypass = true
        else $environment
        end
    ' "$fixture"
    ;;
  repos/*/environments/release/deployment-branch-policies)
    jq -c --arg case "$case_name" '
      .base.branch_policies as $policies
      | .cases[$case] as $config
      | if ($config.extra_policy // false)
         then $policies | .branch_policies += [{"id": 8, "name": "main", "type": "branch"}]
         elif ($config.wrong_tag_type // false)
         then $policies | .branch_policies[0].type = "branch"
         elif ($config.empty_policy // false)
         then $policies | .branch_policies = []
         else $policies
        end
    ' "$fixture"
    ;;
  *)
    echo "fixture has no response for endpoint: $endpoint" >&2
    exit 2
    ;;
esac
