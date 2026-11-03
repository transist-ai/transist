// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use transist::TransistInspectionContext;

use super::format::{
    format_run_browser_duration_ms, format_unix_ms_iso_utc, pluralize, status_color,
};

/// Cache entry for the focused run's metrics in the runs browser.
///
/// The browser does not load metrics synchronously the moment a row is
/// focused (that made arrow-key navigation feel laggy because each load
/// blocks the event loop). Instead a focused row first enters the
/// `Pending` state with the timestamp of focus; the load is deferred and
/// only executed if the same row stays focused long enough (see
/// [`RunBrowserState::load_focused_run_metrics_if_due`]).
#[derive(Debug, Clone)]
pub(super) enum MetricsCacheEntry {
    /// The row is focused, but the load has not started yet. The
    /// timestamp records when focus landed on this row so the event
    /// loop can apply a debounce before performing the (synchronous)
    /// load.
    Pending { since: Instant },
    /// The metrics for this run have been loaded. `None` means the load
    /// completed but no metrics are available (or the home directory
    /// was empty, which short-circuits loading). The payload is boxed
    /// because `RunMetricsSummary` is significantly larger than the
    /// `Pending` discriminant — boxing keeps the enum compact and
    /// silences `clippy::large_enum_variant`.
    Loaded(Box<Option<transist::RunMetricsSummary>>),
}

#[derive(Debug, Clone)]
pub(super) struct RunBrowserCursor {
    pub(super) parent_run_id: Option<String>,
    pub(super) selected: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RunBrowserState {
    pub(super) summaries_by_id: BTreeMap<String, transist::InspectionRunSummary>,
    pub(super) root_runs: Vec<transist::InspectionRunSummary>,
    pub(super) children_by_parent: BTreeMap<String, Vec<transist::InspectionRunSummary>>,
    pub(super) status_filter: Option<String>,
    pub(super) cursor_stack: Vec<RunBrowserCursor>,
    pub(super) path_run_ids: Vec<String>,
    pub(super) current_parent_run_id: Option<String>,
    pub(super) selected: usize,
    pub(super) status_line: String,
    pub(super) home_dir: PathBuf,
    pub(super) metrics_cache: BTreeMap<String, MetricsCacheEntry>,
    pub(super) last_focused_run_id: Option<String>,
}

impl RunBrowserState {
    pub(super) fn from_summaries(
        summaries: Vec<transist::InspectionRunSummary>,
        status_filter: Option<String>,
        home_dir: PathBuf,
    ) -> Self {
        let mut summaries_by_id = BTreeMap::new();
        let mut root_runs = Vec::new();
        let mut children_by_parent = BTreeMap::<String, Vec<transist::InspectionRunSummary>>::new();

        for summary in summaries {
            if let Some(parent_run_id) = summary.parent_run_id.clone() {
                children_by_parent
                    .entry(parent_run_id)
                    .or_default()
                    .push(summary.clone());
            } else {
                root_runs.push(summary.clone());
            }
            summaries_by_id.insert(summary.run_id.clone(), summary);
        }

        sort_run_summaries_desc(&mut root_runs);
        for children in children_by_parent.values_mut() {
            sort_run_summaries_desc(children);
        }

        let root_count = root_runs.len();
        Self {
            summaries_by_id,
            root_runs,
            children_by_parent,
            status_filter: status_filter.clone(),
            cursor_stack: Vec::new(),
            path_run_ids: Vec::new(),
            current_parent_run_id: None,
            selected: 0,
            status_line: if root_count == 0 {
                status_filter
                    .as_deref()
                    .map(|status| format!("No runs found with status '{}'.", status))
                    .unwrap_or_else(|| "No runs found in this home yet.".to_string())
            } else {
                format!(
                    "Showing {} root {}. Enter opens the selected run screen. Right opens child runs{}.",
                    root_count,
                    pluralize("run", root_count),
                    status_filter
                        .as_deref()
                        .map(|status| format!(" (status filter: {})", status))
                        .unwrap_or_default()
                )
            },
            home_dir,
            metrics_cache: BTreeMap::new(),
            last_focused_run_id: None,
        }
    }

    pub(super) fn current_items(&self) -> &[transist::InspectionRunSummary] {
        self.current_items_for_parent(self.current_parent_run_id.as_deref())
    }

    pub(super) fn current_items_for_parent(
        &self,
        parent_run_id: Option<&str>,
    ) -> &[transist::InspectionRunSummary] {
        match parent_run_id {
            Some(parent_run_id) => self
                .children_by_parent
                .get(parent_run_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            None => self.root_runs.as_slice(),
        }
    }

    pub(super) fn current_level_title(&self) -> String {
        self.current_parent_run_id
            .as_ref()
            .map(|run_id| format!("Child runs of {}", run_id))
            .unwrap_or_else(|| "Root runs".to_string())
    }

    pub(super) fn path_label(&self) -> String {
        if self.path_run_ids.is_empty() {
            return "roots".to_string();
        }
        format!("roots / {}", self.path_run_ids.join(" / "))
    }

    pub(super) fn status_filter_label(&self) -> &str {
        self.status_filter.as_deref().unwrap_or("all")
    }

    pub(super) fn child_count(&self, run_id: &str) -> usize {
        self.children_by_parent
            .get(run_id)
            .map(Vec::len)
            .unwrap_or(0)
    }

    pub(super) fn selected_run(&self) -> Option<&transist::InspectionRunSummary> {
        self.current_items().get(self.selected)
    }

    pub(super) fn selected_run_id(&self) -> Option<&str> {
        self.selected_run().map(|summary| summary.run_id.as_str())
    }

    pub(super) fn focus_run(&self) -> Option<&transist::InspectionRunSummary> {
        self.selected_run().or_else(|| {
            self.current_parent_run_id
                .as_ref()
                .and_then(|run_id| self.summaries_by_id.get(run_id))
        })
    }

    pub(super) fn clamp_selected(&mut self) {
        let len = self.current_items().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(len - 1);
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        let len = self.current_items().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, len.saturating_sub(1) as isize) as usize;
    }

    pub(super) fn page_selection(&mut self, delta_pages: isize, page_size: usize) {
        let step = page_size.max(1) as isize;
        self.move_selection(delta_pages.saturating_mul(step));
    }

    pub(super) fn descend(&mut self) -> bool {
        let Some(selected_run) = self.selected_run().cloned() else {
            self.status_line = "No runs are available at this level.".to_string();
            return false;
        };
        let child_count = self.child_count(&selected_run.run_id);
        if child_count == 0 {
            self.status_line = format!(
                "{} has no child runs. Press Enter to open its run screen.",
                selected_run.run_id
            );
            return false;
        }
        self.cursor_stack.push(RunBrowserCursor {
            parent_run_id: self.current_parent_run_id.clone(),
            selected: self.selected,
        });
        self.path_run_ids.push(selected_run.run_id.clone());
        self.current_parent_run_id = Some(selected_run.run_id.clone());
        self.selected = 0;
        self.status_line = format!(
            "Showing {} child {} under {}.",
            child_count,
            pluralize("run", child_count),
            selected_run.run_id
        );
        true
    }

    pub(super) fn ascend(&mut self) -> bool {
        let Some(cursor) = self.cursor_stack.pop() else {
            self.status_line = "Already at root runs. Press q to close the browser.".to_string();
            return false;
        };
        let previous_child = self.path_run_ids.pop().unwrap_or_default();
        self.current_parent_run_id = cursor.parent_run_id;
        self.selected = cursor.selected;
        self.clamp_selected();
        self.status_line = if previous_child.is_empty() {
            "Returned to the previous level.".to_string()
        } else {
            format!("Returned from {} to its parent level.", previous_child)
        };
        true
    }

    pub(super) fn restore_position(
        &mut self,
        parent_run_id: Option<&str>,
        selected_run_id: Option<&str>,
    ) {
        self.cursor_stack.clear();
        self.path_run_ids.clear();
        self.current_parent_run_id = None;
        self.selected = 0;

        let mut chain = Vec::new();
        let mut current = parent_run_id
            .filter(|run_id| self.summaries_by_id.contains_key(*run_id))
            .map(str::to_string);
        while let Some(run_id) = current {
            let Some(summary) = self.summaries_by_id.get(&run_id) else {
                break;
            };
            chain.push(run_id.clone());
            current = summary.parent_run_id.clone();
        }
        chain.reverse();

        let mut current_parent: Option<String> = None;
        for run_id in chain {
            let selected = self
                .current_items_for_parent(current_parent.as_deref())
                .iter()
                .position(|summary| summary.run_id == run_id)
                .unwrap_or(0);
            self.cursor_stack.push(RunBrowserCursor {
                parent_run_id: current_parent.clone(),
                selected,
            });
            self.path_run_ids.push(run_id.clone());
            current_parent = Some(run_id);
        }
        self.current_parent_run_id = current_parent;
        self.selected = selected_run_id
            .and_then(|run_id| {
                self.current_items()
                    .iter()
                    .position(|summary| summary.run_id == run_id)
            })
            .or_else(|| {
                self.current_parent_run_id.as_deref().and_then(|run_id| {
                    self.current_items()
                        .iter()
                        .position(|summary| summary.run_id == run_id)
                })
            })
            .unwrap_or(0);
        self.clamp_selected();
    }
}

pub(super) fn load_run_browser_state(
    context: &transist::TransistContext,
) -> Result<RunBrowserState> {
    load_run_browser_state_with_filter(context, None)
}

pub(super) fn load_run_browser_state_with_filter(
    context: &transist::TransistContext,
    status_filter: Option<&str>,
) -> Result<RunBrowserState> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let runs = inspection.list_all_runs(transist::RunListScope::All, status_filter)?;

    Ok(RunBrowserState::from_summaries(
        runs,
        status_filter.map(str::to_string),
        context.home_dir().to_path_buf(),
    ))
}

pub(super) fn load_root_run_summaries(
    context: &transist::TransistContext,
) -> Result<Vec<transist::InspectionRunSummary>> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    inspection.list_all_runs(transist::RunListScope::Roots, None)
}

pub(super) fn sort_run_summaries_desc(runs: &mut [transist::InspectionRunSummary]) {
    runs.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then(right.created_at_ms.cmp(&left.created_at_ms))
            .then(left.run_id.cmp(&right.run_id))
    });
}

pub(super) fn format_run_browser_item(
    state: &RunBrowserState,
    summary: &transist::InspectionRunSummary,
) -> Line<'static> {
    let child_count = state.child_count(&summary.run_id);
    let mut suffixes = Vec::new();
    if child_count > 0 {
        suffixes.push(format!("children {}", child_count));
    }
    if summary.has_blocked_gates {
        suffixes.push("blocked gates".to_string());
    }
    if summary.current_step_id.is_some() {
        suffixes.push("active step".to_string());
    }
    let suffix = if suffixes.is_empty() {
        String::new()
    } else {
        format!("  [{}]", suffixes.join(", "))
    };
    Line::from(vec![
        Span::styled(
            format!("[{:8}]", summary.status),
            Style::default().fg(status_color(summary.status.as_str())),
        ),
        Span::raw(" "),
        Span::styled(
            summary.run_id.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {}@", summary.workflow_id)),
        Span::styled(
            summary.workflow_version.clone(),
            if transist::is_test_workflow(&summary.workflow_version) {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            },
        ),
        Span::raw(suffix),
    ])
}

pub(super) fn build_run_browser_metrics_lines(
    cache: &BTreeMap<String, MetricsCacheEntry>,
    run_id: &str,
) -> Vec<String> {
    let metrics = match cache.get(run_id) {
        // No entry yet means the user has not started navigating from
        // the initial selection; do not show a loading placeholder yet.
        None => return Vec::new(),
        // Load is pending the debounce window.
        Some(MetricsCacheEntry::Pending { .. }) => {
            return vec!["Metrics".to_string(), "  Loading metrics...".to_string()];
        }
        Some(MetricsCacheEntry::Loaded(boxed)) => match boxed.as_ref() {
            None => return Vec::new(),
            Some(metrics) => metrics,
        },
    };
    let mut lines = Vec::new();
    lines.push("Metrics".to_string());
    lines.push(format!(
        "  Step executions: {} (succeeded {}, failed {}, cancelled {})",
        metrics.total_step_executions,
        metrics.succeeded_step_executions,
        metrics.failed_step_executions,
        metrics.cancelled_step_executions,
    ));
    lines.push(format!("  Artifacts: {}", metrics.total_artifacts_produced));
    lines.push(format!(
        "  Evidence files: {}",
        metrics.total_evidence_files_produced
    ));
    let tokens_in = metrics
        .input_tokens
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let tokens_out = metrics
        .output_tokens
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let tokens_total = metrics
        .total_tokens
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    lines.push(format!(
        "  Tokens: in {} / out {} / total {}",
        tokens_in, tokens_out, tokens_total
    ));
    let wall_clock_duration = metrics.wall_clock_duration_ms.or(metrics.total_duration_ms);
    if let Some(duration_ms) = wall_clock_duration {
        lines.push(format!(
            "  Wall clock: {}",
            format_run_browser_duration_ms(duration_ms)
        ));
    }
    if let Some(duration_ms) = metrics.active_execution_duration_ms {
        lines.push(format!(
            "  Active: {}",
            format_run_browser_duration_ms(duration_ms)
        ));
    }
    if let Some(duration_ms) = metrics.blocked_wait_duration_ms {
        lines.push(format!(
            "  Blocked wait: {}",
            format_run_browser_duration_ms(duration_ms)
        ));
    }
    lines
}

impl RunBrowserState {
    /// Register the focused run for deferred metrics loading.
    ///
    /// This is a fast, pure-state operation with no I/O. The debounce
    /// timestamp is refreshed only when focus moves onto a different
    /// run and that run is still pending, so revisiting a row restarts
    /// the debounce window but a stable focus can still age into a
    /// metrics load (see [`Self::load_focused_run_metrics_if_due`]).
    ///
    /// Returns `true` when the cache entry changed.
    pub(super) fn mark_focused_run_pending(&mut self) -> bool {
        let Some(run_id) = self.focus_run().map(|summary| summary.run_id.clone()) else {
            self.last_focused_run_id = None;
            return false;
        };
        let focus_changed = self.last_focused_run_id.as_deref() != Some(run_id.as_str());
        self.last_focused_run_id = Some(run_id.clone());
        if let Some(entry) = self.metrics_cache.get_mut(&run_id) {
            if focus_changed {
                if let MetricsCacheEntry::Pending { since } = entry {
                    *since = Instant::now();
                    return true;
                }
            }
            return false;
        }
        // No home dir => nothing to load; record an immediate empty
        // result so the placeholder is not shown forever.
        if self.home_dir.as_os_str().is_empty() {
            self.metrics_cache
                .insert(run_id, MetricsCacheEntry::Loaded(Box::new(None)));
            return true;
        }
        self.metrics_cache.insert(
            run_id,
            MetricsCacheEntry::Pending {
                since: Instant::now(),
            },
        );
        true
    }

    /// If the focused run is `Pending` and has been pending for at
    /// least `debounce`, load its metrics synchronously and store them
    /// as `Loaded`. Returns `true` when a load actually completed
    /// (caller should redraw).
    ///
    /// The load is intentionally synchronous: by the time the debounce
    /// elapses the user has stopped navigating, so a brief blocking
    /// call is invisible and avoids the threading/cancellation cost
    /// that an async fetch would introduce.
    pub(super) fn load_focused_run_metrics_if_due(&mut self, debounce: Duration) -> bool {
        let Some(run_id) = self.focus_run().map(|summary| summary.run_id.clone()) else {
            return false;
        };
        let should_load = match self.metrics_cache.get(&run_id) {
            Some(MetricsCacheEntry::Pending { since }) => since.elapsed() >= debounce,
            _ => false,
        };
        if !should_load {
            return false;
        }
        let query =
            transist::TransistContext::with_home(PathBuf::new(), self.home_dir.clone()).metrics();
        let result = query.run(&run_id).ok().flatten();
        self.metrics_cache
            .insert(run_id, MetricsCacheEntry::Loaded(Box::new(result)));
        true
    }
}

pub(super) fn build_run_browser_detail_lines(state: &RunBrowserState) -> Vec<String> {
    let Some(summary) = state.focus_run() else {
        return vec![
            "No runs found in this home yet.".to_string(),
            String::new(),
            "Prepare or run a workflow, then reopen the runs browser.".to_string(),
        ];
    };

    let child_count = state.child_count(&summary.run_id);
    let mut lines = Vec::new();
    if state.current_items().is_empty() && state.current_parent_run_id.is_some() {
        lines.push("This run currently has no direct child runs.".to_string());
        lines.push(String::new());
    } else if !state.current_items().is_empty() {
        lines.push(format!(
            "Selected: {} of {}",
            state.selected.saturating_add(1),
            state.current_items().len()
        ));
        lines.push(String::new());
    }
    let metrics_lines = build_run_browser_metrics_lines(&state.metrics_cache, &summary.run_id);
    if !metrics_lines.is_empty() {
        lines.extend(metrics_lines);
        lines.push(String::new());
    }
    lines.push(format!("Run id: {}", summary.run_id));
    lines.push(format!(
        "Workflow: {}@{}",
        summary.workflow_id, summary.workflow_version
    ));
    lines.push(format!("Status: {}", summary.status));
    lines.push(format!("Root run: {}", summary.root_run_id));
    lines.push(format!(
        "Parent run: {}",
        summary.parent_run_id.as_deref().unwrap_or("(root)")
    ));
    lines.push(format!(
        "Current step: {}",
        summary.current_step_id.as_deref().unwrap_or("(none)")
    ));
    lines.push(format!(
        "Blocked gates: {}",
        if summary.has_blocked_gates {
            "yes"
        } else {
            "no"
        }
    ));
    lines.push(format!("Artifacts: {}", summary.artifact_count));
    lines.push(format!("Evidence: {}", summary.evidence_count));
    lines.push(format!(
        "Events: {}",
        if summary.has_events { "yes" } else { "no" }
    ));
    lines.push(format!("Children: {}", child_count));
    lines.push(format!(
        "Created: {}",
        format_unix_ms_iso_utc(summary.created_at_ms)
    ));
    lines.push(format!(
        "Updated: {}",
        format_unix_ms_iso_utc(summary.updated_at_ms)
    ));
    lines.push(String::new());
    if child_count > 0 {
        lines.push(
            "Enter opens this run screen. Right/l opens the direct child-run level.".to_string(),
        );
    } else {
        lines.push("This is a leaf run. Press Enter to open its run screen.".to_string());
    }
    if state.current_parent_run_id.is_some() {
        lines.push("Left/h/backspace returns to the parent level.".to_string());
    }
    lines
}

pub(super) fn run_browser_visible_count_for_area(size: Size) -> usize {
    let area = Rect::from((Position::ORIGIN, size));
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(6),
            Constraint::Length(4),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(layout[1]);
    body[0].height.saturating_sub(2) as usize
}
