// SPDX-License-Identifier: MPL-2.0
//! Workflow discovery and mapping helpers for inspection APIs.

use super::*;
use crate::steps::step_kind_from_recipe;
use transist_compiler::graph::{WorkflowEdge, flow_graph, recipe_graph};
use transist_compiler::templating::input_refs::collect_input_refs_from;

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredWorkflow {
    pub(crate) workflow_id: String,
    pub(crate) workflow_version: String,
    /// Source path relative to the repo root, when a source file was found.
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) flow: Option<Flow>,
    pub(crate) recipe: Option<Recipe>,
    /// True when this workflow is referenced as a subflow by another discovered workflow.
    pub(crate) is_subflow: bool,
}

impl DiscoveredWorkflow {
    pub(crate) fn workflow_ref(&self) -> String {
        format!("{}@{}", self.workflow_id, self.workflow_version)
    }
}

#[derive(Debug, Clone, Default)]
struct WorkflowHierarchy {
    root_step_ids: Vec<String>,
    parent_by_step: BTreeMap<String, String>,
    children_by_step: BTreeMap<String, Vec<String>>,
}

impl WorkflowHierarchy {
    fn parent_step_id(&self, step_id: &str) -> Option<String> {
        self.parent_by_step.get(step_id).cloned()
    }

    fn child_step_ids(&self, step_id: &str) -> Vec<String> {
        self.children_by_step
            .get(step_id)
            .cloned()
            .unwrap_or_default()
    }
}

pub(crate) fn discover_workflows(store: &LocalInspectionStore) -> Result<Vec<DiscoveredWorkflow>> {
    let mut discovered = BTreeMap::<(String, String), DiscoveredWorkflow>::new();
    let mut subflow_targets: std::collections::BTreeSet<(String, String)> =
        std::collections::BTreeSet::new();

    for recipe in store.list_compiled_recipes()? {
        let key = (recipe.workflow.id.clone(), recipe.workflow.version.clone());

        // Collect subflow references from compiled recipe steps so we can
        // mark subflow workflows for exclusion from root-level launch lists.
        for step in &recipe.steps {
            if let transist_protocol::StepExecutionDescriptor::Subflow { recipe: sub_recipe } =
                &step.execution
            {
                subflow_targets.insert((
                    sub_recipe.workflow.id.clone(),
                    sub_recipe.workflow.version.clone(),
                ));
            }
        }

        discovered.entry(key.clone()).or_insert(DiscoveredWorkflow {
            workflow_id: key.0,
            workflow_version: key.1,
            source_path: None,
            flow: None,
            recipe: Some(recipe),
            is_subflow: false,
        });
    }

    for workflow in discovered.values_mut() {
        let key = (
            workflow.workflow_id.clone(),
            workflow.workflow_version.clone(),
        );
        if subflow_targets.contains(&key) {
            workflow.is_subflow = true;
        }
    }

    Ok(discovered.into_values().collect())
}

pub(crate) fn read_workflow_content(path: &Path) -> Result<InspectionWorkflowContent> {
    Ok(InspectionWorkflowContent {
        path: path.display().to_string(),
        bytes: fs::read(path)
            .with_context(|| format!("failed reading workflow content: {}", path.display()))?,
    })
}

pub(crate) fn compiled_recipe_path(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
) -> PathBuf {
    home_dir
        .join("registry/recipes")
        .join(format!("{workflow_id}-{workflow_version}.yaml"))
}

pub(crate) fn workflow_summary_from_discovery(
    workflow: DiscoveredWorkflow,
) -> InspectionWorkflowSummary {
    let flow = workflow.flow.as_ref();
    let recipe = workflow.recipe.as_ref();
    let hierarchy = workflow_hierarchy(flow, recipe);
    InspectionWorkflowSummary {
        workflow_id: workflow.workflow_id.clone(),
        workflow_version: workflow.workflow_version.clone(),
        source_ref: workflow
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        title: None,
        inputs: workflow_inputs(flow, recipe),
        compiled_recipe_available: workflow.recipe.is_some(),
        runnable_in_principle: workflow.recipe.is_some(),
        step_count: workflow_step_count(flow, recipe),
        root_step_count: hierarchy.root_step_ids.len(),
        is_subflow: workflow.is_subflow,
    }
}

pub(crate) fn workflow_detail_from_discovery(
    home_dir: &Path,
    workflow: DiscoveredWorkflow,
) -> InspectionWorkflowDetail {
    let summary = workflow_summary_from_discovery(workflow.clone());
    let hierarchy = workflow_hierarchy(workflow.flow.as_ref(), workflow.recipe.as_ref());
    let steps =
        workflow_steps_summary(workflow.flow.as_ref(), workflow.recipe.as_ref(), &hierarchy);
    let prompt_references =
        workflow_prompt_references(workflow.flow.as_ref(), workflow.recipe.as_ref());
    InspectionWorkflowDetail {
        summary,
        steps,
        prompt_references,
        validation: workflow_validation_summary(home_dir, workflow.flow.as_ref()),
    }
}

fn workflow_validation_summary(home_dir: &Path, flow: Option<&Flow>) -> WorkflowValidationSummary {
    let Some(flow) = flow else {
        return WorkflowValidationSummary {
            status: "unknown".to_string(),
            errors: Vec::new(),
        };
    };

    match validate_flow_in_home(home_dir, flow) {
        Ok(()) => WorkflowValidationSummary {
            status: "valid".to_string(),
            errors: Vec::new(),
        },
        Err(error) => WorkflowValidationSummary {
            status: "invalid".to_string(),
            errors: vec![error.to_string()],
        },
    }
}

fn workflow_inputs(flow: Option<&Flow>, recipe: Option<&Recipe>) -> Vec<WorkflowInputSummary> {
    if let Some(flow) = flow {
        return flow
            .inputs
            .iter()
            .map(|(name, spec)| WorkflowInputSummary {
                name: name.clone(),
                value_type: spec.value_type.clone(),
                required: spec.required,
                has_default: spec.default.is_some(),
                description: spec.description.clone(),
            })
            .collect();
    }
    recipe
        .map(|recipe| {
            recipe
                .inputs
                .iter()
                .map(|(name, spec)| WorkflowInputSummary {
                    name: name.clone(),
                    value_type: spec.value_type.clone(),
                    required: spec.required,
                    has_default: spec.default.is_some(),
                    description: spec.description.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn workflow_step_count(flow: Option<&Flow>, recipe: Option<&Recipe>) -> usize {
    flow.map(|flow| flow.steps.len())
        .or_else(|| recipe.map(|recipe| recipe.steps.len()))
        .unwrap_or_default()
}

fn workflow_steps_summary(
    flow: Option<&Flow>,
    recipe: Option<&Recipe>,
    hierarchy: &WorkflowHierarchy,
) -> Vec<WorkflowStepSummary> {
    if let Some(flow) = flow {
        let graph_edges = flow_graph_edges_by_from(flow);
        return flow
            .steps
            .iter()
            .map(|step| workflow_step_summary_from_flow(step, graph_edges.get(&step.id), hierarchy))
            .collect();
    }
    recipe
        .map(|recipe| {
            let graph_edges = recipe_graph_edges_by_from(recipe);
            recipe
                .steps
                .iter()
                .map(|step| workflow_step_summary_from_recipe(step, &graph_edges, hierarchy))
                .collect()
        })
        .unwrap_or_default()
}

fn workflow_step_summary_from_flow(
    step: &FlowStep,
    graph_edges: Option<&Vec<WorkflowGraphEdgeSummary>>,
    hierarchy: &WorkflowHierarchy,
) -> WorkflowStepSummary {
    let (prompt_source, prompt_detail) = prompt_fields_from_flow_step(step);
    WorkflowStepSummary {
        step_id: step.id.clone(),
        parent_step_id: hierarchy.parent_step_id(&step.id),
        child_step_ids: hierarchy.child_step_ids(&step.id),
        kind: flow_step_kind(step).to_string(),
        adapter: step.adapter.clone(),
        plugin: step.plugin.clone(),
        action: step.action.clone(),
        execution_kind: step.execution_kind.clone(),
        control: step.control.clone(),
        subflow_path: step.subflow.as_ref().map(|subflow| subflow.path.clone()),
        with_keys: sorted_yaml_keys(&step.with),
        subflow_input_keys: step
            .subflow
            .as_ref()
            .map(|subflow| sorted_yaml_keys(&subflow.inputs))
            .unwrap_or_default(),
        workflow_input_refs: collect_workflow_input_refs_from_flow_step(step),
        if_guard: step
            .if_block
            .as_ref()
            .map(|if_block| if_block.guard.trim().to_string()),
        if_then_refs: step
            .if_block
            .as_ref()
            .map(|if_block| {
                if_block
                    .then_branch
                    .iter()
                    .map(|branch| branch.ref_step.clone())
                    .collect()
            })
            .unwrap_or_default(),
        if_else_refs: step
            .if_block
            .as_ref()
            .map(|if_block| {
                if_block
                    .else_branch
                    .iter()
                    .map(|branch| branch.ref_step.clone())
                    .collect()
            })
            .unwrap_or_default(),
        loop_while: step
            .loop_block
            .as_ref()
            .and_then(|loop_block| loop_block.while_expr.as_ref())
            .map(|value| value.trim().to_string()),
        loop_until: step
            .loop_block
            .as_ref()
            .and_then(|loop_block| loop_block.until.as_ref())
            .map(|value| value.trim().to_string()),
        loop_body_refs: step
            .loop_block
            .as_ref()
            .map(|loop_block| {
                loop_block
                    .body
                    .iter()
                    .map(|branch| branch.ref_step.clone())
                    .collect()
            })
            .unwrap_or_default(),
        loop_max_iters: step
            .loop_block
            .as_ref()
            .map(|loop_block| loop_block.max_iters),
        loop_max_duration: step
            .loop_block
            .as_ref()
            .and_then(|loop_block| loop_block.max_duration.clone()),
        prompt_source,
        prompt_detail,
        skills: step.skills.clone().unwrap_or_default(),
        graph_edges: graph_edges.cloned().unwrap_or_default(),
    }
}

fn workflow_step_summary_from_recipe(
    step: &RecipeStep,
    all_graph_edges: &BTreeMap<String, Vec<WorkflowGraphEdgeSummary>>,
    hierarchy: &WorkflowHierarchy,
) -> WorkflowStepSummary {
    let graph_edges = all_graph_edges.get(&step.id).cloned().unwrap_or_default();
    let (loop_while, loop_until, loop_max_iters, loop_max_duration) =
        recipe_loop_details_from_step(step);
    let (prompt_source, prompt_detail) = prompt_fields_from_recipe_step(step);
    WorkflowStepSummary {
        step_id: step.id.clone(),
        parent_step_id: hierarchy.parent_step_id(&step.id),
        child_step_ids: hierarchy.child_step_ids(&step.id),
        kind: step_kind_from_recipe(step),
        adapter: match &step.execution {
            transist_protocol::StepExecutionDescriptor::Invoke { adapter, .. } => {
                Some(adapter.to_string())
            }
            _ => None,
        },
        plugin: match &step.execution {
            transist_protocol::StepExecutionDescriptor::Invoke { plugin, .. } => {
                Some(plugin.clone())
            }
            _ => None,
        },
        action: match &step.execution {
            transist_protocol::StepExecutionDescriptor::Invoke { action, .. } => {
                Some(action.clone())
            }
            _ => None,
        },
        execution_kind: match &step.execution {
            transist_protocol::StepExecutionDescriptor::Invoke { execution_kind, .. } => {
                Some(execution_kind.clone())
            }
            _ => None,
        },
        control: match &step.execution {
            transist_protocol::StepExecutionDescriptor::Control { primitive } => {
                Some(primitive.clone())
            }
            _ => None,
        },
        subflow_path: match &step.execution {
            transist_protocol::StepExecutionDescriptor::Subflow { recipe } => Some(format!(
                "{}@{}",
                recipe.workflow.id, recipe.workflow.version
            )),
            _ => None,
        },
        with_keys: sorted_kernel_keys(&step.with),
        subflow_input_keys: Vec::new(),
        workflow_input_refs: collect_workflow_input_refs_from_recipe_step(step, &graph_edges),
        if_guard: infer_if_guard_from_edges(
            &graph_edges,
            loop_while.is_none() && loop_until.is_none(),
        ),
        if_then_refs: graph_edges
            .iter()
            .filter(|edge| edge.kind == "then")
            .map(|edge| edge.target_step_id.clone())
            .collect(),
        if_else_refs: graph_edges
            .iter()
            .filter(|edge| edge.kind == "else")
            .map(|edge| edge.target_step_id.clone())
            .collect(),
        loop_while,
        loop_until,
        loop_body_refs: recipe_loop_body_step_ids(&step.id, all_graph_edges),
        loop_max_iters,
        loop_max_duration,
        prompt_source,
        prompt_detail,
        skills: skills_from_recipe_step(step),
        graph_edges,
    }
}

fn workflow_prompt_references(
    flow: Option<&Flow>,
    recipe: Option<&Recipe>,
) -> Vec<WorkflowPromptReference> {
    if let Some(flow) = flow {
        let mut refs = Vec::new();
        for step in &flow.steps {
            if let Some(prompt) = step.prompt.as_ref() {
                refs.push(WorkflowPromptReference {
                    step_id: step.id.clone(),
                    source: "inline".to_string(),
                    path: None,
                    preview: Some(truncate_preview(prompt)),
                });
            }
            if let Some(path) = step.prompt_file.as_ref() {
                refs.push(WorkflowPromptReference {
                    step_id: step.id.clone(),
                    source: "file".to_string(),
                    path: Some(path.clone()),
                    preview: None,
                });
            }
        }
        return refs;
    }
    recipe
        .map(|recipe| {
            let mut refs = Vec::new();
            for step in &recipe.steps {
                if let Some(binding) = step.prompt_binding.as_ref() {
                    match &binding.source {
                        transist_protocol::RecipePromptSource::Inline { value } => {
                            refs.push(WorkflowPromptReference {
                                step_id: step.id.clone(),
                                source: "inline".to_string(),
                                path: None,
                                preview: Some(truncate_preview(value)),
                            })
                        }
                        transist_protocol::RecipePromptSource::File { path } => {
                            refs.push(WorkflowPromptReference {
                                step_id: step.id.clone(),
                                source: "file".to_string(),
                                path: Some(path.clone()),
                                preview: None,
                            })
                        }
                    }
                }
            }
            refs
        })
        .unwrap_or_default()
}

fn flow_graph_edges_by_from(flow: &Flow) -> BTreeMap<String, Vec<WorkflowGraphEdgeSummary>> {
    let mut grouped = BTreeMap::<String, Vec<WorkflowGraphEdgeSummary>>::new();
    for edge in flow_graph::edges_from(flow) {
        let from_step_id = edge.from_step_id.clone();
        push_workflow_graph_edge(
            &mut grouped,
            &from_step_id,
            workflow_graph_edge_summary_from(edge),
        );
    }
    grouped
}

fn workflow_hierarchy(flow: Option<&Flow>, recipe: Option<&Recipe>) -> WorkflowHierarchy {
    if let Some(flow) = flow {
        return flow_workflow_hierarchy(flow);
    }
    recipe.map(recipe_workflow_hierarchy).unwrap_or_default()
}

fn flow_workflow_hierarchy(flow: &Flow) -> WorkflowHierarchy {
    let mut hierarchy = WorkflowHierarchy::default();
    let mut cursor = 0;
    while cursor < flow.steps.len() {
        hierarchy.root_step_ids.push(flow.steps[cursor].id.clone());
        cursor = populate_flow_step_hierarchy(flow, cursor, None, &mut hierarchy);
    }
    hierarchy
}

fn populate_flow_step_hierarchy(
    flow: &Flow,
    idx: usize,
    expected_step_id: Option<&str>,
    hierarchy: &mut WorkflowHierarchy,
) -> usize {
    let step = &flow.steps[idx];
    if let Some(expected_step_id) = expected_step_id {
        debug_assert_eq!(step.id, expected_step_id);
    }
    let mut cursor = idx + 1;
    let mut child_step_ids = Vec::new();

    if let Some(if_block) = step.if_block.as_ref() {
        for entry in if_block
            .then_branch
            .iter()
            .chain(if_block.else_branch.iter())
        {
            let child = flow
                .steps
                .get(cursor)
                .expect("validated flow keeps branch members contiguous");
            debug_assert_eq!(child.id, entry.ref_step);
            hierarchy
                .parent_by_step
                .insert(child.id.clone(), step.id.clone());
            child_step_ids.push(child.id.clone());
            cursor = populate_flow_step_hierarchy(flow, cursor, Some(&entry.ref_step), hierarchy);
        }
    }

    if let Some(loop_block) = step.loop_block.as_ref() {
        for entry in &loop_block.body {
            let child = flow
                .steps
                .get(cursor)
                .expect("validated flow keeps loop members contiguous");
            debug_assert_eq!(child.id, entry.ref_step);
            hierarchy
                .parent_by_step
                .insert(child.id.clone(), step.id.clone());
            child_step_ids.push(child.id.clone());
            cursor = populate_flow_step_hierarchy(flow, cursor, Some(&entry.ref_step), hierarchy);
        }
    }

    if !child_step_ids.is_empty() {
        hierarchy
            .children_by_step
            .insert(step.id.clone(), child_step_ids);
    }

    cursor
}

fn recipe_workflow_hierarchy(recipe: &Recipe) -> WorkflowHierarchy {
    let graph_edges = recipe_graph_edges_by_from(recipe);
    let step_ids = recipe
        .steps
        .iter()
        .map(|step| step.id.clone())
        .collect::<Vec<_>>();
    let index_by_id = recipe
        .steps
        .iter()
        .enumerate()
        .map(|(idx, step)| (step.id.clone(), idx))
        .collect::<BTreeMap<_, _>>();
    let mut hierarchy = WorkflowHierarchy {
        root_step_ids: step_ids,
        ..WorkflowHierarchy::default()
    };

    for (idx, step) in recipe.steps.iter().enumerate() {
        let child_step_ids = recipe_direct_child_step_ids(recipe, idx, &graph_edges, &index_by_id);
        if child_step_ids.is_empty() {
            continue;
        }
        hierarchy
            .children_by_step
            .insert(step.id.clone(), child_step_ids.clone());
        for child_step_id in child_step_ids {
            hierarchy
                .parent_by_step
                .entry(child_step_id)
                .or_insert_with(|| step.id.clone());
        }
    }

    hierarchy
        .root_step_ids
        .retain(|step_id| !hierarchy.parent_by_step.contains_key(step_id));
    hierarchy
}

/// Infers the ordered list of loop body step ids by walking the recipe's graph
/// from the first `loop` edge through `next` edges until a `repeat` back to the
/// loop step or the end of the chain.
fn recipe_loop_body_step_ids(
    loop_step_id: &str,
    graph_edges: &BTreeMap<String, Vec<WorkflowGraphEdgeSummary>>,
) -> Vec<String> {
    let Some(edges) = graph_edges.get(loop_step_id) else {
        return Vec::new();
    };
    let Some(first) = edges.iter().find(|edge| edge.kind == "loop") else {
        return Vec::new();
    };
    let mut body = vec![first.target_step_id.clone()];
    let mut current = first.target_step_id.clone();
    while let Some(next_edges) = graph_edges.get(&current) {
        if next_edges
            .iter()
            .any(|edge| edge.kind == "repeat" && edge.target_step_id == loop_step_id)
        {
            break;
        }
        let Some(next) = next_edges.iter().find(|edge| edge.kind == "next") else {
            break;
        };
        current = next.target_step_id.clone();
        body.push(current.clone());
    }
    body
}

fn recipe_direct_child_step_ids(
    recipe: &Recipe,
    step_idx: usize,
    graph_edges: &BTreeMap<String, Vec<WorkflowGraphEdgeSummary>>,
    index_by_id: &BTreeMap<String, usize>,
) -> Vec<String> {
    let Some(step) = recipe.steps.get(step_idx) else {
        return Vec::new();
    };
    let mut child_step_ids = Vec::new();

    if let Some(edges) = graph_edges.get(&step.id) {
        if edges.iter().any(|edge| edge.kind == "loop") {
            child_step_ids = recipe_loop_body_step_ids(&step.id, graph_edges);
        }
        if let Some(then_child_id) = edges
            .iter()
            .find(|edge| edge.kind == "then")
            .map(|edge| edge.target_step_id.clone())
        {
            child_step_ids.push(then_child_id);
        }
        if let Some(else_edge) = edges.iter().find(|edge| edge.kind == "else") {
            if !recipe_else_target_is_fallthrough(
                recipe,
                step_idx,
                &else_edge.target_step_id,
                graph_edges,
                index_by_id,
            ) {
                child_step_ids.push(else_edge.target_step_id.clone());
            }
        }
    }

    child_step_ids.dedup();
    child_step_ids
}

fn recipe_else_target_is_fallthrough(
    recipe: &Recipe,
    step_idx: usize,
    else_target_step_id: &str,
    graph_edges: &BTreeMap<String, Vec<WorkflowGraphEdgeSummary>>,
    index_by_id: &BTreeMap<String, usize>,
) -> bool {
    let Some(else_target_idx) = index_by_id.get(else_target_step_id).copied() else {
        return false;
    };
    if else_target_idx <= step_idx {
        return false;
    }

    recipe
        .steps
        .iter()
        .take(else_target_idx)
        .skip(step_idx + 1)
        .any(|candidate| {
            graph_edges
                .get(&candidate.id)
                .into_iter()
                .flatten()
                .any(|edge| edge.kind == "next" && edge.target_step_id == else_target_step_id)
        })
}

fn workflow_graph_edge_summary_from(edge: WorkflowEdge) -> WorkflowGraphEdgeSummary {
    let kind = edge.kind.as_str().to_string();
    WorkflowGraphEdgeSummary {
        label: kind.clone(),
        kind,
        target_step_id: edge.target_step_id,
        guard: edge.guard,
        loop_while: edge.loop_while,
        loop_until: edge.loop_until,
        loop_max_iters: edge.loop_max_iters,
    }
}

fn recipe_graph_edges_by_from(recipe: &Recipe) -> BTreeMap<String, Vec<WorkflowGraphEdgeSummary>> {
    let mut grouped = BTreeMap::<String, Vec<WorkflowGraphEdgeSummary>>::new();
    for edge in recipe_graph::edges_from(recipe) {
        let from_step_id = edge.from_step_id.clone();
        push_workflow_graph_edge(
            &mut grouped,
            &from_step_id,
            workflow_graph_edge_summary_from(edge),
        );
    }
    grouped
}

fn push_workflow_graph_edge(
    grouped: &mut BTreeMap<String, Vec<WorkflowGraphEdgeSummary>>,
    from_step_id: &str,
    edge: WorkflowGraphEdgeSummary,
) {
    grouped
        .entry(from_step_id.to_string())
        .or_default()
        .push(edge);
}

fn sorted_yaml_keys(map: &BTreeMap<String, yaml_serde::Value>) -> Vec<String> {
    map.keys().cloned().collect()
}

fn sorted_kernel_keys(map: &BTreeMap<String, KernelValue>) -> Vec<String> {
    map.keys().cloned().collect()
}

fn collect_workflow_input_refs_from_flow_step(step: &FlowStep) -> Vec<String> {
    let mut refs = BTreeSet::new();
    collect_input_refs_from_yaml_map(&step.with, &mut refs);
    if let Some(subflow) = step.subflow.as_ref() {
        collect_input_refs_from_yaml_map(&subflow.inputs, &mut refs);
    }
    if let Some(prompt) = step.prompt.as_ref() {
        insert_input_refs(prompt, &mut refs);
    }
    if let Some(prompt_file) = step.prompt_file.as_ref() {
        insert_input_refs(prompt_file, &mut refs);
    }
    if let Some(if_block) = step.if_block.as_ref() {
        insert_input_refs(&if_block.guard, &mut refs);
    }
    if let Some(loop_block) = step.loop_block.as_ref() {
        if let Some(while_expr) = loop_block.while_expr.as_ref() {
            insert_input_refs(while_expr, &mut refs);
        }
        if let Some(until_expr) = loop_block.until.as_ref() {
            insert_input_refs(until_expr, &mut refs);
        }
    }
    refs.into_iter().collect()
}

fn collect_workflow_input_refs_from_recipe_step(
    step: &RecipeStep,
    graph_edges: &[WorkflowGraphEdgeSummary],
) -> Vec<String> {
    let mut refs = BTreeSet::new();
    collect_input_refs_from_kernel_map(&step.with, &mut refs);
    if let Some(prompt_binding) = step.prompt_binding.as_ref() {
        match &prompt_binding.source {
            transist_protocol::RecipePromptSource::Inline { value } => {
                insert_input_refs(value, &mut refs)
            }
            transist_protocol::RecipePromptSource::File { path } => {
                insert_input_refs(path, &mut refs)
            }
        }
    }
    for edge in graph_edges {
        if let Some(guard) = edge.guard.as_ref() {
            insert_input_refs(guard, &mut refs);
        }
        if let Some(loop_while) = edge.loop_while.as_ref() {
            insert_input_refs(loop_while, &mut refs);
        }
        if let Some(loop_until) = edge.loop_until.as_ref() {
            insert_input_refs(loop_until, &mut refs);
        }
    }
    refs.into_iter().collect()
}

fn collect_input_refs_from_yaml_map(
    map: &BTreeMap<String, yaml_serde::Value>,
    refs: &mut BTreeSet<String>,
) {
    for value in map.values() {
        collect_input_refs_from_yaml_value(value, refs);
    }
}

fn collect_input_refs_from_yaml_value(value: &yaml_serde::Value, refs: &mut BTreeSet<String>) {
    match value {
        yaml_serde::Value::String(value) => insert_input_refs(value, refs),
        yaml_serde::Value::Sequence(values) => {
            for value in values {
                collect_input_refs_from_yaml_value(value, refs);
            }
        }
        yaml_serde::Value::Mapping(values) => {
            for value in values.values() {
                collect_input_refs_from_yaml_value(value, refs);
            }
        }
        _ => {}
    }
}

fn collect_input_refs_from_kernel_map(
    map: &BTreeMap<String, KernelValue>,
    refs: &mut BTreeSet<String>,
) {
    for value in map.values() {
        collect_input_refs_from_kernel_value(value, refs);
    }
}

fn collect_input_refs_from_kernel_value(value: &KernelValue, refs: &mut BTreeSet<String>) {
    match value {
        KernelValue::String(value) => insert_input_refs(value, refs),
        KernelValue::Array(values) => {
            for value in values {
                collect_input_refs_from_kernel_value(value, refs);
            }
        }
        KernelValue::Object(values) => {
            for value in values.values() {
                collect_input_refs_from_kernel_value(value, refs);
            }
        }
        _ => {}
    }
}

fn insert_input_refs(value: &str, refs: &mut BTreeSet<String>) {
    for input_ref in collect_input_refs_from(value) {
        refs.insert(input_ref.key);
    }
}

fn prompt_fields_from_flow_step(step: &FlowStep) -> (Option<String>, Option<String>) {
    if let Some(prompt) = step.prompt.as_ref() {
        return (Some("inline".to_string()), Some(truncate_preview(prompt)));
    }
    if let Some(path) = step.prompt_file.as_ref() {
        return (Some("file".to_string()), Some(path.clone()));
    }
    (None, None)
}

fn prompt_fields_from_recipe_step(step: &RecipeStep) -> (Option<String>, Option<String>) {
    let Some(binding) = step.prompt_binding.as_ref() else {
        return (None, None);
    };
    match &binding.source {
        transist_protocol::RecipePromptSource::Inline { value } => {
            (Some("inline".to_string()), Some(truncate_preview(value)))
        }
        transist_protocol::RecipePromptSource::File { path } => {
            (Some("file".to_string()), Some(path.clone()))
        }
    }
}

fn skills_from_recipe_step(step: &RecipeStep) -> Vec<String> {
    step.skills_binding
        .as_ref()
        .map(|binding| {
            binding
                .bundle
                .authored_refs
                .iter()
                .map(|skill| skill.raw.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn recipe_loop_details_from_step(
    step: &RecipeStep,
) -> (Option<String>, Option<String>, Option<u32>, Option<String>) {
    let Some(KernelValue::Object(loop_meta)) = step.with.get("__transist_loop") else {
        return (None, None, None, None);
    };
    let loop_while = loop_meta
        .get("while")
        .and_then(kernel_string)
        .map(|value| value.trim().to_string());
    let loop_until = loop_meta
        .get("until")
        .and_then(kernel_string)
        .map(|value| value.trim().to_string());
    let loop_max_iters = loop_meta.get("max_iters").and_then(kernel_u32);
    let loop_max_duration = loop_meta
        .get("max_duration_ms")
        .and_then(kernel_u64)
        .map(|value| format!("{value} ms"));
    (loop_while, loop_until, loop_max_iters, loop_max_duration)
}

fn infer_if_guard_from_edges(
    graph_edges: &[WorkflowGraphEdgeSummary],
    allow_inference: bool,
) -> Option<String> {
    if !allow_inference {
        return None;
    }
    graph_edges
        .iter()
        .find(|edge| edge.kind == "then")
        .and_then(|edge| edge.guard.clone())
}

fn kernel_string(value: &KernelValue) -> Option<&str> {
    match value {
        KernelValue::String(value) => Some(value.as_str()),
        _ => None,
    }
}

fn kernel_u64(value: &KernelValue) -> Option<u64> {
    match value {
        KernelValue::Number(value) => value.as_u64(),
        _ => None,
    }
}

fn kernel_u32(value: &KernelValue) -> Option<u32> {
    kernel_u64(value).and_then(|value| u32::try_from(value).ok())
}

fn flow_step_kind(step: &FlowStep) -> &'static str {
    if step.subflow.is_some() {
        "subflow"
    } else if step.control.is_some() {
        "control"
    } else {
        "invoke"
    }
}

fn truncate_preview(raw: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 96;
    if raw.chars().count() <= MAX_PREVIEW_CHARS {
        return raw.to_string();
    }
    let truncated = raw.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    format!("{truncated}...")
}

#[cfg(test)]
mod tests {
    use super::{flow_workflow_hierarchy, recipe_workflow_hierarchy};
    use transist_compiler::{Flow, compile_flow};

    #[test]
    fn flow_hierarchy_tracks_root_steps_and_nested_children() {
        let flow = flow_from_yaml(
            r#"
schema: transist.flow.v1
id: graph_demo
version: v1
steps:
  - id: branch
    control: control.noop
    if:
      guard: inputs.dry_run == true
      then:
        - ref: dry_path
      else:
        - ref: live_path
  - id: dry_path
    control: control.noop
  - id: live_path
    control: control.noop
  - id: review_loop
    control: control.noop
    loop:
      while: inputs.dry_run == false
      body:
        - ref: revise
      max_iters: 3
  - id: revise
    control: control.noop
"#,
        );

        let hierarchy = flow_workflow_hierarchy(&flow);
        assert_eq!(hierarchy.root_step_ids, vec!["branch", "review_loop"]);
        assert_eq!(
            hierarchy.child_step_ids("branch"),
            vec!["dry_path", "live_path"]
        );
        assert_eq!(hierarchy.child_step_ids("review_loop"), vec!["revise"]);
        assert_eq!(
            hierarchy.parent_step_id("dry_path"),
            Some("branch".to_string())
        );
        assert_eq!(
            hierarchy.parent_step_id("live_path"),
            Some("branch".to_string())
        );
        assert_eq!(
            hierarchy.parent_step_id("revise"),
            Some("review_loop".to_string())
        );
    }

    #[test]
    fn recipe_hierarchy_keeps_if_fallthrough_as_root_step() {
        let flow = flow_from_yaml(
            r#"
schema: transist.flow.v1
id: if_fallthrough
version: v1
steps:
  - id: choose
    control: control.noop
    if:
      guard: inputs.run_branch == true
      then:
        - ref: branch_task
  - id: branch_task
    control: control.noop
  - id: finish
    control: control.finish
"#,
        );
        let recipe = compile_flow(&flow).expect("recipe");

        let hierarchy = recipe_workflow_hierarchy(&recipe);
        assert_eq!(hierarchy.root_step_ids, vec!["choose", "finish"]);
        assert_eq!(hierarchy.child_step_ids("choose"), vec!["branch_task"]);
        assert_eq!(
            hierarchy.parent_step_id("branch_task"),
            Some("choose".to_string())
        );
        assert_eq!(hierarchy.parent_step_id("finish"), None);
    }

    fn flow_from_yaml(raw: &str) -> Flow {
        yaml_serde::from_str(raw).expect("flow yaml")
    }
}
