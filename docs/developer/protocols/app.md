# Reference: APP (Adapter Packaging Protocol)

The **Adapter Packaging Protocol (APP)** defines the low-level communication format between a Transist adapter and the plugins it manages. While TEP handles the "kernel-to-adapter" link, APP handles the "adapter-to-plugin" link.

If you are developing a `process` or `http` plugin, it must speak the
**`app.v1`** protocol. MCP plugins use the native MCP protocol described below.

## Protocol Version

- **Current Version:** `app.v1`

## Communication Lifecycle

Communication follows a structured set of message pairs:

### 1. Handshake

The adapter verifies the plugin's identity and capabilities.

- **`app.handshake.request.v1`**
- **`app.handshake.response.v1`**: Identifies the plugin ID, version, and supported capabilities.

### 2. Invoke

The actual work is requested and performed.

- **`app.invoke.request.v1`**: Contains the action name, inputs, the filesystem paths where the plugin should work, the additive host policy, optional timeout budgets, and the exact `requested_capabilities` subset forwarded from the TEP invoke request.
- **`app.invoke.progress.v1`**: Optional streamed progress frame emitted before the terminal result (carries `stream`, `delta_lines`, and an optional `line_preview`).
- **`app.invoke.result.v1`**: Returns the `status` (`success`, `failed`, or `cancelled`), the JSON `output`, `artifact_hints`, `evidence_hints`, numeric `metrics`, optional `usage`, optional canonical `error`, and any `artifact_hints` (output files).

### 3. Cancel

Allows the adapter to gracefully stop a running plugin action.

- **`app.cancel.request.v1`**
- **`app.cancel.result.v1`**

---

## Adapter-Specific Bindings

Local entrypoints receive the invocation-private `TRANSIST_RUNTIME_METADATA_FILE`.
Its unversioned JSON adds `run.id` from the exact current TEP/APP `run_id`; child
invocations expose the child, not a guessed root. This is observational data with
no paths, secrets or authority. See the [public runtime metadata contract](../../public/reference/app.md#invocation-runtime-metadata).

The way these messages are exchanged depends on the `adapter` type you choose.

Action input projection is explicit: only
`bindings.input_projection: action_data_v1` removes top-level `secret_refs`
from a cloned input before schema validation and transport. Static/live
preflight shares this projection. Absent binding retains legacy bytes;
malformed or unknown values fail closed. Process/HTTP APP `input` and MCP
`tools/call.arguments` use the projected value; HTTP then applies existing
body-field secret injection. Every other key remains subject to the original
strict action schema. See the [public manifest contract](../../public/reference/plugin-manifest.md#opt-in-action-input-projection).

### `process` Adapter

Messages are exchanged as **newline-delimited JSON (NDJSON)** over `stdin` and `stdout`. The plugin's `stdout` is strictly reserved for these APP frames; any other output should be written to `stderr`.

Handshake receives distinguish deadline expiry (`timeout_error`, exit code
`124`) from stdout closure or read failure (`transport_error`, exit code `2`).
Malformed handshake frames remain `protocol_error`. Live preflight retains
`adapter_handshake` and never includes plugin stderr; a post-cleanup exit status
does not establish the original cause. See
[Process Handshake Failures](../../public/reference/app.md#process-handshake-failures)
for the operator-facing diagnostics. This distinction applies before invocation;
invoke-result receive and cleanup semantics are unchanged.

### `http` Adapter

Messages are sent via standard REST endpoints:

- `GET /app/handshake`
- `POST /app/invoke`
- `POST /app/cancel`
- `GET /app/health` (Optional, if health checks are supported)

### `mcp` Adapter

The adapter uses native **Model Context Protocol (MCP) `2025-06-18`** tools
messages rather than APP envelopes. It uses stdio UTF-8 JSON-RPC 2.0 with one
compact object per LF or CRLF-delimited line and a 1 MiB per-message limit
excluding the delimiter. `Content-Length` framing is not supported.

The client initializes with empty capabilities, requires the matching protocol
version and a declared `tools` capability, sends `notifications/initialized`,
discovers every page of `tools/list`, and invokes manifest-bound tools through
`tools/call`. It supports interleaved notifications and server `ping` requests.
Unsupported server requests receive JSON-RPC `-32601`.

Native `CallToolResult` mapping is intentionally narrow: `structuredContent` is
the action output when supplied; otherwise output is `{ "content": [...] }`.
Text content blocks form stdout, non-text blocks remain in output, and the
mapped output is checked against the action output schema. The adapter does not
infer APP artifact hints, evidence hints, metrics, or usage. JSON-RPC failures
become `protocol_error`; `isError: true` becomes `tool_execution_error`.

The adapter supports no MCP resources, prompts, sampling, roots, elicitation,
or Streamable HTTP transport. It sends `notifications/cancelled` and closes
stdin before process-group termination when an active tool call times out or is
cancelled.

---

## Advanced Features

- **Artifact and Evidence Hints**: Plugins can use these fields to point the adapter toward specific files it created that should be preserved as part of the run's audit trail.
- **Usage Reporting**: The `usage` object allows plugins (especially AI-based ones) to report token counts and costs back to Transist for aggregation and monitoring.
- **Metrics**: A simple map for reporting custom numeric data that will be exported as Prometheus metrics.

---

### Implementation Details

For the exact message structures and JSON schemas, see the source at `crates/transist-protocol/src/app.rs`.
