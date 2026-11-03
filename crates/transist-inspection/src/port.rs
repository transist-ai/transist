// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

use super::{
    BlobFamily, BlobListScope, EventListQuery, InspectionBlob, InspectionBlobContent,
    InspectionEvent, InspectionGate, InspectionRunDetail, InspectionRunSummary, InspectionStep,
    InspectionStepExecution, InspectionWorkflowContent, InspectionWorkflowDetail,
    InspectionWorkflowFileContent, InspectionWorkflowFileLocatorKind, InspectionWorkflowFileRef,
    InspectionWorkflowSummary, Page, PaginationQuery, RunListQuery, RunListScope,
};
use transist_core::FailureRecord;

/// Read-only query port for inspection data.
pub trait InspectionQueryPort: Send + Sync {
    /// Lists runs with optional filtering and pagination.
    fn list_runs(&self, query: &RunListQuery) -> Result<Page<InspectionRunSummary>>;
    /// Lists all runs matching scope/status without pagination.
    fn list_all_runs(
        &self,
        scope: RunListScope,
        status: Option<&str>,
    ) -> Result<Vec<InspectionRunSummary>> {
        let mut offset = 0;
        let mut runs = Vec::new();
        loop {
            let page = self.list_runs(&RunListQuery {
                pagination: PaginationQuery {
                    limit: super::MAX_PAGE_LIMIT,
                    offset,
                },
                scope,
                status: status.map(str::to_string),
                ..Default::default()
            })?;
            let page_len = page.data.len();
            runs.extend(page.data);
            if page_len == 0 || runs.len() >= page.total {
                break;
            }
            offset += page_len;
        }
        Ok(runs)
    }
    /// Loads one run, returning `None` when the run does not exist.
    fn get_run(&self, run_id: &str) -> Result<Option<InspectionRunDetail>>;
    /// Loads many runs by id, returning details for those that exist.
    fn get_runs(&self, run_ids: &[String]) -> Result<BTreeMap<String, InspectionRunDetail>> {
        let mut out = BTreeMap::new();
        for run_id in run_ids {
            if let Some(detail) = self.get_run(run_id)? {
                out.insert(run_id.clone(), detail);
            }
        }
        Ok(out)
    }
    /// Lists events for one run, returning `None` when the run does not exist.
    fn list_events(
        &self,
        run_id: &str,
        query: &EventListQuery,
    ) -> Result<Option<Page<InspectionEvent>>>;
    /// Lists steps for one run, returning `None` when the run does not exist.
    fn list_steps(&self, run_id: &str) -> Result<Option<Vec<InspectionStep>>>;
    /// Loads one step for one run, returning `None` when the run or step does not exist.
    fn get_step(&self, run_id: &str, step_id: &str) -> Result<Option<InspectionStep>>;
    /// Lists all execution records for one step, sorted ascending by execution
    /// number. Returns `None` when the run does not exist.
    fn list_step_executions(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<Vec<InspectionStepExecution>>>;
    /// Loads one workflow file reference declared by a step, returning `None` when missing.
    fn get_workflow_file(
        &self,
        run_id: &str,
        step_id: &str,
        locator_kind: InspectionWorkflowFileLocatorKind,
        relative_path: &str,
    ) -> Result<Option<InspectionWorkflowFileRef>>;
    /// Reads one workflow file beneath a trusted root without following symlinks.
    fn read_workflow_file_content(
        &self,
        run_id: &str,
        step_id: &str,
        locator_kind: InspectionWorkflowFileLocatorKind,
        relative_path: &str,
        workspace_root: Option<&Path>,
    ) -> Result<Option<InspectionWorkflowFileContent>>;
    /// Lists manual gates for one run, returning `None` when the run does not exist.
    fn list_gates(&self, run_id: &str) -> Result<Option<Vec<InspectionGate>>>;
    /// Resolves a committed revision by opaque ID within the run's strict root lineage.
    fn get_document(
        &self,
        _run_id: &str,
        _revision_id: &str,
    ) -> Result<Option<super::InspectionDocument>> {
        anyhow::bail!("document inspection unavailable")
    }
    /// Reads exact revision/member bytes; member IDs are opaque and revision-scoped.
    fn read_document_content(
        &self,
        _run_id: &str,
        _revision_id: &str,
        _member_id: Option<&str>,
    ) -> Result<Option<InspectionBlobContent>> {
        anyhow::bail!("document inspection unavailable")
    }
    /// Lists classified failures for one run, returning `None` when the run does not exist.
    fn list_failures(&self, run_id: &str) -> Result<Option<Vec<FailureRecord>>>;
    /// Loads one classified failure by id, returning `None` when missing.
    fn get_failure(&self, run_id: &str, failure_id: &str) -> Result<Option<FailureRecord>>;
    /// Lists blobs of one family for one run, returning `None` when the run does not exist.
    fn list_blobs(
        &self,
        run_id: &str,
        family: BlobFamily,
        scope: BlobListScope,
    ) -> Result<Option<Vec<InspectionBlob>>>;
    /// Loads one blob descriptor, returning `None` when either the run or blob is missing.
    fn get_blob(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlob>>;
    /// Reads one blob's bytes, returning `None` when either the run or blob is missing.
    fn read_blob_content(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlobContent>>;
    /// Lists discovered workflows from source files and compiled recipes.
    fn list_workflows(&self, query: &PaginationQuery) -> Result<Page<InspectionWorkflowSummary>>;
    /// Loads workflow detail by `<id>@<version>` reference.
    fn get_workflow(&self, workflow_ref: &str) -> Result<Option<InspectionWorkflowDetail>>;
    /// Reads the authored source file for one workflow when available.
    fn read_workflow_source(&self, workflow_ref: &str)
    -> Result<Option<InspectionWorkflowContent>>;
    /// Reads the stored compiled recipe for one workflow when available.
    fn read_workflow_compiled_recipe(
        &self,
        workflow_ref: &str,
    ) -> Result<Option<InspectionWorkflowContent>>;
}
