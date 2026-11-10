// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, BTreeSet};
use transist_protocol::{Recipe, RecipeGraphEdge, RecipeLoop};

#[derive(Debug, Clone)]
pub struct GraphEdge {
    /// Explicit compiler-owned bounded-loop route, absent for legacy edges.
    pub loop_route: Option<transist_protocol::RecipeLoopRoute>,
    pub to: String,
    pub event: String,
    pub guard: Option<String>,
    pub r#loop: Option<RecipeLoop>,
}

#[derive(Debug, Clone)]
pub struct StepNode {
    /// Declared body identities whose initial/reset/suspended business output is null.
    pub loop_body_steps: BTreeSet<String>,
    /// Validated wrapper cap, used only for explicitly versioned loop routes.
    pub loop_max_iters: Option<u32>,
    pub dependencies: BTreeSet<String>,
    pub outgoing: Vec<GraphEdge>,
}

#[derive(Debug, Clone)]
/// Runtime dependency graph derived from workflow recipe topology.
pub struct ExecutionGraph {
    /// Opt-in bounded-loop routing revision from the persisted recipe.
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
                loop_body_steps: match step.with.get("__transist_loop") {
                    Some(transist_protocol::KernelValue::Object(meta)) => {
                        match meta.get("body_steps") {
                            Some(transist_protocol::KernelValue::Array(body)) => body
                                .iter()
                                .filter_map(|value| match value {
                                    transist_protocol::KernelValue::String(id) => Some(id.clone()),
                                    _ => None,
                                })
                                .collect(),
                            _ => BTreeSet::new(),
                        }
                    }
                    _ => BTreeSet::new(),
                },
                loop_max_iters: match step.with.get("__transist_loop") {
                    Some(transist_protocol::KernelValue::Object(meta)) => {
                        meta.get("max_iters").and_then(|value| match value {
                            transist_protocol::KernelValue::Number(n) => {
                                n.as_u64().and_then(|n| u32::try_from(n).ok())
                            }
                            _ => None,
                        })
                    }
                    _ => None,
                },
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
/// Marked normal exits are always potential paths because cap exhaustion can
/// satisfy them independently of their current predicate.
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
/// Marked bounded-loop exits also remain potential paths regardless of predicate.
pub(crate) fn reachable_steps_in_scope(
    graph: &ExecutionGraph,
    snapshot: Option<&crate::types::KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, transist_protocol::KernelValue>>,
    deferred_inputs: &BTreeSet<String>,
) -> BTreeSet<String> {
    use crate::gate_state::{
        GateState, evaluate_guard_expression_in_scope,
        evaluate_loop_route_guard_expression_in_scope,
    };
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
            let marked_route = (graph.loop_routing_version == Some(1))
                .then_some(edge.loop_route.as_ref())
                .flatten();
            // This is prospective validation, not authorization: a successful
            // bounded wrapper can eventually exit at its cap even if its
            // predicate is currently false (or its current activation failed).
            if marked_route
                .is_some_and(|route| route.role == transist_protocol::RecipeLoopRouteRole::Exit)
            {
                stack.push(edge.to.clone());
                continue;
            }
            let dependency_state = snapshot.and_then(|snap| snap.steps.get(&current));
            let gate = if snapshot.is_none()
                && edge
                    .guard
                    .as_deref()
                    .is_some_and(guard_depends_on_runtime_state)
            {
                GateState::NotEvaluated
            } else if marked_route.is_some() {
                evaluate_loop_route_guard_expression_in_scope(
                    edge.guard.as_deref(),
                    dependency_state,
                    snapshot,
                    workflow_inputs,
                    None,
                    deferred_inputs,
                    None,
                )
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

    fn prospective_loop_recipe(until: bool, predicate: &str, cap: u32, marked: bool) -> Recipe {
        let mut recipe: Recipe =
            serde_json::from_str(include_str!("../tests/loop_cap_routing/legacy.recipe.json"))
                .unwrap();
        recipe.steps[0].with.insert("__transist_loop".into(), serde_json::json!({
            "body_steps":["body"], "max_iters":cap, (if until {"until"} else {"while"}):predicate
        }).into());
        let graph = recipe.graph.as_mut().unwrap();
        graph.edges[0].guard = Some(if until {
            format!("NOT ({predicate})")
        } else {
            predicate.into()
        });
        graph.edges[1].guard = Some(if until {
            predicate.into()
        } else {
            format!("NOT ({predicate})")
        });
        if marked {
            graph.loop_routing_version = Some(1);
            for (edge, role) in graph.edges.iter_mut().take(2).zip([
                transist_protocol::RecipeLoopRouteRole::Enter,
                transist_protocol::RecipeLoopRouteRole::Exit,
            ]) {
                edge.loop_route = Some(transist_protocol::RecipeLoopRoute {
                    owner_step_id: "loop".into(),
                    role,
                });
            }
        }
        recipe
    }

    #[test]
    fn loop_cap_routing_prospective_exit_survives_predicates_and_legacy_stays_unchanged() {
        for cap in [1, 2] {
            for (until, predicate, body) in [
                (true, "false", true),
                (false, "true", true),
                (true, "true", false),
                (false, "false", false),
            ] {
                for marked in [false, true] {
                    let recipe = prospective_loop_recipe(until, predicate, cap, marked);
                    let reached = reachable_recipe_step_ids(&recipe, None, None).unwrap();
                    assert_eq!(reached.contains("body"), body);
                    assert_eq!(reached.contains("mandatory_successor"), marked || !body);
                    assert_eq!(reached.contains("finish"), marked || !body);
                }
            }
        }
    }

    #[test]
    fn loop_cap_routing_prospective_unknowns_are_potential_paths_not_permissions() {
        for until in [false, true] {
            for predicate in [
                "inputs.missing == true",
                "steps.undeclared.out.missing == true",
                "vars.missing",
                "(",
            ] {
                let recipe = prospective_loop_recipe(until, predicate, 2, true);
                let graph = build_execution_graph(&recipe).unwrap();
                let mut snapshot = crate::types::KernelRunSnapshot::new(
                    "run",
                    "frozen_loop_cap",
                    "v1",
                    &recipe
                        .steps
                        .iter()
                        .map(|s| s.id.clone())
                        .collect::<Vec<_>>(),
                );
                let owner = snapshot.steps.get_mut("loop").unwrap();
                owner.status = crate::types::KernelStepStatus::Succeeded;
                owner.executions = 1;
                for state in [None, Some(&snapshot)] {
                    let reached = reachable_steps(&graph, state, None);
                    assert!(reached.contains("body"), "{predicate}");
                    assert!(reached.contains("mandatory_successor"), "{predicate}");
                }
                for target in ["body", "mandatory_successor"] {
                    assert!(
                        !crate::scheduler::incoming_edge_reachability(
                            &graph,
                            &snapshot,
                            &BTreeMap::new(),
                            &BTreeMap::new(),
                            target
                        )
                        .has_satisfied_edge
                    );
                }
                // Prospective traversal remains conservative even during a
                // failed/canceled activation; it never changes execution gates.
                for status in [
                    crate::types::KernelStepStatus::Failed,
                    crate::types::KernelStepStatus::Canceled,
                ] {
                    snapshot.steps.get_mut("loop").unwrap().status = status;
                    assert!(
                        reachable_steps(&graph, Some(&snapshot), None)
                            .contains("mandatory_successor")
                    );
                    assert!(
                        !crate::scheduler::incoming_edge_reachability(
                            &graph,
                            &snapshot,
                            &BTreeMap::new(),
                            &BTreeMap::new(),
                            "mandatory_successor"
                        )
                        .has_satisfied_edge
                    );
                }
            }
        }
    }

    #[test]
    fn loop_cap_routing_prospective_deferred_inputs_and_ordinary_false_edges_keep_scope() {
        let mut recipe = prospective_loop_recipe(true, "inputs.future == true", 2, true);
        recipe.graph.as_mut().unwrap().edges[3].guard = Some("false".into());
        let inputs =
            BTreeMap::from([("future".into(), transist_protocol::KernelValue::Bool(true))]);
        let concrete = reachable_recipe_step_ids(&recipe, None, Some(&inputs)).unwrap();
        assert!(!concrete.contains("body"));
        let deferred = reachable_recipe_step_ids_in_scope(
            &recipe,
            None,
            Some(&inputs),
            &BTreeSet::from(["future".into()]),
        )
        .unwrap();
        assert!(deferred.contains("body"));
        assert!(deferred.contains("mandatory_successor"));
        assert!(
            !deferred.contains("finish"),
            "ordinary false guards must still prune"
        );
        for (predicate, body) in [
            ("true || inputs.future", false),
            ("false && inputs.future", true),
        ] {
            let recipe = prospective_loop_recipe(true, predicate, 2, true);
            let reached = reachable_recipe_step_ids_in_scope(
                &recipe,
                None,
                None,
                &BTreeSet::from(["future".into()]),
            )
            .unwrap();
            assert_eq!(reached.contains("body"), body);
            assert!(reached.contains("finish"));
        }
    }

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
        assert_eq!(marked.nodes["loop"].loop_max_iters, Some(2));
        assert_eq!(marked.nodes["body"].loop_max_iters, None);
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
                    loop_body_steps: BTreeSet::new(),
                    loop_max_iters: None,
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
