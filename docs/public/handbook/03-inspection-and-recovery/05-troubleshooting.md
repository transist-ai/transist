# Troubleshooting

When a workflow doesn't behave as expected, Transist provides a structured way to find and fix the problem. Follow these steps to diagnose issues at different stages of the lifecycle.

## 1. Validation Failures (Before you run)

If `transist workflows check --path <workflow.yaml>` fails, it's usually a structural issue in your YAML file:

* **Wrong Schema**: Ensure your file starts with `schema: transist.flow.v1`.
* **Missing Fields**: Every step needs an `id` and a clear type (Invoke, Control, or Subflow).
* **Mixed Shapes**: Remember, a step can only be one thing. You can't put `control: control.noop` and `uses: process:shell/exec` in the same step.
* **Branching Errors**: Ensure `if` and `loop` blocks are only attached to `control.noop` steps.
* **Bound Inputs**: Use `workflows check <id@version> --inputs-file <path>` to find action-schema, workspace, and policy blockers without creating a run. Checks marked runtime-only require `prepare`; they are not proof that a remote branch, service, secret, or process will be available later.

Ordinary installed checks and authorization perform probe-free validation.
Passing validation leaves readiness `not_ready`; inspect `validation_status`
and `evaluation_mode`. Use `check <id>@<version> --live` or `--output` to
diagnose live plugin readiness. Warnings and incomplete required validation
block success. Applying grants reevaluates the exact reviewed fragment and
destination; changed authority aborts without writes and requires another review.

All `check` modes show grouped actionable findings by default, including in
pipes; passing checks are counted without retaining their records. A blocked `authorize` also
shows why it cannot offer grants. Fix any input or action-schema errors before
reviewing missing authority. An action-schema path such as `/issue_key` names
an action field, not automatically the corresponding workflow input: inspect
the workflow binding. `authorize` does not write policy or create a run while
non-authority failures remain. When grants are also missing, fix the other
blockers, then rerun `workflows authorize` with corrected inputs and the same
`--input` overrides. Once those other failures are fixed, review the
exact grant fragment and destination before confirming. Grants go into the
effective home's policy, not the workflow definition.

Summary checks retain required-input name hints. When a declared required string
input is present but empty after `--input` overrides, full JSON names it under
`diagnostics.inputs.empty_required`. For example, an empty
`jira_ticket_id` may explain `/issue_key` action-schema failures, but confirm
that relationship in the workflow binding. No input value is printed.

The TUI launch validation view uses the same grouped text as the CLI, including
missing or empty required input names such as `repo_path`. Return to the
inputs editor to supply an absolute workspace path; a cleared required field
from a loaded file is rejected before validation. Passing launch review proceeds
to live preparation; it is not proof that a run can be born.

Use `--format json` to search the complete report, including repeated step
paths. The additive `diagnostics.groups` index points into the unchanged
`checks[]` array; `diagnostics.authorization.blocking_codes` lists non-policy
failures preventing a grant review:

```bash
transist workflows check <id>@<version> --inputs-file inputs.json --format json \
  | jq '.checks as $checks | .diagnostics.groups.inputs.check_indices[]? | $checks[.]'
```

Piped output defaults to concise text; JSON parsers must use `--format json`.
`--output readiness.json` writes complete canonical evidence while stdout stays
concise. A failed `check` returns nonzero even when its JSON can be filtered.

```bash
transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
```

## 2. Plugin Issues (Ready to start?)

If your workflow validates but fails immediately when run, check your plugin setup:

* **Not Installed**: Run `transist plugins list` to ensure the required plugin is in your Transist Home.
* **Wrong Reference**: Double-check that your `adapter`, `plugin`, and `action` names match what's in the plugin's documentation.
* **Preflight Check**: Run `transist runs preflight <run-id>` to see if Transist can reach the tool, resolve your secrets, or find your API keys.

## 3. Runtime Failures (During execution)

When a step fails while the workflow is running, use this diagnostic routine:

1. **Check the Status**: `transist runs show <run-id> --failed`. This tells you exactly which step failed and why.
2. **Look at the Proof**: Inspect the **Evidence** for the failing step. This often contains the raw error message from the tool or API that Transist called.
3. **Check the Timeline**: Use `transist runs tail <run-id>` to see the events leading up to the failure.

## 4. Common Misunderstandings

### "My changes aren't taking effect!"

If you edit your workflow file and then run `resume` or `replay`, you won't see your changes. Both commands are "locked" to the version of the workflow that existed when the run was first created. To use your new instructions, you must start a **Fresh Run** with `transist workflows run`.

The same applies to a declared workspace: the run persists its canonical
`repo_path`. Changing CWD, granting a parent directory, or selecting a worktree
does not remediate a workspace mismatch. Start a fresh run with the intended
input. A legacy workflow without `workspace` is the only case that retains the
CLI-directory fallback.

### "The run is stuck!"

Check if the run is **Blocked**. This usually means it has reached a **Manual Gate** and is waiting for you to `approve` it. Use `transist runs status <id>` to check for active gates.

### "I can't resume this run anymore."

If you have already performed a **Cleanup** on a failed run, it is marked as "terminated" and can no longer be resumed. You'll need to **Replay** it or start a fresh run instead.

### "Run failed at preflight."

Inspect `transist runs show <run-id>` and look at `preflight.checks[]`:

* `asset_lock.plugin`: the pinned installed plugin bytes changed or the pinned version is no longer installed.
* `asset_lock.prompt_file`: a locked `prompt_file` was deleted or its bytes changed since the run was created.
* `trusted_execution`: the package signature or an imported public trust root no longer verifies. Do not repair it by re-signing from the runtime; inspect the package and use a fresh authorized promotion.
* `workflow_assets`: inspect the captured-asset manifest and blob digests. A missing manifest or blob is a persisted-run integrity failure; restore it from a trustworthy backup or use a fresh run. Legacy records can still require authored-source recovery.
* Parent-prefixed `step_ids` like `plan_loop/generate_plan`: a reachable nested subflow step failed preflight, and the child blocker was surfaced on the root run.
* `deferred_to_dispatch` findings on a nested step: declared data depends on outputs
  unavailable at preparation. Static graph, known payload and authority checks
  still run; this is not skipped or successful complete validation. The runtime
  resolves the dependency and validates the full materialized request before the
  consumer's probes or external work. Missing grants and invalid known fields
  still block preparation.
* `subflow_preflight.cycle_detected` or `subflow_preflight.depth_exceeded`: recursive preflight stopped walking a nested subflow tree because it would recurse cyclically or exceed the nesting cap.
* `capability_not_granted`: run `transist runs capabilities <run-id> --format yaml`.
    It prints a copyable named `grant_sets` entry and exact `workflow_grants`
    binding; add both under `policy` in the effective home `config.yaml`.
    `policy.allow` is global and additive, so use it only for intentionally
    global authority. Use `--exact` only when inspecting normalized requests.

For `runs preflight`, these appear as structured failed checks instead of a single opaque command error. For `run`, `resume`, and `replay`, the runtime still stops immediately when the same drift is detected.

If Transist says the run was created before run asset locking support, that run
is a legacy run from before persisted re-entry locks existed. Recompile the
workflow and start a fresh run instead of resuming or replaying the legacy one.

---

### Related

* [Inspecting Runs, Artifacts, Evidence, and Events Guide](../../guides/inspection/inspecting-runs-artifacts-evidence-and-events.md)
* [CLI Reference](../../reference/cli.md)
* [Error Taxonomy Reference](../../reference/error-taxonomy.md)

**Navigation:**

* Previous: [Resume and Replay](04-resume-and-replay.md)
* Next: [Determinism and Auditability](../04-trust-and-runtime-behavior/01-determinism-and-auditability.md)
