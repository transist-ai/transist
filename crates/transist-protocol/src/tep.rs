// SPDX-License-Identifier: MPL-2.0
//! TEP transport envelopes and normalized execution result types.

use crate::CapabilityRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Protocol version string carried by TEP envelopes.
pub const TEP_PROTOCOL_VERSION_V1: &str = "tep.v1";
/// Message type for the TEP handshake request envelope.
pub const TEP_HANDSHAKE_REQUEST_TYPE_V1: &str = "tep.handshake.request.v1";
/// Message type for the TEP handshake response envelope.
pub const TEP_HANDSHAKE_RESPONSE_TYPE_V1: &str = "tep.handshake.response.v1";
/// Message type for the TEP invoke request envelope.
pub const TEP_INVOKE_REQUEST_TYPE_V1: &str = "tep.invoke.request.v1";
/// Message type for the TEP invoke result envelope.
pub const TEP_INVOKE_RESULT_TYPE_V1: &str = "tep.invoke.result.v1";

/// Canonical adapter failure codes understood by the Transist runtime.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalErrorCode {
    /// Static configuration or manifest problem.
    ConfigurationError,
    /// Authentication or identity failure.
    AuthError,
    /// Secret lookup or delivery failure.
    SecretResolutionError,
    /// Input or schema validation failure.
    ValidationError,
    /// Parent subflow step failed while binding declared child inputs.
    SubflowInputBindingFailed,
    /// Timeout reached before completion.
    TimeoutError,
    /// Explicit cancellation.
    Cancelled,
    /// Provider or transport rate limit.
    RateLimitError,
    /// Network or IPC transport failure.
    TransportError,
    /// Message framing or protocol contract failure.
    ProtocolError,
    /// Downstream tool or command execution failure.
    ToolExecutionError,
    /// Adapter implementation failure not attributable to the caller.
    AdapterInternalError,
    /// Runtime policy denied execution before dispatching the adapter call.
    PolicyDenied,
    /// An exact capability grant was missing before dispatch.
    CapabilityNotGranted,
}

/// Coarser error bucket used by policy and reporting surfaces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Configuration or authoring failure.
    Configuration,
    /// Authentication failure.
    Auth,
    /// Secret delivery or lookup failure.
    Secret,
    /// Input validation failure.
    Validation,
    /// Timeout failure.
    Timeout,
    /// Cancellation result.
    Cancellation,
    /// Rate-limit condition.
    RateLimit,
    /// Transport failure.
    Transport,
    /// Protocol contract failure.
    Protocol,
    /// Tool execution failure.
    Tool,
    /// Adapter-side internal failure.
    Internal,
    /// Runtime policy denial.
    Policy,
}

/// Guidance about whether the runtime may safely retry an error.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Retryability {
    /// The request should not be retried automatically.
    None,
    /// Retrying is usually safe without operator input.
    Safe,
    /// A human operator is expected to intervene before retrying.
    Operator,
}

impl CanonicalErrorCode {
    /// Maps a canonical code to its broader error class.
    pub fn class(self) -> ErrorClass {
        match self {
            Self::ConfigurationError => ErrorClass::Configuration,
            Self::AuthError => ErrorClass::Auth,
            Self::SecretResolutionError => ErrorClass::Secret,
            Self::ValidationError => ErrorClass::Validation,
            Self::SubflowInputBindingFailed => ErrorClass::Validation,
            Self::TimeoutError => ErrorClass::Timeout,
            Self::Cancelled => ErrorClass::Cancellation,
            Self::RateLimitError => ErrorClass::RateLimit,
            Self::TransportError => ErrorClass::Transport,
            Self::ProtocolError => ErrorClass::Protocol,
            Self::ToolExecutionError => ErrorClass::Tool,
            Self::AdapterInternalError => ErrorClass::Internal,
            Self::PolicyDenied => ErrorClass::Policy,
            Self::CapabilityNotGranted => ErrorClass::Policy,
        }
    }

    /// Maps a canonical code to Transist's retryability guidance.
    pub fn retryability(self) -> Retryability {
        match self {
            Self::ConfigurationError
            | Self::ValidationError
            | Self::SubflowInputBindingFailed
            | Self::Cancelled
            | Self::ProtocolError => Retryability::None,
            Self::AuthError
            | Self::SecretResolutionError
            | Self::AdapterInternalError
            | Self::PolicyDenied
            | Self::CapabilityNotGranted => Retryability::Operator,
            Self::TimeoutError
            | Self::RateLimitError
            | Self::TransportError
            | Self::ToolExecutionError => Retryability::Safe,
        }
    }
}

/// Normalized protocol error payload shared across adapters and runtimes.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ProtocolError {
    /// Stable canonical error code.
    pub code: CanonicalErrorCode,
    /// Broader error class derived from [`Self::code`].
    pub class: ErrorClass,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Retry recommendation derived from [`Self::code`].
    pub retryability: Retryability,
}

impl ProtocolError {
    /// Builds a protocol error while deriving class and retryability from the code.
    pub fn new(code: CanonicalErrorCode, message: impl Into<String>) -> Self {
        Self {
            class: code.class(),
            retryability: code.retryability(),
            code,
            message: message.into(),
        }
    }
}

/// Reference to a persisted artifact or evidence file.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct FileRef {
    /// Relative path within the run-owned file family.
    pub path: String,
    /// SHA-256 checksum of the referenced file.
    pub sha256: String,
    /// File size in bytes.
    pub bytes: u64,
}

/// Host-to-adapter TEP handshake message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Concrete message type, usually [`TEP_HANDSHAKE_REQUEST_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version requested by the caller.
    pub protocol_version: String,
}

/// Adapter handshake response describing the adapter identity and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    /// Concrete message type, usually [`TEP_HANDSHAKE_RESPONSE_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version accepted by the adapter.
    pub protocol_version: String,
    /// Adapter identity serving the request.
    pub adapter: AdapterIdentity,
    /// Action list and optional feature support advertised by the adapter.
    pub capabilities: AdapterCapabilities,
}

/// Stable identity for a TEP adapter implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterIdentity {
    /// Adapter family.
    pub kind: crate::AdapterKind,
    /// Adapter implementation version.
    pub version: String,
}

/// Capabilities exposed by a TEP adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterCapabilities {
    /// Action ids accepted by the adapter.
    pub actions: Vec<String>,
    /// Whether cancellation is implemented when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_cancel: Option<bool>,
    /// Whether health probing is implemented when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_health: Option<bool>,
}

/// Execute one adapter action through the TEP transport contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeRequest {
    /// Concrete message type, usually [`TEP_INVOKE_REQUEST_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version requested by the caller.
    pub protocol_version: String,
    /// Unique invoke id for correlating logs and results.
    pub invoke_id: String,
    /// Transist run id that owns this invoke.
    pub run_id: String,
    /// Workflow id for the owning run.
    pub workflow_id: String,
    /// Step id and execution number being executed.
    pub step: InvokeStep,
    /// Adapter family the caller expects to be speaking to.
    pub adapter_kind: crate::AdapterKind,
    /// Action id to execute.
    pub action: String,
    /// JSON input payload already materialized by the runtime.
    #[serde(default)]
    pub input: Value,
    /// Runtime-owned filesystem paths available to the adapter.
    pub paths: InvokePaths,
    /// Canonical capabilities required by this invoke.
    #[serde(default)]
    pub requested_capabilities: Vec<CapabilityRequest>,
    /// Optional timeout budget in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Optional hard wall-clock timeout budget in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timeout_ms: Option<u64>,
    /// Policy payload forwarded to the adapter or host runtime.
    #[serde(default)]
    pub policy: Value,
    /// Host metadata reserved for runtime bookkeeping.
    #[serde(default)]
    pub metadata: Value,
}

/// Step identity attached to invoke requests and results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeStep {
    /// Workflow step id for the execution execution.
    pub id: String,
    /// Execution number for the step execution, starting at `1`.
    pub execution: u32,
}

/// Filesystem paths the runtime makes available to the adapter for one invoke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvokePaths {
    /// Absolute invocation-private staging root for this invoke. This is never
    /// the durable run root and is removed after the invocation.
    pub run_root: String,
    /// Directory where artifacts should be written.
    pub artifacts_dir: String,
    /// Directory where evidence files should be written.
    pub evidence_dir: String,
    /// Scratch workspace directory for adapter execution.
    pub workspace_dir: String,
}

/// Terminal TEP invoke result returned by an adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeResult {
    /// Concrete message type, usually [`TEP_INVOKE_RESULT_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version used by the adapter.
    pub protocol_version: String,
    /// Invoke id this result completes.
    pub invoke_id: String,
    /// Run id that owns the invoke.
    pub run_id: String,
    /// Step id that produced this result.
    pub step_id: String,
    /// Execution number for the completed step execution.
    pub execution: u32,
    /// Adapter-defined terminal status string.
    pub status: String,
    /// Optional exit code for process-like executions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Captured stdout text, if any.
    #[serde(default)]
    pub stdout: String,
    /// Captured stderr text, if any.
    #[serde(default)]
    pub stderr: String,
    /// Timing information recorded by the adapter.
    pub timing: InvokeTiming,
    /// Persisted artifact file references produced by the invoke.
    #[serde(default)]
    pub artifacts: Vec<FileRef>,
    /// Persisted evidence file references produced by the invoke.
    #[serde(default)]
    pub evidence: Vec<FileRef>,
    /// Flattened numeric metrics associated with the invoke.
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    /// JSON output payload returned by the adapter.
    #[serde(default)]
    pub output: Value,
    /// Number of metric entries dropped because they were not numeric.
    #[serde(skip)]
    pub metrics_validation_errors: u32,
    /// Optional usage or token accounting reported by the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<InvokeUsage>,
    /// Optional canonical error payload for non-success outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// Timing metadata attached to a completed invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeTiming {
    /// RFC 3339 start timestamp.
    pub started_at: String,
    /// RFC 3339 completion timestamp.
    pub finished_at: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// Optional usage accounting returned by model-backed adapters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvokeUsage {
    /// Provider identifier, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model identifier, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Prompt or input token count, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Completion or output token count, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Total token count, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

/// Stable schema identifier for persisted external-transfer evidence records.
pub const EXTERNAL_TRANSFER_SCHEMA_V1: &str = "transist.external_transfer.v1";

/// Policy decision attached to one external-transfer evidence record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTransferPolicyDecision {
    /// Whether the external call was allowed to proceed.
    pub allowed: bool,
    /// Execution mode that produced the decision, e.g. `local_only` or `default`.
    pub mode: String,
}

/// Persisted evidence record for one external data transfer.
///
/// Written for each allowed call whose effective `provider_kind` is
/// [`crate::ProviderKind::External`]. Local-only runs record
/// `external_transfers: []` because no external call is allowed to dispatch.
///
/// Transist does NOT claim DLP-grade redaction or legal compliance:
/// `redaction_applied` is honestly `false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTransferRecord {
    /// Record schema version string.
    pub schema: String,
    /// Stable run id.
    pub run_id: String,
    /// Step id that triggered the external call.
    pub step_id: String,
    /// Execution number for the step execution.
    pub execution: u32,
    /// Timestamp in Unix milliseconds when the transfer was recorded.
    pub timestamp_ms: u64,
    /// Best-effort destination identifier: base_url host or provider name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Adapter kind used for the call (`process`, `http`, or `mcp`).
    pub adapter_kind: crate::AdapterKind,
    /// Provider identifier when known (from `InvokeUsage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Declared provider kind: `local` or `external`.
    pub provider_kind: crate::ProviderKind,
    /// Bytes sent to the external destination when measurable; null for
    /// adapters where this is not measurable (e.g. process).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// SHA-256 hash of the request payload sent to the external destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Whether redaction was applied before sending. Always `false` in v1.
    pub redaction_applied: bool,
    /// Policy decision that governed the transfer.
    pub policy_decision: ExternalTransferPolicyDecision,
}

/// Compatibility alias for the canonical TEP request type.
pub type TepRequest = InvokeRequest;
/// Compatibility alias for the canonical TEP result type.
pub type TepResult = InvokeResult;

/// Encodes one JSON value as a newline-terminated JSONL frame.
pub fn jsonl_encode(value: &Value) -> crate::Result<String> {
    let mut out = serde_json::to_string(value)?;
    out.push('\n');
    Ok(out)
}

/// Decodes a single JSONL line into a JSON value.
pub fn jsonl_decode_line(line: &str) -> crate::Result<Value> {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return Err(crate::Error::EmptyJsonlLine);
    }
    Ok(serde_json::from_str(trimmed)?)
}

/// Flattens nested numeric metrics into dot-separated metric names.
pub fn normalize_metrics(value: &Value) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    flatten_metrics("", value, &mut out);
    out
}

fn flatten_metrics(prefix: &str, value: &Value, out: &mut BTreeMap<String, f64>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_metrics(&next, inner, out);
            }
        }
        Value::Number(number) => {
            if let Some(v) = number.as_f64() {
                out.insert(prefix.to_string(), v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_roundtrip() {
        let value = serde_json::json!({"a": 1});
        let line = jsonl_encode(&value).expect("encode");
        let decoded = jsonl_decode_line(&line).expect("decode");
        assert_eq!(value, decoded);
    }

    #[test]
    fn canonical_error_derives_class_and_retryability() {
        let error = ProtocolError::new(CanonicalErrorCode::TransportError, "boom");
        assert_eq!(error.class, ErrorClass::Transport);
        assert_eq!(error.retryability, Retryability::Safe);
    }

    #[test]
    fn external_transfer_record_round_trips() {
        let record = ExternalTransferRecord {
            schema: EXTERNAL_TRANSFER_SCHEMA_V1.to_string(),
            run_id: "run-1".to_string(),
            step_id: "step-fetch".to_string(),
            execution: 1,
            timestamp_ms: 1700000000,
            destination: Some("api.example.com".to_string()),
            adapter_kind: crate::AdapterKind::Http,
            provider: Some("openai".to_string()),
            provider_kind: crate::ProviderKind::External,
            bytes_sent: Some(2048),
            content_hash: Some("abc123".to_string()),
            redaction_applied: false,
            policy_decision: ExternalTransferPolicyDecision {
                allowed: true,
                mode: "default".to_string(),
            },
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: ExternalTransferRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, parsed);
        assert_eq!(parsed.schema, EXTERNAL_TRANSFER_SCHEMA_V1);
        assert!(!parsed.redaction_applied);
    }

    #[test]
    fn external_transfer_record_nullable_fields_omit_when_none() {
        let record = ExternalTransferRecord {
            schema: EXTERNAL_TRANSFER_SCHEMA_V1.to_string(),
            run_id: "run-1".to_string(),
            step_id: "step-local".to_string(),
            execution: 1,
            timestamp_ms: 1700000000,
            destination: None,
            adapter_kind: crate::AdapterKind::Process,
            provider: None,
            provider_kind: crate::ProviderKind::External,
            bytes_sent: None,
            content_hash: None,
            redaction_applied: false,
            policy_decision: ExternalTransferPolicyDecision {
                allowed: true,
                mode: "default".to_string(),
            },
        };
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(!json.contains("destination"));
        assert!(!json.contains("bytes_sent"));
        assert!(!json.contains("content_hash"));
        let parsed: ExternalTransferRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, parsed);
    }
}
