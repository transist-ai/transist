// SPDX-License-Identifier: MPL-2.0
//! HTTP-adapter execution and preflight behavior.

use super::*;

impl ExecutionPort for HttpAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            actions: vec!["http".to_string()],
            supports_cancel: Some(true),
            supports_health: Some(true),
        }
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        let request = &invocation.request;
        let start = Instant::now();
        let (manifest, plugin_dir, action) = load_plugin(invocation, AdapterKind::Http)?;
        if !invocation.secret_values.is_empty()
            && !invocation
                .remote_secret_policy
                .allows_adapter_kind(request.adapter_kind.as_str())
        {
            return Ok(failure_result(
                request,
                CanonicalErrorCode::ConfigurationError,
                "remote secret delivery is denied by policy for adapter 'http'",
                String::new(),
                Some(2),
                0,
            ));
        }
        let PluginEntrypoint::Http { base_url, ca_cert } = &manifest.entrypoint else {
            bail!("http plugin '{}' has invalid entrypoint", manifest.id);
        };
        let action_headers = build_http_header_bindings(&action, &invocation.secret_values)?;
        let action_input = project_action_input(&action, &request.input)?;
        let action_input =
            inject_http_action_secrets(&action_input, &action, &invocation.secret_values)?;
        if let Err(error) =
            transist_plugins::validate_action_input(&plugin_dir, &action, &action_input)
        {
            return Ok(failure_result(
                request,
                CanonicalErrorCode::ValidationError,
                &error.to_string(),
                String::new(),
                Some(2),
                0,
            ));
        }
        let ca_cert = ca_cert.as_ref().map(|path| plugin_dir.join(path));
        let network_policy = match network_policy_from_request(request) {
            Ok(policy) => policy.unwrap_or_default(),
            Err(error) => {
                return Ok(failure_result(
                    request,
                    CanonicalErrorCode::ConfigurationError,
                    &error,
                    String::new(),
                    Some(2),
                    0,
                ));
            }
        };
        let http_policy = HttpRequestPolicy::from_action(network_policy, &action);
        if let Err(err) = perform_http_handshake(
            base_url,
            ca_cert.as_deref(),
            request,
            &manifest,
            &action,
            &http_policy,
        ) {
            let err_str = err.to_string();
            return Ok(failure_result(
                request,
                if err_str.starts_with("http handshake transport failure:") {
                    CanonicalErrorCode::TransportError
                } else {
                    CanonicalErrorCode::ProtocolError
                },
                &err_str,
                String::new(),
                Some(2),
                start.elapsed().as_millis() as u64,
            ));
        }
        let invoke = AppInvokeRequest {
            message_type: APP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: APP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: request.invoke_id.clone(),
            run_id: request.run_id.clone(),
            workflow_id: request.workflow_id.clone(),
            step: protocol::AppInvokeStep {
                id: request.step.id.clone(),
                execution: request.step.execution,
            },
            plugin: AppPluginIdentity {
                id: manifest.id.clone(),
                version: manifest.version.clone(),
            },
            action: request.action.clone(),
            input: action_input,
            paths: request.paths.clone(),
            requested_capabilities: request.requested_capabilities.clone(),
            policy: request.policy.clone(),
            timeout_ms: request.timeout_ms,
            max_timeout_ms: request.max_timeout_ms,
            metadata: request.metadata.clone(),
        };
        let body = serde_json::to_value(&invoke)?;
        let active = Arc::new(HttpInvocationState {
            cancel_requested: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            cancel_finalized: AtomicBool::new(false),
            cancel_failure: Mutex::new(None),
            cancel_config: HttpCancelConfig {
                base_url: base_url.clone(),
                ca_cert: ca_cert.clone(),
                headers: action_headers.clone(),
                policy: http_policy.clone(),
                invoke_id: request.invoke_id.clone(),
            },
        });
        self.active
            .register(request.invoke_id.clone(), active.clone());

        let response = match http_json_request(
            "POST",
            base_url,
            "/app/invoke",
            Some(body),
            &action_headers,
            HttpRequestOptions {
                ca_cert: ca_cert.as_deref(),
                timeout_ms: request.timeout_ms,
                policy: &http_policy,
            },
        ) {
            Ok(response) => response,
            Err(err) => {
                self.active.remove(&request.invoke_id);
                return Ok(failure_result(
                    request,
                    CanonicalErrorCode::TransportError,
                    &format!("http invoke transport failed: {err}"),
                    String::new(),
                    Some(2),
                    start.elapsed().as_millis() as u64,
                ));
            }
        };

        self.active.remove(&request.invoke_id);
        clear_active_invocation(self.active_record_store.as_ref(), request);
        let stderr_text = String::new();
        if active.cancel_requested.load(Ordering::SeqCst) {
            let cancel_deadline = Instant::now() + self.termination_grace;
            while !active.cancel_finalized.load(Ordering::SeqCst)
                && Instant::now() < cancel_deadline
            {
                thread::sleep(Duration::from_millis(10));
            }
        }
        if let Some((code, message)) = active
            .cancel_failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Ok(failure_result(
                request,
                code,
                &message,
                stderr_text,
                Some(2),
                start.elapsed().as_millis() as u64,
            ));
        }
        if active.cancelled.load(Ordering::SeqCst) {
            return Ok(cancelled_result(
                request,
                "http plugin invocation cancelled",
                start.elapsed().as_millis() as u64,
            ));
        }
        let app_result: AppInvokeResult = match serde_json::from_value(response) {
            Ok(value) => value,
            Err(err) => {
                return Ok(failure_result(
                    request,
                    CanonicalErrorCode::ProtocolError,
                    &format!("invalid HTTP invoke response: {err}"),
                    stderr_text,
                    Some(2),
                    start.elapsed().as_millis() as u64,
                ));
            }
        };
        app_result_to_execution_result(
            request,
            &plugin_dir,
            &action,
            &invocation.secret_values,
            app_result,
            stderr_text,
            start.elapsed().as_millis() as u64,
        )
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        let Some(active) = self.active.get(&request.invoke_id) else {
            return Ok(ExecutionCancelResult {
                invoke_id: request.invoke_id.clone(),
                accepted: false,
            });
        };
        active.cancel_requested.store(true, Ordering::SeqCst);
        let cancel = protocol::AppCancelRequest {
            message_type: protocol::APP_CANCEL_REQUEST_TYPE_V1.to_string(),
            protocol_version: APP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: request.invoke_id.clone(),
        };
        let body = serde_json::to_value(&cancel)?;
        match http_json_request(
            "POST",
            &active.cancel_config.base_url,
            "/app/cancel",
            Some(body),
            &active.cancel_config.headers,
            HttpRequestOptions {
                ca_cert: active.cancel_config.ca_cert.as_deref(),
                timeout_ms: Some(5_000),
                policy: &active.cancel_config.policy,
            },
        ) {
            Ok(response) => {
                let result: protocol::AppCancelResult =
                    serde_json::from_value(response).context("invalid HTTP cancel response")?;
                if result.message_type != protocol::APP_CANCEL_RESULT_TYPE_V1 {
                    active
                        .cancel_failure
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace((
                            CanonicalErrorCode::ProtocolError,
                            format!("invalid HTTP cancel type '{}'", result.message_type),
                        ));
                    active.cancel_finalized.store(true, Ordering::SeqCst);
                    bail!("invalid HTTP cancel type '{}'", result.message_type);
                }
                if result.protocol_version != APP_PROTOCOL_VERSION_V1
                    || result.invoke_id != active.cancel_config.invoke_id
                {
                    active
                        .cancel_failure
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace((
                            CanonicalErrorCode::ProtocolError,
                            "HTTP cancel response did not match invocation".to_string(),
                        ));
                    active.cancel_finalized.store(true, Ordering::SeqCst);
                    bail!("http cancel response did not match invocation");
                }
                if result.accepted {
                    active.cancelled.store(true, Ordering::SeqCst);
                }
                active.cancel_finalized.store(true, Ordering::SeqCst);
                Ok(ExecutionCancelResult {
                    invoke_id: request.invoke_id.clone(),
                    accepted: result.accepted,
                })
            }
            Err(err) => {
                active
                    .cancel_failure
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .replace((
                        CanonicalErrorCode::TransportError,
                        format!("http cancel transport failed: {err}"),
                    ));
                active.cancel_finalized.store(true, Ordering::SeqCst);
                Err(err)
            }
        }
    }
}

impl PreflightPort for HttpAdapter {
    fn validate_deferred_input(
        &self,
        invocation: &PreflightInvocation,
        deferred_paths: &[String],
    ) -> Result<Vec<PreflightCheck>> {
        if invocation.request.adapter_kind != AdapterKind::Http {
            bail!("expected http adapter for deferred validation");
        }
        crate::preflight::validate_deferred_preflight_input(invocation, deferred_paths, None)
    }
    fn preflight_steps(&self, invocations: &[PreflightInvocation]) -> Result<Vec<PreflightCheck>> {
        let mut checks = Vec::new();
        let mut groups = BTreeMap::<String, PreflightProbeGroup>::new();
        for invocation in invocations {
            let (target, static_checks) =
                match validate_static_preflight_target(invocation, AdapterKind::Http) {
                    Ok(result) => result,
                    Err(check) => {
                        checks.push(*check);
                        continue;
                    }
                };
            let blocked = static_checks
                .iter()
                .any(|check| check.status == PreflightStatus::Failed);
            checks.extend(static_checks);
            if blocked {
                continue;
            }
            insert_preflight_probe_target(&mut groups, target)?;
        }

        transist_core::record_preflight_probe_batch(AdapterKind::Http, groups.len(), 0);

        for group in groups.into_values() {
            checks.extend(run_http_preflight_probe(&group)?);
        }
        Ok(checks)
    }
}
