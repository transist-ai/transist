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

## Candidate review handoff

Implementation commits, in order:

| Step | Commit | Change |
| --- | --- | --- |
| 1 | `16340cc` | Opt-in metadata and recursive validation |
| 2 | `f299dbc` | Compiler emission and activation-local scheduler routing |
| 3 | `43831e5` | Prospective checks, graph explanations and inspection/API mapping |
| 4 | `a63f074` | Recovery/document-approval proof and adoption documentation |

Native macOS ARM64 and native Linux ARM64 validated the same implementation at
`a63f07486d518fe88643b924047c80fe196eb625` (tree
`b94c95bcb5de1d87d10179b19cb43db448c2629e`). All 972 source files matched byte
hashes and modes. Canonical source-manifest SHA-256:
`8d2b9a797aa019db10dbd1b3f6d331a371782955bb19faaf27eef75fdea06aaa`.
The review-handoff commit updates this record only; it does not change executable
source. Full validation is repeated after this final documentation update.

| Completed validation | macOS ARM64 | Linux ARM64 |
| --- | --- | --- |
| Full `bash scripts/ci/check_pr_ready.sh`, no flags | PASS; 2,195 Rust tests | PASS; 2,200 Rust tests |
| `bash scripts/ci/check_schema_api_contracts.sh` | PASS; 46 Rust tests | PASS; 46 Rust tests |
| `bash scripts/ci/validate_docs_and_skills.sh` | PASS | PASS |
| `bash scripts/ci/run_security_regression_suite.sh` | PASS; 215 Rust executions | PASS; 215 Rust executions |
| Unchanged downstream runner, release CLI, no case filter | PASS; all five scenarios | PASS; all five scenarios |

Both full gates include all eleven required E2E suites, all twelve shipped
workflows, release builds, archive reproducibility and quickstart smoke. No E2E
warnings, skips or leaked processes were accepted. The Rust-count difference is
native `target_os` coverage (17 Linux-only and 12 macOS-only tests), not skipped
tests. Default-feature exclusions remain the gate's documented feature contract.
On both platforms, iteration-limit fails the intended approval guard exactly once
and leaves parent publication at zero executions.

Exact commands, exit codes, toolchains, binary hashes, failed attempts and fixtures
are retained outside the source tree in the local review evidence:

- `/tmp/loop-cap-step1-results.md`, `/tmp/loop-cap-step2-results.md`,
  `/tmp/loop-cap-step3-results.md`, `/tmp/loop-cap-step4-results.md`.
- `/tmp/loop-cap-linux-20261110-step4/STEP4-RESULTS.md`.
- `/tmp/loop-cap-step5-macos-results.md` and
  `/tmp/loop-cap-linux-step5-20261110-a63f074/STEP5-RESULTS.md`.
- `/tmp/loop-cap-source-comparison-20261110/COMPARISON.md` contains the common
  manifest format, byte/mode comparison and native cfg/count explanation.
- Post-handoff-documentation reruns are retained under
  `/tmp/loop-cap-step5-final-macos/` and `/tmp/loop-cap-step5-final-linux/`.

Environment-only failures were retained and corrected before full reruns, without
changing source, gates, assertions or sandbox policy. The Linux VM's initial
synthetic Git configuration accidentally replaced its global null device; successful
validation uses a correct device in a private mount namespace and an owned writable
Git configuration. The retained VM must use that documented re-entry environment;
its global null device remains affected outside the namespace.

This is candidate implementation/review evidence, not release readiness or a claim
of compatibility with published `0.1.0` binaries. No versions or dependency pins
changed. No push, tag, publication or operational signing is part of this work.
Adoption and rollback follow the preserved runtime/home-pair rules above.
