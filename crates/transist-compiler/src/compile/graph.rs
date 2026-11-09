// SPDX-License-Identifier: MPL-2.0
use crate::flow::{Flow, FlowIfBlock, FlowLoopBlock, FlowStep};
use std::collections::BTreeSet;
use transist_protocol::{RecipeGraph, RecipeGraphEdge, RecipeLoop};

const EVENT_ON_SUCCESS: &str = "on_success";

pub(crate) fn compile_graph(flow: &Flow) -> RecipeGraph {
    let mut edges = Vec::new();
    let block_members = flow
        .steps
        .iter()
        .flat_map(|step| {
            let mut members = Vec::new();
            if let Some(if_block) = step.if_block.as_ref() {
                members.extend(
                    if_block
                        .then_branch
                        .iter()
                        .map(|member| member.ref_step.clone()),
                );
                members.extend(
                    if_block
                        .else_branch
                        .iter()
                        .map(|member| member.ref_step.clone()),
                );
            }
            if let Some(loop_block) = step.loop_block.as_ref() {
                members.extend(loop_block.body.iter().map(|member| member.ref_step.clone()));
            }
            members
        })
        .collect::<BTreeSet<_>>();

    for (idx, step) in flow.steps.iter().enumerate() {
        if block_members.contains(&step.id) {
            continue;
        }

        if let Some(if_block) = step.if_block.as_ref() {
            compile_if_edges(flow, step, idx, if_block, &mut edges);
            continue;
        }
        if let Some(loop_block) = step.loop_block.as_ref() {
            compile_loop_edges(flow, step, idx, loop_block, &mut edges);
            continue;
        }

        if let Some(next) = flow.steps.get(idx + 1) {
            edges.push(RecipeGraphEdge {
                from: step.id.clone(),
                to: next.id.clone(),
                event: EVENT_ON_SUCCESS.to_string(),
                guard: None,
                r#loop: None,
                loop_route: None,
            });
        }
    }

    RecipeGraph {
        entry: flow.steps.first().expect("validated step").id.clone(),
        edges,
        max_iterations: Some(10_000),
        loop_routing_version: None,
    }
}

fn compile_if_edges(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    if_block: &FlowIfBlock,
    edges: &mut Vec<RecipeGraphEdge>,
) {
    let then_first = if_block
        .then_branch
        .first()
        .map(|entry| entry.ref_step.clone());
    let else_first = if_block
        .else_branch
        .first()
        .map(|entry| entry.ref_step.clone());
    let next_after = flow
        .steps
        .get(step_idx + if_block.then_branch.len() + if_block.else_branch.len() + 1)
        .map(|step| step.id.clone());

    if let Some(first) = then_first {
        edges.push(RecipeGraphEdge {
            from: step.id.clone(),
            to: first,
            event: EVENT_ON_SUCCESS.to_string(),
            guard: Some(if_block.guard.trim().to_string()),
            r#loop: None,
            loop_route: None,
        });
    }
    if let Some(first) = else_first {
        edges.push(RecipeGraphEdge {
            from: step.id.clone(),
            to: first,
            event: EVENT_ON_SUCCESS.to_string(),
            guard: Some(format!("NOT ({})", if_block.guard.trim())),
            r#loop: None,
            loop_route: None,
        });
    } else if let Some(next) = next_after.clone() {
        edges.push(RecipeGraphEdge {
            from: step.id.clone(),
            to: next,
            event: EVENT_ON_SUCCESS.to_string(),
            guard: Some(format!("NOT ({})", if_block.guard.trim())),
            r#loop: None,
            loop_route: None,
        });
    }

    add_linear_branch_edges(
        &if_block
            .then_branch
            .iter()
            .map(|entry| entry.ref_step.clone())
            .collect::<Vec<_>>(),
        next_after.clone(),
        edges,
    );
    add_linear_branch_edges(
        &if_block
            .else_branch
            .iter()
            .map(|entry| entry.ref_step.clone())
            .collect::<Vec<_>>(),
        next_after,
        edges,
    );
}

fn compile_loop_edges(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    loop_block: &FlowLoopBlock,
    edges: &mut Vec<RecipeGraphEdge>,
) {
    let body_ids = loop_block
        .body
        .iter()
        .map(|entry| entry.ref_step.clone())
        .collect::<Vec<_>>();
    let next_after = flow
        .steps
        .get(step_idx + body_ids.len() + 1)
        .map(|step| step.id.clone());
    let loop_guard = loop_block
        .until
        .as_deref()
        .map(|expr| (format!("NOT ({})", expr.trim()), expr.trim().to_string()))
        .or_else(|| {
            loop_block
                .while_expr
                .as_deref()
                .map(|expr| (expr.trim().to_string(), format!("NOT ({})", expr.trim())))
        })
        .expect("validated loop guard");

    if let Some(first) = body_ids.first() {
        edges.push(RecipeGraphEdge {
            from: step.id.clone(),
            to: first.clone(),
            event: EVENT_ON_SUCCESS.to_string(),
            guard: Some(loop_guard.0),
            r#loop: None,
            loop_route: None,
        });
    }
    if let Some(next) = next_after.clone() {
        edges.push(RecipeGraphEdge {
            from: step.id.clone(),
            to: next,
            event: EVENT_ON_SUCCESS.to_string(),
            guard: Some(loop_guard.1),
            r#loop: None,
            loop_route: None,
        });
    }

    add_linear_branch_edges(&body_ids, None, edges);
    if let Some(last) = body_ids.last() {
        edges.push(RecipeGraphEdge {
            from: last.clone(),
            to: step.id.clone(),
            event: EVENT_ON_SUCCESS.to_string(),
            guard: None,
            r#loop: Some(RecipeLoop {
                while_expr: None,
                until_expr: None,
                max_iters: None,
            }),
            loop_route: None,
        });
    }
}

fn add_linear_branch_edges(
    ids: &[String],
    next_after: Option<String>,
    edges: &mut Vec<RecipeGraphEdge>,
) {
    for pair in ids.windows(2) {
        edges.push(RecipeGraphEdge {
            from: pair[0].clone(),
            to: pair[1].clone(),
            event: EVENT_ON_SUCCESS.to_string(),
            guard: None,
            r#loop: None,
            loop_route: None,
        });
    }
    if let (Some(last), Some(next)) = (ids.last(), next_after) {
        edges.push(RecipeGraphEdge {
            from: last.clone(),
            to: next,
            event: EVENT_ON_SUCCESS.to_string(),
            guard: None,
            r#loop: None,
            loop_route: None,
        });
    }
}
