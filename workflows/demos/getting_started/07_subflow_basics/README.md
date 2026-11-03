# 07 — Module Basics

## Purpose

Show how to call a package-local module from a root workflow, pass it
inputs, and read an artifact it produced.

## Prerequisites

- released `transist` CLI on your `PATH`
- run commands from the demo bundle root or source checkout root that contains `plugins/shell` and this workflow directory
- bundled `shell` plugin from the demo bundle or runtime source checkout
- `transist home init --mode standalone`
- `transist plugins install --path plugins/shell/plugin.yaml`

## Workflow

Root: [`workflow.yaml`](workflow.yaml).
Module: [`modules/greet.yaml`](modules/greet.yaml).
Inputs: [`inputs.example.json`](inputs.example.json).

### Organization

Keep the root orchestration and the cohesive greeting module in YAML; put their
shell implementation in helpers:

```text
workflow.yaml
scripts/read_greeting.sh
modules/greet.yaml
modules/scripts/write_greeting.sh
```

The root declares the module call and its `process:shell/exec` read step. Its
helper argument is the literal `source://scripts/read_greeting.sh`.
`modules/greet.yaml` declares the write step with the literal
`source://scripts/write_greeting.sh`, which resolves to
`modules/scripts/write_greeting.sh`: helper paths are relative to the YAML file
that declares them, not the root workflow or current directory. There are no
implicit imports or automatic captures of sibling files.

In governed use, capture the declared module and helper assets in the workflow
package and sign it with `transist-admin`. Install the reviewed package and
exact plugin dependencies, then grant only the exact requested capabilities.
Do not grant access to the source checkout to make packaged helpers readable.

Compile the root workflow before running. Its declared module is captured and
compiled as part of the package:

```bash
transist workflows install --path workflows/demos/getting_started/07_subflow_basics/workflow.yaml
```

Then run with the example inputs file:

```bash
transist workflows run demo_07_subflow_basics@0.1.0 \
  --run-ref demo_07_subflow \
  --inputs-file workflows/demos/getting_started/07_subflow_basics/inputs.example.json
```

## Expected Result

The module writes `greeting.txt` in the workspace. The root step
`read_subflow_artifact` reads that file and prints its contents.

## Capability Demonstrated

Package-local module invocation with input passing and a declared shared
workspace artifact.
