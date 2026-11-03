// SPDX-License-Identifier: MPL-2.0
//! Failure classification records and deterministic classifier helpers.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use transist_protocol::StepExecutionDescriptor;

static FAILURE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Observable state mutation recorded before a step execution finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateMutation {
    /// Runtime persisted an artifact or evidence descriptor for adapter output.
    ArtifactWritten {
        /// Logical path written under the execution's artifact root.
        logical_path: String,
        /// SHA-256 checksum reported for the persisted payload.
        sha256: String,
    },
    /// Adapter acknowledged a side-effecting action before the step failed.
    AdapterAcked {
        /// Adapter family that acknowledged the action.
        adapter_kind: String,
        /// Action that was acknowledged.
        action: String,
    },
}

/// Typed failure classes persisted for inspection and reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Workflow authoring, configuration, or static definition problem.
    WorkflowDefinitionError,
    /// Adapter rejected the step-level input payload.
    InputValidationError,
    /// Adapter implementation failed internally.
    AdapterInternalError,
    /// Downstream tool or command failed.
    ToolError,
    /// Adapter/kernel protocol contract was malformed.
    ProtocolError,
    /// Step exceeded its timeout budget.
    Timeout,
    /// Plugin or external service authorization failed.
    AuthorizationError,
    /// Secret lookup or delivery failed.
    SecretResolutionError,
    /// External service rate limit was hit.
    RateLimitError,
    /// Network, IPC, or adapter transport failed.
    TransportError,
    /// Run or step was explicitly cancelled.
    Cancelled,
    /// Runtime policy denied the step before execution.
    PolicyError,
    /// Error code is not known to this runtime version.
    UnknownError,
}

impl FailureClass {
    /// Returns the stable snake_case identifier for this failure class.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkflowDefinitionError => "workflow_definition_error",
            Self::InputValidationError => "input_validation_error",
            Self::AdapterInternalError => "adapter_internal_error",
            Self::ToolError => "tool_error",
            Self::ProtocolError => "protocol_error",
            Self::Timeout => "timeout",
            Self::AuthorizationError => "authorization_error",
            Self::SecretResolutionError => "secret_resolution_error",
            Self::RateLimitError => "rate_limit_error",
            Self::TransportError => "transport_error",
            Self::Cancelled => "cancelled",
            Self::PolicyError => "policy_error",
            Self::UnknownError => "unknown_error",
        }
    }
}

/// First-class record describing one failed step execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRecord {
    /// Stable failure identifier.
    pub failure_id: String,
    /// Run id that owns this failure.
    pub run_id: String,
    /// Step id that failed.
    pub step_id: String,
    /// Step execution attempt that failed.
    pub execution_attempt: u32,
    /// Last successful step observed before this failure in run execution order.
    pub last_stable_step_id: Option<String>,
    /// Deterministic failure class derived from the error code and descriptor.
    pub failure_class: FailureClass,
    /// Canonical protocol error code, or `unknown_error` when unavailable.
    pub error_code: String,
    /// Single-line redacted operator-facing error message.
    pub message_redacted: String,
    /// Adapter family or kernel execution surface that failed.
    pub adapter_kind: String,
    /// Adapter/plugin identity when known.
    pub adapter_name: Option<String>,
    /// Tool/action identity when known.
    pub tool_name: Option<String>,
    /// Whether the kernel believes the step can be retried.
    pub retryable: bool,
    /// Whether no observable state mutation was detected before failure.
    pub safe_to_retry: bool,
    /// Whether operator action is required before recovery.
    pub requires_manual_attention: bool,
    /// Whether at least one observable mutation was recorded before failure.
    pub state_mutation_detected: bool,
    /// First observed timestamp for this failure in Unix milliseconds.
    pub first_seen_at_unix_ms: u64,
    /// Last observed timestamp for this failure in Unix milliseconds.
    pub last_seen_at_unix_ms: u64,
    /// Number of occurrences represented by this record.
    pub occurrence_count: u32,
}

/// Deterministic retry and operator-attention flags for a failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureFlags {
    /// Whether the kernel believes the same step can be retried.
    pub retryable: bool,
    /// Whether the kernel detected no observable state mutation.
    pub safe_to_retry: bool,
    /// Whether operator action is required before recovery.
    pub requires_manual_attention: bool,
}

/// Builds a UUIDv7-shaped failure identifier without introducing a direct UUID dependency.
pub fn new_failure_id(timestamp_ms: u64) -> String {
    let counter = FAILURE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut digest = Sha256::new();
    digest.update(timestamp_ms.to_be_bytes());
    digest.update(counter.to_be_bytes());
    digest.update(std::process::id().to_be_bytes());
    let entropy = digest.finalize();

    let mut bytes = [0_u8; 16];
    let timestamp_bytes = timestamp_ms.to_be_bytes();
    bytes[..6].copy_from_slice(&timestamp_bytes[2..]);
    bytes[6] = 0x70 | (entropy[0] & 0x0f);
    bytes[7] = entropy[1];
    bytes[8] = 0x80 | (entropy[2] & 0x3f);
    bytes[9..].copy_from_slice(&entropy[3..10]);

    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ])
    )
}

/// Classifies one canonical error code in the context of its step descriptor.
pub fn classify(error_code: &str, descriptor: &StepExecutionDescriptor) -> FailureClass {
    match error_code {
        "configuration_error" => FailureClass::WorkflowDefinitionError,
        "validation_error" if matches!(descriptor, StepExecutionDescriptor::Invoke { .. }) => {
            FailureClass::InputValidationError
        }
        "subflow_input_binding_failed" => FailureClass::InputValidationError,
        "validation_error" => FailureClass::WorkflowDefinitionError,
        "adapter_internal_error" => FailureClass::AdapterInternalError,
        "tool_execution_error" => FailureClass::ToolError,
        "protocol_error" => FailureClass::ProtocolError,
        "timeout_error" => FailureClass::Timeout,
        "auth_error" => FailureClass::AuthorizationError,
        "secret_resolution_error" => FailureClass::SecretResolutionError,
        "rate_limit_error" => FailureClass::RateLimitError,
        "transport_error" => FailureClass::TransportError,
        "cancelled" => FailureClass::Cancelled,
        "policy_denied" | "capability_not_granted" => FailureClass::PolicyError,
        _ => FailureClass::UnknownError,
    }
}

/// Returns default retry flags for one failure class.
pub fn default_flags(class: FailureClass) -> FailureFlags {
    match class {
        FailureClass::WorkflowDefinitionError
        | FailureClass::InputValidationError
        | FailureClass::AdapterInternalError
        | FailureClass::ProtocolError
        | FailureClass::AuthorizationError
        | FailureClass::SecretResolutionError
        | FailureClass::PolicyError
        | FailureClass::UnknownError => FailureFlags {
            retryable: false,
            safe_to_retry: false,
            requires_manual_attention: true,
        },
        FailureClass::ToolError | FailureClass::Timeout | FailureClass::TransportError => {
            FailureFlags {
                retryable: true,
                safe_to_retry: false,
                requires_manual_attention: false,
            }
        }
        FailureClass::RateLimitError | FailureClass::Cancelled => FailureFlags {
            retryable: true,
            safe_to_retry: true,
            requires_manual_attention: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::AdapterKind;

    fn invoke_descriptor() -> StepExecutionDescriptor {
        StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: Some("shell".to_string()),
            plugin: "shell".to_string(),
            action: "run".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            plugin_sha256: None,
        }
    }

    fn control_descriptor() -> StepExecutionDescriptor {
        StepExecutionDescriptor::Control {
            primitive: "control.noop".to_string(),
        }
    }

    #[test]
    fn classifies_every_taxonomy_code() {
        let descriptor = invoke_descriptor();
        let cases = [
            ("configuration_error", FailureClass::WorkflowDefinitionError),
            ("auth_error", FailureClass::AuthorizationError),
            (
                "secret_resolution_error",
                FailureClass::SecretResolutionError,
            ),
            ("validation_error", FailureClass::InputValidationError),
            (
                "subflow_input_binding_failed",
                FailureClass::InputValidationError,
            ),
            ("timeout_error", FailureClass::Timeout),
            ("cancelled", FailureClass::Cancelled),
            ("rate_limit_error", FailureClass::RateLimitError),
            ("transport_error", FailureClass::TransportError),
            ("protocol_error", FailureClass::ProtocolError),
            ("tool_execution_error", FailureClass::ToolError),
            ("adapter_internal_error", FailureClass::AdapterInternalError),
            ("policy_denied", FailureClass::PolicyError),
            ("capability_not_granted", FailureClass::PolicyError),
        ];

        for (code, expected) in cases {
            assert_eq!(classify(code, &descriptor), expected, "{code}");
        }
    }

    #[test]
    fn classifies_workflow_validation_outside_invoke_steps() {
        assert_eq!(
            classify("validation_error", &control_descriptor()),
            FailureClass::WorkflowDefinitionError
        );
    }

    #[test]
    fn classifies_unknown_codes_as_unknown_error() {
        assert_eq!(
            classify("adapter_made_this_up", &invoke_descriptor()),
            FailureClass::UnknownError
        );
    }

    #[test]
    fn assigns_default_flags_for_each_class() {
        assert_eq!(
            default_flags(FailureClass::ToolError),
            FailureFlags {
                retryable: true,
                safe_to_retry: false,
                requires_manual_attention: false,
            }
        );
        assert_eq!(
            default_flags(FailureClass::RateLimitError),
            FailureFlags {
                retryable: true,
                safe_to_retry: true,
                requires_manual_attention: false,
            }
        );
        assert_eq!(
            default_flags(FailureClass::SecretResolutionError),
            FailureFlags {
                retryable: false,
                safe_to_retry: false,
                requires_manual_attention: true,
            }
        );
    }
}
