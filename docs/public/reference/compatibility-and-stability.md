# Compatibility and Stability

## Pre-1.0 (Public Beta) Policy

Transist `0.1.1` is a public beta patch release. No surface is called stable or
production-ready before `1.0.0`.

- Patch releases do not intentionally break documented surfaces except for an
  urgent security correction, with changelog, release-note, and migration guidance.
- Minor releases may break documented surfaces. Breaking changes require a
  changelog entry, release note, migration guidance, and contract-fixture updates.
- Versioned public surfaces are protected within the current minor line.
- Schema and protocol `v1` identifiers are format versions, not SemVer stability
  promises. Runtime, admin, and server use `0.1.1`; bundled plugins, workflows,
  modules, and exact catalog pins remain `0.1.0`. See the
  [patch release notes](../releases/0.1.1.md) for loop-routing adoption and recovery.

## Versioned Public Surfaces

Versioned surfaces include:

- Workflow schema `transist.flow.v1` and package-local `transist.module.v1`.
- Plugin manifests `transist.plugin.v1` and the APP/TEP participant protocols.
- Trust envelopes `transist.signature.v1`, `transist.trust_root.v1`, and
  persisted `transist.trust_decision.v1` decisions.
- Run state `transist.run_state.v1` and inspection snapshots
  `transist.inspection.snapshot.v1`.
- Managed-document initialization and revision records
  (`transist.document_initialization.v1`, `transist.document_revision.v1`) and
  exact-review requests (`transist.gate_review_request.v1`).
- CLI reports, including `transist.workflow_install.v1` and
  `transist.workflow_explanation.v1`, and companion `/api/v1/...` routes.
- Documented operator commands and release archive target triples.

See [Workflow Schema](workflow-schema.md), [Plugin Manifest](plugin-manifest.md),
[CLI](cli.md), [Companion Server API](companion-server-api.md), and the
[Kernel Contract](../../developer/architecture/kernel-contract.md) for exact
behavior. Runner labels and container distributions are validation environments,
not distribution-specific compatibility commitments.

Repository-owned authoring guidance lives in `.agents/skills/`. Use the skills
from the same source release as the runtime; they describe current contracts and
do not expand execution authority or compatibility guarantees.

## Workflow Lifecycle and Authority

Installation verifies and persists code only; it does not bind prospective inputs,
grant authority, check live readiness, or create a run. Installed references are
positional and version-pinned:

```bash
transist workflows check demo@0.1.0 --inputs-file inputs.json --format json
transist workflows authorize demo@0.1.0 --inputs-file inputs.json --dry-run --format json
```

Ordinary installed checks and authorization use probe-free validation. Require
`validation_status: passed`; readiness remains `status: not_ready` until live
evaluation. Warnings and genuinely incomplete required validation block success.
Use `check --live` for probes or `check --output readiness.json` for canonical
live evidence. Preparation checks live readiness before run birth. Authored
`check --path` rejects `--live`.

Authorization reevaluates current inputs, canonical resources, trust, policy, and
secret availability. It writes only reviewed exact missing grants, compares the
fragment and destination again before publication, and writes nothing when already
authorized. Changed authority requires another review.

`workflows prepare` and `workflows run` accept an optional `--run-ref`; otherwise
they generate a run ID. `check` and `authorize` default to concise text, including
when redirected; scripts needing complete reports must request `--format json`.
`runs capabilities --exact` selects the normalized capability list.

`workflows explain --path` is read-only and deterministic for identical captured
definitions, inputs, and local contract snapshots within this implementation.
It is not validation, authorization, readiness, semantic equivalence, or execution
consent. Optional omitted inputs bind as null (`unset_null`); opaque external
output is never reinterpreted as executable workflow syntax.

## Dataflow, Documents, and Review

Typed direct step-output bindings and named module exports carry JSON data.
Compiler-owned deferred-binding and subflow-output metadata never reaches plugin
action input. `deferred_to_dispatch` means pending data-dependent validation,
not a pass or a skipped child check. Static contracts and authority must pass;
the full materialized request is validated before the consumer's probes or effects.
Reused children reject changed immutable inputs; use `rerun_mode: fresh` for new
bindings.

Managed documents are bounded root-tree storage. Immutable execution records
commit revisions, manifests, and blobs; latest heads derive from those records.
Process action-data path fields may use recursive `$document` bindings. HTTP/MCP
document bindings are unsupported. Only string paths materialize, after static
authority checks; durable path echoes become logical bindings rather than host
staging paths.

Manual gates with `review_documents` persist an exact review request on their first
blocked execution. Approval must cover current execution/activation identity and
each revision ID and digest before consumer staging or probing. Equal bytes in a
new revision do not reuse approval; waiting restart retains the original request.
Resume retains committed documents; replay creates a fresh document namespace.
Companion document routes use opaque identifiers and expose committed content;
metadata does not expose runtime-home paths. Snapshot exports include review
request summaries only through explicit export policy.

## Recovery and Immutable Packages

Start evaluation in a fresh runtime home. Installed identities are content-immutable.
Changed same-version definitions or helper bytes require new reviewed captures and
signatures and a fresh home if an installed identity conflicts. Never overwrite
pinned payloads or delete runs to clear collisions. There is no in-place home
migration, version alias, or legacy document/review reader.

Preserve the runtime, reviewed bundle, and matching home together. Do not downgrade
a home into an incompatible reader or copy source/replay document namespaces.
Rollback and replay do not undo external side effects. Runtime version labels do
not prove compatibility with another persisted format.

### Bounded-loop routing adoption

Newly compiled recipes containing authored loops pin `graph.loop_routing_version: 1`
and explicit entry/exit roles. Successful wrappers take normal exit at their
activation-local iteration cap; this does not approve a gate or bypass a mandatory
approval action. Existing prepared unmarked recipes retain legacy predicate-only
routing. Resume and replay load their pinned snapshots and never recompile,
upgrade or rewrite them; replay of an unmarked recipe is still unmarked.

To adopt the correction, compile reviewed definitions with the matching candidate
runtime and start a new run. Respect immutable installed identities; use fresh
reviewed captures and a fresh home when same-version content conflicts. Keep
previous runtime/home pairs available for legacy recovery. Do not downgrade a
home containing marked recipes or remove markers to force an older reader to
accept it. Runtime `0.1.0` labels alone do not establish compatibility with
published binaries; validation evidence applies to the exact candidate bytes.

Before adoption, rollback can revert the implementation while retaining the old
home. After marked runs exist, keep those runs with their matching runtime/home
pair; rollback selects a separately preserved pair and never edits run snapshots
or undoes external side effects.

Only revision 1 of the asset-lock and capability-authority contracts is
supported. Higher values, including 2, are rejected before execution or durable
mutation, even in test mode. Zero remains a legacy/synthetic initialization
value; asset-lock zero still cannot re-enter execution. Snapshot-backed assets
use revision 1 with `workflow_asset_snapshot: true`; unmarked version-1 records
retain authored-source resolution. Unsupported records must not be relabeled
or migrated in place; preserve their home and start a fresh supported run.
Rollback uses a separate preserved runtime/home pair.

Rejection applies per unsupported record, not atomically to the whole run tree;
ancestor work may precede discovering an unsupported descendant. Pending
recipe-sync recovery checks both embedded state revisions before repair writes
or intent deletion, preserving the unsupported record's state, recipe and intent.

Every declared cleanup whose `when` matches the root terminal outcome (`success`,
`canceled`, or `failed`) runs automatically as a separate phase. No matching branch
means no action; unexpected termination never implicitly chooses cancellation
cleanup. The primary status is unchanged. Abrupt process death can prevent cleanup
from completing; `runs cleanup` recovers unfinished or failed cleanup. Repeated
cancel requests during cancellation cleanup neither interrupt nor duplicate it.

A failed run without matching cleanup remains recoverable. Once failure cleanup
begins, `cleanup_required` / `cleanup_completed` retry/resume protection applies;
use replay for new primary work. Refused trusted re-entry may refresh documented
trust-decision and update timestamps only: immutable execution history remains
unchanged and no work is dispatched.

## Persistence and Schema Policy

- Existing schema fields do not silently change meaning. New fields are additive;
  clients must tolerate documented optional fields being absent.
- `state.json` is an atomically replaced projection, not append-only history.
  Immutable execution records, artifacts/evidence, and event/failure streams carry
  history. Durable writes and directory syncs protect publication; staged run birth
  exposes either no run or a complete run.
- Step `executions` is a lifetime monotonic dispatch count. The optional
  `activation_executions_baseline` defaults to zero; activation-local counts subtract
  it. Regressing baselines or baselines exceeding lifetime executions fail closed.
- Pinned capability projections govern preflight, dispatch, recovery, cleanup, and
  child execution. Missing or mismatched current authority fails closed. Changing
  workspace authority or run inputs requires a fresh run, not replay.
- Run-birth reconciliation validates the complete staged artifact set. Only
  documented recomputed timestamps are excluded from structural comparison;
  recipe, inputs, and locator remain byte-exact. Corrupt or missing state for a
  locatable run is an error, not an absent run.
- Snapshot-backed runs persist `asset_lock_version: 1` and
  `workflow_asset_snapshot: true`. Persisted metadata selects capture semantics;
  missing or corrupt required captures fail closed.
- Companion file metadata uses workspace-relative `relative_path` and opaque
  resource IDs. Use supplied API links, not reconstructed host paths.
- Step output detail distinguishes omitted output from JSON null and identifies
  the selected execution/status; optional metrics may be absent.
- Plugin action schemas use JSON Schema Draft 2020-12 and compile at installation.
  Package-local transitive references participate in the signed digest. Remote,
  absolute, parent-traversing, and escaping references are rejected.
- `bindings.input_projection: action_data_v1` strips only top-level `secret_refs`
  from cloned action input. Without it, input bytes retain that field. Unknown or
  malformed projection values fail closed; authority and secret policy still apply.
- Host prerequisites inspect binary, environment, and path presence only.
  Executable command/version checks fail with
  `legacy_executable_prerequisite_unsupported` without starting a process.
- Process/MCP `runtime://temp` values are supported only as complete
  `entrypoint.env` values. Resolved paths are invocation-private and non-persistent;
  the first `documents` component is reserved case-insensitively. Symlink traversal
  and overlap with protected runtime trees are rejected. Linux uses the canonical
  OS temporary parent, not caller `TMPDIR`.
- Plugins use `TRANSIST_ARTIFACTS_DIR`, `TRANSIST_EVIDENCE_DIR`, and
  `TRANSIST_RUNTIME_METADATA_FILE` for invocation-private output/context, never
  persistent cross-step storage. Runtime metadata includes the current invocation's
  `run.id`, including child identity for a contained run.
- Runtime-known secrets and their standard/URL-safe base64 encodings are not
  persisted in runtime records or promoted output. This is not a DLP guarantee for
  arbitrary plugin transformations or external exfiltration.
- Inspection and snapshot exports derive from local state; they do not replace it.

Serialization fixtures protect current workflow, recipe, plugin, settings, trust,
and signature contracts. They do not promise byte-identical output across releases.

## Reserved Test Versions

Workflow `v0` and plugin `0` / `v0` are explicit non-SemVer development test inputs.
Production builds reject bypass-eligible inputs unless explicitly rebuilt with
development-only bypass support. Ordinary SemVer releases, including `0.1.0` and
prereleases, never bypass governed signature verification.

## Crate Distribution

- Workspace crates are implementation units, not a supported public Rust library
  or crates.io distribution; all inherit `publish = false`.
- The supported distribution is the signed binary evaluation archive containing
  runtime, server, admin, bundled onboarding resources, and ancillary skills.
  See [Supported Platforms](supported-platforms.md) for all six targets and
  [Release Artifact Verification](release-artifact-verification.md).
- Documentation is published at [transist.ai/docs](https://transist.ai/docs).
  It is not archive payload; no separate documentation archive is published.
- Building from source is supported for evaluation and contribution.
- Skills are ancillary authoring guidance, not a versioned runtime/API promise.

## MSRV

Rust `1.88.0` is the build pin and minimum supported Rust version. Changing it
requires an explicit release-noted policy update. Third-party dependency, API,
tool, and standards versions are independent of Transist release versions.

## What Is Not Promised

- Stability or production readiness before `1.0.0`.
- Byte-for-byte output compatibility across all releases.
- Distributed multi-host scheduling or hosted control-plane behavior.
- Compatibility for undocumented internal layouts beyond documented contracts.
- A public Rust library distribution.

See also [Threat Model](../security/threat-model.md) and
[Known Limitations](known-limitations.md).
