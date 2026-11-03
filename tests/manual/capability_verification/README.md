# Transist Capability Verification

This example is a fully local, operator-run verification workflow for the current Transist v1 authored workflow surface.

It exercises:

- `validate`, `compile`, `start`, and `run`
- `runs preflight`
- invoke steps with `shell.exec` and `shell.echo`
- `control.noop` guards
- `control.noop` loops
- `control.gate.manual`
- `control.finish`
- package-local modules
- `approve`, `request-changes`, `reject`, `runs resume`, and `runs replay`
- CLI inspection, filesystem inspection, and read-only `transist-server` inspection

It does not exercise prompt bindings, prompt-file bindings, skills bindings, remote services, or `/metrics`.

This validation workflow uses only the bundled `shell` plugin from this repo.

## Current Executable Shape

The root workflow is `transist_capability_verification@0.1.0`.

This example includes two package-local modules:

- `snapshot`
- `review_loop`

The manual review loop lives in the `review_loop` module. Gate commands still use the root run id, but the loop step executions, gate records, and review artifacts live under the nested child run.

## Prerequisites

- `cargo`
- `python3`
- `curl`
- `jq`

Run commands from the repository root:

```bash
cd /path/to/transist
export TRANSIST_HARNESS_ROOT="$PWD/tests/manual/capability_verification"
test -f "$TRANSIST_HARNESS_ROOT/workflow.yaml"
```

## Resolve The Effective Home

Inspect the active home first:

```bash
cargo run -p transist-cli -- home get | jq
```

Capture the effective path and use it explicitly in the rest of the session:

```bash
HOME_JSON="$(cargo run -p transist-cli -- home get)"
export TRANSIST_EFFECTIVE_HOME="$(printf '%s\n' "$HOME_JSON" | jq -r '.effective_path')"
printf '%s\n' "$TRANSIST_EFFECTIVE_HOME"
```

Initialize that home if needed:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" init
```

## Install And Inspect The Shell Plugin

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" plugins install --path plugins/shell/plugin.yaml
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" plugins inspect shell
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" plugins list
```

## Start The Read-Only Inspection Server

Run the companion server in a second terminal:

```bash
cd /path/to/transist
TRANSIST_HOME="$TRANSIST_EFFECTIVE_HOME" cargo run -p transist-server -- --port 7777
```

Check health:

```bash
curl -s http://127.0.0.1:7777/health | jq
```

Expected fields include:

- `status: "ok"`
- `service: "transist-server"`
- `mode: "read_only"`

## Validate Before Compile

Validate the root workflow, including both declared modules:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" validate --path "$TRANSIST_HARNESS_ROOT/workflow.yaml"
```

Inspect the parent workflow before compile. At this point `validation.status` should be `valid` and `compiled_recipe_available` should still be `false`:

```bash
curl -s http://127.0.0.1:7777/api/v1/workflows/transist_capability_verification@0.1.0 | jq
```

Compile the root workflow. Its declared modules are captured and compiled with it:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" compile --path "$TRANSIST_HARNESS_ROOT/workflow.yaml"
```

Inspect the parent workflow again. After compile, `compiled_recipe_available` should be `true`:

```bash
curl -s http://127.0.0.1:7777/api/v1/workflows/transist_capability_verification@0.1.0 | jq
```

## Shared Helpers

Use a fresh timestamp suffix for each operator session:

```bash
RUN_STAMP="$(date +%Y%m%d%H%M%S)"
SESSION_PREFIX="capability-${RUN_STAMP}"
REPO_PATH="$(pwd -P)"
```

The example input file lives at:

```bash
cat "$TRANSIST_HARNESS_ROOT/inputs.example.json"
```

Helper for starting root runs:

```bash
start_run() {
  local run_id="$1"
  shift
   cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" workflows run transist_capability_verification@0.1.0 \
     --run-ref "$run_id" \
     --input repo_path="$REPO_PATH" \
     "$@"
}
```

Optional helper for explicit preflight before `run`:

```bash
preflight_run() {
  local run_id="$1"
  cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs preflight "$run_id" | jq
}
```

Helper for locating the latest child run for a workflow plus `session_name` without relying on a child-run id pattern:

```bash
find_run_by_workflow_and_session() {
  local workflow_id="$1"
  local session_name="$2"
  find "$TRANSIST_EFFECTIVE_HOME/runs" -path '*/state.json' -print0 \
    | xargs -0 -n1 jq -r --arg workflow_id "$workflow_id" --arg session_name "$session_name" '
        select(.workflow_id == $workflow_id and (.workflow_inputs.session_name // "") == $session_name)
        | [(.updated_at_ms // 0), .run_id] | @tsv
      ' \
    | sort -n \
    | tail -n 1 \
    | cut -f2
}
```

## Scenario 1: Happy Path

Start the run:

```bash
HAPPY_RUN_ID="cap-happy-${RUN_STAMP}"
HAPPY_SESSION="${SESSION_PREFIX}-happy"

start_run "$HAPPY_RUN_ID" \
  --input session_name="$HAPPY_SESSION" \
  --input include_optional_branch=true \
  --input exercise_resume_demo=false \
  --input checkpoint_number=1

preflight_run "$HAPPY_RUN_ID"
```

Run it. The root run blocks while the child review loop waits at `verification-review`:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs start "$HAPPY_RUN_ID"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$HAPPY_RUN_ID" | jq '.status,.steps.review_loop.status'
```

`preflight_run` is optional in this local example because the shell plugin is
simple, but it demonstrates the same explicit readiness check you would use for
HTTP or MCP-heavy workflows before dispatching work.

Approve the manual gate through the root run id:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" approve "$HAPPY_RUN_ID"
```

Verify terminal success:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$HAPPY_RUN_ID" | jq '.status,.steps.optional_echo.status,.steps.collect_summary.status'
```

## Scenario 2: Guard-Skip Path

```bash
SKIP_RUN_ID="cap-skip-${RUN_STAMP}"
SKIP_SESSION="${SESSION_PREFIX}-skip"

start_run "$SKIP_RUN_ID" \
  --input session_name="$SKIP_SESSION" \
  --input include_optional_branch=false \
  --input exercise_resume_demo=false \
  --input checkpoint_number=2

cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs start "$SKIP_RUN_ID"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" approve "$SKIP_RUN_ID"
```

Confirm `optional_echo` was skipped:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$SKIP_RUN_ID" | jq '.status,.steps.optional_echo.status,.steps.finish.status'
curl -s "http://127.0.0.1:7777/api/v1/runs/${SKIP_RUN_ID}/steps/optional_echo" | jq '.status'
```

## Scenario 3: Request-Changes Loop

```bash
REVISE_RUN_ID="cap-revise-${RUN_STAMP}"
REVISE_SESSION="${SESSION_PREFIX}-revise"

start_run "$REVISE_RUN_ID" \
  --input session_name="$REVISE_SESSION" \
  --input include_optional_branch=true \
  --input exercise_resume_demo=false \
  --input checkpoint_number=3

cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs start "$REVISE_RUN_ID"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" request-changes "$REVISE_RUN_ID" --reason "clarify replay lineage and mention the evidence path"
```

Locate the review child run and confirm that `render_revision` reran and that the regenerated artifact includes the feedback reason:

```bash
REVISE_REVIEW_RUN_ID="$(find_run_by_workflow_and_session review_loop "$REVISE_SESSION")"
printf '%s\n' "$REVISE_REVIEW_RUN_ID"

cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$REVISE_REVIEW_RUN_ID" | jq '.steps.render_revision.executions,.steps.operator_review.status'
sed -n '1,200p' "$TRANSIST_EFFECTIVE_HOME/runs/run_${REVISE_REVIEW_RUN_ID}/artifacts/render_revision/execution_002/artifacts/000_review_candidate.md"
```

Approve the gate and finish the root run:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" approve "$REVISE_RUN_ID"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$REVISE_RUN_ID" | jq '.status'
```

## Scenario 4: Reject Path

```bash
REJECT_RUN_ID="cap-reject-${RUN_STAMP}"
REJECT_SESSION="${SESSION_PREFIX}-reject"

start_run "$REJECT_RUN_ID" \
  --input session_name="$REJECT_SESSION" \
  --input include_optional_branch=true \
  --input exercise_resume_demo=false \
  --input checkpoint_number=4

cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs start "$REJECT_RUN_ID"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" reject "$REJECT_RUN_ID" --reason "stop after validating rejected gate behavior"
```

Verify the root run failed, then inspect the child review run's gate records:

```bash
REJECT_REVIEW_RUN_ID="$(find_run_by_workflow_and_session review_loop "$REJECT_SESSION")"

cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$REJECT_RUN_ID" | jq '.status'
find "$TRANSIST_EFFECTIVE_HOME/runs/run_${REJECT_REVIEW_RUN_ID}/gates" -type f | sort
```

## Scenario 5: Resume Path

```bash
RESUME_RUN_ID="cap-resume-${RUN_STAMP}"
RESUME_SESSION="${SESSION_PREFIX}-resume"

start_run "$RESUME_RUN_ID" \
  --input session_name="$RESUME_SESSION" \
  --input include_optional_branch=true \
  --input exercise_resume_demo=true \
  --input checkpoint_number=5

cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs start "$RESUME_RUN_ID"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" approve "$RESUME_RUN_ID"
```

At this point `resume_checkpoint` should fail because the marker file is missing:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$RESUME_RUN_ID" | jq '.status,.resume_count,.steps.resume_checkpoint.status,.steps.resume_checkpoint.executions'
RESUME_RUN_ROOT="$TRANSIST_EFFECTIVE_HOME/runs/run_${RESUME_RUN_ID}"
printf 'resume now\n' > "$RESUME_RUN_ROOT/operator/resume.ok"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs resume "$RESUME_RUN_ID"
```

Confirm the same run id succeeded and the resume checkpoint has a second execution:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$RESUME_RUN_ID" | jq '.run_id,.status,.resume_count,.steps.resume_checkpoint.executions'
```

## Scenario 6: Replay Path

Replay the successful happy-path run into a new run id:

```bash
REPLAY_RUN_ID="cap-replay-${RUN_STAMP}"
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs replay "$HAPPY_RUN_ID" --run-ref "$REPLAY_RUN_ID"
```

The replay re-executes from the start and blocks at the same child review gate. Approve it:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" approve "$REPLAY_RUN_ID"
```

Verify replay lineage and re-execution:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$REPLAY_RUN_ID" | jq '.status,.lineage,.steps.prepare_workspace.executions'
```

## CLI Inspection Routine

List recent runs:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs list | jq
```

Inspect a root run:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$HAPPY_RUN_ID" | jq
```

Inspect the latest persisted preflight summary:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs preflight "$HAPPY_RUN_ID" | jq
```

Inspect a child review run:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs show "$REVISE_REVIEW_RUN_ID" | jq
```

Tail recent events:

```bash
cargo run -p transist-cli -- --home "$TRANSIST_EFFECTIVE_HOME" runs tail "$REJECT_RUN_ID" --limit 50 | jq
```

Use the root and child run views together to confirm blocked, failed, resumed, replayed, and successful states.

## Filesystem Inspection Routine

Inspect the root run directory directly:

```bash
RUN_ROOT="$TRANSIST_EFFECTIVE_HOME/runs/run_${HAPPY_RUN_ID}"
find "$RUN_ROOT" -maxdepth 4 -type f | sort
```

Open the primary persisted records:

```bash
sed -n '1,220p' "$RUN_ROOT/state.json"
sed -n '1,220p' "$RUN_ROOT/recipe.yaml"
sed -n '1,220p' "$RUN_ROOT/context/run_inputs.json"
sed -n '1,120p' "$RUN_ROOT/events.jsonl"
find "$RUN_ROOT/step_executions" -type f | sort
find "$RUN_ROOT/artifacts" -type f | sort
```

Inspect the review child run, where the loop step executions, review artifacts, and gate history live:

```bash
REVIEW_RUN_ROOT="$TRANSIST_EFFECTIVE_HOME/runs/run_${REVISE_REVIEW_RUN_ID}"
sed -n '1,220p' "$REVIEW_RUN_ROOT/state.json"
sed -n '1,120p' "$REVIEW_RUN_ROOT/events.jsonl"
find "$REVIEW_RUN_ROOT/step_executions" -type f | sort
find "$REVIEW_RUN_ROOT/gates" -type f | sort
find "$REVIEW_RUN_ROOT/artifacts" -type f | sort
```

The current execution-scoped storage contract is:

- `runs/run_<RUN_ID>/artifacts/<STEP_ID>/execution_<NNN>/artifacts/*`
- `runs/run_<RUN_ID>/artifacts/<STEP_ID>/execution_<NNN>/evidence/*`

## Companion-Server Inspection Routine

Inspect root and child runs through the read-only API:

```bash
curl -s "http://127.0.0.1:7777/api/v1/runs?workflow=transist_capability_verification@0.1.0&limit=20&offset=0" | jq
```

The nested `snapshot` module run proves the snapshot module executes as its own child run without relying on a child-run naming convention.

Inspect root run detail, steps, events, artifacts, and evidence:

```bash
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/steps" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/steps/optional_echo" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/events?order=asc&limit=100" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/artifacts" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/evidence" | jq
```

Inspect the child review run detail, render step, gates, and artifacts:

```bash
curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}/steps/render_revision" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}/gates" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}/artifacts" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}/evidence" | jq
```

Fetch blob metadata and content through opaque ids:

```bash
ROOT_ARTIFACT_ID="$(curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/artifacts" | jq -r '.data[] | select(.name=="000_verification_summary.json") | .artifact_id')"
ROOT_EVIDENCE_ID="$(curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/evidence" | jq -r '.data[] | select(.name=="000_inspection_paths.txt") | .evidence_id')"
REVIEW_ARTIFACT_ID="$(curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}/artifacts" | jq -r '.data[] | select(.name=="000_review_candidate.md") | .artifact_id')"

curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/artifacts/${ROOT_ARTIFACT_ID}" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/artifacts/${ROOT_ARTIFACT_ID}/content"
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/evidence/${ROOT_EVIDENCE_ID}" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${HAPPY_RUN_ID}/evidence/${ROOT_EVIDENCE_ID}/content"
curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}/artifacts/${REVIEW_ARTIFACT_ID}" | jq
curl -s "http://127.0.0.1:7777/api/v1/runs/${REVISE_REVIEW_RUN_ID}/artifacts/${REVIEW_ARTIFACT_ID}/content"
```

## Expected Interfaces

This example adds these example-facing workflow interfaces:

- root workflow id: `transist_capability_verification@0.1.0`
- module id: `snapshot`
- module id: `review_loop`
- gate id: `verification-review`
- inputs:
  - `session_name`
  - `include_optional_branch`
  - `exercise_resume_demo`
  - `checkpoint_number`
