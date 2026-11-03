# Snapshot Export E2E Test Plan

## Goal

Exercise the core `transist runs snapshot export` CLI against local runtime state and confirm the hidden `inspection` sync surface stays unavailable.

> **Executable:** `run.sh` in this directory is the committed hermetic script. Run it directly or via `python3 .agents/testing/run_e2e.py --suite snapshot-export`.

## Not Covered

- Companion sync networking.
- `transist-server` HTTP APIs.
- Multi-page snapshot pagination.

## Environment Variables

| Variable | Purpose | Lifecycle | Sensitive |
|---|---|---|---|
| `TRANSIST_BIN` | CLI binary | Boot | No |
| `TRANSIST_HOME` | ephemeral home | Boot | No |
| `SUITE_ROOT` | ephemeral workspace | Boot | No |
| `SUITE_ID` | suite label | Boot | No |

## Boot-time Setup

```bash
if [ -n "${TRANSIST_BIN:-}" ]; then
  :
elif cargo run -p transist-cli -- --help >/dev/null 2>&1; then
  export TRANSIST_BIN="cargo run -p transist-cli --"
else
  echo "FAIL: unable to resolve TRANSIST_BIN"
  exit 1
fi

export SUITE_ID=snapshot-export
export TRANSIST_HOME="/tmp/.transist-${SUITE_ID}-$(date +%s)"
export SUITE_ROOT="/tmp/${SUITE_ID}-$(date +%s)"
mkdir -p "$SUITE_ROOT"
```

## Preconditions

- `plugins/shell/plugin.yaml` exists.
- `plugins/shell/plugin.py` exists.

## Execution

### Step 1 -- Initialize home, install plugin, and run a workflow

```bash
set -euo pipefail
$TRANSIST_BIN --home "$TRANSIST_HOME" home init
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path plugins/shell/plugin.yaml
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
printf '{"ticket":"TICKET-123"}\n' > "$SUITE_ROOT/inputs.json"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check snapshot_demo@v1 --inputs-file "$SUITE_ROOT/inputs.json"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run snapshot_demo@v1 --run-ref snapshot-run --inputs-file "$SUITE_ROOT/inputs.json"
```

```bash
set -euo pipefail
[ -f "$TRANSIST_HOME/runs/run_snapshot-run/state.json" ] && echo "OK: run state persisted" || { echo "FAIL: missing run state"; exit 1; }
```

### Step 2 -- Export metadata-only snapshot to stdout

```bash
set -euo pipefail
snapshot_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export snapshot-run --source-id src_example --workspace-id example/workflows)"
```

```bash
set -euo pipefail
printf '%s' "$snapshot_json" | jq -e '.kind == "transist.inspection.snapshot" and .schema_version == 1 and .root_run_id == "snapshot-run" and .sync_policy.mode == "metadata_only"' >/dev/null && echo "OK: stdout snapshot has expected envelope" || { echo "FAIL: stdout snapshot mismatch"; exit 1; }
printf '%s' "$snapshot_json" | jq -e '.runs[0].run_detail_summary.workflow_input_values == null' >/dev/null && echo "OK: metadata-only snapshot omits raw workflow input values" || { echo "FAIL: metadata-only workflow input redaction mismatch"; exit 1; }
```

### Step 3 -- Export extended snapshot to a file

```bash
set -euo pipefail
export_meta_json="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export snapshot-run --source-id src_example --workspace-id example/workflows --tree-version 7 --output "$SUITE_ROOT/snapshot.json" --sync-mode extended --include-workflow-input-values --include-event-payloads --include-run-detail-full --include-stderr-tail --include-gate-request-summary)"
```

```bash
set -euo pipefail
printf '%s' "$export_meta_json" | jq -e --arg output "$SUITE_ROOT/snapshot.json" '.output == $output and .root_run_id == "snapshot-run"' >/dev/null && echo "OK: file export returned metadata" || { echo "FAIL: file export metadata mismatch"; exit 1; }
jq -e '.sync_policy.mode == "extended" and .tree_version == 7 and .runs[0].run_detail_summary.workflow_input_values.ticket == "TICKET-123" and .runs[0].run_detail_summary.run_detail_full != null' "$SUITE_ROOT/snapshot.json" >/dev/null && echo "OK: extended snapshot file includes opted-in fields" || { echo "FAIL: extended snapshot file mismatch"; exit 1; }
```

### Step 4 -- Negative cases and hidden inspection surface

```bash
set +e
missing_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export does-not-exist --source-id src_example --workspace-id example/workflows 2>&1)"
missing_rc=$?
tiny_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs snapshot export snapshot-run --source-id src_example --workspace-id example/workflows --max-payload-bytes 64 2>&1)"
tiny_rc=$?
inspection_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" inspection status 2>&1)"
inspection_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$missing_rc" -ne 0 ] && echo "OK: missing run rejected" || { echo "FAIL: missing run accepted"; exit 1; }
printf '%s' "$missing_out" | grep -q 'was not found' && echo "OK: missing run token present" || echo "WARN: missing run token changed"
[ "$tiny_rc" -ne 0 ] && echo "OK: tiny payload cap rejected" || { echo "FAIL: tiny payload cap accepted"; exit 1; }
[ "$inspection_rc" -ne 0 ] && echo "OK: hidden inspection command rejected on core surface" || { echo "FAIL: hidden inspection command accepted"; exit 1; }
printf '%s' "$inspection_out" | grep -q 'inspection' && echo "OK: hidden inspection rejection mentions command" || echo "WARN: inspection rejection token changed"
```

### Step 5 -- Cleanup

```bash
set -euo pipefail
HOME_TO_DELETE="$TRANSIST_HOME"
ROOT_TO_DELETE="$SUITE_ROOT"
rm -rf "$HOME_TO_DELETE" "$ROOT_TO_DELETE"
unset TRANSIST_BIN TRANSIST_HOME SUITE_ROOT SUITE_ID
```

```bash
set -euo pipefail
env | grep -qE '^(TRANSIST_BIN|TRANSIST_HOME|SUITE_ROOT|SUITE_ID)=' && { echo "FAIL: suite env still set"; env | grep -E '^(TRANSIST_BIN|TRANSIST_HOME|SUITE_ROOT|SUITE_ID)='; exit 1; } || echo "OK: shell env clean"
[ ! -d "$HOME_TO_DELETE" ] && echo "OK: TRANSIST_HOME removed" || { echo "FAIL: TRANSIST_HOME still present"; exit 1; }
[ ! -d "$ROOT_TO_DELETE" ] && echo "OK: SUITE_ROOT removed" || { echo "FAIL: SUITE_ROOT still present"; exit 1; }
```

## Pass Criteria

- `runs snapshot export` writes a valid metadata-only snapshot to stdout.
- `runs snapshot export --output <path>` writes a valid extended snapshot file.
- Missing run and tiny payload-cap probes fail.
- The hidden `inspection` sync surface remains unavailable on the core CLI.
