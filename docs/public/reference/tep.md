# Reference: TEP (Transist Extension Protocol)

TEP defines the public kernel-to-adapter protocol used when Transist asks an adapter to execute an action.

## Protocol Version

- Current version: `tep.v1`

## Message Families

- `tep.handshake.request.v1` / `tep.handshake.response.v1`
- `tep.invoke.request.v1` / `tep.invoke.result.v1`

Each TEP message has a committed JSON Schema under `schemas/tep/` that matches
the serialized Rust type exactly (`additionalProperties: false`).
Unversioned aliases (`invoke.schema.json`, `result.schema.json`) are pure
`$ref`s to the canonical versioned schemas and never duplicate definitions.
`tests/schema_contract.rs` compiles every committed schema offline and
validates representative payloads serialized from `transist-protocol` against
the matching schema.

## Responsibilities

- The kernel sends an invoke request with the run context, action, input,
  scoped paths, requested capabilities, optional timeout budgets, and policy
  metadata.
- Each invoke also carries `requested_capabilities`, the exact normalized
  persisted authority subset for that checkpoint. The request uses
  `{ capability, resource, operations }`; wildcard resources and operations are
  not supported. Runtime regeneration is an integrity check only and cannot
  replace or expand the persisted subset.
- The adapter performs the work through its chosen transport.
- The adapter returns a structured result with status, optional exit code,
  captured stdout/stderr, timing, artifacts, evidence, metrics, output, and
  optional usage metadata.
- The adapter validates action input before dispatch and validates successful
  action output before converting hints or returning the result. Invalid
  instances use `validation_error` and are not copied into durable result data.
- The runtime also writes additive `transist_runtime_invoke_evidence.json` beside each invoke's evidence files. That runtime-owned record may include `producing_workflow { id, version, digest }` and `producing_plugin { id, version, digest }` summaries derived from the persisted run and invoke request metadata.

### Invocation Timing

Process, HTTP, and MCP results capture `timing.started_at` at adapter entry and
`timing.finished_at` when constructing the terminal result, as Unix-millisecond
strings. `timing.duration_ms` measures elapsed time using a monotonic clock,
including on failure and cancellation; it is not computed by subtracting wall
timestamps. Wall-clock adjustments are preserved, so finish can precede start;
sub-millisecond operations can have equal timestamps. Historical results are not
rewritten to adopt corrected timestamp capture.

### Action Input Projection

The original TEP invoke request remains authoritative for secret references,
metadata, and capability/policy checks. The additive action binding
`input_projection: action_data_v1` affects only the adapter's cloned action
input used for schema validation and transport, removing only top-level
`secret_refs`. It adds no TEP fields and does not alter request persistence or
kernel authority. Static readiness and live preflight use the same projection;
neither resolves secrets for schema validation. Without the binding, legacy
input bytes are preserved. See [Plugin Manifest](plugin-manifest.md#opt-in-action-input-projection).

### Handshake Response

The TEP handshake response identifies the adapter with `adapter { kind, version }`
(`kind` is `process`, `http`, or `mcp`) and advertises `capabilities { actions,
supports_cancel?, supports_health? }`.

### Policy Metadata

TEP `policy` is additive and may include:

- `filesystem` metadata with the effective policy source and declared/persisted workspace root, `workspace_access { mode }` (`full` or `read_only`; `custom` is rejected with `filesystem_custom_mode_unsupported`), and top-level `read_allow_paths` / `write_allow_paths` lists for external path grants outside `workspace_root`. A declared `read_only` workspace caps more permissive global or step policy.
- `network` metadata with the effective policy source, `mode`, `allow_hosts` /
  `allow_ports`, `tls_required`, and `max_redirects`. The HTTP host adapter
  enforces these fields before transport dispatch; other adapters may use them
  as adapter/plugin metadata.

Adapters must forward the policy they receive and treat unknown additive policy fields as forward-compatible. See [Network Policy](network-policy.md) for the network-specific contract and enforcement matrix.

Runtime-temporary directory roots are adapter-private request metadata, not a
TEP field. Plugin manifests declare `runtime://temp` or
`runtime://temp/<relative-path>` in process/MCP
`entrypoint.env`; the runtime resolves it only after request construction and
does not persist the host path in a TEP payload, recipe, run state, or event.

### Capability Requests

The governed policy profile supports only these capability namespaces:
`adapter`, `plugin`, `action`, `provider`, `secret`, `filesystem`, `network`,
and `approval`. Resources are canonicalized before evaluation. Examples are
`process`, `shell@0.1.0/exec`, `local`, `workspace:///work/repo`,
`run://artifacts`, `https://api.example.com:443`, and `demo/review`.

Missing, empty, unknown, removed, malformed, or partially matching requested
authority denies with `capability_not_granted` before secret resolution or
adapter dispatch. Grant it through an exact `policy.allow.capabilities` entry,
or, for the matching invoke request's adapter, plugin, action, and provider
capabilities, through a version-pinned `policy.allow.plugin_grants` entry.
Filesystem and network policy remain an independent operator ceiling; a
capability grant cannot widen either policy.
Filesystem operations are derived from the effective policy, so read-only
resources never request mutations. Network requests include the plugin base URL
and every effective destination; each redirect destination needs its own grant.
Governed invokes require pinned plugin versions, and malformed persisted
capability requests deny rather than receiving a synthetic version. Manual
approval checkpoints persist separate `approval:request` and
`approval:decide` requests; a request grant cannot be used to decide and a
decide grant cannot open a gate.

The workspace capability is generated from the canonical `workspace.path` at
run birth. Workflow source paths, the CLI current directory, and a parent of the
declared repository are different resources and do not authorize it. Exact
input-bound `read_allow_paths` may request secondary context as read-only
`file://` resources; they cannot create a second writable workspace. See [Path
and Resource Semantics](path-and-resource-semantics.md) for capability-resource
and action-path distinctions.

## Boundary

TEP covers the kernel-to-adapter boundary. APP covers the adapter-to-plugin boundary.

## Related

- [APP (Adapter Plugin Protocol)](app.md)
- [Adapter Responsibilities](../handbook/06-architecture-and-extension/04-adapter-responsibilities.md)
- [Error Taxonomy](error-taxonomy.md)
