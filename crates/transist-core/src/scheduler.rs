// SPDX-License-Identifier: MPL-2.0
use crate::execution_graph::{ExecutionGraph, GraphEdge, dependencies};
use crate::gate_state::{
    allows_transition, evaluate_guard_expression, evaluate_loop_route_guard_expression,
};
use crate::types::{KernelRunSnapshot, KernelStepState, KernelStepStatus};
use std::collections::BTreeMap;
use transist_protocol::KernelValue;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IncomingEdgeReachability {
    pub(crate) has_satisfied_edge: bool,
    pub(crate) has_pending_edge: bool,
    pub(crate) has_guard_blocked_edge: bool,
    pub(crate) has_dependency_failed_edge: bool,
}

/// Returns the sorted runnable frontier based on dependency completion and retry/gate state.
pub fn runnable_frontier(
    graph: &ExecutionGraph,
    snapshot: &KernelRunSnapshot,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
    now_ms: u64,
) -> Vec<String> {
    let mut runnable = Vec::new();
    for (step_id, step_state) in &snapshot.steps {
        if matches!(
            step_state.status,
            KernelStepStatus::Skipped | KernelStepStatus::Canceled
        ) {
            continue;
        }
        if step_state.status == KernelStepStatus::RetryWait {
            if let Some(wait_until) = step_state.retry_wait_until_ms {
                if wait_until > now_ms {
                    continue;
                }
            }
        } else if !matches!(
            step_state.status,
            KernelStepStatus::Pending | KernelStepStatus::Runnable
        ) {
            continue;
        }

        let deps = dependencies(graph, step_id);
        if step_id == &graph.entry
            && step_state.activation_executions() == 0
            && has_only_unexecuted_dependencies(snapshot, &deps)
        {
            runnable.push(step_id.clone());
            continue;
        }
        if deps.is_empty() {
            runnable.push(step_id.clone());
            continue;
        }
        let reachability =
            incoming_edge_reachability(graph, snapshot, workflow_inputs, workflow_vars, step_id);
        if reachability.has_satisfied_edge {
            runnable.push(step_id.clone());
        }
    }

    runnable.sort();
    runnable
}

fn has_only_unexecuted_dependencies(snapshot: &KernelRunSnapshot, deps: &[&str]) -> bool {
    !deps.is_empty()
        && deps.iter().all(|dep| {
            snapshot
                .steps
                .get(*dep)
                .map(|state| state.activation_executions() == 0)
                .unwrap_or(false)
        })
}

pub(crate) fn incoming_edge_reachability(
    graph: &ExecutionGraph,
    snapshot: &KernelRunSnapshot,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
    step_id: &str,
) -> IncomingEdgeReachability {
    let mut reachability = IncomingEdgeReachability::default();
    for dep in dependencies(graph, step_id) {
        let Some(dep_state) = snapshot.steps.get(dep) else {
            reachability.has_pending_edge = true;
            continue;
        };
        let Some(dep_node) = graph.nodes.get(dep) else {
            reachability.has_pending_edge = true;
            continue;
        };
        for edge in dep_node.outgoing.iter().filter(|edge| edge.to == step_id) {
            match edge_reachability(
                edge,
                dep_node
                    .loop_max_iters
                    .filter(|_| graph.loop_routing_version == Some(1)),
                &dep_node.loop_body_steps,
                dep_state,
                snapshot,
                workflow_inputs,
                workflow_vars,
            ) {
                EdgeReachability::Satisfied => reachability.has_satisfied_edge = true,
                EdgeReachability::Pending => reachability.has_pending_edge = true,
                EdgeReachability::GuardBlocked => reachability.has_guard_blocked_edge = true,
                EdgeReachability::DependencyFailed => {
                    reachability.has_dependency_failed_edge = true;
                }
            }
        }
    }
    reachability
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeReachability {
    Satisfied,
    Pending,
    GuardBlocked,
    DependencyFailed,
}

fn edge_reachability(
    edge: &GraphEdge,
    loop_max_iters: Option<u32>,
    loop_body_steps: &std::collections::BTreeSet<String>,
    dep_state: &KernelStepState,
    snapshot: &KernelRunSnapshot,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
) -> EdgeReachability {
    match edge.event.as_str() {
        "on_success" => match dep_state.status {
            KernelStepStatus::Succeeded => {
                // Only cap exhaustion overrides guards. Below the cap, use the
                // current scope's guard evaluation, including fail-closed unknowns.
                if let (Some(route), Some(cap)) = (&edge.loop_route, loop_max_iters) {
                    if dep_state.activation_executions() >= cap {
                        return match route.role {
                            transist_protocol::RecipeLoopRouteRole::Enter => {
                                EdgeReachability::GuardBlocked
                            }
                            transist_protocol::RecipeLoopRouteRole::Exit => {
                                EdgeReachability::Satisfied
                            }
                        };
                    }
                }
                let gate = if edge.loop_route.is_some() && loop_max_iters.is_some() {
                    evaluate_loop_route_guard_expression(
                        edge.guard.as_deref(),
                        dep_state,
                        snapshot,
                        workflow_inputs,
                        workflow_vars,
                        loop_body_steps,
                    )
                } else {
                    evaluate_guard_expression(
                        edge.guard.as_deref(),
                        Some(dep_state),
                        Some(snapshot),
                        Some(workflow_inputs),
                        Some(workflow_vars),
                    )
                };
                if allows_transition(gate) {
                    EdgeReachability::Satisfied
                } else {
                    EdgeReachability::GuardBlocked
                }
            }
            KernelStepStatus::Skipped
                if dep_state.skip_reason == Some(crate::types::KernelSkipReason::GuardBlocked) =>
            {
                EdgeReachability::GuardBlocked
            }
            KernelStepStatus::Failed | KernelStepStatus::Canceled | KernelStepStatus::Skipped => {
                EdgeReachability::DependencyFailed
            }
            KernelStepStatus::Pending
            | KernelStepStatus::Runnable
            | KernelStepStatus::Dispatching
            | KernelStepStatus::Running
            | KernelStepStatus::Blocked
            | KernelStepStatus::RetryWait => EdgeReachability::Pending,
        },
        _ => EdgeReachability::DependencyFailed,
    }
}

#[cfg(test)]
#[path = "scheduler_loop_cap_routing_tests.rs"]
mod loop_cap_routing_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_graph::build_execution_graph;
    use crate::types::KernelSkipReason;
    use transist_protocol::{
        Recipe, RecipeGraph, RecipeGraphEdge, RecipeStep, StepExecutionDescriptor, WorkflowRef,
    };

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

    fn graph_recipe() -> Recipe {
        Recipe {
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
                entry: "a".to_string(),
                edges: vec![RecipeGraphEdge {
                    from: "a".to_string(),
                    to: "b".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("true".to_string()),
                    r#loop: None,
                    loop_route: None,
                }],
                max_iterations: None,
                loop_routing_version: None,
            }),
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    #[test]
    fn runnable_frontier_requires_dependencies_to_be_terminal() {
        let recipe = graph_recipe();
        let graph = build_execution_graph(&recipe).unwrap();
        let mut snapshot =
            KernelRunSnapshot::new("run", "wf", "v1", &["a".to_string(), "b".to_string()]);
        if let Some(step) = snapshot.steps.get_mut("a") {
            step.status = KernelStepStatus::Runnable;
        }

        let frontier = runnable_frontier(
            &graph,
            &snapshot,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            0,
        );
        assert_eq!(frontier, vec!["a".to_string()]);

        if let Some(step) = snapshot.steps.get_mut("a") {
            step.status = KernelStepStatus::Succeeded;
        }
        if let Some(step) = snapshot.steps.get_mut("b") {
            step.status = KernelStepStatus::Runnable;
        }
        let frontier = runnable_frontier(
            &graph,
            &snapshot,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            0,
        );
        assert_eq!(frontier, vec!["b".to_string()]);
    }

    #[test]
    fn runnable_frontier_blocks_retry_wait_before_deadline() {
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
            steps: vec![control_step("a")],
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        let graph = build_execution_graph(&recipe).unwrap();
        let mut snapshot = KernelRunSnapshot::new("run", "wf", "v1", &["a".to_string()]);
        snapshot.steps.insert(
            "a".to_string(),
            KernelStepState {
                step_id: "a".to_string(),
                status: KernelStepStatus::RetryWait,
                executions: 1,
                retry_wait_until_ms: Some(200),
                skip_reason: None,
                last_request: None,
                last_result: None,
                started_at_ms: None,
                finished_at_ms: None,
                activation_executions_baseline: 0,
            },
        );
        assert!(
            runnable_frontier(
                &graph,
                &snapshot,
                &std::collections::BTreeMap::new(),
                &std::collections::BTreeMap::new(),
                100
            )
            .is_empty()
        );
        let frontier = runnable_frontier(
            &graph,
            &snapshot,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            200,
        );
        assert_eq!(frontier, vec!["a".to_string()]);
    }

    #[test]
    fn runnable_frontier_blocks_steps_after_dependency_failed_skip() {
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
        let mut snapshot = KernelRunSnapshot::new(
            "run",
            "wf",
            "v1",
            &["a".to_string(), "b".to_string(), "c".to_string()],
        );
        if let Some(step) = snapshot.steps.get_mut("a") {
            step.status = KernelStepStatus::Succeeded;
        }
        if let Some(step) = snapshot.steps.get_mut("b") {
            step.status = KernelStepStatus::Skipped;
            step.skip_reason = Some(KernelSkipReason::DependencyFailed);
        }
        if let Some(step) = snapshot.steps.get_mut("c") {
            step.status = KernelStepStatus::Pending;
        }

        let frontier = runnable_frontier(
            &graph,
            &snapshot,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            0,
        );
        assert!(frontier.is_empty());
    }

    #[test]
    fn runnable_frontier_allows_entry_before_loop_back_edge_has_run() {
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
            steps: vec![
                control_step("entry"),
                control_step("generate"),
                control_step("review"),
            ],
            cleanup: Vec::new(),
            graph: Some(RecipeGraph {
                entry: "entry".to_string(),
                edges: vec![
                    RecipeGraphEdge {
                        from: "entry".to_string(),
                        to: "generate".to_string(),
                        event: "on_success".to_string(),
                        guard: None,
                        r#loop: None,
                        loop_route: None,
                    },
                    RecipeGraphEdge {
                        from: "generate".to_string(),
                        to: "review".to_string(),
                        event: "on_success".to_string(),
                        guard: None,
                        r#loop: None,
                        loop_route: None,
                    },
                    RecipeGraphEdge {
                        from: "review".to_string(),
                        to: "entry".to_string(),
                        event: "on_success".to_string(),
                        guard: None,
                        r#loop: None,
                        loop_route: None,
                    },
                ],
                max_iterations: None,
                loop_routing_version: None,
            }),
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        let graph = build_execution_graph(&recipe).unwrap();
        let mut snapshot = KernelRunSnapshot::new(
            "run",
            "wf",
            "v1",
            &[
                "entry".to_string(),
                "generate".to_string(),
                "review".to_string(),
            ],
        );
        if let Some(step) = snapshot.steps.get_mut("entry") {
            step.status = KernelStepStatus::Runnable;
        }

        let frontier = runnable_frontier(
            &graph,
            &snapshot,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            0,
        );
        assert_eq!(frontier, vec!["entry".to_string()]);

        if let Some(step) = snapshot.steps.get_mut("review") {
            step.executions = 1;
        }
        let frontier = runnable_frontier(
            &graph,
            &snapshot,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            0,
        );
        assert!(frontier.is_empty());
    }

    #[test]
    fn runnable_frontier_allows_join_after_taken_branch_succeeds() {
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
            steps: vec![
                control_step("route"),
                control_step("then_step"),
                control_step("else_step"),
                control_step("finish"),
            ],
            cleanup: Vec::new(),
            graph: Some(RecipeGraph {
                entry: "route".to_string(),
                edges: vec![
                    RecipeGraphEdge {
                        from: "route".to_string(),
                        to: "then_step".to_string(),
                        event: "on_success".to_string(),
                        guard: Some("true".to_string()),
                        r#loop: None,
                        loop_route: None,
                    },
                    RecipeGraphEdge {
                        from: "route".to_string(),
                        to: "else_step".to_string(),
                        event: "on_success".to_string(),
                        guard: Some("false".to_string()),
                        r#loop: None,
                        loop_route: None,
                    },
                    RecipeGraphEdge {
                        from: "then_step".to_string(),
                        to: "finish".to_string(),
                        event: "on_success".to_string(),
                        guard: None,
                        r#loop: None,
                        loop_route: None,
                    },
                    RecipeGraphEdge {
                        from: "else_step".to_string(),
                        to: "finish".to_string(),
                        event: "on_success".to_string(),
                        guard: None,
                        r#loop: None,
                        loop_route: None,
                    },
                ],
                max_iterations: None,
                loop_routing_version: None,
            }),
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        let graph = build_execution_graph(&recipe).unwrap();
        let mut snapshot = KernelRunSnapshot::new(
            "run",
            "wf",
            "v1",
            &[
                "route".to_string(),
                "then_step".to_string(),
                "else_step".to_string(),
                "finish".to_string(),
            ],
        );
        if let Some(step) = snapshot.steps.get_mut("route") {
            step.status = KernelStepStatus::Succeeded;
        }
        if let Some(step) = snapshot.steps.get_mut("then_step") {
            step.status = KernelStepStatus::Succeeded;
        }
        if let Some(step) = snapshot.steps.get_mut("else_step") {
            step.status = KernelStepStatus::Skipped;
            step.skip_reason = Some(KernelSkipReason::GuardBlocked);
        }

        let frontier = runnable_frontier(
            &graph,
            &snapshot,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            0,
        );
        assert_eq!(frontier, vec!["finish".to_string()]);
    }
}
