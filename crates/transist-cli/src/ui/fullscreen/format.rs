// SPDX-License-Identifier: MPL-2.0
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui_textarea::TextArea;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::display::spinner::wrap_attached_step_output_line;

pub(super) fn pluralize(noun: &str, count: usize) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{}s", noun)
    }
}

/// Formats a millisecond Unix timestamp as an RFC3339 / ISO 8601 string in UTC,
/// e.g. `2026-05-03T12:34:56Z`. Returns the raw value as a fallback if the
/// timestamp is outside the representable range.
pub(super) fn format_unix_ms_iso_utc(ms: u64) -> String {
    let nanos = (ms as i128).saturating_mul(1_000_000);
    match OffsetDateTime::from_unix_timestamp_nanos(nanos) {
        Ok(dt) => dt
            .replace_nanosecond(0)
            .ok()
            .and_then(|trimmed| trimmed.format(&Rfc3339).ok())
            .unwrap_or_else(|| ms.to_string()),
        Err(_) => ms.to_string(),
    }
}

pub(super) fn format_run_browser_duration_ms(ms: u64) -> String {
    let total_secs = ms / 1000;
    let millis = ms % 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else if seconds > 0 {
        format!("{}.{:03}s", seconds, millis)
    } else {
        format!("{}ms", ms)
    }
}

pub(super) fn clamp_scroll_offset(
    total_lines: usize,
    visible_lines: usize,
    offset: usize,
) -> usize {
    offset.min(total_lines.saturating_sub(visible_lines.max(1)))
}

pub(super) fn apply_scroll_delta(
    offset: &mut usize,
    total_lines: usize,
    visible_lines: usize,
    delta: isize,
) {
    let max_offset = total_lines.saturating_sub(visible_lines.max(1));
    let next = (*offset as isize + delta).clamp(0, max_offset as isize) as usize;
    *offset = next;
}

/// Adjust `scroll_offset` so the cursor row stays inside the visible window.
/// Cursor at or above the offset scrolls up to the cursor; cursor below the
/// last visible row scrolls down so the cursor is the last visible row.
/// Cursor inside the window is left untouched.
pub(super) fn follow_cursor_with_scroll(
    scroll_offset: &mut usize,
    cursor: usize,
    visible_lines: usize,
    total_lines: usize,
) {
    let visible = visible_lines.max(1);
    let max_offset = total_lines.saturating_sub(visible);
    if cursor < *scroll_offset {
        *scroll_offset = cursor;
    } else if cursor >= scroll_offset.saturating_add(visible) {
        *scroll_offset = cursor + 1 - visible;
    }
    if *scroll_offset > max_offset {
        *scroll_offset = max_offset;
    }
}

pub(super) fn section_block(title: &str, focused: bool) -> Block<'static> {
    let title_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let title_span = Span::styled(title.to_string(), title_style);
    let block = Block::default().borders(Borders::ALL).title(title_span);
    if focused {
        block
            .border_style(Style::default().fg(Color::Cyan))
            .border_type(BorderType::Thick)
    } else {
        block
    }
}

pub(super) fn section_content_style(focused: bool) -> Style {
    let _ = focused;
    Style::default()
}

pub(super) fn section_highlight_style(focused: bool) -> Style {
    let _ = focused;
    Style::default()
        .bg(Color::Blue)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// Outer frame block used for top-level chrome ("Transist", "Keys"). Always
/// renders a bold accented title even though the frame itself is never marked
/// focused, so the user has consistent landmarks while scanning the screen.
pub(super) fn outer_frame_block(title: &str, accent: Color) -> Block<'static> {
    let title_span = Span::styled(
        title.to_string(),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    );
    Block::default().borders(Borders::ALL).title(title_span)
}

/// Style hint tokens (keys / shortcuts) inside footer/help text so the eye can
/// quickly scan keybindings vs. surrounding prose. Tokens are matched by an
/// allow-list to keep the result deterministic and avoid coloring random
/// punctuation. Whitespace runs are preserved in the output.
pub(super) fn style_hint_line(text: &str) -> Line<'static> {
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span<'static>> = Vec::new();
    // Tokenize while preserving whitespace runs as separate raw spans.
    let mut buf = String::new();
    let mut in_ws = false;
    let push_token = |spans: &mut Vec<Span<'static>>, buf: &mut String| {
        if buf.is_empty() {
            return;
        }
        let token = std::mem::take(buf);
        let bare = token.trim_end_matches([',', ';', ':', '.', ')']);
        let trailing = &token[bare.len()..];
        if is_keybinding_token(bare) {
            spans.push(Span::styled(bare.to_string(), key_style));
            if !trailing.is_empty() {
                spans.push(Span::raw(trailing.to_string()));
            }
        } else if let Some(label) = token.strip_suffix(':')
            && is_section_label(label)
        {
            spans.push(Span::styled(token, label_style));
        } else {
            spans.push(Span::raw(token));
        }
    };
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_ws && !buf.is_empty() {
                push_token(&mut spans, &mut buf);
            }
            in_ws = true;
            buf.push(ch);
        } else {
            if in_ws && !buf.is_empty() {
                spans.push(Span::raw(std::mem::take(&mut buf)));
            }
            in_ws = false;
            buf.push(ch);
        }
    }
    if in_ws && !buf.is_empty() {
        spans.push(Span::raw(buf));
    } else if !buf.is_empty() {
        push_token(&mut spans, &mut buf);
    }
    Line::from(spans)
}

fn is_keybinding_token(token: &str) -> bool {
    // Conservative allow-list: covers the keybinding vocabulary used in the
    // various footer strings across the fullscreen UI.
    matches!(
        token,
        "Tab"
            | "Tab/Shift+Tab"
            | "Shift+Tab"
            | "BackTab"
            | "Up/Down"
            | "Left/Right"
            | "↑/↓"
            | "Up"
            | "Down"
            | "Left"
            | "Right"
            | "Enter"
            | "Esc"
            | "Space"
            | "PageUp"
            | "PageDown"
            | "Home"
            | "End"
            | "Ctrl+C"
            | "Ctrl+S"
            | "Ctrl+D"
            | "Ctrl+U"
            | "Ctrl+B"
            | "Ctrl+F"
            | "j/k"
            | "h/l"
            | "1-9"
            | "Left/h"
            | "Left/h/backspace"
            | "Right/l/Tab"
            | "Right/l"
            | "n/Esc"
            | "backspace"
            | "f"
            | "y"
            | "n"
            | "q"
            | "r"
            | "x"
            | "e"
            | "o"
    )
}

fn is_section_label(label: &str) -> bool {
    matches!(label, "Navigation" | "Actions" | "Keys" | "Tip")
}

pub(super) fn wrapped_ui_lines(lines: &[String], width: usize) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| wrap_attached_step_output_line(line, width.max(1)))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WrappedUiLine {
    pub(super) source_index: usize,
    pub(super) text: String,
}

pub(super) fn wrapped_ui_line_entries(lines: &[String], width: usize) -> Vec<WrappedUiLine> {
    lines
        .iter()
        .enumerate()
        .flat_map(|(source_index, line)| {
            wrap_attached_step_output_line(line, width.max(1))
                .into_iter()
                .map(move |text| WrappedUiLine { source_index, text })
        })
        .collect()
}

pub(super) fn visible_lines(lines: &[String], offset: usize, visible_count: usize) -> Vec<String> {
    let visible_count = visible_count.max(1);
    let offset = offset.min(lines.len().saturating_sub(visible_count));
    lines
        .iter()
        .skip(offset)
        .take(visible_count)
        .cloned()
        .collect()
}

pub(super) fn status_color(status: &str) -> Color {
    match status {
        "success" => Color::Green,
        "blocked" => Color::Yellow,
        "failed" => Color::Red,
        "canceled" => Color::Magenta,
        _ => Color::Cyan,
    }
}

pub(super) fn tail_lines(lines: &[String], max_lines: usize) -> Vec<String> {
    let limit = max_lines.max(1);
    lines
        .iter()
        .rev()
        .take(limit)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub(super) fn truncate_vec(lines: &mut Vec<String>, max_lines: usize) {
    if lines.len() > max_lines {
        let drain = lines.len() - max_lines;
        lines.drain(0..drain);
    }
}

pub(super) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub(super) fn join_text_area_lines(textarea: &TextArea<'_>) -> String {
    textarea.lines().join("\n").trim_end().to_string()
}

#[cfg(test)]
mod accent_tests {
    use super::*;

    #[test]
    fn style_hint_line_highlights_known_keybindings() {
        let line = style_hint_line("Navigation: Tab switch  Ctrl+C cancel  q quit");
        let styled_tokens: Vec<&str> = line
            .spans
            .iter()
            .filter(|span| {
                span.style.fg == Some(Color::Yellow)
                    && span.style.add_modifier.contains(Modifier::BOLD)
            })
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(styled_tokens, vec!["Tab", "Ctrl+C", "q"]);
    }

    #[test]
    fn style_hint_line_emphasizes_section_label() {
        let line = style_hint_line("Actions: choose a suggested command");
        // "Actions:" should be bold (no color), prose should be raw.
        let bold_only_tokens: Vec<&str> = line
            .spans
            .iter()
            .filter(|span| {
                span.style.fg.is_none() && span.style.add_modifier.contains(Modifier::BOLD)
            })
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(bold_only_tokens, vec!["Actions:"]);
    }

    #[test]
    fn style_hint_line_leaves_plain_words_uncolored() {
        let line = style_hint_line("hold Option to select text");
        for span in line.spans.iter() {
            assert!(
                span.style.fg.is_none() || span.style.fg == Some(Color::Reset),
                "non-keybinding word should not be colored: {:?}",
                span
            );
        }
    }
}
