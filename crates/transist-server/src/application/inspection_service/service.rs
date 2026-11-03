// SPDX-License-Identifier: MPL-2.0
use super::content::{BlobContentResponse, InlineContentResponse, map_blob_content_response};
use super::mapping::{
    map_artifact_metadata, map_evidence_metadata, map_locality_view, map_run_detail,
    map_run_summary, map_trust_summary, map_workflow_detail, map_workflow_file_metadata,
    map_workflow_summary,
};
use super::pagination::pagination;
use super::resource_ids::{
    ResourceLocator, WorkflowFileLocator, encode_artifact_id, encode_evidence_id,
    encode_workflow_file_id, resource_not_found_for_family,
};
use crate::domain::api_models::{
    ArtifactMetadata, ErrorBody, ErrorEnvelope, EventSummary, EvidenceMetadata, HealthResponse,
    ListResponse, LocalityView, RunSummary, StepSummary, TrustSummaryView, WorkflowDetail,
    WorkflowFileMetadata, WorkflowSummary,
};
use anyhow::Result;
use axum::http::StatusCode;
use std::path::PathBuf;
use std::sync::Arc;
use transist::{
    BlobFamily, BlobListScope, EventListQuery, InspectionQueryPort, MetricsQueryPort,
    PaginationQuery, RunListQuery,
};

pub(crate) const STORAGE_QUERY_FAILURE_MESSAGE: &str = "failed to load inspection data";
pub(crate) const INTERNAL_SERVER_ERROR_MESSAGE: &str = "internal server error";

fn validate_document_id(id: &str) -> Result<(), ApiProblem> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err(ApiProblem::invalid_request(
            "invalid opaque document identity",
        ));
    }
    Ok(())
}

#[derive(Clone)]
/// Read-only inspection service that maps storage/query records into API DTOs.
pub struct InspectionApiService {
    query: Arc<dyn InspectionQueryPort>,
    workspace_root: Option<PathBuf>,
}

impl InspectionApiService {
    /// Builds the service from an inspection query port implementation.
    pub fn from_query(query: Arc<dyn InspectionQueryPort>) -> Self {
        Self {
            query,
            workspace_root: None,
        }
    }

    /// Builds the service with the workspace context used for safe path labels.
    pub fn from_query_with_workspace_root(
        query: Arc<dyn InspectionQueryPort>,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            query,
            workspace_root,
        }
    }

    /// Returns the static health payload for the companion server.
    pub fn health(&self) -> HealthResponse {
        HealthResponse {
            status: "ok",
            service: "transist-server",
            mode: "read_only",
            api_version: "v1",
        }
    }

    /// Lists runs visible through the inspection query port.
    pub fn list_runs(&self, query: &RunListQuery) -> Result<ListResponse<RunSummary>, ApiProblem> {
        let page = self
            .query
            .list_runs(query)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_runs", error))?;
        Ok(ListResponse {
            pagination: pagination(page.total, page.limit, page.offset),
            data: page.data.into_iter().map(map_run_summary).collect(),
        })
    }

    /// Loads detailed run information for a single run identifier.
    pub fn get_run(
        &self,
        run_id: &str,
    ) -> Result<crate::domain::api_models::RunDetail, ApiProblem> {
        let run = self
            .query
            .get_run(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_run", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        map_run_detail(run, self.workspace_root.as_deref())
    }

    /// Lists persisted run events for a run using the requested pagination window.
    pub fn list_events(
        &self,
        run_id: &str,
        query: &EventListQuery,
    ) -> Result<ListResponse<EventSummary>, ApiProblem> {
        let page = self
            .query
            .list_events(run_id, query)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_events", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(ListResponse {
            pagination: pagination(page.total, page.limit, page.offset),
            data: page
                .data
                .into_iter()
                .map(|event| EventSummary {
                    event_id: event.event_id,
                    run_id: run_id.to_string(),
                    timestamp: super::time::format_timestamp(Some(event.timestamp_unix_ms))
                        .unwrap_or_default(),
                    event_type: event.event_type,
                    step_id: event.step_id,
                    summary: serde_json::Value::from(event.payload),
                })
                .collect(),
        })
    }

    /// Lists the step summaries currently recorded for a run.
    pub fn list_steps(&self, run_id: &str) -> Result<ListResponse<StepSummary>, ApiProblem> {
        let steps = self
            .query
            .list_steps(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_steps", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(ListResponse {
            pagination: pagination(steps.len(), steps.len(), 0),
            data: steps
                .into_iter()
                .map(|step| {
                    super::mapping::map_step_summary(run_id, step, self.workspace_root.as_deref())
                })
                .collect(),
        })
    }

    /// Maps compact summaries and authoritative latest outputs from one run's
    /// step-list query for internal HTML previews, without detail queries or writes.
    pub(crate) fn list_steps_with_latest_outputs(
        &self,
        run_id: &str,
    ) -> Result<Vec<(StepSummary, crate::domain::api_models::StepOutput)>, ApiProblem> {
        let steps = self
            .query
            .list_steps(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_steps", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        steps
            .into_iter()
            .map(|step| {
                let (output, _) =
                    super::outputs::select_output(&step, super::OutputExecution::Latest)?;
                let summary =
                    super::mapping::map_step_summary(run_id, step, self.workspace_root.as_deref());
                Ok((summary, output))
            })
            .collect()
    }

    /// Loads a single step summary from a run.
    pub fn get_step(&self, run_id: &str, step_id: &str) -> Result<StepSummary, ApiProblem> {
        let step = self
            .query
            .get_step(run_id, step_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_step", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!(
                    "step '{step_id}' was not found in run '{run_id}'"
                ))
            })?;
        Ok(super::mapping::map_step_summary(
            run_id,
            step,
            self.workspace_root.as_deref(),
        ))
    }

    /// Loads latest step metadata and selected output from one read-only query.
    /// Missing steps or explicit executions return 404; storage failures return 500.
    pub fn get_step_detail(
        &self,
        run_id: &str,
        step_id: &str,
        selection: super::OutputExecution,
    ) -> Result<crate::domain::api_models::StepDetail, ApiProblem> {
        let step = self
            .query
            .get_step(run_id, step_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_step_detail", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!(
                    "step '{step_id}' was not found in run '{run_id}'"
                ))
            })?;
        let (output, ordinals) = super::outputs::select_output(&step, selection)?;
        let summary =
            super::mapping::map_step_summary(run_id, step, self.workspace_root.as_deref());
        Ok(super::mapping::map_step_detail(summary, output, ordinals))
    }

    /// Lists manual gates recorded for a run.
    pub fn list_gates(
        &self,
        run_id: &str,
    ) -> Result<ListResponse<crate::domain::api_models::GateSummary>, ApiProblem> {
        let gates = self
            .query
            .list_gates(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_gates", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(ListResponse {
            pagination: pagination(gates.len(), gates.len(), 0),
            data: gates
                .into_iter()
                .map(|gate| super::mapping::map_gate_summary(run_id, gate))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Loads immutable document metadata by a run-scoped opaque revision ID.
    pub fn get_document(
        &self,
        run_id: &str,
        revision_id: &str,
    ) -> Result<transist::InspectionDocument, ApiProblem> {
        validate_document_id(revision_id)?;
        self.query
            .get_document(run_id, revision_id)
            .map_err(|e| ApiProblem::log_and_storage_query_failure("get_document", e))?
            .ok_or_else(|| ApiProblem::resource_not_found("document revision was not found"))
    }

    /// Downloads exact immutable bytes, selecting tree members only by opaque ID.
    pub fn get_document_content(
        &self,
        run_id: &str,
        revision_id: &str,
        member_id: Option<&str>,
    ) -> Result<super::BlobContentResponse, ApiProblem> {
        validate_document_id(revision_id)?;
        if let Some(id) = member_id {
            validate_document_id(id)?;
        }
        let content = self
            .query
            .read_document_content(run_id, revision_id, member_id)
            .map_err(|e| ApiProblem::log_and_storage_query_failure("read_document_content", e))?
            .ok_or_else(|| ApiProblem::resource_not_found("document content was not found"))?;
        Ok(super::BlobContentResponse {
            file_name: content.name.rsplit('/').next().unwrap_or("document").into(),
            content_type: "application/octet-stream".into(),
            bytes: content.bytes,
        })
    }

    /// Loads the trust and provenance summary for a single run identifier.
    pub fn get_run_trust(&self, run_id: &str) -> Result<TrustSummaryView, ApiProblem> {
        let run = self
            .query
            .get_run(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_run_trust", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(map_trust_summary(run.trust))
    }

    /// Loads the locality and external-transfer summary for a single run identifier.
    pub fn get_run_locality(&self, run_id: &str) -> Result<LocalityView, ApiProblem> {
        let run = self
            .query
            .get_run(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_run_locality", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(map_locality_view(run.locality))
    }

    /// Lists classified failures recorded for a run.
    pub fn list_failures(
        &self,
        run_id: &str,
    ) -> Result<ListResponse<transist::FailureRecord>, ApiProblem> {
        let failures = self
            .query
            .list_failures(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_failures", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(ListResponse {
            pagination: pagination(failures.len(), failures.len(), 0),
            data: failures,
        })
    }

    /// Loads one classified failure record from a run.
    pub fn get_failure(
        &self,
        run_id: &str,
        failure_id: &str,
    ) -> Result<transist::FailureRecord, ApiProblem> {
        self.query
            .get_failure(run_id, failure_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_failure", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!(
                    "failure '{failure_id}' was not found in run '{run_id}'"
                ))
            })
    }

    /// Lists artifact metadata entries produced by a run.
    pub fn list_artifacts(
        &self,
        run_id: &str,
        scope: BlobListScope,
    ) -> Result<ListResponse<ArtifactMetadata>, ApiProblem> {
        let artifacts = self
            .query
            .list_blobs(run_id, BlobFamily::Artifact, scope)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_artifacts", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(ListResponse {
            pagination: pagination(artifacts.len(), artifacts.len(), 0),
            data: artifacts
                .into_iter()
                .map(|artifact| {
                    let artifact_id = encode_artifact_id(
                        &artifact.step_id,
                        artifact.execution,
                        &artifact.relative_path,
                    );
                    map_artifact_metadata(run_id, &artifact_id, artifact)
                })
                .collect(),
        })
    }

    /// Loads artifact metadata for a previously encoded artifact locator.
    pub fn get_artifact_metadata(
        &self,
        run_id: &str,
        locator: &ResourceLocator,
    ) -> Result<ArtifactMetadata, ApiProblem> {
        let artifact = self
            .query
            .get_blob(
                run_id,
                BlobFamily::Artifact,
                &locator.step_id,
                locator.execution,
                &locator.relative_path,
            )
            .map_err(|error| {
                ApiProblem::log_and_storage_query_failure("get_artifact_metadata", error)
            })?
            .ok_or_else(|| resource_not_found_for_family(BlobFamily::Artifact, run_id))?;
        let artifact_id =
            encode_artifact_id(&locator.step_id, locator.execution, &locator.relative_path);
        Ok(map_artifact_metadata(run_id, &artifact_id, artifact))
    }

    /// Loads artifact content bytes for a previously encoded artifact locator.
    pub fn get_artifact_content(
        &self,
        run_id: &str,
        locator: &ResourceLocator,
    ) -> Result<BlobContentResponse, ApiProblem> {
        let content = self
            .query
            .read_blob_content(
                run_id,
                BlobFamily::Artifact,
                &locator.step_id,
                locator.execution,
                &locator.relative_path,
            )
            .map_err(|error| {
                ApiProblem::log_and_storage_query_failure("get_artifact_content", error)
            })?
            .ok_or_else(|| resource_not_found_for_family(BlobFamily::Artifact, run_id))?;
        Ok(map_blob_content_response(
            content.relative_path,
            content.name,
            content.bytes,
        ))
    }

    /// Lists evidence metadata entries produced by a run.
    pub fn list_evidence(
        &self,
        run_id: &str,
        scope: BlobListScope,
    ) -> Result<ListResponse<EvidenceMetadata>, ApiProblem> {
        let evidence = self
            .query
            .list_blobs(run_id, BlobFamily::Evidence, scope)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_evidence", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        Ok(ListResponse {
            pagination: pagination(evidence.len(), evidence.len(), 0),
            data: evidence
                .into_iter()
                .map(|item| {
                    let evidence_id =
                        encode_evidence_id(&item.step_id, item.execution, &item.relative_path);
                    map_evidence_metadata(run_id, &evidence_id, item)
                })
                .collect(),
        })
    }

    /// Loads evidence metadata for a previously encoded evidence locator.
    pub fn get_evidence_metadata(
        &self,
        run_id: &str,
        locator: &ResourceLocator,
    ) -> Result<EvidenceMetadata, ApiProblem> {
        let evidence = self
            .query
            .get_blob(
                run_id,
                BlobFamily::Evidence,
                &locator.step_id,
                locator.execution,
                &locator.relative_path,
            )
            .map_err(|error| {
                ApiProblem::log_and_storage_query_failure("get_evidence_metadata", error)
            })?
            .ok_or_else(|| resource_not_found_for_family(BlobFamily::Evidence, run_id))?;
        let evidence_id =
            encode_evidence_id(&locator.step_id, locator.execution, &locator.relative_path);
        Ok(map_evidence_metadata(run_id, &evidence_id, evidence))
    }

    /// Loads evidence content bytes for a previously encoded evidence locator.
    pub fn get_evidence_content(
        &self,
        run_id: &str,
        locator: &ResourceLocator,
    ) -> Result<BlobContentResponse, ApiProblem> {
        let content = self
            .query
            .read_blob_content(
                run_id,
                BlobFamily::Evidence,
                &locator.step_id,
                locator.execution,
                &locator.relative_path,
            )
            .map_err(|error| {
                ApiProblem::log_and_storage_query_failure("get_evidence_content", error)
            })?
            .ok_or_else(|| resource_not_found_for_family(BlobFamily::Evidence, run_id))?;
        Ok(map_blob_content_response(
            content.relative_path,
            content.name,
            content.bytes,
        ))
    }

    /// Loads workflow file metadata for a previously encoded workflow-file locator.
    pub fn get_workflow_file_metadata(
        &self,
        run_id: &str,
        locator: &WorkflowFileLocator,
    ) -> Result<WorkflowFileMetadata, ApiProblem> {
        let file = self.resolve_workflow_file(run_id, locator)?;
        let locator_kind = file.locator_kind;
        let workflow_file_id = encode_workflow_file_id(
            locator_kind,
            &file.step_id,
            &file.field_name,
            &file.relative_path,
        );
        let display_root = match locator_kind {
            transist::InspectionWorkflowFileLocatorKind::Workspace => {
                self.workspace_root.as_deref()
            }
            transist::InspectionWorkflowFileLocatorKind::RepositoryState => {
                self.workspace_root.as_deref()
            }
            transist::InspectionWorkflowFileLocatorKind::Run => None,
        };
        let content_authority = locator_kind == transist::InspectionWorkflowFileLocatorKind::Run
            || self.workspace_root.is_some();
        if content_authority {
            self.query
                .read_workflow_file_content(
                    run_id,
                    &file.step_id,
                    locator_kind,
                    &file.relative_path,
                    self.workspace_root.as_deref(),
                )
                .map_err(|error| {
                    tracing::warn!(run_id, error = %error, "workflow file metadata read denied");
                    workflow_file_not_found(run_id)
                })?
                .ok_or_else(|| workflow_file_not_found(run_id))?;
        }
        Ok(map_workflow_file_metadata(
            run_id,
            &workflow_file_id,
            file,
            display_root,
            content_authority,
        ))
    }

    /// Loads workflow file bytes after resolving an opaque locator against typed step data.
    pub fn get_workflow_file_content(
        &self,
        run_id: &str,
        locator: &WorkflowFileLocator,
    ) -> Result<BlobContentResponse, ApiProblem> {
        let file = self.resolve_workflow_file(run_id, locator)?;
        let content = self
            .query
            .read_workflow_file_content(
                run_id,
                &file.step_id,
                file.locator_kind,
                &file.relative_path,
                self.workspace_root.as_deref(),
            )
            .map_err(|error| {
                tracing::warn!(run_id, error = %error, "workflow file content read denied");
                workflow_file_not_found(run_id)
            })?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!(
                    "workflow file was not found for run '{run_id}'"
                ))
            })?;
        Ok(map_blob_content_response(
            file.relative_path,
            content.name,
            content.bytes,
        ))
    }

    fn resolve_workflow_file(
        &self,
        run_id: &str,
        locator: &WorkflowFileLocator,
    ) -> Result<transist::InspectionWorkflowFileRef, ApiProblem> {
        let steps = self
            .query
            .list_steps(run_id)
            .map_err(|error| {
                ApiProblem::log_and_storage_query_failure("resolve_workflow_file", error)
            })?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("run '{run_id}' was not found"))
            })?;
        let matches = steps.into_iter().flat_map(|step| {
            step.output_summary
                .workflow_files
                .into_iter()
                .filter_map(move |file| {
                    (encode_workflow_file_id(
                        file.locator_kind,
                        &file.step_id,
                        &file.field_name,
                        &file.relative_path,
                    )
                    .strip_prefix("wf_")
                        == Some(locator.key.as_str()))
                    .then_some(file)
                })
        });
        let Some(file) = unique_workflow_file(matches) else {
            return Err(ApiProblem::resource_not_found(format!(
                "workflow file was not found for run '{run_id}'"
            )));
        };
        Ok(file)
    }

    /// Lists authored workflows discovered under the configured workspace root.
    pub fn list_workflows(
        &self,
        query: &PaginationQuery,
    ) -> Result<ListResponse<WorkflowSummary>, ApiProblem> {
        let page = self
            .query
            .list_workflows(query)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_workflows", error))?;
        Ok(ListResponse {
            pagination: pagination(page.total, page.limit, page.offset),
            data: page
                .data
                .into_iter()
                .map(|workflow| map_workflow_summary(workflow, self.workspace_root.as_deref()))
                .collect(),
        })
    }

    /// Loads detailed workflow metadata for a workflow reference.
    pub fn get_workflow(&self, workflow_ref: &str) -> Result<WorkflowDetail, ApiProblem> {
        let workflow = self
            .query
            .get_workflow(workflow_ref)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_workflow", error))?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!("workflow '{workflow_ref}' was not found"))
            })?;
        Ok(map_workflow_detail(
            workflow,
            self.workspace_root.as_deref(),
        ))
    }

    /// Returns the authored workflow source file content when available.
    pub fn get_workflow_source(
        &self,
        workflow_ref: &str,
    ) -> Result<InlineContentResponse, ApiProblem> {
        let content = self
            .query
            .read_workflow_source(workflow_ref)
            .map_err(|error| {
                ApiProblem::log_and_storage_query_failure("get_workflow_source", error)
            })?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!(
                    "workflow source is not available for workflow '{workflow_ref}'"
                ))
            })?;
        Ok(super::content::map_inline_content_response(
            content.path,
            content.bytes,
        ))
    }

    /// Returns the compiled recipe content for a workflow when available.
    pub fn get_workflow_compiled_recipe(
        &self,
        workflow_ref: &str,
    ) -> Result<InlineContentResponse, ApiProblem> {
        let content = self
            .query
            .read_workflow_compiled_recipe(workflow_ref)
            .map_err(|error| {
                ApiProblem::log_and_storage_query_failure("get_workflow_compiled_recipe", error)
            })?
            .ok_or_else(|| {
                ApiProblem::resource_not_found(format!(
                    "compiled recipe is not available for workflow '{workflow_ref}'"
                ))
            })?;
        Ok(super::content::map_inline_content_response(
            content.path,
            content.bytes,
        ))
    }
}

fn unique_workflow_file(
    mut matches: impl Iterator<Item = transist::InspectionWorkflowFileRef>,
) -> Option<transist::InspectionWorkflowFileRef> {
    let file = matches.next()?;
    matches.next().is_none().then_some(file)
}

fn workflow_file_not_found(run_id: &str) -> ApiProblem {
    ApiProblem::resource_not_found(format!("workflow file was not found for run '{run_id}'"))
}

#[cfg(test)]
mod tests {
    use super::unique_workflow_file;
    use transist::InspectionWorkflowFileRef;

    fn file(path: &str) -> InspectionWorkflowFileRef {
        InspectionWorkflowFileRef {
            step_id: "collect".to_string(),
            field_name: "output_file".to_string(),
            locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
            relative_path: path.to_string(),
            local_path: None,
            name: "output.md".to_string(),
            bytes: 1,
        }
    }

    #[test]
    fn duplicate_workflow_file_matches_fail_closed() {
        assert!(unique_workflow_file(vec![file("a"), file("b")].into_iter()).is_none());
    }

    #[test]
    fn duplicate_identical_workflow_file_locators_fail_closed() {
        assert!(unique_workflow_file(vec![file("same"), file("same")].into_iter()).is_none());
    }
}

#[derive(Clone)]
/// Read-only metrics service that exposes inspection-friendly metrics snapshots.
pub struct MetricsApiService {
    query: Arc<dyn MetricsQueryPort>,
}

impl MetricsApiService {
    /// Builds the service from a metrics query port implementation.
    pub fn from_query(query: Arc<dyn MetricsQueryPort>) -> Self {
        Self { query }
    }

    /// Lists metrics summaries for runs known to the metrics query port.
    pub fn list_run_metrics(
        &self,
    ) -> Result<std::collections::BTreeMap<String, transist::RunListMetricsSummary>, ApiProblem>
    {
        self.query
            .run_list_metrics()
            .map_err(|error| ApiProblem::log_and_storage_query_failure("list_run_metrics", error))
    }

    /// Loads metrics for a single run when metrics are available.
    pub fn get_run_metrics(
        &self,
        run_id: &str,
    ) -> Result<Option<transist::RunMetricsSummary>, ApiProblem> {
        self.query
            .get_run_metrics(run_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_run_metrics", error))
    }

    /// Loads metrics for a single step when metrics are available.
    pub fn get_step_metrics(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<transist::StepMetricsSummary>, ApiProblem> {
        self.query
            .get_step_metrics(run_id, step_id)
            .map_err(|error| ApiProblem::log_and_storage_query_failure("get_step_metrics", error))
    }

    /// Renders the current metrics snapshot in Prometheus exposition format.
    pub fn render_prometheus(&self) -> Result<String, ApiProblem> {
        self.query
            .render_prometheus()
            .map_err(|error| ApiProblem::log_and_storage_query_failure("render_prometheus", error))
    }
}

#[derive(Debug)]
/// Problem details used internally before mapping to the JSON error envelope.
pub struct ApiProblem {
    /// HTTP status code returned to the caller.
    pub status: StatusCode,
    /// Stable machine-readable error code.
    pub code: &'static str,
    /// Sanitized human-readable message returned in the response body.
    pub message: String,
}

impl ApiProblem {
    /// Constructs a `422 invalid_text_encoding` problem without source content.
    pub fn invalid_text_encoding() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_text_encoding",
            message: "requested content is not valid UTF-8 text".to_string(),
        }
    }

    /// Constructs a `400 invalid_request` problem.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: message.into(),
        }
    }

    /// Constructs a `400 unsupported_query_value` problem.
    pub fn unsupported_query_value(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "unsupported_query_value",
            message: message.into(),
        }
    }

    /// Constructs a `404 resource_not_found` problem.
    pub fn resource_not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "resource_not_found",
            message: message.into(),
        }
    }

    /// Logs a storage/query failure and returns a sanitized `500` problem.
    pub fn log_and_storage_query_failure(operation: &'static str, error: anyhow::Error) -> Self {
        tracing::error!(operation, error = %error, ?error, "inspection storage query failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "storage_query_failure",
            message: STORAGE_QUERY_FAILURE_MESSAGE.to_string(),
        }
    }

    /// Logs an unexpected server failure and returns a sanitized `500` problem.
    pub fn log_and_internal_server_error(operation: &'static str, error: anyhow::Error) -> Self {
        tracing::error!(operation, error = %error, ?error, "inspection server internal error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_server_error",
            message: INTERNAL_SERVER_ERROR_MESSAGE.to_string(),
        }
    }

    /// Converts the internal problem into the JSON API error envelope shape.
    pub fn into_envelope(self) -> ErrorEnvelope {
        ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message,
            },
        }
    }
}
