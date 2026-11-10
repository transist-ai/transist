// SPDX-License-Identifier: MPL-2.0
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use transist_core::{RunLineage, RunTreeLeaseRecord};
use transist_protocol::{KernelValue, WorkspaceAccessPolicy};
pub use transist_storage_local::BlobFamily;

/// Default page size for paginated inspection queries.
pub const DEFAULT_PAGE_LIMIT: usize = 50;
/// Maximum allowed page size for paginated inspection queries.
pub const MAX_PAGE_LIMIT: usize = 200;
/// Default number of events returned when a caller does not specify a limit.
pub const DEFAULT_EVENT_LIMIT: usize = 100;
/// Maximum number of events returned by a single inspection query.
pub const MAX_EVENT_LIMIT: usize = 500;

/// Sort direction for event listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Oldest items first.
    Asc,
    /// Newest items first.
    Desc,
}

/// Generic limit/offset pagination parameters.
#[derive(Debug, Clone, Default)]
pub struct PaginationQuery {
    /// Requested page size. `0` falls back to [`DEFAULT_PAGE_LIMIT`].
    pub limit: usize,
    /// Number of items to skip from the front of the ordered result set.
    pub offset: usize,
}

/// Scope selector for run-list queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunListScope {
    /// Return only root runs in the main run browser.
    #[default]
    Roots,
    /// Return both root runs and nested sub-runs.
    All,
}

/// Filters and pagination for listing runs.
#[derive(Debug, Clone, Default)]
pub struct RunListQuery {
    /// Shared limit/offset controls.
    pub pagination: PaginationQuery,
    /// Whether the caller wants only roots or every run in the lineage tree.
    pub scope: RunListScope,
    /// Optional run status filter.
    pub status: Option<String>,
    /// Optional workflow id or `<id>@<version>` workflow reference filter.
    pub workflow: Option<String>,
    /// Lower exclusive bound for `created_at_ms`.
    pub created_after_ms: Option<u64>,
    /// Upper exclusive bound for `created_at_ms`.
    pub created_before_ms: Option<u64>,
    /// Lower exclusive bound for `updated_at_ms`.
    pub updated_after_ms: Option<u64>,
    /// Upper exclusive bound for `updated_at_ms`.
    pub updated_before_ms: Option<u64>,
}

/// Filters for listing runtime events from one run.
#[derive(Debug, Clone)]
pub struct EventListQuery {
    /// Requested event count. `0` falls back to [`DEFAULT_EVENT_LIMIT`].
    pub limit: usize,
    /// Optional upper exclusive event sequence bound.
    pub before: Option<u64>,
    /// Optional lower exclusive event sequence bound.
    pub after: Option<u64>,
    /// Whether to return events oldest-first or newest-first.
    pub order: SortOrder,
}

/// Scope selector for blob list queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlobListScope {
    /// Return only blobs owned directly by the selected run.
    #[default]
    SelfOnly,
    /// Return blobs owned by the selected run and every descendant run.
    Subtree,
}

impl Default for EventListQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_EVENT_LIMIT,
            before: None,
            after: None,
            order: SortOrder::Desc,
        }
    }
}

/// Generic paginated response wrapper.
#[derive(Debug, Clone)]
pub struct Page<T> {
    /// Current page items.
    pub data: Vec<T>,
    /// Total item count before pagination.
    pub total: usize,
    /// Effective page size after normalization.
    pub limit: usize,
    /// Effective offset applied to the result set.
    pub offset: usize,
}

/// Compact run row used by run-list views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionRunSummary {
    /// Stable run identifier.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Current run status as a lowercase string.
    pub status: String,
    /// Run creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Last update timestamp in Unix milliseconds.
    pub updated_at_ms: u64,
    /// First started-at timestamp observed for the run.
    pub started_at_ms: Option<u64>,
    /// Final finished-at timestamp observed for the run.
    pub finished_at_ms: Option<u64>,
    /// Currently active step id when one exists.
    pub current_step_id: Option<String>,
    /// Whether the run currently has blocked manual gates.
    pub has_blocked_gates: bool,
    /// Whether the run has any persisted artifacts or evidence.
    pub has_artifacts: bool,
    /// Whether the run has any persisted runtime events.
    pub has_events: bool,
    /// Total persisted artifact count.
    pub artifact_count: usize,
    /// Total persisted evidence count.
    pub evidence_count: usize,
    /// Root run id for the lineage tree containing this run.
    pub root_run_id: String,
    /// Optional direct parent run id for embedded or replayed runs.
    pub parent_run_id: Option<String>,
}

/// Current active step pointer for a run, when one exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCurrentStep {
    /// Stable step id.
    pub step_id: String,
    /// Current step status as a lowercase string.
    pub status: String,
}

/// Full run detail assembled from run state, blobs, gates, and events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionRunDetail {
    /// Compact run summary for the same run.
    pub summary: InspectionRunSummary,
    /// Materialized workflow inputs used to start the run.
    pub workflow_inputs: BTreeMap<String, KernelValue>,
    /// Persisted global default filesystem policy inferred from effective step policies when available.
    pub global_default_filesystem_policy: Option<InspectionFilesystemPolicySummary>,
    /// Workflow-level filesystem policy retained in the persisted recipe when available.
    pub workflow_filesystem_policy: Option<InspectionFilesystemPolicySummary>,
    /// Current active step pointer when one exists.
    pub current_step: Option<RunCurrentStep>,
    /// Descendant run that currently represents the live or failed focus, when deeper than `summary.run_id`.
    pub actionable_run_id: Option<String>,
    /// Terminal outcome label when the run is terminal.
    pub terminal_outcome: Option<String>,
    /// Failed-run recoverability state when one exists.
    pub recovery_state: Option<String>,
    /// Most recent persisted preflight summary when available.
    pub preflight: Option<InspectionPreflightSummary>,
    /// Persisted cleanup summary when one exists.
    pub cleanup: Option<InspectionCleanupSummary>,
    /// Live root-run-tree lease metadata when one is currently present.
    pub active_lease: Option<RunTreeLeaseRecord>,
    /// Manual gates currently blocking the run.
    pub blocked_gates: Vec<InspectionGate>,
    /// Inspection view of every step in the run.
    pub steps: Vec<InspectionStep>,
    /// Total persisted artifact count.
    pub artifact_count: usize,
    /// Total persisted evidence count.
    pub evidence_count: usize,
    /// Total persisted event count.
    pub event_count: usize,
    /// Latest event id seen for the run.
    pub latest_event_id: Option<String>,
    /// Optional lineage metadata for the run.
    pub lineage: Option<RunLineage>,
    /// Child runs discovered beneath this run.
    pub contained_runs: Vec<InspectionRunSummary>,
    /// Count of steps grouped by status label.
    pub step_counts: BTreeMap<String, usize>,
    /// Whether trusted-execution mode was enabled for this run.
    pub trusted_execution_mode: bool,
    /// Effective runtime operating mode recorded for the run.
    #[serde(default)]
    pub runtime_mode: String,
    /// Effective policy profile recorded for the run.
    #[serde(default)]
    pub policy_profile: String,
    /// Whether capability grants were enforced or explicitly bypassed.
    #[serde(default)]
    pub capability_enforcement: String,
    /// Trust and provenance summary derived from persisted run state.
    pub trust: InspectionTrustSummary,
    /// Provider locality and external-transfer summary for the run.
    pub locality: InspectionLocalityView,
    /// How operators can obtain the exported evidence report.
    pub evidence_export: InspectionEvidenceExportView,
}

/// Snapshot of the persisted run-start policy decision used for inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionPolicyDecision {
    /// Whether the policy allowed execution.
    pub allowed: bool,
    /// Policy checkpoint or execution mode that produced the decision.
    pub mode: String,
    /// Stable policy reason code when one exists.
    pub reason_code: Option<String>,
    /// Human-readable policy message when one exists.
    pub message: Option<String>,
    /// Effective runtime operating mode recorded for the run.
    #[serde(default)]
    pub runtime_mode: String,
    /// Effective policy profile recorded for the run.
    #[serde(default)]
    pub policy_profile: String,
    /// Whether capability grants were enforced or explicitly bypassed.
    #[serde(default)]
    pub capability_enforcement: String,
}

/// Trust metadata for one workflow or plugin subject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionTrustSubject {
    /// Stable workflow or plugin id.
    pub id: String,
    /// Stable workflow or plugin version.
    pub version: String,
    /// Recomputed digest captured for the subject.
    pub digest: String,
    /// Signature/trust status: `verified`, `denied`, `bypassed`, or `not_verified`.
    pub signature_status: String,
    /// Signer key id when one was present in the detached signature.
    pub signer_key_id: Option<String>,
}

/// Run-level trusted-execution decision surfaced for inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionTrustDecision {
    /// Run-level trust outcome: `verified`, `denied`, or `bypassed`.
    pub outcome: String,
    /// Stable reason code when one exists.
    pub reason: Option<String>,
    /// Verification surface that failed when the run was denied.
    pub failed_verification_type: Option<String>,
}

/// Trust and provenance summary for one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionTrustSummary {
    /// Whether trusted-execution mode was enabled.
    pub trusted_execution_mode: bool,
    /// Workflow trust metadata when present.
    pub workflow: Option<InspectionTrustSubject>,
    /// Plugin trust metadata for referenced plugins.
    pub plugins: Vec<InspectionTrustSubject>,
    /// Run-level trust outcome when trusted-execution mode participated.
    pub decision: Option<InspectionTrustDecision>,
    /// Persisted run-start policy decision when provenance exists.
    pub policy_decision: Option<InspectionPolicyDecision>,
}

/// Per-step provider locality row derived from provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionLocalityStep {
    /// Step id that reported provider/model usage.
    pub step_id: String,
    /// Provider identifier when known.
    pub provider: Option<String>,
    /// Model identifier when known.
    pub model: Option<String>,
    /// Declared provider locality marker.
    pub provider_kind: Option<String>,
}

/// One recorded external-transfer evidence item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionExternalTransfer {
    /// Step id that triggered the external call.
    pub step_id: String,
    /// Step execution number.
    pub execution: u32,
    /// Timestamp in Unix milliseconds when the transfer was recorded.
    pub timestamp_ms: u64,
    /// Best-effort destination identifier.
    pub destination: Option<String>,
    /// Adapter kind used for the call.
    pub adapter_kind: String,
    /// Provider identifier when known.
    pub provider: Option<String>,
    /// Declared provider locality marker.
    pub provider_kind: String,
    /// Bytes sent when measurable.
    pub bytes_sent: Option<u64>,
    /// SHA-256 hash of the sent payload when captured.
    pub content_hash: Option<String>,
    /// Execution mode that governed the transfer evidence.
    pub policy_mode: String,
}

/// Provider locality plus external-transfer evidence for one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionLocalityView {
    /// Provider/model rows keyed by step execution provenance.
    pub steps: Vec<InspectionLocalityStep>,
    /// External-transfer evidence records. Local-only runs keep this empty.
    pub external_transfers: Vec<InspectionExternalTransfer>,
}

/// Location metadata for the exported evidence report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionEvidenceExportView {
    /// Where an operator can retrieve or generate the report, when provenance exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Stable command hint for generating the export.
    pub command: String,
    /// Export format currently supported by the runtime.
    pub format: String,
    /// Timestamp associated with the persisted provenance used for the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at_ms: Option<u64>,
}

/// Supported local inspection sync policy modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPolicyMode {
    /// Share metadata only.
    #[default]
    MetadataOnly,
    /// Share additional operator-approved data for trusted governed runs.
    Extended,
}

impl SyncPolicyMode {
    /// Returns the stable serialized identifier for the mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata_only",
            Self::Extended => "extended",
        }
    }
}

/// Explicit sync policy descriptor shared by runtime snapshots and status views.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPolicyConfig {
    /// Selected sync mode.
    #[serde(default)]
    pub mode: SyncPolicyMode,
    /// Include workflow input values when sync mode is `extended`.
    #[serde(default)]
    pub include_workflow_input_values: bool,
    /// Include raw event payloads when sync mode is `extended`.
    #[serde(default)]
    pub include_event_payloads: bool,
    /// Include full run details when sync mode is `extended`.
    #[serde(default)]
    pub include_run_detail_full: bool,
    /// Include stderr tails when sync mode is `extended`.
    #[serde(default)]
    pub include_stderr_tail: bool,
    /// Include gate request summaries when sync mode is `extended`.
    #[serde(default)]
    pub include_gate_request_summary: bool,
}

impl SyncPolicyConfig {
    /// Returns the enabled include flags in display order.
    pub fn enabled_include_flags(&self) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.include_workflow_input_values {
            flags.push("include_workflow_input_values");
        }
        if self.include_event_payloads {
            flags.push("include_event_payloads");
        }
        if self.include_run_detail_full {
            flags.push("include_run_detail_full");
        }
        if self.include_stderr_tail {
            flags.push("include_stderr_tail");
        }
        if self.include_gate_request_summary {
            flags.push("include_gate_request_summary");
        }
        flags
    }
}

/// Metadata-only snapshot view of one run detail record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionRunDetailSummary {
    /// Compact run summary for the same run.
    pub summary: InspectionRunSummary,
    /// Declared workflow input keys that were present on the run.
    pub input_keys: Vec<String>,
    /// Best-effort runtime types inferred from persisted workflow input values.
    pub input_types: BTreeMap<String, String>,
    /// Presence flag retained for each persisted workflow input key.
    pub input_presence: BTreeMap<String, bool>,
    /// Best-effort input source labels. Current persisted runs do not record
    /// origin metadata, so the snapshot emits `"unknown"` for every key.
    pub input_sources: BTreeMap<String, String>,
    /// Optional workflow input values when extended sync explicitly allows them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_input_values: Option<BTreeMap<String, KernelValue>>,
    /// Optional parallel full run detail when extended sync explicitly allows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_detail_full: Option<InspectionRunDetail>,
    /// Gate ids currently blocking the run.
    pub blocked_gate_ids: Vec<String>,
    /// Number of workflow steps present in the run detail.
    pub step_count: usize,
}

impl InspectionRunDetailSummary {
    /// Builds a metadata-only run detail summary from a full inspection detail.
    #[must_use]
    pub fn from_detail(detail: &InspectionRunDetail, policy: &SyncPolicyConfig) -> Self {
        let mut input_keys = detail.workflow_inputs.keys().cloned().collect::<Vec<_>>();
        input_keys.sort();

        let input_types = detail
            .workflow_inputs
            .iter()
            .map(|(key, value)| (key.clone(), kernel_value_type_label(value).to_string()))
            .collect();
        let input_presence = detail
            .workflow_inputs
            .keys()
            .cloned()
            .map(|key| (key, true))
            .collect();
        let input_sources = detail
            .workflow_inputs
            .keys()
            .cloned()
            .map(|key| (key, "unknown".to_string()))
            .collect();
        let blocked_gate_ids = detail
            .blocked_gates
            .iter()
            .map(|gate| gate.gate_id.clone())
            .collect();

        let workflow_input_values = policy
            .include_workflow_input_values
            .then(|| detail.workflow_inputs.clone());
        let run_detail_full = (policy.include_run_detail_full
            || policy.include_stderr_tail
            || policy.include_gate_request_summary)
            .then(|| {
                let mut full = detail.clone();
                for step in &mut full.steps {
                    step.latest_output = None;
                }
                full
            });

        Self {
            summary: detail.summary.clone(),
            input_keys,
            input_types,
            input_presence,
            input_sources,
            workflow_input_values,
            run_detail_full,
            blocked_gate_ids,
            step_count: detail.steps.len(),
        }
    }
}

fn kernel_value_type_label(value: &KernelValue) -> &'static str {
    match value {
        KernelValue::Null => "null",
        KernelValue::Bool(_) => "boolean",
        KernelValue::Number(_) => "number",
        KernelValue::String(_) => "string",
        KernelValue::Array(_) => "array",
        KernelValue::Object(_) => "object",
    }
}

/// One persisted runtime event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionEvent {
    /// Monotonic event id within the run's event stream.
    pub event_id: String,
    /// Event timestamp in Unix milliseconds.
    pub timestamp_unix_ms: u64,
    /// Event kind string.
    pub event_type: String,
    /// Optional step id associated with the event.
    pub step_id: Option<String>,
    /// Structured event payload.
    pub payload: KernelValue,
}

/// Summary of the most recent persisted preflight report for a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionPreflightSummary {
    /// Aggregate preflight status as a lowercase string.
    pub status: String,
    /// Timestamp in Unix milliseconds when preflight ran.
    pub checked_at_ms: u64,
    /// Number of blocking checks.
    pub blocker_count: usize,
    /// Number of warning checks.
    pub warning_count: usize,
    /// Human-readable details for the blocking checks (failed entries only),
    /// preserved here so callers can surface the actual failure reason
    /// without re-reading the full persisted preflight payload.
    pub failed_checks: Vec<InspectionPreflightFailedCheck>,
}

/// One failed preflight check as surfaced to inspection consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionPreflightFailedCheck {
    /// Check family or kind name.
    pub kind: String,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Step ids associated with the finding.
    pub step_ids: Vec<String>,
    /// Optional plugin id associated with the finding.
    pub plugin_id: Option<String>,
    /// Optional action id associated with the finding.
    pub action: Option<String>,
}

/// Summary of the persisted cleanup phase for one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionCleanupSummary {
    /// Cleanup trigger used for this cleanup phase.
    pub trigger: String,
    /// Cleanup lifecycle state as a lowercase string.
    pub state: String,
    /// Cleanup start timestamp in Unix milliseconds when available.
    pub started_at_ms: Option<u64>,
    /// Cleanup finish timestamp in Unix milliseconds when available.
    pub finished_at_ms: Option<u64>,
    /// Cleanup preflight summary when one exists.
    pub preflight: Option<InspectionPreflightSummary>,
    /// Ordered cleanup step summaries.
    pub steps: Vec<InspectionCleanupStep>,
}

/// Inspection view of one cleanup step within a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionCleanupStep {
    /// Stable cleanup step id.
    pub step_id: String,
    /// Current cleanup step status as a lowercase string.
    pub status: String,
    /// Latest execution number when one exists.
    pub execution: Option<u32>,
    /// First started-at timestamp observed for the cleanup step.
    pub started_at_ms: Option<u64>,
    /// Final finished-at timestamp observed for the cleanup step.
    pub finished_at_ms: Option<u64>,
    /// Human-readable failure message when one was recorded.
    pub error_message: Option<String>,
}

/// Reference to one artifact or evidence blob produced by a step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionStepBlobRef {
    /// Step id that produced the blob.
    pub step_id: String,
    /// Execution number that produced the blob.
    pub execution: u32,
    /// Relative path within the blob family.
    pub relative_path: String,
    /// File name component of the blob path.
    pub name: String,
    /// Blob size in bytes.
    pub bytes: u64,
}

/// Root against which a declared workflow-file locator is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionWorkflowFileLocatorKind {
    /// A path relative to the trusted workspace root.
    Workspace,
    /// A path relative to the trusted repository state directory (`.transist`).
    RepositoryState,
    /// A path relative to the trusted run root.
    Run,
}

/// Reference to one declared workflow file produced or refreshed by a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionWorkflowFileRef {
    /// Step id that declared the file.
    pub step_id: String,
    /// Request or result field name that referenced the file path.
    pub field_name: String,
    /// Root used to resolve the relative locator.
    pub locator_kind: InspectionWorkflowFileLocatorKind,
    /// Path relative to the selected trusted root.
    pub relative_path: String,
    /// Absolute path resolved by local inspection for terminal display only.
    /// Never serialized, exported, or used as authority for HTTP content reads.
    #[serde(skip)]
    pub local_path: Option<PathBuf>,
    /// File name component of the path.
    pub name: String,
    /// File size in bytes.
    pub bytes: u64,
}

/// Human-friendly summary of the last known step input payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionFilesystemPolicySummary {
    /// Policy source from the last request when available.
    pub source: Option<String>,
    /// Workspace root from the last request when available.
    pub workspace_root: Option<String>,
    /// Effective workspace access policy carried in the last request.
    pub workspace_access: WorkspaceAccessPolicy,
    /// External read-grant paths outside the workspace root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_allow_paths: Vec<String>,
    /// External write-grant paths outside the workspace root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_allow_paths: Vec<String>,
}

/// Human-friendly summary of the last known step input payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionStepInputSummary {
    /// Whether a last request payload was recorded.
    pub request_present: bool,
    /// Invoke id from the last request when available.
    pub invoke_id: Option<String>,
    /// Adapter family from the last request when available.
    pub adapter_kind: Option<String>,
    /// Plugin identifier from the last request metadata when available.
    pub plugin_id: Option<String>,
    /// Action id from the last request when available.
    pub action: Option<String>,
    /// Top-level input object keys from the last request payload.
    pub input_object_keys: Vec<String>,
    /// Top-level metadata object keys from the last request payload.
    pub metadata_object_keys: Vec<String>,
    /// Effective filesystem policy from the last request when available.
    pub filesystem_policy: Option<InspectionFilesystemPolicySummary>,
}

/// Failed descendant summary durably surfaced by a subflow parent step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionChildFailure {
    /// Failed descendant run id surfaced by a subflow parent step.
    pub child_run_id: String,
    /// Failed descendant step id when one exists.
    pub failed_step_id: Option<String>,
    /// Canonical protocol error code when one was recorded.
    pub error_code: Option<String>,
    /// Compact human-readable failure message when one was recorded.
    pub error_message: Option<String>,
    /// Compact first-line stderr summary when one was recorded.
    pub stderr: Option<String>,
    /// Number of blocking child preflight checks when the child failed before dispatch.
    pub preflight_blocker_count: Option<u32>,
    /// Number of warning child preflight checks when the child failed before dispatch.
    pub preflight_warning_count: Option<u32>,
    /// First failed child preflight message when the child failed before dispatch.
    pub preflight_first_blocker: Option<String>,
}

/// Human-friendly summary of the last known step output payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionStepOutputSummary {
    /// Whether a last result payload was recorded.
    pub result_present: bool,
    /// Invoke id from the last result when available.
    pub invoke_id: Option<String>,
    /// Terminal result status when available.
    pub status: Option<String>,
    /// Exit code when one was recorded.
    pub exit_code: Option<i32>,
    /// Canonical protocol error code when one was recorded.
    pub error_code: Option<String>,
    /// Human-readable failure message when one was recorded.
    pub error_message: Option<String>,
    /// Compact tail of the last result's stderr, when stderr is non-empty.
    ///
    /// Captured as the last non-empty trimmed line of stderr and truncated to
    /// roughly 300 characters (with a trailing ellipsis when truncated). This
    /// is populated whenever the underlying result has non-empty stderr —
    /// independent of `error_message` and of step status — so callers can
    /// surface the actual diagnostic when the protocol-level `error.message`
    /// is generic (e.g. "command exited with code N").
    pub stderr_tail: Option<String>,
    /// Child run identifier for subflow steps when one was recorded.
    pub child_run_id: Option<String>,
    /// Failed descendant summary durably surfaced by a subflow parent step.
    pub child_failure: Option<InspectionChildFailure>,
    /// Top-level output object keys from the last result payload.
    pub output_object_keys: Vec<String>,
    /// Persisted artifact count from the last result.
    pub artifact_count: usize,
    /// Persisted evidence count from the last result.
    pub evidence_count: usize,
    /// Artifact blob references associated with the step.
    pub artifact_refs: Vec<InspectionStepBlobRef>,
    /// Evidence blob references associated with the step.
    pub evidence_refs: Vec<InspectionStepBlobRef>,
    /// Declared workflow files associated with the step.
    pub workflow_files: Vec<InspectionWorkflowFileRef>,
}

/// Execution-attributed persisted output for local inspection, not snapshot export.
///
/// Values are generic plugin data and have not been sanitized for HTTP transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionStepOutputSnapshot {
    /// Selected positive execution ordinal, or no ordinal when none exists.
    pub execution: Option<u32>,
    /// Recorded execution status, or persisted step status for a state fallback.
    pub status: String,
    /// Authoritative source: `execution_record` or `state_snapshot`.
    pub source: String,
    /// Whether a result exists, including a result whose output is JSON null.
    pub result_present: bool,
    /// Complete output value; omitted only when no result exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_output_value"
    )]
    pub value: Option<serde_json::Value>,
}

fn deserialize_output_value<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// Inspection view of one workflow step within a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionStep {
    /// Stable step id.
    pub step_id: String,
    /// Step kind such as `invoke`, `control`, or `subflow`.
    pub step_kind: String,
    /// Current or terminal step status as a lowercase string.
    pub status: String,
    /// Latest execution number when one exists.
    pub execution: Option<u32>,
    /// First started-at timestamp observed for the step.
    pub started_at_ms: Option<u64>,
    /// Final finished-at timestamp observed for the step.
    pub finished_at_ms: Option<u64>,
    /// Summary of the latest request payload.
    pub input_summary: InspectionStepInputSummary,
    /// Summary of the latest result payload.
    pub output_summary: InspectionStepOutputSummary,
    /// Latest execution-scoped output. Absent only when no attempt/result exists.
    /// Excluded from inspection snapshot exports, even with extended-detail flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_output: Option<InspectionStepOutputSnapshot>,
    /// Whether this step is the currently active step for the run.
    pub is_current_active: bool,
    /// Complete ordered execution history for the step, ascending by execution
    /// number. Each entry carries the execution record's status, timestamps,
    /// request, result, and state mutations. Empty when no execution records
    /// have been persisted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executions: Vec<InspectionStepExecution>,
}

/// Inspection view of one persisted step execution record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionStepExecution {
    /// Execution number, ascending from 1 for the lifetime of the run.
    pub execution: u32,
    /// Terminal or transitional status string recorded for the execution.
    pub status: String,
    /// Optional start timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Optional finish timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Optional normalized execution request for the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<transist_protocol::ExecutionRequest>,
    /// Optional normalized execution result for the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<transist_protocol::ExecutionResult>,
    /// Observable state mutations completed before the execution finished.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_mutations: Vec<transist_core::StateMutation>,
}

/// Persisted decision details for a manual gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionGateDecision {
    /// Exact immutable document review request, absent for ordinary gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_request: Option<transist_protocol::GateReviewRequest>,
    /// Decision state such as `approve`, `reject`, or `request_changes`.
    pub state: String,
    /// Optional operator identity recorded with the decision.
    pub actor: Option<String>,
    /// Optional operator-supplied rationale.
    pub reason: Option<String>,
    /// Optional decision timestamp in Unix milliseconds.
    pub decided_at_ms: Option<u64>,
}

/// Inspection view of a manual gate step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionGate {
    /// Stable gate id.
    pub gate_id: String,
    /// Gate status string.
    pub status: String,
    /// Step id that owns the gate.
    pub step_id: String,
    /// Gate creation timestamp in Unix milliseconds when known.
    pub created_at_ms: Option<u64>,
    /// Recorded decision when one exists.
    pub decision: Option<InspectionGateDecision>,
    /// Full chronological history of decisions recorded for this gate (most
    /// recent last). Includes the current `decision` plus any earlier
    /// outcomes (e.g. `request_changes` decisions that re-entered the gate
    /// before the present `blocked` cycle). Empty when no decision has ever
    /// been recorded. Reviewers use this to see why a gate was re-opened.
    pub decision_history: Vec<InspectionGateDecision>,
    /// Selected request fields summarized for inspection.
    pub request_summary: BTreeMap<String, KernelValue>,
}

/// Descriptor metadata recorded alongside a persisted artifact or evidence blob.
#[derive(Debug, Clone)]
pub struct InspectionBlobDescriptor {
    /// Source path recorded by the producer-side descriptor file.
    pub source_path: String,
    /// Optional SHA-256 checksum copied from the descriptor.
    pub sha256: Option<String>,
    /// Optional byte count copied from the descriptor.
    pub bytes: Option<u64>,
    /// Invocation id associated with the descriptor, when recorded.
    pub invoke_id: Option<String>,
    /// Producer-reported status attached to the descriptor, when recorded.
    pub status: Option<String>,
}

/// Metadata for a persisted artifact or evidence blob.
#[derive(Debug, Clone)]
pub struct InspectionBlob {
    /// Run that owns the blob.
    pub run_id: String,
    /// Blob family, such as artifacts or evidence.
    pub family: BlobFamily,
    /// Step id that produced the blob.
    pub step_id: String,
    /// Execution number that produced the blob.
    pub execution: u32,
    /// Relative path within the blob family.
    pub relative_path: String,
    /// File name component of the blob path.
    pub name: String,
    /// Blob size in bytes.
    pub bytes: u64,
    /// Blob creation timestamp in Unix milliseconds when known.
    pub created_at_ms: Option<u64>,
    /// Optional indexed descriptor for structured blob metadata.
    pub descriptor: Option<InspectionBlobDescriptor>,
}

/// Blob bytes loaded from disk.
#[derive(Debug, Clone)]
pub struct InspectionBlobContent {
    /// Relative path within the blob family.
    pub relative_path: String,
    /// File name component of the blob path.
    pub name: String,
    /// Raw file bytes.
    pub bytes: Vec<u8>,
}

/// Workflow file bytes loaded from disk.
#[derive(Debug, Clone)]
pub struct InspectionWorkflowFileContent {
    /// Trusted-root-relative workflow file locator.
    pub path: String,
    /// File name component of the workflow file path.
    pub name: String,
    /// Raw file bytes.
    pub bytes: Vec<u8>,
}

/// Raw workflow source or compiled recipe bytes loaded from disk.
#[derive(Debug, Clone)]
pub struct InspectionWorkflowContent {
    /// Source or compiled file path on disk.
    pub path: String,
    /// Raw file bytes.
    pub bytes: Vec<u8>,
}

/// Summary of one declared workflow input.
#[derive(Debug, Clone)]
pub struct WorkflowInputSummary {
    /// Input name.
    pub name: String,
    /// Declared kernel value type.
    pub value_type: String,
    /// Whether the caller must supply the input.
    pub required: bool,
    /// Whether the input declares a default value.
    pub has_default: bool,
    /// Optional human-oriented input description.
    pub description: Option<String>,
}

/// Reference to prompt material used by a workflow step.
#[derive(Debug, Clone)]
pub struct WorkflowPromptReference {
    /// Step id that owns the prompt.
    pub step_id: String,
    /// Prompt source kind such as `inline` or `file`.
    pub source: String,
    /// Optional prompt file path.
    pub path: Option<String>,
    /// Optional truncated prompt preview.
    pub preview: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_detail_summary_defaults_to_metadata_only() {
        let detail = sample_detail();
        let policy = SyncPolicyConfig::default();

        let summary = InspectionRunDetailSummary::from_detail(&detail, &policy);

        assert!(summary.workflow_input_values.is_none());
        assert!(summary.run_detail_full.is_none());
        assert_eq!(summary.blocked_gate_ids, vec!["review".to_string()]);
    }

    #[test]
    fn run_detail_summary_includes_extended_payloads_when_requested() {
        let detail = sample_detail();
        let policy = SyncPolicyConfig {
            mode: SyncPolicyMode::Extended,
            include_workflow_input_values: true,
            include_event_payloads: true,
            include_run_detail_full: true,
            include_stderr_tail: true,
            include_gate_request_summary: true,
        };

        let summary = InspectionRunDetailSummary::from_detail(&detail, &policy);

        assert_eq!(
            summary.workflow_input_values,
            Some(BTreeMap::from([(
                "ticket".to_string(),
                KernelValue::String("TICKET-123".to_string()),
            )]))
        );

        let full = summary.run_detail_full.expect("full detail");
        assert_eq!(
            full.workflow_inputs.get("ticket"),
            Some(&KernelValue::String("TICKET-123".to_string()))
        );
        assert_eq!(
            full.steps[0].output_summary.stderr_tail.as_deref(),
            Some("boom")
        );
        assert_eq!(
            full.blocked_gates[0]
                .request_summary
                .get("operator_guidance"),
            Some(&KernelValue::String("secret".to_string()))
        );
        assert!(!full.trust.trusted_execution_mode);
        assert_eq!(full.locality.external_transfers.len(), 0);
        assert_eq!(full.evidence_export.format, "markdown");
    }

    #[test]
    fn step_outputs_projection_snapshot_export_excludes_latest_output_for_all_detail_flags() {
        let mut detail = sample_detail();
        let prior = serde_json::to_value(&detail).unwrap();
        detail.steps[0].latest_output = Some(InspectionStepOutputSnapshot {
            execution: Some(1),
            status: "failed".to_string(),
            source: "execution_record".to_string(),
            result_present: true,
            value: Some(serde_json::json!({"response_text": "Fictional diagnostic advice"})),
        });
        let mut second = detail.steps[0].clone();
        second.step_id = "step-2".to_string();
        second.latest_output.as_mut().unwrap().result_present = false;
        second.latest_output.as_mut().unwrap().value = None;
        detail.steps.push(second);
        let original = serde_json::to_value(&detail).unwrap();
        let mut expected = original.clone();
        for step in expected["steps"].as_array_mut().unwrap() {
            step.as_object_mut().unwrap().remove("latest_output");
        }
        assert_eq!(expected["steps"][0], prior["steps"][0]);
        for flags in 0..8 {
            let policy = SyncPolicyConfig {
                mode: SyncPolicyMode::Extended,
                include_run_detail_full: flags & 1 != 0,
                include_stderr_tail: flags & 2 != 0,
                include_gate_request_summary: flags & 4 != 0,
                ..SyncPolicyConfig::default()
            };
            let summary = InspectionRunDetailSummary::from_detail(&detail, &policy);
            let json = serde_json::to_value(summary).unwrap();
            if flags == 0 {
                assert!(json.get("run_detail_full").is_none());
            } else {
                assert_eq!(json["run_detail_full"], expected);
                for step in json["run_detail_full"]["steps"].as_array().unwrap() {
                    assert!(step.get("latest_output").is_none());
                }
            }
            assert_eq!(serde_json::to_value(&detail).unwrap(), original);
        }
    }

    fn sample_detail() -> InspectionRunDetail {
        InspectionRunDetail {
            summary: InspectionRunSummary {
                run_id: "run-1".to_string(),
                workflow_id: "wf.demo".to_string(),
                workflow_version: "v1".to_string(),
                status: "blocked".to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                started_at_ms: Some(1),
                finished_at_ms: None,
                current_step_id: Some("step-1".to_string()),
                has_blocked_gates: true,
                has_artifacts: false,
                has_events: true,
                artifact_count: 0,
                evidence_count: 0,
                root_run_id: "run-1".to_string(),
                parent_run_id: None,
            },
            workflow_inputs: BTreeMap::from([(
                "ticket".to_string(),
                KernelValue::String("TICKET-123".to_string()),
            )]),
            global_default_filesystem_policy: None,
            workflow_filesystem_policy: None,
            current_step: Some(RunCurrentStep {
                step_id: "step-1".to_string(),
                status: "running".to_string(),
            }),
            actionable_run_id: None,
            terminal_outcome: None,
            recovery_state: None,
            preflight: None,
            cleanup: None,
            active_lease: None,
            blocked_gates: vec![InspectionGate {
                gate_id: "review".to_string(),
                status: "blocked".to_string(),
                step_id: "step-1".to_string(),
                created_at_ms: Some(3),
                decision: None,
                decision_history: Vec::new(),
                request_summary: BTreeMap::from([(
                    "operator_guidance".to_string(),
                    KernelValue::String("secret".to_string()),
                )]),
            }],
            steps: vec![InspectionStep {
                step_id: "step-1".to_string(),
                step_kind: "invoke".to_string(),
                status: "failed".to_string(),
                execution: Some(1),
                started_at_ms: Some(1),
                finished_at_ms: Some(2),
                input_summary: InspectionStepInputSummary {
                    request_present: true,
                    invoke_id: Some("invoke-1".to_string()),
                    adapter_kind: Some("process".to_string()),
                    plugin_id: Some("plugin".to_string()),
                    action: Some("run".to_string()),
                    input_object_keys: vec!["ticket".to_string()],
                    metadata_object_keys: vec![],
                    filesystem_policy: Some(InspectionFilesystemPolicySummary {
                        source: Some("step".to_string()),
                        workspace_root: Some("/tmp/workspace".to_string()),
                        workspace_access: WorkspaceAccessPolicy {
                            mode: transist_protocol::WorkspaceAccessMode::ReadOnly,
                        },
                        read_allow_paths: Vec::new(),
                        write_allow_paths: Vec::new(),
                    }),
                },
                output_summary: InspectionStepOutputSummary {
                    result_present: true,
                    invoke_id: Some("invoke-1".to_string()),
                    status: Some("failed".to_string()),
                    exit_code: Some(1),
                    error_code: None,
                    error_message: Some("boom".to_string()),
                    stderr_tail: Some("boom".to_string()),
                    child_run_id: None,
                    child_failure: None,
                    output_object_keys: vec![],
                    artifact_count: 0,
                    evidence_count: 0,
                    artifact_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    workflow_files: Vec::new(),
                },
                is_current_active: true,
                latest_output: None,
                executions: Vec::new(),
            }],
            artifact_count: 0,
            evidence_count: 0,
            event_count: 1,
            latest_event_id: Some("1".to_string()),
            lineage: None,
            contained_runs: Vec::new(),
            step_counts: BTreeMap::from([("failed".to_string(), 1)]),
            trusted_execution_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            trust: InspectionTrustSummary {
                trusted_execution_mode: false,
                workflow: None,
                plugins: Vec::new(),
                decision: None,
                policy_decision: None,
            },
            locality: InspectionLocalityView {
                steps: Vec::new(),
                external_transfers: Vec::new(),
            },
            evidence_export: InspectionEvidenceExportView {
                location: None,
                command: "transist runs evidence export run-1".to_string(),
                format: "markdown".to_string(),
                generated_at_ms: None,
            },
        }
    }
}

/// Validation summary for a discovered workflow source.
#[derive(Debug, Clone)]
pub struct WorkflowValidationSummary {
    /// Validation status string.
    pub status: String,
    /// Validation errors recorded for the workflow.
    pub errors: Vec<String>,
}

/// Summary of one step in a discovered source flow or compiled recipe.
#[derive(Debug, Clone)]
pub struct WorkflowStepSummary {
    /// Stable step id.
    pub step_id: String,
    /// Immediate parent step id when this step lives inside an `if` branch or loop body.
    pub parent_step_id: Option<String>,
    /// Immediate child step ids nested directly under this step.
    pub child_step_ids: Vec<String>,
    /// Step kind such as `invoke`, `control`, or `subflow`.
    pub kind: String,
    /// Adapter family for invoke steps.
    pub adapter: Option<String>,
    /// Plugin id for invoke steps.
    pub plugin: Option<String>,
    /// Action id for invoke steps.
    pub action: Option<String>,
    /// Execution kind for invoke steps.
    pub execution_kind: Option<String>,
    /// Control primitive for control steps.
    pub control: Option<String>,
    /// Referenced subflow path for subflow steps.
    pub subflow_path: Option<String>,
    /// Top-level authored `with` keys.
    pub with_keys: Vec<String>,
    /// Top-level subflow input keys.
    pub subflow_input_keys: Vec<String>,
    /// Workflow input names referenced by the step.
    pub workflow_input_refs: Vec<String>,
    /// Authored `if` guard expression when present.
    pub if_guard: Option<String>,
    /// Step refs from the `then` branch.
    pub if_then_refs: Vec<String>,
    /// Step refs from the `else` branch.
    pub if_else_refs: Vec<String>,
    /// Authored `while` expression for loop blocks.
    pub loop_while: Option<String>,
    /// Authored `until` expression for loop blocks.
    pub loop_until: Option<String>,
    /// Step refs contained in the loop body.
    pub loop_body_refs: Vec<String>,
    /// Authored loop max-iteration value.
    pub loop_max_iters: Option<u32>,
    /// Authored loop max-duration value.
    pub loop_max_duration: Option<String>,
    /// Prompt source kind when prompt data is present.
    pub prompt_source: Option<String>,
    /// Prompt detail such as preview text or file path.
    pub prompt_detail: Option<String>,
    /// Skill references authored on the step.
    pub skills: Vec<String>,
    /// Outgoing graph edges associated with this step.
    pub graph_edges: Vec<WorkflowGraphEdgeSummary>,
}

/// Summary of one workflow-graph edge originating from a step.
#[derive(Debug, Clone)]
pub struct WorkflowGraphEdgeSummary {
    /// High-level edge kind such as `graph`, `if_then`, `if_else`, or `loop`.
    pub kind: String,
    /// Human-friendly label describing the transition.
    pub label: String,
    /// Target step id.
    pub target_step_id: String,
    /// Optional guard expression attached to the edge.
    pub guard: Option<String>,
    /// Optional loop `while` expression attached to the edge.
    pub loop_while: Option<String>,
    /// Optional loop `until` expression attached to the edge.
    pub loop_until: Option<String>,
    /// Optional loop iteration cap attached to the edge.
    pub loop_max_iters: Option<u32>,
    /// Explicit entry/exit routing revision; version 1 includes cap exhaustion
    /// in normal exit. Omitted by serialized consumers for legacy edges.
    pub loop_routing_version: Option<u32>,
}

/// Compact workflow row used by workflow-list views.
#[derive(Debug, Clone)]
pub struct InspectionWorkflowSummary {
    /// Stable workflow id.
    pub workflow_id: String,
    /// Stable workflow version.
    pub workflow_version: String,
    /// Source file reference when one was discovered.
    pub source_ref: Option<String>,
    /// Optional workflow title derived from source content.
    pub title: Option<String>,
    /// Declared workflow inputs.
    pub inputs: Vec<WorkflowInputSummary>,
    /// Whether a compiled recipe file is present.
    pub compiled_recipe_available: bool,
    /// Whether validation and local prerequisites indicate the workflow can run.
    pub runnable_in_principle: bool,
    /// Number of authored steps.
    pub step_count: usize,
    /// Number of root-level authored steps shown in the inspection UI's workflow table.
    pub root_step_count: usize,
    /// Whether this workflow is referenced as a subflow by another discovered workflow.
    ///
    /// Subflows are designed to be embedded in parent workflows and should not appear
    /// in root-level launch lists.
    pub is_subflow: bool,
}

/// Full workflow detail assembled from discovered source and compiled artifacts.
#[derive(Debug, Clone)]
pub struct InspectionWorkflowDetail {
    /// Compact workflow summary for the same workflow.
    pub summary: InspectionWorkflowSummary,
    /// Step-by-step workflow summary records.
    pub steps: Vec<WorkflowStepSummary>,
    /// Prompt references discovered across steps.
    pub prompt_references: Vec<WorkflowPromptReference>,
    /// Validation status and errors for the workflow source.
    pub validation: WorkflowValidationSummary,
}
