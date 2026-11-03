# Inspection Event Summary Allowlist

This table documents the event-summary keys that may flow into `transist.inspection.snapshot.v1`.

| Event type | Allowed keys |
|---|---|
| `run_started` | `mode` |
| `run_resumed` | `resume_count`, `reset_steps` |
| `run_preflight_started` | none |
| `run_preflight_completed`, `run_preflight_failed` | `status`, `blocker_count`, `warning_count` |
| `run_finished`, `run_blocked`, `run_stopped` | `status` |
| `step_started` | `execution` |
| `step_finished`, `step_blocked`, `step_cancelled`, `step_failed` | `execution`, `status`, `exit_code` |
| `policy.denied` | `policy_id`, `reason_code`, `workflow_ref`, `adapter_kind`, `plugin_id`, `secret_ref` |
| `cleanup_started` | `trigger`, `step_count` |
| `cleanup_preflight_started` | `trigger` |
| `cleanup_preflight_completed`, `cleanup_preflight_failed` | `trigger`, `status`, `blocker_count`, `warning_count` |
| `cleanup_step_started` | `execution` |
| `cleanup_step_finished`, `cleanup_step_failed`, `cleanup_step_cancelled` | `execution`, `status`, `exit_code` |
| `cleanup_finished` | `trigger`, `state` |

Unknown event types serialize with `summary: { "unknown_event_type": true }`.
