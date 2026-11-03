# Inspecting Your First Run

After your workflow finishes, Transist does not just leave you with a `success` status. It provides persisted run records that an operator or workflow author can inspect.

## 1. High-Level Status

Start by looking at the general summary of the run:

```bash
transist runs show <RUN_ID>
```

This shows you:

* **Run Status**: Did it succeed, fail, or is it blocked by a gate?
* **Step Summary**: Which steps finished, which were skipped, and which one (if any) caused a failure.
* **Metadata**: Information about the version of the workflow used and the inputs provided.

## 2. The Play-by-Play (Events)

To see exactly what happened and in what order, look at the event stream:

```bash
transist runs tail <RUN_ID> --limit 20
```

This is the runtime event stream. Every time a step starts, finishes, or produces a result, an event is recorded.

## 3. Artifacts and Evidence

Transist separates the *outcome* of a step into two categories:

* **Artifacts**: The final products (e.g., a generated report, a compiled file).
* **Evidence**: Supporting data (e.g., the raw JSON response from an API, the exact command sent to a shell).

These are stored in your Transist Home under:
`runs/run_<RUN_ID>/artifacts/<STEP_ID>/execution_<NNN>/`

*Note: The `execution_<NNN>` folder ensures that if you retry a step, the new results don't overwrite the old ones.*

## 4. The "Recipe" and Inputs

For every run, Transist saves a snapshot of the exact instructions it followed and the inputs it used:

* `recipe.yaml`: The compiled workflow.
* `context/run_inputs.json`: The exact data you provided when starting the run.

These files are what allow Transist to **Resume** or **Replay** from persisted runtime context. External tools, APIs, models, and time-dependent side effects remain outside Transist's determinism guarantees.

---

### Related

* [Inspecting Runs, Artifacts, Evidence, and Events](../../guides/inspection/inspecting-runs-artifacts-evidence-and-events.md)
* [Storage Layout Reference](../../reference/storage-layout.md)
* [Companion Server API](../../reference/companion-server-api.md)

**Navigation:**

* Previous: [Running a Workflow](03-running-a-workflow.md)
* Next: [Workflow File Structure](../02-everyday-usage/01-workflow-file-structure.md)
