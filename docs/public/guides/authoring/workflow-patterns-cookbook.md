# Workflow Patterns Cookbook

This guide is a collection of copy-and-paste examples for the most common and useful patterns in Transist. Use these as a starting point for your own `transist.flow.v1` workflows.

## Workspace Foundation

Start repository workflows with one declared workspace. This requests, but does
not grant, the exact canonical filesystem authority used by every step:

```yaml
inputs:
  repo_path:
    type: string
    required: true
workspace:
  path: "{{inputs.repo_path}}"
  access: full
```

Use `read_only` for inspection-only workflows. Workflow source and CWD are not
workspace authority. For secondary context, use exact input-bound read-only
external paths rather than a second workspace, a worktree, or a parent grant.

## 1. Simple Chain of Steps

The most basic workflow is a straight line. Each step runs after the previous one finishes successfully. This is great for simple, linear processes.

```yaml
plugins:
  shell: 0.1.0
documents:
  collected: {kind: file, max_bytes: 1048576}

steps:
  - id: collect_data
    uses: process:shell/exec
    with:
      command: python3
      args:
        - scripts/collect_data.py
        - --output
        - {$document: collected, access: write}

  - id: transform_data
    uses: process:shell/exec
    with:
      command: python3
      args:
        - scripts/transform_data.py
        - --input
        - {$document: collected, access: read}
```

This fragment assumes the two scripts exist in the declared workspace and accept
ordinary absolute paths outside it. Inspect the selected action's real schema
and bindings before using a document in a path field. Root documents can be
bounded files or trees; trees also require positive `max_files`. `write` starts
an absent output path; `read` copies committed content read-only; `update` starts
from committed content in a writable copy. HTTP/MCP document paths are unsupported.

**Key takeaway:** Use managed documents for intermediate file handoffs and
`{{steps.<id>.out.<field>}}` for ordinary JSON values. Whole-value output references
preserve types; interpolation accepts scalars only. Keep `set`/`vars` for mutable
state. Successful executions publish all document outputs atomically; failures
publish none. Resume retains revisions; replay starts a fresh namespace.
`run://` is invocation-private scratch, never a durable cross-step channel.
Provider temporary directories belong in plugin `runtime://temp` or
`runtime://temp/<relative-path>` manifest declarations, not workflows.
They cannot retain provider credentials, sessions,
or caches between invocations. See [Path and Resource
Semantics](../../reference/path-and-resource-semantics.md).

Maintained delivery examples use documents outside Git for operational
intermediates. Intended source/OpenSpec deliverables stay in the workspace.
Other workflows may explicitly use workspace files, without automatic managed
persistence or rollback. Documents are not secret stores. See
[Managed Document Paths](../../reference/path-and-resource-semantics.md#managed-document-paths).

## 2. Conditional Branching (`if`)

Use an `if` block to change the workflow's path based on an input. This lets you create dynamic workflows that adapt to different situations.

```yaml
- id: check_if_dry_run
  control: control.noop
  if:
    guard: 'inputs.dry_run == true'
    then:
      # If it is a dry run, we jump straight to the 'finish' step.
      - ref: finish
    else:
      # Otherwise, we continue to the 'publish' step.
      - ref: publish
```

## 3. Manual Approval Gate

Need a human in the loop? Use `control.gate.manual` to pause the workflow and wait for someone to approve or reject it before continuing.

```yaml
- id: await_approval
  control: control.gate.manual
  with:
    # This ID is used to identify which gate to approve.
    gate_id: release-review-gate
```

A user would then run `transist runs gate approve <run-id> release-review-gate
--actor <reviewer> --reason <reason>` to unblock the workflow.

For exact document review, add `review_documents: [collected]` to the gate's
`with`, then use a protected read:
`{$document: collected, access: read, reviewed_by: await_approval}`. The reference
names the gate **step ID in the same lexical recipe**, not `gate_id`. Review the
exact revision ID, digest, size and content links. A new revision, even with
identical bytes, needs approval again. Two approvals require two distinct gates;
document review does not approve arbitrary workspace mutations.

## 4. Reusing Logic with Modules

Don't repeat yourself. If you have a set of steps you need to run in multiple places, put it in a package-local `module`. It's like calling a function in a programming language.

```yaml
modules:
  publish:
    path: modules/publish.yaml

steps:
  - id: publish_release
    module:
      ref: publish
      # You can pass inputs to the module, just like a regular workflow.
      inputs:
        repo_path: "{{inputs.repo_path}}"
        issue_ref: "{{inputs.issue_ref}}"
```

The referenced module file declares its required version metadata:

```yaml
# modules/publish.yaml
schema: transist.module.v1
id: publish
version: 0.1.0
steps:
  - id: publish_artifacts
    control: control.noop
```

Modules inherit root documents directly. For JSON data, declare named module
`outputs` and consume `{{steps.publish_release.out.<export>}}`; callers cannot
read the child's internal step IDs. See [module exports](../../reference/workflow-schema.md#3-module-step-package-local-reuse).

### Organizing YAML, Modules, and Helpers

Keep the smallest workflows inline. Extract substantial shell bodies into
helpers and group cohesive steps into package-local modules when that makes
their inputs, outputs, and responsibility easier to review. Orchestration,
branching, approvals, and explicit `adapter:plugin/action` calls stay in YAML;
helpers implement the work of those declared calls.

For example, demo 07 separates root and module-local helpers:

```text
workflow.yaml                     # root orchestration and read step
scripts/read_greeting.sh          # root helper
modules/greet.yaml                # cohesive greeting module
modules/scripts/write_greeting.sh # module helper
```

The root read step uses `process:shell/exec` with `command: sh` and a literal
`source://scripts/read_greeting.sh` argument. The module's write step uses the
literal `source://scripts/write_greeting.sh`, resolved relative to
`modules/greet.yaml`, so it selects `modules/scripts/write_greeting.sh`.
`source://` helpers must be literal, contained regular files relative to their
declaring YAML, not the caller's YAML, workspace, or CWD. There are no implicit
imports or automatic sibling-file captures; explicitly reference each helper
needed by the package.

Demo 05 uses the simpler root layout: `scripts/write_outputs.sh` and
`scripts/read_outputs.sh`. Moving shell bodies out of YAML does not change its
shared workspace artifact handle or make `$TRANSIST_EVIDENCE_DIR` a handoff
channel: evidence remains scoped to each step execution and retained with the run.

For governed execution, capture the declared modules and helpers in the workflow
package and sign the reviewed capture with `transist-admin`. Install that package
and its exact plugin dependencies, then authorize only the exact requested
capabilities. Captured helpers do not require source-checkout filesystem grants;
workspace authority remains separately declared and policy-controlled. See
[Path and Resource Semantics](../../reference/path-and-resource-semantics.md).

## 5. Using Secrets Securely

Never hardcode secrets. Use `with.secret_refs` to tell Transist that your step needs a secret. Transist will fetch it from your configured secret provider and deliver it according to adapter policy.

```yaml
plugins:
  shell: 0.1.0

steps:
  - id: call_secure_api
    uses: process:shell/exec_with_secret_env
    with:
      command: python3
      args: ["scripts/notify_api.py"]
    # Transist will resolve 'API_TOKEN' and make it available to the script,
    # typically as an environment variable, without logging it.
      secret_refs: ["API_TOKEN"]
```

## 6. Outcome-Aware Cleanup Actions

What if a workflow leaves temporary resources behind? Use a top-level `cleanup`
block. Every explicitly declared cleanup runs automatically when its `when`
matches the root outcome (`success`, `canceled`, or `failed`). No matching branch
means no action; failure never implicitly selects cancellation cleanup. The
primary status stays unchanged and cleanup has its own result.

Abrupt process death can prevent cleanup; `transist runs cleanup <run-id>`
recovers unfinished or failed cleanup from durable history. A failed run without
matching cleanup remains recoverable, but once failure cleanup begins primary
retry/resume is blocked; use replay for new primary work.

```yaml
# This block goes at the top level of your workflow file.
cleanup:
  - id: delete_temp_env
    when: [success]
    uses: process:shell/exec
    with:
      command: ./scripts/teardown_env.sh
      args: ["{{inputs.env_name}}"]
```

## 7. Workflow Variables (Inputs Reused Across Steps)

Use top-level `inputs` to declare typed values you want every step to be able
to reference. Within a step, render them with `{{inputs.<name>}}`. Combined
with conditional branching, workflow inputs let one workflow file serve
multiple shapes (e.g. dry-run vs. real, dev vs. prod, summary vs. full).

```yaml
schema: transist.flow.v1
id: vars_example
version: 0.1.0
plugins:
  shell: 0.1.0
inputs:
  environment:
    type: string
    required: true
    description: Target environment (e.g. dev, staging, prod).
  dry_run:
    type: boolean
    default: false
    description: When true, simulate but do not apply.
  retries:
    type: number
    default: 1
steps:
  - id: print_plan
    uses: process:shell/exec
    with:
      command: sh
      args:
        - -c
        - 'printf "env=%s dry_run=%s retries=%s\n" "$1" "$2" "$3"'
        - --
        - "{{inputs.environment}}"
        - "{{inputs.dry_run}}"
        - "{{inputs.retries}}"

  - id: apply_or_skip
    control: control.noop
    if:
      guard: 'inputs.dry_run == true'
      then:
        - ref: finish
      else:
        - ref: do_apply

  - id: do_apply
    uses: process:shell/echo
    with:
      args: ["applying to", "{{inputs.environment}}"]

  - id: finish
    control: control.finish
```

Persisted run inputs are part of every run's record. `transist runs show` and
`transist runs replay` both surface them, which is why replay can rebuild a
historical run without prompting for new inputs.

Fresh snapshot-backed runs retain resolved prompt and shell `source://` helper
bytes in private `workflow_assets/` storage and use those captured bytes for
dispatch and re-entry. Keep those files free of secrets; legacy records retain
their authored-source behavior.
