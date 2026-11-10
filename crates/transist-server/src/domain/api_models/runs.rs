// SPDX-License-Identifier: MPL-2.0
//! JSON DTOs returned by the companion inspection and metrics APIs.

use serde::Serialize;

#[derive(Debug, Serialize)]
/// Pagination metadata returned alongside list responses.
pub struct Pagination {
    /// Total number of records available for the query.
    pub total: usize,
    /// Maximum number of records requested for this page.
    pub limit: usize,
    /// Zero-based offset of the first returned record.
    pub offset: usize,
    /// Whether additional records remain after this page.
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
/// Generic paginated response envelope.
pub struct ListResponse<T> {
    /// Records returned for the current page.
    pub data: Vec<T>,
    /// Pagination metadata for the current page.
    pub pagination: Pagination,
}

#[derive(Debug, Serialize)]
/// Error response envelope used by the companion API.
pub struct ErrorEnvelope {
    /// Error payload describing the failure.
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
/// Machine-readable and human-readable error payload.
pub struct ErrorBody {
    /// Stable error code for clients.
    pub code: &'static str,
    /// Human-readable error message.
    pub message: String,
}

#[derive(Debug, Serialize)]
/// Static health response for the companion server.
pub struct HealthResponse {
    /// Overall service status.
    pub status: &'static str,
    /// Service identifier.
    pub service: &'static str,
    /// Operating mode of the companion server.
    pub mode: &'static str,
    /// HTTP API version string.
    pub api_version: &'static str,
}

#[derive(Debug, Serialize)]
/// Hypermedia links for a resource and its related child resources.
pub struct ResourceLinks {
    /// Canonical URL for the current resource.
    pub self_url: String,
    /// URL for the resource's events collection, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<String>,
    /// URL for the resource's steps collection, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<String>,
    /// URL for the resource's gates collection, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gates: Option<String>,
    /// URL for the resource's artifacts collection, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<String>,
    /// URL for the resource's evidence collection, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Debug, Serialize)]
/// Summary view for a workflow run in list responses.
pub struct RunSummary {
    /// Stable run identifier.
    pub run_id: String,
    /// Authored workflow identifier.
    pub workflow_id: String,
    /// Authored workflow version.
    pub workflow_version: String,
    /// Current run status.
    pub status: String,
    /// Run creation time in RFC 3339 format.
    pub created_at: String,
    /// Last update time in RFC 3339 format.
    pub updated_at: String,
    /// Run start time in RFC 3339 format, when execution has started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Run completion time in RFC 3339 format, when the run has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Active step identifier, when the run is currently executing a step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step_id: Option<String>,
    /// Whether the run currently has blocked manual gates.
    pub has_blocked_gates: bool,
    /// Whether the run has recorded any artifacts.
    pub has_artifacts: bool,
    /// Whether the run has recorded any events.
    pub has_events: bool,
    /// Total persisted artifact count.
    pub artifact_count: usize,
    /// Total persisted evidence count.
    pub evidence_count: usize,
    /// Root run identifier for the lineage tree.
    pub root_run_id: String,
    /// Direct parent run identifier when this run is nested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Aggregated metrics included in list views when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<transist::RunListMetricsSummary>,
    /// Related resource links for the run.
    pub links: ResourceLinks,
}

#[derive(Debug, Serialize)]
/// Summary of a descendant run contained within a parent run hierarchy.
pub struct ContainedRunSummary {
    /// Stable child run identifier.
    pub run_id: String,
    /// Child workflow identifier.
    pub workflow_id: String,
    /// Child workflow version.
    pub workflow_version: String,
    /// Current child run status.
    pub status: String,
    /// Child run creation time in RFC 3339 format.
    pub created_at: String,
    /// Child run update time in RFC 3339 format.
    pub updated_at: String,
    /// Active step identifier for the child run, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step_id: Option<String>,
    /// Number of artifacts recorded by the child run.
    pub artifact_count: usize,
    /// Number of evidence blobs recorded by the child run.
    pub evidence_count: usize,
    /// Parent run identifier when this run is nested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
}

#[derive(Debug, Serialize)]
/// Minimal summary of the current active step for a run.
pub struct CurrentStepSummary {
    /// Stable step identifier.
    pub step_id: String,
    /// Current step status.
    pub status: String,
}

#[derive(Debug, Serialize)]
/// Preflight summary included in run detail responses.
pub struct PreflightSummary {
    /// Overall preflight status.
    pub status: String,
    /// RFC 3339 timestamp of the last preflight evaluation.
    pub checked_at: String,
    /// Number of blocking preflight findings.
    pub blocker_count: usize,
    /// Number of non-blocking preflight findings.
    pub warning_count: usize,
    /// Human-readable details for the blocking checks (failed entries only).
    ///
    /// Provided so API consumers and the HTML inspector can surface the actual
    /// failure reason without re-reading the full persisted preflight payload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_checks: Vec<PreflightFailedCheck>,
}

#[derive(Debug, Serialize)]
/// One failed preflight check surfaced in [`PreflightSummary::failed_checks`].
pub struct PreflightFailedCheck {
    /// Check family or kind name.
    pub kind: String,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Step ids associated with the finding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_ids: Vec<String>,
    /// Optional plugin id associated with the finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Optional action id associated with the finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Serialize)]
/// Effective filesystem policy summary with host-specific paths redacted.
pub struct FilesystemPolicySummary {
    /// Policy source from the last request when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Relative marker for the workspace root. The host path is never returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    /// Effective workspace access policy.
    pub workspace_access: transist::WorkspaceAccessPolicy,
    /// Sanitized read-grant path labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_allow_paths: Vec<String>,
    /// Sanitized write-grant path labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_allow_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
/// Summary of one cleanup step.
pub struct CleanupStepSummary {
    /// Stable cleanup step identifier.
    pub step_id: String,
    /// Cleanup step status.
    pub status: String,
    /// Latest execution number when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<u32>,
    /// Cleanup step start time in RFC 3339 format when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Cleanup step finish time in RFC 3339 format when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Human-readable failure message when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize)]
/// Summary of the persisted cleanup phase for a run.
pub struct CleanupSummary {
    /// Cleanup trigger that selected this phase.
    pub trigger: String,
    /// Cleanup lifecycle state.
    pub state: String,
    /// Cleanup start time in RFC 3339 format when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Cleanup finish time in RFC 3339 format when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Cleanup preflight summary when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight: Option<PreflightSummary>,
    /// Ordered cleanup step summaries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<CleanupStepSummary>,
}

#[derive(Debug, Serialize)]
/// Event summary counts for a run.
pub struct RunEventSummary {
    /// Total number of events recorded for the run.
    pub count: usize,
    /// Most recent event identifier, when events exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_event_id: Option<String>,
}

#[derive(Debug, Serialize)]
/// Blob count summary for artifacts or evidence.
pub struct RunBlobSummary {
    /// Total number of blobs in the family.
    pub count: usize,
}

#[derive(Debug, Serialize)]
/// Detailed view of a workflow run.
pub struct RunDetail {
    /// Stable run identifier.
    pub run_id: String,
    /// Authored workflow identifier.
    pub workflow_id: String,
    /// Authored workflow version.
    pub workflow_version: String,
    /// Current run status.
    pub status: String,
    /// Run creation time in RFC 3339 format.
    pub created_at: String,
    /// Last update time in RFC 3339 format.
    pub updated_at: String,
    /// Run start time in RFC 3339 format, when execution has started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Run completion time in RFC 3339 format, when the run has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Serialized summary of workflow inputs captured for the run.
    pub workflow_input_summary: serde_json::Value,
    /// Effective global filesystem policy, when one was persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_default_filesystem_policy: Option<FilesystemPolicySummary>,
    /// Workflow filesystem policy, when one was persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_filesystem_policy: Option<FilesystemPolicySummary>,
    /// Current active step, when the run is still executing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<CurrentStepSummary>,
    /// Descendant run that currently represents the live or failed focus, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_run_id: Option<String>,
    /// Terminal outcome label, when the run has completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_outcome: Option<String>,
    /// Failed-run recoverability state when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_state: Option<String>,
    /// Most recent preflight result, when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight: Option<PreflightSummary>,
    /// Persisted cleanup summary when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<CleanupSummary>,
    /// Live root-run-tree lease metadata, when one is currently present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_lease: Option<serde_json::Value>,
    /// Currently blocked gates for the run.
    pub blocked_gates: Vec<GateSummary>,
    /// Artifact summary counts for the run.
    pub artifact_summary: RunBlobSummary,
    /// Evidence summary counts for the run.
    pub evidence_summary: RunBlobSummary,
    /// Event summary counts for the run.
    pub event_summary: RunEventSummary,
    /// Root run identifier for the lineage tree.
    pub root_run_id: String,
    /// Direct parent run identifier when this run is nested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Descendant runs contained within this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contained_runs: Vec<ContainedRunSummary>,
    /// Serialized lineage description, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<serde_json::Value>,
    /// Serialized per-status step counts for the run.
    pub step_counts: serde_json::Value,
    /// Detailed run metrics, when metrics are available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<transist::RunMetricsSummary>,
    /// Whether trusted-execution mode was enabled for this run.
    pub trusted_execution_mode: bool,
    /// Effective runtime operating mode recorded for the run.
    pub runtime_mode: String,
    /// Effective policy profile recorded for the run.
    pub policy_profile: String,
    /// Capability enforcement mode recorded for the run.
    pub capability_enforcement: String,
    /// Trust and provenance summary derived from persisted run metadata.
    pub trust: TrustSummaryView,
    /// Provider locality and external-transfer evidence for this run.
    pub locality: LocalityView,
    /// How an operator can obtain the evidence export report.
    pub evidence_export: EvidenceExportView,
    /// Related resource links for the run.
    pub links: ResourceLinks,
}

#[derive(Debug, Serialize)]
/// Persisted run-start policy decision surfaced through inspection.
pub struct PolicyDecisionView {
    /// Whether the policy allowed execution.
    pub allowed: bool,
    /// Policy checkpoint or execution mode that produced the decision.
    pub mode: String,
    /// Stable policy reason code when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Human-readable policy message when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Effective runtime operating mode recorded for the run.
    pub runtime_mode: String,
    /// Effective policy profile recorded for the run.
    pub policy_profile: String,
    /// Capability enforcement mode recorded for the run.
    pub capability_enforcement: String,
}

#[derive(Debug, Serialize)]
/// Trust metadata for one workflow or plugin subject.
pub struct TrustSubjectView {
    /// Stable workflow or plugin id.
    pub id: String,
    /// Stable workflow or plugin version.
    pub version: String,
    /// Recomputed digest captured for the subject.
    pub digest: String,
    /// Signature/trust status for the subject.
    pub signature_status: String,
    /// Signer key id when one was present in the detached signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_key_id: Option<String>,
}

#[derive(Debug, Serialize)]
/// Run-level trust decision surfaced through inspection.
pub struct TrustDecisionView {
    /// Run-level trust outcome.
    pub outcome: String,
    /// Stable reason code when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Verification surface that failed when the run was denied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_verification_type: Option<String>,
}

#[derive(Debug, Serialize)]
/// Trust and provenance summary for one run.
pub struct TrustSummaryView {
    /// Whether trusted-execution mode was enabled.
    pub trusted_execution_mode: bool,
    /// Workflow trust metadata when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<TrustSubjectView>,
    /// Plugin trust metadata for referenced plugins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<TrustSubjectView>,
    /// Run-level trust outcome when trusted execution participated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<TrustDecisionView>,
    /// Persisted run-start policy decision when provenance exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_decision: Option<PolicyDecisionView>,
}

#[derive(Debug, Serialize)]
/// Provider locality row derived from persisted provenance.
pub struct LocalityStepView {
    /// Step id that reported provider/model usage.
    pub step_id: String,
    /// Provider identifier when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model identifier when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Declared provider locality marker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
}

#[derive(Debug, Serialize)]
/// One recorded external-transfer evidence item.
pub struct ExternalTransferView {
    /// Step id that triggered the external call.
    pub step_id: String,
    /// Step execution number.
    pub execution: u32,
    /// RFC 3339 timestamp for the transfer evidence.
    pub timestamp: String,
    /// Best-effort destination identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Adapter kind used for the call.
    pub adapter_kind: String,
    /// Provider identifier when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Declared provider locality marker.
    pub provider_kind: String,
    /// Bytes sent when measurable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// SHA-256 hash of the sent payload when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Execution mode that governed the transfer evidence.
    pub policy_mode: String,
}

#[derive(Debug, Serialize)]
/// Provider locality and external-transfer evidence for one run.
pub struct LocalityView {
    /// Provider/model rows keyed by step execution provenance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<LocalityStepView>,
    /// External-transfer evidence records. Local-only runs keep this empty.
    pub external_transfers: Vec<ExternalTransferView>,
}

#[derive(Debug, Serialize)]
/// Location metadata for the exported evidence report.
pub struct EvidenceExportView {
    /// Where an operator can retrieve or generate the report, when provenance exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Stable command hint for generating the export.
    pub command: String,
    /// Export format currently supported by the runtime.
    pub format: String,
    /// RFC 3339 timestamp associated with the persisted provenance used for the report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
}

#[derive(Debug, Serialize)]
/// Summary view of a persisted run event.
pub struct EventSummary {
    /// Stable event identifier.
    pub event_id: String,
    /// Run that emitted the event.
    pub run_id: String,
    /// Event timestamp in RFC 3339 format.
    pub timestamp: String,
    /// Event type label.
    pub event_type: String,
    /// Step associated with the event, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Serialized summary payload for the event.
    pub summary: serde_json::Value,
}

#[derive(Debug, Serialize)]
/// Reference to an artifact produced by a step.
pub struct ArtifactOutputRef {
    /// Encoded artifact identifier used by the HTTP API.
    pub artifact_id: String,
    /// Artifact file name.
    pub name: String,
    /// Media type inferred for the artifact, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Artifact size in bytes.
    pub size: u64,
}

#[derive(Debug, Serialize)]
/// Reference to an evidence blob produced by a step.
pub struct EvidenceOutputRef {
    /// Encoded evidence identifier used by the HTTP API.
    pub evidence_id: String,
    /// Evidence file name.
    pub name: String,
    /// Media type inferred for the evidence blob, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Evidence size in bytes.
    pub size: u64,
}

#[derive(Debug, Serialize)]
/// Reference to a workflow-owned file associated with a step.
pub struct WorkflowFileOutputRef {
    /// Encoded workflow file identifier when an intrinsic safe locator exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_file_id: Option<String>,
    /// Source field name that referenced the file.
    pub field_name: String,
    /// Locator root used for the file: `workspace` or `run`.
    pub locator_kind: String,
    /// Workflow file name.
    pub name: String,
    /// Trusted-root-relative locator label for the file.
    pub relative_path: String,
    /// Media type inferred for the workflow file, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Workflow file size in bytes.
    pub size: u64,
}

#[derive(Debug, Serialize)]
/// Summary of the stored input material for a step.
pub struct StepInputSummary {
    /// Whether any request/input payload was captured for the step.
    pub request_present: bool,
    /// Invoke identifier associated with the request, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoke_id: Option<String>,
    /// Adapter kind recorded on the request, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_kind: Option<String>,
    /// Plugin id recorded on the request metadata, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Action name recorded on the request, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Top-level keys present in the captured input object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_object_keys: Vec<String>,
    /// Top-level keys present in the captured metadata object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_object_keys: Vec<String>,
    /// Effective filesystem policy from the last request, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicySummary>,
}

#[derive(Debug, Serialize)]
/// Failed descendant summary durably surfaced by a subflow parent step.
///
/// Populated from the kernel's persisted `output.failure` object so read
/// surfaces (CLI, TUI, HTTP API) can present the same compact failure reason
/// as the failed descendant's own step without recomputing it. Omitted when
/// the step is not a failed subflow parent or when the run predates durable
/// child-failure attribution.
pub struct ChildFailureSummary {
    /// Failed descendant run id surfaced by a subflow parent step.
    pub child_run_id: String,
    /// Failed descendant step id when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step_id: Option<String>,
    /// Canonical protocol error code when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Compact human-readable failure message when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Compact first-line stderr summary when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// Number of blocking child preflight checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight_blocker_count: Option<u32>,
    /// Number of warning child preflight checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight_warning_count: Option<u32>,
    /// First failed child preflight message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight_first_blocker: Option<String>,
}

#[derive(Debug, Serialize)]
/// Summary of the stored output material for a step.
pub struct StepOutputSummary {
    /// Whether any result/output payload was captured for the step.
    pub result_present: bool,
    /// Invoke identifier associated with the result, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoke_id: Option<String>,
    /// Invoke status recorded on the result, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Process exit code recorded on the result, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Canonical protocol error code recorded on the result, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Human-readable failure message for the result, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Compact tail of the result's stderr when stderr is non-empty.
    ///
    /// Captured as the last non-empty trimmed line of stderr, truncated to
    /// roughly 300 characters. Populated independently of `error_message` so
    /// callers can surface the real diagnostic when `error_message` is a
    /// generic wrapper (e.g. "command exited with code N").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    /// Child run identifier for subflow results, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
    /// Failed descendant summary durably surfaced by a subflow parent step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_failure: Option<ChildFailureSummary>,
    /// Top-level keys present in the captured output object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_object_keys: Vec<String>,
    /// Number of artifacts produced by the step.
    pub artifact_count: usize,
    /// Number of evidence blobs produced by the step.
    pub evidence_count: usize,
    /// Artifact references included in the output summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<ArtifactOutputRef>,
    /// Evidence references included in the output summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceOutputRef>,
    /// Workflow file references included in the output summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_files: Vec<WorkflowFileOutputRef>,
}

#[derive(Debug, Serialize)]
/// Summary of a run step as returned by the inspection API.
pub struct StepSummary {
    /// Run that owns the step.
    pub run_id: String,
    /// Stable step identifier.
    pub step_id: String,
    /// Authored step kind.
    pub step_kind: String,
    /// Current step status.
    pub status: String,
    /// Execution number for retried steps, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<u32>,
    /// Step start time in RFC 3339 format, when execution started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Step finish time in RFC 3339 format, when execution finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Summary of captured step input data.
    pub input_summary: StepInputSummary,
    /// Summary of captured step output data.
    pub output_summary: StepOutputSummary,
    /// Whether this step is the run's current active step.
    pub is_current_active: bool,
    /// Complete ordered execution history, ascending by execution number.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub executions: Vec<StepExecutionSummary>,
}

#[derive(Debug, Serialize)]
/// Summary of one persisted step execution record.
pub struct StepExecutionSummary {
    /// Execution number, ascending from 1 for the lifetime of the run.
    pub execution: u32,
    /// Terminal or transitional status string recorded for the execution.
    pub status: String,
    /// Step start time in RFC 3339 format, when execution started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Step finish time in RFC 3339 format, when execution finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// Execution-attributed persisted output, before HTTP transport sanitization.
#[derive(Debug, Clone, Serialize)]
pub struct StepOutput {
    /// Selected positive ordinal, or null when the step has never executed.
    pub execution: Option<u32>,
    /// Recorded execution status, independently of the latest step summary.
    pub status: String,
    /// Authoritative source: `execution_record` or `state_snapshot`.
    pub source: String,
    /// Whether a result exists, including a JSON null result.
    pub result_present: bool,
    /// Complete output value; omitted only when no result exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
/// Detailed step view with selected output and optional metrics.
pub struct StepDetail {
    /// Run that owns the step.
    pub run_id: String,
    /// Stable step identifier.
    pub step_id: String,
    /// Authored step kind.
    pub step_kind: String,
    /// Current step status.
    pub status: String,
    /// Execution number for retried steps, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<u32>,
    /// Step start time in RFC 3339 format, when execution started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Step finish time in RFC 3339 format, when execution finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Summary of captured step input data.
    pub input_summary: StepInputSummary,
    /// Summary of captured step output data.
    pub output_summary: StepOutputSummary,
    /// Whether this step is the run's current active step.
    pub is_current_active: bool,
    /// Complete ordered execution history, ascending by execution number.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub executions: Vec<StepExecutionSummary>,
    /// Selected persisted output; existing summary fields still describe latest state.
    pub output: StepOutput,
    /// Selectable positive ordinals, ascending and deduplicated.
    pub output_executions: Vec<u32>,
    /// Detailed metrics for the step, omitted when unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<transist::StepMetricsSummary>,
}

#[derive(Debug, Clone, Serialize)]
/// Manual gate decision summary.
pub struct GateDecisionSummary {
    /// Exact immutable request this decision covered, including revision IDs and digests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_request: Option<transist::GateReviewRequest>,
    /// Decision state such as approved or rejected.
    pub state: String,
    /// Optional identity of the approving or rejecting actor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Optional human-supplied reason for the decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// RFC 3339 decision timestamp, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
}

#[derive(Debug, Serialize)]
/// Summary of a manual gate attached to a run.
pub struct GateSummary {
    /// Exact reviewed revisions with opaque content/metadata links (never current heads).
    pub review_documents: Vec<transist::InspectionDocument>,
    /// Run that owns the gate.
    pub run_id: String,
    /// Stable gate identifier.
    pub gate_id: String,
    /// Current gate status.
    pub status: String,
    /// Step that emitted the gate.
    pub step_id: String,
    /// RFC 3339 creation timestamp, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Decision details, when the gate has been decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<GateDecisionSummary>,
    /// Complete chronological decision history for this gate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_history: Vec<GateDecisionSummary>,
    /// Serialized request summary shown to human reviewers.
    pub request_summary: serde_json::Value,
}

#[derive(Debug, Serialize)]
/// Summary of blob descriptor metadata recorded by the runtime.
pub struct BlobDescriptorSummary {
    /// Basename of the source path used to materialize the blob.
    pub source_path_basename: String,
    /// SHA-256 digest of the blob, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Blob size in bytes, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Invoke identifier associated with blob production, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoke_id: Option<String>,
    /// Invoke status associated with blob production, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
/// Metadata returned for an artifact resource.
pub struct ArtifactMetadata {
    /// Run that owns the artifact.
    pub run_id: String,
    /// Encoded artifact identifier used by the HTTP API.
    pub artifact_id: String,
    /// Artifact file name.
    pub name: String,
    /// Media type inferred for the artifact, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Artifact size in bytes.
    pub size: u64,
    /// Step that produced the artifact.
    pub producing_step_id: String,
    /// Execution number that produced the artifact.
    pub execution: u32,
    /// RFC 3339 creation timestamp, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Additional descriptor metadata, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<BlobDescriptorSummary>,
    /// Related resource links for the artifact.
    pub links: ArtifactLinks,
}

#[derive(Debug, Serialize)]
/// Hypermedia links for an artifact resource.
pub struct ArtifactLinks {
    /// Canonical metadata URL for the artifact.
    pub self_url: String,
    /// Direct content URL for downloading the artifact.
    pub content: String,
}

#[derive(Debug, Serialize)]
/// Metadata returned for an evidence resource.
pub struct EvidenceMetadata {
    /// Run that owns the evidence blob.
    pub run_id: String,
    /// Encoded evidence identifier used by the HTTP API.
    pub evidence_id: String,
    /// Evidence file name.
    pub name: String,
    /// Media type inferred for the evidence blob, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Evidence size in bytes.
    pub size: u64,
    /// Step that produced the evidence blob.
    pub producing_step_id: String,
    /// Execution number that produced the evidence blob.
    pub execution: u32,
    /// RFC 3339 creation timestamp, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Additional descriptor metadata, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<BlobDescriptorSummary>,
    /// Related resource links for the evidence blob.
    pub links: EvidenceLinks,
}

#[derive(Debug, Serialize)]
/// Metadata returned for a workflow file resource.
pub struct WorkflowFileMetadata {
    /// Run that owns the step reference.
    pub run_id: String,
    /// Encoded workflow file identifier used by the HTTP API.
    pub workflow_file_id: String,
    /// Source field name that referenced the file.
    pub field_name: String,
    /// Locator root used for the file: `workspace` or `run`.
    pub locator_kind: String,
    /// Workflow file name.
    pub name: String,
    /// Trusted-root-relative locator label for the file.
    pub relative_path: String,
    /// Media type inferred for the workflow file, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Workflow file size in bytes.
    pub size: u64,
    /// Step that referenced the workflow file.
    pub producing_step_id: String,
    /// Related resource links for the workflow file.
    pub links: WorkflowFileLinks,
}

#[derive(Debug, Serialize)]
/// Hypermedia links for an evidence resource.
pub struct EvidenceLinks {
    /// Canonical metadata URL for the evidence blob.
    pub self_url: String,
    /// Direct content URL for downloading the evidence blob.
    pub content: String,
}

#[derive(Debug, Serialize)]
/// Hypermedia links for a workflow file resource.
pub struct WorkflowFileLinks {
    /// Canonical metadata URL for the workflow file.
    pub self_url: String,
    /// Direct content URL when a trusted workspace root permits content reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Serialize)]
/// Summary of an authored workflow input declaration.
pub struct WorkflowInput {
    /// Input name exposed by the workflow.
    pub name: String,
    /// Declared value type name.
    pub value_type: String,
    /// Whether callers must provide a value.
    pub required: bool,
    /// Whether the input declaration includes a default value.
    pub has_default: bool,
    /// Optional human-readable input description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
/// Summary of an authored workflow step and its compiled graph edges.
pub struct WorkflowStep {
    /// Stable step identifier.
    pub step_id: String,
    /// Immediate parent step id when this step is nested inside a control block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_step_id: Option<String>,
    /// Immediate child step ids nested directly under this step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_step_ids: Vec<String>,
    /// Authored step kind.
    pub kind: String,
    /// Adapter identifier, when the step invokes an adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// Plugin reference, when the step invokes a plugin action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// Action name, when the step invokes an action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Execution kind, when relevant to the step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_kind: Option<String>,
    /// Control-flow mode for control steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// Referenced subflow path, when the step launches a subflow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subflow_path: Option<String>,
    /// Keys present in the authored `with` object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with_keys: Vec<String>,
    /// Keys passed into a subflow input object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subflow_input_keys: Vec<String>,
    /// Workflow input references consumed by the step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_input_refs: Vec<String>,
    /// Conditional guard expression, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_guard: Option<String>,
    /// Target step identifiers reachable from the `if` then branch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub if_then_refs: Vec<String>,
    /// Target step identifiers reachable from the `if` else branch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub if_else_refs: Vec<String>,
    /// Loop continuation expression, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_while: Option<String>,
    /// Loop termination expression, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_until: Option<String>,
    /// Target step identifiers reachable from the loop body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loop_body_refs: Vec<String>,
    /// Maximum loop iteration count, when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_max_iters: Option<u32>,
    /// Maximum loop duration, when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_max_duration: Option<String>,
    /// Prompt source descriptor for prompt-driven steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_source: Option<String>,
    /// Prompt detail or preview information, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_detail: Option<String>,
    /// Skills referenced by the step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Compiled graph edges that leave this step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graph_edges: Vec<WorkflowGraphEdge>,
}

#[derive(Debug, Serialize)]
/// Edge in the workflow step graph.
pub struct WorkflowGraphEdge {
    /// Edge kind such as sequential, branch, or loop.
    pub kind: String,
    /// Human-readable edge label.
    pub label: String,
    /// Target step identifier.
    pub target_step_id: String,
    /// Guard expression for conditional edges, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    /// Loop continuation expression, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_while: Option<String>,
    /// Loop termination expression, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_until: Option<String>,
    /// Maximum loop iteration count, when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_max_iters: Option<u32>,
    /// Explicit loop entry/exit routing revision. Version 1 includes normal exit
    /// at the successful wrapper's activation-local cap; never implies approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_routing_version: Option<u32>,
}

#[derive(Debug, Serialize)]
/// Reference to a prompt source used by a workflow step.
pub struct WorkflowPromptReference {
    /// Step that uses the prompt.
    pub step_id: String,
    /// Prompt source type.
    pub source: String,
    /// Prompt file path, when the source is file-backed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Short prompt preview, when one is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Serialize)]
/// Validation status attached to workflow inspection responses.
pub struct WorkflowValidation {
    /// Overall validation status.
    pub status: String,
    /// Validation errors recorded for the workflow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize)]
/// Summary view of a discovered workflow.
pub struct WorkflowSummary {
    /// Stable workflow reference combining id and version.
    pub workflow_ref: String,
    /// Authored workflow identifier.
    pub workflow_id: String,
    /// Authored workflow version.
    pub workflow_version: String,
    /// Source workflow file reference, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// Optional workflow title from authored metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Summary of declared workflow inputs.
    pub input_schema_summary: Vec<WorkflowInput>,
    /// Whether a compiled recipe file is available for inspection.
    pub compiled_recipe_available: bool,
    /// Whether the workflow is runnable in principle based on validation state.
    pub runnable_in_principle: bool,
    /// Number of steps in the workflow.
    pub step_count: usize,
    /// Number of root-level steps in the workflow.
    pub root_step_count: usize,
    /// Whether another discovered workflow references this workflow as a subflow.
    pub is_subflow: bool,
    /// Related resource links for the workflow.
    pub links: WorkflowLinks,
}

#[derive(Debug, Serialize)]
/// Hypermedia links for a workflow resource.
pub struct WorkflowLinks {
    /// Canonical URL for the workflow resource.
    pub self_url: String,
}

#[derive(Debug, Serialize)]
/// Detailed view of a discovered workflow.
pub struct WorkflowDetail {
    /// Stable workflow reference combining id and version.
    pub workflow_ref: String,
    /// Authored workflow identifier.
    pub workflow_id: String,
    /// Authored workflow version.
    pub workflow_version: String,
    /// Source workflow file reference, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// Optional workflow title from authored metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Full set of declared workflow inputs.
    pub input_declarations: Vec<WorkflowInput>,
    /// Workflow step summaries with graph information.
    pub step_graph_summary: Vec<WorkflowStep>,
    /// Prompt references discovered while compiling/inspecting the workflow.
    pub prompt_references: Vec<WorkflowPromptReference>,
    /// Validation summary for the workflow.
    pub validation: WorkflowValidation,
    /// Whether a compiled recipe file is available for inspection.
    pub compiled_recipe_available: bool,
    /// Whether the workflow is runnable in principle based on validation state.
    pub runnable_in_principle: bool,
    /// Whether another discovered workflow references this workflow as a subflow.
    pub is_subflow: bool,
}
