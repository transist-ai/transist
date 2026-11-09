# Storage Layout Reference

This document describes the on-disk storage layout for a Transist home directory. It is for **Administrators** who need to understand, back up, or troubleshoot the files Transist creates.

**Stability:** The layout described here is a versioned public surface for Transist `v1`. `v1` is a format version, not a SemVer 1.0 stability promise; the layout is protected in the current minor line and stability begins only at `1.0.0`.

## The Transist Home Directory

Transist uses a single "home" directory for runtime-managed data, including
installed plugins, run history, configuration, accepted artifacts/evidence and
managed document revisions. Maintained delivery workflows use bounded root
documents for operational intermediates outside Git. Intended source/OpenSpec
deliverables remain in the workspace. Explicit workspace-file usage in other
workflows is separate mutable host state, not automatically captured or relocated
by `TRANSIST_HOME`. See [Managed Document Paths](path-and-resource-semantics.md#managed-document-paths).

### How the Home Directory is Found

Transist determines which home directory to use in the following order of precedence (first match wins):

1. The `--home <path>` command-line flag.
2. The `TRANSIST_HOME` environment variable.
3. A user-set default path (set via `transist home set <path>`), used flat.
4. The per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`).

If none of the above can be resolved (no `HOME` and no `XDG_STATE_HOME`), non-home commands fail with a structured error. Run `transist home init` to create the per-user default home, or `transist home set <path>` to configure a user-level default at an explicit path.

For a per-use-case recommendation on which of these to rely on, see [Choosing a Home Strategy](../guides/inspection/operating-the-transist-home-directory.md#choosing-a-home-strategy).

### Top-Level Structure

A typical Transist home directory (`<home>`, resolved via `transist home get`) contains the following:

```text
<home>/
├── config.yaml          # Required operating mode and runtime policy root.
├── policy/              # Optional operator-owned policy fragments, including
│   └── workflows/       #   workflow-family YAML fragments.
├── settings.yaml        # Optional local defaults, including filesystem policy.
├── toolchains.yaml      # Toolchain configuration created by home init.
├── installation.json    # Stable installation id and workspace salt.
├── trust/                # Imported public trust roots (never private keys).
│   └── keys/<key-id>.pub

├── plugins/             # Installed plugins.
│   ├── install-index.json          # Digest index of installed versions.
│   └── <plugin-id>/
│       ├── <plugin-version>/       # Atomic, verified snapshot bytes.
│       └── <plugin-version>.sig    # Verified signature sidecar (trusted installs).
├── runs/                # Runtime-managed run records and accepted outputs.
├── locks/               # Live lease files for active runs.
├── control/             # Durable control requests (e.g., cancellation).
├── .transist-staging/   # Ephemeral private invoke staging; cleaned on startup.
├── .transist-run-birth-staging/  # Ephemeral run-birth staging; cleaned on completion.
└── registry/            # (Internal) Caches and workflow metadata.
    └── trust/           #   Trusted workflow source metadata.
```

Governed-home initialization creates `<home>/trust/keys` and an empty loaded
`<home>/policy/workflows/` source (`policy.sources: [workflows]`). The trust
store remains empty until an operator imports a public trust root with `transist
trust import`; Transist never stores private signing keys in the runtime home.

Workflow installation persists only signed public package material in governed
or trusted homes. Private keys remain outside `<home>` and are used only by
`transist-admin workflow package`. Installation serializes immutable registry
mutation per home but does not bind inputs, evaluate readiness, write policy,
or create a prepared run. `workflows authorize` may later create a no-clobber
exact-grant fragment beneath `<home>/policy/workflow-authorizations/`; those
operator-visible fragments are never overwritten by the runtime. `workflows
prepare` or `workflows run` creates durable run state only after a fresh
readiness check succeeds.

Installed plugin snapshots contain only `plugin.yaml` and the files selected by
the source manifest's `package.files`; source-directory documentation, tests,
and other unselected files are not copied into the home.

`config.yaml` is required and missing it fails closed. `settings.yaml` is an
optional override for runtime-wide filesystem and network defaults.

Update `<home>/settings.yaml` directly with any editor. Resolve the path with
`transist home get`; the fullscreen TUI shows this configuration read-only.

The TUI `Setup` pane also shows the effective home runtime settings path and
the current filesystem policy summary.

`transist home init` creates the layout above. The runtime does not use a
`registry/flows/` directory.

### Internal Trusted Workflow Registry (`registry/trust/`)

The separate `registry/workflow-install-index.json` may include optional
`definition_validation` receipts. Revision `2` records the installed recipe
SHA-256 and exact `plugin-id@version` package digests. Successful installation
publishes this metadata under the installation lock and atomic index write;
rollback covers new receipts and restores upgraded legacy entries. Uninstall
removes the workflow's entry.

Read-only bound validation hashes the current recipe and pinned packages before
reusing immutable plugin contracts. Within one evaluation, metadata resolution,
trust verification, and static adapter checks share package inspection and
compiled input-schema roots, including local references. Every materialized
input is still validated. Current signatures, trust roots, policy, secret
availability, prerequisites, and prompt/source assets are checked afresh.

Missing or older receipts use read-only contract validation; checking never
writes an upgrade. An idempotent reinstall adds current metadata. Malformed
receipts, changed installed recipe bytes, and violated package pins fail closed.
The reserved workflow version `v0` bypasses receipt reuse. Receipts contain no
prospective inputs, secrets, or policy decisions, and never authorize dispatch
or replace live preparation and execution checks. See the
[validation boundary](../../developer/architecture/kernel-contract.md#1-workflow-execution-semantics).

Trusted workflow installation writes one internal
`transist.workflow_trust_install.v1` JSON record per workflow id and version.
It records the verified package workflow and package-contained signature source
used for later trust verification; installation does not accept an
operator-selected external signature. Trusted CLI lifecycle commands and
fullscreen TUI workflow runs re-verify those recorded package inputs, and
modified or missing signed content fails closed.

This metadata resolves source for trust verification only. It neither changes
the execution workspace nor grants filesystem authority. Older relative records
remain readable: their file name is resolved against the compiled recipe's
persisted `source_dir`, or against the current repository root only when that
legacy recipe has no source directory.

## Installation Identity (`installation.json`)

Transist persists one stable installation record per home directory at the top level:

- `id`: UUIDv7 created the first time the home is initialized or first used.
- `created_at_unix_ms`: creation timestamp for that installation record.
- `workspace_salt`: internal salt used to derive a stable hashed hostname for outbound metadata.

Transist never rotates this file automatically. Reusing the same home directory keeps the same installation identity.

## Run Data (`runs/`)

The `runs/` directory is the most important for administrators, as it contains the complete history of every workflow execution.

### Run Directory Structure

Each top-level workflow run is stored in its own directory:

`runs/run_<ROOT_RUN_ID>/`

If a run contains nested `loop` or `subflow` steps, their corresponding child runs are stored inside the parent's directory:

`runs/run_<ROOT_RUN_ID>/contained_runs/run_<CHILD_RUN_ID>/`

The `root_run_id` in a run locator describes physical containment, so a
parentless replay has its own locator root. Replay ancestry remains separate:
`state.json.lineage.source_run_id` records the source run that was replayed,
while `state.json.lineage.root_run_id` identifies the replay's own run tree.

### Inside a Run Directory

For a workflow that declares `workspace`, the canonical resolved path and access
mode are persisted in both `recipe.yaml` and `state.json`. That path is the
execution workspace for preflight, dispatch, cleanup, retry, resume, and replay;
the workflow source directory and CLI CWD are not workspace authority. Re-entry
recanonicalizes the persisted path and fails closed on drift. Replay preserves
it, so changing `repo_path` requires a fresh run. Legacy records without a
workspace retain their historical CLI-directory fallback.

Each `run_<ID>` directory is a self-contained record of the execution. Key files include:

Equivalent-authority invocations may share one live adapter probe. The existing
`state.json` preflight check shape is unchanged: its `step_ids` array retains
every associated step identity in insertion order, including nested subflow
prefixes. No schema migration is required.

- `state.json`: The main summary file for the run, including its status, inputs, and a summary of each step. This is the first place to look when inspecting a run. `state.json` is an atomically replaced current-state projection; it is not the append-only history. Immutable execution records, execution-scoped artifacts/evidence, and the `events.jsonl` / `failures.jsonl` streams carry history. The persisted run record includes `asset_locks` plus `asset_lock_version`. Each `asset_locks[]` entry records `kind`, `ref_id`, optional `version`, and `sha256`. Plugin locks store the plugin id in `ref_id`; prompt-file locks store the authored `prompt_file` template string in `ref_id` and are re-resolved against the persisted recipe `source_dir`, `workflow_inputs`, and `workflow_vars` during re-entry verification. Failed subflow parent steps may also persist an additive `last_result.output.failure` object with a compact failed-descendant pointer (`child_run_id`, optional `failed_step_id`, and first-line error fields) so later inspection surfaces can show the child failure reason without recomputing it, including child-input binding failures that prevented the child run from being created. Child preflight failures that happened before dispatch may also add `preflight_blocker_count`, `preflight_warning_count`, and `preflight_first_blocker` inside that `failure` object. Recursive root preflight reuses existing child-run `state.json` snapshots for inspection when those child runs already exist, but it still rebinds the child workflow inputs from the current parent step mapping before evaluating nested blockers. The serialized shape of this record is a versioned public contract: see the canonical `schemas/runtime/run-state.v1.schema.json` (Draft 2020-12), which types `last_request` and `last_result` with the canonical TEP schemas and is validated against real serialized runtime records in `tests/schema_contract.rs` across every run-status/step-status variant and optional-field shape.
- Snapshot-backed runs persist `asset_lock_version: 1` and `workflow_asset_snapshot: true` and resolve prompts and shell `source://` assets from their private workflow-asset blobs. The marker defaults to false and is omitted when false; unmarked version-1 records use authored-source resolution. Snapshot selection depends only on persisted metadata, never on files found on disk; a required manifest or blob that is missing, malformed, or digest-mismatched fails closed. Replay copies required captures and emits version 1 with the marker; source-backed replay retains source resolution. Pre-lock version 0 is rejected for re-entry. Asset-lock and capability-authority revisions above 1 are unsupported and rejected before execution or durable mutation, regardless of snapshot marker or test mode. Do not relabel unsupported records to recover them; preserve their home and use a fresh supported run.
- Current run births also persist `capability_authority_version: 1` in `state.json`. A current non-test run whose recipe lacks the materialized authority map is rejected before re-entry, secret lookup, or transport. Child authority preparation failures are returned as structured errors before parent birth; no child record is created.
- Trusted standalone and governed runs persist `trusted_execution_mode: true`.
  Re-entry refreshes trust before checking cleanup recovery eligibility. With
  unchanged signed inputs and trust roots, a retry/resume refused because failure
  cleanup has begun may update only the `trust_decision.decided_at_ms` and
  top-level `updated_at_ms` audit timestamps in `state.json`. Trust signer,
  digest, decision and authority remain unchanged, as do recovery state,
  execution counts and resume counters. This audit refresh adds no `run_resumed`
  event, execution record, or artifact/evidence change. Untrusted standalone
  refusal preserves the complete state. See cleanup recovery in the
  [CLI reference](cli.md).
- `provenance.json`: Additive run provenance and trust summary. This records the workflow and plugin digests seen by the runtime, trust outcomes, the run-start policy decision, any captured provider/model usage metadata, provider locality (`provider_kind`), external-transfer summary, approval summaries, denial details for refused trusted or local-only runs, and finalization timestamps. When transfer recording is active, `external_transfer_summary.external_transfers[]` stores run-root-relative ledger entries pointing at the recorded `transist_external_transfer_<execution>.json` evidence files together with destination, adapter/provider metadata, content hash, and timestamp. The record is written at run-start (decision-time) and finalized at run-end. Step-dispatch policy, filesystem-policy, capability, and trusted-mode checksum denials update the record before the kernel bails so denied-run evidence is always persisted before execution effects. Readers must reject unknown `schema` versions to fail closed.
- `context/run_inputs.json`: The immutable workflow inputs captured at run birth. It is validated with the other birth artifacts during ambiguous publication recovery and is never treated as a mutable source for re-entry.
- `recipe.yaml`: A snapshot of the compiled workflow that was executed, including the effective per-step filesystem policy and the complete normalized `capabilities` map produced by the typed authority projection. Current run births mark this map as materialized; re-entry rejects missing, malformed, removed, or mismatched authority. Each invoke `last_request` in `state.json` carries the exact checkpoint subset plus policy id/profile, requested and granted capability metadata, and any denial reason. Manual gates retain separate request and decide authority entries.

  Optional `graph.loop_routing_version: 1` reserves explicit bounded-loop routing;
  `graph.edges[].loop_route` contains `owner_step_id` and `role: enter | exit`.
  Marked graphs must describe every loop wrapper's entry and, when present,
  normal successor, consistent with wrapper/body/repeat topology. Validation is
  recursive and rejects unsupported versions and explicit null or malformed
  metadata before executable graph consumption. Absent fields are omitted on
  serialization and preserve legacy bytes and behavior. Newly compiled graphs
  containing authored loops emit version 1, selecting cap-inclusive normal exits
  for successful wrappers. Loading an unmarked prepared recipe never upgrades it.
  Never strip markers or rewrite prepared snapshots to force compatibility with
  an older runtime; preserve the matching runtime/home pair.
- `workflow_assets/`: Present when snapshot-backed fresh run birth resolves authored `prompt_file` or shell `source://` assets. `manifest.json` maps logical asset paths to content-addressed blobs under `blobs/`. The directory is written as part of atomic run birth, is private runtime state, and is not included in inspection or snapshot-export payloads. Snapshot-backed prompt and shell dispatch use these captured bytes; legacy v1 runs retain their authored-source resolution behavior. Workflow assets must not contain secrets.
- During the test-only `runs sync-recipe` mutation, the run root may temporarily contain the hidden `.recipe-sync.intent.json`. It is fsynced before either live file changes and removed only after `state.json`, `recipe.yaml`, and the run directory are durable. It is recovery metadata, not part of the final persisted run format. Recovery validates both embedded old/new state revisions and matches current state bytes to a validated pair before any repair writes or intent deletion. Unsupported revisions preserve the state, recipe and intent unchanged. This guarantee is per unsupported record, not whole-tree atomicity; ancestor work may precede discovering an unsupported descendant.
- `events.jsonl`: An append-only log of all events that occurred during the run. Every append flushes and `sync_all`s the file; first creation also syncs the exact containing directory, so a crash never leaves a partially written record. If a crash leaves the final record without a trailing newline, the next append repairs the tail: when the trailing fragment parses as a complete JSON value, only the missing newline is appended (the record is preserved); otherwise the fragment is truncated to the last newline. Either way a durable `storage.tail_repaired` recovery event is recorded — each distinct repair occurrence on the same stream gets its own event, not just the first. When a different stream (e.g. `failures.jsonl`) is repaired, its recovery note is appended to this events stream only after the events stream itself has been repaired, so a note is never glued onto a truncated fragment inside `events.jsonl`; simultaneous truncation of both streams remains repairable and fully inspectable. Inspectors read through the same tolerant reader, which retains a newline-less final fragment when it parses as a complete JSON value and drops it only when it is genuinely malformed. Malformed complete or interior records (newline-terminated but unparseable) fail inspection without repair.
- `failures.jsonl`: An append-only stream of classified failure records emitted by failed or cancelled step executions. It uses the same durable append and tail-repair semantics as `events.jsonl`.
- `step_executions/`: Contains detailed JSON records for each attempt of every step. Records are append-only for the lifetime of the run; a rerun under the same run id (including a reused subflow child) appends the next execution at `N+1` and never deletes or overwrites a prior record. The runtime rejects any attempt to rewrite an existing execution record. Inspection surfaces list these records in ascending numeric order by execution number. The immutable per-execution record is the completion commit point and carries an optional `retry_wait_until_ms` (the retry deadline of a committed retryable failure, reconciled back as `RetryWait` on recovery); an in-flight ordinal with no completion record receives a single `interrupted` record, no spurious `execution_000` record is ever created for a step that has not consumed an execution, and recovery validates that a loaded record matches the requested `(run, step, execution)` identity. The same commit-point, interrupted-record, and existing-ordinal rules apply to cleanup step execution.
- `artifacts/`: Contains all files produced by the run's steps, including artifacts and evidence. Artifact and evidence files are execution-scoped and are never deleted or overwritten by retry, resume, replay, cleanup, cancellation, or a reused subflow rerun. Runtime-owned artifact and evidence files are written through a temp file, `sync_all`, atomic no-clobber publication, and a sync of the exact containing directory. A retry may reconcile an existing regular file only when its bytes are identical; differing content, symlinks, and directories are rejected. New directory paths are synced from their leaf to the first existing ancestor. Publication is descriptor-anchored: the destination parent is opened as a real directory (`O_DIRECTORY | O_NOFOLLOW`) and the final name is resolved relative to that descriptor (`linkat`/`openat`/`renameat` with `O_NOFOLLOW`), so a same-user racer that replaces a parent path or final file with a symlink cannot redirect or reconcile attacker bytes. The temp source is verified through that same anchored descriptor to be a regular, non-symlink file before it is published, so a temp file replaced with a symlink is rejected rather than hard-linked or renamed into place. Artifact reads are likewise descriptor-anchored and `O_NOFOLLOW`, so a symlink swapped into the final artifact path is rejected rather than followed. The `step_id` and `logical_path` components of an artifact path are validated before any path is built: empty, absolute, `.`/`..`, root, or prefixed components are rejected, so no artifact read or write can escape its execution artifact directory. Atomic replacement (`state.json` and other projections) uses the same anchored model. Run-birth reconciliation holds ONE root descriptor for the whole multi-file recovery and resolves every birth file (`state.json`, `recipe.yaml`, `context/run_inputs.json`, `provenance.json`) relative to it, so a swap of the run root or any intermediate component between individual reads cannot redirect or mix reads; run-locator publication and reconciliation are likewise descriptor-anchored on their own parent. Run-locator resolution is fail-closed: a corrupt or unreadable locator is an error, while a genuinely absent locator (legacy layout) resolves to the deterministic top-level `runs/run_<id>` path.

Active invocation records include an OS-provided process-birth token. Cross-process
cancellation verifies that token before it sends any signal, so a recycled PID
is never targeted. Missing tokens on legacy records, unreadable identities,
permission denials, failed signals, and unconfirmed termination all retain the
record for operator recovery; only confirmed absence or termination removes it.

During an invoke, the adapter writes to a private mode-0700 directory under the
home-level `.transist-staging/` directory, not to this final run tree. The same
protected parent is used for disposable process/MCP preflight staging. Process
and MCP `entrypoint.env` declarations may additionally use `runtime://temp` or
`runtime://temp/<relative-path>`; each resolves to a separate mode-0700 root under the
canonical system temporary parent. The resolved host path is private request
metadata only: durable manifests retain the authored URI and no run state,
event, artifact, or evidence records the host path. The
kernel scans each reported path and file before atomically promoting accepted
bytes into `artifacts/`. Staging directories are removed on normal completion
and stale owner directories are removed at runtime startup; they are not an
inspection or backup surface. Process and MCP plugins do not receive a durable
run-root path or mount; `InvokePaths.run_root` names the current private staging
root only. Their `TMPDIR` is forced to the declared runtime-temporary directory,
or that root when no declaration selects one, after all environment processing,
including preflight.

### Managed document invocation copies

Process `$document` copies use `documents/{read,write}/` inside the separate private
runtime temporary root, outside `<home>` and the workspace. Read copies are
sandbox-enforced read-only; write locations are initially absent and update
locations contain exact copied committed bytes. No hard links to stored revisions
are delivered. Copies disappear with invocation staging; only validated immutable
revisions survive successful completion. Durable request/result metadata retains
logical document references and labels. Text artifact/evidence metadata is
normalized before promotion; business document bytes are never rewritten. A
business document containing a private invocation path is rejected by the
non-persistence check rather than published with an expired path.

The runtime selects the canonical OS temporary parent independently of caller
`TMPDIR`; it rejects exposed-root overlap before creating staging. Source roots
cannot grant broad access to protected temporary/staging trees. Plugin environment
declarations cannot select the reserved `documents` subtree or traverse symlinks
into it; they never create directories inside committed read copies or precreate
missing document output trees.

### Manual-gate history and invocation input

`gates/<gate-id>.json` is the active manual-gate decision consumed during loop
re-entry. `gates/history/` keeps append-only decision records after that active
state is cleared. Plugins must not read either location: `InvokePaths.run_root`
is private staging, not the durable run root. For actions declaring a
`transist_gate_decision_v1` binding, the kernel validates and injects the newest
matching immutable history record into the action's optional input field before
staging. Missing history leaves that field absent; malformed, mismatched, or
ambiguous history fails the invocation before plugin launch.

Document-review gates commit their request in
`step_executions/<gate-step>/execution_<N>.json` as
`result.output.review_request` (`transist.gate_review_request.v1`). It contains
the owning run/gate/step, the original waiting execution ordinal, and mandatory
`review_documents` with full exact revision records. Repeated waiting attempts
reference that original immutable request instead of resolving newer heads.
Active and history decisions carry the same `review_request`; accepted gate
completions also retain `result.output.review_decision`. Clearing active state
never removes the request, decision history or reviewed revisions. Current
execution and activation counters, original-request identity and exact immutable
history are checked before a reviewed consumer can copy content. The kernel
derives decision content from the committed waiting request under the root lease;
caller-supplied or edited request metadata cannot substitute different revisions.
Retrying the same decision/actor/source/reason for that still-waiting request
reconciles a byte-identical history publication and restores active state without
changing its original timestamp or adding another record. A different decision
for an already-decided request fails; later gate cycles have different identities.
Inspection reconciles each full decision with its exact immutable history and,
after consumption, the completion's embedded `review_decision`; actor, source,
reason and timestamp mismatches fail just like changed decision values. Runtime
review-summary keys cannot be supplied through authored gate guidance.
The schema is [`gate-review.v1.schema.json`](../../../schemas/runtime/gate-review.v1.schema.json).

Step execution records persist the effective per-step `request.policy.filesystem` in the request snapshot. Durable event streams may also include `filesystem_policy.denied` and `filesystem_policy.sandbox_failed` records when the runtime rejects a path or cannot build a sandbox safely.

A visual example of the layout for a single run:

```text
run_my-first-run/
├── state.json
├── provenance.json
├── recipe.yaml
├── context/
│   └── run_inputs.json
├── events.jsonl
├── failures.jsonl
├── step_executions/
│   └── get_data/
│       └── execution_001.json
└── artifacts/
    └── get_data/
        └── execution_001/
            ├── artifacts/
            │   └── stdout.txt
            └── evidence/
                └── api_response.json
```

### Managed documents

Workflows may declare root-level bounded documents. Their storage is private to
one root run tree, independent of tickets, workspace paths, and replay ancestry:

```text
run_<ROOT_RUN_ID>/documents/
├── initial.json                  # Immutable root-birth initialization commit.
├── blobs/<sha256>                # Exact file/member or canonical manifest bytes.
├── candidates/<RUN_ID>/<STEP_ID>/execution_<N>.json
└── heads.json                    # Atomic, rebuildable latest-revision projection.
```

`initial.json` uses `transist.document_initialization.v1`, pins the canonical
declaration digest, and references literal file initial revisions. It and their
blobs are part of staged root birth, whose directory rename is the initialization
commit point; discovery still waits for durable locator publication. Child births
do not initialize documents again. Files without `initial` and all trees remain
unpublished until a producer succeeds; an absent head is not an empty file.

The **immutable successful execution record** under `step_executions/` is the
publication commit point for all of one invocation's `document_revisions`.
Capture first validates the complete batch, persists immutable blobs and candidate
metadata, then completion publishes that exact array. Candidates never advance
heads. A crash before completion leaves no published outputs; a crash after it
leaves every committed output readable even if `heads.json` is absent, corrupt,
or stale. Main/cleanup recovery reconstructs the entire heads map from records
under the existing root-tree lease. Validated blob/candidate directory entries
are resynced before completion publication; recovery resyncs ambiguous completion
entries before updating heads or state projections. A contiguous root-tree `commit_sequence`
orders document-producing completions across contained runs and cleanup; initial
revisions use zero and consumed execution ordinals remain the existing N+1 rule.

Hard-link publication can leave a valid final name and its private temp alias
after a process dies between durable linking and temp cleanup. Runtime-owned
publication readers allow those aliases and keep anchored no-follow, exact-size,
digest and record checks; inspection/recovery does not delete either name. The
single-link requirement applies to **untrusted capture sources**, not those
runtime-owned publications. This distinction prevents a genuine durable commit
from becoming permanently unreadable after a crash.

Reconstruction checks the complete consumed ordinal prefix against every main and
cleanup step projection, even when a rerun clears `last_result`. Only the latest
`running`/`dispatching` attempt without a result claiming that ordinal may lack a
record. An existing committed record with an unupdated in-flight projection remains
authoritative. Missing completed tail records or entire execution directories fail
closed; they do not silently restore older or initial heads.

Each revision uses `transist.document_revision.v1` with root namespace, document
name, producing run/step/execution, revision ID, content digest/size, aggregate
business byte count and regular-file count. A tree blob is canonical JSON with
sorted relative file members and directory names; member bytes share the same
digest pool. Blobs are stored once per digest per root tree. Reads rehash exact
bytes, check recorded sizes, validate manifests and producing identities, and
compare completion arrays with their immutable candidates. Prior records, blobs
and candidates survive retries, cancellation and recovery; no automatic document
garbage collection is introduced. Nothing here is mounted into plugin sandboxes.

Trees require positive `max_bytes` and `max_files`. Regular files are counted
against `max_files`; directories are separately bounded by that same limit,
member paths by 4096 UTF-8 bytes and 64 components. Only regular files and real
directories are admitted, including empty members. Symlinks, hard links, special
files, unsafe paths, exceeded bounds, changed captures, and known raw/base64 secret
values cause rejection. Business bytes are never silently redacted. Literal
initial content is bounded at compilation/birth; birth performs no secret lookup
and has no external file source. Runtime-known-value scanning applies when outputs
are captured through the document port.

The whole batch's exact durable bytes are scanned before persistence, including
canonical manifests and their member digests, using raw and recognized padded/
unpadded standard/URL-safe base64 secret forms. Tree capture retains descriptors,
identity/version witnesses and directory inventories until its final fence.
All files/directories must remain unchanged across the complete capture: a mixture
of independently valid file reads is not accepted as a coherent tree snapshot.

Resume uses the same namespace. Replay creates a new root namespace, initializes
only authored literals, and reruns producers; it never copies published source
revisions. Declarations cannot be changed by test-only recipe synchronization.
The canonical storage schemas are
[`managed-documents.v1.schema.json`](../../../schemas/runtime/managed-documents.v1.schema.json)
and [`step-execution.v1.schema.json`](../../../schemas/runtime/step-execution.v1.schema.json).
`state.json` retains its existing projection shape, including the gate's current
result. Public process `$document` bindings use private invocation copies as
described above. Reviewed reads additionally require exact revision ID and digest
coverage by the current recipe-local gate approval. The inspection/storage ports
resolve immutable revision IDs and opaque tree member IDs without exposing paths;
child reads require persisted lineage to agree with physical containment all the
way to the root. Replay ancestry cannot redirect document reads. See the
[document API](companion-server-api.md#managed-documents).
An empty declaration namespace intentionally has no initialization file and its
revision lookup returns no match after lineage validation. Declared documents
still require a valid initialization record; missing state is never an empty
namespace fallback.

Use fresh homes for this format. Rollback selects the preceding runtime and a
preserved home; never downgrade or rewrite a new-format home.

### Local inspection output snapshots

The local Rust inspection model exposes `InspectionStep.latest_output` as an
execution-attributed view of persisted `ExecutionResult.output`. It is read-only
and does not add fields to `state.json` or execution records.

- The latest ordinal is the maximum of the persisted step counter and execution
  record ordinals; zero means no execution.
- The immutable record for that ordinal wins, including its recorded status and
  an explicit no-result snapshot when the record contains no result.
- If that record is absent, a state `last_result` is accepted only when its run,
  step, and execution identifiers match and the persisted step status is
  `success`, `failed`, `retry_wait`, or `canceled`.
- A newer in-flight attempt never displays an earlier attempt's output.
- No attempt and no result yields `latest_output: None`. Otherwise the snapshot
  retains `execution`, `status`, `source` (`execution_record` or `state_snapshot`),
  and `result_present`. `value` is present exactly when a result exists; JSON
  `null`, empty objects/arrays/strings, `false`, and `0` remain real values.
- Malformed execution records and mismatched record identities fail inspection;
  mismatched state results are excluded from the snapshot.

The values are generic plugin data, not HTTP-sanitized presentation text. Existing
summaries and ordered execution history retain their prior meanings. Snapshot
export excludes `latest_output`, including with extended full-detail flags;
existing export fields and payload selection remain unchanged.

### Artifacts and Evidence

All files generated by a run are stored within the `artifacts/` subdirectory of the run. This includes:

- **Artifacts:** Standard outputs from a step's execution, like `stdout.txt` and `stderr.txt`. Stored in `.../artifacts/`.
- **Evidence:** Files that provide proof of execution, such as a downloaded file or an API response. Stored in `.../evidence/`. Transist also writes its own `transist_runtime_invoke_evidence.json` file for every step invocation. Governed runs may additionally write `transist_external_transfer_<execution>.json` for allowed external-provider calls when transfer recording is active.

Current runtime-owned evidence files:

- `transist_runtime_invoke_evidence.json`: additive per-invoke execution metadata, producing workflow/plugin provenance, usage, and filesystem policy snapshot.
- `transist_process_lifecycle_evidence.json`: post-terminal process exit-grace diagnostic (`transist.process_lifecycle.v1`), including validated/redacted original result data, numeric live-boundary snapshot, and confirmed/unconfirmed cleanup. The failed execution output remains `{}`; see [APP lifecycle](app.md#process-lifecycle-after-a-terminal-result).
- `transist_external_transfer_<execution>.json`: additive external-transfer record (`transist.external_transfer.v1`) containing destination, adapter kind, provider/provider_kind, content hash, and policy decision. This file is emitted only when a governance mode activates transfer recording. Current `policy_decision.mode` values are `trusted_execution` and `local_only`. The governed-run scope is honest: `redaction_applied` is always `false`; this is not DLP-grade redaction or a legal-compliance guarantee.
- Runtime-known raw secret values and their padded or unpadded standard and URL-safe base64 encodings are absent from durable run names and bytes. Valid UTF-8 files are redacted before promotion; binary files containing a known variant are rejected. This guarantee does not cover arbitrary plugin transformations or external exfiltration.

Step execution records include a `state_mutations` array when adapter-reported artifacts or evidence were successfully persisted before the execution finished. Each artifact mutation records the logical path and SHA-256 checksum. Runtime-generated diagnostic evidence is excluded from this retry-safety signal because it is written after the adapter result is known.

## Locking and Concurrency (`locks/`)

Transist uses `.lock` files in the `locks/runs/` directory to authorize one process at a time to mutate a root run tree. The kernel acquires the lease for protected mutation sessions, including recipe snapshot synchronization and trusted-execution refresh, and releases it after the durable update. If release fails after a successful update, the caller retains the success and receives a structured warning; the resulting stale lease can be handled by the orphan-lease reaper.

Run birth also uses the shared `plugin-lifecycle-reservation.lock` so installed
plugin snapshots cannot change while a root, replay, prepare, launch, or child
run resolves its plugin and asset locks and publishes its locator. Birth tries
immediately and then every 100 ms for at most 15 seconds. It never reaps,
breaks, replaces, or overwrites a live reservation; a timeout reports the
latest owner id, PID, host, and command. Plugin install and uninstall do not
wait: they acquire this reservation once and fail immediately on contention.
An embedded child stops waiting when its durable root cancellation is observed,
leaving no child run directory or synthetic subflow-start failure.

If a `transist` process is terminated unexpectedly, a stale lock file may be left behind. The system includes a "reaper" that will clean up lock files belonging to non-existent processes on the same host to prevent deadlocks.

## Important Notes for Administrators

- **Local-First:** The entire state of Transist is stored on the local filesystem. There is no database or remote server dependency.
- **Backup:** To back up your Transist instance, you can simply create an archive of the home directory.
- **Troubleshooting:** When a run fails, the `state.json` file and the contents of the `step_executions/` and `artifacts/` directories for the failed step are the most useful files for diagnostics.
- **Audit trail:** When you need to explain which trusted workflow or plugin bytes were evaluated, or why trusted execution refused a run before dispatch, inspect `provenance.json` alongside `state.json`.
- **Immutability:** Once a run is complete, its records are effectively immutable. New actions like `replay` create new runs with a new ID. Within the same run id, retry, resume, and reused subflow reruns append new execution records and never delete prior ones; the step execution counter is append-only and the next dispatch starts at `N+1`.
- **No Cross-Host Locking:** The file-based locking mechanism is designed for a single host. Running `transist` commands against the same home directory from multiple machines concurrently is not supported and may lead to data corruption.
