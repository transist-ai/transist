# 05 — Artifacts And Evidence

## Purpose

Show the difference between a declared workspace artifact and
an evidence file (step-scoped file under `$TRANSIST_EVIDENCE_DIR`, retained with
the run).

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`

## Workflow

Use [`workflow.yaml`](workflow.yaml).

### Organization

Keep the two explicit `process:shell/exec` calls in YAML and their shell bodies
in focused helpers:

```text
workflow.yaml
scripts/write_outputs.sh
scripts/read_outputs.sh
```

The steps pass literal `source://scripts/write_outputs.sh` and
`source://scripts/read_outputs.sh` helper arguments, respectively. Each resolves
relative to the declaring `workflow.yaml`. Only explicitly referenced assets
are captured; sibling files are not implicitly imported or captured.

For governed use, capture the helpers in the workflow package, sign it with
`transist-admin`, and install the reviewed package and exact plugin dependencies.
Authorize the exact requested capabilities, without source-checkout grants for
helper access. Extracting helpers leaves the workspace artifact handle and
step-scoped evidence contract unchanged.

## Expected Result

Step 1 (`write_outputs`) writes the declared `workspace_artifact` handle to the workspace and
`evidence.txt` under its own `$TRANSIST_EVIDENCE_DIR`.

Step 2 (`read_outputs`) reads the same declared artifact handle back from the workspace —
demonstrating that workspace files are shared across steps. It then writes
its own `read-confirmation.txt` to its own `$TRANSIST_EVIDENCE_DIR`,
demonstrating that each step gets a distinct, step-scoped evidence directory.

Evidence from both steps is visible under the run via
`transist runs show <run-id>`.

## Capability Demonstrated

Workspace-scoped artifacts (shared across steps) vs step-scoped evidence
directories (`$TRANSIST_EVIDENCE_DIR` is unique per step execution and is
not a shared run-level channel).
