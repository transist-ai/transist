# The Kernel Contract

The **Kernel Contract** defines the protected behaviors of the Transist
runtime. This document is the contract offered for the public `0.y.z` release
line and it is binding for the development and stability of the core
execution model. While we iterate quickly on features and plugins, the core
execution model described here is **protected in the current minor line**.
Stability begins only at `1.0.0`; until then, `v1` in any schema identifier is
a format version, not a SemVer 1.0 stability promise, and a minor release may
move or remove a protected surface with changelog, release-note, and migration
guidance.

If you find a conflict between this document and any other guide or README regarding core runtime behavior (execution, recovery, replay, or persistence), **this contract wins.**

---

## What This Contract Covers

This contract protects the local runtime behavior that must not drift silently within the current minor line:

- **Workflow Execution**: How recipes are followed and steps are triggered.
- **State Transitions**: The rules for how runs and steps move between statuses.
- **Recovery & Replay**: How failed runs are resumed or restarted from snapshots.
- **Persistence**: How we store state, artifacts, and evidence to preserve an inspectable audit trail.
- **The Boundary**: The protocol between the Kernel and Adapters.

---

## Protected v1 Behavior

### 1. Workflow Execution Semantics

- **Release Trust Boundary**: Ordinary SemVer plugin releases, including `0.x.y`
  and prereleases, always follow governed signature verification. Enabling
  `dev-version-bypass` admits only the explicit non-SemVer plugin test markers
  `0` / `v0` (and the reserved workflow `v0`); it never makes `0.1.0` unsigned
  or tampered packages trusted. Catalog release versions are independent of
  `v1` schema identifiers and the runtime crate release line.
- **The Recipe is King**: Once a run is prepared, it is locked to a `recipe.yaml`. Commands like `run`, `resume`, and `replay` always execute from this persisted snapshot, never from a freshly recompiled workflow. This ensures that even if you change your code on disk, the active run remains deterministic. The one documented exception is the reserved test workflow version `v0`, which still executes from the persisted run-scoped `recipe.yaml` but allows that file to be patched between re-entry attempts, including through the explicit operator command `runs sync-recipe`.
- **Cleanup Phases**: For root runs, every explicitly declared `cleanup` whose
  `when` matches the terminal root outcome (`success`, `canceled`, or `failed`)
  runs automatically as a separate phase. No matching branch means no action;
  failed or unexpected termination never implicitly selects canceled cleanup.
  Cleanup does not change the primary status and has a separate persisted result.
  Abrupt process death may prevent cleanup from starting or finishing; durable
  history supports recovery with `runs cleanup`, not guaranteed completion.
- **Explicit Boundaries**: All external work must cross a formal `adapter + plugin + action` boundary.
- **Validation Boundary**: Bound-workflow validation is read-only evidence. It
  may materialize birth-known requests, project capabilities, inspect policy,
  and verify trust, but it never creates a run, resolves a secret, starts a
  process, opens a network connection, or changes dispatch semantics.
  The application `workflow_validation_report` and `workflow_validation_summary`
  APIs also check secret availability by name and delivery policy without
  resolving values. Their `evaluation_mode: validation` evidence can pass
  required validation while readiness remains `not_ready`; it never authorizes
  dispatch or replaces preparation, execution, or recovery checkpoints.
  Ordinary installed `workflows check` and authorization use this validation
  scope. Required validation must pass for check success; deliberately omitted
  probes do not block it, but warnings and incomplete validation do. Full and
  summary reports share the same fields and predicates. `check --live` and
  `check --output` retain live adapter probes and canonical readiness evidence;
  authored `check --path` remains static and rejects `--live`.
  Installed definition-validation receipts may reuse immutable plugin contracts
  only after current recipe bytes, exact pinned package digests, and validator
  revision match. The operation-scoped catalog shares package inspection and
  compiled schema roots but validates every materialized input. Legacy fallback
  remains read-only; `v0` bypasses receipt reuse. Current trust, authority,
  prerequisites, secret availability, and input-bound assets remain fresh.
- **Authoring Explanation Boundary**: `workflows explain --path` consumes captured
  compiler definitions and read-only pinned plugin contracts only. It never
  initializes a home, evaluates authorization/readiness, resolves secrets, probes
  participants, or creates a run/document revision/gate decision. Requested
  authority and opaque external behavior are labeled explicitly. Structural diffs
  are review aids, not semantic-equivalence proofs or execution consent. The
  explanation-only symbolic recipe is never installed or passed to the kernel.
- **Workflow Lifecycle Boundary**: Installation verifies and persists workflow
  code only. It never binds prospective inputs, evaluates readiness, grants
  policy, or creates a run. `workflows check` is the read-only prospective
  diagnostic; `workflows authorize` may write only reviewed exact missing
  grants after reevaluating current inputs, canonical resources, trust, and
  effective policy. The exact fragment and destination must match the reviewed
  plan; any drift aborts without policy writes and requires another review.
  Already-authorized validation performs no probes or writes. Fullscreen launch
  review uses fast validation, never evidence of run-birth readiness.
  Preparation and execution recheck live readiness immediately before run
  birth, and all diagnostics after birth come from persisted run state.
- **Static Prerequisite Boundary**: Host prerequisites may inspect binary
  presence, environment presence, and paths only. Legacy executable command and
  binary-version fields are parseable for migration but fail with
  `legacy_executable_prerequisite_unsupported` before any process starts.
- **Plugin Readiness Boundary**: Plugin readiness uses the APP handshake and
  advertised APP health operation, not a prerequisite-owned process probe.
  Invocations with equivalent adapter, plugin, action, runtime context, and
  authority may share one live probe. Only top-level `step_id` and
  `workflow_id` metadata are identity-only for grouping; unknown metadata is
  authority-significant by default. Kernel and persisted preflight evidence
  retain every associated step id in insertion order.
  Process and MCP probes may overlap only when their constructed local sandbox
  plan proves a read-only workspace and plugin package, no shared host writes,
  private invocation staging and runtime temporary roots, and denied network.
  The bounded scheduler preserves stable check order and earliest-error
  selection. HTTP and every authority-bearing or otherwise unproven probe stay
  sequential; plugin startup is never assumed to be side-effect-free. The
  current macOS sandbox plan is conservatively treated as ineligible.
- **Action Schema Boundary**: Plugin input and output schemas are compiled during
  plugin validation. Adapters validate materialized input before transport work
  starts and validate successful output before hints or result data are handed
  to the kernel. Invalid instances produce `validation_error` results with only
  schema and instance paths in diagnostics.
- **Deferred Action Data**: Invoke `with`, inline prompts, and module inputs may
  consume recipe-local `{{steps.<id>.out.<field>}}` values. Whole-value bindings
  preserve JSON types recursively; interpolation accepts scalars only. Named
  module exports become the successful module call's output. Compiled recipes
  retain explicit binding locations and selectors. Preparation validates known
  input portions without fabricated instances and records input-dependent checks
  as `deferred_to_dispatch`. This is nonblocking only when static contracts,
  pinned authority, policy, prerequisites, secret availability, and delivery
  requirements pass, including reachable nested modules. The complete request
  is validated before the consumer's first live probe or action effect; its
  deferred readiness probe runs immediately before invocation. Output selection
  requires a current successful producer result with matching run, step and
  execution identity. Missing, skipped, failed, in-flight, or reset-loop outputs
  fail the consumer durably; earlier execution history is never substituted.
  Output text is opaque data, never recursively interpreted as templates.
  Authority, executable identities, `prompt_file`, and `source://` locations
  cannot depend on step outputs. Ordinary deferred child inputs remain symbolic
  at parent birth; nested aliases into authority are rejected. Reused children
  reject changed immutable inputs; authors must select `rerun_mode: fresh` for
  new bindings. Existing `set` completion and `vars` behavior remain available.
  Prospective reachability treats deferred module inputs as unknown operands,
  retaining both possible branches while preserving known Boolean short-circuits.
  Guarded authority, secret, and static prerequisite blockers are checked before
  birth and before live probes. Partial schema validation evaluates composite
  constraints recursively: an unrelated unknown value cannot hide a known failure
  in `anyOf`, `oneOf`, conditionals, or container structure. Existing child runs
  compare only birth-known inputs during a symbolic preview; dispatch resolves and
  compares the complete current input set before reuse. Numeric selectors combine
  every applicable object/array schema possibility rather than preferring one kind.
  Partial unevaluated-member checks distinguish definite, possible, and absent
  evaluation coverage; known forbidden members fail before birth. Numeric const,
  enum, and uniqueness comparisons recurse using mathematical numeric equality
  without rounding integer identities through floating point. A known reused-child
  input mismatch is an explicit failed preflight finding: resume persists a failed,
  recoverable run without dispatch instead of escaping with a running projection.
- **Capability Boundary**: Before secret lookup or adapter dispatch, the kernel
  derives one typed authority projection from the pinned recipe and effective
  filesystem/network policy. The projection records normalized
  `CapabilityRequest` values and source dependencies, rejects mutable
  variables, step outputs, and other post-birth dependencies in authority
  fields, and persists the resulting map at run birth. Preflight, dispatch,
  retry, resume, cleanup, and child execution select only the exact persisted
  checkpoint subset. Missing, partial, malformed, or mismatched authority
  fails closed before secret lookup or transport; manual gate request and
   decision operations are persisted and evaluated separately.
  Local policy may use concise version-pinned selected-action plugin grants, but
  those grants are evaluated only against the exact persisted checkpoint subset;
  they do not replace or persist the authority projection.
- **Manual Gate Identity And Decisions**: An authored `gate_id` is used verbatim
  (never trimmed) and must satisfy the identifier rule; a whitespace-only,
  empty, or non-string value fails before run birth rather than silently falling
  back to the step id. Persisted gate decision records validate their schema,
  step id, and decision value on load; an unknown or tampered decision fails
  closed instead of being treated as a pass. A consumed decision is removed
  through a descriptor-anchored delete with directory sync, so it can never
  reappear after a crash.
- **Child Authority Preparation**: If an embedded child needs authority from a
  child input that cannot be bound immutably, preparation returns a structured
  child-authority failure. The parent birth is aborted and no child run is
  created; ordinary child-input errors that do not affect authority retain
  their structured preflight behavior.
- **Protected Workspace Boundary**: The kernel treats `ExecutionRequest.paths.workspace_dir` as the default user-visible root. It rejects runtime-resolved paths outside that workspace before handing work to an adapter and records the denial durably. The kernel rejects `workspace_access.mode: custom` at compile time, preflight, and adapter dispatch with the uniform `filesystem_custom_mode_unsupported` reason code on both Linux and macOS; only `full` and `read_only` workspace access modes are supported.
- **Kernel-Owned Orchestration**: All control flow (loops, guards) and subflow logic is owned by the Kernel and durably reflected in the run state.
- **Explicit Loop Routing Metadata**: Recipes may carry graph-level
  `loop_routing_version: 1` and edge-level `loop_route` records identifying a
  recipe-local wrapper and `enter`/`exit` role. Recursive validation checks every
  marked wrapper against its ordered body and normal successor before graph
  consumption, including embedded main and cleanup recipes. Unsupported versions,
  explicit null/malformed metadata, unversioned edge records, and incomplete or
  conflicting topology fail closed. Loading never infers or repairs metadata.
  Omitted fields retain legacy routing and serialization. Newly compiled graphs
  containing authored loops emit version 1. For a successful marked owner whose
  activation-local execution count reaches `max_iters`, entry is blocked and the
  normal exit is satisfied, regardless of the predicate. Below the cap, guards
  read current variables/results; unresolved references or malformed predicates
  authorize neither branch. Failed/canceled owners never gain successful exits.
  The existing counting convention remains: a constant-continuing loop with cap
  N executes its wrapper N times and body N−1 times. A normal exit is not approval.
- **Typed Execution Phases**: Loaded-run execution uses consuming internal handoffs (`RecoveredLoadedRun`, `ActivatedLoadedRun`, `PreflightedLoadedRun`, and `ScheduledLoadedRun`) with explicit canceled and preflight-finished outcomes. These are implementation boundaries only: they add no persisted fields or events and preserve persisted bytes, event ordering, dispatch order, and every durability boundary.
- **Kernel-Owned Protected Mutation**: Recipe snapshot synchronization and persisted trusted-execution refresh are kernel operations. The root application supplies the exact compiled recipe source bytes and composes the trust-decision resolver; the kernel validates the input, acquires the root-tree mutation lease, persists the protected run-state changes, and releases the lease.
- **Kernel-Owned Gate Mutation**: Manual-gate decisions are validated by a kernel use case and persisted through the run-store port. Local storage owns gate-directory creation, active-record replacement, and immutable history-file durability; application code does not write protected gate or control-request state directly.
- **Subflow Inputs Bind Before Child Creation**: Embedded subflow steps bind child inputs against the child recipe before creating the child run. Declared child defaults are materialized into the persisted child run inputs, and invalid child-input bindings fail the parent subflow step without creating a child run record.
- **Root Preflight Surfaces Nested Child Blockers**: Prepare-time and manual preflight walk reachable embedded subflows and persist child invoke blockers on the root run's `preflight.checks[]` list using parent-prefixed step ids such as `plan_loop/generate_plan`. When a child run already exists, preflight may inspect that persisted child snapshot while rebinding child workflow inputs from the current parent mapping before evaluating nested blockers. This is additive inspection metadata only; it does not change child-run persistence layout or execution control flow.
- **Read-Only Inspection Composition**: The companion server consumes the root facade's inspection and metrics ports and maps typed persisted records into typed `/api/v1` responses. Workflow-file responses expose opaque identifiers derived from stable host-independent locators and workspace-relative labels instead of absolute host paths. The configured server workspace root is authoritative over untrusted persisted path roots. The server has no runtime-home mutation surface; local persisted state remains authoritative.

### 2. State & Mutation Ownership

- **Managed Document Publication**: Root-lexical `documents` declarations are data
  storage contracts, separate from action input and capability authority. Root
  birth atomically commits an immutable initialization record, its declaration
  digest, and literal UTF-8 file initial revisions (execution/sequence zero).
  There is no external initial-content path. Contained runs share the root
  namespace and blob pool without repeating initialization. Under the existing
  root-tree lease, bounded capture stores exact bytes and an immutable candidate
  batch for the active `(run, step, lifetime execution)`; candidates and orphaned
  blobs are unpublished. A successful immutable execution record publishes the
  entire candidate array atomically before any step/cleanup state projection or
  latest-head update. Failed, canceled, blocked, and interrupted records carry
  no revisions. Main and cleanup use the same completion/recovery hooks and
  ordinal guards; ambiguous attempts are interrupted and replacements use N+1.
  Reads validate producing identities, revision/content digests, sizes, kinds,
  manifests, and candidate/commit agreement. Runtime-owned publications may retain
  a temp hard-link alias after a durable link and process death; anchored readers
  validate their bytes/records without deleting aliases. Untrusted capture sources
  still require single-link regular files. Reconstruction validates every consumed
  main/cleanup lifetime ordinal, including failed/reset activations with no result;
  only the latest genuine in-flight attempt may lack its record. Missing history
  fails closed rather than selecting an older document head. Heads are a rebuildable atomic
  projection; committed records alone select latest revisions by root-tree
  publication sequence. Raw or recognized encoded runtime-known secrets in
  document content or metadata, including exact canonical manifest bytes, cause
  whole-batch rejection before blob persistence, never byte redaction. Trees
  admit only regular files/directories, reject symlinks/hard links/special files
  and unsafe member paths, and enforce aggregate byte, file, structural directory
  and depth bounds. Tree capture retains anchored file/directory witnesses and
  revalidates identities, sizes, ctime/mtime and inventories across the complete
  capture before publication. Resume retains committed history; replay initializes its own
  namespace from literals and reruns producers, never copies source revisions.
  The storage port exposes logical revisions and exact bytes, not durable paths.
   Recursive reserved `{$document: name, access: read|write|update}` objects are
   accepted only as compiled process invoke action data. Recipes retain explicit
   logical input pointers and access requirements, inherited from root declarations
   through modules. HTTP/MCP document paths, multiple writers of one name,
   authority/executable/source/prompt-file/workspace bindings, and binding objects
   introduced through runtime data are rejected. Read bindings may additionally
   require a recipe-local manual gate with `reviewed_by` as specified below.
   Read stages latest committed content as a sandbox-enforced read-only private
   copy; update stages a writable copy; write supplies a new absent output location.
   Private read-copy ancestors cannot be renamed to move inputs out from beneath
   their protection; output locations remain independently writable.
   Unpublished read/update fails before transport. Copies live in this invocation's
   private temporary root outside the durable home, never as links to stored blobs.
   Known schema types must admit a string path; path-dependent checks and probes
   are explicitly `deferred_to_dispatch`, while authority, policy, prerequisites
   and secret availability still run before birth. The full staged action request
   is validated before any consumer probe or transport. Only schema-valid successful
   results are captured. Every declared output must exist and pass the complete
   bounded batch/secret/snapshot validation before any batch blob is persisted.
   Main and cleanup share this boundary and the existing execution-record commit.
   Durable requests retain logical bindings; result path values become logical
   objects and embedded path text uses `$document:<name>` labels. Promoted text
   metadata is normalized before redaction; exact business document bytes are never
   rewritten. Full private staging prefixes are normalized or redacted, never a
   shared parent/home prefix. No new filesystem grant or workspace authority is
    created by a document binding.

- **Document Review Binding**: Only manual gates accept literal
  `with.review_documents` names. Their first blocked immutable execution commits
  a `transist.gate_review_request.v1` request containing the exact committed
  revisions (IDs, digests, names, sizes and producing identities). Unpublished
  documents fail the gate durably. Waiting re-entry references the same request,
  not newer heads. Decision creation under the root-tree lease copies that
  runtime-owned request into active and immutable history records; external
  request fields cannot replace it. A read binding's `reviewed_by` names a gate
  **step in the same recipe**, independent of its authored `gate_id`. Root
  documents are shared across contained runs; gate authority is not inherited.
  Before private input copying, full request validation, the first live probe or
  transport, the kernel requires a current successful gate execution, its exact
  immutable approved decision and history, and coverage of the selected revision
  **ID and digest**. A new revision with identical bytes still needs approval.
  Reset loops/child activations cannot reuse earlier approvals; consumed active
  decisions remain inspectable in immutable history. Rejections never authorize
  consumption. Selection and staging hold the existing exclusive root-tree lease,
  preventing another runtime writer between approval checking and copying.
  Review binds documents only, never arbitrary workspace mutations. Inspection
  resolves exact committed revision/member content through storage ports and
  opaque run-scoped links under strict physical root lineage; replay ancestry is
  never a document namespace. The server stays read-only and plugins never gain
  durable home paths or access.
  Review inspection admits only validated committed requests, excluding authored
  summary-key substitutions. It reconciles full active/history/completion decision
  contents, not just request identity, and fails closed on contradictions or
  missing history. Empty declaration namespaces remain valid no-match lookups;
  root/parent lineage validation still applies and declared initialization cannot
  be silently omitted.

- **Durable State**: Run and step states are written to `state.json` as an atomically replaced current-state projection. The run state file is never the source of append-only history; immutable execution records, execution-scoped artifacts/evidence, and append-only `events.jsonl` / `failures.jsonl` streams carry history. Run birth stages every artifact (initial state, recipe snapshot, run inputs, provenance, and any captured workflow assets) in a hidden directory and atomically renames the directory to the final run root only after every file is flushed and synced, so a crash before the rename exposes no run and a crash after the rename exposes a complete run. The kernel caller publishes the run locator only after the final run root is durable. If publication is ambiguous, recovery validates all birth files, including run inputs and workflow-asset blobs, and validates an existing locator when present before resyncing both rename-parent directories; missing or differing files reject the attempted birth. Run absence is a distinct storage outcome: a run with no published locator is absent, while a locatable run whose `state.json` is missing, unreadable, invalid, or corrupt fails closed as an error and is never reported as "not found". Birth reconciliation of `state.json` and `provenance.json` is structural (timestamp-derived top-level fields and fresh preflight's recomputed `checked_at_ms` are excluded; every other field including nested step state and preflight checks must match exactly), while `recipe.yaml`, `context/run_inputs.json`, and the run locator remain byte-exact.
- **Crash-Durable Streams**: Every append to `events.jsonl` and `failures.jsonl` flushes and `sync_all`s the file and, on first creation, syncs the exact containing directory. Runtime-owned artifact and evidence files are written through a temp file, `sync_all`, atomic rename, and a sync of the exact containing directory. Newly created directory entries are synced from the leaf to the first existing ancestor. A successful kernel transition returns only after required durable writes complete. On inspection, one malformed non-newline-terminated final JSONL fragment is ignored; on the next append under the run lease the fragment is truncated to the last newline, flushed, `sync_all`ed, and a durable `storage.tail_repaired` recovery event is appended to `events.jsonl`. When the repaired stream is `failures.jsonl`, its recovery note is appended to `events.jsonl` only after `events.jsonl` itself has been repaired, so simultaneous truncation of both streams leaves both repairable and fully inspectable. Malformed complete or interior records (newline-terminated but unparseable) are rejected without repair.
- **Append-Only Execution History**: Every step execution record, artifact, evidence file, failure record, and gate decision is durable for the lifetime of its run. Retry, resume, replay, cleanup, and cancellation never delete prior step state, execution records, artifacts, evidence, failures, or gates. A rerun under the same run id (including a reused subflow child) never resets an existing step execution counter to zero; the next dispatch starts at `N+1` and prior executions remain inspectable in ascending order. Use `rerun_mode: fresh` on a subflow step to request a distinct child run id with fresh counters when a from-scratch child rerun is required. The immutable per-execution record is the completion commit point: it is persisted before the terminal step-state projection, so a crash between result promotion, the execution record, the state projection, the failure append, and the completion event is reconciled on re-entry from the already-committed record. A committed result is never dispatched again; an ambiguous in-flight execution ordinal that lacks a completion record first receives an immutable `interrupted` record, and the replacement dispatch begins only at `N+1`. The record persists a committed retryable failure's retry deadline, so a committed retryable failure is reconciled as `RetryWait` (with its deadline) rather than degraded to a plain `Failed`. Recovery backfills a completion/failure event that never reached `events.jsonl`, deduplicated by `(run, step, execution, kind)`. Cleanup execution follows the same rules: the immutable record is the cleanup completion commit point (persisted before the cleanup run-record projection), cleanup steps reconcile committed records without re-running them, ambiguous in-flight cleanup ordinals receive an `interrupted` record, and cleanup dispatch rejects overwriting an existing ordinal. A step showing as in-flight with a zero execution counter never consumed an execution, so recovery creates no spurious `execution_000` record; loaded execution records are validated against the requested `(run, step, execution)` identity.
- **Activation-Local Loop Budgets**: The lifetime execution counter drives execution record paths and artifact directories. Loop budgets (`max_iters`), scheduler cyclic-entry bootstrap, author-facing `executions` expressions, and retry limits use an activation-local count (`executions - activation_executions_baseline`) so that a reused looping child can rerun its internal loop from scratch on each parent re-dispatch without exceeding `max_iters` from a prior activation. The activation baseline is additive (defaults to zero for pre-existing records), is set only when a full rerun resets the step, and is itself monotonic and bounded: it never exceeds the step's lifetime `executions` count and never regresses below its previously persisted value. Writes that violate either invariant are rejected, and a persisted record that violates `baseline <= executions` fails closed on load instead of being silently saturated to zero.
- **Secret Non-Persistence**: Runtime-known raw secret values and their standard or URL-safe base64 forms are redacted before any request result, error, event, state, failure, artifact/evidence metadata, or inspection record becomes durable. This is not a DLP guarantee for arbitrary plugin transformations or external exfiltration.
- **Invocation Isolation**: Process and MCP plugins receive only the workspace,
  plugin package, private invocation staging, and a separate runtime-owned
  temporary root. `runtime://temp` or `runtime://temp/<relative-path>` is an authored process/MCP
  `entrypoint.env` declaration, not a host-path grant or persistent path. The durable Transist
  home and run tree are never exposed or mounted. Runtime metadata is created by
  the kernel before dispatch, and preflight uses a disposable runtime temporary
   root under the canonical system temporary parent that is separate from its
   protected invocation-staging root. Caller `TMPDIR` cannot select
   that parent: local invocations consistently use canonical `/tmp` (Linux) or
   `/private/tmp` (macOS). A parent covered by workspace, home or external grants
   is rejected before staging creation; sandbox plans additionally reject private
   roots overlapping exposed workspace/package/staging/grant paths. Placement
   checks canonicalize home/workspace as literal filesystem paths, preserving
   valid native names such as brackets/question marks; external grants alone use
   grant grammar. Source-directory
   exceptions may not overlap any protected temporary/staging/home parent, so they
   cannot reopen sibling invocations; captured `source://` assets stay available
   through the current private staging root. The entire `runtime://temp/documents`
   namespace, including case aliases, is runtime-reserved. Manifest validation and
   dispatch reject such declarations, traversal and symlink aliases before any
   environment directory is created. Preflight confirms secret availability
   without resolving or delivering a secret value. Linux mounts
  only declared runtime roots; macOS denies reads from the Transist home,
  shared staging parent, and general temporary parent except for the exact
  canonical package, workspace, declared external paths, current staging root,
  and temporary root authorized for the current invocation. Metadata lookup of
  each protected parent remains available so authorized descendants can be
  traversed, while sibling listing and content reads remain denied. External
   grants that cover the temporary parent, reserved private children, or any
   runtime home/staging root are rejected. Exact unrelated user paths beneath
   the temporary parent remain grantable; they never reopen that parent or
   sibling invocations. Linux home-relative grants mount only authorized home
   children under an empty read-only parent, never the whole host home. Exact
   lexical aliases are mapped from their validated canonical resources without
   copying host symlinks or exposing host ancestors. Both canonical targets and
   lexical locations (including resolved parent aliases) must remain outside
   private roots and cannot reopen workspace authority. Strict
   network setup maps the operator to namespace uid/gid 0 so the launcher works
   for non-root operators, then clears every capability before plugin exec;
   the mapped identity never grants host-root credentials. Process and MCP
  execution and preflight force `TMPDIR` to the
  declared temporary child or root after environment processing. Temporary roots
  are removed on ordinary unwind and stale owner-marked roots are reaped; their
  resolved host paths are never persisted.
- **Invocation Metadata Identity**: The private `TRANSIST_RUNTIME_METADATA_FILE`
  JSON includes additive `run: {id: ...}` copied exactly from the owning
  `ExecutionRequest.run_id`. Contained runs expose their current child identity;
  root-ledger initialization reads the root invocation once and shares that
  document. The ID is data, not authority, a filesystem locator, or a root-lineage
  assertion. No home/run path, inputs, policy, grants, or secret values are added.
  The existing unversioned file and staging lifetime remain; publication retains
  normal secret rejection/redaction and document isolation.
- **Lease-Based Mutation**: A root-tree lease authorizes an exclusive mutation session. Execution commands (`run`, `resume`, replay, and cleanup) hand work to a detached **runner process**; synchronous mutation commands such as prepare, preflight, gate decisions, and test-recipe synchronization hold the lease in the foreground process. A durable operation remains successful when lease release fails; every mutation entry point returns a structured `lease_release_warning` alongside the committed result.
- **Bounded Run-Birth Serialization**: Fresh root, prepare, launch, replay, and
  embedded-child births acquire the shared plugin-lifecycle reservation before
  resolving plugins or assets and retain it through staged creation and locator
  publication. A birth tries immediately, then every 100 ms for no more than
  15 seconds; persistent contention reports the current owner metadata without
  inspecting to break, reap, replace, or overwrite that lease. Plugin install
  and uninstall remain single-shot. A child checks its durable root cancellation
  before its first attempt and each retry; cancellation produces a canceled
  subflow result without creating a child run or a `subflow_start_failed`
  result.
- **Process Agnostic**: The Kernel doesn't care about the foreground process. If your CLI crashes, the detached runner continues until it hits a terminal state or a manual gate.
- **Idempotent Cancellation**: Cancellation requests are persisted to disk. Multiple operators can request a cancel safely; the system honors the request before dispatching the next piece of work. The CLI cancellation path immediately terminates a verified active local invocation when present and, when no live root-tree lease owner remains, starts one root runner to consume the durable request and settle kernel-owned state.
  Repeated `runs cancel` during cancellation cleanup is idempotent: it does not
  interrupt cleanup invocations, duplicate cleanup dispatch, or start a competing
  runner.

### 3. Persistence, Recovery, and Replay

- **Snapshots at Birth**: At creation, every run gets a full copy of its recipe, inputs, and initial state. Fresh snapshot-backed runs that resolve source-backed prompt or shell `source://` assets from direct recipe steps also atomically retain private content-addressed captures under `workflow_assets/`; dispatch, preflight, cleanup, resume, and replay consume those captures rather than the authored source directory. Current captures are identified by `asset_lock_version: 1` plus the `workflow_asset_snapshot: true` marker (default false, omitted when false). Unmarked version-1 records use authored-source semantics. Required captures are validated and missing or corrupt captures fail closed, never falling back to source or inferring semantics from disk presence. Snapshot-backed replay copies the captures into a new version-1 marked run, including child runs. Pre-lock version 0 remains rejected for re-entry. Asset-lock and capability-authority revisions above 1 are unsupported and rejected before re-entry, preflight, replay, cleanup, dispatch, or durable state mutation; the snapshot marker and test mode cannot override this rejection. Capability-authority zero retains its existing legacy/synthetic handling, without weakening current authority validation.
- **Revision Rejection Scope**: Rejection is per unsupported record, not an atomic validation of the entire run tree. Ancestor work may precede discovering an unsupported descendant. Filesystem recipe-sync recovery validates both embedded old/new state revisions and matches the current state bytes to a validated pair before repair writes or intent deletion. Unsupported revisions preserve that record's state, recipe, and pending intent unchanged.
- **Filesystem Policy Persistence**: Effective filesystem policy is resolved when a run is prepared or started, persisted into `recipe.yaml` and step requests, and then reused unchanged by `run`, `resume`, and `replay`. Re-entry rejects workspace-root authority drift; replay is the deliberate path for a new authority snapshot.
- **Declared Workspace Persistence**: A fresh run resolves its authored
  `workspace.path` from immutable inputs to one canonical directory, persists it
  in recipe and state, and uses it for projection, preflight, dispatch, cleanup,
  and recovery. The workflow source and CLI root are not workspace authority.
  Child subflows inherit the same path and can only narrow access. A replay
  preserves this snapshot; changing `repo_path` requires a fresh run.
- **Subflow Failure Attribution Is Durable**: When a subflow step fails because a child run failed, child input binding failed, or child birth was denied, the parent step persists a terminal failure result with an additive `last_result.output.failure` object. It carries the compact first-line reason, a pointer to the descendant or intended child run, and optional child-preflight summary counts for failures that happened before dispatch. This metadata is inspection-only and does not alter control flow.
- **Recoverable Failure**: A failed root without matching cleanup remains
  `recoverable`; `retry` (alias `resume`) continues primary work under the same
  run ID. Matching failure cleanup begins automatically and durably records
  `cleanup_required` before cleanup preflight or dispatch. Once it begins,
  retry/resume remain unavailable; successful or skipped completion records
  `cleanup_completed`. Use `runs cleanup` to recover unfinished or failed cleanup,
  and replay for new primary work from the pinned snapshot.
- **Boundary Failures Are Terminal**: Canonical `protocol_error` and `adapter_internal_error` never enter automatic retry wait, even when `retry_max` remains. This applies to main and cleanup executions; explicit operator recovery remains available. Adapters own process lifecycle validation and its evidence, not the kernel.
- **Lineage in Replay**: `runs replay` creates a *new*, independently leased root run from an old recipe. The replay's `source_run_id` links it to the source in lineage history, while its `root_run_id` is the new run id; it starts fresh from the beginning.
- **Behaviors That Must Not Drift Silently**: Registry recipes are content-immutable at `<workflow-id, version>` except for the reserved test version `v0`, fresh runs capture resolved source-backed prompt and shell `source://` assets under private `workflow_assets/` storage, run records persist asset locks for pinned plugins and authored `prompt_file` references, re-entry verifies `prompt_file` locks before dispatch even for `v0`, normal workflow versions also verify pinned-plugin locks, trusted workflow installs retain an explicitly selected detached-signature source for future verification (or use the conventional adjacent sidecar when none was selected), the plugins install index is immutable at `<plugin-id, version>`, and legacy pre-lock runs are rejected for re-entry instead of being silently trusted.

### 4. Artifacts & Evidence

- **Execution Scoping**: All logs, files, and evidence produced by a plugin are stored in a directory scoped to that specific execution. We never overwrite earlier results, even during a retry or a reused subflow rerun.
- **Private File Promotion**: Adapters receive mode-0700 per-invocation artifact and evidence staging directories outside the final run tree. The kernel rejects missing, non-regular, symlinked, escaped, or secret-bearing paths; text files are redacted, binary files containing a known secret are rejected, accepted bytes are re-hashed, and only then are files atomically promoted. A retry may reconcile an existing regular artifact/evidence file only when its bytes are identical; differing content, symlinks, and directories remain errors, while immutable execution records remain duplicate-rejecting.
- **Permanent Audit Trail**: Artifacts and evidence remain available for inspection even if the run ultimately fails, and no rerun, retry, resume, replay, cleanup, or cancellation path removes them.
- **Read-Through Inspection Cache**: Inspection surfaces may use a process-local, in-memory read-through cache for performance, but persisted on-disk state remains authoritative and cache entries are always derived from file fingerprints. Small run-summary projections use content fingerprints over the same captured bytes parsed on a miss, so independent readers detect same-length atomic replacements even within one filesystem timestamp tick. Summary rebuilds bypass cached state/recipe/blob inputs, including caches warmed by other readers, before publishing their derived projection. Unchanged summary reads reuse parsed records; artifact/evidence payloads are not content-hashed for summary lookups.

---

## What We Do NOT Guarantee in v1

To be clear about the limits of the v1 system:

- **External Determinism**: We guarantee *our* logic is deterministic, but we cannot guarantee that your shell scripts, APIs, or LLMs will return the same result every time.
- **Exactly-Once Side Effects**: If a crash happens mid-execution, we cannot guarantee that an external tool didn't already perform a side effect before the Adapter could record the result.
- **Multi-Node Scheduling**: V1 is designed for local execution. We do not currently support distributed coordination across multiple hosts.
- **Mutable Snapshots**: You cannot change a run's recipe, inputs, or authority projection once it has started. If you need to change the workflow, workspace authority, or immutable input-bound secret reference, use replay to create a new snapshot.

---

## Verification & Protection

This contract is not just a document; it is enforced by our core test suite. Any change that violates these guarantees will fail our release gate:

- `tests/runtime_critical_paths.rs`
- `tests/runtime_failure_path_regressions.rs`

---

## Related

- [Compatibility and Stability](../../public/reference/compatibility-and-stability.md)
- [Storage Layout Reference](../../public/reference/storage-layout.md)
- [Determinism and Auditability](../../public/handbook/04-trust-and-runtime-behavior/01-determinism-and-auditability.md)
