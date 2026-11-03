# Reference: Control Request Record

A **Control Request Record** is a JSON file used to send commands to an
active, running workflow. Currently, its primary use is the
**`cancel-request`** control file.

## Purpose

- **Cross-Process Signaling:** It allows one process (like the CLI or a web dashboard) to tell a completely separate background runner to stop what it's doing.
- **Graceful Termination:** The runner monitors these requests and, when it sees a `cancel` request, it can cleanly tear down any running scripts or API calls.
- **Auditability:** Like everything in Transist, the fact that a run was cancelled and who requested it is permanently recorded.

## File Location

Cancellation requests are stored outside the run tree so every descendant
shares one root-owned signal:
`<home>/control/runs/<root-run-id>/cancel-request.json`

## Wire Format

```json
{
  "schema_version": "transist.control_request.v1",
  "request_id": "unique-uuid-123",
  "request_type": "cancel",
  "root_run_id": "my-run-123",
  "requested_at_ms": 1730800000000,
  "requester": {
    "pid": 5678,
    "hostname": "my-laptop",
    "command": "transist runs cancel my-run-123"
  }
}
```

*Note: This artifact uses the field name `schema_version` instead of `schema` for historical reasons in v1.*

## Fields

| Field | Description |
| :--- | :--- |
| **`schema_version`** | Always `transist.control_request.v1`. |
| **`request_type`** | Currently, only `cancel` is supported. |
| **`root_run_id`** | The ID of the run you want to control. |
| **`requested_at_ms`** | A Unix timestamp of when the request was made. |
| **`requester`** | Metadata about who sent the request, including their PID, hostname, and the command they ran. |

---

## How to Cancel a Run

### Supported Producer Surface

The supported producer surface today is `transist runs cancel <RUN_ID>` plus
the supported producer API that writes the durable control file
(`request_run_cancellation`).

Companion server routes are read-only and never write control-request
files.

### The Background Process

When a cancel request is produced:

1. The producer writes the durable JSON control file.
2. `transist runs cancel` attempts immediate cancellation of the deepest
   verified active local invocation in the root run tree.
3. The background runner sees the file during its next "tick" (its main event loop),
   stops any active steps, and marks the root state as `canceled`.
4. If no live root-tree lease owner remains, the CLI starts one detached root
   runner to consume the request. A live lease prevents a competing runner.
5. The CLI polls persisted root state for up to 15 seconds and succeeds only
   after the root is durably `canceled`, including automatic canceled cleanup.

Repeated `runs cancel` while cancellation cleanup is running is idempotent. It
does not signal cleanup invocations, duplicate cleanup dispatch, or start a
competing runner. Failed or unexpected termination does not implicitly request
cancellation or select canceled cleanup. Abrupt process death may prevent cleanup
from completing; `runs cleanup` recovers unfinished or failed cleanup from
durable history without changing the primary outcome.

---

### Versioned Public Surface

The `transist.control_request.v1` schema is a versioned public surface. `v1`
is a format version, not a SemVer 1.0 stability promise. It is protected in
the current minor line: we may add new request types in the future, but the
fundamental way these files are handled will not change without a version
bump to `v2` and migration guidance.
