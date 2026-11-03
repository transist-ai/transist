// SPDX-License-Identifier: MPL-2.0
//! Runtime recipe and persisted execution records shared across Transist layers.

mod skills;
pub use skills::*;
mod dataflow;
pub use dataflow::*;
mod documents;
pub use documents::*;

pub use crate::filesystem_policy::{
    FilesystemPolicy, FilesystemPolicySource, WorkspaceAccessMode, WorkspaceAccessPolicy,
};
pub use crate::network_policy::{NetworkAccessMode, NetworkPolicy, NetworkPolicySource};
use crate::prerequisites::PrerequisiteSet;
use crate::{CapabilityRequest, KernelValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

/// Schema identifier for persisted run locators.
pub const RUN_LOCATOR_SCHEMA_V1: &str = "transist.run_locator.v1";
/// Schema identifier for persisted active invocation records.
pub const ACTIVE_INVOCATION_RECORD_SCHEMA_V1: &str = "transist.active_invocation.v1";

/// Stable workflow identity stored in recipes and persisted run metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRef {
    /// Workflow id without version decoration.
    pub id: String,
    /// Workflow version string.
    pub version: String,
}

/// Durable mapping from a run id to its relative local run-root path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunLocator {
    /// Locator schema version.
    pub schema: String,
    /// Stable run id.
    pub run_id: String,
    /// Root run id for the lineage tree.
    pub root_run_id: String,
    /// Optional direct parent run id for an embedded child run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Run-root path relative to `<home>/runs/`.
    pub relative_path: String,
}

impl RunLocator {
    /// Returns the validated relative path encoded by this locator.
    pub fn relative_path_buf(&self) -> std::io::Result<PathBuf> {
        let path = PathBuf::from(&self.relative_path);
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("run locator path is not relative: {}", self.relative_path),
            ));
        }
        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "run locator path contains unsupported component: {}",
                    self.relative_path
                ),
            ));
        }
        Ok(path)
    }
}

/// Durable record describing an in-flight adapter invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveInvocationRecord {
    /// Schema identifier.
    pub schema: String,
    /// Stable invocation id.
    pub invoke_id: String,
    /// Owning run id.
    pub run_id: String,
    /// Owning step id.
    pub step_id: String,
    /// Adapter kind that owns the process.
    pub adapter_kind: String,
    /// Root process id.
    pub pid: u32,
    /// Optional process-group id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// Executor process id.
    pub executor_pid: u32,
    /// Spawn timestamp in Unix milliseconds.
    pub started_at_ms: u64,
    /// OS process-birth token used for PID reuse protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_birth_token: Option<String>,
}

/// Recipe repository descriptor retained for authored source provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeRepository {
    /// Repository identifier.
    pub id: String,
    /// Repository path or URL as authored.
    pub path: String,
    /// Whether this repository is treated as the default source root.
    #[serde(default)]
    pub default: bool,
}

/// Declared execution workspace for a workflow or persisted run.
///
/// The authored `path` may be an absolute literal or an exact immutable input
/// reference. Fresh run creation resolves it to a canonical absolute path and
/// persists that result in the run-scoped recipe and state record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    /// Authored path template or resolved canonical absolute workspace path.
    pub path: String,
    /// Access requested for the workspace.
    pub access: WorkspaceAccessMode,
}

/// Persisted input schema for one compiled recipe input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeInputSpec {
    /// Kernel value type name expected for the input.
    #[serde(rename = "type")]
    pub value_type: String,
    /// Whether the runtime requires the caller to provide a value.
    #[serde(default)]
    pub required: bool,
    /// Optional default value materialized when the caller omits the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<KernelValue>,
    /// Human-oriented description preserved from authored workflow input docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Compiled execution mode for a recipe step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepExecutionDescriptor {
    /// Invoke a plugin action through a concrete adapter surface.
    Invoke {
        /// Adapter family that owns this invocation.
        adapter: crate::AdapterKind,
        /// Stable plugin package id retained explicitly in compiled recipe state.
        ///
        /// Older recipes may omit this additive field; in that case `plugin`
        /// remains the compatibility source of truth.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin_id: Option<String>,
        /// Plugin package id that owns the action.
        plugin: String,
        /// Plugin action id to invoke.
        action: String,
        /// Execution kind selected by compilation for the adapter call.
        execution_kind: String,
        /// Concrete plugin version pinned at compile time, when a
        /// `plugin_version` selector was present on the authored step or
        /// inferred from a single installed version.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin_version: Option<String>,
        /// Deterministic SHA-256 digest of the installed plugin directory
        /// pinned at compile time, so runtime re-entry can detect byte-level
        /// drift in the pinned plugin. Absent when no home context was
        /// available at compile time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin_sha256: Option<String>,
    },
    /// Execute a built-in control-flow primitive.
    Control {
        /// Primitive id, such as a gate or branch operation.
        primitive: String,
    },
    /// Execute a nested compiled recipe as a subflow.
    Subflow {
        /// Fully compiled child recipe for the subflow.
        recipe: Box<Recipe>,
    },
}

/// Compiled step record retained in a recipe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeStep {
    /// Stable step id.
    pub id: String,
    /// Execution descriptor for the step.
    pub execution: StepExecutionDescriptor,
    /// Materialized static `with` bindings supplied to the step.
    #[serde(default)]
    pub with: BTreeMap<String, KernelValue>,
    /// Optional prompt binding for prompt-capable actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_binding: Option<RecipePromptBinding>,
    /// Optional rendered skills bundle for prompt-capable actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_binding: Option<RecipeSkillsBinding>,
    /// Variable assignments evaluated after the step completes successfully.
    ///
    /// Keys are workflow-var names; values are `KernelValue` literals or
    /// placeholder strings (`{{inputs.*}}`, `{{steps.<id>.out.*}}`) resolved
    /// at runtime against `workflow_inputs` and the step's own `last_result`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, KernelValue>,
    /// Optional authored filesystem policy override for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Optional authored network policy override for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy: Option<NetworkPolicy>,
    /// Effective resolved filesystem policy for this step, populated at
    /// prepare time by merging global, workflow, and step policies. Persisted
    /// in `recipe.yaml` so resume and replay never re-read global settings.
    #[serde(default, skip_serializing_if = "FilesystemPolicy::is_default")]
    pub effective_filesystem_policy: FilesystemPolicy,
    /// Source of the effective filesystem policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_filesystem_policy_source: Option<FilesystemPolicySource>,
    /// Effective resolved network policy for this step, populated at prepare
    /// time by merging global, workflow, and step policies.
    #[serde(default, skip_serializing_if = "NetworkPolicy::is_default")]
    pub effective_network_policy: NetworkPolicy,
    /// Source of the effective network policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_network_policy_source: Option<NetworkPolicySource>,
}

/// Terminal outcome that activates a cleanup step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CleanupTrigger {
    /// Run completed successfully.
    Success,
    /// Run was canceled.
    Canceled,
    /// Run failed and requires explicit cleanup.
    Failed,
}

/// Compiled cleanup step retained separately from the main workflow graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeCleanupStep {
    /// Cleanup trigger outcomes that activate this step.
    #[serde(default)]
    pub when: Vec<CleanupTrigger>,
    /// Flattened invoke step contents reused by cleanup execution.
    #[serde(flatten)]
    pub step: RecipeStep,
}

/// Mapping from authored prompt data into runtime step inputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipePromptBinding {
    /// Input field that receives inline prompt text.
    pub inline_field: String,
    /// Optional input field that receives a prompt file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_field: Option<String>,
    /// Authored prompt source retained by compilation.
    pub source: RecipePromptSource,
}

/// Authored prompt source preserved in the compiled recipe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipePromptSource {
    /// Inline prompt text embedded directly in the workflow.
    Inline {
        /// Prompt text copied directly into the compiled recipe.
        value: String,
    },
    /// Prompt text loaded from a file path during compilation.
    File {
        /// Source file path as recorded by compilation.
        path: String,
    },
}

/// Graph edge describing a possible transition between recipe steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeGraphEdge {
    /// Source step id.
    pub from: String,
    /// Destination step id.
    pub to: String,
    /// Event name that triggers the transition.
    pub event: String,
    /// Optional guard expression that must evaluate truthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    /// Optional loop metadata retained for replay and inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#loop: Option<RecipeLoop>,
}

/// Loop metadata attached to a compiled graph edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeLoop {
    /// Optional loop condition that must remain true to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub while_expr: Option<String>,
    /// Optional completion condition that terminates the loop when true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_expr: Option<String>,
    /// Optional iteration cap for the edge-local loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iters: Option<u32>,
}

/// Compiled transition graph for a recipe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeGraph {
    /// Entry step id.
    pub entry: String,
    /// Directed edges between steps.
    #[serde(default)]
    pub edges: Vec<RecipeGraphEdge>,
    /// Optional global graph iteration cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
}

/// Fully compiled workflow recipe consumed by the runtime kernel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recipe {
    /// Root-lexical managed document declarations, distinct from action data and authority.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub documents: BTreeMap<String, DocumentDeclaration>,
    /// Recipe schema version string.
    pub schema: String,
    /// Source workflow identity.
    pub workflow: WorkflowRef,
    /// Optional source directory retained for provenance and local inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<String>,
    /// Optional authored workspace binding preserved in registry recipes and
    /// resolved to a canonical path in run-scoped recipes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceBinding>,
    /// Source repositories associated with the authored workflow.
    #[serde(default)]
    pub repositories: Vec<RecipeRepository>,
    /// Workflow-level host prerequisites enforced before step dispatch.
    #[serde(default, skip_serializing_if = "PrerequisiteSet::is_empty")]
    pub prerequisites: PrerequisiteSet,
    /// Optional authored workflow-level filesystem policy override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Optional authored workflow-level network policy override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy: Option<NetworkPolicy>,
    /// Compiled input schema for caller-supplied run inputs.
    #[serde(default)]
    pub inputs: BTreeMap<String, RecipeInputSpec>,
    /// Ordered step records owned by the recipe.
    #[serde(default)]
    pub steps: Vec<RecipeStep>,
    /// Optional cleanup steps retained separately from the main workflow graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup: Vec<RecipeCleanupStep>,
    /// Optional execution graph for non-linear control flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<RecipeGraph>,
    /// Complete normalized capability requests keyed by recipe step id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capabilities: BTreeMap<String, Vec<CapabilityRequest>>,
    /// Whether the capability map was materialized by the kernel authority
    /// projection. Older and hand-built recipes remain readable, but a run
    /// born from a materialized recipe fails closed if its map is altered.
    #[serde(default)]
    pub capabilities_persisted: bool,
}

/// Marks whether a visited invoke step came from the cleanup section.
pub type IsCleanup = bool;

/// Visits every invoke step in the recipe, including cleanup and nested subflows.
pub fn for_each_invoke_step<F>(recipe: &Recipe, visit: &mut F)
where
    F: FnMut(&RecipeStep, IsCleanup),
{
    for step in &recipe.steps {
        visit_invoke_steps(step, false, visit);
    }
    for cleanup in &recipe.cleanup {
        visit_invoke_steps(&cleanup.step, true, visit);
    }
}

fn visit_invoke_steps<F>(step: &RecipeStep, is_cleanup: IsCleanup, visit: &mut F)
where
    F: FnMut(&RecipeStep, IsCleanup),
{
    match &step.execution {
        StepExecutionDescriptor::Invoke { .. } => visit(step, is_cleanup),
        StepExecutionDescriptor::Subflow { recipe } => for_each_invoke_step(recipe, visit),
        StepExecutionDescriptor::Control { .. } => {}
    }
}

/// Persisted manual-gate decision record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateDecisionRecord {
    /// Record schema version string.
    pub schema: String,
    /// Run id that owns the gate decision.
    pub run_id: String,
    /// Stable gate id.
    pub gate_id: String,
    /// Step id for the gate step.
    pub step_id: String,
    /// Exact runtime-owned review request; required for document-review gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_request: Option<GateReviewRequest>,
    /// Recorded decision such as `approve` or `reject`.
    pub decision: String,
    /// Optional operator identity recorded for the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Optional origin of the actor identity such as `flag` or `env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<String>,
    /// Optional operator-supplied rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional decision timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at_ms: Option<u64>,
}

/// Persisted record of materialized run inputs and their provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunInputsRecord {
    /// Record schema version string.
    pub schema: String,
    /// Run id that owns the inputs.
    pub run_id: String,
    /// Workflow identity that defined the input schema.
    pub workflow: WorkflowRef,
    /// Final input values seen by the runtime.
    #[serde(default)]
    pub values: BTreeMap<String, KernelValue>,
    /// Source labels describing where each input value came from.
    #[serde(default)]
    pub sources: BTreeMap<String, String>,
}

/// Compatibility alias for the canonical execution request type.
pub type ExecutionRequest = crate::tep::TepRequest;
/// Compatibility alias for the canonical execution result type.
pub type ExecutionResult = crate::tep::TepResult;
/// Compatibility alias for persisted artifact references.
pub type ArtifactEnvelope = crate::tep::FileRef;
/// Compatibility alias for usage accounting returned by adapters.
pub type UsageEnvelope = crate::tep::InvokeUsage;

/// Persisted output envelope for one completed step execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepResultEnvelope {
    /// Kernel output value returned by the step.
    #[serde(default)]
    pub output: KernelValue,
    /// Optional content-address checksums associated with persisted outputs.
    #[serde(default)]
    pub checksums: BTreeMap<String, String>,
}

/// Runtime event record emitted during run execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeEvent {
    /// Event kind name.
    pub kind: String,
    /// Run id that emitted the event.
    pub run_id: String,
    /// Optional step id associated with the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Event timestamp in Unix milliseconds.
    pub timestamp_unix_ms: u64,
    /// Event payload encoded as a kernel value.
    #[serde(default)]
    pub payload: KernelValue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn invoke_step(id: &str) -> RecipeStep {
        RecipeStep {
            id: id.to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: crate::AdapterKind::Process,
                plugin_id: Some("shell".to_string()),
                plugin: "shell".to_string(),
                action: "echo".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }
    }

    fn control_step(id: &str) -> RecipeStep {
        RecipeStep {
            id: id.to_string(),
            execution: StepExecutionDescriptor::Control {
                primitive: "control.noop".to_string(),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }
    }

    fn recipe_with(steps: Vec<RecipeStep>, cleanup: Vec<RecipeCleanupStep>) -> Recipe {
        Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "demo".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps,
            cleanup,
            graph: None,
            capabilities: BTreeMap::new(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    #[test]
    fn for_each_invoke_step_visits_cleanup_and_nested_subflow_steps() {
        let child = recipe_with(
            vec![invoke_step("child_invoke"), control_step("child_control")],
            Vec::new(),
        );
        let recipe = recipe_with(
            vec![
                invoke_step("root_invoke"),
                RecipeStep {
                    id: "subflow".to_string(),
                    execution: StepExecutionDescriptor::Subflow {
                        recipe: Box::new(child),
                    },
                    with: BTreeMap::new(),
                    prompt_binding: None,
                    skills_binding: None,
                    vars: BTreeMap::new(),
                    filesystem_policy: None,
                    network_policy: None,
                    effective_filesystem_policy: Default::default(),
                    effective_filesystem_policy_source: None,
                    effective_network_policy: Default::default(),
                    effective_network_policy_source: None,
                },
            ],
            vec![RecipeCleanupStep {
                when: vec![CleanupTrigger::Failed],
                step: invoke_step("cleanup_invoke"),
            }],
        );

        let mut visited = Vec::new();
        for_each_invoke_step(&recipe, &mut |step, is_cleanup| {
            visited.push((step.id.clone(), is_cleanup));
        });

        assert_eq!(
            visited,
            vec![
                ("root_invoke".to_string(), false),
                ("child_invoke".to_string(), false),
                ("cleanup_invoke".to_string(), true),
            ]
        );
    }

    #[test]
    fn gate_decision_record_deserializes_legacy_payload_without_actor_fields() {
        let payload = json!({
            "schema": "transist.gate_decision.v1",
            "run_id": "run-1",
            "gate_id": "review",
            "step_id": "review",
            "decision": "approved",
            "reason": "looks good",
            "decided_at_ms": 42
        });

        let record: GateDecisionRecord =
            serde_json::from_value(payload).expect("deserialize legacy gate decision");

        assert_eq!(record.actor, None);
        assert_eq!(record.actor_source, None);
        assert_eq!(record.reason.as_deref(), Some("looks good"));
    }

    #[test]
    fn gate_decision_record_round_trips_actor_fields() {
        let record = GateDecisionRecord {
            review_request: None,
            schema: "transist.gate_decision.v1".to_string(),
            run_id: "run-1".to_string(),
            gate_id: "review".to_string(),
            step_id: "review".to_string(),
            decision: "approved".to_string(),
            actor: Some("Reviewer <reviewer@example.com>".to_string()),
            actor_source: Some("flag".to_string()),
            reason: Some("looks good".to_string()),
            decided_at_ms: Some(42),
        };

        let value = serde_json::to_value(&record).expect("serialize gate decision");

        assert_eq!(value["actor"], "Reviewer <reviewer@example.com>");
        assert_eq!(value["actor_source"], "flag");
    }
}
