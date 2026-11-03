// SPDX-License-Identifier: MPL-2.0
//! Fullscreen CLI UI module root.
//!
//! Acts as a thin façade: the actual `FullscreenPrompt` implementation,
//! supporting state types, terminal lifecycle, and prompt helpers live in
//! `prompt.rs`. Sibling modules access shared items via `super::*`, which
//! resolves through the `pub(super) use prompt::*;` re-export below.

mod components;
mod configuration;
mod dashboard;
pub(crate) mod diagnostics;
mod flow;
mod format;
mod pager;
mod problems;
mod prompt;
mod run_browser;
mod run_detail;
mod screens;
mod tui_debug;

use configuration::*;
use dashboard::*;
use flow::*;
use format::*;
use problems::*;
use prompt::*;
use run_browser::*;
use run_detail::*;

pub(crate) use prompt::{
    FullscreenPrompt, fullscreen_prompt_active, fullscreen_ui_available,
    maybe_default_fullscreen_prompt, restore_terminal_state, run_root_dashboard, run_runs_watch,
};
pub(crate) use screens::run_run_detail_screen_reusing_prompt;

#[cfg(test)]
mod tests;
