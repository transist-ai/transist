// SPDX-License-Identifier: MPL-2.0
//! MCP-adapter preflight probes.

use super::super::*;
use super::{PreflightChildGuard, PreparedLocalPreflightProbe, preflight_group_check};

pub(crate) fn run_mcp_preflight_probe(
    prepared: PreparedLocalPreflightProbe,
) -> Result<PreflightCheck> {
    let group = &prepared.group;
    let target = &group.target;
    let private = &prepared.private;
    let request = &private.request;
    let tool_name = target.tool_name.as_deref().unwrap_or_default();
    let PluginEntrypoint::Mcp {
        command,
        args,
        cwd,
        env,
    } = &target.manifest.entrypoint
    else {
        return Ok(preflight_group_check(
            group,
            "adapter_handshake",
            PreflightStatus::Failed,
            format!("mcp plugin '{}' has invalid entrypoint", target.manifest.id),
        ));
    };

    let command_path = target.plugin_dir.join(command);
    let execution_cwd = resolve_execution_cwd(request, &target.plugin_dir, cwd.as_deref())?;
    let runtime_temp_environment = prepare_runtime_temp_environment(request, env)?;
    let mut cmd = match make_sandboxed_command(&command_path, args, &prepared.sandbox_plan) {
        Ok(command) => command,
        Err(error) => {
            return Ok(preflight_group_check(
                group,
                "adapter_handshake",
                PreflightStatus::Failed,
                format!("failed to create sandboxed command: {error}"),
            ));
        }
    };
    apply_managed_child_env_policy(&mut cmd, request);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.current_dir(&execution_cwd);
    apply_runtime_temp_environment(&mut cmd, &runtime_temp_environment);
    inject_runtime_paths_env(&mut cmd, request);
    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    force_private_tmpdir(&mut cmd, &runtime_temp_environment.tmpdir);
    configure_child_process_group(&mut cmd);

    let timeout = request.timeout_ms.unwrap_or(30_000);
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let result = (|| -> Result<PreflightCheck> {
        let child = cmd.spawn().with_context(|| {
            format!(
                "failed spawning MCP plugin '{}' command '{}'",
                target.manifest.id, command
            )
        })?;
        transist_core::record_preflight_probe_batch(AdapterKind::Mcp, 0, 1);
        let child = Arc::new(Mutex::new(child));
        let _child_guard = PreflightChildGuard::new(Arc::clone(&child));
        let stdin = {
            let mut guard = child.lock().unwrap_or_else(|e| e.into_inner());
            McpStdin::new(guard.stdin.take().context("mcp stdin unavailable")?)
        };
        let stdout = {
            let mut guard = child.lock().unwrap_or_else(|e| e.into_inner());
            guard.stdout.take().context("mcp stdout unavailable")?
        };
        let stderr = {
            let mut guard = child.lock().unwrap_or_else(|e| e.into_inner());
            guard.stderr.take().context("mcp stderr unavailable")?
        };
        let messages = spawn_mcp_reader(stdout);
        let stderr_handle = spawn_stderr_reader(stderr);
        let probe = mcp_initialize_and_list_tools(&stdin, &messages, deadline);
        stdin.close();
        let _ = terminate_child(&child, Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS));
        let _ = wait_for_exit(&child, Instant::now() + Duration::from_millis(100));
        let _ = join_stderr(stderr_handle);
        let tools = probe?;
        if !tools.iter().any(|name| name == tool_name) {
            return Ok(preflight_group_check(
                group,
                "adapter_handshake",
                PreflightStatus::Failed,
                format!("MCP tool '{}' is not available", tool_name),
            ));
        }
        Ok(preflight_group_check(
            group,
            "adapter_handshake",
            PreflightStatus::Passed,
            format!(
                "MCP initialize/tools.list succeeded and tool '{}' is available",
                tool_name
            ),
        ))
    })();
    match result {
        Ok(check) => Ok(check),
        Err(error) => Ok(preflight_group_check(
            group,
            "adapter_handshake",
            PreflightStatus::Failed,
            error.to_string(),
        )),
    }
}
