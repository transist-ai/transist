# Reference: Run State Document (`state.json`)

The **Run State Document** is the durable current-state projection for a single workflow run. It is a JSON-formatted record that captures the run's progress, from the initial inputs to the latest result of every step; append-only history is retained in execution records and event streams.

## Purpose

- **The Ground Truth:** It is the authoritative current-state record used by the CLI, TUI, and companion server to show what a workflow is doing.
- **Resumability:** The Transist kernel reads this file to understand where to resume a failed or interrupted run.
- **Auditability:** It provides the raw data for every audit trail and report.

## File Location

You can find this file at:
`<home>/runs/run_<run-id>/state.json`

## Schema and Format

- **Schema ID:** `transist.run_state.v1`
- **Format:** Pretty-printed JSON.

---

## Key Fields

| Field | Description |
| :--- | :--- |
| **`schema`** | Always `transist.run_state.v1`. |
| **`run_id`** | The unique ID assigned to this run. |
| **`status`** | The current status: `created`, `planned`, `running`, `waiting`, `blocked`, `success`, `failed`, or `canceled`. |
| **`steps`** | A map of every step in the workflow, including its status, typed TEP `last_request` and `last_result` envelopes, and any manual gates. |
| **`recipe_snapshot`** | The path and SHA-256 digest of the exact workflow recipe used for this run. |
| **`workflow_inputs`** | The final, resolved values for all workflow-level inputs. |
| **`preflight`** | A record of all pre-run readiness checks and their results. |
| **`parent_run_id`** | (For subflows only) The ID of the run that launched this one. |

---

## Important Technical Notes

### 1. Path Portability

To ensure that a Transist home directory can be moved or shared, all file paths inside `state.json` (like those in `last_request.paths`) are stored as **repo-relative** strings.

- **Producers** write relative paths.
- **Consumers** (like the CLI) must rebase these into absolute paths using the current home directory location. If you are reading this JSON directly, you must handle this rebasing yourself.

`last_request` and `last_result` use the canonical TEP schemas at
`schemas/tep/invoke.v1.schema.json` and `schemas/tep/result.v1.schema.json`.
Unknown fields are rejected by those schemas.

### 2. Atomic Writes and Leases

To prevent data corruption, only the process that currently holds the **Run Tree Lease** may write to this file. The lease authorizes an exclusive root-tree mutation session. Kernel-owned protected operations, including recipe snapshot synchronization and trusted-execution refresh, hold this lease while updating run state. Writes use an "atomic rename" pattern, ensuring that readers never see a partially written file.

If the protected mutation is durably committed but lease release fails, the operation remains successful and returns a structured `lease_release_warning`; the release problem does not discard the committed run state. This applies to run creation, preparation, preflight, execution, cleanup, recipe synchronization, trusted-execution refresh, and manual-gate decision persistence.

### 3. Recovery Commit Point

The immutable per-execution record is the recovery commit point for a step. It is persisted before the terminal step-state projection, so recovery can reconcile `state.json` from a committed result after a crash without dispatching that execution again. A dispatched execution without a completion record receives an `interrupted` record before replacement dispatch begins at the next execution number.

### 4. Monitoring for Changes

If you are building a dashboard or a watcher, you should use filesystem notifications (like `inotify` or `fsevents`) rather than constant polling to detect when this file has been updated.

---

### Versioned Public Surface

The `transist.run_state.v1` schema is a versioned public surface. `v1` is a
format version, not a SemVer 1.0 stability promise. It is protected in the
current minor line: we may add new fields, but we will not remove or change
the meaning of existing ones without a version bump to `v2` and migration
guidance.
