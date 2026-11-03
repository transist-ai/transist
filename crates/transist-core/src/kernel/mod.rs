// SPDX-License-Identifier: MPL-2.0
//! Runtime kernel entry points and focused implementation modules.
//!
//! The kernel owns behavior-preserving orchestration for persisted runs, step
//! progression, manual gates, replay/resume preparation, and preflight checks.

mod asset_locks;
mod cleanup;
mod document_reviews;
mod documents;
mod failure_mapping;
mod gates;
mod inputs;
mod preflight;
mod provenance;
mod replay;
mod run_execution;
mod step_execution;
mod subflows;
mod test_mode;

use crate::app::run_persistence::{
    load_and_rebase_run_record, load_run_record, persist_run_record, persist_run_snapshot,
    persist_step_execution, persist_step_state,
};
use crate::dispatch::{ExecutionRequestContext, make_execution_request_with_capabilities};
use crate::domain::taxonomy::RUN_ID_NESTING_SEPARATOR;
use crate::execution_graph::{ExecutionGraph, build_execution_graph, outgoing_edges};
use crate::gate_state::{GateState, evaluate_loop_state};
use crate::infra::redaction::Redactor;
use crate::ports::{
    ArtifactPort, ArtifactWriteRequest, ClockPort, ControlRequestPort, ControlRequestRecord,
    EventPort, ExecutionCancelRequest, ExecutionCancelResult, ExecutionInvocation, ExecutionPort,
    HomeSettingsPort, IdGeneratorPort, LeaseHandle, LeasePort, PluginMetadataPort,
    PluginVerificationPort, PolicyPort, PreflightInvocation, PreflightPort, PromptFilePort,
    RunBirthArtifacts, RunStorePort, SecretProviderPort, TelemetryEvent, TelemetryPort,
    TrustDecisionPort,
};
use crate::recovery::recover_snapshot;
use crate::scheduler::runnable_frontier;
use crate::types::{
    CleanupPhaseState, KernelOperationOutcome, KernelRunRecoveryState, KernelRunSnapshot,
    KernelRunStatus, KernelRunSummary, KernelSkipReason, KernelStepState, KernelStepStatus,
    KernelStepSummary, LeaseReleaseWarning, PersistedCleanupRecord, PersistedPreflightRecord,
    PersistedRunRecord, PreflightCheck, PreflightReport, PreflightStatus, RecipeSnapshotRef,
    RecipeSyncResult, RunLineage, RunLineageMode, StepExecutionRecord,
};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use transist_protocol::{
    CanonicalErrorCode, CleanupTrigger, ExecutionRequest, ExecutionResult, FileRef, KernelValue,
    ProtocolError, Recipe, RunInputsRecord, RuntimeEvent, StepExecutionDescriptor,
    TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
};

pub(crate) use self::failure_mapping::{
    execution_failed_result, failed_step_result, filesystem_policy_denied_result,
    policy_denied_result, policy_denied_result_with_code, secret_resolution_failed_result,
};
pub(crate) use self::gates::{
    consume_gate_decision_record, load_gate_decision_record,
    load_latest_gate_decision_history_record, validate_recipe_gate_ids, validated_gate_id_for_step,
};

/// Stable reason code for managed secret-ref policy denials.
///
/// The string value is frozen and must match `MANAGED_SECRET_REF_DENIED`
/// in `transist::runtime_auth_context`.  It is replicated here so that
/// `transist-core` can detect managed denials without depending on the
/// top-level crate's internals.
pub(crate) const MANAGED_SECRET_DENIAL_CODE: &str = "MANAGED_SECRET_REF_DENIED";

/// Return type for [`RuntimeKernelInner::resolve_secret_refs`].
///
/// Distinguishes managed auth-context policy denials (where a `policy.denied`
/// event should be emitted and the denied secret name is available) from all
/// other secret-resolution failures.
pub(crate) enum SecretResolveError {
    /// A managed auth-context policy denied access to this secret ref.
    ManagedPolicyDenied {
        /// The name of the secret that was denied.
        secret_name: String,
        /// The underlying protocol error to propagate as the step result.
        error: ProtocolError,
    },
    /// Any other resolution failure (secret not found, provider error, etc.).
    Other(ProtocolError),
}
pub(crate) use self::asset_locks::{
    build_asset_locks, collect_asset_lock_checks, verify_asset_locks,
};
pub use self::inputs::{
    BoundWorkflow, PartialChildInputs, bind_and_materialize_recipe, bind_deferred_subflow_inputs,
    bind_deferred_subflow_inputs_in_scope, persisted_capabilities_for_step,
};
pub(crate) use self::inputs::{
    persisted_workspace_or_legacy, populate_recipe_capabilities, recipe_network_origins,
    resolve_recipe_authority_fields, resolve_recipe_filesystem_policies,
    resolve_recipe_network_policies, resolve_recipe_workspace, runtime_blob_ref_summary,
    serialize_recipe_snapshot, serialize_run_inputs_record, sha256_hex, sorted_json_object_keys,
    validate_persisted_authority,
};
pub(crate) use self::provenance::{
    finalize_run_provenance_record, record_step_dispatch_denied_provenance,
    serialize_birth_provenance_record,
};
pub(crate) use self::replay::{reset_step_for_rerun, run_summary_from_snapshot};
pub(crate) use self::run_execution::normalize_result_file_refs;
pub(crate) use self::run_execution::{
    MutationLeaseGuard, apply_terminal_state_from_record, record_is_completed,
    validate_record_identity,
};
pub use self::subflows::subflow_provided_inputs;

/// Builds the policy descriptor for a materialized invoke request.
pub fn build_step_policy_descriptor(
    workflow_ref: &str,
    request: &ExecutionRequest,
    secret_refs: &[String],
) -> Result<KernelValue> {
    let plugin_id = request
        .metadata
        .get("plugin_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!("step policy descriptor is missing string metadata.plugin_id")
        })?
        .to_string();
    let plugin_version = request
        .metadata
        .get("plugin_version")
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                anyhow::anyhow!(
                    "step policy descriptor metadata.plugin_version must be a string when present"
                )
            })
        })
        .transpose()?;
    let provider_kind = request
        .metadata
        .get("provider_kind")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse().ok())
        .unwrap_or(transist_protocol::ProviderKind::External);
    let requested_capabilities = serde_json::to_value(&request.requested_capabilities)
        .context("serialize step policy descriptor requested capabilities")?;
    let network_policy = request.policy.get("network").cloned().ok_or_else(|| {
        anyhow::anyhow!("step policy descriptor is missing request policy.network")
    })?;
    Ok(KernelValue::Object(BTreeMap::from([
        (
            "workflow_ref".to_string(),
            KernelValue::String(workflow_ref.to_string()),
        ),
        (
            "adapter_kind".to_string(),
            KernelValue::String(request.adapter_kind.to_string()),
        ),
        ("plugin_id".to_string(), KernelValue::String(plugin_id)),
        (
            "plugin_version".to_string(),
            match plugin_version {
                Some(version) => KernelValue::String(version),
                None => KernelValue::Null,
            },
        ),
        (
            "action".to_string(),
            KernelValue::String(request.action.clone()),
        ),
        (
            "provider_kind".to_string(),
            KernelValue::String(provider_kind.as_str().to_string()),
        ),
        (
            "secret_refs".to_string(),
            KernelValue::Array(
                secret_refs
                    .iter()
                    .cloned()
                    .map(KernelValue::String)
                    .collect(),
            ),
        ),
        (
            "requested_capabilities".to_string(),
            KernelValue::from_json(requested_capabilities),
        ),
        (
            "network_policy".to_string(),
            KernelValue::from_json(network_policy),
        ),
    ])))
}

/// Builds the policy checkpoint descriptor for a manual-gate request or
/// decision from the pinned recipe and a persisted (never regenerated)
/// capability subset.
pub fn build_gate_policy_descriptor(
    recipe: &Recipe,
    capabilities: &[transist_protocol::CapabilityRequest],
) -> Result<KernelValue> {
    build_gate_policy_descriptor_for_workflow(
        &format!("{}@{}", recipe.workflow.id, recipe.workflow.version),
        capabilities,
    )
}

/// Builds a manual-gate policy descriptor in an already resolved workflow
/// policy scope.
pub fn build_gate_policy_descriptor_for_workflow(
    workflow_ref: &str,
    capabilities: &[transist_protocol::CapabilityRequest],
) -> Result<KernelValue> {
    let requested_capabilities = serde_json::to_value(capabilities)
        .context("serialize gate policy descriptor requested capabilities")?;
    Ok(KernelValue::Object(BTreeMap::from([
        (
            "workflow_ref".to_string(),
            KernelValue::String(workflow_ref.to_string()),
        ),
        (
            "requested_capabilities".to_string(),
            KernelValue::from_json(requested_capabilities),
        ),
    ])))
}

/// Shared runtime services composed around the kernel.
#[derive(Clone)]
pub struct RuntimeServices {
    /// Port used to execute and cancel step invocations.
    pub execution: Arc<dyn ExecutionPort>,
    /// Port used to persist and load artifacts.
    pub artifacts: Arc<dyn ArtifactPort>,
    /// Port used to persist run records and step state.
    pub run_store: Arc<dyn RunStorePort>,
    /// Port used to coordinate exclusive run mutation.
    pub lease: Arc<dyn LeasePort>,
    /// Port used to persist and inspect durable control requests.
    pub control: Arc<dyn ControlRequestPort>,
    /// Port used to read wall-clock time.
    pub clock: Arc<dyn ClockPort>,
    /// Port used to generate runtime ids.
    pub ids: Arc<dyn IdGeneratorPort>,
    /// Port used to publish runtime events.
    pub events: Arc<dyn EventPort>,
    /// Port used to evaluate policy decisions.
    pub policy: Arc<dyn PolicyPort>,
    /// Port used to run preflight probes.
    pub preflight: Arc<dyn PreflightPort>,
    /// Port used to resolve secrets at execution time.
    pub secrets: Arc<dyn SecretProviderPort>,
    /// Port used to verify installed plugin bytes against persisted asset locks.
    pub plugin_verifier: Arc<dyn PluginVerificationPort>,
    /// Port used to resolve installed plugin action metadata.
    pub plugin_metadata: Arc<dyn PluginMetadataPort>,
    /// Port used to resolve and read prompt files.
    pub prompt_files: Arc<dyn PromptFilePort>,
    /// Port used to load effective-home filesystem and network policy settings.
    pub home_settings: Arc<dyn HomeSettingsPort>,
    /// Port used to publish non-authoritative telemetry signals.
    pub telemetry: Arc<dyn TelemetryPort>,
}

/// Explicit set of service implementations required to construct [`RuntimeServices`].
pub struct RuntimeServiceParts {
    /// Port used to execute and cancel step invocations.
    pub execution: Arc<dyn ExecutionPort>,
    /// Port used to persist and load artifacts.
    pub artifacts: Arc<dyn ArtifactPort>,
    /// Port used to persist run records and step state.
    pub run_store: Arc<dyn RunStorePort>,
    /// Port used to coordinate exclusive run mutation.
    pub lease: Arc<dyn LeasePort>,
    /// Port used to persist and inspect durable control requests.
    pub control: Arc<dyn ControlRequestPort>,
    /// Port used to read wall-clock time.
    pub clock: Arc<dyn ClockPort>,
    /// Port used to generate runtime ids.
    pub ids: Arc<dyn IdGeneratorPort>,
    /// Port used to publish runtime events.
    pub events: Arc<dyn EventPort>,
    /// Port used to evaluate policy decisions.
    pub policy: Arc<dyn PolicyPort>,
    /// Port used to run preflight probes.
    pub preflight: Arc<dyn PreflightPort>,
    /// Port used to resolve secrets at execution time.
    pub secrets: Arc<dyn SecretProviderPort>,
    /// Port used to verify installed plugin bytes against persisted asset locks.
    pub plugin_verifier: Arc<dyn PluginVerificationPort>,
    /// Port used to resolve installed plugin action metadata.
    pub plugin_metadata: Arc<dyn PluginMetadataPort>,
    /// Port used to resolve and read prompt files.
    pub prompt_files: Arc<dyn PromptFilePort>,
    /// Port used to load effective-home filesystem and network policy settings.
    pub home_settings: Arc<dyn HomeSettingsPort>,
    /// Port used to publish non-authoritative telemetry signals.
    pub telemetry: Arc<dyn TelemetryPort>,
}

impl RuntimeServices {
    /// Creates a runtime service set from explicit service implementations.
    pub fn new(parts: RuntimeServiceParts) -> Self {
        Self {
            execution: parts.execution,
            artifacts: parts.artifacts,
            run_store: parts.run_store,
            lease: parts.lease,
            control: parts.control,
            clock: parts.clock,
            ids: parts.ids,
            events: parts.events,
            policy: parts.policy,
            preflight: parts.preflight,
            secrets: parts.secrets,
            plugin_verifier: parts.plugin_verifier,
            plugin_metadata: parts.plugin_metadata,
            prompt_files: parts.prompt_files,
            home_settings: parts.home_settings,
            telemetry: parts.telemetry,
        }
    }
}

/// Behavior-preserving runtime kernel that owns run execution orchestration.
pub struct RuntimeKernel {
    services: RuntimeServices,
    posture: crate::ports::RuntimePosture,
}

#[derive(Clone, Copy)]
pub(crate) struct RuntimePaths<'a> {
    pub(crate) repo_root: &'a Path,
    pub(crate) home_dir: &'a Path,
}

pub(crate) struct ExecuteLoadedRunOptions {
    pub(crate) run_started_payload: Option<KernelValue>,
    pub(crate) apply_recovery: bool,
    pub(crate) preflight_override: Option<PersistedPreflightRecord>,
}

/// Lease key shared by run birth and plugin removal. It prevents a plugin from
/// disappearing after its protected-run scan but before a root or child birth
/// has persisted its asset locks.
pub const PLUGIN_LIFECYCLE_LEASE_ID: &str = "plugin-lifecycle-reservation";

#[derive(Clone)]
pub(crate) struct SubflowExecutionContext<'a> {
    pub(crate) paths: RuntimePaths<'a>,
    pub(crate) run_id: &'a str,
    pub(crate) step: &'a transist_protocol::RecipeStep,
    pub(crate) execution: u32,
    pub(crate) rerun_completed_run: bool,
    pub(crate) previous_child_run_id: Option<String>,
    pub(crate) recipe: &'a Recipe,
    pub(crate) workflow_inputs: &'a BTreeMap<String, KernelValue>,
    pub(crate) workflow_vars: &'a BTreeMap<String, KernelValue>,
    pub(crate) snapshot: &'a KernelRunSnapshot,
    pub(crate) replay_source_run_id: Option<&'a str>,
}

impl RuntimeKernel {
    /// Resolves the exact workflow policy scope for a persisted run.
    ///
    /// Child runs inherit the lineage root workflow's grants. Missing or
    /// malformed root authority fails closed.
    pub fn policy_workflow_ref(&self, record: &PersistedRunRecord) -> Result<String> {
        let requested_run_id = record.run_id.clone();
        let mut current = record.clone();
        let mut visited = BTreeSet::new();
        let mut expected_root_run_id = None;

        loop {
            if !visited.insert(current.run_id.clone()) {
                bail!(
                    "run '{}' has a cycle in its persisted policy lineage at run '{}'",
                    requested_run_id,
                    current.run_id
                );
            }

            let Some(lineage) = current.lineage.as_ref() else {
                if expected_root_run_id
                    .as_deref()
                    .is_some_and(|root_run_id| root_run_id != current.run_id)
                {
                    bail!(
                        "run '{}' has malformed policy lineage: run '{}' has no lineage metadata",
                        requested_run_id,
                        current.run_id
                    );
                }
                return Ok(format!(
                    "{}@{}",
                    current.workflow_id, current.workflow_version
                ));
            };

            let lineage_root_run_id = lineage.root_run_id.trim();
            if lineage_root_run_id.is_empty() {
                bail!(
                    "run '{}' has malformed policy lineage: run '{}' has an empty root run id",
                    requested_run_id,
                    current.run_id
                );
            }
            if let Some(expected) = expected_root_run_id.as_deref() {
                if lineage_root_run_id != expected {
                    bail!(
                        "run '{}' has inconsistent policy lineage: run '{}' names root '{}' instead of '{}'",
                        requested_run_id,
                        current.run_id,
                        lineage_root_run_id,
                        expected
                    );
                }
            } else {
                expected_root_run_id = Some(lineage_root_run_id.to_string());
            }

            let parent_run_id = lineage
                .parent_run_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let parent_step_id = lineage
                .parent_step_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            match (parent_run_id, parent_step_id) {
                (None, None) => {
                    if lineage_root_run_id != current.run_id {
                        bail!(
                            "run '{}' has malformed policy lineage: run '{}' claims root '{}' but has no parent",
                            requested_run_id,
                            current.run_id,
                            lineage_root_run_id
                        );
                    }
                    return Ok(format!(
                        "{}@{}",
                        current.workflow_id, current.workflow_version
                    ));
                }
                (Some(parent_run_id), Some(parent_step_id)) => {
                    if visited.contains(parent_run_id) {
                        bail!(
                            "run '{}' has a cycle in its persisted policy lineage at run '{}'",
                            requested_run_id,
                            parent_run_id
                        );
                    }
                    let parent = load_run_record(
                        self.services.run_store.as_ref(),
                        parent_run_id,
                    )?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "parent run '{}' required for policy evaluation of child run '{}' is missing",
                            parent_run_id,
                            current.run_id
                        )
                    })?;
                    self.validate_policy_parent_binding(
                        &parent,
                        parent_step_id,
                        &current,
                        &requested_run_id,
                    )?;
                    current = parent;
                }
                _ => {
                    bail!(
                        "run '{}' has malformed policy lineage: run '{}' must name both parent_run_id and parent_step_id",
                        requested_run_id,
                        current.run_id
                    );
                }
            }
        }
    }

    fn validate_policy_parent_binding(
        &self,
        parent: &PersistedRunRecord,
        parent_step_id: &str,
        child: &PersistedRunRecord,
        requested_run_id: &str,
    ) -> Result<()> {
        let parent_recipe = self.load_persisted_recipe(&parent.run_id, parent)?;
        let parent_step = parent_recipe
            .steps
            .iter()
            .find(|step| step.id == parent_step_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "run '{}' has malformed policy lineage: parent run '{}' has no step '{}' for child run '{}'",
                    requested_run_id,
                    parent.run_id,
                    parent_step_id,
                    child.run_id
                )
            })?;
        let StepExecutionDescriptor::Subflow {
            recipe: child_recipe,
        } = &parent_step.execution
        else {
            bail!(
                "run '{}' has malformed policy lineage: parent step '{}.{}' is not a subflow",
                requested_run_id,
                parent.run_id,
                parent_step_id
            );
        };
        if child_recipe.workflow.id != child.workflow_id
            || child_recipe.workflow.version != child.workflow_version
        {
            bail!(
                "run '{}' has malformed policy lineage: parent step '{}.{}' references workflow '{}@{}', not child workflow '{}@{}'",
                requested_run_id,
                parent.run_id,
                parent_step_id,
                child_recipe.workflow.id,
                child_recipe.workflow.version,
                child.workflow_id,
                child.workflow_version
            );
        }

        let parent_step_state = parent.steps.get(parent_step_id).ok_or_else(|| {
            anyhow::anyhow!(
                "run '{}' has malformed policy lineage: parent run '{}' has no persisted state for subflow step '{}'",
                requested_run_id,
                parent.run_id,
                parent_step_id
            )
        })?;
        let persisted_child_run_id = parent_step_state
            .last_result
            .as_ref()
            .and_then(|result| result.output.get("child_run_id"));
        if let Some(persisted_child_run_id) = persisted_child_run_id {
            let persisted_child_run_id = persisted_child_run_id
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "run '{}' has malformed policy lineage: parent step '{}.{}' persisted an invalid child run id",
                        requested_run_id,
                        parent.run_id,
                        parent_step_id
                    )
                })?;
            if persisted_child_run_id != child.run_id {
                bail!(
                    "run '{}' has malformed policy lineage: parent step '{}.{}' binds child run '{}', not '{}'",
                    requested_run_id,
                    parent.run_id,
                    parent_step_id,
                    persisted_child_run_id,
                    child.run_id
                );
            }
            return Ok(());
        }

        let base_child_run_id = format!(
            "{}{}{}",
            parent.run_id, RUN_ID_NESTING_SEPARATOR, parent_step_id
        );
        let fresh_child_run_id = format!(
            "{}{}r{:03}",
            base_child_run_id, RUN_ID_NESTING_SEPARATOR, parent_step_state.executions
        );
        if child.run_id != base_child_run_id && child.run_id != fresh_child_run_id {
            bail!(
                "run '{}' has malformed policy lineage: parent step '{}.{}' does not identify child run '{}'",
                requested_run_id,
                parent.run_id,
                parent_step_id,
                child.run_id
            );
        }
        Ok(())
    }

    fn verify_asset_locks(
        &self,
        record: &PersistedRunRecord,
        recipe: &Recipe,
        home_dir: &Path,
    ) -> Result<()> {
        verify_asset_locks(
            record,
            recipe,
            home_dir,
            self.services.plugin_verifier.as_ref(),
            self.services.prompt_files.as_ref(),
        )
    }

    fn collect_asset_lock_checks(
        &self,
        record: &PersistedRunRecord,
        recipe: &Recipe,
        home_dir: &Path,
    ) -> Result<Vec<asset_locks::AssetLockCheckOutcome>> {
        collect_asset_lock_checks(
            record,
            recipe,
            home_dir,
            self.services.plugin_verifier.as_ref(),
            self.services.prompt_files.as_ref(),
        )
    }

    /// Creates a runtime kernel from a composed service set.
    pub fn new(services: RuntimeServices) -> Self {
        Self {
            services,
            posture: crate::ports::RuntimePosture::legacy(),
        }
    }

    /// Creates a runtime kernel with the effective home security posture.
    pub fn new_with_posture(
        services: RuntimeServices,
        posture: crate::ports::RuntimePosture,
    ) -> Self {
        Self { services, posture }
    }

    /// Returns the kernel service set.
    pub fn services(&self) -> &RuntimeServices {
        &self.services
    }

    /// Returns the effective security posture attached to this kernel.
    pub fn posture(&self) -> &crate::ports::RuntimePosture {
        &self.posture
    }
}

pub(crate) fn run_transition_allowed(from: KernelRunStatus, to: KernelRunStatus) -> bool {
    from.can_transition_to(to)
}

pub(crate) fn step_transition_allowed(from: KernelStepStatus, to: KernelStepStatus) -> bool {
    from.can_transition_to(to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{AdapterKind, InvokePaths, InvokeStep, TEP_INVOKE_REQUEST_TYPE_V1};

    fn request(metadata: serde_json::Value, policy: serde_json::Value) -> ExecutionRequest {
        ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke-1".to_string(),
            run_id: "run-1".to_string(),
            workflow_id: "workflow".to_string(),
            step: InvokeStep {
                id: "step".to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "exec".to_string(),
            input: serde_json::json!({}),
            paths: InvokePaths {
                run_root: "/tmp/run".to_string(),
                artifacts_dir: "/tmp/run/artifacts".to_string(),
                evidence_dir: "/tmp/run/evidence".to_string(),
                workspace_dir: "/tmp/workspace".to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: None,
            max_timeout_ms: None,
            policy,
            metadata,
        }
    }

    #[test]
    fn step_policy_descriptor_rejects_missing_required_metadata() {
        let error = build_step_policy_descriptor(
            "workflow@v1",
            &request(
                serde_json::json!({}),
                serde_json::json!({"network": {"mode": "none"}}),
            ),
            &[],
        )
        .expect_err("missing plugin metadata must not become an empty descriptor field");

        assert!(error.to_string().contains("metadata.plugin_id"));
    }

    #[test]
    fn step_policy_descriptor_rejects_missing_network_policy() {
        let error = build_step_policy_descriptor(
            "workflow@v1",
            &request(
                serde_json::json!({"plugin_id": "shell"}),
                serde_json::json!({}),
            ),
            &[],
        )
        .expect_err("missing network policy must not become a null descriptor field");

        assert!(error.to_string().contains("policy.network"));
    }
}
