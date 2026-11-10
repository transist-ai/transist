// SPDX-License-Identifier: MPL-2.0
use super::{WorkflowEdge, WorkflowEdgeKind};
use transist_protocol::{KernelValue, Recipe, RecipeLoopRouteRole, RecipeStep};

/// Derives inspection-ready workflow graph edges from a compiled recipe graph.
#[must_use]
pub fn edges_from(recipe: &Recipe) -> Vec<WorkflowEdge> {
    let loop_meta = recipe
        .steps
        .iter()
        .map(|step| (step.id.clone(), recipe_loop_details_from_step(step)))
        .collect::<std::collections::BTreeMap<_, _>>();

    let Some(graph) = recipe.graph.as_ref() else {
        return Vec::new();
    };

    graph
        .edges
        .iter()
        .map(|edge| {
            let marked_route = (graph.loop_routing_version == Some(1))
                .then_some(edge.loop_route.as_ref())
                .flatten();
            // Marked repeat edges carry the target wrapper's inspection limits;
            // retain the historical source-only metadata for legacy graphs.
            let owner = if graph.loop_routing_version == Some(1) && edge.r#loop.is_some() {
                &edge.to
            } else {
                &edge.from
            };
            let meta = loop_meta
                .get(owner)
                .cloned()
                .unwrap_or((None, None, None, None));
            WorkflowEdge {
                from_step_id: edge.from.clone(),
                target_step_id: edge.to.clone(),
                kind: marked_route
                    .map(|route| match route.role {
                        RecipeLoopRouteRole::Enter => WorkflowEdgeKind::Loop,
                        RecipeLoopRouteRole::Exit => WorkflowEdgeKind::Exit,
                    })
                    .unwrap_or_else(|| {
                        classify_recipe_edge_kind(
                            edge.guard.as_deref(),
                            edge.r#loop.is_some(),
                            &meta,
                        )
                    }),
                guard: edge.guard.clone(),
                loop_while: meta.0,
                loop_until: meta.1,
                loop_max_iters: meta.2,
                loop_routing_version: marked_route.map(|_| 1),
            }
        })
        .collect()
}

fn classify_recipe_edge_kind(
    edge_guard: Option<&str>,
    is_repeat_edge: bool,
    loop_meta: &(Option<String>, Option<String>, Option<u32>, Option<String>),
) -> WorkflowEdgeKind {
    if is_repeat_edge {
        return WorkflowEdgeKind::Repeat;
    }

    if loop_meta.0.is_some() || loop_meta.1.is_some() {
        let positive = loop_meta
            .1
            .as_ref()
            .map(|value| format!("NOT ({value})"))
            .or_else(|| loop_meta.0.clone());
        let negative = loop_meta
            .1
            .clone()
            .or_else(|| loop_meta.0.as_ref().map(|value| format!("NOT ({value})")));
        let edge_guard = edge_guard.map(str::to_string);
        if edge_guard == positive {
            return WorkflowEdgeKind::Loop;
        }
        if edge_guard == negative {
            return WorkflowEdgeKind::Exit;
        }
    }

    match edge_guard {
        Some(guard) if guard.trim_start().starts_with("NOT (") => WorkflowEdgeKind::Else,
        Some(_) => WorkflowEdgeKind::Then,
        None => WorkflowEdgeKind::Next,
    }
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
