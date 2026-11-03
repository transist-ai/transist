// SPDX-License-Identifier: MPL-2.0
//! Process-adapter execution and preflight behavior.

use crate::*;

impl ExecutionPort for ProcessAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            actions: vec!["process".to_string()],
            supports_cancel: Some(true),
            supports_health: Some(false),
        }
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        let request = &invocation.request;
        let start = Instant::now();
        let timeout = request.timeout_ms.unwrap_or(30_000);
        let inactivity_timeout = Duration::from_millis(timeout);
        let handshake_deadline = Instant::now() + inactivity_timeout;
        let (manifest, plugin_dir, action) = load_plugin(invocation, AdapterKind::Process)?;
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
        let PluginEntrypoint::Process {
            command,
            args,
            cwd,
            env,
        } = &manifest.entrypoint
        else {
            bail!("process plugin '{}' has invalid entrypoint", manifest.id);
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
        configure_child_process_group(&mut cmd);

        let mut process_secrets = build_process_secret_env(
            Path::new(&request.paths.run_root),
            &action,
            &invocation.secret_values,
        )?;
        for (key, value) in &process_secrets.env {
            cmd.env(key, value);
        }
        // The macOS sandbox policy enforces the real security boundary (file
        // read/write access). Restricting PATH on top of that is redundant and
        // breaks plugins that rely on user-installed CLIs. Inherit the parent
        // process PATH so any tool reachable by the operator is reachable inside
        // the sandbox too, while still being bound by the filesystem policy.
        if sandbox_used {
            if let Ok(path) = std::env::var("PATH") {
                cmd.env("PATH", path);
            }
        }
        // This is intentionally last: manifests, workflow configuration, user
        // environment, and secret delivery must never select a shared temp root.
        force_private_tmpdir(&mut cmd, &runtime_temp_environment.tmpdir);

        let mut child = match cmd.spawn() {
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
                    "failed spawning process plugin '{}' from '{}': {error}",
                    manifest.id,
                    command_path.display()
                );
            }
        };
        let mut stdin = child.stdin.take().context("plugin stdin unavailable")?;
        let stdout = child.stdout.take().context("plugin stdout unavailable")?;
        let stderr = child.stderr.take().context("plugin stderr unavailable")?;
        let child = Arc::new(Mutex::new(child));
        let active = Arc::new(ProcessInvocationState {
            process: crate::active_invocation::LocalProcess::new(child.clone())?,
            cancelled: AtomicBool::new(false),
        });
        self.active
            .register(request.invoke_id.clone(), active.clone());
        if let Err(err) = publish_active_invocation(
            self.active_record_store.as_ref(),
            request,
            AdapterKind::Process,
            &child,
        ) {
            // Persistence failure is non-fatal: the in-memory registry still
            // allows in-process cancel, and the next persisted state update
            // will surface the IO error path. Log via stderr so it's visible
            // without aborting the spawn we just succeeded at.
            eprintln!(
                "warning: failed to publish active invocation record for {}: {err}",
                request.invoke_id
            );
        }
        let lines = spawn_line_reader(stdout);
        let mut stderr_handle = Some(spawn_stderr_reader(stderr));

        // Single cleanup path for any failure encountered during the protocol
        // exchange: drains stderr, removes bookkeeping, terminates the child,
        // cleans temp files, and builds a failure-or-cancelled result.
        let mut fail = |stderr_handle: &mut Option<_>,
                        code: CanonicalErrorCode,
                        message: &str,
                        diagnostic: Option<String>,
                        exit_code: Option<i32>|
         -> Result<ExecutionResult> {
            let backend = strict_local_sandbox_backend_label();
            let sandbox_wrapper_exited = sandbox_used
                && matches!(
                    code,
                    CanonicalErrorCode::TransportError | CanonicalErrorCode::TimeoutError
                )
                && active.process.has_exited().unwrap_or(false);
            active.process.terminate(self.termination_grace)?;
            self.active.remove(&request.invoke_id);
            let external_cancel =
                external_cancel_observed(self.active_record_store.as_ref(), request);
            clear_active_invocation(self.active_record_store.as_ref(), request);
            let stderr_text = stderr_handle
                .take()
                .map(|h| redact_text(&join_stderr(h), &invocation.secret_values))
                .unwrap_or_default();
            cleanup_temp_files(&mut process_secrets.temp_files);
            if sandbox_wrapper_exited
                && stderr_matches_sandbox_wrapper_failure(backend, &stderr_text)
            {
                let detail = if stderr_text.trim().is_empty() {
                    format!("sandbox wrapper failed: {message}")
                } else {
                    format!("sandbox wrapper failed: {message}; stderr: {stderr_text}")
                };
                return Ok(sandbox_failure_result(
                    request,
                    backend,
                    &detail,
                    &sandbox_ignore_paths,
                ));
            }
            Ok(process_failure_or_cancelled(
                request,
                &active,
                code,
                message,
                append_stderr_diagnostic(stderr_text, diagnostic),
                exit_code,
                start.elapsed().as_millis() as u64,
                external_cancel,
            ))
        };

        let handshake = AppHandshakeRequest {
            message_type: APP_HANDSHAKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: APP_PROTOCOL_VERSION_V1.to_string(),
        };
        if let Err(err) = write_json_line(&mut stdin, &handshake) {
            return fail(
                &mut stderr_handle,
                CanonicalErrorCode::TransportError,
                &format!("failed writing handshake: {err}"),
                None,
                Some(2),
            );
        }
        let handshake_line = match recv_line(&lines, handshake_deadline, "handshake response") {
            Ok(line) => line,
            Err(err) => {
                return fail(
                    &mut stderr_handle,
                    CanonicalErrorCode::TimeoutError,
                    &err.to_string(),
                    None,
                    Some(124),
                );
            }
        };
        let handshake_response: AppHandshakeResponse = match parse_json_line(&handshake_line) {
            Ok(value) => value,
            Err(err) => {
                return fail(
                    &mut stderr_handle,
                    CanonicalErrorCode::ProtocolError,
                    &err.to_string(),
                    None,
                    Some(2),
                );
            }
        };
        if let Err(err) = validate_handshake(&manifest, request, &action, &handshake_response) {
            return fail(
                &mut stderr_handle,
                CanonicalErrorCode::ProtocolError,
                &err.to_string(),
                None,
                Some(2),
            );
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
        if let Err(err) = write_json_line(&mut stdin, &invoke) {
            return fail(
                &mut stderr_handle,
                CanonicalErrorCode::TransportError,
                &format!("failed writing invoke request: {err}"),
                None,
                Some(2),
            );
        }
        drop(stdin);

        let app_result =
            match recv_invoke_result(&lines, inactivity_timeout, request.max_timeout_ms, request) {
                Ok(result) => result,
                Err(err) => {
                    let (code, exit_code, message, diagnostic) = match err {
                        InvokeResultReceiveError::NoTerminalFrame { message } => {
                            (CanonicalErrorCode::TimeoutError, Some(124), message, None)
                        }
                        InvokeResultReceiveError::Protocol { message, raw_line } => (
                            CanonicalErrorCode::ProtocolError,
                            Some(2),
                            format!("APP invoke-result protocol error: {message}"),
                            format_invoke_result_line_diagnostic(
                                raw_line,
                                &invocation.secret_values,
                            ),
                        ),
                        InvokeResultReceiveError::Transport { message } => {
                            (CanonicalErrorCode::TransportError, Some(2), message, None)
                        }
                        InvokeResultReceiveError::Internal { message } => (
                            CanonicalErrorCode::AdapterInternalError,
                            Some(2),
                            message,
                            None,
                        ),
                    };
                    return fail(&mut stderr_handle, code, &message, diagnostic, exit_code);
                }
            };
        let validation = crate::process_lifecycle::validate_terminal_result(
            request,
            &plugin_dir,
            &action,
            &app_result,
        );
        let exit_deadline = Instant::now() + self.termination_grace;
        let exit_observation = active.process.wait_for_exit(exit_deadline);
        if !matches!(exit_observation, Ok(true)) {
            let snapshot = active.process.lifecycle_snapshot();
            let cleanup_confirmed = active.process.terminate(self.termination_grace).is_ok();
            // Unconfirmed cleanup may leave inherited stderr open indefinitely.
            // Detach its reader rather than joining a live boundary.
            if cleanup_confirmed {
                if let Some(handle) = stderr_handle.take() {
                    let _ = join_stderr(handle);
                }
            }
            cleanup_temp_files(&mut process_secrets.temp_files);
            let outcome = crate::process_lifecycle::lifecycle_failure(
                invocation,
                app_result,
                validation,
                snapshot,
                cleanup_confirmed,
                exit_observation.is_err(),
                self.termination_grace,
                start.elapsed().as_millis() as u64,
            );
            // Keep the registered cancellation target through evidence creation.
            // Cancellation may arrive while schema/hint/file work is in progress.
            let external_cancel =
                external_cancel_observed(self.active_record_store.as_ref(), request);
            if cleanup_confirmed {
                self.active.remove(&request.invoke_id);
                clear_active_invocation(self.active_record_store.as_ref(), request);
            }
            if active.cancelled.load(Ordering::SeqCst) || external_cancel {
                let mut result = cancelled_result(
                    request,
                    "process plugin invocation cancelled",
                    start.elapsed().as_millis() as u64,
                );
                // A canceled execution can retain runtime diagnostics, but never
                // promote the plugin's returned output or its staged hints.
                if let Ok(outcome) = outcome {
                    result.evidence = outcome
                        .evidence
                        .into_iter()
                        .filter(|file| {
                            Path::new(&file.path).file_name().is_some_and(|name| {
                                name == "transist_process_lifecycle_evidence.json"
                            })
                        })
                        .collect();
                }
                return Ok(result);
            }
            return outcome;
        }
        let stderr_handle = stderr_handle.expect("stderr handle consumed only on failure paths");
        let stderr_text = redact_text(&join_stderr(stderr_handle), &invocation.secret_values);
        cleanup_temp_files(&mut process_secrets.temp_files);
        let outcome = if let Some(error) = validation.failure {
            Ok(failure_result(
                request,
                error.code,
                &redact_text(&error.message, &invocation.secret_values),
                stderr_text,
                Some(2),
                start.elapsed().as_millis() as u64,
            ))
        } else {
            app_result_to_execution_result(
                request,
                &plugin_dir,
                &action,
                &invocation.secret_values,
                app_result,
                stderr_text,
                start.elapsed().as_millis() as u64,
            )
        };
        let external_cancel = external_cancel_observed(self.active_record_store.as_ref(), request);
        self.active.remove(&request.invoke_id);
        clear_active_invocation(self.active_record_store.as_ref(), request);
        if active.cancelled.load(Ordering::SeqCst) || external_cancel {
            return Ok(cancelled_result(
                request,
                "process plugin invocation cancelled",
                start.elapsed().as_millis() as u64,
            ));
        }
        outcome
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        let Some(active) = self.active.get(&request.invoke_id) else {
            return Ok(ExecutionCancelResult {
                invoke_id: request.invoke_id.clone(),
                accepted: false,
            });
        };
        active.cancelled.store(true, Ordering::SeqCst);
        active.process.terminate(self.termination_grace)?;
        Ok(ExecutionCancelResult {
            invoke_id: request.invoke_id.clone(),
            accepted: true,
        })
    }
}

fn append_stderr_diagnostic(stderr: String, diagnostic: Option<String>) -> String {
    match diagnostic {
        None => stderr,
        Some(diagnostic) if stderr.trim().is_empty() => diagnostic,
        Some(diagnostic) => format!("{stderr}\n{diagnostic}"),
    }
}

fn format_invoke_result_line_diagnostic(
    raw_line: Option<String>,
    secret_values: &BTreeMap<String, String>,
) -> Option<String> {
    raw_line
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let redacted = redact_text(value, secret_values);
            let preview = truncate_diagnostic_preview(&redacted, 240);
            format!("offending APP invoke-result line: {preview}")
        })
}

fn truncate_diagnostic_preview(value: &str, limit: usize) -> String {
    let normalized = value.replace('\n', "\\n").replace('\r', "\\r");
    let mut preview = normalized.chars().take(limit).collect::<String>();
    if normalized.chars().count() > limit {
        preview.push_str("...");
    }
    preview
}

impl PreflightPort for ProcessAdapter {
    fn validate_deferred_input(
        &self,
        invocation: &PreflightInvocation,
        deferred_paths: &[String],
    ) -> Result<Vec<PreflightCheck>> {
        if invocation.request.adapter_kind != AdapterKind::Process {
            bail!("expected process adapter for deferred validation");
        }
        crate::preflight::validate_deferred_preflight_input(invocation, deferred_paths, None)
    }
    fn preflight_steps(&self, invocations: &[PreflightInvocation]) -> Result<Vec<PreflightCheck>> {
        let mut checks = Vec::new();
        let mut groups = BTreeMap::<String, PreflightProbeGroup>::new();
        for invocation in invocations {
            let (target, static_checks) =
                match validate_static_preflight_target(invocation, AdapterKind::Process) {
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

        transist_core::record_preflight_probe_batch(AdapterKind::Process, groups.len(), 0);

        checks.extend(run_local_preflight_probe_groups(
            groups.into_values().collect(),
            self.preflight_probe_concurrency,
            AdapterKind::Process,
        )?);
        Ok(checks)
    }
}
