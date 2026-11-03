// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use transist::InspectionPreflightFailedCheck;
use transist::{
    InspectionFilesystemPolicySummary, InspectionRunDetail, InspectionStep,
    TransistInspectionContext,
};

use crate::actionable::{ActionableRun, latest_step, resolve_actionable_run};
use crate::display::progress::{ProgressState, format_progress_state_detail_lines};
use crate::display::spinner::inspect_live_progress;
use crate::execution::{RecipeCacheOwner, load_recipe_step_order};

use super::{
    RunsShowStepFilter, SuggestedAction, format_runs_show_command, gate_suggested_actions,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SummaryContent {
    pub(crate) body_lines: Vec<String>,
    pub(crate) step_output_lines: Vec<String>,
    pub(crate) plain_action_lines: Vec<String>,
    pub(crate) interactive_action_lines: Vec<String>,
    pub(crate) suggested_actions: Vec<SuggestedAction>,
    pub(crate) selected_action_block_title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunStatusAdvice {
    pub(crate) status: String,
    pub(crate) run_id: String,
    pub(crate) workflow_ref: String,
    pub(crate) actionable_run_id: String,
    pub(crate) current_step: Option<String>,
    pub(crate) headline: String,
    pub(crate) detail_lines: Vec<String>,
    pub(crate) suggested_commands: Vec<String>,
}

pub(crate) fn compact_run_status_lines(
    context: &transist::TransistContext,
    run_id: &str,
    recipe_cache: &RecipeCacheOwner,
) -> Result<Vec<String>> {
    let advice = build_compact_run_status_advice(context, run_id, recipe_cache)?;
    Ok(render_compact_run_status_advice(&advice))
}

pub(crate) fn build_compact_run_status_advice(
    context: &transist::TransistContext,
    run_id: &str,
    recipe_cache: &RecipeCacheOwner,
) -> Result<RunStatusAdvice> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let Some(root_detail) = inspection.get_run(run_id)? else {
        anyhow::bail!("run '{}' was not found", run_id);
    };
    let Some(actionable) = resolve_actionable_run(&inspection, run_id)? else {
        anyhow::bail!("run '{}' was not actionable", run_id);
    };
    let live_progress_state = active_progress_state(
        context,
        run_id,
        root_detail.summary.status.as_str(),
        recipe_cache,
    )?;
    Ok(build_compact_run_status_advice_with_progress(
        Some(context),
        &root_detail,
        &actionable,
        run_id,
        live_progress_state.as_ref(),
    ))
}

pub(crate) fn build_compact_run_status_advice_from_details(
    root_detail: &InspectionRunDetail,
    actionable: &ActionableRun,
    requested_run_id: &str,
) -> RunStatusAdvice {
    build_compact_run_status_advice_with_progress(
        None,
        root_detail,
        actionable,
        requested_run_id,
        None,
    )
}

fn build_compact_run_status_advice_with_progress(
    context: Option<&transist::TransistContext>,
    root_detail: &InspectionRunDetail,
    actionable: &ActionableRun,
    requested_run_id: &str,
    progress_state: Option<&ProgressState>,
) -> RunStatusAdvice {
    let status = root_detail.summary.status.clone();
    let workflow_ref = format!(
        "{}@{}",
        root_detail.summary.workflow_id, root_detail.summary.workflow_version
    );
    let current_step = status_summary_step(actionable).map(|step| step.step_id.clone());
    let mut detail_lines = filesystem_policy_detail_lines(root_detail);
    let mut suggested_commands = Vec::new();
    let headline = match status.as_str() {
        "created" | "planned" => {
            suggested_commands.extend([
                format!("transist runs start {}", requested_run_id),
                format!("transist runs preflight {}", requested_run_id),
                format_runs_show_command(requested_run_id, RunsShowStepFilter::All),
            ]);
            "The run is prepared and ready to execute.".to_string()
        }
        "running" => {
            suggested_commands.extend(inspect_commands(
                requested_run_id,
                &actionable.run_id,
                RunsShowStepFilter::All,
            ));
            detail_lines.extend(active_progress_detail_lines(progress_state));
            match current_step.as_deref() {
                Some(step_id) => format!("This run is running. Current step: '{}'.", step_id),
                None => "This run is running.".to_string(),
            }
        }
        "waiting" => {
            suggested_commands.extend(inspect_commands(
                requested_run_id,
                &actionable.run_id,
                RunsShowStepFilter::All,
            ));
            detail_lines.extend(active_progress_detail_lines(progress_state));
            match current_step.as_deref() {
                Some(step_id) => format!("This run is waiting (current step: '{}').", step_id),
                None => "This run is waiting.".to_string(),
            }
        }
        "blocked" => {
            // Surface persisted preflight failures (e.g. asset-lock drift)
            // even when the run is blocked on a gate. Re-entry verification
            // happens before dispatch, so a blocked run with drifted plugin
            // bytes will stay blocked forever — the operator needs to see why
            // rather than just "Blocked on gate".
            if let Some(preflight) = root_detail.preflight.as_ref()
                && preflight.status.as_str() == "failed"
            {
                detail_lines.push(format!(
                    "Preflight blockers: {} blocker(s), {} warning(s)",
                    preflight.blocker_count, preflight.warning_count
                ));
                for check in &preflight.failed_checks {
                    detail_lines.push(format_failed_preflight_check_line(check));
                }
                suggested_commands.push(format!("transist runs preflight {}", requested_run_id));
            }
            if let Some(gate) = actionable.detail.blocked_gates.first() {
                let gate_actor = context
                    .and_then(|ctx| {
                        transist::resolve_gate_actor_identity_in_home(ctx.home_dir()).ok()
                    })
                    .flatten();
                suggested_commands.extend(
                    gate_suggested_actions(&actionable.root_run_id, None, gate_actor.as_deref())
                        .into_iter()
                        .map(|action| action.display_command),
                );
                suggested_commands.extend(inspect_commands(
                    &actionable.run_id,
                    &actionable.run_id,
                    RunsShowStepFilter::All,
                ));
                format!("Blocked on gate: {}.", gate.gate_id)
            } else {
                suggested_commands.push(format!("transist runs start {}", actionable.root_run_id));
                suggested_commands.extend(inspect_commands(
                    &actionable.run_id,
                    &actionable.run_id,
                    RunsShowStepFilter::All,
                ));
                "Blocked before the actionable subflow reached a manual gate.".to_string()
            }
        }
        "failed" => {
            if preflight_failed_before_dispatch(root_detail, actionable) {
                if let Some(preflight) = root_detail.preflight.as_ref() {
                    detail_lines.push(format!(
                        "Preflight blockers: {} blocker(s), {} warning(s)",
                        preflight.blocker_count, preflight.warning_count
                    ));
                    for check in &preflight.failed_checks {
                        detail_lines.push(format_failed_preflight_check_line(check));
                    }
                }
                suggested_commands.extend([
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed),
                    format!("transist runs preflight {}", actionable.run_id),
                    format!("transist runs retry {}", failure_resume_target(actionable)),
                    replay_command(requested_run_id),
                ]);
                "Preflight failed before step dispatch.".to_string()
            } else {
                if let Some(step) = latest_step(&actionable.detail, &["failed"]) {
                    let failed_step_id = failed_summary_step_id(step);
                    detail_lines.push(format!("Failed step: {failed_step_id}"));
                    if let Some(child_run_id) = step
                        .output_summary
                        .child_failure
                        .as_ref()
                        .map(|failure| failure.child_run_id.as_str())
                    {
                        detail_lines.push(format!("Child run: {child_run_id}"));
                    }
                    if let Some(message) = failed_summary_message(step) {
                        detail_lines.push(format!("Failure reason: {}", message));
                    }
                }
                if let Some(cleanup) = root_detail.cleanup.as_ref() {
                    detail_lines.push(format!("Cleanup state: {}", cleanup.state));
                }
                let cleanup_command = format!("transist runs cleanup {}", actionable.root_run_id);
                let cleanup_required =
                    root_detail.recovery_state.as_deref() == Some("cleanup_required");
                let cleanup_completed =
                    root_detail.recovery_state.as_deref() == Some("cleanup_completed");
                if cleanup_required {
                    suggested_commands.push(cleanup_command);
                } else if !cleanup_completed {
                    suggested_commands.push(format!(
                        "transist runs retry {}",
                        failure_resume_target(actionable)
                    ));
                    suggested_commands.push(cleanup_command);
                }
                suggested_commands.extend(inspect_commands(
                    &actionable.run_id,
                    &actionable.run_id,
                    RunsShowStepFilter::Failed,
                ));
                suggested_commands.push(replay_command(requested_run_id));
                if cleanup_required {
                    "The failed run is no longer resumable because explicit cleanup has begun. Retry cleanup until it succeeds or is skipped."
                        .to_string()
                } else if cleanup_completed {
                    "Cleanup is complete; the failed run is no longer resumable.".to_string()
                } else if actionable.run_id != actionable.root_run_id {
                    format!(
                        "Root run {} stopped because child subflow run {} failed.",
                        actionable.root_run_id, actionable.run_id
                    )
                } else {
                    "The run failed in-place.".to_string()
                }
            }
        }
        "success" => {
            if let Some(cleanup) = root_detail.cleanup.as_ref()
                && matches!(cleanup.state.as_str(), "failed" | "pending" | "running")
            {
                detail_lines.push(format!("Cleanup state: {}", cleanup.state));
                suggested_commands.push(format!("transist runs cleanup {}", requested_run_id));
            }
            suggested_commands.extend(inspect_commands(
                requested_run_id,
                &actionable.run_id,
                RunsShowStepFilter::All,
            ));
            suggested_commands.push(replay_command(requested_run_id));
            "The run completed successfully.".to_string()
        }
        "canceled" => {
            let cleanup_completed = root_detail
                .cleanup
                .as_ref()
                .map(|cleanup| cleanup.state.as_str() == "succeeded")
                .unwrap_or(false);
            if let Some(cleanup) = root_detail.cleanup.as_ref()
                && matches!(cleanup.state.as_str(), "failed" | "pending" | "running")
            {
                detail_lines.push(format!("Cleanup state: {}", cleanup.state));
                suggested_commands.push(format!("transist runs cleanup {}", requested_run_id));
            }
            if !cleanup_completed {
                suggested_commands.push(format!("transist runs retry {}", requested_run_id));
            }
            suggested_commands.extend(inspect_commands(
                requested_run_id,
                &actionable.run_id,
                RunsShowStepFilter::All,
            ));
            suggested_commands.push(replay_command(requested_run_id));
            if cleanup_completed {
                "The run was canceled and cleanup already completed.".to_string()
            } else {
                "The run was canceled. Use `transist runs retry` to continue from the canceled step."
                    .to_string()
            }
        }
        other => {
            suggested_commands.extend(inspect_commands(
                requested_run_id,
                &actionable.run_id,
                RunsShowStepFilter::All,
            ));
            format!("Run is {}.", other)
        }
    };

    RunStatusAdvice {
        status,
        run_id: requested_run_id.to_string(),
        workflow_ref,
        actionable_run_id: actionable.run_id.clone(),
        current_step,
        headline,
        detail_lines,
        suggested_commands: dedupe_command_lines(suggested_commands),
    }
}

fn filesystem_policy_detail_lines(root_detail: &InspectionRunDetail) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(global) = root_detail.global_default_filesystem_policy.as_ref() {
        lines.push(format!(
            "Global default policy: {}",
            format_policy_summary(global)
        ));
    }
    if let Some(workflow) = root_detail.workflow_filesystem_policy.as_ref() {
        lines.push(format!(
            "Workflow policy: {}",
            format_policy_summary(workflow)
        ));
    }
    lines
}

fn format_policy_summary(policy: &InspectionFilesystemPolicySummary) -> String {
    let source = policy.source.as_deref().unwrap_or("unknown");
    let mut summary = format!("source={}, mode={:?}", source, policy.workspace_access.mode)
        .replace("Full", "full")
        .replace("ReadOnly", "read_only")
        .replace("Custom", "custom");
    if !policy.read_allow_paths.is_empty() {
        summary.push_str(&format!(", read=[{}]", policy.read_allow_paths.join(", ")));
    }
    if !policy.write_allow_paths.is_empty() {
        summary.push_str(&format!(
            ", write=[{}]",
            policy.write_allow_paths.join(", ")
        ));
    }
    summary
}

fn active_progress_state(
    context: &transist::TransistContext,
    run_id: &str,
    status: &str,
    recipe_cache: &RecipeCacheOwner,
) -> Result<Option<ProgressState>> {
    if !matches!(status, "running" | "waiting") {
        return Ok(None);
    }
    let root_step_order =
        load_recipe_step_order(context, run_id, recipe_cache)?.unwrap_or_default();
    Ok(
        inspect_live_progress(context, run_id, &root_step_order, Vec::new(), recipe_cache)?
            .map(|progress| progress.state),
    )
}

fn active_progress_detail_lines(progress_state: Option<&ProgressState>) -> Vec<String> {
    progress_state
        .map(format_progress_state_detail_lines)
        .unwrap_or_default()
}

pub(crate) fn render_compact_run_status_advice(advice: &RunStatusAdvice) -> Vec<String> {
    let mut lines = vec![
        format!("Run status: {}", advice.status),
        format!("Run id: {}", advice.run_id),
        format!("Workflow: {}", advice.workflow_ref),
    ];
    if advice.actionable_run_id != advice.run_id {
        lines.push(format!(
            "{}: {}",
            actionable_subflow_label(advice.status.as_str()),
            advice.actionable_run_id
        ));
    }
    if let Some(current_step) = advice.current_step.as_ref() {
        lines.push(format!("Current step: {}", current_step));
    }
    lines.push(format!("Summary: {}", advice.headline));
    lines.extend(advice.detail_lines.iter().cloned());
    lines.push(String::new());
    lines.extend(format_suggested_command_lines(
        "Next:",
        &advice.suggested_commands,
    ));
    lines
}

fn status_summary_step(actionable: &ActionableRun) -> Option<&InspectionStep> {
    actionable
        .detail
        .current_step
        .as_ref()
        .and_then(|current| {
            actionable
                .detail
                .steps
                .iter()
                .find(|step| step.step_id == current.step_id)
        })
        .or_else(|| latest_step(&actionable.detail, &["failed", "blocked", "canceled"]))
}

fn preflight_failed_before_dispatch(
    root_detail: &InspectionRunDetail,
    actionable: &ActionableRun,
) -> bool {
    latest_step(&actionable.detail, &["failed"]).is_none()
        && root_detail
            .preflight
            .as_ref()
            .map(|preflight| preflight.status.as_str() == "failed")
            .unwrap_or(false)
}

fn failure_resume_target(actionable: &ActionableRun) -> String {
    if actionable.run_id != actionable.root_run_id {
        actionable.root_run_id.clone()
    } else {
        actionable.run_id.clone()
    }
}

fn inspect_commands(
    show_run_id: &str,
    tail_run_id: &str,
    show_filter: RunsShowStepFilter,
) -> Vec<String> {
    vec![
        format_runs_show_command(show_run_id, show_filter),
        format!("transist runs tail {} --limit 80", tail_run_id),
    ]
}

fn replay_command(run_id: &str) -> String {
    format!("transist runs replay {} --run-ref <new-run-id>", run_id)
}

fn failed_summary_step_id(step: &InspectionStep) -> &str {
    step.output_summary
        .child_failure
        .as_ref()
        .and_then(|failure| failure.failed_step_id.as_deref())
        .unwrap_or(step.step_id.as_str())
}

fn failed_summary_message(step: &InspectionStep) -> Option<&str> {
    step.output_summary
        .child_failure
        .as_ref()
        .and_then(|failure| {
            // Prefer the specific first preflight blocker over the generic
            // "preflight failed before step dispatch" marker so plain-text
            // summaries match the TUI Details pane.
            failure
                .preflight_first_blocker
                .as_deref()
                .or(failure.error_message.as_deref())
                .or(failure.stderr.as_deref())
        })
        .or_else(|| {
            step.output_summary
                .error_message
                .as_deref()
                .map(str::trim)
                .filter(|message| !message.is_empty())
        })
}

fn format_suggested_command_lines(title: &str, commands: &[String]) -> Vec<String> {
    let mut lines = vec![title.to_string()];
    if commands.is_empty() {
        lines.push("  none".to_string());
        return lines;
    }
    lines.extend(
        commands
            .iter()
            .enumerate()
            .map(|(index, command)| format!("  {}. {}", index + 1, command)),
    );
    lines
}

fn dedupe_command_lines(commands: Vec<String>) -> Vec<String> {
    let mut unique = Vec::new();
    for command in commands {
        if unique.iter().any(|existing: &String| existing == &command) {
            continue;
        }
        unique.push(command);
    }
    unique
}
pub(crate) fn actionable_subflow_label(root_status: &str) -> &'static str {
    match root_status {
        "blocked" => "Internal blocked child run",
        "failed" => "Internal failed child run",
        _ => "Internal child run",
    }
}

pub(crate) fn format_failed_preflight_check_line(check: &InspectionPreflightFailedCheck) -> String {
    let mut line = format!("  - {}", check.kind);
    let plugin_action = match (&check.plugin_id, &check.action) {
        (Some(plugin), Some(action)) => Some(format!("{}/{}", plugin, action)),
        (Some(plugin), None) => Some(plugin.clone()),
        (None, Some(action)) => Some(action.clone()),
        (None, None) => None,
    };
    if let Some(scope) = plugin_action {
        line.push_str(&format!(" ({})", scope));
    }
    if !check.step_ids.is_empty() {
        line.push_str(&format!(" [{}]", check.step_ids.join(", ")));
    }
    line.push_str(": ");
    line.push_str(check.message.trim());
    line
}
