// SPDX-License-Identifier: MPL-2.0
//! Failure-result constructors and error normalization helpers.

use super::*;

pub(crate) fn policy_denied_result(
    request: &transist_protocol::ExecutionRequest,
    message: Option<String>,
) -> ExecutionResult {
    policy_denied_result_with_code(request, message, CanonicalErrorCode::PolicyDenied)
}

pub(crate) fn policy_denied_result_with_code(
    request: &transist_protocol::ExecutionRequest,
    message: Option<String>,
    code: CanonicalErrorCode,
) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        execution: request.step.execution,
        status: "failed".to_string(),
        exit_code: Some(2),
        stdout: String::new(),
        stderr: "policy denied".to_string(),
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError {
            code,
            class: code.class(),
            message: message.unwrap_or_else(|| "step denied by policy".to_string()),
            retryability: code.retryability(),
        }),
    }
}

pub(crate) fn filesystem_policy_denied_result(
    run_id: &str,
    step_id: &str,
    execution: u32,
    denial: &crate::filesystem_policy::FilesystemDenial,
) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("failed_{run_id}_{step_id}_{execution}"),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution,
        status: "failed".to_string(),
        exit_code: Some(2),
        stdout: String::new(),
        stderr: "filesystem policy denied".to_string(),
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError {
            code: CanonicalErrorCode::PolicyDenied,
            class: CanonicalErrorCode::PolicyDenied.class(),
            message: denial.to_string(),
            retryability: CanonicalErrorCode::PolicyDenied.retryability(),
        }),
    }
}

pub(crate) fn failed_step_result(
    run_id: &str,
    step_id: &str,
    execution: u32,
    message: String,
) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("failed_{run_id}_{step_id}_{execution}"),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: message,
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    }
}

pub(crate) fn secret_resolution_failed_result(
    request: &transist_protocol::ExecutionRequest,
    error: ProtocolError,
) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        execution: request.step.execution,
        status: "failed".to_string(),
        exit_code: Some(2),
        stdout: String::new(),
        stderr: "secret resolution failed".to_string(),
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(error),
    }
}

pub(crate) fn execution_failed_result(
    request: &transist_protocol::ExecutionRequest,
    message: String,
) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        execution: request.step.execution,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: message.clone(),
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError {
            code: CanonicalErrorCode::ToolExecutionError,
            class: CanonicalErrorCode::ToolExecutionError.class(),
            message,
            retryability: CanonicalErrorCode::ToolExecutionError.retryability(),
        }),
    }
}
