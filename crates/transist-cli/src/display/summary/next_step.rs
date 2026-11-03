// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use transist::{InspectionRunDetail, InspectionStep, TransistInspectionContext};
use transist::{InspectionWorkflowDetail, WorkflowStepSummary};

use crate::FAILED_STEP_EVENT_PAGER_LIMIT;
use crate::actionable::{ActionableRun, latest_step};
use crate::display::output::append_failed_step_blob_path_lines;
use crate::guidance::blocked_without_gate_guidance;

use super::{
    RunsShowStepFilter, SuggestedAction, SuggestedActionExecution, SummaryContent,
    dedupe_suggested_actions, format_failed_preflight_check_line, format_runs_show_command,
    gate_picker_suggested_actions,
};

pub(crate) fn build_blocked_summary_content(
    context: &transist::TransistContext,
    root_detail: &InspectionRunDetail,
    root_run_id: &str,
    actionable: &ActionableRun,
) -> Result<SummaryContent> {
    let Some(gate) = actionable.detail.blocked_gates.first() else {
        return Ok(blocked_without_gate_summary(
            context,
            root_detail,
            actionable,
            root_run_id,
        ));
    };

    let mut body_lines = vec![format!("Blocked on gate: {}", gate.gate_id)];
    let documents = gate.review_documents()?;
    if !documents.is_empty() {
        body_lines.push(format!(
            "Review documents: {} (exact revisions; no workspace snapshot)",
            documents.len()
        ));
        for document in documents {
            body_lines.push(format!(
                "  {}: {} bytes, {} files; revision {}; SHA-256 {}",
                document.revision.document,
                document.revision.size_bytes,
                document.revision.file_count,
                document.revision.revision_id,
                document.revision.blob.sha256
            ));
            body_lines.push(format!("  Content: {}", document.links.content));
            body_lines.push(format!(
                "  Revision / tree members: {}",
                document.links.metadata
            ));
        }
    }
    // Surface persisted preflight failures (e.g. asset-lock drift) even when
    // the run is blocked on a gate. Re-entry verification happens before
    // dispatch, so a blocked run with drifted plugin bytes stays blocked
    // forever — the operator needs to see why rather than just the gate id.
    if let Some(preflight) = root_detail.preflight.as_ref()
        && preflight.status.as_str() == "failed"
    {
        body_lines.push(format!(
            "Preflight blockers: {} blocker(s), {} warning(s)",
            preflight.blocker_count, preflight.warning_count
        ));
        for check in &preflight.failed_checks {
            body_lines.push(format_failed_preflight_check_line(check));
        }
    }
    if actionable.run_id != root_run_id {
        body_lines.push(format!(
            "Tip: use the root run id (`{root_run_id}`); Transist resolves this nested gate automatically."
        ));
    }
    let step_output_lines = crate::actionable::collect_manual_gate_and_workflow_file_lines(
        context,
        root_run_id,
        actionable,
        gate,
    )?;

    let gate_actor = transist::resolve_gate_actor_identity_in_home(context.home_dir())?;
    let mut suggested_actions =
        gate_picker_suggested_actions(root_run_id, Some(&actionable.run_id), gate_actor.as_deref());
    if let Some(preflight) = root_detail.preflight.as_ref()
        && preflight.status.as_str() == "failed"
    {
        suggested_actions.insert(
            0,
            SuggestedAction::new(SuggestedActionExecution::RunsPreflight {
                run_id: root_run_id.to_string(),
            }),
        );
    }

    Ok(SummaryContent {
        body_lines,
        step_output_lines,
        plain_action_lines: blocked_gate_plain_action_lines(root_run_id),
        interactive_action_lines: blocked_gate_interactive_action_lines(
            context,
            actionable,
            &gate.step_id,
        ),
        suggested_actions,
        selected_action_block_title: "Next:".to_string(),
    })
}

fn blocked_without_gate_summary(
    context: &transist::TransistContext,
    root_detail: &InspectionRunDetail,
    actionable: &ActionableRun,
    root_run_id: &str,
) -> SummaryContent {
    let guidance = blocked_without_gate_guidance(root_run_id);
    let workflow = load_workflow_detail_for_run(context, &actionable.detail);
    let mut body_lines = guidance[..2].to_vec();
    if let Some(preflight) = root_detail.preflight.as_ref()
        && preflight.status.as_str() == "failed"
    {
        body_lines.push(format!(
            "Preflight blockers: {} blocker(s), {} warning(s)",
            preflight.blocker_count, preflight.warning_count
        ));
        for check in &preflight.failed_checks {
            body_lines.push(format_failed_preflight_check_line(check));
        }
    }
    SummaryContent {
        body_lines,
        step_output_lines: Vec::new(),
        plain_action_lines: vec![
            guidance[2].clone(),
            guidance[3].clone(),
            guidance[4].clone(),
            "Inspect:".to_string(),
            format!("  transist runs show {}", actionable.run_id),
            format!("  transist runs tail {} --limit 80", actionable.run_id),
        ],
        interactive_action_lines: vec![
            "Continue:".to_string(),
            format!(
                "  {}",
                append_next_step_hint(
                    "continue execution until the next gate or terminal state",
                    workflow.as_ref(),
                    next_step_for_run_reentry(context, actionable).as_deref(),
                )
            ),
            "Inspect:".to_string(),
            "  show the persisted run record or tail recent events".to_string(),
            "Exit:".to_string(),
            "  leave this blocked run as-is and return to the shell".to_string(),
        ],
        suggested_actions: vec![
            SuggestedAction::new(SuggestedActionExecution::Run {
                run_id: root_run_id.to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: actionable.run_id.clone(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsTail {
                run_id: actionable.run_id.clone(),
                limit: 80,
            }),
            SuggestedAction::new(SuggestedActionExecution::Exit),
        ],
        selected_action_block_title: "Next:".to_string(),
    }
}

fn blocked_gate_plain_action_lines(root_run_id: &str) -> Vec<String> {
    vec![
        String::new(),
        "Continue:".to_string(),
        format!(
            "  transist runs gate approve {} --actor \"Reviewer <reviewer@example.com>\" --reason \"Reviewed output and approved\"",
            root_run_id
        ),
        format!(
            "  transist runs gate request-changes {} --actor \"Reviewer <reviewer@example.com>\" --reason \"...\"",
            root_run_id
        ),
        format!(
            "  transist runs gate reject {} --actor \"Reviewer <reviewer@example.com>\" --reason \"stop the run\"",
            root_run_id
        ),
    ]
}

fn blocked_gate_interactive_action_lines(
    context: &transist::TransistContext,
    actionable: &ActionableRun,
    gate_step_id: &str,
) -> Vec<String> {
    let workflow = load_workflow_detail_for_run(context, &actionable.detail);
    vec![
        "Continue:".to_string(),
        format!(
            "  {}",
            render_gate_approve_action_line(workflow.as_ref(), actionable, gate_step_id)
        ),
        format!(
            "  {}",
            render_gate_request_changes_action_line(workflow.as_ref(), actionable, gate_step_id)
        ),
        "  reject: stop the run here".to_string(),
        "Exit:".to_string(),
        "  leave this blocked run as-is and return to the shell".to_string(),
    ]
}

fn render_gate_approve_action_line(
    workflow: Option<&InspectionWorkflowDetail>,
    actionable: &ActionableRun,
    gate_step_id: &str,
) -> String {
    let next_step = next_step_after_gate_approval_with_workflow(workflow, actionable, gate_step_id);
    if let Some(workflow) = workflow {
        if let Some(loop_step) = enclosing_loop_step_summary(workflow, gate_step_id) {
            let exits_loop = loop_step
                .loop_body_refs
                .last()
                .map(|step_id| step_id == gate_step_id)
                .unwrap_or(false)
                && next_step
                    .as_deref()
                    .map(|step_id| {
                        !loop_step
                            .loop_body_refs
                            .iter()
                            .any(|candidate| candidate == step_id)
                    })
                    .unwrap_or(false);
            if exits_loop {
                if next_step
                    .as_deref()
                    .map(|step_id| step_is_finish(workflow, step_id))
                    .unwrap_or(false)
                {
                    return "approve: accept this cycle and finish the loop".to_string();
                }
                return append_next_step_hint(
                    "approve: accept this cycle and exit the loop",
                    Some(workflow),
                    next_step.as_deref(),
                );
            }
            return append_next_step_hint(
                "approve: accept this cycle and continue",
                Some(workflow),
                next_step.as_deref(),
            );
        }
    }
    append_next_step_hint(
        "approve: accept this gate and continue the run",
        workflow,
        next_step.as_deref(),
    )
}

fn render_gate_request_changes_action_line(
    workflow: Option<&InspectionWorkflowDetail>,
    actionable: &ActionableRun,
    gate_step_id: &str,
) -> String {
    let next_step =
        next_step_after_gate_request_changes_with_workflow(workflow, actionable, gate_step_id);
    let action = if workflow
        .and_then(|workflow| enclosing_loop_step_summary(workflow, gate_step_id))
        .is_some()
    {
        "request-changes: rerun this cycle with an optional reason"
    } else {
        "request-changes: rerun the preceding work"
    };
    append_next_step_hint(action, workflow, next_step.as_deref())
}

fn append_next_step_hint(
    action: &str,
    workflow: Option<&InspectionWorkflowDetail>,
    next_step: Option<&str>,
) -> String {
    next_step
        .map(str::trim)
        .filter(|step| !step.is_empty())
        .map(|step| {
            if workflow
                .map(|workflow| step_is_finish(workflow, step))
                .unwrap_or(false)
            {
                format!("{action}, then finish this workflow")
            } else {
                format!("{action}, then continue at {step}")
            }
        })
        .unwrap_or_else(|| action.to_string())
}

fn step_is_finish(workflow: &InspectionWorkflowDetail, step_id: &str) -> bool {
    workflow_step_summary(workflow, step_id)
        .and_then(|step| step.control.as_deref())
        .map(|control| control == "control.finish")
        .unwrap_or(false)
}

fn load_workflow_detail_for_run(
    context: &transist::TransistContext,
    detail: &InspectionRunDetail,
) -> Option<InspectionWorkflowDetail> {
    let workflow_ref = format!(
        "{}@{}",
        detail.summary.workflow_id, detail.summary.workflow_version
    );
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    inspection.get_workflow(&workflow_ref).ok().flatten()
}

fn workflow_step_summary<'a>(
    workflow: &'a InspectionWorkflowDetail,
    step_id: &str,
) -> Option<&'a WorkflowStepSummary> {
    workflow.steps.iter().find(|step| step.step_id == step_id)
}

fn enclosing_loop_step_summary<'a>(
    workflow: &'a InspectionWorkflowDetail,
    step_id: &str,
) -> Option<&'a WorkflowStepSummary> {
    workflow.steps.iter().rev().find(|step| {
        step.loop_body_refs
            .iter()
            .any(|candidate| candidate == step_id)
    })
}

fn expression_mentions_gate_decision(
    expression: Option<&str>,
    gate_step_id: &str,
    decision: &str,
) -> bool {
    let Some(expression) = expression else {
        return false;
    };
    expression.contains(&format!(
        "steps.{gate_step_id}.out.decision == \"{decision}\""
    )) || expression.contains(&format!(
        "steps.{gate_step_id}.out.decision == '{decision}'"
    ))
}

fn actionable_step_after(actionable: &ActionableRun, step_id: &str) -> Option<String> {
    let step_index = actionable
        .detail
        .steps
        .iter()
        .position(|step| step.step_id == step_id)?;
    actionable
        .detail
        .steps
        .iter()
        .skip(step_index + 1)
        .find(|step| is_pending_or_active_step_status(step.status.as_str()))
        .map(|step| step.step_id.clone())
}

fn fallback_request_changes_next_step(
    actionable: &ActionableRun,
    gate_step_id: &str,
) -> Option<String> {
    let step_index = actionable
        .detail
        .steps
        .iter()
        .position(|step| step.step_id == gate_step_id)?;
    actionable
        .detail
        .steps
        .iter()
        .take(step_index)
        .rev()
        .find(|step| {
            !matches!(
                step.status.as_str(),
                "pending" | "planned" | "created" | "skipped"
            )
        })
        .map(|step| step.step_id.clone())
        .or_else(|| Some(gate_step_id.to_string()))
}

fn is_pending_or_active_step_status(status: &str) -> bool {
    matches!(
        status,
        "created" | "planned" | "pending" | "running" | "dispatching" | "retry_wait" | "blocked"
    )
}

fn next_step_for_run_reentry(
    context: &transist::TransistContext,
    actionable: &ActionableRun,
) -> Option<String> {
    actionable
        .detail
        .current_step
        .as_ref()
        .map(|step| step.step_id.clone())
        .or_else(|| {
            load_workflow_detail_for_run(context, &actionable.detail)
                .and_then(|workflow| workflow.steps.first().map(|step| step.step_id.clone()))
        })
        .or_else(|| {
            actionable
                .detail
                .steps
                .iter()
                .find(|step| is_pending_or_active_step_status(step.status.as_str()))
                .map(|step| step.step_id.clone())
        })
}

fn next_step_after_gate_approval_with_workflow(
    workflow: Option<&InspectionWorkflowDetail>,
    actionable: &ActionableRun,
    gate_step_id: &str,
) -> Option<String> {
    if let Some(workflow) = workflow {
        if let Some(loop_step) = enclosing_loop_step_summary(workflow, gate_step_id) {
            let is_last_loop_body_step = loop_step
                .loop_body_refs
                .last()
                .map(|step_id| step_id == gate_step_id)
                .unwrap_or(false);
            if is_last_loop_body_step {
                if let Some(target) = loop_step
                    .graph_edges
                    .iter()
                    .find(|edge| {
                        edge.kind == "exit"
                            && (expression_mentions_gate_decision(
                                edge.guard.as_deref(),
                                gate_step_id,
                                "approved",
                            ) || expression_mentions_gate_decision(
                                loop_step.loop_until.as_deref(),
                                gate_step_id,
                                "approved",
                            ))
                    })
                    .or_else(|| {
                        loop_step
                            .graph_edges
                            .iter()
                            .find(|edge| edge.kind == "exit")
                    })
                {
                    return Some(target.target_step_id.clone());
                }
            }
        }
        if let Some(step) = workflow_step_summary(workflow, gate_step_id) {
            if let Some(target) = step
                .graph_edges
                .iter()
                .find(|edge| {
                    expression_mentions_gate_decision(
                        edge.guard.as_deref(),
                        gate_step_id,
                        "approved",
                    )
                })
                .or_else(|| step.graph_edges.iter().find(|edge| edge.kind == "next"))
            {
                return Some(target.target_step_id.clone());
            }
        }
    }
    actionable_step_after(actionable, gate_step_id)
}

fn next_step_after_gate_request_changes_with_workflow(
    workflow: Option<&InspectionWorkflowDetail>,
    actionable: &ActionableRun,
    gate_step_id: &str,
) -> Option<String> {
    if let Some(workflow) = workflow {
        if let Some(step) = workflow_step_summary(workflow, gate_step_id) {
            if let Some(target) = step.graph_edges.iter().find(|edge| {
                expression_mentions_gate_decision(
                    edge.guard.as_deref(),
                    gate_step_id,
                    "request_changes",
                )
            }) {
                return Some(target.target_step_id.clone());
            }
        }
        if let Some(loop_step) = enclosing_loop_step_summary(workflow, gate_step_id) {
            if let Some(first_loop_body_step) = loop_step.loop_body_refs.first() {
                return Some(first_loop_body_step.clone());
            }
        }
    }
    fallback_request_changes_next_step(actionable, gate_step_id)
}

fn first_workflow_step_id(
    context: &transist::TransistContext,
    detail: &InspectionRunDetail,
) -> Option<String> {
    load_workflow_detail_for_run(context, detail)
        .and_then(|workflow| workflow.steps.first().map(|step| step.step_id.clone()))
        .or_else(|| detail.steps.first().map(|step| step.step_id.clone()))
}

pub(crate) fn build_failed_summary_content(
    context: &transist::TransistContext,
    root_detail: &InspectionRunDetail,
    actionable: &ActionableRun,
    run_id: &str,
) -> SummaryContent {
    let root_workflow = load_workflow_detail_for_run(context, root_detail);
    let actionable_workflow = load_workflow_detail_for_run(context, &actionable.detail);
    if latest_step(&actionable.detail, &["failed"]).is_none()
        && root_detail
            .preflight
            .as_ref()
            .map(|preflight| preflight.status.as_str() == "failed")
            .unwrap_or(false)
    {
        let mut body_lines = vec!["Outcome: preflight failed before step dispatch.".to_string()];
        if let Some(preflight) = root_detail.preflight.as_ref() {
            body_lines.push(format!(
                "Preflight blockers: {} blocker(s), {} warning(s)",
                preflight.blocker_count, preflight.warning_count
            ));
            for check in &preflight.failed_checks {
                body_lines.push(format_failed_preflight_check_line(check));
            }
        }
        return SummaryContent {
            body_lines,
            step_output_lines: Vec::new(),
            plain_action_lines: vec![
                "Inspect:".to_string(),
                format!(
                    "  {}",
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed)
                ),
                format!("  transist runs preflight {}", actionable.run_id),
                format!("  transist runs retry {}", actionable.run_id),
            ],
            interactive_action_lines: vec![
                "Inspect:".to_string(),
                "  show only the failed preflight checks (transist runs show --failed) or rerun preflight"
                    .to_string(),
                "Retry:".to_string(),
                format!(
                    "  {}",
                    append_next_step_hint(
                        "retry this run after you fix the preflight blockers",
                        root_workflow.as_ref(),
                        first_workflow_step_id(context, root_detail).as_deref(),
                    )
                ),
                "Exit:".to_string(),
                "  leave this failed run as-is and return to the shell".to_string(),
            ],
            suggested_actions: vec![
                SuggestedAction::new(SuggestedActionExecution::RunsShowFailed {
                    run_id: actionable.run_id.clone(),
                }),
                SuggestedAction::new(SuggestedActionExecution::RunsPreflight {
                    run_id: actionable.run_id.clone(),
                }),
                SuggestedAction::new(SuggestedActionExecution::RunsResume {
                    run_id: actionable.run_id.clone(),
                }),
                SuggestedAction::new(SuggestedActionExecution::Exit),
            ],
            selected_action_block_title: "Next:".to_string(),
        };
    }

    let failed_step = latest_step(&actionable.detail, &["failed"]);
    let mut body_lines = Vec::new();
    let mut step_output_lines = Vec::new();
    if let Some(step) = failed_step {
        let failed_step_id = failed_summary_step_id(step);
        step_output_lines.push(format!("Failed step: {failed_step_id}"));
        if let Some(child_run_id) = step
            .output_summary
            .child_failure
            .as_ref()
            .map(|failure| failure.child_run_id.as_str())
        {
            step_output_lines.push(format!("Child run: {child_run_id}"));
        }
        if let Some(message) = failed_summary_message(step) {
            step_output_lines.push(format!("Failure reason: {}", message));
        }
        append_failed_step_blob_path_lines(
            &mut step_output_lines,
            context.home_dir(),
            &actionable.run_id,
            step,
        );
    }

    let mut plain_action_lines = Vec::new();
    let interactive_action_lines;
    let mut suggested_actions = Vec::new();
    let cleanup_required = root_detail.recovery_state.as_deref() == Some("cleanup_required");
    let cleanup_completed = root_detail.recovery_state.as_deref() == Some("cleanup_completed");
    if actionable.run_id != actionable.root_run_id {
        body_lines.push(format!(
            "Outcome: root run {} stopped because child subflow run {} failed.",
            run_id, actionable.run_id
        ));
        body_lines.push("You do not need to resume the child separately.".to_string());
        if cleanup_required {
            body_lines.push(
                "The failed run is no longer resumable because explicit cleanup has begun. Retry cleanup until it succeeds or is skipped."
                    .to_string(),
            );
            plain_action_lines.extend([
                "Cleanup:".to_string(),
                format!("  transist runs cleanup {}", actionable.root_run_id),
                "Inspect:".to_string(),
                format!(
                    "  {}",
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed)
                ),
                format!("  transist runs tail {} --limit 80", actionable.run_id),
            ]);
            interactive_action_lines = vec![
                "Cleanup:".to_string(),
                "  run explicit root-level cleanup if you want to stop recovering this failed run"
                    .to_string(),
                "Inspect:".to_string(),
                "  open failed step details or recent failed-step events in a pager".to_string(),
                "Exit:".to_string(),
                "  leave this failed run as-is and return to the shell".to_string(),
            ];
        } else if cleanup_completed {
            body_lines
                .push("Cleanup is complete; the failed run is no longer resumable.".to_string());
            plain_action_lines.extend([
                "Inspect:".to_string(),
                format!(
                    "  {}",
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed)
                ),
                format!("  transist runs tail {} --limit 80", actionable.run_id),
            ]);
            interactive_action_lines = vec![
                "Inspect:".to_string(),
                "  open failed step details or recent failed-step events in a pager".to_string(),
                "Exit:".to_string(),
                "  leave this failed run as-is and return to the shell".to_string(),
            ];
        } else {
            body_lines.push(
                "Next: retry the root run and Transist will re-enter that failed child subflow automatically."
                    .to_string(),
            );
            plain_action_lines.extend([
                "Retry:".to_string(),
                format!("  transist runs retry {}", run_id),
                "Cleanup:".to_string(),
                format!("  transist runs cleanup {}", actionable.root_run_id),
                "Inspect:".to_string(),
                format!(
                    "  {}",
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed)
                ),
                format!("  transist runs tail {} --limit 80", actionable.run_id),
            ]);
            interactive_action_lines = vec![
                "Retry:".to_string(),
                format!(
                    "  {}",
                    append_next_step_hint(
                        "retry the root run to re-enter the failed child subflow",
                        actionable_workflow.as_ref(),
                        failed_step.map(|step| step.step_id.as_str()),
                    )
                ),
                "Cleanup:".to_string(),
                "  run explicit root-level cleanup if you want to stop recovering this failed run"
                    .to_string(),
                "Inspect:".to_string(),
                "  open failed step details or recent failed-step events in a pager".to_string(),
                "Exit:".to_string(),
                "  leave this failed run as-is and return to the shell".to_string(),
            ];
            suggested_actions.push(SuggestedAction::new(SuggestedActionExecution::RunsResume {
                run_id: run_id.to_string(),
            }));
        }
        suggested_actions.push(SuggestedAction::new(
            SuggestedActionExecution::RunsCleanup {
                run_id: actionable.root_run_id.clone(),
            },
        ));
    } else {
        body_lines.push("Outcome: the run failed in-place.".to_string());
        if cleanup_required {
            body_lines.push(
                "The failed run is no longer resumable because explicit cleanup has begun. Retry cleanup until it succeeds or is skipped."
                    .to_string(),
            );
            plain_action_lines.extend([
                "Cleanup:".to_string(),
                format!("  transist runs cleanup {}", actionable.root_run_id),
                "Inspect:".to_string(),
                format!(
                    "  {}",
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed)
                ),
                format!("  transist runs tail {} --limit 80", actionable.run_id),
            ]);
            interactive_action_lines = vec![
                "Cleanup:".to_string(),
                "  run explicit cleanup if you want to stop recovering this failed run".to_string(),
                "Inspect:".to_string(),
                "  open failed step details or recent failed-step events in a pager".to_string(),
                "Exit:".to_string(),
                "  leave this failed run as-is and return to the shell".to_string(),
            ];
        } else if cleanup_completed {
            body_lines
                .push("Cleanup is complete; the failed run is no longer resumable.".to_string());
            plain_action_lines.extend([
                "Inspect:".to_string(),
                format!(
                    "  {}",
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed)
                ),
                format!("  transist runs tail {} --limit 80", actionable.run_id),
            ]);
            interactive_action_lines = vec![
                "Inspect:".to_string(),
                "  open failed step details or recent failed-step events in a pager".to_string(),
                "Exit:".to_string(),
                "  leave this failed run as-is and return to the shell".to_string(),
            ];
        } else {
            body_lines.push(format!(
                "Next: retry this same run directly to retry the failed step. There is no deeper actionable subflow below {}.",
                actionable.run_id
            ));
            plain_action_lines.extend([
                "Retry:".to_string(),
                format!("  transist runs retry {}", actionable.run_id),
                "Cleanup:".to_string(),
                format!("  transist runs cleanup {}", actionable.root_run_id),
                "Inspect:".to_string(),
                format!(
                    "  {}",
                    format_runs_show_command(&actionable.run_id, RunsShowStepFilter::Failed)
                ),
                format!("  transist runs tail {} --limit 80", actionable.run_id),
            ]);
            interactive_action_lines = vec![
                "Retry:".to_string(),
                format!(
                    "  {}",
                    append_next_step_hint(
                        "retry this run to retry the failed work",
                        actionable_workflow.as_ref(),
                        failed_step.map(|step| step.step_id.as_str()),
                    )
                ),
                "Cleanup:".to_string(),
                "  run explicit cleanup if you want to stop recovering this failed run".to_string(),
                "Inspect:".to_string(),
                "  open failed step details or recent failed-step events in a pager".to_string(),
                "Exit:".to_string(),
                "  leave this failed run as-is and return to the shell".to_string(),
            ];
            suggested_actions.push(SuggestedAction::new(SuggestedActionExecution::RunsResume {
                run_id: actionable.run_id.clone(),
            }));
        }
        if !cleanup_completed {
            suggested_actions.push(SuggestedAction::new(
                SuggestedActionExecution::RunsCleanup {
                    run_id: actionable.root_run_id.clone(),
                },
            ));
        }
    }

    if let Some(step) = latest_step(&actionable.detail, &["failed"]) {
        suggested_actions.push(SuggestedAction::new(
            SuggestedActionExecution::InspectFailedStepDetails {
                run_id: actionable.run_id.clone(),
                step_id: step.step_id.clone(),
            },
        ));
        // Prefer a direct stderr-artifact inspector when one exists; that is
        // almost always the most useful next click for a failed process step.
        // Fall back to the events pager when there is no stderr artifact.
        let stderr_artifact = step.output_summary.artifact_refs.iter().find(|artifact| {
            artifact.name.eq_ignore_ascii_case("stderr.txt")
                || artifact.name.ends_with("_stderr.txt")
                || artifact.name.eq_ignore_ascii_case("stderr")
        });
        if let Some(artifact) = stderr_artifact {
            suggested_actions.push(SuggestedAction::new(
                SuggestedActionExecution::InspectFailedStepStderr {
                    run_id: actionable.run_id.clone(),
                    step_id: step.step_id.clone(),
                    artifact_relative_path: artifact.relative_path.clone(),
                },
            ));
        } else {
            suggested_actions.push(SuggestedAction::new(
                SuggestedActionExecution::InspectFailedStepEvents {
                    run_id: actionable.run_id.clone(),
                    step_id: step.step_id.clone(),
                    limit: FAILED_STEP_EVENT_PAGER_LIMIT,
                },
            ));
        }
    }
    suggested_actions.push(SuggestedAction::new(SuggestedActionExecution::Exit));

    SummaryContent {
        body_lines,
        step_output_lines,
        plain_action_lines,
        interactive_action_lines,
        suggested_actions: dedupe_suggested_actions(suggested_actions),
        selected_action_block_title: "Next:".to_string(),
    }
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
