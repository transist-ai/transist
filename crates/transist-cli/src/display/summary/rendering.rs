// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use transist::TransistInspectionContext;

use crate::actionable::{latest_step, resolve_actionable_run};
use crate::cli::CliUiMode;
use crate::display::output::format_suggested_action_list;
use crate::display::progress::{ProgressState, progress_top_level_step_id};
use crate::display::spinner::{LiveProgress, inspect_terminal_progress};
use crate::pager::RenderedTerminalBlock;
use crate::prompt::PromptUi;
use crate::terminal::current_unix_ms;

use super::{
    RunsShowStepFilter, SuggestedActionOutcome, SummaryContent, actionable_subflow_label,
    build_blocked_summary_content, build_failed_summary_content, format_runs_show_command,
    resolve_selected_action,
};

pub(crate) fn emit_operator_summary(
    context: &transist::TransistContext,
    run_id: &str,
    prompt: &mut dyn PromptUi,
    interrupt_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    allow_interactive_picker: bool,
    ui_mode: CliUiMode,
    previous_progress_lines: &RenderedTerminalBlock,
) -> Result<Option<bool>> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let Some(root_detail) = inspection.get_run(run_id)? else {
        return Ok(None);
    };
    let Some(actionable) = resolve_actionable_run(&inspection, run_id)? else {
        return Ok(None);
    };
    let summary_step = actionable
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
        .or_else(|| latest_step(&actionable.detail, &["failed", "blocked"]));
    let selected_action_anchor_step_id = summary_step
        .and_then(|step| progress_top_level_step_id(run_id, &actionable.run_id, &step.step_id));

    match root_detail.summary.status.as_str() {
        "blocked" | "failed" => {
            let summary = if root_detail.summary.status == "blocked" {
                build_blocked_summary_content(context, &root_detail, run_id, &actionable)?
            } else {
                build_failed_summary_content(context, &root_detail, &actionable, run_id)
            };
            prompt.clear_rendered_frame(previous_progress_lines)?;
            emit_current_step_summary(
                prompt,
                inspect_terminal_progress(context, &inspection, run_id)?,
                &summary,
                selected_action_anchor_step_id.clone(),
            )?;

            let interactive_picker = allow_interactive_picker
                && prompt.picker_available()
                && !summary.suggested_actions.is_empty();
            if interactive_picker {
                loop {
                    let picker_selection = prompt.select_suggested_action(
                        &summary.suggested_actions,
                        &summary.selected_action_block_title,
                    )?;
                    let Some(selected_index) = picker_selection.selected_index else {
                        prompt.clear_rendered_frame(&picker_selection.rendered_lines)?;
                        if picker_selection.dismissed_by_interrupt {
                            if prompt.confirm_exit_transist()? {
                                emit_final_run_summary(
                                    context,
                                    prompt,
                                    run_id,
                                    Some(&summary),
                                    None,
                                )?;
                                return Ok(Some(true));
                            }
                            continue;
                        }
                        break;
                    };
                    let Some(action) = summary.suggested_actions.get(selected_index) else {
                        continue;
                    };
                    let Some(resolved) = resolve_selected_action(prompt, action)? else {
                        continue;
                    };
                    let selected_action_lines = format_suggested_action_list(
                        &summary.selected_action_block_title,
                        &summary.suggested_actions,
                        Some((selected_index, resolved.display_command.as_str())),
                    );
                    prompt.clear_rendered_frame(&picker_selection.rendered_lines)?;
                    if resolved.execution.leaves_selection_trace() {
                        prompt.emit_plain_lines(&selected_action_lines)?;
                    }
                    let outcome = crate::execution::execute_suggested_action(
                        context,
                        prompt,
                        interrupt_flag,
                        &resolved,
                        true,
                        ui_mode,
                    )?;
                    if resolved.execution.exits_transist() {
                        let no_summary_action_lines: &[String] = &[];
                        emit_final_run_summary(
                            context,
                            prompt,
                            run_id,
                            Some(&summary),
                            Some(no_summary_action_lines),
                        )?;
                    }
                    match outcome {
                        SuggestedActionOutcome::Completed(interrupted) => {
                            return Ok(Some(interrupted));
                        }
                        SuggestedActionOutcome::ReopenPicker => {
                            continue;
                        }
                    }
                }
            }
            emit_final_run_summary(context, prompt, run_id, Some(&summary), None)?;
        }
        "canceled" => {
            emit_final_run_summary(context, prompt, run_id, None, None)?;
        }
        "success" => {
            emit_final_run_summary(context, prompt, run_id, None, None)?;
        }
        "running" => {
            let run_metadata_lines = build_run_metadata_lines(
                root_detail.summary.status.as_str(),
                run_id,
                &actionable.run_id,
                summary_step.map(|step| step.step_id.as_str()),
            );
            prompt.emit_plain_lines(&run_metadata_lines)?;
            eprintln!("Outcome: the run is still active.");
            eprintln!("Inspect:");
            eprintln!(
                "  {}",
                format_runs_show_command(run_id, RunsShowStepFilter::All)
            );
            eprintln!("  transist runs tail {} --limit 80", actionable.run_id);
        }
        other => {
            let run_metadata_lines = build_run_metadata_lines(
                root_detail.summary.status.as_str(),
                run_id,
                &actionable.run_id,
                summary_step.map(|step| step.step_id.as_str()),
            );
            prompt.emit_plain_lines(&run_metadata_lines)?;
            eprintln!("Outcome: run is {}.", other);
        }
    }

    Ok(None)
}

pub(crate) fn build_run_metadata_lines(
    root_status: &str,
    run_id: &str,
    actionable_run_id: &str,
    current_step: Option<&str>,
) -> Vec<String> {
    let mut lines = vec![
        format!("Run status: {}", root_status),
        format!("Run id: {}", run_id),
    ];
    if actionable_run_id != run_id {
        lines.push(format!(
            "{}: {}",
            actionable_subflow_label(root_status),
            actionable_run_id
        ));
    }
    if let Some(step) = current_step {
        lines.push(format!("Current step: {}", step));
    }
    lines
}

fn format_run_duration(started_at_ms: Option<u64>, finished_at_ms: Option<u64>) -> Option<String> {
    let started = started_at_ms?;
    let finished = finished_at_ms.unwrap_or_else(current_unix_ms);
    let elapsed_secs = finished.saturating_sub(started) / 1000;
    let minutes = elapsed_secs / 60;
    let seconds = elapsed_secs % 60;
    if minutes > 0 {
        Some(format!("{}m {}s", minutes, seconds))
    } else {
        Some(format!("{}s", seconds))
    }
}

pub(crate) fn summary_detail_lines(
    summary: &SummaryContent,
    action_lines: &[String],
) -> Vec<String> {
    let mut lines = Vec::new();
    if !summary.body_lines.is_empty() {
        lines.extend(summary.body_lines.clone());
    }
    if !summary.step_output_lines.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(summary.step_output_lines.clone());
    }
    if !action_lines.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(action_lines.iter().cloned());
    }
    lines
}

pub(crate) fn summary_action_only_lines(action_lines: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut iter = action_lines
        .iter()
        .skip_while(|line| line.trim().is_empty())
        .peekable();
    if iter.peek().is_none() {
        return lines;
    }
    lines.push(String::new());
    lines.extend(iter.cloned());
    lines
}

pub(crate) fn build_current_step_summary_progress(
    progress_state: ProgressState,
    summary: &SummaryContent,
    anchor_step_id: Option<String>,
    action_lines: &[String],
) -> LiveProgress {
    use crate::display::output::AttachedStepOutputBlock;
    let detail_lines = summary_detail_lines(summary, action_lines);
    LiveProgress {
        state: progress_state,
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: if detail_lines.is_empty() {
            Vec::new()
        } else {
            vec![AttachedStepOutputBlock {
                lines: detail_lines,
                anchor_step_id,
            }]
        },
        footer_lines: Vec::new(),
    }
}

pub(crate) fn emit_current_step_summary(
    prompt: &mut dyn PromptUi,
    progress_state: Option<ProgressState>,
    summary: &SummaryContent,
    anchor_step_id: Option<String>,
) -> Result<()> {
    if let Some(progress_state) = progress_state {
        let progress = build_current_step_summary_progress(
            progress_state,
            summary,
            anchor_step_id,
            &summary.interactive_action_lines,
        );
        prompt.show_summary_progress(Some(&progress), &[])?;
    } else {
        prompt.show_summary_progress(
            None,
            &summary_detail_lines(summary, &summary.interactive_action_lines),
        )?;
    }
    Ok(())
}

pub(crate) fn emit_final_run_summary(
    context: &transist::TransistContext,
    prompt: &mut dyn PromptUi,
    run_id: &str,
    summary: Option<&SummaryContent>,
    action_lines: Option<&[String]>,
) -> Result<()> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let Some(root_detail) = inspection.get_run(run_id)? else {
        return Ok(());
    };
    let Some(actionable) = resolve_actionable_run(&inspection, run_id)? else {
        return Ok(());
    };
    let summary_step = actionable
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
        .or_else(|| latest_step(&actionable.detail, &["failed", "blocked", "canceled"]));

    let mut lines = vec!["------------[run summary]------------".to_string()];
    lines.push(format!("Status: {}", root_detail.summary.status));
    lines.push(format!("Run id: {}", run_id));
    lines.push(format!(
        "Workflow: {}@{}",
        root_detail.summary.workflow_id, root_detail.summary.workflow_version
    ));
    if actionable.run_id != run_id {
        lines.push(format!(
            "{}: {}",
            actionable_subflow_label(root_detail.summary.status.as_str()),
            actionable.run_id
        ));
    }
    if let Some(step) = summary_step {
        lines.push(format!("Current step: {}", step.step_id));
    }
    if let Some(duration) = format_run_duration(
        root_detail.summary.started_at_ms,
        root_detail.summary.finished_at_ms,
    ) {
        lines.push(format!("Duration: {}", duration));
    }
    lines.push(format!("Version: v{}", env!("CARGO_PKG_VERSION")));
    if let Some(summary) = summary {
        if !summary.body_lines.is_empty() {
            lines.push(String::new());
            lines.extend(summary.body_lines.clone());
        }
    }
    if let Some(action_lines) = action_lines {
        let final_action_lines = summary_action_only_lines(action_lines);
        if !final_action_lines.is_empty() {
            lines.extend(final_action_lines);
        }
    } else if let Some(summary) = summary {
        let final_action_lines = summary_action_only_lines(summary.plain_action_lines.as_slice());
        if !final_action_lines.is_empty() {
            lines.extend(final_action_lines);
        }
    }
    prompt.emit_plain_lines(&lines)?;
    Ok(())
}
