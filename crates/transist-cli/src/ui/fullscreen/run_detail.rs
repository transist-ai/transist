// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect, Size};
use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

use transist::InspectionGateDecision;
use transist::{BlobFamily, KernelValue, TransistContext, TransistInspectionContext};

use crate::actionable::{latest_step, resolve_actionable_run_with_root};
use crate::commands::dispatch::reentry_runner_action_for_run;
use crate::display::output::{
    PersistedStepOutputCacheOwner, blob_refs_to_paths, collect_persisted_step_output_blocks,
};
use crate::display::spinner::{
    LiveProgress, inspect_live_progress, inspect_terminal_progress, terminal_live_progress,
};
use crate::display::summary::{
    SuggestedAction, SuggestedActionExecution, build_blocked_summary_content,
    build_failed_summary_content, format_failed_preflight_check_line, format_runs_show_command,
    gate_suggested_actions, resolve_selected_action,
};
use crate::execution::{
    RecipeCacheOwner, execute_suggested_action, inspect_bootstrap_progress,
    run_cleanup_and_build_result_page,
};

use super::format::clamp_scroll_offset;
use super::{ActionPickerState, FullscreenPrompt, selected_step_index};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunDetailSection {
    Steps,
    Details,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PostGateDecisionFocus {
    Apply,
    KeepPending,
    Clear,
}

#[derive(Debug, Clone)]
pub(super) struct RunDetailState {
    pub(super) focused_section: RunDetailSection,
    pub(super) selected_step: usize,
    pub(super) details_scroll: usize,
    /// Row index (0-based, into the *wrapped* details lines) the operator
    /// has highlighted with the row cursor when Details has focus. The `o`
    /// key resolves this to the corresponding `DetailLine.openable` path.
    /// Reset to 0 whenever the selected step changes.
    pub(super) details_cursor: usize,
    pub(super) selected_action: usize,
    /// While `false`, `selected_step` auto-follows the workflow's current
    /// active step on every refresh — so as the runner advances, the
    /// Details pane always shows the step that is running (or about to
    /// run). Set to `true` the moment the operator focuses the Steps
    /// section (via Tab / Shift+Tab) — once they're explicitly inspecting
    /// steps we stop auto-pivoting under their cursor. Reset to `false`
    /// when the viewer pivots to a different run id (e.g. after a
    /// gate-decision spawned a fresh runner) so auto-follow re-engages
    /// for the new run.
    pub(super) step_pinned_by_user: bool,
    pub(super) pane_cache: Option<RunDetailPaneCache>,
}

#[derive(Debug, Clone)]
pub(super) struct RunDetailPaneCache {
    fingerprint: u64,
    display: RunDetailDisplayData,
}

pub(super) fn run_detail_move_focus(
    focus: RunDetailSection,
    forward: bool,
    has_actions: bool,
) -> RunDetailSection {
    if !has_actions {
        return match (focus, forward) {
            (RunDetailSection::Steps, true) => RunDetailSection::Details,
            (RunDetailSection::Details, true) => RunDetailSection::Steps,
            (RunDetailSection::Steps, false) => RunDetailSection::Details,
            (RunDetailSection::Details, false) => RunDetailSection::Steps,
            (RunDetailSection::Actions, _) => RunDetailSection::Details,
        };
    }
    match (focus, forward) {
        (RunDetailSection::Steps, true) => RunDetailSection::Details,
        (RunDetailSection::Details, true) => RunDetailSection::Actions,
        (RunDetailSection::Actions, true) => RunDetailSection::Steps,
        (RunDetailSection::Steps, false) => RunDetailSection::Actions,
        (RunDetailSection::Details, false) => RunDetailSection::Steps,
        (RunDetailSection::Actions, false) => RunDetailSection::Details,
    }
}

pub(super) fn run_detail_actions_focused(focus: RunDetailSection) -> bool {
    matches!(focus, RunDetailSection::Actions)
}

pub(super) fn gate_decision_action_requests_details_focus(
    execution: &SuggestedActionExecution,
) -> bool {
    matches!(
        execution,
        SuggestedActionExecution::Approve { .. }
            | SuggestedActionExecution::Reject { .. }
            | SuggestedActionExecution::RequestChanges { .. }
    )
}

pub(super) fn post_gate_decision_focus(status: &str) -> PostGateDecisionFocus {
    match status {
        "running" | "waiting" => PostGateDecisionFocus::Apply,
        "created" | "planned" => PostGateDecisionFocus::KeepPending,
        _ => PostGateDecisionFocus::Clear,
    }
}

pub(super) fn run_detail_section_metrics(size: Size) -> (usize, usize, usize, usize, usize, usize) {
    let area = Rect::from((Position::ORIGIN, size));
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(layout[1]);
    (
        body[0].width.saturating_sub(2) as usize,
        body[0].height.saturating_sub(2) as usize,
        body[1].width.saturating_sub(2) as usize,
        body[1].height.saturating_sub(2) as usize,
        body[1].width.saturating_sub(2) as usize,
        body[1].height.saturating_sub(2) as usize,
    )
}

/// Mirror the height-budget logic in `draw_run_detail_pane` so keyboard scroll
/// math matches what is actually visible on screen. When the Actions/Commands
/// picker is present, `draw_run_detail_pane` carves up to `commands_height`
/// rows off the bottom of the right pane for the picker; without subtracting
/// that here, `apply_scroll_delta` clamps to the full pane height and the
/// user cannot reach the last `commands_height` rows of pre-wrapped Details
/// content. `picker_item_count == 0` reproduces the no-picker layout.
pub(super) fn details_visible_rows_with_picker(
    full_details_visible: usize,
    picker_item_count: usize,
) -> usize {
    if picker_item_count == 0 {
        return full_details_visible;
    }
    // Same calculation as `draw_run_detail_pane` in `components.rs`:
    //   commands_height = (items + 2) clamped to [3, 12]
    let commands_height = (picker_item_count.saturating_add(2)).clamp(3, 12);
    full_details_visible.saturating_sub(commands_height)
}

pub(super) struct RunDetailRefresh {
    pub(super) actions: Vec<SuggestedAction>,
    pub(super) selected_action: usize,
    pub(super) running: bool,
    pub(super) effective_run_status: String,
    pub(super) terminal: bool,
    /// True when the load returned `current_progress = None` while the
    /// root run is still in a non-terminal status. Signals to the outer
    /// poll loop that the screen is in a step-boundary transition (e.g.,
    /// right after a gate decision in a loop iteration) and should
    /// schedule another refresh soon instead of dropping the poll
    /// timeout to the long-idle cadence — otherwise the panes blank for
    /// the duration of the long idle and only redraw on key input.
    pub(super) transient_no_progress: bool,
    /// True when the parent run has at least one contained child whose
    /// own status is non-terminal (e.g., `pending`, `running`, `waiting`,
    /// `blocked`). The mutation poll loop uses this to keep the hot-poll
    /// cadence alive while a just-spawned child run is making progress
    /// that the parent's persisted status doesn't yet reflect — without
    /// this, a parent that stays `blocked` after Approve drops the poll
    /// loop back to the idle cadence (1-3s) and the run-detail screen
    /// appears frozen until the next slow tick.
    pub(super) has_active_child: bool,
}

fn implies_active_tool_work(current_progress: Option<&LiveProgress>) -> bool {
    matches!(
        current_progress.and_then(|progress| progress.state.current_step_status.as_deref()),
        Some("running" | "dispatching" | "retry_wait")
    )
}

#[derive(Debug, Clone)]
pub(super) struct RunDetailLoadedData {
    pub(super) home_dir: PathBuf,
    pub(super) detail: transist::InspectionRunDetail,
    /// Best-effort loaded inspection details for selected/actionable contained
    /// child runs keyed by child run id.
    pub(super) contained_run_details:
        std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    pub(super) actionable: crate::actionable::ActionableRun,
    pub(super) status_lines: Vec<String>,
    pub(super) current_progress: Option<LiveProgress>,
    pub(super) effective_run_status: String,
    pub(super) actions: Vec<SuggestedAction>,
}

pub(super) fn load_run_detail_data(
    context: &transist::TransistContext,
    run_id: &str,
    selected_step_id: Option<String>,
    recipe_cache: &RecipeCacheOwner,
    output_cache: &PersistedStepOutputCacheOwner,
) -> Result<RunDetailLoadedData> {
    let inspection = context.inspection();
    let Some((detail, actionable)) = resolve_actionable_run_with_root(&inspection, run_id)? else {
        anyhow::bail!("run '{}' was not found or not actionable", run_id);
    };
    let attached_step_output_blocks =
        collect_persisted_step_output_blocks(context, &inspection, run_id, output_cache)?;
    let root_step_order = crate::execution::load_recipe_step_order(context, run_id, recipe_cache)?
        .unwrap_or_default();
    let current_progress = if let Some(progress) = inspect_live_progress(
        context,
        run_id,
        &root_step_order,
        attached_step_output_blocks.clone(),
        recipe_cache,
    )? {
        Some(progress)
    } else if let Some(progress_state) = inspect_terminal_progress(context, &inspection, run_id)? {
        Some(terminal_live_progress(
            progress_state,
            None,
            attached_step_output_blocks,
        ))
    } else {
        inspect_bootstrap_progress(context, run_id, attached_step_output_blocks, recipe_cache)?
    };
    let effective_run_status =
        resolve_effective_run_status(&detail, &actionable, current_progress.as_ref());
    let status_lines = build_run_detail_status_lines(
        context,
        &detail,
        &actionable,
        run_id,
        &effective_run_status,
    )?;
    let actions =
        build_run_detail_actions(context, &detail, &actionable, run_id, &effective_run_status)?;
    // Eagerly fetch a best-effort `InspectionRunDetail` for every child run
    // listed in `detail.contained_runs`. The Details pane renders subflow
    // step bodies by reaching into these maps so a parent step like
    // `plan_loop` can show the artifacts and workflow_files produced inside
    // its child run (e.g. openspec proposal/tasks/design files generated by
    // a child `generate_plan` step) plus any blocked manual gate's request
    // summary. We swallow per-child errors to keep the parent UI usable when
    // a single child run is mid-write or otherwise transiently unreadable.
    let child_run_ids =
        prioritized_child_run_ids(&detail, &actionable, selected_step_id.as_deref());
    let contained_run_details = collect_contained_run_details(&inspection, child_run_ids)?;
    Ok(RunDetailLoadedData {
        home_dir: context.home_dir().to_path_buf(),
        detail,
        contained_run_details,
        actionable,
        status_lines,
        current_progress,
        effective_run_status,
        actions,
    })
}

fn resolve_effective_run_status(
    detail: &transist::InspectionRunDetail,
    actionable: &crate::actionable::ActionableRun,
    current_progress: Option<&LiveProgress>,
) -> String {
    let root_status = detail.summary.status.as_str();

    // When the root has settled into a terminal state, the root status is the
    // *only* truthful answer regardless of what an intermediate actionable
    // descendant might still report. Otherwise the action picker, status
    // banner, and routing decisions all surface stale, non-actionable
    // alternatives (e.g. a `planned` child whose dispatch the kernel will
    // immediately reject because the root is `canceled`).
    if matches!(root_status, "canceled" | "failed" | "success") {
        return root_status.to_string();
    }

    let progress_status = current_progress
        .and_then(|progress| {
            let status = progress.state.run_status.trim();
            (!status.is_empty()).then_some(status)
        })
        .unwrap_or(root_status);
    let actionable_status = actionable.detail.summary.status.as_str();

    // During detached resume hand-off the root summary can briefly lag behind
    // the actionable child. Prefer the actionable descendant's non-success
    // status so the UI tracks the state the operator can actually act on.
    if actionable.run_id != detail.summary.run_id
        && actionable_status != progress_status
        && !matches!(actionable_status, "success" | "created" | "planned")
    {
        return actionable.detail.summary.status.clone();
    }

    progress_status.to_string()
}

fn request_child_detail(
    inspection: &TransistInspectionContext,
    run_id: &str,
    child_detail_cache: &mut std::collections::BTreeMap<String, transist::InspectionRunDetail>,
) -> Result<Option<transist::InspectionRunDetail>> {
    let Some(detail) = inspection.get_run(run_id)? else {
        return Ok(None);
    };
    child_detail_cache.insert(run_id.to_string(), detail.clone());
    Ok(Some(detail))
}

fn collect_contained_run_details(
    inspection: &TransistInspectionContext,
    initial_child_run_ids: Vec<String>,
) -> Result<std::collections::BTreeMap<String, transist::InspectionRunDetail>> {
    // Bound graph traversal defensively: contained-run graphs are expected to
    // be shallow trees, but corrupt state should not trigger unbounded loading.
    const MAX_VISITED_RUNS: usize = 256;

    let mut cache = std::collections::BTreeMap::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut queue: VecDeque<String> = initial_child_run_ids.into();

    while let Some(run_id) = queue.pop_front() {
        if !visited.insert(run_id.clone()) {
            continue;
        }
        if visited.len() > MAX_VISITED_RUNS {
            break;
        }

        let Some(detail) = request_child_detail(inspection, &run_id, &mut cache)? else {
            continue;
        };

        for step in &detail.steps {
            if let Some(child_id) = step.output_summary.child_run_id.as_deref() {
                if !visited.contains(child_id) {
                    queue.push_back(child_id.to_string());
                }
            }
        }

        for contained in &detail.contained_runs {
            if !visited.contains(contained.run_id.as_str()) {
                queue.push_back(contained.run_id.clone());
            }
        }
    }

    Ok(cache)
}

fn prioritized_child_run_ids(
    detail: &transist::InspectionRunDetail,
    actionable: &crate::actionable::ActionableRun,
    selected_step_id: Option<&str>,
) -> Vec<String> {
    let mut run_ids = Vec::new();
    if actionable.run_id != detail.summary.run_id {
        run_ids.push(actionable.run_id.clone());
    }

    // Look up the selected step by its stable step_id (not a checklist index)
    // so that the BFS correctly seeds the child run regardless of the ordering
    // difference between `detail.steps` (recipe order) and the checklist.
    if let Some(step_id) = selected_step_id
        && let Some(step) = detail.steps.iter().find(|s| s.step_id == step_id)
        && let Some(child_run_id) = step.output_summary.child_run_id.as_deref()
        && !run_ids.iter().any(|id| id == child_run_id)
    {
        run_ids.push(child_run_id.to_string());
    }

    run_ids
}

pub(super) fn default_run_detail_state(
    actions: &[SuggestedAction],
    default_selected_step: usize,
    selected_action: usize,
    run_status: &str,
) -> RunDetailState {
    let focused_section = if run_status == "blocked" && !actions.is_empty() {
        RunDetailSection::Actions
    } else {
        RunDetailSection::Steps
    };
    RunDetailState {
        focused_section,
        selected_step: default_selected_step,
        details_scroll: 0,
        details_cursor: 0,
        selected_action,
        step_pinned_by_user: false,
        pane_cache: None,
    }
}

pub(super) fn default_selected_step_for_run_detail(
    detail: &transist::InspectionRunDetail,
) -> usize {
    if let Some(current_step_id) = detail
        .current_step
        .as_ref()
        .map(|step| step.step_id.as_str())
        && let Some(index) = detail
            .steps
            .iter()
            .position(|step| step.step_id == current_step_id)
    {
        return index;
    }
    if let Some(step) = crate::actionable::latest_step(detail, &["failed", "blocked", "canceled"])
        && let Some(index) = detail
            .steps
            .iter()
            .position(|candidate| candidate.step_id == step.step_id)
    {
        return index;
    }
    detail
        .steps
        .iter()
        .position(|step| step.status != "skipped")
        .unwrap_or(0)
}

fn selected_step_index_for_run_detail(
    detail: &transist::InspectionRunDetail,
    selected_step: Option<usize>,
) -> Option<usize> {
    if detail.steps.is_empty() {
        return None;
    }
    Some(
        selected_step
            .unwrap_or_else(|| default_selected_step_for_run_detail(detail))
            .min(detail.steps.len().saturating_sub(1)),
    )
}

fn cleanup_result_action_available(detail: &transist::InspectionRunDetail) -> bool {
    detail.cleanup.as_ref().is_some_and(|cleanup| {
        matches!(
            cleanup.state.as_str(),
            "succeeded" | "failed" | "running" | "skipped"
        )
    })
}

pub(super) fn reconcile_run_detail_state<'a>(
    state: &'a mut RunDetailState,
    previous_actions: &[SuggestedAction],
    actions: &[SuggestedAction],
    default_selected_step: usize,
    step_count: usize,
) -> &'a mut RunDetailState {
    if step_count == 0 {
        state.selected_step = 0;
    } else if !state.step_pinned_by_user {
        // Auto-follow the workflow's current step while the operator
        // hasn't yet focused the Steps section. This keeps Details in
        // sync with the runner so a freshly-advanced step's output is
        // visible without any keypress.
        state.selected_step = default_selected_step.min(step_count.saturating_sub(1));
        // The cursor/scroll reset mirrors what happens on manual
        // Up/Down navigation: a different step's details start scrolled
        // to the top.
        state.details_scroll = 0;
        state.details_cursor = 0;
    } else if state.selected_step >= step_count {
        state.selected_step = default_selected_step.min(step_count.saturating_sub(1));
    }
    state.selected_action =
        reconcile_selected_action(previous_actions, state.selected_action, actions);
    if actions.is_empty() && matches!(state.focused_section, RunDetailSection::Actions) {
        state.focused_section = RunDetailSection::Details;
    }
    state
}

fn reconcile_selected_action(
    previous_actions: &[SuggestedAction],
    previous_index: usize,
    actions: &[SuggestedAction],
) -> usize {
    if actions.is_empty() {
        return 0;
    }
    if previous_actions.is_empty() {
        return previous_index.min(actions.len() - 1);
    }
    if let Some(previous_execution) = previous_actions
        .get(previous_index)
        .map(|action| &action.execution)
        && let Some(index) = actions
            .iter()
            .position(|action| action.execution == *previous_execution)
    {
        return index;
    }
    actions
        .iter()
        .position(|action| !action.execution.is_mutating())
        .unwrap_or(0)
}

/// Whole-frame fingerprint for the run-detail screen. Combined with
/// `FullscreenPrompt::last_drawn_run_detail_frame_fingerprint`, this lets
/// `apply_run_detail_loaded_data` skip `prompt.draw()` whenever a refresh
/// produced data that resolves to a pixel-identical frame. The expensive
/// CPU pattern this short-circuits is the auto-tick (every 500ms while a
/// tool is running) that re-renders the same Steps / Details / Actions
/// content because streaming output advanced without changing any visible
/// status, counter, or block.
///
/// Inputs must cover *every* mutable input flowing into the run-detail
/// frame builder (`components::draw_run_detail_frame`):
/// - run header (workflow ref + run id + run status)
/// - per-step Steps-pane row (`step_id`, `status`, `execution`, started/finished)
/// - selected step's Details pane (via `run_detail_pane_fingerprint`)
/// - blocked-gate list (gate id + status + decision history length)
/// - picker items (next-action list)
/// - run-detail-state selectors (selected_step, focused_section, scroll)
/// - loading overlay visibility
/// - status banner content
/// - geometry (details pane width is the size proxy that matters for
///   wrap-driven content)
/// - attached step output block counts per anchor
///
/// `preview_text` and other live-streaming bits are intentionally excluded —
/// they don't render to any visible cell on this screen.
#[allow(clippy::too_many_arguments)]
fn run_detail_frame_fingerprint(
    run_id: &str,
    workflow_ref: Option<&str>,
    run_status: &str,
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    progress: Option<&LiveProgress>,
    selected_step: Option<usize>,
    selected_action: usize,
    focused_section: RunDetailSection,
    details_scroll: usize,
    actions: &[SuggestedAction],
    loading_visible: bool,
    status_message: Option<&str>,
    details_width: usize,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    run_id.hash(&mut hasher);
    workflow_ref.hash(&mut hasher);
    run_status.hash(&mut hasher);
    loading_visible.hash(&mut hasher);
    status_message
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .hash(&mut hasher);
    selected_action.hash(&mut hasher);
    selected_step.hash(&mut hasher);
    match focused_section {
        RunDetailSection::Steps => 0u8.hash(&mut hasher),
        RunDetailSection::Details => 1u8.hash(&mut hasher),
        RunDetailSection::Actions => 2u8.hash(&mut hasher),
    }
    details_scroll.hash(&mut hasher);
    details_width.hash(&mut hasher);

    actions.len().hash(&mut hasher);
    for action in actions {
        action.display_command.hash(&mut hasher);
    }

    if let Some(detail) = run_detail {
        detail.steps.len().hash(&mut hasher);
        for step in &detail.steps {
            step.step_id.hash(&mut hasher);
            step.status.hash(&mut hasher);
            step.execution.hash(&mut hasher);
            step.started_at_ms.hash(&mut hasher);
            step.finished_at_ms.hash(&mut hasher);
            step.output_summary.error_code.hash(&mut hasher);
            step.output_summary.error_message.hash(&mut hasher);
        }
        detail.blocked_gates.len().hash(&mut hasher);
        for gate in &detail.blocked_gates {
            gate.gate_id.hash(&mut hasher);
            gate.step_id.hash(&mut hasher);
            gate.status.hash(&mut hasher);
            gate.decision_history.len().hash(&mut hasher);
        }
        if let Some(cleanup) = detail.cleanup.as_ref() {
            cleanup.state.hash(&mut hasher);
            cleanup.steps.len().hash(&mut hasher);
            for step in &cleanup.steps {
                step.step_id.hash(&mut hasher);
                step.status.hash(&mut hasher);
            }
        }
    }

    // Fold the *minimum-viable* status signal of every currently-known
    // contained child run into the frame fingerprint. The parent
    // run-detail screen only renders the following child-side facts:
    //   - the child summary status (contained-runs list, actionable badge)
    //   - the child's current step id + status (active glyph/label)
    //   - the child's step count (inline progress bar segments)
    //   - the child's blocked-gates count (gate-blocked indicator)
    // Anything else (per-step timestamps, output blobs, artifact lists)
    // does not appear on this screen and would only inflate the hash
    // without affecting the rendered frame. Without this fold, a child
    // run that advances steps or transitions out of `blocked`
    // (e.g. immediately after Approve dispatches the actionable child)
    // never perturbs the parent-screen frame hash, because the parent's
    // own state file does not change while the child is the one moving.
    if let Some(map) = contained_run_details {
        map.len().hash(&mut hasher);
        for (child_run_id, child_detail) in map {
            child_run_id.hash(&mut hasher);
            child_detail.summary.status.hash(&mut hasher);
            child_detail.steps.len().hash(&mut hasher);
            child_detail.blocked_gates.len().hash(&mut hasher);
            if let Some(current_step) = child_detail.current_step.as_ref() {
                current_step.step_id.hash(&mut hasher);
                current_step.status.hash(&mut hasher);
            }
        }
    }

    if let Some(progress) = progress {
        progress.state.run_status.hash(&mut hasher);
        progress.state.current_step_status.hash(&mut hasher);
        progress.state.current_step_path.hash(&mut hasher);
        progress.state.top_level_step_id.hash(&mut hasher);
        progress.state.execution.hash(&mut hasher);
        progress.state.cycle.hash(&mut hasher);
        progress.state.tool_name.hash(&mut hasher);
        progress.state.error_code.hash(&mut hasher);
        progress.state.exit_code.hash(&mut hasher);
        progress.state.started_at_ms.is_some().hash(&mut hasher);
        progress.state.finished_at_ms.is_some().hash(&mut hasher);
        progress.state.checklist.len().hash(&mut hasher);
        for item in &progress.state.checklist {
            item.step_id.hash(&mut hasher);
            // Discriminant only; ChecklistState transitions are the
            // observable change on the Steps pane row glyph.
            std::mem::discriminant(&item.state).hash(&mut hasher);
        }
        for block in &progress.attached_step_output_blocks {
            block.anchor_step_id.hash(&mut hasher);
            block.lines.len().hash(&mut hasher);
        }
    }

    // Fold the Details-pane fingerprint last so the (selected-step
    // content + child gate chain) variations also break the frame hash.
    run_detail_pane_fingerprint(
        run_detail,
        contained_run_details,
        progress,
        selected_step,
        details_width,
    )
    .hash(&mut hasher);

    hasher.finish()
}

pub(super) fn apply_run_detail_loaded_data(
    prompt: &mut FullscreenPrompt,
    run_id: &str,
    selected_action: usize,
    previous_actions: &[SuggestedAction],
    status_message: Option<&str>,
    focus_details: bool,
    loaded: RunDetailLoadedData,
) -> Result<RunDetailRefresh> {
    super::tui_debug::log(
        "apply.enter",
        &format!(
            "run_id={run_id} status={} contained={} progress={}",
            loaded.detail.summary.status,
            loaded.detail.contained_runs.len(),
            loaded.current_progress.is_some(),
        ),
    );
    let mut status_lines = loaded.status_lines;
    if let Some(status_message) = status_message.filter(|message| !message.trim().is_empty()) {
        status_lines.push(String::new());
        status_lines.push("Status".to_string());
        status_lines.push(format!("  {}", status_message.trim()));
    }
    prompt.replace_history_lines(status_lines);
    prompt.dashboard_state = None;
    prompt.config_screen_state = None;
    prompt.run_browser_state = None;
    prompt.run_detail_screen_active = true;
    prompt.metadata.run_id = Some(run_id.to_string());
    prompt.metadata.workflow_ref = Some(format!(
        "{}@{}",
        loaded.detail.summary.workflow_id, loaded.detail.summary.workflow_version
    ));
    prompt.metadata.status = Some(loaded.effective_run_status.clone());
    prompt.current_home_dir = Some(loaded.home_dir.clone());
    prompt.current_run_detail = Some(loaded.detail.clone());
    prompt.current_actionable_run_detail = loaded
        .contained_run_details
        .get(&loaded.actionable.run_id)
        .cloned();
    prompt.current_contained_run_details = loaded.contained_run_details.clone();
    // Preserve the previous progress snapshot when the incoming refresh
    // returned `None` while the run is still non-terminal. The
    // `inspect_live_progress` / `inspect_terminal_progress` /
    // `inspect_bootstrap_progress` chain can transiently yield `None`
    // mid-step (e.g. when a child run's `events.jsonl` is being rewritten
    // by the runner between two flushes). In the steady-state run-detail
    // loop those transients are now repainted every ~1.5s because the
    // mutation-poll keep-alive (introduced for the post-Approve freeze
    // fix) draws the screen on every refresh; without this guard the
    // Details/right pane visibly flickers to empty for one frame and
    // refills on the next refresh. Once the run reaches a terminal
    // status we let the `None` win so stale "still running" progress
    // doesn't survive the run's end.
    let incoming_terminal = matches!(
        loaded.effective_run_status.as_str(),
        "success" | "failed" | "canceled"
    );
    prompt.current_progress = match (loaded.current_progress, prompt.current_progress.take()) {
        (Some(fresh), _) => Some(fresh),
        (None, Some(previous)) if !incoming_terminal => Some(previous),
        (None, _) => None,
    };
    if let Some(progress) = prompt.current_progress.as_ref() {
        prompt.metadata.actionable_run_id = Some(progress.state.actionable_run_id.clone());
        prompt.metadata.current_step = progress.state.top_level_step_id.clone();
    } else {
        prompt.metadata.actionable_run_id = Some(loaded.actionable.run_id.clone());
        prompt.metadata.current_step = loaded
            .detail
            .current_step
            .as_ref()
            .map(|step| step.step_id.clone());
    }

    let actions = loaded.actions;
    let default_selected_step = prompt
        .current_progress
        .as_ref()
        .and_then(selected_step_index)
        .or_else(|| {
            prompt
                .current_run_detail
                .as_ref()
                .map(default_selected_step_for_run_detail)
        })
        .unwrap_or(0);
    let step_count = {
        prompt
            .current_progress
            .as_ref()
            .map(|progress| progress.state.checklist.len())
            .or_else(|| {
                prompt
                    .current_run_detail
                    .as_ref()
                    .map(|detail| detail.steps.len())
            })
            .unwrap_or(0)
    };
    let (_, _, details_width, full_details_visible, _, _) = prompt
        .terminal
        .size()
        .map(run_detail_section_metrics)
        .unwrap_or((30, 10, 40, 10, 20, 10));
    // Match the on-screen height budget so clamp_scroll_offset can let the
    // user reach the last rows of pre-wrapped Details content even when the
    // Actions/Commands picker is taking the bottom of the right pane.
    let details_visible = details_visible_rows_with_picker(full_details_visible, actions.len());
    prompt.picker_state = (!actions.is_empty()).then(|| ActionPickerState {
        title: "Next:".to_string(),
        items: numbered_run_detail_command_items(&actions),
        selected: selected_action,
        actionable: true,
    });
    let selected_action = {
        let run_detail_state = reconcile_run_detail_state(
            prompt.run_detail_state.get_or_insert_with(|| {
                default_run_detail_state(
                    &actions,
                    default_selected_step,
                    selected_action,
                    loaded.effective_run_status.as_str(),
                )
            }),
            previous_actions,
            &actions,
            default_selected_step,
            step_count,
        );
        if focus_details {
            run_detail_state.focused_section = RunDetailSection::Details;
        }
        let fingerprint = run_detail_pane_fingerprint(
            prompt.current_run_detail.as_ref(),
            Some(&prompt.current_contained_run_details),
            prompt.current_progress.as_ref(),
            Some(run_detail_state.selected_step),
            details_width,
        );
        let detail_display = if let Some(cache) = run_detail_state
            .pane_cache
            .as_ref()
            .filter(|cache| cache.fingerprint == fingerprint)
        {
            cache.display.clone()
        } else {
            let display = build_run_detail_history_lines(
                prompt.current_home_dir.as_deref(),
                prompt.current_run_detail.as_ref(),
                Some(&prompt.current_contained_run_details),
                prompt.current_progress.as_ref(),
                Some(run_detail_state.selected_step),
                details_width,
                prompt.spinner_tick,
            );
            run_detail_state.pane_cache = Some(RunDetailPaneCache {
                fingerprint,
                display: display.clone(),
            });
            display
        };
        run_detail_state.details_scroll = clamp_scroll_offset(
            detail_display.lines.len(),
            details_visible,
            run_detail_state.details_scroll,
        );
        run_detail_state.selected_action
    };
    if let Some(picker_state) = prompt.picker_state.as_mut() {
        picker_state.selected = selected_action;
    }
    // Do not reset `spinner_tick` here: the run-detail event loop bumps the
    // tick on its idle redraw cadence so the in-pane spinner glyph
    // animates between data refreshes. Zeroing it on every load made the
    // spinner appear frozen while elapsed counters kept advancing.
    //
    // Frame-fingerprint gate: skip the redraw when this refresh produced
    // data that resolves to the same visible frame as the previous draw.
    // The auto-tick loop fires every 500ms while a tool is running; in
    // the steady state (streaming output that does not change any step
    // status / counter / gate) every one of those ticks would otherwise
    // walk the full ratatui layout + style pipeline for an identical
    // frame. Skipping the draw is the primary CPU saver for the
    // run-detail screen.
    let (focused_section, details_scroll, selected_step_index_for_hash) = prompt
        .run_detail_state
        .as_ref()
        .map(|state| {
            (
                state.focused_section,
                state.details_scroll,
                Some(state.selected_step),
            )
        })
        .unwrap_or((RunDetailSection::Details, 0, None));
    let frame_fingerprint = run_detail_frame_fingerprint(
        run_id,
        prompt.metadata.workflow_ref.as_deref(),
        &loaded.effective_run_status,
        prompt.current_run_detail.as_ref(),
        Some(&prompt.current_contained_run_details),
        prompt.current_progress.as_ref(),
        selected_step_index_for_hash,
        selected_action,
        focused_section,
        details_scroll,
        &actions,
        prompt.loading_state.is_some(),
        status_message,
        details_width,
    );
    let previous_frame_fingerprint = prompt.last_drawn_run_detail_frame_fingerprint;
    if previous_frame_fingerprint != Some(frame_fingerprint) {
        super::tui_debug::log(
            "apply.fingerprint",
            &format!(
                "decision=draw old={:?} new={:x} loading={}",
                previous_frame_fingerprint,
                frame_fingerprint,
                prompt.loading_state.is_some(),
            ),
        );
        prompt.draw()?;
        prompt.last_drawn_run_detail_frame_fingerprint = Some(frame_fingerprint);
    } else {
        super::tui_debug::log(
            "apply.fingerprint",
            &format!(
                "decision=skip fp={:x} loading={}",
                frame_fingerprint,
                prompt.loading_state.is_some(),
            ),
        );
    }

    let effective_run_status = loaded.effective_run_status;
    let terminal = matches!(
        effective_run_status.as_str(),
        "success" | "failed" | "canceled"
    );
    // Transitional step-boundary detection: progress is None but the run
    // is not in a terminal state. Used by the poll loop to keep the
    // refresh cadence hot until a real next step (or terminal status)
    // materialises.
    let transient_no_progress = prompt.current_progress.is_none() && !terminal;

    // A contained child counts as "active" for poll-cadence purposes if
    // its own status hasn't settled. We deliberately include `blocked`
    // here: a blocked child can be unblocked by an external actor (a
    // sibling shell approving its gate, a reaper releasing a lease)
    // while the parent's own files don't change, so the screen must keep
    // polling to catch the flip. `success|failed|canceled` are the only
    // statuses we treat as quiescent for the child.
    let has_active_child = prompt.current_contained_run_details.values().any(|child| {
        !matches!(
            child.summary.status.as_str(),
            "success" | "failed" | "canceled"
        )
    });

    Ok(RunDetailRefresh {
        actions,
        selected_action,
        running: implies_active_tool_work(prompt.current_progress.as_ref()),
        effective_run_status,
        terminal,
        transient_no_progress,
        has_active_child,
    })
}

pub(super) fn build_run_detail_status_lines(
    context: &transist::TransistContext,
    detail: &transist::InspectionRunDetail,
    actionable: &crate::actionable::ActionableRun,
    run_id: &str,
    run_status: &str,
) -> Result<Vec<String>> {
    let workflow_ref = format!(
        "{}@{}",
        detail.summary.workflow_id, detail.summary.workflow_version
    );
    let mut lines = vec![
        format!("Run status: {}", run_status),
        format!("Run id: {}", run_id),
        format!("Workflow: {}", workflow_ref),
    ];
    if actionable.run_id != run_id {
        lines.push(format!("Internal child run: {}", actionable.run_id));
    }
    if let Some(current_step) = detail.current_step.as_ref() {
        lines.push(format!("Current step: {}", current_step.step_id));
    }
    if let Some(global) = detail.global_default_filesystem_policy.as_ref() {
        lines.push(format!(
            "Global default policy: {}",
            format_inspection_policy_summary(global)
        ));
    }
    if let Some(workflow) = detail.workflow_filesystem_policy.as_ref() {
        lines.push(format!(
            "Workflow policy: {}",
            format_inspection_policy_summary(workflow)
        ));
    }
    if let Some(cleanup) = detail.cleanup.as_ref() {
        lines.push(format!("Cleanup state: {}", cleanup.state));
        if cleanup.state == "failed"
            && let Some(step) = cleanup.steps.iter().find(|step| step.status == "failed")
        {
            lines.push(format!("Cleanup failed step: {}", step.step_id));
            if let Some(message) = step
                .error_message
                .as_deref()
                .map(str::trim)
                .filter(|message| !message.is_empty())
            {
                lines.push(format!("Cleanup failure reason: {}", message));
            }
        }
    }

    let summary = match run_status {
        "blocked" => Some(build_blocked_summary_content(
            context, detail, run_id, actionable,
        )?),
        "failed" => Some(build_failed_summary_content(
            context, detail, actionable, run_id,
        )),
        _ => None,
    };
    if let Some(summary) = summary {
        if !summary.body_lines.is_empty() {
            lines.push(String::new());
            lines.extend(summary.body_lines);
        }
        if !summary.step_output_lines.is_empty() {
            lines.push(String::new());
            lines.extend(summary.step_output_lines);
        }
        return Ok(lines);
    }

    // Hot-path optimization: reuse the already-resolved `detail` + `actionable`
    // instead of calling `compact_run_status_lines(context, run_id)`, which
    // would build a fresh non-cached `TransistInspectionContext` and re-do
    // `inspection.get_run` + `resolve_actionable_run` (full serde walk of the
    // persisted run tree). That duplicate fetch on every refresh-tick was the
    // dominant source of TUI run-detail CPU consumption.
    let advice = crate::display::summary::build_compact_run_status_advice_from_details(
        detail, actionable, run_id,
    );
    Ok(crate::display::summary::render_compact_run_status_advice(
        &advice,
    ))
}

pub(super) fn build_run_detail_actions(
    context: &transist::TransistContext,
    detail: &transist::InspectionRunDetail,
    actionable: &crate::actionable::ActionableRun,
    run_id: &str,
    run_status: &str,
) -> Result<Vec<SuggestedAction>> {
    let mut actions = match run_status {
        "created" | "planned" => vec![
            SuggestedAction::new(SuggestedActionExecution::Run {
                run_id: run_id.to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsPreflight {
                run_id: run_id.to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: run_id.to_string(),
            }),
        ],
        "running" | "waiting" => vec![
            SuggestedAction::new(SuggestedActionExecution::CancelRun {
                run_id: run_id.to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: run_id.to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsTail {
                run_id: actionable.run_id.clone(),
                limit: 80,
            }),
        ],
        "blocked" => {
            build_blocked_summary_content(context, detail, run_id, actionable)?.suggested_actions
        }
        "failed" => {
            build_failed_summary_content(context, detail, actionable, run_id).suggested_actions
        }
        "canceled" => {
            let cleanup_completed = detail
                .cleanup
                .as_ref()
                .map(|cleanup| matches!(cleanup.state.as_str(), "succeeded"))
                .unwrap_or(false);
            let mut actions = Vec::new();
            if cleanup_result_action_available(detail) {
                actions.push(SuggestedAction::new(
                    SuggestedActionExecution::InspectCleanupResult {
                        run_id: run_id.to_string(),
                    },
                ));
            }
            if !cleanup_completed {
                actions.push(SuggestedAction::new(SuggestedActionExecution::RunsResume {
                    run_id: run_id.to_string(),
                }));
            }
            if !cleanup_completed {
                actions.push(SuggestedAction::new(
                    SuggestedActionExecution::RunsCleanup {
                        run_id: run_id.to_string(),
                    },
                ));
            }
            actions.push(SuggestedAction::new(SuggestedActionExecution::RunsReplay {
                run_id: run_id.to_string(),
            }));
            actions.push(SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: run_id.to_string(),
            }));
            actions.push(SuggestedAction::new(SuggestedActionExecution::RunsTail {
                run_id: actionable.run_id.clone(),
                limit: 80,
            }));
            actions
        }
        "success" => {
            let mut actions = Vec::new();
            if cleanup_result_action_available(detail) {
                actions.push(SuggestedAction::new(
                    SuggestedActionExecution::InspectCleanupResult {
                        run_id: run_id.to_string(),
                    },
                ));
            }
            if detail
                .cleanup
                .as_ref()
                .map(|cleanup| matches!(cleanup.state.as_str(), "failed" | "pending" | "running"))
                .unwrap_or(false)
            {
                actions.push(SuggestedAction::new(
                    SuggestedActionExecution::RunsCleanup {
                        run_id: run_id.to_string(),
                    },
                ));
            }
            actions.push(SuggestedAction::new(SuggestedActionExecution::RunsReplay {
                run_id: run_id.to_string(),
            }));
            actions.push(SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: run_id.to_string(),
            }));
            actions.push(SuggestedAction::new(SuggestedActionExecution::RunsTail {
                run_id: actionable.run_id.clone(),
                limit: 80,
            }));
            actions
        }
        _ => vec![
            SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: run_id.to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsTail {
                run_id: actionable.run_id.clone(),
                limit: 80,
            }),
        ],
    };
    // Surface lease inspect/break whenever a live lease is present, regardless
    // of run status — diagnosing a stuck run-tree is exactly when these matter.
    if detail.active_lease.is_some() {
        actions.push(SuggestedAction::new(
            SuggestedActionExecution::LeaseInspect {
                run_id: run_id.to_string(),
            },
        ));
        actions.push(SuggestedAction::new(SuggestedActionExecution::LeaseBreak {
            run_id: run_id.to_string(),
        }));
    }
    if matches!(
        detail.summary.status.as_str(),
        "failed" | "canceled" | "success"
    ) && cleanup_result_action_available(detail)
        && !actions.iter().any(|action| {
            matches!(
                action.execution,
                SuggestedActionExecution::InspectCleanupResult { .. }
            )
        })
    {
        actions.insert(
            0,
            SuggestedAction::new(SuggestedActionExecution::InspectCleanupResult {
                run_id: run_id.to_string(),
            }),
        );
    }
    // Always offer replay for failed runs too — the dedicated `failed` summary
    // builder owns its own action list, so push it explicitly here.
    if detail.summary.status == "failed"
        && !actions.iter().any(|action| {
            matches!(
                action.execution,
                SuggestedActionExecution::RunsReplay { .. }
            )
        })
    {
        actions.push(SuggestedAction::new(SuggestedActionExecution::RunsReplay {
            run_id: run_id.to_string(),
        }));
    }
    if detail.summary.status == "failed"
        && detail
            .preflight
            .as_ref()
            .map(preflight_has_capability_denial)
            .unwrap_or(false)
    {
        actions.insert(
            0,
            SuggestedAction::new(SuggestedActionExecution::RunsCapabilities {
                run_id: run_id.to_string(),
            }),
        );
    }
    if run_has_persisted_diagnostic(detail, &actionable.detail) {
        actions.insert(
            0,
            SuggestedAction::new(SuggestedActionExecution::ViewFullDiagnostic {
                run_id: run_id.to_string(),
            }),
        );
    }
    actions.retain(|action| !matches!(action.execution, SuggestedActionExecution::Exit));
    Ok(actions)
}

pub(super) fn run_has_persisted_diagnostic(
    root_detail: &transist::InspectionRunDetail,
    actionable_detail: &transist::InspectionRunDetail,
) -> bool {
    root_detail.summary.status == "failed"
        || actionable_detail.summary.status == "failed"
        || actionable_detail
            .blocked_gates
            .iter()
            .any(|gate| gate.status == "blocked")
        || actionable_detail
            .steps
            .iter()
            .any(|step| step.status == "failed")
        || actionable_detail
            .preflight
            .as_ref()
            .is_some_and(|preflight| preflight.blocker_count > 0)
        || root_detail.cleanup.as_ref().is_some_and(|cleanup| {
            cleanup.state == "failed"
                || cleanup.steps.iter().any(|step| step.status == "failed")
                || cleanup
                    .preflight
                    .as_ref()
                    .is_some_and(|preflight| preflight.blocker_count > 0)
        })
        || root_detail
            .trust
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == "denied")
        || root_detail
            .trust
            .policy_decision
            .as_ref()
            .is_some_and(|decision| !decision.allowed)
}

pub(super) fn execute_run_detail_action(
    prompt: &mut FullscreenPrompt,
    context: &transist::TransistContext,
    interrupt_flag: &Arc<AtomicBool>,
    action: SuggestedAction,
) -> Result<RunDetailActionOutcome> {
    if let SuggestedActionExecution::ViewFullDiagnostic { run_id } = &action.execution {
        let (root_detail, actionable) =
            crate::actionable::resolve_actionable_run_with_root(&context.inspection(), run_id)?
                .with_context(|| format!("load persisted diagnostic data for run {run_id}"))?;
        let blocked_gate = actionable
            .detail
            .blocked_gates
            .iter()
            .any(|gate| gate.status == "blocked");
        let (severity, fallback_code) = if blocked_gate
            && root_detail.summary.status != "failed"
            && actionable.detail.summary.status != "failed"
        {
            (
                super::diagnostics::DiagnosticSeverity::Attention,
                "run.blocked",
            )
        } else {
            (super::diagnostics::DiagnosticSeverity::Error, "run.failed")
        };
        let report = super::diagnostics::persisted_run_diagnostic_for_actionable(
            &root_detail,
            &actionable.detail,
            severity,
            Some(fallback_code.to_string()),
            "Run diagnostic derived from persisted inspection data.",
        );
        super::diagnostics::show_diagnostic_report(prompt, "Run diagnostic", &report)?;
        return Ok(RunDetailActionOutcome::Stay(Some(
            "Closed full diagnostic.".to_string(),
        )));
    }
    if let SuggestedActionExecution::CancelRun { run_id } = &action.execution {
        // CancelRun is only surfaced for active runs, so assume `running` here;
        // request_run_cancellation_with_confirmation will still display the
        // confirmation modal before contacting the runtime.
        return Ok(RunDetailActionOutcome::Stay(
            request_run_cancellation_with_confirmation(prompt, context, run_id, true)?,
        ));
    }
    if let SuggestedActionExecution::RunsCleanup { run_id } = &action.execution {
        if !prompt.run_confirm_modal(
            run_cleanup_confirm_title(),
            &run_cleanup_confirm_message(run_id),
        )? {
            return Ok(RunDetailActionOutcome::Stay(Some(
                "Run cleanup canceled.".to_string(),
            )));
        }
    }
    if let SuggestedActionExecution::LeaseBreak { run_id } = &action.execution {
        if !prompt.run_confirm_modal(
            run_lease_break_confirm_title(),
            &run_lease_break_confirm_message(run_id),
        )? {
            return Ok(RunDetailActionOutcome::Stay(Some(
                "Lease break canceled.".to_string(),
            )));
        }
    }
    let Some(resolved) = resolve_selected_action(prompt, &action)? else {
        return Ok(RunDetailActionOutcome::Stay(Some(
            "Action canceled.".to_string(),
        )));
    };
    let prepared_gate_input = match &resolved.execution {
        SuggestedActionExecution::Approve { reason, .. }
        | SuggestedActionExecution::Reject { reason, .. }
        | SuggestedActionExecution::RequestChanges { reason, .. } => {
            let Some(input) = gate_decision_input_for_tui(prompt, context, reason.clone())? else {
                return Ok(RunDetailActionOutcome::Stay(Some(
                    "Gate decision canceled. Set a gate actor in user config to avoid this prompt."
                        .to_string(),
                )));
            };
            Some(input)
        }
        _ => None,
    };
    if let SuggestedActionExecution::RunsCleanup { run_id } = &resolved.execution {
        prompt.picker_state = None;
        return run_cleanup_action_and_show_result(prompt, context, run_id);
    }
    if resolved.execution.is_mutating() {
        prompt.loading_state = Some(super::LoadingState {
            message: format!("Applying action: {}…", resolved.execution.action_label()),
        });
        prompt.draw()?;
    }
    prompt.picker_state = None;
    (|| {
        // Mutating actions invoked from inside the run-detail screen are
        // handled in place: we record any required side-effect (gate decision)
        // and spawn the detached runner, then signal the event loop to
        // re-render the same screen pointing at the (possibly new) target run
        // id. Opening a nested viewer here would race the parent loop and the
        // user would see a stale snapshot until they Esc'd back out.
        let repo_root = context.repo_root().to_path_buf();
        let outcome = match &resolved.execution {
            SuggestedActionExecution::Run { run_id } => Some(spawn_runner_and_request_refresh(
                context,
                &repo_root,
                crate::runner::RunnerAction::Run {
                    run_id: run_id.clone(),
                    trusted_execution: false,
                    local_only: false,
                },
                run_id.clone(),
            )?),
            SuggestedActionExecution::Approve {
                run_id, gate_id, ..
            } => {
                let gate_result = context.record_gate_decision_with_input(
                    run_id,
                    gate_id.as_deref(),
                    transist::GateDecision::Approved,
                    prepared_gate_input.clone().expect("gate input prepared"),
                )?;
                if let Some(warning) = &gate_result.lease_release_warning {
                    eprintln!(
                        "warning: gate decision committed with a lease release warning: {}",
                        warning.message
                    );
                }
                let root_run_id = gate_result.root_run_id;
                let runner_action = reentry_runner_action_for_run(context, &root_run_id)?;
                Some(spawn_runner_and_request_refresh(
                    context,
                    &repo_root,
                    runner_action,
                    root_run_id,
                )?)
            }
            SuggestedActionExecution::Reject {
                run_id, gate_id, ..
            } => {
                let gate_result = context.record_gate_decision_with_input(
                    run_id,
                    gate_id.as_deref(),
                    transist::GateDecision::Rejected,
                    prepared_gate_input.clone().expect("gate input prepared"),
                )?;
                if let Some(warning) = &gate_result.lease_release_warning {
                    eprintln!(
                        "warning: gate decision committed with a lease release warning: {}",
                        warning.message
                    );
                }
                let root_run_id = gate_result.root_run_id;
                let runner_action = reentry_runner_action_for_run(context, &root_run_id)?;
                Some(spawn_runner_and_request_refresh(
                    context,
                    &repo_root,
                    runner_action,
                    root_run_id,
                )?)
            }
            SuggestedActionExecution::RequestChanges {
                run_id, gate_id, ..
            } => {
                let gate_result = context.record_gate_decision_with_input(
                    run_id,
                    gate_id.as_deref(),
                    transist::GateDecision::RequestChanges,
                    prepared_gate_input.clone().expect("gate input prepared"),
                )?;
                if let Some(warning) = &gate_result.lease_release_warning {
                    eprintln!(
                        "warning: gate decision committed with a lease release warning: {}",
                        warning.message
                    );
                }
                let root_run_id = gate_result.root_run_id;
                let runner_action = reentry_runner_action_for_run(context, &root_run_id)?;
                Some(spawn_runner_and_request_refresh(
                    context,
                    &repo_root,
                    runner_action,
                    root_run_id,
                )?)
            }
            SuggestedActionExecution::RunsResume { run_id } => {
                Some(spawn_runner_and_request_refresh(
                    context,
                    &repo_root,
                    crate::runner::RunnerAction::Resume {
                        run_id: run_id.clone(),
                        trusted_execution: false,
                        local_only: false,
                    },
                    run_id.clone(),
                )?)
            }
            SuggestedActionExecution::RunsReplay { run_id } => {
                let replay_run_id = uuid::Uuid::new_v4().to_string();
                Some(spawn_runner_and_request_refresh(
                    context,
                    &repo_root,
                    crate::runner::RunnerAction::Replay {
                        source_run_id: run_id.clone(),
                        replay_run_id: replay_run_id.clone(),
                        trusted_execution: false,
                        local_only: false,
                    },
                    replay_run_id,
                )?)
            }
            _ => None,
        };
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        let _ = execute_suggested_action(
            context,
            prompt,
            interrupt_flag,
            &resolved,
            false,
            crate::cli::CliUiMode::Tui,
        )?;
        Ok(RunDetailActionOutcome::Stay(None))
    })()
}

/// Helper for `execute_run_detail_action`: spawn a detached runner for the
/// given action and return a `RefreshNow` outcome that re-points the
/// active run-detail screen at `target_run_id`. Suppresses the spawn
/// announcement (it would corrupt the alt screen) and surfaces a status
/// line summarising the spawn instead.
fn spawn_runner_and_request_refresh(
    context: &transist::TransistContext,
    repo_root: &std::path::Path,
    runner_action: crate::runner::RunnerAction,
    target_run_id: String,
) -> Result<RunDetailActionOutcome> {
    // Capture the runner.log byte offset BEFORE spawning so that the
    // post-spawn poll only scans bytes the new runner wrote (and never
    // surfaces a stale failure-signal from a prior run).
    let log_offset = crate::runner::runner_log_offset(context.home_dir(), &target_run_id);
    let pid = crate::runner::spawn_detached_runner(context.home_dir(), repo_root, &runner_action)?;
    // Brief synchronous wait so we can catch *early* dispatch failures (e.g.
    // the kernel rejects spawning against a terminal-status root). The healthy
    // path resolves to `Ok(None)` within a single poll tick and we fall
    // through to the optimistic status message; the unhealthy path surfaces
    // the runner.log tail so the operator does not stare at a stale modal
    // followed by no visible change.
    // `wait_for_runner_decision` returns a `RunnerEarlyVerdict` that
    // distinguishes a clean exit, a log-observable failure signal (runner
    // still alive but logs already show a fatal error), and "still running"
    // (no early verdict within the poll window). We surface each path with
    // a tailored status message so the operator does not stare at a stale
    // "Applying…" modal followed by no visible change.
    let verdict = crate::runner::wait_for_runner_decision(
        pid,
        context.home_dir(),
        &target_run_id,
        log_offset,
        std::time::Duration::from_millis(250),
    )?;
    let runner_log_watch =
        matches!(verdict, crate::runner::RunnerEarlyVerdict::StillRunning).then(|| {
            RunnerLogWatch {
                run_id: target_run_id.clone(),
                start_offset: log_offset,
            }
        });
    let status_message = match verdict {
        crate::runner::RunnerEarlyVerdict::Exited(code) if code != 0 => {
            let tail = crate::runner::tail_runner_log(context.home_dir(), &target_run_id, 3);
            let summary = if tail.is_empty() {
                format!("runner exited with code {code} (see runner.log)")
            } else {
                format!("runner exited with code {code}: {tail}")
            };
            Some(format!("Detached runner failed: {summary}"))
        }
        crate::runner::RunnerEarlyVerdict::Exited(_) => Some(format!(
            "Detached runner (pid {pid}) exited 0 before we could observe progress. Refreshing run view…"
        )),
        crate::runner::RunnerEarlyVerdict::FailureSignal(tail) => Some(format!(
            "Detached runner (pid {pid}) reported failure in log: {tail}"
        )),
        crate::runner::RunnerEarlyVerdict::StillRunning => Some(format!(
            "Spawned detached runner (pid {pid}). Refreshing run view…"
        )),
    };
    Ok(RunDetailActionOutcome::RefreshNow {
        new_run_id: target_run_id,
        status_message,
        runner_log_watch,
    })
}

fn gate_decision_input_for_tui(
    prompt: &mut FullscreenPrompt,
    context: &transist::TransistContext,
    reason: Option<String>,
) -> Result<Option<transist::GateDecisionInput>> {
    if transist::resolve_gate_actor_identity_in_home(context.home_dir())?.is_some() {
        return Ok(Some(transist::GateDecisionInput {
            reason,
            ..Default::default()
        }));
    }

    let Some(actor) = prompt.run_text_input_modal_with_placeholder(
        "Gate actor required (can be set in user config)",
        "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
        "",
        Some("Reviewer <reviewer@example.com>"),
        true,
    )?
    else {
        return Ok(None);
    };
    let actor = actor.trim();
    if actor.is_empty() {
        return Ok(None);
    }

    Ok(Some(transist::GateDecisionInput {
        actor: Some(actor.to_string()),
        actor_source: Some("prompt".to_string()),
        reason,
        ..Default::default()
    }))
}

/// Outcome returned to `run_run_detail_screen` after handling a picker
/// action.
///
/// - `Stay(message)` keeps the viewer open and optionally surfaces
///   `message` in the status bar on the next refresh.
/// - `RefreshNow { new_run_id, status_message, runner_log_watch }` keeps the viewer open
///   but forces an immediate data reload, optionally re-pointing at a
///   different run id (e.g. the root-run id returned by a gate-decision
///   action). `runner_log_watch` lets the parent screen surface detached-runner
///   dispatch failures that land just after the short spawn handoff wait. Used
///   so mutating actions appear instantly in the same screen instead of opening
///   a nested viewer.
/// - `ExitScreen` tells the outer event loop to return to its caller.
pub(super) enum RunDetailActionOutcome {
    Stay(Option<String>),
    RefreshNow {
        new_run_id: String,
        status_message: Option<String>,
        runner_log_watch: Option<RunnerLogWatch>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunnerLogWatch {
    pub(super) run_id: String,
    pub(super) start_offset: u64,
}

pub(super) fn request_run_cancellation_with_confirmation(
    prompt: &mut FullscreenPrompt,
    context: &transist::TransistContext,
    run_id: &str,
    running: bool,
) -> Result<Option<String>> {
    if !running {
        return Ok(Some(format!(
            "Run {} is not currently active, so there is nothing to interrupt.",
            run_id
        )));
    }
    if !prompt.run_confirm_modal(
        "Interrupt running run?",
        "Request cancellation for this active run tree?",
    )? {
        return Ok(Some("Run interruption canceled.".to_string()));
    }
    let cancellation = match crate::execution::request_active_run_cancellation(context, run_id) {
        Ok(cancellation) => cancellation,
        Err(error) => {
            if let Some(message) =
                crate::execution::inactive_run_cancellation_status_message(&error)
            {
                return Ok(Some(message));
            }
            return Err(error);
        }
    };
    let immediate_kill_note = cancellation
        .active_execution_cancellation_error
        .as_deref()
        .map(|error| format!(" Direct cancellation attempt did not complete: {error}"))
        .unwrap_or_default();
    Ok(Some(format!(
        "Cancellation requested for root run tree {}.{}",
        cancellation.root_run_id, immediate_kill_note
    )))
}

fn run_cleanup_confirm_title() -> &'static str {
    "Run cleanup is destructive"
}

fn run_cleanup_confirm_message(run_id: &str) -> String {
    format!(
        "Approving starts detached cleanup for this run and updates its persisted state. Once cleanup settles, a failed run may become non-resumable. Proceed with transist runs cleanup {}?",
        run_id
    )
}

fn run_cleanup_action_and_show_result(
    prompt: &mut FullscreenPrompt,
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<RunDetailActionOutcome> {
    let context_clone = context.clone();
    let run_id_owned = run_id.to_string();
    let (title, body) =
        crate::prompt::run_with_loading_feedback(prompt, "Cleaning up run...", move || {
            run_cleanup_and_build_result_page(&context_clone, &run_id_owned)
        })?;
    prompt.run_pager_modal(&title, &body)?;
    Ok(RunDetailActionOutcome::Stay(Some(format!(
        "Cleanup settled for run {}.",
        run_id
    ))))
}

fn run_lease_break_confirm_title() -> &'static str {
    "Break persisted lease?"
}

fn run_lease_break_confirm_message(run_id: &str) -> String {
    format!(
        "Approving removes the persisted lease file for this run tree so another runner can acquire it. Use this only when the lease is stale or orphaned. Proceed with transist runs lease break {}?",
        run_id
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RunDetailDisplayData {
    pub(super) lines: Vec<DetailLine>,
}

/// One rendered detail-pane line. `text` is the post-wrap visible string.
/// `openable` is `Some(path)` when the line refers to a single concrete file
/// on disk (workflow file, artifact blob, evidence blob); the run-detail
/// screen reads this to resolve the `o` key into a pager invocation.
///
/// Wrapped continuation lines for the same source row carry the same
/// `openable` so the cursor still resolves to the file regardless of which
/// visual line it lands on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DetailLine {
    pub(super) text: String,
    pub(super) openable: Option<PathBuf>,
}

impl DetailLine {
    #[allow(dead_code)]
    pub(super) fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            openable: None,
        }
    }
}

pub(super) fn build_run_detail_history_lines(
    home_dir: Option<&Path>,
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    progress: Option<&LiveProgress>,
    selected_step: Option<usize>,
    wrap_width: usize,
    spinner_tick: u64,
) -> RunDetailDisplayData {
    if let Some(progress) = progress {
        let (lines, openables) = build_selected_step_detail_lines_with_openables(
            home_dir,
            run_detail,
            contained_run_details,
            progress,
            selected_step,
            spinner_tick,
        );
        if !lines.is_empty() {
            return RunDetailDisplayData {
                lines: wrapped_detail_lines(&lines, &openables, wrap_width),
            };
        }
    }
    if let Some(run_detail) = run_detail {
        let (lines, openables) = build_selected_step_detail_lines_without_progress_with_openables(
            home_dir,
            run_detail,
            contained_run_details,
            selected_step,
        );
        if !lines.is_empty() {
            return RunDetailDisplayData {
                lines: wrapped_detail_lines(&lines, &openables, wrap_width),
            };
        }
    }
    RunDetailDisplayData { lines: Vec::new() }
}

/// Returns a structural fingerprint for the rendered Details pane payload.
/// Keep this in lockstep with fields consumed by `build_unified_step_detail_lines`.
/// `preview_text` is intentionally excluded so live token streaming does not
/// force a full details rebuild every refresh tick.
fn run_detail_pane_fingerprint(
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    progress: Option<&LiveProgress>,
    selected_step: Option<usize>,
    pane_width: usize,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    pane_width.hash(&mut hasher);
    // Descendant file locations are rendered inline in subflow/gate details.
    // A changed local root must invalidate those rows even when counts match.
    if let Some(contained) = contained_run_details {
        for (run_id, detail) in contained {
            run_id.hash(&mut hasher);
            for step in &detail.steps {
                for file in &step.output_summary.workflow_files {
                    file.local_path.hash(&mut hasher);
                }
            }
        }
    }

    let Some(progress) = progress else {
        if let Some(detail) = run_detail
            && let Some(selected_index) = selected_step_index_for_run_detail(detail, selected_step)
            && let Some(step) = detail.steps.get(selected_index)
        {
            selected_index.hash(&mut hasher);
            step.step_id.hash(&mut hasher);
            step.status.hash(&mut hasher);
            step.output_summary.status.hash(&mut hasher);
            step.input_summary.adapter_kind.hash(&mut hasher);
            step.input_summary.plugin_id.hash(&mut hasher);
            step.input_summary.action.hash(&mut hasher);
            step.input_summary.input_object_keys.hash(&mut hasher);
            step.input_summary.metadata_object_keys.hash(&mut hasher);
            if let Some(policy) = step.input_summary.filesystem_policy.as_ref() {
                policy.source.hash(&mut hasher);
                policy.workspace_root.hash(&mut hasher);
                format!("{:?}", policy.workspace_access.mode).hash(&mut hasher);
                policy.read_allow_paths.hash(&mut hasher);
                policy.write_allow_paths.hash(&mut hasher);
            }
            step.execution.hash(&mut hasher);
            step.output_summary.child_run_id.hash(&mut hasher);
            step.started_at_ms.hash(&mut hasher);
            step.finished_at_ms.hash(&mut hasher);
            step.output_summary.error_code.hash(&mut hasher);
            step.output_summary.error_message.hash(&mut hasher);

            if detail_failed_in_preflight_without_failed_step(detail) {
                hash_preflight_failure_summary(&mut hasher, detail);
            }

            step.output_summary.workflow_files.len().hash(&mut hasher);
            for file in &step.output_summary.workflow_files {
                file.relative_path.hash(&mut hasher);
                file.local_path.hash(&mut hasher);
                file.bytes.hash(&mut hasher);
            }

            step.output_summary.artifact_refs.len().hash(&mut hasher);
            for artifact in &step.output_summary.artifact_refs {
                artifact.relative_path.hash(&mut hasher);
                artifact.bytes.hash(&mut hasher);
            }

            step.output_summary.evidence_refs.len().hash(&mut hasher);
            for evidence in &step.output_summary.evidence_refs {
                evidence.relative_path.hash(&mut hasher);
                evidence.bytes.hash(&mut hasher);
            }

            if let Some(child_run_id) = step.output_summary.child_run_id.as_deref() {
                hash_child_failure_summary(&mut hasher, step, child_run_id, contained_run_details);
                if let Some(map) = contained_run_details
                    && let Some(child_detail) = map.get(child_run_id)
                    && let Some(chain) = find_blocked_gate_chain(child_detail, Some(map))
                {
                    for node in chain {
                        node.summary.run_id.hash(&mut hasher);
                        node.blocked_gates.len().hash(&mut hasher);
                        node.steps.len().hash(&mut hasher);
                    }
                }
            }
        }

        if let Some(detail) = run_detail {
            if detail_failed_in_preflight_without_failed_step(detail) {
                hash_preflight_failure_summary(&mut hasher, detail);
            }
            for gate in &detail.blocked_gates {
                gate.gate_id.hash(&mut hasher);
                gate.step_id.hash(&mut hasher);
                gate.status.hash(&mut hasher);
                gate.decision_history.len().hash(&mut hasher);
            }
            if let Some(cleanup) = detail.cleanup.as_ref() {
                cleanup.state.hash(&mut hasher);
                cleanup.steps.len().hash(&mut hasher);
            }
        }

        return hasher.finish();
    };

    let selected_index = selected_step
        .or_else(|| selected_step_index(progress))
        .unwrap_or(0);
    selected_index.hash(&mut hasher);

    // Virtual cleanup step when selected index is beyond the checklist
    if selected_index >= progress.state.checklist.len() {
        if let Some(detail) = run_detail
            && let Some(cleanup) = detail.cleanup.as_ref()
        {
            cleanup.state.hash(&mut hasher);
            cleanup.steps.len().hash(&mut hasher);
        }
        return hasher.finish();
    }

    let Some(selected_item) = progress.state.checklist.get(selected_index) else {
        return hasher.finish();
    };
    selected_item.step_id.hash(&mut hasher);
    let selected_step_id = selected_item.step_id.as_str();

    let step = run_detail.and_then(|detail| {
        detail
            .steps
            .iter()
            .find(|candidate| candidate.step_id == selected_step_id)
    });
    let is_current_top_level =
        progress.state.top_level_step_id.as_deref() == Some(selected_step_id);

    if let Some(step) = step {
        step.status.hash(&mut hasher);
        step.output_summary.status.hash(&mut hasher);
        step.input_summary.adapter_kind.hash(&mut hasher);
        step.input_summary.plugin_id.hash(&mut hasher);
        step.input_summary.action.hash(&mut hasher);
        step.input_summary.input_object_keys.hash(&mut hasher);
        step.input_summary.metadata_object_keys.hash(&mut hasher);
        if let Some(policy) = step.input_summary.filesystem_policy.as_ref() {
            policy.source.hash(&mut hasher);
            policy.workspace_root.hash(&mut hasher);
            format!("{:?}", policy.workspace_access.mode).hash(&mut hasher);
            policy.read_allow_paths.hash(&mut hasher);
            policy.write_allow_paths.hash(&mut hasher);
        }
        let execution = if is_current_top_level {
            progress.state.execution
        } else {
            step.execution
        };
        execution.hash(&mut hasher);
        step.output_summary.child_run_id.hash(&mut hasher);
        step.started_at_ms.hash(&mut hasher);
        step.finished_at_ms.hash(&mut hasher);
        step.output_summary.error_code.hash(&mut hasher);
        step.output_summary.error_message.hash(&mut hasher);

        step.output_summary.workflow_files.len().hash(&mut hasher);
        for file in &step.output_summary.workflow_files {
            file.relative_path.hash(&mut hasher);
            file.local_path.hash(&mut hasher);
            file.bytes.hash(&mut hasher);
        }

        step.output_summary.artifact_refs.len().hash(&mut hasher);
        for artifact in &step.output_summary.artifact_refs {
            artifact.relative_path.hash(&mut hasher);
            artifact.bytes.hash(&mut hasher);
        }

        step.output_summary.evidence_refs.len().hash(&mut hasher);
        for evidence in &step.output_summary.evidence_refs {
            evidence.relative_path.hash(&mut hasher);
            evidence.bytes.hash(&mut hasher);
        }

        if let Some(child_run_id) = step.output_summary.child_run_id.as_deref() {
            hash_child_failure_summary(&mut hasher, step, child_run_id, contained_run_details);
            if let Some(map) = contained_run_details
                && let Some(child_detail) = map.get(child_run_id)
                && let Some(chain) = find_blocked_gate_chain(child_detail, Some(map))
            {
                for node in chain {
                    node.summary.run_id.hash(&mut hasher);
                    node.blocked_gates.len().hash(&mut hasher);
                    node.steps.len().hash(&mut hasher);
                }
            }
        }
    }

    if let Some(detail) = run_detail
        && detail_failed_in_preflight_without_failed_step(detail)
    {
        hash_preflight_failure_summary(&mut hasher, detail);
    }

    if is_current_top_level {
        progress.state.tool_name.hash(&mut hasher);
    }

    for block in &progress.attached_step_output_blocks {
        if block.anchor_step_id.as_deref() == Some(selected_step_id) {
            block.anchor_step_id.hash(&mut hasher);
            block.lines.len().hash(&mut hasher);
        }
    }

    if let Some(detail) = run_detail {
        for gate in &detail.blocked_gates {
            gate.gate_id.hash(&mut hasher);
            gate.step_id.hash(&mut hasher);
            gate.status.hash(&mut hasher);
            gate.decision_history.len().hash(&mut hasher);
        }
    }

    hasher.finish()
}

pub(super) fn build_selected_step_detail_lines(
    home_dir: Option<&Path>,
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    progress: &LiveProgress,
    selected_index: Option<usize>,
    spinner_tick: u64,
) -> Vec<String> {
    if let Some(detail) = run_detail
        && detail_failed_in_preflight_without_failed_step(detail)
    {
        return build_preflight_failure_detail_lines(detail);
    }

    let selected_index = selected_index
        .or_else(|| selected_step_index(progress))
        .unwrap_or(0);

    let Some(selected_item) = progress.state.checklist.get(selected_index) else {
        return Vec::new();
    };

    if let Some(run_detail) = run_detail {
        return build_unified_step_detail_lines(
            home_dir,
            Some(run_detail),
            contained_run_details,
            progress,
            &selected_item.step_id,
            spinner_tick,
        );
    }

    if matches!(
        selected_item.state,
        crate::display::progress::ChecklistState::Active
    ) {
        return build_unified_step_detail_lines(
            home_dir,
            None,
            contained_run_details,
            progress,
            &selected_item.step_id,
            spinner_tick,
        );
    }

    fallback_attached_step_output_lines(progress, &selected_item.step_id)
}

/// Like [`build_selected_step_detail_lines`] but also returns a parallel
/// `Vec<Option<PathBuf>>` indexed by source-line position. Each `Some(path)`
/// tags a line that refers to one concrete on-disk file (workflow file,
/// artifact, evidence). The run-detail screen consumes this vector to
/// resolve the `o` key into a pager invocation against the selected line.
pub(super) fn build_selected_step_detail_lines_with_openables(
    home_dir: Option<&Path>,
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    progress: &LiveProgress,
    selected_index: Option<usize>,
    spinner_tick: u64,
) -> (Vec<String>, Vec<Option<PathBuf>>) {
    let lines = build_selected_step_detail_lines(
        home_dir,
        run_detail,
        contained_run_details,
        progress,
        selected_index,
        spinner_tick,
    );
    let openables = derive_openable_paths(&lines);
    (lines, openables)
}

fn build_selected_step_detail_lines_without_progress(
    home_dir: Option<&Path>,
    run_detail: &transist::InspectionRunDetail,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    selected_index: Option<usize>,
) -> Vec<String> {
    if detail_failed_in_preflight_without_failed_step(run_detail) {
        return build_preflight_failure_detail_lines(run_detail);
    }

    let Some(selected_index) = selected_step_index_for_run_detail(run_detail, selected_index)
    else {
        return Vec::new();
    };

    let Some(step) = run_detail.steps.get(selected_index) else {
        return Vec::new();
    };

    let mut lines = build_failed_step_detail_lines(Some(run_detail), step, contained_run_details);
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push("Execution:".to_string());
    lines.push(format!("  Step status: {}", step.status));
    if let Some(result_status) = step.output_summary.status.as_deref() {
        if !result_status.eq_ignore_ascii_case(step.status.as_str()) {
            lines.push(format!("  Result status: {}", result_status));
        }
    }
    if let Some(adapter) = step.input_summary.adapter_kind.as_deref() {
        lines.push(format!("  Adapter: {}", adapter));
    }
    if let Some(action) = step.input_summary.action.as_deref() {
        lines.push(format!("  Action: {}", action));
    }
    if let Some(policy) = step.input_summary.filesystem_policy.as_ref() {
        lines.push(format!(
            "  Filesystem policy: {}",
            format_inspection_policy_summary(policy)
        ));
        if let Some(workspace_root) = policy.workspace_root.as_deref() {
            lines.push(format!("  Workspace root: {}", workspace_root));
        }
    }
    if let Some(execution) = step.execution {
        lines.push(format!("  Execution: {}", execution));
    }
    if let Some(child_run_id) = step.output_summary.child_run_id.as_deref() {
        lines.push(format!("  Child run: {}", child_run_id));
    }
    if let Some(started) = step.started_at_ms {
        lines.push(format!("  Started: {}", format_unix_ms_iso8601(started)));
    }
    if let Some(finished) = step.finished_at_ms {
        lines.push(format!("  Finished: {}", format_unix_ms_iso8601(finished)));
    }
    if let Some(error_code) = step.output_summary.error_code.as_deref() {
        lines.push(format!("  Error code: {}", error_code));
    }
    if let Some(error_message) = step
        .output_summary
        .error_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        lines.push(format!("  Error: {}", error_message));
    }
    if let Some(stderr_tail) = step
        .output_summary
        .stderr_tail
        .as_deref()
        .map(str::trim)
        .filter(|tail| !tail.is_empty())
    {
        // Render even when error_message already contains the same text: the
        // protocol-level error_message is often a generic wrapper (e.g.
        // "command exited with code N") and the stderr tail is the real
        // diagnostic operators need to see.
        lines.push(format!("  Stderr: {}", stderr_tail));
    }

    append_workflow_file_detail_lines(&mut lines, &step.output_summary.workflow_files);
    append_blob_ref_detail_lines(
        &mut lines,
        "Artifacts",
        home_dir,
        &run_detail.summary.run_id,
        BlobFamily::Artifact,
        &step.output_summary.artifact_refs,
    );
    append_blob_ref_detail_lines(
        &mut lines,
        "Evidence",
        home_dir,
        &run_detail.summary.run_id,
        BlobFamily::Evidence,
        &step.output_summary.evidence_refs,
    );
    append_canceled_step_explanation(&mut lines, step);
    append_gate_detail_lines(&mut lines, home_dir, run_detail, step);

    if let Some(child_run_id) = step.output_summary.child_run_id.as_deref()
        && let Some(map) = contained_run_details
        && let Some(child_detail) = map.get(child_run_id)
    {
        let chain = find_blocked_gate_chain(child_detail, Some(map));
        if let Some(chain) = chain.as_ref() {
            append_gate_chain_detail_lines(&mut lines, home_dir, chain);
        } else {
            let mut visited: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            append_subflow_detail_lines(
                &mut lines,
                home_dir,
                child_detail,
                Some(map),
                &mut visited,
                0,
            );
        }
    }

    lines
}

fn format_inspection_policy_summary(
    policy: &transist::InspectionFilesystemPolicySummary,
) -> String {
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

pub(super) fn build_selected_step_detail_lines_without_progress_with_openables(
    home_dir: Option<&Path>,
    run_detail: &transist::InspectionRunDetail,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    selected_index: Option<usize>,
) -> (Vec<String>, Vec<Option<PathBuf>>) {
    let lines = build_selected_step_detail_lines_without_progress(
        home_dir,
        run_detail,
        contained_run_details,
        selected_index,
    );
    let openables = derive_openable_paths(&lines);
    (lines, openables)
}

/// Scan rendered detail lines and tag each one that references a concrete
/// on-disk file with its absolute path. The renderer emits openable rows in
/// only two shapes:
///   * Workflow file rows under a `Workflow files:` heading — formatted as
///     `  <field>: '<path>' (<n> bytes)`.
///   * Artifact / evidence rows under an `Artifacts:` or `Evidence:`
///     heading — formatted as `  '<path>' (<n> bytes)`.
///
/// Both shapes are reused at deeper indents inside the subflow body, where
/// each level adds two leading spaces. Detection is by single-quoted path
/// extraction so the indent depth does not matter. Only absolute paths are
/// openable; unavailable locations must never be resolved against the TUI CWD.
fn derive_openable_paths(lines: &[String]) -> Vec<Option<PathBuf>> {
    lines
        .iter()
        .map(|line| extract_openable_path(line))
        .collect()
}

pub(super) fn extract_openable_path(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim_end();

    // Primary shape: `...: '<path>' (<n> bytes)` — workflow files, artifacts, evidence.
    // Anchor on the ` bytes)` suffix so we never tag a heading or unrelated line
    // that happens to contain a quote.
    if trimmed.ends_with(" bytes)") {
        let close_paren = trimmed.rfind(" (")?;
        let head = &trimmed[..close_paren];
        let path_close = head.rfind('\'')?;
        let head = &head[..path_close];
        let path_open = head.find('\'')?;
        let path = &head[path_open + 1..];
        if Path::new(path).is_absolute() {
            return Some(PathBuf::from(path));
        }
    }

    // Secondary shape: `...: '<absolute-path>'` — gate request summary resolved
    // paths (run:// resolved, no byte count). Require an absolute path so
    // short-quoted strings like gate IDs are never mistaken for files.
    if let Some(head) = trimmed.strip_suffix('\'') {
        if let Some(path_open) = head.find('\'') {
            let path = &head[path_open + 1..];
            if !path.is_empty() && Path::new(path).is_absolute() {
                return Some(PathBuf::from(path));
            }
        }
    }

    None
}

/// Wrap a parallel `(lines, openables)` pair by source-line into a
/// `Vec<DetailLine>`. Each source line wraps via the same algorithm used
/// elsewhere in the run pane; every produced fragment carries the source
/// line's `openable` so the cursor still resolves to the same file no
/// matter which visual fragment it lands on.
fn wrapped_detail_lines(
    lines: &[String],
    openables: &[Option<PathBuf>],
    width: usize,
) -> Vec<DetailLine> {
    let width = width.max(1);
    let mut out = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        let openable = openables.get(idx).cloned().flatten();
        for fragment in wrap_detail_pane_line(line, width) {
            out.push(DetailLine {
                text: fragment,
                openable: openable.clone(),
            });
        }
    }
    out
}

/// Wrap a single Details-pane source line to `columns`, preserving the
/// readability of any embedded single-quoted file path.
///
/// The Details pane previously delegated to
/// `crate::display::spinner::wrap_attached_step_output_line`, whose
/// path-aware carve-out returns path-bearing lines verbatim — letting
/// long absolute paths run past the right border of the pane and get
/// clipped by ratatui. The pane benefits from a different policy:
/// **always emit fragments no wider than `columns`**, and when the line
/// embeds a path, **prefer to break at `/` boundaries** so each fragment
/// remains scannable. Continuation fragments are indented by the
/// original line's leading whitespace plus two spaces, so the wrapped
/// path visually nests under its label (`workflow.spec_path:`,
/// `Artifacts:`, etc.).
///
/// Lines without an embedded path fall back to the same plain-text
/// hard-wrap used by the spinner. Lines that already fit are returned
/// unchanged.
///
/// Used only by the Details pane (`wrapped_detail_lines` here and the
/// `v`/`e` cursor resolver in `screens.rs`). The spinner wrapper is
/// deliberately untouched so live-progress / non-TUI rendering keep
/// today's behavior.
pub(super) fn wrap_detail_pane_line(line: &str, columns: usize) -> Vec<String> {
    use crate::display::spinner::wrap_attached_step_output_line;
    use crate::terminal::visible_terminal_text_width;

    let columns = columns.max(1);
    if visible_terminal_text_width(line) <= columns {
        return vec![line.to_string()];
    }

    // Detect an embedded single-quoted path — same shape the spinner
    // wrapper used to short-circuit on. We honor the same detection
    // (single-quoted substring containing `/`) but, instead of returning
    // the line verbatim, wrap it path-aware.
    if let Some(fragments) = try_wrap_path_bearing_line(line, columns) {
        return fragments;
    }

    // Fall back to the existing hard-wrap for non-path lines so we
    // preserve existing wrap behavior (leading-indent preservation,
    // unicode-width handling) tested in `main_tests.rs`.
    wrap_attached_step_output_line(line, columns)
}

/// If `line` embeds a single-quoted path-like value, return a wrapped
/// rendering that breaks at `/` boundaries and falls back to a hard
/// character split only when a single segment exceeds the column budget.
/// Returns `None` when no embedded path is detected, letting the caller
/// route the line through the plain-text wrapper.
fn try_wrap_path_bearing_line(line: &str, columns: usize) -> Option<Vec<String>> {
    let open_quote = line.find('\'')?;
    let after_open = open_quote + 1;
    let close_offset = line[after_open..].find('\'')?;
    let close_quote = after_open + close_offset;
    let path = &line[after_open..close_quote];
    if !path.contains('/') {
        return None;
    }

    let leading_ws = line
        .chars()
        .take_while(|ch| ch.is_ascii_whitespace())
        .count();
    let prefix_indent: String = line.chars().take(leading_ws).collect();
    let continuation_indent = format!("{}  ", prefix_indent);

    // Header: text up through the opening quote (e.g. `  workflow.path: '`).
    // Tail: text from the closing quote onward (e.g. `' (123 bytes)`).
    let header = &line[..after_open];
    let tail = &line[close_quote..];

    let mut out: Vec<String> = Vec::new();
    let mut current = header.to_string();

    // Greedy fill: append `/`-delimited path segments until the next
    // segment would overflow `columns`, then start a new continuation
    // line. A segment longer than `columns` is hard-wrapped char-by-char
    // on its own line(s).
    let segments = split_path_segments(path);
    let mut first_segment = true;
    for segment in segments {
        let candidate_len = current.chars().count() + segment.chars().count();
        if candidate_len <= columns {
            current.push_str(&segment);
            first_segment = false;
            continue;
        }
        // Flush current line if it carries content beyond the indent.
        if !first_segment {
            out.push(std::mem::take(&mut current));
            current = continuation_indent.clone();
        }
        // Segment may itself exceed `columns`; emit hard-wrapped chunks
        // when needed. The very first segment is appended directly to
        // the freshly-flushed continuation line so the indent + segment
        // is one contiguous prefix.
        let mut start = current.clone();
        let budget = columns.saturating_sub(start.chars().count()).max(1);
        let chars: Vec<char> = segment.chars().collect();
        if chars.len() <= budget {
            start.push_str(&segment);
            current = start;
        } else {
            // First chunk fills out the freshly-flushed line.
            let first_chunk: String = chars[..budget].iter().collect();
            start.push_str(&first_chunk);
            out.push(start);
            // Subsequent chunks: pure continuation lines.
            let continuation_budget = columns
                .saturating_sub(continuation_indent.chars().count())
                .max(1);
            let mut idx = budget;
            while idx < chars.len() {
                let end = (idx + continuation_budget).min(chars.len());
                let chunk: String = chars[idx..end].iter().collect();
                let mut row = continuation_indent.clone();
                row.push_str(&chunk);
                out.push(row);
                idx = end;
            }
            current = continuation_indent.clone();
        }
        first_segment = false;
    }

    // Append the trailing portion (closing quote + any suffix like
    // ` (123 bytes)`). If it does not fit on the current row, push a new
    // row with continuation indent, and hard-wrap the tail itself when
    // it exceeds `columns` on its own.
    if !tail.is_empty() {
        let candidate_len = current.chars().count() + tail.chars().count();
        if candidate_len > columns {
            if !current.trim_end().is_empty()
                && current.chars().count() > continuation_indent.chars().count()
            {
                out.push(std::mem::take(&mut current));
            }
            current = continuation_indent.clone();
            let tail_chars: Vec<char> = tail.chars().collect();
            let budget = columns.saturating_sub(current.chars().count()).max(1);
            if tail_chars.len() <= budget {
                let chunk: String = tail_chars.iter().collect();
                current.push_str(&chunk);
            } else {
                // First chunk fills the freshly-flushed continuation row.
                let first: String = tail_chars[..budget].iter().collect();
                current.push_str(&first);
                out.push(std::mem::take(&mut current));
                let cont_budget = columns
                    .saturating_sub(continuation_indent.chars().count())
                    .max(1);
                let mut idx = budget;
                while idx < tail_chars.len() {
                    let end = (idx + cont_budget).min(tail_chars.len());
                    let chunk: String = tail_chars[idx..end].iter().collect();
                    let mut row = continuation_indent.clone();
                    row.push_str(&chunk);
                    out.push(row);
                    idx = end;
                }
                current = String::new();
            }
        } else {
            current.push_str(tail);
        }
    }

    if !current.is_empty() {
        out.push(current);
    }

    if out.is_empty() {
        return Some(vec![line.to_string()]);
    }
    Some(out)
}

/// Split a path string into wrap-friendly segments, keeping each
/// separator attached to the segment that follows it. So `/a/b/c`
/// becomes `["/a", "/b", "/c"]` rather than `["", "a", "b", "c"]`,
/// which lets the wrapper greedily concatenate without losing slashes
/// or producing leading-empty fragments.
fn split_path_segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = path.chars().collect();
    let mut idx = 0;
    while idx < chars.len() {
        if chars[idx] == '/' {
            let start = idx;
            idx += 1;
            while idx < chars.len() && chars[idx] != '/' {
                idx += 1;
            }
            out.push(chars[start..idx].iter().collect());
        } else {
            // Leading text before the first `/` (e.g. `relative/path`):
            // treat as its own segment.
            let start = idx;
            while idx < chars.len() && chars[idx] != '/' {
                idx += 1;
            }
            out.push(chars[start..idx].iter().collect());
        }
    }
    out
}

fn fallback_attached_step_output_lines(
    progress: &LiveProgress,
    selected_step_id: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    for block in &progress.attached_step_output_blocks {
        if block.anchor_step_id.as_deref() == Some(selected_step_id) {
            lines.extend(block.lines.iter().cloned());
        }
    }
    lines
}

/// Format a Unix-epoch milliseconds timestamp as ISO-8601 / RFC-3339 in UTC
/// with second precision (e.g. `2026-05-04T12:34:56Z`). Hand-rolled to avoid
/// pulling `chrono` or `time` for a single-use formatter; the algorithm is the
/// civil-from-days conversion (Howard Hinnant) which is well-defined for the
/// proleptic Gregorian calendar across the entire `i64` range.
pub(super) fn format_unix_ms_iso8601(unix_ms: u64) -> String {
    let total_seconds = (unix_ms / 1000) as i64;
    let days = total_seconds.div_euclid(86_400);
    let secs_of_day = total_seconds.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Civil-from-days: Hinnant 2010, "chrono-Compatible Low-Level Date Algorithms".
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, minute, second
    )
}

/// `true` when the step's persisted status indicates it is actively
/// executing (running / dispatching / awaiting retry). Mirrors the
/// `is_running_checklist_item` predicate in `display/spinner.rs` so the
/// Details pane placeholder agrees with the Steps pane's "running" set.
pub(super) fn is_step_actively_running(step: &transist::InspectionStep) -> bool {
    matches!(
        step.status.as_str(),
        "running" | "dispatching" | "retry_wait"
    )
}

/// Wall-clock now in Unix milliseconds. Inlined here so the running-step
/// placeholder can render a live elapsed timer without depending on a
/// kernel clock port.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Information about the deepest currently-running descendant step found
/// when walking `step.output_summary.child_run_id` recursively through
/// the already-loaded `contained_run_details` map. Returned by
/// [`find_deepest_running_leaf`].
///
/// No fresh disk I/O — the walk reads only the in-memory map; the cost is
/// bounded by tree depth (typically <10).
#[derive(Debug, Clone)]
pub(super) struct DeepestRunningLeaf {
    pub(super) run_id: String,
    pub(super) step_id: String,
    pub(super) status: String,
}

/// Walk the latest-execution chain (`step.output_summary.child_run_id`)
/// from `step` downward and return the deepest descendant step whose
/// status is `running`/`dispatching`/`retry_wait`. Returns `None` when
/// no running descendant exists or the chain is unavailable.
///
/// Read-only over the in-memory `contained_run_details` map. Visited set
/// is keyed by run id to guard against cycles (a sub-run referencing
/// itself transitively).
pub(super) fn find_deepest_running_leaf(
    step: &transist::InspectionStep,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
) -> Option<DeepestRunningLeaf> {
    let map = contained_run_details?;
    let mut current_child_run_id = step.output_summary.child_run_id.as_deref()?;
    let mut visited: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut deepest: Option<DeepestRunningLeaf> = None;
    const MAX_DEPTH: usize = 16;
    for _ in 0..MAX_DEPTH {
        if !visited.insert(current_child_run_id.to_string()) {
            break;
        }
        let Some(child_detail) = map.get(current_child_run_id) else {
            break;
        };
        let running_step = child_detail
            .steps
            .iter()
            .find(|s| is_step_actively_running(s));
        let Some(running_step) = running_step else {
            break;
        };
        deepest = Some(DeepestRunningLeaf {
            run_id: child_detail.summary.run_id.clone(),
            step_id: running_step.step_id.clone(),
            status: running_step.status.clone(),
        });
        match running_step.output_summary.child_run_id.as_deref() {
            Some(next) => current_child_run_id = next,
            None => break,
        }
    }
    deepest
}

/// Render the static-when-not-running, live-when-running placeholder for
/// the Details pane. Shown when the selected step's status is
/// `running`/`dispatching`/`retry_wait` to avoid re-walking the
/// descendant subflow tree every 500ms refresh — the costly path the
/// real-terminal CPU profile pinned at ~52% (`load_run_detail_data` →
/// `serde_json::from_str::<PersistedRunRecord>` across 15 loader
/// threads).
///
/// Liveness comes from two cheap sources:
///   * `spinner_tick` advances every 500ms (driven by the event loop's
///     `next_spinner_at` clamp) and rotates the glyph in
///     [`spinner_frame`].
///   * Elapsed wall-clock for the currently-selected top-level step comes
///     from `LiveProgress.state.elapsed_seconds()` so the Details timer
///     stays aligned with the Steps row; other running rows fall back to
///     `step.started_at_ms` against `now_unix_ms()`.
///
/// The deepest running leaf is read from the already-loaded
/// `contained_run_details` map (see [`find_deepest_running_leaf`]) — no
/// fresh disk I/O.
fn render_running_step_placeholder(
    step: &transist::InspectionStep,
    deepest_leaf: Option<&DeepestRunningLeaf>,
    spinner_tick: u64,
    elapsed_seconds: Option<u64>,
) -> Vec<String> {
    let mut lines = vec!["Execution:".to_string()];
    let spinner = crate::display::progress::spinner_frame(spinner_tick);
    let elapsed_suffix = elapsed_seconds
        .map(|s| format!(" ({}s active)", s))
        .unwrap_or_default();
    lines.push(format!(
        "  Step status: {} {}{}",
        step.status, spinner, elapsed_suffix
    ));
    if let Some(adapter) = step.input_summary.adapter_kind.as_deref() {
        lines.push(format!("  Adapter: {}", adapter));
    }
    if let Some(action) = step.input_summary.action.as_deref() {
        lines.push(format!("  Action: {}", action));
    }
    if let Some(policy) = step.input_summary.filesystem_policy.as_ref() {
        lines.push(format!(
            "  Filesystem policy: {}",
            format_inspection_policy_summary(policy)
        ));
        if let Some(workspace_root) = policy.workspace_root.as_deref() {
            lines.push(format!("  Workspace root: {}", workspace_root));
        }
    }
    if let Some(execution) = step.execution {
        lines.push(format!("  Execution: {}", execution));
    }
    if let Some(child_run_id) = step.output_summary.child_run_id.as_deref() {
        lines.push(format!("  Child run: {}", child_run_id));
    }
    if let Some(started) = step.started_at_ms {
        lines.push(format!("  Started: {}", format_unix_ms_iso8601(started)));
    }
    if let Some(leaf) = deepest_leaf {
        lines.push(String::new());
        lines.push("Active leaf:".to_string());
        // Deliberately omit `(Ns)` elapsed here: the "Step status" line
        // above already renders a single authoritative live timer for the
        // selected step. Showing a second elapsed counter on the nested
        // leaf row duplicated information and forced the Details pane to
        // re-render every spinner tick purely to advance the inner
        // counter.
        lines.push(format!(
            "  {} / {}  {}",
            leaf.run_id, leaf.step_id, leaf.status
        ));
    }
    lines
}

/// Unified Details panel builder used for both live (running/waiting) and
/// terminal (success/failed/blocked/canceled) runs. Field order is fixed; any
/// field whose data is absent is omitted silently. The same builder powers the
/// active run screen and `transist runs show --tui` so the layout never
/// diverges between the two surfaces.
///
/// When the selected step is actively running (status ∈ {running,
/// dispatching, retry_wait}), the builder short-circuits to
/// [`render_running_step_placeholder`] which renders a cheap live
/// header + deepest-running-leaf summary. This skips the descendant
/// subflow walk (`find_blocked_gate_chain` →
/// `append_subflow_detail_lines`) that previously dominated CPU during
/// the 500ms refresh cycle on running runs (~52% in real-terminal
/// profiles). Terminal-step renders still produce the full payload.
fn build_unified_step_detail_lines(
    home_dir: Option<&Path>,
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    progress: &LiveProgress,
    selected_step_id: &str,
    spinner_tick: u64,
) -> Vec<String> {
    let step = run_detail.and_then(|detail| {
        detail
            .steps
            .iter()
            .find(|step| step.step_id == selected_step_id)
    });
    let is_current_top_level =
        progress.state.top_level_step_id.as_deref() == Some(selected_step_id);

    if let Some(detail) = run_detail
        && detail_failed_in_preflight_without_failed_step(detail)
    {
        return build_preflight_failure_detail_lines(detail);
    }

    // Short-circuit for actively-running steps. See function-level doc
    // for the CPU rationale. Manual-gate chains live only under
    // *terminal*-or-blocked subflow frontiers (a running step cannot
    // sit above a blocked descendant on its latest-execution chain), so
    // suppressing the subflow walk here does not hide actionable gates.
    if let Some(step) = step {
        if is_step_actively_running(step) {
            let deepest_leaf = find_deepest_running_leaf(step, contained_run_details);
            let elapsed_seconds = if is_current_top_level {
                progress.state.elapsed_seconds()
            } else {
                let now_ms = now_unix_ms();
                step.started_at_ms
                    .filter(|&started| started <= now_ms)
                    .map(|started| (now_ms - started) / 1000)
            };
            return render_running_step_placeholder(
                step,
                deepest_leaf.as_ref(),
                spinner_tick,
                elapsed_seconds,
            );
        }
    }

    let mut lines = step
        .map(|step| build_failed_step_detail_lines(run_detail, step, contained_run_details))
        .unwrap_or_default();
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push("Execution:".to_string());

    // 1. Status (step status; result status only when meaningfully different)
    let step_status = step
        .map(|step| step.status.clone())
        .unwrap_or_else(|| "running".to_string());
    lines.push(format!("  Step status: {}", step_status));
    if let Some(step) = step {
        if let Some(result_status) = step.output_summary.status.as_deref() {
            if !result_status.eq_ignore_ascii_case(step.status.as_str()) {
                lines.push(format!("  Result status: {}", result_status));
            }
        }
    }

    // 2. Tool / adapter / plugin / action
    if let Some(step) = step {
        if let Some(adapter) = step.input_summary.adapter_kind.as_deref() {
            lines.push(format!("  Adapter: {}", adapter));
        }
        if let Some(action) = step.input_summary.action.as_deref() {
            lines.push(format!("  Action: {}", action));
        }
        if let Some(policy) = step.input_summary.filesystem_policy.as_ref() {
            lines.push(format!(
                "  Filesystem policy: {}",
                format_inspection_policy_summary(policy)
            ));
            if let Some(workspace_root) = policy.workspace_root.as_deref() {
                lines.push(format!("  Workspace root: {}", workspace_root));
            }
        }
    }
    if is_current_top_level {
        if let Some(tool_name) = progress.state.tool_name.as_deref() {
            lines.push(format!("  Tool: {}", tool_name));
        }
    }

    // 4. Execution # + child run id (paired with status block)
    let execution = if is_current_top_level {
        progress.state.execution
    } else {
        step.and_then(|step| step.execution)
    };
    if let Some(execution) = execution {
        lines.push(format!("  Execution: {}", execution));
    }
    if let Some(child_run_id) = step.and_then(|step| step.output_summary.child_run_id.as_deref()) {
        lines.push(format!("  Child run: {}", child_run_id));
    }

    // 5. Started / finished (ISO-8601 UTC). Elapsed and exit-code are
    //    intentionally omitted from this pane: per-step duration is already
    //    visible on the Steps checklist, and exit codes are noisy when
    //    successful while error_code/error_message already carry the failure
    //    signal when they exist.
    if let Some(step) = step {
        if let Some(started) = step.started_at_ms {
            lines.push(format!("  Started: {}", format_unix_ms_iso8601(started)));
        }
        if let Some(finished) = step.finished_at_ms {
            lines.push(format!("  Finished: {}", format_unix_ms_iso8601(finished)));
        }
    }

    // 6. Error code, error message (exit code intentionally suppressed)
    if let Some(step) = step {
        if let Some(error_code) = step.output_summary.error_code.as_deref() {
            lines.push(format!("  Error code: {}", error_code));
        }
        if let Some(error_message) = step
            .output_summary
            .error_message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
        {
            lines.push(format!("  Error: {}", error_message));
        }
        if let Some(stderr_tail) = step
            .output_summary
            .stderr_tail
            .as_deref()
            .map(str::trim)
            .filter(|tail| !tail.is_empty())
        {
            // See note in build_failed_step_detail_lines: stderr is rendered
            // even when error_message is set, because error.message is often a
            // generic wrapper ("command exited with code N") that hides the
            // real diagnostic.
            lines.push(format!("  Stderr: {}", stderr_tail));
        }
    }

    // 7. Metrics intentionally omitted: `Output keys` and the
    //    `Counts: artifacts=N evidence=M` summary added noise without enabling
    //    any user action; the actual artifact/evidence file lists below are
    //    the useful surface.

    // 3. Workflow file(s) — placed after the scalar status header for read flow
    if let Some(step) = step {
        append_workflow_file_detail_lines(&mut lines, &step.output_summary.workflow_files);
    }

    // 9. Artifacts, 10. Evidence
    if let (Some(step), Some(run_detail)) = (step, run_detail) {
        append_blob_ref_detail_lines(
            &mut lines,
            "Artifacts",
            home_dir,
            &run_detail.summary.run_id,
            BlobFamily::Artifact,
            &step.output_summary.artifact_refs,
        );
        append_blob_ref_detail_lines(
            &mut lines,
            "Evidence",
            home_dir,
            &run_detail.summary.run_id,
            BlobFamily::Evidence,
            &step.output_summary.evidence_refs,
        );
        append_canceled_step_explanation(&mut lines, step);
        // 11. Gate block (with Next: sub-list when applicable)
        append_gate_detail_lines(&mut lines, home_dir, run_detail, step);
    }

    // 12. Subflow body — if the selected step delegated to a child run, surface
    //     the child run's blocked gates and every child step's
    //     workflow_files/artifacts inline. This is the surface a reviewer
    //     uses to actually inspect what the parent step produced (e.g. the
    //     openspec proposal/tasks/design files generated by a child
    //     `generate_plan` step) without having to drill into the child run
    //     in a separate `runs show`.
    if let Some(step) = step {
        if let Some(child_run_id) = step.output_summary.child_run_id.as_deref() {
            if let Some(map) = contained_run_details {
                if let Some(child_detail) = map.get(child_run_id) {
                    // When a descendant run holds a blocked manual gate,
                    // the operator's only job at this step is to act on
                    // it. Render ONLY the gate-relevant chain
                    // (outermost → innermost descendant runs leading to
                    // the blocked gate); skip the parent step's own
                    // history and the full descendant subflow tree, both
                    // of which buried the gate-relevant files in the
                    // middle of the pane for deeply nested review-plan gates.
                    //
                    // Per-step `output_summary` already reflects only
                    // the latest execution, and we only descend through
                    // `step.output_summary.child_run_id` (also the
                    // latest execution's child run), so we don't need
                    // to filter older executions explicitly.
                    let chain = find_blocked_gate_chain(child_detail, Some(map));
                    if let Some(chain) = chain.as_ref() {
                        append_gate_chain_detail_lines(&mut lines, home_dir, chain);
                    } else {
                        let mut visited: std::collections::BTreeSet<String> =
                            std::collections::BTreeSet::new();
                        append_subflow_detail_lines(
                            &mut lines,
                            home_dir,
                            child_detail,
                            Some(map),
                            &mut visited,
                            0,
                        );
                    }
                }
            }
        }
    }

    lines
}

/// Walk descendant runs (depth-first, following each step's latest
/// execution via `output_summary.child_run_id`) and return the chain of
/// runs from `root` down to the run that owns a blocked manual gate.
///
/// The chain is ordered outermost → innermost: index 0 is `root`, the
/// last entry owns the blocked gate. Returns `None` when no descendant
/// run has a blocked gate.
///
/// We deliberately handle only a single blocked-gate chain. If the tree
/// contains more than one (different branches each holding a blocked
/// gate at the same time), the first one found by depth-first traversal
/// wins. This matches today's workflows where a run pauses on exactly
/// one gate at a time; revisit if real workflows produce multiple
/// concurrent blocked gates.
fn find_blocked_gate_chain<'a>(
    root: &'a transist::InspectionRunDetail,
    contained_run_details: Option<
        &'a std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
) -> Option<Vec<&'a transist::InspectionRunDetail>> {
    fn walk<'a>(
        current: &'a transist::InspectionRunDetail,
        map: Option<&'a std::collections::BTreeMap<String, transist::InspectionRunDetail>>,
        chain: &mut Vec<&'a transist::InspectionRunDetail>,
        visited: &mut std::collections::BTreeSet<String>,
        depth: usize,
    ) -> bool {
        const MAX_DEPTH: usize = 16;
        if depth >= MAX_DEPTH {
            return false;
        }
        if !visited.insert(current.summary.run_id.clone()) {
            return false;
        }
        chain.push(current);
        if !current.blocked_gates.is_empty() {
            return true;
        }
        if let Some(map) = map {
            for step in &current.steps {
                let Some(child_id) = step.output_summary.child_run_id.as_deref() else {
                    continue;
                };
                let Some(child) = map.get(child_id) else {
                    continue;
                };
                if walk(child, Some(map), chain, visited, depth + 1) {
                    return true;
                }
            }
        }
        chain.pop();
        false
    }

    let mut chain: Vec<&transist::InspectionRunDetail> = Vec::new();
    let mut visited: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if walk(root, contained_run_details, &mut chain, &mut visited, 0) {
        Some(chain)
    } else {
        None
    }
}

/// Render the innermost gate-owning descendant run at the top of the
/// Details pane. The blocked-gate chain may be many levels deep
/// (`pr_review_loop` → `review_cycle` → `review_changes_flow` →
/// review_plan_loop → review_plan_revision), and the operator only
/// needs to act on the gate's own request — ancestor subflow runs
/// would just push the reviewable files out of view. We therefore
/// render exactly the last entry of `chain`: its per-step workflow
/// files / artifacts / evidence (latest execution, since
/// `output_summary` already reflects only the latest exec), plus the
/// blocked gate's request summary and decision history.
fn append_gate_chain_detail_lines(
    lines: &mut Vec<String>,
    home_dir: Option<&Path>,
    chain: &[&transist::InspectionRunDetail],
) {
    let Some(detail) = chain.last() else {
        return;
    };

    // Resolve the lineage root run dir once for `run://` path expansion.
    let run_root_dir: Option<PathBuf> =
        home_dir.and_then(|h| lineage_root_dir_for_run(h, &detail.summary.run_id));
    let run_root_dir_ref: Option<&Path> = run_root_dir.as_deref();

    lines.push(String::new());
    lines.push(format!(
        "Gate run: {}  status: {}",
        detail.summary.run_id, detail.summary.status
    ));

    // Blocked gate block first — the operator needs to act on this before
    // anything else. Request summary + decision history surface the context
    // the workflow handed to the gate so the operator does not have to leave
    // the TUI to pull it from `runs show --json`. Decision actions are in the
    // sticky Actions pane at the bottom (see `build_run_detail_actions`).
    for gate in &detail.blocked_gates {
        lines.push(String::new());
        lines.push(format!(
            "  Human gate: {}  step: {}  status: {}",
            gate.gate_id, gate.step_id, gate.status
        ));
        let mut gate_lines: Vec<String> = Vec::new();
        append_gate_request_summary_lines(&mut gate_lines, &gate.request_summary, run_root_dir_ref);
        append_gate_decision_history_lines(&mut gate_lines, &gate.decision_history);
        for line in gate_lines {
            lines.push(format!("  {}", line));
        }
    }

    // Per-step outputs in the gate-owning run. Skip steps that
    // produced nothing reviewable so the block stays compact.
    for step in &detail.steps {
        if !step_has_reviewable_detail(step) {
            continue;
        }
        lines.push(String::new());
        lines.push(format!("  Step: {}  status: {}", step.step_id, step.status));
        let mut step_lines: Vec<String> = Vec::new();
        append_step_last_result_line(&mut step_lines, &step.output_summary);
        append_step_failure_detail_lines(&mut step_lines, &step.output_summary);
        append_step_output_keys_line(&mut step_lines, &step.output_summary);
        append_step_usage_line(&mut step_lines, &step.output_summary);
        append_workflow_file_detail_lines(&mut step_lines, &step.output_summary.workflow_files);
        append_blob_ref_detail_lines(
            &mut step_lines,
            "Artifacts",
            home_dir,
            &detail.summary.run_id,
            BlobFamily::Artifact,
            &step.output_summary.artifact_refs,
        );
        append_blob_ref_detail_lines(
            &mut step_lines,
            "Evidence",
            home_dir,
            &detail.summary.run_id,
            BlobFamily::Evidence,
            &step.output_summary.evidence_refs,
        );
        for line in step_lines {
            lines.push(format!("  {}", line));
        }
    }

    // Breadcrumb: the chain's outer levels are intentionally hidden so
    // the operator's eye lands on reviewable files first. Surface the
    // path so they know how to navigate to ancestor runs if needed.
    if chain.len() > 1 {
        let path: Vec<&str> = chain.iter().map(|d| d.summary.run_id.as_str()).collect();
        lines.push(String::new());
        lines.push(format!("  (gate chain: {})", path.join(" → ")));
    }
}

/// Render a child subflow run's reviewable surface generically: aggregate
/// stats (including wall-clock elapsed for the child run and summed active
/// child-step time), then per-step workflow files / artifacts / evidence,
/// then any blocked manual gates with their request summaries. The function
/// is workflow-agnostic — every child step contributes uniformly. Empty
/// sections are skipped so noise stays out of the parent's Details pane.
///
/// Recurses into grandchild subflows whose details are already loaded in
/// `contained_run_details`, so a deeply nested loop step (e.g.
/// `pr_review_loop → review_cycle → review_changes_flow → …`) surfaces
/// every descendant step's artifacts and any blocked manual gate without
/// the operator having to drill into intermediate runs. `visited` guards
/// against pathological cycles in the contained-run graph; `depth` is used
/// only to bound runaway nesting (we cap at 8 levels — far deeper than any
/// realistic workflow — to keep a corrupt run from blowing the pane).
fn append_subflow_detail_lines(
    lines: &mut Vec<String>,
    home_dir: Option<&Path>,
    child_detail: &transist::InspectionRunDetail,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    visited: &mut std::collections::BTreeSet<String>,
    depth: usize,
) {
    const MAX_SUBFLOW_DEPTH: usize = 8;
    if depth >= MAX_SUBFLOW_DEPTH {
        return;
    }
    if !visited.insert(child_detail.summary.run_id.clone()) {
        return;
    }
    lines.push(String::new());
    lines.push(format!(
        "Subflow: {}  status: {}",
        child_detail.summary.run_id, child_detail.summary.status
    ));

    // Aggregate stats summed across every child step. These let the operator
    // see, at a parent-step glance, how much work the subflow produced
    // without having to scroll through every child step body.
    let mut total_workflow_files: usize = 0;
    let mut total_artifacts: usize = 0;
    let mut total_evidence: usize = 0;
    let mut total_artifact_bytes: u64 = 0;
    let mut total_evidence_bytes: u64 = 0;
    let mut total_workflow_file_bytes: u64 = 0;
    let mut status_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut total_elapsed_ms: u64 = 0;
    for step in &child_detail.steps {
        total_workflow_files += step.output_summary.workflow_files.len();
        total_artifacts += step.output_summary.artifact_refs.len();
        total_evidence += step.output_summary.evidence_refs.len();
        for file in &step.output_summary.workflow_files {
            total_workflow_file_bytes = total_workflow_file_bytes.saturating_add(file.bytes);
        }
        for blob in &step.output_summary.artifact_refs {
            total_artifact_bytes = total_artifact_bytes.saturating_add(blob.bytes);
        }
        for blob in &step.output_summary.evidence_refs {
            total_evidence_bytes = total_evidence_bytes.saturating_add(blob.bytes);
        }
        *status_counts.entry(step.status.clone()).or_default() += 1;
        if let (Some(start), Some(end)) = (step.started_at_ms, step.finished_at_ms) {
            if end >= start {
                total_elapsed_ms = total_elapsed_ms.saturating_add(end - start);
            }
        }
    }

    lines.push(format!(
        "  Steps: {}  ({})",
        child_detail.steps.len(),
        format_status_counts(&status_counts)
    ));
    if total_workflow_files > 0 || total_artifacts > 0 || total_evidence > 0 {
        lines.push(format!(
            "  Totals: workflow_files={} ({})  artifacts={} ({})  evidence={} ({})",
            total_workflow_files,
            format_bytes(total_workflow_file_bytes),
            total_artifacts,
            format_bytes(total_artifact_bytes),
            total_evidence,
            format_bytes(total_evidence_bytes),
        ));
    }
    if let (Some(start), Some(end)) = (
        child_detail.summary.started_at_ms,
        child_detail.summary.finished_at_ms,
    ) {
        if end >= start {
            lines.push(format!("  Wall clock: {}", format_duration_ms(end - start)));
        }
    }
    if total_elapsed_ms > 0 {
        lines.push(format!(
            "  Active (sum of child steps): {}",
            format_duration_ms(total_elapsed_ms)
        ));
    }

    // Blocked gates first — these are what the operator must act on.
    let run_root_dir: Option<PathBuf> =
        home_dir.and_then(|h| lineage_root_dir_for_run(h, &child_detail.summary.run_id));
    let run_root_dir_ref: Option<&Path> = run_root_dir.as_deref();
    for gate in &child_detail.blocked_gates {
        lines.push(String::new());
        lines.push(format!(
            "  Human gate: {}  step: {}  status: {}",
            gate.gate_id, gate.step_id, gate.status
        ));
        let mut gate_lines = Vec::new();
        append_gate_request_summary_lines(&mut gate_lines, &gate.request_summary, run_root_dir_ref);
        append_gate_decision_history_lines(&mut gate_lines, &gate.decision_history);
        for line in gate_lines {
            lines.push(format!("  {}", line));
        }
    }

    // Per-step workflow files / artifacts / evidence. Skip steps with no
    // outputs to keep the block compact.
    for step in &child_detail.steps {
        if !step_has_reviewable_detail(step) {
            continue;
        }
        lines.push(String::new());
        let failed_marker = if step_failed(step) { "  FAILED" } else { "" };
        lines.push(format!(
            "  Step: {}  status: {}{}",
            step.step_id, step.status, failed_marker
        ));
        let mut step_lines = Vec::new();
        append_step_last_result_line(&mut step_lines, &step.output_summary);
        append_step_failure_detail_lines(&mut step_lines, &step.output_summary);
        append_step_output_keys_line(&mut step_lines, &step.output_summary);
        append_step_usage_line(&mut step_lines, &step.output_summary);
        append_workflow_file_detail_lines(&mut step_lines, &step.output_summary.workflow_files);
        append_blob_ref_detail_lines(
            &mut step_lines,
            "Artifacts",
            home_dir,
            &child_detail.summary.run_id,
            BlobFamily::Artifact,
            &step.output_summary.artifact_refs,
        );
        append_blob_ref_detail_lines(
            &mut step_lines,
            "Evidence",
            home_dir,
            &child_detail.summary.run_id,
            BlobFamily::Evidence,
            &step.output_summary.evidence_refs,
        );
        for line in step_lines {
            lines.push(format!("  {}", line));
        }
    }

    // Recurse into every grandchild subflow whose detail we have cached.
    // Iterating after the per-step pass keeps the parent's own steps
    // contiguous in the pane; descendant runs render in a separate block
    // each, with the same `Subflow:` header shape so nesting is visible.
    if let Some(map) = contained_run_details {
        for step in &child_detail.steps {
            let Some(grandchild_id) = step.output_summary.child_run_id.as_deref() else {
                continue;
            };
            let Some(grandchild_detail) = map.get(grandchild_id) else {
                continue;
            };
            append_subflow_detail_lines(
                lines,
                home_dir,
                grandchild_detail,
                Some(map),
                visited,
                depth + 1,
            );
        }
    }
}

/// Render a one-line summary of the most recent persisted result for a child
/// step. Combines terminal status, exit code, and any error fields the
/// runtime persisted. Suppressed when no result has been recorded yet so
/// pending steps stay quiet.
fn append_step_last_result_line(
    lines: &mut Vec<String>,
    output_summary: &transist::InspectionStepOutputSummary,
) {
    if !output_summary.result_present {
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(status) = &output_summary.status {
        parts.push(format!("status={}", status));
    }
    if let Some(exit) = output_summary.exit_code {
        parts.push(format!("exit={}", exit));
    }
    if let Some(code) = &output_summary.error_code {
        parts.push(format!("error={}", code));
    }
    if let Some(msg) = &output_summary.error_message {
        // Trim to the first non-empty line so a multi-line stack trace does
        // not dominate the per-step block. Reviewer drills into the child
        // run for the full payload.
        let first_line = msg
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        if !first_line.is_empty() {
            parts.push(format!("msg={}", first_line));
        }
    }
    if parts.is_empty() {
        return;
    }
    lines.push(format!("Last result: {}", parts.join(" ")));
}

fn append_step_failure_detail_lines(
    lines: &mut Vec<String>,
    output_summary: &transist::InspectionStepOutputSummary,
) {
    if let Some(message) = compact_failure_message(output_summary.error_message.as_deref()) {
        lines.push(format!("Error: {}", message));
    }
    if let Some(stderr_tail) = compact_failure_message(output_summary.stderr_tail.as_deref()) {
        lines.push(format!("Stderr: {}", stderr_tail));
    }
}

fn step_has_reviewable_detail(step: &transist::InspectionStep) -> bool {
    !step.output_summary.workflow_files.is_empty()
        || !step.output_summary.artifact_refs.is_empty()
        || !step.output_summary.evidence_refs.is_empty()
        || step.output_summary.result_present
        || !step.output_summary.output_object_keys.is_empty()
        || step_failed(step)
        || compact_failure_message(step.output_summary.error_message.as_deref()).is_some()
        || compact_failure_message(step.output_summary.stderr_tail.as_deref()).is_some()
}

/// Render top-level keys of the most recent persisted output payload as a
/// compact preview. Values themselves are not loaded here (that would
/// require a blob read); knowing the shape is usually enough for a reviewer
/// to decide whether to inspect further. Suppressed when empty.
fn append_step_output_keys_line(
    lines: &mut Vec<String>,
    output_summary: &transist::InspectionStepOutputSummary,
) {
    if output_summary.output_object_keys.is_empty() {
        return;
    }
    lines.push(format!(
        "Output keys: {}",
        output_summary.output_object_keys.join(", ")
    ));
}

/// Surface a usage marker when the persisted output exposes any
/// token/usage-shaped key. We do not load value bytes here — the keys-only
/// marker tells the reviewer that usage data is present and reachable via
/// `runs show --json` or a dedicated inspection command.
fn append_step_usage_line(
    lines: &mut Vec<String>,
    output_summary: &transist::InspectionStepOutputSummary,
) {
    let keys: Vec<&str> = output_summary
        .output_object_keys
        .iter()
        .filter(|key| {
            let lc = key.to_ascii_lowercase();
            lc == "tokens"
                || lc == "usage"
                || lc == "prompt_tokens"
                || lc == "completion_tokens"
                || lc == "total_tokens"
        })
        .map(String::as_str)
        .collect();
    if keys.is_empty() {
        return;
    }
    lines.push(format!("Usage: keys={}", keys.join(",")));
}

/// Format a status -> count map as `success=3 blocked=1 pending=8`. Stable
/// ordering comes from the BTreeMap so the line is deterministic.
fn format_status_counts(counts: &std::collections::BTreeMap<String, usize>) -> String {
    let parts: Vec<String> = counts
        .iter()
        .map(|(status, count)| format!("{}={}", status, count))
        .collect();
    if parts.is_empty() {
        "no steps".to_string()
    } else {
        parts.join(" ")
    }
}

/// Format a byte count with binary units. Truncates to one decimal so the
/// summary stays compact (e.g. `12.4 KiB`, `3.0 MiB`).
fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{} B", bytes)
    }
}

/// Format a duration in milliseconds as `1h 23m 45s` / `12m 03s` / `45s`.
fn format_duration_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{}h {:02}m {:02}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {:02}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

fn detail_failed_in_preflight_without_failed_step(
    run_detail: &transist::InspectionRunDetail,
) -> bool {
    run_detail
        .preflight
        .as_ref()
        .map(|preflight| preflight.status == "failed")
        .unwrap_or(false)
        && latest_step(run_detail, &["failed"]).is_none()
}

fn build_preflight_failure_detail_lines(run_detail: &transist::InspectionRunDetail) -> Vec<String> {
    let Some(preflight) = run_detail.preflight.as_ref() else {
        return Vec::new();
    };

    let mut lines = vec!["── Failure ──".to_string()];
    lines.push("Outcome: preflight failed before step dispatch.".to_string());
    lines.push(format!(
        "Preflight blockers: {} blocker(s), {} warning(s)",
        preflight.blocker_count, preflight.warning_count
    ));
    for check in &preflight.failed_checks {
        lines.push(format_failed_preflight_check_line(check));
    }
    if preflight_has_capability_denial(preflight) {
        lines.push(
            "Remediation: select capabilities to view the missing capability requests.".to_string(),
        );
    }
    lines
}

fn preflight_has_capability_denial(preflight: &transist::InspectionPreflightSummary) -> bool {
    preflight.failed_checks.iter().any(|check| {
        check.message.contains("code=capability_not_granted")
            || check.message.contains("capability_not_granted")
            || check.message.contains("policy.allow.capabilities")
    })
}

fn build_failed_step_detail_lines(
    run_detail: Option<&transist::InspectionRunDetail>,
    step: &transist::InspectionStep,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
) -> Vec<String> {
    if !step_failed(step) {
        return Vec::new();
    }

    let mut lines = vec!["── Failure ──".to_string()];
    if let Some(child_failure) = step.output_summary.child_failure.as_ref() {
        if let Some(failed_step_id) = child_failure.failed_step_id.as_deref() {
            lines.push(format!("Failed child step: {}", failed_step_id));
        }
        lines.push(format!("Child run: {}", child_failure.child_run_id));
        if let Some(error_code) = child_failure.error_code.as_deref() {
            lines.push(format!("Error code: {}", error_code));
        }
        // Compute the child preflight summary once: it drives both the
        // decision to suppress the generic `Error:` line (so it is not
        // duplicated by the preflight block below) and the preflight block
        // rendering itself.
        let child_preflight_summary =
            child_failure_preflight_summary(child_failure, contained_run_details);
        if let Some(error_message) = compact_failure_message(child_failure.error_message.as_deref())
            && child_preflight_summary.is_none()
        {
            lines.push(format!("Error: {}", error_message));
        }
        if let Some(stderr) = compact_failure_message(child_failure.stderr.as_deref()) {
            lines.push(format!("Stderr: {}", stderr));
        }
        if let Some(preflight_summary) = child_preflight_summary {
            lines.push("Error: preflight failed before step dispatch".to_string());
            lines.push(format!(
                "Preflight blockers: {} blocker(s), {} warning(s)",
                preflight_summary.blocker_count, preflight_summary.warning_count
            ));
            for check_line in preflight_summary.failed_check_lines {
                lines.push(check_line);
            }
        }
        return lines;
    }

    lines.push(format!("Failed step: {}", step.step_id));
    if let Some(child_failure) = failed_child_step_summary(step, contained_run_details) {
        if let Some(child_run_id) = step.output_summary.child_run_id.as_deref() {
            lines.push(format!("Child run: {}", child_run_id));
        }
        lines.push(format!("Failed child step: {}", child_failure.step_id));
        if let Some(error_code) = child_failure.output_summary.error_code.as_deref() {
            lines.push(format!("Error code: {}", error_code));
        }
        if let Some(error_message) =
            compact_failure_message(child_failure.output_summary.error_message.as_deref())
        {
            lines.push(format!("Error: {}", error_message));
        }
        if let Some(stderr_tail) =
            compact_failure_message(child_failure.output_summary.stderr_tail.as_deref())
        {
            lines.push(format!("Stderr: {}", stderr_tail));
        }
    } else if let Some(preflight_summary) =
        descendant_preflight_failure_summary(step, contained_run_details)
    {
        // Falling back for pre-Step-1 runs where the kernel has not written a
        // durable `child_failure` yet. Surface the full preflight blocker
        // summary so operators can still see what blocked the descendant run
        // without expanding the raw run detail.
        lines.push(format!("Child run: {}", preflight_summary.run_id));
        lines.push("Error: preflight failed before step dispatch".to_string());
        lines.push(format!(
            "Preflight blockers: {} blocker(s), {} warning(s)",
            preflight_summary.blocker_count, preflight_summary.warning_count
        ));
        for check_line in preflight_summary.failed_check_lines {
            lines.push(check_line);
        }
    } else {
        if let Some(child_run_id) = step.output_summary.child_run_id.as_deref() {
            lines.push(format!("Child run: {}", child_run_id));
        }
        if let Some(error_code) = step.output_summary.error_code.as_deref() {
            lines.push(format!("Error code: {}", error_code));
        }
        if let Some(error_message) =
            compact_failure_message(step.output_summary.error_message.as_deref())
        {
            lines.push(format!("Error: {}", error_message));
        }
        if let Some(stderr_tail) =
            compact_failure_message(step.output_summary.stderr_tail.as_deref())
        {
            lines.push(format!("Stderr: {}", stderr_tail));
        }
        if let Some(detail) = run_detail
            && detail.summary.status == "failed"
            && detail.summary.run_id != detail.summary.root_run_id
        {
            lines.push(format!("Run: {}", detail.summary.run_id));
        }
    }
    lines
}

fn step_failed(step: &transist::InspectionStep) -> bool {
    step.status == "failed" || step.output_summary.status.as_deref() == Some("failed")
}

fn compact_failure_message(message: Option<&str>) -> Option<String> {
    message
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .and_then(|message| {
            message
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })
}

fn failed_child_step_summary<'a>(
    step: &transist::InspectionStep,
    contained_run_details: Option<
        &'a std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
) -> Option<&'a transist::InspectionStep> {
    let child_run_id = step.output_summary.child_run_id.as_deref()?;
    let child_detail = contained_run_details?.get(child_run_id)?;
    let mut visited = std::collections::BTreeSet::new();
    deepest_failed_like_step(child_detail, contained_run_details, &mut visited)
}

struct DescendantPreflightFailureSummary {
    run_id: String,
    blocker_count: usize,
    warning_count: usize,
    failed_check_lines: Vec<String>,
}

fn descendant_preflight_failure_summary(
    step: &transist::InspectionStep,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
) -> Option<DescendantPreflightFailureSummary> {
    let child_run_id = step.output_summary.child_run_id.as_deref()?;
    let map = contained_run_details?;
    let child_detail = map.get(child_run_id)?;
    let mut visited = std::collections::BTreeSet::new();
    find_descendant_preflight_failure(child_detail, map, &mut visited)
}

fn child_failure_preflight_summary(
    child_failure: &transist::InspectionChildFailure,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
) -> Option<DescendantPreflightFailureSummary> {
    if let Some(map) = contained_run_details
        && let Some(child_detail) = map.get(&child_failure.child_run_id)
        && detail_failed_in_preflight_without_failed_step(child_detail)
    {
        let mut visited = std::collections::BTreeSet::new();
        return find_descendant_preflight_failure(child_detail, map, &mut visited);
    }

    if let Some(blocker_count) = child_failure.preflight_blocker_count {
        return Some(DescendantPreflightFailureSummary {
            run_id: child_failure.child_run_id.clone(),
            blocker_count: blocker_count as usize,
            warning_count: child_failure.preflight_warning_count.unwrap_or_default() as usize,
            failed_check_lines: child_failure
                .preflight_first_blocker
                .as_deref()
                .map(|message| vec![message.to_string()])
                .unwrap_or_default(),
        });
    }

    None
}

fn find_descendant_preflight_failure(
    detail: &transist::InspectionRunDetail,
    contained_run_details: &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    visited: &mut std::collections::BTreeSet<String>,
) -> Option<DescendantPreflightFailureSummary> {
    if !visited.insert(detail.summary.run_id.clone()) {
        return None;
    }

    if detail_failed_in_preflight_without_failed_step(detail)
        && let Some(preflight) = detail.preflight.as_ref()
    {
        return Some(DescendantPreflightFailureSummary {
            run_id: detail.summary.run_id.clone(),
            blocker_count: preflight.blocker_count,
            warning_count: preflight.warning_count,
            failed_check_lines: preflight
                .failed_checks
                .iter()
                .map(format_failed_preflight_check_line)
                .collect(),
        });
    }

    for step in &detail.steps {
        if let Some(child_run_id) = step.output_summary.child_run_id.as_deref()
            && let Some(child_detail) = contained_run_details.get(child_run_id)
            && let Some(summary) =
                find_descendant_preflight_failure(child_detail, contained_run_details, visited)
        {
            return Some(summary);
        }
    }

    None
}

fn failed_like_step(detail: &transist::InspectionRunDetail) -> Option<&transist::InspectionStep> {
    detail
        .steps
        .iter()
        .filter(|step| step_failed(step))
        .max_by_key(|step| {
            step.finished_at_ms
                .or(step.started_at_ms)
                .unwrap_or_default()
        })
}

fn deepest_failed_like_step<'a>(
    detail: &'a transist::InspectionRunDetail,
    contained_run_details: Option<
        &'a std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    visited: &mut std::collections::BTreeSet<String>,
) -> Option<&'a transist::InspectionStep> {
    if !visited.insert(detail.summary.run_id.clone()) {
        return failed_like_step(detail);
    }

    let failed_step = failed_like_step(detail)?;
    let Some(child_run_id) = failed_step.output_summary.child_run_id.as_deref() else {
        return Some(failed_step);
    };
    let Some(map) = contained_run_details else {
        return Some(failed_step);
    };
    let Some(child_detail) = map.get(child_run_id) else {
        return Some(failed_step);
    };

    deepest_failed_like_step(child_detail, contained_run_details, visited).or(Some(failed_step))
}

fn hash_preflight_failure_summary(
    hasher: &mut DefaultHasher,
    run_detail: &transist::InspectionRunDetail,
) {
    let Some(preflight) = run_detail.preflight.as_ref() else {
        return;
    };

    preflight.status.hash(hasher);
    preflight.blocker_count.hash(hasher);
    preflight.warning_count.hash(hasher);
    for check in &preflight.failed_checks {
        check.kind.hash(hasher);
        check.message.hash(hasher);
        check.step_ids.hash(hasher);
        check.plugin_id.hash(hasher);
        check.action.hash(hasher);
    }
}

fn hash_child_failure_summary(
    hasher: &mut DefaultHasher,
    step: &transist::InspectionStep,
    child_run_id: &str,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
) {
    if let Some(child_failure) = step.output_summary.child_failure.as_ref() {
        child_failure.child_run_id.hash(hasher);
        child_failure.failed_step_id.hash(hasher);
        child_failure.error_code.hash(hasher);
        child_failure.error_message.hash(hasher);
        child_failure.stderr.hash(hasher);
        child_failure.preflight_blocker_count.hash(hasher);
        child_failure.preflight_warning_count.hash(hasher);
        child_failure.preflight_first_blocker.hash(hasher);
        if let Some(map) = contained_run_details
            && let Some(child_detail) = map.get(&child_failure.child_run_id)
            && detail_failed_in_preflight_without_failed_step(child_detail)
        {
            child_detail.summary.run_id.hash(hasher);
            if let Some(preflight) = child_detail.preflight.as_ref() {
                preflight.blocker_count.hash(hasher);
                preflight.warning_count.hash(hasher);
                for check in &preflight.failed_checks {
                    check.kind.hash(hasher);
                    check.message.hash(hasher);
                    check.step_ids.hash(hasher);
                    check.plugin_id.hash(hasher);
                    check.action.hash(hasher);
                }
            }
        }
        return;
    }
    let Some(map) = contained_run_details else {
        return;
    };
    let Some(child_detail) = map.get(child_run_id) else {
        return;
    };

    child_detail.summary.run_id.hash(hasher);
    child_detail.summary.status.hash(hasher);
    let mut visited = std::collections::BTreeSet::new();
    if let Some(failed_step) =
        deepest_failed_like_step(child_detail, contained_run_details, &mut visited)
    {
        failed_step.step_id.hash(hasher);
        failed_step.output_summary.error_code.hash(hasher);
        failed_step.output_summary.error_message.hash(hasher);
        failed_step.output_summary.stderr_tail.hash(hasher);
    } else if let Some(map) = contained_run_details {
        let mut preflight_visited = std::collections::BTreeSet::new();
        if let Some(summary) =
            find_descendant_preflight_failure(child_detail, map, &mut preflight_visited)
        {
            summary.run_id.hash(hasher);
            summary.blocker_count.hash(hasher);
            summary.warning_count.hash(hasher);
            for line in &summary.failed_check_lines {
                line.hash(hasher);
            }
        }
    }
}

/// When a step was canceled before its plugin completed, the kernel cannot
/// persist workflow files, artifacts, or evidence for that step (the plugin
/// did not reach its finalizer). Surface this explicitly so the empty
/// `Workflow files`, `Artifacts`, and `Evidence` sections are not mistaken
/// for missing data or a UI bug.
fn append_canceled_step_explanation(lines: &mut Vec<String>, step: &transist::InspectionStep) {
    if step.status != "canceled" {
        return;
    }
    let has_outputs = !step.output_summary.workflow_files.is_empty()
        || !step.output_summary.artifact_refs.is_empty()
        || !step.output_summary.evidence_refs.is_empty();
    if has_outputs {
        return;
    }
    lines.push(String::new());
    lines.push(
        "This step was canceled before its plugin completed, so no workflow files,".to_string(),
    );
    lines.push(
        "artifacts, or evidence were persisted. Retry the run to re-run from this step."
            .to_string(),
    );
}

fn append_workflow_file_detail_lines(
    lines: &mut Vec<String>,
    workflow_files: &[transist::InspectionWorkflowFileRef],
) {
    if workflow_files.is_empty() {
        return;
    }

    lines.push(String::new());
    lines.push("Workflow files:".to_string());

    // Group by `field_name` so reviewers can scan files by the role they
    // play in the step (e.g. all `proposal_path` files together). Insertion
    // order is preserved within each group; group order follows the first
    // appearance of each `field_name` in the input slice so the rendering
    // is stable across reruns.
    let mut group_order: Vec<String> = Vec::new();
    let mut groups: std::collections::BTreeMap<String, Vec<&transist::InspectionWorkflowFileRef>> =
        std::collections::BTreeMap::new();
    for file in workflow_files {
        if !groups.contains_key(&file.field_name) {
            group_order.push(file.field_name.clone());
        }
        groups
            .entry(file.field_name.clone())
            .or_default()
            .push(file);
    }

    // When every workflow file shares a single `field_name`, flatten back to
    // the historical one-row-per-file shape so non-loop / single-role steps
    // do not gain an unnecessary indentation level.
    if group_order.len() <= 1 {
        for file in workflow_files {
            lines.push(format!(
                "  {}: {} ({} bytes)",
                file.field_name,
                workflow_file_detail_label(file),
                file.bytes
            ));
        }
        return;
    }

    for field_name in &group_order {
        let entries = match groups.get(field_name) {
            Some(entries) => entries,
            None => continue,
        };
        lines.push(format!("  {}:", field_name));
        for file in entries {
            lines.push(format!(
                "    {} ({} bytes)",
                workflow_file_detail_label(file),
                file.bytes
            ));
        }
    }
}

/// Formats a validated local file location or an explicitly unavailable label.
fn workflow_file_detail_label(file: &transist::InspectionWorkflowFileRef) -> String {
    crate::display::output::workflow_file_local_path(file)
        .map(|path| format!("'{}'", path.display()))
        .unwrap_or_else(|| format!("{} (path unavailable)", file.name))
}

fn append_blob_ref_detail_lines(
    lines: &mut Vec<String>,
    title: &str,
    home_dir: Option<&Path>,
    run_id: &str,
    family: BlobFamily,
    refs: &[transist::InspectionStepBlobRef],
) {
    if refs.is_empty() {
        return;
    }

    let resolved_paths = home_dir
        .map(|home_dir| blob_refs_to_paths(home_dir, run_id, family, refs))
        .unwrap_or_default();
    let paths: Vec<String> = refs
        .iter()
        .enumerate()
        .map(|(index, blob_ref)| {
            resolved_paths
                .get(index)
                .filter(|path| Path::new(path).is_absolute())
                .map(|path| format!("'{}'", path))
                .unwrap_or_else(|| format!("{} (path unavailable)", blob_ref.name))
        })
        .collect();

    // Group rows by their owning execution. For loop steps that re-execute
    // across iterations, this surfaces the per-iteration directory layout
    // (`execution_001`, `execution_002`, …) instead of one flat list that
    // mixes outputs across cycles. For non-loop steps with a single
    // execution, fall back to the historical flat shape.
    let mut execution_order: Vec<u32> = Vec::new();
    let mut groups: std::collections::BTreeMap<u32, Vec<usize>> = std::collections::BTreeMap::new();
    for (idx, blob_ref) in refs.iter().enumerate() {
        if !groups.contains_key(&blob_ref.execution) {
            execution_order.push(blob_ref.execution);
        }
        groups.entry(blob_ref.execution).or_default().push(idx);
    }
    execution_order.sort_unstable();

    lines.push(String::new());

    if execution_order.len() <= 1 {
        // Single execution: keep the historical one-line-per-file shape so
        // simple steps don't get an extra indentation level.
        lines.push(format!("{}:", title));
        for (blob_ref, path) in refs.iter().zip(paths.iter()) {
            lines.push(format!("  {} ({} bytes)", path, blob_ref.bytes));
        }
        return;
    }

    // Multi-execution (typically a loop step): emit a counts header, then
    // one indented sub-section per execution so reviewers can isolate the
    // outputs of a specific iteration without scrolling a flat list.
    let total_files = refs.len();
    let total_bytes: u64 = refs.iter().map(|blob_ref| blob_ref.bytes).sum();
    lines.push(format!(
        "{}: {} files across {} executions ({})",
        title,
        total_files,
        execution_order.len(),
        format_bytes(total_bytes),
    ));
    for execution in &execution_order {
        let indices = match groups.get(execution) {
            Some(indices) => indices,
            None => continue,
        };
        let group_bytes: u64 = indices.iter().map(|&idx| refs[idx].bytes).sum();
        lines.push(format!(
            "  execution_{:03}: {} files ({})",
            execution,
            indices.len(),
            format_bytes(group_bytes),
        ));
        for &idx in indices {
            lines.push(format!("    {} ({} bytes)", paths[idx], refs[idx].bytes));
        }
    }
}

fn append_gate_detail_lines(
    lines: &mut Vec<String>,
    home_dir: Option<&Path>,
    run_detail: &transist::InspectionRunDetail,
    step: &transist::InspectionStep,
) {
    let matching_gates = run_detail
        .blocked_gates
        .iter()
        .filter(|gate| gate.step_id == step.step_id)
        .collect::<Vec<_>>();
    if matching_gates.is_empty() {
        return;
    }

    let root_run_id = run_detail.summary.root_run_id.as_str();
    let run_root_dir: Option<PathBuf> =
        home_dir.and_then(|h| lineage_root_dir_for_run(h, &run_detail.summary.run_id));
    let run_root_dir_ref: Option<&Path> = run_root_dir.as_deref();
    // No descendant gate-run id here — this path runs when the blocked
    // gate is at the run we're viewing; `gate_suggested_actions`
    // collapses the duplicate `runs show` when gate_run_id equals
    // root_run_id, so passing None keeps the action list compact.
    let gate_actor = home_dir
        .and_then(|home| transist::resolve_gate_actor_identity_in_home(home).ok())
        .flatten();
    let suggested_gate_commands = gate_suggested_actions(root_run_id, None, gate_actor.as_deref())
        .into_iter()
        .map(|action| action.display_command)
        .collect::<Vec<_>>();

    for gate in matching_gates {
        lines.push(String::new());
        lines.push(format!("Human gate: {}", gate.gate_id));
        // Surface the gate's `request_summary` so reviewers see what the
        // workflow handed to the gate (path-shaped strings like file
        // attachments, plus any structured context fields). The summary is a
        // BTreeMap<String, KernelValue> persisted by the runtime when the
        // gate was created; rendering it here means the operator does not
        // have to leave the TUI to pull it from `runs show --json`.
        append_gate_request_summary_lines(lines, &gate.request_summary, run_root_dir_ref);
        append_gate_decision_history_lines(lines, &gate.decision_history);
        lines.push("  Next:".to_string());
        for command in &suggested_gate_commands {
            lines.push(format!("    {}", command));
        }
        if let Some(decision) = gate.decision.as_ref() {
            lines.push("  Continued with command:".to_string());
            lines.push(format!(
                "    {}",
                selected_gate_command(root_run_id, decision)
            ));
        }
    }
}

/// Append a flattened, indented rendering of a gate's request summary so the
/// reviewer sees the structured context the workflow handed to the gate.
///
/// Strings (typically file paths or short identifiers) render verbatim on a
/// single line. Arrays expand into one entry per line with `[i]` indices.
/// Objects expand into `key: value` pairs. Scalars (bool/number/null) render
/// inline. Output is bounded to keep extreme summaries from dominating the
/// pane; a `…` continuation marker is emitted when truncation occurs.
/// Render the chronological history of a manual gate's recorded decisions
/// (oldest → newest) so reviewers can see why an earlier reviewer asked for
/// changes before the gate re-entered `blocked`. Empty when no decision has
/// ever been recorded against the gate.
fn append_gate_decision_history_lines(
    lines: &mut Vec<String>,
    decision_history: &[transist::InspectionGateDecision],
) {
    if decision_history.is_empty() {
        return;
    }
    lines.push("  Decisions:".to_string());
    for entry in decision_history {
        let when = entry
            .decided_at_ms
            .map(format_unix_ms_iso8601)
            .unwrap_or_else(|| "unknown-time".to_string());
        let actor = entry.actor.as_deref().unwrap_or("unknown-actor");
        let reason = entry.reason.as_deref().unwrap_or("(no reason recorded)");
        lines.push(format!(
            "    - {} by {} @ {} — {}",
            entry.state, actor, when, reason
        ));
    }
}

/// Resolve a `run://` URL path to an absolute on-disk path using the lineage
/// root run directory. Falls back to the raw string when the home dir is
/// unavailable or the path does not start with the scheme prefix.
fn resolve_run_url_path(raw: &str, run_root_dir: Option<&Path>) -> String {
    let Some(rest) = raw.strip_prefix("run://") else {
        return raw.to_string();
    };
    let Some(root_dir) = run_root_dir else {
        return raw.to_string();
    };
    root_dir
        .join(rest.trim_start_matches('/'))
        .display()
        .to_string()
}

/// Strip UI-only failure prefixes that some tools include before a mergeability
/// diagnostic line (e.g. `X Pull request org/repo#123 is not mergeable: ...`).
///
/// The Details pane already renders this line inside a failed/blocked gate
/// context, so keeping the leading marker is redundant visual noise.
fn strip_mergeability_failure_marker(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("X ") else {
        return raw;
    };
    if rest.starts_with("Pull request ") && rest.contains(" is not mergeable:") {
        rest
    } else {
        raw
    }
}

/// Compute the lineage root run directory for a given run id and home dir.
/// Returns `None` when the run root cannot be resolved (e.g. run not found).
fn lineage_root_dir_for_run(home_dir: &Path, run_id: &str) -> Option<PathBuf> {
    let run_root = TransistContext::with_home(home_dir, home_dir)
        .run_root(run_id)
        .ok()?;
    run_root
        .ancestors()
        .find(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.starts_with("run_"))
                && candidate.parent().and_then(Path::file_name)
                    == Some(std::ffi::OsStr::new("runs"))
        })
        .map(Path::to_path_buf)
}

fn append_gate_request_summary_lines(
    lines: &mut Vec<String>,
    request_summary: &std::collections::BTreeMap<String, KernelValue>,
    run_root_dir: Option<&Path>,
) {
    if request_summary.is_empty() {
        return;
    }
    lines.push("  Gate request:".to_string());
    // Cap is generous enough to fit typical review payloads (file lists,
    // diff summaries) without ballooning when a workflow stuffs an entire
    // document tree into the request.
    const MAX_LINES: usize = 64;
    let mut budget = MAX_LINES;
    for (key, value) in request_summary {
        if key == "review_request" {
            if let Ok(request) = serde_json::from_value::<transist::GateReviewRequest>(
                serde_json::Value::from(value.clone()),
            ) {
                lines.push(format!(
                    "    Review documents: {} (request execution {})",
                    request.review_documents.len(),
                    request.execution
                ));
                for revision in request.review_documents {
                    let content = format!(
                        "/api/v1/runs/{}/documents/{}/content",
                        request.run_id, revision.revision_id
                    );
                    lines.push(format!(
                        "      {}: {} bytes, {} files",
                        revision.document, revision.size_bytes, revision.file_count
                    ));
                    lines.push(format!(
                        "        Revision: {}; SHA-256: {}",
                        revision.revision_id, revision.blob.sha256
                    ));
                    lines.push(format!("        Content: {content}"));
                }
            }
            continue;
        }
        if budget == 0 {
            break;
        }
        append_gate_request_summary_value_lines(lines, key, value, 2, &mut budget, run_root_dir);
    }
    if budget == 0 {
        lines.push("    …".to_string());
    }
}

fn append_gate_request_summary_value_lines(
    lines: &mut Vec<String>,
    key: &str,
    value: &KernelValue,
    depth: usize,
    budget: &mut usize,
    run_root_dir: Option<&Path>,
) {
    if *budget == 0 {
        return;
    }
    let indent = "  ".repeat(depth);
    match value {
        KernelValue::Null => {
            lines.push(format!("{}{}: null", indent, key));
            *budget -= 1;
        }
        KernelValue::Bool(b) => {
            lines.push(format!("{}{}: {}", indent, key, b));
            *budget -= 1;
        }
        KernelValue::Number(n) => {
            lines.push(format!("{}{}: {}", indent, key, n));
            *budget -= 1;
        }
        KernelValue::String(s) => {
            let s = strip_mergeability_failure_marker(s);
            // Resolve `run://` paths to absolute on-disk paths so the
            // operator can open them without manual translation.
            // Absolute paths are single-quoted so `extract_openable_path`
            // can tag them as openable for the `o` / `e` key.
            let resolved = resolve_run_url_path(s, run_root_dir);
            let display = if Path::new(&resolved).is_absolute() {
                format!("'{}'", resolved)
            } else {
                resolved
            };
            lines.push(format!("{}{}: {}", indent, key, display));
            *budget -= 1;
        }
        KernelValue::Array(items) => {
            if items.is_empty() {
                lines.push(format!("{}{}: []", indent, key));
                *budget -= 1;
                return;
            }
            lines.push(format!("{}{}:", indent, key));
            *budget -= 1;
            for (idx, item) in items.iter().enumerate() {
                if *budget == 0 {
                    break;
                }
                let label = format!("[{}]", idx);
                append_gate_request_summary_value_lines(
                    lines,
                    &label,
                    item,
                    depth + 1,
                    budget,
                    run_root_dir,
                );
            }
        }
        KernelValue::Object(map) => {
            if map.is_empty() {
                lines.push(format!("{}{}: {{}}", indent, key));
                *budget -= 1;
                return;
            }
            lines.push(format!("{}{}:", indent, key));
            *budget -= 1;
            for (sub_key, sub_value) in map {
                if *budget == 0 {
                    break;
                }
                append_gate_request_summary_value_lines(
                    lines,
                    sub_key,
                    sub_value,
                    depth + 1,
                    budget,
                    run_root_dir,
                );
            }
        }
    }
}

fn selected_gate_command(root_run_id: &str, decision: &InspectionGateDecision) -> String {
    match decision.state.as_str() {
        "approved" => SuggestedActionExecution::Approve {
            run_id: root_run_id.to_string(),
            gate_id: None,
            actor: decision.actor.clone(),
            reason: decision.reason.clone(),
        }
        .render_command(),
        "rejected" => SuggestedActionExecution::Reject {
            run_id: root_run_id.to_string(),
            gate_id: None,
            actor: decision.actor.clone(),
            reason: decision.reason.clone(),
        }
        .render_command(),
        "request_changes" => SuggestedActionExecution::RequestChanges {
            run_id: root_run_id.to_string(),
            gate_id: None,
            actor: decision.actor.clone(),
            reason: decision.reason.clone(),
        }
        .render_command(),
        other => other.to_string(),
    }
}

pub(super) fn numbered_run_detail_command_items(actions: &[SuggestedAction]) -> Vec<String> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| format!("{}. {}", index + 1, run_detail_command_label(action)))
        .collect()
}

fn run_detail_command_label(action: &SuggestedAction) -> String {
    match &action.execution {
        SuggestedActionExecution::InspectFailedStepDetails { run_id, .. } => {
            format_runs_show_command(run_id, crate::display::summary::RunsShowStepFilter::Failed)
        }
        SuggestedActionExecution::InspectFailedStepEvents { run_id, limit, .. } => {
            format!("transist runs tail {} --limit {}", run_id, limit)
        }
        SuggestedActionExecution::InspectFailedStepStderr {
            artifact_relative_path,
            ..
        } => format!("inspect stderr artifact ({})", artifact_relative_path),
        execution => execution.render_command(),
    }
}

pub(super) fn focus_run_detail_actions(prompt: &mut FullscreenPrompt) {
    let has_actions = prompt
        .picker_state
        .as_ref()
        .map(|picker| !picker.items.is_empty())
        .unwrap_or(false);
    if let Some(state) = prompt.run_detail_state.as_mut() {
        state.focused_section = if has_actions {
            RunDetailSection::Actions
        } else {
            RunDetailSection::Details
        };
    }
}

#[cfg(test)]
mod confirmation_copy_tests {
    use super::{
        run_cleanup_confirm_message, run_cleanup_confirm_title, run_lease_break_confirm_message,
        run_lease_break_confirm_title,
    };
    use crate::display::summary::SuggestedActionExecution;

    #[test]
    fn approve_confirmation_copy_warns_that_the_run_will_resume() {
        let action = SuggestedActionExecution::Approve {
            run_id: "demo-run".to_string(),
            gate_id: None,
            actor: None,
            reason: Some("Reviewed output and approved".to_string()),
        };
        let (title, message) = action
            .confirmation_prompt()
            .expect("approve confirmation prompt");

        assert_eq!(title, "Approve and resume run?");
        assert!(
            message.contains("immediately resumes the blocked run"),
            "message={message}"
        );
        assert!(message.ends_with("Proceed?"), "message={message}");
    }

    #[test]
    fn run_cleanup_confirmation_copy_warns_about_destructive_effects() {
        let title = run_cleanup_confirm_title();
        let message = run_cleanup_confirm_message("demo-run");

        assert_eq!(title, "Run cleanup is destructive");
        assert!(
            message.contains("starts detached cleanup"),
            "message={message}"
        );
        assert!(message.contains("non-resumable"), "message={message}");
        assert!(
            message.contains("transist runs cleanup demo-run"),
            "message={message}"
        );
    }

    #[test]
    fn run_lease_break_confirmation_copy_explains_effect_of_approval() {
        let title = run_lease_break_confirm_title();
        let message = run_lease_break_confirm_message("demo-run");

        assert_eq!(title, "Break persisted lease?");
        assert!(
            message.contains("removes the persisted lease file"),
            "message={message}"
        );
        assert!(message.contains("stale or orphaned"), "message={message}");
        assert!(
            message.contains("transist runs lease break demo-run"),
            "message={message}"
        );
    }
}

#[cfg(test)]
mod selected_action_reconciliation_tests {
    use super::reconcile_selected_action;
    use crate::display::summary::{SuggestedAction, SuggestedActionExecution};

    fn action(execution: SuggestedActionExecution) -> SuggestedAction {
        SuggestedAction::new(execution)
    }

    #[test]
    fn preserves_action_identity_when_full_diagnostic_appears_and_disappears() {
        let show = action(SuggestedActionExecution::RunsShow {
            run_id: "run".to_string(),
        });
        let replay = action(SuggestedActionExecution::RunsReplay {
            run_id: "run".to_string(),
        });
        let diagnostic = action(SuggestedActionExecution::ViewFullDiagnostic {
            run_id: "run".to_string(),
        });

        assert_eq!(
            reconcile_selected_action(
                &[show.clone(), replay.clone()],
                1,
                &[diagnostic.clone(), show.clone(), replay.clone()],
            ),
            2
        );
        assert_eq!(
            reconcile_selected_action(
                &[diagnostic, show.clone(), replay.clone()],
                2,
                &[show, replay],
            ),
            1
        );
    }

    #[test]
    fn missing_selected_action_falls_back_to_read_only_action() {
        let diagnostic = action(SuggestedActionExecution::ViewFullDiagnostic {
            run_id: "run".to_string(),
        });
        let replay = action(SuggestedActionExecution::RunsReplay {
            run_id: "run".to_string(),
        });
        let show = action(SuggestedActionExecution::RunsShow {
            run_id: "run".to_string(),
        });

        let refreshed = [replay, show];
        let selected = reconcile_selected_action(&[diagnostic], 0, &refreshed);

        assert_eq!(selected, 1);
        assert!(!refreshed[selected].execution.is_mutating());
    }
}

#[cfg(test)]
mod post_gate_decision_focus_tests {
    use super::{
        PostGateDecisionFocus, gate_decision_action_requests_details_focus,
        post_gate_decision_focus,
    };
    use crate::display::summary::SuggestedActionExecution;

    #[test]
    fn gate_decision_actions_request_details_focus() {
        assert!(gate_decision_action_requests_details_focus(
            &SuggestedActionExecution::Approve {
                run_id: "run".to_string(),
                gate_id: None,
                actor: None,
                reason: Some("Reviewed output and approved".to_string()),
            }
        ));
        assert!(gate_decision_action_requests_details_focus(
            &SuggestedActionExecution::Reject {
                run_id: "run".to_string(),
                gate_id: None,
                actor: None,
                reason: None,
            }
        ));
        assert!(gate_decision_action_requests_details_focus(
            &SuggestedActionExecution::RequestChanges {
                run_id: "run".to_string(),
                gate_id: None,
                actor: None,
                reason: None,
            }
        ));
        assert!(!gate_decision_action_requests_details_focus(
            &SuggestedActionExecution::RunsShow {
                run_id: "run".to_string(),
            }
        ));
    }

    #[test]
    fn post_gate_decision_focus_waits_for_live_refresh_then_clears() {
        assert_eq!(
            post_gate_decision_focus("created"),
            PostGateDecisionFocus::KeepPending
        );
        assert_eq!(
            post_gate_decision_focus("planned"),
            PostGateDecisionFocus::KeepPending
        );
        assert_eq!(
            post_gate_decision_focus("running"),
            PostGateDecisionFocus::Apply
        );
        assert_eq!(
            post_gate_decision_focus("waiting"),
            PostGateDecisionFocus::Apply
        );
        assert_eq!(
            post_gate_decision_focus("blocked"),
            PostGateDecisionFocus::Clear
        );
        assert_eq!(
            post_gate_decision_focus("failed"),
            PostGateDecisionFocus::Clear
        );
    }
}

#[cfg(test)]
mod wrap_detail_pane_tests {
    use super::wrap_detail_pane_line;

    #[test]
    fn short_line_returns_unchanged() {
        assert_eq!(
            wrap_detail_pane_line("  hello", 80),
            vec!["  hello".to_string()]
        );
    }

    #[test]
    fn long_path_wraps_at_slash_boundaries() {
        let line = "  workflow.spec_path: '/tmp/very/long/path/to/spec.md' (123 bytes)";
        let wrapped = wrap_detail_pane_line(line, 30);
        assert!(
            wrapped.len() > 1,
            "expected multi-line output, got {:?}",
            wrapped
        );
        for fragment in &wrapped {
            assert!(
                fragment.chars().count() <= 30,
                "fragment exceeds width: {:?}",
                fragment
            );
        }
        let joined: String = wrapped
            .iter()
            .flat_map(|s| {
                // Strip continuation indentation (any whitespace) when re-joining.
                s.chars().filter(|c| !c.is_ascii_whitespace())
            })
            .collect();
        let expected: String = line.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        assert_eq!(
            joined, expected,
            "wrapped fragments should reconstruct original ignoring whitespace"
        );
    }

    #[test]
    fn very_narrow_width_hard_wraps_long_segment() {
        let line = "  '/abcdefghijklmnopqrstuvwxyz' (1 bytes)";
        let wrapped = wrap_detail_pane_line(line, 8);
        for fragment in &wrapped {
            assert!(fragment.chars().count() <= 8, "{:?} exceeds 8", fragment);
        }
        assert!(wrapped.len() >= 4);
    }

    #[test]
    fn non_path_line_falls_back_to_plain_wrap() {
        let wrapped = wrap_detail_pane_line("    Workflow files:", 8);
        assert_eq!(
            wrapped,
            vec![
                "    Work".to_string(),
                "    flow".to_string(),
                "     fil".to_string(),
                "    es:".to_string(),
            ]
        );
    }

    #[test]
    fn artifact_row_wraps_and_keeps_byte_suffix() {
        let line = "    '/abs/path/to/artifact/output.json' (456 bytes)";
        let wrapped = wrap_detail_pane_line(line, 24);
        for fragment in &wrapped {
            assert!(fragment.chars().count() <= 24, "{:?}", fragment);
        }
        let joined = wrapped.join(" ");
        assert!(joined.contains("(456 bytes)"));
        assert!(joined.contains("output.json"));
    }
}

#[cfg(test)]
mod effective_run_status_tests {
    use super::resolve_effective_run_status;
    use crate::actionable::ActionableRun;
    use crate::display::progress::ProgressState;
    use crate::display::spinner::LiveProgress;
    use transist::{InspectionRunDetail, InspectionRunSummary};

    fn detail(run_id: &str, status: &str, root_run_id: &str) -> InspectionRunDetail {
        InspectionRunDetail {
            summary: InspectionRunSummary {
                run_id: run_id.to_string(),
                workflow_id: "wf".to_string(),
                workflow_version: "v1".to_string(),
                status: status.to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                started_at_ms: None,
                finished_at_ms: None,
                current_step_id: None,
                has_blocked_gates: false,
                has_artifacts: false,
                has_events: false,
                artifact_count: 0,
                evidence_count: 0,
                root_run_id: root_run_id.to_string(),
                parent_run_id: None,
            },
            workflow_inputs: Default::default(),
            global_default_filesystem_policy: None,
            workflow_filesystem_policy: None,
            current_step: None,
            actionable_run_id: None,
            terminal_outcome: None,
            recovery_state: None,
            preflight: None,
            cleanup: None,
            active_lease: None,
            blocked_gates: Vec::new(),
            steps: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            event_count: 0,
            latest_event_id: None,
            lineage: None,
            contained_runs: Vec::new(),
            step_counts: Default::default(),
            trusted_execution_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            trust: transist::InspectionTrustSummary {
                trusted_execution_mode: false,
                workflow: None,
                plugins: Vec::new(),
                decision: None,
                policy_decision: None,
            },
            locality: transist::InspectionLocalityView {
                steps: Vec::new(),
                external_transfers: Vec::new(),
            },
            evidence_export: transist::InspectionEvidenceExportView {
                location: None,
                command: "transist runs evidence export run-1".to_string(),
                format: "markdown".to_string(),
                generated_at_ms: None,
            },
        }
    }

    fn progress(
        run_status: &str,
        actionable_run_id: &str,
        current_step_status: &str,
    ) -> LiveProgress {
        LiveProgress {
            state: ProgressState {
                root_run_id: "root".to_string(),
                run_status: run_status.to_string(),
                actionable_run_id: actionable_run_id.to_string(),
                top_level_step_id: None,
                current_step_path: None,
                current_step_status: Some(current_step_status.to_string()),
                execution: None,
                cycle: None,
                tool_name: None,
                started_at_ms: None,
                finished_at_ms: None,
                error_code: None,
                exit_code: None,
                checklist: Vec::new(),
            },
            preview_stream: None,
            preview_text: None,
            attached_step_output_blocks: Vec::new(),
            footer_lines: Vec::new(),
        }
    }

    #[test]
    fn terminal_root_status_wins_over_descendant_running_status() {
        // When the root has settled into a terminal state, the kernel will
        // refuse any mutating action against the run tree. The TUI must
        // surface the root status so the action picker can offer recovery
        // actions (resume/cleanup/replay) instead of normal-step actions
        // routed at a descendant whose dispatch will be immediately rejected.
        let root = detail("root", "canceled", "root");
        let actionable = ActionableRun {
            root_run_id: "root".to_string(),
            run_id: "root__child".to_string(),
            detail: detail("root__child", "running", "root"),
        };
        let live_progress = progress("canceled", "root__child", "dispatching");

        assert_eq!(
            resolve_effective_run_status(&root, &actionable, Some(&live_progress)),
            "canceled"
        );
    }

    #[test]
    fn descendant_running_status_overrides_stale_root_status_when_root_is_non_terminal() {
        // During detached resume hand-off the root summary can briefly lag
        // behind the actionable child. While the root is still non-terminal
        // we prefer the descendant's status so the operator sees the actual
        // running state instead of the stale summary.
        let root = detail("root", "planned", "root");
        let actionable = ActionableRun {
            root_run_id: "root".to_string(),
            run_id: "root__child".to_string(),
            detail: detail("root__child", "running", "root"),
        };
        let live_progress = progress("planned", "root__child", "dispatching");

        assert_eq!(
            resolve_effective_run_status(&root, &actionable, Some(&live_progress)),
            "running"
        );
    }

    #[test]
    fn root_progress_status_is_kept_when_actionable_run_is_root() {
        let root = detail("root", "planned", "root");
        let actionable = ActionableRun {
            root_run_id: "root".to_string(),
            run_id: "root".to_string(),
            detail: detail("root", "planned", "root"),
        };
        let live_progress = progress("running", "root", "running");

        assert_eq!(
            resolve_effective_run_status(&root, &actionable, Some(&live_progress)),
            "running"
        );
    }
}

#[cfg(test)]
mod terminal_fallback_detail_tests {
    use super::{
        build_run_detail_history_lines, build_selected_step_detail_lines_without_progress,
        default_selected_step_for_run_detail,
    };
    use std::collections::BTreeMap;
    use transist::{
        InspectionChildFailure, InspectionPreflightFailedCheck, InspectionPreflightSummary,
        InspectionRunDetail, InspectionRunSummary, InspectionStep, InspectionStepBlobRef,
        InspectionStepInputSummary, InspectionStepOutputSummary, InspectionWorkflowFileRef,
        RunCurrentStep,
    };

    fn sample_detail() -> InspectionRunDetail {
        InspectionRunDetail {
            summary: InspectionRunSummary {
                run_id: "root".to_string(),
                workflow_id: "wf".to_string(),
                workflow_version: "v1".to_string(),
                status: "failed".to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                started_at_ms: Some(1),
                finished_at_ms: Some(2),
                current_step_id: Some("scan".to_string()),
                has_blocked_gates: false,
                has_artifacts: true,
                has_events: false,
                artifact_count: 1,
                evidence_count: 1,
                root_run_id: "root".to_string(),
                parent_run_id: None,
            },
            workflow_inputs: BTreeMap::new(),
            global_default_filesystem_policy: None,
            workflow_filesystem_policy: None,
            current_step: Some(RunCurrentStep {
                step_id: "scan".to_string(),
                status: "failed".to_string(),
            }),
            actionable_run_id: None,
            terminal_outcome: None,
            recovery_state: None,
            preflight: None,
            cleanup: None,
            active_lease: None,
            blocked_gates: Vec::new(),
            steps: vec![
                InspectionStep {
                    step_id: "prepare".to_string(),
                    step_kind: "invoke".to_string(),
                    status: "success".to_string(),
                    execution: Some(1),
                    started_at_ms: Some(10),
                    finished_at_ms: Some(20),
                    input_summary: InspectionStepInputSummary {
                        request_present: false,
                        invoke_id: None,
                        adapter_kind: None,
                        plugin_id: None,
                        action: None,
                        input_object_keys: vec![],
                        metadata_object_keys: vec![],
                        filesystem_policy: None,
                    },
                    output_summary: InspectionStepOutputSummary {
                        result_present: false,
                        invoke_id: None,
                        status: None,
                        exit_code: None,
                        error_code: None,
                        error_message: None,
                        stderr_tail: None,
                        child_run_id: None,
                        child_failure: None,
                        output_object_keys: vec![],
                        artifact_count: 0,
                        evidence_count: 0,
                        artifact_refs: vec![],
                        evidence_refs: vec![],
                        workflow_files: vec![],
                    },
                    is_current_active: false,
                    latest_output: None,
                    executions: Vec::new(),
                },
                InspectionStep {
                    step_id: "scan".to_string(),
                    step_kind: "invoke".to_string(),
                    status: "failed".to_string(),
                    execution: Some(2),
                    started_at_ms: Some(30),
                    finished_at_ms: Some(40),
                    input_summary: InspectionStepInputSummary {
                        request_present: true,
                        invoke_id: Some("invoke-1".to_string()),
                        adapter_kind: Some("process".to_string()),
                        plugin_id: Some("shell".to_string()),
                        action: Some("exec".to_string()),
                        input_object_keys: vec![],
                        metadata_object_keys: vec![],
                        filesystem_policy: None,
                    },
                    output_summary: InspectionStepOutputSummary {
                        result_present: true,
                        invoke_id: Some("invoke-1".to_string()),
                        status: Some("failed".to_string()),
                        exit_code: Some(2),
                        error_code: Some("tool_execution_error".to_string()),
                        error_message: Some("command exited with code 2".to_string()),
                        stderr_tail: None,
                        child_run_id: None,
                        child_failure: None,
                        output_object_keys: vec![],
                        artifact_count: 1,
                        evidence_count: 1,
                        artifact_refs: vec![InspectionStepBlobRef {
                            step_id: "scan".to_string(),
                            execution: 2,
                            relative_path: "artifacts/001_stderr.txt".to_string(),
                            name: "001_stderr.txt".to_string(),
                            bytes: 42,
                        }],
                        evidence_refs: vec![InspectionStepBlobRef {
                            step_id: "scan".to_string(),
                            execution: 2,
                            relative_path: "evidence/runtime.json".to_string(),
                            name: "runtime.json".to_string(),
                            bytes: 24,
                        }],
                        workflow_files: vec![InspectionWorkflowFileRef {
                            step_id: "scan".to_string(),
                            field_name: "script".to_string(),
                            locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
                            relative_path: "scan.sh".to_string(),
                            local_path: Some(std::path::PathBuf::from("/tmp/scan.sh")),
                            name: "scan.sh".to_string(),
                            bytes: 11,
                        }],
                    },
                    is_current_active: false,
                    latest_output: None,
                    executions: Vec::new(),
                },
            ],
            artifact_count: 1,
            evidence_count: 1,
            event_count: 0,
            latest_event_id: None,
            lineage: None,
            contained_runs: Vec::new(),
            step_counts: BTreeMap::new(),
            trusted_execution_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            trust: transist::InspectionTrustSummary {
                trusted_execution_mode: false,
                workflow: None,
                plugins: Vec::new(),
                decision: None,
                policy_decision: None,
            },
            locality: transist::InspectionLocalityView {
                steps: Vec::new(),
                external_transfers: Vec::new(),
            },
            evidence_export: transist::InspectionEvidenceExportView {
                location: None,
                command: "transist runs evidence export run-1".to_string(),
                format: "markdown".to_string(),
                generated_at_ms: None,
            },
        }
    }

    #[test]
    fn default_selected_step_prefers_current_failed_step() {
        assert_eq!(default_selected_step_for_run_detail(&sample_detail()), 1);
    }

    #[test]
    fn detail_pane_falls_back_to_run_detail_when_progress_is_missing() {
        let lines =
            build_run_detail_history_lines(None, Some(&sample_detail()), None, None, None, 80, 0)
                .lines;

        assert!(
            lines.iter().any(|line| line.text.contains("── Failure ──")),
            "lines={lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.text.contains("Action: exec")),
            "lines={lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("Step status: failed")),
            "lines={lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("Error code: tool_execution_error")),
            "lines={lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.text.contains("/tmp/scan.sh")),
            "lines={lines:?}"
        );
    }

    #[test]
    fn detail_pane_surfaces_stderr_tail_when_present_on_failed_step() {
        let mut detail = sample_detail();
        // The failed step in sample_detail() is at index 1; attach a stderr
        // tail that differs from the generic error_message wrapper to confirm
        // the Stderr line is rendered as its own diagnostic and not masked by
        // error_message presence.
        detail.steps[1].output_summary.stderr_tail =
            Some("FileNotFoundError: '/tmp/missing.py'".to_string());

        let lines =
            build_run_detail_history_lines(None, Some(&detail), None, None, None, 80, 0).lines;

        assert!(
            lines.iter().any(|line| line
                .text
                .contains("Stderr: FileNotFoundError: '/tmp/missing.py'")),
            "expected Stderr: line to be rendered alongside Error:, lines={lines:?}"
        );
    }

    fn failed_subflow_parent_detail(
        child_failure: Option<InspectionChildFailure>,
        child_run_id: &str,
    ) -> InspectionRunDetail {
        InspectionRunDetail {
            summary: InspectionRunSummary {
                run_id: "root".to_string(),
                workflow_id: "wf".to_string(),
                workflow_version: "v1".to_string(),
                status: "failed".to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                started_at_ms: Some(1),
                finished_at_ms: Some(2),
                current_step_id: Some("child".to_string()),
                has_blocked_gates: false,
                has_artifacts: false,
                has_events: false,
                artifact_count: 0,
                evidence_count: 0,
                root_run_id: "root".to_string(),
                parent_run_id: None,
            },
            workflow_inputs: BTreeMap::new(),
            global_default_filesystem_policy: None,
            workflow_filesystem_policy: None,
            current_step: Some(RunCurrentStep {
                step_id: "child".to_string(),
                status: "failed".to_string(),
            }),
            actionable_run_id: None,
            terminal_outcome: None,
            recovery_state: None,
            preflight: None,
            cleanup: None,
            active_lease: None,
            blocked_gates: Vec::new(),
            steps: vec![InspectionStep {
                step_id: "child".to_string(),
                step_kind: "subflow".to_string(),
                status: "failed".to_string(),
                execution: Some(1),
                started_at_ms: Some(10),
                finished_at_ms: Some(20),
                input_summary: InspectionStepInputSummary {
                    request_present: true,
                    invoke_id: Some("invoke-child".to_string()),
                    adapter_kind: None,
                    plugin_id: None,
                    action: None,
                    input_object_keys: vec![],
                    metadata_object_keys: vec![],
                    filesystem_policy: None,
                },
                output_summary: InspectionStepOutputSummary {
                    result_present: true,
                    invoke_id: Some("invoke-child".to_string()),
                    status: Some("failed".to_string()),
                    exit_code: Some(1),
                    error_code: None,
                    error_message: None,
                    stderr_tail: None,
                    child_run_id: Some(child_run_id.to_string()),
                    child_failure,
                    output_object_keys: vec![],
                    artifact_count: 0,
                    evidence_count: 0,
                    artifact_refs: vec![],
                    evidence_refs: vec![],
                    workflow_files: vec![],
                },
                is_current_active: false,
                latest_output: None,
                executions: Vec::new(),
            }],
            artifact_count: 0,
            evidence_count: 0,
            event_count: 0,
            latest_event_id: None,
            lineage: None,
            contained_runs: Vec::new(),
            step_counts: BTreeMap::from([("failed".to_string(), 1)]),
            trusted_execution_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            trust: transist::InspectionTrustSummary {
                trusted_execution_mode: false,
                workflow: None,
                plugins: Vec::new(),
                decision: None,
                policy_decision: None,
            },
            locality: transist::InspectionLocalityView {
                steps: Vec::new(),
                external_transfers: Vec::new(),
            },
            evidence_export: transist::InspectionEvidenceExportView {
                location: None,
                command: "transist runs evidence export run-1".to_string(),
                format: "markdown".to_string(),
                generated_at_ms: None,
            },
        }
    }

    fn contained_failed_detail(run_id: &str, step_id: &str, status: &str) -> InspectionRunDetail {
        InspectionRunDetail {
            summary: InspectionRunSummary {
                run_id: run_id.to_string(),
                workflow_id: "wf".to_string(),
                workflow_version: "v1".to_string(),
                status: status.to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                started_at_ms: Some(1),
                finished_at_ms: Some(2),
                current_step_id: Some(step_id.to_string()),
                has_blocked_gates: false,
                has_artifacts: false,
                has_events: false,
                artifact_count: 0,
                evidence_count: 0,
                root_run_id: "root".to_string(),
                parent_run_id: Some("root".to_string()),
            },
            workflow_inputs: BTreeMap::new(),
            global_default_filesystem_policy: None,
            workflow_filesystem_policy: None,
            current_step: Some(RunCurrentStep {
                step_id: step_id.to_string(),
                status: status.to_string(),
            }),
            actionable_run_id: None,
            terminal_outcome: None,
            recovery_state: None,
            preflight: None,
            cleanup: None,
            active_lease: None,
            blocked_gates: Vec::new(),
            steps: vec![InspectionStep {
                step_id: step_id.to_string(),
                step_kind: "invoke".to_string(),
                status: status.to_string(),
                execution: Some(1),
                started_at_ms: Some(10),
                finished_at_ms: Some(20),
                input_summary: InspectionStepInputSummary {
                    request_present: true,
                    invoke_id: Some(format!("invoke-{step_id}")),
                    adapter_kind: None,
                    plugin_id: None,
                    action: None,
                    input_object_keys: vec![],
                    metadata_object_keys: vec![],
                    filesystem_policy: None,
                },
                output_summary: InspectionStepOutputSummary {
                    result_present: true,
                    invoke_id: Some(format!("invoke-{step_id}")),
                    status: Some(status.to_string()),
                    exit_code: Some(1),
                    error_code: Some("tool_execution_error".to_string()),
                    error_message: Some("deep error".to_string()),
                    stderr_tail: Some("deep stderr".to_string()),
                    child_run_id: None,
                    child_failure: None,
                    output_object_keys: vec![],
                    artifact_count: 0,
                    evidence_count: 0,
                    artifact_refs: vec![],
                    evidence_refs: vec![],
                    workflow_files: vec![],
                },
                is_current_active: false,
                latest_output: None,
                executions: Vec::new(),
            }],
            artifact_count: 0,
            evidence_count: 0,
            event_count: 0,
            latest_event_id: None,
            lineage: None,
            contained_runs: Vec::new(),
            step_counts: BTreeMap::from([(status.to_string(), 1)]),
            trusted_execution_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            trust: transist::InspectionTrustSummary {
                trusted_execution_mode: false,
                workflow: None,
                plugins: Vec::new(),
                decision: None,
                policy_decision: None,
            },
            locality: transist::InspectionLocalityView {
                steps: Vec::new(),
                external_transfers: Vec::new(),
            },
            evidence_export: transist::InspectionEvidenceExportView {
                location: None,
                command: "transist runs evidence export run-1".to_string(),
                format: "markdown".to_string(),
                generated_at_ms: None,
            },
        }
    }

    #[test]
    fn parent_subflow_failure_block_renders_first_from_child_failure() {
        let detail = failed_subflow_parent_detail(
            Some(InspectionChildFailure {
                child_run_id: "root__child__grand".to_string(),
                failed_step_id: Some("leaf".to_string()),
                error_code: Some("tool_execution_error".to_string()),
                error_message: Some("deep error".to_string()),
                stderr: Some("deep stderr".to_string()),
                preflight_blocker_count: None,
                preflight_warning_count: None,
                preflight_first_blocker: None,
            }),
            "root__child",
        );
        let contained = BTreeMap::from([(
            "root__child".to_string(),
            contained_failed_detail("root__child", "grand", "failed"),
        )]);

        let lines = build_selected_step_detail_lines_without_progress(
            None,
            &detail,
            Some(&contained),
            None,
        );

        assert_eq!(lines.first().map(String::as_str), Some("── Failure ──"));
        assert!(
            lines.iter().any(|line| line == "Failed child step: leaf"),
            "lines={lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "Child run: root__child__grand"),
            "lines={lines:?}"
        );
        assert!(
            lines.iter().any(|line| line == "Error: deep error"),
            "lines={lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("Preflight blockers:")),
            "lines={lines:?}"
        );
        let execution_index = lines
            .iter()
            .position(|line| line == "Execution:")
            .expect("execution header");
        let failure_index = lines
            .iter()
            .position(|line| line == "── Failure ──")
            .expect("failure header");
        assert!(failure_index < execution_index, "lines={lines:?}");
    }

    #[test]
    fn parent_subflow_failure_block_marks_failed_steps_in_subflow_tree() {
        let detail = failed_subflow_parent_detail(
            Some(InspectionChildFailure {
                child_run_id: "root__child__grand".to_string(),
                failed_step_id: Some("leaf".to_string()),
                error_code: Some("tool_execution_error".to_string()),
                error_message: Some("deep error".to_string()),
                stderr: Some("deep stderr".to_string()),
                preflight_blocker_count: None,
                preflight_warning_count: None,
                preflight_first_blocker: None,
            }),
            "root__child",
        );
        let contained = BTreeMap::from([(
            "root__child".to_string(),
            contained_failed_detail("root__child", "grand", "failed"),
        )]);

        let lines = build_selected_step_detail_lines_without_progress(
            None,
            &detail,
            Some(&contained),
            None,
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("Step: grand  status: failed  FAILED")),
            "lines={lines:?}"
        );
    }

    #[test]
    fn parent_subflow_failure_block_falls_back_to_preflight_failure() {
        let mut detail = failed_subflow_parent_detail(None, "root__child");
        detail.steps[0].output_summary.child_failure = None;
        let contained = BTreeMap::from([(
            "root__child".to_string(),
            InspectionRunDetail {
                summary: InspectionRunSummary {
                    run_id: "root__child".to_string(),
                    workflow_id: "wf".to_string(),
                    workflow_version: "v1".to_string(),
                    status: "failed".to_string(),
                    created_at_ms: 1,
                    updated_at_ms: 2,
                    started_at_ms: Some(1),
                    finished_at_ms: Some(2),
                    current_step_id: None,
                    has_blocked_gates: false,
                    has_artifacts: false,
                    has_events: false,
                    artifact_count: 0,
                    evidence_count: 0,
                    root_run_id: "root".to_string(),
                    parent_run_id: Some("root".to_string()),
                },
                workflow_inputs: BTreeMap::new(),
                global_default_filesystem_policy: None,
                workflow_filesystem_policy: None,
                current_step: None,
                actionable_run_id: None,
                terminal_outcome: None,
                recovery_state: None,
                preflight: Some(InspectionPreflightSummary {
                    status: "failed".to_string(),
                    checked_at_ms: 1,
                    blocker_count: 1,
                    warning_count: 0,
                    failed_checks: vec![InspectionPreflightFailedCheck {
                        kind: "policy".to_string(),
                        message: "denied".to_string(),
                        step_ids: vec!["leaf".to_string()],
                        plugin_id: Some("shell".to_string()),
                        action: Some("echo".to_string()),
                    }],
                }),
                cleanup: None,
                active_lease: None,
                blocked_gates: Vec::new(),
                steps: Vec::new(),
                artifact_count: 0,
                evidence_count: 0,
                event_count: 0,
                latest_event_id: None,
                lineage: None,
                contained_runs: Vec::new(),
                step_counts: BTreeMap::new(),
                trusted_execution_mode: false,
                runtime_mode: "unknown".to_string(),
                policy_profile: "unknown".to_string(),
                capability_enforcement: "unknown".to_string(),
                trust: transist::InspectionTrustSummary {
                    trusted_execution_mode: false,
                    workflow: None,
                    plugins: Vec::new(),
                    decision: None,
                    policy_decision: None,
                },
                locality: transist::InspectionLocalityView {
                    steps: Vec::new(),
                    external_transfers: Vec::new(),
                },
                evidence_export: transist::InspectionEvidenceExportView {
                    location: None,
                    command: "transist runs evidence export run-1".to_string(),
                    format: "markdown".to_string(),
                    generated_at_ms: None,
                },
            },
        )]);

        let lines = build_selected_step_detail_lines_without_progress(
            None,
            &detail,
            Some(&contained),
            None,
        );

        assert_eq!(lines.first().map(String::as_str), Some("── Failure ──"));
        assert!(
            lines.iter().any(|line| line == "Child run: root__child"),
            "lines={lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "Error: preflight failed before step dispatch"),
            "lines={lines:?}"
        );
    }
}

#[cfg(test)]
mod run_detail_pane_fingerprint_tests {
    use super::{append_blob_ref_detail_lines, derive_openable_paths, run_detail_pane_fingerprint};
    use crate::display::output::AttachedStepOutputBlock;
    use crate::display::progress::{ChecklistState, ProgressChecklistItem, ProgressState};
    use crate::display::spinner::LiveProgress;
    use std::collections::{BTreeMap, BTreeSet};
    use transist::{
        BlobFamily, InspectionGate, InspectionGateDecision, InspectionPreflightFailedCheck,
        InspectionPreflightSummary, InspectionRunDetail, InspectionRunSummary, InspectionStep,
        InspectionStepBlobRef, InspectionStepInputSummary, InspectionStepOutputSummary,
        InspectionWorkflowFileRef,
    };

    fn sample_summary(run_id: &str) -> InspectionRunSummary {
        InspectionRunSummary {
            run_id: run_id.to_string(),
            workflow_id: "wf".to_string(),
            workflow_version: "v1".to_string(),
            status: "running".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            started_at_ms: Some(1),
            finished_at_ms: None,
            current_step_id: Some("step-a".to_string()),
            has_blocked_gates: false,
            has_artifacts: true,
            has_events: false,
            artifact_count: 1,
            evidence_count: 1,
            root_run_id: run_id.to_string(),
            parent_run_id: None,
        }
    }

    fn sample_step() -> InspectionStep {
        InspectionStep {
            step_id: "step-a".to_string(),
            step_kind: "invoke".to_string(),
            status: "running".to_string(),
            execution: Some(1),
            started_at_ms: Some(10),
            finished_at_ms: Some(20),
            input_summary: InspectionStepInputSummary {
                request_present: true,
                invoke_id: Some("inv-1".to_string()),
                adapter_kind: Some("mcp".to_string()),
                plugin_id: Some("plugin-a".to_string()),
                action: Some("act".to_string()),
                input_object_keys: vec![],
                metadata_object_keys: vec![],
                filesystem_policy: None,
            },
            output_summary: InspectionStepOutputSummary {
                result_present: true,
                invoke_id: Some("inv-1".to_string()),
                status: Some("running".to_string()),
                exit_code: None,
                error_code: None,
                error_message: None,
                stderr_tail: None,
                child_run_id: Some("child-1".to_string()),
                child_failure: None,
                output_object_keys: vec![],
                artifact_count: 1,
                evidence_count: 1,
                artifact_refs: vec![InspectionStepBlobRef {
                    step_id: "step-a".to_string(),
                    execution: 1,
                    relative_path: "art/a.txt".to_string(),
                    name: "a.txt".to_string(),
                    bytes: 11,
                }],
                evidence_refs: vec![InspectionStepBlobRef {
                    step_id: "step-a".to_string(),
                    execution: 1,
                    relative_path: "ev/e.txt".to_string(),
                    name: "e.txt".to_string(),
                    bytes: 12,
                }],
                workflow_files: vec![InspectionWorkflowFileRef {
                    step_id: "step-a".to_string(),
                    field_name: "spec".to_string(),
                    locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
                    relative_path: "spec.md".to_string(),
                    local_path: Some(std::path::PathBuf::from("/tmp/spec.md")),
                    name: "spec.md".to_string(),
                    bytes: 13,
                }],
            },
            is_current_active: true,
            latest_output: None,
            executions: Vec::new(),
        }
    }

    fn sample_gate() -> InspectionGate {
        InspectionGate {
            gate_id: "gate-1".to_string(),
            status: "blocked".to_string(),
            step_id: "step-a".to_string(),
            created_at_ms: Some(10),
            decision: None,
            decision_history: vec![InspectionGateDecision {
                review_request: None,
                state: "request_changes".to_string(),
                actor: Some("alice".to_string()),
                reason: Some("needs update".to_string()),
                decided_at_ms: Some(11),
            }],
            request_summary: BTreeMap::new(),
        }
    }

    fn sample_detail(run_id: &str) -> InspectionRunDetail {
        InspectionRunDetail {
            summary: sample_summary(run_id),
            workflow_inputs: BTreeMap::new(),
            global_default_filesystem_policy: None,
            workflow_filesystem_policy: None,
            current_step: None,
            actionable_run_id: None,
            terminal_outcome: None,
            recovery_state: None,
            preflight: None,
            cleanup: None,
            active_lease: None,
            blocked_gates: vec![sample_gate()],
            steps: vec![sample_step()],
            artifact_count: 1,
            evidence_count: 1,
            event_count: 0,
            latest_event_id: None,
            lineage: None,
            contained_runs: vec![],
            step_counts: BTreeMap::new(),
            trusted_execution_mode: false,
            runtime_mode: "unknown".to_string(),
            policy_profile: "unknown".to_string(),
            capability_enforcement: "unknown".to_string(),
            trust: transist::InspectionTrustSummary {
                trusted_execution_mode: false,
                workflow: None,
                plugins: Vec::new(),
                decision: None,
                policy_decision: None,
            },
            locality: transist::InspectionLocalityView {
                steps: Vec::new(),
                external_transfers: Vec::new(),
            },
            evidence_export: transist::InspectionEvidenceExportView {
                location: None,
                command: "transist runs evidence export run-1".to_string(),
                format: "markdown".to_string(),
                generated_at_ms: None,
            },
        }
    }

    fn sample_progress() -> LiveProgress {
        LiveProgress {
            state: ProgressState {
                root_run_id: "root-1".to_string(),
                run_status: "running".to_string(),
                actionable_run_id: "root-1".to_string(),
                top_level_step_id: Some("step-a".to_string()),
                current_step_path: None,
                current_step_status: Some("running".to_string()),
                execution: Some(1),
                cycle: None,
                tool_name: Some("tool-a".to_string()),
                started_at_ms: Some(10),
                finished_at_ms: None,
                error_code: None,
                exit_code: None,
                checklist: vec![ProgressChecklistItem {
                    step_id: "step-a".to_string(),
                    state: ChecklistState::Active,
                    duration_seconds: None,
                }],
            },
            preview_stream: None,
            preview_text: Some("hello".to_string()),
            attached_step_output_blocks: vec![AttachedStepOutputBlock {
                lines: vec!["line-a".to_string()],
                anchor_step_id: Some("step-a".to_string()),
            }],
            footer_lines: vec![],
        }
    }

    fn sample_contained() -> BTreeMap<String, InspectionRunDetail> {
        let mut child = sample_detail("child-1");
        child.blocked_gates = vec![sample_gate()];
        let mut map = BTreeMap::new();
        map.insert("child-1".to_string(), child);
        map
    }

    fn fp(
        detail: &InspectionRunDetail,
        contained: &BTreeMap<String, InspectionRunDetail>,
        progress: &LiveProgress,
        width: usize,
    ) -> u64 {
        run_detail_pane_fingerprint(
            Some(detail),
            Some(contained),
            Some(progress),
            Some(0),
            width,
        )
    }

    #[test]
    fn identical_inputs_produce_identical_fingerprint() {
        let detail = sample_detail("root-1");
        let contained = sample_contained();
        let progress = sample_progress();
        assert_eq!(
            fp(&detail, &contained, &progress, 80),
            fp(&detail, &contained, &progress, 80)
        );
    }

    #[test]
    fn local_workflow_file_locations_invalidate_live_and_snapshot_details() {
        let detail = sample_detail("root-1");
        let contained = sample_contained();
        let progress = sample_progress();
        for progress in [None, Some(&progress)] {
            let fingerprint =
                |detail: &InspectionRunDetail,
                 contained: &BTreeMap<String, InspectionRunDetail>| {
                    run_detail_pane_fingerprint(
                        Some(detail),
                        Some(contained),
                        progress,
                        Some(0),
                        80,
                    )
                };
            let baseline = fingerprint(&detail, &contained);
            let mut updated = detail.clone();
            updated.steps[0].output_summary.workflow_files[0].local_path =
                Some(std::path::PathBuf::from("/different/workspace/spec.md"));
            assert_ne!(baseline, fingerprint(&updated, &contained));
            let mut updated = contained.clone();
            updated.get_mut("child-1").expect("child").steps[0]
                .output_summary
                .workflow_files[0]
                .local_path = Some(std::path::PathBuf::from("/different/child/spec.md"));
            assert_ne!(baseline, fingerprint(&detail, &updated));
        }
    }

    #[test]
    fn unavailable_blob_paths_are_visible_and_not_openable() {
        let step = sample_detail("root-1").steps.remove(0);
        let mut lines = Vec::new();
        for (title, family, refs) in [
            (
                "Artifacts",
                BlobFamily::Artifact,
                &step.output_summary.artifact_refs,
            ),
            (
                "Evidence",
                BlobFamily::Evidence,
                &step.output_summary.evidence_refs,
            ),
        ] {
            append_blob_ref_detail_lines(&mut lines, title, None, "root-1", family, refs);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&format!("{} (path unavailable)", refs[0].name)))
            );
        }
        assert!(derive_openable_paths(&lines).iter().all(Option::is_none));
    }

    #[test]
    fn tracked_field_changes_flip_fingerprint() {
        let detail = sample_detail("root-1");
        let contained = sample_contained();
        let progress = sample_progress();
        let baseline = fp(&detail, &contained, &progress, 80);

        let mut seen = BTreeSet::new();

        let mut d = detail.clone();
        d.steps[0].status = "failed".to_string();
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].input_summary.adapter_kind = Some("process".to_string());
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].input_summary.plugin_id = Some("plugin-b".to_string());
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].input_summary.action = Some("other".to_string());
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].input_summary.input_object_keys = vec!["input".to_string()];
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].input_summary.metadata_object_keys = vec!["plugin_id".to_string()];
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].output_summary.child_run_id = Some("other-child".to_string());
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].started_at_ms = Some(99);
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].finished_at_ms = Some(101);
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].output_summary.error_code = Some("E-1".to_string());
        seen.insert(fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0].output_summary.error_message = Some("boom".to_string());
        seen.insert(fp(&d, &contained, &progress, 80));

        assert!(seen.into_iter().all(|value| value != baseline));
    }

    #[test]
    fn changing_only_preview_text_keeps_fingerprint() {
        let detail = sample_detail("root-1");
        let contained = sample_contained();
        let mut progress = sample_progress();
        let before = fp(&detail, &contained, &progress, 80);
        progress.preview_text = Some("different output".to_string());
        let after = fp(&detail, &contained, &progress, 80);
        assert_eq!(before, after);
    }

    #[test]
    fn pane_width_change_flips_fingerprint() {
        let detail = sample_detail("root-1");
        let contained = sample_contained();
        let progress = sample_progress();
        assert_ne!(
            fp(&detail, &contained, &progress, 80),
            fp(&detail, &contained, &progress, 81)
        );
    }

    #[test]
    fn terminal_detail_changes_flip_fingerprint_without_progress() {
        let detail = sample_detail("root-1");
        let contained = sample_contained();
        let baseline =
            run_detail_pane_fingerprint(Some(&detail), Some(&contained), None, Some(0), 80);

        let mut updated = detail.clone();
        updated.steps[0].status = "failed".to_string();

        assert_ne!(
            baseline,
            run_detail_pane_fingerprint(Some(&updated), Some(&contained), None, Some(0), 80)
        );
    }

    #[test]
    fn preflight_failure_summary_changes_flip_fingerprint() {
        let mut detail = sample_detail("root-1");
        detail.steps[0].status = "pending".to_string();
        detail.steps[0].output_summary.status = Some("pending".to_string());
        detail.preflight = Some(InspectionPreflightSummary {
            status: "failed".to_string(),
            checked_at_ms: 0,
            blocker_count: 1,
            warning_count: 0,
            failed_checks: vec![InspectionPreflightFailedCheck {
                kind: "request_preparation".to_string(),
                message: "plugin 'shell' is not installed".to_string(),
                step_ids: vec!["step-a".to_string()],
                plugin_id: Some("shell".to_string()),
                action: Some("exec".to_string()),
            }],
        });

        let baseline = run_detail_pane_fingerprint(Some(&detail), None, None, Some(0), 80);

        let mut updated = detail.clone();
        updated.preflight.as_mut().expect("preflight").failed_checks[0].message =
            "plugin 'shell' version mismatch".to_string();

        assert_ne!(
            baseline,
            run_detail_pane_fingerprint(Some(&updated), None, None, Some(0), 80)
        );
    }

    #[test]
    fn structural_additions_flip_fingerprint() {
        let detail = sample_detail("root-1");
        let contained = sample_contained();
        let progress = sample_progress();
        let baseline = fp(&detail, &contained, &progress, 80);

        let mut d = detail.clone();
        d.steps[0]
            .output_summary
            .artifact_refs
            .push(InspectionStepBlobRef {
                step_id: "step-a".to_string(),
                execution: 1,
                relative_path: "art/b.txt".to_string(),
                name: "b.txt".to_string(),
                bytes: 1,
            });
        assert_ne!(baseline, fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.steps[0]
            .output_summary
            .evidence_refs
            .push(InspectionStepBlobRef {
                step_id: "step-a".to_string(),
                execution: 1,
                relative_path: "ev/f.txt".to_string(),
                name: "f.txt".to_string(),
                bytes: 2,
            });
        assert_ne!(baseline, fp(&d, &contained, &progress, 80));

        let mut p = progress.clone();
        p.attached_step_output_blocks[0]
            .lines
            .push("line-b".to_string());
        assert_ne!(baseline, fp(&detail, &contained, &p, 80));

        let mut d = detail.clone();
        d.blocked_gates.push(InspectionGate {
            gate_id: "gate-2".to_string(),
            status: "blocked".to_string(),
            step_id: "step-a".to_string(),
            created_at_ms: None,
            decision: None,
            decision_history: vec![],
            request_summary: BTreeMap::new(),
        });
        assert_ne!(baseline, fp(&d, &contained, &progress, 80));

        let mut d = detail.clone();
        d.blocked_gates[0]
            .decision_history
            .push(InspectionGateDecision {
                review_request: None,
                state: "approved".to_string(),
                actor: Some("alice".to_string()),
                reason: None,
                decided_at_ms: Some(100),
            });
        assert_ne!(baseline, fp(&d, &contained, &progress, 80));
    }

    #[test]
    fn strips_mergeability_marker_only_for_pr_mergeability_message() {
        let raw = "X Pull request example-org/example-repo#ISSUE-2870 is not mergeable: the base branch policy prohibits the merge.";
        assert_eq!(
            super::strip_mergeability_failure_marker(raw),
            "Pull request example-org/example-repo#ISSUE-2870 is not mergeable: the base branch policy prohibits the merge."
        );
        assert_eq!(
            super::strip_mergeability_failure_marker("X Something else"),
            "X Something else"
        );
    }

    #[test]
    fn document_review_request_renders_exact_metadata_and_opaque_content_link() {
        let revision = "a".repeat(64);
        let digest = "b".repeat(64);
        let summary = BTreeMap::from([(
            "review_request".into(),
            transist::KernelValue::from_json(serde_json::json!({
                "schema":"transist.gate_review_request.v1", "run_id":"child", "gate_id":"operator", "step_id":"review", "execution":3,
                "review_documents":[{"schema":"transist.document_revision.v1", "revision_id":revision, "root_run_id":"root", "document":"notes", "run_id":"root", "step_id":"produce", "execution":1, "commit_sequence":1, "kind":"file", "blob":{"sha256":digest,"size_bytes":7}, "size_bytes":7, "file_count":1}]
            })),
        )]);
        let mut lines = Vec::new();
        super::append_gate_request_summary_lines(&mut lines, &summary, None);
        let text = lines.join("\n");
        assert!(text.contains("Review documents: 1 (request execution 3)"));
        assert!(text.contains("notes: 7 bytes, 1 files"));
        assert!(text.contains(&revision) && text.contains(&digest));
        assert!(text.contains(&format!("/api/v1/runs/child/documents/{revision}/content")));
        assert!(!text.contains("documents/blobs"));
    }
}
