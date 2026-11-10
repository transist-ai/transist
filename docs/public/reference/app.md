# Reference: APP (Adapter Plugin Protocol)

APP defines the public adapter-to-plugin protocol used when an adapter asks a plugin to perform work.

## Protocol Version

- Current version: `app.v1`

## Message Families

- `app.handshake.request.v1` / `app.handshake.response.v1`
- `app.invoke.request.v1` / `app.invoke.progress.v1` / `app.invoke.result.v1`
- `app.cancel.request.v1` / `app.cancel.result.v1`

Each APP message has a committed JSON Schema under `schemas/app/` that matches
the serialized Rust type exactly (`additionalProperties: false`). `tests/schema_contract.rs`
compiles every committed schema offline and validates representative payloads
serialized from `transist-protocol` against the matching schema.

## Responsibilities

- The adapter passes structured input to the plugin.
- The plugin performs the requested action inside its supported execution model.
- The plugin may stream `app.invoke.progress.v1` frames before the terminal
  result, then returns status, output, and any artifact or evidence hints
  required by the adapter.

Action `input_schema` and `output_schema` files use JSON Schema Draft 2020-12.
The adapter validates materialized input before sending it over the process,
HTTP, or MCP transport. For a result with `status: success`, it validates
`output` before converting hints or exposing the result to the kernel. Invalid
instances produce the canonical `validation_error` and do not retain the
invalid output or its hints.

Action `bindings.input_projection: action_data_v1` explicitly opts in to
removing only top-level `secret_refs` from a cloned input before validation
and transport. Unknown business keys and nested references remain. Absent
binding preserves legacy bytes; malformed or unknown binding values fail
closed. Process APP `input` and MCP `tools/call.arguments` use that projection;
HTTP APP `input` uses it followed by existing body-field secret injection.
Outer APP metadata, prompt/skills/manual-gate fields, and timeout semantics
retain their existing meaning. See [Plugin Manifest](plugin-manifest.md#opt-in-action-input-projection).

### Invocation Runtime Metadata

Local process/MCP entrypoints receive `TRANSIST_RUNTIME_METADATA_FILE`, the path
to an invocation-private, runtime-created JSON file. A process plugin may preserve
that environment variable for its command children. Its unversioned format is:

```json
{
  "runtime": {"version": "0.1.1"},
  "workflow": {"id": "example"},
  "run": {"id": "example-run"},
  "plugins": [{"name": "shell", "version": "0.1.0"}]
}
```

`run.id` is the exact current `ExecutionRequest.run_id` (the APP request's
`run_id`), never a directory name or workflow input. Contained invocations expose
their **child** run ID, not a root ID. Retry/resume retains the owning run ID;
fresh runs and replay expose their own IDs. Root-only initialization may read and
validate this ID once into a managed ledger inherited by modules. Missing or
malformed identity must fail that initialization; do not guess from paths, ticket
IDs, or generate a replacement. Older runtimes omit `run`.

This additive identity is data, not authority, a secret, a durable path, or a
root-lineage assertion. The file includes no home/run directory, inputs, secret
values, policy, or grants. Existing secret rejection/redaction still applies if
copied data contains a runtime-known secret. Readers must tolerate additive fields;
there is no new format identifier or APP envelope field. The file lasts only for
the invocation; persist needed non-secret data through declared managed documents,
not the file path. See [Path and Resource Semantics](path-and-resource-semantics.md#invocation-runtime-metadata).

### Process Lifecycle After A Terminal Result

A terminal APP result does not prove that a process invocation has finished.
The process adapter waits up to **250 ms** for its leader and live process-group
members to exit, retaining the unreaped leader identity until boundary teardown
is confirmed. This exit grace is separate from the pre-terminal inactivity and
maximum wall-clock timeouts; those keep their existing timeout behavior.

Immediately after receiving a terminal frame, the process adapter validates its
APP type, version, invocation identity and successful output schema, before
observing exit. A pending protocol or successful-output `validation_error` is
preserved after confirmed termination; an exit-grace violation does not replace
that validation failure. Cancellation still wins, and unconfirmed cleanup
overrides a pending validation failure with `adapter_internal_error`.

If the boundary remains live with no pending validation failure, the adapter
terminates it using its checked local process cleanup and fails closed with
`protocol_error`, exit code `2`, and
`process boundary did not exit within 250 ms after terminal APP result`.
The existing protocol failure classification prevents automatic retries.
Cancellation observed after the terminal frame still wins, including during
evidence construction. Canceled lifecycle failures may retain the runtime
lifecycle diagnostic but do not promote plugin hints. Unconfirmed cleanup
produces `adapter_internal_error` and retains active bookkeeping for recovery;
the adapter does not wait for a live stderr stream to close.

The failed execution's `output` is `{}`. Separate execution-scoped evidence,
`transist_process_lifecycle_evidence.json` (`transist.process_lifecycle.v1`),
records `phase: post_result_exit`, `reason: process_exit_grace_exceeded`,
`exit_grace_ms`, `terminal_result_received`, a pre-cleanup `snapshot` containing
only `leader_alive`, `live_pids`, and `live_pgids`, and `cleanup: confirmed` or
`unconfirmed`. `snapshot_observation` is `observed` or `failed`; an unavailable
snapshot is `null` with `snapshot_observation_reason` set to
`process_lifecycle_snapshot_observation_failed`. Raw OS observation errors are
not persisted. An unavailable snapshot produces `adapter_internal_error` unless
a pending terminal validation failure takes precedence after confirmed cleanup.

APP type, protocol version and invocation identity are validated before any
original payload is retained. Evidence may include `original_terminal_status`,
`original_terminal_error`, and `original_output`. Output is retained only when
it satisfies the action output schema, including failed-plugin output. An empty
failed output that does not satisfy that schema retains only its safe error
summary in lifecycle evidence. Clean provider failures retain the existing
failed-result output and hint handling; only successful output is required to
match the action schema on that path. Invalid identity retains no original status, error,
output or hints. Pending validation failures are recorded as
`terminal_validation_error` in lifecycle evidence.
Eligible hints are retained only after confirmed cleanup with no observation or
validation failure and use ordinary checksum, containment and redaction
promotion. Evidence and hints redact known
raw and base64 secret variants; no raw APP frame, process arguments,
environment or executable paths are recorded in lifecycle diagnostics.
Lifecycle evidence carries an explicit SHA-256 checksum of its redacted staged
bytes before trusted promotion. This adds no APP or TEP fields.

Without a pending terminal validation failure, exit observation failures fail
closed as `adapter_internal_error`, with evidence reason
`process_exit_observation_failed`. Cleanup is still attempted; the same
confirmed/unconfirmed bookkeeping and cancellation rules apply.

### Policy Forwarding

Adapters forward additive policy metadata to APP plugins unchanged after
applying host-side enforcement. In particular, `policy.filesystem` and
`policy.network` are carried through when the adapter has received them from
TEP, so plugins can observe the same effective policy context the kernel
compiled. The HTTP adapter enforces network policy before every transport
phase. See [Network Policy](network-policy.md) for the field-level contract.

### Requested Capabilities

APP invoke requests also carry `requested_capabilities`, the exact normalized
capability subset the kernel evaluated for the invoke (forwarded unchanged
from the TEP invoke request). Each item is
`{ capability, resource, operations }`; wildcard resources and operations are
not supported. Plugins must treat the array as observational: it does not grant
authority and cannot widen host-side enforcement.

## Transport Shapes

- `process` adapters commonly use NDJSON over `stdin` and `stdout`.
- `http` adapters commonly use HTTP endpoints.
- `mcp` adapters map APP semantics onto MCP transport behavior.

## Related

- [TEP (Transist Extension Protocol)](tep.md)
- [Adapter Responsibilities](../handbook/06-architecture-and-extension/04-adapter-responsibilities.md)
- [Plugin Support Matrix](plugin-support.md)
