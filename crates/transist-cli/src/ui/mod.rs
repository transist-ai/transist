// SPDX-License-Identifier: MPL-2.0
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};

use crate::cli::CliUiMode;

pub(crate) mod fullscreen;
pub(crate) mod watch;

pub(crate) fn should_use_tui(ui_mode: CliUiMode) -> bool {
    match ui_mode {
        CliUiMode::Tui => true,
        CliUiMode::Plain => false,
    }
}

pub(crate) fn run_root_entry(
    ui_mode: CliUiMode,
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    interrupt_flag: &Arc<AtomicBool>,
    plain_fallback: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<i32> {
    let use_tui = should_use_tui(ui_mode);

    if use_tui && fullscreen::fullscreen_ui_available() {
        fullscreen::run_root_dashboard(repo_root, cwd, explicit_home, interrupt_flag)?;
        return Ok(0);
    }
    if use_tui {
        anyhow::bail!("tui mode requires an interactive terminal with redraw support");
    }
    plain_fallback()?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_mode_never_selects_tui() {
        assert!(!should_use_tui(CliUiMode::Plain));
    }

    #[test]
    fn tui_mode_always_selects_tui() {
        assert!(should_use_tui(CliUiMode::Tui));
    }
}
