# Plugin Manifest and Bindings

Current manifest schema: `transist.plugin.v1`

## Top-Level Fields

Required manifest fields:

- `schema`
- `id`
- `version`
- `adapter`
- `entrypoint`
- `package`
- `actions`
- `capabilities`

Optional manifest fields:

- `prerequisites` (static plugin-level host readiness using `binaries`, `env`,
  and `paths`; checked during `transist plugins install` and again during runtime
  preflight)
- `default_provider_kind` (`local` or `external`) for manifest-wide locality defaults

`package.files` is required and must be a non-empty list of unique,
package-relative regular-file paths. `plugin.yaml` is implicit and must not be
listed. The list must include the entrypoint, local modules and dynamically
loaded assets, certificates, action schemas, and every local schema `$ref`
target. It is the only payload-selection mechanism; unselected files are not
digested, signed, or installed.

## Supported Adapter Kinds

- `process`
- `http`
- `mcp`

## Entrypoint Shapes

- `process`: `command`, optional `args`, optional `cwd`, optional `env`
- `http`: valid `http://` or `https://` `base_url` without userinfo, plus an
  optional plugin-relative PEM `ca_cert` certificate bundle; HTTPS still
  validates chain, expiry, hostname, and signatures
- `mcp`: `command`, optional `args`, optional `cwd`, optional `env`

## Action Fields

Each action should define:

- `name`
- `input_schema`
- `output_schema`
- optional `provider_kind` (`local` or `external`)
- optional `secret_inputs`
- optional `bindings`
- optional `prerequisites` (static action-level host readiness; runtime-only,
  surfaced during workflow preflight)

Action schemas are JSON Schema Draft 2020-12 documents. Installation compiles
both files. Relative `$ref` targets may point only to regular files within the
canonical package root; remote, absolute, parent-traversing, and
symlink-escaping references are rejected offline. The host adapter validates
materialized input before dispatch and successful output before converting
hints or result data. Invalid instances return `validation_error` with schema
and instance paths only.

When both `default_provider_kind` and action-level `provider_kind` are absent, the runtime treats the action as `external` (fail-safe).

The host HTTP adapter enforces effective `none`, `loopback`, and `allowlist`
network policy for handshake, health, invoke, redirect, and cancellation
requests. Restricted modes disable environment/system proxies.

For `mcp` plugins, the workflow-facing action must declare a non-empty `bindings.tool_name`.

## Workflow-Level Bindings

Current first-class workflow bindings are:

- `transist_prompt_v1`
- `transist_skills_v1`

Use them only when the action is designed to accept workflow-owned prompt or skills inputs.

`transist_prompt_v1` fields:

- `inline_field`
- optional `file_field`

`transist_skills_v1` fields:

- `bundle_field`
- `rendered_field`

## Capabilities

Current manifest capability fields are:

- `supports_cancel`
- `supports_health`
- `supports_artifact_hints`
- `supports_evidence_hints`

`supports_health` matters most for `http` plugins because generic preflight probes `GET /app/health` only when that capability is enabled.

Legacy `commands`, binary `version`, and `version_check` fields remain
parseable for migration but fail with
`legacy_executable_prerequisite_unsupported`; they never execute. Use an
explicit invoke action for executable checks and APP handshake/health for plugin
readiness.

## Usage and Metrics

- APP invoke results may report `usage`.
- Token summaries exist only when `usage` is reported.
- Plugins that wrap a model-backed tool SHOULD populate `usage` using one of the documented extraction patterns; plugins that do not touch a model SHOULD leave it absent.
- Canonical `usage` keys: `input_tokens`, `output_tokens`, `total_tokens`, and optional `reasoning_tokens`, `cache_read_tokens`, `cache_write_tokens`, `model`, `provider`.
- `output_tokens` includes `reasoning_tokens` when the backend separates them; `total_tokens` equals `input_tokens + output_tokens` unless the backend reports a different total.
- When a plugin wraps another model-backed CLI as a raw subprocess (not through the runtime) it MUST extract usage itself; sibling plugin accumulators do not run in that case.
- Pair a populated `usage` with a `usage.json` artifact hint when practical.
- Extraction MUST be best-effort and never fail the primary action.
- Numeric adapter or plugin `metrics` can appear in step summaries, but arbitrary metric names do not become first-class Prometheus series.

## Governed Runtime Implications

- Actions declared `provider_kind: external` can be denied by `--local-only` before dispatch.
- Allowed governed external calls emit additive `transist.external_transfer.v1` evidence and roll up into `provenance.json.external_transfer_summary`.
- Trusted-mode result handling rechecks adapter-reported artifact and evidence checksums before descriptor metadata is persisted.
- Prepared runs pin installed plugin bytes via run-level asset locks; changing package bytes after `prepare` breaks re-entry compatibility.
- Fresh snapshot-backed runs capture workflow-owned prompt and plugin-local `source://` files in private `workflow_assets/` storage and use those bytes for dispatch and re-entry. Keep `source://` documented as plugin-specific; legacy records retain authored-source behavior.

See [usage-reporting.md](usage-reporting.md) for the four extraction patterns (streaming JSON events, session-state files, rollout files, stdout regex fallback) and reference implementations.

## Protocol Boundary

- APP is the plugin-facing semantic contract.
- TEP is the kernel-facing adapter contract.
- Package authoring should stay focused on manifest, bindings, schemas, and entrypoint behavior unless adapter implementation is the blocker.
