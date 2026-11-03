# Calling Tools (Invoke Steps)

An **Invoke Step** is the primary way Transist gets work done. It tells the runner to reach out to a specific tool, perform an action, and record the results.

## The Anatomy of an Invocation

Every invoke step declares one **`uses`** value in the form
`<adapter>:<plugin>/<action>`. The workflow's root `plugins` map pins the
plugin to one exact SemVer version.

Example:

```yaml
plugins:
  shell: 0.1.0

steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["Hello from Transist!"]
```

Never use a version range, `latest`, or an omitted dependency version. The
compiler verifies the declared version, action, and adapter against the
installed plugin manifest before compilation.

## The `with` Block

The `with` block is where you pass parameters to the plugin. Each plugin has its own set of fields it expects. You can use workflow inputs here:

```yaml
  with:
    ticket_id: "{{inputs.jira_ticket}}"
```

### Passing Data Between Steps

Choose the declared data contract appropriate to the value:

**1. Direct typed outputs** — pass a successful producer's JSON value directly
to the next action, without copying it into a variable.

```yaml
plugins:
  my-plugin: 0.1.0

steps:
  - id: fetch_count
    uses: process:my-plugin/query

  - id: report
    uses: process:my-plugin/write
    with:
      row_count: "{{steps.fetch_count.out.count}}"
```

Whole-value references preserve JSON types; interpolation accepts scalars only.
References are recipe-local. Modules export named `outputs`, read by callers as
`{{steps.module_call.out.export_name}}`. Missing/skipped/failed or stale loop
outputs fail before consumption. `deferred_to_dispatch` findings identify checks
that must wait for runtime data, not successful validation. Keep authority and
secret references birth-known. Use run-scoped `set`/`vars` for intentional mutable
state, such as counters. See [Direct Step Output Bindings](../../reference/workflow-schema.md#direct-step-output-bindings).

**2. Managed documents** — bounded root files/trees for intermediate content
shared across steps and modules, outside Git:

```yaml
documents:
  draft: {kind: file, max_bytes: 65536}
  ledger: {kind: file, max_bytes: 4096, initial: '{}'}
  attachments: {kind: tree, max_bytes: 1048576, max_files: 16}
```

Inside a supported process action's path field, use
`{$document: draft, access: write}`, then `read` to consume its committed revision
or `update` to start from a private writable copy. Read/update require published
or literal initial content. The action must accept absolute paths outside the
workspace. HTTP/MCP document paths are unsupported. Successful executions publish
all outputs atomically; failures publish none. Resume keeps revisions; replay
starts a fresh namespace. Documents are not secret stores.

Maintained delivery flows use these documents for operational handoffs and keep
intended source/OpenSpec deliverables in the workspace. Other workflows may
explicitly choose mutable workspace files, but those are not automatically
captured, reviewed or rolled back. See [Managed Document Paths](../../reference/path-and-resource-semantics.md#managed-document-paths).

## Path Conventions

When working with files, distinguish runtime capability resources from
action-specific conventions. See [Path and Resource Semantics](../../reference/path-and-resource-semantics.md)
for the complete model.

* **Workspace-relative paths**: Process actions run in the declared canonical workspace. Use supported relative paths for intended repository deliverables. Other mutable workspace-file sharing is an explicit workflow choice, not managed persistence. `workspace.access: read_only` permits reads but denies mutations. `workspace:///absolute/root` is the runtime capability resource, not a universal action-input URI.
* **`run://`**: Is an action convention when supported by a plugin. In the bundled shell plugin, it is *invocation-private scratch* removed after the invocation, never durable data for a later step.
* **`runtime://temp`**: Is not a workflow path scheme. Plugin authors may use it
  only as a complete process/MCP `entrypoint.env` value to declare a private
  provider temporary directory. It is separate from `run://`, workspace files,
  artifacts, and evidence, and it never grants a workflow access to a host path.
* **`repo://`**: Is an action convention for repository-local operational files. In the bundled shell plugin it requires `repo_path` and resolves under `<repo_path>/.transist/`. Do not use it to select a second repository or infer workspace authority from CWD.
* **`source://`**: Is a shell-plugin-local reference to contained helper scripts alongside the workflow definition. Snapshot-backed runs use captured helper bytes during dispatch and re-entry; legacy records retain authored-source behavior. Use literal, contained paths only; traversal, missing files, and symlink escapes are rejected.

The workflow source is package/trust input, not workspace authority. The runtime
generates exact filesystem capabilities from the declared workspace; policy must
grant them. A source path, worktree path, or guessed parent directory is not a
substitute. Use exact input-bound `read_allow_paths` only for read-only
secondary context.

## AI-Powered Steps (Prompts and Skills)

Some plugins are designed to work with AI models. Transist provides first-class fields to make this easier:

### `prompt` and `prompt_file`

Instead of hiding your AI instructions inside a complex `with` block, you can use these fields directly:

```yaml
plugins:
  codex.cli: 0.1.0

steps:
  - id: summarize
    uses: process:codex.cli/exec_task
    prompt: "Summarize this ticket: {{inputs.jira_ticket}}"
```

### `skills`

You can provide model-backed steps with a "Skill": a collection of specialized knowledge or instructions that help the execution participant perform better. Transist snapshots these skills when the workflow is compiled, so the run has stable context.

Do not put credentials or other secrets in `prompt_file`, `source://` helpers, or
skills. Prompt and helper files may be captured into private run storage during
fresh run birth.

## AI Controls (Model and Reasoning)

If a plugin supports AI, you can usually control its behavior inside the `with` block:

* **`model`**: Which AI model to use (e.g., `gpt-4o`, `claude-3-5-sonnet`).
* **`reasoning_effort`**: How much "thought" the model should put in (choices: `low`, `medium`, `high`, `xhigh`).

---

### Related

* [Advanced Workflow Authoring Guide](../../guides/authoring/advanced-workflow-authoring.md)
* [Plugin Packaging and Installation](../../guides/plugins/plugin-packaging-install-upgrade-removal.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

* Previous: [Inputs](02-inputs.md)
* Next: [Control Steps](04-control-steps.md)
