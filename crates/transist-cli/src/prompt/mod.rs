// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::cli::CliUiMode;
use crate::display::emit_lines;
use crate::display::spinner::LiveProgress;
use crate::display::summary::SuggestedAction;
use crate::pager::RenderedTerminalBlock;
use crate::pager::clear_recent_terminal_lines;

pub(crate) mod dialoguer;
#[cfg(any(test, debug_assertions))]
pub(crate) mod scripted;

pub(crate) trait PromptUi {
    fn picker_available(&self) -> bool;
    /// Whether this backend can ask the operator to confirm a mutation.
    fn confirmation_available(&self) -> bool {
        false
    }
    fn select_suggested_action(
        &mut self,
        actions: &[SuggestedAction],
        selected_block_title: &str,
    ) -> Result<PickerSelection>;
    fn prompt_optional_reason(&mut self, prompt: &str) -> Result<PromptInputOutcome>;
    /// Variant of [`Self::prompt_optional_reason`] that lets callers pass
    /// an opaque `key` identifying *which* multi-step flow this reason
    /// belongs to (for the gate flow: action kind + run + gate). Backends
    /// that support draft preservation across Esc-out + re-entry use the
    /// key to scope drafts so two unrelated reason prompts cannot leak
    /// text into each other. The default delegates to
    /// `prompt_optional_reason` and ignores the key — backends that don't
    /// preserve drafts (cooked-mode dialoguer, scripted) need no change.
    fn prompt_optional_reason_keyed(
        &mut self,
        prompt: &str,
        _key: &str,
    ) -> Result<PromptInputOutcome> {
        self.prompt_optional_reason(prompt)
    }
    fn confirm_suggested_action(&mut self, title: &str, message: &str) -> Result<bool>;
    fn confirm_exit_transist(&mut self) -> Result<bool>;
    fn present_paged_text(&mut self, title: &str, body: &str) -> Result<()>;
    /// Best-effort loading-state hook for prompt backends that can surface a
    /// transient in-progress UI while a slow read-only command gathers data.
    /// Backends that cannot or should not render loading feedback keep the
    /// default no-op implementation.
    fn set_loading_message(&mut self, _message: Option<&str>) -> Result<()> {
        Ok(())
    }
    fn prefers_pager(&self) -> bool {
        false
    }
    fn show_summary_progress(
        &mut self,
        progress: Option<&LiveProgress>,
        fallback_lines: &[String],
    ) -> Result<()> {
        if let Some(progress) = progress {
            self.emit_plain_lines(&progress.render_active_step_lines_plain())
        } else {
            self.emit_plain_lines(fallback_lines)
        }
    }
    fn clear_rendered_frame(&mut self, block: &RenderedTerminalBlock) -> Result<()> {
        clear_recent_terminal_lines(block)
    }
    fn emit_plain_lines(&mut self, lines: &[String]) -> Result<()> {
        emit_lines(lines);
        Ok(())
    }

    /// Re-attach the run-detail viewer for `run_id` after a child runner
    /// has been spawned, **reusing this prompt** rather than constructing
    /// a nested fullscreen terminal owner.
    ///
    /// Returns:
    /// - `Ok(true)`  — the prompt handled the attach itself; the caller
    ///   must NOT also call `crate::ui::watch::attach_run_viewer`.
    /// - `Ok(false)` — the prompt cannot host an inline re-attach (e.g.
    ///   it is the cooked-mode `DialoguerPrompt` or a scripted test
    ///   prompt). The caller should fall back to the free
    ///   `attach_run_viewer` entry point.
    ///
    /// The default is `false` so non-fullscreen backends inherit the old
    /// behavior. The fullscreen backend overrides this to reuse the
    /// active `FullscreenPrompt` and avoid the nested-`Drop` terminal-
    /// tear bug (see `attach_run_viewer_with_prompt`).
    fn attach_run_viewer_inline(
        &mut self,
        _context: &transist::TransistContext,
        _run_id: &str,
    ) -> Result<bool> {
        Ok(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptInputOutcome {
    Submitted(String),
    Canceled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PickerSelection {
    pub(crate) selected_index: Option<usize>,
    pub(crate) dismissed_by_interrupt: bool,
    pub(crate) rendered_lines: RenderedTerminalBlock,
}

pub(crate) fn default_prompt_backend(ui_mode: CliUiMode) -> Result<Box<dyn PromptUi>> {
    #[cfg(debug_assertions)]
    if let Some(raw) = std::env::var(crate::TEST_PROMPT_SCRIPT_ENV)
        .ok()
        .filter(|raw| !raw.trim().is_empty())
    {
        let script: scripted::TestPromptScript = serde_json::from_str(&raw)?;
        return Ok(Box::new(scripted::ScriptedPrompt::from_script(
            script.picker_available.unwrap_or(true),
            script.selections,
            script.picker_interruptions,
            script.reasons,
            script.exit_confirmations,
            script.action_confirmations,
        )));
    }

    if let Some(prompt) = crate::ui::fullscreen::maybe_default_fullscreen_prompt(ui_mode)? {
        return Ok(Box::new(prompt));
    }

    Ok(Box::new(dialoguer::DialoguerPrompt))
}

pub(crate) fn prompt_was_dismissed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
    )
}

pub(crate) fn dialoguer_prompt_was_dismissed(error: &::dialoguer::Error) -> bool {
    matches!(error, ::dialoguer::Error::IO(io_error) if prompt_was_dismissed(io_error))
}

const LOADING_FEEDBACK_THRESHOLD: Duration = Duration::from_millis(100);
const LOADING_FEEDBACK_MINIMUM_VISIBLE: Duration = Duration::from_millis(350);
const LOADING_FEEDBACK_TICK_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn run_with_loading_feedback<T>(
    prompt: &mut dyn PromptUi,
    message: &str,
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T>
where
    T: Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(operation());
    });

    let start = Instant::now();
    let mut shown_at: Option<Instant> = None;

    loop {
        match receiver.recv_timeout(LOADING_FEEDBACK_TICK_INTERVAL) {
            Ok(result) => {
                if let Some(shown_at) = shown_at {
                    while shown_at.elapsed() < LOADING_FEEDBACK_MINIMUM_VISIBLE {
                        prompt.set_loading_message(Some(message))?;
                        std::thread::sleep(LOADING_FEEDBACK_TICK_INTERVAL);
                    }
                }
                prompt.set_loading_message(None)?;
                return result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if start.elapsed() >= LOADING_FEEDBACK_THRESHOLD {
                    shown_at.get_or_insert_with(Instant::now);
                    prompt.set_loading_message(Some(message))?;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("loading operation ended unexpectedly")
            }
        }
    }
}

#[cfg(test)]
mod prompt_loading_tests {
    use super::*;

    #[derive(Default)]
    struct LoadingPrompt {
        events: Vec<(Option<String>, Instant)>,
    }

    impl PromptUi for LoadingPrompt {
        fn picker_available(&self) -> bool {
            false
        }

        fn confirmation_available(&self) -> bool {
            false
        }

        fn select_suggested_action(
            &mut self,
            _actions: &[SuggestedAction],
            _selected_block_title: &str,
        ) -> Result<PickerSelection> {
            Ok(PickerSelection::default())
        }

        fn prompt_optional_reason(&mut self, _prompt: &str) -> Result<PromptInputOutcome> {
            Ok(PromptInputOutcome::Canceled)
        }

        fn confirm_suggested_action(&mut self, _title: &str, _message: &str) -> Result<bool> {
            Ok(false)
        }

        fn confirm_exit_transist(&mut self) -> Result<bool> {
            Ok(false)
        }

        fn present_paged_text(&mut self, _title: &str, _body: &str) -> Result<()> {
            Ok(())
        }

        fn set_loading_message(&mut self, message: Option<&str>) -> Result<()> {
            self.events
                .push((message.map(str::to_string), Instant::now()));
            Ok(())
        }
    }

    #[test]
    fn loading_feedback_skips_modal_for_fast_operations() {
        let mut prompt = LoadingPrompt::default();

        let value = run_with_loading_feedback(&mut prompt, "Loading demo", || Ok(42))
            .expect("loading helper succeeds");

        assert_eq!(value, 42);
        assert_eq!(prompt.events.len(), 1);
        assert_eq!(prompt.events[0].0, None);
    }

    #[test]
    fn loading_feedback_keeps_visible_modal_for_minimum_duration_once_shown() {
        let mut prompt = LoadingPrompt::default();

        let value = run_with_loading_feedback(&mut prompt, "Loading demo", || {
            // Leave enough time for the feedback threshold to elapse before
            // the worker completes, even under a busy hosted runner.
            std::thread::sleep(Duration::from_millis(500));
            Ok(42)
        })
        .expect("loading helper succeeds");

        assert_eq!(value, 42);
        let shown_at = prompt
            .events
            .iter()
            .find_map(|(message, at)| (message.as_deref() == Some("Loading demo")).then_some(*at))
            .expect("loading message shown");
        let cleared_at = prompt
            .events
            .iter()
            .rev()
            .find_map(|(message, at)| message.is_none().then_some(*at))
            .expect("loading message cleared");

        assert!(
            cleared_at.duration_since(shown_at) >= Duration::from_millis(350),
            "expected loading modal to remain visible for at least 350ms"
        );
    }
}
