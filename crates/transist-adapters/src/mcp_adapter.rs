// SPDX-License-Identifier: MPL-2.0
//! MCP-adapter execution and preflight behavior.

use crate::*;

impl ExecutionPort for McpAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            actions: vec!["mcp".to_string()],
            supports_cancel: Some(true),
            supports_health: Some(false),
        }
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        let request = &invocation.request;
        let start = Instant::now();
        if !invocation.secret_values.is_empty()
            && !invocation
                .remote_secret_policy
                .allows_adapter_kind(request.adapter_kind.as_str())
        {
            return Ok(failure_result(
                request,
                CanonicalErrorCode::ConfigurationError,
                "remote secret delivery is denied by policy for adapter 'mcp'",
                String::new(),
                Some(2),
                0,
            ));
        }
        let (manifest, plugin_dir, action) = load_plugin(invocation, AdapterKind::Mcp)?;
        let action_input = project_action_input(&action, &request.input)?;
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
        let tool_name = mcp_tool_name(&action)?;
        let PluginEntrypoint::Mcp {
            command,
            args,
            cwd,
            env,
        } = &manifest.entrypoint
        else {
            bail!("mcp plugin '{}' has invalid entrypoint", manifest.id);
        };

        let command_path = plugin_dir.join(command);
        let execution_cwd = resolve_execution_cwd(request, &plugin_dir, cwd.as_deref())?;
        let runtime_temp_environment = prepare_runtime_temp_environment(request, env)?;
        let sandbox_context = sandbox_context_from_request(request, &plugin_dir);

        let sandbox_used = true;
        let sandbox_ignore_paths: Vec<String> = Vec::new();
        let mut cmd = {
            let filesystem_policy = match filesystem_policy_from_request(request) {
                Ok(policy) => policy.unwrap_or_default(),
                Err(msg) => {
                    return Ok(sandbox_failure_result(request, "none", &msg, &[]));
                }
            };
            let network_policy = match network_policy_from_request(request) {
                Ok(policy) => policy.unwrap_or_default(),
                Err(msg) => {
                    return Ok(sandbox_failure_result(request, "none", &msg, &[]));
                }
            };
            let plan =
                match build_sandbox_plan(&filesystem_policy, &network_policy, &sandbox_context) {
                    Ok(result) => result,
                    Err(msg) => {
                        return Ok(sandbox_failure_result(
                            request,
                            strict_local_sandbox_backend_label(),
                            &msg,
                            &sandbox_ignore_paths,
                        ));
                    }
                };
            match make_sandboxed_command(&command_path, args, &plan) {
                Ok(c) => c,
                Err(msg) => {
                    return Ok(sandbox_failure_result(
                        request,
                        strict_local_sandbox_backend_label(),
                        &msg,
                        &sandbox_ignore_paths,
                    ));
                }
            }
        };
        apply_managed_child_env_policy(&mut cmd, request);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.current_dir(&execution_cwd);
        apply_runtime_temp_environment(&mut cmd, &runtime_temp_environment);
        inject_runtime_paths_env(&mut cmd, request);
        let mcp_secrets = build_mcp_secret_bindings(&action, &invocation.secret_values)
            .with_context(|| {
                format!(
                    "failed building secret bindings for MCP action '{}'",
                    action.name
                )
            })?;
        for (key, value) in &mcp_secrets.env {
            cmd.env(key, value);
        }
        cmd.args(&mcp_secrets.args);
        if sandbox_used {
            if let Ok(path) = std::env::var("PATH") {
                cmd.env("PATH", path);
            }
        }
        // Keep MCP child temporary files inside this invocation after every
        // manifest, workflow, user, and secret environment binding is applied.
        force_private_tmpdir(&mut cmd, &runtime_temp_environment.tmpdir);
        configure_child_process_group(&mut cmd);
        let timeout = request.timeout_ms.unwrap_or(30_000);
        let deadline = Instant::now() + Duration::from_millis(timeout);
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                if sandbox_used {
                    return Ok(sandbox_failure_result(
                        request,
                        strict_local_sandbox_backend_label(),
                        &format!("sandbox wrapper failed to spawn: {error}"),
                        &sandbox_ignore_paths,
                    ));
                }
                bail!(
                    "failed spawning MCP plugin '{}' command '{}': {error}",
                    manifest.id,
                    command
                );
            }
        };
        let child = Arc::new(Mutex::new(child));
        let stdin = {
            let mut guard = child.lock().unwrap_or_else(|e| e.into_inner());
            Arc::new(McpStdin::new(
                guard.stdin.take().context("mcp stdin unavailable")?,
            ))
        };
        let stdout = {
            let mut guard = child.lock().unwrap_or_else(|e| e.into_inner());
            guard.stdout.take().context("mcp stdout unavailable")?
        };
        let stderr = {
            let mut guard = child.lock().unwrap_or_else(|e| e.into_inner());
            guard.stderr.take().context("mcp stderr unavailable")?
        };
        let active = Arc::new(McpInvocationState {
            process: crate::active_invocation::LocalProcess::new(child.clone())?,
            cancelled: AtomicBool::new(false),
            stdin: stdin.clone(),
            active_request_id: Mutex::new(None),
        });
        self.active
            .register(request.invoke_id.clone(), active.clone());
        if let Err(err) = publish_active_invocation(
            self.active_record_store.as_ref(),
            request,
            AdapterKind::Mcp,
            &child,
        ) {
            eprintln!(
                "warning: failed to publish active invocation record for {}: {err}",
                request.invoke_id
            );
        }
        let messages = spawn_mcp_reader(stdout);
        let stderr_handle = spawn_stderr_reader(stderr);

        let listed_tools = match mcp_initialize_and_list_tools(&stdin, &messages, deadline) {
            Ok(tools) => tools,
            Err(err) => {
                let backend = strict_local_sandbox_backend_label();
                let sandbox_wrapper_exited =
                    sandbox_used && active.process.has_exited().unwrap_or(false);
                stdin.close();
                active.process.terminate(self.termination_grace)?;
                let stderr_text =
                    redact_text(&join_stderr(stderr_handle), &invocation.secret_values);
                self.active.remove(&request.invoke_id);
                let external_cancel =
                    external_cancel_observed(self.active_record_store.as_ref(), request);
                clear_active_invocation(self.active_record_store.as_ref(), request);
                if sandbox_wrapper_exited
                    && stderr_matches_sandbox_wrapper_failure(backend, &stderr_text)
                {
                    return Ok(sandbox_failure_result(
                        request,
                        backend,
                        &format!("sandbox wrapper failed: {err}"),
                        &sandbox_ignore_paths,
                    ));
                }
                return Ok(mcp_failure_or_cancelled(
                    request,
                    &active,
                    mcp_failure_code(&err),
                    &err.to_string(),
                    stderr_text,
                    Some(124),
                    start.elapsed().as_millis() as u64,
                    external_cancel,
                ));
            }
        };
        let has_tool = listed_tools.iter().any(|name| name == &tool_name);
        if !has_tool {
            stdin.close();
            active.process.terminate(self.termination_grace)?;
            self.active.remove(&request.invoke_id);
            let external_cancel =
                external_cancel_observed(self.active_record_store.as_ref(), request);
            clear_active_invocation(self.active_record_store.as_ref(), request);
            return Ok(mcp_failure_or_cancelled(
                request,
                &active,
                CanonicalErrorCode::ValidationError,
                &format!("MCP tool '{}' is not available", tool_name),
                redact_text(&join_stderr(stderr_handle), &invocation.secret_values),
                Some(2),
                start.elapsed().as_millis() as u64,
                external_cancel,
            ));
        }
        let request_id = next_mcp_request_id();
        active.set_active_request_id(request_id);
        let call_result = (|| {
            send_mcp_request(
                &stdin,
                request_id,
                "tools/call",
                serde_json::json!({
                    "name": tool_name,
                    "arguments": action_input,
                }),
            )?;
            recv_mcp_response(
                &stdin,
                &messages,
                deadline,
                request_id,
                "tools/call response",
            )
        })();
        let call = match call_result {
            Ok(value) => value,
            Err(err) => {
                active.cancel_active_tool_call();
                thread::sleep(Duration::from_millis(25));
                active.process.terminate(self.termination_grace)?;
                let stderr_text =
                    redact_text(&join_stderr(stderr_handle), &invocation.secret_values);
                self.active.remove(&request.invoke_id);
                let external_cancel =
                    external_cancel_observed(self.active_record_store.as_ref(), request);
                clear_active_invocation(self.active_record_store.as_ref(), request);
                return Ok(mcp_failure_or_cancelled(
                    request,
                    &active,
                    mcp_failure_code(&err),
                    &err.to_string(),
                    stderr_text,
                    Some(124),
                    start.elapsed().as_millis() as u64,
                    external_cancel,
                ));
            }
        };
        active.clear_active_request_id();
        stdin.close();
        let exited = match active.process.wait_for_exit(deadline) {
            Ok(exited) => exited,
            Err(error) => {
                active.process.terminate(self.termination_grace)?;
                self.active.remove(&request.invoke_id);
                let external_cancel =
                    external_cancel_observed(self.active_record_store.as_ref(), request);
                clear_active_invocation(self.active_record_store.as_ref(), request);
                return Ok(mcp_failure_or_cancelled(
                    request,
                    &active,
                    CanonicalErrorCode::AdapterInternalError,
                    &format!("failed waiting for MCP exit: {error}"),
                    redact_text(&join_stderr(stderr_handle), &invocation.secret_values),
                    Some(2),
                    start.elapsed().as_millis() as u64,
                    external_cancel,
                ));
            }
        };
        if !exited {
            active.process.terminate(self.termination_grace)?;
        }
        let stderr_text = redact_text(&join_stderr(stderr_handle), &invocation.secret_values);
        self.active.remove(&request.invoke_id);
        let external_cancel = external_cancel_observed(self.active_record_store.as_ref(), request);
        clear_active_invocation(self.active_record_store.as_ref(), request);
        if !exited {
            return Ok(mcp_failure_or_cancelled(
                request,
                &active,
                CanonicalErrorCode::TimeoutError,
                "MCP invocation timed out",
                stderr_text,
                Some(124),
                start.elapsed().as_millis() as u64,
                external_cancel,
            ));
        }
        if active.cancelled.load(Ordering::SeqCst) || external_cancel {
            return Ok(cancelled_result(
                request,
                "mcp invocation cancelled",
                start.elapsed().as_millis() as u64,
            ));
        }
        match mcp_tool_result_to_execution_result(
            request,
            &plugin_dir,
            &action,
            &invocation.secret_values,
            call,
            stderr_text.clone(),
            start.elapsed().as_millis() as u64,
        ) {
            Ok(result) => Ok(result),
            Err(error) => Ok(failure_result(
                request,
                CanonicalErrorCode::ProtocolError,
                &error.to_string(),
                stderr_text,
                Some(2),
                start.elapsed().as_millis() as u64,
            )),
        }
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        let Some(active) = self.active.get(&request.invoke_id) else {
            return Ok(ExecutionCancelResult {
                invoke_id: request.invoke_id.clone(),
                accepted: false,
            });
        };
        active.cancelled.store(true, Ordering::SeqCst);
        active.cancel_active_tool_call();
        thread::sleep(Duration::from_millis(25));
        active.process.terminate(self.termination_grace)?;
        Ok(ExecutionCancelResult {
            invoke_id: request.invoke_id.clone(),
            accepted: true,
        })
    }
}

fn mcp_failure_code(error: &anyhow::Error) -> CanonicalErrorCode {
    if is_mcp_protocol_error(error) {
        CanonicalErrorCode::ProtocolError
    } else {
        CanonicalErrorCode::TimeoutError
    }
}

#[allow(clippy::too_many_arguments)]
fn mcp_failure_or_cancelled(
    request: &ExecutionRequest,
    active: &McpInvocationState,
    code: CanonicalErrorCode,
    message: &str,
    stderr: String,
    exit_code: Option<i32>,
    duration_ms: u64,
    external_cancel: bool,
) -> ExecutionResult {
    if active.cancelled.load(Ordering::SeqCst) || external_cancel {
        cancelled_result(request, "mcp invocation cancelled", duration_ms)
    } else {
        failure_result(request, code, message, stderr, exit_code, duration_ms)
    }
}

impl PreflightPort for McpAdapter {
    fn validate_deferred_input(
        &self,
        invocation: &PreflightInvocation,
        deferred_paths: &[String],
    ) -> Result<Vec<PreflightCheck>> {
        if invocation.request.adapter_kind != AdapterKind::Mcp {
            bail!("expected mcp adapter for deferred validation");
        }
        crate::preflight::validate_deferred_preflight_input(invocation, deferred_paths, None)
    }
    fn preflight_steps(&self, invocations: &[PreflightInvocation]) -> Result<Vec<PreflightCheck>> {
        let mut checks = Vec::new();
        let mut groups = BTreeMap::<String, PreflightProbeGroup>::new();
        for invocation in invocations {
            let (target, static_checks) =
                match validate_static_preflight_target(invocation, AdapterKind::Mcp) {
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

        transist_core::record_preflight_probe_batch(AdapterKind::Mcp, groups.len(), 0);

        checks.extend(run_local_preflight_probe_groups(
            groups.into_values().collect(),
            self.preflight_probe_concurrency,
            AdapterKind::Mcp,
        )?);
        Ok(checks)
    }
}
