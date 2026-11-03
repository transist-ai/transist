// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::io::IsTerminal;
use std::sync::{Arc, atomic::AtomicBool};

use crate::cli::CliUiMode;

/// Entry point for the explicit `transist runs watch <id>` (and `runs show --tui`)
/// commands. Always opens the fullscreen run-detail viewer; errors if the
/// terminal can't host it.
pub(crate) fn run_runs_watch(context: &transist::TransistContext, run_id: &str) -> Result<()> {
    crate::ui::fullscreen::run_runs_watch(context, run_id)
}

/// Auto-attaches the fullscreen run-detail viewer to a just-spawned detached
/// runner.
///
/// Behavior:
/// - On a TTY with fullscreen UI available: opens `run_run_detail_screen`
///   pointed at `run_id`. The screen polls on-disk run state, shows live
///   progress, and surfaces the action picker on terminal status.
/// - When stdout is not a TTY (CI, pipes, managed mode, redirected output):
///   returns `Ok(())` immediately so the caller can exit cleanly after
///   announcing the spawn.
/// - When fullscreen UI is unavailable (e.g. `TERM=dumb`): returns `Ok(())`
///   immediately. Users on dumb terminals must re-attach explicitly via
///   `transist runs watch <id>` from a capable terminal.
///
/// Quitting the viewer (q / Esc / Ctrl+C with "no" on the cancel prompt)
/// only closes the viewer; the detached runner continues executing. Picking
/// `x` (or answering "yes" on the Ctrl+C prompt) writes a cancellation
/// request that the runner consumes through its on-disk control channel.
///
/// **Top-level entry only.** Callers already inside a fullscreen TUI must
/// use [`attach_run_viewer_with_prompt`] instead so they don't construct a
/// nested `FullscreenPrompt` (which would tear the parent terminal state
/// down on unwind).
pub(crate) fn attach_run_viewer(context: &transist::TransistContext, run_id: &str) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }
    if !crate::ui::fullscreen::fullscreen_ui_available() {
        return Ok(());
    }
    crate::ui::fullscreen::run_runs_watch(context, run_id)
}

pub(crate) fn maybe_attach_run_viewer(
    context: &transist::TransistContext,
    run_id: &str,
    ui_mode: CliUiMode,
) -> Result<()> {
    if !crate::ui::should_use_tui(ui_mode) {
        return Ok(());
    }
    attach_run_viewer(context, run_id)
}

/// Re-attach the fullscreen run-detail viewer for a freshly spawned runner
/// **from inside an already-active TUI** (e.g. after a gate decision or a
/// dashboard launch). Reuses the caller's `FullscreenPrompt` instead of
/// constructing a new one. This is critical: a nested `FullscreenPrompt`
/// would, on `Drop`, call `restore_terminal_state()` and break the
/// parent screen — symptoms include Esc echoed as `^[`, frozen UI, and
/// no key handling.
///
/// Errors from the inner `run_run_detail_screen` propagate to the caller
/// unchanged.
pub(crate) fn attach_run_viewer_with_prompt(
    prompt: &mut crate::ui::fullscreen::FullscreenPrompt,
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }
    if !crate::ui::fullscreen::fullscreen_ui_available() {
        return Ok(());
    }
    // Use a fresh non-cancelling interrupt flag for the re-attached viewer
    // so `Esc` confirms quit-the-viewer (not cancel-the-runner). The
    // detached runner has its own cancellation channel — see
    // `request_run_cancellation_with_confirmation`.
    let interrupt_flag = Arc::new(AtomicBool::new(false));
    crate::ui::fullscreen::run_run_detail_screen_reusing_prompt(
        prompt,
        context,
        &interrupt_flag,
        run_id,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_mode_skips_auto_attach() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
        context.init().expect("init");

        maybe_attach_run_viewer(&context, "demo-run", CliUiMode::Plain)
            .expect("plain mode should skip attach");
    }
}
