// SPDX-License-Identifier: MPL-2.0
//! Typed runtime taxonomies and shared constants.

use serde::{Deserialize, Serialize};

/// Shared run-id nesting separator used by embedded child runs.
pub const RUN_ID_NESTING_SEPARATOR: &str = "__";

/// Marker key used to store subflow metadata in step `with` bindings.
pub const SUBFLOW_META_KEY: &str = "__transist_subflow";

/// Marker key used to store loop metadata in step `with` bindings.
pub const LOOP_META_KEY: &str = "__transist_loop";

/// Supported control-step primitive kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlStepKind {
    /// A manual decision gate controlled by an operator.
    #[serde(rename = "control.gate.manual")]
    GateManual,
}

impl ControlStepKind {
    /// Returns the stable wire string used in recipe steps.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::GateManual => "control.gate.manual",
        }
    }
}

/// Supported workflow edge kinds for inspection graph views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowEdgeKind {
    /// Default successor edge.
    #[serde(rename = "next")]
    Next,
    /// Guard-true branch edge.
    #[serde(rename = "then")]
    Then,
    /// Guard-false branch edge.
    #[serde(rename = "else")]
    Else,
    /// Loop re-entry edge.
    #[serde(rename = "loop")]
    Loop,
    /// Loop exit edge.
    #[serde(rename = "exit")]
    Exit,
    /// Repeated edge used by retry-like control flow.
    #[serde(rename = "repeat")]
    Repeat,
}

impl WorkflowEdgeKind {
    /// Returns the stable wire string used for edge-kind serialization.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Then => "then",
            Self::Else => "else",
            Self::Loop => "loop",
            Self::Exit => "exit",
            Self::Repeat => "repeat",
        }
    }
}
