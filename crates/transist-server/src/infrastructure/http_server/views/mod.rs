// SPDX-License-Identifier: MPL-2.0
//! Server-rendered templates for the `/inspect` UI.
//!
//! The SSR layer reuses the same `InspectionApiService` / `MetricsApiService`
//! instances that back the JSON `/api/v1/...` surface. View structs here are
//! template-input DTOs only: they pre-format strings and pre-compute booleans
//! so templates stay free of business logic.

use askama::Template;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

mod helpers;
mod run_detail;
mod runs_list;
mod step_detail;
mod step_outputs;
mod workflows;

pub(crate) use run_detail::{
    artifact_detail, evidence_detail, run_detail, run_failures, workflow_file_detail,
};
pub(crate) use runs_list::{inspect_root_redirect, inspect_static, runs_list};
pub(crate) use step_detail::step_detail;
pub(crate) use workflows::{workflow_compiled, workflow_detail, workflow_source, workflows_list};

const HTML_CONTENT_TYPE: HeaderValue = HeaderValue::from_static("text/html; charset=utf-8");

/// Renders an askama template into an HTML response with status 200.
pub(crate) fn render<T: Template>(template: &T) -> Response {
    match template.render() {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, HTML_CONTENT_TYPE)],
            Html(body),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(?error, "askama template render failed");
            internal_error_response("template render failed")
        }
    }
}

/// Renders an error template at the given status code.
pub(crate) fn render_error(status: StatusCode, code: &str, message: &str) -> Response {
    let template = ErrorTemplate {
        code: code.to_string(),
        message: message.to_string(),
    };
    let body = template
        .render()
        .unwrap_or_else(|_| format!("error: {code}: {message}"));
    (
        status,
        [(header::CONTENT_TYPE, HTML_CONTENT_TYPE)],
        Html(body),
    )
        .into_response()
}

fn internal_error_response(reason: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, HTML_CONTENT_TYPE)],
        Html(format!("<h1>500 Internal Server Error</h1><p>{reason}</p>")),
    )
        .into_response()
}

/// Maps an `ApiProblem` to an HTML error page.
pub(crate) fn render_api_problem(problem: crate::application::ApiProblem) -> Response {
    render_error(problem.status, problem.code, &problem.message)
}

#[derive(Template)]
#[template(path = "error.html")]
pub(crate) struct ErrorTemplate {
    pub(crate) code: String,
    pub(crate) message: String,
}

// -------- Runs list --------

#[derive(Debug)]
pub(crate) struct RunsListFilters {
    pub status: String,
    pub workflows: Vec<String>,
    pub created_after: String,
    pub created_before: String,
    pub limit: String,
}

#[derive(Debug)]
pub(crate) struct StatusOption {
    pub value: String,
    pub selected: bool,
}

#[derive(Debug)]
pub(crate) struct WorkflowOption {
    pub value: String,
    pub selected: bool,
}

#[derive(Debug)]
pub(crate) struct RunsListRow {
    pub run_id: String,
    pub status: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub updated_at: String,
    pub total_tokens: String,
    pub duration_label: String,
}

#[derive(Debug)]
pub(crate) struct PaginationView {
    pub total: usize,
    pub shown_lo: usize,
    pub shown_hi: usize,
}

#[derive(Template)]
#[template(path = "runs_list.html")]
pub(crate) struct RunsListTemplate {
    pub filters: RunsListFilters,
    pub status_options: Vec<StatusOption>,
    pub workflow_options: Vec<WorkflowOption>,
    pub workflow_selected_count: usize,
    pub runs: Vec<RunsListRow>,
    pub pagination: PaginationView,
    pub prev_url: Option<String>,
    pub next_url: Option<String>,
}

// -------- Run detail --------

#[derive(Debug, Clone)]
pub(crate) struct LinkRef {
    pub label: String,
    pub href: String,
}

#[derive(Debug)]
pub(crate) struct RunHeadline {
    pub run_id: String,
    pub status: String,
    #[allow(dead_code)]
    pub workflow_label: String,
    pub workflow_link: LinkRef,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_label: Option<String>,
    pub root_link: LinkRef,
    pub parent_link: Option<LinkRef>,
    pub children_count: usize,
    pub auto_refresh_eligible: bool,
}

/// Prominent status-explanation banner shown on the run detail page when the
/// run did not finish successfully (or is still in progress). The fields here
/// pre-format human-readable strings so the template stays free of branching
/// over the underlying enums.
#[derive(Debug)]
pub(crate) struct RunStatusBanner {
    /// Run status (drives banner color via CSS class).
    pub status: String,
    /// One-line headline (e.g. "This run failed at step 'dispatch_runs'.").
    pub headline: String,
    /// Optional secondary context line (e.g. error message + exit code).
    pub detail: Option<String>,
    /// Optional link to the failed / current / blocked step.
    pub primary_link: Option<LinkRef>,
    /// Label for the primary link (e.g. "View failed step").
    pub primary_link_label: Option<&'static str>,
    /// Optional link to the stderr artifact for failed process steps.
    pub stderr_link: Option<LinkRef>,
    /// CLI hint surfaced under the banner (e.g. `transist runs gate approve …`).
    pub cli_hint: Option<String>,
    /// Pending blocked-gate identifiers (rendered as a chip list).
    pub blocked_gate_ids: Vec<String>,
    /// Preflight blockers surfaced inline on the banner when the run failed
    /// before any step started (i.e. there is no failed step to link to).
    pub preflight_blockers: Vec<PreflightBlockerView>,
}

#[derive(Debug)]
/// One preflight blocker line rendered under the run status banner.
pub(crate) struct PreflightBlockerView {
    /// Check kind/family (e.g. `secret_ref`, `plugin_action`).
    pub kind: String,
    /// Human-readable diagnostic message from the failed check.
    pub message: String,
    /// Optional plugin id associated with the finding.
    pub plugin_id: Option<String>,
    /// Optional action id associated with the finding.
    pub action: Option<String>,
    /// Step ids associated with the finding.
    pub step_ids: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct OperatorSummary {
    pub action_required: bool,
    pub current_step_id: Option<String>,
    pub blocked_gate_count: usize,
    pub expected_user_action: Option<String>,
    pub recommended_next_action: String,
}

#[derive(Debug, Clone)]
pub(crate) struct GateGuidanceView {
    pub prompt: Option<String>,
    pub reason: Option<String>,
    pub expected_response: Option<String>,
    pub options: Vec<String>,
    pub summary: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StepRow {
    pub output_preview: step_outputs::OutputPreview,
    pub output_execution: Option<u32>,
    pub output_status: String,
    pub output_source: String,
    pub output_url: String,
    pub step_id: String,
    pub step_kind: String,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub artifact_count: usize,
    pub evidence_count: usize,
    pub child_run_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct EventRow {
    pub event_id: String,
    pub timestamp: String,
    pub event_type: String,
    pub step_id: Option<String>,
    pub summary_short: String,
}

#[derive(Debug)]
pub(crate) struct GateRow {
    pub review_documents: Vec<transist::InspectionDocument>,
    pub gate_id: String,
    pub status: String,
    pub step_id: String,
    pub created_at: Option<String>,
    pub decision_state: Option<String>,
    pub decision_actor: Option<String>,
    pub decision_reason: Option<String>,
    pub decision_at: Option<String>,
    pub guidance: GateGuidanceView,
}

#[derive(Debug)]
pub(crate) struct OverviewTrustPluginRow {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub signature_status: String,
    pub signer_key_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct OverviewTrustView {
    pub trusted_execution_mode: bool,
    pub workflow_label: Option<String>,
    pub workflow_digest: Option<String>,
    pub workflow_signature_status: Option<String>,
    pub workflow_signer_key_id: Option<String>,
    pub decision_outcome: Option<String>,
    pub decision_reason: Option<String>,
    pub policy_mode: Option<String>,
    pub policy_reason_code: Option<String>,
    pub plugins: Vec<OverviewTrustPluginRow>,
}

#[derive(Debug)]
pub(crate) struct OverviewLocalityStepRow {
    pub step_id: String,
    pub provider: String,
    pub model: String,
    pub provider_kind: String,
}

#[derive(Debug)]
pub(crate) struct OverviewExternalTransferRow {
    pub step_id: String,
    pub timestamp: String,
    pub destination: String,
    pub provider: String,
    pub provider_kind: String,
    pub policy_mode: String,
}

#[derive(Debug)]
pub(crate) struct OverviewLocalityView {
    pub steps: Vec<OverviewLocalityStepRow>,
    pub external_transfers: Vec<OverviewExternalTransferRow>,
}

#[derive(Debug)]
pub(crate) struct OverviewEvidenceExportView {
    pub location: Option<String>,
    pub command: String,
    pub format: String,
    pub generated_at: Option<String>,
}

#[derive(Debug)]
pub(crate) struct FailureRow {
    pub failure_id: String,
    pub step_id: String,
    pub execution: u32,
    pub failure_class: String,
    pub error_code: String,
    pub message_redacted: String,
    pub adapter_kind: String,
    pub adapter_name: String,
    pub tool_name: String,
    pub retryable: bool,
    pub safe_to_retry: bool,
    pub requires_manual_attention: bool,
    pub state_mutation_detected: bool,
    #[allow(dead_code)]
    pub first_seen_at: String,
    pub last_seen_at: String,
    #[allow(dead_code)]
    pub occurrence_count: u32,
}

#[derive(Debug)]
pub(crate) struct BlobRow {
    #[allow(dead_code)]
    pub blob_id: String,
    pub name: String,
    pub size: u64,
    pub media_type: String,
    pub producing_step_id: String,
    pub execution: u32,
    pub detail_url: String,
}

#[derive(Debug)]
pub(crate) struct WorkflowFileRow {
    #[allow(dead_code)]
    pub workflow_file_id: Option<String>,
    pub field_name: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    #[allow(dead_code)]
    pub media_type: String,
    pub producing_step_id: String,
    pub execution: u32,
    pub detail_url: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ChildRunRow {
    pub run_id: String,
    pub status: String,
    pub workflow_label: String,
    pub artifact_count: usize,
    pub evidence_count: usize,
    pub depth: usize,
}

#[derive(Debug)]
pub(crate) struct OverviewMetrics {
    pub step_executions: u32,
    pub artifacts: usize,
    pub evidence_files: usize,
    pub input_tokens: String,
    pub output_tokens: String,
    pub total_tokens: String,
    pub duration_label: String,
}

#[derive(Debug)]
pub(crate) struct OverviewView {
    pub metrics: Option<OverviewMetrics>,
    pub recovery_state: Option<String>,
    pub cleanup_state: Option<String>,
    pub workflow_inputs_json: String,
    pub step_counts_json: String,
    pub trust: OverviewTrustView,
    pub locality: OverviewLocalityView,
    pub evidence_export: OverviewEvidenceExportView,
}

#[derive(Template)]
#[template(path = "run_detail.html")]
pub(crate) struct RunDetailTemplate {
    pub headline: RunHeadline,
    pub status_banner: Option<RunStatusBanner>,
    pub operator_summary: OperatorSummary,
    pub tab: String,
    pub iteration_scope: &'static str,
    pub selected_iteration: Option<u32>,
    pub available_iterations: Vec<u32>,
    pub tabs: Vec<TabLink>,
    pub overview: Option<OverviewView>,
    pub steps: Vec<StepRow>,
    pub events: Vec<EventRow>,
    pub gates: Vec<GateRow>,
    pub failures: Vec<FailureRow>,
    pub artifacts: Vec<BlobRow>,
    pub evidence: Vec<BlobRow>,
    pub workflow_files: Vec<WorkflowFileRow>,
    pub children: Vec<ChildRunRow>,
}

#[derive(Debug)]
pub(crate) struct TabLink {
    #[allow(dead_code)]
    pub key: &'static str,
    pub label: &'static str,
    pub href: String,
    pub active: bool,
}

// -------- Step detail --------

/// Lightweight snapshot of a child run's state, embedded on the subflow /
/// loop step detail page so the user doesn't have to navigate away just to
/// check whether the child run is progressing.
#[derive(Debug)]
pub(crate) struct ChildRunOverview {
    pub run_id: String,
    pub run_url: String,
    pub workflow_label: String,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub artifact_count: usize,
    pub evidence_count: usize,
    /// Serialised step-counts JSON for the child run (collapsed in a
    /// `<details>` block so it doesn't dominate the page).
    pub step_counts_json: String,
    /// Metrics block, absent when no metrics have been collected yet.
    pub metrics: Option<OverviewMetrics>,
}

#[derive(Debug)]
pub(crate) struct StepDetailTemplateData {
    pub output: step_outputs::OutputView,
    pub output_execution: Option<u32>,
    pub output_status: String,
    pub output_source: String,
    pub output_result_present: bool,
    pub output_selector: String,
    pub output_options: Vec<StatusOption>,
    pub output_json_url: String,
    pub iteration_value: String,
    pub run_id: String,
    pub step_id: String,
    pub step_kind: String,
    pub status: String,
    pub execution: Option<u32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    #[allow(dead_code)]
    pub is_current_active: bool,
    /// Typed fields surfaced from the step's input/output summaries so users
    /// don't have to expand the raw JSON to see them.
    pub summary_fields: Vec<StepSummaryField>,
    /// Output-summary key lists (e.g. `output_object_keys`) rendered as chips.
    pub key_groups: Vec<StepKeyGroup>,
    /// Optional URL to a child run for subflow / loop steps.
    pub child_run_url: Option<String>,
    pub child_run_id: Option<String>,
    /// Inline snapshot of the child run's state, when one is attached to this
    /// step. `None` if the step has no child run or if the child run could not
    /// be loaded (e.g. not yet created).
    pub child_run_overview: Option<ChildRunOverview>,
    pub gate_guidance: Option<GateGuidanceView>,
    /// Preflight blockers that mention this step. Populated when the run
    /// failed during preflight (before this step ever ran), so operators see
    /// the failure context inline instead of bouncing back to the run page.
    pub preflight_blockers: Vec<PreflightBlockerView>,
    pub input_summary_json: String,
    pub output_summary_json: String,
    pub metrics_json: String,
    pub iteration_scope: &'static str,
    pub selected_iteration: Option<u32>,
    pub available_iterations: Vec<u32>,
    pub artifacts: Vec<BlobRow>,
    pub evidence: Vec<BlobRow>,
    pub workflow_files: Vec<WorkflowFileRow>,
}

/// A single labelled field surfaced into the step detail key/value grid.
#[derive(Debug)]
pub(crate) struct StepSummaryField {
    pub label: &'static str,
    pub value: String,
    /// When `true`, render the value in a monospace span.
    pub mono: bool,
}

/// A labelled group of keys (e.g. `output_object_keys`) rendered as chips.
#[derive(Debug)]
pub(crate) struct StepKeyGroup {
    pub label: &'static str,
    pub keys: Vec<String>,
}

#[derive(Template)]
#[template(path = "step_detail.html")]
pub(crate) struct StepDetailTemplate {
    pub data: StepDetailTemplateData,
}

// -------- Blob detail --------

#[derive(Debug, PartialEq)]
pub(crate) enum BlobKind {
    Artifact,
    Evidence,
    WorkflowFile,
}

impl BlobKind {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            BlobKind::Artifact => "Artifact",
            BlobKind::Evidence => "Evidence",
            BlobKind::WorkflowFile => "Workflow file",
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum PreviewMode {
    Text,
    Json,
    Image,
    Html,
    Binary,
    Missing,
}

#[derive(Debug)]
pub(crate) struct BlobDetailTemplateData {
    pub run_id: String,
    pub kind_label: &'static str,
    pub blob_id: String,
    pub name: String,
    pub size: u64,
    pub media_type: String,
    pub producing_step_id: Option<String>,
    pub execution: Option<u32>,
    pub field_name: Option<String>,
    pub path: Option<String>,
    pub content_url: String,
    pub preview_text: Option<String>,
    pub preview_image: bool,
    pub preview_html: bool,
    pub preview_too_large: bool,
}

#[derive(Template)]
#[template(path = "blob_detail.html")]
pub(crate) struct BlobDetailTemplate {
    pub data: BlobDetailTemplateData,
}

// -------- Workflows --------

#[derive(Debug)]
pub(crate) struct WorkflowsListRow {
    pub workflow_ref: String,
    #[allow(dead_code)]
    pub workflow_id: String,
    #[allow(dead_code)]
    pub workflow_version: String,
    pub title: Option<String>,
    pub source_ref: Option<String>,
    #[allow(dead_code)]
    pub step_count: usize,
    pub root_step_count: usize,
    pub runnable: bool,
    pub compiled_recipe_available: bool,
}

#[derive(Template)]
#[template(path = "workflows_list.html")]
pub(crate) struct WorkflowsListTemplate {
    pub workflows: Vec<WorkflowsListRow>,
}

#[derive(Debug)]
pub(crate) struct WorkflowInputRow {
    pub name: String,
    pub value_type: String,
    pub required: bool,
    pub has_default: bool,
    pub description: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowStepRow {
    pub step_id: String,
    pub parent_step_id: Option<String>,
    pub child_step_ids: Vec<String>,
    pub kind: String,
    pub adapter: Option<String>,
    pub plugin: Option<String>,
    pub action: Option<String>,
    #[allow(dead_code)]
    pub if_guard: Option<String>,
    #[allow(dead_code)]
    pub loop_while: Option<String>,
    #[allow(dead_code)]
    pub loop_until: Option<String>,
    pub workflow_input_refs: Vec<String>,
    pub edges: Vec<String>,
    pub depth: usize,
}

#[derive(Debug)]
pub(crate) struct WorkflowPluginRow {
    pub plugin: String,
    pub adapters: Vec<String>,
    pub actions: Vec<String>,
    pub step_ids: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct WorkflowStepGroup {
    pub root: WorkflowStepRow,
    pub descendants: Vec<WorkflowStepRow>,
}

#[derive(Template)]
#[template(path = "workflow_detail.html")]
pub(crate) struct WorkflowDetailTemplate {
    pub workflow_ref: String,
    pub workflow_id: String,
    pub workflow_version: String,
    pub title: Option<String>,
    pub source_ref: Option<String>,
    pub validation_status: String,
    pub validation_errors: Vec<String>,
    pub runnable: bool,
    pub compiled_recipe_available: bool,
    pub tab: String,
    pub tabs: Vec<TabLink>,
    pub plugins: Vec<WorkflowPluginRow>,
    pub inputs: Vec<WorkflowInputRow>,
    pub step_groups: Vec<WorkflowStepGroup>,
    pub source_url: String,
    pub compiled_url: String,
}

#[derive(Template)]
#[template(path = "workflow_content.html")]
pub(crate) struct WorkflowContentTemplate {
    pub workflow_ref: String,
    pub kind_label: &'static str,
    pub content_type: String,
    pub content_text: Option<String>,
    pub other_url: String,
    pub other_label: &'static str,
    pub back_url: String,
}
