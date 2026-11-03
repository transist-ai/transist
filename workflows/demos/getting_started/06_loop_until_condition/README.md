# 06 — Loop Until Condition

## Purpose

Show `control.loop` with a body that increments a counter and an `until`
condition that exits when the body has run enough times. This is the same
loop primitive used by review loops, retry-with-backoff patterns, and any
"keep trying until X" workflow.

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`

## Workflow

Use [`workflow.yaml`](workflow.yaml).

## Expected Result

The loop body runs three times. The workspace contains a `.loop-counter`
file holding `3`.

## Capability Demonstrated

`control.loop` with `body`, `until`, `max_iters`, and `max_duration`.

## Design Notes

The `until` condition references `steps.bump_counter.executions` — the
accumulated execution count for the body step — rather than a field from
the body's output.

This is intentional: after each loop iteration `reset_step_for_rerun`
clears `last_result` (so `out.*` fields disappear) but leaves `executions`
intact. Using `executions` is therefore the reliable way to track how many
times a body step has run within a loop.
