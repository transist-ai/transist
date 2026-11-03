// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::io::{self, IsTerminal, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) fn install_terminal_restore_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        force_restore_terminal();
        previous_hook(panic_info);
    }));
}

pub(crate) fn print_fatal_error(error: &anyhow::Error) {
    if io::stderr().is_terminal() {
        eprint!("\r\x1b[2K");
        let _ = io::stderr().flush();
    }
    for line in format_error_lines(error) {
        eprintln!("{}", line);
    }
}

pub(crate) fn format_error_lines(error: &anyhow::Error) -> Vec<String> {
    if let Some(binding_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<transist::WorkflowInputBindingError>())
    {
        return workflow_input_binding_error_lines(binding_error);
    }
    if let Some(uninstall_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<transist::PluginUninstallError>())
    {
        return plugin_uninstall_error_lines(uninstall_error);
    }
    if let Some(timeout) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<transist::PluginLifecycleReservationTimeout>())
    {
        return plugin_lifecycle_reservation_timeout_error_lines(timeout);
    }
    if let Some(canceled) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<transist::PluginLifecycleReservationCanceled>())
    {
        return vec![format!(
            "Run birth canceled: root run '{}' was canceled while waiting for the plugin lifecycle reservation.",
            canceled.root_run_id
        )];
    }
    if let Some(conflict) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<transist::LeaseConflict>())
    {
        return lease_conflict_error_lines(conflict);
    }

    let mut lines = vec![format!("Error: {error}")];
    let mut causes = error.chain().skip(1).peekable();
    if causes.peek().is_none() {
        return lines;
    }
    lines.push("Caused by:".to_string());
    lines.extend(causes.map(|cause| format!("  {cause}")));
    lines
}

fn workflow_input_binding_error_lines(error: &transist::WorkflowInputBindingError) -> Vec<String> {
    let mut lines = vec![
        format!("------------[prepare {}]------------", error.workflow_ref()),
        "[!] workflow input validation".to_string(),
        "    Outcome: prepare failed before run creation.".to_string(),
        format!("    Failure reason: {}", error),
        "    Compiled recipe:".to_string(),
        format!("      '{}'", error.recipe_path.display()),
        "    Declared inputs:".to_string(),
    ];
    lines.extend(
        error
            .declared_inputs
            .iter()
            .map(|name| format!("      '{}'", name)),
    );
    lines.push("    Next:".to_string());
    lines.push("      update --input/--inputs-file to match the compiled recipe".to_string());
    lines.push(
        "      recompile the workflow if the authored inputs changed after this recipe was built"
            .to_string(),
    );
    lines
}

fn plugin_uninstall_error_lines(error: &transist::PluginUninstallError) -> Vec<String> {
    match error {
        transist::PluginUninstallError::InUse {
            plugin_id,
            plugin_version,
            run_ids,
        } => {
            let target = match plugin_version {
                Some(version) => format!("{}@{}", plugin_id, version),
                None => plugin_id.clone(),
            };
            let mut lines = vec![format!(
                "------------[plugins uninstall {}]------------",
                target
            )];
            lines.push("[!] plugins uninstall blocked".to_string());
            lines.push(
                "    Outcome: uninstall rejected while active runs still reference the plugin."
                    .to_string(),
            );
            lines.push(format!("    Failure reason: {}", error));
            lines.push("    Referencing runs:".to_string());
            lines.extend(run_ids.iter().map(|run_id| format!("      '{}'", run_id)));
            lines.push("    Next:".to_string());
            lines.push(
                "      finish, cancel, or clean up the listed runs before retrying uninstall"
                    .to_string(),
            );
            lines
        }
    }
}

fn lease_conflict_error_lines(conflict: &transist::LeaseConflict) -> Vec<String> {
    let lease = &conflict.live_lease;
    let mut lines = vec![
        format!(
            "Lease conflict: root run tree '{}' is already owned.",
            conflict.root_run_id
        ),
        format!("  Owner id: {}", lease.owner_id),
        format!("  PID: {}", lease.pid),
        format!("  Host: {}", lease.hostname),
        format!("  Command: {}", lease.command),
        format!("  Acquired at (unix ms): {}", lease.acquired_at_ms),
    ];
    if let Some(external) = lease.external.as_ref() {
        lines.push(format!("  Host id: {}", external.host_id));
        if let Some(runner_id) = external.runner_id.as_deref() {
            lines.push(format!("  Runner id: {}", runner_id));
        }
        if let Some(external_run_id) = external.external_run_id.as_deref() {
            lines.push(format!("  External run id: {}", external_run_id));
        }
    }
    lines
}

fn plugin_lifecycle_reservation_timeout_error_lines(
    timeout: &transist::PluginLifecycleReservationTimeout,
) -> Vec<String> {
    let lease = &timeout.live_lease;
    vec![
        format!(
            "Plugin lifecycle reservation timed out after {} ms.",
            timeout.timeout_ms
        ),
        format!("  Owner id: {}", lease.owner_id),
        format!("  PID: {}", lease.pid),
        format!("  Host: {}", lease.hostname),
        format!("  Command: {}", lease.command),
    ]
}

pub(crate) fn force_restore_terminal() {
    // Only restore terminal state when the TUI has actually been entered.
    // Calling restore_terminal_state() unconditionally on a plain terminal
    // sends ClearAll + LeaveAlternateScreen even when no alt-screen was ever
    // opened, which (a) clears the user's scrollback and (b) — if a previous
    // crashed session left the terminal in SGR mouse-tracking mode — causes
    // the terminal to flush buffered mouse events as literal escape-sequence
    // text after the process exits.
    if crate::ui::fullscreen::fullscreen_prompt_active() {
        crate::ui::fullscreen::restore_terminal_state();
    }
}

pub(crate) fn install_interrupt_handler() -> Result<Arc<AtomicBool>> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let handler_flag = interrupted.clone();
    if let Err(err) = ctrlc::set_handler(move || {
        handler_flag.store(true, Ordering::SeqCst);
    }) {
        // On macOS the standard Terminal.app can deny sigaction(SIGINT) with
        // EPERM in certain restricted or sandboxed contexts.  Treat this as a
        // non-fatal degradation: the interrupt flag will never be set by a
        // signal, so Ctrl+C will rely solely on crossterm key events inside
        // the TUI.  Warn to stderr only when it is an interactive terminal so
        // we do not pollute piped / managed-mode output.
        if io::stderr().is_terminal() {
            eprintln!(
                "Warning: could not install Ctrl+C handler: {err}\n  \
                 Interrupt detection via signal is disabled; \
                 use Ctrl+C inside the TUI or kill the process manually."
            );
        }
    }
    Ok(interrupted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn fatal_error_lines_render_workflow_input_binding_failures_as_structured_start_diagnostics() {
        let error = anyhow::Error::new(transist::WorkflowInputBindingError {
            workflow_id: "backend_development_plan_implementation".to_string(),
            workflow_version: "v1".to_string(),
            recipe_path: PathBuf::from(
                "/tmp/.transist/registry/recipes/backend_development_plan_implementation-v1.yaml",
            ),
            declared_inputs: vec![
                "approved_plan_dir".to_string(),
                "branch_name".to_string(),
                "repo_path".to_string(),
                "worktree_path".to_string(),
            ],
            kind: transist::WorkflowInputBindingErrorKind::UndeclaredInput {
                input_name: "worktree_root".to_string(),
            },
        });

        let lines = format_error_lines(&error);

        assert_eq!(
            lines[0],
            "------------[prepare backend_development_plan_implementation@v1]------------"
        );
        assert_eq!(lines[1], "[!] workflow input validation");
        assert_eq!(lines[2], "    Outcome: prepare failed before run creation.");
        assert!(lines[3].contains("worktree_root"));
        assert!(lines.iter().any(|line| line == "      'worktree_path'"));
        assert!(lines.iter().any(|line| {
            line == "      update --input/--inputs-file to match the compiled recipe"
        }));
    }

    #[test]
    fn fatal_error_lines_render_lease_conflicts_with_owner_metadata() {
        let error = anyhow::Error::new(transist::LeaseConflict {
            root_run_id: "run-1".to_string(),
            live_lease: transist::RunTreeLeaseRecord {
                schema_version: transist::RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
                root_run_id: "run-1".to_string(),
                lease_id: "lease-1".to_string(),
                owner_id: "owner-1".to_string(),
                management_mode: transist::ManagementMode::Managed,
                pid: 42,
                hostname: "host-a".to_string(),
                command: "transist workflows run run-1".to_string(),
                transist_version: env!("CARGO_PKG_VERSION").to_string(),
                acquired_at_ms: 123,
                last_heartbeat_ms: 124,
                external: Some(transist::ExternalOwnerMetadata {
                    host_id: "host-a".to_string(),
                    runner_id: Some("runner-1".to_string()),
                    external_run_id: Some("run-ext-1".to_string()),
                }),
            },
        });

        let lines = format_error_lines(&error);

        assert!(lines[0].contains("Lease conflict"));
        assert!(lines.iter().any(|line| line.contains("PID: 42")));
        assert!(lines.iter().any(|line| line.contains("Host id: host-a")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("External run id: run-ext-1"))
        );
    }

    #[test]
    fn fatal_error_lines_render_plugin_lifecycle_reservation_timeout_metadata() {
        let error = anyhow::Error::new(transist::PluginLifecycleReservationTimeout {
            timeout_ms: 15_000,
            live_lease: transist::RunTreeLeaseRecord {
                schema_version: transist::RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
                root_run_id: "plugin-lifecycle-reservation".to_string(),
                lease_id: "lease-1".to_string(),
                owner_id: "owner-1".to_string(),
                management_mode: transist::ManagementMode::Standalone,
                pid: 42,
                hostname: "host-a".to_string(),
                command: "transist plugins install".to_string(),
                transist_version: env!("CARGO_PKG_VERSION").to_string(),
                acquired_at_ms: 123,
                last_heartbeat_ms: 124,
                external: None,
            },
        });

        let lines = format_error_lines(&error);

        assert!(lines[0].contains("15000 ms"));
        assert!(lines.iter().any(|line| line.contains("Owner id: owner-1")));
        assert!(lines.iter().any(|line| line.contains("PID: 42")));
        assert!(lines.iter().any(|line| line.contains("Host: host-a")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Command: transist plugins install"))
        );
    }

    #[test]
    fn fatal_error_lines_render_plugin_uninstall_in_use_as_structured_diagnostics() {
        let error = anyhow::Error::new(transist::PluginUninstallError::InUse {
            plugin_id: "shell".to_string(),
            plugin_version: Some("0.1.0".to_string()),
            run_ids: vec!["run-a".to_string(), "run-b".to_string()],
        });

        let lines = format_error_lines(&error);

        assert_eq!(
            lines[0],
            "------------[plugins uninstall shell@0.1.0]------------"
        );
        assert_eq!(lines[1], "[!] plugins uninstall blocked");
        assert!(lines[2].contains("uninstall rejected"));
        assert!(lines[3].contains("cannot uninstall plugin 'shell' version '0.1.0'"));
        assert!(lines.iter().any(|line| line == "      'run-a'"));
        assert!(lines.iter().any(|line| line == "      'run-b'"));
    }

    #[test]
    fn fatal_error_lines_fall_back_to_generic_error_rendering() {
        let error = anyhow::anyhow!("plain failure");
        let lines = format_error_lines(&error);
        assert_eq!(lines, vec!["Error: plain failure".to_string()]);
    }
}
