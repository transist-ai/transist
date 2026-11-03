# 02 — Review With Manual Gate

## Purpose

Show a small realistic workflow that gathers context, produces a summary, and
persists evidence for later inspection.

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

The run succeeds, writes a review context file in the workspace, emits stdout
artifacts for both steps, and writes an evidence summary under the run-scoped
evidence directory.

## Capability Demonstrated

Multi-step local workflow execution with persisted artifacts, persisted
evidence, and workflow inputs loaded from a file.
