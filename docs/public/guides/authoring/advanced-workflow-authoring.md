# Build Your First Real Workflow

Author a complete offline workflow: generate a greeting, pause for human review,
then pass the first action's output into a second action. The final action runs
**only after approval**. Nothing is published, sent over the network, or written
to a repository workspace.

## 1. Prepare an isolated project

Use Bash, Python 3, and a release-installed `transist` on `PATH`. Start at a
Transist repository checkout root, which supplies the bundled `plugins/shell`
and `examples/authoring/workflow` sources. The authoring examples are not included
in the minimal release archive. No AI assistant is needed.

On Linux install working bubblewrap; macOS uses system `sandbox-exec`. See
[Quickstart prerequisites](../getting-started/quickstart.md#prerequisites).
For a source-built CLI, first run:

```bash
cargo build --locked -p transist-cli
export PATH="$PWD/target/debug:$PATH"
```

Building requires the repository toolchain and dependencies; the exercise is
offline once those prerequisites are installed. Keep this shell at the checkout
root. Copy the example into an editable directory and initialize a separate home:

```bash
export TUTORIAL_ROOT="$(mktemp -d /tmp/transist-workflow-tutorial.XXXXXX)"
export TUTORIAL_ROOT="$(cd "$TUTORIAL_ROOT" && pwd -P)"
cp -R examples/authoring/workflow/. "$TUTORIAL_ROOT/"
export TRANSIST_HOME="$TUTORIAL_ROOT/home"
mkdir -p "$TRANSIST_HOME"
transist home init --mode standalone --home "$TRANSIST_HOME"
transist plugins install --path plugins/shell/plugin.yaml
transist plugins inspect shell
```

Standalone mode accepts unsigned packages without capability grants. It is for
this disposable exercise, not a replacement for governed delivery. All subsequent
commands use the exported home; your normal home is unchanged.
The `pwd -P` command removes symlink components such as macOS `/tmp` from the
runtime-home path before initialization.

## 2. Author the two files

Open `$TUTORIAL_ROOT/workflow.yaml` and `$TUTORIAL_ROOT/inputs.json` in an editor,
or create them with the complete contents below. The labels are relative to
`examples/authoring/workflow/` and to your editable copy.

### `workflow.yaml`

<!-- tutorial-file: workflow.yaml -->
```yaml
schema: transist.flow.v1
id: tutorial.reviewed_greeting
version: 0.1.0
plugins:
  shell: 0.1.0
inputs:
  name:
    type: string
    required: true
steps:
  - id: draft
    uses: process:shell/echo
    with:
      args: ["Hello,", "{{inputs.name}}"]
  - id: review
    control: control.gate.manual
    with:
      gate_id: greeting-review
  - id: deliver
    uses: process:shell/echo
    with:
      args: ["Approved:", "{{steps.draft.out.stdout}}"]
```

### `inputs.json`

<!-- tutorial-file: inputs.json -->
```json
{"name":"Ada"}
```

Read the workflow from top to bottom:

- `schema` selects the workflow format; `id` and `version` identify this workflow.
- Root `plugins` pins the installed package version. It is not a step field.
- `inputs.name` requires a string; the JSON file binds it to `Ada`.
- `uses: process:shell/echo` means adapter `process`, plugin `shell`, action `echo`.
  `with` must match that action's input schema. This action joins its arguments
  and returns `stdout`; it does not invoke an operating-system `echo` command.
- `draft` returns `{"stdout":"Hello, Ada", ...}`. Outputs are JSON data, not
  variables or shared filesystem paths.
- `review` blocks the run on `greeting-review`. Inspect the draft before deciding.
- `deliver` consumes `draft.out.stdout` through the direct step-output reference
  and produces `Approved: Hello, Ada`.

Whole-value `{{steps.<id>.out.<field>}}` references preserve JSON types.
Interpolation inside larger strings accepts scalars only. Use
[managed documents](../../reference/path-and-resource-semantics.md#managed-document-paths)
for later file/tree handoffs, not invocation-private scratch paths. This tutorial
uses no files between actions and declares no explicit workspace. The current
fallback is the CLI working directory with full workspace access. The chosen
`echo` action performs no workspace writes, but standalone mode and an isolated
home do **not** remove that authority. For real file-processing workflows, declare
an exact input-bound workspace and prefer `read_only` when no mutation is needed;
see the [workspace foundation](workflow-patterns-cookbook.md#workspace-foundation).

## 3. Check and explain before installing

```bash
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml"
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
transist workflows explain --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/inputs.json" --format json
```

Without bound inputs, required input checks can remain `not_evaluated`. The
second command checks representative bindings. The downstream output reference
may be `deferred_to_dispatch`: its value is validated after `draft` produces it,
before the consumer runs. This does not skip validation.

Explanation shows the ordered actions, dependency and manual gate; it does not
execute an action, grant permission, or prove live readiness. Source checks and
explanation create no run and perform no plugin probes.

```bash
transist workflows install --path "$TUTORIAL_ROOT/workflow.yaml"
transist workflows check tutorial.reviewed_greeting@0.1.0 \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
```

Review and confirm the install plan if prompted. Installation stores the workflow
but does not run it. The ordinary installed check is probe-free; `--live` requests
live readiness separately. `workflows run` performs live checks before run birth.

## 4. Launch and wait for the gate

The following command executes only this local greeting exercise:

```bash
transist workflows run tutorial.reviewed_greeting@0.1.0 --run-ref greeting-demo \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
```

The runner is detached. Define this bounded Bash helper; it checks persisted
status rather than assuming a fixed sleep is enough:

```bash
wait_for_status() (
  set -e
  expected="$1"
  deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    transist runs show greeting-demo --format json > "$TUTORIAL_ROOT/state.json"
    status=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["status"])' \
      "$TUTORIAL_ROOT/state.json")
    if [ "$status" = "$expected" ]; then
      transist runs lease inspect greeting-demo --format json > "$TUTORIAL_ROOT/lease.json"
      if python3 -c 'import json, sys; sys.exit(0 if json.load(open(sys.argv[1])) is None else 1)' \
        "$TUTORIAL_ROOT/lease.json"; then
        return 0
      fi
      sleep 0.2
      continue
    fi
    case "$status" in
      failed|canceled|success)
        printf 'Unexpected terminal status: %s\n' "$status" >&2
        return 1 ;;
    esac
    sleep 0.2
  done
  printf 'Timed out; retain the home and inspect the run.\n' >&2
  return 1
)
wait_for_status blocked
```

If a command fails, stop here and inspect the retained home. A timeout does not
cancel the runner; do not delete an active run's files.
The helper also waits for the runner to release its lease, so approval cannot
race the transition into the blocked state. Never break a live lease to approve.

## 5. Inspect, approve, and verify the handoff

```bash
transist runs show greeting-demo --format json > "$TUTORIAL_ROOT/state.json"
python3 - "$TUTORIAL_ROOT/state.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    state = json.load(source)
assert state["status"] == "blocked", state["status"]
draft = state["steps"]["draft"]["last_result"]["output"]["stdout"]
assert draft == "Hello, Ada", draft
assert state["steps"]["deliver"]["status"] == "pending"
print("Review this draft:", draft)
PY
```

For this synthetic run, approve only after reading that draft:

```bash
transist runs gate approve greeting-demo greeting-review \
  --actor "Tutorial Reviewer <reviewer@example.com>" \
  --reason "Reviewed the greeting; allow the final local echo"
wait_for_status success
transist runs show greeting-demo --format json > "$TUTORIAL_ROOT/state.json"
python3 - "$TUTORIAL_ROOT/state.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    state = json.load(source)
assert state["status"] == "success", state["status"]
delivered = state["steps"]["deliver"]["last_result"]["output"]["stdout"]
assert delivered == "Approved: Hello, Ada", delivered
print(delivered)
PY
transist runs tail greeting-demo --limit 20
```

Expected final output: **`Approved: Hello, Ada`**. This gate records a decision
before the final action; it does not approve arbitrary workspace mutations.
For immutable file review, use a gate with `review_documents` and a protected
document read as described in the [cookbook](workflow-patterns-cookbook.md#3-manual-approval-gate).

## 6. Change an input, not the old run

Change `Ada` to `Grace` in your copied `inputs.json`, launch with a **new**
`--run-ref`, inspect its draft, and approve its own gate. A run pins its inputs
and workflow; editing source cannot change a run already created. If changing
workflow bytes in this maintained `0.1.0` example, install the edited source in a
fresh standalone home and reinstall `shell` there. Preserve the preceding home
for inspection; never edit installed recipes or historical run state.

## Troubleshooting

| Symptom | Correction |
| --- | --- |
| Missing plugin/action | Install `plugins/shell/plugin.yaml` in this exported home; inspect `shell` and verify pin `0.1.0`. |
| Missing or wrong-type input | Supply the shown JSON file with a string `name`; rerun the bound source check. |
| Run remains blocked | This is expected before review. Inspect `draft`, then approve the exact `greeting-review` gate with actor and reason. |
| Duplicate run reference | Choose a new reference; do not delete the previous run to reuse it. |
| Sandbox failure | Install functional bubblewrap on Linux; check system `sandbox-exec` on macOS. Standalone mode does not disable sandboxing. |
| Polling timeout or failed run | Keep the home; use `runs show greeting-demo --failed --format json` and `runs tail greeting-demo --limit 20`. |

## Cleanup and next steps

After the shown run reaches success, verify again before removing the disposable
directory. If you launched extra runs, confirm they are terminal too; do not run
cleanup while a gate or invocation is active.

```bash
(
  transist runs show greeting-demo --format json > "$TUTORIAL_ROOT/state.json" || exit 1
  python3 -c 'import json, sys; assert json.load(open(sys.argv[1]))["status"] == "success"' \
    "$TUTORIAL_ROOT/state.json" || exit 1
  transist runs lease inspect greeting-demo --format json > "$TUTORIAL_ROOT/lease.json" || exit 1
  python3 -c 'import json, sys; assert json.load(open(sys.argv[1])) is None' \
    "$TUTORIAL_ROOT/lease.json" || exit 1
  rm -rf -- "$TUTORIAL_ROOT"
) && unset TRANSIST_HOME TUTORIAL_ROOT
```

- [Workflow patterns cookbook](workflow-patterns-cookbook.md): branches, modules,
  documents, secrets, and cleanup.
- [Workflow schema reference](../../reference/workflow-schema.md): exact fields.
- [Write your first plugin](../plugins/write-your-first-plugin.md): replace an
  existing integration with your own explicit action contract.
- [Governed runtime setup and delivery](../trust/governed-runtime-setup-and-delivery.md):
  signing outside the runtime, trust import, exact authorization, and preparation.
