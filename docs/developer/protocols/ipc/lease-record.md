# Reference: Run Tree Lease Record (`lease.json`)

The **Run Tree Lease Record** is a special file that ensures **exclusive write access** to a workflow run. It prevents multiple processes from trying to manage or modify the same run at the same time, which could lead to data corruption.

## Purpose

Ordinary installed `workflows check` and authorization validation create no run
or run-tree lease and perform no live plugin probes. Passing validation does
not replace live preparation, kernel authority, or the recovery lease contract.
Use `check --live` or `--output` when live readiness evidence is required.

- **Locking:** It acts like a "lock file" or a "pidfile." Before any Transist process (like a background runner) starts working on a run, it must acquire this lease.
- **Ownership:** It tells other tools (like the TUI or the companion server) exactly which process is currently "in charge" of the run.
- **Safety:** It allows Transist to detect and handle "orphaned" runs where the managing process has crashed or been killed.

## File Location

The lease file is located in the run's directory:
`<home>/runs/run_<run-id>/lease.json`

## Wire Format

```json
{
  "schema_version": "transist.run_tree_lease.v1",
  "root_run_id": "my-run-123",
  "owner_id": "my-laptop:12345",
  "pid": 12345,
  "hostname": "my-laptop",
  "command": "transist workflows run my_flow@v1 --run-ref my-run",
  "acquired_at_ms": 1730800000000,
  "last_heartbeat_ms": 1730800012345
}
```

*Note: This artifact uses the field name `schema_version` instead of `schema` for historical reasons in v1.*

## Fields

| Field | Description |
| :--- | :--- |
| **`schema_version`** | Always `transist.run_tree_lease.v1`. |
| **`root_run_id`** | The ID of the run covered by this lease. |
| **`pid`** | The operating system process ID (PID) of the current owner. |
| **`hostname`** | The name of the machine where the owner is running. |
| **`command`** | The exact command that the owner is executing. |
| **`acquired_at_ms`** | A Unix timestamp of when the lease was first taken. |
| **`last_heartbeat_ms`** | A timestamp that the owner updates periodically to show it is still alive. |

---

## How it Works

### 1. Acquisition

When you run `transist workflows run` or `transist runs start`, the CLI first checks if a `lease.json` file already exists.

- If it doesn't, the CLI creates it and takes ownership.
- If it *does* exist, the CLI checks the `pid` and `last_heartbeat_ms`. If the owner is still alive and active, the CLI will refuse to start and will show a "Lease Conflict" error.

### 2. Heartbeats

While the workflow is running, the background runner periodically updates the `last_heartbeat_ms` field. This tells other tools, "I'm still here and working."

### 3. Release

Once the workflow finishes gracefully, the runner deletes the `lease.json` file, allowing other commands (like `runs cleanup`) to take control.

### 4. Handling Crashes (Orphan Leases)

If a process crashes, the `lease.json` file will be left behind. Other Transist commands can detect this because the PID will no longer be active or the heartbeat will be too old. In these cases, you can use the `transist runs lease break` command to manually remove the stale lease.

---

### Versioned Public Surface

The `transist.run_tree_lease.v1` schema is a versioned public surface. `v1`
is a format version, not a SemVer 1.0 stability promise. It is protected in
the current minor line: the fundamental logic of lease acquisition and
heartbeats will remain consistent within this minor line; a change requires
a version bump to `v2` and migration guidance.
