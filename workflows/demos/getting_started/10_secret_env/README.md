# 10 -- Secret Env

## Purpose

Show end-to-end secret delivery into a process plugin without writing the secret value to disk.

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`
- export `MY_API_TOKEN` before running the workflow

## Workflow

Use [`workflow.yaml`](workflow.yaml).

## Expected Result

The run succeeds, prints only the secret length, and never writes the secret value itself to the run state or logs.

## Capability Demonstrated

`with.secret_refs` delivery into the shell plugin via an environment variable binding.
