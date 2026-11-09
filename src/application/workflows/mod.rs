// SPDX-License-Identifier: MPL-2.0
//! Workflow-oriented application operations.
//!
//! This module owns authored-flow validation, recipe compilation, and run preparation
//! from compiled workflow references.

mod errors;
mod in_use;
mod install_index;
mod reports;
mod validation_reuse;
pub use errors::{
    WorkflowInputBindingError, WorkflowInputBindingErrorKind, WorkflowUninstallError,
};
use reports::{
    CheckCollector, ReportDetail, evaluate_authored_workflow, evaluate_workflow_with_cache,
    validate_bound_workflow_in_home_inner,
};
pub use reports::{
    WorkflowCheckFinding, WorkflowCheckMode, WorkflowCheckSummary, WorkflowEvaluationMode,
};

/// Workflow service facade for compilation, validation, and start operations.
pub mod service;
pub use service::WorkflowService;

use crate::application::runs;
use crate::composition::router_cache::RouterCache;
use crate::composition::{LocalHomeSettings, LocalPluginMetadata, LocalPromptFiles};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use transist_core::ports::SecretProviderPort;
use transist_core::{
    ExecutionRequestContext, PreflightInvocation, PreflightPort, RecipeInputBindingError,
    RecipeInputBindingErrorKind, RemoteSecretPolicy, bind_and_materialize_recipe,
    bind_recipe_inputs, build_gate_policy_descriptor_for_workflow, build_step_policy_descriptor,
};
use transist_prerequisites::{
    EvaluationContext as PrerequisiteEvaluationContext, PrerequisiteScope, PrerequisiteStatus,
    evaluate_prerequisites,
};
use transist_protocol::{
    CapabilityName, CapabilityRequest, InvokePaths, KernelValue, Recipe, RecipeStep,
    StepExecutionDescriptor, WorkflowRef,
};

/// One immutable workflow definition and its detached signature sidecar.
#[derive(Debug, Clone)]
pub struct WorkflowPackageSnapshot {
    definition: transist_compiler::WorkflowDefinitionSnapshot,
    source_path: PathBuf,
    signature: Option<Vec<u8>>,
    signature_source_path: Option<PathBuf>,
}

/// Detached signature bytes and their optional explicit source location for one compile.
#[derive(Debug, Default)]
struct WorkflowSignatureInput<'a> {
    bytes: Option<&'a [u8]>,
    source_path: Option<&'a Path>,
}

/// Exclusive home-scoped ownership of workflow install mutations.
pub struct WorkflowInstallLock {
    home_dir: PathBuf,
    _guard: transist_storage_local::DirectoryLock,
}

/// Tracks durable workflow-install artifacts created by one locked operation.
#[derive(Debug, Default)]
pub struct WorkflowInstallOwnershipJournal {
    owned_files: BTreeSet<PathBuf>,
    owned_index_entry: Option<(String, String, String)>,
    upgraded_index_entry: Option<(
        install_index::WorkflowInstallIndexEntry,
        install_index::WorkflowInstallIndexEntry,
    )>,
    remove_empty_index: bool,
}

impl WorkflowInstallOwnershipJournal {
    /// Creates an empty ownership journal for one workflow install attempt.
    pub fn new() -> Self {
        Self::default()
    }

    /// Removes newly published artifacts and restores receipt upgrades owned by this attempt.
    pub fn rollback(&self, home_dir: &Path) -> Result<()> {
        let mut failures = Vec::new();
        if let Some((previous, published)) = &self.upgraded_index_entry
            && let Err(error) = install_index::restore_install_entry(home_dir, previous, published)
        {
            failures.push(format!("receipt rollback: {error:#}"));
        }
        for path in &self.owned_files {
            if let Err(error) = transist_storage_local::remove_file_anchored(path) {
                let missing = error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
                if !missing {
                    failures.push(format!("{}: {error:#}", path.display()));
                }
            }
        }
        if let Some((workflow_id, workflow_version, digest)) = &self.owned_index_entry
            && let Err(error) = install_index::rollback_install_entry(
                home_dir,
                workflow_id,
                workflow_version,
                digest,
                self.remove_empty_index,
            )
        {
            failures.push(format!(
                "{}: {error:#}",
                home_dir
                    .join("registry/workflow-install-index.json")
                    .display()
            ));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("workflow install rollback failed: {}", failures.join("; "))
        }
    }

    fn record_file_if_created(&mut self, path: &Path, existed: bool) {
        if !existed {
            self.owned_files.insert(path.to_path_buf());
        }
    }

    fn record_index_publication(
        &mut self,
        entry: &install_index::WorkflowInstallIndexEntry,
        entry_created: bool,
        index_created: bool,
    ) {
        if entry_created {
            self.owned_index_entry = Some((
                entry.workflow_id.clone(),
                entry.workflow_version.clone(),
                entry.workflow_definition_sha256.clone(),
            ));
            self.remove_empty_index = index_created;
        }
    }
}

/// Captures a workflow package exactly once through no-follow reads.
pub fn capture_workflow_package(flow_path: &Path) -> Result<WorkflowPackageSnapshot> {
    let definition = transist_compiler::capture_workflow_definition_with_signature(flow_path)?;
    let source_path = definition.workflow_root.join(&definition.top_level);
    let signature = definition.signature.clone();
    Ok(WorkflowPackageSnapshot {
        definition,
        source_path,
        signature,
        signature_source_path: None,
    })
}

/// Captures a workflow package once, optionally reading its detached signature
/// from an explicit source instead of the conventional sibling sidecar.
pub fn capture_workflow_package_with_signature(
    flow_path: &Path,
    signature_path: Option<&Path>,
) -> Result<WorkflowPackageSnapshot> {
    let Some(signature_path) = signature_path else {
        return capture_workflow_package(flow_path);
    };
    let definition = transist_compiler::capture_workflow_definition(flow_path)?;
    let signature_source_path = absolute_signature_path(signature_path)?;
    let signature = fs::read(&signature_source_path).with_context(|| {
        format!(
            "failed reading workflow signature '{}'",
            signature_source_path.display()
        )
    })?;
    let source_path = definition.workflow_root.join(&definition.top_level);
    Ok(WorkflowPackageSnapshot {
        definition,
        source_path,
        signature: Some(signature),
        signature_source_path: Some(signature_source_path),
    })
}

fn absolute_signature_path(signature_path: &Path) -> Result<PathBuf> {
    if signature_path.is_absolute() {
        return Ok(signature_path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .context("failed resolving current directory for workflow signature")?
        .join(signature_path))
}

/// Acquires the lock shared by ordinary workflow install and promotion.
pub fn lock_workflow_installation(home_dir: &Path) -> Result<WorkflowInstallLock> {
    Ok(WorkflowInstallLock {
        home_dir: home_dir.to_path_buf(),
        _guard: transist_storage_local::lock_directory(home_dir)?,
    })
}

impl WorkflowPackageSnapshot {
    /// Returns the exact identity parsed from the captured top-level workflow.
    pub fn workflow_ref(&self) -> Result<String> {
        let flow = self.definition.top_level_flow()?;
        Ok(format!("{}@{}", flow.id, flow.version))
    }

    /// Returns the canonical digest of the captured workflow definition bytes.
    pub fn definition_sha256(&self) -> &str {
        &self.definition.digest
    }

    /// Verifies the exact captured definition and sidecar bytes.
    pub fn verify_signature(
        &self,
        home_dir: &Path,
    ) -> Result<transist_core::SignatureVerification> {
        let Some(signature) = self.signature.as_deref() else {
            return Ok(transist_core::SignatureVerification {
                verified: false,
                key_id: None,
                digest_matched: false,
                reason_code: Some("unsigned".to_string()),
            });
        };
        crate::trust::verify_workflow_snapshot_signature_bytes(
            home_dir,
            &self.definition,
            signature,
        )
    }

    /// Writes and relocates the captured package without re-reading its source.
    pub fn materialize(&self, destination: &Path) -> Result<Self> {
        fs::create_dir(destination).with_context(|| {
            format!(
                "failed creating workflow package snapshot: {}",
                destination.display()
            )
        })?;
        let anchored = transist_storage_local::AnchoredDir::open(destination)?;
        for asset in &self.definition.assets {
            anchored.write_new(Path::new(&asset.relative_path), &asset.bytes)?;
        }
        let top_level = Path::new(&self.definition.top_level);
        if let Some(signature) = &self.signature {
            let name = transist_core::signature_sidecar_path(top_level);
            anchored.write_new(&name, signature)?;
        }
        let mut relocated = self.clone();
        relocated.definition.workflow_root = destination.to_path_buf();
        relocated.source_path = destination.join(top_level);
        relocated.signature_source_path = None;
        Ok(relocated)
    }
}

/// Removes only the install-index entry matching this promotion's exact digest.
pub fn rollback_workflow_install_index(
    home_dir: &Path,
    workflow_ref: &str,
    expected_digest: &str,
    remove_empty_index: bool,
) -> Result<()> {
    let (workflow_id, workflow_version) = workflow_ref
        .split_once('@')
        .context("workflow ref must use '<id>@<version>'")?;
    install_index::rollback_install_entry(
        home_dir,
        workflow_id,
        workflow_version,
        expected_digest,
        remove_empty_index,
    )
}

/// Reports whether the durable install index already owns a workflow reference.
pub fn workflow_install_index_contains(home_dir: &Path, workflow_ref: &str) -> Result<bool> {
    install_index::contains_install_entry(home_dir, workflow_ref)
}

/// Atomically creates a promotion-owned file beneath the active runtime home.
pub fn write_workflow_promotion_file(
    home_dir: &Path,
    destination: &Path,
    contents: &[u8],
) -> Result<()> {
    let relative = destination.strip_prefix(home_dir).with_context(|| {
        format!(
            "workflow promotion destination is outside the runtime home: {}",
            destination.display()
        )
    })?;
    transist_storage_local::AnchoredDir::open(home_dir)?.write_new(relative, contents)
}

/// Removes one promotion-owned file without following symlinks.
pub fn remove_workflow_promotion_file(path: &Path) -> Result<()> {
    transist_storage_local::remove_file_anchored(path)
}

const MAX_SUBFLOW_VALIDATION_DEPTH: usize = 8;

pub use runs::{PrepareReport, RunReport};
use transist_inspection::{InspectionWorkflowSummary, WorkflowInputSummary};

/// One compiled workflow recipe discovered in a local Transist home.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledWorkflow {
    /// Stable workflow id without version decoration.
    pub id: String,
    /// Stable workflow version string.
    pub version: String,
    /// Absolute path to the compiled recipe file.
    pub path: PathBuf,
}

/// Serializable summary emitted when a workflow is installed into the local registry.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowInstallReport {
    /// Stable workflow id.
    pub workflow_id: String,
    /// Stable workflow version.
    pub workflow_version: String,
    /// Canonical authored-workflow digest captured at install time.
    pub workflow_definition_sha256: String,
    /// Absolute path to the compiled recipe stored in the local registry.
    pub recipe_path: PathBuf,
    /// Absolute path to the authored workflow source file used at install time.
    pub workflow_source_path: PathBuf,
    /// Signer key id when trusted-execution verified a detached signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key_id: Option<String>,
    /// Whether trusted-execution verification was required during install.
    pub trusted_execution: bool,
}

impl WorkflowInstallReport {
    /// Returns whether this install uses the reserved local test-workflow version.
    pub fn is_test_workflow(&self) -> bool {
        crate::is_test_workflow(&self.workflow_version)
    }
}

/// Serializable summary of one declared workflow input used by workflow inspection.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowInspectionInputSummary {
    /// Input name.
    pub name: String,
    /// Declared kernel value type.
    pub value_type: String,
    /// Whether the caller must supply the input.
    pub required: bool,
    /// Whether the input declares a default value.
    pub has_default: bool,
    /// Optional human-oriented input description.
    pub description: Option<String>,
}

impl From<WorkflowInputSummary> for WorkflowInspectionInputSummary {
    fn from(value: WorkflowInputSummary) -> Self {
        Self {
            name: value.name,
            value_type: value.value_type,
            required: value.required,
            has_default: value.has_default,
            description: value.description,
        }
    }
}

/// Serializable workflow summary returned by `workflows inspect`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowInspectionSummary {
    /// Stable workflow id.
    pub workflow_id: String,
    /// Stable workflow version.
    pub workflow_version: String,
    /// Source file reference when one was discovered.
    pub source_ref: Option<String>,
    /// Optional workflow title derived from source content.
    pub title: Option<String>,
    /// Declared workflow inputs.
    pub inputs: Vec<WorkflowInspectionInputSummary>,
    /// Whether a compiled recipe file is present.
    pub compiled_recipe_available: bool,
    /// Whether validation and local prerequisites indicate the workflow can run.
    pub runnable_in_principle: bool,
    /// Number of authored steps.
    pub step_count: usize,
    /// Number of root-level authored steps.
    pub root_step_count: usize,
    /// Whether this workflow is referenced as a subflow by another discovered workflow.
    pub is_subflow: bool,
}

impl From<InspectionWorkflowSummary> for WorkflowInspectionSummary {
    fn from(value: InspectionWorkflowSummary) -> Self {
        Self {
            workflow_id: value.workflow_id,
            workflow_version: value.workflow_version,
            source_ref: value.source_ref,
            title: value.title,
            inputs: value.inputs.into_iter().map(Into::into).collect(),
            compiled_recipe_available: value.compiled_recipe_available,
            runnable_in_principle: value.runnable_in_principle,
            step_count: value.step_count,
            root_step_count: value.root_step_count,
            is_subflow: value.is_subflow,
        }
    }
}

impl WorkflowInspectionSummary {
    fn from_recipe(recipe: &Recipe) -> Self {
        Self {
            workflow_id: recipe.workflow.id.clone(),
            workflow_version: recipe.workflow.version.clone(),
            source_ref: recipe.source_dir.clone(),
            title: None,
            inputs: recipe
                .inputs
                .iter()
                .map(|(name, spec)| WorkflowInspectionInputSummary {
                    name: name.clone(),
                    value_type: spec.value_type.clone(),
                    required: spec.required,
                    has_default: spec.default.is_some(),
                    description: None,
                })
                .collect(),
            compiled_recipe_available: true,
            runnable_in_principle: true,
            step_count: recipe.steps.len(),
            root_step_count: recipe.steps.len(),
            is_subflow: false,
        }
    }
}

/// Compiled workflow recipe plus a CLI-safe summary for one workflow reference.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorkflowInspection {
    /// Stable workflow id.
    pub id: String,
    /// Stable workflow version.
    pub version: String,
    /// Absolute path to the compiled recipe file on disk.
    pub path: PathBuf,
    /// Compiled recipe loaded from the local registry.
    pub recipe: Recipe,
    /// Serializable summary derived from local workflow inspection.
    pub summary: WorkflowInspectionSummary,
}

/// Status of an application-side static validation check or report.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    /// The condition was evaluated and passed.
    Passed,
    /// Declared data dependency; full input checks occur immediately before dispatch.
    DeferredToDispatch,
    /// The condition was evaluated and failed.
    Failed,
    /// The condition was evaluated and produced a non-blocking warning.
    Warning,
    /// The condition depends on runtime or external state unavailable to static validation.
    NotEvaluated,
}

impl ValidationStatus {
    /// Returns the stable lowercase wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::DeferredToDispatch => "deferred_to_dispatch",
            Self::Failed => "failed",
            Self::Warning => "warning",
            Self::NotEvaluated => "not_evaluated",
        }
    }
}

/// One stable, validation-specific finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationCheck {
    /// Stable machine-readable finding code.
    pub code: String,
    /// Evaluation outcome.
    pub status: ValidationStatus,
    /// Root-relative step path, including subflow prefixes when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_path: Option<String>,
    /// Human-readable finding detail.
    pub message: String,
    /// Exact or actionable remediation when one is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Read-only result from binding and validating an installed workflow.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BoundWorkflowValidation {
    /// Exact workflow identity validated from the installed recipe.
    pub workflow_ref: String,
    /// Canonical workspace selected from the bound workflow authority.
    pub workspace_dir: PathBuf,
    /// Aggregate validation status.
    pub status: ValidationStatus,
    /// Static validation findings.
    pub checks: Vec<ValidationCheck>,
    /// Reachable normalized authority requested by this workflow tree.
    pub required_capabilities: Vec<CapabilityRequest>,
    /// Required authority covered by the effective policy.
    pub granted_capabilities: Vec<CapabilityRequest>,
    /// Required authority not covered by the effective policy.
    pub missing_capabilities: Vec<CapabilityRequest>,
    /// Exact root-workflow-bound policy fragment suggested for missing authority.
    pub policy_suggestion: crate::CapabilityPolicyHint,
}

impl BoundWorkflowValidation {
    /// Returns whether no static validation check failed.
    pub fn is_valid(&self) -> bool {
        self.status != ValidationStatus::Failed
    }
}

/// Stable outcome of a prospective workflow invocation.
///
/// Readiness is intentionally distinct from static validation: it describes
/// whether the exact supplied inputs may create a new run after policy, secret,
/// prerequisite, and adapter checks have been evaluated.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowReadinessStatus {
    /// All birth-known checks passed and a new run may be created.
    Ready,
    /// The workflow remains inspectable but has non-deliverable pending checks.
    NotReady,
    /// A deterministic policy, trust, dependency, or external blocker prevents delivery.
    NotDeliverable,
    /// The installed workflow or its supplied inputs could not be evaluated.
    InvalidDefinition,
}

/// Typed, read-only validation or live-readiness evidence for one prospective workflow run.
///
/// This report never contains workflow input values and never creates a run.
/// CLI and UI callers may add transport-specific fingerprints around this
/// application-owned diagnostic without reimplementing its status mapping.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowReadinessReport {
    /// Stable readiness protocol identifier.
    pub schema: &'static str,
    /// Installed workflow identity that was evaluated.
    pub workflow: String,
    /// Aggregate prospective-run outcome.
    pub status: WorkflowReadinessStatus,
    /// Validation-only or live adapter-readiness evaluation.
    pub evaluation_mode: WorkflowEvaluationMode,
    /// Aggregate required validation outcome.
    pub validation_status: ValidationStatus,
    /// Readiness is always read-only.
    pub run_created: bool,
    /// Stable flattened checks for display and machine consumers.
    pub checks: Vec<ValidationCheck>,
    /// Complete bound-validation evidence when evaluation reached validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<BoundWorkflowValidation>,
}

/// Result of planning or applying exact workflow authority grants.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAuthorizationStatus {
    /// Exact authority is valid but has not been published.
    Planned,
    /// One new exact authority fragment was published and revalidated.
    Applied,
    /// The operator reviewed and declined the exact proposed grants.
    Declined,
    /// Existing effective policy already covers the required authority.
    AlreadyAuthorized,
    /// A non-authority readiness blocker or unsafe destination prevents publication.
    Blocked,
}

/// Read-only authorization plan or the result of publishing one new fragment.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowAuthorizationReport {
    /// Stable authorization protocol identifier.
    pub schema: &'static str,
    /// Target workflow identity.
    pub workflow: String,
    /// Plan/application outcome.
    pub status: WorkflowAuthorizationStatus,
    /// Authorization never creates a run.
    pub run_created: bool,
    /// Readiness evidence used to project the exact grants.
    pub readiness: WorkflowReadinessReport,
    /// Exact fragment proposed or applied when authority is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<serde_json::Value>,
    /// Absolute policy destination when authority is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_output: Option<PathBuf>,
}

/// Compact authorization plan or receipt; never contains a full diagnostic report.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowAuthorizationSummary {
    /// Exact target workflow identity and binding scope.
    pub workflow: String,
    /// Planning or publication outcome; authorization never creates a run.
    pub status: WorkflowAuthorizationStatus,
    /// Compact pre-application evidence, not a claim of remaining gaps after application.
    pub readiness: WorkflowCheckSummary,
    /// Actual exact proposed or published policy fragment.
    pub policy: Option<serde_json::Value>,
    /// Absolute destination of that fragment, when available.
    pub policy_output: Option<PathBuf>,
}

struct AuthorizationPlan {
    status: WorkflowAuthorizationStatus,
    policy: Option<serde_json::Value>,
    policy_output: Option<PathBuf>,
}

impl WorkflowReadinessReport {
    /// Builds a readiness report from completed bound validation evidence.
    pub fn from_validation(validation: BoundWorkflowValidation) -> Self {
        Self::from_validation_in_mode(validation, WorkflowEvaluationMode::LiveReadiness)
    }

    pub(super) fn from_validation_in_mode(
        validation: BoundWorkflowValidation,
        evaluation_mode: WorkflowEvaluationMode,
    ) -> Self {
        #[cfg(test)]
        reports::READINESS_RESULTS.with(|count| count.set(count.get() + 1));
        let status = match evaluation_mode {
            WorkflowEvaluationMode::Validation => {
                reports::validation_readiness_status(validation.status)
            }
            WorkflowEvaluationMode::LiveReadiness => reports::readiness_status(validation.status),
        };
        Self {
            schema: "transist.workflow_readiness.v1",
            workflow: validation.workflow_ref.clone(),
            status,
            evaluation_mode,
            validation_status: validation.status,
            run_created: false,
            checks: validation.checks.clone(),
            validation: Some(validation),
        }
    }

    /// Builds an invalid-definition report when readiness could not be evaluated.
    pub fn invalid_definition(workflow: impl Into<String>, error: impl std::fmt::Display) -> Self {
        #[cfg(test)]
        reports::READINESS_RESULTS.with(|count| count.set(count.get() + 1));
        Self {
            schema: "transist.workflow_readiness.v1",
            workflow: workflow.into(),
            status: WorkflowReadinessStatus::InvalidDefinition,
            evaluation_mode: WorkflowEvaluationMode::LiveReadiness,
            validation_status: ValidationStatus::Failed,
            run_created: false,
            checks: vec![validation_check(
                "bound_validation",
                ValidationStatus::Failed,
                None,
                error.to_string(),
                None,
            )],
            validation: None,
        }
    }

    /// Returns whether the report authorizes fresh run birth.
    pub const fn is_ready(&self) -> bool {
        matches!(self.evaluation_mode, WorkflowEvaluationMode::LiveReadiness)
            && matches!(self.status, WorkflowReadinessStatus::Ready)
    }

    /// Returns whether every required validation check passed; does not authorize dispatch.
    pub fn is_validation_passed(&self) -> bool {
        self.validation_status == ValidationStatus::Passed
    }
}

/// Plans exact authority publication from one completed validation report.
///
/// The plan is side-effect free. It is intentionally restricted to validation
/// failures caused only by missing policy authority; malformed inputs, trust,
/// workspace, secret, and dependency failures remain blocked.
pub(crate) fn plan_workflow_authorization_in_home(
    home_dir: &Path,
    readiness: WorkflowReadinessReport,
    requested_output: Option<&Path>,
) -> Result<WorkflowAuthorizationReport> {
    let missing = readiness
        .validation
        .as_ref()
        .map(|validation| validation.missing_capabilities.clone())
        .unwrap_or_default();
    let only_authority_blockers = readiness
        .checks
        .iter()
        .filter(|check| {
            !matches!(
                check.status,
                ValidationStatus::Passed | ValidationStatus::DeferredToDispatch
            )
        })
        .all(|check| {
            check.status == ValidationStatus::Failed && reports::authority_failure_code(&check.code)
        });
    let plan = plan_authorization(
        home_dir,
        &readiness.workflow,
        readiness.is_validation_passed(),
        only_authority_blockers,
        &missing,
        readiness.validation.as_ref().map(|v| &v.policy_suggestion),
        requested_output,
    )?;
    #[cfg(test)]
    reports::AUTHORIZATION_RESULTS.with(|count| count.set(count.get() + 1));
    Ok(WorkflowAuthorizationReport {
        schema: "transist.workflow_authorization.v1",
        workflow: readiness.workflow.clone(),
        status: plan.status,
        run_created: false,
        readiness,
        policy: plan.policy,
        policy_output: plan.policy_output,
    })
}

fn plan_authorization(
    home_dir: &Path,
    workflow: &str,
    ready: bool,
    only_authority_blockers: bool,
    missing: &[CapabilityRequest],
    suggestion: Option<&crate::CapabilityPolicyHint>,
    requested_output: Option<&Path>,
) -> Result<AuthorizationPlan> {
    if missing.is_empty() {
        return Ok(AuthorizationPlan {
            status: if ready {
                WorkflowAuthorizationStatus::AlreadyAuthorized
            } else {
                WorkflowAuthorizationStatus::Blocked
            },
            policy: None,
            policy_output: None,
        });
    }
    if !only_authority_blockers {
        return Ok(AuthorizationPlan {
            status: WorkflowAuthorizationStatus::Blocked,
            policy: None,
            policy_output: None,
        });
    }
    let mut policy =
        serde_json::to_value(suggestion.context("missing capabilities require validation")?)?;
    let authorization_identity = (workflow, missing);
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&authorization_identity)?)
    );
    let grant_set_name = format!("authorization-{}", &digest[..16]);
    let grant_sets = policy
        .get_mut("grant_sets")
        .and_then(serde_json::Value::as_object_mut)
        .context("readiness policy suggestion has no grant sets")?;
    if grant_sets.len() != 1 {
        bail!("readiness policy suggestion must contain exactly one grant set");
    }
    let (_, grant_set) = grant_sets
        .iter()
        .next()
        .map(|(name, value)| (name.clone(), value.clone()))
        .expect("checked grant set length");
    grant_sets.clear();
    grant_sets.insert(grant_set_name.clone(), grant_set);
    let bindings = policy
        .get_mut("workflow_grants")
        .and_then(serde_json::Value::as_array_mut)
        .context("readiness policy suggestion has no workflow binding")?;
    if bindings.len() != 1 {
        bail!("readiness policy suggestion must contain exactly one workflow binding");
    }
    bindings[0]["workflow"] = serde_json::Value::String(workflow.to_string());
    bindings[0]["use"] = serde_json::json!([grant_set_name]);
    let destination =
        workflow_authorization_destination(home_dir, requested_output, workflow, &digest)?;
    let status = match fs::read(&destination) {
        Ok(_) => WorkflowAuthorizationStatus::Blocked,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            WorkflowAuthorizationStatus::Planned
        }
        Err(error) => return Err(error.into()),
    };
    Ok(AuthorizationPlan {
        status,
        policy: Some(policy),
        policy_output: Some(destination),
    })
}

/// Publishes one planned authorization fragment without replacing operator files.
pub(crate) fn apply_workflow_authorization_in_home(
    home_dir: &Path,
    mut report: WorkflowAuthorizationReport,
) -> Result<WorkflowAuthorizationReport> {
    if report.status != WorkflowAuthorizationStatus::Planned {
        return Ok(report);
    }
    publish_authorization(
        home_dir,
        report.policy.as_ref(),
        report.policy_output.as_deref(),
    )?;
    report.status = WorkflowAuthorizationStatus::Applied;
    Ok(report)
}

fn publish_authorization(
    home_dir: &Path,
    policy: Option<&serde_json::Value>,
    output: Option<&Path>,
) -> Result<()> {
    let policy = policy.context("authorization plan has no policy")?;
    let output = output.context("authorization plan has no output")?;
    let yaml = yaml_serde::to_string(policy)?;
    write_workflow_promotion_file(home_dir, output, yaml.as_bytes())?;
    if let Err(error) = crate::runtime_policy::load_policy(home_dir) {
        let rollback = remove_workflow_promotion_file(output);
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback) => Err(anyhow::anyhow!(
                "authorization validation failed: {error:#}; rollback failed: {rollback:#}"
            )),
        };
    }
    Ok(())
}

pub(super) fn authorize_summary_in_home(
    home_dir: &Path,
    readiness: WorkflowCheckSummary,
    missing: &[CapabilityRequest],
    requested_output: Option<&Path>,
    apply: bool,
) -> Result<WorkflowAuthorizationSummary> {
    let mut plan = plan_authorization(
        home_dir,
        &readiness.workflow_ref,
        readiness.is_validation_passed(),
        readiness.authorization_eligible,
        missing,
        readiness.policy_suggestion.as_ref(),
        requested_output,
    )?;
    if apply && plan.status == WorkflowAuthorizationStatus::Planned {
        publish_authorization(
            home_dir,
            plan.policy.as_ref(),
            plan.policy_output.as_deref(),
        )?;
        plan.status = WorkflowAuthorizationStatus::Applied;
    }
    Ok(WorkflowAuthorizationSummary {
        workflow: readiness.workflow_ref.clone(),
        status: plan.status,
        readiness,
        policy: plan.policy,
        policy_output: plan.policy_output,
    })
}

fn workflow_authorization_destination(
    home_dir: &Path,
    requested: Option<&Path>,
    workflow_ref: &str,
    digest: &str,
) -> Result<PathBuf> {
    let policy_root = home_dir.join("policy");
    let (workflow, version) = workflow_ref
        .split_once('@')
        .context("workflow ref must use '<id>@<version>'")?;
    let destination = requested.map(Path::to_path_buf).unwrap_or_else(|| {
        policy_root
            .join("workflow-authorizations")
            .join(format!("{workflow}-{version}-{digest}.yaml"))
    });
    let destination = if destination.is_absolute() {
        destination
    } else {
        policy_root.join(destination)
    };
    let relative = destination
        .strip_prefix(&policy_root)
        .context("policy output must be beneath <home>/policy")?;
    if relative.as_os_str().is_empty()
        || relative.extension().and_then(|value| value.to_str()) != Some("yaml")
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("policy output must be a .yaml file beneath <home>/policy");
    }
    let raw = fs::read_to_string(home_dir.join("config.yaml"))?;
    let config: serde_json::Value = yaml_serde::from_str(&raw)?;
    let covered = config
        .pointer("/policy/sources")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .any(|source| {
            relative == Path::new(source) || relative.parent() == Some(Path::new(source))
        });
    if !covered {
        bail!(
            "policy output '{}' is not covered by policy.sources",
            destination.display()
        );
    }
    let mut current = policy_root.clone();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            bail!(
                "policy output path contains a symlink: {}",
                current.display()
            );
        }
    }
    Ok(destination)
}

/// Read-only authored validation result produced from one immutable definition capture.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoredWorkflowValidation {
    /// Exact workflow identity parsed from the captured top-level definition.
    pub workflow_ref: String,
    /// Aggregate validation status.
    pub status: ValidationStatus,
    /// Ordered authored validation findings.
    pub checks: Vec<ValidationCheck>,
}

impl AuthoredWorkflowValidation {
    /// Returns whether the authored definition has no blocking validation failure.
    pub fn is_valid(&self) -> bool {
        self.status != ValidationStatus::Failed
    }
}

/// Validates a flow file using an explicit Transist home directory.
pub fn validate_flow_file_in_home(
    _repo_root: &Path,
    home_dir: &Path,
    flow_path: &Path,
) -> Result<AuthoredWorkflowValidation> {
    Ok(
        evaluate_authored_workflow(home_dir, flow_path, ReportDetail::Full)?
            .full
            .expect("full detail"),
    )
}

/// Binds an installed workflow and validates its materialized adapter requests.
///
/// This path is intentionally read-only: it does not create a run, resolve
/// secret values, or call an adapter preflight probe.
pub fn validate_bound_workflow_in_home(
    repo_root: &Path,
    home_dir: &Path,
    workflow_ref: &str,
    workflow_inputs: BTreeMap<String, KernelValue>,
    trusted_execution: bool,
    local_only: bool,
) -> Result<BoundWorkflowValidation> {
    Ok(validate_bound_workflow_in_home_inner(
        repo_root,
        home_dir,
        workflow_ref,
        workflow_inputs,
        trusted_execution,
        local_only,
        None,
        None,
        ReportDetail::Full,
    )?
    .full
    .expect("full detail"))
}

/// Evaluates complete fresh-run readiness using the same cached router and
/// secret provider that run preparation would use. This must run before any
/// fresh run owns durable storage.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_workflow_readiness_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    workflow_ref: &str,
    workflow_inputs: BTreeMap<String, KernelValue>,
    modes: runs::execution::RunModes,
) -> Result<BoundWorkflowValidation> {
    Ok(evaluate_workflow_with_cache(
        repo_root,
        home_dir,
        router_cache,
        workflow_ref,
        workflow_inputs,
        modes,
        WorkflowEvaluationMode::LiveReadiness,
        ReportDetail::Full,
    )?
    .full
    .expect("full detail"))
}

#[allow(clippy::too_many_arguments)] // Recursive projection keeps policy inputs and traversal state explicit.
fn collect_projected_capability_requirements(
    recipe: &Recipe,
    inputs: &BTreeMap<String, KernelValue>,
    deferred_inputs: &BTreeSet<String>,
    workspace_dir: &Path,
    exact_grants: &BTreeSet<CapabilityRequest>,
    plugin_grants: &BTreeSet<transist_policy::PluginCapabilityGrant>,
    required: &mut BTreeSet<CapabilityRequest>,
    granted: &mut BTreeSet<CapabilityRequest>,
    depth: usize,
    workflow_stack: &mut Vec<WorkflowRef>,
) -> Result<()> {
    if !recipe.prerequisites.is_empty()
        && evaluate_prerequisites(
            &recipe.prerequisites,
            &PrerequisiteEvaluationContext::new(PrerequisiteScope::Workflow, workspace_dir),
        )
        .map_or(true, |report| report.has_blockers())
    {
        return Ok(());
    }
    let reachable = transist_core::reachable_recipe_step_ids_in_scope(
        recipe,
        None,
        Some(inputs),
        deferred_inputs,
    )?;
    for step in recipe
        .steps
        .iter()
        .filter(|step| reachable.contains(&step.id))
    {
        if let Some(requests) = recipe.capabilities.get(&step.id) {
            collect_projected_step_grants(requests, exact_grants, plugin_grants, required, granted);
        }
        let StepExecutionDescriptor::Subflow { recipe: child } = &step.execution else {
            continue;
        };
        let child_depth = depth.saturating_add(1);
        if child_depth > MAX_SUBFLOW_VALIDATION_DEPTH || workflow_stack.contains(&child.workflow) {
            continue;
        }
        let child_inputs = transist_core::bind_deferred_subflow_inputs_in_scope(
            child,
            &step.with,
            inputs,
            &BTreeMap::new(),
            deferred_inputs,
        )
        .ok();
        if let Some(child_inputs) = child_inputs {
            workflow_stack.push(child.workflow.clone());
            collect_projected_capability_requirements(
                child,
                &child_inputs.values,
                &child_inputs.deferred_names,
                workspace_dir,
                exact_grants,
                plugin_grants,
                required,
                granted,
                child_depth,
                workflow_stack,
            )?;
            workflow_stack.pop();
        } else {
            for requests in child.capabilities.values() {
                collect_projected_step_grants(
                    requests,
                    exact_grants,
                    plugin_grants,
                    required,
                    granted,
                );
            }
        }
    }
    for cleanup in &recipe.cleanup {
        if let Some(requests) = recipe.capabilities.get(&cleanup.step.id) {
            collect_projected_step_grants(requests, exact_grants, plugin_grants, required, granted);
        }
    }
    Ok(())
}

fn collect_projected_step_grants(
    requests: &[CapabilityRequest],
    exact_grants: &BTreeSet<CapabilityRequest>,
    plugin_grants: &BTreeSet<transist_policy::PluginCapabilityGrant>,
    required: &mut BTreeSet<CapabilityRequest>,
    granted: &mut BTreeSet<CapabilityRequest>,
) {
    required.extend(requests.iter().cloned());
    granted.extend(
        requests
            .iter()
            .filter(|request| exact_grants.iter().any(|grant| grant.grants(request)))
            .cloned(),
    );
    let covered_packages = requests
        .iter()
        .filter(|request| request.capability == CapabilityName::Action)
        .filter_map(|request| request.resource.rsplit_once('/'))
        .filter(|(package, action)| {
            plugin_grants
                .iter()
                .any(|grant| grant.plugin == *package && grant.actions.contains(*action))
        })
        .map(|(package, _)| package)
        .collect::<BTreeSet<_>>();
    if covered_packages.is_empty() {
        return;
    }
    granted.extend(
        requests
            .iter()
            .filter(|request| match request.capability {
                CapabilityName::Action => request
                    .resource
                    .rsplit_once('/')
                    .is_some_and(|(package, _)| covered_packages.contains(package)),
                CapabilityName::Plugin => covered_packages.contains(request.resource.as_str()),
                CapabilityName::Adapter | CapabilityName::Provider => true,
                _ => false,
            })
            .cloned(),
    );
}

#[allow(clippy::too_many_arguments)] // Recursive static materialization keeps port dependencies explicit.
fn collect_static_recipe_validation(
    recipe: &Recipe,
    inputs: &BTreeMap<String, KernelValue>,
    deferred_inputs: &BTreeSet<String>,
    workspace_dir: &Path,
    home_dir: &Path,
    local_only: bool,
    plugin_metadata: &LocalPluginMetadata,
    prompt_files: &LocalPromptFiles,
    root_workflow_ref: &str,
    prefix: &str,
    policy: &dyn transist_core::PolicyPort,
    exact_grants: &BTreeSet<CapabilityRequest>,
    plugin_grants: &BTreeSet<transist_policy::PluginCapabilityGrant>,
    invocations: &mut Vec<PreflightInvocation>,
    checks: &mut CheckCollector,
    required: &mut BTreeSet<CapabilityRequest>,
    granted: &mut BTreeSet<CapabilityRequest>,
    descriptor_missing: &mut BTreeSet<CapabilityRequest>,
    workflow_stack: &mut Vec<String>,
    secrets: Option<&dyn SecretProviderPort>,
    live_adapters: bool,
) -> Result<()> {
    if !collect_workflow_prerequisites(recipe, workspace_dir, prefix, checks) {
        return Ok(());
    }
    let reachable = transist_core::reachable_recipe_step_ids_in_scope(
        recipe,
        None,
        Some(inputs),
        deferred_inputs,
    )?;
    for step in recipe
        .steps
        .iter()
        .filter(|step| reachable.contains(&step.id))
        .chain(recipe.cleanup.iter().map(|cleanup| &cleanup.step))
    {
        let step_path = if prefix.is_empty() {
            step.id.clone()
        } else {
            format!("{prefix}/{}", step.id)
        };
        let step_capabilities = recipe
            .capabilities
            .get(&step.id)
            .cloned()
            .unwrap_or_default();
        let applicable_capabilities = if matches!(
            &step.execution,
            StepExecutionDescriptor::Control { primitive }
                if primitive == transist_core::ControlStepKind::GateManual.as_wire_str()
        ) {
            step_capabilities
                .iter()
                .filter(|request| {
                    request.capability == CapabilityName::Approval
                        && request.operations.as_slice() == ["request"]
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            step_capabilities.clone()
        };
        required.extend(applicable_capabilities.iter().cloned());
        let runtime_dependencies = runtime_dependent_bindings(step);
        for code in &runtime_dependencies {
            checks.push(validation_check(
                *code,
                ValidationStatus::DeferredToDispatch,
                Some(step_path.clone()),
                "step depends on a value unavailable during static validation",
                Some("evaluate this condition during live preparation or execution".to_string()),
            ));
        }
        match &step.execution {
            StepExecutionDescriptor::Invoke { .. } => {
                match transist_core::make_static_execution_request_in_scope(
                    ExecutionRequestContext {
                        repo_root: workspace_dir,
                        home_dir,
                        recipe_source_dir: recipe.source_dir.as_deref().map(Path::new),
                        run_id: "static-validation",
                        workflow_id: &recipe.workflow.id,
                        workflow_inputs: inputs,
                        workflow_vars: &BTreeMap::new(),
                        execution: 0,
                        invoke_id: "static-validation",
                        local_only_mode: local_only,
                    },
                    step,
                    recipe.workspace.is_some(),
                    static_validation_invoke_paths(home_dir, workspace_dir, &step_path),
                    plugin_metadata,
                    prompt_files,
                    deferred_inputs,
                ) {
                    Ok(Some(mut prepared)) => {
                        prepared.request.step.id = step_path.clone();
                        if let Err(error) = validation_reuse::check_source_assets(
                            &prepared.request.input,
                            recipe.source_dir.as_deref().map(Path::new),
                            &step_path,
                            prompt_files,
                        ) {
                            checks.push(validation_check(
                                "source_asset",
                                ValidationStatus::Failed,
                                Some(step_path.clone()),
                                error.to_string(),
                                None,
                            ));
                        }
                        let descriptor = build_step_policy_descriptor(
                            root_workflow_ref,
                            &prepared.request,
                            &prepared.secret_refs,
                        )?;
                        match transist_policy::granted_capabilities_for_descriptor(
                            &descriptor,
                            &step_capabilities,
                            exact_grants,
                            plugin_grants,
                        ) {
                            Ok(step_grants) => {
                                descriptor_missing.extend(
                                    step_capabilities
                                        .iter()
                                        .filter(|request| !step_grants.contains(*request))
                                        .cloned(),
                                );
                                granted.extend(step_grants);
                            }
                            Err(error) => checks.push(policy_error_check(
                                "step_policy",
                                Some(step_path.clone()),
                                error,
                            )),
                        }
                        let mut remote_secret_policy = RemoteSecretPolicy::default();
                        let mut policy_allowed = false;
                        match policy.validate_step_execution(
                            "static-validation",
                            &step_path,
                            &descriptor,
                        ) {
                            Ok(decision) => {
                                policy_allowed = decision.allowed;
                                remote_secret_policy = decision.remote_secret_policy;
                                checks.push(validation_check(
                                    decision.reason_code.as_deref().unwrap_or("step_policy"),
                                    if decision.allowed {
                                        ValidationStatus::Passed
                                    } else {
                                        ValidationStatus::Failed
                                    },
                                    Some(step_path.clone()),
                                    decision.message.unwrap_or_else(|| {
                                        if decision.allowed {
                                            "step execution is allowed by policy".to_string()
                                        } else {
                                            "step execution is denied by policy".to_string()
                                        }
                                    }),
                                    (!decision.allowed).then(|| {
                                        "apply the exact policy_suggestion fragment from this report"
                                            .to_string()
                                    }),
                                ));
                            }
                            Err(error) => checks.push(policy_error_check(
                                "step_policy",
                                Some(step_path.clone()),
                                error,
                            )),
                        }
                        let mut available_secret_refs = Vec::new();
                        let mut missing_secret = false;
                        if let Some(secrets) = secrets {
                            for secret_ref in &prepared.secret_refs {
                                match secrets.has_secret(secret_ref) {
                                    Ok(true) => available_secret_refs.push(secret_ref.clone()),
                                    Err(error) => {
                                        missing_secret = true;
                                        checks.push(validation_check(
                                            "secret_resolution",
                                            ValidationStatus::Failed,
                                            Some(step_path.clone()),
                                            error.to_string(),
                                            None,
                                        ));
                                    }
                                    Ok(false) => {
                                        missing_secret = true;
                                        checks.push(validation_check(
                                        "secret_resolution",
                                        ValidationStatus::Failed,
                                        Some(step_path.clone()),
                                        format!("secret '{secret_ref}' is not available"),
                                        Some(format!(
                                            "provide secret '{secret_ref}' through the configured secret provider"
                                        )),
                                    ));
                                    }
                                }
                            }
                            if !prepared.secret_refs.is_empty() && !missing_secret {
                                checks.push(validation_check(
                                    "secret_resolution",
                                    ValidationStatus::Passed,
                                    Some(step_path.clone()),
                                    format!(
                                        "verified {} secret reference(s)",
                                        prepared.secret_refs.len()
                                    ),
                                    None,
                                ));
                            }
                        } else if !prepared.secret_refs.is_empty() {
                            checks.push(validation_check(
                                "secret_value",
                                ValidationStatus::NotEvaluated,
                                Some(step_path.clone()),
                                "secret values and availability are not evaluated statically",
                                Some(
                                    "run live preflight to verify secret availability".to_string(),
                                ),
                            ));
                        }
                        if !live_adapters && secrets.is_none() {
                            checks.push(validation_check(
                                "external_state",
                                ValidationStatus::NotEvaluated,
                                Some(step_path.clone()),
                                "adapter readiness and external state are not probed statically",
                                Some("run live preflight to evaluate external state".to_string()),
                            ));
                        }
                        let delivery_denied = !available_secret_refs.is_empty()
                            && !remote_secret_policy
                                .allows_adapter_kind(prepared.request.adapter_kind.as_str());
                        if delivery_denied {
                            checks.push(validation_check(
                                "remote_secret_policy",
                                ValidationStatus::Failed,
                                Some(step_path.clone()),
                                format!(
                                    "remote secret delivery is denied by policy for adapter '{}'",
                                    prepared.request.adapter_kind
                                ),
                                None,
                            ));
                        }
                        let deferred_paths = prepared.deferred_paths;
                        let invocation = PreflightInvocation {
                            request: prepared.request,
                            available_secret_refs,
                            remote_secret_policy,
                        };
                        if !deferred_paths.is_empty()
                            || missing_secret
                            || delivery_denied
                            || !policy_allowed
                        {
                            // Static contracts remain inspectable even when authority or
                            // availability blocks delivery. This path never runs probes.
                            for check in transist_adapters::validate_deferred_preflight_input(
                                &invocation,
                                &deferred_paths,
                                plugin_metadata.catalog(),
                            )? {
                                checks.preflight(check);
                            }
                        }
                        if !deferred_paths.is_empty() {
                            checks.push(validation_check("deferred_to_dispatch", ValidationStatus::DeferredToDispatch, Some(step_path.clone()), "complete input validation and live readiness deferred until consumer dispatch", None));
                        } else if !missing_secret && !delivery_denied && policy_allowed {
                            invocations.push(invocation);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        checks.push(static_request_preparation_check(&step_path, error));
                    }
                }
            }
            StepExecutionDescriptor::Subflow { recipe: child } => {
                let child_inputs = transist_core::bind_deferred_subflow_inputs_in_scope(
                    child,
                    &step.with,
                    inputs,
                    &BTreeMap::new(),
                    deferred_inputs,
                );
                match child_inputs {
                    Ok(child_inputs) => {
                        let child_workflow_ref =
                            format!("{}@{}", child.workflow.id, child.workflow.version);
                        let child_depth = workflow_stack.len();
                        if workflow_stack.contains(&child_workflow_ref)
                            || child_depth > MAX_SUBFLOW_VALIDATION_DEPTH
                        {
                            checks.push(subflow_recursion_check(
                                &step_path,
                                &child_workflow_ref,
                                child_depth,
                                workflow_stack.contains(&child_workflow_ref),
                            ));
                            continue;
                        }
                        workflow_stack.push(child_workflow_ref);
                        let result = collect_static_recipe_validation(
                            child,
                            &child_inputs.values,
                            &child_inputs.deferred_names,
                            workspace_dir,
                            home_dir,
                            local_only,
                            plugin_metadata,
                            prompt_files,
                            root_workflow_ref,
                            &step_path,
                            policy,
                            exact_grants,
                            plugin_grants,
                            invocations,
                            checks,
                            required,
                            granted,
                            descriptor_missing,
                            workflow_stack,
                            secrets,
                            live_adapters,
                        );
                        workflow_stack.pop();
                        result?;
                    }
                    Err(error) => checks.push(static_request_preparation_check(&step_path, error)),
                }
            }
            StepExecutionDescriptor::Control { primitive }
                if primitive == transist_core::ControlStepKind::GateManual.as_wire_str() =>
            {
                let descriptor = build_gate_policy_descriptor_for_workflow(
                    root_workflow_ref,
                    &applicable_capabilities,
                )?;
                match transist_policy::granted_capabilities_for_descriptor(
                    &descriptor,
                    &applicable_capabilities,
                    exact_grants,
                    plugin_grants,
                ) {
                    Ok(step_grants) => {
                        descriptor_missing.extend(
                            applicable_capabilities
                                .iter()
                                .filter(|request| !step_grants.contains(*request))
                                .cloned(),
                        );
                        granted.extend(step_grants);
                    }
                    Err(error) => checks.push(policy_error_check(
                        "approval_policy",
                        Some(step_path.clone()),
                        error,
                    )),
                }
                match policy.validate_step_execution("static-validation", &step_path, &descriptor) {
                    Ok(decision) => checks.push(validation_check(
                        decision.reason_code.as_deref().unwrap_or("approval_policy"),
                        if decision.allowed {
                            ValidationStatus::Passed
                        } else {
                            ValidationStatus::Failed
                        },
                        Some(step_path),
                        decision.message.unwrap_or_else(|| {
                            "manual approval request capability is allowed".to_string()
                        }),
                        (!decision.allowed).then(|| {
                            "apply the exact policy_suggestion fragment from this report"
                                .to_string()
                        }),
                    )),
                    Err(error) => checks.push(policy_error_check(
                        "approval_policy",
                        Some(step_path),
                        error,
                    )),
                }
            }
            StepExecutionDescriptor::Control { .. } => {}
        }
    }
    Ok(())
}

fn static_validation_invoke_paths(
    home_dir: &Path,
    workspace_dir: &Path,
    step_path: &str,
) -> InvokePaths {
    let root = home_dir
        .join(".transist-staging")
        .join("static-validation")
        .join(step_path.replace('/', "_"));
    InvokePaths {
        run_root: root.display().to_string(),
        artifacts_dir: root.join("artifacts").display().to_string(),
        evidence_dir: root.join("evidence").display().to_string(),
        workspace_dir: workspace_dir.display().to_string(),
    }
}

fn static_request_preparation_check(
    step_id: &str,
    error: impl std::fmt::Display,
) -> ValidationCheck {
    validation_check(
        "request_preparation",
        ValidationStatus::Failed,
        Some(step_id.to_string()),
        error.to_string(),
        None,
    )
}

fn collect_workflow_prerequisites(
    recipe: &Recipe,
    workspace_dir: &Path,
    prefix: &str,
    checks: &mut CheckCollector,
) -> bool {
    if recipe.prerequisites.is_empty() {
        return true;
    }
    let report = match evaluate_prerequisites(
        &recipe.prerequisites,
        &PrerequisiteEvaluationContext::new(PrerequisiteScope::Workflow, workspace_dir),
    ) {
        Ok(report) => report,
        Err(error) => {
            checks.push(validation_check(
                "workflow_prerequisite",
                ValidationStatus::Failed,
                (!prefix.is_empty()).then(|| prefix.to_string()),
                error.to_string(),
                None,
            ));
            return false;
        }
    };
    let has_blockers = report.has_blockers();
    for check in report.checks {
        checks.push(validation_check(
            format!("workflow_prerequisite.{}", check.kind),
            match check.status {
                PrerequisiteStatus::Passed => ValidationStatus::Passed,
                PrerequisiteStatus::Warning => ValidationStatus::Warning,
                PrerequisiteStatus::Failed => ValidationStatus::Failed,
            },
            (!prefix.is_empty()).then(|| prefix.to_string()),
            check.message,
            None,
        ));
    }
    !has_blockers
}

fn runtime_dependent_bindings(step: &RecipeStep) -> Vec<&'static str> {
    let bindings = transist_protocol::step_deferred_bindings(step).unwrap_or_default();
    let mut dependencies = Vec::new();
    if bindings.iter().any(|b| b.selector.starts_with("vars.")) {
        dependencies.push("mutable_variable");
    }
    if bindings.iter().any(|b| b.selector.starts_with("steps.")) {
        dependencies.push("step_output");
    }
    dependencies
}

fn policy_error_check(
    code: &str,
    step_path: Option<String>,
    error: impl std::fmt::Display,
) -> ValidationCheck {
    validation_check(
        code,
        ValidationStatus::Failed,
        step_path,
        error.to_string(),
        Some("fix the invalid policy configuration and validate again".to_string()),
    )
}

fn subflow_recursion_check(
    step_path: &str,
    child_workflow_ref: &str,
    child_depth: usize,
    cycle: bool,
) -> ValidationCheck {
    validation_check(
        if cycle {
            "subflow_preflight.cycle_detected"
        } else {
            "subflow_preflight.depth_exceeded"
        },
        ValidationStatus::Warning,
        Some(format!("{step_path}/")),
        if cycle {
            format!(
                "child validation skipped: workflow '{child_workflow_ref}' would recurse cyclically"
            )
        } else {
            format!(
                "child validation skipped: subflow nesting depth {child_depth} exceeds limit {MAX_SUBFLOW_VALIDATION_DEPTH}"
            )
        },
        None,
    )
}

fn validation_checks_from_preflight(check: transist_core::PreflightCheck) -> Vec<ValidationCheck> {
    let status = match check.status {
        transist_core::PreflightStatus::Passed => ValidationStatus::Passed,
        transist_core::PreflightStatus::DeferredToDispatch => ValidationStatus::DeferredToDispatch,
        transist_core::PreflightStatus::Warning => ValidationStatus::Warning,
        transist_core::PreflightStatus::Failed => ValidationStatus::Failed,
    };
    let step_paths = if check.step_ids.is_empty() {
        vec![None]
    } else {
        check.step_ids.into_iter().map(Some).collect()
    };
    step_paths
        .into_iter()
        .map(|step_path| ValidationCheck {
            code: check.kind.clone(),
            status,
            step_path,
            message: check.message.clone(),
            remediation: None,
        })
        .collect()
}

#[cfg(test)]
mod preflight_readiness_tests {
    use super::*;

    #[test]
    fn validation_checks_from_preflight_expands_grouped_checks_with_nested_paths() {
        let checks = validation_checks_from_preflight(transist_core::PreflightCheck {
            kind: "adapter_handshake".to_string(),
            status: transist_core::PreflightStatus::Warning,
            step_ids: vec![
                "parent/child/step-b".to_string(),
                "parent/child/step-a".to_string(),
            ],
            adapter_kind: Some("process".to_string()),
            plugin_id: Some("plugin".to_string()),
            action: Some("run".to_string()),
            message: "shared diagnostic".to_string(),
        });

        assert_eq!(checks.len(), 2);
        assert_eq!(
            checks
                .iter()
                .map(|check| check.step_path.as_deref())
                .collect::<Vec<_>>(),
            [Some("parent/child/step-b"), Some("parent/child/step-a")]
        );
        assert!(checks.iter().all(|check| {
            check.code == "adapter_handshake"
                && check.status == ValidationStatus::Warning
                && check.message == "shared diagnostic"
                && check.remediation.is_none()
        }));
    }

    #[test]
    fn validation_checks_from_preflight_keeps_unscoped_evidence() {
        let checks = validation_checks_from_preflight(transist_core::PreflightCheck {
            kind: "run_policy".to_string(),
            status: transist_core::PreflightStatus::Passed,
            step_ids: Vec::new(),
            adapter_kind: None,
            plugin_id: None,
            action: None,
            message: "allowed".to_string(),
        });

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].step_path, None);
    }
}

fn validation_check(
    code: impl Into<String>,
    status: ValidationStatus,
    step_path: Option<String>,
    message: impl Into<String>,
    remediation: Option<String>,
) -> ValidationCheck {
    ValidationCheck {
        code: code.into(),
        status,
        step_path,
        message: message.into(),
        remediation,
    }
}

/// Result of compiling a workflow with optional trust verification.
#[derive(Debug, Clone)]
pub struct WorkflowCompileResult {
    /// Absolute path to the compiled recipe file.
    pub recipe_path: PathBuf,
    /// Workflow definition digest verified against the detached signature.
    pub verified_digest: Option<String>,
    /// Signer key id when signature verification succeeded.
    pub signer_key_id: Option<String>,
    /// Stable workflow id loaded from the authored source.
    pub workflow_id: String,
    /// Stable workflow version loaded from the authored source.
    pub workflow_version: String,
}

/// Read-only trust metadata status for an authored workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowCompileTrustStatus {
    /// Stable workflow id loaded from the authored source.
    pub workflow_id: String,
    /// Stable workflow version loaded from the authored source.
    pub workflow_version: String,
    /// Whether trusted compile metadata exists for this workflow version.
    pub trusted_metadata_exists: bool,
}

/// Returns whether trusted compile metadata exists for an authored workflow.
pub fn workflow_compile_trust_status_in_home(
    home_dir: &Path,
    flow_path: &Path,
) -> Result<WorkflowCompileTrustStatus> {
    let flow = transist_compiler::load_flow_unvalidated(flow_path)?;
    let metadata_path =
        crate::trusted_execution::workflow_trust_install_path(home_dir, &flow.id, &flow.version)?;
    Ok(WorkflowCompileTrustStatus {
        workflow_id: flow.id,
        workflow_version: flow.version,
        trusted_metadata_exists: metadata_path.is_file(),
    })
}

impl WorkflowCompileResult {
    /// Returns whether this compile uses the reserved local test-workflow version.
    pub fn is_test_workflow(&self) -> bool {
        crate::is_test_workflow(&self.workflow_version)
    }
}

/// Compiles a flow file and writes the recipe artifact under an explicit home directory.
pub fn compile_flow_file_in_home(
    repo_root: &Path,
    home_dir: &Path,
    flow_path: &Path,
) -> Result<PathBuf> {
    Ok(compile_flow_file_in_home_with_trust(repo_root, home_dir, flow_path, false)?.recipe_path)
}

/// Compiles, registers, and durably records a workflows install in the local home.
pub fn install_flow_file_in_home_with_trust(
    _repo_root: &Path,
    home_dir: &Path,
    flow_path: &Path,
    trusted_execution: bool,
) -> Result<WorkflowInstallReport> {
    install_flow_file_in_home_with_trust_and_signature(
        _repo_root,
        home_dir,
        flow_path,
        trusted_execution,
        None,
    )
}

/// Compiles, registers, and durably records a workflow install using an
/// optional caller-selected detached signature source.
pub fn install_flow_file_in_home_with_trust_and_signature(
    _repo_root: &Path,
    home_dir: &Path,
    flow_path: &Path,
    trusted_execution: bool,
    signature_path: Option<&Path>,
) -> Result<WorkflowInstallReport> {
    let package = capture_workflow_package_with_signature(flow_path, signature_path)?;
    let install_lock = lock_workflow_installation(home_dir)?;
    let mut ownership = WorkflowInstallOwnershipJournal::new();
    let result = install_workflow_package_snapshot_in_home_with_trust(
        home_dir,
        &package,
        trusted_execution,
        &install_lock,
        &mut ownership,
    );
    match result {
        Ok(report) => Ok(report),
        Err(error) => match ownership.rollback(home_dir) {
            Ok(()) => Err(error),
            Err(rollback) => {
                bail!("workflow install failed: {error:#}; additionally, {rollback:#}")
            }
        },
    }
}

/// Installs an already captured package while the caller owns the home install lock.
pub fn install_workflow_package_snapshot_in_home_with_trust(
    home_dir: &Path,
    package: &WorkflowPackageSnapshot,
    trusted_execution: bool,
    install_lock: &WorkflowInstallLock,
    ownership: &mut WorkflowInstallOwnershipJournal,
) -> Result<WorkflowInstallReport> {
    if install_lock.home_dir != home_dir {
        bail!("workflow install lock belongs to a different Transist home");
    }
    let trusted_execution =
        crate::runtime_policy::trusted_execution_enabled(home_dir, trusted_execution)?;
    let snapshot = &package.definition;
    let flow = snapshot.top_level_flow()?;
    let compile = compile_captured_flow_file_in_home_with_trust(
        home_dir,
        &package.source_path,
        trusted_execution,
        snapshot,
        flow.clone(),
        WorkflowSignatureInput {
            bytes: package.signature.as_deref(),
            source_path: package.signature_source_path.as_deref(),
        },
        Some(ownership),
    )?;
    let (installed_recipe, installed_path) =
        load_compiled_recipe(home_dir, &format!("{}@{}", flow.id, flow.version))?;
    let definition_validation = if crate::is_test_workflow(&flow.version) {
        None
    } else {
        Some(validation_reuse::receipt(
            home_dir,
            &installed_recipe,
            &installed_path,
        )?)
    };
    let previous_entry =
        install_index::install_entry(home_dir, &format!("{}@{}", flow.id, flow.version))?;
    let (entry, entry_created, index_created) = install_index::record_install_entry(
        home_dir,
        install_index::WorkflowInstallIndexEntry {
            workflow_id: flow.id.clone(),
            workflow_version: flow.version.clone(),
            workflow_definition_sha256: snapshot.digest.clone(),
            recipe_sha256: recipe_sha256(&compile.recipe_path)?,
            definition_validation,
            recipe_path: compile.recipe_path.display().to_string(),
            workflow_source_path: snapshot
                .workflow_root
                .join(&snapshot.top_level)
                .display()
                .to_string(),
            signer_key_id: compile.signer_key_id,
            trusted_execution,
            installed_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        },
    )?;
    ownership.record_index_publication(&entry, entry_created, index_created);
    if let Some(previous) = previous_entry
        && previous != entry
    {
        ownership.upgraded_index_entry = Some((previous, entry.clone()));
    }
    Ok(WorkflowInstallReport {
        workflow_id: entry.workflow_id,
        workflow_version: entry.workflow_version,
        workflow_definition_sha256: entry.workflow_definition_sha256,
        recipe_path: PathBuf::from(entry.recipe_path),
        workflow_source_path: PathBuf::from(entry.workflow_source_path),
        signer_key_id: entry.signer_key_id,
        trusted_execution: entry.trusted_execution,
    })
}

/// Returns the SHA-256 digest of a durable compiled recipe file.
fn recipe_sha256(recipe_path: &Path) -> Result<String> {
    let bytes = transist_storage_local::read_regular_file_no_follow(recipe_path)
        .with_context(|| format!("failed reading compiled recipe: {}", recipe_path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Compiles a flow file and optionally records trusted-execution install metadata.
pub fn compile_flow_file_in_home_with_trust(
    _repo_root: &Path,
    home_dir: &Path,
    flow_path: &Path,
    trusted_execution: bool,
) -> Result<WorkflowCompileResult> {
    let trusted_execution =
        crate::runtime_policy::trusted_execution_enabled(home_dir, trusted_execution)?;
    let snapshot = if trusted_execution {
        transist_compiler::capture_workflow_definition_with_signature(flow_path)?
    } else {
        transist_compiler::capture_workflow_definition(flow_path)?
    };
    let flow = snapshot.top_level_flow()?;
    compile_captured_flow_file_in_home_with_trust(
        home_dir,
        flow_path,
        trusted_execution,
        &snapshot,
        flow,
        WorkflowSignatureInput::default(),
        None,
    )
}

fn compile_captured_flow_file_in_home_with_trust(
    home_dir: &Path,
    flow_path: &Path,
    trusted_execution: bool,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    flow: transist_compiler::Flow,
    signature: WorkflowSignatureInput<'_>,
    mut ownership: Option<&mut WorkflowInstallOwnershipJournal>,
) -> Result<WorkflowCompileResult> {
    let trust = crate::trusted_execution::verify_workflow_package_compile(
        home_dir,
        flow_path,
        &flow.id,
        &flow.version,
        trusted_execution,
        snapshot,
        signature.bytes,
    )?;
    let recipe = transist_compiler::compile_workflow_snapshot_in_home(home_dir, snapshot)?;
    let recipe_path = home_dir
        .join("registry/recipes")
        .join(format!("{}-{}.yaml", flow.id, flow.version));
    let recipe_existed = recipe_path.exists();
    let recipe_path = transist_compiler::write_recipe_in_home(home_dir, &recipe)?;
    if let Some(journal) = ownership.as_deref_mut() {
        journal.record_file_if_created(&recipe_path, recipe_existed);
        transist_storage_local::fault_injection::check_fault(
            transist_storage_local::fault_injection::FAULT_WORKFLOW_INSTALL_AFTER_RECIPE,
        )?;
    }
    let trust_path =
        crate::trusted_execution::workflow_trust_install_path(home_dir, &flow.id, &flow.version)?;
    let trust_existed = trust_path.exists();
    crate::trusted_execution::publish_workflow_compile_trust_metadata(
        home_dir,
        snapshot,
        &flow.id,
        &flow.version,
        trusted_execution,
        signature.source_path,
    )?;
    if let Some(journal) = ownership {
        if trusted_execution {
            journal.record_file_if_created(&trust_path, trust_existed);
        }
        transist_storage_local::fault_injection::check_fault(
            transist_storage_local::fault_injection::FAULT_WORKFLOW_INSTALL_AFTER_TRUST,
        )?;
    }
    Ok(WorkflowCompileResult {
        recipe_path,
        verified_digest: trust.verified_digest,
        signer_key_id: trust.signer_key_id,
        workflow_id: flow.id,
        workflow_version: flow.version,
    })
}

/// Lists compiled workflow recipes from an explicit home directory.
pub fn list_workflows_in_home(
    _repo_root: &Path,
    home_dir: &Path,
) -> Result<Vec<InstalledWorkflow>> {
    Ok(transist_compiler::list_recipes_in_home(home_dir)?
        .into_iter()
        .map(|workflow| InstalledWorkflow {
            id: workflow.id,
            version: workflow.version,
            path: workflow.path,
        })
        .collect())
}

/// Loads one compiled workflow recipe plus a workflow summary from an explicit home directory.
pub fn inspect_workflow_in_home(
    repo_root: &Path,
    home_dir: &Path,
    workflow_ref: &str,
) -> Result<WorkflowInspection> {
    let (recipe, path) = load_compiled_recipe(home_dir, workflow_ref)?;
    let inspection = crate::inspection_context::TransistInspectionContext::with_home(
        repo_root.to_path_buf(),
        home_dir.to_path_buf(),
    );
    let summary = inspection
        .get_workflow(workflow_ref)?
        .map(|detail| WorkflowInspectionSummary::from(detail.summary))
        .unwrap_or_else(|| WorkflowInspectionSummary::from_recipe(&recipe));
    Ok(WorkflowInspection {
        id: recipe.workflow.id.clone(),
        version: recipe.workflow.version.clone(),
        path,
        recipe,
        summary,
    })
}

/// Removes one compiled workflow recipe from an explicit home directory.
pub fn uninstall_workflow_in_home(
    _repo_root: &Path,
    home_dir: &Path,
    workflow_ref: &str,
    _force: bool,
) -> Result<String> {
    let (workflow_id, workflow_version) = parse_workflow_ref(workflow_ref)?;
    let run_ids =
        in_use::find_active_runs_for_workflow_in_home(home_dir, &workflow_id, &workflow_version)?;
    if !run_ids.is_empty() {
        return Err(WorkflowUninstallError::InUse {
            workflow_id,
            workflow_version,
            run_ids,
        }
        .into());
    }
    transist_compiler::remove_recipe_in_home(home_dir, &workflow_id, &workflow_version)?;
    install_index::remove_install_entry(home_dir, &workflow_id, &workflow_version)?;
    Ok(format!("{}@{}", workflow_id, workflow_version))
}

pub(crate) fn prepare_workflow_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    workflow_ref: &str,
    run_ref: Option<&str>,
    workflow_inputs: BTreeMap<String, KernelValue>,
) -> Result<PrepareReport> {
    prepare_workflow_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        workflow_ref,
        run_ref,
        workflow_inputs,
        runs::execution::RunModes {
            trusted_execution: false,
            local_only: false,
        },
    )
}

pub(crate) fn prepare_workflow_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    workflow_ref: &str,
    run_ref: Option<&str>,
    workflow_inputs: BTreeMap<String, KernelValue>,
    modes: runs::execution::RunModes,
) -> Result<PrepareReport> {
    reject_deterministic_pre_dispatch_blockers(
        repo_root,
        home_dir,
        router_cache,
        workflow_ref,
        workflow_inputs.clone(),
        modes,
    )?;
    let (recipe, recipe_path) = load_compiled_recipe(home_dir, workflow_ref)?;
    let bound_inputs = bind_workflow_inputs(&recipe, &recipe_path, workflow_inputs)?;
    runs::execution::prepare_recipe_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        &recipe,
        run_ref,
        bound_inputs,
        modes,
    )
}

pub(crate) fn launch_workflow_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    workflow_ref: &str,
    run_ref: Option<&str>,
    workflow_inputs: BTreeMap<String, KernelValue>,
) -> Result<RunReport> {
    launch_workflow_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        workflow_ref,
        run_ref,
        workflow_inputs,
        runs::execution::RunModes {
            trusted_execution: false,
            local_only: false,
        },
    )
}

pub(crate) fn launch_workflow_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    workflow_ref: &str,
    run_ref: Option<&str>,
    workflow_inputs: BTreeMap<String, KernelValue>,
    modes: runs::execution::RunModes,
) -> Result<RunReport> {
    reject_deterministic_pre_dispatch_blockers(
        repo_root,
        home_dir,
        router_cache,
        workflow_ref,
        workflow_inputs.clone(),
        modes,
    )?;
    let (recipe, recipe_path) = load_compiled_recipe(home_dir, workflow_ref)?;
    let bound_inputs = bind_workflow_inputs(&recipe, &recipe_path, workflow_inputs)?;
    runs::execution::run_recipe_with_inputs_and_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        &recipe,
        run_ref,
        bound_inputs,
        modes,
    )
}

/// Rejects static workflow blockers before a fresh run can claim durable storage.
fn reject_deterministic_pre_dispatch_blockers(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    workflow_ref: &str,
    workflow_inputs: BTreeMap<String, KernelValue>,
    modes: runs::execution::RunModes,
) -> Result<()> {
    let check_inputs = workflow_inputs
        .keys()
        .map(|name| format!("--input '{name}=<value>'"))
        .collect::<Vec<_>>();
    let validation = evaluate_workflow_readiness_in_home_with_cache(
        repo_root,
        home_dir,
        router_cache,
        workflow_ref,
        workflow_inputs,
        modes,
    )?;
    if validation.is_valid() {
        return Ok(());
    }
    let check_inputs = if check_inputs.is_empty() {
        "--input '<name>=<value>'".to_string()
    } else {
        check_inputs.join(" ")
    };
    bail!(
        "workflow '{}' has deterministic pre-dispatch blockers; inspect them with `transist workflows check {} {}` or `--inputs-file <path>` using the same values supplied for preparation (a check without inputs cannot validate required workflow inputs)",
        validation.workflow_ref,
        validation.workflow_ref,
        check_inputs,
    );
}

fn load_compiled_recipe(home_dir: &Path, workflow_ref: &str) -> Result<(Recipe, PathBuf)> {
    let (recipe, path, _) = load_compiled_recipe_with_digest(home_dir, workflow_ref)?;
    Ok((recipe, path))
}

fn load_compiled_recipe_with_digest(
    home_dir: &Path,
    workflow_ref: &str,
) -> Result<(Recipe, PathBuf, String)> {
    let (workflow_id, workflow_version) = parse_workflow_ref(workflow_ref)?;
    let path = home_dir
        .join("registry/recipes")
        .join(format!("{workflow_id}-{workflow_version}.yaml"));
    let raw = transist_storage_local::read_regular_file_no_follow(&path).with_context(|| {
        format!(
            "failed reading compiled workflow recipe: {}",
            path.display()
        )
    })?;
    let recipe: Recipe = yaml_serde::from_slice(&raw)
        .with_context(|| format!("invalid compiled workflow recipe: {}", path.display()))?;
    if recipe.workflow.id != workflow_id || recipe.workflow.version != workflow_version {
        bail!("installed_recipe_mismatch: recipe identity differs from requested workflow");
    }
    Ok((recipe, path, format!("{:x}", Sha256::digest(raw))))
}

fn parse_workflow_ref(raw: &str) -> Result<(String, String)> {
    let (workflow_id, workflow_version) = raw
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("workflow ref must use '<id>@<version>'"))?;
    if workflow_id.trim().is_empty() || workflow_version.trim().is_empty() {
        bail!("workflow ref must use '<id>@<version>'");
    }
    Ok((
        workflow_id.trim().to_string(),
        workflow_version.trim().to_string(),
    ))
}

fn bind_workflow_inputs(
    recipe: &Recipe,
    recipe_path: &Path,
    provided_inputs: BTreeMap<String, KernelValue>,
) -> Result<BTreeMap<String, KernelValue>> {
    bind_recipe_inputs(recipe, provided_inputs)
        .map_err(|error| workflow_input_binding_error(recipe_path, *error).into())
}

fn workflow_input_binding_error(
    recipe_path: &Path,
    error: RecipeInputBindingError,
) -> WorkflowInputBindingError {
    WorkflowInputBindingError {
        workflow_id: error.workflow_id,
        workflow_version: error.workflow_version,
        recipe_path: recipe_path.to_path_buf(),
        declared_inputs: error.declared_inputs,
        kind: map_recipe_input_binding_error_kind(error.kind),
    }
}

fn map_recipe_input_binding_error_kind(
    kind: RecipeInputBindingErrorKind,
) -> WorkflowInputBindingErrorKind {
    match kind {
        RecipeInputBindingErrorKind::UndeclaredInput { input_name } => {
            WorkflowInputBindingErrorKind::UndeclaredInput { input_name }
        }
        RecipeInputBindingErrorKind::MissingRequiredInput { input_name } => {
            WorkflowInputBindingErrorKind::MissingRequiredInput { input_name }
        }
        RecipeInputBindingErrorKind::TypeMismatch {
            input_name,
            expected_type,
            received_type,
        } => WorkflowInputBindingErrorKind::TypeMismatch {
            input_name,
            expected_type,
            received_type,
        },
        RecipeInputBindingErrorKind::UnsupportedDeclaredType {
            input_name,
            expected_type,
        } => WorkflowInputBindingErrorKind::UnsupportedDeclaredType {
            input_name,
            expected_type,
        },
    }
}

#[cfg(test)]
mod cli_positional_tests {
    #[test]
    fn workflow_cli_positional_preparation_remediation_is_bound_and_creates_no_run() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        crate::init_home_dir_with_mode(&home, crate::RuntimeOperatingMode::Standalone).unwrap();
        let flow = root.join("workflow.yaml");
        std::fs::write(&flow, format!("schema: transist.flow.v1\nid: positional.demo\nversion: v1\ninputs:\n  subject:\n    type: string\n    required: true\nprerequisites:\n  paths:\n    - path: {}\n      kind: file\n      required: true\nsteps:\n  - id: finish\n    control: control.finish\n", root.join("absent-file").display())).unwrap();
        let context = crate::TransistContext::with_home(&root, &home);
        context.compile_flow_file(&flow).unwrap();
        let inputs = std::collections::BTreeMap::from([(
            "subject".into(),
            crate::KernelValue::String("redacted-example".into()),
        )]);
        let error = context
            .prepare_workflow_with_trust(
                "positional.demo@v1",
                Some("blocked"),
                inputs,
                false,
                false,
            )
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("transist workflows check positional.demo@v1"),
            "{message}"
        );
        assert!(message.contains("--input 'subject=<value>'") && message.contains("--inputs-file"));
        assert!(!message.contains("redacted-example"));
        // Intentional legacy spelling: remediation must not advertise a removed option.
        assert!(!message.contains("--workflow"));
        assert!(context.runs_list().unwrap().is_empty());
        assert!(!home.join("runs/run_blocked").exists());
    }
}
