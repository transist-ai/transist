// SPDX-License-Identifier: MPL-2.0
use crate::types::{KernelRunSnapshot, KernelStepState};
use std::collections::BTreeMap;
use transist_protocol::KernelValue;

pub(crate) type Value = serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Expression {
    Source(String),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EvaluationContext<'a> {
    pub(crate) dependency_state: Option<&'a KernelStepState>,
    pub(crate) snapshot: Option<&'a KernelRunSnapshot>,
    pub(crate) workflow_inputs: Option<&'a BTreeMap<String, KernelValue>>,
    pub(crate) workflow_vars: Option<&'a BTreeMap<String, KernelValue>>,
    pub(crate) deferred_inputs: Option<&'a std::collections::BTreeSet<String>>,
    /// Marked loop guards reject unresolved references and incomplete operands.
    pub(crate) strict_operands: bool,
}

impl<'a> EvaluationContext<'a> {
    pub(crate) fn new(
        dependency_state: Option<&'a KernelStepState>,
        snapshot: Option<&'a KernelRunSnapshot>,
        workflow_inputs: Option<&'a BTreeMap<String, KernelValue>>,
        workflow_vars: Option<&'a BTreeMap<String, KernelValue>>,
    ) -> Self {
        Self {
            dependency_state,
            snapshot,
            workflow_inputs,
            workflow_vars,
            deferred_inputs: None,
            strict_operands: false,
        }
    }
}
