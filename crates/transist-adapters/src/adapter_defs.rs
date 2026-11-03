// SPDX-License-Identifier: MPL-2.0
//! Built-in adapter struct definitions and `Default` implementations.
//!
//! The adapter behavior (`ExecutionPort`/`PreflightPort` impls) lives in the
//! `*_adapter` modules; this file owns just the data shape so that `lib.rs`
//! can stay focused on module wiring and crate-level re-exports.

use crate::active_invocation::{
    ActiveInvocations, HttpInvocationState, McpInvocationState, ProcessInvocationState,
};
use std::sync::Arc;
use std::time::Duration;
use transist_core::ports::ActiveRecordPort;

/// Public default termination grace (in milliseconds) used by the adapter
/// process termination path. Cross-process cancellation helpers use the same
/// default so external callers signal with consistent semantics.
pub const DEFAULT_TERMINATION_GRACE_MS: u64 = 250;

/// Default maximum number of sandbox-isolated live probes per adapter preflight call.
pub const DEFAULT_PREFLIGHT_PROBE_CONCURRENCY: usize = 4;

/// Built-in adapter that executes process-backed plugins over stdio.
#[derive(Clone)]
pub struct ProcessAdapter {
    pub(crate) active: ActiveInvocations<ProcessInvocationState>,
    pub(crate) active_record_store: Arc<dyn ActiveRecordPort>,
    pub(crate) termination_grace: Duration,
    pub(crate) preflight_probe_concurrency: usize,
}

/// Built-in adapter that executes HTTP-backed plugins via JSON requests.
#[derive(Clone)]
pub struct HttpAdapter {
    pub(crate) active: ActiveInvocations<HttpInvocationState>,
    pub(crate) active_record_store: Arc<dyn ActiveRecordPort>,
    pub(crate) termination_grace: Duration,
}

/// Built-in adapter that executes MCP-backed plugins over stdio JSON-RPC.
#[derive(Clone)]
pub struct McpAdapter {
    pub(crate) active: ActiveInvocations<McpInvocationState>,
    pub(crate) active_record_store: Arc<dyn ActiveRecordPort>,
    pub(crate) termination_grace: Duration,
    pub(crate) preflight_probe_concurrency: usize,
}

impl ProcessAdapter {
    /// Creates a process adapter with the active-invocation persistence port it must use.
    pub fn new(active_record_store: Arc<dyn ActiveRecordPort>) -> Self {
        Self {
            active: ActiveInvocations::default(),
            active_record_store,
            termination_grace: Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
            preflight_probe_concurrency: DEFAULT_PREFLIGHT_PROBE_CONCURRENCY,
        }
    }
}

impl HttpAdapter {
    /// Creates an HTTP adapter with the active-invocation persistence port it must use.
    pub fn new(active_record_store: Arc<dyn ActiveRecordPort>) -> Self {
        Self {
            active: ActiveInvocations::default(),
            active_record_store,
            termination_grace: Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
        }
    }
}

impl McpAdapter {
    /// Creates an MCP adapter with the active-invocation persistence port it must use.
    pub fn new(active_record_store: Arc<dyn ActiveRecordPort>) -> Self {
        Self {
            active: ActiveInvocations::default(),
            active_record_store,
            termination_grace: Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
            preflight_probe_concurrency: DEFAULT_PREFLIGHT_PROBE_CONCURRENCY,
        }
    }
}

impl ProcessAdapter {
    /// Creates a process adapter with an explicit bounded preflight probe limit.
    pub fn with_preflight_probe_concurrency(
        active_record_store: Arc<dyn ActiveRecordPort>,
        concurrency: usize,
    ) -> anyhow::Result<Self> {
        crate::preflight::validate_preflight_probe_concurrency(concurrency)?;
        let mut adapter = Self::new(active_record_store);
        adapter.preflight_probe_concurrency = concurrency;
        Ok(adapter)
    }
}

impl McpAdapter {
    /// Creates an MCP adapter with an explicit bounded preflight probe limit.
    pub fn with_preflight_probe_concurrency(
        active_record_store: Arc<dyn ActiveRecordPort>,
        concurrency: usize,
    ) -> anyhow::Result<Self> {
        crate::preflight::validate_preflight_probe_concurrency(concurrency)?;
        let mut adapter = Self::new(active_record_store);
        adapter.preflight_probe_concurrency = concurrency;
        Ok(adapter)
    }
}
