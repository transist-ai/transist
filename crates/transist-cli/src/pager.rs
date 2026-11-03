// SPDX-License-Identifier: MPL-2.0

use anyhow::Result;
use dialoguer::console::{Key, Style, Term};
use std::io::{self, IsTerminal, Write};

use crate::prompt::prompt_was_dismissed;
use crate::terminal::{
    current_terminal_columns, current_terminal_rows, rendered_terminal_rows,
    terminal_supports_redraw,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RenderedTerminalBlock {
    pub(crate) lines: Vec<String>,
    pub(crate) rendered_rows: usize,
}

impl RenderedTerminalBlock {
    pub(crate) fn from_rendered_lines(lines: Vec<String>) -> Self {
        let rendered_rows = rendered_terminal_rows(&lines);
        Self {
            lines,
            rendered_rows,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rendered_rows == 0
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn discard_terminal_input_unix() {
    // SAFETY: `tcflush(STDIN_FILENO, TCIFLUSH)` discards pending input on the
    // terminal; it reads no memory and the fd is the standard input descriptor.
    unsafe {
        let _ = libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
    }
}

pub(crate) fn discard_pending_terminal_input() {
    if !io::stdin().is_terminal() {
        return;
    }
    #[cfg(unix)]
    {
        discard_terminal_input_unix();
    }
    #[cfg(not(unix))]
    {
        // No-op on non-Unix platforms.
    }
}

pub(crate) fn present_built_in_pager(title: &str, body: &str) -> Result<()> {
    let term = Term::stderr();
    if !term.is_term() {
        eprintln!("{}", title);
        if !body.is_empty() {
            eprintln!("{}", body);
        }
        return Ok(());
    }

    let wrapped_body = wrap_pager_body(body, current_terminal_columns());
    let page_height = current_terminal_rows().saturating_sub(2).max(4);
    let body_height = pager_body_height(page_height);
    let mut offset = 0usize;
    let mut rendered_rows = 0usize;
    discard_pending_terminal_input();
    term.hide_cursor()?;

    loop {
        if rendered_rows > 0 {
            term.clear_last_lines(rendered_rows)?;
        }
        let lines = render_pager_lines(title, &wrapped_body, offset, page_height);
        for line in &lines {
            term.write_line(line)?;
        }
        term.flush()?;
        rendered_rows = rendered_terminal_rows(&lines);

        match term.read_key() {
            Ok(Key::ArrowDown | Key::Char('j')) => {
                offset = pager_next_offset(offset, 1, wrapped_body.len(), body_height);
            }
            Ok(Key::ArrowUp | Key::Char('k')) => {
                offset = offset.saturating_sub(1);
            }
            Ok(Key::PageDown | Key::Char('f') | Key::Char(' ')) => {
                offset = pager_next_offset(offset, body_height, wrapped_body.len(), body_height);
            }
            Ok(Key::PageUp | Key::Char('b')) => {
                offset = offset.saturating_sub(body_height);
            }
            Ok(Key::Home | Key::Char('g')) => {
                offset = 0;
            }
            Ok(Key::End | Key::Char('G')) => {
                offset = pager_max_offset(wrapped_body.len(), body_height);
            }
            Ok(Key::Enter | Key::Escape | Key::Char('q') | Key::CtrlC) => {
                if rendered_rows > 0 {
                    term.clear_last_lines(rendered_rows)?;
                }
                term.show_cursor()?;
                term.flush()?;
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if prompt_was_dismissed(&error) => {
                if rendered_rows > 0 {
                    term.clear_last_lines(rendered_rows)?;
                }
                term.show_cursor()?;
                term.flush()?;
                return Ok(());
            }
            Err(error) => {
                if rendered_rows > 0 {
                    term.clear_last_lines(rendered_rows)?;
                }
                term.show_cursor()?;
                term.flush()?;
                return Err(error.into());
            }
        }
    }
}

pub(crate) fn render_pager_lines(
    title: &str,
    body_lines: &[String],
    offset: usize,
    page_height: usize,
) -> Vec<String> {
    let body_height = pager_body_height(page_height);
    let max_offset = pager_max_offset(body_lines.len(), body_height);
    let offset = offset.min(max_offset);
    let end = offset.saturating_add(body_height).min(body_lines.len());
    let range_end = end
        .max(offset.saturating_add(1))
        .min(body_lines.len().max(1));
    let mut lines = vec![
        picker_active_item_style().apply_to(title).to_string(),
        Style::new()
            .for_stderr()
            .dim()
            .apply_to(format!(
                "Lines {}-{} of {}  •  j/k or arrows to move, space/b page, q to return",
                offset.saturating_add(1).min(body_lines.len().max(1)),
                range_end,
                body_lines.len().max(1),
            ))
            .to_string(),
    ];
    if body_lines.is_empty() {
        lines.push("(no data)".to_string());
        return lines;
    }
    lines.extend(body_lines[offset..end].iter().cloned());
    lines
}

pub(crate) fn pager_body_height(page_height: usize) -> usize {
    page_height.saturating_sub(2).max(1)
}

pub(crate) fn body_exceeds_pager_page(body: &str, columns: usize, rows: usize) -> bool {
    let wrapped_body = wrap_pager_body(body, columns);
    let page_height = rows.saturating_sub(2).max(4);
    wrapped_body.len() > pager_body_height(page_height)
}

pub(crate) fn should_page_command_output(body: &str) -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && io::stderr().is_terminal()
        && terminal_supports_redraw()
        && body_exceeds_pager_page(body, current_terminal_columns(), current_terminal_rows())
}

pub(crate) fn wrap_pager_body(body: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(1);
    let mut lines = Vec::new();
    for raw_line in body.lines() {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        // Mirror `should_preserve_unwrapped_terminal_line` from the spinner
        // so paged output keeps single-quoted absolute or relative paths
        // intact instead of hard-wrapping them mid-character.
        if pager_line_should_preserve_path(raw_line) {
            lines.push(raw_line.to_string());
            continue;
        }
        let chars = raw_line.chars().collect::<Vec<_>>();
        let mut start = 0usize;
        while start < chars.len() {
            let end = (start + columns).min(chars.len());
            lines.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn pager_line_should_preserve_path(line: &str) -> bool {
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

pub(crate) fn pager_max_offset(total_lines: usize, page_height: usize) -> usize {
    total_lines.saturating_sub(page_height.max(1))
}

pub(crate) fn pager_next_offset(
    offset: usize,
    delta: usize,
    total_lines: usize,
    page_height: usize,
) -> usize {
    offset
        .saturating_add(delta)
        .min(pager_max_offset(total_lines, page_height))
}

pub(crate) fn picker_active_item_style() -> Style {
    let style = Style::new().for_stderr().bold();
    if std::env::var_os("NO_COLOR").is_some() {
        style
    } else {
        style.cyan()
    }
}

pub(crate) fn terminal_written_block_lines(mut lines: Vec<String>) -> RenderedTerminalBlock {
    lines.push(String::new());
    RenderedTerminalBlock::from_rendered_lines(lines)
}

pub(crate) fn clear_rendered_live_block(previous_block: &RenderedTerminalBlock) {
    if previous_block.rendered_rows == 0 {
        return;
    }
    eprint!("\r\x1b[2K");
    for _ in 1..previous_block.rendered_rows {
        eprint!("\x1b[1A\r\x1b[2K");
    }
}

pub(crate) fn clear_recent_terminal_lines(block: &RenderedTerminalBlock) -> Result<()> {
    if block.is_empty() || !io::stderr().is_terminal() {
        return Ok(());
    }
    clear_rendered_live_block(block);
    io::stderr().flush()?;
    Ok(())
}
