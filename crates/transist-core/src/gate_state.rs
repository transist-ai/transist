// SPDX-License-Identifier: MPL-2.0
use crate::expr::{EvaluationContext, Expression};
use crate::types::{KernelRunSnapshot, KernelStepState};
use std::collections::BTreeMap;
use transist_protocol::{KernelValue, RecipeLoop};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Decision outcome for guard and loop condition evaluation.
pub enum GateState {
    /// The guard could not be parsed or evaluated deterministically.
    NotEvaluated,
    /// The guard evaluated to false.
    ConditionFalse,
    /// The guard evaluated to true.
    ConditionTrue,
    /// Loop should continue with the next iteration number.
    LoopActive {
        /// Next loop iteration index to execute.
        iteration: u32,
    },
    /// Loop reached a completion condition.
    LoopCompleted,
    /// Loop halted because its `while` condition became false.
    LoopAborted,
}

/// Evaluates a guard expression against dependency step state.
pub fn evaluate_guard_expression(
    guard: Option<&str>,
    dependency_state: Option<&KernelStepState>,
    snapshot: Option<&KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, KernelValue>>,
    workflow_vars: Option<&BTreeMap<String, KernelValue>>,
) -> GateState {
    evaluate_guard_expression_in_scope(
        guard,
        dependency_state,
        snapshot,
        workflow_inputs,
        workflow_vars,
        &Default::default(),
    )
}

/// Evaluates a prospective guard with symbolic module inputs kept as unknown operands.
pub(crate) fn evaluate_guard_expression_in_scope(
    guard: Option<&str>,
    dependency_state: Option<&KernelStepState>,
    snapshot: Option<&KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, KernelValue>>,
    workflow_vars: Option<&BTreeMap<String, KernelValue>>,
    deferred_inputs: &std::collections::BTreeSet<String>,
) -> GateState {
    let mut ctx =
        EvaluationContext::new(dependency_state, snapshot, workflow_inputs, workflow_vars);
    ctx.deferred_inputs = Some(deferred_inputs);
    evaluate_guard_in_context(guard, &ctx)
}

/// Evaluates a marked loop edge's guard against current state, keeping missing
/// references unknown rather than using the legacy unquoted-literal fallback.
/// Declared body outputs with an initial/reset/suspended projection are known null;
/// this never substitutes a historical result or approval.
pub(crate) fn evaluate_loop_route_guard_expression(
    guard: Option<&str>,
    dependency_state: &KernelStepState,
    snapshot: &KernelRunSnapshot,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
    loop_body_steps: &std::collections::BTreeSet<String>,
) -> GateState {
    evaluate_loop_route_guard_expression_in_scope(
        guard,
        Some(dependency_state),
        Some(snapshot),
        Some(workflow_inputs),
        Some(workflow_vars),
        &Default::default(),
        Some(loop_body_steps),
    )
}

/// Evaluates a marked guard prospectively, preserving symbolic and missing
/// references as unknown. This produces no execution permission by itself.
pub(crate) fn evaluate_loop_route_guard_expression_in_scope(
    guard: Option<&str>,
    dependency_state: Option<&KernelStepState>,
    snapshot: Option<&KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, KernelValue>>,
    workflow_vars: Option<&BTreeMap<String, KernelValue>>,
    deferred_inputs: &std::collections::BTreeSet<String>,
    empty_loop_body_outputs: Option<&std::collections::BTreeSet<String>>,
) -> GateState {
    let mut ctx =
        EvaluationContext::new(dependency_state, snapshot, workflow_inputs, workflow_vars);
    ctx.deferred_inputs = Some(deferred_inputs);
    ctx.strict_operands = true;
    ctx.empty_loop_body_outputs = empty_loop_body_outputs;
    evaluate_guard_in_context(guard, &ctx)
}

fn evaluate_guard_in_context(guard: Option<&str>, ctx: &EvaluationContext<'_>) -> GateState {
    let Some(expr) = guard.map(str::trim).filter(|expr| !expr.is_empty()) else {
        return GateState::ConditionTrue;
    };

    if expr.eq_ignore_ascii_case("true") {
        return GateState::ConditionTrue;
    }
    if expr.eq_ignore_ascii_case("false") {
        return GateState::ConditionFalse;
    }

    let parsed = match Expression::parse(expr) {
        Ok(value) => value,
        Err(_) => return GateState::NotEvaluated,
    };
    match parsed.evaluate(ctx) {
        Ok(serde_json::Value::Bool(true)) => GateState::ConditionTrue,
        Ok(serde_json::Value::Bool(false)) => GateState::ConditionFalse,
        _ => GateState::NotEvaluated,
    }
}

/// Evaluates loop metadata using the current step state.
pub fn evaluate_loop_state(
    loop_cfg: &RecipeLoop,
    step_state: &KernelStepState,
    snapshot: Option<&KernelRunSnapshot>,
    workflow_inputs: Option<&BTreeMap<String, KernelValue>>,
    workflow_vars: Option<&BTreeMap<String, KernelValue>>,
) -> GateState {
    let activation_executions = step_state.activation_executions();
    if let Some(max_iters) = loop_cfg.max_iters {
        if activation_executions >= max_iters {
            return GateState::LoopCompleted;
        }
    }

    if let Some(until_expr) = loop_cfg.until_expr.as_deref() {
        if allows_transition(evaluate_guard_expression(
            Some(until_expr),
            Some(step_state),
            snapshot,
            workflow_inputs,
            workflow_vars,
        )) {
            return GateState::LoopCompleted;
        }
    }

    if let Some(while_expr) = loop_cfg.while_expr.as_deref() {
        if !allows_transition(evaluate_guard_expression(
            Some(while_expr),
            Some(step_state),
            snapshot,
            workflow_inputs,
            workflow_vars,
        )) {
            return GateState::LoopAborted;
        }
    }

    GateState::LoopActive {
        iteration: activation_executions.saturating_add(1),
    }
}

/// Returns whether the evaluated gate state allows downstream transition.
pub fn allows_transition(state: GateState) -> bool {
    matches!(
        state,
        GateState::ConditionTrue | GateState::LoopActive { .. } | GateState::LoopCompleted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::KernelStepStatus;

    fn state(executions: u32, status: KernelStepStatus) -> KernelStepState {
        let mut step = KernelStepState::pending("step");
        step.executions = executions;
        step.status = status;
        step
    }

    #[test]
    fn evaluate_loop_state_applies_max_iters() {
        let mut step = state(3, KernelStepStatus::Succeeded);
        step.last_result = Some(transist_protocol::ExecutionResult {
            message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke".to_string(),
            run_id: "run".to_string(),
            step_id: "step".to_string(),
            execution: 3,
            status: "success".to_string(),
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timing: transist_protocol::InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "0".to_string(),
                duration_ms: 0,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: std::collections::BTreeMap::new(),
            output: serde_json::json!({}),
            metrics_validation_errors: 0,
            usage: None,
            error: None,
        });
        let loop_cfg = RecipeLoop {
            while_expr: None,
            until_expr: None,
            max_iters: Some(3),
        };
        assert_eq!(
            evaluate_loop_state(&loop_cfg, &step, None, None, None),
            GateState::LoopCompleted
        );
    }
}
