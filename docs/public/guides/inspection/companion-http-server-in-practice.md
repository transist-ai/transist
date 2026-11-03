# Guide: The Companion HTTP Server

While the `transist` CLI is the primary tool for *running* workflows, the `transist-server` provides a local, read-only HTTP API for *inspecting* them. It is useful for technical evaluators, operators, and platform engineers building local dashboards, integrating with internal tools, or getting a structured look at run data.

This guide walks you through starting the server and using its API endpoints.

Before run birth, ordinary installed CLI checks and authorization use probe-free
validation; a pass is not live readiness. Use `check --live` or `--output` for
live evidence. Preparation still checks live readiness; server inspection and
recovery use persisted run state, never a prospective validation result.

**The core philosophy is simple:**

- The **CLI** starts and controls workflow execution.
- The **companion server** exposes the state of those workflows over HTTP in a strictly read-only mode.

## What You Will Do

1. Author and run a simple workflow using the CLI.
2. Start the companion HTTP server.
3. Use `curl` to explore the API endpoints for workflows and runs.
4. Learn how to access artifacts, evidence, and metrics.

## Prerequisites

- A local clone of the `transist` repository.
- `transist` and `transist-server` installed and on your `PATH`.
- `curl` and `jq` (for pretty-printing JSON) installed.

---

## 1. Run a Workflow

First, let's create a workflow to inspect. Save this as `workflow.yaml`:

```yaml
schema: transist.flow.v1
id: companion_demo
version: 0.1.0
plugins:
  shell: 0.1.0
inputs:
  name:
    type: string
    required: true
steps:
  - id: hello
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "printf 'hello %s\\n' \"$1\"", "--", "{{inputs.name}}"]
```

Now, use the CLI to install the `shell` plugin, install the workflow, and run it.

```bash
# Run these from your transist repo checkout directory
transist plugins install --path plugins/shell/plugin.yaml
transist workflows install --path workflow.yaml
transist workflows run companion_demo@0.1.0 --run-ref companion-demo-run --input name=world
```

The run is now complete. The companion server will only be inspecting the results.

## 2. Start the Companion Server

In a **new terminal**, start the server.

```bash
# Run this from the same directory as your workflow.yaml
transist-server --port 7777
```

The server will bind to `127.0.0.1:7777` by default and resolves the Transist home the same way the CLI does: `--home` flag, then `TRANSIST_HOME` env, then the persisted user-set home, then the per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`).

## 3. Explore the API with `curl`

Now you can start making requests to the server.

### Health Check

Always a good first step.

```bash
curl -s http://127.0.0.1:7777/health | jq
```

### List Workflows

Discover all the workflows the server knows about.

```bash
curl -s http://127.0.0.1:7777/api/v1/workflows | jq
```

### Inspect a Workflow

Get detailed metadata, the original source code, and the compiled recipe for a specific workflow.

```bash
# Get metadata
curl -s http://127.0.0.1:7777/api/v1/workflows/companion_demo@0.1.0 | jq

# Get the raw source YAML
curl -s http://127.0.0.1:7777/api/v1/workflows/companion_demo@0.1.0/source

# Get the compiled recipe
curl -s http://127.0.0.1:7777/api/v1/workflows/companion_demo@0.1.0/compiled-recipe
```

### List Runs

Get a list of all workflow runs.

```bash
curl -s http://127.0.0.1:7777/api/v1/runs | jq
```

You can filter by workflow: `?workflow=companion_demo@0.1.0`.

API resource identifiers are opaque. Workflow-file, artifact, and evidence
responses expose IDs and relative display paths, never absolute host paths.
Use the returned content links rather than constructing filesystem locations.

### Inspect a Single Run

This is where it gets interesting. You can dive deep into a specific run.

```bash
# Get the main run detail
curl -s http://127.0.0.1:7777/api/v1/runs/companion-demo-run | jq

# Get the event timeline
curl -s http://127.0.0.1:7777/api/v1/runs/companion-demo-run/events | jq

# List all artifacts produced by the run
curl -s http://127.0.0.1:7777/api/v1/runs/companion-demo-run/artifacts | jq
```

### Scrape Prometheus Metrics

The server also exposes a standard `/metrics` endpoint for monitoring.

```bash
curl -s http://127.0.0.1:7777/metrics
```

This includes run counters, step duration histograms, and more.

## 4. Read Step Outputs in the Browser

Open `http://127.0.0.1:7777/inspect/runs/companion-demo-run?tab=steps`.
Each step has an expandable **Latest output preview** beneath its row, with
execution, recorded status, and source attribution. Previews show sanitized
pretty JSON capped at 512 Unicode scalar values; **Preview truncated** appears
only when content was omitted. **View outputs** opens the exact execution for
persisted results, including failed results, or `execution=latest` when no result
exists. A newer in-flight attempt never displays an earlier result. Nested runs
show their own previews and outputs on their existing run and step pages.
Previews use one step-list query for the displayed run, with no per-step detail
queries; run-list and step-list JSON responses remain compact.

Open `http://127.0.0.1:7777/inspect/runs/companion-demo-run/steps/hello`.
The **Outputs** section shows persisted plugin output using its original field
names. For example, a model result's `response_text` appears as readable,
multiline text. Output is generic data: HTML, script tags, and Markdown are
escaped text, never rendered markup.

Use the **Output execution** GET form to select `latest` (the default) or a
recorded ordinal. Each selection shows its own execution, status, and source;
a failed historical result remains readable even after a successful retry.
**Latest step input summary** and **Latest step output summary** describe the
current step snapshot, independently of the selected output.

Object fields are alphabetically ordered. Nested objects and arrays expand as
formatted JSON; root values use the same rendering rules. Empty strings appear
as `""`, and null, empty objects/arrays, booleans, and numbers remain explicit.
Pending or newer in-flight attempts show **No persisted result for this
execution.** rather than an earlier result. Matching legacy state results remain
inspectable when an execution record is unavailable.

Host paths, including nested values and path-shaped keys, are sanitized using
the API transport rules before formatting. Valid HTTP(S) URLs retain their
existing behavior. When sanitized pretty JSON exceeds 65,536 bytes, the page
shows its first 65,536 bytes, cut at a valid UTF-8 boundary, and **Output
truncated**. **Complete sanitized detail JSON** links to the full value with
the exact same execution selector:

```bash
curl -s 'http://127.0.0.1:7777/api/v1/runs/companion-demo-run/steps/hello?execution=1' | jq .output
```

The separate **Artifact/evidence iteration filter** only filters artifacts and
evidence. Changing the output selection preserves `iteration`; changing the
iteration filter preserves the output selection. Workflow-file links continue
to reflect the latest step snapshot. These controls work without JavaScript,
and inspection GET requests never dispatch work or rewrite run data.

## A Read-Only Boundary

It is critical to understand that the companion server **cannot change anything**. It will **not**:

- Run, resume, or replay a workflow.
- Approve or reject a manual gate.
- Modify plugins, secrets, or configuration.

If you need to control execution, you must use the `transist` CLI. This clear separation of concerns ensures that inspection is always a safe, read-only operation.

## Next Steps

- [**Companion Server API Reference**](../../reference/companion-server-api.md): The full API contract.
- [**Metrics Reference**](../../reference/metrics.md): Details on all exported Prometheus metrics.
- [**CLI Inspection Guide**](inspecting-runs-artifacts-evidence-and-events.md): A refresher on the CLI-first inspection path.
