// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use transist_storage_local::InspectionCache;

use crate::application::inspection::{
    BlobFamily, BlobListScope, EventListQuery, InspectionBlob, InspectionBlobContent,
    InspectionEvent, InspectionGate, InspectionQueryPort, InspectionRunDetail,
    InspectionRunSummary, InspectionStep, InspectionWorkflowContent, InspectionWorkflowDetail,
    InspectionWorkflowSummary, LocalInspectionQuery, Page, PaginationQuery, RunListQuery,
    RunListScope,
};

/// Convenience wrapper around the local inspection query API.
#[derive(Clone)]
pub struct TransistInspectionContext {
    query: Arc<dyn InspectionQueryPort>,
}

impl TransistInspectionContext {
    /// Creates an inspection context that reads from the repository-local `.transist` directory.
    pub fn repo_local(repo_root: impl Into<PathBuf>) -> Self {
        let repo_root = repo_root.into();
        Self::with_home(&repo_root, crate::repo_local_home_dir(&repo_root))
    }

    /// Creates an inspection context that reads from an explicit home directory.
    pub fn with_home(repo_root: impl Into<PathBuf>, home_dir: impl Into<PathBuf>) -> Self {
        let repo_root = repo_root.into();
        let home_dir = home_dir.into();
        Self {
            query: Arc::new(LocalInspectionQuery::new(repo_root, home_dir)),
        }
    }

    /// Creates an inspection context that reads from an explicit home directory and shared cache.
    pub(crate) fn with_home_and_cache(
        repo_root: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
        cache: Arc<InspectionCache>,
    ) -> Self {
        let repo_root = repo_root.into();
        let home_dir = home_dir.into();
        Self {
            query: Arc::new(LocalInspectionQuery::with_cache(repo_root, home_dir, cache)),
        }
    }

    /// Lists runs with pagination and filtering.
    pub fn list_runs(&self, query: &RunListQuery) -> Result<Page<InspectionRunSummary>> {
        self.query.list_runs(query)
    }
    /// Lists all runs matching scope/status in one call.
    pub fn list_all_runs(
        &self,
        scope: RunListScope,
        status: Option<&str>,
    ) -> Result<Vec<InspectionRunSummary>> {
        self.query.list_all_runs(scope, status)
    }
    /// Loads one run, returning `None` when the run does not exist.
    pub fn get_run(&self, run_id: &str) -> Result<Option<InspectionRunDetail>> {
        self.query.get_run(run_id)
    }
    /// Loads many runs by id, returning details for those that exist.
    pub fn get_runs(&self, run_ids: &[String]) -> Result<BTreeMap<String, InspectionRunDetail>> {
        self.query.get_runs(run_ids)
    }
    /// Lists events for one run, returning `None` when the run does not exist.
    pub fn list_events(
        &self,
        run_id: &str,
        query: &EventListQuery,
    ) -> Result<Option<Page<InspectionEvent>>> {
        self.query.list_events(run_id, query)
    }
    /// Lists steps for one run, returning `None` when the run does not exist.
    pub fn list_steps(&self, run_id: &str) -> Result<Option<Vec<InspectionStep>>> {
        self.query.list_steps(run_id)
    }
    /// Lists manual gates for one run, returning `None` when the run does not exist.
    pub fn list_gates(&self, run_id: &str) -> Result<Option<Vec<InspectionGate>>> {
        self.query.list_gates(run_id)
    }
    /// Lists blobs of one family for one run, returning `None` when the run does not exist.
    pub fn list_blobs(
        &self,
        run_id: &str,
        family: BlobFamily,
        scope: BlobListScope,
    ) -> Result<Option<Vec<InspectionBlob>>> {
        self.query.list_blobs(run_id, family, scope)
    }
    /// Loads one blob descriptor, returning `None` when either the run or blob is missing.
    pub fn get_blob(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlob>> {
        self.query
            .get_blob(run_id, family, step_id, execution, relative_path)
    }
    /// Reads one blob's bytes, returning `None` when either the run or blob is missing.
    pub fn read_blob_content(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlobContent>> {
        self.query
            .read_blob_content(run_id, family, step_id, execution, relative_path)
    }
    /// Lists discovered workflows from source flows and compiled recipes.
    pub fn list_workflows(
        &self,
        query: &PaginationQuery,
    ) -> Result<Page<InspectionWorkflowSummary>> {
        self.query.list_workflows(query)
    }
    /// Loads workflow detail by `<id>@<version>` reference.
    pub fn get_workflow(&self, workflow_ref: &str) -> Result<Option<InspectionWorkflowDetail>> {
        self.query.get_workflow(workflow_ref)
    }
    /// Reads the authored source file for one workflow when available.
    pub fn read_workflow_source(
        &self,
        workflow_ref: &str,
    ) -> Result<Option<InspectionWorkflowContent>> {
        self.query.read_workflow_source(workflow_ref)
    }
    /// Reads the stored compiled recipe for one workflow when available.
    pub fn read_workflow_compiled_recipe(
        &self,
        workflow_ref: &str,
    ) -> Result<Option<InspectionWorkflowContent>> {
        self.query.read_workflow_compiled_recipe(workflow_ref)
    }

    pub(crate) fn query_port(&self) -> &dyn InspectionQueryPort {
        self.query.as_ref()
    }
}

impl InspectionQueryPort for TransistInspectionContext {
    fn get_document(
        &self,
        run_id: &str,
        revision_id: &str,
    ) -> Result<Option<crate::InspectionDocument>> {
        self.query.get_document(run_id, revision_id)
    }

    fn read_document_content(
        &self,
        run_id: &str,
        revision_id: &str,
        member_id: Option<&str>,
    ) -> Result<Option<InspectionBlobContent>> {
        self.query
            .read_document_content(run_id, revision_id, member_id)
    }
    fn list_runs(&self, query: &RunListQuery) -> Result<Page<InspectionRunSummary>> {
        self.query.list_runs(query)
    }

    fn get_run(&self, run_id: &str) -> Result<Option<InspectionRunDetail>> {
        self.query.get_run(run_id)
    }

    fn list_events(
        &self,
        run_id: &str,
        query: &EventListQuery,
    ) -> Result<Option<Page<InspectionEvent>>> {
        self.query.list_events(run_id, query)
    }

    fn list_steps(&self, run_id: &str) -> Result<Option<Vec<InspectionStep>>> {
        self.query.list_steps(run_id)
    }

    fn get_step(&self, run_id: &str, step_id: &str) -> Result<Option<InspectionStep>> {
        self.query.get_step(run_id, step_id)
    }

    fn list_step_executions(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<Vec<crate::application::inspection::InspectionStepExecution>>> {
        self.query.list_step_executions(run_id, step_id)
    }

    fn get_workflow_file(
        &self,
        run_id: &str,
        step_id: &str,
        locator_kind: crate::application::inspection::InspectionWorkflowFileLocatorKind,
        relative_path: &str,
    ) -> Result<Option<crate::application::inspection::InspectionWorkflowFileRef>> {
        self.query
            .get_workflow_file(run_id, step_id, locator_kind, relative_path)
    }

    fn read_workflow_file_content(
        &self,
        run_id: &str,
        step_id: &str,
        locator_kind: crate::application::inspection::InspectionWorkflowFileLocatorKind,
        relative_path: &str,
        workspace_root: Option<&std::path::Path>,
    ) -> Result<Option<crate::application::inspection::InspectionWorkflowFileContent>> {
        self.query.read_workflow_file_content(
            run_id,
            step_id,
            locator_kind,
            relative_path,
            workspace_root,
        )
    }

    fn list_gates(&self, run_id: &str) -> Result<Option<Vec<InspectionGate>>> {
        self.query.list_gates(run_id)
    }

    fn list_failures(&self, run_id: &str) -> Result<Option<Vec<transist_core::FailureRecord>>> {
        self.query.list_failures(run_id)
    }

    fn get_failure(
        &self,
        run_id: &str,
        failure_id: &str,
    ) -> Result<Option<transist_core::FailureRecord>> {
        self.query.get_failure(run_id, failure_id)
    }

    fn list_blobs(
        &self,
        run_id: &str,
        family: BlobFamily,
        scope: BlobListScope,
    ) -> Result<Option<Vec<InspectionBlob>>> {
        self.query.list_blobs(run_id, family, scope)
    }

    fn get_blob(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlob>> {
        self.query
            .get_blob(run_id, family, step_id, execution, relative_path)
    }

    fn read_blob_content(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlobContent>> {
        self.query
            .read_blob_content(run_id, family, step_id, execution, relative_path)
    }

    fn list_workflows(&self, query: &PaginationQuery) -> Result<Page<InspectionWorkflowSummary>> {
        self.query.list_workflows(query)
    }

    fn get_workflow(&self, workflow_ref: &str) -> Result<Option<InspectionWorkflowDetail>> {
        self.query.get_workflow(workflow_ref)
    }

    fn read_workflow_source(
        &self,
        workflow_ref: &str,
    ) -> Result<Option<InspectionWorkflowContent>> {
        self.query.read_workflow_source(workflow_ref)
    }

    fn read_workflow_compiled_recipe(
        &self,
        workflow_ref: &str,
    ) -> Result<Option<InspectionWorkflowContent>> {
        self.query.read_workflow_compiled_recipe(workflow_ref)
    }
}
