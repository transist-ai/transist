# Prometheus and Metric Families

Canonical metric families include:

- `transist_runs_created_total`
- `transist_runs_started_total`
- `transist_runs_finished_total`
- `transist_runs_finished_by_status_total`
- `transist_runs_replayed_total`
- `transist_runs_resumed_total`
- `transist_runs_active`
- `transist_runs_blocked_on_gates`
- `transist_step_executions_started_total`
- `transist_step_executions_finished_total`
- `transist_step_executions_finished_by_status_total`
- `transist_step_retries_total`
- `transist_gate_waits_total`
- `transist_gate_approvals_total`
- `transist_gate_rejections_total`
- `transist_run_duration_ms`
- `transist_step_execution_duration_ms`
- `transist_gate_wait_duration_ms`
- `transist_usage_input_tokens_total`
- `transist_usage_output_tokens_total`
- `transist_usage_total_tokens_total`
- `transist_usage_reports_total`
- `transist_metrics_rebuild_errors_total`
- `transist_metrics_export_errors_total`
- `transist_metrics_validation_errors_total`

## Label Constraints

Allowed labels stay bounded. They include workflow, status, step kind, adapter kind, action, provider, and model dimensions.

Forbidden labels include:

- `run_id`
- `step_id`
- arbitrary plugin metric names
- filenames
- prompt text
- raw user content

## Usage and Passthrough Semantics

- token metrics appear only when adapters or plugins report `usage`
- partial `usage` payloads increment only the reported counters
- `transist_usage_reports_total` increments whenever a `usage` object is present
- `transist_step_executions_started_total` rebuilds from persisted step-state `executions`, so it can lead finished-execution counts. The `executions` counter is append-only for the lifetime of a run: retry, resume, replay, cleanup, cancellation, and reused subflow reruns never reset it to zero, so this counter reflects the total number of dispatches under the run id.
- `transist_metrics_validation_errors_total` rebuilds from persisted per-execution invalid APP metric entries
- numeric adapter or plugin `metrics` stay in step summaries
- arbitrary passthrough names do not become first-class Prometheus series

## Histogram Semantics

Duration histograms are published for runs, step executions, and gate waits. Interpret them as local-instance duration distributions, not global fleet telemetry.
