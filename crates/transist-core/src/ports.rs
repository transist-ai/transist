// SPDX-License-Identifier: MPL-2.0
//! Runtime service ports and request/response records used by the kernel.

use crate::failures::FailureRecord;
use crate::trust::{ImportedTrustRoot, TrustRootRecord};
use crate::types::{AssetLock, PersistedRunRecord, PreflightCheck, TrustDecisionRecord};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use transist_protocol::{
    AdapterCapabilities, AdapterKind, ExecutionRequest, ExecutionResult, GateDecisionRecord,
    HomeSettings, KernelValue, ProviderKind, RuntimeEvent, StepExecutionDescriptor,
};

#[path = "ports/active_record.rs"]
mod active_record;
pub use active_record::{ActiveRecordPort, ActiveRecordWrite};

/// Borrowed run identifier used by observer hooks.
pub type RunId = str;

/// Verifies an installed plugin against the digest pinned in a run's asset lock.
pub trait PluginVerificationPort: Send + Sync {
    /// Verifies one plugin id/version pair and returns an error on missing or changed bytes.
    fn verify_plugin_digest(&self, home_dir: &Path, lock: &AssetLock) -> Result<()>;
}

/// Metadata declared by one plugin action, projected across the kernel boundary.
#[derive(Debug, Clone)]
pub struct PluginActionMetadata {
    /// Effective provider locality for the action.
    pub provider_kind: ProviderKind,
    /// Secret inputs declared by the action.
    pub secret_inputs: Vec<PluginSecretInput>,
}

/// One secret input declared by a plugin action.
#[derive(Debug, Clone)]
pub struct PluginSecretInput {
    /// Stable secret reference name.
    pub name: String,
    /// Whether the action requires this reference.
    pub required: bool,
}

/// Resolves plugin metadata without exposing manifest or installation details to the kernel.
pub trait PluginMetadataPort: Send + Sync {
    /// Resolves an action projection for an installed plugin version.
    fn resolve_action(
        &self,
        plugin_id: &str,
        plugin_version: &str,
        action: &str,
    ) -> Result<Option<PluginActionMetadata>>;
    /// Resolves the declared network origin for an installed plugin version.
    fn resolve_network_origin(
        &self,
        plugin_id: &str,
        plugin_version: &str,
    ) -> Result<Option<String>>;
}

/// Host-resolved prompt-file reference used by the kernel without filesystem access.
#[derive(Debug, Clone)]
pub struct PromptFileReference {
    /// Validated absolute or host-specific source path used for policy and audit.
    pub path: PathBuf,
    /// Immutable validated UTF-8 content supplied to the action.
    pub content: String,
    /// SHA-256 of the validated prompt-file bytes, computed by the host.
    pub sha256: String,
}

/// Resolves prompt files and computes their digests on behalf of the kernel.
pub trait PromptFilePort: Send + Sync {
    /// Resolves and validates one authored prompt-file path.
    fn resolve_prompt_file(
        &self,
        raw_path: &str,
        recipe_source_dir: Option<&Path>,
        step_id: &str,
    ) -> Result<PromptFileReference>;
    /// Reads one validated plugin-local source asset on behalf of the kernel.
    fn read_source_asset(
        &self,
        _raw_path: &str,
        _recipe_source_dir: Option<&Path>,
        _step_id: &str,
    ) -> Result<Vec<u8>> {
        anyhow::bail!("workflow source asset resolution is unavailable")
    }
}

/// Loads effective-home runtime settings for policy resolution.
pub trait HomeSettingsPort: Send + Sync {
    /// Loads the current settings DTO, returning defaults when the file is absent.
    fn load_settings(&self, home_dir: &Path) -> Result<HomeSettings>;
}

/// Resolves trusted-execution decisions for persisted run transitions.
pub trait TrustDecisionPort: Send + Sync {
    /// Recomputes the decision for a run from its persisted recipe and current
    /// repository/home trust inputs.
    fn resolve_run_decision(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        record: &PersistedRunRecord,
        recipe: &transist_protocol::Recipe,
    ) -> Result<TrustDecisionRecord>;
}

/// Policy controlling which adapter families may receive resolved secret values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteSecretPolicy {
    /// Whether process adapters may receive secret material directly.
    pub process: bool,
    /// Whether HTTP adapters may receive secret material directly.
    pub http: bool,
    /// Whether MCP adapters may receive secret material directly.
    pub mcp: bool,
}

impl Default for RemoteSecretPolicy {
    fn default() -> Self {
        Self {
            process: true,
            http: false,
            mcp: false,
        }
    }
}

impl RemoteSecretPolicy {
    /// Returns whether this policy allows secrets to be sent to the named adapter family.
    pub fn allows_adapter_kind(self, adapter_kind: &str) -> bool {
        adapter_kind
            .parse::<AdapterKind>()
            .map(|kind| self.allows_adapter_kind_enum(kind))
            .unwrap_or(false)
    }

    /// Returns whether this policy allows secrets for the provided adapter kind.
    pub fn allows_adapter_kind_enum(self, adapter_kind: AdapterKind) -> bool {
        match adapter_kind {
            AdapterKind::Process => self.process,
            AdapterKind::Http => self.http,
            AdapterKind::Mcp => self.mcp,
        }
    }

    /// Serializes the policy into a kernel value suitable for persistence or policy payloads.
    pub fn as_kernel_value(self) -> KernelValue {
        KernelValue::Object(BTreeMap::from([
            ("process".to_string(), KernelValue::Bool(self.process)),
            ("http".to_string(), KernelValue::Bool(self.http)),
            ("mcp".to_string(), KernelValue::Bool(self.mcp)),
        ]))
    }
}

/// Fully materialized execution call forwarded to an [`ExecutionPort`].
#[derive(Debug, Clone)]
pub struct ExecutionInvocation {
    /// Normalized execution request envelope for the step execution.
    pub request: ExecutionRequest,
    /// Resolved secret values keyed by secret input name.
    pub secret_values: BTreeMap<String, String>,
    /// Remote-secret policy that was applied for this invocation.
    pub remote_secret_policy: RemoteSecretPolicy,
}

/// Preflight probe request forwarded to a [`PreflightPort`].
#[derive(Debug, Clone)]
pub struct PreflightInvocation {
    /// Normalized execution request envelope for the candidate step.
    pub request: ExecutionRequest,
    /// Secret references confirmed available without resolving or delivering values.
    pub available_secret_refs: Vec<String>,
    /// Remote-secret policy that was applied for this probe.
    pub remote_secret_policy: RemoteSecretPolicy,
}

/// Cancellation request for an in-flight execution.
#[derive(Debug, Clone)]
pub struct ExecutionCancelRequest {
    /// Invoke id to cancel.
    pub invoke_id: String,
}

/// Result returned after attempting to cancel an execution.
#[derive(Debug, Clone)]
pub struct ExecutionCancelResult {
    /// Invoke id that was targeted for cancellation.
    pub invoke_id: String,
    /// Whether the cancellation request was accepted.
    pub accepted: bool,
}

/// Artifact write request emitted by the kernel after a step completes.
#[derive(Debug, Clone)]
pub struct ArtifactWriteRequest {
    /// Run id that owns the artifact.
    pub run_id: String,
    /// Step id that produced the artifact.
    pub step_id: String,
    /// Execution number that produced the artifact.
    pub execution: u32,
    /// Logical artifact path relative to the execution family root.
    pub logical_path: String,
    /// Artifact payload represented as a kernel value.
    pub payload: KernelValue,
}

/// Artifact read request used by inspection or runtime helpers.
#[derive(Debug, Clone)]
pub struct ArtifactReadRequest {
    /// Run id that owns the artifact.
    pub run_id: String,
    /// Step id that produced the artifact.
    pub step_id: String,
    /// Execution number that produced the artifact.
    pub execution: u32,
    /// Logical artifact path relative to the execution family root.
    pub logical_path: String,
}

/// Durable update for one step-state record in the run store.
#[derive(Debug, Clone)]
pub struct RunStoreStepState {
    /// Run id that owns the step.
    pub run_id: String,
    /// Step id being updated.
    pub step_id: String,
    /// Serialized step-state payload.
    pub state: KernelValue,
}

/// Durable immutable execution record for one step execution.
#[derive(Debug, Clone)]
pub struct RunStoreStepExecution {
    /// Run id that owns the execution.
    pub run_id: String,
    /// Step id that produced the execution.
    pub step_id: String,
    /// Execution number.
    pub execution: u32,
    /// Serialized execution payload.
    pub state: KernelValue,
}

/// Process-management mode configured for one runtime process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagementMode {
    /// Standalone/manual runtime behavior.
    Standalone,
    /// Experimental managed runtime behavior for externally supervised runs.
    #[serde(rename = "managed", alias = "fleet")]
    Managed,
}

impl ManagementMode {
    /// Returns the stable string label used in diagnostics and JSON payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Managed => "managed",
        }
    }
}

/// Security posture selected for local runtime execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOperatingMode {
    /// Standalone local execution with explicit policy configured in `config.yaml`.
    #[default]
    Standalone,
    /// Governed execution: trust and policy inputs must be present and allowed.
    Governed,
    /// Development-only execution; must be selected explicitly and should be visibly marked.
    Development,
}

impl RuntimeOperatingMode {
    /// Returns the stable string label used in config, diagnostics, and evidence.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Governed => "governed",
            Self::Development => "development",
        }
    }
}

/// Effective security posture selected by a Transist home configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePosture {
    /// Operating mode selected for the home.
    pub operating_mode: RuntimeOperatingMode,
    /// Policy profile selected for the home.
    pub policy_profile: String,
    /// Capability enforcement status recorded for operators.
    pub capability_enforcement: String,
}

impl RuntimePosture {
    /// Creates the legacy posture used by directly composed test kernels.
    pub fn legacy() -> Self {
        Self {
            operating_mode: RuntimeOperatingMode::Standalone,
            policy_profile: "permissive".to_string(),
            capability_enforcement: "bypassed".to_string(),
        }
    }
}

/// Optional external supervision metadata attached to an owner identity or lease.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalOwnerMetadata {
    /// External host identifier for the runtime process.
    #[serde(alias = "node_id")]
    pub host_id: String,
    /// Optional runner identifier within the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "worker_id")]
    pub runner_id: Option<String>,
    /// Optional external run identifier supplied by the supervisor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "fleet_run_id")]
    pub external_run_id: Option<String>,
}

/// Stable owner identity generated once per runtime process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeOwnerIdentity {
    /// Runtime management mode active for the process.
    pub management_mode: ManagementMode,
    /// Stable owner identifier for the process.
    pub owner_id: String,
    /// Stable lease identifier reused across lease acquisitions by the process.
    pub lease_id: String,
    /// Local operating-system process identifier.
    pub pid: u32,
    /// Hostname observed when the process started.
    pub hostname: String,
    /// Rendered command line used to start the process.
    pub command: String,
    /// Transist binary or library version for the process.
    pub transist_version: String,
    /// Optional external supervisor metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalOwnerMetadata>,
    /// Reserved forward-only central runtime enrollment identifier. Not
    /// populated by default; central fleet tooling can populate this
    /// later without breaking older runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// Reserved forward-only central organization identifier. Not populated
    /// by default; central fleet tooling can populate this later so an org
    /// can enroll runtimes into a fleet without re-architecting trust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
}

/// Persisted live lease record for one root run tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunTreeLeaseRecord {
    /// Record schema version string.
    pub schema_version: String,
    /// Root run id covered by the lease.
    pub root_run_id: String,
    /// Stable lease identifier for the owning process.
    pub lease_id: String,
    /// Stable owner identifier for the owning process.
    pub owner_id: String,
    /// Runtime management mode recorded when the lease was acquired.
    pub management_mode: ManagementMode,
    /// Local operating-system process identifier.
    pub pid: u32,
    /// Hostname observed by the owning process.
    pub hostname: String,
    /// Rendered command line used to start the owning process.
    pub command: String,
    /// Transist version reported by the owning process.
    pub transist_version: String,
    /// Lease acquisition timestamp in Unix milliseconds.
    pub acquired_at_ms: u64,
    /// Latest heartbeat timestamp in Unix milliseconds.
    pub last_heartbeat_ms: u64,
    /// Optional external supervisor metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalOwnerMetadata>,
}

impl RunTreeLeaseRecord {
    /// Stable schema identifier for persisted run-tree leases.
    pub const SCHEMA_VERSION: &str = "transist.run_tree_lease.v1";

    /// Returns whether the lease heartbeat appears stale at the supplied time.
    pub fn is_stale(&self, now_ms: u64, stale_after_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_heartbeat_ms) > stale_after_ms
    }
}

/// Lease token proving exclusive ownership of a root-run-tree mutation session.
#[derive(Debug, Clone)]
pub struct LeaseHandle {
    /// Root run id covered by the lease.
    pub root_run_id: String,
    /// Stable lease identifier for the owning process.
    pub lease_id: String,
    /// Stable process owner metadata for the lease holder.
    pub owner: RuntimeOwnerIdentity,
}

/// Structured lease conflict returned when another live owner already holds the run tree.
#[derive(Debug, Clone)]
pub struct LeaseConflict {
    /// Root run id that could not be acquired.
    pub root_run_id: String,
    /// Live lease record that blocked the acquisition.
    pub live_lease: RunTreeLeaseRecord,
}

impl std::fmt::Display for LeaseConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "root run tree '{}' is already leased by owner '{}' on '{}'",
            self.root_run_id, self.live_lease.owner_id, self.live_lease.hostname
        )
    }
}

impl std::error::Error for LeaseConflict {}

/// Run birth could not acquire the shared plugin-lifecycle reservation before
/// the bounded wait elapsed.
#[derive(Debug, Clone)]
pub struct PluginLifecycleReservationTimeout {
    /// Configured bounded wait in milliseconds.
    pub timeout_ms: u64,
    /// Most recent live lease that prevented run birth.
    pub live_lease: RunTreeLeaseRecord,
}

impl std::fmt::Display for PluginLifecycleReservationTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "plugin lifecycle reservation remained busy for {} ms (owner '{}', pid {}, host '{}', command '{}')",
            self.timeout_ms,
            self.live_lease.owner_id,
            self.live_lease.pid,
            self.live_lease.hostname,
            self.live_lease.command,
        )
    }
}

impl std::error::Error for PluginLifecycleReservationTimeout {}

/// A durable root-run cancellation stopped an embedded child before its run was created.
#[derive(Debug, Clone)]
pub struct PluginLifecycleReservationCanceled {
    /// Durable root run id whose cancellation stopped child run birth.
    pub root_run_id: String,
}

impl std::fmt::Display for PluginLifecycleReservationCanceled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "plugin lifecycle reservation wait canceled for root run '{}'",
            self.root_run_id,
        )
    }
}

impl std::error::Error for PluginLifecycleReservationCanceled {}

/// Options controlling an explicit lease-break request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseBreakOptions {
    /// Whether a locally live owner may be broken explicitly.
    pub force_live_owner: bool,
}

/// Supported durable control request kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlRequestType {
    /// Request cancellation of the active run tree.
    Cancel,
}

impl ControlRequestType {
    /// Returns the stable string label used in diagnostics and JSON payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
        }
    }
}

/// Optional requester metadata persisted with one control request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlRequesterMetadata {
    /// Stable owner identifier for the requester.
    pub owner_id: String,
    /// Local operating-system process identifier.
    pub pid: u32,
    /// Hostname observed by the requester.
    pub hostname: String,
    /// Rendered command line used to start the requester.
    pub command: String,
    /// Optional external supervisor metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalOwnerMetadata>,
}

/// Persisted durable control request for one root run tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlRequestRecord {
    /// Record schema version string.
    pub schema_version: String,
    /// Stable request identifier.
    pub request_id: String,
    /// Requested control action.
    pub request_type: ControlRequestType,
    /// Root run id covered by the request.
    pub root_run_id: String,
    /// Request creation timestamp in Unix milliseconds.
    pub requested_at_ms: u64,
    /// Optional requester metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<ControlRequesterMetadata>,
}

impl ControlRequestRecord {
    /// Stable schema identifier for persisted control requests.
    pub const SCHEMA_VERSION: &str = "transist.control_request.v1";
}

/// Policy engine decision returned for run start or step execution checks.
#[derive(Debug, Clone)]
pub struct PolicyDecision {
    /// Whether the operation is allowed to proceed.
    pub allowed: bool,
    /// Optional stable machine-readable denial reason.
    pub reason_code: Option<String>,
    /// Optional human-readable policy message.
    pub message: Option<String>,
    /// Additional policy payload preserved for inspection or downstream use.
    pub payload: KernelValue,
    /// Remote-secret policy chosen by the policy layer.
    pub remote_secret_policy: RemoteSecretPolicy,
}

/// Port used by the kernel to execute and optionally cancel step invocations.
pub trait ExecutionPort: Send + Sync {
    /// Returns the adapter capabilities exposed through this execution port.
    fn capabilities(&self) -> AdapterCapabilities;
    /// Executes one prepared step invocation and returns the normalized result.
    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult>;
    /// Attempts to cancel an in-flight execution previously started by this port.
    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult>;
}

/// Port used by the kernel to run preflight probes without executing workflow state changes.
pub trait PreflightPort: Send + Sync {
    /// Checks static contracts and known input portions without probing. JSON-pointer
    /// masks identify unresolved data; an empty mask requires full schema validation.
    /// Masked logical document objects are deferred string paths: check known schema
    /// type compatibility without inventing a path instance or probing a plugin.
    /// Implementations lacking this boundary fail closed for deferred invocations.
    fn validate_deferred_input(
        &self,
        _invocation: &PreflightInvocation,
        _deferred_paths: &[String],
    ) -> Result<Vec<PreflightCheck>> {
        anyhow::bail!("static deferred-input validation is unavailable")
    }
    /// Evaluates the supplied step invocations and returns one or more preflight checks.
    fn preflight_steps(&self, invocations: &[PreflightInvocation]) -> Result<Vec<PreflightCheck>>;
}

/// Port used by the kernel to persist and retrieve run-scoped artifacts.
pub trait ArtifactPort: Send + Sync {
    /// Persists one artifact payload and returns its concrete filesystem path.
    fn write_artifact(&self, request: &ArtifactWriteRequest) -> Result<PathBuf>;
    /// Loads one previously persisted artifact payload.
    fn read_artifact(&self, request: &ArtifactReadRequest) -> Result<KernelValue>;
}

/// Borrowed bundle of every artifact the kernel persists atomically at
/// run birth. The store stages these in a hidden directory and renames
/// the directory to the final run root only after every file is
/// flushed and synced, so a crash before the rename exposes no run and
/// a crash after the rename exposes a complete run. The run locator is
/// not part of this bundle; the caller writes it after the staged run
/// root is durable.
#[derive(Debug, Clone)]
pub struct RunBirthArtifacts<'a> {
    /// Initial persisted run record written to `state.json`.
    pub record: &'a PersistedRunRecord,
    /// Recipe snapshot YAML written to `recipe.yaml`.
    pub recipe_yaml: &'a str,
    /// Serialized `transist.run_inputs.v1` record written to
    /// `context/run_inputs.json`.
    pub run_inputs_json: &'a [u8],
    /// Serialized `transist.run_provenance.v1` record written to
    /// `provenance.json`.
    pub provenance_json: &'a [u8],
    /// Optional canonical workflow-asset manifest written under the run root.
    /// When present, every blob is committed with the other birth artifacts.
    pub workflow_asset_manifest_json: Option<&'a [u8]>,
    /// Exact workflow-asset blobs addressed by raw SHA-256 digest.
    pub workflow_asset_blobs: &'a [WorkflowAssetBlob],
}

/// One exact workflow asset included in an atomic run birth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowAssetBlob {
    /// Lowercase SHA-256 of `bytes`; also the durable blob filename.
    pub sha256: String,
    /// Exact source bytes captured by the caller.
    pub bytes: Vec<u8>,
}

/// One external-transfer evidence record paired with its run-relative path.
#[derive(Debug, Clone)]
pub struct ExternalTransferEvidence {
    /// Decoded persisted external-transfer evidence.
    pub record: transist_protocol::ExternalTransferRecord,
    /// Path to the evidence file relative to the run root.
    pub evidence_path: PathBuf,
}

/// Port implemented by the local storage backend for private invocation files.
///
/// The kernel owns validation and redaction policy, while the implementation
/// owns staging directories, filesystem traversal, and durable promotion.
pub trait InvocationStagingPort: Send {
    /// Copies committed documents into private storage and substitutes action-data
    /// paths. Backends without this boundary reject document-bearing requests.
    fn materialize_documents(
        &self,
        request: &mut ExecutionRequest,
        _store: &dyn DocumentStorePort,
    ) -> Result<()> {
        if transist_protocol::contains_document_binding(&request.input) {
            anyhow::bail!("document staging unavailable");
        }
        Ok(())
    }
    /// Returns all declared output locations; plugin-reported paths cannot select them.
    fn document_outputs(&self) -> Vec<DocumentOutput> {
        Vec::new()
    }
    /// Replaces invocation paths in result metadata with logical document references.
    /// Called after artifact/evidence promotion so concrete file refs remain verifiable.
    fn normalize_result(&self, _result: &mut ExecutionResult) -> Result<()> {
        Ok(())
    }
    /// Materializes declared workflow source assets into this invocation's
    /// private staging root.
    fn materialize_source_assets(&self, _assets: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        Ok(())
    }
    /// Replaces plugin-visible paths with the private staging paths.
    fn apply_to_request(&self, request: &mut ExecutionRequest);
    /// Validates, redacts, hashes, and promotes reported files.
    fn promote(
        &self,
        result: &mut ExecutionResult,
        redactor: &crate::infra::redaction::Redactor,
    ) -> Result<()>;
    /// Returns invocation-local paths that must be redacted before output is
    /// persisted as state, events, artifacts, evidence, or provenance.
    fn transient_redaction_values(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Runtime-owned document capture/publication support. Mutations require the existing
/// root-tree lease; candidates never grant authority or advance visible heads.
pub trait DocumentStorePort: Send + Sync {
    /// Validates and stores one invocation's complete output batch as unpublished
    /// candidates. Exact bytes are rejected when secret-bearing, never redacted.
    fn prepare_document_revisions(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
        outputs: &[DocumentOutput],
        redactor: &crate::infra::redaction::Redactor,
    ) -> Result<Vec<transist_protocol::DocumentRevision>>;
    /// Loads validated immutable candidates for the exact producing execution.
    fn prepared_document_revisions(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<Vec<transist_protocol::DocumentRevision>>;
    /// Derives all latest revisions from committed records, ignoring candidate blobs
    /// and cached heads. An unpublished document is absent, never borrowed from another run.
    fn document_heads(
        &self,
        run_id: &str,
    ) -> Result<BTreeMap<String, transist_protocol::DocumentRevision>>;
    /// Resolves an opaque immutable revision identity inside this run's strict root namespace.
    /// Unpublished candidates and other root trees are never returned.
    fn document_revision(
        &self,
        _run_id: &str,
        _revision_id: &str,
    ) -> Result<Option<transist_protocol::DocumentRevision>> {
        anyhow::bail!("document revision inspection unsupported")
    }
    /// Validates a committed revision and returns its exact file or tree-manifest bytes.
    fn read_document_blob(
        &self,
        run_id: &str,
        revision: &transist_protocol::DocumentRevision,
    ) -> Result<Vec<u8>>;
    /// Reads an exact regular-file member of a committed tree revision. The path
    /// must be manifest-relative; callers never receive durable host paths.
    fn read_document_member(
        &self,
        run_id: &str,
        revision: &transist_protocol::DocumentRevision,
        member: &str,
    ) -> Result<Vec<u8>>;
    /// Rebuilds the entire atomic heads projection from immutable committed records.
    fn reconcile_document_heads(&self, run_id: &str) -> Result<()>;
}

/// A runtime-selected output inside private staging, never a plugin-selected durable path.
#[derive(Debug, Clone)]
pub struct DocumentOutput {
    /// Root-lexical declared document name.
    pub document: String,
    /// File/tree capture location; storage walks it without following symlinks.
    pub path: PathBuf,
}

/// Port used by the kernel to persist run state, step state, and step executions.
pub trait RunStorePort: Send + Sync {
    /// Returns document support composed with this same execution-record store.
    /// Backends without support must reject recipes with document declarations.
    fn document_store(&self) -> Option<&dyn DocumentStorePort> {
        None
    }
    /// Creates the initial durable run record.
    fn create_run(&self, record: &PersistedRunRecord) -> Result<()>;
    /// Atomically stages and commits every run-birth artifact (initial
    /// state, recipe snapshot, run inputs, and run provenance) so a crash
    /// at any point during run creation exposes either no run or a
    /// complete run. The caller must write the run locator after this
    /// returns successfully; the run is not discoverable until the
    /// locator exists. Implementations that cannot stage on disk may
    /// store the artifacts atomically in their backing store instead.
    fn create_run_staged(&self, artifacts: &RunBirthArtifacts) -> Result<()>;
    /// Loads one immutable workflow-asset blob for a snapshot-backed run.
    /// Backends without local workflow-asset storage return `None`.
    fn read_workflow_asset_blob(&self, _run_id: &str, _sha256: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Loads the immutable workflow-asset manifest for a snapshot-backed run.
    /// Backends without local workflow-asset storage return `None`.
    fn read_workflow_asset_manifest(&self, _run_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Reports whether this store supports durable workflow-asset snapshots.
    fn supports_workflow_asset_snapshots(&self) -> bool {
        false
    }
    /// Returns whether a run record already exists.
    fn run_exists(&self, run_id: &str) -> Result<bool>;
    /// Loads the durable run record. Run absence is a distinct, non-error
    /// outcome: `Ok(None)` means no run record exists. Corruption and I/O
    /// errors are propagated as `Err` and must never be conflated with
    /// absence, so a corrupt run state cannot be reported as "not found".
    fn load_run(&self, run_id: &str) -> Result<Option<PersistedRunRecord>>;
    /// Replaces the durable run record state.
    fn update_run_state(&self, run_id: &str, record: &PersistedRunRecord) -> Result<()>;
    /// Commits a recipe snapshot and its corresponding run state as one
    /// recoverable transaction. Implementations must not expose a mixed pair
    /// after an interrupted write.
    fn sync_recipe_snapshot(
        &self,
        run_id: &str,
        record: &PersistedRunRecord,
        recipe_yaml: &str,
    ) -> Result<()>;
    /// Persists one step-state update.
    fn store_step_state(&self, step_state: &RunStoreStepState) -> Result<()>;
    /// Persists one immutable step-execution record.
    fn store_step_execution(&self, execution: &RunStoreStepExecution) -> Result<()>;
    /// Returns whether an execution record already exists for the given run,
    /// step, and execution number. Used as a pre-dispatch invariant check to
    /// ensure no prior execution can be overwritten before external work runs.
    fn step_execution_record_exists(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<bool>;
    /// Loads the immutable execution record for the given run, step, and
    /// execution number. Returns `Ok(None)` when no record exists.
    fn load_step_execution(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<Option<crate::types::StepExecutionRecord>>;
    /// Reads the durable runtime events stream for a run. Used to backfill
    /// missing failure/completion events on recovery without duplicating an
    /// event that was already durably published. Returns an empty vector when
    /// no events stream exists yet.
    fn load_run_events(&self, run_id: &str) -> Result<Vec<transist_protocol::RuntimeEvent>>;
    /// Appends one classified failure record for a run.
    fn store_failure_record(&self, failure: &FailureRecord) -> Result<()>;
    /// Reads the durable classified failure stream for a run. Used to backfill
    /// a missing failure record on recovery without duplicating one that was
    /// already durably appended. Returns an empty vector when no failure stream
    /// exists yet.
    fn load_run_failures(&self, run_id: &str) -> Result<Vec<FailureRecord>>;
    /// Writes the recipe snapshot associated with a run.
    fn write_recipe_snapshot(&self, run_id: &str, recipe_yaml: &str) -> Result<()>;
    /// Loads the recipe snapshot associated with a run.
    fn load_recipe_snapshot(&self, run_id: &str) -> Result<String>;

    /// Loads external-transfer evidence records and their run-relative paths.
    ///
    /// The store owns local artifact traversal and evidence decoding. The
    /// kernel owns provenance mapping, validation, and ordering.
    fn load_external_transfer_evidence(
        &self,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<Vec<ExternalTransferEvidence>>;

    /// Resolves a persisted run root through the store's layout policy.
    fn resolve_run_root(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf>;

    /// Resolves a persisted run root, allowing the legacy top-level layout.
    fn resolve_run_root_or_default(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf>;

    /// Resolves the public paths for one adapter invocation.
    ///
    /// The store owns the run-layout and execution-directory shape. The kernel
    /// supplies the resulting paths to the adapter without constructing local
    /// filesystem paths itself.
    fn resolve_invocation_paths(
        &self,
        home_dir: &Path,
        workspace_dir: &Path,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<transist_protocol::InvokePaths>;

    /// Reads bytes from one adapter-reported result file.
    fn read_result_file(&self, path: &Path) -> Result<Vec<u8>>;

    /// Returns whether a result file is contained by a canonicalized storage root.
    fn result_file_is_within(&self, path: &Path, root: &Path) -> bool;

    /// Resolves the root run id for a persisted run.
    fn resolve_root_run_id(&self, home_dir: &Path, run_id: &str) -> Result<String>;

    /// Resolves the persisted provenance path for a run.
    fn run_provenance_path(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf>;

    /// Resolves the persisted manual-gate directory for a run.
    fn gates_dir(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf>;

    /// Returns whether a run has an active manual-gate decision file.
    ///
    /// Storage and filesystem read errors are treated as no pending decision.
    fn has_pending_gate_decision(&self, home_dir: &Path, run_id: &str) -> bool;

    /// Resolves an exact validated manual-gate decision path.
    fn gate_decision_path(&self, home_dir: &Path, run_id: &str, gate_id: &str) -> Result<PathBuf>;

    /// Loads the raw bytes of the active manual-gate decision, if present.
    ///
    /// The kernel owns JSON decoding and identity/decision validation. The
    /// store owns locating and safely reading the local record.
    fn load_gate_decision(
        &self,
        home_dir: &Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<Option<Vec<u8>>>;

    /// Loads raw immutable history records for one exact manual-gate identity.
    ///
    /// The kernel owns decoding, identity validation, and deterministic latest
    /// record selection. The store owns the durable layout and safe file reads;
    /// no durable path is exposed to plugin invocations.
    fn load_gate_decision_history(
        &self,
        home_dir: &Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<Vec<Vec<u8>>>;

    /// Consumes the active manual-gate decision durably.
    ///
    /// Implementations must make deletion safe against path replacement and
    /// preserve the directory-sync ordering required for crash recovery.
    fn consume_gate_decision(&self, home_dir: &Path, run_id: &str, gate_id: &str) -> Result<()>;

    /// Persists an active manual-gate decision and its immutable history record.
    ///
    /// The kernel validates the record before calling this port. The storage
    /// implementation owns directory creation, serialization, and durable I/O.
    fn store_gate_decision(&self, home_dir: &Path, record: &GateDecisionRecord) -> Result<()>;

    /// Loads the raw bytes of a run's provenance record, if present.
    fn read_run_provenance(&self, home_dir: &Path, run_id: &str) -> Result<Option<Vec<u8>>>;

    /// Atomically replaces a run's provenance record with the supplied bytes.
    fn write_run_provenance(&self, home_dir: &Path, run_id: &str, payload: &[u8]) -> Result<()>;

    /// Lists run roots for filesystem-backed stores. Non-local stores may return an empty list.
    fn list_run_roots(&self, home_dir: &Path) -> Result<Vec<PathBuf>>;

    /// Lists only the selected physical root tree, including its root run.
    /// Filesystem stores override this to traverse that tree alone, so damage
    /// in an unrelated tree cannot prevent selected-tree cancellation/recovery.
    /// The default filters paths before any per-run state or lineage is read.
    fn list_run_tree_roots(&self, home_dir: &Path, root_run_id: &str) -> Result<Vec<PathBuf>> {
        let root = self.resolve_run_root(home_dir, root_run_id)?;
        Ok(self
            .list_run_roots(home_dir)?
            .into_iter()
            .filter(|path| path.starts_with(&root))
            .collect())
    }

    /// Extracts a run id from a persisted run-root directory name.
    fn run_id_from_run_root(&self, run_root: &Path) -> Option<String>;

    /// Rebases persisted invocation paths against the live local layout.
    fn rebase_persisted_run_record(
        &self,
        record: &mut PersistedRunRecord,
        home_dir: &Path,
        repo_root: &Path,
    ) -> Result<()>;

    /// Publishes the run locator after a local run root is durable.
    ///
    /// Non-filesystem stores may leave this as a no-op because their run index
    /// is implicit. The local implementation persists the existing locator
    /// bytes without changing the run-birth ordering.
    fn publish_run_locator(&self, _home_dir: &Path, _record: &PersistedRunRecord) -> Result<()> {
        Ok(())
    }

    /// Removes stale private invocation staging owned by exited processes.
    fn cleanup_stale_staging(&self, _home_dir: &Path) -> Result<()> {
        Ok(())
    }

    /// Creates private storage for one adapter invocation.
    ///
    /// Implementations must provide isolation or return an error. The kernel
    /// never falls back to exposing durable run paths to an adapter.
    fn create_invocation_staging(
        &self,
        home_dir: &Path,
        request: &ExecutionRequest,
    ) -> Result<Box<dyn InvocationStagingPort>>;
}

/// Observer hook fired after durable run-store writes complete.
pub trait RunWriteObserver: Send + Sync {
    /// Reacts to a successful run-store write for the provided run id.
    fn on_run_write(&self, run_id: &RunId);
}

/// Port used to coordinate exclusive mutation of persisted runs.
pub trait LeasePort: Send + Sync {
    /// Returns the stable runtime owner identity associated with this process.
    fn owner_identity(&self) -> RuntimeOwnerIdentity;
    /// Acquires a lease for the given root run tree.
    fn acquire_root_run_tree_lease(&self, root_run_id: &str) -> Result<LeaseHandle>;
    /// Refreshes a previously acquired lease heartbeat and returns the updated record.
    fn refresh_root_run_tree_lease(&self, handle: &LeaseHandle) -> Result<RunTreeLeaseRecord>;
    /// Loads the live lease record for one root run tree when present.
    fn inspect_root_run_tree_lease(&self, root_run_id: &str) -> Result<Option<RunTreeLeaseRecord>>;
    /// Breaks a persisted lease explicitly and returns the removed record when one existed.
    fn break_root_run_tree_lease(
        &self,
        root_run_id: &str,
        options: LeaseBreakOptions,
    ) -> Result<Option<RunTreeLeaseRecord>>;
    /// Releases a previously acquired lease.
    fn release_root_run_tree_lease(&self, handle: &LeaseHandle) -> Result<()>;
}

/// Port used to persist and inspect durable run-tree control requests.
pub trait ControlRequestPort: Send + Sync {
    /// Writes or returns the durable cancellation request for one root run tree.
    /// An existing request is returned unchanged, never replaced by a new id.
    fn request_cancel(&self, root_run_id: &str) -> Result<ControlRequestRecord>;
    /// Loads the durable cancellation request for one root run tree when present.
    fn inspect_cancel_request(&self, root_run_id: &str) -> Result<Option<ControlRequestRecord>>;
    /// Clears any durable cancellation request for one root run tree (no-op when absent).
    /// The caller must own the root mutation lease, making it the sole remover;
    /// concurrent request writers may only create an absent marker or return it.
    fn clear_cancel_request(&self, root_run_id: &str) -> Result<()>;
}

/// Port used by the kernel to obtain wall-clock time in Unix milliseconds.
pub trait ClockPort: Send + Sync {
    /// Returns the current Unix timestamp in milliseconds.
    fn now_unix_ms(&self) -> u64;
}

/// Port used by the kernel to generate stable runtime identifiers.
pub trait IdGeneratorPort: Send + Sync {
    /// Generates a new run id.
    fn generate_run_id(&self) -> String;
    /// Generates a new step-local invoke id.
    fn generate_step_id(&self) -> String;
    /// Generates a new artifact id when one is needed.
    fn generate_artifact_id(&self) -> String;
}

/// Port used by the kernel to publish runtime events.
pub trait EventPort: Send + Sync {
    /// Publishes one runtime event.
    fn publish(&self, event: &RuntimeEvent) -> Result<()>;
}

/// Observer hook fired after runtime events are durably published.
pub trait EventObserver: Send + Sync {
    /// Reacts to one published runtime event.
    fn on_event(&self, event: &RuntimeEvent);
}

/// Telemetry event payload emitted by the runtime kernel.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum TelemetryEvent {
    /// One run session started.
    RunSessionStarted {
        run_id: String,
        workflow_id: String,
        workflow_version: String,
        lineage_mode: String,
        resume_count: u32,
    },
    /// One run session finished.
    RunSessionFinished {
        run_id: String,
        terminal_status: String,
    },
    /// Preflight started for one run.
    PreflightStarted { run_id: String },
    /// Preflight finished for one run.
    PreflightFinished { run_id: String, status: String },
    /// One step attempt started.
    StepStarted {
        run_id: String,
        step_id: String,
        execution: u32,
        descriptor: StepExecutionDescriptor,
        adapter_kind: String,
        action: String,
    },
    /// One step attempt finished.
    StepFinished {
        run_id: String,
        step_id: String,
        execution: u32,
        status: String,
        duration_ms: u64,
        descriptor: StepExecutionDescriptor,
        adapter_kind: String,
        action: String,
        provider: Option<String>,
        request_model: Option<String>,
        response_model: Option<String>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        error_type: Option<String>,
    },
    /// Manual gate wait started for one step.
    GateWaitStarted {
        run_id: String,
        step_id: String,
        execution: u32,
    },
    /// Manual gate wait finished for one step.
    GateWaitFinished {
        run_id: String,
        step_id: String,
        execution: u32,
        decision: String,
    },
}

/// Port used by the kernel to publish non-authoritative telemetry signals.
pub trait TelemetryPort: Send + Sync {
    /// Records one telemetry event.
    fn record(&self, event: &TelemetryEvent);
}

/// No-op telemetry sink used when telemetry export is disabled.
pub struct NoopTelemetry;

impl TelemetryPort for NoopTelemetry {
    fn record(&self, _event: &TelemetryEvent) {}
}

/// Port used by the kernel to validate policy decisions before mutating runtime state.
pub trait PolicyPort: Send + Sync {
    /// Validates that a workflow run may be started for the given repository and workflow ref.
    fn validate_run_start(&self, repo_root: &Path, workflow_ref: &str) -> Result<PolicyDecision>;
    /// Validates that a specific step execution may proceed.
    fn validate_step_execution(
        &self,
        run_id: &str,
        step_id: &str,
        descriptor: &KernelValue,
    ) -> Result<PolicyDecision>;
}

/// Port used by the kernel to resolve named secrets at execution time.
pub trait SecretProviderPort: Send + Sync {
    /// Returns the resolved secret value when the named secret exists.
    fn get_secret(&self, name: &str) -> Result<Option<String>>;
    /// Returns whether a named secret is available without materializing its value.
    fn has_secret(&self, name: &str) -> Result<bool>;
}

/// Port used by trust enforcement to load and manage trusted public-key records.
///
/// The kernel owns trust schemas and signature verification. Implementations own
/// the storage medium, path layout, and mutation durability of trust roots.
pub trait TrustStoragePort: Send + Sync {
    /// Imports one validated public trust-root record from an external file.
    fn import_trust_root(&self, pubkey_path: &Path) -> Result<ImportedTrustRoot>;
    /// Lists imported trust roots in deterministic order.
    fn list_trust_roots(&self) -> Result<Vec<ImportedTrustRoot>>;
    /// Loads one imported trust root for operator inspection.
    fn inspect_trust_root(&self, key_id: &str) -> Result<TrustRootRecord>;
    /// Removes one imported trust root.
    fn remove_trust_root(&self, key_id: &str) -> Result<ImportedTrustRoot>;
    /// Loads a trust root for verification, returning `None` when it is absent.
    fn load_trust_root(&self, key_id: &str) -> Result<Option<TrustRootRecord>>;
}
