// SPDX-License-Identifier: MPL-2.0
use super::params::{
    BlobListQueryParams, EventsQueryParams, PaginationParams, RunsQueryParams,
    ensure_valid_workflow_ref, parse_blob_list_scope, parse_events_query, parse_pagination_query,
    parse_runs_query,
};
use super::responses::{api_problem, blob_content_response, inline_content_response};
use super::router::AppState;
use super::transport::sanitized_json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

pub(crate) async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    sanitized_json(state.inspection.health())
}

pub(crate) async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    match state.metrics.render_prometheus() {
        Ok(payload) => (
            [(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            payload,
        )
            .into_response(),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn list_runs(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RunsQueryParams>,
) -> Response {
    let query = match parse_runs_query(params) {
        Ok(query) => query,
        Err(problem) => return api_problem(problem),
    };
    match (
        state.inspection.list_runs(&query),
        state.metrics.list_run_metrics(),
    ) {
        (Ok(mut payload), Ok(metrics)) => {
            for run in &mut payload.data {
                run.metrics = metrics.get(&run.run_id).cloned();
            }
            sanitized_json(payload)
        }
        (Err(problem), _) | (_, Err(problem)) => api_problem(problem),
    }
}

pub(crate) async fn get_step(
    State(state): State<Arc<AppState>>,
    Path((run_id, step_id)): Path<(String, String)>,
    params: Result<
        Query<super::params::StepOutputParams>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Response {
    let selection = match params {
        Ok(Query(params)) => super::params::parse_output_execution(params.execution.as_deref()),
        Err(_) => Err(crate::application::ApiProblem::invalid_request(
            "invalid step output query",
        )),
    };
    let selection = match selection {
        Ok(selection) => selection,
        Err(problem) => return api_problem(problem),
    };
    match (
        state
            .inspection
            .get_step_detail(&run_id, &step_id, selection),
        state.metrics.get_step_metrics(&run_id, &step_id),
    ) {
        (Ok(mut step), Ok(metrics)) => {
            step.metrics = metrics;
            sanitized_json(step)
        }
        (Err(problem), _) | (_, Err(problem)) => api_problem(problem),
    }
}

pub(crate) async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Response {
    match (
        state.inspection.get_run(&run_id),
        state.metrics.get_run_metrics(&run_id),
    ) {
        (Ok(mut payload), Ok(metrics)) => {
            payload.metrics = metrics;
            sanitized_json(payload)
        }
        (Err(problem), _) | (_, Err(problem)) => api_problem(problem),
    }
}

pub(crate) async fn list_events(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Query(params): Query<EventsQueryParams>,
) -> Response {
    let query = match parse_events_query(params) {
        Ok(query) => query,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.list_events(&run_id, &query) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn list_steps(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Response {
    match state.inspection.list_steps(&run_id) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn list_gates(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Response {
    match state.inspection.list_gates(&run_id) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_run_trust(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Response {
    match state.inspection.get_run_trust(&run_id) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_run_locality(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Response {
    match state.inspection.get_run_locality(&run_id) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn list_failures(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Response {
    match state.inspection.list_failures(&run_id) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_failure(
    State(state): State<Arc<AppState>>,
    Path((run_id, failure_id)): Path<(String, String)>,
) -> Response {
    match state.inspection.get_failure(&run_id, &failure_id) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn list_artifacts(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Query(params): Query<BlobListQueryParams>,
) -> Response {
    let scope = match parse_blob_list_scope(params.scope.as_deref()) {
        Ok(scope) => scope,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.list_artifacts(&run_id, scope) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_artifact(
    State(state): State<Arc<AppState>>,
    Path((run_id, artifact_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_artifact_id(&artifact_id) {
        Ok(locator) => locator,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.get_artifact_metadata(&run_id, &locator) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_artifact_content(
    State(state): State<Arc<AppState>>,
    Path((run_id, artifact_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_artifact_id(&artifact_id) {
        Ok(locator) => locator,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.get_artifact_content(&run_id, &locator) {
        Ok(payload) => blob_content_response(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn list_evidence(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Query(params): Query<BlobListQueryParams>,
) -> Response {
    let scope = match parse_blob_list_scope(params.scope.as_deref()) {
        Ok(scope) => scope,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.list_evidence(&run_id, scope) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_evidence(
    State(state): State<Arc<AppState>>,
    Path((run_id, evidence_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_evidence_id(&evidence_id) {
        Ok(locator) => locator,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.get_evidence_metadata(&run_id, &locator) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_evidence_content(
    State(state): State<Arc<AppState>>,
    Path((run_id, evidence_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_evidence_id(&evidence_id) {
        Ok(locator) => locator,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.get_evidence_content(&run_id, &locator) {
        Ok(payload) => blob_content_response(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_workflow_file(
    State(state): State<Arc<AppState>>,
    Path((run_id, workflow_file_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_workflow_file_id(&workflow_file_id) {
        Ok(locator) => locator,
        Err(problem) => return api_problem(problem),
    };
    match state
        .inspection
        .get_workflow_file_metadata(&run_id, &locator)
    {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_workflow_file_content(
    State(state): State<Arc<AppState>>,
    Path((run_id, workflow_file_id)): Path<(String, String)>,
) -> Response {
    let locator = match crate::application::parse_workflow_file_id(&workflow_file_id) {
        Ok(locator) => locator,
        Err(problem) => return api_problem(problem),
    };
    match state
        .inspection
        .get_workflow_file_content(&run_id, &locator)
    {
        Ok(payload) => blob_content_response(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn list_workflows(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Response {
    let query = match parse_pagination_query(params) {
        Ok(query) => query,
        Err(problem) => return api_problem(problem),
    };
    match state.inspection.list_workflows(&query) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_document(
    State(state): State<Arc<AppState>>,
    Path((run_id, revision_id)): Path<(String, String)>,
) -> Response {
    match state.inspection.get_document(&run_id, &revision_id) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_document_content(
    State(state): State<Arc<AppState>>,
    Path((run_id, revision_id)): Path<(String, String)>,
    Query(params): Query<std::collections::BTreeMap<String, String>>,
) -> Response {
    if params.keys().any(|k| k != "member_id") {
        return api_problem(crate::application::ApiProblem::invalid_request(
            "only opaque member_id is accepted",
        ));
    }
    match state.inspection.get_document_content(
        &run_id,
        &revision_id,
        params.get("member_id").map(String::as_str),
    ) {
        Ok(payload) => blob_content_response(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_workflow(
    State(state): State<Arc<AppState>>,
    Path(workflow_ref): Path<String>,
) -> Response {
    if let Err(problem) = ensure_valid_workflow_ref(&workflow_ref) {
        return api_problem(problem);
    }
    match state.inspection.get_workflow(&workflow_ref) {
        Ok(payload) => sanitized_json(payload),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_workflow_source(
    State(state): State<Arc<AppState>>,
    Path(workflow_ref): Path<String>,
) -> Response {
    if let Err(problem) = ensure_valid_workflow_ref(&workflow_ref) {
        return api_problem(problem);
    }
    match state.inspection.get_workflow_source(&workflow_ref) {
        Ok(payload) => inline_content_response(payload).unwrap_or_else(api_problem),
        Err(problem) => api_problem(problem),
    }
}

pub(crate) async fn get_workflow_compiled_recipe(
    State(state): State<Arc<AppState>>,
    Path(workflow_ref): Path<String>,
) -> Response {
    if let Err(problem) = ensure_valid_workflow_ref(&workflow_ref) {
        return api_problem(problem);
    }
    match state.inspection.get_workflow_compiled_recipe(&workflow_ref) {
        Ok(payload) => inline_content_response(payload).unwrap_or_else(api_problem),
        Err(problem) => api_problem(problem),
    }
}
