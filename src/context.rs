// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use transist_core::RunStorePort;
use transist_core::reachable_recipe_step_ids;
use transist_protocol::{KernelValue, Recipe};
use transist_storage_local::{
    InspectionCache, InspectionCacheKey, LocalRunStore, LocalStorageConfig,
};

use crate::application;
use crate::application::runs::{
    GateDecisionInput, PrepareReport, RunCancellationRequest, RunEvidenceExport, RunReport,
    RunTreeLeaseRecord,
};
use crate::{
    InstalledPlugin, InstalledWorkflow, PluginInstallReport, PluginManifest, PreflightReport,
    application::workflows::{WorkflowInspection, WorkflowInstallReport},
};

/// High-level context for workflow, run, and plugin operations.
#[derive(Debug, Clone)]
pub struct TransistContext {
    repo_root: PathBuf,
    home_dir: PathBuf,
    router_cache: Arc<crate::composition::router_cache::RouterCache>,
    inspection_cache: Arc<InspectionCache>,
    workflows: application::workflows::WorkflowService,
    runs: application::runs::RunService,
    plugins: application::plugins::PluginService,
    trust: application::trust::TrustService,
}

impl TransistContext {
    /// Creates a context with a per-repository home outside the workspace.
    pub fn repo_local(repo_root: impl Into<PathBuf>) -> Self {
        let repo_root = repo_root.into();
        let home_dir = crate::repo_local_home_dir(&repo_root);
        Self::with_cache_capacity(repo_root, home_dir, crate::DEFAULT_INSPECTION_CACHE_RUNS)
    }

    fn with_cache_capacity(
        repo_root: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
        cache_capacity: usize,
    ) -> Self {
        let repo_root = repo_root.into();
        let home_dir = home_dir.into();
        let router_cache = Arc::new(crate::composition::router_cache::RouterCache::local());
        Self {
            workflows: application::workflows::WorkflowService::new(
                repo_root.clone(),
                home_dir.clone(),
                Arc::clone(&router_cache),
            ),
            runs: application::runs::RunService::new(
                repo_root.clone(),
                home_dir.clone(),
                Arc::clone(&router_cache),
            ),
            plugins: application::plugins::PluginService::new(home_dir.clone()),
            trust: application::trust::TrustService::new(home_dir.clone()),
            router_cache,
            inspection_cache: Arc::new(InspectionCache::with_default_capacity(cache_capacity)),
            repo_root,
            home_dir,
        }
    }

    /// Creates a context that uses an explicit home directory.
    pub fn with_home(repo_root: impl Into<PathBuf>, home_dir: impl Into<PathBuf>) -> Self {
        Self::with_cache_capacity(repo_root, home_dir, crate::DEFAULT_INSPECTION_CACHE_RUNS)
    }

    /// Creates a context with an explicit inspection-cache capacity.
    pub fn with_home_and_inspection_cache_capacity(
        repo_root: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
        cache_capacity: usize,
    ) -> Self {
        Self::with_cache_capacity(repo_root, home_dir, cache_capacity)
    }

    /// Returns the repository root used by this context.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Returns the Transist home directory used by this context.
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Returns the local inspection facade composed for this context.
    pub fn inspection(&self) -> crate::TransistInspectionContext {
        crate::TransistInspectionContext::with_home_and_cache(
            self.repo_root.clone(),
            self.home_dir.clone(),
            Arc::clone(&self.inspection_cache),
        )
    }

    /// Returns the local metrics facade composed for this context.
    pub fn metrics(&self) -> crate::TransistMetricsContext {
        crate::TransistMetricsContext::with_home(self.home_dir.clone())
    }

    /// Builds a read-only inspection snapshot using the context's composed query services.
    pub fn inspection_snapshot(
        &self,
        request: &crate::InspectionSnapshotRequest,
    ) -> Result<Option<crate::InspectionSnapshotEnvelope>> {
        let inspection = self.inspection();
        let metrics = self.metrics();
        crate::InspectionSnapshotBuilder::new(inspection.query_port(), metrics.query_port())
            .build(request)
    }

    /// Invalidates cached inspection data for one run.
    pub fn invalidate_inspection_cache(&self, run_id: &str) -> Result<()> {
        let key = InspectionCacheKey::new(&self.home_dir, run_id)?;
        self.inspection_cache.invalidate(&key);
        Ok(())
    }

    /// Resolves the persisted run-root directory for a run id.
    pub fn run_root(&self, run_id: &str) -> Result<PathBuf> {
        transist_storage_local::run_paths::resolve_run_root(&self.home_dir, run_id)
    }

    /// Resolves a persisted run root, falling back to the legacy layout when needed.
    pub fn run_root_or_default(&self, run_id: &str) -> Result<PathBuf> {
        transist_storage_local::run_paths::resolve_run_root_or_default(&self.home_dir, run_id)
    }

    /// Loads the recipe snapshot persisted with a run.
    pub fn persisted_recipe(&self, run_id: &str) -> Result<Recipe> {
        let raw = LocalRunStore::new(LocalStorageConfig::new(&self.home_dir))
            .load_recipe_snapshot(run_id)?;
        yaml_serde::from_str(&raw).map_err(Into::into)
    }

    /// Returns the recipe steps visible from the persisted run state.
    pub fn visible_step_ids(
        &self,
        run_id: &str,
        run_state: &KernelValue,
    ) -> Result<Option<Vec<String>>> {
        let raw = match LocalRunStore::new(LocalStorageConfig::new(&self.home_dir))
            .load_recipe_snapshot(run_id)
        {
            Ok(raw) => raw,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let recipe: Recipe = yaml_serde::from_str(&raw)?;
        let all_step_ids = recipe
            .steps
            .iter()
            .map(|step| step.id.clone())
            .collect::<Vec<_>>();
        let snapshot = match serde_json::from_value::<transist_core::KernelRunSnapshot>(
            serde_json::Value::from(run_state.clone()),
        ) {
            Ok(snapshot) => snapshot,
            Err(_) => return Ok(Some(all_step_ids)),
        };
        let run_state_json = serde_json::Value::from(run_state.clone());
        let workflow_inputs = run_state_json
            .get("workflow_inputs")
            .and_then(|value| value.as_object())
            .map(|inputs| {
                inputs
                    .iter()
                    .map(|(key, value)| (key.clone(), KernelValue::from(value.clone())))
                    .collect()
            })
            .unwrap_or_default();
        let reachable =
            reachable_recipe_step_ids(&recipe, Some(&snapshot), Some(&workflow_inputs))?;
        Ok(Some(
            recipe
                .steps
                .iter()
                .filter(|step| reachable.contains(&step.id))
                .map(|step| step.id.clone())
                .collect(),
        ))
    }

    /// Ensures the local installation record exists for this context's home.
    pub fn ensure_installation(&self) -> Result<()> {
        transist_storage_local::Installation::load_or_create(&self.home_dir).map(|_| ())
    }

    /// Verifies that the local installation record exists and is valid.
    pub fn validate_installation(&self) -> Result<()> {
        transist_storage_local::Installation::load(&self.home_dir).map(|_| ())
    }

    /// Resolves the active invoke step targeted by cancellation for a run tree.
    pub fn cancellation_target(
        &self,
        run_id: &str,
    ) -> Result<Option<application::runs::RunCancellationTarget>> {
        application::runs::resolve_cancellation_target_in_home(&self.home_dir, run_id)
    }

    /// Creates the home directory structure and default config files if needed.
    pub fn init(&self) -> Result<()> {
        crate::init_home_dir(&self.home_dir)
    }

    /// Compiles a flow file and writes its recipe artifact under this context's home.
    pub fn compile_flow_file(&self, flow_path: &Path) -> Result<PathBuf> {
        self.workflows.compile_flow_file(flow_path)
    }

    /// Compiles a flow file with optional trusted-execution install verification.
    pub fn compile_flow_file_with_trust(
        &self,
        flow_path: &Path,
        trusted_execution: bool,
    ) -> Result<crate::application::workflows::WorkflowCompileResult> {
        self.workflows
            .compile_flow_file_with_trust(flow_path, trusted_execution)
    }

    /// Returns read-only trusted compile metadata status for an authored workflow.
    pub fn workflow_compile_trust_status(
        &self,
        flow_path: &Path,
    ) -> Result<crate::WorkflowCompileTrustStatus> {
        self.workflows.workflow_compile_trust_status(flow_path)
    }

    /// Compiles and durably records a workflows install in this context's home.
    pub fn install_flow_file_with_trust(
        &self,
        flow_path: &Path,
        trusted_execution: bool,
    ) -> Result<WorkflowInstallReport> {
        self.workflows
            .install_flow_file_with_trust(flow_path, trusted_execution)
    }
    /// Installs a workflow file with optional trusted verification using a
    /// caller-selected detached signature source.
    pub fn install_flow_file_with_trust_and_signature(
        &self,
        flow_path: &Path,
        trusted_execution: bool,
        signature_path: Option<&Path>,
    ) -> Result<WorkflowInstallReport> {
        self.workflows.install_flow_file_with_trust_and_signature(
            flow_path,
            trusted_execution,
            signature_path,
        )
    }

    /// Installs exact captured package bytes while the caller owns the home install lock.
    pub fn install_workflow_package_snapshot_with_trust(
        &self,
        package: &crate::WorkflowPackageSnapshot,
        trusted_execution: bool,
        install_lock: &crate::WorkflowInstallLock,
    ) -> Result<WorkflowInstallReport> {
        self.workflows
            .install_package_snapshot_with_trust(package, trusted_execution, install_lock)
    }

    /// Installs exact captured bytes and records each newly published artifact.
    pub fn install_workflow_package_snapshot_with_trust_and_ownership(
        &self,
        package: &crate::WorkflowPackageSnapshot,
        trusted_execution: bool,
        install_lock: &crate::WorkflowInstallLock,
        ownership: &mut crate::WorkflowInstallOwnershipJournal,
    ) -> Result<WorkflowInstallReport> {
        self.workflows
            .install_package_snapshot_with_trust_and_ownership(
                package,
                trusted_execution,
                install_lock,
                ownership,
            )
    }

    /// Validates a flow file against the flow schema and local plugin/toolchain state.
    pub fn validate_flow_file_summary(
        &self,
        flow_path: &Path,
    ) -> Result<crate::WorkflowCheckSummary> {
        self.workflows.validate_flow_file_summary(flow_path)
    }

    /// Binds prospective inputs and returns compact static validation without creating a run.
    pub fn validate_bound_workflow_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::WorkflowCheckSummary> {
        self.workflows.validate_bound_workflow_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
    }

    /// Returns compact probe-free validation for prospective inputs and current authority/modes.
    /// No inputs or secret values are persisted, and no run or live probe is created.
    pub fn workflow_validation_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::WorkflowCheckSummary> {
        Ok(self.workflows.workflow_validation_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        ))
    }

    /// Returns full validation and its compact view from one probe-free evaluation of supplied inputs.
    /// Current policy, trust, prerequisites and secret availability are checked without writes.
    pub fn workflow_validation_report_with_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<(crate::WorkflowReadinessReport, crate::WorkflowCheckSummary)> {
        Ok(self.workflows.workflow_validation_report_with_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        ))
    }

    /// Returns probe-free validation evidence for supplied inputs and current modes without writes.
    /// Passing validation does not establish live readiness or permit dispatch.
    pub fn workflow_validation_report(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::WorkflowReadinessReport> {
        Ok(self.workflows.workflow_validation_report(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        ))
    }

    /// Returns compact live readiness; input values are evaluated in memory and never persisted.
    pub fn workflow_readiness_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::WorkflowCheckSummary> {
        Ok(self.workflows.workflow_readiness_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        ))
    }

    /// Returns full readiness and its compact view from a single read-only evaluation.
    pub fn workflow_readiness_report_with_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<(crate::WorkflowReadinessReport, crate::WorkflowCheckSummary)> {
        Ok(self.workflows.workflow_readiness_report_with_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        ))
    }

    /// Validates a flow file against the flow schema and local plugin/toolchain state.
    pub fn validate_flow_file(
        &self,
        flow_path: &Path,
    ) -> Result<crate::AuthoredWorkflowValidation> {
        self.workflows.validate_flow_file(flow_path)
    }

    /// Binds and statically validates an installed workflow without creating a run.
    pub fn validate_bound_workflow(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::BoundWorkflowValidation> {
        self.workflows.validate_bound_workflow(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
    }

    /// Evaluates complete workflow readiness without creating a run or dispatching an action.
    pub fn evaluate_workflow_readiness(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::BoundWorkflowValidation> {
        self.workflows.evaluate_readiness(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
    }

    /// Returns the typed read-only readiness report for a prospective run.
    ///
    /// Supplied input values are evaluated in memory only and are never
    /// persisted by this operation.
    pub fn workflow_readiness_report(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::WorkflowReadinessReport> {
        Ok(self.workflows.workflow_readiness_report(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        ))
    }

    /// Plans or applies exact workflow authority grants without creating a run.
    pub fn authorize_workflow(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        policy_output: Option<&Path>,
        apply: bool,
    ) -> Result<crate::WorkflowAuthorizationReport> {
        self.workflows
            .authorize_workflow(workflow_ref, workflow_inputs, policy_output, apply)
    }

    /// Plans or applies exact grants for prospective inputs without a full report or run.
    /// Publication is no-clobber and rolls back if effective-policy validation fails.
    pub fn authorize_workflow_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        policy_output: Option<&Path>,
        apply: bool,
    ) -> Result<crate::WorkflowAuthorizationSummary> {
        self.workflows.authorize_workflow_summary(
            workflow_ref,
            workflow_inputs,
            policy_output,
            apply,
        )
    }

    /// Revalidates current inputs/resources/policy and applies an unchanged reviewed full plan.
    /// Changed or blocked authority requires another review; no probes, runs, or writes on mismatch.
    pub fn apply_reviewed_workflow_authorization(
        &self,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        reviewed: &crate::WorkflowAuthorizationReport,
    ) -> Result<crate::WorkflowAuthorizationReport> {
        self.workflows
            .apply_reviewed_workflow_authorization(workflow_inputs, reviewed)
    }

    /// Revalidates current inputs/resources/policy and applies an unchanged reviewed compact plan.
    /// Changed or blocked authority requires another review; no probes, runs, or writes on mismatch.
    pub fn apply_reviewed_workflow_authorization_summary(
        &self,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        reviewed: &crate::WorkflowAuthorizationSummary,
    ) -> Result<crate::WorkflowAuthorizationSummary> {
        self.workflows
            .apply_reviewed_workflow_authorization_summary(workflow_inputs, reviewed)
    }

    /// Lists compiled workflow recipes from this context's home directory.
    pub fn list_workflows(&self) -> Result<Vec<InstalledWorkflow>> {
        self.workflows.list_workflows()
    }

    /// Loads one compiled workflow recipe plus a workflow summary.
    pub fn inspect_workflow(&self, workflow_ref: &str) -> Result<WorkflowInspection> {
        self.workflows.inspect_workflow(workflow_ref)
    }

    /// Removes one compiled workflow recipe from this context's home directory.
    pub fn uninstall_workflow(&self, workflow_ref: &str, force: bool) -> Result<String> {
        self.workflows.uninstall_workflow(workflow_ref, force)
    }

    /// Creates a prepared run from a compiled workflow reference without executing it.
    pub fn prepare_workflow(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
    ) -> Result<PrepareReport> {
        self.workflows
            .prepare_workflow(workflow_ref, run_ref, workflow_inputs)
    }

    /// Creates a prepared run with optional trusted-execution enforcement.
    pub fn prepare_workflow_with_trust(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<PrepareReport> {
        self.workflows.prepare_workflow_with_trust(
            workflow_ref,
            run_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
    }

    /// Prepares and then executes a run from a compiled workflow reference.
    pub fn launch_workflow(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
    ) -> Result<RunReport> {
        self.workflows
            .launch_workflow(workflow_ref, run_ref, workflow_inputs)
    }

    /// Starts a supplied compiled recipe without executing its steps.
    pub fn start_recipe(
        &self,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
    ) -> Result<RunReport> {
        application::runs::execution::start_recipe_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            recipe,
            run_ref,
            workflow_inputs,
        )
    }

    /// Launches a workflow with optional trusted-execution enforcement.
    pub fn launch_workflow_with_trust(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, transist_protocol::KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        self.workflows.launch_workflow_with_trust(
            workflow_ref,
            run_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
    }

    /// Executes a previously started run by run id.
    pub fn run_started_run(&self, run_id: &str) -> Result<RunReport> {
        self.runs.run_started_run(run_id)
    }
    /// Executes a previously started run with optional trusted-execution enforcement.
    pub fn run_started_run_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        self.runs
            .run_started_run_with_trust(run_id, trusted_execution, local_only)
    }
    /// Executes a previously prepared run while reusing its stored successful preflight.
    pub fn run_prepared_run(&self, run_id: &str) -> Result<RunReport> {
        self.runs.run_prepared_run(run_id)
    }
    /// Executes a previously prepared run with optional trusted-execution refresh.
    pub fn run_prepared_run_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        self.runs
            .run_prepared_run_with_trust(run_id, trusted_execution, local_only)
    }
    /// Runs the generic preflight routine for a persisted run without executing it.
    pub fn runs_preflight(&self, run_id: &str) -> Result<PreflightReport> {
        self.runs.runs_preflight(run_id)
    }
    /// Runs preflight with optional trusted-execution enforcement.
    pub fn runs_preflight_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<PreflightReport> {
        self.runs
            .runs_preflight_with_trust(run_id, trusted_execution, local_only)
    }
    /// Returns read-only capability allowlist guidance for a persisted run.
    pub fn run_capabilities(&self, run_id: &str) -> Result<crate::RunCapabilityReport> {
        self.runs.run_capabilities(run_id)
    }
    /// Records an approval decision for the current manual gate and resumes the run.
    pub fn approve_gate(&self, run_id: &str, gate_id: &str) -> Result<RunReport> {
        self.runs.approve_gate(run_id, gate_id)
    }

    /// Records an approval decision with explicit gate-decision metadata and resumes the run.
    pub fn approve_gate_with_input(
        &self,
        run_id: &str,
        gate_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        self.runs.approve_gate_with_input(run_id, gate_id, input)
    }
    /// Records an approval decision for the currently blocked manual gate inferred from the run.
    pub fn approve_current_gate(&self, run_id: &str) -> Result<RunReport> {
        self.runs.approve_current_gate(run_id)
    }

    /// Records an approval decision for the current gate with explicit metadata and resumes the run.
    pub fn approve_current_gate_with_input(
        &self,
        run_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        self.runs.approve_current_gate_with_input(run_id, input)
    }
    /// Records a rejection decision for the current manual gate and resumes the run.
    pub fn reject_gate(
        &self,
        run_id: &str,
        gate_id: &str,
        reason: Option<&str>,
    ) -> Result<RunReport> {
        self.runs.reject_gate(run_id, gate_id, reason)
    }

    /// Records a rejection decision with explicit metadata and resumes the run.
    pub fn reject_gate_with_input(
        &self,
        run_id: &str,
        gate_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        self.runs.reject_gate_with_input(run_id, gate_id, input)
    }
    /// Records a rejection decision for the currently blocked manual gate inferred from the run.
    pub fn reject_current_gate(&self, run_id: &str, reason: Option<&str>) -> Result<RunReport> {
        self.runs.reject_current_gate(run_id, reason)
    }

    /// Records a rejection decision for the current gate with explicit metadata and resumes the run.
    pub fn reject_current_gate_with_input(
        &self,
        run_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        self.runs.reject_current_gate_with_input(run_id, input)
    }
    /// Records a request-changes decision for the current manual gate and resumes the run.
    pub fn request_changes_gate(
        &self,
        run_id: &str,
        gate_id: &str,
        reason: Option<&str>,
    ) -> Result<RunReport> {
        self.runs.request_changes_gate(run_id, gate_id, reason)
    }

    /// Records a request-changes decision with explicit metadata and resumes the run.
    pub fn request_changes_gate_with_input(
        &self,
        run_id: &str,
        gate_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        self.runs
            .request_changes_gate_with_input(run_id, gate_id, input)
    }
    /// Records a request-changes decision for the currently blocked manual gate inferred from the run.
    pub fn request_changes_current_gate(
        &self,
        run_id: &str,
        reason: Option<&str>,
    ) -> Result<RunReport> {
        self.runs.request_changes_current_gate(run_id, reason)
    }

    /// Records a request-changes decision for the current gate with explicit metadata and resumes the run.
    pub fn request_changes_current_gate_with_input(
        &self,
        run_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        self.runs
            .request_changes_current_gate_with_input(run_id, input)
    }
    /// Persists a manual gate decision **without** resuming the run.
    pub fn record_gate_decision(
        &self,
        run_id: &str,
        gate_id: Option<&str>,
        decision: crate::application::runs::GateDecision,
        reason: Option<&str>,
    ) -> Result<crate::application::runs::GateDecisionReport> {
        self.runs
            .record_gate_decision(run_id, gate_id, decision, reason)
    }

    /// Persists a manual gate decision without resuming the run, with explicit metadata.
    pub fn record_gate_decision_with_input(
        &self,
        run_id: &str,
        gate_id: Option<&str>,
        decision: crate::application::runs::GateDecision,
        input: GateDecisionInput,
    ) -> Result<crate::application::runs::GateDecisionReport> {
        self.runs
            .record_gate_decision_with_input(run_id, gate_id, decision, input)
    }
    /// Resumes a failed run from its persisted run-scoped recipe snapshot.
    pub fn runs_resume(&self, run_id: &str) -> Result<RunReport> {
        self.runs.runs_resume(run_id)
    }
    /// Resumes a run with optional trusted-execution refresh.
    pub fn runs_resume_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        self.runs
            .runs_resume_with_trust(run_id, trusted_execution, local_only)
    }
    /// Executes terminal cleanup for a root run or the root run containing `run_id`.
    pub fn runs_cleanup(&self, run_id: &str) -> Result<RunReport> {
        self.runs.runs_cleanup(run_id)
    }
    /// Executes cleanup with optional trusted-execution refresh.
    pub fn runs_cleanup_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        self.runs
            .runs_cleanup_with_trust(run_id, trusted_execution, local_only)
    }
    /// Replays an existing run as a new run using its persisted recipe snapshot.
    pub fn runs_replay(&self, source_run_id: &str, new_run_ref: Option<&str>) -> Result<RunReport> {
        self.runs.runs_replay(source_run_id, new_run_ref)
    }
    /// Replays a run with optional trusted-execution refresh on the source run.
    pub fn runs_replay_with_trust(
        &self,
        source_run_id: &str,
        new_run_ref: Option<&str>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        self.runs
            .runs_replay_with_trust(source_run_id, new_run_ref, trusted_execution, local_only)
    }
    /// Replaces a run-scoped `recipe.yaml` snapshot with the current compiled registry recipe.
    pub fn runs_sync_recipe(
        &self,
        run_id: &str,
    ) -> Result<crate::application::runs::RunRecipeSyncReport> {
        self.runs.runs_sync_recipe(run_id)
    }
    /// Loads the live lease record for the root run tree containing `run_id`, when present.
    pub fn runs_lease_inspect(&self, run_id: &str) -> Result<Option<RunTreeLeaseRecord>> {
        self.runs.runs_lease_inspect(run_id)
    }
    /// Breaks the live lease for the root run tree containing `run_id`, when present.
    pub fn runs_lease_break(
        &self,
        run_id: &str,
        force_live_owner: bool,
    ) -> Result<Option<RunTreeLeaseRecord>> {
        self.runs.runs_lease_break(run_id, force_live_owner)
    }
    /// Reaps orphaned run-tree leases whose owning runtime process has exited.
    pub fn release_orphan_leases(&self) -> Result<Vec<RunTreeLeaseRecord>> {
        self.runs.release_orphan_leases()
    }
    /// Writes a durable cancellation request for the root run tree containing `run_id`.
    pub fn request_run_cancellation(&self, run_id: &str) -> Result<RunCancellationRequest> {
        self.runs.request_run_cancellation(run_id)
    }
    /// Lists known run identifiers from this context's home directory.
    pub fn runs_list(&self) -> Result<Vec<String>> {
        self.runs.runs_list()
    }
    /// Loads stored run state for a single run.
    pub fn runs_show(&self, run_id: &str) -> Result<transist_protocol::KernelValue> {
        self.runs.runs_show(run_id)
    }
    /// Returns up to `limit` latest runtime events for a run.
    pub fn runs_tail(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<transist_protocol::RuntimeEvent>> {
        self.runs.runs_tail(run_id, limit)
    }
    /// Exports a read-only evidence report for one run.
    pub fn run_evidence_export(&self, run_id: &str) -> Result<RunEvidenceExport> {
        self.runs.run_evidence_export(run_id)
    }
    /// Cancels the active invoke step in a run tree, if one is currently running.
    pub fn cancel_active_run_execution(
        &self,
        run_id: &str,
    ) -> Result<Option<application::runs::ActiveRunCancellation>> {
        self.runs.cancel_active_run_execution(run_id)
    }
    /// Installs a plugin from its manifest file into this context's home directory.
    ///
    /// Prefer this over [`Self::install_plugin_dir`] for new callers; pass the
    /// plugin's `plugin.yaml` path so install semantics match
    /// `workflows install --path --approve`.
    pub fn install_plugin_file(&self, manifest_file: &Path) -> Result<PluginInstallReport> {
        self.plugins.install_plugin_file(manifest_file)
    }
    /// Installs a plugin manifest file with optional trusted-execution verification.
    pub fn install_plugin_file_with_trust(
        &self,
        manifest_file: &Path,
        trusted_execution: bool,
    ) -> Result<PluginInstallReport> {
        self.plugins
            .install_plugin_file_with_trust(manifest_file, trusted_execution)
    }
    /// Installs a plugin manifest with optional trusted verification using a
    /// caller-selected detached signature source.
    pub fn install_plugin_file_with_trust_and_signature(
        &self,
        manifest_file: &Path,
        trusted_execution: bool,
        signature_path: Option<&Path>,
    ) -> Result<PluginInstallReport> {
        self.plugins.install_plugin_file_with_trust_and_signature(
            manifest_file,
            trusted_execution,
            signature_path,
        )
    }
    /// Installs a plugin directory into this context's home directory.
    ///
    /// Directory-based compatibility wrapper around [`Self::install_plugin_file`].
    pub fn install_plugin_dir(&self, plugin_dir: &Path) -> Result<PluginInstallReport> {
        self.plugins.install_plugin_dir(plugin_dir)
    }
    /// Installs a plugin directory with optional trusted-execution verification.
    pub fn install_plugin_dir_with_trust(
        &self,
        plugin_dir: &Path,
        trusted_execution: bool,
    ) -> Result<PluginInstallReport> {
        self.plugins
            .install_plugin_dir_with_trust(plugin_dir, trusted_execution)
    }
    /// Installs a plugin directory with optional trusted verification using a
    /// caller-selected detached signature source.
    pub fn install_plugin_dir_with_trust_and_signature(
        &self,
        plugin_dir: &Path,
        trusted_execution: bool,
        signature_path: Option<&Path>,
    ) -> Result<PluginInstallReport> {
        self.plugins.install_plugin_dir_with_trust_and_signature(
            plugin_dir,
            trusted_execution,
            signature_path,
        )
    }
    /// Lists installed plugins from this context's home directory.
    pub fn list_plugins(&self) -> Result<Vec<InstalledPlugin>> {
        self.plugins.list_plugins()
    }
    /// Imports one trust-root public key into this context's home directory.
    pub fn import_trust_root(&self, pubkey_path: &Path) -> Result<crate::ImportedTrustRoot> {
        self.trust.import_trust_root(pubkey_path)
    }
    /// Lists imported trust roots from this context's home directory.
    pub fn list_trust_roots(&self) -> Result<Vec<crate::ImportedTrustRoot>> {
        self.trust.list_trust_roots()
    }
    /// Loads one imported trust root from this context's home directory.
    pub fn inspect_trust_root(&self, key_id: &str) -> Result<crate::TrustRootRecord> {
        self.trust.inspect_trust_root(key_id)
    }
    /// Removes one imported trust root from this context's home directory.
    pub fn remove_trust_root(&self, key_id: &str) -> Result<crate::ImportedTrustRoot> {
        self.trust.remove_trust_root(key_id)
    }
    /// Loads manifests for all installed versions of a plugin id.
    pub fn inspect_plugin(&self, plugin_id: &str) -> Result<Vec<PluginManifest>> {
        self.plugins.inspect_plugin(plugin_id)
    }
    /// Removes one installed plugin version from this context's home directory.
    pub fn remove_plugin(&self, plugin_id: &str, plugin_version: &str) -> Result<()> {
        self.plugins.remove_plugin(plugin_id, plugin_version)
    }
    /// Removes all installed versions of a plugin from this context's home directory.
    pub fn uninstall_plugin(&self, plugin_id: &str) -> Result<Vec<String>> {
        self.plugins.uninstall_plugin(plugin_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_step_ids_returns_none_when_recipe_snapshot_is_absent() {
        let home = tempfile::tempdir().expect("home");
        let run_root = home.path().join("runs").join("run_run_missing_recipe");
        std::fs::create_dir_all(&run_root).expect("run root");
        let context = TransistContext::with_home(home.path(), home.path());

        assert_eq!(
            context
                .visible_step_ids("run_missing_recipe", &KernelValue::Null)
                .expect("visible steps"),
            None
        );
    }

    #[test]
    fn visible_step_ids_surfaces_corrupt_recipe_snapshot() {
        let home = tempfile::tempdir().expect("home");
        let run_root = home.path().join("runs").join("run_run_corrupt_recipe");
        std::fs::create_dir_all(&run_root).expect("run root");
        std::fs::write(run_root.join("recipe.yaml"), [0xff]).expect("recipe");
        let context = TransistContext::with_home(home.path(), home.path());

        assert!(
            context
                .visible_step_ids("run_corrupt_recipe", &KernelValue::Null)
                .is_err()
        );
    }
}
