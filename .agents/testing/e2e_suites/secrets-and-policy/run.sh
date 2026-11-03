#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: secrets-and-policy
# Hermetic E2E — secret delivery, non-persistence, policy denial, child env isolation.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
export SUITE_ID=secrets-and-policy
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"
export MY_API_TOKEN="local-test-secret"

cleanup() {
  rm -rf "$TRANSIST_HOME" 2>/dev/null || true
}
trap cleanup EXIT

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

# Step 1 -- Boot and compile
export SECRET_VALUE="$MY_API_TOKEN"
$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode standalone
cat > "$TRANSIST_HOME/config.yaml" <<'YAML'
version: v1
runtime:
  mode: standalone
policy:
  profile: permissive
  remote_secret_delivery:
    process: true
    http: false
    mcp: false
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$REPO_ROOT/workflows/demos/getting_started/10_secret_env/workflow.yaml" --approve
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_10_secret_env@0.1.0 >/dev/null

[ -f "$TRANSIST_HOME/registry/recipes/demo_10_secret_env-0.1.0.yaml" ] && echo "OK: secret demo compiled" || { echo "FAIL: missing secret recipe"; exit 1; }

# Step 2 -- Happy path delivery
secret_run_id="secret-ok-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_10_secret_env@0.1.0 --run-ref "$secret_run_id"

for _ in $(seq 1 40); do
  secret_state="$(jq -r '.status // ""' "$TRANSIST_HOME/runs/run_${secret_run_id}/state.json" 2>/dev/null || true)"
  [ "$secret_state" = "success" ] && break
  sleep 1
done
jq -e '.status == "success"' "$TRANSIST_HOME/runs/run_${secret_run_id}/state.json" >/dev/null && echo "OK: secret run succeeded" || { echo "FAIL: secret run did not succeed"; exit 1; }

# Step 3 -- Non-persistence
set +e
grep -r "$SECRET_VALUE" "$TRANSIST_HOME/" >/tmp/secrets-hit_$$.out 2>&1
secret_rc=$?
set -e

[ "$secret_rc" -eq 1 ] && echo "OK: secret value not found on disk" || { echo "FAIL: secret value leaked"; exit 1; }
prefix="${SECRET_VALUE:0:8}"
set +e
grep -r "$prefix" "$TRANSIST_HOME/" >/tmp/secrets-prefix_$$.out 2>&1
prefix_rc=$?
set -e
[ "$prefix_rc" -eq 1 ] && echo "OK: secret prefix not found on disk" || echo "WARN: secret prefix appeared"
rm -f /tmp/secrets-hit_$$.out /tmp/secrets-prefix_$$.out

# Step 4 -- Missing-secret negative
unset MY_API_TOKEN
missing_run_id="secret-missing-$(now_ms)"
set +e
missing_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_10_secret_env@0.1.0 --run-ref "$missing_run_id" 2>&1)"
missing_rc=$?
set -e
export MY_API_TOKEN="$SECRET_VALUE"

[ "$missing_rc" -ne 0 ] && echo "OK: missing secret run failed fast (nonzero exit)" || { echo "FAIL: missing secret run unexpectedly succeeded"; exit 1; }
[ ! -e "$TRANSIST_HOME/runs/run_${missing_run_id}" ] && echo "OK: missing secret rejected run birth" || { echo "FAIL: missing secret created run state"; exit 1; }

# Step 5 -- Process policy denial
cat >"$TRANSIST_HOME/config.yaml" <<'YAML'
version: v1
runtime:
  mode: standalone
policy:
  profile: permissive
  remote_secret_delivery:
    process: false
    http: false
    mcp: false
YAML
denied_run_id="secret-process-denied-$(now_ms)"
set +e
denied_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_10_secret_env@0.1.0 --run-ref "$denied_run_id" 2>&1)"
denied_rc=$?
set -e

[ "$denied_rc" -ne 0 ] && echo "OK: process-policy denial run failed fast (nonzero exit)" || { echo "FAIL: process-policy denial run unexpectedly succeeded"; exit 1; }
[ ! -e "$TRANSIST_HOME/runs/run_${denied_run_id}" ] && echo "OK: process secret delivery denial rejected run birth" || { echo "FAIL: process policy denial created run state"; exit 1; }

# Step 6 -- Capability allowlist guidance
cat >"$TRANSIST_HOME/capability-hint.yaml" <<'YAML'
schema: transist.flow.v1
id: capability_hint
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: echo
    uses: process:shell/echo
    with:
      args: ["hello"]
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/capability-hint.yaml" --approve
cat >"$TRANSIST_HOME/config.yaml" <<'YAML'
version: v1
runtime:
  mode: standalone
policy:
  profile: allowlist
YAML
capability_run_id="capability-hint-$(now_ms)"
capability_runs_before="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs list --ids-only)"
set +e
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows prepare capability_hint@v1 --run-ref "$capability_run_id" >/tmp/capability-prepare-$$.out 2>/tmp/capability-prepare-$$.err
capability_prepare_rc=$?
set -e
[ "$capability_prepare_rc" -ne 0 ] && echo "OK: ungranted capability preparation was rejected" || { echo "FAIL: ungranted capability run unexpectedly prepared"; exit 1; }
jq -e '.schema == "transist.workflow_readiness.v1" and .status == "not_deliverable" and .run_created == false' /tmp/capability-prepare-$$.out >/dev/null && echo "OK: capability rejection reports no run creation" || { echo "FAIL: capability rejection report mismatch"; exit 1; }
jq -e '.validation.policy_suggestion.schema == "transist.policy_fragment.v1" and (.validation.policy_suggestion | has("grant_sets")) and (.validation.policy_suggestion | has("workflow_grants"))' /tmp/capability-prepare-$$.out >/dev/null && echo "OK: capability rejection exposes policy fragment grants" || { echo "FAIL: capability rejection policy mismatch"; exit 1; }
[ "$($TRANSIST_BIN --home "$TRANSIST_HOME" runs list --ids-only)" = "$capability_runs_before" ] && echo "OK: capability rejection leaves runs list unchanged" || { echo "FAIL: capability rejection changed runs list"; exit 1; }
[ ! -e "$TRANSIST_HOME/runs/run_${capability_run_id}" ] && echo "OK: capability rejection created no run storage" || { echo "FAIL: capability rejection created run storage"; exit 1; }
rm -f /tmp/capability-prepare-$$.out /tmp/capability-prepare-$$.err

# Step 7 -- Child environment isolation
export TRANSIST_STEP2_UNRELATED_ENV="should-not-leak"
cat >"$TRANSIST_HOME/child-env-check.yaml" <<'YAML'
schema: transist.flow.v1
id: child_env_check
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: inspect_env
    uses: process:shell/exec
    with:
      command: sh
      args:
        - -c
        - |
          printf '%s|%s|%s' "${PATH:+present}" "${TRANSIST_MANAGED_RUNTIME_AUTH_ACTIVE:-}" "${TRANSIST_STEP2_UNRELATED_ENV:+present}"
YAML
cat >"$TRANSIST_HOME/config.yaml" <<'YAML'
version: v1
runtime:
  mode: standalone
policy:
  profile: permissive
  child_env:
    inherit: false
    allowlist: ["PATH"]
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/child-env-check.yaml" --approve
child_env_run_id="child-env-check-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run child_env_check@v1 --run-ref "$child_env_run_id"

for _ in $(seq 1 40); do
  child_env_state="$(jq -r '.status // ""' "$TRANSIST_HOME/runs/run_${child_env_run_id}/state.json" 2>/dev/null || true)"
  [ "$child_env_state" = "success" ] && break
  sleep 1
done
[ "$child_env_state" = "success" ] && echo "OK: child-env check run succeeded" || { echo "FAIL: child-env check run did not succeed"; exit 1; }
jq -e '.steps.inspect_env.last_result.output.stdout == "present|1|"' "$TRANSIST_HOME/runs/run_${child_env_run_id}/state.json" >/dev/null && echo "OK: unrelated parent env var was not inherited" || { echo "FAIL: child env isolation output mismatch"; exit 1; }
unset TRANSIST_STEP2_UNRELATED_ENV

# Step 8 -- Authoring validation
cat >"$TRANSIST_HOME/invalid-secret-ref.yaml" <<'YAML'
schema: transist.flow.v1
id: invalid_secret_ref
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: bad_secret_ref
    uses: process:shell/echo
    with:
      args: ["hello"]
      secret_refs: ["MY_API_TOKEN"]
YAML
set +e
invalid_secret_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/invalid-secret-ref.yaml" --approve 2>&1)"
invalid_secret_rc=$?
set -e

[ "$invalid_secret_rc" -ne 0 ] && echo "OK: compile rejected undeclared secret ref" || { echo "FAIL: expected compile failure for undeclared secret ref"; exit 1; }
printf '%s' "$invalid_secret_out" | grep -q "undeclared secret ref" && echo "OK: undeclared secret ref message surfaced" || { echo "FAIL: expected undeclared secret ref message"; exit 1; }

cat >"$TRANSIST_HOME/missing-required-secret.yaml" <<'YAML'
schema: transist.flow.v1
id: missing_required_secret
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: missing_secret
    uses: process:shell/exec_with_secret_env
    with:
      command: sh
      args: ["-c", "printf ok"]
YAML
set +e
missing_required_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$TRANSIST_HOME/missing-required-secret.yaml" --approve 2>&1)"
missing_required_rc=$?
set -e

[ "$missing_required_rc" -ne 0 ] && echo "OK: compile rejected missing required secret ref" || { echo "FAIL: expected compile failure for missing required secret ref"; exit 1; }
printf '%s' "$missing_required_out" | grep -q "missing required secret ref" && echo "OK: missing required secret ref message surfaced" || { echo "FAIL: expected missing required secret ref message"; exit 1; }

echo "PASS: $SUITE_ID"
