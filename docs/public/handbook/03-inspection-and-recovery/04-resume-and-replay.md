# Resume and Replay

In the real world, workflows fail. Networks go down, APIs time out, and tools error out. Transist provides two primary ways to get your work back on track: **Resume** and **Replay**.

Transist makes failure visible and recoverable. It does not pretend that every external tool call is deterministic or safely repeatable.

## Capability status

| Capability | Status | Notes |
|---|---|---|
| Explicit failure states | implemented | Failed runs preserve state, events, artifacts, evidence, and failed step context. |
| Partial execution inspection | implemented | Use `runs status`, `runs show`, `runs tail`, and the run directory. |
| Resume | implemented | `transist runs retry <run-id>` (alias: `transist runs resume <run-id>`) continues a failed run when recovery is still valid. |
| Replay | implemented | `transist runs replay <run-id> [--run-ref <new-run-id>]` creates a new run from persisted source-run data. |
| Cleanup | implemented | Declared matching cleanup runs automatically; `transist runs cleanup <run-id>` recovers unfinished or failed cleanup. |
| Arbitrary external retry safety | out of scope | Transist cannot guarantee external side effects are idempotent. |

## 1. Resume (fix and continue)

Use `transist runs retry <run-id>` when a run has failed and you want to pick up exactly where it left off. (`transist runs resume <run-id>` is an alias for the same behavior.)

Before retrying, Transist re-verifies the run's persisted asset locks. That means retry fails clearly if a pinned plugin version is no longer installed with the same bytes. Snapshot-backed runs resolve prompt and `source://` files from their private `workflow_assets/` capture, so edits or deletion of the authored workflow source do not affect retry. Legacy v1 runs retain live prompt-file resolution and require their authored source directory.

**How Retry works:**

- It keeps the same **Run ID**.
- It uses the same **Recipe** snapshot.
- It uses the same persisted canonical **Workspace** when one was declared;
  changing CWD does not change it.
- It re-enters the failed run from persisted state.
- It preserves the history of failed attempts so you can see what went wrong.
- Execution history is append-only: retry never deletes prior step state, execution records, artifacts, evidence, failures, or gate decisions, and never resets an existing step execution counter to zero. The next dispatch of a step starts at `N+1`, where `N` is the number of times that step has already been dispatched under this run id. `state.json` is an atomically replaced current-state projection; `events.jsonl`, `failures.jsonl`, and execution-scoped artifacts/evidence carry the durable history. Every append and run-birth write is flushed and synced so a crash at any point exposes either no run or a complete run; a truncated final JSONL record is repaired on the next append (a complete valid record missing only its newline is preserved; a genuinely malformed fragment is truncated) and a `storage.tail_repaired` recovery event is recorded for each distinct repair. The immutable per-execution record is the completion commit point: on crash recovery, a step whose completed execution record is already durable is reconciled from that record and is never re-dispatched, while an ambiguous in-flight execution ordinal without a completion record first gets an immutable `interrupted` record and the replacement dispatch starts only at `N+1`. The record persists a committed retryable failure's retry deadline, so recovery restores such a step to `RetryWait` instead of flattening it to `Failed`. Recovery also backfills a completion/failure event that never reached the events stream (deduplicated by run, step, execution, and event kind), and applies the identical commit-point rules to cleanup steps: a committed cleanup result is never re-run and an ambiguous in-flight cleanup ordinal keeps exactly one immutable record. A step in flight with a zero execution counter never produces a spurious `execution_000` record.

**When to use it**: Use Resume after you've fixed an environment issue (e.g., fixed your internet connection or updated an API key) and you want to finish the job you already started.

## 2. Replay (start over with the same plan)

Use `transist runs replay <run-id>` when you want to start a completely brand-new run, but using the exact same inputs and instructions as a previous one.

Replay also re-verifies the source run's persisted asset locks before starting the new run. A replay can therefore fail if the pinned plugin bytes drifted. Snapshot-backed replays copy the source run's validated manifest and blobs, so the authored source tree is not required; legacy v1 replays retain live-source behavior.

**How Replay works:**

- It creates a **new run ID**.
- It uses the persisted **inputs** from the original run.
- It uses the persisted **recipe** snapshot.
- It preserves the persisted **workspace** and cannot replace `repo_path`.
- It starts execution from the beginning.
- It records lineage back to the source run.

**When to use it**: Use replay for debugging and comparison. If a run failed in a strange way, replay can help you see whether the same persisted recipe and inputs fail again.

Replay does not mean every external side effect is safe to repeat. If the workflow calls a payment API, opens an issue, deploys something, or sends a message, the external system still controls the side effect.

Managed documents retain committed revisions during retry/resume. Replay starts
a separate document namespace from declarations/initial content and reruns
producers; it does not reuse the source run's latest document heads or approvals.
Waiting restart retains the gate's exact review content; any newly published
revision needs new approval before a protected read.

Workspace files are not restored from runtime history during retry or replay.
Maintained delivery workflows keep operational intermediates in managed documents
and intended source/OpenSpec deliverables in the workspace. Other workflows may
explicitly share workspace files; preserve active-run dependencies and coordinate
collisions because a new replay ID does not isolate workspace mutations. See
[Managed Document Paths](../../reference/path-and-resource-semantics.md#managed-document-paths).

## Rerun modes for subflows

A subflow step with `rerun_mode: reuse` (the default) re-enters the existing child run under the same run id when the parent re-dispatches the step. The child's execution history is append-only: prior execution records, artifacts, and evidence remain inspectable, the child's step execution counters are never reset, and the next dispatch starts at `N+1`. Use `rerun_mode: fresh` when a parent loop or review cycle should create a distinct child run id with fresh counters for each re-entry (for example, so a looping child restarts its internal loop from the beginning on every parent iteration).

## 3. Cleanup

Every explicitly declared cleanup runs automatically when its `when` matches the
root outcome (`success`, `canceled`, or `failed`). No matching branch means no
action; failed or unexpected termination never implicitly selects cancellation
cleanup. The primary status remains unchanged and cleanup has a separate result.

Use `transist runs cleanup <run-id>` to recover unfinished or failed cleanup,
including after abrupt process death. It is not a required normal-path trigger
for the failed branch. Committed successful cleanup steps are not repeated.
Repeating `runs cancel` during cancellation cleanup is idempotent and does not
interrupt or duplicate cleanup.

In plain mode (the default), the CLI waits for cleanup to reach a terminal state and prints a compact result summary to stderr, including the cleanup state, per-step statuses, and inspect commands. In TUI mode (pass `--ui tui`), the CLI waits for cleanup to settle and then shows the result in a pager-style modal. After the modal closes, the run-detail screen keeps a compact cleanup status line and exposes a `View last cleanup result` action. Interrupting the foreground CLI while cleanup is in progress does **not** stop the detached runner — cleanup continues independently.

A failed run without matching cleanup remains recoverable with retry/resume.
Once failure cleanup **begins**, recovery records `cleanup_required` before
preflight or dispatch and blocks primary retry/resume, even if cleanup fails.
Successful or skipped completion records `cleanup_completed`. Use `runs cleanup`
for cleanup recovery and `runs replay` for new primary work with the same pinned
recipe and inputs. Durable history supports recovery; abrupt process death can
still prevent automatic cleanup from starting or finishing.

## When to start a "Fresh" Run instead?

If you have changed the workflow file itself, updated a prompt, changed a skill bundle, changed input values, or need another `repo_path` workspace, start a **fresh run** with `transist workflows run`.

Neither Resume nor Replay will see your new changes; they are both "locked" to the version of the workflow that existed when the original run was first started. This ensures that your run history is always accurate and never changes after the fact.

---

### Related

- [Inspecting Runs, Artifacts, Evidence, and Events Guide](../../guides/inspection/inspecting-runs-artifacts-evidence-and-events.md)
- [CLI Reference](../../reference/cli.md)
- [Storage Layout Reference](../../reference/storage-layout.md)

**Navigation:**

- Previous: [Manual Gates](03-manual-gates.md)
- Next: [Troubleshooting](05-troubleshooting.md)
