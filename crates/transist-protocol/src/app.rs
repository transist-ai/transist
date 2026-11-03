// SPDX-License-Identifier: MPL-2.0
//! APP protocol messages exchanged with plugin-provided application adapters.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{CapabilityRequest, InvokePaths, InvokeUsage, ProtocolError};

/// Protocol version string carried by APP handshake and invoke messages.
pub const APP_PROTOCOL_VERSION_V1: &str = "app.v1";
/// Message type for the APP handshake request envelope.
pub const APP_HANDSHAKE_REQUEST_TYPE_V1: &str = "app.handshake.request.v1";
/// Message type for the APP handshake response envelope.
pub const APP_HANDSHAKE_RESPONSE_TYPE_V1: &str = "app.handshake.response.v1";
/// Message type for the APP invoke request envelope.
pub const APP_INVOKE_REQUEST_TYPE_V1: &str = "app.invoke.request.v1";
/// Message type for streamed APP progress frames.
pub const APP_INVOKE_PROGRESS_TYPE_V1: &str = "app.invoke.progress.v1";
/// Message type for the terminal APP invoke result.
pub const APP_INVOKE_RESULT_TYPE_V1: &str = "app.invoke.result.v1";
/// Message type for an APP cancel request.
pub const APP_CANCEL_REQUEST_TYPE_V1: &str = "app.cancel.request.v1";
/// Message type for an APP cancel response.
pub const APP_CANCEL_RESULT_TYPE_V1: &str = "app.cancel.result.v1";

/// Stable identity for the plugin package serving an APP adapter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppPluginIdentity {
    /// Plugin identifier declared by the installed manifest.
    pub id: String,
    /// Plugin version declared by the installed manifest.
    pub version: String,
}

/// Capabilities advertised by an APP adapter during handshake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppCapabilities {
    /// Action ids the adapter can accept through this APP surface.
    pub actions: Vec<String>,
    /// Whether the adapter accepts cancel messages for in-flight invokes.
    pub supports_cancel: bool,
    /// Whether the adapter exposes a health probe on this surface.
    pub supports_health: bool,
    /// Whether invoke results may include artifact hint records.
    pub supports_artifact_hints: bool,
    /// Whether invoke results may include evidence hint records.
    pub supports_evidence_hints: bool,
}

/// Initial message sent by the host to negotiate the APP protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandshakeRequest {
    /// Concrete message type, usually [`APP_HANDSHAKE_REQUEST_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version requested by the caller.
    pub protocol_version: String,
}

/// Adapter handshake response describing the plugin identity and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandshakeResponse {
    /// Concrete message type, usually [`APP_HANDSHAKE_RESPONSE_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version accepted by the adapter.
    pub protocol_version: String,
    /// Identity of the plugin package serving this adapter.
    pub plugin: AppPluginIdentity,
    /// Adapter family declared by the plugin.
    pub adapter_kind: crate::AdapterKind,
    /// Supported actions and optional features for this adapter.
    pub capabilities: AppCapabilities,
}

/// Step identity attached to APP invoke and progress envelopes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvokeStep {
    /// Workflow step id for the execution being executed.
    pub id: String,
    /// Execution number for this step execution, starting at `1`.
    pub execution: u32,
}

/// Execute a single plugin action through the APP adapter protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvokeRequest {
    /// Concrete message type, usually [`APP_INVOKE_REQUEST_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version requested by the caller.
    pub protocol_version: String,
    /// Unique invoke id for correlating progress and result messages.
    pub invoke_id: String,
    /// Transist run id that owns this invoke.
    pub run_id: String,
    /// Workflow identifier for the owning run.
    pub workflow_id: String,
    /// Step id and execution number for the current invoke.
    pub step: InvokeStep,
    /// Plugin package identity expected by the host.
    pub plugin: AppPluginIdentity,
    /// Plugin action id to execute.
    pub action: String,
    /// JSON input payload already materialized by the runtime.
    #[serde(default)]
    pub input: Value,
    /// Filesystem paths made available to the adapter for this invoke.
    pub paths: InvokePaths,
    /// Canonical capabilities required by this invoke, forwarded from the
    /// TEP invoke request so the plugin observes the exact authority subset
    /// the kernel evaluated.
    #[serde(default)]
    pub requested_capabilities: Vec<CapabilityRequest>,
    /// Additive host policy metadata forwarded from the TEP invoke request.
    #[serde(default)]
    pub policy: Value,
    /// Optional timeout budget in milliseconds for the external call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Optional hard wall-clock timeout in milliseconds.
    ///
    /// When present, adapters should fail the invocation once this total
    /// budget is exceeded even if progress frames are still being emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timeout_ms: Option<u64>,
    /// Host-provided metadata reserved for runtime bookkeeping.
    #[serde(default)]
    pub metadata: Value,
}

/// Optional progress frame emitted before the terminal APP invoke result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvokeProgress {
    /// Concrete message type, usually [`APP_INVOKE_PROGRESS_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version used by the adapter.
    pub protocol_version: String,
    /// Invoke id this progress frame belongs to.
    pub invoke_id: String,
    /// Logical stream name, typically `stdout` or `stderr`.
    #[serde(default)]
    pub stream: String,
    /// Count of new lines represented by this progress update.
    #[serde(default = "default_progress_delta")]
    pub delta_lines: u64,
    /// Optional preview of the most recent emitted line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_preview: Option<String>,
}

/// Default `delta_lines` value when a progress frame omits the field.
///
/// One line per frame is the natural choice for line-oriented adapters
/// (e.g., process plugins emitting JSONL): a frame is normally produced once a
/// new line is observed, so the absence of an explicit count means "exactly
/// one new line".
fn default_progress_delta() -> u64 {
    1
}

/// Terminal APP invoke result produced by an application adapter.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InvokeResult {
    /// Concrete message type, usually [`APP_INVOKE_RESULT_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version used by the adapter.
    pub protocol_version: String,
    /// Invoke id this result completes.
    pub invoke_id: String,
    /// Adapter-defined terminal status string.
    pub status: String,
    /// JSON output payload returned by the plugin action.
    #[serde(default)]
    pub output: Value,
    /// Optional artifact descriptors that the host may persist.
    #[serde(default)]
    pub artifact_hints: Vec<Value>,
    /// Optional evidence descriptors that the host may persist.
    #[serde(default)]
    pub evidence_hints: Vec<Value>,
    /// Numeric metrics normalized from the adapter payload.
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    /// Optional usage or token accounting supplied by the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<InvokeUsage>,
    /// Optional canonical protocol error for non-success outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
    /// Number of metric entries dropped because they were not numeric.
    #[serde(skip)]
    pub metrics_validation_errors: u32,
}

impl<'de> Deserialize<'de> for InvokeResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawInvokeResult {
            #[serde(rename = "type")]
            message_type: String,
            protocol_version: String,
            invoke_id: String,
            status: String,
            #[serde(default)]
            output: Value,
            #[serde(default)]
            artifact_hints: Vec<Value>,
            #[serde(default)]
            evidence_hints: Vec<Value>,
            #[serde(default)]
            metrics: Value,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            usage: Option<InvokeUsage>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            error: Option<ProtocolError>,
        }

        let raw = RawInvokeResult::deserialize(deserializer)?;
        let (metrics, metrics_validation_errors) = sanitize_numeric_metrics_map(raw.metrics);
        Ok(Self {
            message_type: raw.message_type,
            protocol_version: raw.protocol_version,
            invoke_id: raw.invoke_id,
            status: raw.status,
            output: raw.output,
            artifact_hints: raw.artifact_hints,
            evidence_hints: raw.evidence_hints,
            metrics,
            usage: raw.usage,
            error: raw.error,
            metrics_validation_errors,
        })
    }
}

fn sanitize_numeric_metrics_map(raw: Value) -> (BTreeMap<String, f64>, u32) {
    let mut out = BTreeMap::new();
    let mut invalid_entries = 0_u32;
    let Some(object) = raw.as_object() else {
        if !raw.is_null() {
            invalid_entries = 1;
        }
        return (out, invalid_entries);
    };
    for (key, value) in object {
        if let Some(number) = value.as_f64() {
            out.insert(key.clone(), number);
        } else {
            invalid_entries = invalid_entries.saturating_add(1);
        }
    }
    (out, invalid_entries)
}

/// Cancel a previously issued APP invoke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelRequest {
    /// Concrete message type, usually [`APP_CANCEL_REQUEST_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version used by the caller.
    pub protocol_version: String,
    /// Invoke id to cancel.
    pub invoke_id: String,
}

/// Result of attempting to cancel an APP invoke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelResult {
    /// Concrete message type, usually [`APP_CANCEL_RESULT_TYPE_V1`].
    #[serde(rename = "type")]
    pub message_type: String,
    /// Protocol version used by the adapter.
    pub protocol_version: String,
    /// Invoke id that was targeted for cancellation.
    pub invoke_id: String,
    /// Whether the adapter accepted the cancellation request.
    pub accepted: bool,
}
