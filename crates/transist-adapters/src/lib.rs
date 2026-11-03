// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Adapter implementations for process, HTTP, and MCP-backed plugin execution.
//!
//! This crate owns host-side adapter ports, transport handshakes, preflight
//! probing, and adapter-specific secret materialization for Transist plugins.
//! It may depend on `transist-core`, `transist-protocol`, and plugin manifest
//! resolution, but it must not own runtime orchestration, storage, CLI, or HTTP
//! API behavior.
//!
//! The adapter types exported from the crate root and the router are the intended
//! stable surface. Internal runtime/helper modules remain implementation details.

/// Adapter crate version.
pub const TRANSIST_ADAPTERS_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) const LIVE_PROGRESS_FILE_NAME: &str = ".transist-live-progress.json";

use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;
use transist_core::Redactor;
use transist_core::ports::{
    ExecutionCancelRequest, ExecutionCancelResult, ExecutionInvocation, ExecutionPort,
    PreflightInvocation, PreflightPort,
};
use transist_core::{PreflightCheck, PreflightStatus};
use transist_plugins::{
    ActionDescriptor, AdapterKind, PluginEntrypoint, PluginManifest, SecretBinding,
};
use transist_protocol::{
    self as protocol, APP_HANDSHAKE_REQUEST_TYPE_V1, APP_INVOKE_PROGRESS_TYPE_V1,
    APP_INVOKE_REQUEST_TYPE_V1, APP_PROTOCOL_VERSION_V1, AdapterCapabilities, AppHandshakeRequest,
    AppHandshakeResponse, AppInvokeProgress, AppInvokeRequest, AppInvokeResult, AppPluginIdentity,
    CanonicalErrorCode, ExecutionRequest, ExecutionResult, NetworkAccessMode, NetworkPolicy,
};

// NOTE (Step 2.4 close): A shared `execute_step()` template was re-evaluated after
// Step 2.3 extracted the active-invocation registry, and intentionally not
// extracted. The three adapters' execute paths still diverge transport-specifically:
//   - Process: stdio JSON-line APP protocol, temp-file cleanup, handshake + invoke phases.
//   - HTTP:    reqwest transport, HTTP handshake + POST invoke, async cancel FSM.
//   - MCP:     stdio MCP JSON-RPC, initialize + tools/list + tools/call phases.
// The shared scaffolding worth lifting (active-invocation register/lookup/remove)
// is already factored into `ActiveInvocations<T>`. Lifting `execute_step` further
// would require closures or trait objects for each phase, adding more complexity
// than the remaining duplication removes (YAGNI/KISS).

pub mod os_process;
pub mod router;

pub use adapter_defs::{DEFAULT_PREFLIGHT_PROBE_CONCURRENCY, DEFAULT_TERMINATION_GRACE_MS};
pub use cancel_state::{CancelEntryResult, CancelOutcome, cancel_active_invocations_on_disk};
pub use router::AdapterRouter;

mod action_input;
mod active_invocation;
mod adapter_defs;
mod cancel_state;
mod filesystem_sandbox;
mod handshake;
mod hints;
mod http_adapter;
mod http_runtime;
mod mcp_adapter;
mod mcp_runtime;
mod paths;
mod plugin_loader;
mod preflight;
mod process_adapter;
mod process_lifecycle;
mod process_runtime;
mod protocol_conversion;
mod record_state;
mod secrets;
mod timing;

pub use adapter_defs::{HttpAdapter, McpAdapter, ProcessAdapter};
pub use preflight::{
    validate_deferred_preflight_input, validate_static_preflight_steps,
    validate_static_preflight_steps_with_catalog,
};

pub(crate) use action_input::project_action_input;
pub(crate) use active_invocation::{
    HttpCancelConfig, HttpInvocationState, McpInvocationState, ProcessInvocationState,
};
pub(crate) use filesystem_sandbox::{
    SandboxPlan, build_sandbox_plan, filesystem_policy_from_request, make_sandboxed_command,
    network_policy_from_request, sandbox_context_from_request,
    stderr_matches_sandbox_wrapper_failure, strict_filesystem_policy_requested,
    strict_local_policy_requested, strict_local_sandbox_backend_label,
};
pub(crate) use handshake::{
    mcp_initialize_and_list_tools, perform_http_handshake, perform_http_health, validate_handshake,
};
pub(crate) use hints::*;
pub(crate) use http_runtime::{HttpRequestOptions, HttpRequestPolicy, http_json_request};
pub(crate) use mcp_runtime::{
    MCP_PROTOCOL_VERSION, McpStdin, is_mcp_protocol_error, next_mcp_request_id, recv_mcp_response,
    send_mcp_notification, send_mcp_request, spawn_mcp_reader,
};
pub(crate) use paths::*;
pub(crate) use plugin_loader::load_plugin;
pub(crate) use preflight::{
    PreflightProbeGroup, insert_preflight_probe_target, mcp_tool_name, preflight_check,
    run_http_preflight_probe, run_local_preflight_probe_groups, validate_static_preflight_target,
};
pub(crate) use process_runtime::{
    InvokeResultReceiveError, child_has_exited, configure_child_process_group, join_stderr,
    parse_json_line, recv_invoke_result, recv_line, resolve_execution_cwd, spawn_line_reader,
    spawn_stderr_reader, terminate_child, wait_for_exit, write_json_line,
};
pub(crate) use protocol_conversion::sandbox_failure_result;
pub(crate) use protocol_conversion::*;
pub(crate) use record_state::{
    clear_active_invocation, external_cancel_observed, publish_active_invocation,
};
pub(crate) use secrets::{
    build_http_header_bindings, build_mcp_secret_bindings, build_process_secret_env,
    cleanup_temp_files, inject_http_action_secrets,
};
pub(crate) use timing::*;

pub(crate) fn redact_json(
    value: serde_json::Value,
    secret_values: &BTreeMap<String, String>,
) -> serde_json::Value {
    build_redactor(secret_values).redact_json(value)
}

pub(crate) fn redact_text(text: &str, secret_values: &BTreeMap<String, String>) -> String {
    build_redactor(secret_values).redact_str(text)
}

fn build_redactor(secret_values: &BTreeMap<String, String>) -> Redactor {
    let secrets: Vec<&str> = secret_values
        .values()
        .map(std::string::String::as_str)
        .collect();
    Redactor::from_secrets(secrets)
}
