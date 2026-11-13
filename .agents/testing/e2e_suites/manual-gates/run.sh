#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: manual-gates
# Hermetic E2E — governed local-only approve, reject, request-changes, gate-id mismatch.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
TRANSIST_ADMIN_BIN="$REPO_ROOT/target/debug/transist-admin"
export SUITE_ID=manual-gates
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"
export TRANSIST_ACTOR="e2e-runner@test.local"
work_root="$(mktemp -d -t "transist-${SUITE_ID}-XXXXXX")"

cleanup() {
  rm -rf "$TRANSIST_HOME" "$work_root" 2>/dev/null || true
}
trap cleanup EXIT

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

wait_for_blocked() {
  local run_id="$1"
  for _ in $(seq 1 30); do
    status="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs show "$run_id" | jq -r '.status // ""')"
    [ "$status" = "blocked" ] && return 0
    sleep 1
  done
  echo "FAIL: run '$run_id' did not reach blocked state" >&2
  return 1
}

# Step 1 -- Create signed temporary artifacts and a governed local-only home.
for required in "$TRANSIST_BIN" "$TRANSIST_ADMIN_BIN"; do
  [ -x "$required" ] || { echo "FAIL: required binary is missing or not executable: $required" >&2; exit 1; }
done

staged_root="$work_root/staged"
mkdir -p "$staged_root"
cp -R "$REPO_ROOT/plugins/shell" "$staged_root/shell"
cp "$REPO_ROOT/workflows/demos/getting_started/08_manual_gate/workflow.yaml" "$staged_root/manual-gates.workflow.yaml"

key_path="$work_root/manual-gates-e2e.key"
public_key_path="$work_root/manual-gates-e2e.pub"
$TRANSIST_ADMIN_BIN key generate --name manual-gates-e2e --private-key-file "$key_path" >/dev/null
$TRANSIST_ADMIN_BIN key export-public --private-key-file "$key_path" --trust-root-file "$public_key_path" >/dev/null
$TRANSIST_ADMIN_BIN plugin sign "$staged_root/shell" --private-key-file "$key_path" --output "$staged_root/shell.sig" >/dev/null
workflow_package="$work_root/manual-gates-package"
$TRANSIST_ADMIN_BIN workflow package "$staged_root/manual-gates.workflow.yaml" \
  --version 0.1.0 --private-key-file "$key_path" --output "$workflow_package" >/dev/null
rm -f "$key_path"

$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode governed >/dev/null
cat > "$TRANSIST_HOME/config.yaml" <<'YAML'
version: v1
runtime:
  mode: governed
policy:
  profile: allowlist
  sources: [workflow-authorizations]
YAML
[ -d "$TRANSIST_HOME/policy" ] && echo "OK: governed policy root loaded" || { echo "FAIL: governed policy root missing"; exit 1; }
$TRANSIST_BIN --home "$TRANSIST_HOME" trust import "$public_key_path" >/dev/null
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$staged_root/shell/plugin.yaml" --trusted-execution >/dev/null
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check --path "$staged_root/manual-gates.workflow.yaml" >/dev/null
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --package "$workflow_package" --approve >/dev/null
inputs_file="$work_root/inputs.json"
printf '{}\n' > "$inputs_file"
if $TRANSIST_BIN --home "$TRANSIST_HOME" workflows check \
  demo_08_manual_gate@0.1.0 --inputs-file "$inputs_file" --trusted-execution --local-only >/dev/null 2>&1; then
  echo "FAIL: manual-gates check unexpectedly passed before authorization"; exit 1
fi
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows authorize \
  demo_08_manual_gate@0.1.0 --inputs-file "$inputs_file" --approve >/dev/null
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check \
  demo_08_manual_gate@0.1.0 --inputs-file "$inputs_file" --trusted-execution --local-only >/dev/null

shopt -s nullglob
policy_files=("$TRANSIST_HOME"/policy/workflow-authorizations/*.yaml)
[ "${#policy_files[@]}" -eq 1 ] && echo "OK: generated policy fragment applied" || { echo "FAIL: generated policy fragment missing"; exit 1; }
shopt -u nullglob

[ -f "$TRANSIST_HOME/registry/recipes/demo_08_manual_gate-0.1.0.yaml" ] && echo "OK: manual gate compiled" || { echo "FAIL: missing manual gate recipe"; exit 1; }

# Step 2 -- Approve path
approve_run_id="gate-approve-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$approve_run_id" --trusted-execution --local-only
wait_for_blocked "$approve_run_id"
request_json_path="$TRANSIST_HOME/runs/run_${approve_run_id}/steps/review_gate.json"
# Assert the active step's identity before approval lets the runner consume it.
# gates/review-gate.json is a transient decision, not the pending request.
[ -f "$request_json_path" ] && echo "OK: gate request captured" || { echo "FAIL: missing gate request"; exit 1; }
jq -e '.last_result.output.gate_id == "review-gate"' "$request_json_path" >/dev/null && echo "OK: gate id pinned" || { echo "FAIL: gate id mismatch"; exit 1; }
$TRANSIST_BIN --home "$TRANSIST_HOME" runs gate approve "$approve_run_id" --reason "lgtm"
python3 "$REPO_ROOT/.agents/testing/manual_gate_wait.py" \
  "$TRANSIST_HOME/runs/run_${approve_run_id}" approved lgtm --expected-status success

# Step 3 -- Reject path
reject_run_id="gate-reject-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$reject_run_id" --trusted-execution --local-only
wait_for_blocked "$reject_run_id"
set +e
$TRANSIST_BIN --home "$TRANSIST_HOME" runs gate reject "$reject_run_id" --reason "no-go"
reject_rc=$?
set -e

[ "$reject_rc" -eq 0 ] && echo "OK: reject command accepted" || { echo "FAIL: reject command failed"; exit 1; }
python3 "$REPO_ROOT/.agents/testing/manual_gate_wait.py" \
  "$TRANSIST_HOME/runs/run_${reject_run_id}" rejected no-go

# Step 4 -- Request-changes path
changes_run_id="gate-changes-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$changes_run_id" --trusted-execution --local-only
wait_for_blocked "$changes_run_id"
$TRANSIST_BIN --home "$TRANSIST_HOME" runs gate request-changes "$changes_run_id" --reason "fixes"

python3 "$REPO_ROOT/.agents/testing/manual_gate_wait.py" \
  "$TRANSIST_HOME/runs/run_${changes_run_id}" request_changes fixes

# Step 5 -- Gate-id mismatch
mismatch_run_id="gate-mismatch-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$mismatch_run_id" --trusted-execution --local-only
wait_for_blocked "$mismatch_run_id"
set +e
mismatch_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs gate approve "$mismatch_run_id" wrong-gate-id 2>&1)"
mismatch_rc=$?
set -e

[ "$mismatch_rc" -ne 0 ] && echo "OK: wrong gate id rejected" || { echo "FAIL: wrong gate id accepted"; exit 1; }
printf '%s' "$mismatch_out" | grep -Eq 'blocked on gate|gate_id|not found' && echo "OK: mismatch token present" || echo "WARN: mismatch token changed"

echo "PASS: $SUITE_ID"
