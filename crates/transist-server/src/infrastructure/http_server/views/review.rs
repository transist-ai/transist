// SPDX-License-Identifier: MPL-2.0
//! Read-only gate review navigation and immutable document previews.

use super::{LinkRef, helpers::url_encode, render, render_api_problem};
use crate::application::ApiProblem;
use crate::domain::api_models::GateSummary;
use crate::infrastructure::http_server::router::AppState;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use std::collections::BTreeMap;
use std::sync::Arc;

const PREVIEW_LIMIT_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub(crate) struct ReviewPanel {
    pub gate_id: String,
    pub step: LinkRef,
    pub owning_run_id: String,
    pub execution: Option<u32>,
    pub documents: Vec<ReviewDocument>,
}

impl ReviewPanel {
    /// Builds links from the inspector-validated gate request, without resolving document heads.
    pub(super) fn new(owning_run_id: &str, gate: &GateSummary) -> Self {
        let execution = gate_execution(gate);
        let selection = execution.map_or_else(String::new, |n| format!("?execution={n}"));
        Self {
            gate_id: gate.gate_id.clone(),
            execution,
            step: LinkRef {
                label: gate.step_id.clone(),
                href: format!(
                    "/inspect/runs/{}/steps/{}{selection}#needs-review",
                    url_encode(owning_run_id),
                    url_encode(&gate.step_id),
                ),
            },
            owning_run_id: owning_run_id.into(),
            documents: gate
                .review_documents
                .iter()
                .map(|doc| ReviewDocument::new(owning_run_id, doc))
                .collect(),
        }
    }
}

pub(super) fn gate_execution(gate: &GateSummary) -> Option<u32> {
    gate.request_summary
        .pointer("/review_request/execution")?
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
}

fn workspace_files_url(run_id: &str) -> String {
    format!("/inspect/runs/{}?tab=workflow-files", url_encode(run_id))
}

#[derive(Debug)]
pub(crate) struct ReviewDocument {
    pub document: transist::InspectionDocument,
    pub preview_url: String,
}

impl ReviewDocument {
    /// Adds a browser URL pinned to this document's immutable revision.
    pub(super) fn new(run_id: &str, document: &transist::InspectionDocument) -> Self {
        Self {
            document: document.clone(),
            preview_url: preview_url(run_id, &document.revision.revision_id),
        }
    }
}

fn preview_url(run_id: &str, revision_id: &str) -> String {
    format!(
        "/inspect/runs/{}/documents/{}",
        url_encode(run_id),
        url_encode(revision_id),
    )
}

#[derive(Template)]
#[template(path = "review_document.html")]
struct DocumentPreview {
    run_id: String,
    run_url: String,
    workspace_files_url: String,
    revision_url: String,
    document: transist::InspectionDocument,
    member_links: Vec<LinkRef>,
    selected_member: Option<SelectedMember>,
    download_url: String,
    preview: Preview,
}

struct SelectedMember {
    path: String,
    sha256: String,
}

enum Preview {
    Text(String),
    NonText,
    TooLarge,
}

impl Preview {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        if bytes.len() as u64 > PREVIEW_LIMIT_BYTES {
            return Self::TooLarge;
        }
        match String::from_utf8(bytes) {
            Ok(text) if !text.chars().any(is_non_text_control) => Self::Text(text),
            _ => Self::NonText,
        }
    }
}

fn is_non_text_control(c: char) -> bool {
    c.is_control() && !matches!(c, '\t' | '\n' | '\r')
}

/// Renders an immutable document or tree member as escaped text through the same
/// run-scoped authorization and content verification used by the download API.
pub(crate) async fn document_preview(
    State(state): State<Arc<AppState>>,
    Path((run_id, revision_id)): Path<(String, String)>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Response {
    match load_preview(&state, &run_id, &revision_id, &params) {
        Ok(preview) => render(&preview),
        Err(problem) => render_api_problem(problem),
    }
}

fn load_preview(
    state: &AppState,
    run_id: &str,
    revision_id: &str,
    params: &BTreeMap<String, String>,
) -> Result<DocumentPreview, ApiProblem> {
    if params.keys().any(|key| key != "member_id") {
        return Err(ApiProblem::invalid_request(
            "only opaque member_id is accepted",
        ));
    }
    let document = state.inspection.get_document(run_id, revision_id)?;
    let member = params
        .get("member_id")
        .map(|id| {
            document
                .members
                .iter()
                .find(|member| &member.member_id == id)
                .ok_or_else(|| ApiProblem::resource_not_found("document member was not found"))
        })
        .transpose()?;
    let size = member.map_or(document.revision.blob.size_bytes, |m| m.size_bytes);
    let preview = if size > PREVIEW_LIMIT_BYTES {
        Preview::TooLarge
    } else {
        let content = state.inspection.get_document_content(
            run_id,
            revision_id,
            member.map(|m| m.member_id.as_str()),
        )?;
        Preview::from_bytes(content.bytes)
    };
    let revision_url = preview_url(run_id, revision_id);
    let member_links = document
        .members
        .iter()
        .map(|m| LinkRef {
            label: m.relative_path.clone(),
            href: format!("{revision_url}?member_id={}", url_encode(&m.member_id)),
        })
        .collect();

    Ok(DocumentPreview {
        run_id: run_id.into(),
        run_url: format!("/inspect/runs/{}#needs-review", url_encode(run_id)),
        workspace_files_url: workspace_files_url(run_id),
        revision_url,
        selected_member: member.map(|m| SelectedMember {
            path: m.relative_path.clone(),
            sha256: m.sha256.clone(),
        }),
        download_url: member.map_or_else(|| document.links.content.clone(), |m| m.content.clone()),
        document,
        member_links,
        preview,
    })
}
