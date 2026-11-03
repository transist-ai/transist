# Guide: Inspecting Runs, Artifacts, and Events

Every time you run a workflow, Transist creates a detailed, auditable record of everything that happens. This guide shows you how to use the `transist` CLI to inspect that record. This is the primary, official way to understand your workflow runs.

> **Prerequisite:** You should have already completed the [Quickstart guide](../getting-started/quickstart.md) and have a `quickstart-run` run to inspect.
>
> **Conceptual Background:** For a deeper dive, see the [Inspecting Runs handbook page](../../handbook/03-inspection-and-recovery/01-inspecting-runs.md).

## The Goal

To build a repeatable routine for inspecting any workflow run. By the end, you'll know how to find:

- A list of all your runs.
- The complete event history for a specific run.
- The final state, including any errors.
- The files (artifacts) and logs (evidence) produced by each step.

## The Inspection Toolkit

The `transist runs` subcommand is your primary tool. Let's use it to examine the `quickstart-run` run.

### 1. List Root Runs

First, see one-line status summaries for the root runs in your current Transist home. Add `--all` when you want nested child runs too, or `--ids-only` for just run ids.

```bash
transist runs list
```

### 2. Get a Quick Status Summary

For a high-level overview of a specific run, use `status`. It tells you if the run succeeded or failed and suggests the next command to run.

```bash
transist runs status quickstart-run
```

### 3. View the Detailed State

To see the complete, final state of a run in JSON format, use `show`. This is the ground truth for what happened.

```bash
transist runs show quickstart-run
```

This command is packed with information. Look for key fields like `status`, `steps` (to see the status of each step), `last_request`, and `last_result`.

### 4. Tail the Event Log

To see the play-by-play timeline of a run, use `tail`. This is great for debugging, as it shows you the exact sequence of events, from `run_started` to `step_finished`.

```bash
transist runs tail quickstart-run --limit 20
```

## Digging into the Filesystem

While the CLI is the best starting point, all this data is stored in simple files on your disk. Understanding the structure is key to advanced inspection.

Runtime-managed run records live under the effective Transist home (resolve it
with `transist home get`; by default `${XDG_STATE_HOME:-$HOME/.local/state}/transist`).
The persisted history for `quickstart-run` is in `<home>/runs/run_quickstart-run/`.

- `state.json`: The final snapshot of the run. This is the same data you see with `transist runs show`.
- `events.jsonl`: The append-only event log. This is the data you see with `transist runs tail`.
- `artifacts/<step>/execution_<n>/artifacts/`: Contains accepted files produced by that step execution.
- `artifacts/<step>/execution_<n>/evidence/`: Contains supporting files that document *what happened*, including runtime invocation records.

The adapter's temporary staging directory is outside the run tree and is not an
inspection surface. It is removed after promotion or failure, with stale
directories cleaned at startup. Inspect accepted artifacts/evidence in the
execution-scoped tree, not expired staging paths.

Maintained delivery workflows share context, drafts, reports and ledgers through
bounded root managed documents outside Git. Inspect successful execution records'
`document_revisions` and gate review metadata for exact revision IDs, digests,
sizes and content links. Follow the opaque [document inspection links](../../reference/companion-server-api.md#managed-documents)
for exact bytes; `$document:<name>` labels are not openable paths. Resume retains
committed revisions; replay creates a fresh namespace. Secret-bearing document
bytes are rejected rather than silently redacted.

Intended source/OpenSpec deliverables stay in the workspace. Other workflows may
explicitly share mutable workspace files; those are not automatically captured,
redacted or rolled back. Preserve active-run dependencies and do not treat their
latest contents as an immutable record of an earlier execution. See
[Managed Document Paths](../../reference/path-and-resource-semantics.md#managed-document-paths).

### What's the Difference Between Artifacts and Evidence?

- **Artifacts** are the *outputs* of your workflow. They are the files you care about, like a generated report, a compiled binary, or a JSON payload.
- **Evidence** consists of *audit records*. They explain what the workflow did. For every `invoke` step, Transist records the command, arguments, and environment details available to the runtime, giving reviewers a durable audit trail.

## A Repeatable Inspection Workflow

When something goes wrong, or you just need to understand a run, follow these steps:

1. Start with `transist runs status <run_id>` for a quick summary.
2. If it failed, use `transist runs show <run_id>` to find the exact step and error message.
3. Use `transist runs tail <run_id>` to see the sequence of events leading up to the failure.
4. Examine the `artifacts/` and `evidence/` directories for the failed step to see the raw output and the exact command that was run.

By following this pattern, you can diagnose any workflow run with confidence, backed by a complete and trustworthy audit trail.

## Trust, Locality, and Approvals

### Acting on a Blocked Manual Gate

Review the preceding step's artifacts and evidence before choosing a decision.
In the fullscreen TUI or interactive terminal action picker, selecting
`approve` or `request-changes` prompts for a required reason and then a
confirmation that defaults to **No**. Confirm only when ready to resume the run;
requesting changes may rerun preceding work. Declining (or pressing **Esc** in
the TUI) returns without recording a decision or resuming execution. Direct
`transist runs gate` commands do not add this confirmation. See the
[Manual Gates handbook](../../handbook/03-inspection-and-recovery/03-manual-gates.md)
for actor resolution and command examples.

### Inspecting Recorded Decisions

For governed runs, the same inspection routine now also answers:

- Was trusted execution enabled?
- Which workflow and plugins were verified, bypassed, or denied?
- Which provider/model pairs were used, and were they local or external?
- Were any external transfers recorded?
- Who approved a manual gate, why, and when?

Use these commands when you need those facts from persisted local state:

```bash
transist runs show quickstart-run
transist runs evidence export quickstart-run
```

If you prefer the companion HTTP surface, check `GET /api/v1/runs/<run-id>/trust`, `GET /api/v1/runs/<run-id>/locality`, and `GET /api/v1/runs/<run-id>/gates`.

The evidence-export inspection field (`evidence_export.command`) is always available as a CLI hint. The optional `evidence_export.location` appears once the run has provenance, indicating the export is ready to generate.
