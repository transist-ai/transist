// SPDX-License-Identifier: MPL-2.0
//! Serialized static-authoring views shared by compiler and application services.
use crate::{DocumentBindingLocation, PrerequisiteSet, RecipeGraph, WorkflowRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Versioned static authoring view. No field is execution or authorization evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowExplanation {
    /// Format identifier, independent of runtime and catalog release versions.
    pub schema: String,
    /// Explicit boundary statement shown in both output formats.
    pub notice: String,
    /// Root workflow identity.
    pub workflow: WorkflowRef,
    /// Captured definition-set digest (LF-normalized signature contract).
    pub definition_sha256: String,
    /// Exact captured file digests keyed by package-relative path.
    pub assets: BTreeMap<String, ExplanationAsset>,
    /// Actual installed contract snapshots, keyed by exact id@version.
    pub pins: BTreeMap<String, ExplanationPin>,
    /// Root document declarations; literal initial content is represented by digest.
    pub documents: BTreeMap<String, Value>,
    /// Recipe-local scopes keyed by root or module-call path.
    pub scopes: BTreeMap<String, ExplanationScope>,
    /// Steps keyed by lexical call path, not by host path or generated child id.
    pub steps: BTreeMap<String, ExplanationStep>,
    /// Optional previous-to-current structural comparison.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison: Option<ExplanationComparison>,
}

/// Exact bytes captured by the compiler; paths are package-relative.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationAsset {
    /// SHA-256 of exact source bytes.
    pub sha256: String,
    /// SHA-256 of LF-normalized bytes used for trust digests.
    pub normalized_sha256: String,
}

/// A pinned package is resolved only from the selected read-only local catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationPin {
    /// resolved, unresolved_missing, or unresolved_invalid; never a trust decision.
    pub status: String,
    /// Actual manifest/payload/schema digest when resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Input state distinguishes absence from explicit null and symbolic module data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationInput {
    /// Declared workflow input type.
    #[serde(rename = "type")]
    pub value_type: String,
    /// Whether execution requires a value.
    pub required: bool,
    /// provided, default, unset_null (omitted optional), missing, or symbolic.
    pub state: String,
    /// Actual birth-known value, or symbolic binding. Missing values are omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// One compiled recipe scope, including ordered step and cleanup membership.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationScope {
    /// Definition source path relative to package root.
    pub source: String,
    /// Bound known and explicitly unresolved inputs.
    pub inputs: BTreeMap<String, ExplanationInput>,
    /// Named module exports, including their original selectors.
    pub outputs: Value,
    /// Requested authored authority; neither effective policy nor grants.
    pub authority_requests: Value,
    /// Declared prerequisites only; not evaluated or executed.
    pub prerequisites: Value,
    /// Authored order, independent of sorted map ordering.
    pub step_order: Vec<String>,
    /// Separate cleanup phase in authored order.
    pub cleanup_order: Vec<String>,
    /// Compiler-generated transitions, guards, and graph limits.
    pub graph: Option<RecipeGraph>,
}

/// A declared dependency, with an exact JSON pointer and recipe-local selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ExplanationDependency {
    /// JSON pointer within this step or scope.
    pub path: String,
    /// Input, variable, direct output, or expression selector.
    pub selector: String,
}

/// Structural step view. Arbitrary external behavior is always opaque.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationStep {
    /// invoke, control, or module.
    pub kind: String,
    /// Full adapter:plugin/action identity, or control primitive.
    pub action: Option<String>,
    /// Exact id@version key into pins.
    pub pin: Option<String>,
    /// resolved, unresolved_missing/invalid/action/adapter, or not_applicable.
    pub contract_status: String,
    /// Actual action descriptor and locality from the pinned package, if resolved.
    pub contract: Option<ExplanationContract>,
    /// External actions, scripts, and prompts have no inferred behavioral guarantees.
    pub behavior: String,
    /// Profile/context-expanded authored action or module input bindings.
    pub bindings: Value,
    /// Birth-known bindings; unresolved templates remain symbolic, never fake values.
    pub bound_bindings: Value,
    /// Variable writes after successful completion.
    pub variable_assignments: Value,
    /// Sorted declared input, output, and variable dependencies.
    pub dependencies: Vec<ExplanationDependency>,
    /// Compiler-owned document paths/access and optional exact-review requirements.
    pub documents: Vec<DocumentBindingLocation>,
    /// Manual gate declaration, without invented decisions or revisions.
    pub gate: Option<ExplanationGate>,
    /// Gate decision injection declaration; not an approval.
    pub gate_decision: Option<ExplanationGateDecision>,
    /// Authored then/else membership and guard, if any.
    pub branch: Option<Value>,
    /// Authored body, predicates and limits, plus compiler-derived
    /// `exit_on_max_iters: true` for version-1 loop routing. Not an approval.
    pub loop_limits: Option<Value>,
    /// Separate cleanup triggers. Empty for main steps.
    pub cleanup_when: Vec<String>,
    /// Child scope path for a module call.
    pub module: Option<String>,
    /// Opaque inline/file prompt digests and contract binding, without prompt inference.
    pub prompt: Option<ExplanationPrompt>,
    /// Authored skills references (captured bytes are in assets).
    pub skills: Vec<String>,
    /// Requested workspace/filesystem/network/secrets and action, not authorization.
    pub authority_requests: Value,
}

/// Actual descriptor from an exact resolved package; never a synthesized action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationContract {
    /// Action schema filenames, secret delivery names and extension bindings.
    pub action: Value,
    /// Locality declared by the action/package, not verified effects.
    pub provider_kind: crate::ProviderKind,
    /// Declared package prerequisites, never evaluated here.
    pub package_prerequisites: PrerequisiteSet,
}

/// An authored gate requirement, without a runtime decision or revision identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationGate {
    /// Literal runtime gate name (defaults to the authored step id).
    pub gate_id: String,
    /// Root-lexical document names that a future gate would review.
    pub review_documents: Vec<String>,
    /// Always not_created; declaration is not approval.
    pub decision_state: String,
    /// Always not_created; exact revisions are selected only during execution.
    pub revision_state: String,
}

/// A request to inject a future recipe-local gate decision into action data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationGateDecision {
    /// Authored gate step id in the same scope.
    pub step: String,
    /// Always not_created, including when the action contract is unresolved.
    pub decision_state: String,
    /// Actual injection field, only when declared by a resolved action contract.
    pub contract_binding: Option<Value>,
}

/// Opaque prompt content identity, not a description of enforced behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExplanationPrompt {
    /// Inline authored text, with birth-known input substitution only.
    Inline {
        /// Always opaque_prompt.
        behavior: String,
        /// symbolic or birth_known; a partial digest never implies a concrete request.
        binding_state: String,
        /// Digest of declared text after compiler declaration expansion.
        sha256: String,
        /// Digest after partial input binding; unresolved templates remain literal.
        bound_sha256: String,
        /// Actual action prompt field declaration, when resolved.
        contract_binding: Option<Value>,
    },
    /// Captured detached text, relative to its declaring module.
    File {
        /// Always opaque_prompt.
        behavior: String,
        /// Package-relative captured asset label.
        path: String,
        /// Digest of exact captured bytes.
        sha256: String,
        /// Actual action prompt field declaration, when resolved.
        contract_binding: Option<Value>,
    },
}

/// Deterministic content comparison; absence of differences is not equivalence proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationComparison {
    /// Previous definition-set digest.
    pub previous_definition_sha256: String,
    /// Explicit limit on interpretation of the comparison.
    pub notice: String,
    /// Lexically sorted JSON-pointer changes; arrays are compared as ordered content.
    pub changes: Vec<ExplanationChange>,
}

/// One structural change; omitted before/after means absence, not JSON null.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplanationChange {
    /// assets, pins, actions, gates, limits, authority, bindings, or structure.
    pub category: String,
    /// Escaped JSON pointer into the explanation.
    pub path: String,
    /// Previous value when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    /// Current value when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}
