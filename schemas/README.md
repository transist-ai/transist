# Schemas Folder

This folder contains machine-readable JSON Schema and OpenAPI files for the
current public Transist contracts.

- Rust code remains the runtime source of truth. These files are for tooling,
  validation, and editor integrations.
- All JSON Schemas target JSON Schema Draft 2020-12.

## Workflow Contract

`workflow.schema.json` describes the authored `transist.flow.v1` surface:

- top-level `schema`, `id`, `version`, optional `inputs`, and ordered `steps`
- invoke steps with `uses: <adapter>:<plugin>/<action>`
- optional first-class prompt and skills fields on invoke steps: `prompt`, `prompt_file`, `skills`
- control steps with `control`
- first-class `if` and `loop` blocks on `control.noop`
- package-local `module` steps declared by the root workflow's `modules` map

The old nested `execution` object is not part of the current contract.

`runtime/workflow-asset-snapshot.v1.schema.json` describes the private manifest
written when fresh run birth captures resolved prompt and shell `source://`
assets. The manifest contains logical paths and content hashes only; raw blobs
remain under the run's private `workflow_assets/` directory and are not exposed
through inspection exports.

## APP Contract

The `app/` folder contains the adapter-to-plugin protocol schemas. Each schema
matches the serialized Rust type in `crates/transist-protocol/src/app.rs`
exactly (`additionalProperties: false`):

- `app/handshake.request.v1.schema.json`
- `app/handshake.response.v1.schema.json` (plugin identity, adapter kind, full capabilities)
- `app/invoke.request.v1.schema.json` (carries `requested_capabilities` forwarded from TEP)
- `app/invoke.progress.v1.schema.json` (streamed progress frames)
- `app/invoke.result.v1.schema.json` (output, artifact/evidence hints, metrics, usage, canonical error)
- `app/cancel.request.v1.schema.json`
- `app/cancel.result.v1.schema.json`

## TEP Contract

The `tep/` folder contains the kernel-to-adapter protocol schemas. Each schema
matches the serialized Rust type in `crates/transist-protocol/src/tep.rs`
exactly (`additionalProperties: false`):

- `tep/handshake.request.v1.schema.json`
- `tep/handshake.response.v1.schema.json` (adapter identity `{kind, version}`, adapter capabilities)
- `tep/invoke.v1.schema.json` (canonical invoke request, including `requested_capabilities`)
- `tep/invoke.schema.json` (current alias — a pure `$ref` to `invoke.v1.schema.json`)
- `tep/result.v1.schema.json` (canonical invoke result)
- `tep/result.schema.json` (current alias — a pure `$ref` to `result.v1.schema.json`)
- `tep/error.schema.json` (canonical `ProtocolError`, shared by TEP and APP result schemas)
- `tep/capabilities.schema.json` (canonical `AdapterCapabilities`)

### Alias Policy

Unversioned aliases (`invoke.schema.json`, `result.schema.json`) contain only
a `$ref` to the canonical versioned schema plus metadata (`$schema`, `$id`,
`title`). They never duplicate definitions, so aliases cannot drift from the
canonical contract. `tests/schema_contract.rs` compiles every committed schema
offline and validates representative payloads serialized from
`transist-protocol` against the matching schema.

Both invoke schemas require the normalized `requested_capabilities` array.
Each item is `{ capability, resource, operations }`; the runtime rejects
wildcards and non-canonical resources before policy evaluation.

## Runtime Contract

The `runtime/` folder contains the persisted run-state contract
`transist.run_state.v1` — the serialized shape of the run record written to
each run's `state.json`:

- `runtime/run-state.v1.schema.json` (every run-status and step-status variant,
  every optional-field shape, lineage/cleanup/preflight/trust/asset-lock
  structures)

The schema matches the serialized Rust type
`transist_core::types::PersistedRunRecord` (and its nested record structures)
exactly. Enum variants and optional fields are exercised exhaustively by
`tests/schema_contract.rs`, which serializes real runtime records and validates
them against this schema.

## HTTP Contract

`schemas/http/companion-api.v1.openapi.yaml` is the OpenAPI 3.0 contract for
every `/api/v1` route exposed by `transist-server`. Every operation is `GET`;
the companion API is strictly read-only. `crates/transist-server/src/infrastructure/http_server/routes.rs`
holds the canonical route inventory, and the `transist-server`
`api_contract_parity` test asserts bidirectional parity between the inventory,
the registered Axum router, and the OpenAPI contract, and that no mutating
method is exposed. `scripts/ci/check_schema_api_contracts.sh` gates both the
schema and HTTP contract surfaces in PR, mainline, and release.

## Inspection Snapshot Contract

The `inspection/` folder contains the outbound sync contract used
by the local-first inspection read model:

- `inspection/snapshot.v1.schema.json` — the `transist.inspection.snapshot.v1`
  envelope produced by `transist::InspectionSnapshotBuilder`.

A reference payload (root run plus one child run) lives at
`examples/inspection-snapshot/v1.example.json`.

This contract is **experimental**. Local inspection surfaces remain
authoritative; the envelope only carries metadata and diagnostic facts and
never raw artifact bytes, evidence bytes, prompt bodies, response bodies, or
full stdout/stderr payloads.

## Notes

- `v1` in a schema identifier or path is a format version, not a SemVer 1.0
  stability promise.
