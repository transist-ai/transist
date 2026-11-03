// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Authored workflow loading, validation, and recipe compilation for Transist.
//!
//! This crate owns authored flow models, validation rules, and behavior-preserving
//! compilation into runtime recipes. It may depend on `transist-protocol` and
//! plugin manifest data, but it must not own runtime execution or interface-layer
//! behavior.
//!
//! The documented crate-root compile and validation entry points form the intended
//! stable surface. Internal helper modules remain implementation details.

mod common;
mod compile;
mod digest;
/// Deterministic, read-only authoring explanations and structural comparisons.
pub mod explanation;
/// Filesystem policy settings loader for the Transist home directory.
pub mod filesystem_settings;
mod flow;
pub mod graph;
mod io;
mod modules;
/// Network policy settings loader for the Transist home directory.
pub mod network_settings;
mod registry;
pub mod settings;
pub mod templating;
mod uninstall;
mod validate;

pub use compile::{
    compile_flow, compile_flow_in_home, compile_workflow_snapshot,
    compile_workflow_snapshot_in_home,
};
pub use digest::{
    WorkflowDefinitionAsset, WorkflowDefinitionSnapshot, capture_workflow_definition,
    capture_workflow_definition_with_signature, workflow_definition_sha256,
};
pub use flow::{
    FLOW_SCHEMA_V1, Flow, FlowArtifactScope, FlowArtifactSpec, FlowCleanupStep, FlowCleanupWhen,
    FlowIfBlock, FlowInputSpec, FlowLoopBlock, FlowProfile, FlowRef, FlowStep, FlowSubflowCall,
    RECIPE_SCHEMA_V1, TEST_WORKFLOW_VERSION, is_reserved_test_workflow_version, is_test_workflow,
};
pub use io::{load_flow, load_flow_unvalidated, write_recipe, write_recipe_in_home};
pub use modules::MODULE_SCHEMA_V1;
pub use registry::{InstalledWorkflow, list_recipes_in_home};
pub use uninstall::remove_recipe_in_home;
pub use validate::{
    validate_flow, validate_flow_in_home, validate_workflow_snapshot,
    validate_workflow_snapshot_in_home,
};
