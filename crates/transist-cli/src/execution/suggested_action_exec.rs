// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use serde_json::{Value, to_string_pretty};
use std::sync::{Arc, atomic::AtomicBool};

use crate::cli::CliUiMode;
use crate::commands::dispatch::{filter_runs_show_steps, reentry_runner_action_for_run};
use crate::display::summary::{
    ResolvedSuggestedAction, RunsShowStepFilter, SuggestedActionExecution, SuggestedActionOutcome,
};
use crate::prompt::PromptUi;

use super::build_cleanup_result_page;
use super::failed_step_inspection::{
    build_failed_step_details_page, build_failed_step_events_page, build_failed_step_stderr_page,
};

fn load_then_present_json<T>(
    prompt: &mut dyn PromptUi,
    loading_message: &str,
    title: String,
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<SuggestedActionOutcome>
where
    T: serde::Serialize + Send + 'static,
{
    let value = crate::prompt::run_with_loading_feedback(prompt, loading_message, operation)?;
    prompt.present_paged_text(&title, &to_string_pretty(&value)?)?;
    Ok(SuggestedActionOutcome::ReopenPicker)
}

fn load_then_present_text(
    prompt: &mut dyn PromptUi,
    loading_message: &str,
    operation: impl FnOnce() -> Result<(String, String)> + Send + 'static,
) -> Result<SuggestedActionOutcome> {
    let (title, body) =
        crate::prompt::run_with_loading_feedback(prompt, loading_message, operation)?;
    prompt.present_paged_text(&title, &body)?;
    Ok(SuggestedActionOutcome::ReopenPicker)
}

/// Spawns a detached runner for the given action and (on a TTY) re-attaches
/// the run-detail viewer pointed at the resulting root run id.
fn spawn_detached_action_and_attach(
    context: &transist::TransistContext,
    prompt: &mut dyn PromptUi,
    runner_action: crate::runner::RunnerAction,
    ui_mode: CliUiMode,
) -> Result<SuggestedActionOutcome> {
    let _ = crate::runner::spawn_and_attach(context, prompt, &runner_action, ui_mode)?;
    Ok(SuggestedActionOutcome::Completed(false))
}

pub(crate) fn execute_suggested_action(
    context: &transist::TransistContext,
    prompt: &mut dyn PromptUi,
    interrupt_flag: &Arc<AtomicBool>,
    action: &ResolvedSuggestedAction,
    allow_interactive_picker: bool,
    ui_mode: CliUiMode,
) -> Result<SuggestedActionOutcome> {
    match &action.execution {
        SuggestedActionExecution::Exit => Ok(SuggestedActionOutcome::Completed(false)),
        SuggestedActionExecution::Run { run_id } => spawn_detached_action_and_attach(
            context,
            prompt,
            crate::runner::RunnerAction::Run {
                run_id: run_id.clone(),
                trusted_execution: false,
                local_only: false,
            },
            ui_mode,
        ),
        SuggestedActionExecution::Approve {
            run_id,
            gate_id,
            reason,
            ..
        } => {
            let gate_result = context.record_gate_decision_with_input(
                run_id,
                gate_id.as_deref(),
                transist::GateDecision::Approved,
                transist::GateDecisionInput {
                    reason: reason.clone(),
                    ..Default::default()
                },
            )?;
            if let Some(warning) = &gate_result.lease_release_warning {
                eprintln!(
                    "warning: gate decision committed with a lease release warning: {}",
                    warning.message
                );
            }
            let root_run_id = gate_result.root_run_id;
            let runner_action = reentry_runner_action_for_run(context, &root_run_id)?;
            spawn_detached_action_and_attach(context, prompt, runner_action, ui_mode)
        }
        SuggestedActionExecution::Reject {
            run_id,
            gate_id,
            reason,
            ..
        } => {
            let gate_result = context.record_gate_decision_with_input(
                run_id,
                gate_id.as_deref(),
                transist::GateDecision::Rejected,
                transist::GateDecisionInput {
                    reason: reason.clone(),
                    ..Default::default()
                },
            )?;
            if let Some(warning) = &gate_result.lease_release_warning {
                eprintln!(
                    "warning: gate decision committed with a lease release warning: {}",
                    warning.message
                );
            }
            let root_run_id = gate_result.root_run_id;
            let runner_action = reentry_runner_action_for_run(context, &root_run_id)?;
            spawn_detached_action_and_attach(context, prompt, runner_action, ui_mode)
        }
        SuggestedActionExecution::RequestChanges {
            run_id,
            gate_id,
            reason,
            ..
        } => {
            let gate_result = context.record_gate_decision_with_input(
                run_id,
                gate_id.as_deref(),
                transist::GateDecision::RequestChanges,
                transist::GateDecisionInput {
                    reason: reason.clone(),
                    ..Default::default()
                },
            )?;
            if let Some(warning) = &gate_result.lease_release_warning {
                eprintln!(
                    "warning: gate decision committed with a lease release warning: {}",
                    warning.message
                );
            }
            let root_run_id = gate_result.root_run_id;
            let runner_action = reentry_runner_action_for_run(context, &root_run_id)?;
            spawn_detached_action_and_attach(context, prompt, runner_action, ui_mode)
        }
        SuggestedActionExecution::RunsResume { run_id } => spawn_detached_action_and_attach(
            context,
            prompt,
            crate::runner::RunnerAction::Resume {
                run_id: run_id.clone(),
                trusted_execution: false,
                local_only: false,
            },
            ui_mode,
        ),
        SuggestedActionExecution::RunsCleanup { run_id } => spawn_detached_action_and_attach(
            context,
            prompt,
            crate::runner::RunnerAction::Cleanup {
                run_id: run_id.clone(),
                trusted_execution: false,
                local_only: false,
            },
            ui_mode,
        ),
        SuggestedActionExecution::CancelRun { run_id } => {
            let _ = (interrupt_flag, allow_interactive_picker);
            let cancellation = crate::execution::request_active_run_cancellation(context, run_id)?;
            let immediate_kill_note = cancellation
                .active_execution_cancellation_error
                .as_deref()
                .map(|error| format!(" Direct cancellation attempt did not complete: {error}"))
                .unwrap_or_default();
            prompt.present_paged_text(
                &format!("Cancellation requested: {}", run_id),
                &format!(
                    "Cancellation requested for root run tree {}.{}",
                    cancellation.root_run_id, immediate_kill_note
                ),
            )?;
            Ok(SuggestedActionOutcome::ReopenPicker)
        }
        SuggestedActionExecution::RunsShow { run_id } => load_then_present_json(
            prompt,
            "Loading run details",
            format!("Run details: {}", run_id),
            {
                let context = context.clone();
                let run_id = run_id.clone();
                move || context.runs_show(&run_id).map(Value::from)
            },
        ),
        SuggestedActionExecution::RunsShowFailed { run_id } => load_then_present_json(
            prompt,
            "Loading failed run details",
            format!("Run details (failed only): {}", run_id),
            {
                let context = context.clone();
                let run_id = run_id.clone();
                move || {
                    context
                        .runs_show(&run_id)
                        .map(Value::from)
                        .map(|run| filter_runs_show_steps(run, RunsShowStepFilter::Failed))
                }
            },
        ),
        SuggestedActionExecution::RunsPreflight { run_id } => load_then_present_json(
            prompt,
            "Running preflight",
            format!("Run preflight: {}", run_id),
            {
                let context = context.clone();
                let run_id = run_id.clone();
                move || context.runs_preflight(&run_id)
            },
        ),
        SuggestedActionExecution::RunsCapabilities { run_id } => {
            load_then_present_text(prompt, "Loading capability remediation", {
                let context = context.clone();
                let run_id = run_id.clone();
                move || {
                    let report = context.run_capabilities(&run_id)?;
                    Ok((
                        format!("Run capabilities: {}", run_id),
                        yaml_serde::to_string(&report.policy)?,
                    ))
                }
            })
        }
        SuggestedActionExecution::ViewFullDiagnostic { .. } => {
            anyhow::bail!("View full diagnostic is only available in the fullscreen TUI")
        }
        SuggestedActionExecution::RunsTail { run_id, limit } => load_then_present_json(
            prompt,
            "Loading run events",
            format!("Run events: {} (limit {})", run_id, limit),
            {
                let context = context.clone();
                let run_id = run_id.clone();
                let limit = *limit;
                move || context.runs_tail(&run_id, limit)
            },
        ),
        SuggestedActionExecution::InspectFailedStepDetails { run_id, step_id } => {
            load_then_present_text(prompt, "Loading failed step details", {
                let context = context.clone();
                let run_id = run_id.clone();
                let step_id = step_id.clone();
                move || build_failed_step_details_page(&context, &run_id, &step_id)
            })
        }
        SuggestedActionExecution::InspectFailedStepEvents {
            run_id,
            step_id,
            limit,
        } => load_then_present_text(prompt, "Loading failed step events", {
            let context = context.clone();
            let run_id = run_id.clone();
            let step_id = step_id.clone();
            let limit = *limit;
            move || build_failed_step_events_page(&context, &run_id, &step_id, limit)
        }),
        SuggestedActionExecution::InspectFailedStepStderr {
            run_id,
            step_id,
            artifact_relative_path,
        } => load_then_present_text(prompt, "Loading failed step stderr", {
            let context = context.clone();
            let run_id = run_id.clone();
            let step_id = step_id.clone();
            let artifact_relative_path = artifact_relative_path.clone();
            move || {
                build_failed_step_stderr_page(&context, &run_id, &step_id, &artifact_relative_path)
            }
        }),
        SuggestedActionExecution::InspectCleanupResult { run_id } => {
            load_then_present_text(prompt, "Loading cleanup result", {
                let context = context.clone();
                let run_id = run_id.clone();
                move || build_cleanup_result_page(&context, &run_id)
            })
        }
        SuggestedActionExecution::RunsReplay { run_id } => {
            // Auto-generate a fresh replay run-ref. Operators wanting a custom
            // run-ref drop to `transist runs replay <id> <ref>` from the CLI.
            let replay_run_id = uuid::Uuid::new_v4().to_string();
            spawn_detached_action_and_attach(
                context,
                prompt,
                crate::runner::RunnerAction::Replay {
                    source_run_id: run_id.clone(),
                    replay_run_id: replay_run_id.clone(),
                    trusted_execution: false,
                    local_only: false,
                },
                ui_mode,
            )
        }
        SuggestedActionExecution::LeaseInspect { run_id } => {
            load_then_present_text(prompt, "Loading lease details", {
                let context = context.clone();
                let run_id = run_id.clone();
                move || {
                    let body = match context.runs_lease_inspect(&run_id)? {
                        Some(record) => to_string_pretty(&record)?,
                        None => "(no active lease for this run tree)".to_string(),
                    };
                    Ok((format!("Lease inspect: {}", run_id), body))
                }
            })
        }
        SuggestedActionExecution::LeaseBreak { run_id } => {
            let _ = (interrupt_flag, allow_interactive_picker);
            // Picker variant never forces a live owner; require the CLI's
            // explicit `--force-live-owner` flag for that escalation.
            let lease = context.runs_lease_break(run_id, false)?;
            let body = match lease {
                Some(record) => format!(
                    "Removed lease for {}\n\n{}",
                    record.root_run_id,
                    to_string_pretty(&record)?
                ),
                None => "(no lease file present)".to_string(),
            };
            prompt.present_paged_text(&format!("Lease break: {}", run_id), &body)?;
            Ok(SuggestedActionOutcome::ReopenPicker)
        }
    }
}
