// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use serde::Deserialize;
use std::collections::VecDeque;

use crate::display::emit_lines;
use crate::display::summary::SuggestedAction;
use crate::pager::terminal_written_block_lines;
use crate::prompt::dialoguer::{numbered_picker_items, render_picker_screen_lines};
use crate::prompt::{PickerSelection, PromptInputOutcome, PromptUi};

#[cfg(debug_assertions)]
#[derive(Debug, Default, Deserialize)]
pub(crate) struct TestPromptScript {
    #[serde(default)]
    pub(crate) picker_available: Option<bool>,
    #[serde(default)]
    pub(crate) selections: Vec<Option<usize>>,
    #[serde(default)]
    pub(crate) picker_interruptions: Vec<bool>,
    #[serde(default)]
    pub(crate) reasons: Vec<Option<String>>,
    #[serde(default)]
    pub(crate) exit_confirmations: Vec<bool>,
    #[serde(default)]
    pub(crate) action_confirmations: Vec<bool>,
}

#[derive(Debug, Default)]
pub(crate) struct ScriptedPrompt {
    pub(crate) picker_available: bool,
    pub(crate) selections: VecDeque<Option<usize>>,
    pub(crate) picker_interruptions: VecDeque<bool>,
    pub(crate) reasons: VecDeque<Option<String>>,
    pub(crate) exit_confirmations: VecDeque<bool>,
    pub(crate) action_confirmations: VecDeque<bool>,
    pub(crate) paged_text_presentations: Vec<(String, String)>,
    pub(crate) loading_events: Vec<Option<String>>,
}

impl ScriptedPrompt {
    pub(crate) fn from_script(
        picker_available: bool,
        selections: Vec<Option<usize>>,
        picker_interruptions: Vec<bool>,
        reasons: Vec<Option<String>>,
        exit_confirmations: Vec<bool>,
        action_confirmations: Vec<bool>,
    ) -> Self {
        Self {
            picker_available,
            selections: selections.into(),
            picker_interruptions: picker_interruptions.into(),
            reasons: reasons.into(),
            exit_confirmations: exit_confirmations.into(),
            action_confirmations: action_confirmations.into(),
            paged_text_presentations: Vec::new(),
            loading_events: Vec::new(),
        }
    }
}

impl PromptUi for ScriptedPrompt {
    fn picker_available(&self) -> bool {
        self.picker_available
    }

    fn confirmation_available(&self) -> bool {
        self.picker_available
    }

    fn select_suggested_action(
        &mut self,
        actions: &[SuggestedAction],
        _selected_block_title: &str,
    ) -> Result<PickerSelection> {
        let selection = self.selections.pop_front().unwrap_or(None);
        if actions.is_empty() {
            return Ok(PickerSelection::default());
        }
        let selected = selection.unwrap_or(0).min(actions.len().saturating_sub(1));
        let rendered_lines = render_picker_screen_lines(&numbered_picker_items(actions), selected);
        emit_lines(&rendered_lines);
        Ok(PickerSelection {
            selected_index: selection,
            dismissed_by_interrupt: selection.is_none()
                && self.picker_interruptions.pop_front().unwrap_or(false),
            rendered_lines: terminal_written_block_lines(rendered_lines),
        })
    }

    fn prompt_optional_reason(&mut self, _prompt: &str) -> Result<PromptInputOutcome> {
        Ok(match self.reasons.pop_front() {
            Some(Some(reason)) => PromptInputOutcome::Submitted(reason),
            Some(None) => PromptInputOutcome::Canceled,
            None => PromptInputOutcome::Submitted(String::new()),
        })
    }

    fn confirm_suggested_action(&mut self, _title: &str, _message: &str) -> Result<bool> {
        Ok(self.action_confirmations.pop_front().unwrap_or(true))
    }

    fn confirm_exit_transist(&mut self) -> Result<bool> {
        Ok(self.exit_confirmations.pop_front().unwrap_or(true))
    }

    fn present_paged_text(&mut self, title: &str, body: &str) -> Result<()> {
        self.paged_text_presentations
            .push((title.to_string(), body.to_string()));
        eprintln!("{}", title);
        if !body.is_empty() {
            eprintln!("{}", body);
        }
        Ok(())
    }

    fn set_loading_message(&mut self, message: Option<&str>) -> Result<()> {
        self.loading_events.push(message.map(str::to_string));
        Ok(())
    }
}
