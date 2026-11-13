// SPDX-License-Identifier: MPL-2.0
//! Process-adapter preflight probes.

use super::super::*;
use super::{PreflightChildGuard, PreparedLocalPreflightProbe, preflight_group_check};

pub(crate) fn run_process_preflight_probe(
    prepared: PreparedLocalPreflightProbe,
) -> Result<PreflightCheck> {
    let group = &prepared.group;
    let target = &group.target;
    let private = &prepared.private;
    let request = &private.request;
    let PluginEntrypoint::Process {
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
            format!(
                "process plugin '{}' has invalid entrypoint",
                target.manifest.id
            ),
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
    configure_child_process_group(&mut cmd);

    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    force_private_tmpdir(&mut cmd, &runtime_temp_environment.tmpdir);

    let timeout = request.timeout_ms.unwrap_or(30_000);
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let result = (|| -> Result<PreflightCheck> {
        let child = cmd.spawn().with_context(|| {
            format!(
                "failed spawning process plugin '{}' from '{}'",
                target.manifest.id,
                command_path.display()
            )
        })?;
        transist_core::record_preflight_probe_batch(AdapterKind::Process, 0, 1);
        let child = Arc::new(Mutex::new(child));
        let _child_guard = PreflightChildGuard::new(Arc::clone(&child));
        let (mut stdin, stdout, stderr) = {
            let mut child = child.lock().unwrap_or_else(|error| error.into_inner());
            (
                child.stdin.take().context("plugin stdin unavailable")?,
                child.stdout.take().context("plugin stdout unavailable")?,
                child.stderr.take().context("plugin stderr unavailable")?,
            )
        };
        let lines = spawn_line_reader(stdout);
        let stderr_handle = spawn_stderr_reader(stderr);
        let probe = (|| -> Result<()> {
            write_json_line(
                &mut stdin,
                &AppHandshakeRequest {
                    message_type: APP_HANDSHAKE_REQUEST_TYPE_V1.to_string(),
                    protocol_version: APP_PROTOCOL_VERSION_V1.to_string(),
                },
            )?;
            let handshake_line = recv_line(&lines, deadline, "handshake response")?;
            let handshake: AppHandshakeResponse = parse_json_line(&handshake_line)?;
            validate_handshake(&target.manifest, request, &target.action, &handshake)?;
            Ok(())
        })();
        drop(stdin);
        let _ = terminate_child(&child, Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS));
        let _ = wait_for_exit(&child, Instant::now() + Duration::from_millis(100));
        let exit_status = child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten());
        let _ = join_stderr(stderr_handle);
        if let Err(error) = probe {
            // Stderr is untrusted plugin output and may contain secrets, so it
            // is deliberately not surfaced in durable preflight diagnostics.
            // This status is observed after cleanup; it does not establish the
            // original exit cause or who sent a terminating signal.
            if let Some(status) = exit_status {
                bail!("{error}; plugin process exited with {status}");
            }
            return Err(error);
        }
        Ok(preflight_group_check(
            group,
            "adapter_handshake",
            PreflightStatus::Passed,
            "APP handshake succeeded",
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
