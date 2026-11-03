// SPDX-License-Identifier: MPL-2.0
//! Run-oriented application operations.
//!
//! This module owns run lifecycle commands built on top of the default runtime
//! kernel plus helpers for persisted gate decisions and active execution lookup.

mod capabilities;
pub mod execution;
mod gates;
mod helpers;
/// Run lifecycle service facade for execution, gate decisions, and cancellation.
pub mod service;
pub use service::RunService;

pub use gates::{
    GateDecision, GateDecisionInput, GateDecisionReport, resolve_gate_actor_identity_in_home,
};
pub(crate) use gates::{decide_gate_in_home_with_cache, record_gate_decision_in_home_with_cache};

use crate::composition::router_cache::RouterCache;
use anyhow::{Result, bail};
use std::path::Path;
use transist_core::LeasePort;
use transist_protocol::{KernelValue, RuntimeEvent};
use transist_storage_local::run_paths::resolve_root_run_id;

use helpers::{
    reenter_run_path_with_cache, resolve_actionable_failed_run_path, resolve_active_execution,
    run_status,
};

pub(crate) use capabilities::capability_policy_hint;
pub use capabilities::{CapabilityPolicyHint, RunCapabilityReport};
pub use execution::{
    ActiveRunCancellation, PrepareReport, RunCancellationRequest, RunEvidenceExport,
    RunRecipeSyncReport, RunReport, StepReport,
};
pub use transist_core::PreflightReport;
pub use transist_core::RunTreeLeaseRecord;

/// Active invoke target selected for cancellation in a run tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCancellationTarget {
    /// Run id that owns the active invoke step.
    pub run_id: String,
    /// Active invoke step id.
    pub step_id: String,
}

/// Resolves the active invoke step that a cancellation request should target.
pub fn resolve_cancellation_target_in_home(
    home_dir: &Path,
    run_id: &str,
) -> Result<Option<RunCancellationTarget>> {
    Ok(
        resolve_active_execution(home_dir, run_id)?.map(|target| RunCancellationTarget {
            run_id: target.run_id,
            step_id: target.step_id,
        }),
    )
}

pub(crate) fn run_started_run_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunReport> {
    run_started_run_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        false,
        false,
    )
}

pub(crate) fn run_started_run_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let status = run_status(home_dir, run_id)?;
    if status == "failed" {
        bail!(
            "run '{}' cannot be re-entered with 'transist runs start' because status is 'failed'; use 'transist runs resume {}' instead",
            run_id,
            run_id
        );
    }
    execution::run_started_run_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
        local_only,
    )
}

pub(crate) fn runs_preflight_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<PreflightReport> {
    runs_preflight_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        false,
        false,
    )
}

pub(crate) fn run_capabilities_in_home(
    home_dir: &Path,
    run_id: &str,
) -> Result<RunCapabilityReport> {
    capabilities::run_capabilities_in_home(home_dir, run_id)
}

pub(crate) fn runs_preflight_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<PreflightReport> {
    execution::preflight_run_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
        local_only,
    )
}

pub(crate) fn run_prepared_run_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunReport> {
    run_prepared_run_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        false,
        false,
    )
}

pub(crate) fn run_prepared_run_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let status = run_status(home_dir, run_id)?;
    if status == "failed" {
        bail!(
            "run '{}' cannot be launched with 'transist runs start --prepared' because status is 'failed'; use 'transist runs resume {}' instead",
            run_id,
            run_id
        );
    }
    execution::run_prepared_run_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
        local_only,
    )
}

pub(crate) fn runs_resume_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunReport> {
    runs_resume_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        false,
        false,
    )
}

pub(crate) fn runs_resume_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let status = run_status(home_dir, run_id)?;
    if !matches!(status.as_str(), "failed" | "canceled") {
        bail!(
            "run '{}' cannot be resumed because status is '{}' (expected 'failed' or 'canceled')",
            run_id,
            status
        );
    }
    let run_path = resolve_actionable_failed_run_path(home_dir, run_id)?;
    let _ = run_path;
    execution::resume_run_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        trusted_execution,
        local_only,
    )
}

pub(crate) fn runs_cleanup_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunReport> {
    runs_cleanup_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        run_id,
        false,
        false,
    )
}

pub(crate) fn runs_cleanup_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    let root_run_id = resolve_root_run_id_or_default(home_dir, run_id)?;
    let status = run_status(home_dir, &root_run_id)?;
    if !matches!(status.as_str(), "success" | "failed" | "canceled") {
        bail!(
            "run '{}' cannot be cleaned up because status is '{}'",
            root_run_id,
            status
        );
    }
    execution::cleanup_run_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        &root_run_id,
        trusted_execution,
        local_only,
    )
}

pub(crate) fn runs_replay_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    source_run_id: &str,
    new_run_ref: Option<&str>,
) -> Result<RunReport> {
    runs_replay_in_home_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        source_run_id,
        new_run_ref,
        false,
        false,
    )
}

pub(crate) fn runs_replay_in_home_with_trust_and_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    source_run_id: &str,
    new_run_ref: Option<&str>,
    trusted_execution: bool,
    local_only: bool,
) -> Result<RunReport> {
    execution::replay_run_with_trust_and_cache(
        repo_root,
        home_dir,
        router_cache,
        source_run_id,
        new_run_ref,
        trusted_execution,
        local_only,
    )
}

pub(crate) fn runs_sync_recipe_in_home_with_cache(
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunRecipeSyncReport> {
    execution::sync_run_recipe_snapshot_with_cache(home_dir, router_cache, run_id)
}

pub(crate) fn cancel_active_run_execution_in_home_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<Option<ActiveRunCancellation>> {
    let root_run_id = resolve_root_run_id(home_dir, run_id)?;
    let Some(resolution) = resolve_active_execution(home_dir, &root_run_id)? else {
        return Ok(None);
    };
    Ok(Some(execution::cancel_step_with_cache(
        repo_root,
        home_dir,
        router_cache,
        &resolution.run_id,
        &resolution.step_id,
    )?))
}

pub(crate) fn request_run_cancellation_in_home_with_cache(
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunCancellationRequest> {
    execution::request_run_cancellation_with_cache(home_dir, router_cache, run_id)
}

/// Loads the live lease record for the root run tree containing `run_id`, when present.
pub fn runs_lease_inspect_in_home(
    home_dir: &Path,
    run_id: &str,
) -> Result<Option<RunTreeLeaseRecord>> {
    let root_run_id = resolve_root_run_id_or_default(home_dir, run_id)?;
    let store =
        transist_storage_local::LocalLeaseStore::new(home_dir, crate::runtime_owner_identity()?);
    store.inspect_root_run_tree_lease(&root_run_id)
}

/// Breaks the live lease for the root run tree containing `run_id`, when present.
pub fn runs_lease_break_in_home(
    home_dir: &Path,
    run_id: &str,
    force_live_owner: bool,
) -> Result<Option<RunTreeLeaseRecord>> {
    let root_run_id = resolve_root_run_id_or_default(home_dir, run_id)?;
    let store =
        transist_storage_local::LocalLeaseStore::new(home_dir, crate::runtime_owner_identity()?);
    store.break_root_run_tree_lease(
        &root_run_id,
        transist_core::LeaseBreakOptions { force_live_owner },
    )
}

fn resolve_root_run_id_or_default(home_dir: &Path, run_id: &str) -> Result<String> {
    match resolve_root_run_id(home_dir, run_id) {
        Ok(root_run_id) => Ok(root_run_id),
        Err(error) if is_not_found_error(&error) => Ok(run_id.to_string()),
        Err(error) => Err(error),
    }
}

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound)
    })
}

/// Sweeps `<home>/locks/runs/*.lock` and removes leases whose owning PID is no
/// longer alive on this host and whose last heartbeat is older than the
/// configured staleness threshold ([`transist_storage_local::ReaperConfig`]).
///
/// Returns the records of the leases that were reaped.
pub fn runs_release_orphan_leases_in_home(home_dir: &Path) -> Result<Vec<RunTreeLeaseRecord>> {
    let store =
        transist_storage_local::LocalLeaseStore::new(home_dir, crate::runtime_owner_identity()?);
    store.release_orphan_leases(transist_storage_local::reaper_config_from_env())
}

/// Lists known runs stored under an explicit home directory.
pub fn runs_list_in_home(home_dir: &Path) -> Result<Vec<String>> {
    execution::list_runs(home_dir)
}

/// Returns stored run state from an explicit home directory.
pub fn runs_show_in_home(home_dir: &Path, run_id: &str) -> Result<KernelValue> {
    execution::show_run(home_dir, run_id)
}

/// Returns recent runtime events from an explicit home directory.
pub fn runs_tail_in_home(home_dir: &Path, run_id: &str, limit: usize) -> Result<Vec<RuntimeEvent>> {
    execution::tail_events(home_dir, run_id, limit)
}

/// Exports an evidence summary for one run from an explicit home directory.
pub fn run_evidence_export_in_home(home_dir: &Path, run_id: &str) -> Result<RunEvidenceExport> {
    execution::export_run_evidence(home_dir, run_id)
}
