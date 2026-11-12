// SPDX-License-Identifier: MPL-2.0
//! Adapter-router composition for dispatching to built-in or registered adapters.

use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::Arc;
use transist_core::ports::{
    ActiveRecordPort, ExecutionCancelRequest, ExecutionCancelResult, ExecutionInvocation,
    ExecutionPort, PreflightInvocation, PreflightPort,
};
use transist_protocol::{
    AdapterCapabilities, AdapterKind, CanonicalErrorCode, ExecutionResult, InvokeTiming,
    ProtocolError, TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
};

use crate::preflight::preflight_probe_concurrency_from_env;
use crate::timing::InvocationTimer;
use crate::{HttpAdapter, McpAdapter, ProcessAdapter, preflight_check};

#[derive(Default)]
/// Router that dispatches execution and preflight requests by adapter kind.
pub struct AdapterRouter {
    adapters: BTreeMap<AdapterKind, Arc<dyn ExecutionPort>>,
    preflight_adapters: BTreeMap<AdapterKind, Arc<dyn PreflightPort>>,
}

impl AdapterRouter {
    /// Creates an empty router with no registered adapters.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Creates a router populated with the built-in process, HTTP, and MCP adapters.
    pub fn with_builtin_adapters(active_record_store: Arc<dyn ActiveRecordPort>) -> Result<Self> {
        let preflight_probe_concurrency = preflight_probe_concurrency_from_env()?;
        Self::with_builtin_adapters_and_preflight_probe_concurrency(
            active_record_store,
            preflight_probe_concurrency,
        )
    }

    fn with_builtin_adapters_and_preflight_probe_concurrency(
        active_record_store: Arc<dyn ActiveRecordPort>,
        preflight_probe_concurrency: usize,
    ) -> Result<Self> {
        let mut router = Self::empty();
        router.register(
            AdapterKind::Process,
            Arc::new(ProcessAdapter::with_preflight_probe_concurrency(
                Arc::clone(&active_record_store),
                preflight_probe_concurrency,
            )?),
        );
        router.register(
            AdapterKind::Http,
            Arc::new(HttpAdapter::new(Arc::clone(&active_record_store))),
        );
        router.register(
            AdapterKind::Mcp,
            Arc::new(McpAdapter::with_preflight_probe_concurrency(
                active_record_store,
                preflight_probe_concurrency,
            )?),
        );
        Ok(router)
    }

    /// Registers an adapter under the supplied [`AdapterKind`].
    pub fn register<T>(&mut self, adapter_kind: AdapterKind, adapter: Arc<T>)
    where
        T: ExecutionPort + PreflightPort + 'static,
    {
        let execution_adapter: Arc<dyn ExecutionPort> = adapter.clone();
        let preflight_adapter: Arc<dyn PreflightPort> = adapter;
        self.adapters.insert(adapter_kind, execution_adapter);
        self.preflight_adapters
            .insert(adapter_kind, preflight_adapter);
    }

    /// Returns the adapter kinds currently registered for execution dispatch.
    pub fn registered_kinds(&self) -> Vec<AdapterKind> {
        self.adapters.keys().copied().collect()
    }
}

fn registered_kinds_label(kinds: &[AdapterKind]) -> String {
    let labels: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    labels.join(", ")
}

impl ExecutionPort for AdapterRouter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            actions: self
                .registered_kinds()
                .into_iter()
                .map(|kind| kind.to_string())
                .collect(),
            supports_cancel: Some(true),
            supports_health: Some(true),
        }
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        let timer = InvocationTimer::new();
        let request = &invocation.request;
        let Some(adapter) = self.adapters.get(&request.adapter_kind) else {
            return Ok(router_failure_result(
                request,
                CanonicalErrorCode::ValidationError,
                &format!(
                    "unknown adapter kind '{}'; registered kinds: {}",
                    request.adapter_kind,
                    registered_kinds_label(&self.registered_kinds())
                ),
                timer.snapshot(),
            ));
        };
        adapter.execute_step(invocation)
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        let mut last_error = None;
        for adapter in self.adapters.values() {
            match adapter.cancel_step(request) {
                Ok(result) if result.accepted => return Ok(result),
                Ok(_) => {}
                Err(err) => last_error = Some(err),
            }
        }
        if let Some(err) = last_error {
            return Err(err);
        }
        Ok(ExecutionCancelResult {
            invoke_id: request.invoke_id.clone(),
            accepted: false,
        })
    }
}

impl PreflightPort for AdapterRouter {
    fn validate_deferred_input(
        &self,
        invocation: &PreflightInvocation,
        deferred_paths: &[String],
    ) -> Result<Vec<transist_core::PreflightCheck>> {
        crate::preflight::validate_deferred_preflight_input(invocation, deferred_paths, None)
    }
    fn preflight_steps(
        &self,
        invocations: &[PreflightInvocation],
    ) -> Result<Vec<transist_core::PreflightCheck>> {
        let mut grouped = BTreeMap::<AdapterKind, Vec<PreflightInvocation>>::new();
        let mut checks = Vec::new();
        for invocation in invocations {
            let adapter_kind = invocation.request.adapter_kind;
            if !self.preflight_adapters.contains_key(&adapter_kind) {
                checks.push(preflight_check(
                    "adapter_probe",
                    transist_core::PreflightStatus::Failed,
                    vec![invocation.request.step.id.clone()],
                    Some(adapter_kind.to_string()),
                    invocation
                        .request
                        .metadata
                        .get("plugin_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    Some(invocation.request.action.clone()),
                    format!(
                        "unknown adapter kind '{}'; registered kinds: {}",
                        adapter_kind,
                        registered_kinds_label(&self.registered_kinds())
                    ),
                ));
                continue;
            }
            grouped
                .entry(adapter_kind)
                .or_default()
                .push(invocation.clone());
        }

        for (adapter_kind, grouped_invocations) in grouped {
            let Some(adapter) = self.preflight_adapters.get(&adapter_kind) else {
                continue;
            };
            checks.extend(adapter.preflight_steps(&grouped_invocations)?);
        }
        Ok(checks)
    }
}

fn router_failure_result(
    request: &transist_protocol::ExecutionRequest,
    code: CanonicalErrorCode,
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
        status: "failed".to_string(),
        exit_code: Some(2),
        stdout: String::new(),
        stderr: String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use transist_storage_local::LocalActiveRecordStore;

    #[test]
    fn builtin_router_rejects_invalid_preflight_concurrency_before_registration() {
        for concurrency in [0, 33] {
            let result = AdapterRouter::with_builtin_adapters_and_preflight_probe_concurrency(
                Arc::new(LocalActiveRecordStore),
                concurrency,
            );
            let error = match result {
                Ok(_) => panic!("invalid concurrency must fail construction"),
                Err(error) => error,
            };

            assert!(error.to_string().contains("1..=32"), "{error:#}");
        }
    }
}
