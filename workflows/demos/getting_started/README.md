# Getting Started Demos

Eleven small, ordered demos. Each uses only the bundled `shell` plugin and runs
locally without external services or network access.

Every workflow in this bundle ships with automatic hermetic execution coverage:
CI executes each one to its declared terminal result via
`scripts/ci/run_shipped_workflows.sh` (from the source checkout and again from
the extracted release archive). Adding a new workflow requires declaring its
plan and expected result in `scripts/ci/run_shipped_workflows.py`; a shipped
workflow without coverage fails CI. See
[CONTRIBUTING.md](../../../CONTRIBUTING.md#shipped-workflow-coverage).

## Prerequisites

```bash
transist home init --mode standalone
transist plugins install --path plugins/shell/plugin.yaml
```

Run commands from the demo bundle root or source checkout root.

## The Roadmap

1. **[`01_hello_world`](01_hello_world/README.md)** — the smallest deterministic workflow.
2. **[`02_review_with_manual_gate`](02_review_with_manual_gate/README.md)** — multi-step run producing artifacts and evidence.
3. **[`03_resume_after_failure`](03_resume_after_failure/README.md)** — fail a run, fix the cause, continue with `runs resume`.
4. **[`04_replay`](04_replay/README.md)** — replay a completed run from its persisted recipe and inputs.
5. **[`05_artifacts_and_evidence`](05_artifacts_and_evidence/README.md)** — focused shell helpers, shared workspace artifacts, and step-scoped evidence retained with the run.
6. **[`06_loop_until_condition`](06_loop_until_condition/README.md)** — `control.loop` with a body and an `until` exit condition.
7. **[`07_subflow_basics`](07_subflow_basics/README.md)** — call a cohesive package-local module with its own helper and read its workspace artifact.
8. **[`08_manual_gate`](08_manual_gate/README.md)** — a tiny manual gate workflow with before and after steps.
9. **[`09_cleanup_branches`](09_cleanup_branches/README.md)** — success, canceled, and failed cleanup branches.
10. **[`10_secret_env`](10_secret_env/README.md)** — end-to-end secret delivery into a shell step.
11. **[`11_filesystem_policy`](11_filesystem_policy/README.md)** — workspace-scoped local filesystem policy in `full` and `read_only` modes, external path grants, and `custom`-mode rejection.

## Organizing Larger Examples

Keep the smallest demos inline. As a workflow grows, keep orchestration and
explicit plugin calls in YAML, group cohesive steps into modules, and extract
substantial shell bodies into helpers. Demos 05 and 07 illustrate root and
module-local `scripts/` layouts. Literal `source://` helper paths resolve relative
to the declaring YAML; there are no implicit imports or sibling-file captures.
Workspace artifacts remain shared, while evidence stays step-scoped.

For governed execution, capture and sign the declared package assets with
`transist-admin`, install exact plugin dependencies, and authorize exact requested
capabilities rather than granting access to the source checkout. See the
[Workflow Patterns Cookbook](../../../docs/public/guides/authoring/workflow-patterns-cookbook.md#organizing-yaml-modules-and-helpers).
