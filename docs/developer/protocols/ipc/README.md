# IPC Contract Reference

This documentation describes the **Inter-Process Communication (IPC) contract** for Transist. It defines how different parts of the system—the kernel, the CLI, the TUI, the companion server, and even your own custom tools—talk to each other.

In Transist, the **filesystem is the message bus**. There is no background daemon or network socket. Instead, cooperating processes coordinate by reading and writing specific JSON files (artifacts) in the Transist home directory.

## Versioning and Stability

Every IPC artifact includes a `schema` (or `schema_version`) field. This allows different versions of Transist tools to understand exactly what data they are reading.

- **Versioned public surface (pre-1.0):** This contract is part of the `v1` product surface. `v1` is a format version, not a SemVer 1.0 stability promise. It is protected in the current minor line: field additions are additive, and we will not remove or change the meaning of a field without bumping the version number and providing migration guidance. Stability begins only at `1.0.0`.
- **Strict Matching:** Tools will reject any file with a schema version they don't recognize.

### Common Schema Versions

- `transist.run_state.v1`: The main state of a workflow run.
- `transist.runner_spawned.v1`: A signal that a new background runner has started.
- `transist.gate_decision.v1`: A record of a manual approval or rejection.

---

## Core Artifacts

These files are the building blocks of Transist's communication.

| Document | Who writes it? | Who reads it? | What is it for? |
| :--- | :--- | :--- | :--- |
| [**Run State**](run-state.md) | Background Runner | Everyone | The source of truth for a run's status. |
| [**Spawn Announcement**](spawn-announcement.md) | CLI Foreground | CLI Foreground | Confirms that a background runner process was successfully started. |
| [**Gate Decision**](gate-decision.md) | CLI / User | Background Runner | Tells a paused workflow to continue or stop. |
| [**Control Request**](control-request.md) | CLI / User | Background Runner | Commands like "cancel" sent to a running workflow. |
| [**Run Tree Lease**](lease-record.md) | Background Runner | Everyone | Prevents multiple processes from trying to manage the same run at once. |
| [**Live Progress**](live-progress.md) | Runner / Adapter | Everyone | Real-time status updates for the rich UI and dashboards. |

---

## Building Your Own Tools

If you want to build a custom dashboard or a tool that interacts with Transist, the pattern is simple:

1. **Read the index**: Find all runs by looking at the `.transist/runs/` directory.
2. **Monitor state**: Watch the `state.json` file in a run's directory for changes.
3. **Attach to progress**: For a live feel, tail the `runner.log` file and watch the `.transist-live-progress.json` file.
4. **Drive decisions**: Use the `transist` CLI (e.g., `transist runs gate approve`) to send commands. This is safer and easier than writing decision files manually.
5. **Spawn runs**: Call `transist runs start` or `transist workflows run` and parse the resulting JSON output to get the new run ID.

---

### Related Technical Docs

- [**Storage Layout Reference**](../../../public/reference/storage-layout.md)
- [**Kernel Architecture**](../../architecture/kernel-contract.md)
- [**Stability Contract**](../../../public/reference/compatibility-and-stability.md)
