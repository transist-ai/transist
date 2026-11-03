// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

/// Raised when an operator tries to cancel a run that is no longer active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunNotActiveForCancellationError {
    run_id: String,
    status: String,
}

impl RunNotActiveForCancellationError {
    pub(crate) fn new(run_id: &str, status: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            status: status.to_string(),
        }
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn status(&self) -> &str {
        &self.status
    }
}

impl fmt::Display for RunNotActiveForCancellationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "run '{}' is not currently active, so there is nothing to interrupt.",
            self.run_id
        )
    }
}

impl std::error::Error for RunNotActiveForCancellationError {}

/// Converts an inactive-cancellation error into the non-fatal TUI status text.
pub(crate) fn inactive_run_cancellation_status_message(error: &anyhow::Error) -> Option<String> {
    let inactive = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RunNotActiveForCancellationError>())?;
    let status = inactive.status().trim();
    let status_suffix = if status.is_empty() {
        String::new()
    } else {
        format!(" (status: {status})")
    };
    Some(format!(
        "Run {} is not currently active{}, so there is nothing to interrupt. Refreshing run view.",
        inactive.run_id(),
        status_suffix
    ))
}

/// Structured result returned after a CLI/TUI cancel request.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunCancellationReport {
    pub(crate) run_id: String,
    pub(crate) root_run_id: String,
    pub(crate) cancellation_requested: bool,
    pub(crate) active_execution_cancellation: Option<transist::ActiveRunCancellation>,
    pub(crate) active_execution_cancellation_error: Option<String>,
    pub(crate) recovery_runner_pid: Option<u32>,
    pub(crate) settled: bool,
    pub(crate) final_status: String,
    pub(crate) cleanup_state: Option<String>,
    pub(crate) error: Option<String>,
}

/// Classifies cancellation fulfillment separately from the primary outcome.
fn cancellation_cleanup_state(run: &Value) -> Result<Option<String>> {
    let Some(cleanup) = run.get("cleanup").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let state = cleanup["state"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("cancellation cleanup has no state"))?;
    if !matches!(
        state,
        "pending" | "running" | "succeeded" | "skipped" | "failed"
    ) {
        anyhow::bail!("cancellation cleanup has unexpected state '{state}'");
    }
    Ok(Some(state.to_string()))
}

/// Requests cancellation for an active run using the shared CLI/TUI policy.
pub(crate) fn request_active_run_cancellation(
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<RunCancellationReport> {
    let run = Value::from(context.runs_show(run_id)?);
    let status = run["status"].as_str().unwrap_or_default();
    let root_run_id = run["lineage"]["root_run_id"]
        .as_str()
        .unwrap_or(run_id)
        .to_string();
    let root = Value::from(context.runs_show(&root_run_id)?);
    let already_canceled = root["status"].as_str() == Some("canceled");
    if !already_canceled && !matches!(status, "running" | "waiting") {
        return Err(RunNotActiveForCancellationError::new(run_id, status).into());
    }

    // Repeated cancellation observes the same operation. In particular, do not
    // publish another marker or signal the cleanup fulfilling the first request.
    if !already_canceled {
        context.request_run_cancellation(run_id)?;
    }
    let (active_execution_cancellation, active_execution_cancellation_error) = if already_canceled {
        (None, None)
    } else {
        match context.cancel_active_run_execution(run_id) {
            Ok(cancellation) => (cancellation, None),
            Err(error) => (None, Some(error.to_string())),
        }
    };

    Ok(RunCancellationReport {
        run_id: run_id.to_string(),
        root_run_id,
        cancellation_requested: true,
        active_execution_cancellation,
        active_execution_cancellation_error,
        recovery_runner_pid: None,
        settled: false,
        final_status: status.to_string(),
        cleanup_state: cancellation_cleanup_state(&root)?,
        error: None,
    })
}

/// Requests cancellation, ensures a runner can consume it, and waits for the
/// durable root state to settle. The TUI intentionally uses only
/// [`request_active_run_cancellation`] so it never blocks its refresh loop.
pub(crate) fn settle_active_run_cancellation(
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<RunCancellationReport> {
    let mut report = request_active_run_cancellation(context, run_id)?;
    // A terminal projection can precede cleanup initialization. Absence of a
    // cleanup record proves completion only when the pinned recipe has none.
    let has_cleanup_declarations = !context
        .persisted_recipe(&report.root_run_id)?
        .cleanup
        .is_empty();

    if let Err(error) = context.release_orphan_leases() {
        report.error = Some(format!("failed reaping orphan leases: {error}"));
        return Ok(report);
    }

    // A detached runner can exit before releasing its lease. Break only a
    // same-host lease through the non-forced path, which refuses live owners.
    if let Ok(Some(lease)) = context.runs_lease_inspect(&report.root_run_id)
        && lease.hostname == local_hostname()
    {
        let _ = context.runs_lease_break(&report.root_run_id, false);
    }

    match context.runs_lease_inspect(&report.root_run_id) {
        Ok(None) => {
            let root_run = match context.runs_show(&report.root_run_id) {
                Ok(run) => Value::from(run),
                Err(error) => {
                    report.error = Some(format!(
                        "failed reading root run '{}' before recovery: {error}",
                        report.root_run_id
                    ));
                    return Ok(report);
                }
            };
            let action = if root_run["status"].as_str() == Some("canceled") {
                if root_run["cleanup"]["state"].as_str() == Some("failed") {
                    report.final_status = "canceled".to_string();
                    report.cleanup_state = Some("failed".to_string());
                    report.error = Some(format!(
                        "cancellation cleanup failed for '{}'; recover with `transist runs cleanup {}`",
                        report.root_run_id, report.root_run_id
                    ));
                    return Ok(report);
                }
                // A completed cancellation is idempotent. Incomplete historical
                // projections recover only cleanup, never primary work.
                if matches!(
                    root_run["cleanup"]["state"].as_str(),
                    Some("succeeded" | "skipped")
                ) {
                    report.final_status = "canceled".to_string();
                    report.cleanup_state = cancellation_cleanup_state(&root_run)?;
                    report.settled = true;
                    return Ok(report);
                }
                crate::runner::RunnerAction::Cleanup {
                    run_id: report.root_run_id.clone(),
                    trusted_execution: root_run["trusted_execution_mode"]
                        .as_bool()
                        .unwrap_or(false),
                    local_only: root_run["local_only_mode"].as_bool().unwrap_or(false),
                }
            } else {
                crate::runner::RunnerAction::Run {
                    run_id: report.root_run_id.clone(),
                    trusted_execution: root_run["trusted_execution_mode"]
                        .as_bool()
                        .unwrap_or(false),
                    local_only: root_run["local_only_mode"].as_bool().unwrap_or(false),
                }
            };
            match crate::runner::spawn_detached_runner(
                context.home_dir(),
                context.repo_root(),
                &action,
            ) {
                Ok(pid) => report.recovery_runner_pid = Some(pid),
                Err(error) => {
                    report.error = Some(format!(
                        "failed starting recovery runner for root run '{}': {error}",
                        report.root_run_id
                    ));
                    return Ok(report);
                }
            }
        }
        Ok(Some(_)) => {}
        Err(error) => {
            report.error = Some(format!(
                "failed inspecting lease for root run '{}': {error}",
                report.root_run_id
            ));
            return Ok(report);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match context.runs_show(&report.root_run_id) {
            Ok(run) => {
                let run = Value::from(run);
                report.final_status = run["status"].as_str().unwrap_or_default().to_string();
                if report.final_status == "canceled" {
                    report.cleanup_state = cancellation_cleanup_state(&run)?;
                    if report.cleanup_state.as_deref() == Some("failed") {
                        report.error = Some(format!(
                            "root run '{}' is canceled but cleanup failed; recover with `transist runs cleanup {}`",
                            report.root_run_id, report.root_run_id
                        ));
                        return Ok(report);
                    }
                    // Terminal primary state is published before automatic
                    // cleanup starts. Lease release proves finalization is done,
                    // including the no-declaration case.
                    if context.runs_lease_inspect(&report.root_run_id)?.is_none() {
                        // Read again after finalization releases its lease. The
                        // earlier state may precede a cleanup failure/commit.
                        let finished = Value::from(context.runs_show(&report.root_run_id)?);
                        report.final_status =
                            finished["status"].as_str().unwrap_or_default().to_string();
                        report.cleanup_state = cancellation_cleanup_state(&finished)?;
                        if report.cleanup_state.as_deref() == Some("failed") {
                            report.error = Some(format!(
                                "root run '{}' is canceled but cleanup failed; recover with `transist runs cleanup {}`",
                                report.root_run_id, report.root_run_id
                            ));
                            return Ok(report);
                        }
                        if report.final_status == "canceled"
                            && cleanup_fulfilled(
                                report.cleanup_state.as_deref(),
                                has_cleanup_declarations,
                            )
                        {
                            report.settled = true;
                            return Ok(report);
                        }
                    }
                }
                if matches!(report.final_status.as_str(), "success" | "failed") {
                    report.error = Some(format!(
                        "root run '{}' settled as '{}' instead of canceled",
                        report.root_run_id, report.final_status
                    ));
                    return Ok(report);
                }
            }
            Err(error) => {
                report.error = Some(format!(
                    "failed reading root run '{}' while waiting for cancellation: {error}",
                    report.root_run_id
                ));
                return Ok(report);
            }
        }
        if Instant::now() >= deadline {
            report.error = Some(format!(
                "timed out after 15 seconds waiting for root run '{}' cancellation and cleanup (last status: '{}', cleanup: {:?}); the detached runner may still be active",
                report.root_run_id, report.final_status, report.cleanup_state
            ));
            return Ok(report);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn local_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// Missing state is not completion when the pinned recipe declares cleanup.
fn cleanup_fulfilled(state: Option<&str>, has_declarations: bool) -> bool {
    matches!(state, Some("succeeded" | "skipped")) || (state.is_none() && !has_declarations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_progress_distinguishes_unstarted_running_failed_and_finished() {
        for state in ["pending", "running", "failed", "succeeded", "skipped"] {
            let run = serde_json::json!({"status": "canceled", "cleanup": {"state": state}});
            assert_eq!(
                cancellation_cleanup_state(&run).unwrap().as_deref(),
                Some(state)
            );
        }
        for run in [
            serde_json::json!({"status": "canceled"}),
            serde_json::json!({"cleanup": null}),
        ] {
            assert_eq!(cancellation_cleanup_state(&run).unwrap(), None);
        }
    }

    #[test]
    fn malformed_cleanup_progress_cannot_be_reported_as_settled() {
        for cleanup in [
            serde_json::json!({}),
            serde_json::json!({"state": "unknown"}),
        ] {
            assert!(cancellation_cleanup_state(&serde_json::json!({"cleanup": cleanup})).is_err());
        }
    }
}
#[test]
fn uninitialized_declared_cleanup_does_not_settle_before_recovery_runner_starts() {
    assert!(!cleanup_fulfilled(None, true));
    assert!(cleanup_fulfilled(None, false));
    for state in ["pending", "running", "failed"] {
        assert!(!cleanup_fulfilled(Some(state), true));
    }
    for state in ["succeeded", "skipped"] {
        assert!(cleanup_fulfilled(Some(state), true));
    }
}
