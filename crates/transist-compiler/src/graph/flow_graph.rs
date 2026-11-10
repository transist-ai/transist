// SPDX-License-Identifier: MPL-2.0
use super::{WorkflowEdge, WorkflowEdgeKind};
use crate::flow::{Flow, FlowIfBlock, FlowLoopBlock, FlowStep};
use std::collections::BTreeSet;

/// Derives inspection-ready workflow graph edges from authored flow steps.
#[must_use]
pub fn edges_from(flow: &Flow) -> Vec<WorkflowEdge> {
    let mut edges = Vec::new();
    let block_members = flow_block_member_ids(flow);

    for (idx, step) in flow.steps.iter().enumerate() {
        if block_members.contains(&step.id) {
            continue;
        }

        if let Some(if_block) = step.if_block.as_ref() {
            append_if_edges(flow, step, idx, if_block, &block_members, &mut edges);
            continue;
        }
        if let Some(loop_block) = step.loop_block.as_ref() {
            append_loop_edges(flow, step, idx, loop_block, &block_members, &mut edges);
            continue;
        }

        if let Some(next) = next_top_level_step_id(flow, idx, &block_members) {
            edges.push(WorkflowEdge {
                from_step_id: step.id.clone(),
                target_step_id: next,
                kind: WorkflowEdgeKind::Next,
                guard: None,
                loop_while: None,
                loop_until: None,
                loop_max_iters: None,
                loop_routing_version: None,
            });
        }
    }

    edges
}

fn flow_block_member_ids(flow: &Flow) -> BTreeSet<String> {
    flow.steps
        .iter()
        .flat_map(|step| {
            let mut members = Vec::new();
            if let Some(if_block) = step.if_block.as_ref() {
                members.extend(
                    if_block
                        .then_branch
                        .iter()
                        .map(|entry| entry.ref_step.trim().to_string()),
                );
                members.extend(
                    if_block
                        .else_branch
                        .iter()
                        .map(|entry| entry.ref_step.trim().to_string()),
                );
            }
            if let Some(loop_block) = step.loop_block.as_ref() {
                members.extend(
                    loop_block
                        .body
                        .iter()
                        .map(|entry| entry.ref_step.trim().to_string()),
                );
            }
            members
        })
        .collect()
}

fn next_top_level_step_id(
    flow: &Flow,
    step_idx: usize,
    block_members: &BTreeSet<String>,
) -> Option<String> {
    flow.steps
        .iter()
        .skip(step_idx + 1)
        .find(|step| !block_members.contains(&step.id))
        .map(|step| step.id.clone())
}

fn append_if_edges(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    if_block: &FlowIfBlock,
    block_members: &BTreeSet<String>,
    edges: &mut Vec<WorkflowEdge>,
) {
    let then_ids = if_block
        .then_branch
        .iter()
        .map(|entry| entry.ref_step.trim().to_string())
        .collect::<Vec<_>>();
    let else_ids = if_block
        .else_branch
        .iter()
        .map(|entry| entry.ref_step.trim().to_string())
        .collect::<Vec<_>>();
    let next_after = next_top_level_step_id(flow, step_idx, block_members);
    let guard = if_block.guard.trim().to_string();

    if let Some(first) = then_ids.first() {
        edges.push(WorkflowEdge {
            from_step_id: step.id.clone(),
            target_step_id: first.clone(),
            kind: WorkflowEdgeKind::Then,
            guard: Some(guard.clone()),
            loop_while: None,
            loop_until: None,
            loop_max_iters: None,
            loop_routing_version: None,
        });
    }
    if let Some(first) = else_ids.first() {
        edges.push(WorkflowEdge {
            from_step_id: step.id.clone(),
            target_step_id: first.clone(),
            kind: WorkflowEdgeKind::Else,
            guard: Some(format!("NOT ({guard})")),
            loop_while: None,
            loop_until: None,
            loop_max_iters: None,
            loop_routing_version: None,
        });
    } else if let Some(next) = next_after.clone() {
        edges.push(WorkflowEdge {
            from_step_id: step.id.clone(),
            target_step_id: next,
            kind: WorkflowEdgeKind::Else,
            guard: Some(format!("NOT ({guard})")),
            loop_while: None,
            loop_until: None,
            loop_max_iters: None,
            loop_routing_version: None,
        });
    }

    append_linear_edges(&then_ids, next_after.clone(), edges);
    append_linear_edges(&else_ids, next_after, edges);
}

fn append_loop_edges(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    loop_block: &FlowLoopBlock,
    block_members: &BTreeSet<String>,
    edges: &mut Vec<WorkflowEdge>,
) {
    let body_ids = loop_block
        .body
        .iter()
        .map(|entry| entry.ref_step.trim().to_string())
        .collect::<Vec<_>>();
    let next_after = next_top_level_step_id(flow, step_idx, block_members);
    let loop_while = loop_block
        .while_expr
        .as_ref()
        .map(|value| value.trim().to_string());
    let loop_until = loop_block
        .until
        .as_ref()
        .map(|value| value.trim().to_string());
    let loop_entry_guard = loop_until
        .as_ref()
        .map(|value| format!("NOT ({value})"))
        .or_else(|| loop_while.clone());
    let loop_exit_guard = loop_until
        .clone()
        .or_else(|| loop_while.as_ref().map(|value| format!("NOT ({value})")));

    if let Some(first) = body_ids.first() {
        edges.push(WorkflowEdge {
            from_step_id: step.id.clone(),
            target_step_id: first.clone(),
            kind: WorkflowEdgeKind::Loop,
            guard: loop_entry_guard,
            loop_while: loop_while.clone(),
            loop_until: loop_until.clone(),
            loop_max_iters: Some(loop_block.max_iters),
            loop_routing_version: Some(1),
        });
    }
    if let Some(next) = next_after.clone() {
        edges.push(WorkflowEdge {
            from_step_id: step.id.clone(),
            target_step_id: next,
            kind: WorkflowEdgeKind::Exit,
            guard: loop_exit_guard,
            loop_while: loop_while.clone(),
            loop_until: loop_until.clone(),
            loop_max_iters: Some(loop_block.max_iters),
            loop_routing_version: Some(1),
        });
    }

    append_linear_edges(&body_ids, None, edges);
    if let Some(last) = body_ids.last() {
        edges.push(WorkflowEdge {
            from_step_id: last.clone(),
            target_step_id: step.id.clone(),
            kind: WorkflowEdgeKind::Repeat,
            guard: None,
            loop_while,
            loop_until,
            loop_max_iters: Some(loop_block.max_iters),
            loop_routing_version: None,
        });
    }
}

fn append_linear_edges(ids: &[String], next_after: Option<String>, edges: &mut Vec<WorkflowEdge>) {
    for pair in ids.windows(2) {
        edges.push(WorkflowEdge {
            from_step_id: pair[0].clone(),
            target_step_id: pair[1].clone(),
            kind: WorkflowEdgeKind::Next,
            guard: None,
            loop_while: None,
            loop_until: None,
            loop_max_iters: None,
            loop_routing_version: None,
        });
    }
    if let (Some(last), Some(next)) = (ids.last(), next_after) {
        edges.push(WorkflowEdge {
            from_step_id: last.clone(),
            target_step_id: next,
            kind: WorkflowEdgeKind::Next,
            guard: None,
            loop_while: None,
            loop_until: None,
            loop_max_iters: None,
            loop_routing_version: None,
        });
    }
}
