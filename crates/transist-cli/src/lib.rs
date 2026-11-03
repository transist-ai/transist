// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Library entry points for local Transist workflow execution and inspection.
//!
//! This binary owns user-facing flag parsing, terminal presentation, and wiring
//! into the root `transist` facade. It may depend on lower-level crates through
//! the facade, but it must not own core runtime, compiler, protocol, or storage
//! semantics itself.

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches};

mod actionable;
mod cli;
mod commands;
mod display;
mod execution;
mod extensions;
mod guidance;
mod input;
mod managed_exit_codes;
mod pager;
mod prompt;
mod runner;
mod signals;
mod terminal;
mod ui;

use cli::Cli;
pub use cli::CliSurface;
#[cfg(test)]
use cli::{Commands, RunsSubcommand, WorkflowSubcommand};
pub use cli::{InspectionSubcommand, InspectionSyncMode};
use commands::dispatch::dispatch;
pub use extensions::{
    CliExtensions, InspectionCommandResponse, InspectionSyncConfigureRequest,
    InspectionSyncSummary, install_cli_extensions,
};
pub use managed_exit_codes::{
    CanceledOrInterruptedError, InternalError, UsageError, WorkflowValidationError,
    cli_error_chain_contains, is_interrupted_io_error,
};
use managed_exit_codes::{
    wrap_canceled_or_interrupted_error, wrap_internal_error, wrap_usage_error,
};
use prompt::default_prompt_backend;
use signals::{
    force_restore_terminal, install_interrupt_handler, install_terminal_restore_panic_hook,
    print_fatal_error,
};

pub(crate) use extensions::{
    handle_inspection_command as handle_inspection_command_extension, inspection_sync_available,
    load_inspection_sync_summary as load_inspection_sync_summary_extension,
};

pub(crate) const LIVE_PROGRESS_FILE_NAME: &str = ".transist-live-progress.json";
pub(crate) const LIVE_PREVIEW_MAX_CHARS: usize = 150;
pub(crate) const DEFAULT_TERMINAL_COLUMNS: usize = 80;
pub(crate) const DEFAULT_TERMINAL_ROWS: usize = 24;
pub(crate) const MIN_TERMINAL_COLUMNS: usize = 20;
pub(crate) const MIN_TERMINAL_ROWS: usize = 8;
pub(crate) const TERMINAL_EDGE_MARGIN_COLUMNS: usize = 1;
pub(crate) const TERMINAL_EDGE_MARGIN_ROWS: usize = 2;
pub(crate) const FAILED_STEP_EVENT_PAGER_LIMIT: usize = 80;
pub(crate) const DISPLAY_REASON_MAX_CHARS: usize = 80;
#[cfg(debug_assertions)]
pub(crate) const TEST_PROMPT_SCRIPT_ENV: &str = "TRANSIST_CLI_TEST_PROMPT";

/// Classifies a CLI failure into a process exit code.
pub trait ExitCodeClassifier {
    /// Returns the process exit code for `error`.
    fn exit_code(&self, error: &anyhow::Error) -> i32;

    /// Returns whether the caller should suppress the human fatal-error renderer.
    fn suppress_fatal_render(&self) -> bool {
        false
    }
}

/// Default exit-code classifier: success is zero and every failure exits with `1`.
#[derive(Debug, Default, Clone, Copy)]
pub struct OssExitCodeClassifier;

impl ExitCodeClassifier for OssExitCodeClassifier {
    fn exit_code(&self, _error: &anyhow::Error) -> i32 {
        1
    }
}

/// Build-local exit-code classifier.
#[derive(Debug, Default, Clone, Copy)]
pub struct BuildExitCodeClassifier;

impl ExitCodeClassifier for BuildExitCodeClassifier {
    fn exit_code(&self, error: &anyhow::Error) -> i32 {
        let _ = error;
        1
    }
}

/// Runs the CLI for the requested surface and returns the desired process exit code.
pub fn run_cli_process(surface: CliSurface, classifier: &dyn ExitCodeClassifier) -> i32 {
    install_terminal_restore_panic_hook();
    match run_main(surface) {
        Ok(exit_code) => {
            if exit_code != 0 {
                force_restore_terminal();
            }
            exit_code
        }
        Err(error) => {
            force_restore_terminal();
            if !classifier.suppress_fatal_render() {
                print_fatal_error(&error);
            }
            classifier.exit_code(&error)
        }
    }
}

fn run_main(surface: CliSurface) -> Result<i32> {
    let cli = parse_cli(surface)?;
    let runtime_overrides = runtime_overrides_from_cli(&cli)?;

    // Resolve UI mode with correct precedence:
    // 1. Explicit --ui flag (if provided)
    // 2. TRANSIST_UI env var
    // 3. Default to Plain
    let ui_mode = cli.ui.unwrap_or_else(|| {
        std::env::var("TRANSIST_UI")
            .ok()
            .and_then(|s| parse_ui_mode(&s))
            .unwrap_or(cli::CliUiMode::Plain)
    });

    transist::configure_runtime_process(runtime_overrides)
        .map_err(|source| wrap_usage_error("invalid managed-mode runtime configuration", source))?;
    let cwd = std::env::current_dir().context(
        "could not determine the current working directory; \
                  ensure the directory exists and the process has read permission on it",
    )?;
    let repo_root = cwd.clone();
    let interrupt_flag = install_interrupt_handler()?;
    if cli.command.is_none() {
        return ui::run_root_entry(
            ui_mode,
            &repo_root,
            &cwd,
            cli.home.as_deref(),
            &interrupt_flag,
            || {
                let mut command = Cli::command();
                command.print_help()?;
                println!();
                Ok(())
            },
        );
    }
    let mut prompt = default_prompt_backend(ui_mode)?;
    dispatch(
        cli,
        ui_mode,
        &repo_root,
        &cwd,
        &interrupt_flag,
        prompt.as_mut(),
    )
    .map_err(|source| {
        if is_interrupted_io_error(&source) {
            wrap_canceled_or_interrupted_error(source)
        } else {
            wrap_internal_error("command dispatch failed", source)
        }
    })
}

fn parse_cli(surface: CliSurface) -> Result<Cli> {
    let command = cli_command_for_surface(surface);
    let matches = command.get_matches();
    Cli::from_arg_matches(&matches).context("failed to parse CLI arguments")
}

fn cli_command_for_surface(surface: CliSurface) -> clap::Command {
    let mut command = cli::command_for_surface(surface);
    command = command.version(env!("CARGO_PKG_VERSION"));
    command
}

#[cfg(test)]
fn cli_command() -> clap::Command {
    cli_command_for_surface(CliSurface::current_build())
}

#[cfg(test)]
fn command_detaches_runner(cli: &Cli) -> bool {
    matches!(
        cli.command,
        Some(Commands::Workflow {
            command: WorkflowSubcommand::Run { .. }
        }) | Some(Commands::Runs {
            command: RunsSubcommand::Run { runner: false, .. }
        }) | Some(Commands::Runs {
            command: RunsSubcommand::Resume { runner: false, .. }
                | RunsSubcommand::Retry { runner: false, .. }
                | RunsSubcommand::Replay { runner: false, .. },
        })
    )
}

fn parse_ui_mode(s: &str) -> Option<cli::CliUiMode> {
    match s.trim().to_lowercase().as_str() {
        "plain" => Some(cli::CliUiMode::Plain),
        "tui" => Some(cli::CliUiMode::Tui),
        _ => None,
    }
}

#[cfg(test)]
fn fatal_exit_code_for_mode(_mode: transist::ManagementMode, _error: &anyhow::Error) -> i32 {
    1
}

#[cfg(feature = "managed-mode")]
fn runtime_overrides_from_cli(cli: &Cli) -> Result<transist::RuntimeProcessOverrides> {
    let management_mode = match cli
        .management_mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) if value.eq_ignore_ascii_case("standalone") => {
            Some(transist::ManagementMode::Standalone)
        }
        Some(value) if value.eq_ignore_ascii_case("managed") => {
            Some(transist::ManagementMode::Managed)
        }
        Some(value) if value.eq_ignore_ascii_case("fleet") => {
            Some(transist::ManagementMode::Managed)
        }
        Some(value) => {
            return Err(anyhow::Error::new(UsageError::new(format!(
                "unsupported management mode '{}'; expected 'standalone' or 'managed'",
                value
            ))));
        }
        None => None,
    };
    Ok(transist::RuntimeProcessOverrides {
        management_mode,
        owner_id: cli.owner_id.clone(),
        host_id: cli.host_id.clone(),
        runner_id: cli.runner_id.clone(),
        external_run_id: cli.external_run_id.clone(),
    })
}

#[cfg(not(feature = "managed-mode"))]
fn runtime_overrides_from_cli(_cli: &Cli) -> Result<transist::RuntimeProcessOverrides> {
    Ok(transist::RuntimeProcessOverrides::default())
}

#[cfg(test)]
mod main_tests;
