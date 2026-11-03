# APP and TEP Essentials

Use this reference when package behavior depends on protocol details.

## APP

APP is the plugin-facing contract.

Current message families:

- `app.handshake.request.v1`
- `app.handshake.response.v1`
- `app.invoke.request.v1`
- `app.invoke.progress.v1`
- `app.invoke.result.v1`
- `app.cancel.request.v1`
- `app.cancel.result.v1`

Each APP message has a committed JSON Schema under `schemas/app/` that matches
the serialized Rust type exactly (`additionalProperties: false`).

Important plugin-facing facts:

- handshake identifies plugin id, plugin version, adapter kind, and capabilities
- invoke requests carry `plugin`, `action`, `input`, `paths`, the additive
  `policy`, optional `timeout_ms` / `max_timeout_ms`, host `metadata`, and the
  exact `requested_capabilities` subset forwarded from the TEP invoke request
- optional `app.invoke.progress.v1` frames (`stream`, `delta_lines`, optional
  `line_preview`) may stream before the terminal result
- invoke results may include `output`, `artifact_hints`, `evidence_hints`,
  numeric `metrics`, optional `usage`, and optional canonical `error`
- action input is validated before transport dispatch and successful output is
  validated before hints or output data are converted; failures use
  `validation_error` and path-only diagnostics

## TEP

TEP is the adapter-facing kernel contract.

Current message families:

- `tep.handshake.request.v1`
- `tep.handshake.response.v1`
- `tep.invoke.request.v1`
- `tep.invoke.result.v1`

Each TEP message has a committed JSON Schema under `schemas/tep/`; the
unversioned aliases `invoke.schema.json` and `result.schema.json` are pure
`$ref`s to the canonical versioned schemas.

Important adapter-facing facts:

- execution identity is still the workflow's chosen `adapter + plugin + action`
- handshake response identifies the adapter with `adapter { kind, version }`
  and advertises `capabilities { actions, supports_cancel?, supports_health? }`
- invoke requests carry `step`, `adapter_kind`, `action`, `input`, adapter-facing
  `paths` (including `workspace_dir`), `requested_capabilities`, optional
  `timeout_ms` / `max_timeout_ms`, `policy`, and `metadata`
- invoke results may include `artifacts`, `evidence`, numeric `metrics`,
  optional `usage`, and optional canonical `error`

## Usage and Metrics

- `usage` is optional and is the only source for token aggregation
- numeric `metrics` survive into step summaries when valid
- invalid metric entries are discarded without inventing new Prometheus metric families

## Error Codes

Common canonical failure codes include:

- `configuration_error`
- `auth_error`
- `secret_resolution_error`
- `validation_error`
- `timeout_error`
- `cancelled`
- `rate_limit_error`
- `transport_error`
- `protocol_error`
- `tool_execution_error`
- `adapter_internal_error`
