#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: run-cleanup
# Hermetic E2E — success, canceled, and failed cleanup branches.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
export SUITE_ID=run-cleanup
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"
export SUITE_ROOT="$(mktemp -d -t "${SUITE_ID}-workspaces-XXXXXX")"
export SUITE_ROOT="$(cd "$SUITE_ROOT" && pwd -P)"

cleanup() {
  rm -rf "$TRANSIST_HOME" "$SUITE_ROOT" 2>/dev/null || true
}
trap cleanup EXIT

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

# Wait for persisted evidence, including cleanup completion rather than only the
# primary terminal status. Poll intervals are not assumptions about run duration.
wait_state() {
  local run_id="$1" predicate="$2"
  local state="$TRANSIST_HOME/runs/run_${run_id}/state.json"
  for _ in $(seq 1 600); do
    if jq -e "$predicate" "$state" >/dev/null 2>&1; then return 0; fi
    sleep 0.1
  done
  echo "FAIL: $run_id did not satisfy $predicate" >&2
  jq '{status, recovery_state, cleanup, steps}' "$state" >&2 || true
  return 1
}

wait_runner_finished() {
  local run_id="$1"
  for _ in $(seq 1 300); do
    if jq -Rse --arg id "$run_id" '
      split("\n") | map(fromjson?
       | select(.schema == "transist.runner_event.v1" and .run_id == $id)
       | select(.event == "started" or .event == "finished")) | last | .event == "finished"
    ' "$TRANSIST_HOME/runs/run_${run_id}/runner.log" >/dev/null 2>&1; then return 0; fi
    sleep 0.1
  done
  echo "FAIL: runner for $run_id did not finish" >&2
  return 1
}

assert_cleanup() {
  local run_id="$1" outcome="$2" workspace="$3" step="on_$2"
  wait_state "$run_id" ".status == \"$outcome\" and .cleanup.trigger == \"$outcome\" and .cleanup.state == \"succeeded\" and .cleanup.steps.$step.status == \"succeeded\" and .cleanup.steps.$step.executions == 1"
  wait_runner_finished "$run_id"
  [ "$(cat "$workspace/cleanup-$outcome.txt")" = "cleanup-$outcome ran" ]
  for other in success canceled failed; do
    if [ "$other" != "$outcome" ]; then [ ! -e "$workspace/cleanup-$other.txt" ]; fi
  done
  echo "OK: $run_id automatically completed only $outcome cleanup"
}

# Step 1 -- Boot and compile
$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode standalone
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$REPO_ROOT/workflows/demos/getting_started/09_cleanup_branches/workflow.yaml" --approve
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_09_cleanup_branches@0.1.0 >/dev/null

[ -f "$TRANSIST_HOME/registry/recipes/demo_09_cleanup_branches-0.1.0.yaml" ] && echo "OK: cleanup demo compiled" || { echo "FAIL: missing cleanup recipe"; exit 1; }

# Step 2 -- Success branch
success_run_id="cleanup-success-$(now_ms)"
success_workspace="$SUITE_ROOT/ws-success"
mkdir -p "$success_workspace"
( cd "$success_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_09_cleanup_branches@0.1.0 --run-ref "$success_run_id" --input fail=false --input slow=false )
assert_cleanup "$success_run_id" success "$success_workspace"

# Step 3 -- Automatic failed cleanup makes primary resume unavailable
failed_run_id="cleanup-failed-$(now_ms)"
failed_workspace="$SUITE_ROOT/ws-failed"
mkdir -p "$failed_workspace"
( cd "$failed_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_09_cleanup_branches@0.1.0 --run-ref "$failed_run_id" --input fail=true )
assert_cleanup "$failed_run_id" failed "$failed_workspace"
set +e
resume_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs resume "$failed_run_id" 2>&1)"
resume_rc=$?
set -e

[ "$resume_rc" -ne 0 ] && echo "OK: resume of cleanup-completed failed run rejected (rc=$resume_rc)" || { echo "FAIL: resume of cleanup-completed failed run should not succeed"; exit 1; }
[ -f "$failed_workspace/cleanup-failed.txt" ] && echo "OK: cleanup artifact still present" || { echo "FAIL: cleanup artifact missing after resume attempt"; exit 1; }
jq -e '.recovery_state == "cleanup_completed" and .status == "failed"' "$TRANSIST_HOME/runs/run_${failed_run_id}/state.json" >/dev/null && echo "OK: resume attempt left failed cleanup state unchanged" || { echo "FAIL: resume changed failed cleanup state"; exit 1; }
printf '%s' "$resume_out" | grep -q 'cleanup has begun' || { echo "FAIL: resume failed for an unrelated reason: $resume_out"; exit 1; }
wait_state "$failed_run_id" '.status == "failed" and .recovery_state == "cleanup_completed" and .cleanup.state == "succeeded" and .cleanup.steps.on_failed.executions == 1'

# Step 4 -- Canceled branch
canceled_run_id="cleanup-canceled-$(now_ms)"
canceled_workspace="$SUITE_ROOT/ws-canceled"
mkdir -p "$canceled_workspace"
( cd "$canceled_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_09_cleanup_branches@0.1.0 --run-ref "$canceled_run_id" --input slow=true )
python3 - "$TRANSIST_HOME" "$canceled_run_id" <<'PY'
import json
import pathlib
import sys
import time

home = pathlib.Path(sys.argv[1])
run_id = sys.argv[2]
run_root = home / "runs" / f"run_{run_id}"
deadline = time.monotonic() + 30
state = {}
observed = {}
while time.monotonic() < deadline:
    try:
        state = json.loads((run_root / "state.json").read_text())
    except (OSError, ValueError):
        time.sleep(0.05)
        continue
    request = state.get("steps", {}).get("maybe_fail_or_slow", {}).get("last_request") or {}
    # The adapter receives staged runtime paths. Its live record may therefore
    # be under private staging rather than the durable logical run directory.
    paths = list((run_root / "active_invocations").glob("*.json"))
    paths += list((home / ".transist-staging").glob("invoke-*/active_invocations/*.json"))
    for path in paths:
        try:
            record = json.loads(path.read_text())
        except (OSError, ValueError) as error:
            observed[str(path)] = str(error)
            continue
        observed[str(path)] = record
        if (
            record.get("run_id") == run_id
            and record.get("step_id") == "maybe_fail_or_slow"
            and record.get("adapter_kind") == "process"
            and record.get("invoke_id") == request.get("invoke_id")
        ):
            print(f"OK: cancel target persisted invocation ready: {path}", flush=True)
            sys.exit(0)
    if state.get("status") in ("success", "failed", "canceled"):
        break
    time.sleep(0.05)
print("FAIL: cancel target never reached its persisted invocation", file=sys.stderr)
print(json.dumps({"state": state, "observed_invocations": observed}, indent=2), file=sys.stderr)
sys.exit(1)
PY
cancel_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs cancel "$canceled_run_id")"

printf '%s' "$cancel_json" | jq -e '.cancellation_requested == true and .settled == true and .final_status == "canceled" and .cleanup_state == "succeeded" and .error == null and .active_execution_cancellation_error == null' >/dev/null && echo "OK: cancel settled" || { echo "FAIL: cancel request mismatch"; exit 1; }
printf '%s' "$cancel_json" | jq -e --arg id "$canceled_run_id" '.run_id == $id and .root_run_id == $id' >/dev/null && echo "OK: cancel report ids match" || { echo "FAIL: cancel report ids mismatch"; exit 1; }
assert_cleanup "$canceled_run_id" canceled "$canceled_workspace"

# Step 5 -- Cancel rejection on terminal run
terminal_run_id="cleanup-cancel-terminal-$(now_ms)"
terminal_workspace="$SUITE_ROOT/ws-terminal"
mkdir -p "$terminal_workspace"
( cd "$terminal_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_09_cleanup_branches@0.1.0 --run-ref "$terminal_run_id" --input fail=false --input slow=false )
assert_cleanup "$terminal_run_id" success "$terminal_workspace"
set +e
cancel_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs cancel "$terminal_run_id" 2>&1)"
cancel_rc=$?
set -e

[ "$cancel_rc" -ne 0 ] && echo "OK: cancel of terminal run rejected" || { echo "FAIL: cancel of terminal run succeeded"; exit 1; }
printf '%s' "$cancel_out" | grep -q 'not currently active' || { echo "FAIL: unexpected cancel rejection: $cancel_out"; exit 1; }

# Step 6 -- Explicit cleanup is recovery after deliberately failed teardown
recovery_workspace="$SUITE_ROOT/ws-recovery"
mkdir -p "$recovery_workspace"
cat > "$SUITE_ROOT/recovery.flow.yaml" <<'YAML'
schema: transist.flow.v1
id: e2e_cleanup_recovery
version: 0.1.0
plugins:
  shell: 0.1.0
steps:
  - id: primary_failure
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "exit 1"]
cleanup:
  - id: teardown
    when: [failed]
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "printf 'attempt\\n' >> cleanup-attempts.txt; test -f cleanup-ready || exit 1; printf 'recovered\\n' > cleanup-recovered.txt"]
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$SUITE_ROOT/recovery.flow.yaml" --approve
recovery_run_id="cleanup-recovery-$(now_ms)"
( cd "$recovery_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run e2e_cleanup_recovery@0.1.0 --run-ref "$recovery_run_id" )
wait_state "$recovery_run_id" '.status == "failed" and .recovery_state == "cleanup_required" and .cleanup.state == "failed" and .cleanup.steps.teardown.executions == 1'
wait_runner_finished "$recovery_run_id"
[ ! -f "$recovery_workspace/cleanup-recovered.txt" ]
touch "$recovery_workspace/cleanup-ready"
( cd "$recovery_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" runs cleanup "$recovery_run_id" )
wait_state "$recovery_run_id" '.status == "failed" and .recovery_state == "cleanup_completed" and .cleanup.state == "succeeded" and .cleanup.steps.teardown.status == "succeeded" and .cleanup.steps.teardown.executions == 2 and .steps.primary_failure.executions == 1'
wait_runner_finished "$recovery_run_id"
[ "$(cat "$recovery_workspace/cleanup-recovered.txt")" = recovered ]
[ "$(wc -l < "$recovery_workspace/cleanup-attempts.txt" | tr -d ' ')" = 2 ]
echo "OK: explicit recovery retried failed teardown without rerunning primary work"

echo "PASS: $SUITE_ID"
