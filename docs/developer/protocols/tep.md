# Reference: TEP (Transist Extension Protocol)

The **Transist Extension Protocol (TEP)** is the technical specification for how the Transist kernel communicates with its adapters (`process`, `http`, and `mcp`).

If you are building a new adapter for Transist, your tool must implement the **`tep.v1`** protocol.

TEP lets Transist treat external systems as controlled execution participants without baking their behavior into the runtime kernel.

## Protocol Version

- **Current Version:** `tep.v1`

## Message Types

Communication consists of four main message types:

1. **`tep.handshake.request.v1`**: The kernel asks the adapter to identify itself and its capabilities.
2. **`tep.handshake.response.v1`**: The adapter responds with `adapter { kind, version }` (`kind` is `process`, `http`, or `mcp`) and its `capabilities { actions, supports_cancel?, supports_health? }`.
3. **`tep.invoke.request.v1`**: The kernel asks the adapter to perform a specific action.
4. **`tep.invoke.result.v1`**: The adapter returns the outcome of that action.

Each message has a committed JSON Schema under `schemas/tep/` that matches the
serialized Rust type exactly; the unversioned aliases `invoke.schema.json` and
`result.schema.json` are pure `$ref`s to the canonical versioned schemas.

TEP covers the kernel-to-adapter boundary. APP covers the adapter-to-plugin boundary.

---

## The Invoke Request

When the kernel wants to run a step, it sends an `invoke.request`. Key fields include:

- **`invoke_id`**: A unique ID for this specific invocation.
- **`run_id`** / **`workflow_id`**: The owning run and workflow identifiers.
- **`step`**: `{ id, execution }` for the current step execution.
- **`adapter_kind`**: The adapter family (`process`, `http`, or `mcp`).
- **`action`**: The name of the action to perform (e.g., `exec`).
- **`input`**: The JSON payload containing the action's arguments.
- **`paths`**: A set of filesystem paths where the adapter can read/write data (see [Invoke Paths](#invoke-paths)).
- **`requested_capabilities`**: The exact normalized capability subset the kernel evaluated for this invoke (see [Capability Requests](#capability-requests)).
- **`timeout_ms`** / **`max_timeout_ms`**: Optional timeout budgets (omitted when unset).
- **`policy`**: Security settings, such as whether secret delivery is allowed.
- **`metadata`**: Host-provided bookkeeping metadata.

### Action Input Projection

Action `bindings.input_projection: action_data_v1` selects only an
adapter-owned clone for schema validation and transport, removing top-level
`secret_refs`. The original TEP input, metadata, persisted capability subset,
and upstream secret/policy checks retain their meaning. No serialized TEP
field is added. Without the binding, legacy action-input bytes are preserved.
See the [public manifest contract](../../public/reference/plugin-manifest.md#opt-in-action-input-projection).

### Additive policy metadata

The `policy` object may include additive runtime metadata:

- `remote_secret_delivery`
- `filesystem` (compiled step policy with `workspace_access {mode}` (`full` or `read_only`; `custom` is rejected with `filesystem_custom_mode_unsupported`) and top-level `read_allow_paths` / `write_allow_paths` for paths outside the workspace root)
- deployment-specific metadata that adapters do not need to understand in order
  to remain compatible

Adapters must treat unknown additive `policy` fields as forward-compatible.

### Invoke Paths

The kernel provides several pre-configured directories to the adapter. The
artifact and evidence paths are private per-invocation staging paths; they are
outside the final run tree and must be treated as write-only handoff paths.
The kernel validates and atomically promotes accepted files after the adapter
returns.

- **`run_root`**: An invocation-private staging root, not the durable run
  directory. It is removed after the invocation and must not be used for
  inspection or persistent state.
- **`artifacts_dir`**: Where the adapter should write user-facing output files during this invocation.
- **`evidence_dir`**: Where the adapter should write internal audit/log files during this invocation.
- **`workspace_dir`**: The declared, canonical execution workspace for adapter
  execution. It is derived from `workspace.path` at run birth and persists
  through resume, retry, cleanup, and replay; it is not the CLI current
  directory or the workflow source directory. Legacy records without a
  workspace retain the historical CLI-directory fallback.

### Runtime-owned gate decisions

An action that declares `transist_gate_decision_v1` can receive an optional
`transist.gate_decision.v1` object in its manifest-declared input field. The
kernel resolves the newest valid immutable record for the compiled gate binding
before request preparation and before invocation staging. It omits the field on
the initial iteration when no decision exists.

Adapters must not inspect `run_root`, `gates/`, or `gates/history/` to recover
feedback. Active gate state is intentionally consumed during loop re-entry;
only the kernel can inspect durable history and inject the validated record.

### Capability Requests

`requested_capabilities` is the exact normalized subset of the complete
capability request persisted in the recipe and run state. Each item is
`{ capability, resource, operations }`; wildcard resources and operations are
not supported. See the [public TEP reference](../../public/reference/tep.md)
for the canonical capability namespaces and resource forms.

For declared-workspace runs, the filesystem request is generated from that exact
canonical path. `workspace.access: full` can request workspace mutations;
`read_only` requests only list/read operations. Operator grants must match the
generated resource. Workflow source paths, guessed parents, and separate
worktree paths do not authorize the workspace. A child subflow inherits the
parent path and may narrow, but not widen, its access.

---

## The Invoke Result

After the action completes, the adapter returns an `invoke.result`. Key fields include:

- **`status`**: The outcome of the task (`success`, `failed`, or `cancelled`).
- **`exit_code`**: (For process-based plugins) The raw exit code of the tool.
- **`output`**: The JSON result of the action, matching its `output_schema`.
- **`artifacts` / `evidence`**: Lists of files created by the adapter.
- **`usage`**: (Optional) Data for tracking costs, such as token counts.
- **`error`**: (Optional) A structured error object if the action failed.

Adapters should report usage only when the underlying execution participant provides it. Transist does not infer model cost or token counts from ordinary output.

The host adapter enforces the action schema boundary: materialized input is
validated before handshake or transport dispatch, and successful output is
validated before artifact/evidence hints or output data are converted. Invalid
instances return `validation_error` with schema and instance paths only.

---

## Error Handling

Adapters must use our [**Canonical Error Codes**](../../public/reference/error-taxonomy.md) when reporting failures. This ensures that the kernel can provide clear, actionable feedback to the user.

---

### Implementation Details

For the exact Rust definitions and message structures, see the source at `crates/transist-protocol/src/tep.rs`.
