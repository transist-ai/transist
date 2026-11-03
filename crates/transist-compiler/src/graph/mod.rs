// SPDX-License-Identifier: MPL-2.0
//! Workflow graph derivation helpers used by inspection and compiler-adjacent surfaces.

/// Graph derivation from authored flow structures.
pub mod flow_graph;
/// Graph derivation from compiled recipe structures.
pub mod recipe_graph;

/// Canonical graph edge kinds used by authored-flow and compiled-recipe graph views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowEdgeKind {
    /// Linear step-to-step transition.
    Next,
    /// Positive branch transition for conditional blocks.
    Then,
    /// Negative branch transition for conditional blocks.
    Else,
    /// Loop-enter transition.
    Loop,
    /// Loop-exit transition.
    Exit,
    /// Loop back-edge transition.
    Repeat,
}

impl WorkflowEdgeKind {
    /// Returns the stable string form used by inspection DTOs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
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

/// Typed graph edge derived from a flow or recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowEdge {
    /// Source step id.
    pub from_step_id: String,
    /// Target step id.
    pub target_step_id: String,
    /// Transition kind.
    pub kind: WorkflowEdgeKind,
    /// Optional guard expression.
    pub guard: Option<String>,
    /// Optional loop `while` expression.
    pub loop_while: Option<String>,
    /// Optional loop `until` expression.
    pub loop_until: Option<String>,
    /// Optional loop max-iteration metadata.
    pub loop_max_iters: Option<u32>,
}
