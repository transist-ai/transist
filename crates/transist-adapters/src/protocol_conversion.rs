// SPDX-License-Identifier: MPL-2.0
//! Conversion between app-protocol types and adapter execution results.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use transist_protocol::{
    self as protocol, APP_PROTOCOL_VERSION_V1, AppInvokeResult, CanonicalErrorCode,
    ExecutionRequest, ExecutionResult, InvokeTiming, ProtocolError, TEP_INVOKE_RESULT_TYPE_V1,
    TEP_PROTOCOL_VERSION_V1,
};

use super::ProcessInvocationState;
use crate::{canonicalize_hints, redact_json, redact_text};

const SANDBOX_FAILURE_OUTPUT_KEY: &str = "__sandbox_failed__";

/// Validates terminal APP framing identity before any payload or hint retention.
pub(crate) fn validate_app_result_identity(
    request: &ExecutionRequest,
    result: &AppInvokeResult,
) -> Result<()> {
    if result.message_type != protocol::APP_INVOKE_RESULT_TYPE_V1 {
        anyhow::bail!("invalid APP result type");
    }
    if result.protocol_version != APP_PROTOCOL_VERSION_V1 {
        anyhow::bail!("unsupported APP protocol");
    }
    if result.invoke_id != request.invoke_id {
        anyhow::bail!("APP invoke_id mismatch");
    }
    Ok(())
}

pub(crate) fn app_result_to_execution_result(
    request: &ExecutionRequest,
    plugin_dir: &Path,
    action: &transist_plugins::ActionDescriptor,
    secret_values: &BTreeMap<String, String>,
    app_result: AppInvokeResult,
    stderr_text: String,
    timing: InvokeTiming,
) -> Result<ExecutionResult> {
    if app_result.message_type != protocol::APP_INVOKE_RESULT_TYPE_V1 {
        return Ok(failure_result(
            request,
            CanonicalErrorCode::ProtocolError,
            &format!("invalid APP result type '{}'", app_result.message_type),
            stderr_text,
            Some(2),
            timing,
        ));
    }
    if app_result.protocol_version != APP_PROTOCOL_VERSION_V1 {
        return Ok(failure_result(
            request,
            CanonicalErrorCode::ProtocolError,
            &format!("unsupported APP protocol '{}'", app_result.protocol_version),
            stderr_text,
            Some(2),
            timing,
        ));
    }
    if app_result.invoke_id != request.invoke_id {
        return Ok(failure_result(
            request,
            CanonicalErrorCode::ProtocolError,
            "APP invoke_id mismatch",
            stderr_text,
            Some(2),
            timing,
        ));
    }
    if app_result.status == "success"
        && let Err(error) =
            transist_plugins::validate_action_output(plugin_dir, action, &app_result.output)
    {
        return Ok(failure_result(
            request,
            CanonicalErrorCode::ValidationError,
            &error.to_string(),
            redact_text(&stderr_text, secret_values),
            Some(2),
            timing,
        ));
    }
    let (artifacts, evidence) = canonicalize_hints(
        request,
        &app_result.artifact_hints,
        &app_result.evidence_hints,
        secret_values,
    )?;
    let stdout = app_result
        .output
        .get("stdout")
        .and_then(serde_json::Value::as_str)
        .map(|value| redact_text(value, secret_values))
        .unwrap_or_default();
    let stderr = if stderr_text.is_empty() {
        app_result
            .output
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .map(|value| redact_text(value, secret_values))
            .unwrap_or_default()
    } else {
        stderr_text
    };
    Ok(ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        execution: request.step.execution,
        status: app_result.status.clone(),
        exit_code: app_result
            .output
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .map(|value| value as i32),
        stdout,
        stderr,
        timing,
        artifacts,
        evidence,
        metrics: app_result.metrics,
        output: sanitize_execution_output(redact_json(app_result.output, secret_values)),
        metrics_validation_errors: app_result.metrics_validation_errors,
        usage: app_result.usage,
        error: app_result.error.map(|error| ProtocolError {
            message: redact_text(&error.message, secret_values),
            ..error
        }),
    })
}

/// Maps an MCP `CallToolResult` into the native execution result shape without
/// importing APP-only fields such as hints, metrics, or usage.
pub(crate) fn mcp_tool_result_to_execution_result(
    request: &ExecutionRequest,
    plugin_dir: &Path,
    action: &transist_plugins::ActionDescriptor,
    secret_values: &BTreeMap<String, String>,
    result: serde_json::Value,
    stderr_text: String,
    timing: InvokeTiming,
) -> Result<ExecutionResult> {
    let result = result
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("MCP tools/call result must be an object"))?;
    let content = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MCP tools/call result is missing content array"))?;
    if result
        .get("isError")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(anyhow::anyhow!("MCP tools/call isError must be a boolean"));
    }
    if result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(failure_result(
            request,
            CanonicalErrorCode::ToolExecutionError,
            "MCP tool returned isError: true",
            stderr_text,
            Some(1),
            timing,
        ));
    }
    let output = result
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"content": content}));
    if let Err(error) = transist_plugins::validate_action_output(plugin_dir, action, &output) {
        return Ok(failure_result(
            request,
            CanonicalErrorCode::ValidationError,
            &format!("MCP tool result does not match action output schema: {error}"),
            redact_text(&stderr_text, secret_values),
            Some(2),
            timing,
        ));
    }
    let stdout = content
        .iter()
        .filter_map(|block| {
            block
                .as_object()
                .filter(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                })
                .and_then(|block| block.get("text"))
                .and_then(serde_json::Value::as_str)
        })
        .collect::<String>();
    Ok(ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        execution: request.step.execution,
        status: "success".to_string(),
        exit_code: Some(0),
        stdout: redact_text(&stdout, secret_values),
        stderr: redact_text(&stderr_text, secret_values),
        timing,
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: sanitize_execution_output(redact_json(output, secret_values)),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    })
}

pub(crate) fn failure_result(
    request: &ExecutionRequest,
    code: CanonicalErrorCode,
    message: &str,
    stderr: String,
    exit_code: Option<i32>,
    timing: InvokeTiming,
) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        execution: request.step.execution,
        status: "failed".to_string(),
        exit_code,
        stdout: String::new(),
        stderr,
        timing,
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError::new(code, message)),
    }
}

/// Creates a failure result for sandbox setup failures with structured metadata
/// in the output field so the kernel can emit a `filesystem_policy.sandbox_failed` event.
pub(crate) fn sandbox_failure_result(
    request: &ExecutionRequest,
    backend: &str,
    reason: &str,
    ignore_paths: &[String],
    timing: InvokeTiming,
) -> ExecutionResult {
    let filesystem = request
        .policy
        .get("filesystem")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
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
        stderr: String::new(),
        timing,
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({
            SANDBOX_FAILURE_OUTPUT_KEY: {
                "step_id": request.step.id,
                "adapter": request.adapter_kind,
                "backend": backend,
                "reason": reason,
                "source": filesystem.get("source").cloned().unwrap_or(serde_json::Value::Null),
                "mode": filesystem
                    .get("workspace_access")
                    .and_then(|value| value.get("mode"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                "legacy_ignore_paths": ignore_paths,
            }
        }),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError::new(
            CanonicalErrorCode::PolicyDenied,
            format!("workspace-scoped filesystem sandboxing failed: {reason}"),
        )),
    }
}

fn sanitize_execution_output(mut output: serde_json::Value) -> serde_json::Value {
    if let Some(object) = output.as_object_mut() {
        object.remove(SANDBOX_FAILURE_OUTPUT_KEY);
    }
    output
}

pub(crate) fn cancelled_result(
    request: &ExecutionRequest,
    message: &str,
    timing: InvokeTiming,
) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        execution: request.step.execution,
        status: "cancelled".to_string(),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        timing,
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError::new(CanonicalErrorCode::Cancelled, message)),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_failure_or_cancelled(
    request: &ExecutionRequest,
    active: &ProcessInvocationState,
    code: CanonicalErrorCode,
    message: &str,
    stderr: String,
    exit_code: Option<i32>,
    timing: InvokeTiming,
    external_cancel: bool,
) -> ExecutionResult {
    if active.cancelled.load(Ordering::SeqCst) || external_cancel {
        cancelled_result(request, "process invocation cancelled", timing)
    } else {
        failure_result(request, code, message, stderr, exit_code, timing)
    }
}
