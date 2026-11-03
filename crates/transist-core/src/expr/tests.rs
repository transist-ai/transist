// SPDX-License-Identifier: MPL-2.0
use super::{EvaluationContext, Expression, Value};
use crate::types::{KernelRunSnapshot, KernelStepState, KernelStepStatus};
use std::collections::BTreeMap;
use transist_protocol::KernelValue;

fn state(executions: u32, status: KernelStepStatus) -> KernelStepState {
    let mut step = KernelStepState::pending("step");
    step.executions = executions;
    step.status = status;
    step
}

fn evaluate(expr: &str, ctx: &EvaluationContext<'_>) -> Value {
    let parsed = Expression::parse(expr).expect("parse expression");
    parsed.evaluate(ctx).expect("evaluate expression")
}

#[test]
fn supports_execution_predicates() {
    let dep = state(2, KernelStepStatus::Succeeded);
    let ctx = EvaluationContext::new(Some(&dep), None, None, None);
    assert_eq!(evaluate("executions < 3", &ctx), Value::Bool(true));
    assert_eq!(evaluate("executions > 3", &ctx), Value::Bool(false));
}

#[test]
fn supports_status_predicates() {
    let dep = state(1, KernelStepStatus::Succeeded);
    let ctx = EvaluationContext::new(Some(&dep), None, None, None);
    assert_eq!(evaluate("status == succeeded", &ctx), Value::Bool(true));
    assert_eq!(evaluate("status == failed", &ctx), Value::Bool(false));
}

#[test]
fn supports_inputs_and_step_outputs() {
    let mut snapshot =
        KernelRunSnapshot::new("run", "wf", "v1", &["review".to_string(), "pr".to_string()]);
    let mut review = state(1, KernelStepStatus::Succeeded);
    review.last_result = Some(transist_protocol::ExecutionResult {
        message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_review".to_string(),
        run_id: "run".to_string(),
        step_id: "review".to_string(),
        execution: 1,
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
        metrics: BTreeMap::new(),
        output: serde_json::json!({ "decision": "approved" }),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    });
    let mut pr = state(1, KernelStepStatus::Succeeded);
    pr.last_result = Some(transist_protocol::ExecutionResult {
        message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_pr".to_string(),
        run_id: "run".to_string(),
        step_id: "pr".to_string(),
        execution: 1,
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
        metrics: BTreeMap::new(),
        output: serde_json::json!({ "pull_request": { "state": "MERGED" }, "count": 2 }),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    });
    snapshot.steps.insert("review".to_string(), review.clone());
    snapshot.steps.insert("pr".to_string(), pr);
    let inputs = BTreeMap::from([("dry_run".to_string(), KernelValue::Bool(false))]);
    let ctx = EvaluationContext::new(Some(&review), Some(&snapshot), Some(&inputs), None);

    assert_eq!(
        evaluate("steps.review.out.decision == \"approved\"", &ctx),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate("steps.pr.out.pull_request.state == \"MERGED\"", &ctx),
        Value::Bool(true)
    );
    assert_eq!(evaluate("steps.pr.out.count > 1", &ctx), Value::Bool(true));
    assert_eq!(
        evaluate("NOT (inputs.dry_run == true)", &ctx),
        Value::Bool(true)
    );
}

#[test]
fn supports_short_circuit_logical_operators() {
    let mut snapshot =
        KernelRunSnapshot::new("run", "wf", "v1", &["review".to_string(), "pr".to_string()]);
    let mut review = state(1, KernelStepStatus::Succeeded);
    review.last_result = Some(transist_protocol::ExecutionResult {
        message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_review".to_string(),
        run_id: "run".to_string(),
        step_id: "review".to_string(),
        execution: 1,
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
        metrics: BTreeMap::new(),
        output: serde_json::json!({ "decision": "approved" }),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    });
    let mut pr = state(1, KernelStepStatus::Succeeded);
    pr.last_result = Some(transist_protocol::ExecutionResult {
        message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_pr".to_string(),
        run_id: "run".to_string(),
        step_id: "pr".to_string(),
        execution: 1,
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
        metrics: BTreeMap::new(),
        output: serde_json::json!({ "pull_request": { "state": "OPEN" } }),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    });
    snapshot.steps.insert("review".to_string(), review.clone());
    snapshot.steps.insert("pr".to_string(), pr);
    let ctx = EvaluationContext::new(Some(&review), Some(&snapshot), None, None);

    assert_eq!(
        evaluate(
            "steps.review.out.decision == \"approved\" && steps.pr.out.pull_request.state != \"MERGED\"",
            &ctx,
        ),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate(
            "steps.review.out.decision != \"approved\" && steps.missing.out.value == 1",
            &ctx,
        ),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate(
            "steps.review.out.decision == \"approved\" || steps.missing.out.value == 1",
            &ctx,
        ),
        Value::Bool(true)
    );
}
