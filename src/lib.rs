// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Public library entry points for local Transist workflow execution and inspection.

mod application;
/// Default local runtime composition used by the public application APIs.
mod composition;
mod context;
mod home;
mod init;
mod inspection_context;
mod metrics_context;
mod runtime_extensions;
mod runtime_policy;
mod runtime_process;
mod trust;
mod trusted_execution;
mod workflow_explanation;
pub use transist_protocol::explanation::WorkflowExplanation;
pub use workflow_explanation::{
    explain_workflow_file, render_workflow_explanation, resolve_workflow_explanation_home,
};

pub use context::TransistContext;
pub use home::{
    DEFAULT_INSPECTION_CACHE_RUNS, HomeResolutionError, HomeSource, ResolvedHome,
    clear_user_gate_actor, clear_user_global_home_root, clear_user_home_path,
    clear_user_inspection_cache_runs, clear_user_preferred_editor, clear_user_preferred_pager,
    load_user_gate_actor, load_user_global_home_root, load_user_home_path,
    load_user_inspection_cache_runs, load_user_preferred_editor, load_user_preferred_pager,
    per_user_default_home, record_user_home_path, repo_local_home_dir, resolve_home,
    resolve_settings_payload, save_user_gate_actor, save_user_global_home_root,
    save_user_home_path, save_user_inspection_cache_runs, save_user_preferred_editor,
    save_user_preferred_pager, user_settings_path,
};
pub use init::{init_home_dir, init_home_dir_with_mode, init_repo};
pub use metrics_context::TransistMetricsContext;

/// Validates that an effective home has an explicit, supported runtime configuration.
pub fn validate_runtime_configuration(home_dir: &std::path::Path) -> anyhow::Result<()> {
    runtime_policy::load_posture(home_dir).map(|_| ())
}
/// Validates the fully composed runtime policy without mutating runtime state.
pub fn validate_effective_runtime_policy(home_dir: &std::path::Path) -> anyhow::Result<()> {
    runtime_policy::load_policy(home_dir).map(|_| ())
}
/// Returns the flattened, effective runtime configuration for operator inspection.
pub fn effective_runtime_configuration_yaml(home_dir: &std::path::Path) -> anyhow::Result<String> {
    runtime_policy::effective_runtime_config_yaml(home_dir)
}
pub use inspection_context::TransistInspectionContext;
pub use runtime_extensions::{
    RuntimeServiceOverlay, RuntimeServiceOverlayInput, RuntimeServiceOverlayOutput,
    set_runtime_service_overlay,
};
pub use runtime_process::{
    RuntimeProcessOverrides, RuntimeProcessResolver, configure_runtime_process,
    detached_runner_management_env, runtime_management_mode, runtime_owner_identity,
    set_runtime_process_resolver,
};
pub use trust::{
    import_trust_root_file, inspect_trust_root, is_reserved_test_plugin_version, is_test_plugin,
    list_trust_roots, remove_trust_root, trust_store_dir, verify_plugin_signature,
    verify_workflow_definition_signature, workflow_definition_sha256,
};

pub use application::inspection::{
    BlobFamily, BlobListScope, DEFAULT_EVENT_LIMIT, DEFAULT_PAGE_LIMIT, EventListQuery,
    FailureClass, FailureFlags, FailureRecord, InspectionBlob, InspectionBlobContent,
    InspectionBlobDescriptor, InspectionChildFailure, InspectionCleanupStep,
    InspectionCleanupSummary, InspectionDocument, InspectionDocumentLinks,
    InspectionDocumentMember, InspectionEvent, InspectionEvidenceExportView,
    InspectionExternalTransfer, InspectionFilesystemPolicySummary, InspectionGate,
    InspectionGateDecision, InspectionLocalityStep, InspectionLocalityView,
    InspectionPolicyDecision, InspectionPreflightFailedCheck, InspectionPreflightSummary,
    InspectionQueryPort, InspectionRunDetail, InspectionRunDetailSummary, InspectionRunSummary,
    InspectionStep, InspectionStepBlobRef, InspectionStepExecution, InspectionStepInputSummary,
    InspectionStepOutputSnapshot, InspectionStepOutputSummary, InspectionTrustDecision,
    InspectionTrustSubject, InspectionTrustSummary, InspectionWorkflowContent,
    InspectionWorkflowDetail, InspectionWorkflowFileContent, InspectionWorkflowFileLocatorKind,
    InspectionWorkflowFileRef, InspectionWorkflowSummary, MAX_EVENT_LIMIT, MAX_PAGE_LIMIT, Page,
    PaginationQuery, RunCurrentStep, RunListQuery, RunListScope, SortOrder, StateMutation,
    SyncPolicyConfig, SyncPolicyMode, WorkflowGraphEdgeSummary, WorkflowInputSummary,
    WorkflowPromptReference, WorkflowStepSummary, WorkflowValidationSummary, workflow_ref_parts,
};
pub use application::inspection_snapshot::{
    DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES, EnvelopeContainsForbiddenKey, INSPECTION_SNAPSHOT_KIND,
    INSPECTION_SNAPSHOT_SCHEMA_VERSION, InspectionSnapshotApproverIdentity,
    InspectionSnapshotBlobDescriptor, InspectionSnapshotBlobMetadata, InspectionSnapshotBuilder,
    InspectionSnapshotEnvelope, InspectionSnapshotEvent, InspectionSnapshotEvidenceExportView,
    InspectionSnapshotExternalTransfer, InspectionSnapshotExternalTransferLedgerEntry,
    InspectionSnapshotExternalTransferSummary, InspectionSnapshotGate,
    InspectionSnapshotLocalityStep, InspectionSnapshotLocalityView, InspectionSnapshotPluginTrust,
    InspectionSnapshotRequest, InspectionSnapshotRun, InspectionSnapshotStepMetrics,
    InspectionSnapshotUsageFact, validate_snapshot_contract,
};
pub use application::metrics::{
    MetricsQueryPort, MetricsSnapshot, RunListMetricsSummary, RunMetricsSummary,
    StepExecutionMetricsSummary, StepMetricsSummary, UsageBreakdownEntry,
};
pub use application::plugins::PluginUninstallError;
pub use application::runs::{
    ActiveRunCancellation, CapabilityPolicyHint, GateDecision, GateDecisionInput, PrepareReport,
    RunCancellationRequest, RunCancellationTarget, RunCapabilityReport, RunEvidenceExport,
    RunRecipeSyncReport, RunReport, StepReport, resolve_cancellation_target_in_home,
    resolve_gate_actor_identity_in_home,
};
pub use application::workflows::{
    AuthoredWorkflowValidation, BoundWorkflowValidation, InstalledWorkflow, ValidationCheck,
    ValidationStatus, WorkflowAuthorizationReport, WorkflowAuthorizationStatus,
    WorkflowAuthorizationSummary, WorkflowCheckFinding, WorkflowCheckMode, WorkflowCheckSummary,
    WorkflowCompileResult, WorkflowCompileTrustStatus, WorkflowEvaluationMode,
    WorkflowInputBindingError, WorkflowInputBindingErrorKind, WorkflowInspection,
    WorkflowInspectionInputSummary, WorkflowInspectionSummary, WorkflowInstallLock,
    WorkflowInstallOwnershipJournal, WorkflowInstallReport, WorkflowPackageSnapshot,
    WorkflowReadinessReport, WorkflowReadinessStatus, WorkflowUninstallError,
    capture_workflow_package, capture_workflow_package_with_signature, lock_workflow_installation,
    remove_workflow_promotion_file, rollback_workflow_install_index,
    workflow_install_index_contains, write_workflow_promotion_file,
};

/// Authored workflow schema identifier.
pub const FLOW_SCHEMA_V1: &str = "transist.flow.v1";
/// Compiled recipe schema identifier.
pub const RECIPE_SCHEMA_V1: &str = "transist.recipe.v1";
pub use transist_core::{
    AssetLock, AssetLockKind, ExternalOwnerMetadata, ImportedTrustRoot, KernelRunSnapshot,
    KernelRunStatus, KernelStepState, KernelStepStatus, LeaseConflict, ManagementMode,
    PersistedPreflightRecord, PluginLifecycleReservationCanceled,
    PluginLifecycleReservationTimeout, PreflightCheck, PreflightMeasurement,
    PreflightMeasurementPhase, PreflightMeasurementScope, PreflightReport, PreflightStatus,
    RunLineage, RunLineageMode, RunTreeLeaseRecord, RuntimeOperatingMode, SIGNATURE_SCHEMA_V1,
    SignatureEnvelope, SignatureSubject, SignatureSubjectKind, SignatureVerification,
    TRUST_DECISION_SCHEMA_V1, TRUST_ROOT_SCHEMA_V1, TrustDecisionRecord, TrustRootRecord,
    TrustSubjectDecision, signature_sidecar_path,
};
pub use transist_core::{
    capture_preflight_measurements, measure_preflight_phase, with_preflight_measurement_scope,
};
pub use transist_plugins::{InstalledPlugin, PluginInstallReport, PluginManifest};
pub use transist_protocol::{
    FilesystemPolicy, GateReviewRequest, KernelValue, NetworkAccessMode, NetworkPolicy, Recipe,
    WorkspaceAccessMode, WorkspaceAccessPolicy,
};
/// Environment variable that disables local inspection caching when set to `off`.
pub const INSPECTION_CACHE_DISABLE_ENV: &str = "TRANSIST_INSPECTION_CACHE";

/// Default lease heartbeat interval in milliseconds.
pub const REAPER_DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
/// Default orphan-lease staleness threshold in milliseconds.
pub const REAPER_DEFAULT_STALE_AFTER_MS: u64 = 30_000;
/// Environment variable controlling orphan-lease reaping.
pub const REAPER_DISABLE_ENV: &str = "TRANSIST_REAPER_DISABLE";

/// Resolved orphan-lease reaper settings exposed by the root facade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaperSettings {
    /// Whether passive orphan-lease reaping is disabled.
    pub disabled: bool,
    /// Staleness threshold in milliseconds.
    pub stale_after_ms: u64,
}

/// Loads the filesystem policy selected by local runtime settings.
pub fn runtime_filesystem_policy(
    home_dir: &std::path::Path,
) -> anyhow::Result<Option<transist_protocol::FilesystemPolicy>> {
    transist_compiler::filesystem_settings::load_home_filesystem_policy(home_dir)
}

/// Resolves orphan-lease reaper settings from the process environment.
pub fn reaper_settings_from_env() -> ReaperSettings {
    let config = transist_storage_local::reaper_config_from_env();
    ReaperSettings {
        disabled: config.disabled,
        stale_after_ms: config.stale_after_ms,
    }
}

/// Validates that a local Transist installation record exists for a home.
pub fn validate_local_installation(home_dir: &std::path::Path) -> anyhow::Result<()> {
    transist_storage_local::Installation::load(home_dir).map(|_| ())
}

/// Stable test-workflow version used by local development fixtures.
pub const TEST_WORKFLOW_VERSION: &str = "v0";

/// Returns whether a workflow version is reserved for local test fixtures.
pub fn is_test_workflow(version: &str) -> bool {
    cfg!(feature = "dev-version-bypass") && version == TEST_WORKFLOW_VERSION
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn runtime_configuration_validation_keeps_posture_semantics() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("config.yaml"),
            "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  workflow_grants:\n    - workflow: demo@v1\n      use: [missing]\n",
        )
        .expect("config");

        validate_runtime_configuration(temp.path()).expect("valid posture");
        let error = validate_effective_runtime_policy(temp.path()).expect_err("invalid policy");
        assert!(
            error
                .to_string()
                .contains("workflow grants must reference named grant sets")
        );
    }
}
