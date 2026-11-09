// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Stable protocol and value-contract definitions shared across Transist crates.
//!
//! This crate owns the serialized message families, runtime recipe records, and
//! generic kernel values exchanged between the compiler, runtime, adapters,
//! storage, and interface layers. It may not depend on higher-level crates.
//!
//! The public types and constants exported from this crate are intended to be the
//! stable contract surface. Internal module layout is an implementation detail.

pub mod adapter_kind;
pub mod app;
pub mod capability;
pub mod error;
pub mod explanation;
pub mod filesystem_policy;
pub mod ids;
pub mod network_policy;
pub mod prerequisites;
pub mod provider_kind;
pub mod runtime;
pub mod settings;
pub mod tep;
pub mod value;

pub use adapter_kind::{AdapterKind, UnknownAdapterKind};
pub use capability::{
    CapabilityError, CapabilityName, CapabilityProjection, CapabilityRequest, CapabilitySource,
    CapabilitySourceDependency, canonical_external_policy_path, filesystem_capabilities,
    invoke_capabilities, network_origins,
};
pub use error::{Error, Result};
pub use ids::{IdentifierKind, validate_identifier};
pub use provider_kind::{ProviderKind, UnknownProviderKind};
pub use settings::HomeSettings;

pub use app::{
    APP_CANCEL_REQUEST_TYPE_V1, APP_CANCEL_RESULT_TYPE_V1, APP_HANDSHAKE_REQUEST_TYPE_V1,
    APP_HANDSHAKE_RESPONSE_TYPE_V1, APP_INVOKE_PROGRESS_TYPE_V1, APP_INVOKE_REQUEST_TYPE_V1,
    APP_INVOKE_RESULT_TYPE_V1, APP_PROTOCOL_VERSION_V1, AppCapabilities, AppPluginIdentity,
    CancelRequest as AppCancelRequest, CancelResult as AppCancelResult,
    HandshakeRequest as AppHandshakeRequest, HandshakeResponse as AppHandshakeResponse,
    InvokeProgress as AppInvokeProgress, InvokeRequest as AppInvokeRequest,
    InvokeResult as AppInvokeResult, InvokeStep as AppInvokeStep,
};
pub use prerequisites::{
    BinaryPrerequisite, CommandPrerequisite, EnvPrerequisite, PathKind, PathPrerequisite,
    PrerequisiteSet, VersionCheck,
};
pub use runtime::{
    ACTIVE_INVOCATION_RECORD_SCHEMA_V1, ActiveInvocationRecord, ArtifactEnvelope, CleanupTrigger,
    DEFERRED_BINDINGS_KEY, DOCUMENT_BINDINGS_KEY, DeferredBinding, DocumentAccess, DocumentBinding,
    DocumentBindingLocation, DocumentBlob, DocumentDeclaration, DocumentInitialization,
    DocumentKind, DocumentRevision, DocumentTreeManifest, ExecutionRequest, ExecutionResult,
    FilesystemPolicy, FilesystemPolicySource, GateDecisionRecord, GateReviewRequest,
    NetworkAccessMode, NetworkPolicy, NetworkPolicySource, RUN_LOCATOR_SCHEMA_V1, Recipe,
    RecipeCleanupStep, RecipeGraph, RecipeGraphEdge, RecipeInputSpec, RecipeLoop, RecipeLoopRoute,
    RecipeLoopRouteRole, RecipePromptBinding, RecipePromptSource, RecipeRepository,
    RecipeSkillBundle, RecipeSkillDocument, RecipeSkillRef, RecipeSkillRoot, RecipeSkillsBinding,
    RecipeStep, RunInputsRecord, RunLocator, RuntimeEvent, SUBFLOW_BINDINGS_KEY,
    StepExecutionDescriptor, StepResultEnvelope, UsageEnvelope, WorkflowRef, WorkspaceAccessMode,
    WorkspaceAccessPolicy, WorkspaceBinding, binding_pointer_segment, binding_selectors,
    collect_deferred_bindings, collect_document_bindings, contains_document_binding,
    document_authority_field, for_each_invoke_step, gate_review_documents, split_output_selector,
    split_output_selector_for_ids, step_deferred_bindings, step_document_bindings,
    validate_document_reviews, validate_recipe_loop_routing,
};
pub use tep::{
    AdapterCapabilities, AdapterIdentity, CanonicalErrorCode, EXTERNAL_TRANSFER_SCHEMA_V1,
    ErrorClass, ExternalTransferPolicyDecision, ExternalTransferRecord, FileRef, HandshakeRequest,
    HandshakeResponse, InvokePaths, InvokeRequest, InvokeResult, InvokeStep, InvokeTiming,
    InvokeUsage, ProtocolError, Retryability, TEP_HANDSHAKE_REQUEST_TYPE_V1,
    TEP_HANDSHAKE_RESPONSE_TYPE_V1, TEP_INVOKE_REQUEST_TYPE_V1, TEP_INVOKE_RESULT_TYPE_V1,
    TEP_PROTOCOL_VERSION_V1, TepRequest, TepResult, jsonl_decode_line, jsonl_encode,
    normalize_metrics,
};
pub use value::KernelValue;
