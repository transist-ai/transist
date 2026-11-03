// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use transist_protocol::KernelValue;

use crate::application::runs::{PrepareReport, RunReport};
use crate::application::workflows::{
    AuthoredWorkflowValidation, InstalledWorkflow, WorkflowInspection, WorkflowInstallLock,
    WorkflowInstallOwnershipJournal, WorkflowInstallReport, WorkflowPackageSnapshot,
};

fn verify_reviewed_authorization(
    status: super::WorkflowAuthorizationStatus,
    policy: Option<&serde_json::Value>,
    output: Option<&Path>,
    reviewed_policy: Option<&serde_json::Value>,
    reviewed_output: Option<&Path>,
) -> Result<()> {
    if status != super::WorkflowAuthorizationStatus::Planned
        || policy != reviewed_policy
        || output != reviewed_output
        || policy.is_none()
        || output.is_none()
    {
        anyhow::bail!(
            "workflow authorization changed since review; review current inputs, resources, and policy again; no grants written"
        );
    }
    Ok(())
}

/// Workflow compile, validate, prepare, and launch operations.
#[derive(Debug, Clone)]
pub struct WorkflowService {
    pub(crate) repo_root: PathBuf,
    pub(crate) home_dir: PathBuf,
    pub(crate) router_cache: Arc<crate::composition::router_cache::RouterCache>,
}

impl PartialEq for WorkflowService {
    fn eq(&self, other: &Self) -> bool {
        self.repo_root == other.repo_root && self.home_dir == other.home_dir
    }
}

impl Eq for WorkflowService {}

impl WorkflowService {
    /// Validates an authored file read-only, retaining grouped diagnostics and input names only.
    pub fn validate_flow_file_summary(
        &self,
        flow_path: &Path,
    ) -> Result<super::WorkflowCheckSummary> {
        Ok(super::evaluate_authored_workflow(
            &self.home_dir,
            flow_path,
            super::ReportDetail::Summary,
        )?
        .summary)
    }

    /// Binds prospective inputs and collects static diagnostics without creating a full report or run.
    pub fn validate_bound_workflow_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<super::WorkflowCheckSummary> {
        Ok(super::validate_bound_workflow_in_home_inner(
            &self.repo_root,
            &self.home_dir,
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
            None,
            None,
            super::ReportDetail::Summary,
        )?
        .summary)
    }

    /// Evaluates birth-known readiness without persisting inputs or creating a full report or run.
    /// Binding and evaluation errors become typed invalid-definition summaries.
    pub fn workflow_readiness_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> super::WorkflowCheckSummary {
        self.evaluation_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
            super::WorkflowEvaluationMode::LiveReadiness,
        )
    }

    /// Validates prospective inputs and current authority/secret availability without probes or writes.
    /// Evaluation errors become typed invalid-definition summaries; no secret value is resolved.
    pub fn workflow_validation_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> super::WorkflowCheckSummary {
        self.evaluation_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
            super::WorkflowEvaluationMode::Validation,
        )
    }

    fn evaluation_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
        evaluation_mode: super::WorkflowEvaluationMode,
    ) -> super::WorkflowCheckSummary {
        match super::evaluate_workflow_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            workflow_inputs,
            crate::application::runs::execution::RunModes {
                trusted_execution,
                local_only,
            },
            evaluation_mode,
            super::ReportDetail::Summary,
        ) {
            Ok(evaluation) => evaluation.summary,
            Err(error) => {
                let mut summary =
                    super::WorkflowCheckSummary::invalid_definition(workflow_ref, &error);
                summary.evaluation_mode = evaluation_mode;
                if evaluation_mode == super::WorkflowEvaluationMode::Validation {
                    summary.mode = super::WorkflowCheckMode::Bound;
                }
                summary
            }
        }
    }

    /// Evaluates readiness once, returning full evidence and its incrementally collected summary.
    /// Evaluation errors produce matching invalid-definition results; no run is created.
    pub fn workflow_readiness_report_with_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> (super::WorkflowReadinessReport, super::WorkflowCheckSummary) {
        self.evaluation_report_with_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
            super::WorkflowEvaluationMode::LiveReadiness,
        )
    }

    /// Returns validation evidence and its compact view from one probe-free, read-only evaluation.
    /// Inputs remain in memory, secrets are checked by name only, and no run is created.
    pub fn workflow_validation_report_with_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> (super::WorkflowReadinessReport, super::WorkflowCheckSummary) {
        self.evaluation_report_with_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
            super::WorkflowEvaluationMode::Validation,
        )
    }

    /// Returns full probe-free validation for supplied inputs and current modes, without writes.
    /// Errors are typed invalid-definition reports; passing validation is not live readiness.
    pub fn workflow_validation_report(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> super::WorkflowReadinessReport {
        self.workflow_validation_report_with_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
        .0
    }

    fn evaluation_report_with_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
        evaluation_mode: super::WorkflowEvaluationMode,
    ) -> (super::WorkflowReadinessReport, super::WorkflowCheckSummary) {
        match super::evaluate_workflow_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            workflow_inputs,
            crate::application::runs::execution::RunModes {
                trusted_execution,
                local_only,
            },
            evaluation_mode,
            super::ReportDetail::Full,
        ) {
            Ok(evaluation) => (
                super::WorkflowReadinessReport::from_validation_in_mode(
                    evaluation.full.expect("full detail"),
                    evaluation_mode,
                ),
                evaluation.summary,
            ),
            Err(error) => {
                let mut report =
                    super::WorkflowReadinessReport::invalid_definition(workflow_ref, &error);
                let mut summary =
                    super::WorkflowCheckSummary::invalid_definition(workflow_ref, &error);
                report.evaluation_mode = evaluation_mode;
                summary.evaluation_mode = evaluation_mode;
                if evaluation_mode == super::WorkflowEvaluationMode::Validation {
                    summary.mode = super::WorkflowCheckMode::Bound;
                }
                (report, summary)
            }
        }
    }
    pub(crate) fn new(
        repo_root: PathBuf,
        home_dir: PathBuf,
        router_cache: Arc<crate::composition::router_cache::RouterCache>,
    ) -> Self {
        Self {
            repo_root,
            home_dir,
            router_cache,
        }
    }

    /// Compiles a flow file and writes its recipe artifact under this context's home.
    pub fn compile_flow_file(&self, flow_path: &Path) -> Result<PathBuf> {
        crate::application::workflows::compile_flow_file_in_home(
            &self.repo_root,
            &self.home_dir,
            flow_path,
        )
    }

    /// Compiles a flow file with optional trusted-execution install verification.
    pub fn compile_flow_file_with_trust(
        &self,
        flow_path: &Path,
        trusted_execution: bool,
    ) -> Result<crate::application::workflows::WorkflowCompileResult> {
        let _ = &self.repo_root;
        crate::application::workflows::compile_flow_file_in_home_with_trust(
            &self.repo_root,
            &self.home_dir,
            flow_path,
            trusted_execution,
        )
    }

    /// Returns read-only trusted compile metadata status for an authored workflow.
    pub fn workflow_compile_trust_status(
        &self,
        flow_path: &Path,
    ) -> Result<crate::application::workflows::WorkflowCompileTrustStatus> {
        crate::application::workflows::workflow_compile_trust_status_in_home(
            &self.home_dir,
            flow_path,
        )
    }

    /// Compiles and durably records a workflows install in this context's home.
    pub fn install_flow_file_with_trust(
        &self,
        flow_path: &Path,
        trusted_execution: bool,
    ) -> Result<WorkflowInstallReport> {
        crate::application::workflows::install_flow_file_in_home_with_trust(
            &self.repo_root,
            &self.home_dir,
            flow_path,
            trusted_execution,
        )
    }

    /// Installs a workflow with optional trusted verification using a
    /// caller-selected detached signature source.
    pub fn install_flow_file_with_trust_and_signature(
        &self,
        flow_path: &Path,
        trusted_execution: bool,
        signature_path: Option<&Path>,
    ) -> Result<WorkflowInstallReport> {
        crate::application::workflows::install_flow_file_in_home_with_trust_and_signature(
            &self.repo_root,
            &self.home_dir,
            flow_path,
            trusted_execution,
            signature_path,
        )
    }

    /// Installs exact captured package bytes while the caller owns the home install lock.
    pub fn install_package_snapshot_with_trust(
        &self,
        package: &WorkflowPackageSnapshot,
        trusted_execution: bool,
        install_lock: &WorkflowInstallLock,
    ) -> Result<WorkflowInstallReport> {
        let mut ownership = WorkflowInstallOwnershipJournal::new();
        let result = self.install_package_snapshot_with_trust_and_ownership(
            package,
            trusted_execution,
            install_lock,
            &mut ownership,
        );
        match result {
            Ok(report) => Ok(report),
            Err(error) => match ownership.rollback(&self.home_dir) {
                Ok(()) => Err(error),
                Err(rollback) => {
                    anyhow::bail!("workflow install failed: {error:#}; additionally, {rollback:#}")
                }
            },
        }
    }

    /// Installs exact captured bytes and records each newly published artifact.
    pub fn install_package_snapshot_with_trust_and_ownership(
        &self,
        package: &WorkflowPackageSnapshot,
        trusted_execution: bool,
        install_lock: &WorkflowInstallLock,
        ownership: &mut WorkflowInstallOwnershipJournal,
    ) -> Result<WorkflowInstallReport> {
        crate::application::workflows::install_workflow_package_snapshot_in_home_with_trust(
            &self.home_dir,
            package,
            trusted_execution,
            install_lock,
            ownership,
        )
    }

    /// Validates a flow file against the flow schema and local plugin/toolchain state.
    pub fn validate_flow_file(&self, flow_path: &Path) -> Result<AuthoredWorkflowValidation> {
        crate::application::workflows::validate_flow_file_in_home(
            &self.repo_root,
            &self.home_dir,
            flow_path,
        )
    }

    /// Binds and validates an installed workflow without creating a run.
    pub fn validate_bound_workflow(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::application::workflows::BoundWorkflowValidation> {
        crate::application::workflows::validate_bound_workflow_in_home(
            &self.repo_root,
            &self.home_dir,
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
    }

    /// Evaluates complete workflow readiness without creating a run or dispatching an action.
    pub fn evaluate_readiness(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<crate::application::workflows::BoundWorkflowValidation> {
        crate::application::workflows::evaluate_workflow_readiness_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            workflow_inputs,
            crate::application::runs::execution::RunModes {
                trusted_execution,
                local_only,
            },
        )
    }

    /// Evaluates a prospective workflow run and returns the shared typed report.
    ///
    /// Evaluation is read-only: it binds the supplied inputs and probes
    /// birth-known state without persisting input values or creating a run.
    pub fn workflow_readiness_report(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> crate::application::workflows::WorkflowReadinessReport {
        self.workflow_readiness_report_with_summary(
            workflow_ref,
            workflow_inputs,
            trusted_execution,
            local_only,
        )
        .0
    }

    /// Plans or applies one exact policy fragment for a validation authority gap without probes.
    ///
    /// The plan binds only the supplied prospective inputs. Applying is
    /// no-clobber and validates the effective policy after publication.
    pub fn authorize_workflow(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        policy_output: Option<&Path>,
        apply: bool,
    ) -> Result<crate::application::workflows::WorkflowAuthorizationReport> {
        let readiness =
            self.workflow_validation_report(workflow_ref, workflow_inputs.clone(), false, false);
        let report = crate::application::workflows::plan_workflow_authorization_in_home(
            &self.home_dir,
            readiness,
            policy_output,
        )?;
        if apply && report.status == super::WorkflowAuthorizationStatus::Planned {
            self.apply_reviewed_workflow_authorization(workflow_inputs, &report)
        } else {
            Ok(report)
        }
    }

    /// Plans or applies exact grants for supplied inputs, retaining only compact diagnostics.
    /// No full report or run is constructed; application is no-clobber with policy rollback.
    pub fn authorize_workflow_summary(
        &self,
        workflow_ref: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        policy_output: Option<&Path>,
        apply: bool,
    ) -> Result<super::WorkflowAuthorizationSummary> {
        if apply {
            let reviewed = self.authorize_workflow_summary(
                workflow_ref,
                workflow_inputs.clone(),
                policy_output,
                false,
            )?;
            if reviewed.status != super::WorkflowAuthorizationStatus::Planned {
                return Ok(reviewed);
            }
            return self.apply_reviewed_workflow_authorization_summary(workflow_inputs, &reviewed);
        }
        let (readiness, missing) = match super::evaluate_workflow_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            workflow_inputs,
            crate::application::runs::execution::RunModes {
                trusted_execution: false,
                local_only: false,
            },
            super::WorkflowEvaluationMode::Validation,
            super::ReportDetail::Summary,
        ) {
            Ok(evaluation) => (evaluation.summary, evaluation.missing),
            Err(error) => {
                let mut summary =
                    super::WorkflowCheckSummary::invalid_definition(workflow_ref, &error);
                summary.evaluation_mode = super::WorkflowEvaluationMode::Validation;
                summary.mode = super::WorkflowCheckMode::Bound;
                (summary, Vec::new())
            }
        };
        super::authorize_summary_in_home(&self.home_dir, readiness, &missing, policy_output, apply)
    }

    /// Revalidates supplied current inputs, resources, and policy against a reviewed full plan.
    /// Publishes only an unchanged exact fragment/destination; changed or blocked plans error without writes.
    pub fn apply_reviewed_workflow_authorization(
        &self,
        workflow_inputs: BTreeMap<String, KernelValue>,
        reviewed: &super::WorkflowAuthorizationReport,
    ) -> Result<super::WorkflowAuthorizationReport> {
        if reviewed.status != super::WorkflowAuthorizationStatus::Planned {
            return self.authorize_workflow(
                &reviewed.workflow,
                workflow_inputs,
                reviewed.policy_output.as_deref(),
                false,
            );
        }
        let current = self.authorize_workflow(
            &reviewed.workflow,
            workflow_inputs,
            reviewed.policy_output.as_deref(),
            false,
        )?;
        verify_reviewed_authorization(
            current.status,
            current.policy.as_ref(),
            current.policy_output.as_deref(),
            reviewed.policy.as_ref(),
            reviewed.policy_output.as_deref(),
        )?;
        super::apply_workflow_authorization_in_home(&self.home_dir, current)
    }

    /// Revalidates supplied current inputs/resources/policy against a reviewed compact plan, without probes.
    /// Publishes only its unchanged exact fragment/destination; errors require another review and write nothing.
    pub fn apply_reviewed_workflow_authorization_summary(
        &self,
        workflow_inputs: BTreeMap<String, KernelValue>,
        reviewed: &super::WorkflowAuthorizationSummary,
    ) -> Result<super::WorkflowAuthorizationSummary> {
        if reviewed.status != super::WorkflowAuthorizationStatus::Planned {
            return self.authorize_workflow_summary(
                &reviewed.workflow,
                workflow_inputs,
                reviewed.policy_output.as_deref(),
                false,
            );
        }
        let mut current = self.authorize_workflow_summary(
            &reviewed.workflow,
            workflow_inputs,
            reviewed.policy_output.as_deref(),
            false,
        )?;
        verify_reviewed_authorization(
            current.status,
            current.policy.as_ref(),
            current.policy_output.as_deref(),
            reviewed.policy.as_ref(),
            reviewed.policy_output.as_deref(),
        )?;
        super::publish_authorization(
            &self.home_dir,
            current.policy.as_ref(),
            current.policy_output.as_deref(),
        )?;
        current.status = super::WorkflowAuthorizationStatus::Applied;
        Ok(current)
    }

    /// Lists compiled workflow recipes from this context's home directory.
    pub fn list_workflows(&self) -> Result<Vec<InstalledWorkflow>> {
        crate::application::workflows::list_workflows_in_home(&self.repo_root, &self.home_dir)
    }

    /// Loads one compiled workflow recipe plus a workflow summary.
    pub fn inspect_workflow(&self, workflow_ref: &str) -> Result<WorkflowInspection> {
        crate::application::workflows::inspect_workflow_in_home(
            &self.repo_root,
            &self.home_dir,
            workflow_ref,
        )
    }

    /// Removes one compiled workflow recipe from this context's home directory.
    pub fn uninstall_workflow(&self, workflow_ref: &str, force: bool) -> Result<String> {
        crate::application::workflows::uninstall_workflow_in_home(
            &self.repo_root,
            &self.home_dir,
            workflow_ref,
            force,
        )
    }

    /// Creates a prepared run from a compiled workflow reference without executing it.
    pub fn prepare_workflow(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
    ) -> Result<PrepareReport> {
        crate::application::workflows::prepare_workflow_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            run_ref,
            workflow_inputs,
        )
    }

    /// Creates a prepared run with optional trusted-execution enforcement.
    pub fn prepare_workflow_with_trust(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<PrepareReport> {
        crate::application::workflows::prepare_workflow_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            run_ref,
            workflow_inputs,
            crate::application::runs::execution::RunModes {
                trusted_execution,
                local_only,
            },
        )
    }

    /// Prepares and then executes a run from a compiled workflow reference.
    pub fn launch_workflow(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
    ) -> Result<RunReport> {
        crate::application::workflows::launch_workflow_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            run_ref,
            workflow_inputs,
        )
    }

    /// Launches a workflow with optional trusted-execution enforcement.
    pub fn launch_workflow_with_trust(
        &self,
        workflow_ref: &str,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        crate::application::workflows::launch_workflow_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            workflow_ref,
            run_ref,
            workflow_inputs,
            crate::application::runs::execution::RunModes {
                trusted_execution,
                local_only,
            },
        )
    }
}
