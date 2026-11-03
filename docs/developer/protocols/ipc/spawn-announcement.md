# Reference: Spawn Announcement

The **Spawn Announcement** is a single line of JSON that the `transist` command prints to its **standard output (stdout)** whenever it starts a background runner process.

## Purpose

It provides a reliable way for parent processes (like a CI runner, a supervisor, or a test script) to discover the details of the new background process without having to search through log files or poll the filesystem.

## Wire Format

The announcement is always a single, compact JSON line:

```json
{"schema":"transist.runner_spawned.v1","run_id":"my-run-123","runner_pid":4567,"runner_log":"/path/to/runner.log"}
```

## Fields

| Field | Description |
| :--- | :--- |
| **`schema`** | Always `transist.runner_spawned.v1`. |
| **`run_id`** | The ID of the workflow run the background process is managing. |
| **`runner_pid`** | The operating system process ID (PID) of the new background runner. |
| **`runner_log`** | The full, absolute path to the runner's log file (where its stdout and stderr are redirected). |

## How to Consume This

If you are writing a script or tool that calls Transist:

1. **Read stdout line by line.**
2. **Filter for JSON.** Look for a line that begins with `{"schema":"transist.runner_spawned.v1"`.
3. **Parse the data.** Use the `runner_pid` to monitor the process or `runner_log` to stream its progress back to your own users.

**Important Note:** Human-readable status messages and banners from the CLI are sent to **stderr**, so you can safely capture stdout for this machine-readable signal.

---

### Versioned Public Surface

The `transist.runner_spawned.v1` schema is a versioned public surface. `v1`
is a format version, not a SemVer 1.0 stability promise. It is protected in
the current minor line: we may add new fields in the future, but we will not
remove the existing ones without a version bump to `v2` and migration
guidance.
