# Manual Gates E2E Test Plan

## Goal

Exercise approve, reject, request-changes, and explicit gate-id mismatch with per-step shell assertions.

> **Executable:** `run.sh` in this directory is the committed hermetic script. Run it directly or via `python3 .agents/testing/run_e2e.py --suite manual-gates`.

## Not Covered

- Central inspector or docker.
- Network access.
- Resume or replay.

## Environment Variables

| Variable | Purpose | Lifecycle | Sensitive |
|---|---|---|---|
| `TRANSIST_BIN` | CLI binary | Boot | No |
| `TRANSIST_HOME` | ephemeral home | Boot | No |
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

export SUITE_ID=manual-gates
export TRANSIST_HOME="/tmp/.transist-${SUITE_ID}-$(date +%s)"

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }
```

## Preconditions

- `plugins/shell/plugin.yaml` exists.
- `workflows/demos/getting_started/08_manual_gate/workflow.yaml` exists.

## Execution

### Step 1 -- Package, install, authorize, and recheck

```bash
$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode governed
transist-admin workflow package workflows/demos/getting_started/08_manual_gate/workflow.yaml --version 0.1.0 --private-key-file "$KEY" --output "$PACKAGE"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --package "$PACKAGE" --approve
printf '{}\n' > "$SUITE_ROOT/inputs.json"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_08_manual_gate@0.1.0 --inputs-file "$SUITE_ROOT/inputs.json" # blocked before authorization
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows authorize demo_08_manual_gate@0.1.0 --inputs-file "$SUITE_ROOT/inputs.json" --approve
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_08_manual_gate@0.1.0 --inputs-file "$SUITE_ROOT/inputs.json"
```

```bash
set -euo pipefail
[ -f "$TRANSIST_HOME/registry/recipes/demo_08_manual_gate-0.1.0.yaml" ] && echo "OK: manual gate compiled" || { echo "FAIL: missing manual gate recipe"; exit 1; }
```

### Step 2 -- Approve path

```bash
approve_run_id="gate-approve-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$approve_run_id"
for _ in $(seq 1 30); do
  status="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs show "$approve_run_id" | jq -r '.status // ""')"
  [ "$status" = "blocked" ] && break
  sleep 1
done
request_json_path="$TRANSIST_HOME/runs/run_${approve_run_id}/steps/review_gate.json"
$TRANSIST_BIN --home "$TRANSIST_HOME" runs gate approve "$approve_run_id" --reason "lgtm"
```

```bash
set -euo pipefail
[ -f "$request_json_path" ] && echo "OK: gate request captured" || { echo "FAIL: missing gate request"; exit 1; }
jq -e '.last_result.output.gate_id == "review-gate"' "$request_json_path" >/dev/null && echo "OK: gate id pinned" || { echo "FAIL: gate id mismatch"; exit 1; }
decision_json="$(ls "$TRANSIST_HOME/runs/run_${approve_run_id}/gates/history/"*approved.json 2>/dev/null | head -n 1)"
[ -n "$decision_json" ] && echo "OK: approval recorded" || { echo "FAIL: missing approval decision"; exit 1; }
jq -e '.decision == "approved"' "$decision_json" >/dev/null && echo "OK: approval decision matches" || { echo "FAIL: approval decision mismatch"; exit 1; }
```

### Step 3 -- Reject path

```bash
reject_run_id="gate-reject-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$reject_run_id"
for _ in $(seq 1 30); do
  status="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs show "$reject_run_id" | jq -r '.status // ""')"
  [ "$status" = "blocked" ] && break
  sleep 1
done
set +e
$TRANSIST_BIN --home "$TRANSIST_HOME" runs gate reject "$reject_run_id" --reason "no-go"
reject_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$reject_rc" -eq 0 ] && echo "OK: reject command accepted" || { echo "FAIL: reject command failed"; exit 1; }
reject_json="$(ls "$TRANSIST_HOME/runs/run_${reject_run_id}/gates/history/"*rejected.json 2>/dev/null | head -n 1)"
jq -e '.decision == "rejected" and .reason == "no-go"' "$reject_json" >/dev/null && echo "OK: rejection recorded" || { echo "FAIL: rejection record mismatch"; exit 1; }
```

### Step 4 -- Request-changes path

```bash
changes_run_id="gate-changes-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$changes_run_id"
for _ in $(seq 1 30); do
  status="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs show "$changes_run_id" | jq -r '.status // ""')"
  [ "$status" = "blocked" ] && break
  sleep 1
done
$TRANSIST_BIN --home "$TRANSIST_HOME" runs gate request-changes "$changes_run_id" --reason "fixes"
```

```bash
set -euo pipefail
decision_json="$(ls "$TRANSIST_HOME/runs/run_${changes_run_id}/gates/history/"*request_changes.json 2>/dev/null | head -n 1)"
[ -n "$decision_json" ] && echo "OK: changes decision recorded" || { echo "FAIL: missing request-changes decision"; exit 1; }
jq -e '.decision == "request_changes" and .reason == "fixes"' "$decision_json" >/dev/null && echo "OK: changes reason recorded" || { echo "FAIL: changes reason mismatch"; exit 1; }
```

### Step 5 -- Gate-id mismatch

```bash
mismatch_run_id="gate-mismatch-$(now_ms)"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_08_manual_gate@0.1.0 --run-ref "$mismatch_run_id"
for _ in $(seq 1 30); do
  status="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs show "$mismatch_run_id" | jq -r '.status // ""')"
  [ "$status" = "blocked" ] && break
  sleep 1
done
set +e
mismatch_out="$($TRANSIST_BIN --home "$TRANSIST_HOME" runs gate approve "$mismatch_run_id" wrong-gate-id 2>&1)"
mismatch_rc=$?
set -e
```

```bash
set -euo pipefail
[ "$mismatch_rc" -ne 0 ] && echo "OK: wrong gate id rejected" || { echo "FAIL: wrong gate id accepted"; exit 1; }
printf '%s' "$mismatch_out" | grep -Eq 'blocked on gate|gate_id|not found' && echo "OK: mismatch token present" || echo "WARN: mismatch token changed"
```

### Step 6 -- Cleanup

```bash
HOME_TO_DELETE="$TRANSIST_HOME"
for _ in $(seq 1 10); do
  rm -rf "$HOME_TO_DELETE" 2>/dev/null || true
  [ ! -d "$HOME_TO_DELETE" ] && break
  sleep 1
done
unset TRANSIST_BIN TRANSIST_HOME SUITE_ID
```

```bash
set -euo pipefail
leaked=""
for v in TRANSIST_BIN TRANSIST_HOME SUITE_ID; do
  if [ -n "${!v:-}" ]; then leaked="$leaked $v"; fi
done
[ -z "$leaked" ] && echo "OK: suite-scoped shell env clean" || { echo "FAIL: suite env still set:$leaked"; env | grep -E "^(TRANSIST_BIN|TRANSIST_HOME|SUITE_ID)="; exit 1; }
[ ! -d "$HOME_TO_DELETE" ] && echo "OK: TRANSIST_HOME removed" || echo "WARN: TRANSIST_HOME still present"
```

## Pass Criteria

- The gate request is persisted under `gates/review-gate/`.
- Approve, reject, and request-changes all write the expected decision files.
- The gate-id mismatch is rejected.

Last verified:

- transist @ main
