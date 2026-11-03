# 04 — Replay

## Purpose

Show how Transist replays a completed run as a new run using the persisted
recipe snapshot and persisted workflow inputs from the source run.

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this
  workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`

## Workflow

Use [`workflow.yaml`](workflow.yaml) with [`inputs.example.json`](inputs.example.json).

## Expected Result

The source run appends one line to `replay-history.log`. Replaying it with a new
run id appends the same line again, proving the replay reused the persisted run
input rather than asking for new inputs.

## Capability Demonstrated

Full-run replay with explicit lineage and persisted-input reuse.
