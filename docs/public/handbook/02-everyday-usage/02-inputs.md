# Passing Data with Inputs

Inputs are how you feed data into your workflow. They allow you to write a single workflow and use it for many different situations just by changing the starting values.

## Defining Inputs

Installed `workflows check` and authorization validate each invocation's inputs
and exact resources without plugin probes. Passing validation is not live
readiness; use `--live` or `--output` for that scope. Applying grants revalidates
the reviewed exact fragment against current inputs/resources/policy and aborts
without writes on drift. Preparation still checks live readiness before birth.

You declare your inputs at the very top of your workflow file:

```yaml
inputs:
  jira_ticket:
    type: string
    required: true
  dry_run:
    type: boolean
    default: false
```

### Supported Types

* `string`
* `number`
* `boolean`
* `object` (a nested set of key-value pairs)
* `array` (a list of values)

### Rules

* **`required: true`**: The workflow won't start unless this value is provided (or a default exists).
* **`default`**: A fallback value if no input is provided when the workflow runs. It must match the declared type.

The same binding rules apply when a workflow is used as a subflow: the child workflow's
declared defaults are materialized before the child run starts, and the child run's
`context/run_inputs.json` records those bound values.

Optional inputs without a default are still bound as `null`. That binding is durable and
inspectable, but a nested invoke step may still fail later if its own request schema rejects
that `null` value.

The persisted `context/run_inputs.json` includes a `sources` map that labels each input
value as `caller_provided`, `default_materialized`, or `unset_null` to help auditors
distinguish caller intent from runtime binding behavior.

### `repo_path` as Workspace Input

New repository workflows should declare a required string `repo_path` and bind
it exactly as `workspace.path: "{{inputs.repo_path}}"`. That immutable input is
resolved and canonicalized at fresh run birth, then persisted for preflight,
dispatch, cleanup, retry, resume, and replay. Changing it requires a fresh
run, not a changed CWD or replay. Secondary repository inputs are not
workspaces: use exact existing paths only as input-bound read-only external
context.

## Using Inputs in Steps (Placeholders)

Once you've defined an input, you can use it in your steps using the "double curly brace" syntax: `{{inputs.<name>}}`.

```yaml
- id: my_step
  ...
  with:
    ticket_id: "{{inputs.jira_ticket}}"
```

### Where can you use placeholders?

* In the `with` block of an **Invoke** step.
* In the `prompt` or `prompt_file` fields for AI-powered steps.
* In the input mappings for a **Subflow**.

### Passing a previous step's output

Use `{{steps.step_a.out.field}}` directly in invoke `with`, inline prompts or
module inputs. Whole-value references preserve JSON types, including objects,
arrays, Booleans and null; interpolation in larger strings accepts scalars only.
`{{steps.step_a.output}}` is not the supported syntax. A module exposes named
`outputs`; its caller reads `{{steps.module_call.out.export_name}}`, not internal
child step IDs. Keep `set`/`vars` for intentional mutable state such as counters.

Missing, skipped, failed or previous-iteration producer outputs fail before
consumer dispatch. Prospective `deferred_to_dispatch` findings describe checks
that need runtime data; they are not completed validation. Authority and secret
references must remain birth-known.

For intermediate files/trees, declare bounded root `documents` and use process
action path bindings such as `{$document: notes, access: write}` followed by
`read`. The action must support absolute paths outside the workspace; HTTP/MCP
document paths are unsupported. Keep intended source/OpenSpec deliverables in the
workspace. See [Direct Step Output Bindings](../../reference/workflow-schema.md#direct-step-output-bindings)
and [Path and Resource Semantics](../../reference/path-and-resource-semantics.md).

## Providing Inputs at Runtime

When you're ready to run your workflow, you can provide inputs through the CLI in two ways:

1. **Directly**: Use the `--input` flag.

    ```bash
    transist workflows run my-flow@0.1.0 --run-ref my-run --input jira_ticket=PROJ-123 --input dry_run=true
    ```

2. **File**: Provide a JSON or YAML file with all your inputs.

    ```bash
    transist workflows run my-flow@0.1.0 --run-ref my-run --inputs-file ./inputs.json
    ```

## The Paper Trail

When a run starts, Transist saves the exact inputs you used to a file in the run folder:
`<transist-home>/runs/run_<RUN_ID>/context/run_inputs.json`

This ensures that if you ever need to **Replay** the run, you can do so with the exact same data.

---

### Related

* [Advanced Workflow Authoring Guide](../../guides/authoring/advanced-workflow-authoring.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)
* [CLI Reference](../../reference/cli.md)

**Navigation:**

* Previous: [Workflow File Structure](01-workflow-file-structure.md)
* Next: [Invoke Steps](03-invoke-steps.md)
