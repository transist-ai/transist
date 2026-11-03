// SPDX-License-Identifier: MPL-2.0
// SAFETY: OS-integration: terminal size detection uses libc ioctl.
#![allow(unsafe_code)]

use std::io::{self, IsTerminal};
use std::mem::MaybeUninit;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    DEFAULT_TERMINAL_COLUMNS, DEFAULT_TERMINAL_ROWS, LIVE_PREVIEW_MAX_CHARS, MIN_TERMINAL_COLUMNS,
    MIN_TERMINAL_ROWS, TERMINAL_EDGE_MARGIN_COLUMNS, TERMINAL_EDGE_MARGIN_ROWS,
};

pub(crate) fn terminal_supports_redraw() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let term = term.trim();
    !term.is_empty() && term != "dumb"
}

pub(crate) fn format_preview_text(raw: &str, max_chars: usize) -> Option<String> {
    let mut normalized = raw
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            _ if ch.is_control() => ' ',
            _ => ch,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    normalized = normalized
        .trim_end_matches([':', ';', ',', '.', '!', '?'])
        .trim()
        .to_string();
    if normalized.is_empty() {
        return None;
    }
    let effective_limit = max_chars.max(12);
    let char_count = normalized.chars().count();
    let mut snippet = if char_count <= effective_limit {
        normalized
    } else {
        normalized.chars().take(effective_limit).collect::<String>()
    };
    if !snippet.ends_with("...") {
        snippet.push_str("...");
    }
    Some(snippet)
}

pub(crate) fn working_preview_text(tool_name: &str) -> String {
    format!("{}: \"Working...\"", tool_name)
}

pub(crate) fn preview_body_limit(tool_name: &str) -> usize {
    let terminal_budget = current_terminal_columns()
        .saturating_sub(tool_name.chars().count() + 6)
        .max(12);
    LIVE_PREVIEW_MAX_CHARS.min(terminal_budget)
}

pub(crate) fn rendered_terminal_rows(lines: &[String]) -> usize {
    rendered_terminal_rows_for_columns(lines, current_terminal_columns())
}

pub(crate) fn rendered_terminal_rows_for_columns(lines: &[String], columns: usize) -> usize {
    lines
        .iter()
        .map(|line| rendered_terminal_rows_for_line(line, columns))
        .sum()
}

pub(crate) fn rendered_terminal_rows_for_line(line: &str, columns: usize) -> usize {
    let columns = columns.max(1);
    let width = visible_terminal_text_width(line);
    width.saturating_sub(1) / columns + 1
}

pub(crate) fn visible_terminal_text_width(line: &str) -> usize {
    strip_ansi_for_terminal_measurement(line)
        .chars()
        .count()
        .max(1)
}

pub(crate) fn strip_ansi_for_terminal_measurement(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            let _ = chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

pub(crate) fn current_terminal_columns() -> usize {
    if !io::stderr().is_terminal() {
        return 4096;
    }
    let detected = stderr_terminal_columns()
        .or_else(env_terminal_columns)
        .unwrap_or(DEFAULT_TERMINAL_COLUMNS);
    safe_terminal_columns(detected)
}

pub(crate) fn current_terminal_rows() -> usize {
    let detected = stderr_terminal_rows()
        .or_else(env_terminal_rows)
        .unwrap_or(DEFAULT_TERMINAL_ROWS);
    safe_terminal_rows(detected)
}

pub(crate) fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn safe_terminal_rows(raw_rows: usize) -> usize {
    raw_rows
        .saturating_sub(TERMINAL_EDGE_MARGIN_ROWS)
        .max(MIN_TERMINAL_ROWS)
}

pub(crate) fn safe_terminal_columns(raw_columns: usize) -> usize {
    raw_columns
        .saturating_sub(TERMINAL_EDGE_MARGIN_COLUMNS)
        .max(MIN_TERMINAL_COLUMNS)
}

pub(crate) fn env_terminal_columns() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value >= MIN_TERMINAL_COLUMNS)
}

pub(crate) fn env_terminal_rows() -> Option<usize> {
    std::env::var("LINES")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value >= MIN_TERMINAL_ROWS)
}

pub(crate) fn stderr_terminal_columns() -> Option<usize> {
    if !io::stderr().is_terminal() {
        return None;
    }
    #[cfg(unix)]
    {
        let mut winsize = MaybeUninit::<libc::winsize>::zeroed();
        let status =
            // SAFETY: ioctl(TIOCGWINSZ) writes the window size into the zeroed
            // winsize buffer; the pointer and size match libc::winsize, and the
            // fd is stderr confirmed as a terminal. Read only when ioctl returns 0.
            unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) };
        if status == 0 {
            // SAFETY: assume_init is justified only because status == 0 proves ioctl
            // wrote the complete winsize struct; on failure the caller does not reach here.
            let winsize = unsafe { winsize.assume_init() };
            return Some(winsize.ws_col as usize).filter(|value| *value >= MIN_TERMINAL_COLUMNS);
        }
    }
    None
}

pub(crate) fn stderr_terminal_rows() -> Option<usize> {
    if !io::stderr().is_terminal() {
        return None;
    }
    #[cfg(unix)]
    {
        let mut winsize = MaybeUninit::<libc::winsize>::zeroed();
        let status =
            // SAFETY: ioctl(TIOCGWINSZ) writes the size into the zeroed winsize
            // buffer; the fd is stderr confirmed as a terminal. Read on status==0.
            unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) };
        if status == 0 {
            // SAFETY: assume_init is justified by status == 0, which ioctl returns only
            // after writing the complete winsize struct into the buffer.
            let winsize = unsafe { winsize.assume_init() };
            return Some(winsize.ws_row as usize).filter(|value| *value >= MIN_TERMINAL_ROWS);
        }
    }
    None
}
