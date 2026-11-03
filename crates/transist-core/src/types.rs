// SPDX-License-Identifier: MPL-2.0
//! Persisted runtime state records and inspection-oriented summaries.

use crate::failures::StateMutation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use transist_protocol::{
    CleanupTrigger, ExecutionRequest, ExecutionResult, KernelValue, WorkspaceBinding,
};

/// Stable run-level lifecycle statuses persisted by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelRunStatus {
    /// Run record exists but execution has not been planned yet.
    Created,
    /// Run has been planned and is ready to execute.
    Planned,
    /// Run currently has active or runnable work.
    Running,
    /// Run is waiting on a retry deadline.
    Waiting,
    /// Run is blocked on a manual gate or similar operator action.
    Blocked,
    /// Run completed successfully.
    Success,
    /// Run completed with a failure.
    Failed,
    /// Run was canceled.
    Canceled,
}

impl KernelRunStatus {
    /// Returns the stable string form used in persisted run state.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    /// Returns whether this run status is terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Success | Self::Failed | Self::Canceled)
    }

    /// Returns whether this status indicates active (non-terminal) progression.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Created | Self::Planned | Self::Running | Self::Waiting | Self::Blocked
        )
    }

    /// Returns the valid successor statuses from this status.
    pub fn transitions(&self) -> &'static [Self] {
        match self {
            Self::Created => &[Self::Planned, Self::Running, Self::Failed, Self::Canceled],
            Self::Planned => &[Self::Running, Self::Canceled],
            Self::Running => &[
                Self::Waiting,
                Self::Blocked,
                Self::Success,
                Self::Failed,
                Self::Canceled,
            ],
            Self::Waiting => &[Self::Running, Self::Failed, Self::Canceled],
            Self::Blocked => &[Self::Running, Self::Failed, Self::Canceled],
            Self::Success => &[Self::Canceled],
            Self::Failed => &[Self::Running, Self::Canceled],
            Self::Canceled => &[Self::Running, Self::Canceled],
        }
    }

    /// Returns whether transition to `other` is allowed by the lifecycle matrix.
    pub fn can_transition_to(&self, other: Self) -> bool {
        self.transitions().contains(&other)
    }
}

/// Persisted recoverability state for a failed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelRunRecoveryState {
    /// The failed run may still be resumed.
    Recoverable,
    /// Explicit cleanup has begun and must be retried instead of resuming.
    CleanupRequired,
    /// Explicit cleanup completed, so the failed run is no longer resumable.
    CleanupCompleted,
}

impl KernelRunRecoveryState {
    /// Returns the stable lowercase string form.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Recoverable => "recoverable",
            Self::CleanupRequired => "cleanup_required",
            Self::CleanupCompleted => "cleanup_completed",
        }
    }
}

/// Persisted cleanup lifecycle states retained separately from the primary run outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPhaseState {
    /// Matching cleanup exists but has not run yet.
    Pending,
    /// Cleanup is currently running.
    Running,
    /// Cleanup completed successfully.
    Succeeded,
    /// Cleanup completed with one or more failures.
    Failed,
    /// Cleanup was intentionally skipped for this outcome.
    Skipped,
}

impl CleanupPhaseState {
    /// Returns the stable lowercase string form.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// Stable per-step lifecycle statuses persisted by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelStepStatus {
    /// Step has not become runnable yet.
    Pending,
    /// Step is eligible for dispatch.
    Runnable,
    /// Step request is being prepared or sent.
    Dispatching,
    /// Step is actively executing.
    Running,
    /// Step is blocked on a gate or child workflow state.
    Blocked,
    /// Step completed successfully.
    Succeeded,
    /// Step completed with a failure.
    Failed,
    /// Step is waiting for a retry deadline.
    RetryWait,
    /// Step was skipped.
    Skipped,
    /// Step was canceled.
    Canceled,
}

impl KernelStepStatus {
    /// Returns whether this step status is terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Skipped | Self::Canceled
        )
    }

    /// Returns whether this status indicates active (non-terminal) progression.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Pending
                | Self::Runnable
                | Self::Dispatching
                | Self::Running
                | Self::Blocked
                | Self::RetryWait
        )
    }

    /// Returns the valid successor statuses from this status.
    pub fn transitions(&self) -> &'static [Self] {
        match self {
            Self::Pending => &[Self::Runnable, Self::Skipped, Self::Canceled],
            Self::Runnable => &[Self::Dispatching, Self::Skipped, Self::Canceled],
            Self::Dispatching => &[Self::Running, Self::Skipped, Self::Canceled],
            Self::Running => &[
                Self::Blocked,
                Self::Succeeded,
                Self::Failed,
                Self::RetryWait,
                Self::Skipped,
                Self::Canceled,
            ],
            Self::Blocked => &[Self::Runnable, Self::Skipped, Self::Canceled],
            Self::Succeeded => &[Self::Skipped, Self::Canceled],
            Self::Failed => &[Self::Runnable, Self::Skipped, Self::Canceled],
            Self::RetryWait => &[Self::Runnable, Self::Skipped, Self::Canceled],
            Self::Skipped => &[Self::Skipped, Self::Canceled],
            Self::Canceled => &[Self::Runnable, Self::Skipped, Self::Canceled],
        }
    }

    /// Returns whether transition to `other` is allowed by the lifecycle matrix.
    pub fn can_transition_to(&self, other: Self) -> bool {
        self.transitions().contains(&other)
    }
}

/// Reason a step was skipped without execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelSkipReason {
    /// One or more dependencies failed.
    DependencyFailed,
    /// A guard expression prevented the step from running.
    GuardBlocked,
}

/// Relationship between a run and the run lineage that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLineageMode {
    /// Run was started from authored workflow state.
    Fresh,
    /// Run was created by replaying another run.
    Replay,
}

/// Reference to a persisted recipe snapshot owned by a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeSnapshotRef {
    /// Relative snapshot path within the run store.
    pub path: String,
    /// SHA-256 checksum of the snapshot contents.
    pub sha256: String,
}

/// Stable schema identifier for trusted-execution run decisions.
pub const TRUST_DECISION_SCHEMA_V1: &str = "transist.trust_decision.v1";

/// Per-input trusted-execution decision persisted for one workflow or plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustSubjectDecision {
    /// Stable workflow or plugin id.
    pub id: String,
    /// Stable workflow or plugin version.
    pub version: String,
    /// Recomputed digest evaluated at decision time.
    pub digest: String,
    /// Whether detached-signature verification succeeded.
    pub signature_verified: bool,
    /// Signer key id when one was present in the detached signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key_id: Option<String>,
    /// Stable verification reason code when the subject was denied or bypassed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Subject-level outcome: `verified`, `denied`, or `bypassed`.
    pub trust_decision: String,
}

/// Persisted trusted-execution decision recorded for one run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustDecisionRecord {
    /// Record schema version string.
    pub schema: String,
    /// Stable run id.
    pub run_id: String,
    /// Whether trusted-execution mode was enabled for the run.
    pub trusted_execution_mode: bool,
    /// Run-level outcome: `allowed`, `denied`, or `bypassed`.
    pub decision: String,
    /// Workflow verification result.
    pub workflow: TrustSubjectDecision,
    /// Referenced plugin verification results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<TrustSubjectDecision>,
    /// Stable denial reason code when the run was denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Which verification surface failed, such as `workflow_signature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_verification_type: Option<String>,
    /// Reserved future central organization identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Reserved future policy revision identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_revision: Option<String>,
    /// Decision timestamp in Unix milliseconds.
    pub decided_at_ms: u64,
}

impl TrustDecisionRecord {
    /// Returns `true` when the persisted decision denies execution.
    pub fn denied(&self) -> bool {
        self.decision == "denied"
    }
}

/// Stable schema identifier for persisted run provenance.
pub const RUN_PROVENANCE_SCHEMA_V1: &str =
    crate::infra::records::schema_versions::RUN_PROVENANCE_RECORD_SCHEMA_V1;

/// Workflow or plugin provenance captured for one run subject.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunProvenanceSubject {
    /// Stable subject id.
    pub id: String,
    /// Stable subject version.
    pub version: String,
    /// Recomputed digest captured for the subject.
    pub digest: String,
    /// Whether detached-signature verification succeeded.
    pub signature_verified: bool,
    /// Signer key id when one was present in the detached signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key_id: Option<String>,
    /// Subject-level trust outcome.
    pub trust_decision: String,
}

/// Persisted policy decision summary attached to a provenance record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunProvenancePolicyDecision {
    /// Whether policy allowed the evaluated action.
    pub allowed: bool,
    /// Stable policy reason code when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Optional human-readable policy message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Policy checkpoint or mode that produced the decision.
    pub mode: String,
}

/// Provider and model provenance retained from invoke usage metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunProviderProvenance {
    /// Step id that reported the provider/model pair.
    pub step_id: String,
    /// Provider identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Forward-compatible provider locality marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
}

/// Persisted approval summary for one manual gate decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunApprovalProvenance {
    /// Stable gate id.
    pub gate_id: String,
    /// Recorded decision such as `approved` or `rejected`.
    pub decision: String,
    /// Reserved future actor identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Optional human rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional decision timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at_ms: Option<u64>,
}

/// Persisted external-transfer summary for one run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunExternalTransferSummary {
    /// Number of recorded external transfers.
    pub count: u64,
    /// Recorded transfer ledger entries for this run.
    #[serde(default)]
    pub external_transfers: Vec<ExternalTransferRef>,
}

/// Persisted reference to one recorded external-transfer evidence file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTransferRef {
    /// Step id that triggered the external call.
    pub step_id: String,
    /// Run-root-relative path to the recorded evidence file.
    pub evidence_path: String,
    /// Best-effort destination identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Adapter kind used for the call.
    pub adapter_kind: String,
    /// Provider identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Declared provider locality marker.
    pub provider_kind: String,
    /// SHA-256 hash of the payload sent to the external destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Timestamp in Unix milliseconds when the transfer was recorded.
    pub timestamp_ms: u64,
}

/// Persisted denial detail for a refused run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunProvenanceDenied {
    /// Stable reason code or summary string for the denial.
    pub reason: String,
    /// Denied surface kind such as `workflow` or `plugin`.
    pub offending_kind: String,
    /// Stable offending workflow or plugin id.
    pub offending_id: String,
    /// Specific verification or policy surface that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_verification_type: Option<String>,
    /// Denial timestamp in Unix milliseconds.
    pub at_ms: u64,
}

/// Durable run provenance written beside `state.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunProvenanceRecord {
    /// Record schema version string.
    pub schema: String,
    /// Stable run id.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Workflow digest captured for trust verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_digest: Option<String>,
    /// Whether workflow signature verification succeeded.
    pub workflow_signature_verified: bool,
    /// Signer key id when one was present in the workflow signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_signer_key_id: Option<String>,
    /// Workflow trust outcome: `verified`, `denied`, `bypassed`, or `not_verified`.
    pub workflow_trust_decision: String,
    /// Referenced plugin provenance summaries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<RunProvenanceSubject>,
    /// Whether trusted-execution mode was enabled for this run.
    pub trusted_execution_mode: bool,
    /// Whether local-only mode was enforced for this run.
    #[serde(default)]
    pub local_only_mode: bool,
    /// Effective runtime operating mode for the run.
    #[serde(default = "default_runtime_mode")]
    pub runtime_mode: String,
    /// Effective policy profile for the run.
    #[serde(default = "default_policy_profile")]
    pub policy_profile: String,
    /// Whether capability grants were enforced or explicitly bypassed.
    #[serde(default = "default_capability_enforcement")]
    pub capability_enforcement: String,
    /// Persisted run-start policy decision summary.
    pub policy_decision: RunProvenancePolicyDecision,
    /// Provider/model provenance captured from invoke usage metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<RunProviderProvenance>,
    /// External-transfer summary for the run.
    pub external_transfer_summary: RunExternalTransferSummary,
    /// Manual-gate approval summaries captured for the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approval_summary: Vec<RunApprovalProvenance>,
    /// Run-level outcome: `allowed` or `denied`.
    pub decision: String,
    /// Denial metadata when the run was refused before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied: Option<RunProvenanceDenied>,
    /// Reserved future central organization identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Reserved future policy revision identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_revision: Option<String>,
    /// Record creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Finalization timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at_ms: Option<u64>,
}

/// Kind of live asset locked at run creation time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssetLockKind {
    /// Pinned plugin directory.
    Plugin,
    /// Resolved prompt file.
    PromptFile,
}

/// One run-level asset lock persisted on the run record.
///
/// At run creation, every invoke step's pinned plugin and resolved
/// prompt file is materialized into a lock so re-entry paths can verify
/// byte-level immutability without re-parsing the recipe snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetLock {
    /// Step id that references this asset.
    pub step_id: String,
    /// Kind of locked asset.
    pub kind: AssetLockKind,
    /// Plugin id for plugin locks; authored prompt_file template for prompt files.
    pub ref_id: String,
    /// Concrete plugin version for plugin locks; empty for prompt files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// SHA-256 digest of the locked bytes at run creation time.
    pub sha256: String,
}

/// Persisted lineage metadata for a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunLineage {
    /// Whether the run is fresh or replayed.
    pub mode: RunLineageMode,
    /// Optional immediate parent run id for replayed runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    /// Root run id for the lineage tree.
    pub root_run_id: String,
    /// Optional direct parent run id for embedded child runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Optional parent step id for embedded child runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_step_id: Option<String>,
}

/// Durable runtime state for one workflow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelStepState {
    /// Stable step id.
    pub step_id: String,
    /// Current lifecycle status.
    pub status: KernelStepStatus,
    /// Lifetime number of executions dispatched for the step under the
    /// current run id. This counter is append-only: it is never reset to
    /// zero, so the next dispatch starts at `N+1` and prior execution
    /// records, artifacts, and evidence remain inspectable. Execution
    /// record paths and artifact directories use this lifetime ordinal.
    pub executions: u32,
    /// Baseline lifetime execution count captured when the current rerun
    /// activation began. Loop budgets (`max_iters`), scheduler cyclic-entry
    /// bootstrap, and author-facing `executions` expressions use
    /// `executions - activation_executions_baseline` (the activation-local
    /// count) so that a reused looping child can rerun its internal loop
    /// from scratch without exceeding `max_iters` from a prior activation.
    /// Defaults to zero for records created before this field existed, so
    /// existing persisted runs remain readable without migration.
    #[serde(default)]
    pub activation_executions_baseline: u32,
    /// Optional Unix timestamp at which retry becomes eligible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_wait_until_ms: Option<u64>,
    /// Optional reason the step was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<KernelSkipReason>,
    /// Last dispatched execution request, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request: Option<ExecutionRequest>,
    /// Last execution result, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<ExecutionResult>,
    /// Optional Unix timestamp when the current or latest execution started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Optional Unix timestamp when the current or latest execution finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
}

impl KernelStepState {
    /// Builds the initial pending state for a step id.
    pub fn pending(step_id: &str) -> Self {
        Self {
            step_id: step_id.to_string(),
            status: KernelStepStatus::Pending,
            executions: 0,
            activation_executions_baseline: 0,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: None,
            finished_at_ms: None,
        }
    }

    /// Returns the number of executions in the current rerun activation.
    /// This is the lifetime `executions` minus the `activation_executions_baseline`
    /// captured when this activation began. Loop budgets, scheduler bootstrap,
    /// and author-facing `executions` expressions use this value so that a
    /// reused looping child can rerun its internal loop from scratch on each
    /// activation without exceeding `max_iters` from a prior activation.
    pub fn activation_executions(&self) -> u32 {
        self.executions
            .saturating_sub(self.activation_executions_baseline)
    }
}

/// In-memory snapshot of the current durable run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelRunSnapshot {
    /// Stable run id.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Current run lifecycle status.
    pub status: KernelRunStatus,
    /// Step-state map keyed by step id.
    pub steps: BTreeMap<String, KernelStepState>,
}

impl KernelRunSnapshot {
    /// Builds a new snapshot with all supplied steps initialized as pending.
    pub fn new(
        run_id: &str,
        workflow_id: &str,
        workflow_version: &str,
        step_ids: &[String],
    ) -> Self {
        let mut steps = BTreeMap::new();
        for step_id in step_ids {
            steps.insert(step_id.clone(), KernelStepState::pending(step_id));
        }
        Self {
            run_id: run_id.to_string(),
            workflow_id: workflow_id.to_string(),
            workflow_version: workflow_version.to_string(),
            status: KernelRunStatus::Created,
            steps,
        }
    }
}

/// Durable cleanup state persisted beside the primary run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedCleanupRecord {
    /// Terminal outcome that selected the cleanup steps.
    pub trigger: CleanupTrigger,
    /// Current cleanup lifecycle status.
    pub state: CleanupPhaseState,
    /// Cleanup step-state map keyed by cleanup step id.
    #[serde(default)]
    pub steps: BTreeMap<String, KernelStepState>,
    /// Optional cleanup preflight summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight: Option<PersistedPreflightRecord>,
    /// Cleanup start timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Cleanup finish timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
}

impl PersistedCleanupRecord {
    /// Builds a new cleanup record with all supplied cleanup steps initialized as pending.
    pub fn new(trigger: CleanupTrigger, step_ids: &[String], state: CleanupPhaseState) -> Self {
        let mut steps = BTreeMap::new();
        for step_id in step_ids {
            steps.insert(step_id.clone(), KernelStepState::pending(step_id));
        }
        Self {
            trigger,
            state,
            steps,
            preflight: None,
            started_at_ms: None,
            finished_at_ms: None,
        }
    }
}

/// Default schema string for [`PersistedRunRecord`] used when serde encounters
/// older on-disk records that predate the explicit `schema` field.
fn default_run_state_schema() -> String {
    crate::infra::records::schema_versions::RUN_STATE_RECORD_SCHEMA_V1.to_string()
}

fn default_runtime_mode() -> String {
    "unknown".to_string()
}

fn default_policy_profile() -> String {
    "unknown".to_string()
}

fn default_capability_enforcement() -> String {
    "unknown".to_string()
}

/// Durable run record persisted by the run store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedRunRecord {
    /// Record schema version string.
    ///
    /// Defaults to the stable run-state schema identifier when missing
    /// from on-disk records written by older Transist versions. Readers should
    /// reject unknown values to fail closed against future incompatible
    /// formats.
    #[serde(default = "default_run_state_schema")]
    pub schema: String,
    /// Stable run id.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Current run lifecycle status.
    pub status: KernelRunStatus,
    /// Step-state map keyed by step id.
    pub steps: BTreeMap<String, KernelStepState>,
    /// Execution counters keyed by step id.
    #[serde(default)]
    pub executions: BTreeMap<String, u32>,
    /// Optional recipe snapshot reference retained for resume and replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_snapshot: Option<RecipeSnapshotRef>,
    /// Optional lineage metadata retained for replay trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RunLineage>,
    /// Number of explicit resume operations applied to this run.
    #[serde(default)]
    pub resume_count: u32,
    /// Materialized workflow inputs used to start the run.
    #[serde(default)]
    pub workflow_inputs: BTreeMap<String, KernelValue>,
    /// Canonical execution workspace captured at run birth, when the workflow
    /// declared one. Missing metadata preserves the legacy CLI-root fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceBinding>,
    /// Accumulated workflow variables written by `set` blocks during the run.
    ///
    /// Persisted alongside `workflow_inputs` so vars survive interruption and
    /// resume. Defaults to empty for runs created before this field existed.
    #[serde(default)]
    pub workflow_vars: BTreeMap<String, KernelValue>,
    /// Persisted recoverability state for failed runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_state: Option<KernelRunRecoveryState>,
    /// Optional persisted cleanup summary for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<PersistedCleanupRecord>,
    /// Optional persisted preflight summary for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight: Option<PersistedPreflightRecord>,
    /// Whether trusted-execution mode is enforced for this run.
    #[serde(default)]
    pub trusted_execution_mode: bool,
    /// Persisted trusted-execution decision for this run when the mode is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_decision: Option<TrustDecisionRecord>,
    /// Whether local-only mode is enforced for this run. When true, any action
    /// whose effective `provider_kind` is `external` is denied before dispatch.
    #[serde(default)]
    pub local_only_mode: bool,
    /// Effective runtime operating mode for the run.
    #[serde(default = "default_runtime_mode")]
    pub runtime_mode: String,
    /// Effective policy profile for the run.
    #[serde(default = "default_policy_profile")]
    pub policy_profile: String,
    /// Whether capability grants were enforced or explicitly bypassed.
    #[serde(default = "default_capability_enforcement")]
    pub capability_enforcement: String,
    /// Run-level asset locks materialized at run creation time so re-entry
    /// paths can verify byte-level immutability of pinned plugins and
    /// resolved prompt files without re-parsing the recipe snapshot. Empty
    /// for legacy runs created before asset locking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_locks: Vec<AssetLock>,
    /// Version of the persisted run-level asset-lock contract.
    ///
    /// `0` means the run predates durable asset-lock verification and is
    /// treated as a legacy run for re-entry purposes. `1` is the initial
    /// persisted asset-lock contract; higher revisions are unsupported.
    /// Current births use `1` with `workflow_asset_snapshot`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub asset_lock_version: u32,
    /// Whether workflow assets must resolve from private captured bytes.
    /// Missing/false preserves historical version-1 authored-source semantics;
    /// missing captures never permit fallback to authored source.
    #[serde(default, skip_serializing_if = "is_false")]
    pub workflow_asset_snapshot: bool,
    /// Whether the owning workflow uses the reserved `v0` test-workflow version.
    #[serde(default)]
    pub test_mode: bool,
    /// Version of the persisted authority contract used at run birth.
    /// `0` denotes a legacy/synthetic record; `1` requires a materialized
    /// capability projection on current non-test recipe snapshots.
    #[serde(default)]
    pub capability_authority_version: u32,
    /// Record creation timestamp in Unix milliseconds.
    #[serde(default)]
    pub created_at_ms: u64,
    /// Last update timestamp in Unix milliseconds.
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl PersistedRunRecord {
    /// Builds a persisted run record from an in-memory snapshot plus durable metadata.
    pub fn from_snapshot(
        snapshot: KernelRunSnapshot,
        recipe_snapshot: Option<RecipeSnapshotRef>,
        lineage: Option<RunLineage>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        resume_count: u32,
        created_at_ms: u64,
        updated_at_ms: u64,
    ) -> Self {
        let executions = snapshot
            .steps
            .iter()
            .map(|(step_id, step)| (step_id.clone(), step.executions))
            .collect();
        Self {
            schema: default_run_state_schema(),
            run_id: snapshot.run_id,
            workflow_id: snapshot.workflow_id,
            workflow_version: snapshot.workflow_version,
            status: snapshot.status,
            steps: snapshot.steps,
            executions,
            recipe_snapshot,
            lineage,
            resume_count,
            workflow_inputs,
            workspace: None,
            workflow_vars: BTreeMap::new(),
            recovery_state: None,
            cleanup: None,
            preflight: None,
            trusted_execution_mode: false,
            trust_decision: None,
            local_only_mode: false,
            runtime_mode: default_runtime_mode(),
            policy_profile: default_policy_profile(),
            capability_enforcement: default_capability_enforcement(),
            asset_locks: Vec::new(),
            asset_lock_version: 0,
            workflow_asset_snapshot: false,
            test_mode: false,
            capability_authority_version: 0,
            created_at_ms,
            updated_at_ms,
        }
    }

    /// Rejects unsupported persisted contract revisions without changing state.
    /// Zero remains valid for synthetic/uninitialized records; recovery applies
    /// its additional pre-lock rejection separately.
    pub fn validate_contract_versions(&self) -> anyhow::Result<()> {
        if self.asset_lock_version > 1 {
            anyhow::bail!(
                "run '{}' uses unsupported asset lock version {}; expected version 1",
                self.run_id,
                self.asset_lock_version
            );
        }
        if self.capability_authority_version > 1 {
            anyhow::bail!(
                "run '{}' uses unsupported capability authority version {}; expected version 0 or 1",
                self.run_id,
                self.capability_authority_version
            );
        }
        Ok(())
    }

    /// Returns whether this record requires captured workflow assets.
    /// This is a metadata-only decision and never probes storage.
    pub fn uses_workflow_asset_snapshot(&self) -> bool {
        self.workflow_asset_snapshot
    }

    /// Returns an in-memory snapshot view of the durable run record.
    pub fn snapshot(&self) -> KernelRunSnapshot {
        KernelRunSnapshot {
            run_id: self.run_id.clone(),
            workflow_id: self.workflow_id.clone(),
            workflow_version: self.workflow_version.clone(),
            status: self.status,
            steps: self.steps.clone(),
        }
    }

    /// Recomputes per-step execution counters from the current step map.
    pub fn refresh_executions(&mut self) {
        self.executions = self
            .steps
            .iter()
            .map(|(step_id, step)| (step_id.clone(), step.executions))
            .collect();
    }
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Immutable record for one persisted step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionRecord {
    /// All managed outputs published atomically by this successful completion record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_revisions: Vec<transist_protocol::DocumentRevision>,
    /// Run id that owns the execution.
    pub run_id: String,
    /// Step id that produced the execution.
    pub step_id: String,
    /// Execution number.
    pub execution: u32,
    /// Terminal or transitional status string recorded for the execution.
    pub status: String,
    /// Optional start timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Optional finish timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Optional Unix timestamp at which a committed retryable failure becomes
    /// eligible for retry. Set when the committed execution transitioned the
    /// owning step to `RetryWait`. Recovery reconciles the step's `RetryWait`
    /// state from this deadline instead of degrading a committed retryable
    /// failure to a plain `Failed`; a nil deadline means the committed
    /// execution was not retryable. Defaults to `None` for records predating
    /// this field so existing immutable records remain readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_wait_until_ms: Option<u64>,
    /// Optional normalized execution request for the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<ExecutionRequest>,
    /// Optional normalized execution result for the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExecutionResult>,
    /// Optional count of metrics dropped during normalization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_validation_errors: Option<u32>,
    /// Observable state mutations completed before the execution finished.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_mutations: Vec<StateMutation>,
}

/// Compact step summary used by high-level run summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelStepSummary {
    /// Stable step id.
    pub step_id: String,
    /// Current or terminal step status.
    pub status: KernelStepStatus,
    /// Number of executions observed for the step.
    pub executions: u32,
    /// Latest exit code when one is available.
    pub exit_code: Option<i32>,
}

/// Compact run summary used by high-level kernel APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelRunSummary {
    /// Stable run id.
    pub run_id: String,
    /// Current or terminal run status.
    pub status: KernelRunStatus,
    /// Per-step summaries ordered by step id.
    pub steps: Vec<KernelStepSummary>,
    /// Lease-release warning retained when the durable result committed but
    /// lease finalization failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_release_warning: Option<LeaseReleaseWarning>,
}

/// Structured warning produced when a durable operation completed but its
/// mutation lease could not be released cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseReleaseWarning {
    /// Contextual release failure returned by the lease port.
    pub message: String,
}

/// Durable operation result together with non-fatal lease finalization state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelOperationOutcome<T> {
    /// Result committed by the kernel before lease finalization.
    pub value: T,
    /// Release warning, when the durable result committed but lease release failed.
    pub lease_release_warning: Option<LeaseReleaseWarning>,
}

/// Result of replacing a run-scoped recipe snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeSyncResult {
    /// Run whose persisted recipe snapshot was replaced.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Registry recipe path used as the sync source.
    pub source_registry_path: std::path::PathBuf,
    /// Persisted run-scoped recipe path that was updated.
    pub run_recipe_path: std::path::PathBuf,
}

/// Overall status assigned to a preflight report or check aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightStatus {
    /// All checks passed.
    Passed,
    /// Static and authority requirements pass; input-dependent checks run at dispatch.
    DeferredToDispatch,
    /// At least one warning exists but no blocker exists.
    Warning,
    /// At least one blocking failure exists.
    Failed,
}

impl PreflightStatus {
    /// Returns the stable lowercase string persisted and exposed by inspection layers.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::DeferredToDispatch => "deferred_to_dispatch",
            Self::Warning => "warning",
            Self::Failed => "failed",
        }
    }
}

/// One preflight finding for a candidate run or step invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreflightCheck {
    /// Check family or kind name.
    pub kind: String,
    /// Result status for the individual check.
    pub status: PreflightStatus,
    /// Step ids associated with the finding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_ids: Vec<String>,
    /// Optional adapter family associated with the finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_kind: Option<String>,
    /// Optional plugin id associated with the finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Optional action id associated with the finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Durable aggregate preflight record stored with a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedPreflightRecord {
    /// Aggregate preflight status.
    pub status: PreflightStatus,
    /// Timestamp in Unix milliseconds when preflight was evaluated.
    pub checked_at_ms: u64,
    /// Number of blocking checks.
    pub blocker_count: usize,
    /// Number of warning checks.
    pub warning_count: usize,
    /// Individual checks retained for inspection.
    #[serde(default)]
    pub checks: Vec<PreflightCheck>,
}

impl PersistedPreflightRecord {
    /// Builds an aggregate preflight record from individual checks.
    pub fn from_checks(checked_at_ms: u64, checks: Vec<PreflightCheck>) -> Self {
        let blocker_count = checks
            .iter()
            .filter(|check| check.status == PreflightStatus::Failed)
            .count();
        let warning_count = checks
            .iter()
            .filter(|check| check.status == PreflightStatus::Warning)
            .count();
        let status = if blocker_count > 0 {
            PreflightStatus::Failed
        } else if warning_count > 0 {
            PreflightStatus::Warning
        } else {
            PreflightStatus::Passed
        };
        Self {
            status,
            checked_at_ms,
            blocker_count,
            warning_count,
            checks,
        }
    }

    /// Converts the persisted record into a run-scoped public report.
    pub fn to_report(&self, run_id: impl Into<String>) -> PreflightReport {
        PreflightReport {
            run_id: run_id.into(),
            status: self.status,
            checked_at_ms: self.checked_at_ms,
            blocker_count: self.blocker_count,
            warning_count: self.warning_count,
            checks: self.checks.clone(),
            lease_release_warning: None,
        }
    }
}

/// Public preflight report returned by application APIs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreflightReport {
    /// Run id the report belongs to.
    pub run_id: String,
    /// Aggregate preflight status.
    pub status: PreflightStatus,
    /// Timestamp in Unix milliseconds when preflight was evaluated.
    pub checked_at_ms: u64,
    /// Number of blocking checks.
    pub blocker_count: usize,
    /// Number of warning checks.
    pub warning_count: usize,
    /// Individual checks retained for inspection.
    #[serde(default)]
    pub checks: Vec<PreflightCheck>,
    /// Lease-release warning from a committed mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_release_warning: Option<LeaseReleaseWarning>,
}
