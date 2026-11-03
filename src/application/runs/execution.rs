// SPDX-License-Identifier: MPL-2.0
//! Thin execution facade over the default runtime kernel.
//!
//! These helpers remain intentionally small: they execute or query runs using the
//! default local composition and normalize the output into compact DTOs used by
//! higher-level application APIs.

use crate::composition::router_cache::RouterCache;
use crate::composition::{default_runtime_kernel, default_runtime_kernel_with_modes};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;
use transist_core::{
    ExecutionCancelResult, KernelStepStatus, LeaseReleaseWarning, PersistedRunRecord,
    PreflightReport, PreflightStatus, RunApprovalProvenance, RunProvenanceRecord, RunStorePort,
};
use transist_protocol::{IdentifierKind, KernelValue, Recipe, RuntimeEvent, validate_identifier};
use transist_storage_local::{LocalRunQueryService, LocalRunStore, LocalStorageConfig};

/// Compact summary of one step inside a [`RunReport`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepReport {
    /// Stable step identifier from the compiled recipe.
    pub step_id: String,
    /// Whether the final observed step status was `succeeded`.
    pub success: bool,
    /// Exit code recorded in the last step result, or `0` when absent.
    pub exit_code: i32,
}

/// Compact cleanup summary returned by workflow and run operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupReport {
    /// Cleanup trigger that selected this cleanup phase.
    pub trigger: Option<String>,
    /// Current cleanup phase state.
    pub state: String,
}

/// High-level summary returned by workflow and run operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunReport {
    /// Stable run identifier.
    pub run_id: String,
    /// Final or current run status as a string.
    pub status: String,
    /// Failed-run recoverability state when one exists.
    pub recovery_state: Option<String>,
    /// Cleanup summary when one exists.
    pub cleanup: Option<CleanupReport>,
    /// Per-step compact execution reports.
    pub steps: Vec<StepReport>,
    /// Lease-release warning retained after the durable operation committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_release_warning: Option<LeaseReleaseWarning>,
}

/// High-level summary returned by workflows prepare operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrepareReport {
    /// Stable run identifier.
    pub run_id: String,
    /// Current run status after prepare-time preflight.
    pub status: String,
    /// Per-step compact execution reports.
    pub steps: Vec<StepReport>,
    /// Latest persisted preflight report for the prepared run.
    pub preflight: PreflightReport,
    /// Lease-release warning retained after the durable operation committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_release_warning: Option<LeaseReleaseWarning>,
}

/// Cancellation result for one active run step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveRunCancellation {
    /// Run that owned the cancelled active step.
    pub run_id: String,
    /// Stable step identifier that was targeted for cancellation.
    pub step_id: String,
    /// Whether the runtime accepted the cancellation request.
    pub accepted: bool,
}

/// Durable cancellation request written for one root run tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunCancellationRequest {
    /// Root run id covered by the durable cancellation request.
    pub root_run_id: String,
}

/// Result of syncing a run-scoped recipe snapshot from the compiled registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecipeSyncReport {
    /// Stable run identifier whose snapshot was updated.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Compiled registry recipe path used as the sync source.
    pub source_registry_path: String,
    /// Run-scoped `recipe.yaml` path that was updated.
    pub run_recipe_path: String,
    /// Lease-release warning retained after the durable snapshot update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_release_warning: Option<LeaseReleaseWarning>,
}

/// Read-only evidence export summary for one run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceExport {
    /// Stable run identifier.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Current run status from `state.json`.
    pub run_status: String,
    /// Persisted provenance record loaded from `provenance.json`.
    pub provenance: RunProvenanceRecord,
    /// Markdown report suitable for operator review or attachment.
    pub markdown: String,
}

/// Execution-mode flags shared by run and workflows run paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunModes {
    /// Whether trusted-execution verification is enforced for the launch.
    pub trusted_execution: bool,
    /// Whether only the local composition is allowed.
    pub local_only: bool,
}

pub(crate) fn start_recipe_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    recipe: &Recipe,
    run_ref: Option<&str>,
    workflow_inputs: std::collections::BTreeMap<String, KernelValue>,
) -> Result<RunReport> {
    validate_run_ref(run_ref)?;
    let kernel = default_runtime_kernel(router_cache, repo_root, home_dir)?;
    let outcome =
        kernel.start_recipe_with_outcome(repo_root, home_dir, recipe, run_ref, workflow_inputs)?;
    let warning = outcome.lease_release_warning.as_ref().cloned();
    let mut report = report_for_run(home_dir, &outcome.value)?;
    report.lease_release_warning = warning;
    Ok(report)
}

pub(crate) fn prepare_recipe_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    recipe: &Recipe,
    run_ref: Option<&str>,
    workflow_inputs: std::collections::BTreeMap<String, KernelValue>,
    modes: RunModes,
) -> Result<PrepareReport> {
    let trusted_execution =
        crate::runtime_policy::trusted_execution_enabled(home_dir, modes.trusted_execution)?;
    validate_run_ref(run_ref)?;
    let run_id = run_ref
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let trust = crate::trusted_execution::resolve_trusted_execution_for_fresh_run(
        repo_root,
        home_dir,
        &run_id,
        recipe,
        trusted_execution,
    )?;
    let kernel = default_runtime_kernel_with_modes(
        router_cache,
        repo_root,
        home_dir,
        trust.decision.clone().filter(|decision| !decision.denied()),
        modes.local_only,
    )?;
    let outcome = kernel.prepare_recipe_with_trust_and_assets_outcome(
        repo_root,
        home_dir,
        recipe,
        Some(&run_id),
        workflow_inputs,
        trust.mode,
        trust.decision,
        modes.local_only,
        trust.preset_workflow_assets,
    )?;
    let warning = outcome.lease_release_warning.as_ref().cloned();
    let (run_id, preflight) = outcome.value;
    let mut report = prepare_report_for_run(home_dir, &run_id, preflight)?;
    report.lease_release_warning = warning;
    Ok(report)
}

pub(crate) fn run_recipe_with_inputs_and_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    recipe: &Recipe,
    run_ref: Option<&str>,
    workflow_inputs: std::collections::BTreeMap<String, KernelValue>,
    modes: RunModes,
) -> Result<RunReport> {
    let trusted_execution =
        crate::runtime_policy::trusted_execution_enabled(home_dir, modes.trusted_execution)?;
    validate_run_ref(run_ref)?;
    let run_id = run_ref
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let trust = crate::trusted_execution::resolve_trusted_execution_for_fresh_run(
        repo_root,
        home_dir,
        &run_id,
        recipe,
        trusted_execution,
    )?;
    let kernel = default_runtime_kernel_with_modes(
        router_cache,
        repo_root,
        home_dir,
        trust.decision.clone().filter(|decision| !decision.denied()),
        modes.local_only,
    )?;
    let summary = kernel.execute_recipe_with_inputs_and_trust_and_assets(
        repo_root,
        home_dir,
        recipe,
        Some(&run_id),
        workflow_inputs,
        trust.mode,
        trust.decision,
        modes.local_only,
        trust.preset_workflow_assets,
    )?;
    report_for_kernel_summary(home_dir, &summary)
}

pub(crate) fn run_started_run_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let trust_warning = refresh_run_trust_decision_with_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
    )?;
    let kernel =
        kernel_for_run_with_local_only(home_dir, repo_root, router_cache, run_id, local_only)?;
    let summary = kernel.run_started_run(repo_root, home_dir, run_id)?;
    report_for_kernel_summary_with_warning(home_dir, &summary, trust_warning)
}

pub(crate) fn preflight_run_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<PreflightReport> {
    let trust_warning = refresh_run_trust_decision_with_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
    )?;
    let kernel =
        kernel_for_run_with_local_only(home_dir, repo_root, router_cache, run_id, local_only)?;
    let mut report = kernel.preflight_run(repo_root, home_dir, run_id)?;
    report.lease_release_warning =
        combine_lease_release_warnings(report.lease_release_warning, trust_warning);
    Ok(report)
}

pub(crate) fn run_prepared_run_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let trust_warning = refresh_run_trust_decision_with_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
    )?;
    let kernel =
        kernel_for_run_with_local_only(home_dir, repo_root, router_cache, run_id, local_only)?;
    let summary = kernel.run_prepared_run(repo_root, home_dir, run_id)?;
    report_for_kernel_summary_with_warning(home_dir, &summary, trust_warning)
}

pub(crate) fn resume_run_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let trust_warning = refresh_run_trust_decision_with_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
    )?;
    let kernel =
        kernel_for_run_with_local_only(home_dir, repo_root, router_cache, run_id, local_only)?;
    let summary = kernel.resume_run(repo_root, home_dir, run_id)?;
    report_for_kernel_summary_with_warning(home_dir, &summary, trust_warning)
}

pub(crate) fn cleanup_run_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let trust_warning = refresh_run_trust_decision_with_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
    )?;
    let kernel =
        kernel_for_run_with_local_only(home_dir, repo_root, router_cache, run_id, local_only)?;
    let summary = kernel.cleanup_run(repo_root, home_dir, run_id)?;
    report_for_kernel_summary_with_warning(home_dir, &summary, trust_warning)
}

pub(crate) fn replay_run_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    source_run_id: &str,
    run_ref: Option<&str>,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let trust_warning = refresh_run_trust_decision_with_cache(
        repo_root,
        home_dir,
        router_cache,
        source_run_id,
        trusted_execution,
    )?;
    validate_run_ref(run_ref)?;
    let kernel = kernel_for_run_with_local_only(
        home_dir,
        repo_root,
        router_cache,
        source_run_id,
        local_only,
    )?;
    let summary = kernel.replay_run(repo_root, home_dir, source_run_id, run_ref)?;
    report_for_kernel_summary_with_warning(home_dir, &summary, trust_warning)
}

/// Validates that an optional caller-supplied run reference satisfies the
/// identifier containment rule before it is used to derive any run id or
/// filesystem path. `None` (auto-generated run id) is always accepted.
fn validate_run_ref(run_ref: Option<&str>) -> Result<()> {
    if let Some(value) = run_ref {
        validate_identifier(IdentifierKind::RunRef, value).context("invalid run reference")?;
    }
    Ok(())
}

pub(crate) fn sync_run_recipe_snapshot_with_cache(
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunRecipeSyncReport> {
    let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    let record = store
        .load_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
    if !crate::is_test_workflow(&record.workflow_version) {
        anyhow::bail!(
            "run '{}' cannot sync its recipe snapshot because workflow {}@{} is not a test workflow; only version '{}' is allowed",
            run_id,
            record.workflow_id,
            record.workflow_version,
            crate::TEST_WORKFLOW_VERSION
        );
    }
    if matches!(
        record.status,
        transist_core::KernelRunStatus::Running | transist_core::KernelRunStatus::Waiting
    ) {
        anyhow::bail!(
            "run '{}' cannot sync its recipe snapshot while status is '{}'",
            run_id,
            record.status.as_str()
        );
    }

    let source_registry_path = home_dir.join("registry").join("recipes").join(format!(
        "{}-{}.yaml",
        record.workflow_id, record.workflow_version
    ));
    let recipe_yaml = std::fs::read_to_string(&source_registry_path).with_context(|| {
        format!(
            "failed reading compiled registry recipe: {}",
            source_registry_path.display()
        )
    })?;
    let recipe: Recipe = yaml_serde::from_str(&recipe_yaml).with_context(|| {
        format!(
            "invalid compiled registry recipe yaml: {}",
            source_registry_path.display()
        )
    })?;
    if recipe.workflow.id != record.workflow_id
        || recipe.workflow.version != record.workflow_version
    {
        anyhow::bail!(
            "registry recipe '{}' does not match run '{}' workflow {}@{}",
            source_registry_path.display(),
            run_id,
            record.workflow_id,
            record.workflow_version
        );
    }

    let kernel = default_runtime_kernel(router_cache, home_dir, home_dir)?;
    let outcome =
        kernel.sync_recipe_snapshot(home_dir, run_id, &source_registry_path, &recipe_yaml)?;
    let lease_release_warning = outcome.lease_release_warning.clone();
    if let Some(warning) = &lease_release_warning {
        tracing::warn!(run_id, warning = %warning.message, "recipe synchronization completed with a lease release warning");
    }
    let synced = outcome.value;
    Ok(RunRecipeSyncReport {
        run_id: synced.run_id,
        workflow_id: synced.workflow_id,
        workflow_version: synced.workflow_version,
        source_registry_path: synced.source_registry_path.display().to_string(),
        run_recipe_path: synced.run_recipe_path.display().to_string(),
        lease_release_warning,
    })
}

fn refresh_run_trust_decision_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
) -> Result<Option<LeaseReleaseWarning>> {
    let trusted_execution =
        crate::runtime_policy::trusted_execution_enabled(home_dir, trusted_execution)?;
    let kernel = default_runtime_kernel(router_cache, repo_root, home_dir)?;
    let resolver = crate::trusted_execution::local_trust_decision_resolver();
    let outcome = kernel.refresh_trusted_execution_for_run(
        repo_root,
        home_dir,
        run_id,
        trusted_execution,
        &resolver,
    )?;
    Ok(outcome.lease_release_warning)
}

/// Builds the runtime kernel for a re-entry path, preserving local-only mode
/// from the persisted record when the CLI flag does not explicitly request it.
pub(super) fn kernel_for_run_with_local_only(
    home_dir: &Path,
    repo_root: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    local_only_override: bool,
) -> Result<transist_core::RuntimeKernel> {
    let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    let record = store
        .load_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
    let effective_local_only = local_only_override || record.local_only_mode;
    default_runtime_kernel_with_modes(
        router_cache,
        repo_root,
        home_dir,
        record
            .trust_decision
            .clone()
            .filter(|decision| !decision.denied()),
        effective_local_only,
    )
}

pub(crate) fn request_run_cancellation_with_cache(
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunCancellationRequest> {
    let kernel = default_runtime_kernel(router_cache, home_dir, home_dir)?;
    let request = kernel.request_run_cancellation(home_dir, run_id)?;
    Ok(RunCancellationRequest {
        root_run_id: request.root_run_id,
    })
}

pub(crate) fn cancel_step_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    step_id: &str,
) -> Result<ActiveRunCancellation> {
    // Branch 1: in-process kernel cancel.
    let kernel = default_runtime_kernel(router_cache, repo_root, home_dir)?;
    let kernel_result: Result<ExecutionCancelResult> = kernel.cancel_step(run_id, step_id);

    // Branch 2: on-disk process-group signal. Always run, even when the
    // kernel branch already accepted, so the same code path covers
    // external supervisors and stale records.
    let run_root =
        transist_storage_local::run_paths::resolve_run_root_or_default(home_dir, run_id)?;
    let grace = std::time::Duration::from_millis(transist_adapters::DEFAULT_TERMINATION_GRACE_MS);
    let on_disk = transist_adapters::cancel_active_invocations_on_disk(
        &transist_storage_local::LocalActiveRecordStore,
        &run_root,
        step_id,
        grace,
    )?;
    if let Some(failed) = on_disk.iter().find(|entry| {
        !matches!(
            entry.outcome,
            transist_adapters::CancelOutcome::Absent
                | transist_adapters::CancelOutcome::Terminated
                | transist_adapters::CancelOutcome::Killed
        )
    }) {
        anyhow::bail!(
            "failed canceling active invocation '{}' for step '{}': {:?}",
            failed.record.invoke_id,
            step_id,
            failed.outcome
        );
    }
    let on_disk_accepted = on_disk.iter().any(|entry| {
        matches!(
            entry.outcome,
            transist_adapters::CancelOutcome::Absent
                | transist_adapters::CancelOutcome::Terminated
                | transist_adapters::CancelOutcome::Killed
        )
    });
    let kernel_result = match kernel_result {
        Ok(result) => result,
        Err(_error) if on_disk_accepted => ExecutionCancelResult {
            invoke_id: String::new(),
            accepted: false,
        },
        Err(error) => return Err(error),
    };

    Ok(ActiveRunCancellation {
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        accepted: kernel_result.accepted || on_disk_accepted,
    })
}

pub(crate) fn report_for_run(home_dir: &Path, run_id: &str) -> Result<RunReport> {
    let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    let record: PersistedRunRecord = store
        .load_run(run_id)?
        .with_context(|| format!("run '{}' was not found", run_id))?;
    // Started-but-not-yet-run records are read back from typed persisted state rather
    // than from a kernel summary, so they use the same report shape as execution paths.
    let steps = record
        .steps
        .values()
        .map(|step| StepReport {
            step_id: step.step_id.clone(),
            success: step.status == KernelStepStatus::Succeeded,
            exit_code: step
                .last_result
                .as_ref()
                .and_then(|result| result.exit_code)
                .unwrap_or_default(),
        })
        .collect();
    let cleanup = record.cleanup.as_ref().map(|cleanup| CleanupReport {
        trigger: Some(
            match cleanup.trigger {
                transist_protocol::CleanupTrigger::Success => "success",
                transist_protocol::CleanupTrigger::Canceled => "canceled",
                transist_protocol::CleanupTrigger::Failed => "failed",
            }
            .to_string(),
        ),
        state: match cleanup.state {
            transist_core::CleanupPhaseState::Pending => "pending",
            transist_core::CleanupPhaseState::Running => "running",
            transist_core::CleanupPhaseState::Succeeded => "succeeded",
            transist_core::CleanupPhaseState::Failed => "failed",
            transist_core::CleanupPhaseState::Skipped => "skipped",
        }
        .to_string(),
    });
    Ok(RunReport {
        run_id: record.run_id,
        status: record.status.as_str().to_string(),
        recovery_state: record
            .recovery_state
            .map(|state| state.as_str().to_string()),
        cleanup,
        steps,
        lease_release_warning: None,
    })
}

fn report_for_kernel_summary(
    home_dir: &Path,
    summary: &transist_core::KernelRunSummary,
) -> Result<RunReport> {
    report_for_kernel_summary_with_warning(home_dir, summary, None)
}

fn report_for_kernel_summary_with_warning(
    home_dir: &Path,
    summary: &transist_core::KernelRunSummary,
    prior_warning: Option<LeaseReleaseWarning>,
) -> Result<RunReport> {
    let mut report = report_for_run(home_dir, &summary.run_id)?;
    report.lease_release_warning =
        combine_lease_release_warnings(summary.lease_release_warning.clone(), prior_warning);
    Ok(report)
}

pub(crate) fn combine_lease_release_warnings(
    current: Option<LeaseReleaseWarning>,
    prior: Option<LeaseReleaseWarning>,
) -> Option<LeaseReleaseWarning> {
    match (current, prior) {
        (Some(current), Some(prior)) => Some(LeaseReleaseWarning {
            message: format!("{}; prior mutation: {}", current.message, prior.message),
        }),
        (current @ Some(_), _) => current,
        (_, prior) => prior,
    }
}

pub(crate) fn prepare_report_for_run(
    home_dir: &Path,
    run_id: &str,
    preflight: PreflightReport,
) -> Result<PrepareReport> {
    let report = report_for_run(home_dir, run_id)?;
    let status = if preflight.status == PreflightStatus::Failed {
        "failed".to_string()
    } else {
        report.status.clone()
    };
    Ok(PrepareReport {
        run_id: report.run_id,
        status,
        steps: report.steps,
        preflight,
        lease_release_warning: None,
    })
}

/// Lists known run identifiers from local storage.
pub fn list_runs(home_dir: &Path) -> Result<Vec<String>> {
    LocalRunQueryService::new(home_dir).list_runs()
}

/// Loads the stored run state for a single run.
pub fn show_run(home_dir: &Path, run_id: &str) -> Result<KernelValue> {
    LocalRunQueryService::new(home_dir).show_run(run_id)
}

/// Returns up to `limit` latest runtime events for a run.
pub fn tail_events(home_dir: &Path, run_id: &str, limit: usize) -> Result<Vec<RuntimeEvent>> {
    LocalRunQueryService::new(home_dir).tail_events(run_id, limit)
}

/// Exports a run evidence summary from persisted run state and provenance.
pub fn export_run_evidence(home_dir: &Path, run_id: &str) -> Result<RunEvidenceExport> {
    let run_store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    let record = run_store
        .load_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
    let provenance_path = transist_storage_local::run_paths::run_provenance_path(home_dir, run_id)?;
    let raw = std::fs::read_to_string(&provenance_path).with_context(|| {
        format!(
            "failed reading run provenance: {}",
            provenance_path.display()
        )
    })?;
    let provenance: RunProvenanceRecord = serde_json::from_str(&raw)
        .with_context(|| format!("invalid run provenance json: {}", provenance_path.display()))?;
    if provenance.schema != transist_core::RUN_PROVENANCE_SCHEMA_V1 {
        anyhow::bail!(
            "run provenance '{}' uses unsupported schema '{}'; expected '{}'",
            provenance_path.display(),
            provenance.schema,
            transist_core::RUN_PROVENANCE_SCHEMA_V1
        );
    }
    let approval_summary = collect_run_tree_approval_summary(home_dir, run_id, &provenance)?;
    let markdown =
        render_run_evidence_markdown(&record, &provenance, &provenance_path, &approval_summary);
    Ok(RunEvidenceExport {
        run_id: record.run_id.clone(),
        workflow_id: record.workflow_id.clone(),
        workflow_version: record.workflow_version.clone(),
        run_status: record.status.as_str().to_string(),
        provenance,
        markdown,
    })
}

fn render_run_evidence_markdown(
    record: &transist_core::PersistedRunRecord,
    provenance: &RunProvenanceRecord,
    provenance_path: &Path,
    approval_summary: &[RunApprovalProvenance],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Run Evidence Report");
    let _ = writeln!(out);
    let _ = writeln!(out, "- Run ID: `{}`", record.run_id);
    let _ = writeln!(
        out,
        "- Workflow: `{}@{}`",
        record.workflow_id, record.workflow_version
    );
    let _ = writeln!(out, "- Run Status: `{}`", record.status.as_str());
    let _ = writeln!(
        out,
        "- Trusted Execution Mode: `{}`",
        provenance.trusted_execution_mode
    );
    let _ = writeln!(out, "- Provenance Record: `{}`", provenance_path.display());
    let _ = writeln!(out);
    let _ = writeln!(out, "## Trust Decision");
    let _ = writeln!(out);
    let _ = writeln!(out, "- Run Decision: `{}`", provenance.decision);
    let _ = writeln!(
        out,
        "- Workflow Trust Decision: `{}`",
        provenance.workflow_trust_decision
    );
    let _ = writeln!(
        out,
        "- Workflow Signature Verified: `{}`",
        provenance.workflow_signature_verified
    );
    if let Some(digest) = provenance.workflow_digest.as_deref() {
        let _ = writeln!(out, "- Workflow Digest: `{}`", digest);
    }
    if let Some(key_id) = provenance.workflow_signer_key_id.as_deref() {
        let _ = writeln!(out, "- Workflow Signer Key ID: `{}`", key_id);
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Plugins");
    let _ = writeln!(out);
    if provenance.plugins.is_empty() {
        let _ = writeln!(out, "- None recorded");
    } else {
        for plugin in &provenance.plugins {
            let _ = writeln!(
                out,
                "- `{}@{}`: decision=`{}`, signature_verified=`{}`, digest=`{}`",
                plugin.id,
                plugin.version,
                plugin.trust_decision,
                plugin.signature_verified,
                plugin.digest
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Policy");
    let _ = writeln!(out);
    let _ = writeln!(out, "- Allowed: `{}`", provenance.policy_decision.allowed);
    let _ = writeln!(out, "- Mode: `{}`", provenance.policy_decision.mode);
    if let Some(reason_code) = provenance.policy_decision.reason_code.as_deref() {
        let _ = writeln!(out, "- Reason Code: `{}`", reason_code);
    }
    if let Some(message) = provenance.policy_decision.message.as_deref() {
        let _ = writeln!(out, "- Message: {}", message);
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Providers");
    let _ = writeln!(out);
    if provenance.providers.is_empty() {
        let _ = writeln!(out, "- None recorded");
    } else {
        for provider in &provenance.providers {
            let _ = writeln!(
                out,
                "- Step `{}`: provider=`{}`, model=`{}`",
                provider.step_id,
                provider.provider.as_deref().unwrap_or("unknown"),
                provider.model.as_deref().unwrap_or("unknown")
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Approvals");
    let _ = writeln!(out);
    if approval_summary.is_empty() {
        let _ = writeln!(out, "- None recorded");
    } else {
        for approval in approval_summary {
            let _ = writeln!(
                out,
                "- Gate `{}`: decision=`{}`{}{}{}",
                approval.gate_id,
                approval.decision,
                approval
                    .actor
                    .as_deref()
                    .map(|actor| format!(", actor={actor}"))
                    .unwrap_or_default(),
                approval
                    .reason
                    .as_deref()
                    .map(|reason| format!(", reason={reason}"))
                    .unwrap_or_default(),
                approval
                    .decided_at_ms
                    .map(|ts| format!(", decided_at_ms={ts}"))
                    .unwrap_or_default()
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## External Transfers");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- Count: `{}`",
        provenance.external_transfer_summary.count
    );
    if provenance
        .external_transfer_summary
        .external_transfers
        .is_empty()
    {
        let _ = writeln!(out, "- Ledger Entries: none");
    } else {
        for transfer in &provenance.external_transfer_summary.external_transfers {
            let _ = writeln!(
                out,
                "- Step `{}`: evidence=`{}`, destination=`{}`, adapter_kind=`{}`, provider=`{}`, content_hash=`{}`, timestamp_ms=`{}`",
                transfer.step_id,
                transfer.evidence_path,
                transfer.destination.as_deref().unwrap_or("unknown"),
                transfer.adapter_kind,
                transfer.provider.as_deref().unwrap_or("unknown"),
                transfer.content_hash.as_deref().unwrap_or("unknown"),
                transfer.timestamp_ms
            );
        }
    }
    let _ = writeln!(out, "- Local-Only Mode: `{}`", provenance.local_only_mode);
    if let Some(denied) = provenance.denied.as_ref() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Denied Run Detail");
        let _ = writeln!(out);
        let _ = writeln!(out, "- Reason: `{}`", denied.reason);
        let _ = writeln!(out, "- Offending Kind: `{}`", denied.offending_kind);
        let _ = writeln!(out, "- Offending ID: `{}`", denied.offending_id);
        if let Some(kind) = denied.failed_verification_type.as_deref() {
            let _ = writeln!(out, "- Failed Verification Type: `{}`", kind);
        }
    }
    out
}

fn collect_run_tree_approval_summary(
    home_dir: &Path,
    run_id: &str,
    root_provenance: &RunProvenanceRecord,
) -> Result<Vec<RunApprovalProvenance>> {
    let mut approvals = root_provenance.approval_summary.clone();
    let root_run = transist_storage_local::run_paths::resolve_run_root(home_dir, run_id)?;
    collect_nested_run_approvals(&root_run, &mut approvals)?;
    Ok(approvals)
}

fn collect_nested_run_approvals(
    run_root: &Path,
    approvals: &mut Vec<RunApprovalProvenance>,
) -> Result<()> {
    let contained_runs = run_root.join("contained_runs");
    if !contained_runs.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&contained_runs).with_context(|| {
        format!(
            "failed reading contained runs dir: {}",
            contained_runs.display()
        )
    })? {
        let entry = entry?;
        let child_root = entry.path();
        if !child_root.is_dir() {
            continue;
        }
        let provenance_path = child_root.join("provenance.json");
        if provenance_path.is_file() {
            let raw = std::fs::read_to_string(&provenance_path).with_context(|| {
                format!(
                    "failed reading run provenance: {}",
                    provenance_path.display()
                )
            })?;
            let record: RunProvenanceRecord = serde_json::from_str(&raw).with_context(|| {
                format!("invalid run provenance json: {}", provenance_path.display())
            })?;
            approvals.extend(record.approval_summary);
        }
        collect_nested_run_approvals(&child_root, approvals)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{
        CleanupTrigger, KernelValue, Recipe, RecipeStep, StepExecutionDescriptor, WorkflowRef,
    };

    #[test]
    fn run_recipe_executes_control_step() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "wf".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: std::collections::BTreeMap::new(),
            steps: vec![RecipeStep {
                id: "noop".to_string(),
                execution: StepExecutionDescriptor::Control {
                    primitive: "noop".to_string(),
                },
                with: std::collections::BTreeMap::new(),
                prompt_binding: None,
                skills_binding: None,
                vars: std::collections::BTreeMap::new(),
                filesystem_policy: Default::default(),
                network_policy: Default::default(),
                effective_filesystem_policy: Default::default(),
                effective_filesystem_policy_source: None,
                effective_network_policy: Default::default(),
                effective_network_policy_source: None,
            }],
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };

        let home_dir = tmp.path().join("transist-home");
        crate::init_home_dir_with_mode(&home_dir, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init home");
        let report = run_recipe_with_inputs_and_trust_and_cache(
            tmp.path(),
            &home_dir,
            &RouterCache::local(),
            &recipe,
            Some("run_test"),
            std::collections::BTreeMap::new(),
            RunModes {
                trusted_execution: false,
                local_only: false,
            },
        )
        .expect("run");
        assert_eq!(report.status, "success");
        assert_eq!(report.steps.len(), 1);
        let loaded = show_run(&home_dir, "run_test").expect("load run");
        assert!(matches!(loaded, KernelValue::Object(_)));
    }

    #[test]
    fn report_for_run_reads_typed_persisted_state() {
        let tmp = tempfile::tempdir().expect("home");
        let home_dir = tmp.path().join("transist-home");
        crate::init_home_dir_with_mode(&home_dir, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init home");
        let record = PersistedRunRecord::from_snapshot(
            transist_core::KernelRunSnapshot::new(
                "typed-report-run",
                "wf",
                "v1",
                &["step".to_string()],
            ),
            None,
            None,
            std::collections::BTreeMap::new(),
            0,
            1,
            2,
        );
        LocalRunStore::new(LocalStorageConfig::new(&home_dir))
            .create_run(&record)
            .expect("persist run");

        let report = report_for_run(&home_dir, "typed-report-run").expect("report");

        assert_eq!(report.run_id, "typed-report-run");
        assert_eq!(report.status, "created");
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].step_id, "step");
        assert!(!report.steps[0].success);
        assert_eq!(report.steps[0].exit_code, 0);
    }

    #[test]
    fn report_for_run_preserves_legacy_terminal_status_exit_cleanup_and_recovery() {
        let tmp = tempfile::tempdir().expect("home");
        let home_dir = tmp.path().join("transist-home");
        crate::init_home_dir_with_mode(&home_dir, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init home");

        let mut snapshot = transist_core::KernelRunSnapshot::new(
            "legacy-report-run",
            "wf",
            "v1",
            &["step".to_string()],
        );
        snapshot.status = transist_core::KernelRunStatus::Failed;
        let step = snapshot.steps.get_mut("step").expect("step");
        step.status = KernelStepStatus::Failed;
        step.executions = 1;
        step.last_result = Some(
            serde_json::from_value(serde_json::json!({
                "type": "tep.invoke.result.v1",
                "protocol_version": "tep.v1",
                "invoke_id": "invoke_step",
                "run_id": "legacy-report-run",
                "step_id": "step",
                "execution": 1,
                "status": "failed",
                "exit_code": 7,
                "stdout": "",
                "stderr": "failed",
                "timing": {
                    "started_at": "2025-01-01T00:00:01Z",
                    "finished_at": "2025-01-01T00:00:02Z",
                    "duration_ms": 1000
                },
                "artifacts": [],
                "evidence": [],
                "metrics": {},
                "output": null,
                "usage": null,
                "error": null
            }))
            .expect("result"),
        );
        let mut record = PersistedRunRecord::from_snapshot(
            snapshot,
            None,
            None,
            std::collections::BTreeMap::new(),
            0,
            1,
            2,
        );
        record.recovery_state = Some(transist_core::KernelRunRecoveryState::Recoverable);
        record.cleanup = Some(transist_core::PersistedCleanupRecord::new(
            CleanupTrigger::Failed,
            &[],
            transist_core::CleanupPhaseState::Failed,
        ));
        let store = LocalRunStore::new(LocalStorageConfig::new(&home_dir));
        store.create_run(&record).expect("persist run");

        // Remove fields introduced after the legacy record was written. The raw
        // state remains unchanged by the typed report reader.
        let state_path = home_dir.join("runs/run_legacy-report-run/state.json");
        let raw = std::fs::read(&state_path).expect("state bytes");
        let mut legacy: serde_json::Value = serde_json::from_slice(&raw).expect("state json");
        let object = legacy.as_object_mut().expect("state object");
        for field in [
            "schema",
            "workflow_vars",
            "runtime_mode",
            "policy_profile",
            "capability_enforcement",
            "asset_locks",
            "asset_lock_version",
            "test_mode",
            "capability_authority_version",
        ] {
            object.remove(field);
        }
        let legacy_raw = serde_json::to_vec_pretty(&legacy).expect("legacy state");
        std::fs::write(&state_path, &legacy_raw).expect("write legacy state");

        let report = report_for_run(&home_dir, "legacy-report-run").expect("report");
        assert_eq!(report.status, "failed");
        assert_eq!(report.recovery_state.as_deref(), Some("recoverable"));
        assert_eq!(report.steps[0].exit_code, 7);
        assert!(!report.steps[0].success);
        assert_eq!(
            report
                .cleanup
                .as_ref()
                .map(|cleanup| cleanup.trigger.as_deref()),
            Some(Some("failed"))
        );
        assert_eq!(
            report
                .cleanup
                .as_ref()
                .map(|cleanup| cleanup.state.as_str()),
            Some("failed")
        );
        assert_eq!(
            std::fs::read(&state_path).expect("state after read"),
            legacy_raw
        );
    }

    #[test]
    fn export_run_evidence_reads_provenance_and_renders_markdown() {
        let repo = tempfile::tempdir().expect("tempdir");
        let home_tmp = tempfile::tempdir().expect("home tempdir");
        let home_dir = home_tmp.path().to_path_buf();
        crate::init_home_dir_with_mode(&home_dir, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init repo");
        let context = crate::TransistContext::with_home(repo.path(), &home_dir);
        let flow_path = repo.path().join("workflow.yaml");
        std::fs::write(
            &flow_path,
            r#"schema: transist.flow.v1
id: export_demo
version: v1
steps:
  - id: finish
    control: control.finish
"#,
        )
        .expect("write workflow");
        context
            .compile_flow_file(&flow_path)
            .expect("compile workflow");
        context
            .prepare_workflow(
                "export_demo@v1",
                Some("evidence-export-run"),
                Default::default(),
            )
            .expect("prepare workflow");
        context
            .run_started_run("evidence-export-run")
            .expect("run workflow");

        let exported = export_run_evidence(context.home_dir(), "evidence-export-run")
            .expect("export evidence");
        assert_eq!(exported.run_id, "evidence-export-run");
        assert_eq!(exported.provenance.decision, "allowed");
        assert!(exported.markdown.contains("# Run Evidence Report"));
        assert!(exported.markdown.contains("Workflow: `export_demo@v1`"));
    }

    #[test]
    fn export_run_evidence_rejects_unknown_provenance_schema() {
        let repo = tempfile::tempdir().expect("tempdir");
        let home_tmp = tempfile::tempdir().expect("home tempdir");
        let home_dir = home_tmp.path().to_path_buf();
        crate::init_home_dir_with_mode(&home_dir, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init repo");
        let context = crate::TransistContext::with_home(repo.path(), &home_dir);
        let flow_path = repo.path().join("workflow.yaml");
        std::fs::write(
            &flow_path,
            r#"schema: transist.flow.v1
id: schema_check_demo
version: v1
steps:
  - id: finish
    control: control.finish
"#,
        )
        .expect("write workflow");
        context.compile_flow_file(&flow_path).expect("compile");
        context
            .prepare_workflow(
                "schema_check_demo@v1",
                Some("schema-check-run"),
                Default::default(),
            )
            .expect("prepare");
        context.run_started_run("schema-check-run").expect("run");

        let prov_path = transist_storage_local::run_paths::run_provenance_path(
            context.home_dir(),
            "schema-check-run",
        )
        .expect("provenance path");
        let raw = std::fs::read_to_string(&prov_path).expect("read provenance");
        let mut json: serde_json::Value =
            serde_json::from_str(&raw).expect("parse provenance json");
        json["schema"] = serde_json::json!("transist.run_provenance.v99");
        std::fs::write(&prov_path, serde_json::to_vec_pretty(&json).unwrap())
            .expect("write tampered provenance");

        let error = export_run_evidence(context.home_dir(), "schema-check-run");
        assert!(error.is_err());
        let msg = error.unwrap_err().to_string();
        assert!(msg.contains("unsupported schema"), "actual error: {msg}");
    }
}
