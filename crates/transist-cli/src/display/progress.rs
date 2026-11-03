// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use dialoguer::console::Style;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, IsTerminal};
use transist::{InspectionRunDetail, InspectionStep};

use crate::actionable::ActionableRun;
use crate::terminal::current_unix_ms;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgressState {
    pub(crate) root_run_id: String,
    pub(crate) run_status: String,
    pub(crate) actionable_run_id: String,
    pub(crate) top_level_step_id: Option<String>,
    pub(crate) current_step_path: Option<String>,
    pub(crate) current_step_status: Option<String>,
    pub(crate) execution: Option<u32>,
    pub(crate) cycle: Option<u32>,
    pub(crate) tool_name: Option<String>,
    pub(crate) started_at_ms: Option<u64>,
    pub(crate) finished_at_ms: Option<u64>,
    pub(crate) error_code: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) checklist: Vec<ProgressChecklistItem>,
}

impl ProgressState {
    pub(crate) fn elapsed_seconds(&self) -> Option<u64> {
        let started_at = self.started_at_ms?;
        let finished_at = match self.current_step_status.as_deref() {
            Some("running" | "dispatching" | "retry_wait") => current_unix_ms(),
            _ => self.finished_at_ms?,
        };
        finished_at
            .checked_sub(started_at)
            .map(|elapsed_ms| elapsed_ms / 1000)
    }
}

pub(crate) fn format_progress_state_detail_lines(state: &ProgressState) -> Vec<String> {
    let mut lines = Vec::new();
    let show_result_snapshot =
        should_show_step_result_snapshot(state.current_step_status.as_deref());
    if let Some(path) = state.current_step_path.as_ref() {
        lines.push(format!("Path: {}", path));
    }
    if let Some(status) = state.current_step_status.as_ref() {
        lines.push(format!("Step state: {}", status));
    }
    if let Some(tool_name) = state.tool_name.as_ref() {
        lines.push(format!("Tool: {}", tool_name));
    }
    if let Some(execution) = state.execution {
        lines.push(format!("Execution: {}", execution));
    }
    if let Some(cycle) = state.cycle {
        lines.push(format!("Cycle: {}", cycle));
    }
    if let Some(elapsed) = state.elapsed_seconds() {
        lines.push(format!(
            "{}: {}s",
            progress_elapsed_label(state.current_step_status.as_deref()),
            elapsed
        ));
    }
    if show_result_snapshot {
        if let Some(error_code) = state.error_code.as_ref() {
            lines.push(format!("Error code: {}", error_code));
        }
        if let Some(exit_code) = state.exit_code {
            lines.push(format!("Exit code: {}", exit_code));
        }
    }
    lines
}

fn should_show_step_result_snapshot(current_step_status: Option<&str>) -> bool {
    !current_step_status.is_some_and(crate::actionable::is_live_progress_step)
}

fn progress_elapsed_label(current_step_status: Option<&str>) -> &'static str {
    match current_step_status {
        Some("running" | "dispatching" | "retry_wait") => "Active",
        Some("blocked") => "Blocked for",
        _ => "Wall clock",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChecklistState {
    Success,
    Active,
    Pending,
    Blocked,
    Failed,
    Canceled,
    Skipped,
}

impl ChecklistState {
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::Success => "[✔]",
            Self::Active => "[>]",
            Self::Pending => "[ ]",
            Self::Blocked => "[-]",
            Self::Failed => "[!]",
            Self::Canceled => "[x]",
            Self::Skipped => "[ ]",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChecklistOverride {
    pub(crate) step_id: String,
    pub(crate) state: ChecklistState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StepTimingBounds {
    pub(crate) started_at_ms: Option<u64>,
    pub(crate) finished_at_ms: Option<u64>,
}

pub(crate) type StepTimingOverrides = BTreeMap<String, StepTimingBounds>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProgressChecklistItem {
    pub(crate) step_id: String,
    pub(crate) state: ChecklistState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) duration_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActionableRunState {
    pub(crate) run_id: String,
    pub(crate) state: Value,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CurrentStepStateRef<'a> {
    pub(crate) step_id: &'a str,
    pub(crate) step: &'a Value,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProgressStateFromStatesArgs<'a> {
    pub(crate) root_run_id: &'a str,
    pub(crate) root_state: &'a Value,
    pub(crate) root_step_order: &'a [String],
    pub(crate) actionable: &'a ActionableRunState,
    pub(crate) current_step: CurrentStepStateRef<'a>,
    pub(crate) live_active_override: bool,
}

pub(crate) fn build_progress_state_from_states(
    context: &transist::TransistContext,
    args: ProgressStateFromStatesArgs<'_>,
    tool_name: Option<String>,
) -> Result<ProgressState> {
    let visible_step_order = crate::execution::load_visible_recipe_step_order(
        context,
        args.root_run_id,
        args.root_state,
    )?
    .unwrap_or_else(|| args.root_step_order.to_vec());
    let step_timing_overrides = crate::execution::subflow_step_timing_overrides_for_state(
        context,
        args.root_run_id,
        args.root_state,
    )?;
    Ok(build_progress_state_from_states_with_timing_overrides(
        ProgressStateFromStatesArgs {
            root_step_order: &visible_step_order,
            ..args
        },
        tool_name,
        &step_timing_overrides,
    ))
}

pub(crate) fn build_progress_state_from_states_with_timing_overrides(
    args: ProgressStateFromStatesArgs<'_>,
    tool_name: Option<String>,
    step_timing_overrides: &StepTimingOverrides,
) -> ProgressState {
    let current_step_status = args.current_step.step["status"].as_str();
    let show_result_snapshot = should_show_step_result_snapshot(current_step_status);

    let top_level_step_id = progress_top_level_step_id(
        args.root_run_id,
        &args.actionable.run_id,
        args.current_step.step_id,
    );
    let checklist_override = progress_checklist_override_from_state(
        args.root_run_id,
        args.actionable,
        top_level_step_id.as_deref(),
        args.live_active_override,
    );
    let (execution, cycle) = progress_executions_from_state(
        args.root_run_id,
        &args.actionable.run_id,
        args.current_step.step,
    );
    let top_level_timing_bounds = top_level_step_id
        .as_deref()
        .and_then(|step_id| {
            args.root_state["steps"].get(step_id).map(|step| {
                resolved_step_timing_bounds(
                    step_id,
                    step["started_at_ms"].as_u64(),
                    step["finished_at_ms"].as_u64(),
                    step_timing_overrides,
                )
            })
        })
        .unwrap_or(StepTimingBounds {
            started_at_ms: args.current_step.step["started_at_ms"].as_u64(),
            finished_at_ms: args.current_step.step["finished_at_ms"].as_u64(),
        });
    let active_elapsed_bounds = active_elapsed_timing_bounds(
        args.current_step.step["status"].as_str(),
        args.current_step.step["started_at_ms"].as_u64(),
        args.current_step.step["finished_at_ms"].as_u64(),
        top_level_timing_bounds.started_at_ms,
        top_level_timing_bounds.finished_at_ms,
        top_level_step_id
            .as_deref()
            .is_some_and(|step_id| step_id != args.current_step.step_id),
    );

    ProgressState {
        root_run_id: args.root_run_id.to_string(),
        run_status: args.root_state["status"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        actionable_run_id: args.actionable.run_id.clone(),
        top_level_step_id: top_level_step_id.clone(),
        current_step_path: Some(crate::actionable::live_step_path(
            args.root_run_id,
            &args.actionable.run_id,
            args.current_step.step_id,
        )),
        current_step_status: current_step_status.map(str::to_string),
        execution,
        cycle,
        tool_name,
        started_at_ms: active_elapsed_bounds.started_at_ms,
        finished_at_ms: active_elapsed_bounds.finished_at_ms,
        error_code: show_result_snapshot
            .then(|| {
                args.current_step.step["last_result"]["error"]["code"]
                    .as_str()
                    .map(str::to_string)
            })
            .flatten(),
        exit_code: show_result_snapshot
            .then(|| {
                args.current_step.step["last_result"]["exit_code"]
                    .as_i64()
                    .map(|value| value as i32)
            })
            .flatten(),
        checklist: build_progress_checklist_from_state_with_timing_overrides(
            args.root_state,
            args.root_step_order,
            checklist_override.as_ref(),
            step_timing_overrides,
        ),
    }
}

pub(crate) fn build_progress_state_with_timing_overrides(
    root_run_id: &str,
    root_detail: &InspectionRunDetail,
    actionable: &ActionableRun,
    selected_step: Option<&InspectionStep>,
    tool_name: Option<String>,
    live_active_override: bool,
    step_timing_overrides: &StepTimingOverrides,
) -> ProgressState {
    let current_step_status = selected_step.map(|step| step.status.as_str());
    let show_result_snapshot = should_show_step_result_snapshot(current_step_status);

    let top_level_step_id = selected_step.and_then(|step| {
        progress_top_level_step_id(root_run_id, &actionable.run_id, &step.step_id)
    });
    let checklist_override = progress_checklist_override(
        root_run_id,
        actionable,
        top_level_step_id.as_deref(),
        live_active_override,
    );
    let (execution, cycle) = progress_executions(root_run_id, &actionable.run_id, selected_step);
    let top_level_timing_bounds = top_level_step_id
        .as_deref()
        .and_then(|step_id| {
            root_detail
                .steps
                .iter()
                .find(|step| step.step_id == step_id)
                .map(|step| {
                    resolved_step_timing_bounds(
                        step.step_id.as_str(),
                        step.started_at_ms,
                        step.finished_at_ms,
                        step_timing_overrides,
                    )
                })
        })
        .unwrap_or(StepTimingBounds {
            started_at_ms: selected_step.and_then(|step| step.started_at_ms),
            finished_at_ms: selected_step.and_then(|step| step.finished_at_ms),
        });
    let active_elapsed_bounds = active_elapsed_timing_bounds(
        selected_step.map(|step| step.status.as_str()),
        selected_step.and_then(|step| step.started_at_ms),
        selected_step.and_then(|step| step.finished_at_ms),
        top_level_timing_bounds.started_at_ms,
        top_level_timing_bounds.finished_at_ms,
        top_level_step_id
            .as_deref()
            .is_some_and(|top_level_step_id| {
                selected_step
                    .map(|step| step.step_id.as_str() != top_level_step_id)
                    .unwrap_or(false)
            }),
    );

    ProgressState {
        root_run_id: root_run_id.to_string(),
        run_status: root_detail.summary.status.clone(),
        actionable_run_id: actionable.run_id.clone(),
        top_level_step_id: top_level_step_id.clone(),
        current_step_path: selected_step.map(|step| {
            crate::actionable::live_step_path(root_run_id, &actionable.run_id, &step.step_id)
        }),
        current_step_status: current_step_status.map(str::to_string),
        execution,
        cycle,
        tool_name,
        started_at_ms: active_elapsed_bounds.started_at_ms,
        finished_at_ms: active_elapsed_bounds.finished_at_ms,
        error_code: show_result_snapshot
            .then(|| selected_step.and_then(|step| step.output_summary.error_code.clone()))
            .flatten(),
        exit_code: show_result_snapshot
            .then(|| selected_step.and_then(|step| step.output_summary.exit_code))
            .flatten(),
        checklist: build_progress_checklist_with_timing_overrides(
            root_detail,
            checklist_override.as_ref(),
            step_timing_overrides,
        ),
    }
}

#[cfg(test)]
pub(crate) fn build_progress_state(
    root_run_id: &str,
    root_detail: &InspectionRunDetail,
    actionable: &ActionableRun,
    selected_step: Option<&InspectionStep>,
    tool_name: Option<String>,
    live_active_override: bool,
) -> ProgressState {
    build_progress_state_with_timing_overrides(
        root_run_id,
        root_detail,
        actionable,
        selected_step,
        tool_name,
        live_active_override,
        &BTreeMap::new(),
    )
}

fn active_elapsed_timing_bounds(
    current_step_status: Option<&str>,
    current_step_started_at_ms: Option<u64>,
    current_step_finished_at_ms: Option<u64>,
    top_level_started_at_ms: Option<u64>,
    top_level_finished_at_ms: Option<u64>,
    current_step_is_nested_under_top_level: bool,
) -> StepTimingBounds {
    match current_step_status {
        Some("running" | "dispatching" | "retry_wait") => StepTimingBounds {
            started_at_ms: current_step_started_at_ms.or(top_level_started_at_ms),
            finished_at_ms: None,
        },
        _ if current_step_is_nested_under_top_level => merge_step_timing_bounds(
            StepTimingBounds {
                started_at_ms: top_level_started_at_ms,
                finished_at_ms: top_level_finished_at_ms,
            },
            StepTimingBounds {
                started_at_ms: current_step_started_at_ms,
                finished_at_ms: current_step_finished_at_ms,
            },
        ),
        _ => StepTimingBounds {
            started_at_ms: current_step_started_at_ms.or(top_level_started_at_ms),
            finished_at_ms: current_step_finished_at_ms.or(top_level_finished_at_ms),
        },
    }
}

#[cfg(test)]
pub(crate) fn build_progress_checklist(
    root_detail: &InspectionRunDetail,
    step_override: Option<&ChecklistOverride>,
) -> Vec<ProgressChecklistItem> {
    build_progress_checklist_with_timing_overrides(root_detail, step_override, &BTreeMap::new())
}

pub(crate) fn build_progress_checklist_with_timing_overrides(
    root_detail: &InspectionRunDetail,
    step_override: Option<&ChecklistOverride>,
    step_timing_overrides: &StepTimingOverrides,
) -> Vec<ProgressChecklistItem> {
    root_detail
        .steps
        .iter()
        .filter_map(|step| {
            let state = checklist_state_for_step(step, step_override);
            if matches!(state, ChecklistState::Skipped) {
                return None;
            }
            let timing = resolved_step_timing_bounds(
                step.step_id.as_str(),
                step.started_at_ms,
                step.finished_at_ms,
                step_timing_overrides,
            );
            Some(ProgressChecklistItem {
                step_id: step.step_id.clone(),
                state,
                duration_seconds: checklist_duration_seconds(
                    timing.started_at_ms,
                    timing.finished_at_ms,
                    state,
                ),
            })
        })
        .collect()
}

pub(crate) fn build_progress_checklist_from_state_with_timing_overrides(
    root_state: &Value,
    step_order: &[String],
    step_override: Option<&ChecklistOverride>,
    step_timing_overrides: &StepTimingOverrides,
) -> Vec<ProgressChecklistItem> {
    let Some(steps) = root_state["steps"].as_object() else {
        return Vec::new();
    };
    let ordered_step_ids = if step_order.is_empty() {
        steps.keys().cloned().collect::<Vec<_>>()
    } else {
        step_order.to_vec()
    };
    ordered_step_ids
        .into_iter()
        .filter_map(|step_id| {
            let step = steps.get(&step_id)?;
            let state = checklist_state_for_step_state(&step_id, step, step_override);
            if matches!(state, ChecklistState::Skipped) {
                return None;
            }
            let timing = resolved_step_timing_bounds(
                &step_id,
                step["started_at_ms"].as_u64(),
                step["finished_at_ms"].as_u64(),
                step_timing_overrides,
            );
            Some(ProgressChecklistItem {
                step_id: step_id.clone(),
                state,
                duration_seconds: checklist_duration_seconds(
                    timing.started_at_ms,
                    timing.finished_at_ms,
                    state,
                ),
            })
        })
        .collect()
}

pub(crate) fn resolved_step_timing_bounds(
    step_id: &str,
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    step_timing_overrides: &StepTimingOverrides,
) -> StepTimingBounds {
    step_timing_overrides
        .get(step_id)
        .copied()
        .filter(|bounds| bounds.started_at_ms.is_some() || bounds.finished_at_ms.is_some())
        .unwrap_or(StepTimingBounds {
            started_at_ms,
            finished_at_ms,
        })
}

pub(crate) fn merge_step_timing_bounds(
    left: StepTimingBounds,
    right: StepTimingBounds,
) -> StepTimingBounds {
    StepTimingBounds {
        started_at_ms: match (left.started_at_ms, right.started_at_ms) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        },
        finished_at_ms: match (left.finished_at_ms, right.finished_at_ms) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        },
    }
}

pub(crate) fn persisted_run_execution_timestamps(
    home_dir: &std::path::Path,
    run_id: &str,
) -> Result<StepTimingBounds> {
    use std::fs;
    let executions_root = transist::TransistContext::with_home(home_dir, home_dir)
        .run_root_or_default(run_id)?
        .join("step_executions");
    if !executions_root.exists() {
        return Ok(StepTimingBounds::default());
    }

    let mut bounds = StepTimingBounds::default();
    for step_dir in fs::read_dir(&executions_root)
        .with_context(|| format!("failed reading directory: {}", executions_root.display()))?
    {
        let step_dir = step_dir.with_context(|| {
            format!("failed iterating directory: {}", executions_root.display())
        })?;
        if !step_dir.path().is_dir() {
            continue;
        }
        for execution_entry in fs::read_dir(step_dir.path())
            .with_context(|| format!("failed reading directory: {}", step_dir.path().display()))?
        {
            let execution_entry = execution_entry.with_context(|| {
                format!("failed iterating directory: {}", step_dir.path().display())
            })?;
            if !execution_entry.path().is_file() {
                continue;
            }
            let raw = fs::read_to_string(execution_entry.path()).with_context(|| {
                format!(
                    "failed reading step execution: {}",
                    execution_entry.path().display()
                )
            })?;
            let execution: Value = serde_json::from_str(&raw).with_context(|| {
                format!(
                    "invalid step execution json: {}",
                    execution_entry.path().display()
                )
            })?;
            bounds = merge_step_timing_bounds(
                bounds,
                StepTimingBounds {
                    started_at_ms: execution["started_at_ms"].as_u64(),
                    finished_at_ms: execution["finished_at_ms"].as_u64(),
                },
            );
        }
    }

    Ok(bounds)
}

pub(crate) fn run_detail_step_timestamps(run_detail: &InspectionRunDetail) -> StepTimingBounds {
    let mut started_at_ms: Option<u64> = None;
    let mut finished_at_ms: Option<u64> = None;
    for step in &run_detail.steps {
        started_at_ms = match (started_at_ms, step.started_at_ms) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
        finished_at_ms = match (finished_at_ms, step.finished_at_ms) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
    }
    StepTimingBounds {
        started_at_ms,
        finished_at_ms,
    }
}

pub(crate) fn run_state_step_timestamps(run_state: &Value) -> StepTimingBounds {
    let Some(steps) = run_state["steps"].as_object() else {
        return StepTimingBounds::default();
    };
    let mut started_at_ms: Option<u64> = None;
    let mut finished_at_ms: Option<u64> = None;
    for step in steps.values() {
        started_at_ms = match (started_at_ms, step["started_at_ms"].as_u64()) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
        finished_at_ms = match (finished_at_ms, step["finished_at_ms"].as_u64()) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
    }
    StepTimingBounds {
        started_at_ms,
        finished_at_ms,
    }
}

fn checklist_duration_seconds(
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    state: ChecklistState,
) -> Option<u64> {
    if !matches!(state, ChecklistState::Success) {
        return None;
    }
    let started_at = started_at_ms?;
    let finished_at = finished_at_ms?;
    let duration_ms = finished_at.saturating_sub(started_at);
    Some(if duration_ms == 0 {
        0
    } else {
        (duration_ms / 1000).max(1)
    })
}

pub(crate) fn checklist_state_for_step(
    step: &InspectionStep,
    step_override: Option<&ChecklistOverride>,
) -> ChecklistState {
    if let Some(override_state) =
        step_override.filter(|override_state| override_state.step_id == step.step_id)
    {
        return override_state.state;
    }
    match step.status.as_str() {
        "success" => ChecklistState::Success,
        "running" | "dispatching" | "retry_wait" => ChecklistState::Active,
        "pending" | "runnable" => ChecklistState::Pending,
        "blocked" => ChecklistState::Blocked,
        "failed" => ChecklistState::Failed,
        "canceled" => ChecklistState::Canceled,
        "skipped" => ChecklistState::Skipped,
        _ => ChecklistState::Pending,
    }
}

pub(crate) fn checklist_state_for_step_state(
    step_id: &str,
    step: &Value,
    step_override: Option<&ChecklistOverride>,
) -> ChecklistState {
    if let Some(override_state) =
        step_override.filter(|override_state| override_state.step_id == step_id)
    {
        return override_state.state;
    }
    match step["status"].as_str().unwrap_or_default() {
        "success" | "succeeded" => ChecklistState::Success,
        "running" | "dispatching" | "retry_wait" => ChecklistState::Active,
        "pending" | "runnable" => ChecklistState::Pending,
        "blocked" => ChecklistState::Blocked,
        "failed" => ChecklistState::Failed,
        "canceled" => ChecklistState::Canceled,
        "skipped" => ChecklistState::Skipped,
        _ => ChecklistState::Pending,
    }
}

pub(crate) fn progress_checklist_override(
    root_run_id: &str,
    actionable: &ActionableRun,
    top_level_step_id: Option<&str>,
    live_active_override: bool,
) -> Option<ChecklistOverride> {
    let step_id = top_level_step_id?;
    if actionable.run_id == root_run_id {
        return None;
    }
    let state = match actionable.detail.summary.status.as_str() {
        "running" | "waiting" if live_active_override => ChecklistState::Active,
        "blocked" => ChecklistState::Blocked,
        "failed" => ChecklistState::Failed,
        "canceled" => ChecklistState::Canceled,
        _ => return None,
    };
    Some(ChecklistOverride {
        step_id: step_id.to_string(),
        state,
    })
}

pub(crate) fn progress_checklist_override_from_state(
    root_run_id: &str,
    actionable: &ActionableRunState,
    top_level_step_id: Option<&str>,
    live_active_override: bool,
) -> Option<ChecklistOverride> {
    let step_id = top_level_step_id?;
    if actionable.run_id == root_run_id {
        return None;
    }
    let state = match actionable.state["status"].as_str().unwrap_or_default() {
        "running" | "waiting" if live_active_override => ChecklistState::Active,
        "blocked" => ChecklistState::Blocked,
        "failed" => ChecklistState::Failed,
        "canceled" => ChecklistState::Canceled,
        _ => return None,
    };
    Some(ChecklistOverride {
        step_id: step_id.to_string(),
        state,
    })
}

pub(crate) fn progress_top_level_step_id(
    root_run_id: &str,
    actionable_run_id: &str,
    step_id: &str,
) -> Option<String> {
    if actionable_run_id == root_run_id {
        return Some(step_id.to_string());
    }
    actionable_run_id
        .strip_prefix(root_run_id)
        .and_then(|suffix| suffix.strip_prefix("__"))
        .and_then(|suffix| suffix.split("__").next())
        .map(str::to_string)
}

pub(crate) fn progress_executions(
    root_run_id: &str,
    actionable_run_id: &str,
    step: Option<&InspectionStep>,
) -> (Option<u32>, Option<u32>) {
    let execution = step.and_then(|step| step.execution);
    let cycle = (actionable_run_id != root_run_id)
        .then_some(execution)
        .flatten()
        .filter(|execution| *execution > 1);
    let execution = (actionable_run_id == root_run_id)
        .then_some(execution)
        .flatten();
    (execution, cycle)
}

pub(crate) fn progress_executions_from_state(
    root_run_id: &str,
    actionable_run_id: &str,
    step: &Value,
) -> (Option<u32>, Option<u32>) {
    let execution = step["executions"].as_u64().map(|value| value as u32);
    let cycle = (actionable_run_id != root_run_id)
        .then_some(execution)
        .flatten()
        .filter(|execution| *execution > 1);
    let execution = (actionable_run_id == root_run_id)
        .then_some(execution)
        .flatten();
    (execution, cycle)
}

pub(crate) fn select_live_progress_step_state(run_state: &Value) -> Option<(String, &Value)> {
    run_state["steps"].as_object().and_then(|steps| {
        steps
            .iter()
            .filter(|(_, step)| {
                crate::actionable::is_live_progress_step(
                    step["status"].as_str().unwrap_or_default(),
                )
            })
            .max_by_key(|(_, step)| {
                (
                    step["started_at_ms"].as_u64().unwrap_or_default(),
                    step["executions"].as_u64().unwrap_or_default(),
                )
            })
            .map(|(step_id, step)| (step_id.clone(), step))
    })
}

pub(crate) fn select_bootstrap_progress_step_state<'a>(
    run_state: &'a Value,
    step_order: &[String],
) -> Option<(String, &'a Value)> {
    select_live_progress_step_state(run_state).or_else(|| {
        let steps = run_state["steps"].as_object()?;
        let ordered_step_ids = if step_order.is_empty() {
            steps.keys().cloned().collect::<Vec<_>>()
        } else {
            step_order.to_vec()
        };
        ordered_step_ids.into_iter().find_map(|step_id| {
            let step = steps.get(&step_id)?;
            matches!(
                step["status"].as_str().unwrap_or_default(),
                "pending" | "runnable" | "blocked" | "failed" | "canceled"
            )
            .then_some((step_id, step))
        })
    })
}

pub(crate) fn select_terminal_progress_step(actionable: &ActionableRun) -> Option<&InspectionStep> {
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
        .or_else(|| {
            crate::actionable::latest_step(&actionable.detail, &["failed", "blocked", "canceled"])
        })
}

pub(crate) fn spinner_frame(spinner_tick: u64) -> &'static str {
    match spinner_tick % 6 {
        0 => "[=  ]",
        1 => "[== ]",
        2 => "[===]",
        3 => "[ ==]",
        4 => "[  =]",
        _ => "[ = ]",
    }
}

pub(crate) fn progress_marker_styles_enabled() -> bool {
    io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

pub(crate) fn style_checklist_marker(
    marker: &str,
    state: ChecklistState,
    is_running: bool,
    dimmed: bool,
) -> String {
    if is_running || !progress_marker_styles_enabled() {
        return marker.to_string();
    }
    match state {
        ChecklistState::Success => format!(
            "[{}]",
            if dimmed {
                Style::new().for_stderr().green().dim().apply_to("✔")
            } else {
                Style::new().for_stderr().green().apply_to("✔")
            }
        ),
        ChecklistState::Blocked => {
            format!(
                "[{}]",
                if dimmed {
                    Style::new().for_stderr().yellow().dim().apply_to("-")
                } else {
                    Style::new().for_stderr().yellow().apply_to("-")
                }
            )
        }
        ChecklistState::Failed => {
            format!(
                "[{}]",
                if dimmed {
                    Style::new().for_stderr().red().bold().dim().apply_to("!")
                } else {
                    Style::new().for_stderr().red().bold().apply_to("!")
                }
            )
        }
        _ if dimmed => Style::new().for_stderr().dim().apply_to(marker).to_string(),
        _ => marker.to_string(),
    }
}

pub(crate) fn style_non_current_progress_line(line: &str) -> String {
    if !progress_marker_styles_enabled() {
        return line.to_string();
    }
    Style::new().for_stderr().dim().apply_to(line).to_string()
}

pub(crate) fn should_dim_line_for_running_focus(
    running_focus_step_id: Option<&str>,
    line_step_id: Option<&str>,
) -> bool {
    matches!(
        (running_focus_step_id, line_step_id),
        (Some(current_step_id), Some(line_step_id)) if current_step_id != line_step_id
    )
}
