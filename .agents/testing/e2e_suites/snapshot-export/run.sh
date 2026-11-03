#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: snapshot-export
# Hermetic E2E — snapshot export CLI and hidden inspection surface denial.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
export SUITE_ID=snapshot-export
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"
export SUITE_ROOT="/tmp/${SUITE_ID}-$$"
mkdir -p "$SUITE_ROOT"

cleanup() {
  rm -rf "$TRANSIST_HOME" "$SUITE_ROOT" 2>/dev/null || true
}
trap cleanup EXIT

# Step 1 -- Initialize home, install plugin, and run a workflow
$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode standalone
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml"
cat > "$SUITE_ROOT/workflow.yaml" <<'YAML'
schema: transist.flow.v1
id: snapshot_demo
version: v1
plugins:
  shell: 0.1.0
inputs:
  ticket:
    type: string
steps:
  - id: say
    uses: process:shell/echo
    with:
      args: ["hello", "{{inputs.ticket}}"]
YAML
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$SUITE_ROOT/workflow.yaml" --approve
inputs_file="$SUITE_ROOT/inputs.json"
printf '{"ticket":"TICKET-123"}\n' > "$inputs_file"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check snapshot_demo@v1 --inputs-file "$inputs_file" >/dev/null
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run snapshot_demo@v1 --run-ref snapshot-run --inputs-file "$inputs_file"

[ -f "$TRANSIST_HOME/runs/run_snapshot-run/state.json" ] && echo "OK: run state persisted" || { echo "FAIL: missing run state"; exit 1; }

# Step 2 -- Export metadata-only snapshot to stdout
  snapshot_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export snapshot-run --source-id src_example --workspace-id example/workflows)"

printf '%s' "$snapshot_json" | jq -e '.kind == "transist.inspection.snapshot" and .schema_version == 1 and .root_run_id == "snapshot-run" and .sync_policy.mode == "metadata_only"' >/dev/null && echo "OK: stdout snapshot has expected envelope" || { echo "FAIL: stdout snapshot mismatch"; exit 1; }
printf '%s' "$snapshot_json" | jq -e '.runs[0].run_detail_summary.workflow_input_values == null' >/dev/null && echo "OK: metadata-only snapshot omits raw workflow input values" || { echo "FAIL: metadata-only workflow input redaction mismatch"; exit 1; }

# Step 3 -- Export extended snapshot to a file
  export_meta_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export snapshot-run --source-id src_example --workspace-id example/workflows --tree-version 7 --output "$SUITE_ROOT/snapshot.json" --sync-mode extended --include-workflow-input-values --include-event-payloads --include-run-detail-full --include-stderr-tail --include-gate-request-summary)"

printf '%s' "$export_meta_json" | jq -e --arg output "$SUITE_ROOT/snapshot.json" '.output == $output and .root_run_id == "snapshot-run"' >/dev/null && echo "OK: file export returned metadata" || { echo "FAIL: file export metadata mismatch"; exit 1; }
jq -e '.sync_policy.mode == "extended" and .tree_version == 7 and .runs[0].run_detail_summary.workflow_input_values.ticket == "TICKET-123" and .runs[0].run_detail_summary.run_detail_full != null' "$SUITE_ROOT/snapshot.json" >/dev/null && echo "OK: extended snapshot file includes opted-in fields" || { echo "FAIL: extended snapshot file mismatch"; exit 1; }

# Step 4 -- Negative cases and hidden inspection surface
set +e
  missing_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export does-not-exist --source-id src_example --workspace-id example/workflows 2>&1)"
missing_rc=$?
  tiny_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export snapshot-run --source-id src_example --workspace-id example/workflows --max-payload-bytes 64 2>&1)"
tiny_rc=$?
inspection_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" inspection status 2>&1)"
inspection_rc=$?
set -e

[ "$missing_rc" -ne 0 ] && echo "OK: missing run rejected" || { echo "FAIL: missing run accepted"; exit 1; }
printf '%s' "$missing_out" | grep -q 'was not found' && echo "OK: missing run token present" || echo "WARN: missing run token changed"
[ "$tiny_rc" -ne 0 ] && echo "OK: tiny payload cap rejected" || { echo "FAIL: tiny payload cap accepted"; exit 1; }
[ "$inspection_rc" -ne 0 ] && echo "OK: hidden inspection command rejected on core surface" || { echo "FAIL: hidden inspection command accepted"; exit 1; }
printf '%s' "$inspection_out" | grep -q 'inspection' && echo "OK: hidden inspection rejection mentions command" || echo "WARN: inspection rejection token changed"

echo "PASS: $SUITE_ID"
