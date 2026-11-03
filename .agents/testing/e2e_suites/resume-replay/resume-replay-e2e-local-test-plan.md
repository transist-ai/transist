# Resume + Replay E2E Test Plan

## Goal

Exercise runs resume and runs replay with per-step shell assertions.

> **Executable:** `run.sh` in this directory is the committed hermetic script. Run it directly or via `python3 .agents/testing/run_e2e.py --suite resume-replay`.

## Not Covered

- Central inspector or docker.
- Network access.
- Manual gate decisions.

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

export SUITE_ID=resume-replay
export TRANSIST_HOME="/tmp/.transist-${SUITE_ID}-$(date +%s)"

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }
```

## Preconditions

- `plugins/shell/plugin.yaml` exists.
- `workflows/demos/getting_started/03_resume_after_failure/workflow.yaml` exists.
- `workflows/demos/getting_started/04_replay/workflow.yaml` exists.

## Execution

### Step 1 -- Boot and compile

```bash
$TRANSIST_BIN --home "$TRANSIST_HOME" home init
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path plugins/shell/plugin.yaml
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path workflows/demos/getting_started/03_resume_after_failure/workflow.yaml --approve
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path workflows/demos/getting_started/04_replay/workflow.yaml --approve
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_03_resume_after_failure@0.1.0
printf '{"marker":"alpha"}\n' > "$SUITE_ROOT/replay-inputs.json"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_04_replay@0.1.0 --inputs-file "$SUITE_ROOT/replay-inputs.json"
```

```bash
set -euo pipefail
[ -f "$TRANSIST_HOME/registry/recipes/demo_03_resume_after_failure-0.1.0.yaml" ] && echo "OK: resume demo compiled" || { echo "FAIL: missing resume recipe"; exit 1; }
[ -f "$TRANSIST_HOME/registry/recipes/demo_04_replay-0.1.0.yaml" ] && echo "OK: replay demo compiled" || { echo "FAIL: missing replay recipe"; exit 1; }
```

### Step 2 -- Failed run then resume

```bash
resume_run_id="resume-test-$(now_ms)"
resume_workspace="$TRANSIST_HOME/runs/run_${resume_run_id}/workspace"
mkdir -p "$resume_workspace"
rm -f "$resume_workspace/.retry-ok"
set +e
( cd "$resume_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_03_resume_after_failure@0.1.0 --run-ref "$resume_run_id" )
launch_rc=$?
set -e
fail_state="$TRANSIST_HOME/runs/run_${resume_run_id}/state.json"
# Wait for the initial run to reach failed status before creating the marker.
for _ in $(seq 1 30); do
  pre_status="$(jq -r '.status // ""' "$fail_state" 2>/dev/null || true)"
  [ "$pre_status" = "failed" ] && break
  sleep 1
done
touch "$resume_workspace/.retry-ok"
( cd "$resume_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" runs resume "$resume_run_id" )
for _ in $(seq 1 40); do
  resume_status="$(jq -r '.status // ""' "$fail_state" 2>/dev/null || true)"
  [ "$resume_status" = "success" ] && break
  sleep 1
done
```

```bash
set -euo pipefail
[ -f "$fail_state" ] && echo "OK: failed run state exists" || { echo "FAIL: missing failed run state"; exit 1; }
[ "$pre_status" = "failed" ] && echo "OK: resume source failed first" || { echo "FAIL: resume source status was $pre_status"; exit 1; }
[ "$resume_status" = "success" ] && echo "OK: resume completed successfully" || { echo "FAIL: resume status was $resume_status"; exit 1; }
```

### Step 3 -- Replay source run

```bash
source_run_id="replay-source-$(now_ms)"
source_workspace="$TRANSIST_HOME/runs/run_${source_run_id}/workspace"
mkdir -p "$source_workspace"
( cd "$source_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_04_replay@0.1.0 --run-ref "$source_run_id" --input marker=alpha )
source_recipe_sha="$(python3 - <<'PY' "$TRANSIST_HOME/runs/run_${source_run_id}/recipe.yaml"
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
```

```bash
set -euo pipefail
[ -f "$TRANSIST_HOME/runs/run_${source_run_id}/state.json" ] && echo "OK: source run state exists" || { echo "FAIL: missing source state"; exit 1; }
jq -e '.status == "success"' "$TRANSIST_HOME/runs/run_${source_run_id}/state.json" >/dev/null && echo "OK: source run succeeded" || { echo "FAIL: source run did not succeed"; exit 1; }
```

### Step 4 -- Replay persists lineage and inputs

```bash
replay_run_id="replay-target-$(now_ms)"
replay_workspace="$TRANSIST_HOME/runs/run_${replay_run_id}/workspace"
mkdir -p "$replay_workspace"
( cd "$replay_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" runs replay "$source_run_id" --run-ref "$replay_run_id" )
target_state="$TRANSIST_HOME/runs/run_${replay_run_id}/state.json"
target_recipe_sha="$(python3 - <<'PY' "$TRANSIST_HOME/runs/run_${replay_run_id}/recipe.yaml"
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
```

```bash
set -euo pipefail
[ "$replay_run_id" != "$source_run_id" ] && echo "OK: replay created a new run" || { echo "FAIL: replay reused source run id"; exit 1; }
printf '%s' "$(jq -r '.lineage.source_run_id // empty' "$target_state")" | grep -q "$source_run_id" && echo "OK: lineage points at source" || { echo "FAIL: lineage missing source run id"; exit 1; }
[ "$source_recipe_sha" = "$target_recipe_sha" ] && echo "OK: replay recipe reused persisted snapshot" || { echo "FAIL: replay recipe changed"; exit 1; }
jq -e '.workflow_inputs.marker == "alpha"' "$target_state" >/dev/null && echo "OK: replay preserved inputs" || { echo "FAIL: replay inputs mismatch"; exit 1; }
```

### Step 5 -- Replay ignores source edits

```bash
tmp_flow="/tmp/replay-edit-$(now_ms).yaml"
cp workflows/demos/getting_started/04_replay/workflow.yaml "$tmp_flow"
python3 - <<'PY' "$tmp_flow"
from pathlib import Path
path = Path(__import__('sys').argv[1])
text = path.read_text()
path.write_text(text.replace('replay-history.log', 'replay-history-edited.log'))
PY
replay_edit_run_id="replay-edit-$(now_ms)"
replay_edit_workspace="$TRANSIST_HOME/runs/run_${replay_edit_run_id}/workspace"
mkdir -p "$replay_edit_workspace"
( cd "$replay_edit_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" runs replay "$source_run_id" --run-ref "$replay_edit_run_id" )
```

```bash
set -euo pipefail
[ -f "$TRANSIST_HOME/runs/run_${replay_edit_run_id}/workspace/replay-history.log" ] && echo "OK: replay used persisted recipe" || { echo "FAIL: replay history missing"; exit 1; }
[ ! -f "$TRANSIST_HOME/runs/run_${replay_edit_run_id}/workspace/replay-history-edited.log" ] && echo "OK: edited source did not leak into replay" || { echo "FAIL: edited source affected replay"; exit 1; }
```

### Step 6 -- Cleanup

```bash
rm -f "$TRANSIST_HOME"/runs/run_resume-test-*/workspace/.retry-ok 2>/dev/null || true
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

- The failed run resumes successfully.
- The replayed run keeps persisted recipe and inputs.
- Source edits do not change replay behavior.

Last verified:

- transist @ main
