// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use transist_core::PreflightReport;
use transist_protocol::{KernelValue, RuntimeEvent};

use super::{
    ActiveRunCancellation, GateDecision, GateDecisionInput, GateDecisionReport,
    RunCancellationRequest, RunCapabilityReport, RunEvidenceExport, RunRecipeSyncReport, RunReport,
    RunTreeLeaseRecord, decide_gate_in_home_with_cache, record_gate_decision_in_home_with_cache,
};

/// Run lifecycle, gate decision, and cancellation operations.
#[derive(Debug, Clone)]
pub struct RunService {
    pub(crate) repo_root: PathBuf,
    pub(crate) home_dir: PathBuf,
    pub(crate) router_cache: Arc<crate::composition::router_cache::RouterCache>,
}

impl PartialEq for RunService {
    fn eq(&self, other: &Self) -> bool {
        self.repo_root == other.repo_root && self.home_dir == other.home_dir
    }
}

impl Eq for RunService {}

impl RunService {
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

    /// Executes a previously started run by run id.
    pub fn run_started_run(&self, run_id: &str) -> Result<RunReport> {
        super::run_started_run_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
        )
    }

    /// Executes a previously started run with optional trusted-execution enforcement.
    pub fn run_started_run_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        super::run_started_run_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            trusted_execution,
            local_only,
        )
    }

    /// Executes a previously prepared run while reusing its stored successful preflight.
    pub fn run_prepared_run(&self, run_id: &str) -> Result<RunReport> {
        super::run_prepared_run_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
        )
    }

    /// Executes a previously prepared run with optional trusted-execution refresh.
    pub fn run_prepared_run_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        super::run_prepared_run_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            trusted_execution,
            local_only,
        )
    }

    /// Runs the generic preflight routine for a persisted run without executing it.
    pub fn runs_preflight(&self, run_id: &str) -> Result<PreflightReport> {
        super::runs_preflight_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
        )
    }

    /// Runs preflight with optional trusted-execution enforcement.
    pub fn runs_preflight_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<PreflightReport> {
        super::runs_preflight_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            trusted_execution,
            local_only,
        )
    }

    /// Returns the persisted run's required and currently missing capability grants.
    pub fn run_capabilities(&self, run_id: &str) -> Result<RunCapabilityReport> {
        super::run_capabilities_in_home(&self.home_dir, run_id)
    }

    /// Records an approval decision for the current manual gate and resumes the run.
    pub fn approve_gate(&self, run_id: &str, gate_id: &str) -> Result<RunReport> {
        self.approve_gate_with_input(run_id, gate_id, GateDecisionInput::default())
    }

    /// Records an approval decision with explicit gate-decision metadata.
    pub fn approve_gate_with_input(
        &self,
        run_id: &str,
        gate_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        decide_gate_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            Some(gate_id),
            GateDecision::Approved,
            input,
        )
    }

    /// Records an approval decision for the currently blocked manual gate inferred from the run.
    pub fn approve_current_gate(&self, run_id: &str) -> Result<RunReport> {
        self.approve_current_gate_with_input(run_id, GateDecisionInput::default())
    }

    /// Records an approval decision for the current blocked gate with explicit metadata.
    pub fn approve_current_gate_with_input(
        &self,
        run_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        decide_gate_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            None,
            GateDecision::Approved,
            input,
        )
    }

    /// Records a rejection decision for the current manual gate and resumes the run.
    pub fn reject_gate(
        &self,
        run_id: &str,
        gate_id: &str,
        reason: Option<&str>,
    ) -> Result<RunReport> {
        self.reject_gate_with_input(
            run_id,
            gate_id,
            GateDecisionInput {
                reason: reason.map(str::to_string),
                ..GateDecisionInput::default()
            },
        )
    }

    /// Records a rejection decision with explicit gate-decision metadata.
    pub fn reject_gate_with_input(
        &self,
        run_id: &str,
        gate_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        decide_gate_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            Some(gate_id),
            GateDecision::Rejected,
            input,
        )
    }

    /// Records a rejection decision for the currently blocked manual gate inferred from the run.
    pub fn reject_current_gate(&self, run_id: &str, reason: Option<&str>) -> Result<RunReport> {
        self.reject_current_gate_with_input(
            run_id,
            GateDecisionInput {
                reason: reason.map(str::to_string),
                ..GateDecisionInput::default()
            },
        )
    }

    /// Records a rejection decision for the current blocked gate with explicit metadata.
    pub fn reject_current_gate_with_input(
        &self,
        run_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        decide_gate_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            None,
            GateDecision::Rejected,
            input,
        )
    }

    /// Records a request-changes decision for the current manual gate and resumes the run.
    pub fn request_changes_gate(
        &self,
        run_id: &str,
        gate_id: &str,
        reason: Option<&str>,
    ) -> Result<RunReport> {
        self.request_changes_gate_with_input(
            run_id,
            gate_id,
            GateDecisionInput {
                reason: reason.map(str::to_string),
                ..GateDecisionInput::default()
            },
        )
    }

    /// Records a request-changes decision with explicit gate-decision metadata.
    pub fn request_changes_gate_with_input(
        &self,
        run_id: &str,
        gate_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        decide_gate_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            Some(gate_id),
            GateDecision::RequestChanges,
            input,
        )
    }

    /// Records a request-changes decision for the currently blocked manual gate inferred from the run.
    pub fn request_changes_current_gate(
        &self,
        run_id: &str,
        reason: Option<&str>,
    ) -> Result<RunReport> {
        self.request_changes_current_gate_with_input(
            run_id,
            GateDecisionInput {
                reason: reason.map(str::to_string),
                ..GateDecisionInput::default()
            },
        )
    }

    /// Records a request-changes decision for the current blocked gate with explicit metadata.
    pub fn request_changes_current_gate_with_input(
        &self,
        run_id: &str,
        input: GateDecisionInput,
    ) -> Result<RunReport> {
        decide_gate_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            None,
            GateDecision::RequestChanges,
            input,
        )
    }

    /// Persists a manual gate decision **without** resuming the run.
    ///
    /// Returns the resolved root run id of the run lineage so callers
    /// (e.g. the detached-runner spawner) can target the correct run for
    /// re-entry. Used by the detached runner control flow where the foreground
    /// process records the decision and a separate runner process re-enters
    /// the run to consume it.
    pub fn record_gate_decision(
        &self,
        run_id: &str,
        gate_id: Option<&str>,
        decision: GateDecision,
        reason: Option<&str>,
    ) -> Result<GateDecisionReport> {
        self.record_gate_decision_with_input(
            run_id,
            gate_id,
            decision,
            GateDecisionInput {
                reason: reason.map(str::to_string),
                ..GateDecisionInput::default()
            },
        )
    }

    /// Persists a manual gate decision without resuming the run, with explicit metadata.
    pub fn record_gate_decision_with_input(
        &self,
        run_id: &str,
        gate_id: Option<&str>,
        decision: GateDecision,
        input: GateDecisionInput,
    ) -> Result<GateDecisionReport> {
        record_gate_decision_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            gate_id,
            decision,
            input,
        )
    }

    /// Resumes a failed run from its persisted run-scoped recipe snapshot.
    pub fn runs_resume(&self, run_id: &str) -> Result<RunReport> {
        super::runs_resume_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
        )
    }

    /// Resumes a failed or canceled run with optional trusted-execution refresh.
    pub fn runs_resume_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        super::runs_resume_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            trusted_execution,
            local_only,
        )
    }

    /// Executes terminal cleanup for a root run or the root run containing `run_id`.
    pub fn runs_cleanup(&self, run_id: &str) -> Result<RunReport> {
        super::runs_cleanup_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
        )
    }

    /// Executes terminal cleanup with optional trusted-execution refresh.
    pub fn runs_cleanup_with_trust(
        &self,
        run_id: &str,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        super::runs_cleanup_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
            trusted_execution,
            local_only,
        )
    }

    /// Replays an existing run as a new run using its persisted recipe snapshot.
    pub fn runs_replay(&self, source_run_id: &str, new_run_ref: Option<&str>) -> Result<RunReport> {
        super::runs_replay_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            source_run_id,
            new_run_ref,
        )
    }

    /// Replays a run with optional trusted-execution refresh on the source run.
    pub fn runs_replay_with_trust(
        &self,
        source_run_id: &str,
        new_run_ref: Option<&str>,
        trusted_execution: bool,
        local_only: bool,
    ) -> Result<RunReport> {
        super::runs_replay_in_home_with_trust_and_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            source_run_id,
            new_run_ref,
            trusted_execution,
            local_only,
        )
    }

    /// Replaces a run-scoped `recipe.yaml` snapshot with the current compiled registry recipe.
    pub fn runs_sync_recipe(&self, run_id: &str) -> Result<RunRecipeSyncReport> {
        super::runs_sync_recipe_in_home_with_cache(&self.home_dir, &self.router_cache, run_id)
    }

    /// Loads the live lease record for the root run tree containing `run_id`, when present.
    pub fn runs_lease_inspect(&self, run_id: &str) -> Result<Option<RunTreeLeaseRecord>> {
        super::runs_lease_inspect_in_home(&self.home_dir, run_id)
    }

    /// Breaks the live lease for the root run tree containing `run_id`, when present.
    pub fn runs_lease_break(
        &self,
        run_id: &str,
        force_live_owner: bool,
    ) -> Result<Option<RunTreeLeaseRecord>> {
        super::runs_lease_break_in_home(&self.home_dir, run_id, force_live_owner)
    }

    /// Sweeps the lease directory and reaps orphaned leases (owning PID dead
    /// and heartbeat past the configured staleness threshold).
    ///
    /// Honors `TRANSIST_REAPER_DISABLE` and `TRANSIST_REAPER_STALE_AFTER_MS`.
    pub fn release_orphan_leases(&self) -> Result<Vec<RunTreeLeaseRecord>> {
        super::runs_release_orphan_leases_in_home(&self.home_dir)
    }

    /// Writes a durable cancellation request for the root run tree containing `run_id`.
    pub fn request_run_cancellation(&self, run_id: &str) -> Result<RunCancellationRequest> {
        super::request_run_cancellation_in_home_with_cache(
            &self.home_dir,
            &self.router_cache,
            run_id,
        )
    }

    /// Lists known run identifiers from this context's home directory.
    pub fn runs_list(&self) -> Result<Vec<String>> {
        super::runs_list_in_home(&self.home_dir)
    }

    /// Loads stored run state for a single run.
    pub fn runs_show(&self, run_id: &str) -> Result<KernelValue> {
        super::runs_show_in_home(&self.home_dir, run_id)
    }

    /// Returns up to `limit` latest runtime events for a run.
    pub fn runs_tail(&self, run_id: &str, limit: usize) -> Result<Vec<RuntimeEvent>> {
        super::runs_tail_in_home(&self.home_dir, run_id, limit)
    }

    /// Exports a read-only evidence summary for one run.
    pub fn run_evidence_export(&self, run_id: &str) -> Result<RunEvidenceExport> {
        super::run_evidence_export_in_home(&self.home_dir, run_id)
    }

    /// Cancels the active invoke step in a run tree, if one is currently running.
    pub fn cancel_active_run_execution(
        &self,
        run_id: &str,
    ) -> Result<Option<ActiveRunCancellation>> {
        super::cancel_active_run_execution_in_home_with_cache(
            &self.repo_root,
            &self.home_dir,
            &self.router_cache,
            run_id,
        )
    }
}
