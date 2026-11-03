// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use serde_json::Value;
use std::fs;
#[cfg(test)]
use std::io;
#[cfg(test)]
use std::io::IsTerminal;
use std::time::{SystemTime, UNIX_EPOCH};
use transist::TransistInspectionContext;

use crate::LIVE_PROGRESS_FILE_NAME;
use crate::display::output::AttachedStepOutputBlocks;
use crate::display::progress::{
    ChecklistState, CurrentStepStateRef, ProgressChecklistItem, ProgressState,
    ProgressStateFromStatesArgs, build_progress_state_from_states,
    build_progress_state_with_timing_overrides, select_live_progress_step_state,
    select_terminal_progress_step, should_dim_line_for_running_focus, style_checklist_marker,
    style_non_current_progress_line,
};
use crate::terminal::{
    current_terminal_columns, format_preview_text, preview_body_limit, working_preview_text,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveProgress {
    pub(crate) state: ProgressState,
    pub(crate) preview_stream: Option<String>,
    pub(crate) preview_text: Option<String>,
    pub(crate) attached_step_output_blocks: AttachedStepOutputBlocks,
    pub(crate) footer_lines: Vec<String>,
}

const LIVE_PROGRESS_STALL_THRESHOLD_SECS: u64 = 60;

impl LiveProgress {
    #[cfg(test)]
    pub(crate) fn render_lines_plain(&self) -> Vec<String> {
        self.render_progress_lines(self.render_preview_line_plain())
    }

    #[cfg(test)]
    pub(crate) fn render_progress_lines(&self, preview_line: Option<String>) -> Vec<String> {
        let mut lines = Vec::new();
        let preview_insert_index = self.preview_insert_index();
        for (index, item) in self.state.checklist.iter().enumerate() {
            lines.push(self.render_checklist_line(item));
            self.push_attached_step_output_blocks(&mut lines, Some(item.step_id.as_str()), true);
            if preview_insert_index == Some(index)
                && let Some(preview_line) = preview_line.as_ref()
            {
                lines.push(indent_step_output_line(preview_line));
            }
        }

        self.push_attached_step_output_blocks(&mut lines, None, false);
        if preview_insert_index.is_none()
            && let Some(preview_line) = preview_line
        {
            lines.push(preview_line);
        }
        lines.extend(
            self.footer_lines
                .iter()
                .map(|line| style_run_footer_line(line)),
        );

        lines
    }

    pub(crate) fn render_active_step_lines_plain(&self) -> Vec<String> {
        self.render_active_step_lines_with_preview(self.render_preview_line_plain())
    }

    fn render_active_step_lines_with_preview(&self, preview_line: Option<String>) -> Vec<String> {
        let mut lines = Vec::new();
        let Some(item) = self.current_focus_checklist_item() else {
            self.push_attached_step_output_blocks(&mut lines, None, false);
            if let Some(preview_line) = preview_line {
                lines.push(preview_line);
            }
            return lines;
        };

        lines.push(self.render_checklist_line(item));
        self.push_attached_step_output_blocks(&mut lines, Some(item.step_id.as_str()), true);
        if let Some(preview_line) = preview_line {
            lines.push(indent_step_output_line(&preview_line));
        }
        self.push_attached_step_output_blocks(&mut lines, None, true);
        lines
    }

    pub(crate) fn render_checklist_line(&self, item: &ProgressChecklistItem) -> String {
        let is_running = self.is_running_checklist_item(item);
        let is_current_non_running = self.is_current_checklist_item(item)
            && !is_running
            && self.state.finished_at_ms.is_some();
        let should_dim = should_dim_line_for_running_focus(
            self.running_focus_step_id(),
            Some(item.step_id.as_str()),
        );
        // The Steps pane intentionally renders a static `[>]` marker for
        // running items rather than the rotating `spinner_frame(spinner_tick)`
        // glyph. We still include a compact elapsed suffix so operators can
        // spot long-running work at a glance.
        //
        // A running item and a "current but not-yet-finished" item both
        // render as `[>]` (Active) — the former because execution is in
        // flight, the latter so a freshly-entered pending step still
        // surfaces as the active row before its first state transition.
        let render_as_active = is_running
            || (self.is_current_checklist_item(item)
                && matches!(item.state, ChecklistState::Pending | ChecklistState::Active));
        let marker = if render_as_active {
            ChecklistState::Active.marker()
        } else {
            item.state.marker()
        };
        let marker = style_checklist_marker(marker, item.state, is_running, should_dim);
        let mut text = format!(" {}", item.step_id);
        if is_running {
            if let Some(seconds) = self.state.elapsed_seconds() {
                text.push_str(&format!(" ({}s active)", seconds));
            }
        } else if is_current_non_running {
            // The displayed seconds here are derived from the top-level
            // step's wall-clock-equivalent bounds (start through latest
            // observed timestamp). For a blocked step that wall-clock value
            // is the blocked-wait window; for any other non-running current
            // step it is wall clock. Label both cases explicitly rather
            // than the legacy ambiguous "paused" wording.
            if let Some(seconds) = self.state.elapsed_seconds() {
                let suffix = match item.state {
                    ChecklistState::Blocked => format!("(blocked for {}s)", seconds),
                    _ => format!("({}s wall clock)", seconds),
                };
                text.push(' ');
                text.push_str(&suffix);
            }
        } else if matches!(item.state, ChecklistState::Success) {
            if let Some(seconds) = item.duration_seconds {
                text.push_str(&format!(" ({}s)", seconds));
            }
        }
        if should_dim {
            text = style_non_current_progress_line(&text);
        }
        format!("{}{}", marker, text)
    }

    #[cfg(test)]
    fn preview_insert_index(&self) -> Option<usize> {
        self.preview_body()?;
        self.state
            .checklist
            .iter()
            .position(|item| self.is_current_checklist_item(item))
    }

    fn current_focus_checklist_item(&self) -> Option<&ProgressChecklistItem> {
        self.state
            .top_level_step_id
            .as_deref()
            .and_then(|step_id| {
                self.state
                    .checklist
                    .iter()
                    .find(|item| item.step_id == step_id)
            })
            .or_else(|| {
                self.state.checklist.iter().find(|item| {
                    matches!(
                        item.state,
                        ChecklistState::Active
                            | ChecklistState::Blocked
                            | ChecklistState::Failed
                            | ChecklistState::Canceled
                    )
                })
            })
            .or_else(|| {
                self.state
                    .checklist
                    .iter()
                    .rev()
                    .find(|item| item.state != ChecklistState::Pending)
            })
            .or_else(|| self.state.checklist.first())
    }

    pub(crate) fn push_attached_step_output_blocks(
        &self,
        lines: &mut Vec<String>,
        anchor_step_id: Option<&str>,
        indent: bool,
    ) {
        let should_dim =
            should_dim_line_for_running_focus(self.running_focus_step_id(), anchor_step_id);
        let terminal_columns = current_terminal_columns();
        for block in &self.attached_step_output_blocks {
            if block.anchor_step_id.as_deref() != anchor_step_id {
                continue;
            }
            lines.extend(block.lines.iter().flat_map(|line| {
                let rendered = if indent {
                    indent_step_output_line(line)
                } else {
                    line.clone()
                };
                wrap_attached_step_output_line(&rendered, terminal_columns)
                    .into_iter()
                    .map(|wrapped| {
                        if should_dim {
                            style_non_current_progress_line(&wrapped)
                        } else {
                            wrapped
                        }
                    })
            }));
        }
    }

    fn is_current_checklist_item(&self, item: &ProgressChecklistItem) -> bool {
        self.state.top_level_step_id.as_deref() == Some(item.step_id.as_str())
    }

    fn is_running_checklist_item(&self, item: &ProgressChecklistItem) -> bool {
        self.is_current_checklist_item(item)
            && matches!(
                self.state.current_step_status.as_deref(),
                Some("running" | "dispatching" | "retry_wait")
            )
    }

    fn running_focus_step_id(&self) -> Option<&str> {
        matches!(
            self.state.current_step_status.as_deref(),
            Some("running" | "dispatching" | "retry_wait")
        )
        .then_some(self.state.top_level_step_id.as_deref())
        .flatten()
    }

    fn preview_body(&self) -> Option<String> {
        let tool_name = self.state.tool_name.as_deref()?;
        let body_limit = preview_body_limit(tool_name);
        match self.preview_stream.as_deref() {
            Some("stdout") | Some("stderr") | Some("status") => Some(
                self.preview_text
                    .as_deref()
                    .and_then(|raw| format_preview_text(raw, body_limit))
                    .map(|text| format!("{}: \"{}\"", tool_name, text))
                    .unwrap_or_else(|| working_preview_text(tool_name)),
            ),
            _ if matches!(
                self.state.current_step_status.as_deref(),
                Some("running" | "dispatching" | "retry_wait")
            ) =>
            {
                Some(working_preview_text(tool_name))
            }
            _ => None,
        }
    }

    fn render_preview_line_plain(&self) -> Option<String> {
        self.preview_body()
    }
}

pub(crate) fn indent_step_output_line(line: &str) -> String {
    format!("    {}", line)
}

pub(crate) fn wrap_attached_step_output_line(line: &str, columns: usize) -> Vec<String> {
    if should_preserve_unwrapped_terminal_line(line) {
        return vec![line.to_string()];
    }
    wrap_terminal_render_line(line, columns)
}

fn should_preserve_unwrapped_terminal_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        return true;
    }
    // Preserve any line that embeds a single-quoted absolute or relative path
    // (e.g. `  workflow.path: '/abs/path' (123 bytes)`). Hard-wrapping such
    // lines splits the path mid-character, breaking visual scan and any
    // downstream click-to-open behavior. We detect the pattern by finding a
    // pair of single quotes whose enclosed content contains a path separator.
    if let Some(open) = line.find('\'') {
        if let Some(close_offset) = line[open + 1..].find('\'') {
            let inside = &line[open + 1..open + 1 + close_offset];
            if inside.contains('/') {
                return true;
            }
        }
    }
    false
}

pub(crate) fn wrap_terminal_render_line(line: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(1);
    let visible_width = crate::terminal::visible_terminal_text_width(line);
    if visible_width <= columns {
        return vec![line.to_string()];
    }

    let leading_whitespace = line
        .chars()
        .take_while(|ch| ch.is_ascii_whitespace())
        .count();
    if leading_whitespace == 0 || leading_whitespace >= columns {
        return wrap_plain_text_chunks(line, columns);
    }

    let prefix = line.chars().take(leading_whitespace).collect::<String>();
    let content = line.chars().skip(leading_whitespace).collect::<String>();
    let content_columns = columns.saturating_sub(leading_whitespace).max(1);
    wrap_plain_text_chunks(&content, content_columns)
        .into_iter()
        .map(|chunk| format!("{}{}", prefix, chunk))
        .collect()
}

pub(crate) fn wrap_plain_text_chunks(text: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(1);
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + columns).min(chars.len());
        lines.push(chars[start..end].iter().collect());
        start = end;
    }
    lines
}

#[cfg(test)]
pub(crate) fn style_run_footer_line(line: &str) -> String {
    if !io::stderr().is_terminal() {
        return line.to_string();
    }
    use dialoguer::console::Style;
    Style::new().for_stderr().dim().apply_to(line).to_string()
}

pub(crate) fn terminal_live_progress(
    state: ProgressState,
    last_progress: Option<&LiveProgress>,
    attached_step_output_blocks: AttachedStepOutputBlocks,
) -> LiveProgress {
    LiveProgress {
        state,
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: if attached_step_output_blocks.is_empty() {
            last_progress
                .map(|progress| progress.attached_step_output_blocks.clone())
                .unwrap_or_default()
        } else {
            attached_step_output_blocks
        },
        footer_lines: Vec::new(),
    }
}

pub(crate) fn inspect_live_progress(
    context: &transist::TransistContext,
    root_run_id: &str,
    root_step_order: &[String],
    attached_step_output_blocks: AttachedStepOutputBlocks,
    recipe_cache: &crate::execution::RecipeCacheOwner,
) -> Result<Option<LiveProgress>> {
    let Some(root_state) = crate::execution::load_run_state(context, root_run_id)? else {
        return Ok(None);
    };
    let Some(actionable) =
        crate::execution::resolve_actionable_run_state(context, root_run_id, recipe_cache)?
    else {
        return Ok(None);
    };
    // During detached `runs retry`, the actionable run/step can already be
    // running while the root snapshot still reads as its previous terminal
    // status. Gate on the actionable state's live step instead of the root
    // status so the run-detail screen can follow the resumed work immediately.
    let Some((step_id, current_step)) = select_live_progress_step_state(&actionable.state) else {
        return Ok(None);
    };

    // When executing in the root run (not a subflow), use the same state that provided the
    // current step so the checklist reflects the step's actual status without a TOCTOU gap.
    let root_state = if actionable.run_id == root_run_id {
        actionable.state.clone()
    } else {
        root_state
    };

    let tool_name =
        step_tool_name_from_run(&actionable.state, &step_id).unwrap_or_else(|| "tool".to_string());
    let live_snapshot = current_step_live_progress_from_step(current_step, tool_name);
    Ok(Some(LiveProgress {
        state: build_progress_state_from_states(
            context,
            ProgressStateFromStatesArgs {
                root_run_id,
                root_state: &root_state,
                root_step_order,
                actionable: &actionable,
                current_step: CurrentStepStateRef {
                    step_id: &step_id,
                    step: current_step,
                },
                live_active_override: true,
            },
            Some(live_snapshot.tool_name.clone()),
        )?,
        preview_stream: live_snapshot.last_stream,
        preview_text: add_stall_hint(live_snapshot.last_line_preview, live_snapshot.updated_at_ms),
        attached_step_output_blocks,
        footer_lines: Vec::new(),
    }))
}

pub(crate) fn inspect_terminal_progress(
    context: &transist::TransistContext,
    inspection: &TransistInspectionContext,
    root_run_id: &str,
) -> Result<Option<ProgressState>> {
    let Some(root_detail) = inspection.get_run(root_run_id)? else {
        return Ok(None);
    };
    if !matches!(
        root_detail.summary.status.as_str(),
        "success" | "blocked" | "failed" | "canceled"
    ) {
        return Ok(None);
    }
    let Some(actionable) = crate::actionable::resolve_actionable_run(inspection, root_run_id)?
    else {
        return Ok(None);
    };
    let selected_step = select_terminal_progress_step(&actionable);
    let tool_name = if let Some(step) = selected_step.filter(|step| step.step_kind == "invoke") {
        step_tool_name(context, &actionable.run_id, &step.step_id)?
    } else {
        None
    };
    let step_timing_overrides = crate::actionable::subflow_step_timing_overrides_for_detail(
        context.home_dir(),
        inspection,
        root_run_id,
        &root_detail,
    )?;
    Ok(Some(build_progress_state_with_timing_overrides(
        root_run_id,
        &root_detail,
        &actionable,
        selected_step,
        tool_name,
        false,
        &step_timing_overrides,
    )))
}

#[cfg(test)]
pub(crate) fn render_plain_interactive_progress(progress: &LiveProgress) -> Vec<String> {
    progress.render_active_step_lines_plain()
}

pub(crate) fn current_step_live_progress_from_step(
    step: &Value,
    tool_name: String,
) -> LiveProgressSnapshot {
    let Some(artifacts_dir) = step["last_request"]["paths"]["artifacts_dir"].as_str() else {
        return LiveProgressSnapshot {
            tool_name,
            ..LiveProgressSnapshot::default()
        };
    };
    let path = std::path::Path::new(artifacts_dir).join(LIVE_PROGRESS_FILE_NAME);
    if !path.exists() {
        return LiveProgressSnapshot {
            tool_name,
            ..LiveProgressSnapshot::default()
        };
    }
    let payload = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_default();
    LiveProgressSnapshot {
        tool_name,
        last_stream: payload["last_stream"].as_str().map(str::to_string),
        last_line_preview: payload["last_line_preview"].as_str().map(str::to_string),
        updated_at_ms: payload["updated_at_ms"].as_u64(),
    }
}

pub(crate) fn step_tool_name(
    context: &transist::TransistContext,
    run_id: &str,
    step_id: &str,
) -> Result<Option<String>> {
    let run = Value::from(context.runs_show(run_id)?);
    Ok(step_tool_name_from_run(&run, step_id))
}

pub(crate) fn step_tool_name_from_run(run: &Value, step_id: &str) -> Option<String> {
    run["steps"]
        .get(step_id)
        .and_then(step_tool_name_from_run_step)
        .or_else(|| step_tool_name_from_preflight(run, step_id))
}

fn step_tool_name_from_run_step(step: &Value) -> Option<String> {
    step["last_request"]["metadata"]["plugin_id"]
        .as_str()
        .or_else(|| step["last_request"]["plugin"]["id"].as_str())
        .map(str::to_string)
}

fn step_tool_name_from_preflight(run: &Value, step_id: &str) -> Option<String> {
    run["preflight"]["checks"].as_array().and_then(|checks| {
        checks.iter().find_map(|check| {
            let applies_to_step = check["step_ids"]
                .as_array()
                .map(|step_ids| step_ids.iter().any(|id| id.as_str() == Some(step_id)))
                .unwrap_or(false);
            applies_to_step
                .then(|| check["plugin_id"].as_str())
                .flatten()
                .map(str::to_string)
        })
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LiveProgressSnapshot {
    pub(crate) tool_name: String,
    pub(crate) last_stream: Option<String>,
    pub(crate) last_line_preview: Option<String>,
    pub(crate) updated_at_ms: Option<u64>,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn add_stall_hint(preview: Option<String>, updated_at_ms: Option<u64>) -> Option<String> {
    let Some(updated_at_ms) = updated_at_ms else {
        return preview;
    };
    let stalled_for = now_unix_ms().saturating_sub(updated_at_ms) / 1000;
    if stalled_for < LIVE_PROGRESS_STALL_THRESHOLD_SECS {
        return preview;
    }
    let hint = format!("no output for {}s", stalled_for);
    match preview {
        Some(existing) if !existing.is_empty() => Some(format!("{} ({})", existing, hint)),
        _ => Some(hint),
    }
}

#[cfg(test)]
mod tests {
    use super::add_stall_hint;

    #[test]
    fn add_stall_hint_keeps_recent_preview_unchanged() {
        let now_ms = super::now_unix_ms();
        let updated_at_ms = now_ms.saturating_sub(5_000);
        assert_eq!(
            add_stall_hint(Some("still working".to_string()), Some(updated_at_ms)),
            Some("still working".to_string())
        );
    }

    #[test]
    fn add_stall_hint_appends_no_output_for_stalled_preview() {
        let now_ms = super::now_unix_ms();
        let updated_at_ms = now_ms.saturating_sub(70_000);
        let hinted = add_stall_hint(Some("still working".to_string()), Some(updated_at_ms))
            .expect("hinted preview");
        assert!(hinted.starts_with("still working (no output for "));
        assert!(hinted.ends_with("s)"));
    }
}
