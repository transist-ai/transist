# Inspection Snapshot Envelope

This page documents the `transist.inspection.snapshot.v1` contract produced by the local library surface and the core `transist runs snapshot export` CLI for optional outbound inspection export.

**Status:** Experimental contract.

## Purpose

- Keep local inspection authoritative.
- Export a read-only, metadata-first snapshot of a root run tree.
- Give higher-level export code one serializable envelope to send to an external read model.

## What It Is Not

- It is not a replacement for the local `runs/` tree in the effective Transist home.
- It is not exposed as a `transist-server` HTTP route.
- It does not include raw artifact bytes, raw evidence bytes, prompt bodies, response bodies, or full stdout/stderr payloads.

## Machine-Readable Contract

- JSON Schema: [`schemas/inspection/snapshot.v1.schema.json`](../../../schemas/inspection/snapshot.v1.schema.json)
- Reference example: [`examples/inspection-snapshot/v1.example.json`](../../../examples/inspection-snapshot/v1.example.json)

The Rust types remain the runtime source of truth; the JSON Schema is for
external producers, validators, and editor tooling. The example payload
contains a root run plus one child run and round-trips through the schema.

## Current Rust API

- Builder: `transist::InspectionSnapshotBuilder`
- Request: `transist::InspectionSnapshotRequest`
- Envelope: `transist::InspectionSnapshotEnvelope`
- Core CLI: `transist runs snapshot export <run-id> --source-id <id> --workspace-id <label>`

The builder resolves any run id to its root run tree, collects run detail, step summaries, selected step metrics, gates, event deltas, artifact/evidence metadata, classified failures, and per-step usage facts, then validates and serializes the result.

The CLI wrapper uses the same local inspection and local metrics queries, validates the envelope again against the bundled schema, and then writes JSON to stdout or a requested file.

## Top-Level Fields

- `kind`: fixed to `transist.inspection.snapshot`
- `schema_version`: fixed to `1`
- `source_id`: deployment-assigned durable source id
- `workspace_id`: optional workspace label
- `operator_label`: optional human label for the source
- `produced_at_unix_ms`: local production time in unix-ms UTC
- `tree_version`: caller-assigned monotonic version for the root-run tree
- `root_run_id`: root run for the enclosed tree
- `tree_fingerprint`: hash of ordered per-run fingerprints
- `sync_policy`: active sync mode plus include flags
- `runs`: root run plus descendants
- `extensions`, `provider_facts`, `policy_facts`: reserved additive objects

## Per-Run Block

Each run block contains:

- `run_summary`: compact list-style run row
- `run_detail_summary`: metadata-only run detail summary assembled from persisted local state
- `run_detail_summary.workflow_input_values`: optional raw workflow inputs when enabled
- `run_detail_summary.run_detail_full`: optional full run detail when enabled
- `step_metrics`: selected step metrics rows
- `gates`: manual gate records
- `events`: event delta after the supplied per-run cursor
- `artifacts`, `evidence`: metadata only
- `failures`: classified failure records for the run; successful runs emit an empty array
- `usage_facts`: per-step usage rows for downstream aggregation
- `run_fingerprint`: hash of run-local fingerprint inputs
- `last_event_seq`: highest known event sequence for the run

Local inspection's execution-scoped `steps[].latest_output` is excluded from
every copied `run_detail_full`, including copies enabled by stderr-tail or gate
request-summary flags. Existing snapshot fields, including execution history,
retain their prior payload selection; these flags do not opt into the new local
output projection.

## What Flows To The Export Target

| Field | Default payload | Notes |
|---|---|---|
| `run_summary` | present | existing compact run metadata |
| `run_detail_summary.summary` | present | same run summary anchor for the detail block |
| `run_detail_summary.input_keys` | present | input names only, not values |
| `run_detail_summary.input_types` | present | best-effort runtime type labels |
| `run_detail_summary.input_presence` | present | boolean presence only |
| `run_detail_summary.input_sources` | present | currently `unknown` because persisted runs do not store origins |
| `run_detail_summary.workflow_input_values` | optional | present only when the sync policy allows raw workflow inputs |
| `run_detail_summary.run_detail_full` | optional | present only when the sync policy allows the full run detail |
| `gates[].request_keys` | present | keys only; gate request values are omitted |
| `gates[].request_summary` | optional | present only when the sync policy allows the full gate request payload |
| `events[].summary` | present | validated against forbidden keys; raw payloads are optional |
| `sync_policy` | present | defaults to `metadata_only` when absent from the producer |
| `artifacts` / `evidence` | metadata only | never includes bytes |
| `failures` | present | failure metadata only; includes `execution_attempt`, optional `last_stable_step_id`, and `message_redacted` is validated for forbidden key tokens |
| `workflow_inputs` values | omitted | replaced by metadata-only `run_detail_summary` |
| `step_metrics[].had_stderr` | present | boolean discriminator; `true` when the step produced any stderr output |
| `step_metrics[].had_error` | present | boolean discriminator; `true` when the step recorded an error message |

## Compatibility

- Producers now emit `schema_version = 1`.
- The snapshot envelope is currently defined only as v1.
- `run_detail` is deprecated in favor of `run_detail_summary`.

## Event Semantics

- Event cursors are supplied per run via `event_after_by_run`.
- The builder pages through the local event stream until all events after the cursor are included.
- Each exported event carries `event_seq`, `timestamp_unix_ms`, `event_type`, optional `step_id`, and `summary`.
- `summary` is filtered through the event allowlist documented in [`inspection-event-summary-allowlist.md`](./inspection-event-summary-allowlist.md) unless the sync policy explicitly enables raw event payloads.

## Fingerprints

Run fingerprint inputs:

- run status
- last touched step id
- last touched step status
- step count
- highest known event sequence
- run `updated_at_ms`

Tree fingerprint input:

- ordered `{run_id, run_fingerprint}` pairs for the root run and descendants

## Size Cap

- Default maximum serialized payload size: `5 MiB`
- The builder rejects oversized envelopes today.
- Event paging across multiple envelopes is owned by the local export process
  (see `plan-1-local-sync`), not by this builder.

## Redaction Implementation Detail

The recursive forbidden-key walker (`crates/transist-inspection/src/redaction.rs`)
treats `input_types`, `input_presence`, and `input_sources` as safe containers
whose keys are user-declared workflow input names. These maps are not checked
for forbidden keys at their first level because input names like `prompt` or
`ticket` may legitimately match forbidden key entries. The prohibition still
applies to values nested inside those maps and to all other parts of the envelope.

## Stability Notes

- The contract is intended for inspection export work.
- Additive fields may be introduced under the reserved objects.
- Local inspection surfaces remain the primary v1 contract.

## Field Handling

- `had_stderr` and `had_error` are boolean discriminators on `step_metrics`.
  Policy-driven field stripping controls payload exposure independently.
- `input_types`, `input_presence`, `input_sources` are exempt from the
  recursive forbidden-key walk at their first level (see
  [Redaction Implementation Detail](#redaction-implementation-detail)).

## Heartbeat Payload (v1)

Optional inspection heartbeat uses a sibling payload, `transist.heartbeat.v1`, to report node liveness independently from snapshot uploads.

Top-level fields:

- `schema`: fixed to `transist.heartbeat.v1`
- `source_id`: deployment-assigned durable source id
- `installation_id`: stable UUIDv7 for the local Transist home
- `transist_version`, `runtime_version`, `sync_daemon_version`: local version inventory
- `os`, `arch`: normalized runtime platform labels
- `hostname_hash`: stable hashed hostname derived from the persisted workspace salt
- `uptime_seconds`: best-effort local uptime reported by the runtime
- `active_run_count`: current count of non-terminal runs in the local home
- `queue_depth`: current local inspection export queue depth
- `features`: explicit capability flags such as `sync_policy_v1`
- `emitted_at_unix_ms`: local emit time in unix-ms UTC
