# Workflow File Structure

Before we dive into specific actions, let's look at the basic "skeleton" of a Transist workflow file. Understanding this top-level structure will help you keep your workflows organized as they grow.

## The Top-Level Skeleton

Every Transist workflow (version `v1`) follows this basic shape:

```yaml
schema: transist.flow.v1
id: example-workflow
version: 0.1.0

# Data required to start the run
inputs:
  repo_path:
    type: string
    required: true

# One declared execution workspace
workspace:
  path: "{{inputs.repo_path}}"
  access: full # or read_only

# The main sequence of work
steps: []

# Optional: Actions to take after the work is done (success or failure)
cleanup: []
```

### Key Fields

* **`schema`**: Tells Transist which version of the runner logic to use.
* **`id` & `version`**: Used to identify and version your workflow in the registry.
* **`inputs`**: Where you define what information (like a ticket ID or a file path) is needed to start.
* **`workspace`**: The one canonical directory where the workflow runs. Its
  `path` is an absolute literal or exactly a required string input reference;
  `full` permits mutation and `read_only` denies it.
* **`steps`**: An ordered list of everything you want Transist to do.
* **`cleanup`**: A special block for "teardown" tasks, such as deleting temporary
  files or sending a final notification. Every declared entry runs automatically
  when its `when` matches the root outcome (`success`, `canceled`, or `failed`).
  No matching branch means no action; failure does not imply canceled cleanup.

The workflow source directory and the directory where you invoke `transist` are
not the declared workspace and grant no workspace authority. The runtime
canonicalizes and persists `workspace.path` at run birth. Exact generated
capabilities still need operator approval. A workflow without `workspace` is a
legacy shape and uses the CLI-directory fallback; do not rely on that fallback
for new workflows.

## What's in a Step?

The `steps` block is where the real work happens. Every step in the list must have a unique `id` and must choose **exactly one** of these three forms:

1. **Invoke**: Calls an external tool (e.g., "Run a shell script").
2. **Control**: Manages the flow of the workflow (e.g., "Wait for approval" or "Start a loop").
3. **Subflow**: Runs another entire workflow as a single step.

**Important:** You cannot mix these forms in a single step. A step is either an invocation, a control point, or a subflow.

## Authoring vs. Reality (Compiled Recipes)

When you write a workflow in YAML, you're creating the **Authored Flow**. This is designed to be human-readable and easy to edit.

When you run `transist workflows install`, Transist turns that YAML (or a governed package) into a **Compiled Recipe**. This is a flattened, machine-optimized version that the runner actually uses. In governed homes, installation persists the signed package only; readiness and authority are checked separately.

*Tip: If you see older documentation showing complex, nested "execution" blocks in the YAML, those are likely outdated. Modern Transist workflows use the flat, clean structure shown above.*

---

### Related

* [Advanced Workflow Authoring Guide](../../guides/authoring/advanced-workflow-authoring.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

* Previous: [Inspecting Your First Run](../01-first-steps/04-first-run-inspection.md)
* Next: [Inputs](02-inputs.md)
