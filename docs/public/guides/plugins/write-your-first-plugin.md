# Write Your First Plugin

Build a local Python plugin that turns `{"name":"Ada"}` into
`{"message":"Hello, Ada!"}`, then call it from a Transist workflow. Everything
runs offline with Python's standard library: no model, service, or credentials.

## 1. Prepare an isolated project

Start with a release-installed `transist` on `PATH` and a Transist repository
checkout as your current directory. The checkout supplies `examples/` and
`plugins/`; **the authoring examples are not bundled in the minimal release
archive**. Keep this shell at the repository root throughout the tutorial.

You also need:

- Python 3.9 or newer, available as `python3` both to your shell and in the
  process preflight path `/usr/bin:/bin:/usr/sbin:/sbin`.
- The process sandbox: system `sandbox-exec` on macOS, or `bwrap` (bubblewrap)
  on Linux. See the [Quickstart prerequisites](../getting-started/quickstart.md#prerequisites)
  for installation instructions.
- A Bash shell for the commands below.

Verify the interpreter used by the readiness probe before proceeding:

```bash
PATH=/usr/bin:/bin:/usr/sbin:/sbin python3 -c \
  'import sys; assert sys.version_info >= (3, 9), sys.version; print(sys.version)'
```

A Python available only through Homebrew, pyenv, or `/usr/local/bin` does not
satisfy that probe. If this check fails, stop and use a host with a compatible
system interpreter; changing only your interactive shell's `PATH` is insufficient.

If you prefer a source-built CLI, build it once from the repository root:

```bash
cargo build --locked -p transist-cli
export PATH="$PWD/target/debug:$PATH"
```

That optional build requires the Rust toolchain and cached dependencies or
network access to obtain them. The tutorial itself needs no network after its
prerequisites are installed.

Copy the example into a user-editable project directory and initialize a
separate runtime home:

```bash
export TUTORIAL_ROOT="$(mktemp -d /tmp/transist-plugin-tutorial.XXXXXX)"
export TUTORIAL_ROOT="$(cd "$TUTORIAL_ROOT" && pwd -P)"
cp -R examples/authoring/plugin/. "$TUTORIAL_ROOT/"
export TRANSIST_HOME="$TUTORIAL_ROOT/home"
mkdir -p "$TRANSIST_HOME"
transist home init --mode standalone --home "$TRANSIST_HOME"
chmod +x "$TUTORIAL_ROOT/greeting/plugin.py"
```

Standalone mode accepts unsigned local packages without capability grants.
`TRANSIST_HOME` selects this isolated home for subsequent commands. Leave it
intact while a run is active; cleanup comes at the end.
`pwd -P` resolves symlink components such as macOS `/tmp` before the home is used.
The workflow omits an explicit workspace: the current fallback is the CLI
working directory with full access. The greeting implementation does no workspace
writes, but this isolated home does not revoke that authority. Real file actions
should use an [explicit workspace](../authoring/workflow-patterns-cookbook.md#workspace-foundation).

## 2. Understand the six files

The copy contains these exact files. Paths in the labels are relative to
`examples/authoring/plugin/` in the checkout and to `$TUTORIAL_ROOT` in your copy.

### Manifest: the installed contract

`greeting/plugin.yaml` declares the plugin identity, process entrypoint, action,
schemas, and Python prerequisite. `default_provider_kind: local` declares that
this action performs local work.

<!-- tutorial-file: greeting/plugin.yaml -->
```yaml
schema: transist.plugin.v1
id: tutorial.greeting
version: 0.1.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.py
package:
  files:
    - plugin.py
    - schemas/greet.input.json
    - schemas/greet.output.json
prerequisites:
  binaries:
    - name: python3
actions:
  - name: greet
    input_schema: schemas/greet.input.json
    output_schema: schemas/greet.output.json
    bindings: {}
capabilities:
  supports_cancel: false
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
```

The installed payload is exactly `plugin.yaml` plus `package.files`.
`plugin.yaml` is implicit; every other required runtime file must be listed
explicitly. Globs, negation, and ignore files are not supported. The surrounding
workflow and sample inputs are separate from the plugin package.

### Schemas: accepted input and successful output

The input is an object with exactly one string, `name`, of 1–80 characters.
The output is an object with exactly one string, `message`.

<!-- tutorial-file: greeting/schemas/greet.input.json -->
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["name"],
  "properties": {"name": {"type": "string", "minLength": 1, "maxLength": 80}},
  "additionalProperties": false
}
```

<!-- tutorial-file: greeting/schemas/greet.output.json -->
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["message"],
  "properties": {"message": {"type": "string"}},
  "additionalProperties": false
}
```

Both schemas use JSON Schema Draft 2020-12 and compile at installation. The
adapter validates materialized action input before dispatch and successful
output before exposing it to the runtime. The `$schema` identifier does not
cause a network download.

### Implementation: a small APP participant

`greeting/plugin.py` implements the greeting and the Adapter Plugin Protocol
(APP). Its extra blank-name check is application validation: whitespace has a
nonzero length but is not a useful name.

<!-- tutorial-file: greeting/plugin.py -->
```python
#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""One-shot local greeting action over APP NDJSON; no network or file writes."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any


def emit(frame: dict[str, Any]) -> None:
    """Write and flush one APP frame to stdout; reserve stderr for diagnostics."""
    print(json.dumps(frame), flush=True)


def read_frame() -> dict[str, Any] | None:
    """Read one APP object, returning None on EOF and rejecting malformed frames."""
    line = sys.stdin.readline()
    if not line:
        return None
    frame = json.loads(line)
    if not isinstance(frame, dict) or frame.get("protocol_version") != "app.v1":
        raise ValueError("Invalid APP object or version")
    return frame


def plugin_version() -> str:
    """Read the package's simple top-level version without a YAML dependency."""
    manifest = Path(__file__).with_name("plugin.yaml").read_text(encoding="utf-8")
    return next(
        line.removeprefix("version: ")
        for line in manifest.splitlines()
        if line.startswith("version: ")
    )


def invoke(request: dict[str, Any]) -> dict[str, Any]:
    """Return a correlated terminal result; reject blank names without logging input."""
    result: dict[str, Any] = {
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": request["invoke_id"],
        "status": "success",
        "output": {},
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {},
    }
    data = request.get("input")
    name = data.get("name") if isinstance(data, dict) else None
    if (
        request.get("action") != "greet"
        or not isinstance(name, str)
        or not name.strip()
        or len(name) > 80
        or set(data) != {"name"}
    ):
        result["status"] = "failed"
        result["error"] = {
            "code": "validation_error",
            "class": "validation",
            "message": "greet requires one non-blank name of at most 80 characters",
            "retryability": "none",
        }
    else:
        result["output"] = {"message": f"Hello, {name}!"}
    return result


def main() -> int:
    """Handshake, serve at most one invocation, then exit; EOF supports probes."""
    try:
        request = read_frame()
        if request is None or request.get("type") != "app.handshake.request.v1":
            raise ValueError("Expected APP handshake")
        emit(
            {
                "type": "app.handshake.response.v1",
                "protocol_version": "app.v1",
                "plugin": {"id": "tutorial.greeting", "version": plugin_version()},
                "adapter_kind": "process",
                "capabilities": {
                    "actions": ["greet"],
                    "supports_cancel": False,
                    "supports_health": False,
                    "supports_artifact_hints": False,
                    "supports_evidence_hints": False,
                },
            }
        )
        request = read_frame()
        if request is None:
            return 0
        if (
            request.get("type") != "app.invoke.request.v1"
            or not isinstance(request.get("invoke_id"), str)
            or not request["invoke_id"]
        ):
            raise ValueError("Expected correlated APP invocation")
        emit(invoke(request))
        return 0
    except (ValueError, KeyError, OSError, StopIteration):
        print("Invalid APP request or package metadata", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
```

The minimal exchange starts with **two handshake frames**: an adapter
`app.handshake.request.v1` and this program's `app.handshake.response.v1`.
A readiness probe may close stdin after that handshake; EOF exits cleanly.
For actual work, an optional `app.invoke.request.v1` follows, answered by one
terminal `app.invoke.result.v1`.

- All frames use `protocol_version: app.v1`. The handshake reports the manifest's
  `tutorial.greeting` identity and version `0.1.0`.
- The result copies the request's `invoke_id` so the adapter can correlate it.
- Stdout carries only flushed newline-delimited JSON (NDJSON), one frame per
  line. Put diagnostics on stderr, never banners or debug prints on stdout.
- The plugin advertises neither cancellation nor health support, so it does not
  implement those optional exchanges.
- It exits promptly after the terminal result. The process adapter requires the
  process boundary to exit within its 250 ms post-result grace period.

The manual checks defend the implementation; they do not replace runtime schema
validation. A general-purpose plugin should use the full APP contract rather
than treating this small example as a complete protocol validator.

### Environment and effects

The `#!/usr/bin/env python3` shebang uses `PATH` to locate the interpreter.
`PATH` is an optional inherited, runtime-controlled environment value; it must
resolve your installed `python3`. The manifest declares the unconditional
`python3` prerequisite. This script reads no custom environment variables and
starts no child processes, so it forwards no environment to children.

The runtime creates and injects an invocation-private `TMPDIR`. This plugin
does not use it, and it is not durable storage. The script reads only its
package manifest and protocol input, makes no network calls, and writes no
files. Transist itself persists the run and result in the selected home.
Credentials and secret references are unsupported by this action; do not put
them in `name`, ordinary inputs, or the package.

### Workflow and sample input

The workflow pins the exact plugin version and binds its required string input
to `greet`. The step's `uses` value names the adapter, plugin, and action.

<!-- tutorial-file: workflow.yaml -->
```yaml
schema: transist.flow.v1
id: tutorial.plugin_greeting
version: 0.1.0
plugins:
  tutorial.greeting: 0.1.0
inputs:
  name:
    type: string
    required: true
steps:
  - id: greet
    uses: process:tutorial.greeting/greet
    with:
      name: "{{inputs.name}}"
```

<!-- tutorial-file: inputs.json -->
```json
{"name":"Ada"}
```

## 3. Install, check, and explain

Install the copied plugin, then inspect the installed contract:

```bash
transist plugins install --path "$TUTORIAL_ROOT/greeting/plugin.yaml"
transist plugins list
transist plugins inspect tutorial.greeting
```

Expect `tutorial.greeting` at `0.1.0` with action `greet`. Installation copies the
selected package; later edits to your source copy do not modify that install.

Check the workflow source both without and with representative inputs, then
explain the bound workflow:

```bash
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml"
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
transist workflows explain --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/inputs.json" --format json
```

Without inputs, the required `name` is `not_evaluated`, not invented. With
inputs, the checks validate its binding against the installed action contract.
Explanation shows the resolved plugin pin and action, schemas, and bound input;
it does not execute or interpret Python behavior. These source commands create
no run and perform no plugin probe.

Install the workflow and check its installed definition:

```bash
transist workflows install --path "$TUTORIAL_ROOT/workflow.yaml"
transist workflows check tutorial.plugin_greeting@0.1.0 \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
```

Review the install plan and confirm it when prompted. A passing installed check
is validation, not live readiness. The next command checks live readiness,
including the APP handshake, before creating the run.

## 4. Run and wait for a terminal result

```bash
transist workflows run tutorial.plugin_greeting@0.1.0 --run-ref plugin-demo \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
```

Execution uses a detached runner. Define this Bash helper and call it after
each launch. It polls `runs show` JSON with Python's standard library, waits at
most 30 seconds, and returns nonzero on failure, cancellation, inspection error,
or time limit. The short sleeps only space the polls; completion is determined
from persisted status.

```bash
wait_for_run() (
  set -e
  run_ref="$1"
  deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    transist runs show "$run_ref" --format json \
      > "$TUTORIAL_ROOT/$run_ref.state.json"
    status=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["status"])' \
      "$TUTORIAL_ROOT/$run_ref.state.json")
    case "$status" in
      success)
        transist runs lease inspect "$run_ref" --format json \
          > "$TUTORIAL_ROOT/$run_ref.lease.json"
        if python3 -c 'import json, sys; sys.exit(0 if json.load(open(sys.argv[1])) is None else 1)' \
          "$TUTORIAL_ROOT/$run_ref.lease.json"; then
          return 0
        fi ;;
      failed|canceled)
        printf 'Run %s ended: %s\n' "$run_ref" "$status" >&2
        return 1 ;;
    esac
    sleep 0.2
  done
  printf 'Timed out waiting for %s; keep its home for inspection.\n' "$run_ref" >&2
  return 1
)
wait_for_run plugin-demo
```

If a command fails, stop the happy path and inspect the retained run. A timeout
does not stop the runner. Do not delete its home or assume it finished.

## 5. Inspect the greeting step

The [CLI reference](../../reference/cli.md#runs-show-run-id) exposes step state
through `runs show`. Select `greet` from that JSON to inspect its request,
result, and status:

```bash
transist runs show plugin-demo --format json
transist runs show plugin-demo --format json > "$TUTORIAL_ROOT/plugin-demo.state.json"
python3 - "$TUTORIAL_ROOT/plugin-demo.state.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    state = json.load(source)
step = state["steps"]["greet"]
print(json.dumps(step, indent=2))
assert state["status"] == "success", state["status"]
assert step["status"] == "succeeded", step["status"]
assert step["last_result"]["output"]["message"] == "Hello, Ada!"
PY
transist runs tail plugin-demo --limit 20
```

Expect `steps.greet.last_result.output.message` to be **`Hello, Ada!`**.
The JSON file above is a read-only inspection copy. Never edit the runtime's
persisted state, installed package, or history to repair a run.

## 6. Try two validation failures

### Wrong type: rejected before a run

Create an invalid numeric input in the isolated copy:

```bash
python3 - "$TUTORIAL_ROOT/invalid-type.json" <<'PY'
import json
import sys

with open(sys.argv[1], "w", encoding="utf-8") as target:
    json.dump({"name": 42}, target)
    target.write("\n")
PY
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/invalid-type.json"
```

Expect a nonzero check result: `name` must be a string. This source check creates
no run and dispatches no plugin action. Correct the input rather than running it.

### Blank name: rejected by the plugin

Whitespace passes the workflow string type and the schema's length constraint.
The plugin applies the additional non-blank rule and returns `validation_error`.

```bash
python3 - "$TUTORIAL_ROOT/blank-name.json" <<'PY'
import json
import sys

with open(sys.argv[1], "w", encoding="utf-8") as target:
    json.dump({"name": "   "}, target)
    target.write("\n")
PY
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/blank-name.json"
transist workflows run tutorial.plugin_greeting@0.1.0 --run-ref plugin-blank \
  --inputs-file "$TUTORIAL_ROOT/blank-name.json"
wait_for_run plugin-blank
```

The wait command should return nonzero for this deliberately failed run. Inspect
it and verify the plugin error; a timeout or unrelated failure is not the
expected result:

```bash
transist runs show plugin-blank --failed --format json
transist runs show plugin-blank --format json > "$TUTORIAL_ROOT/plugin-blank.state.json"
python3 - "$TUTORIAL_ROOT/plugin-blank.state.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    state = json.load(source)
assert state["status"] == "failed", state["status"]
error = state["steps"]["greet"]["last_result"]["error"]
assert error["code"] == "validation_error", error
print(json.dumps(error, indent=2))
PY
```

## 7. Edit and test without replacing installed bytes

Open `$TUTORIAL_ROOT/greeting/plugin.py` in your editor. Change only
`f"Hello, {name}!"` to `f"Welcome, {name}!"` and save it. Leave the repository's
maintained sources, manifest version, and workflow pin at `0.1.0`.

Try reinstalling into the original home:

```bash
transist plugins install --path "$TUTORIAL_ROOT/greeting/plugin.yaml"
```

Expect a version/content collision. `tutorial.greeting@0.1.0` is immutable in
that home. Uninstalling does not clear its recorded digest, and protected runs
can also block uninstall. Preserve the original home and its runs.

For this repository-maintained tutorial, test changed `0.1.0` bytes in a second
fresh standalone home:

```bash
export TRANSIST_HOME="$TUTORIAL_ROOT/home-edited"
mkdir -p "$TRANSIST_HOME"
transist home init --mode standalone --home "$TRANSIST_HOME"
transist plugins install --path "$TUTORIAL_ROOT/greeting/plugin.yaml"
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml"
transist workflows check --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
transist workflows explain --path "$TUTORIAL_ROOT/workflow.yaml" \
  --inputs-file "$TUTORIAL_ROOT/inputs.json" --format json
transist workflows install --path "$TUTORIAL_ROOT/workflow.yaml"
transist workflows run tutorial.plugin_greeting@0.1.0 --run-ref plugin-edited \
  --inputs-file "$TUTORIAL_ROOT/inputs.json"
wait_for_run plugin-edited
transist runs show plugin-edited --format json
```

Review and confirm the new workflow install plan. Expect the edited run's
`steps.greet.last_result.output.message` to be `Welcome, Ada!`. You can still
inspect the original result explicitly:

```bash
transist runs show plugin-demo --home "$TUTORIAL_ROOT/home" --format json
```

In an independent plugin repository, a distinct version is an alternative for
changed bytes installed in the same home: update the manifest version, reported
handshake version, and workflow pin together. Here the handshake reads its
version from the manifest. This repository's maintained examples and plugins
stay at `0.1.0`; use fresh homes for their edit-test cycle.

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| Permission denied at startup | Run `chmod +x "$TUTORIAL_ROOT/greeting/plugin.py"` before installing. Preserve the executable bit in the source package. |
| `python3` missing or shebang fails | Repeat the system-interpreter check above. Python 3.9+ must resolve in the fixed preflight path, not only in your shell's `PATH`. |
| APP parse/protocol error | Remove stdout chatter. Only flushed, single-line JSON APP frames belong on stdout; diagnostics go to stderr. Check `app.v1`, plugin identity/version, and matching `invoke_id`. |
| Successful output rejected | Match `greet.output.json` exactly: an object containing only a string `message`. The adapter returns `validation_error` for invalid successful output. |
| Same-version installation rejected | Use the fresh-home edit-test cycle. Uninstall does not erase immutable version history. |
| Sandbox unavailable or startup denied | Check `sandbox-exec` on macOS or install working bubblewrap on Linux; follow the Quickstart prerequisites. |

For execution failures, use `runs show <run-ref> --failed --format json` and
`runs tail <run-ref> --limit 20` in the correct home. Keep that home until you
have inspected the failure and confirmed the run is terminal.

## Cleanup and next steps

Only clean up after every run you launched is terminal (`success`, `failed`, or
`canceled`). The following guard refreshes inspection for the happy-path run
and any optional runs from this walkthrough. An inspection error or nonterminal
status fails the command and leaves the tutorial directory intact:

```bash
(
  transist runs show plugin-demo --home "$TUTORIAL_ROOT/home" --format json \
    > "$TUTORIAL_ROOT/plugin-demo.state.json" || exit 1
  transist runs lease inspect plugin-demo --home "$TUTORIAL_ROOT/home" --format json \
    > "$TUTORIAL_ROOT/plugin-demo.lease.json" || exit 1
  if [ -f "$TUTORIAL_ROOT/blank-name.json" ]; then
    transist runs show plugin-blank --home "$TUTORIAL_ROOT/home" --format json \
      > "$TUTORIAL_ROOT/plugin-blank.state.json" || exit 1
    transist runs lease inspect plugin-blank --home "$TUTORIAL_ROOT/home" --format json \
      > "$TUTORIAL_ROOT/plugin-blank.lease.json" || exit 1
  fi
  if [ -d "$TUTORIAL_ROOT/home-edited" ]; then
    transist runs show plugin-edited --home "$TUTORIAL_ROOT/home-edited" --format json \
      > "$TUTORIAL_ROOT/plugin-edited.state.json" || exit 1
    transist runs lease inspect plugin-edited --home "$TUTORIAL_ROOT/home-edited" --format json \
      > "$TUTORIAL_ROOT/plugin-edited.lease.json" || exit 1
  fi
  python3 - "$TUTORIAL_ROOT" <<'PY' || exit 1
import json
import sys
from pathlib import Path

for snapshot in Path(sys.argv[1]).glob("*.state.json"):
    with snapshot.open(encoding="utf-8") as source:
        state = json.load(source)
    if state["status"] not in {"success", "failed", "canceled"}:
        raise SystemExit(f"Keep the home: {snapshot.name} is {state['status']}")
for lease in Path(sys.argv[1]).glob("*.lease.json"):
    with lease.open(encoding="utf-8") as source:
        if json.load(source) is not None:
            raise SystemExit(f"Keep the home: {lease.name} is still owned")
PY
  rm -rf -- "$TUTORIAL_ROOT"
) && unset TRANSIST_HOME TUTORIAL_ROOT
```

If you launched additional runs, inspect those too before cleanup. There is no
automatic deletion trap, so a failed or timed-out exercise retains its evidence.

Continue with:

- [Plugin lifecycle management](plugin-packaging-install-upgrade-removal.md)
  for immutable installs, upgrades, and removal.
- [Governed runtime setup and delivery](../trust/governed-runtime-setup-and-delivery.md)
  for signed packages, public trust roots, and policy-controlled delivery.
- [Plugin manifest reference](../../reference/plugin-manifest.md) and
  [APP reference](../../reference/app.md) for the complete contracts.
