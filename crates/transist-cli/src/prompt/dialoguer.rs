// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use dialoguer::{
    Input,
    console::{Key, Term},
};
use std::io::{self, IsTerminal, Write};

use crate::display::output::format_suggested_action_list;
use crate::display::summary::SuggestedAction;
use crate::pager::RenderedTerminalBlock;
use crate::pager::{
    clear_rendered_live_block, discard_pending_terminal_input, picker_active_item_style,
    terminal_written_block_lines,
};
use crate::prompt::{
    PickerSelection, PromptInputOutcome, PromptUi, dialoguer_prompt_was_dismissed,
    prompt_was_dismissed,
};
use crate::terminal::terminal_supports_redraw;

#[derive(Default)]
pub(crate) struct DialoguerPrompt;

impl PromptUi for DialoguerPrompt {
    fn picker_available(&self) -> bool {
        io::stdin().is_terminal() && io::stderr().is_terminal() && terminal_supports_redraw()
    }

    fn confirmation_available(&self) -> bool {
        confirmation_available_for_terminals(
            io::stdin().is_terminal(),
            io::stdout().is_terminal(),
            io::stderr().is_terminal(),
        )
    }

    fn select_suggested_action(
        &mut self,
        actions: &[SuggestedAction],
        selected_block_title: &str,
    ) -> Result<PickerSelection> {
        interact_suggested_action_picker(actions, selected_block_title)
    }

    fn prompt_optional_reason(&mut self, prompt: &str) -> Result<PromptInputOutcome> {
        discard_pending_terminal_input();
        match Input::<String>::new()
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text()
        {
            Ok(reason) => {
                clear_text_prompt_footprint(prompt, Some(&reason));
                Ok(PromptInputOutcome::Submitted(reason))
            }
            Err(error) if dialoguer_prompt_was_dismissed(&error) => {
                clear_text_prompt_footprint(prompt, None);
                Ok(PromptInputOutcome::Canceled)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn confirm_suggested_action(&mut self, title: &str, message: &str) -> Result<bool> {
        prompt_confirmation(&format!("{title}: {message} [y/N]"))
    }

    fn confirm_exit_transist(&mut self) -> Result<bool> {
        prompt_confirmation("Interrupt received. Exit the run and return to the shell? [y/N]")
    }

    fn present_paged_text(&mut self, title: &str, body: &str) -> Result<()> {
        crate::pager::present_built_in_pager(title, body)
    }
}

/// Returns whether a confirmation can follow output that the operator can see.
fn confirmation_available_for_terminals(stdin: bool, stdout: bool, stderr: bool) -> bool {
    stdin && stdout && stderr
}

fn interact_suggested_action_picker(
    actions: &[SuggestedAction],
    selected_block_title: &str,
) -> Result<PickerSelection> {
    let term = Term::stderr();
    if !term.is_term() {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "not a terminal").into());
    }
    if actions.is_empty() {
        return Ok(PickerSelection::default());
    }

    let items = numbered_picker_items(actions);
    let mut selected = 0usize;
    let mut rendered_lines = RenderedTerminalBlock::default();
    discard_pending_terminal_input();
    term.hide_cursor()?;

    loop {
        if !rendered_lines.is_empty() {
            clear_rendered_live_block(&rendered_lines);
        }
        let lines = render_picker_screen_lines(&items, selected);
        for line in &lines {
            term.write_line(line)?;
        }
        term.flush()?;
        rendered_lines = terminal_written_block_lines(lines);

        match term.read_key() {
            Ok(Key::ArrowDown | Key::Tab | Key::Char('j')) => {
                selected = (selected + 1) % items.len();
            }
            Ok(Key::ArrowUp | Key::BackTab | Key::Char('k')) => {
                selected = (selected + items.len() - 1) % items.len();
            }
            Ok(Key::Enter | Key::Char(' ')) => {
                if !rendered_lines.is_empty() {
                    clear_rendered_live_block(&rendered_lines);
                }
                let selected_lines =
                    render_selected_picker_screen_lines(actions, selected, selected_block_title);
                for line in &selected_lines {
                    term.write_line(line)?;
                }
                term.flush()?;
                finish_suggested_action_picker(&term)?;
                return Ok(PickerSelection {
                    selected_index: Some(selected),
                    dismissed_by_interrupt: false,
                    rendered_lines: terminal_written_block_lines(selected_lines),
                });
            }
            Ok(Key::Escape | Key::Char('q')) => {
                finish_suggested_action_picker(&term)?;
                return Ok(PickerSelection {
                    selected_index: None,
                    dismissed_by_interrupt: false,
                    rendered_lines,
                });
            }
            Ok(Key::CtrlC) => {
                finish_suggested_action_picker(&term)?;
                return Ok(PickerSelection {
                    selected_index: None,
                    dismissed_by_interrupt: true,
                    rendered_lines,
                });
            }
            Ok(Key::Char(ch)) => {
                if let Some(index) = suggested_action_shortcut_index(ch, items.len()) {
                    if !rendered_lines.is_empty() {
                        clear_rendered_live_block(&rendered_lines);
                    }
                    let selected_lines =
                        render_selected_picker_screen_lines(actions, index, selected_block_title);
                    for line in &selected_lines {
                        term.write_line(line)?;
                    }
                    term.flush()?;
                    finish_suggested_action_picker(&term)?;
                    return Ok(PickerSelection {
                        selected_index: Some(index),
                        dismissed_by_interrupt: false,
                        rendered_lines: terminal_written_block_lines(selected_lines),
                    });
                }
            }
            Ok(_) => {}
            Err(error) if prompt_was_dismissed(&error) => {
                finish_suggested_action_picker(&term)?;
                return Ok(PickerSelection {
                    selected_index: None,
                    dismissed_by_interrupt: picker_dismissed_by_interrupt(&error),
                    rendered_lines,
                });
            }
            Err(error) => {
                finish_suggested_action_picker(&term)?;
                return Err(error.into());
            }
        }
    }
}

fn finish_suggested_action_picker(term: &Term) -> io::Result<()> {
    term.show_cursor()?;
    term.flush()
}

pub(crate) fn picker_dismissed_by_interrupt(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Interrupted
}

pub(crate) fn render_picker_screen_lines(items: &[String], selected_index: usize) -> Vec<String> {
    picker_block_lines_from_items(items, selected_index)
}

fn render_selected_picker_screen_lines(
    actions: &[SuggestedAction],
    selected_index: usize,
    selected_block_title: &str,
) -> Vec<String> {
    format_suggested_action_list(
        selected_block_title,
        actions,
        Some((
            selected_index,
            actions[selected_index].display_command.as_str(),
        )),
    )
}

fn picker_block_lines_from_items(items: &[String], selected_index: usize) -> Vec<String> {
    let mut lines = vec![suggested_action_picker_prompt(items.len())];
    lines.extend(items.iter().enumerate().map(|(index, item)| {
        if index == selected_index {
            picker_active_item_style()
                .apply_to(format!("> {}", item))
                .to_string()
        } else {
            format!("  {}", item)
        }
    }));
    lines
}

pub(crate) fn numbered_picker_items(actions: &[SuggestedAction]) -> Vec<String> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| format!("{}. {}", index + 1, action.display_command))
        .collect()
}

fn suggested_action_picker_prompt(item_count: usize) -> String {
    if item_count <= 9 {
        format!(
            "Select a suggested command (press 1-{}, use arrows, or q/Esc to exit)",
            item_count
        )
    } else {
        "Select a suggested command (press a number, use arrows, or q/Esc to exit)".to_string()
    }
}

pub(crate) fn suggested_action_shortcut_index(ch: char, item_count: usize) -> Option<usize> {
    let digit = ch.to_digit(10)? as usize;
    if digit == 0 || digit > item_count {
        return None;
    }
    Some(digit - 1)
}

fn clear_text_prompt_footprint(prompt: &str, answer: Option<&str>) {
    if !io::stderr().is_terminal() {
        return;
    }
    let rendered = answer
        .map(|answer| format!("{}: {}", prompt, answer))
        .unwrap_or_else(|| format!("{}: ", prompt));
    clear_rendered_live_block(&terminal_written_block_lines(vec![rendered]));
    let _ = io::stderr().flush();
}

fn prompt_confirmation(prompt: &str) -> Result<bool> {
    discard_pending_terminal_input();
    loop {
        match Input::<String>::new()
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text()
        {
            Ok(raw) => {
                clear_text_prompt_footprint(prompt, Some(&raw));
                if let Some(confirmed) = parse_interrupt_confirmation(&raw) {
                    return Ok(confirmed);
                }
                eprintln!("Please answer y or n.");
            }
            Err(error) if dialoguer_prompt_was_dismissed(&error) => {
                clear_text_prompt_footprint(prompt, None);
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

pub(crate) fn parse_interrupt_confirmation(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" | "" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::confirmation_available_for_terminals;

    #[test]
    fn confirmation_requires_every_standard_stream_to_be_a_terminal() {
        assert!(confirmation_available_for_terminals(true, true, true));
        assert!(!confirmation_available_for_terminals(false, true, true));
        assert!(!confirmation_available_for_terminals(true, false, true));
        assert!(!confirmation_available_for_terminals(true, true, false));
    }
}
