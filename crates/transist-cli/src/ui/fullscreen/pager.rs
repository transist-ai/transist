// SPDX-License-Identifier: MPL-2.0
use std::cmp::min;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect, Size};

use super::FullscreenPrompt;
use super::centered_rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagnosticPagerShortcut {
    Close,
    Copy,
    ExternalPager,
}

fn diagnostic_pager_shortcut(key: KeyEvent) -> Option<DiagnosticPagerShortcut> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(DiagnosticPagerShortcut::Close);
    }
    if !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('c') => Some(DiagnosticPagerShortcut::Copy),
        KeyCode::Char('e') => Some(DiagnosticPagerShortcut::ExternalPager),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(super) struct PagerState {
    pub(super) title: String,
    pub(super) lines: Vec<String>,
    pub(super) offset: usize,
    pub(super) search_query: Option<String>,
    pub(super) search_matches: Vec<usize>,
    pub(super) search_index: usize,
    pub(super) pending_g: bool,
    pub(super) status_line: String,
    pub(super) diagnostic_body: Option<String>,
}

pub(super) fn pager_visible_count_for_area(size: Size) -> usize {
    let area = Rect::from((Position::ORIGIN, size));
    let overlay = centered_rect(92, 85, area);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(overlay);
    layout[1].height.saturating_sub(2) as usize
}

/// Splits pager text into scrollable visual rows without dropping or shortening values.
pub(super) fn pager_text_lines(body: &str, size: Size) -> Vec<String> {
    let overlay = centered_rect(92, 85, Rect::from((Position::ORIGIN, size)));
    let width = overlay.width.saturating_sub(2).max(1) as usize;
    if body.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    for line in body.lines() {
        let line = ratatui::text::Line::raw(line);
        let mut row = String::new();
        let mut row_width = 0;
        for grapheme in line.styled_graphemes(ratatui::style::Style::default()) {
            let grapheme_width = ratatui::text::Line::raw(grapheme.symbol).width();
            if row_width + grapheme_width > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
            }
            row.push_str(grapheme.symbol);
            row_width += grapheme_width;
        }
        rows.push(row);
    }
    rows
}

pub(super) fn handle_pager_key(
    prompt: &mut FullscreenPrompt,
    key: KeyEvent,
    area_height: usize,
) -> Result<bool> {
    let diagnostic_shortcut = diagnostic_pager_shortcut(key);
    if diagnostic_shortcut == Some(DiagnosticPagerShortcut::Close) {
        return Ok(true);
    }
    if matches!(
        diagnostic_shortcut,
        Some(DiagnosticPagerShortcut::Copy | DiagnosticPagerShortcut::ExternalPager)
    ) {
        let diagnostic_body = prompt
            .pager_state
            .as_ref()
            .and_then(|state| state.diagnostic_body.clone());
        if let Some(body) = diagnostic_body {
            let status = match diagnostic_shortcut {
                Some(DiagnosticPagerShortcut::Copy) => {
                    super::diagnostics::copy_diagnostic_to_clipboard(&body)
                }
                Some(DiagnosticPagerShortcut::ExternalPager) => {
                    super::diagnostics::open_diagnostic_in_external_pager(prompt, &body)
                }
                _ => unreachable!("matched diagnostic pager shortcut"),
            };
            if let Some(state) = prompt.pager_state.as_mut() {
                state.status_line = match status {
                    Ok(message) => message,
                    Err(error) => format!("Diagnostic action failed: {error}"),
                };
            }
            return Ok(false);
        }
    }
    let search_request = matches!(key.code, KeyCode::Char('/'));
    let search_seed = if search_request {
        prompt
            .pager_state
            .as_ref()
            .and_then(|state| state.search_query.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    if search_request {
        let query = prompt.run_text_input_modal(
            "Pager search",
            "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
            &search_seed,
            true,
        )?;
        if let Some(state) = prompt.pager_state.as_mut() {
            if let Some(query) = query {
                update_pager_search(state, &query);
            }
        }
        return Ok(false);
    }

    let Some(state) = prompt.pager_state.as_mut() else {
        return Ok(true);
    };
    let page = area_height.max(1);
    let page_half = (page / 2).max(1);
    match key.code {
        KeyCode::Esc => return Ok(true),
        KeyCode::Down | KeyCode::Char('j') => {
            state.pending_g = false;
            state.offset = min(
                state.offset.saturating_add(1),
                state.lines.len().saturating_sub(1),
            );
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.pending_g = false;
            state.offset = state.offset.saturating_sub(1);
        }
        KeyCode::PageDown | KeyCode::Char('f') | KeyCode::Char(' ') => {
            state.pending_g = false;
            state.offset = min(
                state.offset.saturating_add(page),
                state.lines.len().saturating_sub(1),
            );
        }
        KeyCode::PageUp | KeyCode::Char('b') => {
            state.pending_g = false;
            state.offset = state.offset.saturating_sub(page);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.pending_g = false;
            state.offset = min(
                state.offset.saturating_add(page_half),
                state.lines.len().saturating_sub(1),
            );
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.pending_g = false;
            state.offset = state.offset.saturating_sub(page_half);
        }
        KeyCode::Char('G') | KeyCode::End => {
            state.pending_g = false;
            state.offset = state.lines.len().saturating_sub(page);
        }
        KeyCode::Home => {
            state.pending_g = false;
            state.offset = 0;
        }
        KeyCode::Char('g') => {
            if state.pending_g {
                state.offset = 0;
                state.pending_g = false;
                state.status_line =
                    "Top of document. j/k move  Ctrl+D/Ctrl+U page  / search  Esc close"
                        .to_string();
            } else {
                state.pending_g = true;
                state.status_line =
                    "g pressed. Press g again for top, or use / to search".to_string();
            }
        }
        KeyCode::Char('n') => {
            state.pending_g = false;
            pager_next_match(state, false)
        }
        KeyCode::Char('N') => {
            state.pending_g = false;
            pager_next_match(state, true)
        }
        _ => {
            state.pending_g = false;
            state.status_line =
                "j/k move  Ctrl+D/Ctrl+U page  gg/G top/bottom  / search  n/N next/prev  Esc close"
                    .to_string();
        }
    }
    Ok(false)
}

fn update_pager_search(state: &mut PagerState, query: &str) {
    let normalized = query.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        state.search_query = None;
        state.search_matches.clear();
        state.search_index = 0;
        state.status_line =
            "Search cleared. j/k move  Ctrl+D/Ctrl+U page  gg/G top/bottom  Esc close".to_string();
        return;
    }
    state.search_query = Some(query.to_string());
    state.search_matches = state
        .lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.to_ascii_lowercase().contains(&normalized))
        .map(|(index, _)| index)
        .collect();
    state.search_index = 0;
    if let Some(first_match) = state.search_matches.first().copied() {
        state.offset = first_match;
        state.status_line = format!(
            "Match 1/{} for '{}'",
            state.search_matches.len(),
            query.trim()
        );
    } else {
        state.status_line = format!("No matches for '{}'", query.trim());
    }
}

fn pager_next_match(state: &mut PagerState, reverse: bool) {
    if state.search_matches.is_empty() {
        state.status_line = "No active search. Press / to search".to_string();
        return;
    }
    if reverse {
        if state.search_index == 0 {
            state.search_index = state.search_matches.len() - 1;
        } else {
            state.search_index -= 1;
        }
    } else {
        state.search_index = (state.search_index + 1) % state.search_matches.len();
    }
    state.offset = state.search_matches[state.search_index];
    state.status_line = format!(
        "Match {}/{} for '{}'",
        state.search_index + 1,
        state.search_matches.len(),
        state.search_query.as_deref().unwrap_or_default().trim()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_shortcuts_require_the_expected_modifiers() {
        assert_eq!(
            diagnostic_pager_shortcut(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(DiagnosticPagerShortcut::Close)
        );
        assert_eq!(
            diagnostic_pager_shortcut(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)),
            Some(DiagnosticPagerShortcut::Copy)
        );
        assert_eq!(
            diagnostic_pager_shortcut(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
            Some(DiagnosticPagerShortcut::ExternalPager)
        );

        for (code, modifiers) in [
            (KeyCode::Char('c'), KeyModifiers::ALT),
            (KeyCode::Char('c'), KeyModifiers::SHIFT),
            (KeyCode::Char('e'), KeyModifiers::CONTROL),
            (KeyCode::Char('e'), KeyModifiers::ALT),
            (KeyCode::Char('e'), KeyModifiers::SHIFT),
        ] {
            assert_eq!(
                diagnostic_pager_shortcut(KeyEvent::new(code, modifiers)),
                None
            );
        }
    }
}
