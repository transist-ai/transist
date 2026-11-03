// SPDX-License-Identifier: MPL-2.0
//! Public metrics models and query traits.

use super::*;

/// Aggregated usage totals for a provider/model combination.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageBreakdownEntry {
    /// Provider id when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Aggregated input tokens when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Aggregated output tokens when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Aggregated total tokens when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Number of usage reports merged into this breakdown row.
    pub usage_reports: u64,
}

/// Metrics summary for one concrete step execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepExecutionMetricsSummary {
    /// Run id that owns the execution.
    pub run_id: String,
    /// Step id that produced the execution.
    pub step_id: String,
    /// Execution number.
    pub execution: u32,
    /// Step kind such as `invoke`, `control`, or `subflow`.
    pub step_kind: String,
    /// Adapter family when the execution is an invoke step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_kind: Option<String>,
    /// Action id when the execution is an invoke step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Execution status string.
    pub status: String,
    /// Execution start timestamp in Unix milliseconds when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Execution finish timestamp in Unix milliseconds when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Execution duration in milliseconds when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Provider id when reported by the adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id when reported by the adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Input tokens when reported by the adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens when reported by the adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Total tokens when reported by the adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Flattened numeric metrics reported by the adapter.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub adapter_metrics: BTreeMap<String, f64>,
    /// Persisted artifact count for the execution.
    pub artifact_count: usize,
    /// Persisted evidence count for the execution.
    pub evidence_count: usize,
}

/// Aggregated metrics summary across all executions of one step.
///
/// Numeric fields are **subtree-inclusive**: for a `subflow` step they include every
/// execution, artifact, and token contributed by the child run and every run nested below
/// it. For `invoke` and `control` steps the subtree equals the step itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepMetricsSummary {
    /// Run id that owns the step.
    pub run_id: String,
    /// Step id being summarized.
    pub step_id: String,
    /// Step kind such as `invoke`, `control`, or `subflow`.
    pub step_kind: String,
    /// Adapter family when the step is an invoke step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_kind: Option<String>,
    /// Action id when the step is an invoke step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Total recorded executions across the step's subtree.
    pub execution_count: u32,
    /// Count of executions grouped by status string across the step's subtree.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub status_counts: BTreeMap<String, u32>,
    /// Latest execution summary for the step itself (not aggregated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_execution: Option<StepExecutionMetricsSummary>,
    /// Per-execution detail for the step itself (not aggregated across subflow runs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executions: Vec<StepExecutionMetricsSummary>,
    /// Persisted artifacts produced across the step's subtree.
    pub artifacts_produced: usize,
    /// Persisted evidence files produced across the step's subtree.
    pub evidence_files_produced: usize,
    /// Input tokens across the step's subtree when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens across the step's subtree when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Total tokens across the step's subtree when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Provider/model usage breakdown rows across the step's subtree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_breakdown: Vec<UsageBreakdownEntry>,
    /// Child run id when the step is a subflow and the child run is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
    /// Summed active execution time in milliseconds across the step subtree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_execution_duration_ms: Option<u64>,
    /// Summed blocked manual-gate wait time in milliseconds across the step subtree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_wait_duration_ms: Option<u64>,
}

/// Compact metrics row used by run-list views. All numeric fields are subtree-inclusive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunListMetricsSummary {
    /// Stable run id.
    pub run_id: String,
    /// Total run duration in milliseconds when known (self-only; parent already waits on subflows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<u64>,
    /// Wall-clock run duration in milliseconds (alias of `total_duration_ms`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_clock_duration_ms: Option<u64>,
    /// Summed active execution time in milliseconds across the run subtree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_execution_duration_ms: Option<u64>,
    /// Summed blocked manual-gate wait time in milliseconds across the run subtree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_wait_duration_ms: Option<u64>,
    /// Total step executions across the run's entire subtree.
    pub total_step_executions: u32,
    /// Total artifacts produced across the run's entire subtree.
    pub total_artifacts_produced: usize,
    /// Total evidence files produced across the run's entire subtree.
    pub total_evidence_files_produced: usize,
    /// Total tokens across the run's entire subtree when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

/// Full run-level metrics summary. All numeric fields except timestamps and
/// `total_duration_ms` are subtree-inclusive (self + every nested subflow run).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunMetricsSummary {
    /// Stable run id.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Run creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// First run start timestamp in Unix milliseconds when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Final run finish timestamp in Unix milliseconds when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Terminal status label when the run itself is terminal (self-only, categorical).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_status: Option<String>,
    /// Wall-clock duration of this run in milliseconds (self-only; parent covers subflow time).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<u64>,
    /// Wall-clock duration of this run in milliseconds (alias of `total_duration_ms`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_clock_duration_ms: Option<u64>,
    /// Summed active execution time in milliseconds across the run subtree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_execution_duration_ms: Option<u64>,
    /// Summed blocked manual-gate wait time in milliseconds across the run subtree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_wait_duration_ms: Option<u64>,
    /// Currently active (dispatching or running) steps across the subtree.
    pub current_active_step_count: usize,
    /// Currently blocked manual gates across the subtree.
    pub blocked_gate_count: usize,
    /// Total recorded step executions across the subtree.
    pub total_step_executions: u32,
    /// Succeeded step executions across the subtree.
    pub succeeded_step_executions: u32,
    /// Failed step executions across the subtree.
    pub failed_step_executions: u32,
    /// Cancelled step executions across the subtree.
    pub cancelled_step_executions: u32,
    /// Persisted artifacts produced across the subtree.
    pub total_artifacts_produced: usize,
    /// Persisted evidence files produced across the subtree.
    pub total_evidence_files_produced: usize,
    /// Input tokens across the subtree when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens across the subtree when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Total tokens across the subtree when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Provider/model usage breakdown rows across the subtree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_breakdown: Vec<UsageBreakdownEntry>,
}

/// Complete metrics snapshot exposed by [`MetricsQueryPort`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MetricsSnapshot {
    /// Run-list rows keyed by run id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub run_list: BTreeMap<String, RunListMetricsSummary>,
    /// Full run summaries keyed by run id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runs: BTreeMap<String, RunMetricsSummary>,
    /// Per-run, per-step summaries keyed by run id then step id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, BTreeMap<String, StepMetricsSummary>>,
}

/// Read-only metrics query surface used by CLI, server, and library callers.
pub trait MetricsQueryPort: Send + Sync {
    /// Returns the full cached metrics snapshot.
    fn snapshot(&self) -> Result<MetricsSnapshot>;
    /// Returns run-level metrics for one run when present.
    fn get_run_metrics(&self, run_id: &str) -> Result<Option<RunMetricsSummary>>;
    /// Returns step-level metrics for one run/step pair when present.
    fn get_step_metrics(&self, run_id: &str, step_id: &str) -> Result<Option<StepMetricsSummary>>;
    /// Returns run-list rows keyed by run id.
    fn run_list_metrics(&self) -> Result<BTreeMap<String, RunListMetricsSummary>>;
    /// Renders the current metrics snapshot in Prometheus exposition format.
    fn render_prometheus(&self) -> Result<String>;
}
