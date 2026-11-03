// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Runtime kernel, ports, and persisted state model for Transist.
//!
//! This crate owns workflow execution orchestration, runtime service ports, run
//! lifecycle state, recovery helpers, and the scheduling primitives used by the
//! local implementation. It may depend on `transist-protocol`, but it must not
//! own CLI, HTTP server, or local-storage-specific behavior.
//!
//! The crate-root ports, types, and kernel entry points are intended to be the
//! stable public surface. Internal module organization remains an implementation
//! detail.

/// Runtime kernel/core crate version.
pub const TRANSIST_CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

mod app;
mod dispatch;
mod domain;
mod execution_graph;
mod expr;
mod failures;
mod filesystem_policy;
mod gate_state;
mod infra;
mod input_binding;
mod invocation_paths;
#[path = "kernel/mod.rs"]
mod kernel;
pub mod ports;
mod preflight_measurement;
mod recovery;
mod scheduler;
mod templating;
mod trust;
mod types;

pub use dispatch::{
    ExecutionRequestContext, PreparedExecution, make_static_execution_request,
    make_static_execution_request_in_scope, resolve_input_placeholders,
};
pub use domain::taxonomy::{ControlStepKind, WorkflowEdgeKind};
pub use execution_graph::{reachable_recipe_step_ids, reachable_recipe_step_ids_in_scope};
pub use failures::{FailureClass, FailureFlags, FailureRecord, StateMutation};
pub use filesystem_policy::{FilesystemDenial, FilesystemOperation};
pub use infra::execution_faults::{
    FAULT_STEP_COMPLETION_EVENT, FAULT_STEP_DISPATCH_INTENT, FAULT_STEP_EXECUTION_RECORD,
    FAULT_STEP_FAILURE_APPEND, FAULT_STEP_RESULT_PROMOTION, FAULT_STEP_START_EVENT,
    FAULT_STEP_STATE_PROJECTION, check_fault, clear, set, set_occurrence,
};
pub use infra::records::schema_versions::{
    LIVE_PROGRESS_RECORD_SCHEMA_V1, RUN_PROVENANCE_RECORD_SCHEMA_V1, RUN_STATE_RECORD_SCHEMA_V1,
};
pub use infra::redaction::Redactor;
pub use input_binding::{
    ChildAuthorityPreparationError, RecipeInputBindingError, RecipeInputBindingErrorKind,
    bind_recipe_inputs,
};
pub use invocation_paths::{runtime_temp_parent, validate_runtime_temp_parent};
pub use kernel::bind_deferred_subflow_inputs;
pub use kernel::{
    BoundWorkflow, RuntimeKernel, RuntimeServiceParts, RuntimeServices,
    bind_and_materialize_recipe, build_gate_policy_descriptor,
    build_gate_policy_descriptor_for_workflow, build_step_policy_descriptor,
    persisted_capabilities_for_step, subflow_provided_inputs,
};
pub use kernel::{PartialChildInputs, bind_deferred_subflow_inputs_in_scope};
pub use ports::{
    ArtifactPort, ClockPort, ControlRequestPort, ControlRequestRecord, ControlRequestType,
    ControlRequesterMetadata, EventPort, ExecutionCancelRequest, ExecutionCancelResult,
    ExecutionInvocation, ExecutionPort, ExternalOwnerMetadata, ExternalTransferEvidence,
    HomeSettingsPort, IdGeneratorPort, LeaseBreakOptions, LeaseConflict, LeaseHandle, LeasePort,
    ManagementMode, NoopTelemetry, PluginActionMetadata, PluginLifecycleReservationCanceled,
    PluginLifecycleReservationTimeout, PluginMetadataPort, PluginSecretInput,
    PluginVerificationPort, PolicyPort, PreflightInvocation, PreflightPort, PromptFilePort,
    PromptFileReference, RemoteSecretPolicy, RunBirthArtifacts, RunStorePort, RunTreeLeaseRecord,
    RuntimeOperatingMode, RuntimeOwnerIdentity, RuntimePosture, SecretProviderPort, TelemetryEvent,
    TelemetryPort, TrustDecisionPort, TrustStoragePort, WorkflowAssetBlob,
};
pub use preflight_measurement::{
    PreflightMeasurement, PreflightMeasurementContext, PreflightMeasurementPhase,
    PreflightMeasurementScope, capture_preflight_measurements,
    current_preflight_measurement_context, measure_preflight_phase, phase_span,
    record_preflight_phase_duration, record_preflight_probe_batch,
    with_preflight_measurement_context, with_preflight_measurement_scope,
};
pub use recovery::recover_snapshot;
pub use templating::dataflow::resolve_action_value;
pub use templating::dataflow::{PartialActionValue, resolve_partial_action_value};
pub use transist_protocol::{
    EXTERNAL_TRANSFER_SCHEMA_V1, ExternalTransferPolicyDecision, ExternalTransferRecord,
    ProviderKind, UnknownProviderKind,
};
pub use trust::{
    ImportedTrustRoot, SIGNATURE_SCHEMA_V1, SignatureEnvelope, SignatureSubject,
    SignatureSubjectKind, SignatureVerification, TRUST_ROOT_SCHEMA_V1, TrustRootRecord,
    parse_signature_envelope, parse_trust_root, signature_sidecar_path, verify_signed_digest_bytes,
};
pub use types::*;
