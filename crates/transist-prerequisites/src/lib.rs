// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Shared prerequisite contracts and static local host-side evaluation.

mod evaluate;
mod model;

pub use evaluate::{
    EvaluationContext, PrerequisiteCheck, PrerequisiteError, PrerequisiteReport, PrerequisiteScope,
    PrerequisiteStatus, blocking_error_message, evaluate_prerequisites,
};
pub use model::{
    BinaryPrerequisite, CommandPrerequisite, EnvPrerequisite, PathKind, PathPrerequisite,
    PrerequisiteSet, VersionCheck, validate_prerequisite_set,
};
