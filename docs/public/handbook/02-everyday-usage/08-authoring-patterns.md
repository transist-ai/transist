# Authoring Patterns & Best Practices

This chapter summarizes the most effective ways to build workflows in Transist. These patterns are designed to take full advantage of the runner's transparency and auditability.

> **Looking for examples?** Check out the [Workflow Patterns Cookbook](../../guides/authoring/workflow-patterns-cookbook.md) for copy-pasteable snippets.

## 0. Declare One Workspace

For repository work, require `repo_path` and declare it as
`workspace.path: "{{inputs.repo_path}}"`. Choose `full` only when the workflow
must mutate that repository; use `read_only` for inspection-only flows. The
declaration creates exact capability requests, not grants. Keep secondary
repositories as exact, input-bound read-only external context. Do not use
worktree modes, broad parent grants, the workflow source directory, or CWD as
an alternative workspace.

## 1. The Human-in-the-Loop Pattern

Don't let your automation make critical decisions alone. Use a **Manual Gate** to pause the workflow at key points.

1. **Generate**: An AI step creates a draft (e.g., a PR description).
2. **Inspect**: `with.review_documents` captures the exact managed draft revision.
3. **Gate**: Use `control.gate.manual` to wait for approval.
4. **Execute**: A protected read uses `reviewed_by: <gate-step-id>` in the same lexical recipe. Changed revisions require new approval. Two decisions require two distinct gates, not a label saying "two approvals." Document review does not cover workspace mutations.

## 2. The Implementation-Review Loop

For tasks that require refinement (like fixing a bug or writing documentation), use a bounded **Loop**.

* Instead of putting a "retry" loop inside a shell script, use a Transist `loop`.
* This ensures that every attempt is recorded as a separate execution in the Run history.
* **Always** set a `max_iters` to prevent wasting resources if a solution isn't being found.

## 3. Direct Dataflow and Managed Documents

Use `{{steps.<id>.out.<field>}}` directly in action data and module input bindings.
Whole references preserve JSON types; interpolation accepts scalars only. Modules
export named `outputs`, consumed as `{{steps.<module-call>.out.<export>}}`; child
step IDs are not visible in the caller. Missing/skipped producer values fail before
consumption. `deferred_to_dispatch` records necessary later checks, not a pass.
Authority and secret references remain birth-known. Keep `set`/`vars` for authored
mutable counters/accumulators, not redundant copies of preceding outputs.

For intermediate files/trees declare bounded root documents:

```yaml
documents:
  notes: {kind: file, max_bytes: 65536}
  ledger: {kind: file, max_bytes: 4096, initial: '{}'}
  attachments: {kind: tree, max_bytes: 1048576, max_files: 16}
```

In a real process action's path field use `{$document: notes, access: write}`,
then `read` to consume the latest revision, or `update` for a private writable copy.
Read/update need committed or literal initial content. HTTP/MCP document paths
are unsupported; ordinary typed references remain available. Trees reject unsafe
members and exceeded positive bounds. Successful invocations publish all output
revisions atomically; failures publish none. Resume retains revisions and replay
starts a separate namespace. Managed documents are not secret stores.

Keep only intended repository source/OpenSpec deliverables in the workspace.
Documents do not require Git exclusions, but shared-workspace mutations still need
coordination. `run://` scratch disappears after the invocation and cannot carry
cross-step data. See [Path semantics](../../reference/path-and-resource-semantics.md).

Provider-owned temporary state belongs in a plugin's `entrypoint.env` as
`runtime://temp` or `runtime://temp/<relative-path>`, not in a workflow path,
hardcoded `/tmp`, or a workspace grant. It is ephemeral and cannot carry data
to another step or retain provider authentication, sessions, or caches between
invocations. Give credential-requiring plugins an explicit secret binding;
never depend on a host-home login when the plugin declares a temporary `HOME`
or provider configuration directory.

## 4. Shell Orchestration

Shell steps are useful, but don't use them to hide logic that Transist can handle better.

* **Use Transist `if` blocks**, not shell `if` statements, when the branch is workflow control flow.
* **Use Transist `loop` blocks**, not shell `for` or `while` loops, when each attempt should be inspectable.
* **Refer to scripts**, don't write long inline bash commands. Use `source://scripts/my_script.py` for better readability. Keep the path literal and inside the workflow package. Snapshot-backed runs dispatch captured helper bytes; legacy records retain authored-source behavior.

## 5. Cleaning Up

Use the top-level **`cleanup`** block for teardown tasks that should run after the main work is finished.

* **Example**: Deleting a temporary git branch or removing a scratch directory.
* Declare the intended `when` outcomes explicitly: `success`, `canceled`, and/or
  `failed`. Every matching entry runs automatically; no match means no action.
  Failure never implicitly selects cancellation cleanup.
* A failed run without matching cleanup remains recoverable with **Resume**.
  Once failure cleanup begins, primary retry/resume is blocked; use replay for
  new primary work. `runs cleanup` recovers unfinished or failed cleanup rather
  than triggering a mandatory normal-path failed branch.

## 6. AI & Humans Together

When working with model-backed tools:

* Use `prompt` or `prompt_file` for clarity.
* Keep prompts, helper scripts, and skills free of secrets because fresh run birth may copy their bytes into private run storage.
* Use `skills` to provide specialized context to the AI.
* Keep the model-backed tool as an execution participant; keep gates, artifacts, and evidence in the workflow run.
* Have a human review final AI output before it is committed or deployed.
* Use the [conversational authoring guide](../../guides/authoring/ai-assisted-authoring-with-repo-skills.md): capture intent, clarify gaps, inspect contracts, generate, validate, explain/compare, then ask separately for execution approval.

---

### Summary of Rules

* **Keep Inputs Typed**: Makes it harder to start a run with bad data.
* **Make Branches Explicit**: Makes the "logic tree" visible to everyone.
* **Cap Your Loops**: Prevents accidents and resource waste.
* **Use Gates for Real Decisions**: Keeps accountability in the record.

---

### Related

* [Workflow Patterns Cookbook](../../guides/authoring/workflow-patterns-cookbook.md)
* [Advanced Workflow Authoring Guide](../../guides/authoring/advanced-workflow-authoring.md)
* [Workflow Schema Reference](../../reference/workflow-schema.md)

**Navigation:**

* Previous: [Organizing with Subflows](07-subflows.md)
* Next: [Inspecting Runs](../03-inspection-and-recovery/01-inspecting-runs.md)
