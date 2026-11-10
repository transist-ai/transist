// SPDX-License-Identifier: MPL-2.0
//! Server-rendered HTML handlers for the `/inspect` UI. These reuse the same
//! application services that back the JSON `/api/v1/...` surface.

use super::{
    BlobDetailTemplate, BlobDetailTemplateData, BlobKind, BlobRow, ChildRunOverview, ChildRunRow,
    EventRow, FailureRow, GateGuidanceView, GateRow, LinkRef, OperatorSummary,
    OverviewEvidenceExportView, OverviewExternalTransferRow, OverviewLocalityStepRow,
    OverviewLocalityView, OverviewMetrics, OverviewTrustPluginRow, OverviewTrustView, OverviewView,
    PaginationView, RunDetailTemplate, RunHeadline, RunsListFilters, RunsListRow, RunsListTemplate,
    StatusOption, StepDetailTemplate, StepDetailTemplateData, StepKeyGroup, StepRow,
    StepSummaryField, TabLink, WorkflowContentTemplate, WorkflowDetailTemplate, WorkflowFileRow,
    WorkflowInputRow, WorkflowOption, WorkflowPluginRow, WorkflowStepGroup, WorkflowStepRow,
    WorkflowsListRow, WorkflowsListTemplate, render, render_api_problem, render_error,
};
use crate::application::ApiProblem;
use crate::domain::api_models::{
    ArtifactMetadata, EvidenceMetadata, GateSummary, RunDetail, WorkflowFileMetadata,
};
use crate::infrastructure::http_server::params::{
    EventsQueryParams, ensure_valid_workflow_ref, parse_events_query,
};
use crate::infrastructure::http_server::router::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const STATIC_INSPECT_CSS: &str = include_str!("../../../../ui/static/inspect.css");
const STATIC_INSPECT_JS: &str = include_str!("../../../../ui/static/inspect.js");
const PREVIEW_LIMIT_BYTES: u64 = 256 * 1024;

const NON_TERMINAL_STATUSES: &[&str] = &["created", "planned", "running", "waiting", "blocked"];

#[derive(Debug)]
struct BlockedGateContext {
    owning_run_id: String,
    gate: GateSummary,
}

/// Failure context resolved from a parent run's `actionable_run_id` descendant.
/// Used so that when a parent run failed because of a deeper subflow descendant
/// (e.g. `pr_review_loop` whose own last_result has no error_message), the
/// banner can still surface the root-cause step and message inline.
#[derive(Debug)]
struct ActionableFailureContext {
    run_id: String,
    failed_step: crate::domain::api_models::StepSummary,
}

pub(crate) async fn inspect_root_redirect() -> Response {
    Redirect::to("/inspect/runs").into_response()
}

#[derive(Deserialize)]
pub(crate) struct StaticPath {
    file: String,
}

pub(crate) async fn inspect_static(Path(StaticPath { file }): Path<StaticPath>) -> Response {
    let (body, content_type) = match file.as_str() {
        "inspect.css" => (STATIC_INSPECT_CSS, "text/css; charset=utf-8"),
        "inspect.js" => (STATIC_INSPECT_JS, "text/javascript; charset=utf-8"),
        _ => {
            return render_error(
                StatusCode::NOT_FOUND,
                "resource_not_found",
                "static asset not found",
            );
        }
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}

// ---------------- Runs list ----------------

pub(crate) async fn runs_list(
    State(state): State<Arc<AppState>>,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> Response {
    // Parse the raw query string ourselves so we can capture multiple
    // `workflow=...` checkbox values (axum's serde-based Query keeps only the
    // last occurrence of repeated keys).
    let pairs: Vec<(String, String)> = raw_query
        .as_deref()
        .filter(|q| !q.is_empty())
        .map(|q| serde_urlencoded::from_str(q).unwrap_or_default())
        .unwrap_or_default();

    let mut workflows_selected: Vec<String> = Vec::new();
    let mut status_filter = String::new();
    let mut created_after_raw = String::new();
    let mut created_before_raw = String::new();
    let mut limit_raw = String::new();
    let mut offset_raw = String::new();
    for (k, v) in &pairs {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            continue;
        }
        match k.as_str() {
            "workflow" => workflows_selected.push(trimmed.to_string()),
            "status" => status_filter = trimmed.to_string(),
            "created_after" => created_after_raw = trimmed.to_string(),
            "created_before" => created_before_raw = trimmed.to_string(),
            "limit" => limit_raw = trimmed.to_string(),
            "offset" => offset_raw = trimmed.to_string(),
            _ => {}
        }
    }
    workflows_selected.sort();
    workflows_selected.dedup();

    // Validate explicit status filter against the known status enum to keep
    // legacy 400-on-unknown behavior even though the dropdown now sources its
    // options from runtime data.
    if !status_filter.is_empty()
        && !matches!(
            status_filter.as_str(),
            "created"
                | "planned"
                | "running"
                | "waiting"
                | "blocked"
                | "success"
                | "failed"
                | "canceled"
        )
    {
        return render_api_problem(crate::application::ApiProblem::unsupported_query_value(
            format!("unsupported run status '{status_filter}'"),
        ));
    }

    // Default page size 50; clamp to a sensible upper bound.
    let user_limit = limit_raw
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0 && *n <= 500)
        .unwrap_or(50);
    let user_offset = offset_raw.parse::<usize>().ok().unwrap_or(0);

    // Fetch the full root-run set once: facets need to be independent of
    // currently-applied filters, and applying status/workflow/datetime filters
    // in Rust is cheap at local-scale.
    let all_query = transist::RunListQuery {
        pagination: transist::PaginationQuery {
            limit: 10_000,
            offset: 0,
        },
        scope: transist::RunListScope::Roots,
        status: None,
        workflow: None,
        created_after_ms: None,
        created_before_ms: None,
        updated_after_ms: None,
        updated_before_ms: None,
    };
    let payload = match state.inspection.list_runs(&all_query) {
        Ok(p) => p,
        Err(problem) => return render_api_problem(problem),
    };
    let metrics_map = state.metrics.list_run_metrics().unwrap_or_default();

    // Compute facet options from the unfiltered set.
    let mut status_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut workflow_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for r in &payload.data {
        status_set.insert(r.status.clone());
        workflow_set.insert(format!("{}@{}", r.workflow_id, r.workflow_version));
    }

    // Parse datetime filter inputs (datetime-local or RFC3339) into ms.
    let after_ms = if created_after_raw.is_empty() {
        None
    } else {
        crate::application::parse_rfc3339_to_ms(&normalize_datetime_input(
            created_after_raw.clone(),
        ))
        .ok()
    };
    let before_ms = if created_before_raw.is_empty() {
        None
    } else {
        crate::application::parse_rfc3339_to_ms(&normalize_datetime_input(
            created_before_raw.clone(),
        ))
        .ok()
    };

    let workflow_filter: std::collections::HashSet<String> =
        workflows_selected.iter().cloned().collect();

    let filtered: Vec<crate::domain::api_models::RunSummary> = payload
        .data
        .into_iter()
        .filter(|r| {
            if !status_filter.is_empty() && r.status != status_filter {
                return false;
            }
            if !workflow_filter.is_empty() {
                let wref = format!("{}@{}", r.workflow_id, r.workflow_version);
                if !workflow_filter.contains(&wref) {
                    return false;
                }
            }
            if after_ms.is_some() || before_ms.is_some() {
                let created_ms = crate::application::parse_rfc3339_to_ms(&r.created_at).ok();
                if let (Some(t), Some(after)) = (created_ms, after_ms) {
                    if t < after {
                        return false;
                    }
                }
                if let (Some(t), Some(before)) = (created_ms, before_ms) {
                    if t > before {
                        return false;
                    }
                }
            }
            true
        })
        .collect();

    let total = filtered.len();
    let page: Vec<crate::domain::api_models::RunSummary> = filtered
        .into_iter()
        .skip(user_offset)
        .take(user_limit)
        .collect();
    let returned = page.len();
    let shown_lo = if returned == 0 { 0 } else { user_offset + 1 };
    let shown_hi = user_offset + returned;

    let rows: Vec<RunsListRow> = page
        .into_iter()
        .map(|run| {
            let metrics = metrics_map.get(&run.run_id);
            RunsListRow {
                run_id: run.run_id.clone(),
                status: run.status,
                workflow_id: run.workflow_id,
                workflow_version: run.workflow_version,
                updated_at: run.updated_at,
                total_tokens: metrics
                    .and_then(|m| m.total_tokens)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".to_string()),
                duration_label: metrics
                    .map(format_run_list_duration)
                    .unwrap_or_else(|| "—".to_string()),
            }
        })
        .collect();

    let filters = RunsListFilters {
        status: status_filter.clone(),
        workflows: workflows_selected.clone(),
        created_after: created_after_raw,
        created_before: created_before_raw,
        limit: user_limit.to_string(),
    };

    let prev_url = if user_offset > 0 {
        let new_offset = user_offset.saturating_sub(user_limit);
        Some(build_runs_url(&filters, new_offset, user_limit))
    } else {
        None
    };
    let next_url = if shown_hi < total {
        Some(build_runs_url(
            &filters,
            user_offset + user_limit,
            user_limit,
        ))
    } else {
        None
    };

    let status_options: Vec<StatusOption> = status_set
        .into_iter()
        .map(|v| StatusOption {
            selected: filters.status == v,
            value: v,
        })
        .collect();
    let workflow_options: Vec<WorkflowOption> = workflow_set
        .into_iter()
        .map(|v| WorkflowOption {
            selected: workflow_filter.contains(&v),
            value: v,
        })
        .collect();

    let workflow_selected_count = workflow_options.iter().filter(|o| o.selected).count();

    render(&RunsListTemplate {
        filters,
        status_options,
        workflow_options,
        workflow_selected_count,
        runs: rows,
        pagination: PaginationView {
            total,
            shown_lo,
            shown_hi,
        },
        prev_url,
        next_url,
    })
}

fn build_runs_url(filters: &RunsListFilters, offset: usize, limit: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !filters.status.is_empty() {
        parts.push(format!("status={}", url_encode(&filters.status)));
    }
    for w in &filters.workflows {
        parts.push(format!("workflow={}", url_encode(w)));
    }
    if !filters.created_after.is_empty() {
        parts.push(format!(
            "created_after={}",
            url_encode(&filters.created_after)
        ));
    }
    if !filters.created_before.is_empty() {
        parts.push(format!(
            "created_before={}",
            url_encode(&filters.created_before)
        ));
    }
    parts.push(format!("limit={limit}"));
    parts.push(format!("offset={offset}"));
    format!("/inspect/runs?{}", parts.join("&"))
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![b as char].into_iter().collect::<Vec<_>>()
            }
            other => format!("%{other:02X}").chars().collect::<Vec<_>>(),
        })
        .collect()
}

/// Normalizes an HTML `datetime-local` value (e.g. `2026-04-29T14:30` or
/// `2026-04-29T14:30:45`) into RFC3339 in UTC, so the existing application
/// parser accepts it. Existing RFC3339 input (anything with a timezone
/// designator) is left untouched. Empty / whitespace input becomes empty.
fn normalize_datetime_input(value: String) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let has_tz = trimmed.ends_with('Z')
        || trimmed.ends_with('z')
        || trimmed
            .rfind(['+', '-'])
            .map(|idx| idx > 10)
            .unwrap_or(false);
    if has_tz {
        return trimmed.to_string();
    }
    let bytes = trimmed.as_bytes();
    let looks_like_local = bytes.len() >= 16 && bytes.get(10) == Some(&b'T');
    if !looks_like_local {
        return trimmed.to_string();
    }
    match trimmed.len() {
        16 => format!("{trimmed}:00Z"),
        19 => format!("{trimmed}Z"),
        _ => trimmed.to_string(),
    }
}

fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let total_seconds = ms / 1000;
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("{minutes}m {seconds}s")
    }
}

fn format_overview_duration(metrics: &transist::RunMetricsSummary) -> String {
    let wall_clock = metrics
        .wall_clock_duration_ms
        .or(metrics.total_duration_ms)
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    let active = metrics
        .active_execution_duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    let blocked = metrics
        .blocked_wait_duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    format!("wall clock {wall_clock} | active {active} | blocked {blocked}")
}

fn format_run_list_duration(metrics: &transist::RunListMetricsSummary) -> String {
    let wall_clock = metrics
        .wall_clock_duration_ms
        .or(metrics.total_duration_ms)
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    let active = metrics
        .active_execution_duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    let blocked = metrics
        .blocked_wait_duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    format!("wall clock {wall_clock} | active {active} | blocked {blocked}")
}

// ---------------- Run detail ----------------

#[derive(Deserialize, Default)]
pub(crate) struct RunDetailParams {
    pub tab: Option<String>,
    pub iteration: Option<String>,
    pub limit: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub order: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct WorkflowDetailParams {
    pub tab: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct StepDetailParams {
    pub iteration: Option<String>,
    pub execution: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IterationScope {
    All,
    Last,
    Specific(u32),
}

impl IterationScope {
    fn key(self) -> &'static str {
        match self {
            IterationScope::All => "all",
            IterationScope::Last => "last",
            IterationScope::Specific(_) => "specific",
        }
    }
}

fn parse_iteration_scope(raw: Option<&str>) -> IterationScope {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some("all") => IterationScope::All,
        Some("last") => IterationScope::Last,
        Some(value) => match value.parse::<u32>() {
            Ok(iteration) => IterationScope::Specific(iteration),
            Err(_) => IterationScope::Last,
        },
        None => IterationScope::Last,
    }
}

fn filter_blob_rows_by_iteration(
    rows: Vec<BlobRow>,
    scope: IterationScope,
    latest_execution: u32,
) -> Vec<BlobRow> {
    match scope {
        IterationScope::All => rows,
        IterationScope::Last => rows
            .into_iter()
            .filter(|row| row.execution == latest_execution)
            .collect(),
        IterationScope::Specific(iteration) => rows
            .into_iter()
            .filter(|row| row.execution == iteration)
            .collect(),
    }
}

fn filter_workflow_file_rows_by_iteration(
    rows: Vec<WorkflowFileRow>,
    scope: IterationScope,
    latest_execution: u32,
) -> Vec<WorkflowFileRow> {
    match scope {
        IterationScope::All => rows,
        IterationScope::Last => rows
            .into_iter()
            .filter(|row| row.execution == latest_execution)
            .collect(),
        IterationScope::Specific(iteration) => rows
            .into_iter()
            .filter(|row| row.execution == iteration)
            .collect(),
    }
}

pub(crate) async fn run_detail(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Query(params): Query<RunDetailParams>,
) -> Response {
    let run = match state.inspection.get_run(&run_id) {
        Ok(r) => r,
        Err(problem) => return render_api_problem(problem),
    };
    let metrics = state.metrics.get_run_metrics(&run_id).ok().flatten();
    let iteration_scope = parse_iteration_scope(params.iteration.as_deref());
    let tab = params.tab.as_deref().unwrap_or("overview").to_string();
    let valid = matches!(
        tab.as_str(),
        "overview"
            | "steps"
            | "events"
            | "gates"
            | "failures"
            | "artifacts"
            | "evidence"
            | "workflow-files"
            | "children"
    );
    if !valid {
        return render_error(StatusCode::NOT_FOUND, "invalid_tab", "unknown tab");
    }

    let headline = build_headline(&run, metrics.as_ref(), params.tab.is_none());
    let tabs = tab_links(&run_id, &tab);

    // For runs in non-trivial intermediate/failed states, fetch the step list
    // once so the status banner can surface the failing/current step's
    // error_message and link. Successful, canceled and freshly-created runs
    // don't get a banner, so skip the extra round-trip in those cases.
    let need_steps_for_banner = !matches!(run.status.as_str(), "success" | "canceled" | "created");
    let need_steps_for_tab = tab == "steps";
    let mut latest_outputs = Vec::new();
    let mut steps_data: Option<Vec<crate::domain::api_models::StepSummary>> =
        if need_steps_for_banner || need_steps_for_tab {
            match state.inspection.list_steps_with_latest_outputs(&run_id) {
                Ok(rows) => {
                    let (summaries, outputs) = rows.into_iter().unzip();
                    latest_outputs = outputs;
                    Some(summaries)
                }
                Err(problem) => {
                    if need_steps_for_tab {
                        return render_api_problem(problem);
                    }
                    None
                }
            }
        } else {
            None
        };
    let blocked_gate_context = resolve_blocked_gate_context(state.as_ref(), &run);
    // When a failed parent run points at an actionable descendant, fetch it so
    // the banner can surface the descendant's failed-step error inline. Subflow
    // steps don't carry their own error_message; without this walk the banner
    // would only say "Exit code N." which hides the actual failure reason.
    let actionable_failure_context: Option<ActionableFailureContext> = if run.status == "failed" {
        run.actionable_run_id
            .as_ref()
            .filter(|id| id.as_str() != run.run_id.as_str())
            .and_then(|id| {
                let actionable_steps = state.inspection.list_steps(id).ok()?.data;
                let failed = actionable_steps
                    .into_iter()
                    .find(|s| s.status == "failed" || s.status == "canceled")?;
                Some(ActionableFailureContext {
                    run_id: id.clone(),
                    failed_step: failed,
                })
            })
    } else {
        None
    };
    let status_banner = build_status_banner(
        &run,
        steps_data.as_deref(),
        blocked_gate_context.as_ref(),
        actionable_failure_context.as_ref(),
    );
    let operator_summary = build_operator_summary(&run, blocked_gate_context.as_ref());

    let overview = if tab == "overview" {
        Some(build_overview(&run, metrics.as_ref()))
    } else {
        None
    };

    let steps = if tab == "steps" {
        let data = steps_data.take().unwrap_or_default();
        data.into_iter()
            .zip(latest_outputs)
            .map(|(s, output)| StepRow {
                output_preview: super::step_outputs::present_preview(output.value.as_ref()),
                output_execution: output.execution,
                output_status: output.status,
                output_source: output.source,
                output_url: format!(
                    "/inspect/runs/{run_id}/steps/{}?execution={}",
                    s.step_id,
                    output
                        .execution
                        .filter(|_| output.result_present)
                        .map(|ordinal| ordinal.to_string())
                        .unwrap_or_else(|| "latest".to_string()),
                ),
                step_id: s.step_id,
                step_kind: s.step_kind,
                status: s.status,
                started_at: s.started_at,
                finished_at: s.finished_at,
                artifact_count: s.output_summary.artifact_count,
                evidence_count: s.output_summary.evidence_count,
                child_run_id: s.output_summary.child_run_id,
            })
            .collect()
    } else {
        Vec::new()
    };

    let events = if tab == "events" {
        let events_query = match parse_events_query(EventsQueryParams {
            limit: params.limit.clone(),
            before: params.before.clone(),
            after: params.after.clone(),
            order: params.order.clone(),
        }) {
            Ok(q) => q,
            Err(problem) => return render_api_problem(problem),
        };
        match state.inspection.list_events(&run_id, &events_query) {
            Ok(p) => p
                .data
                .into_iter()
                .map(|e| EventRow {
                    event_id: e.event_id,
                    timestamp: e.timestamp,
                    event_type: e.event_type,
                    step_id: e.step_id,
                    summary_short: short_json(&e.summary, 200),
                })
                .collect(),
            Err(problem) => return render_api_problem(problem),
        }
    } else {
        Vec::new()
    };

    let gates = if tab == "gates" {
        match state.inspection.list_gates(&run_id) {
            Ok(p) => p
                .data
                .into_iter()
                .map(|g| GateRow {
                    review_documents: g.review_documents.clone(),
                    guidance: build_gate_guidance(&g.request_summary),
                    gate_id: g.gate_id,
                    status: g.status,
                    step_id: g.step_id,
                    created_at: g.created_at,
                    decision_state: g.decision.as_ref().map(|d| d.state.clone()),
                    decision_actor: g.decision.as_ref().and_then(|d| d.actor.clone()),
                    decision_reason: g.decision.as_ref().and_then(|d| d.reason.clone()),
                    decision_at: g.decision.and_then(|d| d.decided_at),
                })
                .collect(),
            Err(problem) => return render_api_problem(problem),
        }
    } else {
        Vec::new()
    };

    let failures = if tab == "failures" {
        match state.inspection.list_failures(&run_id) {
            Ok(p) => p.data.into_iter().map(failure_to_row).collect(),
            Err(problem) => return render_api_problem(problem),
        }
    } else {
        Vec::new()
    };

    let mut available_iterations: Vec<u32> = Vec::new();

    let artifacts = if tab == "artifacts" {
        match state
            .inspection
            .list_artifacts(&run_id, transist::BlobListScope::SelfOnly)
        {
            Ok(p) => {
                let mut rows = p.data;
                rows.sort_by(|left, right| {
                    right
                        .created_at
                        .cmp(&left.created_at)
                        .then(right.execution.cmp(&left.execution))
                        .then(left.producing_step_id.cmp(&right.producing_step_id))
                        .then(left.name.cmp(&right.name))
                });
                available_iterations = rows.iter().map(|row| row.execution).collect();
                available_iterations.sort_unstable();
                available_iterations.dedup();
                let latest_execution = available_iterations.iter().copied().max().unwrap_or(0);
                let rows: Vec<BlobRow> = rows
                    .into_iter()
                    .map(|a| artifact_to_row(&run_id, a))
                    .collect();
                filter_blob_rows_by_iteration(rows, iteration_scope, latest_execution)
            }
            Err(problem) => return render_api_problem(problem),
        }
    } else {
        Vec::new()
    };

    let evidence = if tab == "evidence" {
        match state
            .inspection
            .list_evidence(&run_id, transist::BlobListScope::SelfOnly)
        {
            Ok(p) => {
                let mut rows = p.data;
                rows.sort_by(|left, right| {
                    right
                        .created_at
                        .cmp(&left.created_at)
                        .then(right.execution.cmp(&left.execution))
                        .then(left.producing_step_id.cmp(&right.producing_step_id))
                        .then(left.name.cmp(&right.name))
                });
                available_iterations = rows.iter().map(|row| row.execution).collect();
                available_iterations.sort_unstable();
                available_iterations.dedup();
                let latest_execution = available_iterations.iter().copied().max().unwrap_or(0);
                let rows: Vec<BlobRow> = rows
                    .into_iter()
                    .map(|a| evidence_to_row(&run_id, a))
                    .collect();
                filter_blob_rows_by_iteration(rows, iteration_scope, latest_execution)
            }
            Err(problem) => return render_api_problem(problem),
        }
    } else {
        Vec::new()
    };

    let workflow_files = if tab == "workflow-files" {
        match state.inspection.list_steps(&run_id) {
            Ok(p) => {
                let mut rows: Vec<(Option<String>, Option<String>, u32, WorkflowFileRow)> =
                    Vec::new();
                for s in p.data {
                    let step_finished_at = s.finished_at.clone();
                    let step_started_at = s.started_at.clone();
                    let step_execution = s.execution.unwrap_or(0);
                    for w in s.output_summary.workflow_files {
                        rows.push((
                            step_finished_at.clone(),
                            step_started_at.clone(),
                            step_execution,
                            WorkflowFileRow {
                                detail_url: w.workflow_file_id.as_ref().map(|id| {
                                    format!("/inspect/runs/{run_id}/workflow-files/{id}")
                                }),
                                workflow_file_id: w.workflow_file_id,
                                field_name: w.field_name,
                                name: w.name,
                                path: w.relative_path,
                                size: w.size,
                                media_type: w.media_type.unwrap_or_default(),
                                producing_step_id: s.step_id.clone(),
                                execution: step_execution,
                            },
                        ));
                    }
                }
                rows.sort_by(|left, right| {
                    right
                        .0
                        .cmp(&left.0)
                        .then(right.1.cmp(&left.1))
                        .then(right.2.cmp(&left.2))
                        .then(left.3.producing_step_id.cmp(&right.3.producing_step_id))
                        .then(left.3.field_name.cmp(&right.3.field_name))
                        .then(left.3.name.cmp(&right.3.name))
                        .then(left.3.path.cmp(&right.3.path))
                });
                let rows: Vec<WorkflowFileRow> =
                    rows.into_iter().map(|(_, _, _, row)| row).collect();
                available_iterations = rows.iter().map(|row| row.execution).collect();
                available_iterations.sort_unstable();
                available_iterations.dedup();
                let latest_execution = available_iterations.iter().copied().max().unwrap_or(0);
                filter_workflow_file_rows_by_iteration(rows, iteration_scope, latest_execution)
            }
            Err(problem) => return render_api_problem(problem),
        }
    } else {
        Vec::new()
    };

    let children = if tab == "children" {
        run.contained_runs
            .iter()
            .map(|c| ChildRunRow {
                run_id: c.run_id.clone(),
                status: c.status.clone(),
                workflow_label: format!("{}@{}", c.workflow_id, c.workflow_version),
                artifact_count: c.artifact_count,
                evidence_count: c.evidence_count,
                depth: 1,
            })
            .collect()
    } else {
        Vec::new()
    };

    render(&RunDetailTemplate {
        headline,
        status_banner,
        operator_summary,
        tab,
        iteration_scope: iteration_scope.key(),
        selected_iteration: match iteration_scope {
            IterationScope::Specific(iteration) => Some(iteration),
            _ => None,
        },
        available_iterations,
        tabs,
        overview,
        steps,
        events,
        gates,
        failures,
        artifacts,
        evidence,
        workflow_files,
        children,
    })
}

pub(crate) async fn run_failures(Path(run_id): Path<String>) -> Response {
    Redirect::to(&format!("/inspect/runs/{run_id}?tab=failures")).into_response()
}

fn build_headline(
    run: &RunDetail,
    metrics: Option<&transist::RunMetricsSummary>,
    _is_default_tab: bool,
) -> RunHeadline {
    let workflow_label = format!("{}@{}", run.workflow_id, run.workflow_version);
    let workflow_link = LinkRef {
        label: workflow_label.clone(),
        href: format!("/inspect/workflows/{workflow_label}"),
    };
    let parent_link = run.parent_run_id.as_ref().map(|p| LinkRef {
        label: p.clone(),
        href: format!("/inspect/runs/{p}"),
    });
    let root_link = LinkRef {
        label: run.root_run_id.clone(),
        href: format!("/inspect/runs/{}", run.root_run_id),
    };
    let auto_refresh_eligible = NON_TERMINAL_STATUSES.contains(&run.status.as_str());
    RunHeadline {
        run_id: run.run_id.clone(),
        status: run.status.clone(),
        workflow_label,
        workflow_link,
        created_at: run.created_at.clone(),
        updated_at: run.updated_at.clone(),
        started_at: run.started_at.clone(),
        finished_at: run.finished_at.clone(),
        duration_label: metrics
            .and_then(|m| m.wall_clock_duration_ms.or(m.total_duration_ms))
            .map(format_duration_ms),
        root_link,
        parent_link,
        children_count: run.contained_runs.len(),
        auto_refresh_eligible,
    }
}

fn resolve_blocked_gate_context(state: &AppState, run: &RunDetail) -> Option<BlockedGateContext> {
    if let Some(gate) = run
        .blocked_gates
        .iter()
        .find(|gate| gate.status == "blocked")
    {
        return Some(BlockedGateContext {
            owning_run_id: run.run_id.clone(),
            gate: GateSummary {
                review_documents: gate.review_documents.clone(),
                run_id: gate.run_id.clone(),
                gate_id: gate.gate_id.clone(),
                status: gate.status.clone(),
                step_id: gate.step_id.clone(),
                created_at: gate.created_at.clone(),
                decision: gate.decision.as_ref().map(|decision| {
                    crate::domain::api_models::GateDecisionSummary {
                        review_request: decision.review_request.clone(),
                        state: decision.state.clone(),
                        actor: decision.actor.clone(),
                        reason: decision.reason.clone(),
                        decided_at: decision.decided_at.clone(),
                    }
                }),
                decision_history: gate.decision_history.clone(),
                request_summary: gate.request_summary.clone(),
            },
        });
    }

    let actionable_run_id = run.actionable_run_id.as_ref()?;
    if actionable_run_id == &run.run_id {
        return None;
    }
    let actionable = state.inspection.get_run(actionable_run_id).ok()?;
    let gate = actionable
        .blocked_gates
        .into_iter()
        .find(|candidate| candidate.status == "blocked")?;
    Some(BlockedGateContext {
        owning_run_id: actionable.run_id,
        gate,
    })
}

/// Builds a prominent status-explanation banner for non-successful runs.
/// Returns `None` when the run finished successfully — the headline strip is
/// enough in that case.
fn build_status_banner(
    run: &RunDetail,
    steps: Option<&[crate::domain::api_models::StepSummary]>,
    blocked_gate_context: Option<&BlockedGateContext>,
    actionable_failure: Option<&ActionableFailureContext>,
) -> Option<super::RunStatusBanner> {
    if matches!(run.status.as_str(), "success" | "canceled" | "created") {
        return None;
    }

    let failed_step = steps
        .unwrap_or(&[])
        .iter()
        .find(|s| s.status == "failed" || s.status == "canceled");

    let current_step = run.current_step.as_ref();

    // Headline.
    let headline = match run.status.as_str() {
        "failed" => {
            let preflight_failed = failed_step.is_none()
                && run
                    .preflight
                    .as_ref()
                    .map(|p| !p.failed_checks.is_empty())
                    .unwrap_or(false);
            match (failed_step, run.terminal_outcome.as_deref()) {
                (Some(s), _) => format!("This run failed at step '{}'.", s.step_id),
                (None, _) if preflight_failed => "This run failed during preflight.".to_string(),
                (None, Some(t)) if !t.is_empty() => format!("This run failed: {t}."),
                _ => "This run failed.".to_string(),
            }
        }
        "canceled" => match run.terminal_outcome.as_deref() {
            Some(t) if !t.is_empty() => format!("This run was canceled: {t}."),
            _ => "This run was canceled.".to_string(),
        },
        "blocked" => {
            let count = run
                .blocked_gates
                .len()
                .max(usize::from(blocked_gate_context.is_some()));
            if count == 0 {
                "This run is blocked.".to_string()
            } else if count == 1 {
                if let Some(g) = blocked_gate_context
                    .map(|ctx| &ctx.gate)
                    .or_else(|| run.blocked_gates.first())
                {
                    format!(
                        "This run is blocked: step '{}' is awaiting approval at gate '{}'.",
                        g.step_id, g.gate_id
                    )
                } else {
                    "This run is blocked.".to_string()
                }
            } else {
                let preview = run
                    .blocked_gates
                    .iter()
                    .take(3)
                    .map(|g| format!("'{}'", g.step_id))
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = if count > 3 {
                    format!(" (and {} more)", count - 3)
                } else {
                    String::new()
                };
                format!(
                    "This run is blocked: {count} steps are awaiting approval — {preview}{more}."
                )
            }
        }
        "waiting" => {
            if let Some(c) = current_step {
                format!("This run is waiting (current step: '{}').", c.step_id)
            } else {
                "This run is waiting.".to_string()
            }
        }
        "running" => {
            if let Some(c) = current_step {
                format!("This run is running. Current step: '{}'.", c.step_id)
            } else {
                "This run is running.".to_string()
            }
        }
        "planned" => "This run is planned but has not started yet.".to_string(),
        "created" => "This run was created and is queued to start.".to_string(),
        other => format!("Run status: {other}."),
    };

    // Detail line:
    // - failed: prefer the actionable descendant's failed-step error_message
    //   when the local failed step is a subflow with no error of its own.
    //   Otherwise fall back to the local failed step's error_message + exit.
    // - blocked: short excerpt from the gate request_summary
    // - other: terminal_outcome when populated
    let detail = if run.status == "failed" {
        let from_actionable = actionable_failure.and_then(|ctx| {
            let s = &ctx.failed_step;
            let msg = s
                .output_summary
                .error_message
                .as_deref()
                .unwrap_or("")
                .trim();
            if msg.is_empty() {
                return None;
            }
            let code = s
                .output_summary
                .error_code
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty());
            let exit = s.output_summary.exit_code;
            // Format: "<msg> (in step 'X' of run 'Y', code=<error_code>, exit code N)"
            let mut suffix_parts: Vec<String> =
                vec![format!("in step '{}' of run '{}'", s.step_id, ctx.run_id)];
            if let Some(c) = code {
                suffix_parts.push(format!("code={c}"));
            }
            if let Some(c) = exit {
                suffix_parts.push(format!("exit code {c}"));
            }
            Some(format!("{} ({}).", msg, suffix_parts.join(", ")))
        });
        from_actionable.or_else(|| {
            failed_step.and_then(|s| {
                let msg = s
                    .output_summary
                    .error_message
                    .as_deref()
                    .unwrap_or("")
                    .trim();
                let exit = s.output_summary.exit_code;
                match (msg.is_empty(), exit) {
                    (true, Some(code)) => Some(format!("Exit code {code}.")),
                    (false, Some(code)) => Some(format!("{msg} (exit code {code}).")),
                    (false, None) => Some(msg.to_string()),
                    (true, None) => None,
                }
            })
        })
    } else if run.status == "blocked" {
        blocked_gate_context
            .map(|ctx| &ctx.gate)
            .or_else(|| run.blocked_gates.first())
            .and_then(|g| extract_gate_prompt(&g.request_summary))
            .map(|p| format!("Awaiting approval: {p}"))
    } else {
        run.terminal_outcome.clone().filter(|t| !t.is_empty())
    };

    // Primary link to the relevant step (failed/current/blocked).
    let (primary_link, primary_link_label) = match run.status.as_str() {
        "failed" | "canceled" => {
            // Prefer deep-linking to the actionable descendant's failed step
            // (the actual root cause) over the local subflow step.
            let actionable_link = actionable_failure.map(|ctx| {
                (
                    Some(LinkRef {
                        label: format!("{} · {}", ctx.run_id, ctx.failed_step.step_id),
                        href: format!(
                            "/inspect/runs/{}/steps/{}",
                            ctx.run_id, ctx.failed_step.step_id
                        ),
                    }),
                    Some("View root-cause step"),
                )
            });
            actionable_link
                .or_else(|| {
                    failed_step.map(|s| {
                        (
                            Some(LinkRef {
                                label: s.step_id.clone(),
                                href: format!("/inspect/runs/{}/steps/{}", run.run_id, s.step_id),
                            }),
                            Some("View failed step"),
                        )
                    })
                })
                .unwrap_or((None, None))
        }
        "running" | "waiting" => current_step
            .map(|c| {
                (
                    Some(LinkRef {
                        label: c.step_id.clone(),
                        href: format!("/inspect/runs/{}/steps/{}", run.run_id, c.step_id),
                    }),
                    Some("View current step"),
                )
            })
            .unwrap_or((None, None)),
        "blocked" => blocked_gate_context
            .map(|ctx| {
                (
                    Some(LinkRef {
                        label: ctx.gate.step_id.clone(),
                        href: format!(
                            "/inspect/runs/{}/steps/{}",
                            ctx.owning_run_id, ctx.gate.step_id
                        ),
                    }),
                    Some("View gated step"),
                )
            })
            .or_else(|| {
                run.blocked_gates.first().map(|g| {
                    (
                        Some(LinkRef {
                            label: g.step_id.clone(),
                            href: format!("/inspect/runs/{}/steps/{}", run.run_id, g.step_id),
                        }),
                        Some("View gated step"),
                    )
                })
            })
            .unwrap_or((None, None)),
        _ => (None, None),
    };

    // For failed process steps, surface a direct link to the stderr artifact
    // when one exists — that's almost always the most useful next click.
    let stderr_link = if run.status == "failed" {
        failed_step.and_then(|s| {
            s.output_summary
                .artifact_refs
                .iter()
                .find(|a| {
                    a.name.eq_ignore_ascii_case("stderr.txt")
                        || a.name.ends_with("_stderr.txt")
                        || a.name == "stderr"
                })
                .map(|a| LinkRef {
                    label: a.name.clone(),
                    href: format!("/inspect/runs/{}/artifacts/{}", run.run_id, a.artifact_id),
                })
        })
    } else {
        None
    };

    let cli_hint = match run.status.as_str() {
        "blocked" => match run.blocked_gates.as_slice() {
            [single] => Some(format!(
                "transist runs gate approve {} {}",
                run.run_id, single.gate_id
            )),
            _ => blocked_gate_context
                .map(|ctx| {
                    format!(
                        "transist runs gate approve {} {}",
                        ctx.owning_run_id, ctx.gate.gate_id
                    )
                })
                .or_else(|| {
                    Some(format!(
                        "transist runs gate approve {} <gate-id>",
                        run.run_id
                    ))
                }),
        },
        "failed" => Some(format!("transist runs replay {}", run.run_id)),
        _ => None,
    };

    let blocked_gate_ids = if let Some(ctx) = blocked_gate_context {
        vec![ctx.gate.gate_id.clone()]
    } else {
        run.blocked_gates
            .iter()
            .map(|g| g.gate_id.clone())
            .collect()
    };

    // When a run failed before any step started, there is no failed step to
    // link to. Surface the preflight blocker list inline instead so the
    // operator can see the actual failure reason without leaving the page.
    let preflight_blockers = if run.status == "failed" && failed_step.is_none() {
        run.preflight
            .as_ref()
            .map(|p| {
                p.failed_checks
                    .iter()
                    .map(|c| super::PreflightBlockerView {
                        kind: c.kind.clone(),
                        message: c.message.clone(),
                        plugin_id: c.plugin_id.clone(),
                        action: c.action.clone(),
                        step_ids: c.step_ids.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Some(super::RunStatusBanner {
        status: run.status.clone(),
        headline,
        detail,
        primary_link,
        primary_link_label,
        stderr_link,
        cli_hint,
        blocked_gate_ids,
        preflight_blockers,
    })
}

fn build_operator_summary(
    run: &RunDetail,
    blocked_gate_context: Option<&BlockedGateContext>,
) -> OperatorSummary {
    let blocked_gate_count = run
        .blocked_gates
        .len()
        .max(usize::from(blocked_gate_context.is_some()));
    let action_required = run.status == "blocked" && blocked_gate_count > 0;
    let expected_user_action = blocked_gate_context
        .map(|ctx| {
            format!(
                "Review gate '{}' for step '{}' in run '{}'.",
                ctx.gate.gate_id, ctx.gate.step_id, ctx.owning_run_id
            )
        })
        .or_else(|| {
            run.blocked_gates
                .first()
                .map(|g| format!("Review gate '{}' for step '{}'.", g.gate_id, g.step_id))
        });
    let recommended_next_action = if action_required {
        if let Some(ctx) = blocked_gate_context {
            format!(
                "Open run '{}' step '{}' and decide gate '{}'.",
                ctx.owning_run_id, ctx.gate.step_id, ctx.gate.gate_id
            )
        } else if let Some(gate) = run.blocked_gates.first() {
            format!(
                "Open the gated step '{}' and decide gate '{}'.",
                gate.step_id, gate.gate_id
            )
        } else {
            "Open the Gates tab to review blocked manual gates.".to_string()
        }
    } else {
        match run.status.as_str() {
            "running" | "waiting" => "Monitor current step progress in the Steps tab.".to_string(),
            "failed" => "Open the failed step and inspect error details.".to_string(),
            "success" => "Review artifacts/evidence and run outputs.".to_string(),
            _ => "Review run tabs for details.".to_string(),
        }
    };

    OperatorSummary {
        action_required,
        current_step_id: run.current_step.as_ref().map(|s| s.step_id.clone()),
        blocked_gate_count,
        expected_user_action,
        recommended_next_action,
    }
}

/// Extracts a short human-readable prompt/reason from a gate request summary
/// JSON. Looks at common keys (`prompt`, `message`, `question`, `reason`,
/// `description`, `summary`) and falls back to `None` when none of them carry
/// usable text. The returned string is trimmed and capped at ~200 chars so it
/// fits comfortably in the banner.
fn extract_gate_prompt(value: &serde_json::Value) -> Option<String> {
    fn pluck(v: &serde_json::Value) -> Option<&str> {
        const KEYS: &[&str] = &[
            "prompt",
            "message",
            "question",
            "reason",
            "description",
            "summary",
            "title",
        ];
        let obj = v.as_object()?;
        for k in KEYS {
            if let Some(s) = obj.get(*k).and_then(|x| x.as_str()) {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
        None
    }
    let raw = pluck(value)?;
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(if collapsed.chars().count() > 200 {
        let truncated: String = collapsed.chars().take(197).collect();
        format!("{truncated}…")
    } else {
        collapsed
    })
}

fn build_gate_guidance(value: &serde_json::Value) -> GateGuidanceView {
    fn field(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
        let obj = value.as_object()?;
        for key in keys {
            if let Some(text) = obj.get(*key).and_then(|v| v.as_str()) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    fn list(value: &serde_json::Value, keys: &[&str]) -> Vec<String> {
        let Some(obj) = value.as_object() else {
            return Vec::new();
        };
        for key in keys {
            if let Some(items) = obj.get(*key).and_then(|v| v.as_array()) {
                let out: Vec<String> = items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::trim))
                    .filter(|item| !item.is_empty())
                    .map(ToString::to_string)
                    .collect();
                if !out.is_empty() {
                    return out;
                }
            }
        }
        Vec::new()
    }

    GateGuidanceView {
        prompt: field(value, &["prompt", "question", "message"]),
        reason: field(value, &["reason", "description"]),
        expected_response: field(value, &["expected_response", "expected", "instruction"]),
        options: list(value, &["options", "choices", "decision_options"]),
        summary: field(value, &["summary", "title"]),
    }
}

fn tab_links(run_id: &str, active: &str) -> Vec<TabLink> {
    let entries = [
        ("overview", "Overview"),
        ("steps", "Steps"),
        ("events", "Events"),
        ("gates", "Gates"),
        ("failures", "Failures"),
        ("artifacts", "Artifacts"),
        ("evidence", "Evidence"),
        ("workflow-files", "Files"),
        ("children", "Children"),
    ];
    entries
        .iter()
        .map(|(k, l)| TabLink {
            key: k,
            label: l,
            href: format!("/inspect/runs/{run_id}?tab={k}"),
            active: *k == active,
        })
        .collect()
}

fn workflow_tab_links(workflow_ref: &str, active: &str) -> Vec<TabLink> {
    let entries = [
        ("plugins", "Plugins"),
        ("inputs", "Inputs"),
        ("steps", "Steps"),
    ];
    entries
        .iter()
        .map(|(k, l)| TabLink {
            key: k,
            label: l,
            href: format!("/inspect/workflows/{workflow_ref}?tab={k}"),
            active: *k == active,
        })
        .collect()
}

fn workflow_plugin_rows(steps: &[WorkflowStepRow]) -> Vec<WorkflowPluginRow> {
    let mut by_plugin =
        BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>, Vec<String>)>::new();

    for step in steps {
        let Some(plugin) = step.plugin.as_ref() else {
            continue;
        };
        let entry = by_plugin.entry(plugin.clone()).or_default();
        if let Some(adapter) = step.adapter.as_ref() {
            entry.0.insert(adapter.clone());
        }
        if let Some(action) = step.action.as_ref() {
            entry.1.insert(action.clone());
        }
        entry.2.push(step.step_id.clone());
    }

    by_plugin
        .into_iter()
        .map(
            |(plugin, (adapters, actions, step_ids))| WorkflowPluginRow {
                plugin,
                adapters: adapters.into_iter().collect(),
                actions: actions.into_iter().collect(),
                step_ids,
            },
        )
        .collect()
}

fn workflow_step_groups(steps: Vec<WorkflowStepRow>) -> Vec<WorkflowStepGroup> {
    let steps_by_id = steps
        .iter()
        .cloned()
        .map(|step| (step.step_id.clone(), step))
        .collect::<BTreeMap<_, _>>();
    let mut children_by_parent = BTreeMap::<String, Vec<String>>::new();
    let mut root_step_ids = Vec::new();

    for step in &steps {
        match step.parent_step_id.as_deref() {
            Some(parent_step_id) if steps_by_id.contains_key(parent_step_id) => {
                children_by_parent
                    .entry(parent_step_id.to_string())
                    .or_default()
                    .push(step.step_id.clone());
            }
            _ => root_step_ids.push(step.step_id.clone()),
        }
    }

    root_step_ids
        .into_iter()
        .filter_map(|root_step_id| {
            let mut root = steps_by_id.get(&root_step_id)?.clone();
            root.depth = 0;
            Some(WorkflowStepGroup {
                descendants: collect_workflow_descendants(
                    &root_step_id,
                    1,
                    &steps_by_id,
                    &children_by_parent,
                ),
                root,
            })
        })
        .collect()
}

fn collect_workflow_descendants(
    step_id: &str,
    depth: usize,
    steps_by_id: &BTreeMap<String, WorkflowStepRow>,
    children_by_parent: &BTreeMap<String, Vec<String>>,
) -> Vec<WorkflowStepRow> {
    let mut rows = Vec::new();

    if let Some(child_step_ids) = children_by_parent.get(step_id) {
        for child_step_id in child_step_ids {
            let Some(step) = steps_by_id.get(child_step_id) else {
                continue;
            };
            let mut step = step.clone();
            step.depth = depth;
            rows.push(step);
            rows.extend(collect_workflow_descendants(
                child_step_id,
                depth + 1,
                steps_by_id,
                children_by_parent,
            ));
        }
    }

    rows
}

fn build_overview(run: &RunDetail, metrics: Option<&transist::RunMetricsSummary>) -> OverviewView {
    let workflow_inputs_json =
        pretty_json(&run.workflow_input_summary).unwrap_or_else(|_| "{}".to_string());
    let step_counts_json = pretty_json(&run.step_counts).unwrap_or_else(|_| "{}".to_string());
    let cleanup_state = run
        .cleanup
        .as_ref()
        .map(|c| format!("trigger={}, state={}", c.trigger, c.state));
    let metrics_view = metrics.map(|m| OverviewMetrics {
        step_executions: m.total_step_executions,
        artifacts: m.total_artifacts_produced,
        evidence_files: m.total_evidence_files_produced,
        input_tokens: m
            .input_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_string()),
        output_tokens: m
            .output_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_string()),
        total_tokens: m
            .total_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_string()),
        duration_label: format_overview_duration(m),
    });
    OverviewView {
        metrics: metrics_view,
        recovery_state: run.recovery_state.clone(),
        cleanup_state,
        workflow_inputs_json,
        step_counts_json,
        trust: OverviewTrustView {
            trusted_execution_mode: run.trust.trusted_execution_mode,
            workflow_label: run
                .trust
                .workflow
                .as_ref()
                .map(|workflow| format!("{}@{}", workflow.id, workflow.version)),
            workflow_digest: run
                .trust
                .workflow
                .as_ref()
                .map(|workflow| workflow.digest.clone()),
            workflow_signature_status: run
                .trust
                .workflow
                .as_ref()
                .map(|workflow| workflow.signature_status.clone()),
            workflow_signer_key_id: run
                .trust
                .workflow
                .as_ref()
                .and_then(|workflow| workflow.signer_key_id.clone()),
            decision_outcome: run
                .trust
                .decision
                .as_ref()
                .map(|decision| decision.outcome.clone()),
            decision_reason: run
                .trust
                .decision
                .as_ref()
                .and_then(|decision| decision.reason.clone()),
            policy_mode: run
                .trust
                .policy_decision
                .as_ref()
                .map(|policy| policy.mode.clone()),
            policy_reason_code: run
                .trust
                .policy_decision
                .as_ref()
                .and_then(|policy| policy.reason_code.clone()),
            plugins: run
                .trust
                .plugins
                .iter()
                .map(|plugin| OverviewTrustPluginRow {
                    id: plugin.id.clone(),
                    version: plugin.version.clone(),
                    digest: plugin.digest.clone(),
                    signature_status: plugin.signature_status.clone(),
                    signer_key_id: plugin.signer_key_id.clone(),
                })
                .collect(),
        },
        locality: OverviewLocalityView {
            steps: run
                .locality
                .steps
                .iter()
                .map(|step| OverviewLocalityStepRow {
                    step_id: step.step_id.clone(),
                    provider: step.provider.clone().unwrap_or_else(|| "—".to_string()),
                    model: step.model.clone().unwrap_or_else(|| "—".to_string()),
                    provider_kind: step
                        .provider_kind
                        .clone()
                        .unwrap_or_else(|| "—".to_string()),
                })
                .collect(),
            external_transfers: run
                .locality
                .external_transfers
                .iter()
                .map(|transfer| OverviewExternalTransferRow {
                    step_id: transfer.step_id.clone(),
                    timestamp: transfer.timestamp.clone(),
                    destination: transfer
                        .destination
                        .clone()
                        .unwrap_or_else(|| "—".to_string()),
                    provider: transfer.provider.clone().unwrap_or_else(|| "—".to_string()),
                    provider_kind: transfer.provider_kind.clone(),
                    policy_mode: transfer.policy_mode.clone(),
                })
                .collect(),
        },
        evidence_export: OverviewEvidenceExportView {
            location: run.evidence_export.location.clone(),
            command: run.evidence_export.command.clone(),
            format: run.evidence_export.format.clone(),
            generated_at: run.evidence_export.generated_at.clone(),
        },
    }
}

fn artifact_to_row(run_id: &str, a: ArtifactMetadata) -> BlobRow {
    BlobRow {
        detail_url: format!("/inspect/runs/{run_id}/artifacts/{}", a.artifact_id),
        blob_id: a.artifact_id,
        name: a.name,
        size: a.size,
        media_type: a.media_type.unwrap_or_default(),
        producing_step_id: a.producing_step_id,
        execution: a.execution,
    }
}

fn evidence_to_row(run_id: &str, a: EvidenceMetadata) -> BlobRow {
    BlobRow {
        detail_url: format!("/inspect/runs/{run_id}/evidence/{}", a.evidence_id),
        blob_id: a.evidence_id,
        name: a.name,
        size: a.size,
        media_type: a.media_type.unwrap_or_default(),
        producing_step_id: a.producing_step_id,
        execution: a.execution,
    }
}

fn failure_to_row(failure: transist::FailureRecord) -> FailureRow {
    FailureRow {
        failure_id: failure.failure_id,
        step_id: failure.step_id,
        execution: failure.execution_attempt,
        failure_class: failure.failure_class.as_str().to_string(),
        error_code: failure.error_code,
        message_redacted: failure.message_redacted,
        adapter_kind: failure.adapter_kind,
        adapter_name: failure.adapter_name.unwrap_or_else(|| "—".to_string()),
        tool_name: failure.tool_name.unwrap_or_else(|| "—".to_string()),
        retryable: failure.retryable,
        safe_to_retry: failure.safe_to_retry,
        requires_manual_attention: failure.requires_manual_attention,
        state_mutation_detected: failure.state_mutation_detected,
        first_seen_at: crate::application::format_timestamp(Some(failure.first_seen_at_unix_ms))
            .unwrap_or_else(|| failure.first_seen_at_unix_ms.to_string()),
        last_seen_at: crate::application::format_timestamp(Some(failure.last_seen_at_unix_ms))
            .unwrap_or_else(|| failure.last_seen_at_unix_ms.to_string()),
        occurrence_count: failure.occurrence_count,
    }
}

fn pretty_json(value: &serde_json::Value) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value)
}

fn short_json(value: &serde_json::Value, max: usize) -> String {
    let s = value.to_string();
    if s.len() > max {
        format!("{}…", &s[..max.min(s.len())])
    } else {
        s
    }
}

// ---------------- Step detail ----------------

pub(crate) async fn step_detail(
    State(state): State<Arc<AppState>>,
    Path((run_id, step_id)): Path<(String, String)>,
    params: Result<Query<StepDetailParams>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let params = match params {
        Ok(Query(params)) => params,
        Err(_) => {
            return render_api_problem(ApiProblem::invalid_request("invalid step output query"));
        }
    };
    let selection = match crate::infrastructure::http_server::params::parse_output_execution(
        params.execution.as_deref(),
    ) {
        Ok(selection) => selection,
        Err(problem) => return render_api_problem(problem),
    };
    let step = match state
        .inspection
        .get_step_detail(&run_id, &step_id, selection)
    {
        Ok(s) => s,
        Err(problem) => return render_api_problem(problem),
    };
    let metrics = state
        .metrics
        .get_step_metrics(&run_id, &step_id)
        .ok()
        .flatten();

    let iteration_scope = parse_iteration_scope(params.iteration.as_deref());

    let workflow_files = step
        .output_summary
        .workflow_files
        .iter()
        .map(|w| WorkflowFileRow {
            detail_url: w
                .workflow_file_id
                .as_ref()
                .map(|id| format!("/inspect/runs/{run_id}/workflow-files/{id}")),
            workflow_file_id: w.workflow_file_id.clone(),
            field_name: w.field_name.clone(),
            name: w.name.clone(),
            path: w.relative_path.clone(),
            size: w.size,
            media_type: w.media_type.clone().unwrap_or_default(),
            producing_step_id: step.step_id.clone(),
            execution: step.execution.unwrap_or(0),
        })
        .collect();

    let all_artifacts = match state
        .inspection
        .list_artifacts(&run_id, transist::BlobListScope::SelfOnly)
    {
        Ok(page) => page
            .data
            .into_iter()
            .filter(|artifact| artifact.producing_step_id == step.step_id)
            .map(|artifact| artifact_to_row(&run_id, artifact))
            .collect::<Vec<_>>(),
        Err(problem) => return render_api_problem(problem),
    };

    let all_evidence = match state
        .inspection
        .list_evidence(&run_id, transist::BlobListScope::SelfOnly)
    {
        Ok(page) => page
            .data
            .into_iter()
            .filter(|evidence| evidence.producing_step_id == step.step_id)
            .map(|evidence| evidence_to_row(&run_id, evidence))
            .collect::<Vec<_>>(),
        Err(problem) => return render_api_problem(problem),
    };

    let mut available_iterations: Vec<u32> = all_artifacts
        .iter()
        .map(|row| row.execution)
        .chain(all_evidence.iter().map(|row| row.execution))
        .collect();
    available_iterations.sort_unstable();
    available_iterations.dedup();

    let latest_execution = available_iterations
        .iter()
        .copied()
        .max()
        .or(step.execution)
        .unwrap_or(0);

    let artifacts = filter_blob_rows_by_iteration(all_artifacts, iteration_scope, latest_execution);
    let evidence = filter_blob_rows_by_iteration(all_evidence, iteration_scope, latest_execution);

    let selected_iteration = match iteration_scope {
        IterationScope::Specific(iteration) => Some(iteration),
        _ => None,
    };

    let metrics_json = match metrics {
        Some(m) => serde_json::to_string_pretty(&m).unwrap_or_else(|_| "{}".to_string()),
        None => "(no metrics)".to_string(),
    };
    let input_summary_json =
        serde_json::to_string_pretty(&step.input_summary).unwrap_or_else(|_| "{}".to_string());
    let output_summary_json =
        serde_json::to_string_pretty(&step.output_summary).unwrap_or_else(|_| "{}".to_string());

    // Surface typed fields from the step's input/output summary into a
    // structured kv grid so users don't have to expand raw JSON to see the
    // most useful values (especially `child_run_id` for subflow/loop steps).
    let mut summary_fields: Vec<StepSummaryField> = Vec::new();
    if let Some(invoke_id) = step
        .output_summary
        .invoke_id
        .as_ref()
        .or(step.input_summary.invoke_id.as_ref())
    {
        summary_fields.push(StepSummaryField {
            label: "Invoke ID",
            value: invoke_id.clone(),
            mono: true,
        });
    }
    if let Some(adapter_kind) = &step.input_summary.adapter_kind {
        summary_fields.push(StepSummaryField {
            label: "Adapter",
            value: adapter_kind.clone(),
            mono: true,
        });
    }
    if let Some(plugin_id) = &step.input_summary.plugin_id {
        summary_fields.push(StepSummaryField {
            label: "Plugin",
            value: plugin_id.clone(),
            mono: true,
        });
    }
    if let Some(action) = &step.input_summary.action {
        summary_fields.push(StepSummaryField {
            label: "Action",
            value: action.clone(),
            mono: true,
        });
    }
    if let Some(status) = &step.output_summary.status {
        summary_fields.push(StepSummaryField {
            label: "Result status",
            value: status.clone(),
            mono: true,
        });
    }
    if let Some(exit_code) = step.output_summary.exit_code {
        summary_fields.push(StepSummaryField {
            label: "Exit code",
            value: exit_code.to_string(),
            mono: true,
        });
    }
    // Surface CLI-parity failure context (Plugin/Error code/Error) inline in
    // the top kv-grid when the step failed, so users don't have to expand the
    // raw output summary JSON to see why. Mirrors
    // `build_failed_step_detail_lines` in the CLI TUI run-detail view.
    let step_failed =
        step.status == "failed" || step.output_summary.status.as_deref() == Some("failed");
    if step_failed {
        // Durable child_failure attribution (added when the step is a failed
        // subflow parent) is written by the kernel. Prefer it here so the
        // reason and pointer surface without recomputation. Falls back to the
        // step's own error fields for leaf steps or pre-durable runs.
        if let Some(child_failure) = step.output_summary.child_failure.as_ref() {
            if let Some(failed_step_id) = child_failure.failed_step_id.as_deref() {
                summary_fields.push(StepSummaryField {
                    label: "Failed child step",
                    value: failed_step_id.to_string(),
                    mono: true,
                });
            }
            summary_fields.push(StepSummaryField {
                label: "Child run",
                value: child_failure.child_run_id.clone(),
                mono: true,
            });
            if let Some(error_code) = child_failure.error_code.as_deref() {
                summary_fields.push(StepSummaryField {
                    label: "Error code",
                    value: error_code.to_string(),
                    mono: true,
                });
            }
            if let Some(error_message) = child_failure.error_message.as_deref() {
                summary_fields.push(StepSummaryField {
                    label: "Error",
                    value: error_message.to_string(),
                    mono: false,
                });
            }
            if let Some(stderr) = child_failure.stderr.as_deref() {
                summary_fields.push(StepSummaryField {
                    label: "Stderr",
                    value: stderr.to_string(),
                    mono: false,
                });
            }
        } else {
            if let Some(error_code) = &step.output_summary.error_code {
                summary_fields.push(StepSummaryField {
                    label: "Error code",
                    value: error_code.clone(),
                    mono: true,
                });
            }
            if let Some(error_message) = &step.output_summary.error_message {
                summary_fields.push(StepSummaryField {
                    label: "Error",
                    value: error_message.clone(),
                    mono: false,
                });
            }
        }
    }
    summary_fields.push(StepSummaryField {
        label: "Request present",
        value: if step.input_summary.request_present {
            "yes".to_string()
        } else {
            "no".to_string()
        },
        mono: false,
    });
    summary_fields.push(StepSummaryField {
        label: "Result present",
        value: if step.output_summary.result_present {
            "yes".to_string()
        } else {
            "no".to_string()
        },
        mono: false,
    });
    if step.is_current_active {
        summary_fields.push(StepSummaryField {
            label: "Active",
            value: "current active step".to_string(),
            mono: false,
        });
    }

    let mut key_groups: Vec<StepKeyGroup> = Vec::new();
    if !step.input_summary.input_object_keys.is_empty() {
        key_groups.push(StepKeyGroup {
            label: "Input keys",
            keys: step.input_summary.input_object_keys.clone(),
        });
    }
    if !step.input_summary.metadata_object_keys.is_empty() {
        key_groups.push(StepKeyGroup {
            label: "Metadata keys",
            keys: step.input_summary.metadata_object_keys.clone(),
        });
    }
    if !step.output_summary.output_object_keys.is_empty() {
        key_groups.push(StepKeyGroup {
            label: "Output keys",
            keys: step.output_summary.output_object_keys.clone(),
        });
    }

    let child_run_id = step.output_summary.child_run_id.clone();
    let child_run_url = child_run_id
        .as_ref()
        .map(|cid| format!("/inspect/runs/{cid}"));

    // If a child run exists, fetch its current state and build a lightweight
    // inline overview so the user can see it without navigating away.
    // Failures here are non-fatal: the child run may not yet exist if the
    // subflow step only just started, so we silently skip the overview.
    let child_run_overview = child_run_id.as_deref().and_then(|cid| {
        let child = state.inspection.get_run(cid).ok()?;
        let metrics_view = child.metrics.as_ref().map(|m| OverviewMetrics {
            step_executions: m.total_step_executions,
            artifacts: m.total_artifacts_produced,
            evidence_files: m.total_evidence_files_produced,
            input_tokens: m
                .input_tokens
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".to_string()),
            output_tokens: m
                .output_tokens
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".to_string()),
            total_tokens: m
                .total_tokens
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".to_string()),
            duration_label: format_overview_duration(m),
        });
        let step_counts_json = serde_json::to_string_pretty(&child.step_counts).unwrap_or_default();
        Some(ChildRunOverview {
            run_url: format!("/inspect/runs/{cid}"),
            run_id: cid.to_string(),
            workflow_label: format!("{}@{}", child.workflow_id, child.workflow_version),
            status: child.status.clone(),
            started_at: child.started_at.clone(),
            finished_at: child.finished_at.clone(),
            artifact_count: child.artifact_summary.count,
            evidence_count: child.evidence_summary.count,
            step_counts_json,
            metrics: metrics_view,
        })
    });

    let run_detail = state.inspection.get_run(&run_id).ok();

    let gate_guidance = run_detail
        .as_ref()
        .and_then(|run| {
            run.blocked_gates
                .iter()
                .find(|gate| gate.step_id == step.step_id && gate.status == "blocked")
        })
        .map(|gate| build_gate_guidance(&gate.request_summary));

    // When the run failed during preflight, surface any failed checks that
    // name this step so operators see the blocker without bouncing back to
    // the run page.
    let preflight_blockers: Vec<super::PreflightBlockerView> = run_detail
        .as_ref()
        .and_then(|run| run.preflight.as_ref())
        .map(|p| {
            p.failed_checks
                .iter()
                .filter(|c| c.step_ids.iter().any(|s| s == &step.step_id))
                .map(|c| super::PreflightBlockerView {
                    kind: c.kind.clone(),
                    message: c.message.clone(),
                    plugin_id: c.plugin_id.clone(),
                    action: c.action.clone(),
                    step_ids: c.step_ids.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    let output_selector = params.execution.unwrap_or_else(|| "latest".to_string());
    let mut output_options = vec![StatusOption {
        value: "latest".to_string(),
        selected: matches!(selection, crate::application::OutputExecution::Latest),
    }];
    output_options.extend(step.output_executions.iter().map(|ordinal| {
        let selected = matches!(selection, crate::application::OutputExecution::Ordinal(selected) if selected == *ordinal);
        StatusOption {
            // Keep accepted leading zeros in both submissions and JSON links.
            value: if selected {
                output_selector.clone()
            } else {
                ordinal.to_string()
            },
            selected,
        }
    }));
    let output_json_url = format!(
        "/api/v1/runs/{}/steps/{}?execution={output_selector}",
        url_encode(&run_id),
        url_encode(&step_id),
    );

    render(&StepDetailTemplate {
        data: StepDetailTemplateData {
            output: super::step_outputs::present_output(step.output.value.as_ref()),
            output_execution: step.output.execution,
            output_status: step.output.status.clone(),
            output_source: step.output.source.clone(),
            output_result_present: step.output.result_present,
            output_options,
            output_selector,
            output_json_url,
            iteration_value: params
                .iteration
                .clone()
                .unwrap_or_else(|| "last".to_string()),
            run_id: step.run_id.clone(),
            step_id: step.step_id.clone(),
            step_kind: step.step_kind.clone(),
            status: step.status.clone(),
            execution: step.execution,
            started_at: step.started_at.clone(),
            finished_at: step.finished_at.clone(),
            is_current_active: step.is_current_active,
            summary_fields,
            key_groups,
            child_run_id,
            child_run_url,
            child_run_overview,
            gate_guidance,
            preflight_blockers,
            input_summary_json,
            output_summary_json,
            metrics_json,
            iteration_scope: iteration_scope.key(),
            selected_iteration,
            available_iterations,
            artifacts,
            evidence,
            workflow_files,
        },
    })
}

// ---------------- Blob detail (artifact / evidence / workflow file) ----------------

pub(crate) async fn artifact_detail(
    State(state): State<Arc<AppState>>,
    Path((run_id, artifact_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_artifact_id(&artifact_id) {
        Ok(l) => l,
        Err(problem) => return render_api_problem(problem),
    };
    let metadata = match state.inspection.get_artifact_metadata(&run_id, &locator) {
        Ok(m) => m,
        Err(problem) => return render_api_problem(problem),
    };
    let content_url = format!(
        "/api/v1/runs/{run_id}/artifacts/{}/content",
        metadata.artifact_id
    );
    let content = if metadata.size <= PREVIEW_LIMIT_BYTES {
        state
            .inspection
            .get_artifact_content(&run_id, &locator)
            .ok()
            .map(|c| c.bytes)
    } else {
        None
    };
    render_blob(
        run_id,
        BlobKind::Artifact,
        metadata.artifact_id,
        metadata.name,
        metadata.size,
        metadata.media_type,
        Some(metadata.producing_step_id),
        Some(metadata.execution),
        None,
        None,
        content_url,
        content,
    )
}

pub(crate) async fn evidence_detail(
    State(state): State<Arc<AppState>>,
    Path((run_id, evidence_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_evidence_id(&evidence_id) {
        Ok(l) => l,
        Err(problem) => return render_api_problem(problem),
    };
    let metadata = match state.inspection.get_evidence_metadata(&run_id, &locator) {
        Ok(m) => m,
        Err(problem) => return render_api_problem(problem),
    };
    let content_url = format!(
        "/api/v1/runs/{run_id}/evidence/{}/content",
        metadata.evidence_id
    );
    let content = if metadata.size <= PREVIEW_LIMIT_BYTES {
        state
            .inspection
            .get_evidence_content(&run_id, &locator)
            .ok()
            .map(|c| c.bytes)
    } else {
        None
    };
    render_blob(
        run_id,
        BlobKind::Evidence,
        metadata.evidence_id,
        metadata.name,
        metadata.size,
        metadata.media_type,
        Some(metadata.producing_step_id),
        Some(metadata.execution),
        None,
        None,
        content_url,
        content,
    )
}

pub(crate) async fn workflow_file_detail(
    State(state): State<Arc<AppState>>,
    Path((run_id, workflow_file_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_workflow_file_id(&workflow_file_id) {
        Ok(l) => l,
        Err(problem) => return render_api_problem(problem),
    };
    let metadata: WorkflowFileMetadata = match state
        .inspection
        .get_workflow_file_metadata(&run_id, &locator)
    {
        Ok(m) => m,
        Err(problem) => return render_api_problem(problem),
    };
    let content_url = metadata.links.content.clone().unwrap_or_default();
    let content = if metadata.size <= PREVIEW_LIMIT_BYTES {
        state
            .inspection
            .get_workflow_file_content(&run_id, &locator)
            .ok()
            .map(|c| c.bytes)
    } else {
        None
    };
    render_blob(
        run_id,
        BlobKind::WorkflowFile,
        metadata.workflow_file_id,
        metadata.name,
        metadata.size,
        metadata.media_type,
        Some(metadata.producing_step_id),
        None,
        Some(metadata.field_name),
        Some(metadata.relative_path),
        content_url,
        content,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_blob(
    run_id: String,
    kind: BlobKind,
    blob_id: String,
    name: String,
    size: u64,
    media_type: Option<String>,
    producing_step_id: Option<String>,
    execution: Option<u32>,
    field_name: Option<String>,
    path: Option<String>,
    content_url: String,
    content: Option<Vec<u8>>,
) -> Response {
    let media_type_str = media_type.clone().unwrap_or_default();
    let preview_too_large = size > PREVIEW_LIMIT_BYTES;
    let mut preview_text: Option<String> = None;
    let mut preview_image = false;
    let mut preview_html = false;
    if !preview_too_large {
        if media_type_str.starts_with("image/") {
            preview_image = true;
        } else if media_type_str == "text/html" {
            preview_html = true;
        } else if media_type_str.starts_with("text/")
            || media_type_str == "application/json"
            || media_type_str.is_empty()
        {
            if let Some(bytes) = content {
                if let Ok(s) = String::from_utf8(bytes) {
                    preview_text = Some(s);
                }
            }
        }
    }
    let kind_label = kind.label();
    render(&BlobDetailTemplate {
        data: BlobDetailTemplateData {
            run_id,
            kind_label,
            blob_id,
            name,
            size,
            media_type: media_type_str,
            producing_step_id,
            execution,
            field_name,
            path,
            content_url,
            preview_text,
            preview_image,
            preview_html,
            preview_too_large,
        },
    })
}

// ---------------- Workflows ----------------

pub(crate) async fn workflows_list(State(state): State<Arc<AppState>>) -> Response {
    let payload = match state
        .inspection
        .list_workflows(&transist::PaginationQuery::default())
    {
        Ok(p) => p,
        Err(problem) => return render_api_problem(problem),
    };
    let workflows = payload
        .data
        .into_iter()
        .map(|w| WorkflowsListRow {
            workflow_ref: w.workflow_ref,
            workflow_id: w.workflow_id,
            workflow_version: w.workflow_version,
            title: w.title,
            source_ref: w.source_ref,
            step_count: w.step_count,
            root_step_count: w.root_step_count,
            runnable: w.runnable_in_principle,
            compiled_recipe_available: w.compiled_recipe_available,
        })
        .collect();
    render(&WorkflowsListTemplate { workflows })
}

pub(crate) async fn workflow_detail(
    State(state): State<Arc<AppState>>,
    Path(workflow_ref): Path<String>,
    Query(params): Query<WorkflowDetailParams>,
) -> Response {
    if let Err(problem) = ensure_valid_workflow_ref(&workflow_ref) {
        return render_api_problem(problem);
    }
    let tab = params.tab.as_deref().unwrap_or("steps").to_string();
    if !matches!(tab.as_str(), "plugins" | "inputs" | "steps") {
        return render_error(StatusCode::NOT_FOUND, "invalid_tab", "unknown tab");
    }
    let detail = match state.inspection.get_workflow(&workflow_ref) {
        Ok(d) => d,
        Err(problem) => return render_api_problem(problem),
    };
    let tabs = workflow_tab_links(&workflow_ref, &tab);
    let inputs = detail
        .input_declarations
        .into_iter()
        .map(|i| WorkflowInputRow {
            name: i.name,
            value_type: i.value_type,
            required: i.required,
            has_default: i.has_default,
            description: i.description.unwrap_or_default(),
        })
        .collect();
    let step_rows = detail
        .step_graph_summary
        .into_iter()
        .map(|s| WorkflowStepRow {
            step_id: s.step_id,
            parent_step_id: s.parent_step_id,
            child_step_ids: s.child_step_ids,
            kind: s.kind,
            adapter: s.adapter,
            plugin: s.plugin,
            action: s.action,
            if_guard: s.if_guard,
            loop_while: s.loop_while,
            loop_until: s.loop_until,
            workflow_input_refs: s.workflow_input_refs,
            edges: s
                .graph_edges
                .into_iter()
                .map(|e| format!("{}→{}", e.label, e.target_step_id))
                .collect(),
            depth: 0,
        })
        .collect::<Vec<_>>();
    let plugins = workflow_plugin_rows(&step_rows);
    let step_groups = workflow_step_groups(step_rows);
    render(&WorkflowDetailTemplate {
        workflow_ref: detail.workflow_ref.clone(),
        workflow_id: detail.workflow_id,
        workflow_version: detail.workflow_version,
        title: detail.title,
        source_ref: detail.source_ref,
        validation_status: detail.validation.status,
        validation_errors: detail.validation.errors,
        runnable: detail.runnable_in_principle,
        compiled_recipe_available: detail.compiled_recipe_available,
        tab,
        tabs,
        plugins,
        inputs,
        step_groups,
        source_url: format!("/inspect/workflows/{workflow_ref}/source"),
        compiled_url: format!("/inspect/workflows/{workflow_ref}/compiled"),
    })
}

pub(crate) async fn workflow_source(
    State(state): State<Arc<AppState>>,
    Path(workflow_ref): Path<String>,
) -> Response {
    if let Err(problem) = ensure_valid_workflow_ref(&workflow_ref) {
        return render_api_problem(problem);
    }
    let result = state.inspection.get_workflow_source(&workflow_ref);
    render_workflow_content(&workflow_ref, "Source", result, "compiled")
}

pub(crate) async fn workflow_compiled(
    State(state): State<Arc<AppState>>,
    Path(workflow_ref): Path<String>,
) -> Response {
    if let Err(problem) = ensure_valid_workflow_ref(&workflow_ref) {
        return render_api_problem(problem);
    }
    let result = state.inspection.get_workflow_compiled_recipe(&workflow_ref);
    render_workflow_content(&workflow_ref, "Compiled recipe", result, "source")
}

fn render_workflow_content(
    workflow_ref: &str,
    kind_label: &'static str,
    result: Result<crate::application::InlineContentResponse, ApiProblem>,
    other: &'static str,
) -> Response {
    let (other_url, other_label) = if other == "source" {
        (
            format!("/inspect/workflows/{workflow_ref}/source"),
            "source",
        )
    } else {
        (
            format!("/inspect/workflows/{workflow_ref}/compiled"),
            "compiled recipe",
        )
    };
    let back_url = format!("/inspect/workflows/{workflow_ref}");
    match result {
        Ok(content) => {
            let text =
                match crate::infrastructure::http_server::transport::sanitize_text(content.bytes) {
                    Ok(bytes) => String::from_utf8(bytes).expect("sanitized text remains UTF-8"),
                    Err(()) => return render_api_problem(ApiProblem::invalid_text_encoding()),
                };
            render(&WorkflowContentTemplate {
                workflow_ref: workflow_ref.to_string(),
                kind_label,
                content_type: content.content_type,
                content_text: Some(text),
                other_url,
                other_label,
                back_url,
            })
        }
        Err(problem) if problem.status == StatusCode::NOT_FOUND => {
            render(&WorkflowContentTemplate {
                workflow_ref: workflow_ref.to_string(),
                kind_label,
                content_type: "—".to_string(),
                content_text: None,
                other_url,
                other_label,
                back_url,
            })
        }
        Err(problem) => render_api_problem(problem),
    }
}
