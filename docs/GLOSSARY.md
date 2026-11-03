# Transist Glossary

This glossary defines the core terms used throughout the Transist product and documentation. Whether you are evaluating Transist, writing workflows, operating local runs, or extending Transist with plugins and adapters, these definitions provide the shared language.

---

## Action

A specific operation provided by a **Plugin**. Think of it as a single tool in a toolbox (e.g., "Write File" or "Post to Slack").

## Action Schema

The JSON Schema Draft 2020-12 input or output contract declared by a plugin
action. Transist compiles both schemas during installation and validates
materialized input and successful output at the adapter boundary.

## Action Input Projection Binding

The optional action binding `input_projection: action_data_v1` in a plugin's
existing `bindings` map. It selects an adapter-owned clone of action input
with only top-level `secret_refs` removed for schema validation and transport.
Nested references and every other key remain; absent binding preserves legacy
bytes. It never removes references from the original authority-bearing request
or relaxes secret/capability policy. Unknown or malformed values fail closed.

## Adapter

The execution mechanism for a plugin. Adapters bridge the runtime kernel and an execution participant. Built-in adapter kinds include `process`, `http`, and `mcp`.

## APP (Adapter Plugin Protocol)

The "final mile" protocol spoken between an Adapter and the Plugin it is triggering.

## Artifact

A durable output of a workflow step, such as a compiled binary, generated report, stdout capture, or deployment manifest.

## Authorization

The operator-reviewed mutation that writes one exact, no-clobber workflow
policy fragment for missing capabilities reported by a prospective readiness
check. Authorization neither installs code nor creates a run; rerun
`workflows check` after it succeeds.

## Authoring Explanation

A deterministic `workflows explain --path` view of captured authored definitions,
compiled control/dataflow and actual locally resolved plugin contract pins. It
labels missing inputs/contracts and opaque external behavior, and can compare
structural content with a previous definition. It creates no revisions or
approvals and is not validation, authorization, readiness, or execution consent.

## Conversational Authoring

Using an existing assistant and the checked-out authoring skill to capture intent,
clarify missing decisions/bindings, inspect real contracts, generate source,
validate it and present deterministic explanation/comparison. It creates no hidden
runtime planning loop. See the [authoring guide](public/guides/authoring/ai-assisted-authoring-with-repo-skills.md).

## Execution Approval

A separate operator decision permitting the specified execution scope, systems,
data and effects. A goal-authoring request, source check, explanation or fake-test
authorization is not approval for real publication, network/secret use, policy
writes or signing. Authored manual gates require their own later run decisions.

## Companion Inspection UI

The web-based user interface hosted by the **Companion Server** at `/inspect`, providing a graphical view of runs, steps, events, and artifacts.

## Companion Server

A local, read-only web interface (`transist-server`) that lets you inspect active runs, view artifacts, monitor system metrics, and browse the **Companion Inspection UI**.

## Companion API OpenAPI Contract

The machine-readable OpenAPI 3.0 contract at `schemas/http/companion-api.v1.openapi.yaml` that declares every `/api/v1` route, parameter, response, pagination envelope, content response, and error exposed by the **Companion Server**. It is kept in bidirectional parity with the registered router and the route inventory in `crates/transist-server/src/infrastructure/http_server/routes.rs`; every operation is `GET`.

## Deferred Binding

An explicit action-data dependency whose value arrives during workflow execution,
such as `{{steps.fetch.out.records}}`. The recipe records its selector and input
location. Validation reports `deferred_to_dispatch` for input-dependent work;
policy, authority, prerequisites, and secret checks still run before preparation.
Complete input validation and deferred probes run before consumer invocation.

## Managed Document

A root-declared bounded `file` or `tree` whose exact intermediate content is owned
by one runtime root-run namespace outside the workflow workspace. Modules inherit
declarations; replay has a separate namespace. Declarations are data storage
contracts, not filesystem grants. Process actions receive invocation-private copies
through explicit document bindings; durable storage is inaccessible to plugins.

## Document Binding

A reserved `{$document: name, access: read|write|update}` object recursively inside
process invoke action data. It compiles to a logical input location and access
requirement, then materializes as a string path outside the durable home. Read
copies are sandboxed read-only; write/update publish only with successful execution.
It grants no workspace authority or review approval. Durable metadata retains
logical references rather than temporary paths; HTTP/MCP bindings are unsupported.
Optional `reviewed_by` on a read requires the current recipe-local gate's approved
exact revision ID and digest before copying or probing.

## Document Revision

An immutable, content-addressed document reference attributed to a producing run,
step and lifetime execution ordinal, with digest and size. The successful immutable
execution record publishes all of one invocation's revisions atomically; root
birth publishes literal initial revisions. Candidate blobs do not establish a revision
head, and known-secret-bearing content is rejected without altering business bytes.

## Document Head

The latest committed revision of one logical document. The local `heads.json` file
is a rebuildable projection, not authority: recovery and reads derive heads from
committed initialization/execution records, never from another run's ticket or workspace.

## Document Review Request

An immutable manual-gate request (`transist.gate_review_request.v1`) committed in
its first waiting execution. It identifies the run, recipe-local gate step, gate
ID, original execution and exact reviewed revisions, digests and sizes. Decisions
retain that same request in immutable history. Waiting restart does not follow
new document heads; a new revision requires new approval, even for identical bytes.
Document review is not approval of an arbitrary workspace snapshot.

## Module Export

A named value declared in a package-local module's `outputs` map. It is resolved
from the child's current successful execution and persisted in the parent call's
output, accessible as `{{steps.<module-call>.out.<export>}}`. It does not expose
undeclared child data or confer execution authority.

## Evidence

Supporting data saved during a step to explain how execution happened, such as logs, tool call records, approval records, error records, or intermediate API responses.

## Event

A timestamped record in a run's permanent stream, capturing every state change and significant action the system performs.

## Execution

A single attempt to run a specific **Step**. If a step is retried or resumed, each attempt is recorded separately.

## Execution Participant

An external agent, CLI, API, MCP server, shell script, or internal tool called by a workflow. Participants perform work; Transist governs the workflow run.

## Requested Capabilities

The exact normalized capability subset the kernel evaluated for one invoke, carried on TEP and APP invoke requests as `{ capability, resource, operations }`. It is observational: it does not grant authority and cannot widen host-side policy.

## Failed Step Inspection

Detailed inspection of a failed workflow step surfaced through CLI pagers and the TUI, including its stderr output, runtime events, and associated artifacts.

## Gate (Manual Gate)

A deliberate pause in a workflow that requires a human to review the progress and provide a "Go/No-Go" decision.

## Host Prerequisite

A static readiness check for binary presence, environment presence, or path
presence. Executable command and binary-version probes are legacy fields that
fail with `legacy_executable_prerequisite_unsupported` instead of starting a
process.

## Inspection

The subsystem responsible for read-only observation of runs, steps, events, gates, artifacts, and evidence, exposed via the **Inspection Surface**.

## Inspection Cache

A read-through LRU cache over persisted run state, events, and blob listings that accelerates repeated inspection queries, controlled by the `TRANSIST_INSPECTION_CACHE` environment variable.

## Inspection Surface

The set of read-only interfaces for observing runtime state: the CLI (`transist runs show`, `transist runs list`, `transist runs tail`), the **Companion Server** HTTP API, the **Companion Inspection UI**, and the interactive TUI.

## Invocation Staging

A private mode-0700 artifact and evidence directory outside the final run tree. Adapters write there for one invocation; the kernel validates, redacts, hashes, and atomically promotes accepted files into durable execution-scoped storage.

## Runtime Temporary Directory

An invocation-private mode-0700 directory selected by a complete process or MCP
`entrypoint.env` declaration of `runtime://temp` or
`runtime://temp/<relative-path>`. It is separate from invocation staging,
workspace paths, artifacts, evidence, and `run://` scratch. It grants no host
authority, is sandbox-authorized only for the current invocation, and is removed
after dispatch or preflight; durable data retains the authored URI, never its
resolved host path.

See [Path and Resource Semantics](public/reference/path-and-resource-semantics.md)
for the complete distinction between runtime capability resources, action path
conventions, workflow assets, and manifest-only declarations.

## Plugin

A versioned integration package that exposes an execution participant's
**Actions** to Transist. A plugin is not necessarily a small in-process module:
the participant may be a script, CLI wrapper, multi-file program, standalone
HTTP service, or MCP server. The installed package pins its manifest, action
schemas, entrypoint or service location, and selected local assets in the
**Transist Home**. The participant conforms to APP over `process` or `http`, or
to the supported MCP tools protocol mapped into APP semantics by the `mcp`
adapter.

## Plugin Grant

An operator policy shorthand that authorizes explicit actions from one exact
plugin version. It covers only the matching invocation's plugin, action,
adapter, and provider capabilities; secrets, filesystem, network, and approval
authority remain exact capability grants.

## Grant Set

A named, operator-authored collection of plugin grants and exact capabilities.
An exact workflow-version binding selects grant sets; workflow inputs cannot
select or widen them.

## Policy

The security layer that decides whether sensitive operations—like delivering a secret to a remote server—are allowed based on the user's trust settings.

## Public Beta (Pre-1.0)

The Transist `0.y.z` release line. Versioned public surfaces are protected in the current minor line; stability begins only at `1.0.0`. Patch releases do not intentionally break documented surfaces except an urgent security correction with migration guidance; minor releases may break documented surfaces with changelog, release-note, and migration guidance. Workspace crates are implementation units (`publish = false`); the only supported distribution is the signed binary archive.

## Versioned Public Surface

A schema, route, or contract identified by a `v1` (or `vN`) format version. `v1` is a format version, not a SemVer 1.0 stability promise. Versioned public surfaces are protected in the current minor line; a breaking change requires a version bump (e.g. `v2`) and migration guidance. Examples include `transist.flow.v1`, `transist.module.v1`, `transist.plugin.v1`, `transist.run_state.v1`, and the companion `/api/v1/...` routes.

## Module

A package-local `transist.module.v1` child workflow declared by a workflow or
module document's `modules` map. Modules lower to embedded child recipes and
cannot be compiled, installed, or launched independently.

## Governed Home

A Transist home initialized with `runtime.mode: governed` and
`policy.profile: allowlist`. Governed homes require exact capability grants and
trusted workflow/plugin verification before lifecycle or recovery execution.

## Capability Enforcement

The persisted operator-visible posture showing whether exact capability grants
were `enforced` or explicitly `bypassed` by a standalone/development home.

## Capability Request

The normalized `{ capability, resource, operations }` authority request
generated from a frozen recipe. Governed execution compares every request with
exact operator grants before dispatch; a grant cannot widen structured
filesystem or network policy.

## Readiness Check

The application distinguishes `evaluation_mode: validation` from
`evaluation_mode: live_readiness`. Validation checks prospective inputs,
schemas, current trust and policy, host prerequisites, secret availability,
and secret-delivery policy without plugin probes or secret-value resolution.
Passing validation is not live readiness: its readiness status remains
`not_ready`. Live readiness additionally probes adapters/plugins before fresh
run birth. Both modes produce read-only evidence.

A read-only prospective invocation diagnostic produced by `workflows check`.
It binds supplied inputs without persisting them, reports definition, trust,
dependency, workspace, authority, secret, prerequisite, and health findings,
and always reports `run_created: false`.

## Definition Validation Receipt

Optional installation metadata recording the validator revision, exact recipe
digest, and pinned plugin package digests. A matching receipt lets a read-only
evaluation reuse immutable contracts; it contains no inputs or authority and
does not prove live readiness. See
[installation metadata](public/reference/storage-layout.md#internal-trusted-workflow-registry-registrytrust).

## Workflow Installation

The code-lifecycle operation that verifies and persists an immutable workflow
package in governed/trusted homes or an unsigned source workflow in standalone
or development homes. Installation does not bind inputs, evaluate readiness,
grant authority, or create a run.

## Authority Projection

The kernel-owned typed projection that classifies authority-bearing fields,
records immutable source dependencies, and emits the normalized capability
requests persisted at run birth. Mutable variables, step outputs, and other
post-birth dependencies are not valid authority sources.

## Filesystem Policy

Path access controls persisted into run recipes and step requests. The policy has two separate concerns:

- **`workspace_access`** — controls access *inside* `workspace_root` via `mode` (`full` or `read_only`). Global, workflow, and step declarations compose monotonically: any `read_only` declaration keeps the effective mode read-only, so a narrower scope cannot widen inherited access. `custom` is unsupported and rejected with `filesystem_custom_mode_unsupported` before dispatch.
- **`read_allow_paths` / `write_allow_paths`** (top-level) — grant additive access to paths *outside* `workspace_root`, such as system config files (`/etc/gitconfig`) or home-directory tools (`~/.gitconfig`, `~/.ssh`). These lists are merged (union) across global → workflow → step so that broader grants are always inherited.

Effective policy is resolved from step, workflow, global home settings, or builtin defaults.

## Workspace Access Mode

The enforcement mode for workspace-internal access inside a filesystem policy. `full` allows full access inside the workspace; `read_only` denies workspace mutation. `custom` is unsupported and rejected before dispatch with `filesystem_custom_mode_unsupported` on both Linux and macOS. Does not affect external path grants.

## Workspace Binding

The workflow-level `workspace { path, access }` declaration that names one
execution directory. `path` is an absolute literal or exactly a required string
input reference, commonly `{{inputs.repo_path}}`; fresh run birth canonicalizes
and persists it. `full` allows workspace mutation and `read_only` denies it.
The declaration requests authority only: exact generated capabilities still need
operator grants. Workflow source paths, CLI CWD, and parent directories are not
substitutes. Child subflows inherit the same canonical path and may only narrow
access. Older workflows without this field use the legacy CLI-directory fallback.

## Workspace Boundary

The local host-filesystem boundary enforced by the kernel and local adapters. Host paths outside `workspace_root` are write-denied by default for local `process` and local `mcp` execution on both Linux and macOS. On Linux, reads outside the workspace are also denied unless explicitly granted. On macOS, reads from the Transist home, shared `.transist-staging` parent, and general temporary parent are denied, then the current plugin package, workspace, declared external paths, current staging root, and exact runtime-temporary root are reopened. Grants overlapping protected runtime roots, including the general temporary parent, are rejected. Unrelated same-user host files remain outside this guarantee. Operators can grant explicit read/write access to external paths via `filesystem_policy.read_allow_paths` and `filesystem_policy.write_allow_paths` in `settings.yaml` or in the workflow/step policy.

## filesystem_custom_mode_unsupported

Reason code emitted by the compiler, preflight, and both sandbox backends when a workflow or persisted recipe declares `workspace_access.mode: custom`. The `custom` mode and its workspace-relative `read_allow_paths` / `write_allow_paths` allow-lists were never enforced precisely by either sandbox backend, so the runtime rejects it uniformly before dispatch on both Linux and macOS. Use `read_only` for workspace write protection and the top-level `read_allow_paths` / `write_allow_paths` for external path grants.

## Recipe

A compiled, immutable snapshot of a **Workflow** at the moment a run is started. This ensures that even if you change your code on disk, the active run remains deterministic.

## Replay

Starting a brand-new run using the persisted **Recipe** and inputs from a previous run.

## Resume

Continuing a failed run from persisted state, keeping the same Run ID and history when recovery is still valid.

## Run

A single instance of a workflow definition with concrete inputs and persisted state. This is the primary operational object in Transist.

## Run ID

The unique fingerprint for a specific **Run**, used to track its history, artifacts, and logs.

## Step

A single unit of work within a workflow. Steps can invoke tools, control logic (loops/guards), or trigger sub-workflows.

## Trust Root

A runtime-imported public key record (`transist.trust_root.v1`) stored under `<home>/trust/keys/<key_id>.pub`. Trust roots let the runtime verify detached workflow and plugin signatures without ever handling private keys.

## TEP (Transist Execution Protocol)

The internal language spoken between the Kernel and its Adapters to request work and receive results.

## Signature Sidecar

A detached `transist.signature.v1` JSON file stored next to a signed workflow file or plugin directory. For plugins, the signed digest covers exactly `plugin.yaml` plus the manifest-declared `package.files`. The sidecar records the subject identity, signed digest, signer `key_id`, and the base64 Ed25519 signature over the raw digest bytes.

## Transist Home

The directory where Transist stores plugins, run history, configuration, leases, artifacts, and evidence. Resolution precedence: `--home` flag > `TRANSIST_HOME` env > user-set default > per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`) > error. Use `transist home init` to create the per-user default home, or `transist home set <path>` to configure a user-level default at an explicit path.

## User-Set Home

A home directory path persisted in the user-scoped settings file via `transist home set <path>`. Used flat (no per-repository namespacing) when no higher-priority source resolves.

## Per-User Default Home

The home resolved by `transist home init` (or any command) when no `--home` flag, `TRANSIST_HOME`, or persisted user-set home is present: `${XDG_STATE_HOME:-$HOME/.local/state}/transist`. Shared across every project on the machine; never derived from the current working directory.

## Usage Facts

Per-step usage data collected in inspection snapshots, recording the plugin, action, provider, model, and token counts for aggregation and billing.

## Workflow

A YAML document (`transist.flow.v1`) that describes the sequence of steps, inputs, and logic you want Transist to automate.

## Workflow Definition Digest

The canonical SHA-256 digest used for detached workflow signing. In the current contract this is computed from the full authored definition set rooted at the top-level workflow file: the workflow file itself, declared module files, referenced subflow files, referenced `prompt_file` targets, and referenced `source://` helper scripts. Each file is LF-normalized first, and the final digest is distinct from the compiled `recipe.yaml` snapshot hash. Templated `prompt_file` paths are refused at sign time.

## Workflow Run

The execution of a workflow definition. A workflow run owns the recipe snapshot, inputs, state, events, artifacts, evidence, gates, and recovery lineage.

## Workflow Asset Snapshot

The private, content-addressed `workflow_assets/` capture created during fresh
run birth for resolved source-backed prompt files and shell `source://` helper
files. Its manifest maps logical paths to raw-byte hashes. It is atomic run
data, not an inspection or export surface. Snapshot-backed runs consume these
bytes during dispatch and re-entry; legacy records retain authored-source
behavior.
