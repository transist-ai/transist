# The Workflow Lifecycle

Running a workflow in Transist isn't just one command; it's a lifecycle that ensures everything is ready before the first action is taken.

## The Standard Path

For most situations, you'll follow this simple path:

1. **Check**: `transist workflows check --path <path>`
    * Checks for syntax errors and structural issues.
2. **Install**: `transist workflows install --path <path>`
    * Saves the workflow into your local registry as a "recipe."
3. **Run**: `transist workflows run <id@version> --run-ref <run-id>`
     * Starts the run immediately. Omit `--run-ref` to let Transist generate a run ID.

For governed delivery, validate authored source, package and sign outside the
runtime, install the package, check the installed workflow with actual inputs,
authorize only exact missing authority, check again, then prepare or run.
Validation never starts a run; preparation can still fail on live prerequisites,
secret availability, or external state.

Ordinary installed checks and authorization perform probe-free validation.
Passing required validation leaves readiness `not_ready`; warnings and incomplete
checks block success. Use `check --live` or `--output` for live readiness.
Applying grants reevaluates current inputs/resources/policy and compares the
exact reviewed fragment; drift aborts without writes for another review.

The governed publishing sequence is:

```bash
transist workflows check --path workflow.yaml
transist-admin workflow package workflow.yaml --version 0.1.0 \
   --private-key-file /secure/trust/acme.key --output package-v1
transist workflows install --package package-v1
transist workflows check <id>@0.1.0 --inputs-file inputs.json
transist workflows authorize <id>@0.1.0 --input repo_path=/path/to/repo
transist workflows check <id>@0.1.0 --inputs-file inputs.json
```

Installation persists the verified package only. It neither binds inputs nor
publishes authority; authorization creates a no-clobber exact policy fragment.
`check` and blocked authorization return `run_created: false`.
Both commands accept repeatable `--input key=value` arguments or an
`--inputs-file`; authorize the same prospective inputs that were checked.

## The "Careful" Path (Prepare then Run)

Default and piped `check`/`authorize` output is concise. Request full JSON
explicitly for automation; JSON authorization never prompts and requires
`--approve` to apply grants noninteractively:

```bash
transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
```

Use `check --output readiness.json` to retain full canonical evidence for
`prepare --readiness-file readiness.json`. Stdout remains concise, using the
summary collected during the same evaluation.

If you're running a complex or expensive workflow (like one that uses costly AI tokens or modifies production data), you might want to split the run phase:

1. **Prepare**: `transist workflows prepare <id@version> --run-ref <run-id>`
    * Creates the run record and checks that all inputs are valid, but **does not start execution**.
2. **Preflight**: `transist runs preflight <run-id>`
    * Checks that all plugins are ready, APIs are reachable, and secrets are available.
3. **Run**: `transist runs start <run-id>`
    * Actually starts the work.

## Humans in the Loop (Gates)

If your workflow includes a "Manual Gate" (a step where a human needs to approve something), the run will stop and wait. You can then use these commands to move forward:

* `transist runs gate approve <run-id> --actor "Reviewer <reviewer@example.com>" --reason "Reviewed output and approved"`: Proceed to the next step.
* `transist runs gate reject <run-id> --actor "Reviewer <reviewer@example.com>" --reason "Stop the run"`: Stop the run entirely.
* `transist runs gate request-changes <run-id> --actor "Reviewer <reviewer@example.com>" --reason "Needs another pass"`: Ask for changes before proceeding.

## Getting More Information

As a run progresses (or after it finishes), you can use these tools to see what's happening:

* `transist runs show <run-id>`: See the high-level status (Which steps finished? Which failed?).
* `transist runs tail <run-id>`: See the live stream of events.
* `transist runs watch <run-id>`: Open a fullscreen live viewer to watch the run in real-time.

---

### Related

* [Installation and Local Setup Guide](../../guides/getting-started/installation-and-local-setup.md)
* [Inspecting Runs, Artifacts, Evidence, and Events](../../guides/inspection/inspecting-runs-artifacts-evidence-and-events.md)
* [CLI Reference](../../reference/cli.md)

**Navigation:**

* Previous: [Your First Workflow](02-your-first-workflow.md)
* Next: [First Run Inspection](04-first-run-inspection.md)
