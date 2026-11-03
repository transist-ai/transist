// SPDX-License-Identifier: MPL-2.0
#![deny(missing_docs)]
//! Read-only inspection models for persisted workflows and runs.
//!
//! This module assembles query DTOs from the local storage layer so callers can drive
//! UIs, CLIs, or APIs without depending on the raw on-disk Transist runtime layout.

mod documents;
mod gates;
pub use documents::{InspectionDocument, InspectionDocumentLinks, InspectionDocumentMember};
mod local_query;
mod models;
mod pagination;
mod port;
mod redaction;
mod runs;
mod secure_file;
/// Serializable local inspection snapshot builder and offline export sinks.
pub mod snapshot;
mod steps;
mod workflows;

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use transist_compiler::{Flow, FlowStep, validate_flow_in_home};
use transist_core::{KernelRunStatus, KernelStepState, KernelStepStatus, PersistedRunRecord};
use transist_protocol::{
    ExecutionRequest, ExecutionResult, GateDecisionRecord, KernelValue, Recipe, RecipeStep,
};
pub(crate) use transist_storage_local::PersistedRunSummaryRecord;
use transist_storage_local::{IndexedBlob, LocalInspectionStore, LocalRunSummaryStore};

use self::gates::build_gates;
#[allow(unused_imports)]
pub use self::local_query::LocalInspectionQuery;
#[allow(unused_imports)]
pub use self::models::*;
use self::pagination::normalize_limit;
#[allow(unused_imports)]
pub use self::pagination::workflow_ref_parts;
#[allow(unused_imports)]
pub use self::port::InspectionQueryPort;
#[allow(unused_imports)]
pub use self::redaction::*;
use self::runs::{
    build_run_summary, count_step_statuses, is_terminal_run_status,
    list_contained_runs_from_summaries, matches_run_filters, projected_run_summaries,
    resolve_actionable_run_id, terminal_outcome,
};
use self::steps::build_steps;
use self::workflows::{
    compiled_recipe_path, discover_workflows, read_workflow_content,
    workflow_detail_from_discovery, workflow_summary_from_discovery,
};
pub use transist_core::{FailureClass, FailureFlags, FailureRecord, StateMutation};
