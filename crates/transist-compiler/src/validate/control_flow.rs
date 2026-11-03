// SPDX-License-Identifier: MPL-2.0
use super::inputs::validate_vars_references_in_expression;
use crate::common::parse_duration_to_ms;
use crate::flow::{Flow, FlowIfBlock, FlowLoopBlock, FlowRef, FlowStep};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate_if_block(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    if_block: &FlowIfBlock,
    step_index: &BTreeMap<String, usize>,
    block_members: &mut BTreeSet<String>,
    known_vars: &BTreeSet<String>,
) -> Result<()> {
    if if_block.guard.trim().is_empty() {
        bail!("flow.steps[{step_idx}].if.guard is required");
    }
    validate_vars_references_in_expression(
        &if_block.guard,
        known_vars,
        &format!("flow.steps[{step_idx}].if.guard"),
    )?;
    if if_block.then_branch.is_empty() {
        bail!("flow.steps[{step_idx}].if.then must contain at least one step reference");
    }

    let mut seen = BTreeSet::new();
    for (offset, branch_ref) in if_block.then_branch.iter().enumerate() {
        validate_block_ref(
            flow,
            step,
            step_idx,
            step_index,
            block_members,
            &mut seen,
            branch_ref,
            offset,
            step_idx + offset + 1,
            "if.then",
        )?;
    }
    for (offset, branch_ref) in if_block.else_branch.iter().enumerate() {
        validate_block_ref(
            flow,
            step,
            step_idx,
            step_index,
            block_members,
            &mut seen,
            branch_ref,
            offset,
            step_idx + if_block.then_branch.len() + offset + 1,
            "if.else",
        )?;
    }
    Ok(())
}

pub(crate) fn validate_loop_block(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    loop_block: &FlowLoopBlock,
    step_index: &BTreeMap<String, usize>,
    block_members: &mut BTreeSet<String>,
    known_vars: &BTreeSet<String>,
) -> Result<()> {
    if loop_block.body.is_empty() {
        bail!("flow.steps[{step_idx}].loop.body must contain at least one step reference");
    }
    let has_while = loop_block
        .while_expr
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let has_until = loop_block
        .until
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    if has_while == has_until {
        bail!(
            "flow.steps[{step_idx}].loop requires exactly one exit condition: 'while' or 'until'"
        );
    }
    // The loop's exit expressions are evaluated after each body iteration,
    // so vars written by body steps are observable. Treat them as known
    // for the purpose of validating `while`/`until`.
    let mut loop_known_vars = known_vars.clone();
    extend_with_body_vars(flow, &loop_block.body, &mut loop_known_vars);
    if let Some(expr) = loop_block.while_expr.as_deref() {
        validate_vars_references_in_expression(
            expr,
            &loop_known_vars,
            &format!("flow.steps[{step_idx}].loop.while"),
        )?;
    }
    if let Some(expr) = loop_block.until.as_deref() {
        validate_vars_references_in_expression(
            expr,
            &loop_known_vars,
            &format!("flow.steps[{step_idx}].loop.until"),
        )?;
    }
    if loop_block.max_iters == 0 {
        bail!("flow.steps[{step_idx}].loop.max_iters must be greater than 0");
    }
    if let Some(value) = loop_block.max_duration.as_deref() {
        parse_duration_to_ms(value)
            .with_context(|| format!("invalid flow.steps[{step_idx}].loop.max_duration"))?;
    }

    let mut seen = BTreeSet::new();
    for (offset, body_ref) in loop_block.body.iter().enumerate() {
        validate_block_ref(
            flow,
            step,
            step_idx,
            step_index,
            block_members,
            &mut seen,
            body_ref,
            offset,
            step_idx + offset + 1,
            "loop.body",
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_block_ref(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    step_index: &BTreeMap<String, usize>,
    block_members: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    branch_ref: &FlowRef,
    branch_offset: usize,
    expected_idx: usize,
    label: &str,
) -> Result<()> {
    let ref_step = branch_ref.ref_step.trim();
    if ref_step.is_empty() {
        bail!("flow.steps[{step_idx}].{label}[{branch_offset}].ref is required");
    }
    if ref_step == step.id {
        bail!(
            "flow.steps[{step_idx}].{label}[{branch_offset}] cannot reference control step '{}'",
            step.id
        );
    }
    if !seen.insert(ref_step.to_string()) {
        bail!(
            "flow.steps[{step_idx}].{label} contains duplicate ref '{}'",
            ref_step
        );
    }
    let referenced_idx = step_index.get(ref_step).copied().ok_or_else(|| {
        anyhow::anyhow!(
            "flow.steps[{step_idx}].{label}[{branch_offset}] references unknown step '{}'",
            ref_step
        )
    })?;
    if referenced_idx != expected_idx {
        bail!(
            "flow.steps[{step_idx}].{label}[{branch_offset}] must reference contiguous step at index {}",
            expected_idx
        );
    }
    let referenced = flow
        .steps
        .get(referenced_idx)
        .ok_or_else(|| anyhow::anyhow!("internal error: missing block member"))?;
    if referenced.if_block.is_some() || referenced.loop_block.is_some() {
        bail!(
            "flow.steps[{step_idx}].{label}[{branch_offset}] cannot reference nested control block '{}'",
            ref_step
        );
    }
    if !block_members.insert(ref_step.to_string()) {
        bail!(
            "step '{}' is already part of another control block body; overlapping control blocks are unsupported",
            ref_step
        );
    }
    Ok(())
}

/// Collects the union of `set` keys declared by each step referenced in
/// `body_refs`. Used to extend the lexically-prior `known_vars` set when
/// validating a loop's `while`/`until` expression, since loop exit
/// expressions are evaluated after each body iteration and therefore can
/// observe vars written by body steps.
fn extend_with_body_vars(flow: &Flow, body_refs: &[FlowRef], known_vars: &mut BTreeSet<String>) {
    for body_ref in body_refs {
        let ref_id = body_ref.ref_step.trim();
        if ref_id.is_empty() {
            continue;
        }
        let Some(body_step) = flow.steps.iter().find(|s| s.id == ref_id) else {
            continue;
        };
        if let Some(set_map) = body_step.set.as_ref() {
            for key in set_map.keys() {
                known_vars.insert(key.clone());
            }
        }
    }
}
