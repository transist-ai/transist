# 08 -- Manual Gate

## Purpose

Show a minimal workflow that blocks on a manual gate, then continues after approval.

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`

## Workflow

Use [`workflow.yaml`](workflow.yaml).

## Expected Result

The run blocks at `review-gate` after `pre_gate` runs. Approving the gate lets `post_gate` run and the workflow finishes successfully.

## Capability Demonstrated

Manual gate control with a deterministic before/after path in one workflow.
