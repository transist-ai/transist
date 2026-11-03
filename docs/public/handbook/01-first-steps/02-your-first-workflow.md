# Your First Workflow

This page is for workflow authors learning the smallest useful `transist.flow.v1` shape. The example is intentionally minimal so the file structure is clear.

> **Want to run this right now?** Follow the [Quickstart Guide](../../guides/getting-started/quickstart.md).

For a complete, runnable multi-step tutorial with typed inputs, a direct JSON
output handoff, a manual gate, and a final echo, follow
[Build Your First Real Workflow](../../guides/authoring/advanced-workflow-authoring.md).

## A Simple Example

Create a file named `workflow.yaml` with the following content:

```yaml
schema: transist.flow.v1
id: hello_world
version: 0.1.0
plugins:
  shell: 0.1.0

inputs:
  message:
    type: string
    required: true

steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["{{inputs.message}}"]
```

## Anatomy of a Workflow

* **`schema`**: Always `transist.flow.v1` for current workflows. This tells Transist how to read the file.
* **`id` & `version`**: Unique identifiers for your workflow.
* **`plugins`**: Exact plugin versions required by the workflow, declared at the root alongside `inputs` and `steps`.
* **`inputs`**: The data your workflow needs to start. Here, we require a string called `message`.
* **`steps`**: An ordered list of actions to take.
  * **`id`**: A name for this specific step.
  * **`uses`**: The adapter, plugin, and action to invoke (here, `process:shell/echo`).
  * **`with`**: The inputs for this specific action. We use `{{inputs.message}}` to pass in the message we provided at the start.

## From Code to Run

To actually use this workflow, you follow a three-step process:

### Standalone or Demo Workflow

For a local standalone workflow or a bundled demo, check, install, and
run directly:

1. **Validate:** Check for typos or structural errors.

    ```bash
    transist workflows check --path workflow.yaml
    ```

2. **Install:** Turn your YAML into a "recipe" that Transist can execute.

    ```bash
    transist workflows install --path workflow.yaml
    ```

3. **Run:** Start the run and provide the inputs.

    ```bash
    transist workflows run hello_world@0.1.0 --run-ref hello-run --input message="Hello, Transist!"
    ```

### Governed Workflow

For regular governed delivery, create a signed package outside the runtime,
then let the runtime verify and promote that package:

1. Validate the authored workflow.

   ```bash
   transist workflows check --path workflow.yaml
   ```

2. Package and sign it with `transist-admin` on an authorized administrative
   machine.

   ```bash
   transist-admin workflow package workflow.yaml --version 0.1.0 \
      --private-key-file /secure/trust/acme.key --output package-v1
   ```

3. Install the signed package. Installation verifies and persists immutable
   workflow bytes only; it does not bind inputs, grant authority, or create a
   run.

   ```bash
   transist workflows install --package package-v1
   ```

4. Check the installed workflow with the actual prospective inputs. When it
   reports only missing authority, review and apply the exact authorization,
   then check again.

   Ordinary installed checks and authorization validate without plugin probes;
   a passing `validation_status` leaves readiness `not_ready`. Use `--live` or
   `--output` for live readiness. Preparation still performs live checks.
   Applying reevaluates current inputs/resources/policy and aborts without writes
   if the exact reviewed fragment changed; review again.

   Create `inputs.json` with the input required by this workflow:

   ```json
   {"message":"Hello, Transist!"}
   ```

   ```bash
   transist workflows check hello_world@0.1.0 --inputs-file inputs.json
   transist workflows authorize hello_world@0.1.0 --inputs-file inputs.json
   transist workflows check hello_world@0.1.0 --inputs-file inputs.json
   ```

   `authorize` supports both repeatable `--input key=value` arguments and
   `--inputs-file`, just like `check`; bind the same prospective invocation.

5. Prepare and preflight a run before explicitly starting it.

   ```bash
   transist workflows prepare hello_world@0.1.0 --run-ref hello-run \
     --inputs-file inputs.json --trusted-execution
   transist runs preflight hello-run
   transist runs start hello-run
   ```

The runtime verifies the package and enforces policy; it does not use or create
the private signing key. Prospective `check` and `authorize` diagnostics always
return `run_created: false`; preparation does not dispatch workflow actions.

## What Happens Behind the Scenes?

`check` and `authorize` display concise summaries by default, even in a pipe.
Use `--format json` for full diagnostics or scripts that parse JSON:

```bash
transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
```

`check --output readiness.json` writes full canonical evidence while keeping
stdout concise. Both views reuse one evaluation.

When you run `workflows run`, Transist creates a new folder in your home directory dedicated to this run:
`<transist-home>/runs/run_hello-run/`

Inside, you'll find a complete history of the run, including:

* `state.json`: The current status of the run.
* `recipe.yaml`: A snapshot of the workflow instructions used.
* `events.jsonl`: A play-by-play log of every action.
* `artifacts/`: Accepted files reported by adapters and promoted by the runtime,
  scoped to each step execution.

This run directory does not capture every workspace write. The maintained
delivery workflows use root managed documents for intermediate files/trees,
persisted as immutable revisions outside Git. Intended source/OpenSpec
deliverables remain in the workspace; arbitrary workspace writes are not
automatically captured. See [Managed Document Paths](../../reference/path-and-resource-semantics.md#managed-document-paths).

---

### Related

* [Quickstart Guide](../../guides/getting-started/quickstart.md)
* [Build Your First Real Workflow](../../guides/authoring/advanced-workflow-authoring.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)
* [CLI Reference](../../reference/cli.md)

**Navigation:**

* Previous: [Setting Up Your Environment](01-installation-and-local-setup.md)
* Next: [Running a Workflow](03-running-a-workflow.md)
