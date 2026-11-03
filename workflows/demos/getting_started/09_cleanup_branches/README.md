# 09 -- Cleanup Branches

## Purpose

Show all three cleanup branches from one workflow using a small input switch.

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`

## Workflow

Use [`workflow.yaml`](workflow.yaml).

## Expected Result

Each explicit cleanup branch runs automatically on its matching final outcome:

- `fail=false slow=false` succeeds and writes `cleanup-success.txt`.
- `fail=true` fails and writes `cleanup-failed.txt`.
- `fail=false slow=true` stays running long enough to cancel with
  `transist runs cancel <run-ref>`, then writes `cleanup-canceled.txt`.

Cleanup is root-owned and preserves the primary outcome. Inspect the run until
`cleanup.state` is `succeeded`; a terminal primary status can appear before
teardown finishes. Repeated cancellation does not interrupt canceled cleanup.

`transist runs cleanup <run-ref>` is a recovery command for failed or interrupted
cleanup, not a required follow-up for normal failed or canceled runs. Once failed
cleanup has torn down the run, `runs resume` cannot restart its primary work.

## Capability Demonstrated

Automatic outcome-aware cleanup for explicit `success`, `canceled`, and `failed`
branches, with a separate recovery path if teardown itself fails.
