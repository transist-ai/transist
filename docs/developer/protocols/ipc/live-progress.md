# Live Progress Snapshot

## Purpose

- Compact, frequently-updated record of the most recent line emitted by
  the active step's tool, used to power live spinners and dashboards.
- Avoids the overhead of tailing the full event log just to render a
  one-line progress string.

## Location

- `<run-root>/.transist-live-progress.json`.

## Schema

- Schema string: `transist.live_progress.v1`.
- Field name: `schema`.

## Wire format

```json
{
  "schema": "transist.live_progress.v1",
  "total_lines": 1423,
  "updated_at_ms": 1730800012345,
  "last_line_preview": "fetched 42/100 records",
  "last_stream": "stdout"
}
```

## Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `schema` | string | yes | `transist.live_progress.v1` |
| `total_lines` | integer | yes | Cumulative line count produced by the active step |
| `updated_at_ms` | integer | yes | Last update timestamp (Unix ms) |
| `last_line_preview` | string | no | Truncated copy of the most recent stdout/stderr line |
| `last_stream` | string | no | `stdout` or `stderr` — origin of `last_line_preview` |

## Producer

- `crates/transist-adapters/src/process_runtime.rs::persist_live_progress`.
- The process adapter rewrites the file atomically as new lines stream
  from a child process.
- File presence implies a live step. Absence is normal between steps.

## Consumer expectations

- Read-only for observers. Watch the file via filesystem notifications
  to drive UI repaints.
- Reject documents whose `schema` is not understood. Missing `schema` is
  tolerated for one release window and treated as `transist.live_progress.v1`.
- The CLI's own spinner reads the document loosely (does not enforce
  `schema`). Strict consumers SHOULD enforce.

## Lifetime

- Created when the first line is emitted in a step.
- Updated continuously while the step runs.
- Left in place when the step finishes; overwritten by the next step.
- Removed by `transist runs cleanup` along with the rest of the run
  scratch artifacts.

## Stability

- Part of the v1 IPC contract.
- Field additions are additive within `v1`.
