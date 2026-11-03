# 03 — Resume After Failure

## Purpose

Show the supported recovery path when a run fails and you want to continue
the same run after fixing the cause.

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this
  workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`

## Workflow

Use [`workflow.yaml`](workflow.yaml).

## Expected Result

The first execution fails because `.retry-ok` does not exist. After you create
that file in the workspace and run `transist runs retry demo_03_resume_after_failure`
(or `transist runs resume demo_03_resume_after_failure`), the same run succeeds
with an appended execution history.

## Capability Demonstrated

Failure inspection plus the supported failed-run recovery path through
`runs retry` (preferred) / `runs resume` (legacy alias). Both commands keep the
same run ID, preserve execution history, and re-enter the run from persisted
state to retry only the failed or canceled steps.
