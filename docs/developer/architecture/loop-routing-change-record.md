# Bounded-loop routing change record

## Problem and core justification

Predicate-only graph exits conflicted with kernel completion at `max_iters`:
constant continuation could finish a wrapper while silently skipping its normal
successor, including a mandatory approval guard. The compiler and scheduler own
that contract; a workflow, plugin, adapter, policy or inspection tool cannot fix
pinned orchestration semantics.

Rejected workarounds: workflow counters duplicate kernel budgets; relocating
approval guards changes authored behavior; treating exhaustion as approval is
unsafe; expression-string inference is brittle; upgrading prepared snapshots
changes their meaning. Duration enforcement is separate work.

## Design decisions and runtime impact

- Compiler-owned graph version 1 and typed owner/entry/exit metadata opt new
  recipes into corrected routing. Shared recursive validation rejects malformed,
  incomplete, unsupported or inconsistent marked topology before consumption.
- Only a successful marked wrapper at its activation-local cap overrides guards:
  block entry, permit normal exit. Failed/canceled wrappers do not gain an exit.
  Below the cap, evaluate current predicates; genuinely unknown or malformed
  values authorize neither branch. Ordinary and unmarked edges retain legacy rules.
- A declared body's initial/reset/suspended business output is known null in
  marked loop guards while the body is pending/runnable/blocked/retry-waiting.
  Validated body identities select this projection. It permits initial entry and
  continuation of a waiting iteration, not a successful exit based on prior
  approval. Running/dispatching outputs and missing fields on completed results
  remain unknown. No fake result, historical output lookup or persisted cache is
  introduced. Prospective scopes keep future output unknown instead.
- Counting remains wrapper N/body N−1 for constant continuation at cap N. Existing
  body reset and repeat-target activation code is unchanged.
- Prospective traversal retains cap exits so downstream authority, schema and
  prerequisite checks cannot disappear. Inspection/explanation remain read-only;
  cap exhaustion is never approval. Optional HTTP edge metadata is omitted for
  legacy recipes, including source-backed views.

The typed phase decomposition remains `RecoveredLoadedRun` → `ActivatedLoadedRun`
→ `PreflightedLoadedRun` → `ScheduledLoadedRun`. No phase handoff, cancellation or
preflight terminal outcome was refactored. Execution ordinals, dispatch durability
boundaries and immutable completion records remain authoritative. Corrected marked
routing intentionally changes successor eligibility, not completion-event shape
or commit ordering.

## Persistence, recovery and rollback

Only additive omitted-by-default recipe routing fields select the new contract.
No `state.json`, execution-record, artifact/evidence, gate-history or document
revision format changes are introduced. No secrets or private paths are added.
Existing history remains append-only; there is no new completion state/event.

Resume reconciles committed successor records without redispatch. Crash recovery
retains nonzero activation baselines and repeat-target activation. Reused looping
children preserve lifetime history and receive a fresh activation-local budget;
fresh children retain their distinct identities. Replay preserves marked routing
or legacy absence and creates its own document namespace. Loading never infers,
repairs, upgrades or rewrites old snapshots.

Adoption requires newly compiled recipes and new runs. Replay is not adoption.
Preserve the previous runtime/home pair for legacy recovery. Do not downgrade
marked homes, strip metadata or overwrite immutable installed definitions. Before
adoption, revert candidate changes with the old home intact; after marked runs
exist, retain their matching runtime/home pair and choose a separate preserved
pair for rollback. Rollback never reverses external effects.

## Proof and validation boundary

- Compiler/graph tests cover strict metadata, explicit roles, legacy bytes, guard
  preservation, nested topology and authored/recipe parity.
- Scheduler/prospective tests cover until/while, caps 1/2, early exits, fresh
  variables/results, empty versus unknown body outputs, failed/canceled wrappers,
  deferred scopes, downstream checks and absence of implicit execution authority.
- `crates/transist-core/tests/loop_cap_routing/recovery.rs` covers approval-guard
  failure before parent publication, resume after a committed successor, faults
  after immutable completion, nonzero activation-budget reconciliation, reused and
  fresh looping children, and byte-preserving marked/legacy replay.
- `tests/document_gate_loop_cap_routing/mod.rs` covers repeated request-changes
  reaching the real post-loop approval helper, parent publication denial, early
  and last-body approvals bound to exact current document revisions, and resume
  without repeating the guard or approved-copy publication.
- Related executable suites: `resume-replay`, `manual-gates`, `run-cleanup`.
  The unchanged downstream hotfix runner must pass `approve`, `request-changes`,
  `reject`, `early-failure`, and `iteration-limit`; the final case must fail at its
  approval guard, not later from missing publication inputs.

Step 4 macOS commands, counts, failures and retained evidence are recorded outside
the source tree. Native Linux Step 4 downstream validation remains a separate
requirement after source settles. Step 5 requires the complete final gates on both
native ARM64 platforms. Scoped macOS evidence alone is not Step 4 completion or
release readiness, and the candidate version label does not prove compatibility
with published `0.1.0` binaries.
