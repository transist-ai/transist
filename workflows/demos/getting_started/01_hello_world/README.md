# 01 — Hello World

## Purpose

Show the smallest deterministic Transist workflow that still uses the normal
check, install, run, and inspect lifecycle.

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

The run succeeds and the step emits `hello from transist`.

## Capability Demonstrated

Minimal deterministic workflow execution with the standard CLI lifecycle.
