// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use transist::{InspectionGate, InspectionRunDetail, InspectionStep, TransistInspectionContext};

use crate::display::output::{append_workflow_file_path_lines, workflow_file_paths};
use crate::display::progress::{
    StepTimingBounds, StepTimingOverrides, merge_step_timing_bounds,
    persisted_run_execution_timestamps, run_detail_step_timestamps,
};
use crate::guidance::collect_manual_gate_guidance_lines;

#[derive(Debug, Clone)]
pub(crate) struct ActionableRun {
    pub(crate) root_run_id: String,
    pub(crate) run_id: String,
    pub(crate) detail: InspectionRunDetail,
}

pub(crate) fn resolve_actionable_run(
    inspection: &TransistInspectionContext,
    root_run_id: &str,
) -> Result<Option<ActionableRun>> {
    resolve_actionable_run_with_root(inspection, root_run_id).map(|outcome| outcome.map(|o| o.1))
}

/// Walks from `root_run_id` down to the deepest actionable subflow descendant,
/// returning both the root's `InspectionRunDetail` and the resolved
/// `ActionableRun`. Returning the root detail lets callers (notably the TUI
/// run-detail refresh loop) avoid a redundant `inspection.get_run(root)` call,
/// which is the dominant FS cost while a run is active.
pub(crate) fn resolve_actionable_run_with_root(
    inspection: &TransistInspectionContext,
    root_run_id: &str,
) -> Result<Option<(InspectionRunDetail, ActionableRun)>> {
    let mut current_run_id = root_run_id.to_string();
    let mut visited = BTreeSet::new();
    let mut root_detail: Option<InspectionRunDetail> = None;

    loop {
        if !visited.insert(current_run_id.clone()) {
            return Ok(None);
        }
        let Some(detail) = inspection.get_run(&current_run_id)? else {
            return Ok(None);
        };
        if root_detail.is_none() {
            root_detail = Some(detail.clone());
        }
        let next = next_actionable_subflow_run(inspection, &current_run_id, &detail)?;
        if let Some(next_run_id) = next {
            current_run_id = next_run_id;
            continue;
        }
        let root = root_detail.expect("root detail captured on first iteration");
        return Ok(Some((
            root,
            ActionableRun {
                root_run_id: root_run_id.to_string(),
                run_id: current_run_id,
                detail,
            },
        )));
    }
}

pub(crate) fn next_actionable_subflow_run(
    inspection: &TransistInspectionContext,
    _run_id: &str,
    detail: &InspectionRunDetail,
) -> Result<Option<String>> {
    if detail
        .blocked_gates
        .iter()
        .any(|gate| gate.status == "blocked")
    {
        return Ok(None);
    }

    let mut preferred = None;
    let mut fallback = None;

    for step in detail
        .steps
        .iter()
        .filter(|step| step.step_kind == "subflow")
    {
        let Some(child_run_id) = child_run_id_for_inspection_step(step) else {
            continue;
        };
        let Some(child) = inspection.get_run(&child_run_id)? else {
            continue;
        };
        if child.summary.status == "success" {
            continue;
        }
        if step.is_current_active || child.summary.status == "blocked" {
            preferred = Some(child_run_id);
            break;
        }
        let sort_key = step_sort_key(step);
        match &fallback {
            Some((best_key, _)) if *best_key >= sort_key => {}
            _ => fallback = Some((sort_key, child_run_id)),
        }
    }

    Ok(preferred.or_else(|| fallback.map(|(_, run_id)| run_id)))
}

pub(crate) fn child_run_id_for_inspection_step(step: &InspectionStep) -> Option<String> {
    step.output_summary
        .child_run_id
        .as_deref()
        .map(str::trim)
        .filter(|child_run_id| !child_run_id.is_empty())
        .map(str::to_string)
}

pub(crate) fn latest_step<'a>(
    detail: &'a InspectionRunDetail,
    statuses: &[&str],
) -> Option<&'a InspectionStep> {
    detail
        .steps
        .iter()
        .filter(|step| statuses.iter().any(|status| *status == step.status))
        .max_by_key(|step| step_sort_key(step))
}

pub(crate) fn step_sort_key(step: &InspectionStep) -> u64 {
    step.finished_at_ms
        .or(step.started_at_ms)
        .unwrap_or_default()
}

pub(crate) fn live_step_path(root_run_id: &str, actionable_run_id: &str, step_id: &str) -> String {
    let mut parts = Vec::new();
    if let Some(suffix) = actionable_run_id
        .strip_prefix(root_run_id)
        .and_then(|suffix| suffix.strip_prefix("__"))
    {
        parts.extend(
            suffix
                .split("__")
                .filter(|segment| !segment.trim().is_empty())
                .map(str::to_string),
        );
    }
    parts.push(step_id.to_string());
    parts.join(" -> ")
}

pub(crate) fn is_live_progress_step(status: &str) -> bool {
    matches!(status, "running" | "dispatching" | "retry_wait")
}

pub(crate) fn subflow_step_timing_overrides_for_detail(
    home_dir: &std::path::Path,
    inspection: &TransistInspectionContext,
    _root_run_id: &str,
    root_detail: &InspectionRunDetail,
) -> Result<StepTimingOverrides> {
    let mut overrides = BTreeMap::new();
    for step in root_detail
        .steps
        .iter()
        .filter(|step| step.step_kind == "subflow")
    {
        let Some(child_run_id) = child_run_id_for_inspection_step(step) else {
            continue;
        };
        let Some(child_detail) = inspection.get_run(&child_run_id)? else {
            continue;
        };
        let persisted_execution_timing =
            persisted_run_execution_timestamps(home_dir, &child_run_id)?;
        overrides.insert(
            step.step_id.clone(),
            merge_step_timing_bounds(
                merge_step_timing_bounds(
                    StepTimingBounds {
                        started_at_ms: step.started_at_ms,
                        finished_at_ms: step.finished_at_ms,
                    },
                    StepTimingBounds {
                        started_at_ms: child_detail.summary.started_at_ms,
                        finished_at_ms: child_detail.summary.finished_at_ms,
                    },
                ),
                merge_step_timing_bounds(
                    run_detail_step_timestamps(&child_detail),
                    persisted_execution_timing,
                ),
            ),
        );
    }
    Ok(overrides)
}

pub(crate) fn collect_manual_gate_and_workflow_file_lines(
    context: &transist::TransistContext,
    root_run_id: &str,
    actionable: &ActionableRun,
    gate: &InspectionGate,
) -> Result<Vec<String>> {
    let mut lines = collect_manual_gate_guidance_lines(context, root_run_id, actionable, gate)?;
    let workflow_paths = blocked_gate_workflow_paths(root_run_id, actionable, gate);
    if !workflow_paths.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        append_workflow_file_path_lines(&mut lines, workflow_paths);
    }
    Ok(lines)
}

fn blocked_gate_workflow_paths(
    root_run_id: &str,
    actionable: &ActionableRun,
    gate: &InspectionGate,
) -> Vec<String> {
    use crate::display::progress::progress_top_level_step_id;
    let Some(anchor_step_id) =
        progress_top_level_step_id(root_run_id, &actionable.run_id, &gate.step_id)
    else {
        return Vec::new();
    };

    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for step in &actionable.detail.steps {
        let step_anchor =
            progress_top_level_step_id(root_run_id, &actionable.run_id, &step.step_id);
        if step_anchor.as_deref() != Some(anchor_step_id.as_str()) {
            continue;
        }
        for path in workflow_file_paths(&step.output_summary.workflow_files) {
            if !path.trim().is_empty() && seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }
    paths
}
