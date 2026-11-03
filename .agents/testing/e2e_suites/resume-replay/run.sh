#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: resume-replay
# Hermetic E2E — runs resume and runs replay.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
export SUITE_ID=resume-replay
export TRANSIST_HOME="$(mktemp -d -t ".transist-${SUITE_ID}-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"
export SUITE_ROOT="/tmp/${SUITE_ID}-$$"
mkdir -p "$SUITE_ROOT"

cleanup() {
  rm -rf "$TRANSIST_HOME" "$SUITE_ROOT" 2>/dev/null || true
}
trap cleanup EXIT

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

count_jsonl() {
  python3 - "$1" <<'PY'
import sys
try:
    n = len([l for l in open(sys.argv[1], encoding="utf-8").read().splitlines() if l.strip()])
except FileNotFoundError:
    n = 0
print(n)
PY
}

# Asserts a JSONL stream is non-empty and every line parses as a single record
# carrying the expected run_id. Exits non-zero on any violation.
assert_jsonl_run_stream() {
  local path="$1" run_id="$2" label="$3"
  python3 - "$path" "$run_id" "$label" <<'PY'
import json, sys
path, run_id, label = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    lines = [l for l in open(path, encoding="utf-8").read().splitlines() if l.strip()]
except FileNotFoundError:
    print(f"FAIL: {label}: missing stream {path}")
    sys.exit(1)
if not lines:
    print(f"FAIL: {label}: empty stream {path}")
    sys.exit(1)
for i, line in enumerate(lines, start=1):
    try:
        record = json.loads(line)
    except json.JSONDecodeError:
        print(f"FAIL: {label}: line {i} of {path} is not valid JSON")
        sys.exit(1)
    if record.get("run_id") != run_id:
        print(f"FAIL: {label}: line {i} run_id != {run_id}")
        sys.exit(1)
print(f"OK: {label}: {len(lines)} valid record(s)")
PY
}

# Step 1 -- Boot and compile
$TRANSIST_BIN --home "$TRANSIST_HOME" home init --mode standalone
$TRANSIST_BIN --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$REPO_ROOT/workflows/demos/getting_started/03_resume_after_failure/workflow.yaml" --approve
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install \
  --path "$REPO_ROOT/workflows/demos/getting_started/04_replay/workflow.yaml" \
  --approve
replay_inputs="$SUITE_ROOT/replay-inputs.json"
printf '{"marker":"alpha"}\n' > "$replay_inputs"
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_03_resume_after_failure@0.1.0 >/dev/null
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows check demo_04_replay@0.1.0 --inputs-file "$replay_inputs" >/dev/null

[ -f "$TRANSIST_HOME/registry/recipes/demo_03_resume_after_failure-0.1.0.yaml" ] && echo "OK: resume demo compiled" || { echo "FAIL: missing resume recipe"; exit 1; }
[ -f "$TRANSIST_HOME/registry/recipes/demo_04_replay-0.1.0.yaml" ] && echo "OK: replay demo compiled" || { echo "FAIL: missing replay recipe"; exit 1; }

# Step 2 -- Failed run then resume
resume_run_id="resume-test-$(now_ms)"
resume_workspace="$SUITE_ROOT/ws-resume"
mkdir -p "$resume_workspace"
rm -f "$resume_workspace/.retry-ok"
set +e
( cd "$resume_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_03_resume_after_failure@0.1.0 --run-ref "$resume_run_id" )
set -e
fail_state="$TRANSIST_HOME/runs/run_${resume_run_id}/state.json"
for _ in $(seq 1 30); do
  pre_status="$(jq -r '.status // ""' "$fail_state" 2>/dev/null || true)"
  [ "$pre_status" = "failed" ] && break
  sleep 1
done
pre_failure_count="$(count_jsonl "$TRANSIST_HOME/runs/run_${resume_run_id}/failures.jsonl")"
touch "$resume_workspace/.retry-ok"
( cd "$resume_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" runs resume "$resume_run_id" )
for _ in $(seq 1 40); do
  resume_status="$(jq -r '.status // ""' "$fail_state" 2>/dev/null || true)"
  [ "$resume_status" = "success" ] && break
  sleep 1
done

[ -f "$fail_state" ] && echo "OK: failed run state exists" || { echo "FAIL: missing failed run state"; exit 1; }
[ "$pre_status" = "failed" ] && echo "OK: resume source failed first" || { echo "FAIL: resume source status was $pre_status"; exit 1; }
[ "$resume_status" = "success" ] && echo "OK: resume completed successfully" || { echo "FAIL: resume status was $resume_status"; exit 1; }

# -- History-preservation assertions for resume -----------------------------
resume_run_root="$TRANSIST_HOME/runs/run_${resume_run_id}"
# The failing workflow run dispatches ordinal 1; the resume dispatches ordinal 2.
jq -e '.steps.wait_for_fix.executions == 2' "$fail_state" >/dev/null \
  && echo "OK: resume left lifetime executions == 2" \
  || { echo "FAIL: resume executions != 2 ($(jq -r '.steps.wait_for_fix.executions' "$fail_state"))"; exit 1; }
jq -e '.resume_count == 1' "$fail_state" >/dev/null \
  && echo "OK: resume_count == 1" \
  || { echo "FAIL: resume_count != 1"; exit 1; }
# Immutable execution records: the prior and the resumed attempt both persist.
[ -f "$resume_run_root/step_executions/wait_for_fix/execution_001.json" ] \
  && echo "OK: execution_001 record retained" \
  || { echo "FAIL: missing execution_001 record"; exit 1; }
[ -f "$resume_run_root/step_executions/wait_for_fix/execution_002.json" ] \
  && echo "OK: execution_002 record appended" \
  || { echo "FAIL: missing execution_002 record"; exit 1; }
# Per-invocation runtime evidence is append-only across resume.
[ -f "$resume_run_root/artifacts/wait_for_fix/execution_001/evidence/transist_runtime_invoke_evidence.json" ] \
  && echo "OK: evidence_001 retained" \
  || { echo "FAIL: missing evidence 001"; exit 1; }
[ -f "$resume_run_root/artifacts/wait_for_fix/execution_002/evidence/transist_runtime_invoke_evidence.json" ] \
  && echo "OK: evidence_002 appended" \
  || { echo "FAIL: missing evidence 002"; exit 1; }
# Events and failures streams are durable, valid, and append-only.
assert_jsonl_run_stream "$resume_run_root/events.jsonl" "$resume_run_id" "resume events stream"
assert_jsonl_run_stream "$resume_run_root/failures.jsonl" "$resume_run_id" "resume failures stream"
[ "$pre_failure_count" -ge 1 ] \
  && echo "OK: pre-resume failure count == $pre_failure_count" \
  || { echo "FAIL: no failure record before resume"; exit 1; }
[ "$(count_jsonl "$resume_run_root/failures.jsonl")" = "$pre_failure_count" ] \
  && echo "OK: resume success added no failures (append-only, count preserved)" \
  || { echo "FAIL: failures stream grew after successful resume"; exit 1; }

# Step 3 -- Replay source run
source_run_id="replay-source-$(now_ms)"
source_workspace="$SUITE_ROOT/ws-source"
mkdir -p "$source_workspace"
( cd "$source_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" workflows run demo_04_replay@0.1.0 --run-ref "$source_run_id" --input marker=alpha )
# Replay re-resolves the runtime filesystem policy's workspace-dir resource
# to the operator's current working directory, so the persisted run recipe is
# NOT byte-identical to the source recipe. The replay invariant is that the
# workflow definition is preserved: compare the recipes with the runtime-
# resolved `resource: workspace://` line normalized out, matching every other
# reviewed definition field byte-for-byte.
recipe_sha() {
  python3 - "$1" <<'PY'
import hashlib, pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text()
norm = re.sub(r'^\s*resource: workspace://.*$', '', text, flags=re.MULTILINE)
print(hashlib.sha256(norm.encode()).hexdigest())
PY
}
source_state="$TRANSIST_HOME/runs/run_${source_run_id}/state.json"
for _ in $(seq 1 40); do
  source_status="$(jq -r '.status // ""' "$source_state" 2>/dev/null || true)"
  [[ "$source_status" == "success" || "$source_status" == "failed" ]] && break
  sleep 1
done
source_recipe_sha="$(recipe_sha "$TRANSIST_HOME/runs/run_${source_run_id}/recipe.yaml")"

[ -f "$TRANSIST_HOME/runs/run_${source_run_id}/state.json" ] && echo "OK: source run state exists" || { echo "FAIL: missing source state"; exit 1; }
jq -e '.status == "success"' "$source_state" >/dev/null && echo "OK: source run succeeded" || { echo "FAIL: source run did not succeed (status: '"$source_status"')"; exit 1; }

# Step 4 -- Replay persists lineage and inputs
replay_run_id="replay-target-$(now_ms)"
replay_workspace="$SUITE_ROOT/ws-replay"
mkdir -p "$replay_workspace"
( cd "$replay_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" runs replay "$source_run_id" --run-ref "$replay_run_id" )
target_state="$TRANSIST_HOME/runs/run_${replay_run_id}/state.json"
replay_recipe="$TRANSIST_HOME/runs/run_${replay_run_id}/recipe.yaml"
# The replay command spawns a detached runner, so wait for the replay run to
# reach a terminal state before asserting its persisted execution records.
replay_status=""
for _ in $(seq 1 40); do
  [ -f "$target_state" ] || { sleep 1; continue; }
  replay_status="$(jq -r '.status // ""' "$target_state" 2>/dev/null || true)"
  [[ "$replay_status" == "success" || "$replay_status" == "failed" ]] && break
  sleep 1
done
for _ in $(seq 1 30); do
  [ -f "$replay_recipe" ] && break
  sleep 1
done
target_recipe_sha="$(recipe_sha "$TRANSIST_HOME/runs/run_${replay_run_id}/recipe.yaml")"

[ "$replay_run_id" != "$source_run_id" ] && echo "OK: replay created a new run" || { echo "FAIL: replay reused source run id"; exit 1; }
jq -e ".lineage.source_run_id == \"$source_run_id\"" "$target_state" >/dev/null && echo "OK: lineage points at source" || { echo "FAIL: lineage missing source run id"; exit 1; }
jq -e ".lineage.root_run_id == \"$replay_run_id\"" "$target_state" >/dev/null && echo "OK: replay lineage is self-rooted" || { echo "FAIL: replay lineage root mismatch"; exit 1; }
replay_locator="$TRANSIST_HOME/runs/_index/run_${replay_run_id}.json"
jq -e ".run_id == \"$replay_run_id\" and .root_run_id == \"$replay_run_id\" and .relative_path == \"run_${replay_run_id}\"" "$replay_locator" >/dev/null && echo "OK: replay locator is self-rooted" || { echo "FAIL: replay locator root mismatch"; exit 1; }
grep -q 'id: demo_04_replay' "$replay_recipe" && echo "OK: replay recipe has correct workflow id" || { echo "FAIL: replay recipe workflow id mismatch"; exit 1; }
jq -e '.workflow_inputs.marker == "alpha"' "$target_state" >/dev/null && echo "OK: replay preserved inputs" || { echo "FAIL: replay inputs mismatch"; exit 1; }
[ "$source_recipe_sha" = "$target_recipe_sha" ] && echo "OK: replay recipe matches source (workflow definition preserved)" || { echo "FAIL: replay recipe mismatch (normalized): $source_recipe_sha != $target_recipe_sha"; exit 1; }

# -- History assertions for replay ------------------------------------------
target_run_root="$TRANSIST_HOME/runs/run_${replay_run_id}"
jq -e '.lineage.mode == "replay"' "$target_state" >/dev/null \
  && echo "OK: replay lineage mode == replay" \
  || { echo "FAIL: replay lineage mode != replay ($(jq -r '.lineage.mode' "$target_state"))"; exit 1; }
jq -e '.steps.append_marker.executions == 1' "$target_state" >/dev/null \
  && echo "OK: replay step lifetime executions == 1" \
  || { echo "FAIL: replay step executions != 1"; exit 1; }
[ -f "$target_run_root/step_executions/append_marker/execution_001.json" ] \
  && echo "OK: replay execution record written" \
  || { echo "FAIL: replay execution record missing"; exit 1; }
[ -f "$target_run_root/artifacts/append_marker/execution_001/evidence/transist_runtime_invoke_evidence.json" ] \
  && echo "OK: replay runtime evidence written" \
  || { echo "FAIL: replay runtime evidence missing"; exit 1; }
assert_jsonl_run_stream "$target_run_root/events.jsonl" "$replay_run_id" "replay events stream"

# Step 5 -- Replay ignores stray edits to the workflow source. The registry
# recipe for a normal (non-v0) workflow version is content-immutable, so a
# modified same-version file cannot replace it; replay must still execute the
# source run's persisted recipe. Author a modified copy, attempt to compile it
# (compile must succeed or be idempotent; it must NOT override the registry
# recipe), and verify replay still writes the original output filename.
tmp_flow="$SUITE_ROOT/04_replay_edited.yaml"
cp "$REPO_ROOT/workflows/demos/getting_started/04_replay/workflow.yaml" "$tmp_flow"
python3 - "$tmp_flow" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
path.write_text(text.replace('replay-history.log', 'replay-history-edited.log'))
PY
# Compile the edited source. The registry recipe is immutable for the same
# workflow id+version, so the compiled recipe on disk is unchanged. Replay
# must still produce the original output file, proving it uses the persisted
# recipe rather than the newly edited source.
$TRANSIST_BIN --home "$TRANSIST_HOME" workflows install --path "$tmp_flow" --approve 2>/dev/null || true
replay_edit_run_id="replay-edit-$(now_ms)"
replay_edit_workspace="$SUITE_ROOT/ws-replay-edit"
mkdir -p "$replay_edit_workspace"
( cd "$replay_edit_workspace" && $TRANSIST_BIN --home "$TRANSIST_HOME" runs replay "$source_run_id" --run-ref "$replay_edit_run_id" )
rm -f "$tmp_flow"
for _ in $(seq 1 30); do
  [ -f "$replay_edit_workspace/replay-history.log" ] && break
  sleep 1
done

[ -f "$replay_edit_workspace/replay-history.log" ] && echo "OK: replay used persisted recipe" || { echo "FAIL: replay history missing"; exit 1; }
[ ! -f "$replay_edit_workspace/replay-history-edited.log" ] && echo "OK: edited source did not leak into replay" || { echo "FAIL: edited source affected replay"; exit 1; }

echo "PASS: $SUITE_ID"
