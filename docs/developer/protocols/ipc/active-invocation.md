# Active Invocation Record

## Purpose

- Locates **live external processes** (process / http / mcp adapters)
  that belong to a specific run+step.
- Used by cross-process cancellation to deliver signals to the right
  process group without depending on the original spawning process.

## Location

- `<run-root>/active_invocations/<invoke-id>.json`.

## Schema

- Schema string: `transist.active_invocation.v1`.
- Field name: `schema`.

## Wire format

```json
{
  "schema": "transist.active_invocation.v1",
  "invoke_id": "<uuid>",
  "run_id": "<run-id>",
  "step_id": "<step-id>",
  "adapter_kind": "process",
  "pid": 12345,
  "pgid": 12345,
  "executor_pid": 12300,
  "started_at_ms": 1730800000000,
  "process_birth_token": "<opaque-os-identity>"
}
```

## Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `schema` | string | yes | `transist.active_invocation.v1` |
| `invoke_id` | string | yes | Stable invoke id assigned by the kernel |
| `run_id` | string | yes | Run id that owns the invocation |
| `step_id` | string | yes | Step id within the run |
| `adapter_kind` | string | yes | `process`, `http`, or `mcp` |
| `pid` | integer | yes | PID of the spawned root process |
| `pgid` | integer | no | Process group id (Unix); enables `-pgid` signalling to descendants |
| `executor_pid` | integer | yes | PID of the Transist process that owns the worker |
| `started_at_ms` | integer | yes | Spawn timestamp (Unix ms) |
| `process_birth_token` | string | no | Opaque OS identity captured at spawn time; records without it are never signalled |

## Cancel sentinel

- A sibling file `<run-root>/active_invocations/<invoke-id>.cancel`
  signals "this invocation was canceled out-of-band". The owning
  adapter checks for it before classifying a non-zero exit as a
  failure-vs-cancel outcome.
- Producers: durable filesystem control requests, kernel cancellation
  paths.
- Consumers: the spawning adapter (process / http / mcp).

## Producer

- The adapter writes the record immediately after spawning the child
  and removes it once the invocation finishes (success, failure, or
  cancel).

## Cancellation producer surface

- The supported producer surface is `transist runs cancel <RUN_ID>` or the
  supported producer API that writes the durable control file under the
  Transist home.
- The companion server is read-only and never writes control-request
  files.

## Consumer expectations

- Cross-process cancellation lists records under `active_invocations/` and
  resolves from the root run through active subflow descendants to the deepest
  active invoke step. It never treats a control, loop, or subflow parent as an
  invocation target. An unpublished child is not an error; malformed or
  inconsistent existing child locators fail closed.
- Cancellation requires the resolved invoke step id before it can signal
  anything. Records for other steps are ignored and never receive a sentinel.
- Before writing the sentinel or sending `SIGTERM`, cancellation confirms the
  PID birth token and, when present, the process-group identity. It repeats
  both checks immediately before `SIGKILL` escalation.
- Cancellation reports absent, terminated, killed, permission-denied,
  identity-mismatch, signal-failure, and timeout outcomes. Only confirmed
  absence or termination removes the active record; failed or unconfirmed
  attempts retain it, and their sentinel is removed.
- Observers MAY use the records to display "process tree" badges in a
  TUI but MUST NOT signal processes that they did not themselves spawn
  unless coordinated with the run lease holder.

## Stability

- Part of the v1 IPC contract.
- Field additions are additive within `v1`.
