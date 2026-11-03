// SPDX-License-Identifier: MPL-2.0
//! Fullscreen UI screens and their interaction logic.
//!
//! Each screen is responsible for its own key bindings, focus management, and
//! refresh behavior. Rendering itself is delegated to shared UI components.

// SAFETY: OS-integration: pre_exec uses libc::setsid for child session isolation.
#![allow(unsafe_code)]

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool, mpsc};
use std::time::{Duration, Instant};

use crate::prompt::dialoguer::suggested_action_shortcut_index;

use super::*;

/// Map Ctrl+C to KeyCode::Esc so the standard quit/cancel arms in screen
/// loops can handle Ctrl+C uniformly without duplicating bodies.
///
/// In crossterm raw mode Ctrl+C is delivered as `KeyCode::Char('c')` with
/// `KeyModifiers::CONTROL` rather than triggering a SIGINT handler, so each
/// fullscreen screen must opt in to handling it.
fn remap_ctrl_c_to_esc(code: KeyCode, modifiers: KeyModifiers) -> KeyCode {
    if matches!(code, KeyCode::Char('c')) && modifiers.contains(KeyModifiers::CONTROL) {
        KeyCode::Esc
    } else {
        code
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationPollMode {
    Off,
    AwaitActive,
    UntilInactive,
}

const STALE_RESUME_TERMINAL_SNAPSHOT_LIMIT: u8 = 5;

fn mutation_status_is_running(status: &str) -> bool {
    matches!(status, "created" | "planned" | "running" | "waiting")
}

fn mutation_status_is_settled(status: &str) -> bool {
    matches!(status, "blocked" | "success" | "failed" | "canceled")
}

fn mutation_commands_are_stable(mode: MutationPollMode) -> bool {
    !matches!(mode, MutationPollMode::AwaitActive)
}

fn run_detail_refresh_window(
    refreshed: &RunDetailRefresh,
    mutation_poll_mode: MutationPollMode,
    running_interval: Duration,
    idle_interval: Duration,
    terminal_interval: Duration,
) -> Duration {
    // Detached mutating actions can briefly refresh into a stale terminal
    // snapshot before the runner's next state write lands. While the
    // mutation hand-off is still armed, keep the hot-poll cadence alive so
    // the first real post-action state is observed without user input.
    if !matches!(mutation_poll_mode, MutationPollMode::Off) {
        return running_interval;
    }
    if refreshed.running {
        running_interval
    } else if refreshed.terminal {
        terminal_interval
    } else {
        idle_interval
    }
}

/// Returns true when a run-detail snapshot should keep background polling
/// active even if there is no currently-active step/tool signal.
///
/// These statuses can flip to `running` from outside the current TUI loop
/// (for example another shell issuing `transist runs retry <run-id>`), so
/// parking on a very long poll timeout causes a visually stale screen.
fn status_is_externally_mutable(status: &str) -> bool {
    matches!(
        status,
        "blocked" | "waiting" | "failed" | "interrupted" | "created" | "planned" | "running"
    )
}

fn clear_mutation_poll_state(
    mutation_poll_mode: &mut MutationPollMode,
    mutation_refresh_requested: &mut bool,
    mutation_action_label: &mut Option<String>,
    active_ready_streak: &mut u8,
    inactive_streak: &mut u8,
) {
    *mutation_poll_mode = MutationPollMode::Off;
    *mutation_refresh_requested = false;
    *mutation_action_label = None;
    *active_ready_streak = 0;
    *inactive_streak = 0;
}

/// When a replay pivots the run-detail screen to a fresh run id, the detached
/// runner may fail before the replay run ever materializes on disk. In that
/// case the next inspection load for the replay id resolves to the generic
/// "not found or not actionable" error, which is technically true but hides
/// the real runner failure from the operator. Recover by jumping back to the
/// source run and surfacing the best failure summary we can extract.
fn recover_failed_fresh_run_pivot(
    home_dir: &Path,
    pending_run_id: &str,
    previous_run_id: Option<&str>,
    pending_status_message: Option<&str>,
    load_error: &anyhow::Error,
) -> Option<(String, String)> {
    let previous_run_id = previous_run_id?;
    let failure_signal =
        crate::runner::runner_log_failure_signal_since(home_dir, pending_run_id, 0, 3);
    let status_message = if !failure_signal.is_empty() {
        format!("Detached runner failed: {failure_signal}")
    } else if let Some(existing) = pending_status_message
        .map(str::trim)
        .filter(|message| message.starts_with("Detached runner failed:"))
    {
        existing.to_string()
    } else {
        let tail = crate::runner::tail_runner_log(home_dir, pending_run_id, 3);
        if !tail.is_empty() {
            format!(
                "Detached runner failed before new run '{}' became inspectable: {tail}",
                pending_run_id
            )
        } else {
            format!(
                "Detached runner failed before new run '{}' became inspectable: {}",
                pending_run_id, load_error
            )
        }
    };
    Some((previous_run_id.to_string(), status_message))
}

fn runner_failure_status_since(home_dir: &Path, watch: &RunnerLogWatch) -> Option<String> {
    let failure = crate::runner::runner_log_failure_signal_since(
        home_dir,
        &watch.run_id,
        watch.start_offset,
        3,
    );
    (!failure.is_empty()).then(|| format!("Detached runner failed: {failure}"))
}

fn advance_mutation_poll_mode(
    refreshed: &RunDetailRefresh,
    mutation_poll_mode: &mut MutationPollMode,
    mutation_refresh_requested: &mut bool,
    mutation_action_label: &mut Option<String>,
    active_ready_streak: &mut u8,
    inactive_streak: &mut u8,
) {
    let run_status = refreshed.effective_run_status.as_str();
    let stale_resume_terminal_status = matches!(
        (mutation_action_label.as_deref(), run_status),
        (Some("retry"), "failed" | "canceled")
    );
    // A parent that stays `blocked` after spawning a child (the common
    // pattern for review-cycle / gate-pivot workflows) reports a settled
    // status of its own but is NOT actually quiescent: the just-spawned
    // child is running and its progress will not flip the parent's files
    // until the child itself settles. If we cleared the poll mode here
    // we'd drop back to the idle 1-3s cadence and the run-detail screen
    // would appear frozen for seconds at a time. Treat such a parent as
    // still in active mutation and keep the hot-poll cadence alive.
    let parent_blocked_with_active_child = run_status == "blocked" && refreshed.has_active_child;
    match mutation_poll_mode {
        MutationPollMode::AwaitActive => {
            // Detached `retry` can race the first refresh tick: the runner is
            // already alive, but the persisted root snapshot can still read as
            // the pre-retry terminal status for one poll cycle. Keep the hot
            // poll alive long enough to observe the transition to `running`
            // instead of clearing immediately and leaving the stale `retry`
            // action on screen.
            if stale_resume_terminal_status {
                *inactive_streak = inactive_streak.saturating_add(1);
                if *inactive_streak >= STALE_RESUME_TERMINAL_SNAPSHOT_LIMIT {
                    clear_mutation_poll_state(
                        mutation_poll_mode,
                        mutation_refresh_requested,
                        mutation_action_label,
                        active_ready_streak,
                        inactive_streak,
                    );
                }
                return;
            }
            *inactive_streak = 0;
            if mutation_status_is_settled(run_status) && !parent_blocked_with_active_child {
                clear_mutation_poll_state(
                    mutation_poll_mode,
                    mutation_refresh_requested,
                    mutation_action_label,
                    active_ready_streak,
                    inactive_streak,
                );
                return;
            }
            if refreshed.running
                || mutation_status_is_running(run_status)
                || parent_blocked_with_active_child
            {
                *active_ready_streak = active_ready_streak.saturating_add(1);
                if *active_ready_streak >= 2 {
                    *mutation_poll_mode = MutationPollMode::UntilInactive;
                    *active_ready_streak = 0;
                    *inactive_streak = 0;
                }
            } else {
                *active_ready_streak = 0;
            }
        }
        MutationPollMode::UntilInactive => {
            if mutation_status_is_settled(run_status) && !parent_blocked_with_active_child {
                clear_mutation_poll_state(
                    mutation_poll_mode,
                    mutation_refresh_requested,
                    mutation_action_label,
                    active_ready_streak,
                    inactive_streak,
                );
                return;
            }
            if refreshed.running
                || mutation_status_is_running(run_status)
                || parent_blocked_with_active_child
            {
                *inactive_streak = 0;
            } else {
                *inactive_streak = inactive_streak.saturating_add(1);
                if *inactive_streak >= 2 {
                    clear_mutation_poll_state(
                        mutation_poll_mode,
                        mutation_refresh_requested,
                        mutation_action_label,
                        active_ready_streak,
                        inactive_streak,
                    );
                }
            }
        }
        MutationPollMode::Off => {}
    }
}

pub(crate) fn run_root_dashboard(
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    interrupt_flag: &Arc<AtomicBool>,
) -> Result<()> {
    if !fullscreen_ui_available() {
        anyhow::bail!("transist root UI requires an interactive terminal with redraw support");
    }
    let mut prompt = FullscreenPrompt::new(false)?;
    prompt.set_mouse_capture_enabled(true)?;
    let mut status_message = None::<UiStatusMessage>;
    let mut session_error = None::<super::diagnostics::SessionDiagnostic>;
    let mut dashboard = rebuild_root_dashboard(
        repo_root,
        cwd,
        explicit_home,
        None,
        &mut status_message,
        &mut session_error,
    )?;
    update_dashboard_problem_count(&mut dashboard, session_error.as_ref());
    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
    loop {
        match event::read()? {
            Event::Resize(_, _) => prompt.handle_terminal_resize()?,
            Event::Mouse(_) => {}
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') => {
                    if confirm_quit_main_screen(
                        &mut prompt,
                        "Exit the Transist CLI UI and return to the shell?",
                    )? {
                        return Ok(());
                    }
                    let previous_state = dashboard.dashboard_state.clone();
                    dashboard = rebuild_root_dashboard(
                        repo_root,
                        cwd,
                        explicit_home,
                        Some(&dashboard),
                        &mut status_message,
                        &mut session_error,
                    )?;
                    update_dashboard_problem_count(&mut dashboard, session_error.as_ref());
                    restore_dashboard_view_state(&previous_state, &mut dashboard.dashboard_state);
                    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                }
                KeyCode::BackTab => {
                    dashboard.dashboard_state.focused_section =
                        dashboard_move_focus(dashboard.dashboard_state.focused_section, false);
                    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                }
                KeyCode::Tab => {
                    dashboard.dashboard_state.focused_section =
                        dashboard_move_focus(dashboard.dashboard_state.focused_section, true);
                    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let (setup_width, setup_visible, inventory_width, inventory_visible) = prompt
                        .terminal
                        .size()
                        .map(dashboard_section_metrics)
                        .unwrap_or((40, 10, 40, 10));
                    match dashboard.dashboard_state.focused_section {
                        DashboardSection::Setup => apply_scroll_delta(
                            &mut dashboard.dashboard_state.setup_scroll,
                            wrapped_ui_lines(&dashboard.dashboard_state.setup_lines, setup_width)
                                .len(),
                            setup_visible,
                            1,
                        ),
                        DashboardSection::Inventory => apply_scroll_delta(
                            &mut dashboard.dashboard_state.inventory_scroll,
                            wrapped_ui_lines(
                                &dashboard.dashboard_state.inventory_lines,
                                inventory_width,
                            )
                            .len(),
                            inventory_visible,
                            1,
                        ),
                        DashboardSection::Actions => {
                            dashboard.dashboard_state.selected_action =
                                (dashboard.dashboard_state.selected_action + 1)
                                    % dashboard.dashboard_state.action_items.len();
                        }
                    }
                    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let (setup_width, setup_visible, inventory_width, inventory_visible) = prompt
                        .terminal
                        .size()
                        .map(dashboard_section_metrics)
                        .unwrap_or((40, 10, 40, 10));
                    match dashboard.dashboard_state.focused_section {
                        DashboardSection::Setup => apply_scroll_delta(
                            &mut dashboard.dashboard_state.setup_scroll,
                            wrapped_ui_lines(&dashboard.dashboard_state.setup_lines, setup_width)
                                .len(),
                            setup_visible,
                            -1,
                        ),
                        DashboardSection::Inventory => apply_scroll_delta(
                            &mut dashboard.dashboard_state.inventory_scroll,
                            wrapped_ui_lines(
                                &dashboard.dashboard_state.inventory_lines,
                                inventory_width,
                            )
                            .len(),
                            inventory_visible,
                            -1,
                        ),
                        DashboardSection::Actions => {
                            dashboard.dashboard_state.selected_action =
                                (dashboard.dashboard_state.selected_action
                                    + dashboard.dashboard_state.action_items.len()
                                    - 1)
                                    % dashboard.dashboard_state.action_items.len();
                        }
                    }
                    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                }
                KeyCode::Char('r') => {
                    status_message = Some(UiStatusMessage::info("Dashboard refreshed."));
                    let previous_state = dashboard.dashboard_state.clone();
                    dashboard = rebuild_root_dashboard(
                        repo_root,
                        cwd,
                        explicit_home,
                        Some(&dashboard),
                        &mut status_message,
                        &mut session_error,
                    )?;
                    update_dashboard_problem_count(&mut dashboard, session_error.as_ref());
                    restore_dashboard_view_state(&previous_state, &mut dashboard.dashboard_state);
                    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let action = dashboard.dashboard_state.action_items
                        [dashboard.dashboard_state.selected_action]
                        .action;
                    match handle_dashboard_action(
                        &mut prompt,
                        repo_root,
                        cwd,
                        explicit_home,
                        interrupt_flag,
                        &dashboard,
                        action,
                        &mut session_error,
                    ) {
                        Ok(DashboardCommand::Quit) => return Ok(()),
                        Ok(DashboardCommand::OpenProblems) => {
                            if let Err(error) = run_problems_screen(
                                &mut prompt,
                                repo_root,
                                cwd,
                                explicit_home,
                                &dashboard,
                                &mut session_error,
                            ) {
                                let report = dashboard_operation_report(&error);
                                session_error = Some(
                                    super::diagnostics::SessionDiagnostic::capture(report.clone()),
                                );
                                status_message = Some(UiStatusMessage::diagnostic_summary(&report));
                            }
                        }
                        Ok(DashboardCommand::Status(message)) => {
                            status_message = Some(UiStatusMessage::info(message))
                        }
                        Err(error) => {
                            let report = dashboard_operation_report(&error);
                            session_error = Some(super::diagnostics::SessionDiagnostic::capture(
                                report.clone(),
                            ));
                            status_message = Some(UiStatusMessage::diagnostic_summary(&report));
                        }
                    }
                    let previous_state = dashboard.dashboard_state.clone();
                    dashboard = rebuild_root_dashboard(
                        repo_root,
                        cwd,
                        explicit_home,
                        Some(&dashboard),
                        &mut status_message,
                        &mut session_error,
                    )?;
                    update_dashboard_problem_count(&mut dashboard, session_error.as_ref());
                    restore_dashboard_view_state(&previous_state, &mut dashboard.dashboard_state);
                    prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                }
                KeyCode::Char(ch) if key.modifiers.is_empty() => {
                    if let Some(index) = suggested_action_shortcut_index(
                        ch,
                        dashboard.dashboard_state.action_items.len(),
                    ) {
                        dashboard.dashboard_state.selected_action = index;
                        let action = dashboard.dashboard_state.action_items[index].action;
                        match handle_dashboard_action(
                            &mut prompt,
                            repo_root,
                            cwd,
                            explicit_home,
                            interrupt_flag,
                            &dashboard,
                            action,
                            &mut session_error,
                        ) {
                            Ok(DashboardCommand::Quit) => return Ok(()),
                            Ok(DashboardCommand::OpenProblems) => {
                                if let Err(error) = run_problems_screen(
                                    &mut prompt,
                                    repo_root,
                                    cwd,
                                    explicit_home,
                                    &dashboard,
                                    &mut session_error,
                                ) {
                                    let report = dashboard_operation_report(&error);
                                    session_error =
                                        Some(super::diagnostics::SessionDiagnostic::capture(
                                            report.clone(),
                                        ));
                                    status_message =
                                        Some(UiStatusMessage::diagnostic_summary(&report));
                                }
                            }
                            Ok(DashboardCommand::Status(message)) => {
                                status_message = Some(UiStatusMessage::info(message))
                            }
                            Err(error) => {
                                let report = dashboard_operation_report(&error);
                                session_error = Some(
                                    super::diagnostics::SessionDiagnostic::capture(report.clone()),
                                );
                                status_message = Some(UiStatusMessage::diagnostic_summary(&report));
                            }
                        }
                        let previous_state = dashboard.dashboard_state.clone();
                        dashboard = rebuild_root_dashboard(
                            repo_root,
                            cwd,
                            explicit_home,
                            Some(&dashboard),
                            &mut status_message,
                            &mut session_error,
                        )?;
                        update_dashboard_problem_count(&mut dashboard, session_error.as_ref());
                        restore_dashboard_view_state(
                            &previous_state,
                            &mut dashboard.dashboard_state,
                        );
                        prompt.set_dashboard_state(dashboard.dashboard_state.clone())?;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn dashboard_operation_report(error: &anyhow::Error) -> super::diagnostics::DiagnosticReport {
    super::diagnostics::DiagnosticReport::from_error(
        super::diagnostics::DiagnosticSeverity::Error,
        Some("tui.operation_failed".to_string()),
        "dashboard action",
        "current selection",
        "The selected dashboard operation failed.",
        None,
        error,
    )
}

fn rebuild_root_dashboard(
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    current: Option<&DashboardData>,
    status_message: &mut Option<UiStatusMessage>,
    session_error: &mut Option<super::diagnostics::SessionDiagnostic>,
) -> Result<DashboardData> {
    let rebuilt = build_root_dashboard(repo_root, cwd, explicit_home, status_message.as_ref());
    handle_dashboard_rebuild_result(
        repo_root,
        cwd,
        current,
        status_message,
        session_error,
        rebuilt,
    )
}

fn handle_dashboard_rebuild_result(
    repo_root: &Path,
    cwd: &Path,
    current: Option<&DashboardData>,
    status_message: &mut Option<UiStatusMessage>,
    session_error: &mut Option<super::diagnostics::SessionDiagnostic>,
    rebuilt: Result<DashboardData>,
) -> Result<DashboardData> {
    match rebuilt {
        Ok(dashboard) => Ok(dashboard),
        Err(error) => {
            let report = dashboard_operation_report(&error);
            *session_error = Some(super::diagnostics::SessionDiagnostic::capture(
                report.clone(),
            ));
            *status_message = Some(UiStatusMessage::diagnostic_summary(&report));
            if let Some(current) = current {
                Ok(current.clone())
            } else {
                build_dashboard_error(repo_root, cwd, status_message.as_ref())
            }
        }
    }
}

pub(crate) fn run_runs_watch(context: &transist::TransistContext, run_id: &str) -> Result<()> {
    if !fullscreen_ui_available() {
        anyhow::bail!("transist runs watch requires an interactive terminal with redraw support")
    }
    let mut prompt = FullscreenPrompt::new(false)?;
    run_run_detail_screen(
        &mut prompt,
        context,
        &Arc::new(AtomicBool::new(false)),
        run_id,
        true,
    )
}

/// Re-attach the run-detail screen using a `FullscreenPrompt` that is
/// already active on the call stack. Used by in-TUI flows
/// (`attach_run_viewer_with_prompt`) that must NOT construct a nested
/// `FullscreenPrompt` (its `Drop` would tear the parent terminal state
/// down on unwind — the original "freeze + ^[ on Esc" symptom).
pub(crate) fn run_run_detail_screen_reusing_prompt(
    prompt: &mut FullscreenPrompt,
    context: &transist::TransistContext,
    interrupt_flag: &Arc<AtomicBool>,
    run_id: &str,
    confirm_on_quit: bool,
) -> Result<()> {
    run_run_detail_screen(prompt, context, interrupt_flag, run_id, confirm_on_quit)
}

pub(super) fn run_configuration_screen(
    prompt: &mut FullscreenPrompt,
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    session_error: &mut Option<super::diagnostics::SessionDiagnostic>,
) -> Result<()> {
    prompt.set_mouse_capture_enabled(true)?;
    let mut status_message = None::<UiStatusMessage>;
    let mut config_state =
        build_configuration_screen_state(repo_root, cwd, explicit_home, status_message.as_ref())?;
    prompt.dashboard_state = None;
    prompt.run_browser_state = None;
    prompt.run_detail_screen_active = false;
    prompt.run_detail_state = None;
    prompt.current_progress = None;
    prompt.picker_state = None;
    loop {
        prompt.config_screen_state = Some(config_state.clone());
        prompt.draw()?;
        match event::read()? {
            Event::Resize(_, _) => prompt.handle_terminal_resize()?,
            Event::Mouse(_) => {}
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match remap_ctrl_c_to_esc(key.code, key.modifiers) {
                    KeyCode::Esc => {
                        prompt.config_screen_state = None;
                        prompt.draw()?;
                        return Ok(());
                    }
                    KeyCode::BackTab => {
                        config_state.focused_section =
                            config_move_focus(config_state.focused_section, false);
                        prompt.config_screen_state = Some(config_state.clone());
                        prompt.draw()?;
                    }
                    KeyCode::Tab => {
                        config_state.focused_section =
                            config_move_focus(config_state.focused_section, true);
                        prompt.config_screen_state = Some(config_state.clone());
                        prompt.draw()?;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let (details_width, details_visible) = prompt
                            .terminal
                            .size()
                            .map(config_screen_metrics)
                            .unwrap_or((50, 10));
                        match config_state.focused_section {
                            ConfigSection::Details => apply_scroll_delta(
                                &mut config_state.details_scroll,
                                wrapped_ui_lines(&config_state.details_lines, details_width).len(),
                                details_visible,
                                1,
                            ),
                            ConfigSection::Actions => {
                                config_state.selected_action = (config_state.selected_action + 1)
                                    % config_state.action_items.len();
                            }
                        }
                        prompt.config_screen_state = Some(config_state.clone());
                        prompt.draw()?;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let (details_width, details_visible) = prompt
                            .terminal
                            .size()
                            .map(config_screen_metrics)
                            .unwrap_or((50, 10));
                        match config_state.focused_section {
                            ConfigSection::Details => apply_scroll_delta(
                                &mut config_state.details_scroll,
                                wrapped_ui_lines(&config_state.details_lines, details_width).len(),
                                details_visible,
                                -1,
                            ),
                            ConfigSection::Actions => {
                                config_state.selected_action = (config_state.selected_action
                                    + config_state.action_items.len()
                                    - 1)
                                    % config_state.action_items.len();
                            }
                        }
                        prompt.config_screen_state = Some(config_state.clone());
                        prompt.draw()?;
                    }
                    KeyCode::Char('r') => {
                        status_message = Some(UiStatusMessage::info("Preferences refreshed."));
                        let previous_state = config_state.clone();
                        config_state = build_configuration_screen_state(
                            repo_root,
                            cwd,
                            explicit_home,
                            status_message.as_ref(),
                        )?;
                        restore_config_view_state(&previous_state, &mut config_state);
                        prompt.config_screen_state = Some(config_state.clone());
                        prompt.draw()?;
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if !matches!(config_state.focused_section, ConfigSection::Actions) {
                            continue;
                        }
                        let action = config_state.action_items[config_state.selected_action].action;
                        status_message = Some(
                            match execute_runtime_configuration_action(
                                prompt,
                                repo_root,
                                cwd,
                                explicit_home,
                                action,
                            ) {
                                Ok(message) => UiStatusMessage::info(message),
                                Err(error) => record_preference_failure(session_error, &error),
                            },
                        );
                        let previous_state = config_state.clone();
                        config_state = build_configuration_screen_state(
                            repo_root,
                            cwd,
                            explicit_home,
                            status_message.as_ref(),
                        )?;
                        restore_config_view_state(&previous_state, &mut config_state);
                        prompt.config_screen_state = Some(config_state.clone());
                        prompt.draw()?;
                    }
                    KeyCode::Char(ch) if key.modifiers.is_empty() => {
                        if let Some(index) =
                            suggested_action_shortcut_index(ch, config_state.action_items.len())
                        {
                            config_state.selected_action = index;
                            config_state.focused_section = ConfigSection::Actions;
                            let action = config_state.action_items[index].action;
                            status_message = Some(
                                match execute_runtime_configuration_action(
                                    prompt,
                                    repo_root,
                                    cwd,
                                    explicit_home,
                                    action,
                                ) {
                                    Ok(message) => UiStatusMessage::info(message),
                                    Err(error) => record_preference_failure(session_error, &error),
                                },
                            );
                            let previous_state = config_state.clone();
                            config_state = build_configuration_screen_state(
                                repo_root,
                                cwd,
                                explicit_home,
                                status_message.as_ref(),
                            )?;
                            restore_config_view_state(&previous_state, &mut config_state);
                            prompt.config_screen_state = Some(config_state.clone());
                            prompt.draw()?;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn preference_operation_report(error: &anyhow::Error) -> super::diagnostics::DiagnosticReport {
    super::diagnostics::DiagnosticReport::from_error(
        super::diagnostics::DiagnosticSeverity::Error,
        Some("tui.operation_failed".to_string()),
        "preference operation",
        "current selection",
        "The selected preference operation failed.",
        None,
        error,
    )
}

fn record_preference_failure(
    session_error: &mut Option<super::diagnostics::SessionDiagnostic>,
    error: &anyhow::Error,
) -> UiStatusMessage {
    *session_error = Some(super::diagnostics::SessionDiagnostic::capture(
        preference_operation_report(error),
    ));
    UiStatusMessage::from_error(error)
}

#[cfg(test)]
mod remediation_tests {
    use super::*;

    #[test]
    fn failed_refresh_retains_dashboard_and_records_session_diagnostic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = build_dashboard_error(temp.path(), temp.path(), None).expect("dashboard");
        let retained_home = current.context.home_dir().to_path_buf();
        let mut status = None;
        let mut session = None;
        let retained = handle_dashboard_rebuild_result(
            temp.path(),
            temp.path(),
            Some(&current),
            &mut status,
            &mut session,
            Err(anyhow::anyhow!("refresh failed")),
        )
        .expect("retained dashboard");

        assert_eq!(retained.context.home_dir(), retained_home);
        assert_eq!(
            session
                .as_ref()
                .and_then(|value| value.report.error_code.as_deref()),
            Some("tui.operation_failed")
        );
        assert!(session.expect("session diagnostic").captured_at_ms > 0);
    }

    #[test]
    fn preference_failure_becomes_a_timestamped_session_diagnostic() {
        let mut session = None;
        let status = record_preference_failure(&mut session, &anyhow::anyhow!("write denied"));
        assert_eq!(status.tone, UiStatusTone::Error);
        let diagnostic = session.expect("session diagnostic");
        assert_eq!(
            diagnostic.report.error_code.as_deref(),
            Some("tui.operation_failed")
        );
        assert!(diagnostic.captured_at_ms > 0);
    }
}

pub(super) fn run_runs_browser_screen(
    prompt: &mut FullscreenPrompt,
    context: &transist::TransistContext,
    interrupt_flag: &Arc<AtomicBool>,
) -> Result<()> {
    prompt.set_mouse_capture_enabled(true)?;
    let initial_context = context.clone();
    let mut browser_state = prompt.with_spinner("Loading runs browser", move || {
        load_run_browser_state(&initial_context)
    })?;
    // Debounce before the focused run's metrics are fetched. Arrow-key
    // navigation must feel snappy; loads only fire when the user has
    // stopped on a row.
    const METRICS_DEBOUNCE: Duration = Duration::from_millis(2000);
    // Poll cadence for the event loop. Short enough that the debounce
    // window resolves promptly, long enough to be cheap.
    const POLL_TIMEOUT: Duration = Duration::from_millis(100);
    let mut has_selection_interaction = false;
    loop {
        prompt.dashboard_state = None;
        prompt.run_detail_screen_active = false;
        prompt.run_detail_state = None;
        prompt.current_progress = None;
        prompt.picker_state = None;
        if has_selection_interaction {
            // Register the focused run as Pending (no I/O) only after
            // the user starts navigating.
            browser_state.mark_focused_run_pending();
        }
        prompt.run_browser_state = Some(browser_state.clone());
        prompt.draw()?;
        if !event::poll(POLL_TIMEOUT)? {
            // Idle tick: if the focused run has been Pending long
            // enough, load its metrics inline and redraw.
            if has_selection_interaction
                && browser_state.load_focused_run_metrics_if_due(METRICS_DEBOUNCE)
            {
                prompt.run_browser_state = Some(browser_state.clone());
                prompt.draw()?;
            }
            continue;
        }
        match event::read()? {
            Event::Resize(_, _) => prompt.handle_terminal_resize()?,
            Event::Mouse(_) => {}
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let page_size = prompt
                    .terminal
                    .size()
                    .map(run_browser_visible_count_for_area)
                    .unwrap_or(10)
                    .max(1);
                match remap_ctrl_c_to_esc(key.code, key.modifiers) {
                    KeyCode::Esc => {
                        prompt.run_browser_state = None;
                        prompt.draw()?;
                        return Ok(());
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        has_selection_interaction = true;
                        browser_state.move_selection(1)
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        has_selection_interaction = true;
                        browser_state.move_selection(-1)
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        has_selection_interaction = true;
                        browser_state.page_selection(1, page_size)
                    }
                    KeyCode::PageUp | KeyCode::Char('b') => {
                        has_selection_interaction = true;
                        browser_state.page_selection(-1, page_size)
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        has_selection_interaction = true;
                        browser_state.selected = 0
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        has_selection_interaction = true;
                        browser_state.selected =
                            browser_state.current_items().len().saturating_sub(1)
                    }
                    KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                        has_selection_interaction = true;
                        browser_state.ascend();
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        has_selection_interaction = true;
                        browser_state.descend();
                    }
                    KeyCode::Enter => {
                        let Some(selected_run_id) =
                            browser_state.selected_run_id().map(str::to_string)
                        else {
                            browser_state.status_line =
                                "No run is selected at this level.".to_string();
                            prompt.run_browser_state = Some(browser_state.clone());
                            prompt.draw()?;
                            continue;
                        };
                        let selected_parent_run_id = browser_state.current_parent_run_id.clone();
                        prompt.run_browser_state = None;
                        run_run_detail_screen(
                            prompt,
                            context,
                            interrupt_flag,
                            &selected_run_id,
                            false,
                        )?;
                        prompt.set_mouse_capture_enabled(true)?;
                        let status_filter = browser_state.status_filter.clone();
                        let refresh_context = context.clone();
                        browser_state =
                            prompt.with_spinner("Refreshing runs browser", move || {
                                load_run_browser_state_with_filter(
                                    &refresh_context,
                                    status_filter.as_deref(),
                                )
                            })?;
                        browser_state.restore_position(
                            selected_parent_run_id.as_deref(),
                            Some(&selected_run_id),
                        );
                    }
                    KeyCode::Char('r') => {
                        let selected_run_id = browser_state.selected_run_id().map(str::to_string);
                        let selected_parent_run_id = browser_state.current_parent_run_id.clone();
                        let status_filter = browser_state.status_filter.clone();
                        let refresh_context = context.clone();
                        browser_state =
                            prompt.with_spinner("Refreshing runs browser", move || {
                                load_run_browser_state_with_filter(
                                    &refresh_context,
                                    status_filter.as_deref(),
                                )
                            })?;
                        browser_state.restore_position(
                            selected_parent_run_id.as_deref(),
                            selected_run_id.as_deref(),
                        );
                        browser_state.status_line = "Runs browser refreshed.".to_string();
                    }
                    KeyCode::Char('f') => {
                        let mut statuses: Vec<String> = browser_state
                            .summaries_by_id
                            .values()
                            .map(|summary| summary.status.clone())
                            .collect();
                        statuses.sort();
                        statuses.dedup();
                        if statuses.is_empty() {
                            browser_state.status_line =
                                "No runs available to derive status filters from.".to_string();
                            prompt.run_browser_state = Some(browser_state.clone());
                            prompt.draw()?;
                            continue;
                        }
                        let mut items: Vec<String> =
                            Vec::with_capacity(statuses.len().saturating_add(1));
                        items.push("(all)".to_string());
                        items.extend(statuses.iter().cloned());
                        let initial = browser_state
                            .status_filter
                            .as_deref()
                            .and_then(|current| statuses.iter().position(|s| s == current))
                            .map(|index| index + 1)
                            .unwrap_or(0);
                        let Some(selection) =
                            prompt.run_select_modal("Run status filter", items, initial)?
                        else {
                            prompt.run_browser_state = Some(browser_state.clone());
                            prompt.draw()?;
                            continue;
                        };
                        let filter = if selection == 0 {
                            None
                        } else {
                            Some(statuses[selection - 1].clone())
                        };
                        let refresh_context = context.clone();
                        let filter_for_load = filter.clone();
                        browser_state =
                            prompt.with_spinner("Filtering runs browser", move || {
                                load_run_browser_state_with_filter(
                                    &refresh_context,
                                    filter_for_load.as_deref(),
                                )
                            })?;
                    }
                    _ => {}
                }
                prompt.run_browser_state = Some(browser_state.clone());
                prompt.draw()?;
            }
            _ => {}
        }
    }
}

pub(super) fn run_run_detail_screen(
    prompt: &mut FullscreenPrompt,
    context: &transist::TransistContext,
    interrupt_flag: &Arc<AtomicBool>,
    run_id: &str,
    confirm_on_quit: bool,
) -> Result<()> {
    prompt.set_mouse_capture_enabled(true)?;
    prompt.run_detail_screen_active = true;
    prompt.run_browser_state = None;
    prompt.dashboard_state = None;
    prompt.run_detail_state = None;
    prompt.loading_state = None;
    // Force the first draw of this screen entry to run unconditionally —
    // we may be re-entering after navigating away, and the cached frame
    // fingerprint would otherwise mistakenly suppress the first redraw.
    prompt.last_drawn_run_detail_frame_fingerprint = None;
    // Owned copy: mutating actions (gate decisions, replay) may pivot the
    // viewer to a different (root) run id mid-loop. The loop reads from
    // `run_id` everywhere a `&str` was previously borrowed.
    let mut run_id: String = run_id.to_string();
    let mut selected_action = 0;
    let mut status_message = None::<String>;
    let mut refresh_needed = true;
    // Always auto-refresh while the run-detail screen is open. Previously
    // this was gated on `loaded.detail.summary.status` being
    // `running`/`waiting`, which meant a freshly-attached viewer for a
    // run still in `created`/`planned` (e.g. just-spawned by the
    // dashboard or by a gate-decision action) was a static snapshot
    // until the user pressed `r`. The polling cost is trivial (one
    // disk read per cadence tick).
    let auto_refresh = true;
    // Cadence the background `load_run_detail_data` spawn while the run
    // is actively executing. 1s is the sweet spot: it matches the
    // human's "live" perception (steps transition every few hundred ms
    // at most, output streaming chunks render within one tick) while
    // cutting the steady-state refresh-loop CPU roughly in half versus
    // a 500ms cadence. Every spawn does a full inspection walk
    // (events, gates, blob listings, lease check) because an active
    // workflow constantly mutates `state.json` and `events.jsonl`, so
    // halving the spawn rate is the most direct CPU reduction available
    // once the duplicate-walk and fingerprint-skip optimizations are in
    // place.
    let running_refresh_interval = Duration::from_millis(1_000);
    let idle_refresh_interval = Duration::from_millis(3_000);
    let terminal_refresh_interval = Duration::from_millis(10_000);
    let spinner_interval = Duration::from_millis(500);
    let refresh_respawn_guard = Duration::from_millis(350);
    let mut next_refresh_at = Instant::now();
    let mut next_spinner_at = Instant::now() + spinner_interval;
    let recipe_cache = Arc::new(crate::execution::RecipeCacheOwner::default());
    let output_cache = Arc::new(crate::display::output::PersistedStepOutputCacheOwner::default());
    // Attach warm-up window. Right after entering the screen the run on
    // disk often still reads as `created`/`planned`/`pending` with step 1
    // not yet flipped to `running` — the detached runner forks and only
    // then publishes its first step transition. In that window none of
    // the regular polling-eligibility flags (`has_active_tool_work`,
    // `externally_mutable`, `transient_no_progress`, mutation poll mode)
    // are true, so the loop would otherwise park on
    // `event::poll(3600s)` after a single snapshot refresh and the
    // operator would see a frozen screen until the next keypress. Force
    // polling-eligibility for a short window so the first runner-driven
    // transitions surface live without operator interaction. Mirrors
    // `pivot_grace_until` (replay-pivot path) and
    // `MutationPollMode::AwaitActive` (post-mutation path), which solve
    // the analogous problem for those entry paths.
    let attach_warmup_until = Instant::now() + Duration::from_secs(5);
    // Clamp a candidate refresh window down to the running cadence
    // while the attach warm-up is active and the run is non-terminal.
    // This keeps the cadence hot in the moments right after the screen
    // opens, when the persisted snapshot may still classify the run as
    // idle and `run_detail_refresh_interval` would otherwise return the
    // 3s idle interval. Terminal runs (success/failed/canceled) opt
    // out — they never need to repoll.
    let clamp_for_warmup = |window: Duration, terminal: bool| -> Duration {
        if !terminal && Instant::now() < attach_warmup_until {
            window.min(running_refresh_interval)
        } else {
            window
        }
    };
    let mut last_refreshed = None::<RunDetailRefresh>;
    let cache_capacity = transist::load_user_inspection_cache_runs()?
        .unwrap_or(transist::DEFAULT_INSPECTION_CACHE_RUNS);
    let context = transist::TransistContext::with_home_and_inspection_cache_capacity(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
        cache_capacity,
    );
    let mut pending_refresh: Option<mpsc::Receiver<Result<RunDetailLoadedData>>> = None;
    let mut focus_details_after_gate_decision = false;
    let mut mutation_poll_mode = MutationPollMode::Off;
    let mut mutation_refresh_requested = false;
    let mut mutation_action_label: Option<String> = None;
    let mut active_ready_streak: u8 = 0;
    let mut inactive_streak: u8 = 0;
    let runner_log_watch_window = Duration::from_secs(10);
    let mut runner_log_watch: Option<RunnerLogWatch> = None;
    let mut runner_log_watch_until: Option<Instant> = None;
    let mut frozen_next_items: Option<Vec<String>> = None;
    let mut last_refresh_completed_at = Instant::now();
    // Tracks the persisted-file fingerprint of the most recent
    // `load_run_detail_data` spawn. When the auto-refresh tick fires
    // and the fingerprint hasn't moved (no new step transition and no
    // new runtime event), we skip the spawn entirely. This is the
    // dominant CPU savings on long-running workflows where the 500ms
    // running-interval would otherwise re-walk the inspection store
    // even when nothing changed.
    let mut last_spawn_fingerprint: Option<RunDetailFingerprint> = None;
    // Grace window after a `RefreshNow` pivot to a brand-new run id
    // (e.g. just-issued replay). The detached runner forks and only
    // then creates the run directory and `state.json`; the TUI's first
    // `load_run_detail_data` for the new id can race ahead and observe
    // "run '…' was not found or not actionable". Within the grace
    // window we swallow such transient load failures, re-arm a fast
    // refresh, and try again instead of bubbling the error up to the
    // dashboard (which would bounce the operator off the run-detail
    // screen).
    let mut pivot_grace_until: Option<Instant> = None;
    let pivot_grace_window = Duration::from_secs(3);
    let pivot_grace_retry_delay = Duration::from_millis(150);
    let mut pivot_return_run_id: Option<String> = None;
    let invalidate_visible_run_cache = |prompt: &FullscreenPrompt, run_id: &str| {
        let invalidate = |id: &str| {
            let _ = context.invalidate_inspection_cache(id);
        };
        invalidate(run_id);
        if let Some(detail) = prompt.current_run_detail.as_ref() {
            invalidate(&detail.summary.run_id);
        }
        if let Some(detail) = prompt.current_actionable_run_detail.as_ref() {
            invalidate(&detail.summary.run_id);
        }
        for detail in prompt.current_contained_run_details.values() {
            invalidate(&detail.summary.run_id);
        }
    };
    loop {
        let mutation_was_canceled = |message: Option<&str>| {
            matches!(
                message.map(str::trim),
                Some("Action canceled.")
                    | Some("Run cleanup canceled.")
                    | Some("Lease break canceled.")
            )
        };
        let show_mutation_loading = |prompt: &mut FullscreenPrompt,
                                     mode: MutationPollMode,
                                     action_label: Option<&str>,
                                     run_is_active: bool|
         -> Result<()> {
            let previous_loading = prompt.loading_state.is_some();
            if !matches!(mode, MutationPollMode::AwaitActive) || run_is_active {
                prompt.loading_state = None;
            } else {
                let label = action_label.unwrap_or("action");
                prompt.loading_state = Some(LoadingState {
                    message: format!("Applying action: {label}…"),
                });
            }
            // If loading-state visibility flipped, force a redraw. The
            // run-detail screen caches frame fingerprints to skip
            // redundant draws (`apply_run_detail_loaded_data`), and the
            // fingerprint includes `loading_state.is_some()`. When the
            // mutation poll mode resolves on a refresh tick and we
            // clear the modal here, no subsequent code path in the loop
            // redraws unless the user interacts or another refresh
            // changes the fingerprint, so the cleared modal stays
            // visually present until the next event. Invalidate the
            // cached fingerprint and draw immediately.
            let new_loading = prompt.loading_state.is_some();
            if previous_loading != new_loading {
                super::tui_debug::log(
                    "show_loading.flip",
                    &format!("prev={previous_loading} new={new_loading}"),
                );
                prompt.last_drawn_run_detail_frame_fingerprint = None;
                prompt.draw()?;
            }
            Ok(())
        };
        if runner_log_watch_until
            .map(|deadline| Instant::now() >= deadline)
            .unwrap_or(false)
        {
            runner_log_watch = None;
            runner_log_watch_until = None;
        }
        let detected_runner_failure = runner_log_watch.as_ref().and_then(|watch| {
            runner_failure_status_since(context.home_dir(), watch)
                .map(|status| (watch.run_id.clone(), status))
        });
        if let Some((watch_run_id, failure_status)) = detected_runner_failure {
            super::tui_debug::log(
                "runner_log.failure",
                &format!("run_id={watch_run_id} status={failure_status:?}"),
            );
            runner_log_watch = None;
            runner_log_watch_until = None;
            frozen_next_items = None;
            clear_mutation_poll_state(
                &mut mutation_poll_mode,
                &mut mutation_refresh_requested,
                &mut mutation_action_label,
                &mut active_ready_streak,
                &mut inactive_streak,
            );
            status_message = Some(failure_status);
            refresh_needed = true;
            next_refresh_at = Instant::now();
            show_mutation_loading(prompt, mutation_poll_mode, None, false)?;
        }
        if let Some(receiver) = pending_refresh.as_ref() {
            match receiver.try_recv() {
                Ok(loaded) => {
                    pending_refresh = None;
                    let loaded = match loaded {
                        Ok(data) => data,
                        Err(err) => {
                            // Tolerate transient "not found" right after
                            // a pivot to a fresh replay run id: the
                            // detached runner hasn't materialized
                            // `state.json` yet. Retry quietly until the
                            // grace window expires.
                            if pivot_grace_until
                                .map(|deadline| Instant::now() < deadline)
                                .unwrap_or(false)
                            {
                                super::tui_debug::log(
                                    "loop.refresh_recv.pivot_retry",
                                    &format!("run_id={run_id} err={err:#}"),
                                );
                                refresh_needed = true;
                                next_refresh_at = Instant::now() + pivot_grace_retry_delay;
                                last_spawn_fingerprint = None;
                                continue;
                            }
                            if let Some((previous_run_id, recovery_status)) =
                                recover_failed_fresh_run_pivot(
                                    context.home_dir(),
                                    &run_id,
                                    pivot_return_run_id.as_deref(),
                                    status_message.as_deref(),
                                    &err,
                                )
                            {
                                super::tui_debug::log(
                                    "loop.refresh_recv.pivot_failed",
                                    &format!(
                                        "pending_run_id={run_id} previous_run_id={previous_run_id} err={err:#}"
                                    ),
                                );
                                run_id = previous_run_id;
                                pivot_grace_until = None;
                                pivot_return_run_id = None;
                                clear_mutation_poll_state(
                                    &mut mutation_poll_mode,
                                    &mut mutation_refresh_requested,
                                    &mut mutation_action_label,
                                    &mut active_ready_streak,
                                    &mut inactive_streak,
                                );
                                frozen_next_items = None;
                                last_spawn_fingerprint = None;
                                status_message = Some(recovery_status);
                                refresh_needed = true;
                                next_refresh_at = Instant::now();
                                show_mutation_loading(prompt, mutation_poll_mode, None, false)?;
                                continue;
                            }
                            return Err(err);
                        }
                    };
                    pivot_grace_until = None;
                    pivot_return_run_id = None;
                    super::tui_debug::log(
                        "loop.refresh_recv",
                        &format!(
                            "run_id={run_id} status={} contained={} mutation_mode={:?}",
                            loaded.detail.summary.status,
                            loaded.detail.contained_runs.len(),
                            mutation_poll_mode,
                        ),
                    );
                    let focus_details = if focus_details_after_gate_decision {
                        match post_gate_decision_focus(loaded.detail.summary.status.as_str()) {
                            PostGateDecisionFocus::Apply => {
                                focus_details_after_gate_decision = false;
                                true
                            }
                            PostGateDecisionFocus::KeepPending => false,
                            PostGateDecisionFocus::Clear => {
                                focus_details_after_gate_decision = false;
                                false
                            }
                        }
                    } else {
                        false
                    };
                    let status_for_refresh = status_message.take();
                    let refreshed = apply_run_detail_loaded_data(
                        prompt,
                        &run_id,
                        selected_action,
                        last_refreshed
                            .as_ref()
                            .map(|refresh| refresh.actions.as_slice())
                            .unwrap_or(&[]),
                        status_for_refresh.as_deref(),
                        focus_details,
                        loaded,
                    )?;
                    selected_action = refreshed.selected_action;
                    advance_mutation_poll_mode(
                        &refreshed,
                        &mut mutation_poll_mode,
                        &mut mutation_refresh_requested,
                        &mut mutation_action_label,
                        &mut active_ready_streak,
                        &mut inactive_streak,
                    );
                    if mutation_commands_are_stable(mutation_poll_mode) {
                        frozen_next_items = None;
                    } else if let Some(items) = frozen_next_items.as_ref()
                        && let Some(picker_state) = prompt.picker_state.as_mut()
                    {
                        picker_state.title = "Next: (updating...)".to_string();
                        picker_state.items = items.clone();
                        picker_state.actionable = false;
                        picker_state.selected =
                            picker_state.selected.min(items.len().saturating_sub(1));
                    }
                    show_mutation_loading(
                        prompt,
                        mutation_poll_mode,
                        mutation_action_label.as_deref(),
                        refreshed.running
                            || mutation_status_is_running(refreshed.effective_run_status.as_str()),
                    )?;
                    let refresh_window = run_detail_refresh_window(
                        &refreshed,
                        mutation_poll_mode,
                        running_refresh_interval,
                        idle_refresh_interval,
                        terminal_refresh_interval,
                    );
                    next_refresh_at =
                        Instant::now() + clamp_for_warmup(refresh_window, refreshed.terminal);
                    last_refresh_completed_at = Instant::now();
                    last_refreshed = Some(refreshed);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    pending_refresh = None;
                    show_mutation_loading(
                        prompt,
                        mutation_poll_mode,
                        mutation_action_label.as_deref(),
                        last_refreshed
                            .as_ref()
                            .map(|state| {
                                state.running
                                    || mutation_status_is_running(
                                        state.effective_run_status.as_str(),
                                    )
                            })
                            .unwrap_or(false),
                    )?;
                }
            }
        }

        let has_active_tool_work = last_refreshed
            .as_ref()
            .map(|r| {
                r.running
                    || r.has_active_child
                    || mutation_status_is_running(r.effective_run_status.as_str())
            })
            .unwrap_or(false);
        // Treat `blocked` and `waiting` as polling-eligible: external
        // actors (other shells, the runner reaper, lease grants, gate
        // decisions issued from outside the TUI) can flip a stalled
        // viewer into an actively-running iteration at any moment. The
        // fingerprint cache at the spawn branch already short-circuits
        // when nothing on disk changed, so the polling cost stays trivial.
        // Truly terminal statuses (success/failed/canceled) keep the
        // long-idle cadence — and even those project up an active child
        // status via `effective_run_projection` when one exists.
        let externally_mutable = last_refreshed
            .as_ref()
            .map(|r| status_is_externally_mutable(r.effective_run_status.as_str()))
            .unwrap_or(false);
        // Step-boundary transient: progress is None for a non-terminal
        // run (e.g., right after a gate decision in a loop iteration,
        // before the next step is marked `pending`/`running` on disk).
        // Keeps the poll cadence hot for a tick or two so the panes
        // recover their content without requiring a key press.
        let transient_no_progress = last_refreshed
            .as_ref()
            .map(|r| r.transient_no_progress)
            .unwrap_or(false);
        if pending_refresh.is_none() {
            let force_polling =
                !matches!(mutation_poll_mode, MutationPollMode::Off) || runner_log_watch.is_some();
            let in_attach_warmup = Instant::now() < attach_warmup_until;
            let should_refresh = refresh_needed
                || (auto_refresh
                    && (has_active_tool_work
                        || force_polling
                        || externally_mutable
                        || transient_no_progress
                        || in_attach_warmup)
                    && Instant::now() >= next_refresh_at);
            if should_refresh {
                let load_context = context.clone();
                let load_run_id = run_id.to_string();
                let load_recipe_cache = recipe_cache.clone();
                let load_output_cache = output_cache.clone();
                if refresh_needed
                    && !mutation_refresh_requested
                    && matches!(mutation_poll_mode, MutationPollMode::Off)
                {
                    let status_for_refresh = status_message.take();
                    let selected_step_for_load =
                        prompt.run_detail_state.as_ref().and_then(|state| {
                            prompt
                                .current_progress
                                .as_ref()
                                .and_then(|progress| {
                                    progress.state.checklist.get(state.selected_step)
                                })
                                .map(|item| item.step_id.clone())
                                .or_else(|| {
                                    prompt.current_run_detail.as_ref().and_then(|detail| {
                                        detail
                                            .steps
                                            .get(state.selected_step)
                                            .map(|step| step.step_id.clone())
                                    })
                                })
                        });
                    let loaded = prompt.with_spinner("Loading run view", move || {
                        load_run_detail_data(
                            &load_context,
                            &load_run_id,
                            selected_step_for_load,
                            &load_recipe_cache,
                            &load_output_cache,
                        )
                    })?;
                    let focus_details = if focus_details_after_gate_decision {
                        match post_gate_decision_focus(loaded.detail.summary.status.as_str()) {
                            PostGateDecisionFocus::Apply => {
                                focus_details_after_gate_decision = false;
                                true
                            }
                            PostGateDecisionFocus::KeepPending => false,
                            PostGateDecisionFocus::Clear => {
                                focus_details_after_gate_decision = false;
                                false
                            }
                        }
                    } else {
                        false
                    };
                    let refreshed = apply_run_detail_loaded_data(
                        prompt,
                        &run_id,
                        selected_action,
                        last_refreshed
                            .as_ref()
                            .map(|refresh| refresh.actions.as_slice())
                            .unwrap_or(&[]),
                        status_for_refresh.as_deref(),
                        focus_details,
                        loaded,
                    )?;
                    selected_action = refreshed.selected_action;
                    advance_mutation_poll_mode(
                        &refreshed,
                        &mut mutation_poll_mode,
                        &mut mutation_refresh_requested,
                        &mut mutation_action_label,
                        &mut active_ready_streak,
                        &mut inactive_streak,
                    );
                    if mutation_commands_are_stable(mutation_poll_mode) {
                        frozen_next_items = None;
                    } else if let Some(items) = frozen_next_items.as_ref()
                        && let Some(picker_state) = prompt.picker_state.as_mut()
                    {
                        picker_state.title = "Next: (updating...)".to_string();
                        picker_state.items = items.clone();
                        picker_state.actionable = false;
                        picker_state.selected =
                            picker_state.selected.min(items.len().saturating_sub(1));
                    }
                    show_mutation_loading(
                        prompt,
                        mutation_poll_mode,
                        mutation_action_label.as_deref(),
                        refreshed.running
                            || mutation_status_is_running(refreshed.effective_run_status.as_str()),
                    )?;
                    let refresh_window = run_detail_refresh_window(
                        &refreshed,
                        mutation_poll_mode,
                        running_refresh_interval,
                        idle_refresh_interval,
                        terminal_refresh_interval,
                    );
                    next_refresh_at =
                        Instant::now() + clamp_for_warmup(refresh_window, refreshed.terminal);
                    last_refresh_completed_at = Instant::now();
                    refresh_needed = false;
                    last_refreshed = Some(refreshed);
                } else {
                    if !refresh_needed
                        && Instant::now().duration_since(last_refresh_completed_at)
                            < refresh_respawn_guard
                    {
                        continue;
                    }
                    // Skip the spawn when the run's persisted-file
                    // fingerprint is identical to the previous spawn AND
                    // the refresh wasn't explicitly requested (mutation
                    // or `r` key). The fingerprint covers `state.json`
                    // (step transitions, summary changes) and
                    // `events.jsonl` (runtime events, output streaming);
                    // any user-visible change to the run-detail screen
                    // requires one of those files to move. Two `stat`
                    // calls cost <10us versus a full inspection walk
                    // costing >100ms while artifact dirs are hot.
                    // Include every contained child run id currently known
                    // for this view in the fingerprint. The run-detail
                    // screen renders contained-child state inline (e.g.
                    // gate-blocked child of a parent that itself stays
                    // `blocked`), so a refresh that only watches the
                    // parent's files would miss child-side progress and
                    // leave the screen stale until the user pressed a key.
                    let contained_run_ids: Vec<String> = prompt
                        .current_run_detail
                        .as_ref()
                        .map(|detail| {
                            detail
                                .contained_runs
                                .iter()
                                .map(|child| child.run_id.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    let current_fingerprint =
                        run_detail_fingerprint(context.home_dir(), &run_id, &contained_run_ids);
                    if !refresh_needed
                        && !mutation_refresh_requested
                        && Some(&current_fingerprint) == last_spawn_fingerprint.as_ref()
                    {
                        super::tui_debug::log(
                            "loop.refresh_skip",
                            &format!(
                                "run_id={run_id} mutation_mode={:?} reason=fingerprint_match",
                                mutation_poll_mode,
                            ),
                        );
                        // Re-arm the next-refresh deadline using the
                        // same cadence the completed-refresh branch
                        // would have used, so we don't busy-loop here.
                        let refresh_window = last_refreshed
                            .as_ref()
                            .map(|refreshed| {
                                clamp_for_warmup(
                                    run_detail_refresh_interval(
                                        refreshed,
                                        running_refresh_interval,
                                        idle_refresh_interval,
                                        terminal_refresh_interval,
                                    ),
                                    refreshed.terminal,
                                )
                            })
                            .unwrap_or(running_refresh_interval);
                        next_refresh_at = Instant::now() + refresh_window;
                        last_refresh_completed_at = Instant::now();
                        continue;
                    }
                    last_spawn_fingerprint = Some(current_fingerprint);
                    super::tui_debug::log(
                        "loop.refresh_spawn",
                        &format!(
                            "run_id={run_id} refresh_needed={refresh_needed} mutation_req={mutation_refresh_requested} mutation_mode={mutation_poll_mode:?}",
                        ),
                    );
                    let (sender, receiver) = mpsc::channel();
                    let load_recipe_cache = recipe_cache.clone();
                    let load_output_cache = output_cache.clone();
                    let selected_step_for_load =
                        prompt.run_detail_state.as_ref().and_then(|state| {
                            prompt
                                .current_progress
                                .as_ref()
                                .and_then(|progress| {
                                    progress.state.checklist.get(state.selected_step)
                                })
                                .map(|item| item.step_id.clone())
                                .or_else(|| {
                                    prompt.current_run_detail.as_ref().and_then(|detail| {
                                        detail
                                            .steps
                                            .get(state.selected_step)
                                            .map(|step| step.step_id.clone())
                                    })
                                })
                        });
                    std::thread::spawn(move || {
                        let _ = sender.send(load_run_detail_data(
                            &load_context,
                            &load_run_id,
                            selected_step_for_load,
                            &load_recipe_cache,
                            &load_output_cache,
                        ));
                    });
                    pending_refresh = Some(receiver);
                    if mutation_refresh_requested {
                        refresh_needed = false;
                    }
                    let refresh_window = last_refreshed
                        .as_ref()
                        .map(|refreshed| {
                            clamp_for_warmup(
                                run_detail_refresh_interval(
                                    refreshed,
                                    running_refresh_interval,
                                    idle_refresh_interval,
                                    terminal_refresh_interval,
                                ),
                                refreshed.terminal,
                            )
                        })
                        .unwrap_or(running_refresh_interval);
                    next_refresh_at = Instant::now() + refresh_window;
                }
            }
        }

        let poll_timeout = if pending_refresh.is_some() {
            let force_polling =
                !matches!(mutation_poll_mode, MutationPollMode::Off) || runner_log_watch.is_some();
            if has_active_tool_work || force_polling {
                Duration::from_millis(150)
            } else {
                Duration::from_secs(3600)
            }
        } else if auto_refresh {
            let force_polling =
                !matches!(mutation_poll_mode, MutationPollMode::Off) || runner_log_watch.is_some();
            let in_attach_warmup = Instant::now() < attach_warmup_until;
            if has_active_tool_work
                || force_polling
                || externally_mutable
                || transient_no_progress
                || in_attach_warmup
            {
                let until_refresh = next_refresh_at.saturating_duration_since(Instant::now());
                if has_active_tool_work {
                    let until_spinner = next_spinner_at.saturating_duration_since(Instant::now());
                    until_refresh
                        .min(until_spinner)
                        .min(Duration::from_millis(250))
                } else {
                    until_refresh.min(Duration::from_millis(250))
                }
            } else {
                Duration::from_secs(3600)
            }
        } else {
            Duration::from_secs(3600)
        };
        if !event::poll(poll_timeout)? {
            if pending_refresh.is_some() || !matches!(mutation_poll_mode, MutationPollMode::Off) {
                super::tui_debug::log(
                    "poll.timeout",
                    &format!(
                        "pending={} mutation_mode={mutation_poll_mode:?} timeout_ms={}",
                        pending_refresh.is_some(),
                        poll_timeout.as_millis(),
                    ),
                );
            }
            if has_active_tool_work && Instant::now() >= next_spinner_at {
                prompt.spinner_tick = prompt.spinner_tick.wrapping_add(1);
                prompt.draw()?;
                next_spinner_at = Instant::now() + spinner_interval;
            }
            continue;
        }
        let event = event::read()?;
        let Some(refreshed) = last_refreshed.as_ref() else {
            match event {
                Event::Resize(_, _) => prompt.handle_terminal_resize()?,
                Event::Mouse(_) => {}
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match remap_ctrl_c_to_esc(key.code, key.modifiers) {
                        KeyCode::Esc => {
                            prompt.run_detail_screen_active = false;
                            prompt.run_detail_state = None;
                            prompt.current_progress = None;
                            prompt.current_home_dir = None;
                            prompt.current_run_detail = None;
                            prompt.picker_state = None;
                            prompt.set_mouse_capture_enabled(true)?;
                            prompt.draw()?;
                            return Ok(());
                        }
                        KeyCode::Char('q') if key.modifiers.is_empty() => {
                            if confirm_on_quit
                                && !confirm_quit_main_screen(
                                    prompt,
                                    "Leave this run screen and return to the previous shell view?",
                                )?
                            {
                                continue;
                            }
                            prompt.run_detail_screen_active = false;
                            prompt.run_detail_state = None;
                            prompt.current_progress = None;
                            prompt.current_home_dir = None;
                            prompt.current_run_detail = None;
                            prompt.picker_state = None;
                            prompt.set_mouse_capture_enabled(true)?;
                            prompt.draw()?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            continue;
        };
        match event {
            Event::Resize(_, _) => prompt.handle_terminal_resize()?,
            Event::Mouse(_) => {}
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => {
                    prompt.run_detail_screen_active = false;
                    prompt.run_detail_state = None;
                    prompt.current_progress = None;
                    prompt.current_home_dir = None;
                    prompt.current_run_detail = None;
                    prompt.picker_state = None;
                    prompt.set_mouse_capture_enabled(true)?;
                    prompt.draw()?;
                    return Ok(());
                }
                // Defense-in-depth: a second always-available escape
                // route. If a refresh worker stalls or a redraw glitch
                // hides the status bar, the user can still press `q` to
                // exit.
                KeyCode::Char('q') if key.modifiers.is_empty() => {
                    if confirm_on_quit
                        && !confirm_quit_main_screen(
                            prompt,
                            "Leave this run screen and return to the previous shell view?",
                        )?
                    {
                        continue;
                    }
                    prompt.run_detail_screen_active = false;
                    prompt.run_detail_state = None;
                    prompt.current_progress = None;
                    prompt.current_home_dir = None;
                    prompt.current_run_detail = None;
                    prompt.picker_state = None;
                    prompt.set_mouse_capture_enabled(true)?;
                    prompt.draw()?;
                    return Ok(());
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    status_message = request_run_cancellation_with_confirmation(
                        prompt,
                        &context,
                        &run_id,
                        matches!(
                            refreshed.effective_run_status.as_str(),
                            "running" | "waiting"
                        ),
                    )?;
                    refresh_needed = true;
                }
                KeyCode::BackTab => {
                    let has_actions = prompt
                        .picker_state
                        .as_ref()
                        .map(|picker| !picker.items.is_empty())
                        .unwrap_or(false);
                    if let Some(state) = prompt.run_detail_state.as_mut() {
                        state.focused_section =
                            run_detail_move_focus(state.focused_section, false, has_actions);
                        // Once the operator focuses the Steps or Details
                        // section we stop auto-following the runner's
                        // current step so their selection — and the
                        // details_cursor they're scrolling through —
                        // stays put across periodic refreshes.
                        if matches!(
                            state.focused_section,
                            RunDetailSection::Steps | RunDetailSection::Details
                        ) {
                            state.step_pinned_by_user = true;
                        }
                        prompt.draw()?;
                    }
                }
                KeyCode::Tab => {
                    let has_actions = prompt
                        .picker_state
                        .as_ref()
                        .map(|picker| !picker.items.is_empty())
                        .unwrap_or(false);
                    let mut actions_focused = false;
                    if let Some(state) = prompt.run_detail_state.as_mut() {
                        state.focused_section =
                            run_detail_move_focus(state.focused_section, true, has_actions);
                        if matches!(
                            state.focused_section,
                            RunDetailSection::Steps | RunDetailSection::Details
                        ) {
                            state.step_pinned_by_user = true;
                        }
                        actions_focused = run_detail_actions_focused(state.focused_section);
                    }
                    if actions_focused {
                        focus_run_detail_actions(prompt);
                    }
                    if prompt.run_detail_state.is_some() {
                        prompt.draw()?;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let (_, _, details_width, full_details_visible, _, _) = prompt
                        .terminal
                        .size()
                        .map(run_detail_section_metrics)
                        .unwrap_or((30, 10, 40, 10, 20, 10));
                    let picker_item_count = prompt
                        .picker_state
                        .as_ref()
                        .map(|state| state.items.len())
                        .unwrap_or(0);
                    let details_visible =
                        details_visible_rows_with_picker(full_details_visible, picker_item_count);
                    if let Some(state) = prompt.run_detail_state.as_mut() {
                        match state.focused_section {
                            RunDetailSection::Steps => {
                                let step_count = prompt
                                    .current_progress
                                    .as_ref()
                                    .map(|progress| progress.state.checklist.len())
                                    .or_else(|| {
                                        prompt
                                            .current_run_detail
                                            .as_ref()
                                            .map(|detail| detail.steps.len())
                                    })
                                    .unwrap_or(0);
                                if step_count > 0 {
                                    let previous_step = state.selected_step;
                                    state.selected_step =
                                        (state.selected_step + 1).min(step_count.saturating_sub(1));
                                    state.details_scroll = 0;
                                    state.details_cursor = 0;
                                    // Manual step navigation should always pin
                                    // selection to the operator's choice and
                                    // disable auto-follow on refresh.
                                    state.step_pinned_by_user = true;
                                    if state.selected_step != previous_step {
                                        refresh_needed = true;
                                    }
                                    prompt.draw()?;
                                }
                            }
                            RunDetailSection::Details => {
                                let detail_line_count = build_run_detail_history_lines(
                                    prompt.current_home_dir.as_deref(),
                                    prompt.current_run_detail.as_ref(),
                                    Some(&prompt.current_contained_run_details),
                                    prompt.current_progress.as_ref(),
                                    Some(state.selected_step),
                                    details_width,
                                    prompt.spinner_tick,
                                )
                                .lines
                                .len();
                                if detail_line_count > 0 {
                                    state.details_cursor = (state.details_cursor + 1)
                                        .min(detail_line_count.saturating_sub(1));
                                    follow_cursor_with_scroll(
                                        &mut state.details_scroll,
                                        state.details_cursor,
                                        details_visible,
                                        detail_line_count,
                                    );
                                    prompt.draw()?;
                                }
                            }
                            RunDetailSection::Actions => {
                                if frozen_next_items.is_some() {
                                    status_message =
                                        Some("Commands are updating... please wait.".to_string());
                                    continue;
                                }
                                if !refreshed.actions.is_empty() {
                                    state.selected_action =
                                        (state.selected_action + 1) % refreshed.actions.len();
                                    selected_action = state.selected_action;
                                    if let Some(picker_state) = prompt.picker_state.as_mut() {
                                        picker_state.selected = state.selected_action;
                                    }
                                    prompt.draw()?;
                                }
                            }
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let (_, _, details_width, full_details_visible, _, _) = prompt
                        .terminal
                        .size()
                        .map(run_detail_section_metrics)
                        .unwrap_or((30, 10, 40, 10, 20, 10));
                    let picker_item_count = prompt
                        .picker_state
                        .as_ref()
                        .map(|state| state.items.len())
                        .unwrap_or(0);
                    let details_visible =
                        details_visible_rows_with_picker(full_details_visible, picker_item_count);
                    if let Some(state) = prompt.run_detail_state.as_mut() {
                        match state.focused_section {
                            RunDetailSection::Steps => {
                                let step_count = prompt
                                    .current_progress
                                    .as_ref()
                                    .map(|progress| progress.state.checklist.len())
                                    .or_else(|| {
                                        prompt
                                            .current_run_detail
                                            .as_ref()
                                            .map(|detail| detail.steps.len())
                                    })
                                    .unwrap_or(0);
                                if step_count > 0 {
                                    let previous_step = state.selected_step;
                                    state.selected_step = state.selected_step.saturating_sub(1);
                                    state.details_scroll = 0;
                                    state.details_cursor = 0;
                                    // Manual step navigation should always pin
                                    // selection to the operator's choice and
                                    // disable auto-follow on refresh.
                                    state.step_pinned_by_user = true;
                                    if state.selected_step != previous_step {
                                        refresh_needed = true;
                                    }
                                    prompt.draw()?;
                                }
                            }
                            RunDetailSection::Details => {
                                let detail_line_count = build_run_detail_history_lines(
                                    prompt.current_home_dir.as_deref(),
                                    prompt.current_run_detail.as_ref(),
                                    Some(&prompt.current_contained_run_details),
                                    prompt.current_progress.as_ref(),
                                    Some(state.selected_step),
                                    details_width,
                                    prompt.spinner_tick,
                                )
                                .lines
                                .len();
                                if detail_line_count > 0 {
                                    state.details_cursor = state.details_cursor.saturating_sub(1);
                                    follow_cursor_with_scroll(
                                        &mut state.details_scroll,
                                        state.details_cursor,
                                        details_visible,
                                        detail_line_count,
                                    );
                                    prompt.draw()?;
                                }
                            }
                            RunDetailSection::Actions => {
                                if frozen_next_items.is_some() {
                                    status_message =
                                        Some("Commands are updating... please wait.".to_string());
                                    continue;
                                }
                                if !refreshed.actions.is_empty() {
                                    state.selected_action =
                                        (state.selected_action + refreshed.actions.len() - 1)
                                            % refreshed.actions.len();
                                    selected_action = state.selected_action;
                                    if let Some(picker_state) = prompt.picker_state.as_mut() {
                                        picker_state.selected = state.selected_action;
                                    }
                                    prompt.draw()?;
                                }
                            }
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(state) = prompt.run_detail_state.as_ref()
                        && !matches!(state.focused_section, RunDetailSection::Actions)
                    {
                        continue;
                    }
                    if frozen_next_items.is_some() {
                        status_message = Some("Commands are updating... please wait.".to_string());
                        continue;
                    }
                    if refreshed.actions.is_empty() {
                        continue;
                    }
                    let action = refreshed.actions[prompt
                        .run_detail_state
                        .as_ref()
                        .map(|state| state.selected_action)
                        .unwrap_or(selected_action)]
                    .clone();
                    let request_details_focus =
                        gate_decision_action_requests_details_focus(&action.execution);
                    let action_mutates = action.execution.is_mutating();
                    let action_label = action.execution.action_label().to_string();
                    let outcome =
                        execute_run_detail_action(prompt, &context, interrupt_flag, action)?;
                    match outcome {
                        RunDetailActionOutcome::Stay(message) => {
                            super::tui_debug::log(
                                "action.outcome",
                                &format!(
                                    "kind=Stay site=enter action={action_label:?} mutates={action_mutates} msg={message:?}"
                                ),
                            );
                            status_message = message;
                            refresh_needed = true;
                            if action_mutates && !mutation_was_canceled(status_message.as_deref()) {
                                frozen_next_items = prompt
                                    .picker_state
                                    .as_ref()
                                    .map(|picker| picker.items.clone());
                                invalidate_visible_run_cache(prompt, &run_id);
                                last_spawn_fingerprint = None;
                                mutation_action_label = Some(action_label.clone());
                                mutation_poll_mode = MutationPollMode::AwaitActive;
                                next_refresh_at = Instant::now();
                                mutation_refresh_requested = true;
                                active_ready_streak = 0;
                                inactive_streak = 0;
                                show_mutation_loading(
                                    prompt,
                                    mutation_poll_mode,
                                    mutation_action_label.as_deref(),
                                    false,
                                )?;
                            }
                        }
                        RunDetailActionOutcome::RefreshNow {
                            new_run_id,
                            status_message: spawn_status,
                            runner_log_watch: spawn_runner_log_watch,
                        } => {
                            super::tui_debug::log(
                                "action.outcome",
                                &format!(
                                    "kind=RefreshNow site=enter action={action_label:?} mutates={action_mutates} new_run_id={new_run_id} msg={spawn_status:?}"
                                ),
                            );
                            invalidate_visible_run_cache(prompt, &run_id);
                            invalidate_visible_run_cache(prompt, &new_run_id);
                            pivot_return_run_id = (new_run_id != run_id).then(|| run_id.clone());
                            run_id = new_run_id;
                            status_message = spawn_status;
                            runner_log_watch = spawn_runner_log_watch;
                            runner_log_watch_until = runner_log_watch
                                .as_ref()
                                .map(|_| Instant::now() + runner_log_watch_window);
                            refresh_needed = true;
                            // Arm pivot grace: the new run id may not
                            // yet have `state.json` on disk (detached
                            // runner is racing). Tolerate transient
                            // load failures for a few seconds.
                            pivot_grace_until = Some(Instant::now() + pivot_grace_window);
                            last_spawn_fingerprint = None;
                            focus_details_after_gate_decision = request_details_focus;
                            if action_mutates {
                                frozen_next_items = prompt
                                    .picker_state
                                    .as_ref()
                                    .map(|picker| picker.items.clone());
                                mutation_action_label = Some(action_label.clone());
                                mutation_poll_mode = MutationPollMode::AwaitActive;
                                next_refresh_at = Instant::now();
                                mutation_refresh_requested = true;
                                active_ready_streak = 0;
                                inactive_streak = 0;
                                show_mutation_loading(
                                    prompt,
                                    mutation_poll_mode,
                                    mutation_action_label.as_deref(),
                                    false,
                                )?;
                            }
                            // Pivoting to a different run id resets
                            // step auto-follow: the new run starts on
                            // its current step regardless of where the
                            // operator was looking on the prior run.
                            if let Some(state) = prompt.run_detail_state.as_mut() {
                                state.step_pinned_by_user = false;
                            }
                        }
                    }
                }
                KeyCode::Char(ch) if key.modifiers.is_empty() && ch.is_ascii_digit() => {
                    if frozen_next_items.is_some() {
                        status_message = Some("Commands are updating... please wait.".to_string());
                        continue;
                    }
                    if let Some(index) =
                        suggested_action_shortcut_index(ch, refreshed.actions.len())
                    {
                        selected_action = index;
                        if let Some(state) = prompt.run_detail_state.as_mut() {
                            state.selected_action = index;
                            state.focused_section = RunDetailSection::Actions;
                        }
                        let action = refreshed.actions[index].clone();
                        let request_details_focus =
                            gate_decision_action_requests_details_focus(&action.execution);
                        let action_mutates = action.execution.is_mutating();
                        let action_label = action.execution.action_label().to_string();
                        let outcome =
                            execute_run_detail_action(prompt, &context, interrupt_flag, action)?;
                        match outcome {
                            RunDetailActionOutcome::Stay(message) => {
                                super::tui_debug::log(
                                    "action.outcome",
                                    &format!(
                                        "kind=Stay site=digit action={action_label:?} mutates={action_mutates} msg={message:?}"
                                    ),
                                );
                                status_message = message;
                                refresh_needed = true;
                                if action_mutates
                                    && !mutation_was_canceled(status_message.as_deref())
                                {
                                    frozen_next_items = prompt
                                        .picker_state
                                        .as_ref()
                                        .map(|picker| picker.items.clone());
                                    invalidate_visible_run_cache(prompt, &run_id);
                                    last_spawn_fingerprint = None;
                                    mutation_action_label = Some(action_label.clone());
                                    mutation_poll_mode = MutationPollMode::AwaitActive;
                                    next_refresh_at = Instant::now();
                                    mutation_refresh_requested = true;
                                    active_ready_streak = 0;
                                    inactive_streak = 0;
                                    show_mutation_loading(
                                        prompt,
                                        mutation_poll_mode,
                                        mutation_action_label.as_deref(),
                                        false,
                                    )?;
                                }
                            }
                            RunDetailActionOutcome::RefreshNow {
                                new_run_id,
                                status_message: spawn_status,
                                runner_log_watch: spawn_runner_log_watch,
                            } => {
                                super::tui_debug::log(
                                    "action.outcome",
                                    &format!(
                                        "kind=RefreshNow site=digit action={action_label:?} mutates={action_mutates} new_run_id={new_run_id} msg={spawn_status:?}"
                                    ),
                                );
                                invalidate_visible_run_cache(prompt, &run_id);
                                invalidate_visible_run_cache(prompt, &new_run_id);
                                pivot_return_run_id =
                                    (new_run_id != run_id).then(|| run_id.clone());
                                run_id = new_run_id;
                                status_message = spawn_status;
                                runner_log_watch = spawn_runner_log_watch;
                                runner_log_watch_until = runner_log_watch
                                    .as_ref()
                                    .map(|_| Instant::now() + runner_log_watch_window);
                                refresh_needed = true;
                                pivot_grace_until = Some(Instant::now() + pivot_grace_window);
                                last_spawn_fingerprint = None;
                                focus_details_after_gate_decision = request_details_focus;
                                if action_mutates {
                                    frozen_next_items = prompt
                                        .picker_state
                                        .as_ref()
                                        .map(|picker| picker.items.clone());
                                    mutation_action_label = Some(action_label.clone());
                                    mutation_poll_mode = MutationPollMode::AwaitActive;
                                    next_refresh_at = Instant::now();
                                    mutation_refresh_requested = true;
                                    active_ready_streak = 0;
                                    inactive_streak = 0;
                                    show_mutation_loading(
                                        prompt,
                                        mutation_poll_mode,
                                        mutation_action_label.as_deref(),
                                        false,
                                    )?;
                                }
                                if let Some(state) = prompt.run_detail_state.as_mut() {
                                    state.step_pinned_by_user = false;
                                }
                            }
                        }
                    }
                }
                KeyCode::Char('r') => {
                    let _ = context.invalidate_inspection_cache(&run_id);
                    if let Some(detail) = prompt.current_run_detail.as_ref() {
                        for child in &detail.contained_runs {
                            let _ = context.invalidate_inspection_cache(&child.run_id);
                        }
                    }
                    status_message = Some("Run view refreshed.".to_string());
                    refresh_needed = true;
                }
                KeyCode::Char('x') => {
                    status_message = request_run_cancellation_with_confirmation(
                        prompt,
                        &context,
                        &run_id,
                        matches!(
                            refreshed.effective_run_status.as_str(),
                            "running" | "waiting"
                        ),
                    )?;
                    refresh_needed = true;
                }
                KeyCode::Char('o') => {
                    status_message = open_selected_detail_line(prompt, OpenWith::Pager)?;
                }
                KeyCode::Char('e') => {
                    status_message = open_selected_detail_line(prompt, OpenWith::Editor)?;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Which external tool the operator asked to open the highlighted file
/// with. `Pager` (bound to `o`) prefers the persisted preferred pager,
/// falls back to `$PAGER`, and finally to the in-process pager overlay.
/// `Editor` (bound to `e`) requires an explicitly configured preferred
/// editor; it does **not** consult `$EDITOR` and refuses with a status
/// message when no preference is set.
#[derive(Debug, Clone, Copy)]
enum OpenWith {
    Pager,
    Editor,
}

/// Resolve the operator's `o`/`e` keystroke against the highlighted
/// detail-pane row and open the underlying file with the selected tool.
/// The cursor row's `openable` field (populated when the row references a
/// workflow file, artifact, or evidence blob) selects the path. The TUI is
/// suspended for the duration of the external command and restored when
/// the child exits.
///
/// Returns a status-line message describing the outcome (a short hint when
/// the cursor sits on a non-openable row, an error description when the
/// file cannot be read, or `None` when the in-process pager overlay
/// handled drawing itself). The caller surfaces the message verbatim on
/// the status bar.
fn open_selected_detail_line(
    prompt: &mut FullscreenPrompt,
    open_with: OpenWith,
) -> Result<Option<String>> {
    use super::run_detail::build_selected_step_detail_lines_with_openables;
    use super::run_detail::build_selected_step_detail_lines_without_progress_with_openables;
    use super::run_detail::wrap_detail_pane_line;

    let Some(state) = prompt.run_detail_state.as_ref() else {
        return Ok(Some("No run-detail state available.".to_string()));
    };
    if !matches!(state.focused_section, RunDetailSection::Details) {
        let key = match open_with {
            OpenWith::Pager => "o",
            OpenWith::Editor => "e",
        };
        return Ok(Some(format!(
            "Press Tab to focus Details, then {key} to open the highlighted file."
        )));
    }

    let (_, _, details_width, _, _, _) = prompt
        .terminal
        .size()
        .map(run_detail_section_metrics)
        .unwrap_or((30, 10, 40, 10, 20, 10));
    let (raw_lines, raw_openables) = if let Some(progress) = prompt.current_progress.as_ref() {
        build_selected_step_detail_lines_with_openables(
            prompt.current_home_dir.as_deref(),
            prompt.current_run_detail.as_ref(),
            Some(&prompt.current_contained_run_details),
            progress,
            Some(state.selected_step),
            prompt.spinner_tick,
        )
    } else if let Some(run_detail) = prompt.current_run_detail.as_ref() {
        build_selected_step_detail_lines_without_progress_with_openables(
            prompt.current_home_dir.as_deref(),
            run_detail,
            Some(&prompt.current_contained_run_details),
            Some(state.selected_step),
        )
    } else {
        return Ok(Some("No run details to inspect.".to_string()));
    };
    if raw_lines.is_empty() {
        return Ok(Some("No detail lines to open.".to_string()));
    }

    // Re-derive the wrapped path so the cursor index lines up with what the
    // operator sees. Each source line wraps into N visible fragments, all
    // tagged with the source row's openable; we walk in lockstep using the
    // same wrap function the renderer uses (`wrap_detail_pane_line`).
    let width = details_width.max(1);
    let mut wrapped_openables: Vec<Option<std::path::PathBuf>> =
        Vec::with_capacity(raw_lines.len());
    for (idx, line) in raw_lines.iter().enumerate() {
        let openable = raw_openables.get(idx).cloned().flatten();
        for _ in wrap_detail_pane_line(line, width) {
            wrapped_openables.push(openable.clone());
        }
    }
    let cursor = state.details_cursor;
    let path = match wrapped_openables.get(cursor).and_then(|p| p.clone()) {
        Some(p) => p,
        None => {
            return Ok(Some(
                "No openable file on this row. Move with j/k to a workflow file, artifact, or evidence row.".to_string(),
            ));
        }
    };

    match open_with {
        OpenWith::Pager => open_path_in_pager(prompt, &path),
        OpenWith::Editor => open_path_in_editor(prompt, &path),
    }
}

/// Open `path` in the operator's preferred pager. Precedence:
///   1. Persisted "preferred pager" from user settings (set via the
///      Configuration screen). Most predictable for operators who
///      configure once and forget.
///   2. `$PAGER` env var, for per-shell overrides.
///   3. In-process pager overlay (`run_pager_modal`), capped to a
///      reasonable size so a multi-megabyte log does not stall the
///      redraw.
///
/// When an external command runs, the TUI is suspended (alternate screen
/// off, raw mode off, cursor restored) so the pager owns the terminal
/// with native key bindings. After the child exits the TUI is restored
/// and a forced redraw rebuilds the run-detail screen without artifacts.
fn open_path_in_pager(
    prompt: &mut FullscreenPrompt,
    path: &std::path::Path,
) -> Result<Option<String>> {
    let configured = transist::load_user_preferred_pager()?;
    let pager_cmd = configured.unwrap_or_else(|| std::env::var("PAGER").unwrap_or_default());
    let pager_cmd = pager_cmd.trim().to_string();
    if !pager_cmd.is_empty() {
        return run_external_tool(prompt, &pager_cmd, path, "pager");
    }
    run_inprocess_pager(prompt, path)
}

/// Open `path` in the operator's preferred editor. Unlike the pager flow
/// there is no env-var fallback: an editor must be explicitly configured
/// via the Configuration screen. This avoids accidentally launching a
/// modal vi-without-an-exit-strategy on operators who never opted in.
///
/// Behavior depends on the configured command:
/// - **Known GUI editor** (VS Code, Cursor, JetBrains, Sublime, Zed, …):
///   the editor is spawned detached and the TUI keeps running, so the
///   operator can read the file in another window while continuing to
///   interact with transist (e.g. typing a `--reason` for
///   `request-changes`).
/// - **TUI / unknown editor** (vim, nvim, helix, custom wrapper, …):
///   the TUI is suspended (alt screen off, raw mode off, main buffer
///   cleared) so the editor owns the terminal; transist resumes when
///   the editor exits.
pub(super) fn open_path_in_editor(
    prompt: &mut FullscreenPrompt,
    path: &std::path::Path,
) -> Result<Option<String>> {
    let editor_cmd = match transist::load_user_preferred_editor().ok().flatten() {
        Some(cmd) if !cmd.trim().is_empty() => cmd,
        _ => {
            return Ok(Some(
                "No preferred editor is configured. Set one in Preferences.".to_string(),
            ));
        }
    };
    run_external_tool(prompt, editor_cmd.trim(), path, "editor")
}

/// Suspend the TUI, run `<command> <path>`, then resume. `tool_label`
/// names the surface in status messages (`"pager"` or `"editor"`).
///
/// Argument splitting uses `shell-words` semantics so configured commands
/// like `"bat --paging=always"` or `"code --wait"` are interpreted as a
/// program plus argv rather than passed to a shell. Falling back to
/// `sh -c` only on parse failure keeps backward compatibility with users
/// who set a complex `$PAGER` value (e.g. with pipes).
///
/// Editors get an additional fast path: if the configured command's
/// `argv[0]` resolves to a known GUI editor (VS Code, Cursor, JetBrains,
/// Sublime, …) the TUI is **not** suspended; the editor is spawned
/// detached so the operator can read the file in another window while
/// the transist TUI remains fully interactive (e.g. to type a
/// `--reason` for `request-changes`). Pagers always take the
/// suspend-and-block path because pagers are inherently terminal UIs.
fn run_external_tool(
    prompt: &mut FullscreenPrompt,
    command: &str,
    path: &std::path::Path,
    tool_label: &str,
) -> Result<Option<String>> {
    use std::process::Command;

    let argv = shell_words::split(command).ok();
    let argv0_basename = argv
        .as_deref()
        .and_then(|a| a.first())
        .map(|p| editor_program_basename(p));

    if tool_label == "editor" && argv0_basename.as_deref().is_some_and(is_known_gui_editor) {
        return spawn_gui_editor_detached(command, argv.as_deref(), path);
    }

    let status = prompt.with_suspended_terminal(|| {
        Ok(match argv {
            Some(mut argv) if !argv.is_empty() => {
                let program = argv.remove(0);
                Command::new(program).args(argv).arg(path).status()
            }
            _ => Command::new("sh")
                .arg("-c")
                .arg(format!("{} \"{}\"", command, path.display()))
                .status(),
        })
    })?;
    match status {
        Ok(status) if status.success() => Ok(Some(format!(
            "Opened {} in {} ({}).",
            path.display(),
            tool_label,
            command
        ))),
        Ok(status) => Ok(Some(format!(
            "{} ({}) exited with status {}.",
            tool_label,
            command,
            status.code().unwrap_or(-1)
        ))),
        Err(err) => Ok(Some(format!(
            "Failed to spawn {} ({}): {}",
            tool_label, command, err
        ))),
    }
}

/// Spawn a GUI editor as a detached child without disturbing the TUI.
///
/// The TUI keeps drawing and accepting input. The editor's stdio is
/// redirected to `/dev/null` so any chatter (VS Code's `--wait` will log
/// to stderr in some setups) cannot smear the alt-screen frame. On Unix
/// the child is placed in its own process group so a Ctrl+C delivered
/// to transist does not also kill the editor.
///
/// Whether or not the user's configured command includes `--wait` is
/// irrelevant here: we never call `.wait()` ourselves, so the child runs
/// to completion in the background. Closing the editor window simply
/// reaps the child via the OS; transist neither knows nor cares.
fn spawn_gui_editor_detached(
    command: &str,
    argv: Option<&[String]>,
    path: &std::path::Path,
) -> Result<Option<String>> {
    use std::process::{Command, Stdio};

    let mut cmd = match argv {
        Some(argv) if !argv.is_empty() => {
            let program = &argv[0];
            let rest = &argv[1..];
            let mut c = Command::new(program);
            c.args(rest);
            c
        }
        _ => {
            // Fallback parity with the suspend path: hand the whole
            // string to a shell so pipes/expansions in pathological
            // configs still work.
            let mut c = Command::new("sh");
            c.arg("-c")
                .arg(format!("{} \"{}\"", command, path.display()));
            c
        }
    };

    cmd.arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and creates a fresh
        // session+process group for the child so it does not receive
        // SIGINT from the controlling terminal when the operator
        // presses Ctrl+C inside transist.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    match cmd.spawn() {
        Ok(_child) => Ok(Some(format!(
            "Opened {} in editor ({}). TUI remains active.",
            path.display(),
            command
        ))),
        Err(err) => Ok(Some(format!(
            "Failed to spawn editor ({}): {}",
            command, err
        ))),
    }
}

/// Extract the lowercase basename of an editor program path, stripping
/// any `.exe` suffix. Used by [`is_known_gui_editor`] for matching.
fn editor_program_basename(program: &str) -> String {
    std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase()
}

/// Conservative allow-list of GUI editor binary names. Returns true only
/// for editors that we are confident open in their own window and do
/// not need the terminal at all.
///
/// This is consulted **after** a hard-coded negative list of common TUI
/// editors so that an unusual symlink or alias (e.g. someone pointing
/// `code` at `nvim`) cannot accidentally trip the GUI path. False
/// positives here would cause the editor and the TUI to fight for the
/// terminal, so the rule is: when in doubt, stay on the safe
/// suspend-and-block path.
fn is_known_gui_editor(basename: &str) -> bool {
    // Hard-exclude known TUI editors first. If a user genuinely
    // configured a TUI editor we want the suspend+clear path.
    const TUI_EDITORS: &[&str] = &[
        "vi", "vim", "nvim", "neovim", "nano", "micro", "helix", "hx", "emacs", "joe", "ed", "ne",
        "kak", "kakoune", "less", "more", "bat",
    ];
    if TUI_EDITORS.contains(&basename) {
        return false;
    }

    const GUI_EDITORS: &[&str] = &[
        // VS Code family
        "code",
        "code-insiders",
        "codium",
        "vscodium",
        // VS Code forks
        "cursor",
        "windsurf",
        // Modern GUI editors
        "zed",
        "zeditor",
        // Sublime / TextMate / BBEdit
        "subl",
        "sublime_text",
        "mate",
        "smultron",
        "bbedit",
        // JetBrains family (common launcher names)
        "idea",
        "pycharm",
        "webstorm",
        "goland",
        "rustrover",
        "clion",
        "rider",
        "phpstorm",
        "rubymine",
        "datagrip",
        "fleet",
        "android-studio",
        "studio",
        // GNOME / KDE / other native GUI editors
        "gedit",
        "kate",
        "kwrite",
        "gnome-text-editor",
        "xed",
        "pluma",
        "leafpad",
        // Windows / macOS native
        "notepad",
        "notepad++",
        "notepad-plus-plus",
        "textedit",
        // Atom and lite-xl (still used)
        "atom",
        "lite-xl",
        "brackets",
    ];
    GUI_EDITORS.contains(&basename)
}

fn run_inprocess_pager(
    prompt: &mut FullscreenPrompt,
    path: &std::path::Path,
) -> Result<Option<String>> {
    // 5 MiB is enough for typical openspec proposals/tasks/design docs
    // plus modest logs without letting a runaway artifact (a multi-GB blob
    // a misbehaving workflow somehow recorded as an artifact) freeze the
    // redraw. The cap is intentionally generous; the operator can still
    // fall back to `$PAGER` for anything larger.
    const MAX_BYTES: u64 = 5 * 1024 * 1024;
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(err) => {
            return Ok(Some(format!("Cannot stat {}: {}", path.display(), err)));
        }
    };
    if metadata.len() > MAX_BYTES {
        return Ok(Some(format!(
            "{} is {} bytes (> {} cap); set PAGER to view externally.",
            path.display(),
            metadata.len(),
            MAX_BYTES
        )));
    }
    let body = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) => {
            return Ok(Some(format!("Cannot read {}: {}", path.display(), err)));
        }
    };
    let title = format!("Pager: {}", path.display());
    prompt.run_pager_modal(&title, &body)?;
    Ok(None)
}

fn run_detail_refresh_interval(
    refreshed: &RunDetailRefresh,
    running_interval: Duration,
    idle_interval: Duration,
    terminal_interval: Duration,
) -> Duration {
    if refreshed.running {
        running_interval
    } else if refreshed.terminal {
        terminal_interval
    } else {
        idle_interval
    }
}

/// Cheap fingerprint of the run's two most-frequently-updated persisted
/// files, sampled before spawning the background `load_run_detail_data`
/// worker. While a workflow is active, the runner rewrites `state.json`
/// on every step transition and appends to `events.jsonl` on every
/// runtime event. Comparing this fingerprint to the previous spawn's
/// lets the auto-refresh loop skip the worker entirely when nothing
/// observable has changed since the last refresh.
///
/// Each component is `Some(metadata)` when the file existed and could be
/// stat-ed, and `None` otherwise; `None` values still participate in
/// equality so a file disappearing or first appearing reliably forces a
/// re-spawn.
///
/// The fingerprint covers the parent run **and every contained child run
/// currently surfaced in the run-detail screen**. The run-detail screen
/// renders contained-child status, progress, and gate state inline; if we
/// only sampled the parent's files, a workflow where the active work
/// happens in a child run (e.g. a gate decision approved on the parent
/// that unblocks a `__plan_loop` child) would leave the parent's files
/// untouched for many seconds while the child runs, and the spawn-skip
/// branch would suppress refreshes until the user pressed a key. Sampling
/// every known contained run ensures any progress visible on the screen
/// also moves the fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunDetailFingerprint {
    entries: Vec<RunDetailFingerprintEntry>,
}

/// File-fingerprint tuple (state.json, events.jsonl, cancel-request.json)
/// for a single run id.
type RunDetailFingerprintEntry = (
    Option<(i128, u64)>,
    Option<(i128, u64)>,
    Option<(i128, u64)>,
);

fn run_detail_fingerprint(
    home_dir: &Path,
    run_id: &str,
    contained_run_ids: &[String],
) -> RunDetailFingerprint {
    let mut entries = Vec::with_capacity(1 + contained_run_ids.len());
    entries.push(fingerprint_entry_for_run(home_dir, run_id));
    for child_id in contained_run_ids {
        entries.push(fingerprint_entry_for_run(home_dir, child_id));
    }
    RunDetailFingerprint { entries }
}

fn fingerprint_entry_for_run(home_dir: &Path, run_id: &str) -> RunDetailFingerprintEntry {
    let Ok(run_root) = transist::TransistContext::with_home(home_dir, home_dir).run_root(run_id)
    else {
        return (None, None, None);
    };
    let cancel_request = home_dir
        .join("control")
        .join("runs")
        .join(run_id)
        .join("cancel-request.json");
    (
        file_fingerprint_tuple(&run_root.join("state.json")),
        file_fingerprint_tuple(&run_root.join("events.jsonl")),
        file_fingerprint_tuple(&cancel_request),
    )
}

fn file_fingerprint_tuple(path: &Path) -> Option<(i128, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0);
    Some((mtime_nanos, metadata.len()))
}

#[cfg(test)]
mod ctrl_c_remap_tests {
    use super::*;

    #[test]
    fn ctrl_c_remaps_to_esc() {
        assert_eq!(
            remap_ctrl_c_to_esc(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyCode::Esc
        );
    }

    #[test]
    fn lowercase_c_without_modifiers_passes_through() {
        assert_eq!(
            remap_ctrl_c_to_esc(KeyCode::Char('c'), KeyModifiers::NONE),
            KeyCode::Char('c')
        );
    }

    #[test]
    fn uppercase_c_with_control_does_not_match_lowercase_check() {
        // Crossterm reports Ctrl+C as Char('c') regardless of shift; uppercase
        // 'C' from a normal Shift+C press should not be treated as quit.
        assert_eq!(
            remap_ctrl_c_to_esc(KeyCode::Char('C'), KeyModifiers::SHIFT),
            KeyCode::Char('C')
        );
    }

    #[test]
    fn other_control_keys_pass_through() {
        assert_eq!(
            remap_ctrl_c_to_esc(KeyCode::Char('d'), KeyModifiers::CONTROL),
            KeyCode::Char('d')
        );
    }

    #[test]
    fn esc_passes_through_unchanged() {
        assert_eq!(
            remap_ctrl_c_to_esc(KeyCode::Esc, KeyModifiers::NONE),
            KeyCode::Esc
        );
    }
}

#[cfg(test)]
mod gui_editor_detection_tests {
    use super::{editor_program_basename, is_known_gui_editor};

    #[test]
    fn known_gui_editors_are_detected() {
        for name in [
            "code",
            "code-insiders",
            "cursor",
            "windsurf",
            "zed",
            "subl",
            "idea",
            "pycharm",
            "webstorm",
            "goland",
            "rustrover",
            "fleet",
            "gedit",
            "kate",
        ] {
            assert!(
                is_known_gui_editor(name),
                "expected `{name}` to be classified as GUI editor"
            );
        }
    }

    #[test]
    fn known_tui_editors_are_not_classified_as_gui() {
        for name in [
            "vi", "vim", "nvim", "neovim", "nano", "micro", "helix", "hx", "emacs", "less", "more",
            "bat",
        ] {
            assert!(
                !is_known_gui_editor(name),
                "expected `{name}` NOT to be classified as GUI editor"
            );
        }
    }

    #[test]
    fn unknown_editors_default_to_safe_tui_path() {
        // Unknown / wrapper / custom binaries must fall through to the
        // safe suspend-and-block path. is_known_gui_editor returning
        // false here is what guarantees that.
        for name in ["edit", "my-editor", "custom-wrapper", "ed-wrapper", ""] {
            assert!(
                !is_known_gui_editor(name),
                "expected unknown `{name}` to take the safe TUI path"
            );
        }
    }

    #[test]
    fn basename_strips_path_and_exe_suffix_and_lowercases() {
        assert_eq!(editor_program_basename("/usr/local/bin/code"), "code");
        assert_eq!(
            editor_program_basename("/Applications/Cursor.app/Contents/MacOS/Cursor"),
            "cursor"
        );
        // file_stem strips the trailing extension regardless of its
        // contents, so `Code.exe` and `Code.EXE` both reduce to `code`
        // after lowercasing. Note: on Unix runners std::path::Path
        // does not interpret backslashes as separators, so we do not
        // assert against Windows-style absolute paths here.
        assert_eq!(editor_program_basename("Code.exe"), "code");
        assert_eq!(editor_program_basename("Code.EXE"), "code");
        assert_eq!(editor_program_basename("nvim"), "nvim");
    }

    #[test]
    fn full_path_to_gui_editor_is_detected_via_basename() {
        // Real-world case: shell-words may yield an absolute path if
        // the user configured `/usr/local/bin/code --wait`.
        let basename = editor_program_basename("/usr/local/bin/code");
        assert!(is_known_gui_editor(&basename));
    }

    #[test]
    fn full_path_to_tui_editor_is_not_misclassified() {
        let basename = editor_program_basename("/usr/bin/nvim");
        assert!(!is_known_gui_editor(&basename));
    }
}

#[cfg(test)]
mod mutation_poll_mode_tests {
    use super::{
        MutationPollMode, STALE_RESUME_TERMINAL_SNAPSHOT_LIMIT, advance_mutation_poll_mode,
        run_detail_refresh_interval, run_detail_refresh_window,
    };
    use crate::ui::fullscreen::run_detail::RunDetailRefresh;
    use std::time::Duration;

    fn refreshed(status: &str, running: bool, terminal: bool) -> RunDetailRefresh {
        refreshed_with(status, running, terminal, false)
    }

    fn refreshed_with(
        status: &str,
        running: bool,
        terminal: bool,
        has_active_child: bool,
    ) -> RunDetailRefresh {
        RunDetailRefresh {
            actions: Vec::new(),
            selected_action: 0,
            running,
            effective_run_status: status.to_string(),
            terminal,
            transient_no_progress: false,
            has_active_child,
        }
    }

    #[test]
    fn await_active_clears_when_run_reblocks_without_live_tool_output() {
        let refreshed = refreshed("blocked", false, false);
        let mut mode = MutationPollMode::AwaitActive;
        let mut refresh_requested = true;
        let mut action_label = Some("retry".to_string());
        let mut active_ready_streak = 0;
        let mut inactive_streak = 0;

        advance_mutation_poll_mode(
            &refreshed,
            &mut mode,
            &mut refresh_requested,
            &mut action_label,
            &mut active_ready_streak,
            &mut inactive_streak,
        );

        assert_eq!(mode, MutationPollMode::Off);
        assert!(!refresh_requested);
        assert!(action_label.is_none());
    }

    #[test]
    fn await_active_keeps_polling_while_stale_failed_resume_snapshots_are_within_grace() {
        let refreshed = refreshed("failed", false, true);
        let mut mode = MutationPollMode::AwaitActive;
        let mut refresh_requested = true;
        let mut action_label = Some("retry".to_string());
        let mut active_ready_streak = 0;
        let mut inactive_streak = 0;

        for expected_streak in 1..STALE_RESUME_TERMINAL_SNAPSHOT_LIMIT {
            advance_mutation_poll_mode(
                &refreshed,
                &mut mode,
                &mut refresh_requested,
                &mut action_label,
                &mut active_ready_streak,
                &mut inactive_streak,
            );

            assert_eq!(mode, MutationPollMode::AwaitActive);
            assert!(refresh_requested);
            assert_eq!(action_label.as_deref(), Some("retry"));
            assert_eq!(inactive_streak, expected_streak);
        }
    }

    #[test]
    fn await_active_clears_after_stale_failed_resume_grace_expires() {
        let refreshed = refreshed("failed", false, true);
        let mut mode = MutationPollMode::AwaitActive;
        let mut refresh_requested = true;
        let mut action_label = Some("retry".to_string());
        let mut active_ready_streak = 0;
        let mut inactive_streak = STALE_RESUME_TERMINAL_SNAPSHOT_LIMIT - 1;

        advance_mutation_poll_mode(
            &refreshed,
            &mut mode,
            &mut refresh_requested,
            &mut action_label,
            &mut active_ready_streak,
            &mut inactive_streak,
        );

        assert_eq!(mode, MutationPollMode::Off);
        assert!(!refresh_requested);
        assert!(action_label.is_none());
        assert_eq!(inactive_streak, 0);
    }

    #[test]
    fn await_active_promotes_running_status_without_live_preview_signal() {
        let refreshed = refreshed("running", false, false);
        let mut mode = MutationPollMode::AwaitActive;
        let mut refresh_requested = true;
        let mut action_label = Some("retry".to_string());
        let mut active_ready_streak = 0;
        let mut inactive_streak = 0;

        advance_mutation_poll_mode(
            &refreshed,
            &mut mode,
            &mut refresh_requested,
            &mut action_label,
            &mut active_ready_streak,
            &mut inactive_streak,
        );
        assert_eq!(mode, MutationPollMode::AwaitActive);

        advance_mutation_poll_mode(
            &refreshed,
            &mut mode,
            &mut refresh_requested,
            &mut action_label,
            &mut active_ready_streak,
            &mut inactive_streak,
        );

        assert_eq!(mode, MutationPollMode::UntilInactive);
        assert!(refresh_requested);
        assert_eq!(action_label.as_deref(), Some("retry"));
    }

    #[test]
    fn active_mutation_poll_keeps_hot_refresh_even_on_terminal_snapshot() {
        let refreshed = refreshed("failed", false, true);

        assert_eq!(
            run_detail_refresh_window(
                &refreshed,
                MutationPollMode::AwaitActive,
                Duration::from_millis(500),
                Duration::from_secs(3),
                Duration::from_secs(3600),
            ),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn terminal_snapshot_uses_terminal_interval_once_mutation_poll_is_off() {
        let refreshed = refreshed("failed", false, true);

        assert_eq!(
            run_detail_refresh_window(
                &refreshed,
                MutationPollMode::Off,
                Duration::from_millis(500),
                Duration::from_secs(3),
                Duration::from_secs(3600),
            ),
            run_detail_refresh_interval(
                &refreshed,
                Duration::from_millis(500),
                Duration::from_secs(3),
                Duration::from_secs(3600),
            )
        );
    }

    /// After a mutating action (e.g. Approve /
    /// request-changes) spawns a child run, the parent's own status stays
    /// `blocked`. The old logic treated `blocked` as fully settled and
    /// dropped the hot-poll cadence to idle, freezing the screen until the
    /// user pressed a key. With `has_active_child=true` the poll mode must
    /// stay alive so the just-spawned child's progress is picked up.
    #[test]
    fn await_active_stays_hot_when_parent_blocked_with_active_child() {
        let refreshed = refreshed_with("blocked", false, false, true);
        let mut mode = MutationPollMode::AwaitActive;
        let mut refresh_requested = true;
        let mut action_label = Some("request-changes".to_string());
        let mut active_ready_streak = 0;
        let mut inactive_streak = 0;

        // First tick: counts toward the active-ready streak instead of
        // clearing to Off.
        advance_mutation_poll_mode(
            &refreshed,
            &mut mode,
            &mut refresh_requested,
            &mut action_label,
            &mut active_ready_streak,
            &mut inactive_streak,
        );
        assert_eq!(mode, MutationPollMode::AwaitActive);
        assert!(refresh_requested);

        // Second tick promotes to UntilInactive (hot-poll continues).
        advance_mutation_poll_mode(
            &refreshed,
            &mut mode,
            &mut refresh_requested,
            &mut action_label,
            &mut active_ready_streak,
            &mut inactive_streak,
        );
        assert_eq!(mode, MutationPollMode::UntilInactive);
        assert!(refresh_requested);
    }

    /// Once the contained child settles (`success`/`failed`/`canceled`),
    /// the `has_active_child` override goes away and a parent that is
    /// still `blocked` clears back to `Off` as expected.
    #[test]
    fn until_inactive_clears_once_child_settles_and_parent_stays_blocked() {
        let mut mode = MutationPollMode::UntilInactive;
        let mut refresh_requested = true;
        let mut action_label = Some("request-changes".to_string());
        let mut active_ready_streak = 0;
        let mut inactive_streak = 0;

        // While the child is active we stay in UntilInactive across ticks.
        let active = refreshed_with("blocked", false, false, true);
        for _ in 0..3 {
            advance_mutation_poll_mode(
                &active,
                &mut mode,
                &mut refresh_requested,
                &mut action_label,
                &mut active_ready_streak,
                &mut inactive_streak,
            );
            assert_eq!(mode, MutationPollMode::UntilInactive);
        }

        // Child settles → parent_blocked_with_active_child becomes false,
        // and the settled-status branch fires.
        let quiescent = refreshed_with("blocked", false, false, false);
        advance_mutation_poll_mode(
            &quiescent,
            &mut mode,
            &mut refresh_requested,
            &mut action_label,
            &mut active_ready_streak,
            &mut inactive_streak,
        );
        assert_eq!(mode, MutationPollMode::Off);
        assert!(!refresh_requested);
        assert!(action_label.is_none());
    }
}

#[cfg(test)]
mod external_mutability_tests {
    use super::status_is_externally_mutable;

    #[test]
    fn statuses_that_can_flip_to_running_stay_poll_eligible() {
        for status in [
            "blocked",
            "waiting",
            "failed",
            "interrupted",
            "created",
            "planned",
            "running",
        ] {
            assert!(
                status_is_externally_mutable(status),
                "status {status} should keep polling active"
            );
        }
    }

    #[test]
    fn terminal_success_and_canceled_do_not_force_polling() {
        assert!(!status_is_externally_mutable("success"));
        assert!(!status_is_externally_mutable("canceled"));
    }
}

#[cfg(test)]
mod failed_fresh_run_pivot_tests {
    use super::recover_failed_fresh_run_pivot;
    use anyhow::anyhow;
    use std::fs;

    #[test]
    fn prefers_runner_log_failure_signal_over_generic_not_actionable_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let log_path = crate::runner::runner_log_path(&home_dir, "replay-run");
        fs::create_dir_all(log_path.parent().expect("runner log dir"))
            .expect("create runner log dir");
        fs::write(
            &log_path,
            concat!(
                "{\"schema\":\"transist.runner_event.v1\",\"event\":\"started\"}\n",
                "Error: command dispatch failed\n",
                "Caused by: source run cannot be replayed anymore\n"
            ),
        )
        .expect("write runner log");

        let recovered = recover_failed_fresh_run_pivot(
            &home_dir,
            "replay-run",
            Some("source-run"),
            Some("Spawned detached runner (pid 123). Refreshing run view..."),
            &anyhow!("run 'replay-run' was not found or not actionable"),
        )
        .expect("recover failed pivot");

        assert_eq!(recovered.0, "source-run");
        assert_eq!(
            recovered.1,
            "Detached runner failed: Error: command dispatch failed; Caused by: source run cannot be replayed anymore"
        );
    }

    #[test]
    fn preserves_existing_detached_runner_failure_when_log_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");

        let recovered = recover_failed_fresh_run_pivot(
            &home_dir,
            "replay-run",
            Some("source-run"),
            Some(
                "Detached runner failed: runner exited with code 1: Error: command dispatch failed",
            ),
            &anyhow!("run 'replay-run' was not found or not actionable"),
        )
        .expect("recover failed pivot");

        assert_eq!(recovered.0, "source-run");
        assert_eq!(
            recovered.1,
            "Detached runner failed: runner exited with code 1: Error: command dispatch failed"
        );
    }
}

#[cfg(test)]
mod runner_failure_watch_tests {
    use super::{RunnerLogWatch, runner_failure_status_since};
    use std::fs;

    #[test]
    fn surfaces_failure_lines_written_after_spawn_offset() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let run_id = "late-failure-run";
        let log_path = crate::runner::runner_log_path(&home_dir, run_id);
        fs::create_dir_all(log_path.parent().expect("runner log dir"))
            .expect("create runner log dir");
        let initial = "{\"schema\":\"transist.runner_event.v1\",\"event\":\"started\"}\n";
        fs::write(
            &log_path,
            format!("{initial}Error: command dispatch failed\nCaused by: run cannot continue\n"),
        )
        .expect("write runner log");

        let status = runner_failure_status_since(
            &home_dir,
            &RunnerLogWatch {
                run_id: run_id.to_string(),
                start_offset: initial.len() as u64,
            },
        )
        .expect("failure status");

        assert_eq!(
            status,
            "Detached runner failed: Error: command dispatch failed; Caused by: run cannot continue"
        );
    }

    #[test]
    fn ignores_runner_event_lines_after_spawn_offset() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let run_id = "event-only-run";
        let log_path = crate::runner::runner_log_path(&home_dir, run_id);
        fs::create_dir_all(log_path.parent().expect("runner log dir"))
            .expect("create runner log dir");
        fs::write(
            &log_path,
            concat!(
                "{\"schema\":\"transist.runner_event.v1\",\"event\":\"started\"}\n",
                "{\"schema\":\"transist.runner_event.v1\",\"event\":\"finished\"}\n"
            ),
        )
        .expect("write runner log");

        let status = runner_failure_status_since(
            &home_dir,
            &RunnerLogWatch {
                run_id: run_id.to_string(),
                start_offset: 0,
            },
        );

        assert!(status.is_none());
    }
}

#[cfg(test)]
mod run_detail_fingerprint_tests {
    use super::*;
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;
    use transist::{RunLineage, RunLineageMode};
    use transist_storage_local::run_paths::{plan_run_locator, runs_root, write_run_locator};

    fn write_run_locator_and_dir(home_dir: &Path, run_id: &str, lineage: Option<&RunLineage>) {
        let locator = plan_run_locator(home_dir, run_id, lineage).expect("plan locator");
        write_run_locator(home_dir, &locator).expect("write locator");
        let rel = locator.relative_path_buf().expect("relative path");
        fs::create_dir_all(runs_root(home_dir).join(&rel)).expect("run dir");
    }

    fn touch_state(home_dir: &Path, run_id: &str, contents: &str) {
        let run_root = transist::TransistContext::with_home(home_dir, home_dir)
            .run_root(run_id)
            .expect("resolve");
        fs::write(run_root.join("state.json"), contents).expect("write state.json");
    }

    fn touch_cancel_request(home_dir: &Path, run_id: &str, contents: &str) {
        let control_dir = home_dir.join("control").join("runs").join(run_id);
        fs::create_dir_all(&control_dir).expect("create control run dir");
        fs::write(control_dir.join("cancel-request.json"), contents)
            .expect("write cancel-request.json");
    }

    /// Regression: a child-only mutation must move the fingerprint when the
    /// parent's contained children are included in the sample set. Without
    /// this, the spawn-skip optimization on the run-detail screen would keep
    /// suppressing refreshes after Approve until the user pressed a key.
    #[test]
    fn fingerprint_changes_when_child_state_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let parent_id = "root-run";
        let child_id = "root-run__child";

        write_run_locator_and_dir(&home_dir, parent_id, None);
        write_run_locator_and_dir(
            &home_dir,
            child_id,
            Some(&RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: parent_id.to_string(),
                parent_run_id: Some(parent_id.to_string()),
                parent_step_id: Some("child".to_string()),
            }),
        );

        touch_state(&home_dir, parent_id, "{\"status\":\"blocked\"}");
        touch_state(&home_dir, child_id, "{\"status\":\"blocked\"}");

        let children = vec![child_id.to_string()];
        let before = run_detail_fingerprint(&home_dir, parent_id, &children);

        // mtime granularity on some filesystems is >=1ms; sleep to ensure
        // the new write differs.
        sleep(Duration::from_millis(20));
        touch_state(&home_dir, child_id, "{\"status\":\"running\"}");

        let after_with_children = run_detail_fingerprint(&home_dir, parent_id, &children);
        assert_ne!(
            before, after_with_children,
            "child mutation must change the fingerprint when contained runs are sampled"
        );

        // Sanity check: with no contained ids, the parent-only fingerprint
        // is identical even though the child changed - documenting the bug
        // this fix addresses.
        let parent_only_before = run_detail_fingerprint(&home_dir, parent_id, &[]);
        let parent_only_after = run_detail_fingerprint(&home_dir, parent_id, &[]);
        assert_eq!(
            parent_only_before, parent_only_after,
            "parent-only fingerprint should be insensitive to child changes"
        );
    }

    #[test]
    fn fingerprint_stable_when_nothing_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let parent_id = "root-run";
        write_run_locator_and_dir(&home_dir, parent_id, None);
        touch_state(&home_dir, parent_id, "{\"status\":\"blocked\"}");

        let a = run_detail_fingerprint(&home_dir, parent_id, &[]);
        let b = run_detail_fingerprint(&home_dir, parent_id, &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_changes_when_cancel_request_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let parent_id = "root-run";
        write_run_locator_and_dir(&home_dir, parent_id, None);
        touch_state(&home_dir, parent_id, "{\"status\":\"running\"}");

        let before = run_detail_fingerprint(&home_dir, parent_id, &[]);
        sleep(Duration::from_millis(20));
        touch_cancel_request(
            &home_dir,
            parent_id,
            "{\"request_type\":\"cancel\",\"request_id\":\"r1\"}",
        );
        let after = run_detail_fingerprint(&home_dir, parent_id, &[]);

        assert_ne!(
            before, after,
            "cancel request writes must move the run-detail fingerprint"
        );
    }
}
