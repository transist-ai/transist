// SPDX-License-Identifier: MPL-2.0
//! Shared fullscreen UI rendering components.
//!
//! These functions render reusable panes, overlays, and frame layouts used by
//! multiple screens. Keeping them here lets behavior-specific screen modules
//! focus on state transitions and key bindings.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::cmp::min;
use std::path::Path;

use crate::display::progress::{ChecklistState, checklist_state_for_step};
use crate::display::spinner::LiveProgress;

use super::pager::PagerState;
use super::*;

fn render_wrapped_ui_lines(
    lines: &[String],
    error_line_range: Option<&std::ops::Range<usize>>,
    width: usize,
    offset: usize,
    visible_count: usize,
) -> Vec<Line<'static>> {
    wrapped_ui_line_entries(lines, width)
        .into_iter()
        .skip(offset)
        .take(visible_count.max(1))
        .map(|entry| {
            if error_line_range
                .as_ref()
                .is_some_and(|range| range.contains(&entry.source_index))
            {
                Line::from(Span::styled(entry.text, Style::default().fg(Color::Red)))
            } else {
                Line::from(entry.text)
            }
        })
        .collect()
}

fn styled_dashboard_header_line(line: &str) -> Line<'static> {
    let Some(after_running) = line.strip_prefix("Running: ") else {
        return Line::from(line.to_string());
    };
    let Some((running_count, blocked_count)) = after_running.split_once("   Blocked: ") else {
        return Line::from(line.to_string());
    };
    Line::from(vec![
        Span::styled(
            format!("Running: {}", running_count),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            format!("Blocked: {}", blocked_count),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn checklist_prefix_style(state: ChecklistState) -> Style {
    match state {
        ChecklistState::Success => Style::default().fg(Color::Green),
        ChecklistState::Active => Style::default().fg(Color::Cyan),
        ChecklistState::Pending | ChecklistState::Skipped => Style::default().fg(Color::DarkGray),
        ChecklistState::Blocked => Style::default().fg(Color::Yellow),
        ChecklistState::Failed => Style::default().fg(Color::Red),
        ChecklistState::Canceled => Style::default().fg(Color::Magenta),
    }
}

fn styled_step_line(
    text: &str,
    state: ChecklistState,
    focused: bool,
    selected: bool,
) -> Line<'static> {
    let dim_style = if focused || selected {
        section_content_style(true)
    } else {
        section_content_style(false)
    };
    let Some(end) = text.find(']') else {
        return Line::from(Span::styled(text.to_string(), dim_style));
    };
    let (prefix, rest) = text.split_at(end + 1);
    let prefix_style = checklist_prefix_style(state);
    Line::from(vec![
        Span::styled(prefix.to_string(), prefix_style),
        Span::styled(rest.to_string(), dim_style),
    ])
}

pub(super) fn draw_base_frame(
    frame: &mut ratatui::Frame<'_>,
    metadata: &ScreenMetadata,
    dashboard_state: Option<&DashboardState>,
    progress: Option<&LiveProgress>,
    history_lines: &[String],
    picker_state: Option<&ActionPickerState>,
) {
    if let Some(dashboard_state) = dashboard_state {
        draw_dashboard_frame(frame, dashboard_state);
        return;
    }
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let header_lines = build_header_lines(metadata, progress);
    let header = Paragraph::new(header_lines).block(outer_frame_block("Transist", Color::Cyan));
    frame.render_widget(header, layout[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(41),
            Constraint::Percentage(25),
        ])
        .split(layout[1]);

    // See note in `draw_run_detail_frame`: wipe before sub-rendering so a
    // previous frame using a different layout (e.g. 2-column run-detail)
    // does not bleed through into the now-active 3-column layout.
    frame.render_widget(Clear, layout[1]);

    draw_steps_pane(frame, body[0], progress, history_lines, None, false);
    draw_details_pane(frame, body[1], progress, history_lines);
    draw_sidebar_pane(frame, body[2], history_lines, picker_state, false);

    let footer = Paragraph::new(build_footer_lines(progress, picker_state, false))
        .block(outer_frame_block("Keys", Color::Yellow))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_run_detail_frame(
    frame: &mut ratatui::Frame<'_>,
    metadata: &ScreenMetadata,
    home_dir: Option<&Path>,
    progress: Option<&LiveProgress>,
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    spinner_tick: u64,
    run_detail_state: Option<&RunDetailState>,
    picker_state: Option<&ActionPickerState>,
) {
    let run_detail_state = run_detail_state.cloned().unwrap_or(RunDetailState {
        focused_section: RunDetailSection::Details,
        selected_step: progress.and_then(selected_step_index).unwrap_or(0),
        details_scroll: 0,
        details_cursor: 0,
        selected_action: 0,
        step_pinned_by_user: false,
        pane_cache: None,
    });
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let header_lines = build_header_lines(metadata, progress);
    // Run-detail screen headers are titled with the run id rather than the
    // generic product name so an operator can see at a glance which run they
    // are inspecting. Workflow id@version stays inside the header body,
    // emitted by `build_header_lines` from `metadata.workflow_ref`.
    let header_title = match metadata.run_id.as_deref() {
        Some(run_id) => format!("Run: {}", run_id),
        None => "Run".to_string(),
    };
    let header = Paragraph::new(header_lines).block(outer_frame_block(&header_title, Color::Cyan));
    frame.render_widget(header, layout[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(layout[1]);

    // Wipe stale buffer cells in the body region before redrawing the
    // sub-panes. Without this, transitioning from `draw_base_frame` (3
    // columns) or from a different sub-layout (e.g. picker showing/hiding)
    // can leave fragments from the previous frame visible because ratatui's
    // diff renderer only repaints cells that widgets actively write into,
    // and the new layout's panes do not necessarily cover the same cells.
    frame.render_widget(Clear, layout[1]);

    draw_run_detail_steps_pane(
        frame,
        body[0],
        progress,
        run_detail,
        &[],
        Some(run_detail_state.selected_step),
        matches!(run_detail_state.focused_section, RunDetailSection::Steps),
    );
    draw_run_detail_pane(
        frame,
        body[1],
        home_dir,
        run_detail,
        contained_run_details,
        progress,
        picker_state,
        &run_detail_state,
        spinner_tick,
    );

    let footer = Paragraph::new(vec![
        style_hint_line(
            "Navigation: Tab/Shift+Tab switch section  Up/Down move within section  Enter execute action  r refresh  x or Ctrl+C interrupt active run  Esc back",
        ),
        style_hint_line(
            "Details: j/k moves the row cursor  o opens the highlighted file in pager  e opens it in editor (if configured)",
        ),
        style_hint_line(
            "Actions: suggested commands are listed at the bottom of Details; read-only inspection opens a modal",
        ),
    ])
    .block(outer_frame_block("Keys", Color::Yellow))
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

pub(super) fn draw_dashboard_frame(
    frame: &mut ratatui::Frame<'_>,
    dashboard_state: &DashboardState,
) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let header_lines = dashboard_state
        .header_lines
        .iter()
        .map(|line| styled_dashboard_header_line(line))
        .collect::<Vec<_>>();
    let header = Paragraph::new(header_lines)
        .block(outer_frame_block("Transist", Color::Cyan))
        .wrap(Wrap { trim: false });
    frame.render_widget(header, layout[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(36),
            Constraint::Percentage(39),
            Constraint::Percentage(25),
        ])
        .split(layout[1]);

    let setup = Paragraph::new(render_wrapped_ui_lines(
        &dashboard_state.setup_lines,
        dashboard_state.setup_error_line_range.as_ref(),
        body[0].width.saturating_sub(2) as usize,
        dashboard_state.setup_scroll,
        body[0].height.saturating_sub(2) as usize,
    ))
    .block(section_block(
        "Setup",
        matches!(dashboard_state.focused_section, DashboardSection::Setup),
    ))
    .style(section_content_style(matches!(
        dashboard_state.focused_section,
        DashboardSection::Setup
    )))
    .wrap(Wrap { trim: false });
    frame.render_widget(setup, body[0]);

    let inventory_lines = wrapped_ui_lines(
        &dashboard_state.inventory_lines,
        body[1].width.saturating_sub(2) as usize,
    );
    let inventory = Paragraph::new(
        visible_lines(
            &inventory_lines,
            dashboard_state.inventory_scroll,
            body[1].height.saturating_sub(2) as usize,
        )
        .join("\n"),
    )
    .block(section_block(
        "Inventory",
        matches!(dashboard_state.focused_section, DashboardSection::Inventory),
    ))
    .style(section_content_style(matches!(
        dashboard_state.focused_section,
        DashboardSection::Inventory
    )))
    .wrap(Wrap { trim: false });
    frame.render_widget(inventory, body[1]);

    let action_items = dashboard_state
        .action_items
        .iter()
        .map(|item| ListItem::new(item.label.clone()))
        .collect::<Vec<_>>();
    let mut action_state = ListState::default();
    action_state.select(Some(dashboard_state.selected_action));
    let actions = List::new(action_items)
        .block(section_block(
            "Actions",
            matches!(dashboard_state.focused_section, DashboardSection::Actions),
        ))
        .style(section_content_style(matches!(
            dashboard_state.focused_section,
            DashboardSection::Actions
        )))
        .highlight_style(section_highlight_style(matches!(
            dashboard_state.focused_section,
            DashboardSection::Actions
        )));
    frame.render_stateful_widget(actions, body[2], &mut action_state);

    let footer = Paragraph::new(vec![
        style_hint_line(
            "Navigation: Tab/Shift+Tab switch section  Up/Down move within section  Enter select  r refresh  q quit  1-9 shortcuts",
        ),
        style_hint_line("Actions: dashboard panes now keep independent scroll state"),
    ])
        .block(outer_frame_block("Keys", Color::Yellow))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

pub(super) fn draw_problems_frame(frame: &mut ratatui::Frame<'_>, state: &ProblemsViewState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let header = Paragraph::new(format!("Detected problems: {}", state.items.len()))
        .block(outer_frame_block("Problems", Color::Cyan));
    frame.render_widget(header, layout[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(layout[1]);
    let items = state
        .items
        .iter()
        .map(|item| ListItem::new(item.list_label()))
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    if !state.items.is_empty() {
        list_state.select(Some(state.selected));
    }
    let list = List::new(items)
        .block(section_block("Detected", true))
        .highlight_style(section_highlight_style(true));
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let detail = Paragraph::new(state.selected_detail_lines().join("\n"))
        .block(section_block("Selected", false))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, body[1]);

    let footer = Paragraph::new(vec![
        style_hint_line(&state.status_line),
        style_hint_line(
            "j/k move  Enter/o open diagnostic  r recompute  d dismiss session error  Esc dashboard",
        ),
    ])
    .block(outer_frame_block("Keys", Color::Yellow))
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

pub(super) fn draw_config_screen_frame(
    frame: &mut ratatui::Frame<'_>,
    config_state: &ConfigScreenState,
) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let header = Paragraph::new(
        config_state
            .header_lines
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect::<Vec<_>>(),
    )
    .block(Block::default().borders(Borders::ALL).title("Preferences"))
    .wrap(Wrap { trim: false });
    frame.render_widget(header, layout[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(layout[1]);

    let details_width = body[0].width.saturating_sub(2) as usize;
    let details_visible = body[0].height.saturating_sub(2) as usize;
    let details = Paragraph::new(render_wrapped_ui_lines(
        &config_state.details_lines,
        config_state.details_error_line_range.as_ref(),
        details_width,
        config_state.details_scroll,
        details_visible,
    ))
    .block(section_block(
        "Details",
        matches!(config_state.focused_section, ConfigSection::Details),
    ))
    .style(section_content_style(matches!(
        config_state.focused_section,
        ConfigSection::Details
    )))
    .wrap(Wrap { trim: false });
    frame.render_widget(details, body[0]);

    let mut action_render_lines: Vec<Line<'_>> = Vec::new();
    let selected = config_state.selected_action;
    let is_action_focused = matches!(config_state.focused_section, ConfigSection::Actions);
    for (idx, item) in config_state.action_items.iter().enumerate() {
        if let Some(ref heading) = item.section_heading {
            action_render_lines.push(Line::styled(
                format!(" {} ", heading),
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        let is_selected = idx == selected;
        let style = if is_selected && is_action_focused {
            section_highlight_style(true)
        } else {
            Style::default()
        };
        action_render_lines.push(Line::styled(item.label.clone(), style));
    }
    let actions = Paragraph::new(action_render_lines)
        .block(section_block("Actions", is_action_focused))
        .style(section_content_style(is_action_focused))
        .wrap(Wrap { trim: false });
    frame.render_widget(actions, body[1]);

    let footer = Paragraph::new(vec![
        style_hint_line(
            "Navigation: Tab/Shift+Tab switch section  Up/Down move within section  Enter execute action  r refresh  Esc back  1-9 shortcuts",
        ),
        style_hint_line("Actions: manage user preferences only"),
    ])
    .block(outer_frame_block("Keys", Color::Yellow))
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

pub(super) fn draw_steps_pane(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    progress: Option<&LiveProgress>,
    history_lines: &[String],
    selected_step: Option<usize>,
    focused: bool,
) {
    if let Some(progress) = progress {
        let selected_index = selected_step.or_else(|| selected_step_index(progress));
        let items = progress
            .state
            .checklist
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let raw = crate::terminal::strip_ansi_for_terminal_measurement(
                    &progress.render_checklist_line(item),
                );
                ListItem::new(styled_step_line(
                    &raw,
                    item.state,
                    focused,
                    selected_index == Some(idx),
                ))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(selected_index);
        let list = List::new(items)
            .block(section_block("Steps", focused))
            .style(section_content_style(focused))
            .highlight_style(section_highlight_style(focused));
        frame.render_stateful_widget(list, area, &mut state);
        return;
    }
    let lines = tail_lines(history_lines, area.height as usize);
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(section_block("Transcript", focused))
        .style(section_content_style(focused))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_run_detail_steps_pane(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    progress: Option<&LiveProgress>,
    run_detail: Option<&transist::InspectionRunDetail>,
    history_lines: &[String],
    selected_step: Option<usize>,
    focused: bool,
) {
    if let Some(progress) = progress {
        let selected_index = selected_step
            .or_else(|| selected_step_index(progress))
            .map(|idx| idx.min(progress.state.checklist.len().saturating_sub(1)));
        let items: Vec<ListItem> = progress
            .state
            .checklist
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let raw = crate::terminal::strip_ansi_for_terminal_measurement(
                    &progress.render_checklist_line(item),
                );
                ListItem::new(styled_step_line(
                    &raw,
                    item.state,
                    focused,
                    selected_index == Some(idx),
                ))
            })
            .collect();
        let mut state = ListState::default();
        state.select(selected_index);
        let list = List::new(items)
            .block(section_block("Steps", focused))
            .style(section_content_style(focused))
            .highlight_style(section_highlight_style(focused));
        frame.render_stateful_widget(list, area, &mut state);
        return;
    }
    if let Some(run_detail) = run_detail {
        let selected_index = if run_detail.steps.is_empty() {
            None
        } else {
            Some(
                selected_step
                    .unwrap_or_else(|| {
                        super::run_detail::default_selected_step_for_run_detail(run_detail)
                    })
                    .min(run_detail.steps.len().saturating_sub(1)),
            )
        };
        let items: Vec<ListItem> = run_detail
            .steps
            .iter()
            .enumerate()
            .map(|(idx, step)| {
                let state = checklist_state_for_step(step, None);
                let raw = format!("{} {}", state.marker(), step.step_id);
                ListItem::new(styled_step_line(
                    &raw,
                    state,
                    focused,
                    selected_index == Some(idx),
                ))
            })
            .collect();
        let mut state = ListState::default();
        state.select(selected_index);
        let list = List::new(items)
            .block(section_block("Steps", focused))
            .style(section_content_style(focused))
            .highlight_style(section_highlight_style(focused));
        frame.render_stateful_widget(list, area, &mut state);
        return;
    }
    draw_steps_pane(frame, area, None, history_lines, selected_step, focused);
}

pub(super) fn draw_details_pane(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    progress: Option<&LiveProgress>,
    history_lines: &[String],
) {
    let lines = if let Some(progress) = progress {
        build_progress_detail_lines(progress, None)
    } else {
        tail_lines(history_lines, area.height as usize)
    };
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(outer_frame_block("Details", Color::Cyan))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_run_detail_pane(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    home_dir: Option<&Path>,
    run_detail: Option<&transist::InspectionRunDetail>,
    contained_run_details: Option<
        &std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    >,
    progress: Option<&LiveProgress>,
    picker_state: Option<&ActionPickerState>,
    run_detail_state: &RunDetailState,
    spinner_tick: u64,
) {
    let has_commands = picker_state
        .map(|state| !state.items.is_empty())
        .unwrap_or(false);

    // Wipe the right-pane area before splitting into Details + Commands so
    // shrinking/growing the commands sub-rect (when picker_state appears or
    // disappears, or its item count changes) does not leave stale cells
    // from the prior frame visible.
    frame.render_widget(Clear, area);

    let (body_area, commands_area) = if has_commands {
        let commands_height = picker_state
            .map(|state| (state.items.len() as u16).saturating_add(2).clamp(3, 12))
            .unwrap_or(0);
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(commands_height)])
            .split(area);
        (split[0], Some(split[1]))
    } else {
        (area, None)
    };

    let body_focused = matches!(run_detail_state.focused_section, RunDetailSection::Details);
    let body_lines = build_run_detail_history_lines(
        home_dir,
        run_detail,
        contained_run_details,
        progress,
        Some(run_detail_state.selected_step),
        body_area.width.saturating_sub(2) as usize,
        spinner_tick,
    )
    .lines;
    // Slice to the visible window driven by the scroll offset, then build a
    // styled `Line` per visible row. The cursor row (only meaningful when
    // Details has focus) is rendered with reverse-video so the operator
    // sees which line `o` will open.
    let visible_count = body_area.height.saturating_sub(2) as usize;
    let visible_count = visible_count.max(1);
    let max_offset = body_lines.len().saturating_sub(visible_count);
    let offset = run_detail_state.details_scroll.min(max_offset);
    let cursor_pos = run_detail_state.details_cursor;
    let visible_slice = body_lines
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_count);
    let rendered: Vec<Line> = visible_slice
        .map(|(idx, detail_line)| {
            if body_focused && idx == cursor_pos {
                // Match the steps list selection color (blue background,
                // white foreground) so all focusable lists in the run-detail
                // screen share a consistent highlight.
                Line::from(Span::styled(
                    detail_line.text.clone(),
                    Style::default().bg(Color::Blue).fg(Color::White),
                ))
            } else {
                Line::from(detail_line.text.clone())
            }
        })
        .collect();
    let paragraph = Paragraph::new(rendered)
        .block(section_block("Details", body_focused))
        .style(section_content_style(body_focused));
    // Note: ratatui's Wrap is intentionally NOT applied here. body_lines is
    // already pre-wrapped to body width by `wrap_detail_pane_line`, which
    // breaks long single-quoted paths at `/` boundaries so they remain
    // scannable. Re-wrapping here would split paths mid-character.
    frame.render_widget(paragraph, body_area);

    if let (Some(commands_area), Some(picker_state)) = (commands_area, picker_state) {
        let commands_focused =
            matches!(run_detail_state.focused_section, RunDetailSection::Actions);
        let commands_disabled = !picker_state.actionable;
        let items = picker_state
            .items
            .iter()
            .map(|item| {
                if commands_disabled {
                    ListItem::new(Line::from(Span::styled(
                        item.clone(),
                        Style::default().fg(Color::DarkGray),
                    )))
                } else {
                    ListItem::new(item.clone())
                }
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(Some(picker_state.selected));
        let title = if picker_state.title.is_empty() {
            "Commands".to_string()
        } else {
            picker_state.title.clone()
        };
        let list = List::new(items)
            .block(section_block(&title, commands_focused))
            .style(if commands_disabled {
                section_content_style(commands_focused).fg(Color::DarkGray)
            } else {
                section_content_style(commands_focused)
            })
            .highlight_style(section_highlight_style(commands_focused));
        frame.render_stateful_widget(list, commands_area, &mut state);
    }
}

pub(super) fn draw_sidebar_pane(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    history_lines: &[String],
    picker_state: Option<&ActionPickerState>,
    focused: bool,
) {
    if let Some(picker_state) = picker_state {
        let items = picker_state
            .items
            .iter()
            .map(|item| ListItem::new(item.clone()))
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(Some(picker_state.selected));
        let list = List::new(items)
            .block(section_block(&picker_state.title, focused))
            .style(section_content_style(focused))
            .highlight_style(section_highlight_style(focused));
        frame.render_stateful_widget(list, area, &mut state);
        return;
    }
    let lines = tail_lines(history_lines, area.height as usize);
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(section_block("Recent output", focused))
        .style(section_content_style(focused))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

pub(super) fn draw_run_browser_frame(frame: &mut ratatui::Frame<'_>, state: &RunBrowserState) {
    let area = frame.area();
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
    let header = Paragraph::new(vec![
        Line::from("Runs browser"),
        Line::from(format!(
            "Path: {}   Visible: {}   Total: {}   Status filter: {}",
            state.path_label(),
            state.current_items().len(),
            state.summaries_by_id.len(),
            state.status_filter_label(),
        )),
    ])
    .block(outer_frame_block("Run tree", Color::Cyan));
    frame.render_widget(header, layout[0]);

    let run_items = if state.current_items().is_empty() {
        vec![ListItem::new(if state.current_parent_run_id.is_some() {
            "(no direct child runs at this level)".to_string()
        } else {
            "(no runs found)".to_string()
        })]
    } else {
        state
            .current_items()
            .iter()
            .map(|summary| ListItem::new(format_run_browser_item(state, summary)))
            .collect::<Vec<_>>()
    };
    let mut list_state = ListState::default();
    if !state.current_items().is_empty() {
        list_state.select(Some(state.selected));
    }
    let list = List::new(run_items)
        .block(section_block(&state.current_level_title(), true))
        .style(section_content_style(true))
        .highlight_style(section_highlight_style(true));
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let selection_title = state
        .focus_run()
        .map(|summary| format!("Run {}", summary.run_id))
        .unwrap_or_else(|| "Selection".to_string());
    let details = Paragraph::new(build_run_browser_detail_lines(state).join("\n"))
        .block(section_block(&selection_title, false))
        .wrap(Wrap { trim: false });
    frame.render_widget(details, body[1]);

    let footer = Paragraph::new(vec![
        Line::from(state.status_line.clone()),
        style_hint_line(
            "Enter inspect run  Right/l child level  Left/h/backspace parent  f filter  j/k move  r refresh  q dashboard",
        ),
    ])
    .block(outer_frame_block("Keys", Color::Yellow))
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[2]);
}

pub(super) fn draw_pager_overlay(frame: &mut ratatui::Frame<'_>, state: &PagerState) {
    let area = centered_rect(92, 85, frame.area());
    frame.render_widget(Clear, area);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(area);
    let header = Paragraph::new(vec![
        Line::from(state.title.clone()),
        Line::from(format!(
            "Lines {}-{} of {}",
            state.offset.saturating_add(1).min(state.lines.len().max(1)),
            min(
                state
                    .offset
                    .saturating_add(layout[1].height.saturating_sub(2) as usize),
                state.lines.len().max(1)
            ),
            state.lines.len().max(1)
        )),
    ])
    .block(outer_frame_block("Pager", Color::Cyan));
    frame.render_widget(header, layout[0]);
    let visible_count = layout[1].height.saturating_sub(2) as usize;
    let end = min(
        state.offset.saturating_add(visible_count),
        state.lines.len(),
    );
    let body = Paragraph::new(state.lines[state.offset..end].join("\n"))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(body, layout[1]);
    let footer = Paragraph::new(style_hint_line(&state.status_line))
        .block(outer_frame_block("Keys", Color::Yellow));
    frame.render_widget(footer, layout[2]);
}

pub(super) fn draw_picker_overlay(frame: &mut ratatui::Frame<'_>, state: &ActionPickerState) {
    let area = centered_rect(62, 55, frame.area());
    frame.render_widget(Clear, area);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(area);
    let items = state
        .items
        .iter()
        .map(|item| ListItem::new(item.clone()))
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(state.title.clone()),
        )
        .highlight_style(Style::default().bg(Color::Green).fg(Color::Black));
    frame.render_stateful_widget(list, layout[0], &mut list_state);
    let footer = Paragraph::new(style_hint_line(
        "j/k move  Enter select  Esc close  1-9 shortcuts",
    ))
    .block(outer_frame_block("Keys", Color::Yellow));
    frame.render_widget(footer, layout[1]);
}

pub(super) fn draw_text_input_overlay(frame: &mut ratatui::Frame<'_>, state: &TextInputState) {
    let area = centered_rect(70, 50, frame.area());
    frame.render_widget(Clear, area);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(area);
    // Render the live TextArea so its built-in cursor (reverse-video block at
    // the cursor position) appears on top of the text. The textarea owns its
    // own bordered block (with the prompt title), so we don't wrap it again.
    frame.render_widget(&state.textarea, layout[0]);
    let footer = Paragraph::new(style_hint_line(&state.help))
        .block(outer_frame_block("Keys", Color::Yellow));
    frame.render_widget(footer, layout[1]);
}

pub(super) fn draw_workflow_inputs_overlay(
    frame: &mut ratatui::Frame<'_>,
    state: &WorkflowInputsModalState,
) {
    let area = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, area);
    let title = match state.file_label.as_ref() {
        Some(label) => format!("Workflow inputs — file: {}", label),
        None => "Workflow inputs — file: (none)".to_string(),
    };
    let outer = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner_area = outer.inner(area);
    frame.render_widget(outer, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(inner_area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(layout[0]);

    let list_focused = matches!(state.focused_pane, WorkflowInputsPane::List);
    let editor_focused = matches!(state.focused_pane, WorkflowInputsPane::Editor);

    let list_border_style = if list_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let editor_border_style = if editor_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };

    let rows: Vec<String> = state
        .declared
        .iter()
        .map(workflow_inputs_row_label)
        .collect();
    let items: Vec<ListItem> = rows.iter().map(|row| ListItem::new(row.clone())).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(list_border_style)
                .title("Parameters (* required)"),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut list_state = ListState::default();
    if !rows.is_empty() {
        list_state.select(Some(state.selected.min(rows.len() - 1)));
    }
    frame.render_stateful_widget(list, body[0], &mut list_state);

    // Render the live editor TextArea inside our own bordered block so the
    // border style can reflect focus while the textarea draws its cursor.
    let editor_title = state
        .declared
        .get(state.selected)
        .and_then(|item| item.comment.as_deref())
        .map(str::trim)
        .filter(|comment| !comment.is_empty())
        .map(|comment| format!("Value (JSON) — {}", comment))
        .unwrap_or_else(|| "Value (JSON)".to_string());
    let editor_block = Block::default()
        .borders(Borders::ALL)
        .border_style(editor_border_style)
        .title(editor_title);
    let editor_inner = editor_block.inner(body[1]);
    frame.render_widget(editor_block, body[1]);
    frame.render_widget(&state.editor, editor_inner);

    let status_text = state.status.clone().unwrap_or_default();
    let status_style = if state.status.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    };
    let status = Paragraph::new(Line::from(Span::styled(status_text, status_style)));
    frame.render_widget(status, layout[1]);

    let footer = Paragraph::new(style_hint_line(
        "Tab switch pane  ↑/↓ pick  Enter save/list  Shift+Enter newline  Ctrl+S submit  Esc cancel",
    ))
    .block(outer_frame_block("Keys", Color::Yellow));
    frame.render_widget(footer, layout[2]);
}

fn workflow_inputs_row_label(item: &WorkflowInputItem) -> String {
    let raw = item.value.as_deref().unwrap_or("").trim();
    let required_marker = if item.required { "*" } else { " " };
    let origin_tag = if !raw.is_empty() {
        match item.origin {
            WorkflowInputOrigin::File => " [file]",
            WorkflowInputOrigin::UserEdited => " [edited]",
            WorkflowInputOrigin::None => "",
        }
    } else if item.default_value.is_some() {
        " [default]"
    } else {
        ""
    };
    format!(
        "{} {}:{}{}",
        required_marker, item.name, item.value_type, origin_tag
    )
}

pub(super) fn draw_confirm_overlay(frame: &mut ratatui::Frame<'_>, state: &ConfirmState) {
    let area = centered_rect(55, 30, frame.area());
    frame.render_widget(Clear, area);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);
    let body = Paragraph::new(vec![Line::from(state.message.clone())])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(state.title.clone()),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(body, layout[0]);
    let yes_style = if state.selected_yes {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let no_style = if state.selected_yes {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    };
    let buttons = Paragraph::new(Line::from(vec![
        Span::styled("  Yes  ", yes_style),
        Span::raw("   "),
        Span::styled("  No  ", no_style),
    ]))
    .block(outer_frame_block("Confirm", Color::Yellow));
    frame.render_widget(buttons, layout[1]);
    let footer = Paragraph::new(style_hint_line(
        "Left/h yes  Right/l/Tab no  Enter confirm  y yes  n/Esc no",
    ))
    .block(outer_frame_block("Keys", Color::Yellow));
    frame.render_widget(footer, layout[2]);
}

pub(super) fn draw_loading_overlay(frame: &mut ratatui::Frame<'_>, state: &LoadingState) {
    let screen = frame.area();
    let content_width = state
        .message
        .chars()
        .count()
        .saturating_add("Please wait...".len())
        .saturating_add(8);
    let max_width = screen.width.saturating_sub(4) as usize;
    let overlay_width = if max_width >= 36 {
        content_width.clamp(36, max_width) as u16
    } else {
        screen.width.saturating_sub(2).max(1)
    };
    let overlay_height = 7_u16.min(screen.height.saturating_sub(2)).max(5);
    let x = screen
        .x
        .saturating_add(screen.width.saturating_sub(overlay_width) / 2);
    let y = screen
        .y
        .saturating_add(screen.height.saturating_sub(overlay_height) / 2);
    let area = Rect::new(x, y, overlay_width, overlay_height);
    frame.render_widget(Clear, area);
    let body = Paragraph::new(vec![
        Line::from(""),
        Line::from(state.message.clone()),
        Line::from("Please wait..."),
    ])
    .alignment(Alignment::Center)
    .block(outer_frame_block("Loading", Color::Cyan))
    .wrap(Wrap { trim: false });
    frame.render_widget(body, area);
}
