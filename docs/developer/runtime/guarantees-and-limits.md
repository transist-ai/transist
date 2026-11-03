# Runtime Guarantees And Limits

This page summarizes the practical runtime contract for Transist v1. It is for operators, platform engineers, workflow authors, plugin authors, and technical evaluators who need to understand what Transist does and does not guarantee.

For the protected kernel behavior, see
[`docs/developer/architecture/kernel-contract.md`](../architecture/kernel-contract.md).
For supported product-surface classification, see
[`docs/public/reference/compatibility-and-stability.md`](../../public/reference/compatibility-and-stability.md).

## What Transist Guarantees

Transist v1 guarantees the following within its documented local-first model:

- workflow execution follows explicit authored workflow structure and explicit
  runtime state transitions
- each run persists a durable local record that includes run state, events,
  step-execution records, artifacts, evidence, and a run-scoped recipe snapshot
- `run`, `runs retry` (alias `runs resume`), `runs cleanup`, and `runs replay` operate against persisted run data,
  not hidden in-memory state
- artifacts and evidence remain attributable to run, step, and execution context
- CLI inspection and the read-only companion server can read the persisted
  runtime record without mutating it
- workflow installation persists code without binding inputs, granting policy,
  or creating a run; prospective `workflows check` diagnostics remain read-only
  and `workflows authorize` publishes only operator-reviewed exact grants
- root-run cleanup is tracked separately from the primary run outcome so operators can inspect outcome, cleanup trigger, and failed-run recoverability independently
- raw secret values resolved by the runtime are redacted from persisted request
  and result payloads, artifacts, evidence, and error text when the runtime sees
  those raw values directly

## What Transist Does Not Guarantee

Transist does not guarantee:

- deterministic behavior inside external tools, models, shells, APIs, or MCP
  servers
- semantic correctness of tool output
- distributed lease coordination across multiple hosts or separate processes
- remote or backend-pluggable storage in v1
- that transformed, derived, hashed, summarized, or partially rewritten secret
  values can always be detected automatically as secrets

The runtime protects explicit workflow execution semantics and persisted auditability.
It does not claim to make every downstream side effect deterministic or safe by
magic.

## What “Deterministic” Means In Practice

In Transist, deterministic means deterministic at the workflow execution layer.

That includes:

- explicit authored workflow structure
- explicit run and step state transitions
- explicit gate decisions
- explicit persisted recipe and workflow-input snapshots
- private, content-addressed capture of resolved source-backed prompt and `source://` assets from direct recipe steps at fresh run birth
- explicit execution history for retries or recovery

It does not mean every tool invocation returns identical bytes every time.
External tools remain untrusted participants.

## What Retry Means In Practice

`runs retry <run-id>` (alias: `runs resume`) retries the failed step(s) of a
failed or canceled run.

In practice, retry behavior means:

- step executions are append-only within the same run
- failed-run recovery uses `runs retry <run-id>` when continuing the same run
  is valid
- runtime recovery may create a later execution record for the same step when
  execution is retried inside the existing run context

The important guarantee is preserved execution history, not a promise that every
failure can be retried arbitrarily.

## What Replay Means In Practice

Replay creates a new run from persisted source-run data.

In practice that means:

- `runs replay <run-id> [--run-ref <new-run-id>]` uses the source run's
  persisted recipe snapshot
- replay also reuses the source run's persisted workflow inputs
- replay records explicit lineage so operators can see the source run, while the
  replay owns an independent root run and mutation lease

Replay is not “run whatever the live files say now and pretend it is the same
execution.” If the workflow meaning changed, start a fresh run instead.

## What Manual Preflight Means In Practice

`runs preflight <run-id>` evaluates the same persisted recipe and run context
without dispatching work.

In practice that means:

- it can refresh the run's persisted `preflight` snapshot for later inspection
- it does not change the run's lifecycle status on its own
- run-asset drift appears as structured failed checks rather than a single opaque abort

## Run Asset Locks

Transist locks two classes of live assets for re-entry safety:

- content already compiled into the recipe snapshot, such as embedded skill bytes and inline prompt text
- run-record `asset_locks`, which pin installed plugin bytes via `plugin_sha256` and pin `prompt_file` bytes resolved from the authored path plus persisted inputs and vars

Fresh snapshot-backed runs retain captured source-backed bytes under
`workflow_assets/` in the run root and use them for dispatch and re-entry.
Legacy records retain their authored-source behavior.

Transist does not lock:

- out-of-band manual filesystem edits beyond the explicitly locked assets
- plugins installed under a different id than the one referenced by the recipe

## What Cleanup Means In Practice

Cleanup is an optional authored top-level workflow phase, not an implicit retry.

In practice that means:

- every explicitly declared cleanup runs automatically when its `when` matches
  the terminal root outcome: `success`, `canceled`, or `failed`
- no matching branch means no action; failed or unexpected termination never
  implicitly selects cancellation cleanup
- a failed run without matching cleanup remains recoverable; once failure cleanup
  begins, it records `cleanup_required` before preflight or dispatch and blocks
  primary retry/resume; use replay for new primary work
- `runs cleanup <run-id>` recovers unfinished or failed cleanup, not a mandatory
  normal-path failed branch; committed successful cleanup steps are not repeated
- failed-run cleanup records `cleanup_completed` only after it succeeds or is skipped; failed preflight and cleanup steps retain `cleanup_required`
- cleanup does not rewrite the primary run status; inspect `cleanup` and, for failed runs, `recovery_state`
- repeated `runs cancel` during cancellation cleanup is idempotent and does not
  interrupt cleanup or duplicate dispatch
- abrupt process death may prevent cleanup from starting or finishing; durable
  history enables explicit recovery, not guaranteed cleanup completion

## What Artifacts And Evidence Are

Artifacts are persisted operator-meaningful outputs produced by a step execution.
Examples include stdout captures, generated files, and structured result
payloads.

Evidence is persisted supporting material that helps explain or justify what
happened during execution. Examples include traces, proofs, and execution-scoped
supporting files.

Artifacts and evidence are:

- persisted under the run's local storage tree
- scoped to step execution context
- available for later inspection

Artifacts and evidence are not:

- a secret store
- a guarantee that the content itself is correct
- a substitute for current run state or event ordering
- ephemeral temporary output that can always be discarded safely

## Secret-Handling Boundary And Limits

Secrets belong in secret providers and ephemeral delivery paths, not in durable
runtime outputs.

Transist v1 protects against the most obvious leakage path: the runtime redacts
raw secret values it resolved itself when those values appear directly in
persisted request or result payloads, artifacts, evidence, stdout, stderr, or
error text.

The limit is important:

- if a tool transforms a secret before returning it, Transist may no longer be
  able to recognize it automatically
- plugin authors and workflow authors must still avoid writing secrets into
  human-facing logs, artifacts, or evidence on purpose

## Related

- [Kernel Contract](../architecture/kernel-contract.md)
- [Compatibility And Stability](../../public/reference/compatibility-and-stability.md)
- [Storage Layout](../../public/reference/storage-layout.md)
- [Secrets And Policy](../../public/handbook/04-trust-and-runtime-behavior/02-secrets-and-policy.md)
