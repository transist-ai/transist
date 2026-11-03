// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;

use crate::guidance::render_gate_command;
use crate::prompt::PromptUi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunsShowStepFilter {
    All,
    Failed,
    Success,
}

pub(crate) fn format_runs_show_command(run_id: &str, filter: RunsShowStepFilter) -> String {
    match filter {
        RunsShowStepFilter::All => format!("transist runs show {}", run_id),
        RunsShowStepFilter::Failed => format!("transist runs show {} --failed", run_id),
        RunsShowStepFilter::Success => format!("transist runs show {} --success", run_id),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SuggestedAction {
    pub(crate) display_command: String,
    pub(crate) execution: SuggestedActionExecution,
}

impl SuggestedAction {
    pub(crate) fn new(execution: SuggestedActionExecution) -> Self {
        Self {
            display_command: execution.render_command(),
            execution,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SuggestedActionExecution {
    Exit,
    Run {
        run_id: String,
    },
    Approve {
        run_id: String,
        gate_id: Option<String>,
        actor: Option<String>,
        reason: Option<String>,
    },
    Reject {
        run_id: String,
        gate_id: Option<String>,
        actor: Option<String>,
        reason: Option<String>,
    },
    RequestChanges {
        run_id: String,
        gate_id: Option<String>,
        actor: Option<String>,
        reason: Option<String>,
    },
    RunsResume {
        run_id: String,
    },
    RunsCleanup {
        run_id: String,
    },
    CancelRun {
        run_id: String,
    },
    RunsShow {
        run_id: String,
    },
    RunsShowFailed {
        run_id: String,
    },
    RunsPreflight {
        run_id: String,
    },
    RunsCapabilities {
        run_id: String,
    },
    /// Opens the persisted, redacted diagnostic report in the fullscreen TUI.
    ViewFullDiagnostic {
        run_id: String,
    },
    RunsTail {
        run_id: String,
        limit: usize,
    },
    InspectFailedStepDetails {
        run_id: String,
        step_id: String,
    },
    InspectFailedStepEvents {
        run_id: String,
        step_id: String,
        limit: usize,
    },
    InspectFailedStepStderr {
        run_id: String,
        step_id: String,
        artifact_relative_path: String,
    },
    InspectCleanupResult {
        run_id: String,
    },
    /// Spawns a detached replay runner with a fresh run-ref (auto-generated UUID).
    /// Operators wanting a custom run-ref drop to `transist runs replay <id> <ref>`.
    RunsReplay {
        run_id: String,
    },
    /// Renders the live lease record for the run-tree as paged JSON.
    LeaseInspect {
        run_id: String,
    },
    /// Removes the lease file for an apparently-orphaned run-tree.
    /// The picker variant always sends `force_live_owner = false`; reaping a
    /// live owner requires the CLI's explicit `--force-live-owner` flag.
    LeaseBreak {
        run_id: String,
    },
}

impl SuggestedActionExecution {
    pub(crate) fn action_label(&self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Run { .. } => "run",
            Self::Approve { .. } => "approve",
            Self::Reject { .. } => "reject",
            Self::RequestChanges { .. } => "request-changes",
            Self::RunsResume { .. } => "retry",
            Self::RunsCleanup { .. } => "cleanup",
            Self::CancelRun { .. } => "cancel-run",
            Self::RunsShow { .. } => "show",
            Self::RunsShowFailed { .. } => "show-failed",
            Self::RunsPreflight { .. } => "preflight",
            Self::RunsCapabilities { .. } => "capabilities",
            Self::ViewFullDiagnostic { .. } => "view-full-diagnostic",
            Self::RunsTail { .. } => "tail",
            Self::InspectFailedStepDetails { .. } => "inspect-failed-step-details",
            Self::InspectFailedStepEvents { .. } => "inspect-failed-step-events",
            Self::InspectFailedStepStderr { .. } => "inspect-failed-step-stderr",
            Self::InspectCleanupResult { .. } => "inspect-cleanup-result",
            Self::RunsReplay { .. } => "replay",
            Self::LeaseInspect { .. } => "lease-inspect",
            Self::LeaseBreak { .. } => "lease-break",
        }
    }

    pub(crate) fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Run { .. }
                | Self::Approve { .. }
                | Self::Reject { .. }
                | Self::RequestChanges { .. }
                | Self::RunsResume { .. }
                | Self::RunsCleanup { .. }
                | Self::CancelRun { .. }
                | Self::RunsReplay { .. }
                | Self::LeaseBreak { .. }
        )
    }

    pub(crate) fn render_command(&self) -> String {
        match self {
            Self::Exit => "exit the run".to_string(),
            Self::Run { run_id } => format!("transist runs start {}", run_id),
            Self::Approve {
                run_id,
                gate_id,
                actor,
                reason,
            } => render_gate_command(
                "approve",
                run_id,
                gate_id,
                actor.as_deref(),
                reason.as_deref(),
            ),
            Self::Reject {
                run_id,
                gate_id,
                actor,
                reason,
            } => render_gate_command(
                "reject",
                run_id,
                gate_id,
                actor.as_deref(),
                reason.as_deref(),
            ),
            Self::RequestChanges {
                run_id,
                gate_id,
                actor,
                reason,
            } => render_gate_command(
                "request-changes",
                run_id,
                gate_id,
                actor.as_deref(),
                reason.as_deref(),
            ),
            Self::RunsResume { run_id } => format!("transist runs retry {}", run_id),
            Self::RunsCleanup { run_id } => format!("transist runs cleanup {}", run_id),
            Self::CancelRun { run_id } => format!("transist runs cancel {}", run_id),
            Self::RunsShow { run_id } => format_runs_show_command(run_id, RunsShowStepFilter::All),
            Self::RunsShowFailed { run_id } => {
                format_runs_show_command(run_id, RunsShowStepFilter::Failed)
            }
            Self::RunsPreflight { run_id } => format!("transist runs preflight {}", run_id),
            Self::RunsCapabilities { run_id } => {
                format!("transist runs capabilities {} --format yaml", run_id)
            }
            Self::ViewFullDiagnostic { .. } => "View full diagnostic".to_string(),
            Self::RunsTail { run_id, limit } => {
                format!("transist runs tail {} --limit {}", run_id, limit)
            }
            Self::InspectFailedStepDetails { .. } => "inspect failed step details".to_string(),
            Self::InspectFailedStepEvents { .. } => "inspect failed step events".to_string(),
            Self::InspectFailedStepStderr { .. } => "inspect failed step stderr".to_string(),
            Self::InspectCleanupResult { .. } => "view last cleanup result".to_string(),
            Self::RunsReplay { run_id } => format!("transist runs replay {}", run_id),
            Self::LeaseInspect { run_id } => format!("transist runs lease inspect {}", run_id),
            Self::LeaseBreak { run_id } => format!("transist runs lease break {}", run_id),
        }
    }

    pub(crate) fn reason_prompt(&self) -> Option<&'static str> {
        match self {
            Self::Approve { .. } => Some("Approval reason (required)"),
            Self::Reject { .. } => Some("Reject reason (required)"),
            Self::RequestChanges { .. } => Some("Request-changes reason (required)"),
            _ => None,
        }
    }

    pub(crate) fn confirmation_prompt(&self) -> Option<(&'static str, String)> {
        match self {
            Self::Approve { .. } => Some((
                "Approve and resume run?",
                "Approving records the gate decision and immediately resumes the blocked run. Proceed?"
                    .to_string(),
            )),
            Self::RequestChanges { .. } => Some((
                "Request changes and resume run?",
                "Requesting changes records the gate decision and immediately resumes the blocked run, which may rerun preceding work. Proceed?"
                    .to_string(),
            )),
            _ => None,
        }
    }

    /// Stable identity used by backends to scope a preserved reason draft
    /// to a single (action kind, run, gate) so an Esc-out + re-entry on
    /// the same action re-seeds the textarea, while two different rejects
    /// never share state. Returns `None` for actions that don't take a
    /// reason; those never reach the keyed prompt path.
    pub(crate) fn draft_key(&self) -> Option<String> {
        let (kind, run_id, gate_id) = match self {
            Self::Approve {
                run_id, gate_id, ..
            } => ("approve", run_id, gate_id.as_deref()),
            Self::Reject {
                run_id, gate_id, ..
            } => ("reject", run_id, gate_id.as_deref()),
            Self::RequestChanges {
                run_id, gate_id, ..
            } => ("request-changes", run_id, gate_id.as_deref()),
            _ => return None,
        };
        Some(format!("{}:{}:{}", kind, run_id, gate_id.unwrap_or("-")))
    }

    pub(crate) fn with_prompted_reason(&self, reason: Option<String>) -> Self {
        match self {
            Self::Approve {
                run_id,
                gate_id,
                actor,
                ..
            } => Self::Approve {
                run_id: run_id.clone(),
                gate_id: gate_id.clone(),
                actor: actor.clone(),
                reason,
            },
            Self::Reject {
                run_id,
                gate_id,
                actor,
                ..
            } => Self::Reject {
                run_id: run_id.clone(),
                gate_id: gate_id.clone(),
                actor: actor.clone(),
                reason,
            },
            Self::RequestChanges {
                run_id,
                gate_id,
                actor,
                ..
            } => Self::RequestChanges {
                run_id: run_id.clone(),
                gate_id: gate_id.clone(),
                actor: actor.clone(),
                reason,
            },
            other => other.clone(),
        }
    }

    pub(crate) fn exits_transist(&self) -> bool {
        matches!(self, Self::Exit)
    }

    pub(crate) fn leaves_selection_trace(&self) -> bool {
        matches!(
            self,
            Self::Exit
                | Self::Run { .. }
                | Self::Approve { .. }
                | Self::Reject { .. }
                | Self::RequestChanges { .. }
                | Self::RunsResume { .. }
                | Self::RunsCleanup { .. }
                | Self::CancelRun { .. }
                | Self::RunsReplay { .. }
                | Self::LeaseBreak { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuggestedActionOutcome {
    Completed(bool),
    ReopenPicker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSuggestedAction {
    pub(crate) display_command: String,
    pub(crate) execution: SuggestedActionExecution,
}

pub(crate) fn dedupe_suggested_actions(actions: Vec<SuggestedAction>) -> Vec<SuggestedAction> {
    let mut unique = Vec::new();
    for action in actions {
        if unique
            .iter()
            .any(|existing: &SuggestedAction| existing.display_command == action.display_command)
        {
            continue;
        }
        unique.push(action);
    }
    unique
}
/// Build the action list shown next to a blocked manual gate. The
/// caller passes `root_run_id` (the run owning the gate from the
/// operator's command-line perspective) and, when known,
/// `gate_run_id` (the innermost descendant run that actually carries
/// the blocked gate, e.g. `root-run` →
/// `root-run__pr_review_loop__review_cycle__review_changes_flow__review_plan_loop`).
///
/// We always emit the three decision actions
/// (`Approve` / `RequestChanges` / `Reject`). When `gate_run_id` is
/// provided and differs from the root, we additionally emit a single
/// `RunsShow` targeting the gate run so the operator can drill into
/// the subflow that owns the gate without re-deriving the descendant
/// id from JSON. Surfaces that already attach broader inspect
/// commands separately (`advice.rs` adds `runs show` / `runs tail`
/// on top) intentionally pass `None` here to avoid duplicates.
///
/// The fullscreen Details pane intentionally hides ancestor subflow
/// runs at a gate (see `append_gate_chain_detail_lines`); the
/// gate-run inspect command added here is the recovery path for
/// "I want to see everything about the run/subflow I just collapsed."
pub(crate) fn gate_suggested_actions(
    root_run_id: &str,
    gate_run_id: Option<&str>,
    actor: Option<&str>,
) -> Vec<SuggestedAction> {
    let mut actions = vec![
        SuggestedAction::new(SuggestedActionExecution::Approve {
            run_id: root_run_id.to_string(),
            gate_id: None,
            actor: actor.map(str::to_string),
            reason: Some("Reviewed output and approved".to_string()),
        }),
        SuggestedAction::new(SuggestedActionExecution::RequestChanges {
            run_id: root_run_id.to_string(),
            gate_id: None,
            actor: actor.map(str::to_string),
            reason: Some("...".to_string()),
        }),
        SuggestedAction::new(SuggestedActionExecution::Reject {
            run_id: root_run_id.to_string(),
            gate_id: None,
            actor: actor.map(str::to_string),
            reason: Some("stop the run".to_string()),
        }),
    ];
    if let Some(gate_run_id) = gate_run_id {
        if gate_run_id != root_run_id {
            actions.push(SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: gate_run_id.to_string(),
            }));
        }
    }
    actions
}

pub(crate) fn gate_picker_suggested_actions(
    root_run_id: &str,
    gate_run_id: Option<&str>,
    actor: Option<&str>,
) -> Vec<SuggestedAction> {
    let mut actions = gate_suggested_actions(root_run_id, gate_run_id, actor);
    actions.push(SuggestedAction::new(SuggestedActionExecution::Exit));
    actions
}

pub(crate) fn resolve_selected_action(
    prompt: &mut dyn PromptUi,
    action: &SuggestedAction,
) -> Result<Option<ResolvedSuggestedAction>> {
    use crate::prompt::PromptInputOutcome;
    let execution = if let Some(reason_prompt) = action.execution.reason_prompt() {
        // Build a stable key per (action kind, run, gate) so backends that
        // preserve drafts across Esc-out + re-entry don't leak text from
        // one rejected gate into another. Falls back to the prompt label
        // when the action has no run/gate context (no current variants
        // hit this branch — defensive only).
        let key = action
            .execution
            .draft_key()
            .unwrap_or_else(|| reason_prompt.to_string());
        match prompt.prompt_optional_reason_keyed(reason_prompt, &key)? {
            PromptInputOutcome::Submitted(reason) => {
                let reason = reason.trim();
                if reason.is_empty() {
                    return Ok(None);
                }
                action
                    .execution
                    .with_prompted_reason(Some(reason.to_string()))
            }
            PromptInputOutcome::Canceled => return Ok(None),
        }
    } else {
        action.execution.clone()
    };

    if let Some((title, message)) = execution.confirmation_prompt() {
        if !prompt.confirm_suggested_action(title, &message)? {
            return Ok(None);
        }
    }

    Ok(Some(ResolvedSuggestedAction {
        display_command: execution.render_command(),
        execution,
    }))
}

#[cfg(test)]
mod tests {
    use super::SuggestedActionExecution;

    #[test]
    fn cancel_run_renders_cli_command() {
        assert_eq!(
            SuggestedActionExecution::CancelRun {
                run_id: "demo-run".to_string(),
            }
            .render_command(),
            "transist runs cancel demo-run"
        );
    }

    #[test]
    fn capabilities_renders_yaml_remediation_command() {
        let action = SuggestedActionExecution::RunsCapabilities {
            run_id: "demo-run".to_string(),
        };

        assert_eq!(
            action.render_command(),
            "transist runs capabilities demo-run --format yaml"
        );
        assert!(!action.is_mutating());
    }

    #[test]
    fn approve_renders_resolved_actor() {
        assert_eq!(
            SuggestedActionExecution::Approve {
                run_id: "demo-run".to_string(),
                gate_id: None,
                actor: Some("reviewer@example.com".to_string()),
                reason: Some("looks good".to_string()),
            }
            .render_command(),
            "transist runs gate approve demo-run --actor \"reviewer@example.com\" --reason \"looks good\""
        );
    }

    #[test]
    fn request_changes_confirmation_warns_that_work_may_rerun() {
        let action = SuggestedActionExecution::RequestChanges {
            run_id: "demo-run".to_string(),
            gate_id: None,
            actor: None,
            reason: Some("tighten wording".to_string()),
        };
        let (title, message) = action
            .confirmation_prompt()
            .expect("request-changes confirmation prompt");

        assert_eq!(title, "Request changes and resume run?");
        assert!(message.contains("records the gate decision"));
        assert!(message.contains("immediately resumes the blocked run"));
        assert!(message.contains("may rerun preceding work"));
        assert!(message.ends_with("Proceed?"));
    }
}
