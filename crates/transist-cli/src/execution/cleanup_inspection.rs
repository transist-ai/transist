// SPDX-License-Identifier: MPL-2.0

use anyhow::Result;
use std::time::{Duration, Instant};
use transist::{InspectionCleanupSummary, TransistInspectionContext};

use crate::display::summary::format_failed_preflight_check_line;

const CLEANUP_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(120);
const CLEANUP_SETTLEMENT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Maximum time the cleanup start gate waits for a still-exiting run runner to
/// release the run-tree lease before failing with a clear message.
pub(crate) const CLEANUP_START_LEASE_TIMEOUT: Duration = Duration::from_secs(15);

/// Run a bounded wait for the run-tree lease to be free before a manual
/// `runs cleanup` dispatches its detached runner.
///
/// A canceled (or failed) run is persisted as terminal while its runner is still
/// tearing down its sandboxed step, so the run-tree lease can still be held by
/// a live process at the moment an eager `runs cleanup` is issued. The cleanup
/// runner's lease acquisition is single-shot: if it loses that race it aborts
/// and the cleanup steps are never dispatched. This gate waits (bounded) for the
/// just-exiting owner to release its lease — reaping it when the owner is dead —
/// so the subsequently spawned cleanup runner acquires the tree deterministically.
pub(crate) fn ensure_run_tree_lease_free(
    context: &transist::TransistContext,
    run_id: &str,
    timeout: Duration,
) -> Result<()> {
    let run = serde_json::Value::from(context.runs_show(run_id)?);
    let is_terminal = run
        .get("status")
        .and_then(|status| status.as_str())
        .is_some_and(|status| matches!(status, "canceled" | "failed" | "success"));
    if !is_terminal {
        return Ok(());
    }

    let poll_interval = Duration::from_millis(250);
    let deadline = Instant::now() + timeout;
    loop {
        if context.runs_lease_inspect(run_id)?.is_none() {
            return Ok(());
        }
        if Instant::now() < deadline {
            std::thread::sleep(poll_interval);
            continue;
        }
        // Deadline reached: the owner may have crashed and left a dead lease
        // behind. Reap dead owners once before failing.
        let _ = context.release_orphan_leases();
        let lease = match context.runs_lease_inspect(run_id)? {
            Some(lease) => lease,
            None => return Ok(()),
        };
        return Err(anyhow::anyhow!(
            "cannot start cleanup for '{}': run tree is still owned by an active run process (pid {}) that is finishing; retry `transist runs cleanup {}` shortly",
            run_id,
            lease.pid,
            run_id
        ));
    }
}

pub(crate) fn build_cleanup_result_page(
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<(String, String)> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let Some(detail) = inspection.get_run(run_id)? else {
        anyhow::bail!("run not found");
    };
    let Some(cleanup) = detail.cleanup.as_ref() else {
        anyhow::bail!("cleanup result not found");
    };
    Ok((
        format!("Cleanup result: {}", run_id),
        format_cleanup_result_body(cleanup),
    ))
}

pub(crate) fn run_cleanup_and_build_result_page(
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<(String, String)> {
    ensure_run_tree_lease_free(context, run_id, CLEANUP_START_LEASE_TIMEOUT)?;
    let pid = crate::runner::spawn_detached_runner(
        context.home_dir(),
        context.repo_root(),
        &crate::runner::RunnerAction::Cleanup {
            run_id: run_id.to_string(),
            trusted_execution: false,
            local_only: false,
        },
    )?;
    wait_for_cleanup_settlement(context, run_id, pid)?;
    match build_cleanup_result_page(context, run_id) {
        Ok(page) => Ok(page),
        Err(error) => {
            let log_tail = crate::runner::tail_runner_log(context.home_dir(), run_id, 8);
            let body = if log_tail.is_empty() {
                format!(
                    "Cleanup runner pid {} exited, but no cleanup result could be loaded.\n\n{}",
                    pid, error
                )
            } else {
                format!(
                    "Cleanup runner pid {} exited, but no cleanup result could be loaded.\n\n{}\n\nrunner.log:\n{}",
                    pid, error, log_tail
                )
            };
            Ok((format!("Cleanup result: {}", run_id), body))
        }
    }
}

#[allow(unsafe_code)]
fn wait_for_cleanup_settlement(
    context: &transist::TransistContext,
    run_id: &str,
    runner_pid: u32,
) -> Result<()> {
    let deadline = Instant::now() + CLEANUP_SETTLEMENT_TIMEOUT;
    loop {
        if cleanup_state(context, run_id)?
            .as_deref()
            .is_some_and(is_terminal_cleanup_state)
        {
            return Ok(());
        }

        #[cfg(unix)]
        {
            // SAFETY: `kill(pid, 0)` checks process liveness without sending
            // a signal; the pid is a validated runner pid from an active record.
            if unsafe { libc::kill(runner_pid as i32, 0) } != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(());
                }
            }
        }

        if Instant::now() >= deadline {
            return Ok(());
        }

        std::thread::sleep(CLEANUP_SETTLEMENT_POLL_INTERVAL);
    }
}

fn cleanup_state(context: &transist::TransistContext, run_id: &str) -> Result<Option<String>> {
    let run = serde_json::Value::from(context.runs_show(run_id)?);
    Ok(run
        .get("cleanup")
        .and_then(|cleanup| cleanup.get("state"))
        .and_then(|state| state.as_str())
        .map(str::to_string))
}

fn is_terminal_cleanup_state(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "skipped")
}

fn format_cleanup_result_body(cleanup: &InspectionCleanupSummary) -> String {
    format_cleanup_result_lines(cleanup).join("\n")
}

fn format_cleanup_result_lines(cleanup: &InspectionCleanupSummary) -> Vec<String> {
    let mut lines = vec!["Cleanup:".to_string()];
    lines.push(format!("  State: {}", cleanup.state));
    lines.push(format!("  Trigger: {}", cleanup.trigger));
    if let Some(started) = cleanup.started_at_ms {
        lines.push(format!("  Started: {}", format_unix_ms_iso8601(started)));
    }
    if let Some(finished) = cleanup.finished_at_ms {
        lines.push(format!("  Finished: {}", format_unix_ms_iso8601(finished)));
    }

    if let Some(preflight) = cleanup.preflight.as_ref() {
        lines.push(String::new());
        lines.push("  Preflight:".to_string());
        lines.push(format!("    Status: {}", preflight.status));
        lines.push(format!(
            "    {} blocker(s), {} warning(s)",
            preflight.blocker_count, preflight.warning_count
        ));
        for check in &preflight.failed_checks {
            lines.push(format_failed_preflight_check_line(check));
        }
    }

    if cleanup.state == "failed" {
        if let Some(failed_step) = cleanup.steps.iter().find(|step| step.status == "failed") {
            lines.push(format!("  Failed step: {}", failed_step.step_id));
            match failed_step
                .error_message
                .as_deref()
                .map(str::trim)
                .filter(|message| !message.is_empty())
            {
                Some(message) => lines.push(format!("  Error: {}", message)),
                None => lines.push("  Error: (no error details captured)".to_string()),
            }
        } else if cleanup.preflight.is_none() {
            lines.push("  No individual step recorded as failed.".to_string());
        }
    }

    if !cleanup.steps.is_empty() {
        lines.push(String::new());
        lines.push("  Cleanup steps:".to_string());
        for step in &cleanup.steps {
            let state_marker = match step.status.as_str() {
                "success" => "[✔]",
                "failed" => "[!]",
                "running" => "[>]",
                "pending" | "skipped" => "[ ]",
                _ => "[?]",
            };
            lines.push(format!("    {} {}", state_marker, step.step_id));
            if let Some(started) = step.started_at_ms {
                lines.push(format!(
                    "      Started: {}",
                    format_unix_ms_iso8601(started)
                ));
            }
            if let Some(finished) = step.finished_at_ms {
                lines.push(format!(
                    "      Finished: {}",
                    format_unix_ms_iso8601(finished)
                ));
            }
            if step.status == "failed" {
                match step
                    .error_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|message| !message.is_empty())
                {
                    Some(message) => lines.push(format!("      Error: {}", message)),
                    None => lines.push("      Error: (no error details)".to_string()),
                }
            }
        }
    }

    lines
}

fn format_unix_ms_iso8601(unix_ms: u64) -> String {
    let total_seconds = (unix_ms / 1000) as i64;
    let days = total_seconds.div_euclid(86_400);
    let secs_of_day = total_seconds.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

#[cfg(test)]
mod tests {
    use super::format_cleanup_result_lines;

    #[test]
    fn cleanup_result_lines_render_succeeded_cleanup() {
        let cleanup = transist::InspectionCleanupSummary {
            trigger: "success".to_string(),
            state: "succeeded".to_string(),
            started_at_ms: Some(1_000),
            finished_at_ms: Some(2_000),
            preflight: None,
            steps: vec![transist::InspectionCleanupStep {
                step_id: "cleanup_ok".to_string(),
                status: "success".to_string(),
                execution: Some(1),
                started_at_ms: Some(1_100),
                finished_at_ms: Some(1_900),
                error_message: None,
            }],
        };

        let lines = format_cleanup_result_lines(&cleanup);
        assert!(lines.iter().any(|line| line == "Cleanup:"));
        assert!(lines.iter().any(|line| line == "  State: succeeded"));
        assert!(lines.iter().any(|line| line == "  Trigger: success"));
        assert!(lines.iter().any(|line| line.trim() == "[✔] cleanup_ok"));
    }

    #[test]
    fn cleanup_result_lines_render_failed_cleanup() {
        let cleanup = transist::InspectionCleanupSummary {
            trigger: "failed".to_string(),
            state: "failed".to_string(),
            started_at_ms: Some(1_000),
            finished_at_ms: Some(2_000),
            preflight: None,
            steps: vec![transist::InspectionCleanupStep {
                step_id: "cleanup_on_fail".to_string(),
                status: "failed".to_string(),
                execution: Some(1),
                started_at_ms: Some(1_100),
                finished_at_ms: Some(1_900),
                error_message: Some("permission denied".to_string()),
            }],
        };

        let lines = format_cleanup_result_lines(&cleanup);
        assert!(lines.iter().any(|line| line == "Cleanup:"));
        assert!(lines.iter().any(|line| line == "  State: failed"));
        assert!(
            lines
                .iter()
                .any(|line| line == "  Failed step: cleanup_on_fail")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "  Error: permission denied")
        );
        assert!(
            lines
                .iter()
                .any(|line| line.trim() == "[!] cleanup_on_fail")
        );
    }
}
