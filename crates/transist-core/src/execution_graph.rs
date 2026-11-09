// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, BTreeSet};
use transist_protocol::{Recipe, RecipeGraphEdge, RecipeLoop};

#[derive(Debug, Clone)]
pub struct GraphEdge {
    /// Explicit compiler-owned bounded-loop route, absent for legacy edges.
    #[allow(dead_code)] // Propagated now; scheduler consumption follows in Step 2.
    pub loop_route: Option<transist_protocol::RecipeLoopRoute>,
    pub to: String,
    pub event: String,
    pub guard: Option<String>,
    pub r#loop: Option<RecipeLoop>,
}

#[derive(Debug, Clone)]
pub struct StepNode {
    pub dependencies: BTreeSet<String>,
    pub outgoing: Vec<GraphEdge>,
}

#[derive(Debug, Clone)]
/// Runtime dependency graph derived from workflow recipe topology.
pub struct ExecutionGraph {
    /// Opt-in bounded-loop routing revision from the persisted recipe.
    #[allow(dead_code)] // Propagated now; scheduler consumption follows in Step 2.
    pub loop_routing_version: Option<u32>,
    /// Entry step id used as the first frontier candidate.
    pub entry: String,
    /// Optional graph-level iteration ceiling for loop traversal.
    pub max_iterations: Option<u32>,
    /// Per-step dependency and outgoing-edge topology.
    pub nodes: BTreeMap<String, StepNode>,
}

/// Builds a dependency graph from the recipe graph edges or falls back to a linear chain.
/// Rejects invalid loop-routing metadata, including in embedded recipes, without
/// producing an executable graph or modifying the supplied recipe.
pub fn build_execution_graph(recipe: &Recipe) -> anyhow::Result<ExecutionGraph> {
    transist_protocol::validate_recipe_loop_routing(recipe).map_err(anyhow::Error::msg)?;
    let entry = recipe
        .graph
        .as_ref()
        .map(|graph| graph.entry.clone())
        .unwrap_or_else(|| {
            recipe
                .steps
                .first()
                .map(|step| step.id.clone())
                .unwrap_or_default()
        });
    let max_iterations = recipe.graph.as_ref().and_then(|graph| graph.max_iterations);

    let mut nodes = BTreeMap::new();
    for step in &recipe.steps {
        nodes.insert(
            step.id.clone(),
            StepNode {
                dependencies: BTreeSet::new(),
                outgoing: Vec::new(),
            },
        );
    }

    let edges = if let Some(graph) = &recipe.graph {
        graph.edges.clone()
    } else {
        recipe
            .steps
            .windows(2)
            .map(|pair| RecipeGraphEdge {
                from: pair[0].id.clone(),
                to: pair[1].id.clone(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
                loop_route: None,
            })
            .collect()
    };

    for edge in edges {
        if edge.from != edge.to {
            if let Some(node) = nodes.get_mut(&edge.to) {
                node.dependencies.insert(edge.from.clone());
            }
        }
        if let Some(node) = nodes.get_mut(&edge.from) {
            node.outgoing.push(GraphEdge {
                to: edge.to,
                event: edge.event,
                guard: edge.guard,
                r#loop: edge.r#loop,
                loop_route: edge.loop_route,
            });
        }
    }

    Ok(ExecutionGraph {
        loop_routing_version: recipe
            .graph
            .as_ref()
            .and_then(|graph| graph.loop_routing_version),
        entry,
        max_iterations,
        nodes,
    })
}

/// Returns direct dependency step ids for the provided step.
pub fn dependencies<'a>(graph: &'a ExecutionGraph, step_id: &str) -> Vec<&'a str> {
    graph
        .nodes
        .get(step_id)
        .map(|node| node.dependencies.iter().map(String::as_str).collect())
        .unwrap_or_default()
}

/// Returns outgoing edges for the provided step.
pub fn outgoing_edges<'a>(graph: &'a ExecutionGraph, step_id: &str) -> Vec<&'a GraphEdge> {
    graph
        .nodes
        .get(step_id)
        .map(|node| node.outgoing.iter().collect())
        .unwrap_or_default()
}

/// Returns the set of step ids reachable from the graph entry, skipping outgoing
/// edges whose guards definitively evaluate to false against the supplied
/// workflow inputs and snapshot. Edges whose guards cannot be evaluated yet are
/// considered potentially reachable so their targets are still preflighted.
pub fn reachable_steps(
    graph: &ExecutionGraph,
    snapshot: Option<&crate::types::KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, transist_protocol::KernelValue>>,
) -> BTreeSet<String> {
    reachable_steps_in_scope(graph, snapshot, workflow_inputs, &BTreeSet::new())
}

/// Returns potential reachability while treating declared symbolic inputs as unknown.
/// Guards depending on `deferred_inputs` retain both branches; concrete guards keep
/// their existing pruning behavior. This is inspection/preflight, not scheduling.
pub(crate) fn reachable_steps_in_scope(
    graph: &ExecutionGraph,
    snapshot: Option<&crate::types::KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, transist_protocol::KernelValue>>,
    deferred_inputs: &BTreeSet<String>,
) -> BTreeSet<String> {
    use crate::gate_state::{GateState, evaluate_guard_expression_in_scope};
    let mut reached = BTreeSet::new();
    if graph.entry.is_empty() {
        return reached;
    }
    let mut stack = vec![graph.entry.clone()];
    while let Some(current) = stack.pop() {
        if !reached.insert(current.clone()) {
            continue;
        }
        let Some(node) = graph.nodes.get(&current) else {
            continue;
        };
        for edge in &node.outgoing {
            let dependency_state = snapshot.and_then(|snap| snap.steps.get(&current));
            let gate = if snapshot.is_none()
                && edge
                    .guard
                    .as_deref()
                    .is_some_and(guard_depends_on_runtime_state)
            {
                GateState::NotEvaluated
            } else {
                evaluate_guard_expression_in_scope(
                    edge.guard.as_deref(),
                    dependency_state,
                    snapshot,
                    workflow_inputs,
                    None,
                    deferred_inputs,
                )
            };
            if matches!(gate, GateState::ConditionFalse) {
                continue;
            }
            stack.push(edge.to.clone());
        }
    }
    reached
}

fn guard_depends_on_runtime_state(guard: &str) -> bool {
    guard.contains("steps.") || guard.contains("vars.")
}

/// Returns the authored recipe step ids that remain reachable for the supplied
/// persisted run snapshot and workflow inputs.
/// Returns an error if recursive loop-routing validation fails.
pub fn reachable_recipe_step_ids(
    recipe: &Recipe,
    snapshot: Option<&crate::types::KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, transist_protocol::KernelValue>>,
) -> anyhow::Result<BTreeSet<String>> {
    let graph = build_execution_graph(recipe)?;
    Ok(reachable_steps(&graph, snapshot, workflow_inputs))
}

/// Returns potential recipe reachability in a typed partial module-input scope.
/// Unknown input values cannot prune authority, secret, or static prerequisite checks.
/// Returns an error if recursive loop-routing validation fails.
pub fn reachable_recipe_step_ids_in_scope(
    recipe: &Recipe,
    snapshot: Option<&crate::types::KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, transist_protocol::KernelValue>>,
    deferred_inputs: &BTreeSet<String>,
) -> anyhow::Result<BTreeSet<String>> {
    Ok(reachable_steps_in_scope(
        &build_execution_graph(recipe)?,
        snapshot,
        workflow_inputs,
        deferred_inputs,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{RecipeGraph, RecipeStep, StepExecutionDescriptor, WorkflowRef};

    #[test]
    fn loop_routing_graph_rejects_invalid_metadata_and_preserves_legacy_loading() {
        let mut recipe: Recipe = serde_json::from_value(serde_json::json!({
            "schema": "transist.recipe.v1", "workflow": {"id":"test", "version":"v1"},
            "steps": [{"id":"loop", "execution":{"kind":"control", "primitive":"control.noop"},
                "with":{"__transist_loop":{"body_steps":["body"], "max_iters":2}}},
                {"id":"body", "execution":{"kind":"control", "primitive":"control.noop"}}],
            "graph": {"entry":"loop", "edges":[
                {"from":"loop", "to":"body", "event":"on_success", "guard":"true"},
                {"from":"body", "to":"loop", "event":"on_success", "loop":{}}]}
        }))
        .unwrap();
        let legacy = build_execution_graph(&recipe).unwrap();
        assert_eq!(legacy.loop_routing_version, None);
        assert!(legacy.nodes["loop"].outgoing[0].loop_route.is_none());
        recipe.graph.as_mut().unwrap().loop_routing_version = Some(1);
        assert!(build_execution_graph(&recipe).is_err());
        let route = transist_protocol::RecipeLoopRoute {
            owner_step_id: "loop".into(),
            role: transist_protocol::RecipeLoopRouteRole::Enter,
        };
        recipe.graph.as_mut().unwrap().edges[0].loop_route = Some(route.clone());
        let marked = build_execution_graph(&recipe).unwrap();
        assert_eq!(marked.loop_routing_version, Some(1));
        assert_eq!(marked.nodes["loop"].outgoing[0].loop_route, Some(route));
        assert_eq!(
            marked.nodes["loop"].outgoing[0].guard.as_deref(),
            Some("true")
        );
        assert!(marked.nodes["body"].outgoing[0].r#loop.is_some());
    }

    #[test]
    fn review_partial_guard_scope_preserves_unknown_branches_and_known_short_circuits() {
        let mut nodes = BTreeMap::new();
        for id in ["guard", "then", "else", "dead", "quoted", "prefix"] {
            nodes.insert(
                id.into(),
                StepNode {
                    dependencies: BTreeSet::new(),
                    outgoing: Vec::new(),
                },
            );
        }
        for (target, guard) in [
            ("then", "inputs.flag == true"),
            ("else", "NOT (inputs.flag == true)"),
            ("dead", "false && inputs.flag == true"),
            ("quoted", "'inputs.flag' == 'different'"),
            ("prefix", "inputs.flagship == true"),
        ] {
            nodes.get_mut("guard").unwrap().outgoing.push(GraphEdge {
                to: target.into(),
                event: "on_success".into(),
                guard: Some(guard.into()),
                r#loop: None,
                loop_route: None,
            });
        }
        let graph = ExecutionGraph {
            entry: "guard".into(),
            max_iterations: None,
            loop_routing_version: None,
            nodes,
        };
        let inputs = BTreeMap::from([
            (
                "flag".into(),
                transist_protocol::KernelValue::String("{{steps.produce.out.flag}}".into()),
            ),
            (
                "flagship".into(),
                transist_protocol::KernelValue::Bool(false),
            ),
        ]);
        let expected = BTreeSet::from(["guard".into(), "then".into(), "else".into()]);
        assert_eq!(
            reachable_steps_in_scope(
                &graph,
                None,
                Some(&inputs),
                &BTreeSet::from(["flag".into()])
            ),
            expected
        );
        let snapshot = crate::types::KernelRunSnapshot::new("guard-test", "workflow", "0.1.0", &[]);
        assert_eq!(
            reachable_steps_in_scope(
                &graph,
                Some(&snapshot),
                Some(&inputs),
                &BTreeSet::from(["flag".into()])
            ),
            expected
        );
    }

    fn control_step(id: &str) -> RecipeStep {
        RecipeStep {
            id: id.to_string(),
            execution: StepExecutionDescriptor::Control {
                primitive: "noop".to_string(),
            },
            with: std::collections::BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: std::collections::BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }
    }

    #[test]
    fn build_execution_graph_creates_linear_edges_without_explicit_graph() {
        let recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "wf".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: std::collections::BTreeMap::new(),
            steps: vec![control_step("a"), control_step("b"), control_step("c")],
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        let graph = build_execution_graph(&recipe).unwrap();
        assert_eq!(graph.entry, "a");
        assert_eq!(dependencies(&graph, "b"), vec!["a"]);
        assert_eq!(dependencies(&graph, "c"), vec!["b"]);
    }

    #[test]
    fn build_execution_graph_preserves_explicit_edges() {
        let recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "wf".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: std::collections::BTreeMap::new(),
            steps: vec![control_step("a"), control_step("b")],
            cleanup: Vec::new(),
            graph: Some(RecipeGraph {
                entry: "b".to_string(),
                edges: vec![transist_protocol::RecipeGraphEdge {
                    from: "b".to_string(),
                    to: "a".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("true".to_string()),
                    r#loop: None,
                    loop_route: None,
                }],
                max_iterations: Some(5),
                loop_routing_version: None,
            }),
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        let graph = build_execution_graph(&recipe).unwrap();
        assert_eq!(graph.entry, "b");
        assert_eq!(graph.max_iterations, Some(5));
        let outgoing = outgoing_edges(&graph, "b");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].to, "a");
        assert_eq!(dependencies(&graph, "a"), vec!["b"]);
    }

    #[test]
    fn reachability_keeps_runtime_dependent_branches_without_a_snapshot() {
        let mut recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "wf".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: std::collections::BTreeMap::new(),
            steps: vec![control_step("inspect"), control_step("conditional")],
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        recipe.graph = Some(transist_protocol::RecipeGraph {
            entry: "inspect".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "inspect".to_string(),
                to: "conditional".to_string(),
                event: "then".to_string(),
                guard: Some("steps.inspect.out.ready == true".to_string()),
                r#loop: None,
                loop_route: None,
            }],
            max_iterations: None,
            loop_routing_version: None,
        });

        assert_eq!(
            reachable_recipe_step_ids(&recipe, None, None).unwrap(),
            BTreeSet::from(["conditional".to_string(), "inspect".to_string()])
        );
    }
}
