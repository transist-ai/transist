// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect, Size};
use serde::Deserialize;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};

use transist::TransistInspectionContext;

use crate::InspectionSyncSummary;
use crate::commands::workflow_reports::{render_authorization_summary, render_check_summary};
use crate::prompt::{PromptUi, run_with_loading_feedback};

use super::diagnostics::{DiagnosticReport, DiagnosticSeverity, SessionDiagnostic};
use super::problems::{ProblemItem, collect_run_problems, problem_count};
use super::screens;
use super::{
    FullscreenPrompt, LoadingState, StepHint, StepOutcome, confirm_cancel_with_data_loss,
    home_source_label, launchable_workflow_refs, load_root_run_summaries,
    pick_launchable_workflow_with_back, prompt_optional_run_ref_with_back,
    prompt_workflow_inputs_with_back, transist_home_is_initialized,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DashboardSection {
    Setup,
    Inventory,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DashboardActionId {
    LaunchWorkflow,
    ShowRunsList,
    Problems,
    Preferences,
    Refresh,
    Quit,
    SetPreferredPager,
    SetGateActor,
    SetPreferredEditor,
    SetInspectionCacheRuns,
}

#[derive(Debug, Clone)]
pub(super) struct DashboardActionItem {
    pub(super) action: DashboardActionId,
    pub(super) label: String,
}

#[derive(Debug, Clone)]
pub(super) struct DashboardState {
    pub(super) header_lines: Vec<String>,
    pub(super) setup_lines: Vec<String>,
    pub(super) setup_error_line_range: Option<Range<usize>>,
    pub(super) inventory_lines: Vec<String>,
    pub(super) action_items: Vec<DashboardActionItem>,
    pub(super) selected_action: usize,
    pub(super) focused_section: DashboardSection,
    pub(super) setup_scroll: usize,
    pub(super) inventory_scroll: usize,
}

#[derive(Debug, Clone)]
pub(super) struct DashboardData {
    pub(super) context: transist::TransistContext,
    pub(super) initialized: bool,
    pub(super) run_problem_items: Vec<ProblemItem>,
    pub(super) plugin_inventory_diagnostic: Option<DiagnosticReport>,
    pub(super) workflow_inventory_diagnostic: Option<DiagnosticReport>,
    pub(super) workflows: Vec<transist::InspectionWorkflowSummary>,
    pub(super) dashboard_state: DashboardState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UiStatusTone {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub(super) struct UiStatusMessage {
    pub(super) lines: Vec<String>,
    pub(super) tone: UiStatusTone,
}

impl UiStatusMessage {
    pub(super) fn info(message: impl Into<String>) -> Self {
        Self {
            lines: vec![message.into()],
            tone: UiStatusTone::Info,
        }
    }

    pub(super) fn from_error(error: &anyhow::Error) -> Self {
        Self {
            lines: crate::signals::format_error_lines(error),
            tone: UiStatusTone::Error,
        }
    }

    pub(super) fn diagnostic_summary(report: &DiagnosticReport) -> Self {
        Self {
            lines: vec![format!(
                "{} Open Problems for full diagnostic.",
                report.summary
            )],
            tone: UiStatusTone::Error,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct UiSectionLines {
    pub(super) lines: Vec<String>,
    pub(super) error_line_range: Option<Range<usize>>,
}

pub(super) fn load_inspection_sync_summary(
    repo_root: &Path,
    home_dir: &Path,
    resolved: &transist::ResolvedHome,
) -> InspectionSyncSummary {
    crate::load_inspection_sync_summary_extension(repo_root, home_dir, resolved)
}

pub(super) fn restore_dashboard_view_state(previous: &DashboardState, next: &mut DashboardState) {
    next.focused_section = previous.focused_section;
    next.setup_scroll = previous.setup_scroll;
    next.inventory_scroll = previous.inventory_scroll;
    next.selected_action = previous
        .selected_action
        .min(next.action_items.len().saturating_sub(1));
}

pub(super) fn update_dashboard_problem_count(
    dashboard: &mut DashboardData,
    session_error: Option<&SessionDiagnostic>,
) {
    dashboard.dashboard_state.action_items =
        dashboard_action_items_with_problem_count(problem_count(
            dashboard.initialized,
            dashboard.context.home_dir(),
            &dashboard.run_problem_items,
            dashboard.plugin_inventory_diagnostic.as_ref(),
            dashboard.workflow_inventory_diagnostic.as_ref(),
            &dashboard.workflows,
            session_error,
        ));
    dashboard.dashboard_state.selected_action = dashboard.dashboard_state.selected_action.min(
        dashboard
            .dashboard_state
            .action_items
            .len()
            .saturating_sub(1),
    );
}

pub(super) fn dashboard_section_metrics(size: Size) -> (usize, usize, usize, usize) {
    let area = Rect::from((Position::ORIGIN, size));
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(36),
            Constraint::Percentage(39),
            Constraint::Percentage(25),
        ])
        .split(layout[1]);
    (
        body[0].width.saturating_sub(2) as usize,
        body[0].height.saturating_sub(2) as usize,
        body[1].width.saturating_sub(2) as usize,
        body[1].height.saturating_sub(2) as usize,
    )
}

pub(super) fn dashboard_move_focus(focus: DashboardSection, forward: bool) -> DashboardSection {
    match (focus, forward) {
        (DashboardSection::Setup, true) => DashboardSection::Inventory,
        (DashboardSection::Inventory, true) => DashboardSection::Actions,
        (DashboardSection::Actions, true) => DashboardSection::Setup,
        (DashboardSection::Setup, false) => DashboardSection::Actions,
        (DashboardSection::Inventory, false) => DashboardSection::Setup,
        (DashboardSection::Actions, false) => DashboardSection::Inventory,
    }
}

pub(super) enum DashboardCommand {
    Quit,
    OpenProblems,
    Status(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePostureView {
    mode: transist::RuntimeOperatingMode,
    policy_profile: String,
}

#[derive(Debug, Deserialize)]
struct RuntimePostureConfig {
    runtime: RuntimePostureRuntimeConfig,
    policy: RuntimePosturePolicyConfig,
}

#[derive(Debug, Deserialize)]
struct RuntimePostureRuntimeConfig {
    mode: Option<transist::RuntimeOperatingMode>,
}

#[derive(Debug, Deserialize)]
struct RuntimePosturePolicyConfig {
    profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreshExecutionMode {
    Standard,
    Trusted,
    TrustedRequired,
}

impl FreshExecutionMode {
    fn trusted_execution(self) -> bool {
        !matches!(self, Self::Standard)
    }

    fn status_label(self) -> &'static str {
        match self {
            Self::Standard => "standard execution",
            Self::Trusted => "trusted execution",
            Self::TrustedRequired => "trusted execution (required)",
        }
    }
}

fn load_runtime_posture_view(home_dir: &Path) -> Result<RuntimePostureView> {
    transist::validate_runtime_configuration(home_dir)?;
    let effective = transist::effective_runtime_configuration_yaml(home_dir)?;
    let config: RuntimePostureConfig = yaml_serde::from_str(&effective)?;
    let mode = config
        .runtime
        .mode
        .ok_or_else(|| anyhow::anyhow!("missing 'runtime.mode' in config.yaml"))?;
    let policy_profile = config
        .policy
        .profile
        .filter(|profile| !profile.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing or empty 'policy.profile' in config.yaml"))?;
    Ok(RuntimePostureView {
        mode,
        policy_profile,
    })
}

fn resolve_fresh_execution_mode(
    prompt: &mut FullscreenPrompt,
    home_dir: &Path,
    selected: Option<FreshExecutionMode>,
) -> Result<StepOutcome<FreshExecutionMode>> {
    let posture = load_runtime_posture_view(home_dir)?;
    if posture.mode == transist::RuntimeOperatingMode::Governed {
        return Ok(StepOutcome::Ok(FreshExecutionMode::TrustedRequired));
    }
    let outcome = prompt.run_select_modal_with_back(
        "Select execution mode",
        fresh_execution_mode_picker_items(),
        selected.map(fresh_execution_mode_picker_index).unwrap_or(0),
    )?;
    Ok(match outcome {
        StepOutcome::Ok(index) => StepOutcome::Ok(fresh_execution_mode_from_picker_index(index)),
        StepOutcome::Back => StepOutcome::Back,
        StepOutcome::Cancel => StepOutcome::Cancel,
    })
}

fn launch_step_before_run_ref(execution_mode: FreshExecutionMode) -> LaunchStep {
    if execution_mode == FreshExecutionMode::TrustedRequired {
        LaunchStep::PickWorkflow
    } else {
        LaunchStep::ExecutionMode
    }
}

fn fresh_execution_mode_picker_items() -> Vec<String> {
    vec![
        "Standard execution - no signature verification".to_string(),
        "Trusted execution - verify workflow and plugin signatures".to_string(),
    ]
}

fn fresh_execution_mode_from_picker_index(index: usize) -> FreshExecutionMode {
    match index {
        0 => FreshExecutionMode::Standard,
        1 => FreshExecutionMode::Trusted,
        _ => unreachable!("execution mode picker returned an invalid selection"),
    }
}

fn fresh_execution_mode_picker_index(mode: FreshExecutionMode) -> usize {
    match mode {
        FreshExecutionMode::Standard => 0,
        FreshExecutionMode::Trusted | FreshExecutionMode::TrustedRequired => 1,
    }
}

fn runner_launch_withhold_reason(
    run_id: &str,
    status: &str,
    lease_release_warning: Option<&str>,
) -> Option<String> {
    if status == "failed" {
        return Some(format!(
            "Run {run_id} failed during preparation; no runner was started. Inspect the run details and resume it if appropriate."
        ));
    }
    lease_release_warning.map(|warning| {
        format!(
            "Run {run_id} prepared successfully but automatic execution was withheld because the mutation lease could not be released: {}; inspect or repair the lease, then run `transist runs start {run_id}`.",
            warning
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_dashboard_action(
    prompt: &mut FullscreenPrompt,
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    interrupt_flag: &Arc<AtomicBool>,
    dashboard: &DashboardData,
    action: DashboardActionId,
    session_error: &mut Option<SessionDiagnostic>,
) -> Result<DashboardCommand> {
    match action {
        DashboardActionId::LaunchWorkflow => {
            if !dashboard.initialized {
                return Ok(DashboardCommand::Status(
                    "Initialize the effective home with `transist home init`, then install a runnable workflow: use `transist-admin workflow package` followed by `transist workflows install --package <package-directory>` for governed homes, or `transist workflows install --path <workflow.yaml>` for standalone and development homes."
                        .to_string(),
                ));
            }
            if launchable_workflow_refs(&dashboard.workflows).is_empty() {
                return Ok(DashboardCommand::Status(
                    "No runnable workflows are installed. For governed homes, create a package with `transist-admin workflow package` and install it with `transist workflows install --package <package-directory>`. For standalone and development homes, use `transist workflows install --path <workflow.yaml>`, then refresh the dashboard."
                        .to_string(),
                ));
            }

            // Multi-step launch flow with Back navigation. Each step preserves
            // its previously entered value so a user pressing Esc rewinds one
            // step instead of losing data; Ctrl+C aborts the whole flow at any
            // step. Step 1 (workflow pick) treats Esc as cancel — there is
            // nothing to go back to — but if a later step has populated the
            // flow's state, we ask for confirmation before discarding.
            //
            // The state machine is intentionally local: the launch flow is
            // self-contained, a `LaunchFlowState` struct here is simpler than
            // promoting it out, and the five-variant `LaunchStep` enum makes
            // the back/forward transitions easy to read.
            let outcome = run_launch_workflow_flow(prompt, dashboard, cwd)?;
            let LaunchOutcome {
                workflow_ref,
                run_ref,
                workflow_inputs,
                execution_mode,
            } = match outcome {
                LaunchFlowResult::Submitted(payload) => payload,
                LaunchFlowResult::Canceled(message) => {
                    return Ok(DashboardCommand::Status(message));
                }
            };

            // Materialize the run on disk via the prepare phase, then hand off
            // to a detached runner. After the spawn, auto-attach the run-detail
            // viewer so the operator sees live progress in the same terminal
            // (consistent with the `transist runs start` / `transist workflows run` CLI
            // commands). Pressing `q` in the viewer returns to the dashboard.
            let prepare_context = dashboard.context.clone();
            let prepare_workflow_ref = workflow_ref.clone();
            let prepare_report = prompt
                .with_spinner("Preparing run", move || {
                    prepare_context.prepare_workflow_with_trust(
                        &prepare_workflow_ref,
                        run_ref.as_deref(),
                        workflow_inputs,
                        execution_mode.trusted_execution(),
                        false,
                    )
                })
                .with_context(|| format!("Failed to prepare workflow {}", workflow_ref))?;
            let tracked_run_id = prepare_report.run_id.clone();
            if let Some(message) = runner_launch_withhold_reason(
                &tracked_run_id,
                &prepare_report.status,
                prepare_report
                    .lease_release_warning
                    .as_ref()
                    .map(|warning| warning.message.as_str()),
            ) {
                return Ok(DashboardCommand::Status(message));
            }

            prompt.loading_state = Some(LoadingState {
                message: "Starting run".to_string(),
            });
            prompt.draw()?;
            prompt.dashboard_state = None;

            let spawn_result = crate::runner::spawn_and_attach(
                &dashboard.context,
                prompt,
                &crate::runner::RunnerAction::Run {
                    run_id: tracked_run_id.clone(),
                    trusted_execution: execution_mode.trusted_execution(),
                    local_only: false,
                },
                crate::cli::CliUiMode::Tui,
            );
            prompt.loading_state = None;
            let pid = spawn_result.with_context(|| {
                format!("Failed to spawn detached runner for {}", tracked_run_id)
            })?;

            Ok(DashboardCommand::Status(format!(
                "Started workflow {} as run {} ({}, runner pid {}).",
                workflow_ref,
                tracked_run_id,
                execution_mode.status_label(),
                pid
            )))
        }
        DashboardActionId::ShowRunsList => {
            if !dashboard.initialized {
                return Ok(DashboardCommand::Status(
                    "Initialize the effective home with `transist home init` before browsing runs."
                        .to_string(),
                ));
            }
            screens::run_runs_browser_screen(prompt, &dashboard.context, interrupt_flag)?;
            Ok(DashboardCommand::Status("Closed runs browser.".to_string()))
        }
        DashboardActionId::Problems => Ok(DashboardCommand::OpenProblems),
        DashboardActionId::Refresh => {
            Ok(DashboardCommand::Status("Dashboard refreshed.".to_string()))
        }
        DashboardActionId::Preferences => {
            screens::run_configuration_screen(
                prompt,
                repo_root,
                cwd,
                explicit_home,
                session_error,
            )?;
            Ok(DashboardCommand::Status("Closed Preferences.".to_string()))
        }
        DashboardActionId::Quit => Ok(DashboardCommand::Quit),
        // The pager/editor preferences are surfaced only through the
        // Preferences screen, not as top-level
        // dashboard actions. If they ever leak into the dashboard action
        // list, fall through to a no-op status rather than panicking.
        DashboardActionId::SetPreferredPager
        | DashboardActionId::SetGateActor
        | DashboardActionId::SetPreferredEditor
        | DashboardActionId::SetInspectionCacheRuns => Ok(DashboardCommand::Status(
            "Open Preferences to manage local user settings.".to_string(),
        )),
    }
}

pub(super) fn execute_runtime_configuration_action(
    prompt: &mut FullscreenPrompt,
    _repo_root: &Path,
    _cwd: &Path,
    _explicit_home: Option<&Path>,
    action: DashboardActionId,
) -> Result<String> {
    match action {
        DashboardActionId::SetPreferredPager => {
            let initial = transist::load_user_preferred_pager()?.unwrap_or_default();
            let Some(raw) = prompt.run_text_input_modal_with_placeholder(
                "Set preferred pager",
                "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
                &initial,
                Some("less -R"),
                true,
            )?
            else {
                return Ok("Preferred pager update canceled.".to_string());
            };
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                transist::clear_user_preferred_pager()?;
                return Ok("Cleared preferred pager (will fall back to $PAGER).".to_string());
            }
            transist::save_user_preferred_pager(trimmed)?;
            Ok(format!("Saved preferred pager: {}", trimmed))
        }
        DashboardActionId::SetGateActor => {
            let initial = transist::load_user_gate_actor()?.unwrap_or_default();
            let Some(raw) = prompt.run_text_input_modal_with_placeholder(
                "Set gate actor",
                "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
                &initial,
                Some("Reviewer <reviewer@example.com>"),
                true,
            )?
            else {
                return Ok("Gate actor update canceled.".to_string());
            };
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                transist::clear_user_gate_actor()?;
                return Ok("Cleared gate actor (TUI will prompt when needed).".to_string());
            }
            transist::save_user_gate_actor(trimmed)?;
            Ok(format!("Saved gate actor: {}", trimmed))
        }
        DashboardActionId::SetPreferredEditor => {
            let initial = transist::load_user_preferred_editor()?.unwrap_or_default();
            let Some(raw) = prompt.run_text_input_modal_with_placeholder(
                "Set preferred editor",
                "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
                &initial,
                Some("nvim"),
                true,
            )?
            else {
                return Ok("Preferred editor update canceled.".to_string());
            };
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                transist::clear_user_preferred_editor()?;
                return Ok("Cleared preferred editor.".to_string());
            }
            transist::save_user_preferred_editor(trimmed)?;
            Ok(format!("Saved preferred editor: {}", trimmed))
        }
        DashboardActionId::SetInspectionCacheRuns => {
            let initial = transist::load_user_inspection_cache_runs()?
                .map(|value| value.to_string())
                .unwrap_or_default();
            let Some(raw) = prompt.run_text_input_modal_with_placeholder(
                "Set inspection cache size (runs)",
                "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
                &initial,
                Some(&transist::DEFAULT_INSPECTION_CACHE_RUNS.to_string()),
                true,
            )?
            else {
                return Ok("Inspection cache size update canceled.".to_string());
            };
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                transist::clear_user_inspection_cache_runs()?;
                return Ok(format!(
                    "Cleared inspection cache size (default = {}).",
                    transist::DEFAULT_INSPECTION_CACHE_RUNS
                ));
            }
            let value = trimmed.parse::<usize>().with_context(|| {
                format!(
                    "invalid inspection cache size '{}': expected a non-negative integer",
                    trimmed
                )
            })?;
            transist::save_user_inspection_cache_runs(value)?;
            Ok(format!("Saved inspection cache size: {} runs.", value))
        }
        _ => anyhow::bail!("unsupported runtime configuration action"),
    }
}

/// Builds a dashboard state for when home resolution fails with [`transist::HomeResolutionError`].
///
/// Uses a dummy `ResolvedHome` and `TransistContext` only to render a read-only
/// fallback. It does not imply that `<cwd>/.transist` is an initialization target.
fn build_no_home_dashboard(
    repo_root: &Path,
    cwd: &Path,
    status_message: Option<&UiStatusMessage>,
) -> Result<DashboardData> {
    let settings_path = transist::user_settings_path()?;

    let mut setup_lines: Vec<String> = Vec::new();
    setup_lines.push("Current home".to_string());
    let error_start = setup_lines.len();
    setup_lines.push(format!("  No Transist home resolved in {}.", cwd.display()));
    setup_lines
        .push("  Run `transist home init` to initialize the per-user default home, or".to_string());
    setup_lines
        .push("  `transist --home <path> home init` to initialize an explicit home.".to_string());
    let error_end = setup_lines.len();

    setup_lines.push(String::new());
    setup_lines.push("Settings".to_string());
    setup_lines.push(format!(
        "  global settings file: {}",
        settings_path.display()
    ));

    if crate::inspection_sync_available() {
        setup_lines.push(String::new());
        setup_lines.push("Inspection".to_string());
        setup_lines.push("  inspection sync: unavailable (no Transist home resolved)".to_string());
    }

    setup_lines.push(String::new());
    setup_lines.push("Next".to_string());
    setup_lines
        .push("  1. Run `transist home init` to create the per-user default home.".to_string());
    setup_lines
        .push("  2. Or run `transist --home <path> home init` for an explicit home.".to_string());

    // Override the error range with a status-message error if one is present.
    let mut error_line_range: Option<std::ops::Range<usize>> = Some(error_start..error_end);
    if let Some(msg) = status_message.filter(|m| m.lines.iter().any(|l| !l.trim().is_empty())) {
        setup_lines.push(String::new());
        setup_lines.push("Status".to_string());
        let status_start = setup_lines.len();
        setup_lines.extend(
            msg.lines
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| format!("  {}", l.trim_end())),
        );
        if matches!(msg.tone, UiStatusTone::Error) {
            error_line_range = Some(status_start..setup_lines.len());
        }
    }

    let dummy_home = cwd.join(".transist");
    let dummy_context = transist::TransistContext::with_home(repo_root, dummy_home.clone());

    let dashboard_state = DashboardState {
        header_lines: vec![
            "Home: (none)".to_string(),
            "Source: unresolved".to_string(),
            "Initialized: no".to_string(),
            "Running: 0   Blocked: 0".to_string(),
        ],
        setup_lines,
        setup_error_line_range: error_line_range,
        inventory_lines: build_dashboard_inventory_lines(0, &[], None, &[], None, false),
        action_items: dashboard_action_items_with_problem_count(problem_count(
            false,
            &dummy_home,
            &[],
            None,
            None,
            &[],
            None,
        )),
        selected_action: 0,
        focused_section: DashboardSection::Actions,
        setup_scroll: 0,
        inventory_scroll: 0,
    };

    Ok(DashboardData {
        context: dummy_context,
        initialized: false,
        run_problem_items: Vec::new(),
        plugin_inventory_diagnostic: None,
        workflow_inventory_diagnostic: None,
        workflows: Vec::new(),
        dashboard_state,
    })
}

/// Keeps the fullscreen console available when a dashboard reload cannot
/// resolve normal runtime data. The full error remains in the session Problems
/// view; this fallback intentionally exposes only non-mutating navigation.
pub(super) fn build_dashboard_error(
    repo_root: &Path,
    cwd: &Path,
    status_message: Option<&UiStatusMessage>,
) -> Result<DashboardData> {
    build_no_home_dashboard(repo_root, cwd, status_message)
}

pub(super) fn build_root_dashboard(
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    status_message: Option<&UiStatusMessage>,
) -> Result<DashboardData> {
    let resolved = match transist::resolve_settings_payload(repo_root, cwd, explicit_home) {
        Ok(r) => r,
        Err(e) if e.downcast_ref::<transist::HomeResolutionError>().is_some() => {
            return build_no_home_dashboard(repo_root, cwd, status_message);
        }
        Err(e) => return Err(e),
    };
    let context = transist::TransistContext::with_home(repo_root, resolved.effective_home.clone());
    let initialized = transist_home_is_initialized(context.home_dir());
    let root_run_summaries = if initialized {
        load_root_run_summaries(&context)?
    } else {
        Vec::new()
    };
    let run_problem_items = collect_run_problems(&context, &root_run_summaries);
    let (plugins, plugin_inventory_diagnostic) = if initialized {
        match context.list_plugins() {
            Ok(plugins) => (plugins, None),
            Err(error) => (
                Vec::new(),
                Some(DiagnosticReport::from_error(
                    DiagnosticSeverity::Error,
                    Some("tui.plugin_inventory_failed".to_string()),
                    "plugin inventory",
                    context.home_dir().display().to_string(),
                    "Unable to load installed plugin inventory.",
                    Some(
                        "The dashboard remains available with the plugin inventory omitted."
                            .to_string(),
                    ),
                    &error,
                )),
            ),
        }
    } else {
        (Vec::new(), None)
    };
    let (workflows, workflow_inventory_diagnostic) = if initialized {
        let inspection = TransistInspectionContext::with_home(
            context.repo_root().to_path_buf(),
            context.home_dir().to_path_buf(),
        );
        match inspection.list_workflows(&transist::PaginationQuery::default()) {
            Ok(workflows) => (workflows.data, None),
            Err(error) => (
                Vec::new(),
                Some(DiagnosticReport::from_error(
                    DiagnosticSeverity::Error,
                    Some("tui.workflow_inventory_failed".to_string()),
                    "workflow inventory",
                    context.home_dir().display().to_string(),
                    "Unable to load installed workflow inventory.",
                    Some(
                        "The dashboard remains available with the workflow inventory omitted."
                            .to_string(),
                    ),
                    &error,
                )),
            ),
        }
    } else {
        (Vec::new(), None)
    };
    let settings_path = transist::user_settings_path()?;
    let inspection_available = crate::inspection_sync_available();
    let inspection_summary = if inspection_available {
        load_inspection_sync_summary(context.repo_root(), context.home_dir(), &resolved)
    } else {
        InspectionSyncSummary::unavailable()
    };
    let running_count = root_run_summaries
        .iter()
        .filter(|summary| summary.status == "running")
        .count();
    let blocked_count = root_run_summaries
        .iter()
        .filter(|summary| summary.status == "blocked")
        .count();
    let setup = build_dashboard_setup_lines(
        &resolved,
        &settings_path,
        initialized,
        SetupView::Dashboard,
        &inspection_summary,
        inspection_available,
        status_message,
    );
    let dashboard_state = DashboardState {
        header_lines: vec![
            format!("Home: {}", resolved.effective_home.display()),
            format!("Source: {}", home_source_label(resolved.source)),
            format!("Initialized: {}", if initialized { "yes" } else { "no" }),
            format!("Running: {}   Blocked: {}", running_count, blocked_count),
        ],
        setup_lines: setup.lines,
        setup_error_line_range: setup.error_line_range,
        inventory_lines: build_dashboard_inventory_lines(
            root_run_summaries.len(),
            &plugins,
            plugin_inventory_diagnostic.as_ref(),
            &workflows,
            workflow_inventory_diagnostic.as_ref(),
            initialized,
        ),
        action_items: dashboard_action_items_with_problem_count(problem_count(
            initialized,
            context.home_dir(),
            &run_problem_items,
            plugin_inventory_diagnostic.as_ref(),
            workflow_inventory_diagnostic.as_ref(),
            &workflows,
            None,
        )),
        selected_action: 0,
        focused_section: DashboardSection::Actions,
        setup_scroll: 0,
        inventory_scroll: 0,
    };
    Ok(DashboardData {
        context,
        initialized,
        run_problem_items,
        plugin_inventory_diagnostic,
        workflow_inventory_diagnostic,
        workflows,
        dashboard_state,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SetupView {
    Dashboard,
    Configuration,
}

pub(super) fn build_dashboard_setup_lines(
    resolved: &transist::ResolvedHome,
    settings_path: &Path,
    initialized: bool,
    view: SetupView,
    inspection_summary: &InspectionSyncSummary,
    inspection_available: bool,
    status_message: Option<&UiStatusMessage>,
) -> UiSectionLines {
    let mut lines = Vec::new();

    // --- Current home ---
    lines.push("Current home".to_string());
    lines.push(format!("  transist version: {}", env!("CARGO_PKG_VERSION")));
    lines.push(format!("  path: {}", resolved.effective_home.display()));
    lines.push(format!("  source: {}", home_source_label(resolved.source)));
    lines.push(format!(
        "  initialized: {}",
        if initialized { "yes" } else { "no" }
    ));

    // user-set home row — always shown in Configuration view; on Dashboard shown whenever a
    // persisted value exists so the user can see "I set X, but local won"
    if view == SetupView::Configuration || resolved.persisted_user_home.is_some() {
        lines.push(format!(
            "  user-set home: {}",
            resolved
                .persisted_user_home
                .as_ref()
                .map(|path: &std::path::PathBuf| path.display().to_string())
                .unwrap_or_else(|| "(none)".to_string())
        ));
    }

    // --- Settings ---
    lines.push(String::new());
    lines.push("Settings".to_string());
    lines.push(format!(
        "  global settings file: {}",
        settings_path.display()
    ));
    lines.push(format!(
        "  runtime config file: {}",
        resolved.effective_home.join("config.yaml").display()
    ));
    match load_runtime_posture_view(&resolved.effective_home) {
        Ok(posture) => {
            lines.push(format!("  runtime mode: {}", posture.mode.as_str()));
            lines.push(format!("  policy profile: {}", posture.policy_profile));
        }
        Err(_error) if !resolved.effective_home.join("config.yaml").is_file() => {
            lines.push("  runtime mode: (not initialized)".to_string());
            lines.push("  policy profile: (not initialized)".to_string());
        }
        Err(error) => {
            lines.push(format!("  runtime mode: unavailable ({error})"));
            lines.push("  policy profile: unavailable (invalid runtime configuration)".to_string());
        }
    }
    lines.push(format!(
        "  runtime settings file: {}",
        resolved.effective_home.join("settings.yaml").display()
    ));
    lines.push(format!(
        "  runtime filesystem policy: {}",
        describe_runtime_filesystem_policy(&resolved.effective_home)
    ));
    // preferred pager/editor — dashboard hides when unset
    let gate_actor = transist::load_user_gate_actor().ok().flatten();
    let pager = transist::load_user_preferred_pager().ok().flatten();
    let editor = transist::load_user_preferred_editor().ok().flatten();
    if view == SetupView::Configuration || gate_actor.is_some() {
        lines.push(format!(
            "  gate actor: {}",
            gate_actor.unwrap_or_else(|| "(none — TUI will prompt when needed)".to_string())
        ));
    }
    if view == SetupView::Configuration || pager.is_some() {
        lines.push(format!(
            "  preferred pager: {}",
            pager.unwrap_or_else(|| {
                let env = std::env::var("PAGER").unwrap_or_default();
                if env.trim().is_empty() {
                    "(none — internal pager)".to_string()
                } else {
                    format!("(none — falls back to $PAGER: {})", env.trim())
                }
            })
        ));
    }
    if view == SetupView::Configuration || editor.is_some() {
        lines.push(format!(
            "  preferred editor: {}",
            editor.unwrap_or_else(|| "(none — `e` disabled)".to_string())
        ));
    }
    // stale-lock cleanup — dashboard collapses at defaults
    lines.push(format!(
        "  stale-lock cleanup: {}",
        describe_reaper_config(view)
    ));

    if view == SetupView::Configuration {
        render_imported_trust_roots(&mut lines, &resolved.effective_home);
    }

    if inspection_available {
        // --- Inspection ---
        lines.push(String::new());
        lines.push("Inspection".to_string());
        lines.push(format!(
            "  inspection cache (runs): {}",
            transist::load_user_inspection_cache_runs()
                .ok()
                .flatten()
                .map(|value| value.to_string())
                .unwrap_or_else(|| format!(
                    "default = {}",
                    transist::DEFAULT_INSPECTION_CACHE_RUNS
                ))
        ));
        render_inspection_sync_block(&mut lines, inspection_summary, view);
    }

    // --- Next guidance (uninitialized only) ---
    if !initialized {
        lines.push(String::new());
        lines.push("Next".to_string());
        lines.push("  1. Initialize the effective home".to_string());
        lines.push("  2. Optionally set a user home (`transist home set <path>`)".to_string());
        lines.push("  3. Install a plugin such as plugins/shell".to_string());
    }

    // --- Status section ---
    let mut error_line_range = None;
    if let Some(status_message) =
        status_message.filter(|message| message.lines.iter().any(|line| !line.trim().is_empty()))
    {
        lines.push(String::new());
        lines.push("Status".to_string());
        let status_start = lines.len();
        lines.extend(
            status_message
                .lines
                .iter()
                .filter(|line| !line.trim().is_empty())
                .map(|line| format!("  {}", line.trim_end())),
        );
        if matches!(status_message.tone, UiStatusTone::Error) {
            error_line_range = Some(status_start..lines.len());
        }
    }
    UiSectionLines {
        lines,
        error_line_range,
    }
}

/// Renders imported trust-root metadata and public keys for the configuration view.
fn render_imported_trust_roots(lines: &mut Vec<String>, home_dir: &Path) {
    lines.push(String::new());
    lines.push("Trust".to_string());
    match transist::list_trust_roots(home_dir) {
        Ok(roots) if roots.is_empty() => {
            lines.push("  imported trust root (public key): (none)".to_string());
        }
        Ok(roots) => {
            for root in roots {
                match transist::inspect_trust_root(home_dir, &root.key_id) {
                    Ok(record) => {
                        lines.push(format!("  imported trust root: {}", record.name));
                        lines.push(format!("    key id: {}", record.key_id));
                        lines.push(format!(
                            "    path: {}",
                            transist::trust_store_dir(home_dir)
                                .join(format!("{}.pub", record.key_id))
                                .display()
                        ));
                    }
                    Err(error) => lines.push(format!(
                        "  imported trust root: {} ({}) unavailable ({error})",
                        root.name, root.key_id
                    )),
                }
            }
        }
        Err(error) => lines.push(format!(
            "  imported trust root (public key): unavailable ({error})"
        )),
    }
}

fn render_inspection_sync_block(
    lines: &mut Vec<String>,
    summary: &InspectionSyncSummary,
    view: SetupView,
) {
    if summary.configured {
        lines.push("  inspection sync: configured".to_string());
        if view == SetupView::Configuration {
            if let Some(ref sid) = summary.source_id {
                lines.push(format!("    source id: {}", sid));
            }
            if let Some(ref url) = summary.registry_url {
                lines.push(format!("    registry: {}", url));
            }
            lines.push(format!("    daemon: {}", summary.daemon_status));
            lines.push(format!("    queue depth: {}", summary.queue_depth));
        }
    } else {
        lines.push("  inspection sync: not configured".to_string());
    }
}

fn describe_runtime_filesystem_policy(home_dir: &Path) -> String {
    match transist::runtime_filesystem_policy(home_dir) {
        Ok(Some(policy)) => {
            let spec = format_filesystem_policy(&policy);
            format!("{} (from runtime settings.yaml)", spec)
        }
        Ok(None) => {
            let spec = format_filesystem_policy(&transist::FilesystemPolicy::default());
            format!("{} (builtin default)", spec)
        }
        Err(error) => format!("invalid ({})", error),
    }
}

fn format_filesystem_policy(policy: &transist::FilesystemPolicy) -> String {
    let mode = match policy.workspace_access.as_ref().map(|wa| wa.mode) {
        Some(transist::WorkspaceAccessMode::Full) => "mode=full".to_string(),
        Some(transist::WorkspaceAccessMode::ReadOnly) => "mode=read_only".to_string(),
        Some(transist::WorkspaceAccessMode::Custom) => "mode=custom".to_string(),
        None => "mode=full (builtin)".to_string(),
    };
    let mut result = mode;
    if !policy.read_allow_paths.is_empty() {
        result.push_str(&format!(", read=[{}]", policy.read_allow_paths.join(", ")));
    }
    if !policy.write_allow_paths.is_empty() {
        result.push_str(&format!(
            ", write=[{}]",
            policy.write_allow_paths.join(", ")
        ));
    }
    result
}

/// Renders the resolved orphan-lease reaper configuration into a single line
/// suitable for the configure-screen "Setup" pane. The reaper is configured
/// purely via environment variables, so this function reflects what
/// [`transist::reaper_settings_from_env`] returned at startup.
///
/// Dashboard view uses the compact form at defaults; Configuration view always
/// emits the full detail.
fn describe_reaper_config(view: SetupView) -> String {
    let cfg = transist::reaper_settings_from_env();
    let at_defaults = cfg.stale_after_ms == transist::REAPER_DEFAULT_STALE_AFTER_MS;
    if cfg.disabled {
        format!("disabled (env: {})", transist::REAPER_DISABLE_ENV)
    } else if view == SetupView::Configuration || !at_defaults {
        format!(
            "enabled (lease heartbeat {}s, stale timeout {}s)",
            transist::REAPER_DEFAULT_HEARTBEAT_INTERVAL_MS / 1000,
            cfg.stale_after_ms / 1000,
        )
    } else {
        "enabled".to_string()
    }
}

fn build_dashboard_inventory_lines(
    root_run_count: usize,
    plugins: &[transist::InstalledPlugin],
    plugin_inventory_diagnostic: Option<&DiagnosticReport>,
    workflows: &[transist::InspectionWorkflowSummary],
    workflow_inventory_diagnostic: Option<&DiagnosticReport>,
    initialized: bool,
) -> Vec<String> {
    let compiled_workflows = workflows
        .iter()
        .filter(|workflow| workflow.compiled_recipe_available)
        .collect::<Vec<_>>();
    let mut lines = vec!["Inventory".to_string()];
    if !initialized {
        lines.push("  initialize the home to discover runs, plugins, and workflows".to_string());
        return lines;
    }
    lines.push(format!("  root runs: {}", root_run_count));
    lines.push(String::new());
    if plugin_inventory_diagnostic.is_some() {
        lines.push("  plugins: unavailable (see diagnostic)".to_string());
    } else {
        lines.push(format!("  plugins: {}", plugins.len()));
        if plugins.is_empty() {
            lines.push("    - (none installed)".to_string());
        }
        for plugin in plugins {
            lines.push(format!("    - {}@{}", plugin.id, plugin.version));
        }
    }
    lines.push(String::new());
    if workflow_inventory_diagnostic.is_some() {
        lines.push("  workflows: unavailable (see diagnostic)".to_string());
    } else {
        lines.push(format!(
            "  compiled workflows in home: {}",
            compiled_workflows.len()
        ));
        if compiled_workflows.is_empty() {
            lines.push("    - (none compiled yet)".to_string());
        }
        for workflow in compiled_workflows {
            lines.push(format!(
                "    - {}@{}",
                workflow.workflow_id, workflow.workflow_version,
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{
        FreshExecutionMode, InspectionSyncSummary, SetupView, build_dashboard_inventory_lines,
        build_dashboard_setup_lines, build_no_home_dashboard, build_root_dashboard,
        fresh_execution_mode_from_picker_index, fresh_execution_mode_picker_items,
        launch_readiness_action, launch_step_before_run_ref, load_runtime_posture_view,
        runner_launch_withhold_reason,
    };

    fn readiness_summary(
        status: transist::WorkflowReadinessStatus,
    ) -> transist::WorkflowCheckSummary {
        transist::WorkflowCheckSummary {
            mode: transist::WorkflowCheckMode::Readiness,
            evaluation_mode: transist::WorkflowEvaluationMode::LiveReadiness,
            validation_status: if status == transist::WorkflowReadinessStatus::Ready {
                transist::ValidationStatus::Passed
            } else {
                transist::ValidationStatus::Failed
            },
            workflow_ref: "demo@v1".into(),
            workspace_dir: None,
            status: if status == transist::WorkflowReadinessStatus::Ready {
                transist::ValidationStatus::Passed
            } else {
                transist::ValidationStatus::Failed
            },
            readiness_status: Some(status),
            passed_checks: 4,
            findings: Vec::new(),
            required_input_names: Vec::new(),
            missing_capability_count: 0,
            policy_suggestion: None,
            authorization_eligible: false,
        }
    }

    #[test]
    fn inventory_counts_root_runs_only() {
        let plugins = Vec::new();
        let workflows = Vec::new();

        let lines = build_dashboard_inventory_lines(4, &plugins, None, &workflows, None, true);

        assert!(lines.iter().any(|line| line == "  root runs: 4"));
        assert!(!lines.iter().any(|line| line.starts_with("  runs:")));
    }

    #[test]
    fn plugin_inventory_renders_each_id_and_version() {
        let plugins = vec![
            transist::InstalledPlugin {
                id: "shell".to_string(),
                version: "0.1.0".to_string(),
                dir: std::path::PathBuf::from("/plugins/shell/0.1.0"),
            },
            transist::InstalledPlugin {
                id: "notes".to_string(),
                // Deliberately distinct to catch accidental reuse of shell's version.
                version: "2.0.0".to_string(),
                dir: std::path::PathBuf::from("/plugins/notes/2.0.0"),
            },
        ];

        let lines = build_dashboard_inventory_lines(0, &plugins, None, &[], None, true);

        assert!(lines.iter().any(|line| line == "    - shell@0.1.0"));
        assert!(lines.iter().any(|line| line == "    - notes@2.0.0"));
    }

    #[test]
    fn plugin_inventory_load_failure_stays_in_dashboard_diagnostic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        transist::init_repo(tmp.path()).expect("init repo");
        let home = transist::repo_local_home_dir(tmp.path());
        let plugins_root = home.join("plugins");
        if plugins_root.is_dir() {
            std::fs::remove_dir_all(&plugins_root).expect("remove plugin directory");
        }
        std::fs::write(&plugins_root, "not a directory").expect("replace plugin directory");

        let dashboard = build_root_dashboard(tmp.path(), tmp.path(), Some(&home), None)
            .expect("dashboard remains available");
        let diagnostic = dashboard
            .plugin_inventory_diagnostic
            .expect("plugin inventory diagnostic");

        assert!(
            dashboard
                .dashboard_state
                .inventory_lines
                .iter()
                .any(|line| line == "  plugins: unavailable (see diagnostic)")
        );
        assert!(diagnostic.render().contains("failed reading plugin root"));
    }

    #[test]
    fn workflow_inventory_load_failure_stays_in_dashboard_diagnostic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        transist::init_repo(tmp.path()).expect("init repo");
        let home = transist::repo_local_home_dir(tmp.path());
        let recipes_root = home.join("registry/recipes");
        std::fs::remove_dir_all(&recipes_root).expect("remove recipe directory");
        std::fs::write(&recipes_root, "not a directory").expect("replace recipe directory");

        let dashboard = build_root_dashboard(tmp.path(), tmp.path(), Some(&home), None)
            .expect("dashboard remains available");
        let diagnostic = dashboard
            .workflow_inventory_diagnostic
            .expect("workflow inventory diagnostic");

        assert!(
            dashboard
                .dashboard_state
                .inventory_lines
                .iter()
                .any(|line| line == "  workflows: unavailable (see diagnostic)")
        );
        assert!(diagnostic.render().contains("failed reading directory"));
    }

    #[test]
    fn setup_lines_show_runtime_filesystem_policy_summary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        transist::init_repo(tmp.path()).expect("init repo");
        let effective_home = transist::repo_local_home_dir(tmp.path());
        std::fs::write(
            effective_home.join("settings.yaml"),
            "filesystem_policy:\n  workspace_access:\n    mode: read_only\n",
        )
        .expect("runtime settings");
        let resolved =
            transist::resolve_settings_payload(tmp.path(), tmp.path(), Some(&effective_home))
                .expect("resolve home");
        let settings_path = std::path::Path::new("/tmp/user-settings.yaml");
        let summary = InspectionSyncSummary {
            configured: false,
            source_id: None,
            registry_url: None,
            workspace_id: None,
            daemon_status: "stopped".to_string(),
            queue_depth: 0,
        };

        for view in [SetupView::Dashboard, SetupView::Configuration] {
            let setup = build_dashboard_setup_lines(
                &resolved,
                settings_path,
                true,
                view,
                &summary,
                false,
                None,
            );

            assert!(
                setup
                    .lines
                    .iter()
                    .any(|line| line.contains("runtime settings file:")),
                "view={view:?}: missing runtime settings file line"
            );
            assert!(
                setup.lines.iter().any(|line| {
                    line.contains("runtime filesystem policy:") && line.contains("read_only")
                }),
                "view={view:?}: lines: {:?}",
                setup.lines
            );
        }
    }

    fn empty_summary() -> InspectionSyncSummary {
        InspectionSyncSummary {
            configured: false,
            source_id: None,
            registry_url: None,
            workspace_id: None,
            daemon_status: "stopped".to_string(),
            queue_depth: 0,
        }
    }

    fn configured_summary() -> InspectionSyncSummary {
        InspectionSyncSummary {
            configured: true,
            source_id: Some("src-1".to_string()),
            registry_url: Some("https://example.com/registry".to_string()),
            workspace_id: Some("example/workflows".to_string()),
            daemon_status: "running".to_string(),
            queue_depth: 3,
        }
    }

    #[test]
    fn setup_lines_omit_inspection_heading_when_unavailable() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();

        let for_dashboard = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Dashboard,
            &summary,
            false,
            None,
        );
        let for_config = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Configuration,
            &summary,
            false,
            None,
        );

        for (label, setup) in [("dashboard", &for_dashboard), ("config", &for_config)] {
            let current_home_idx = setup.lines.iter().position(|l| l == "Current home");
            let settings_idx = setup.lines.iter().position(|l| l == "Settings");
            assert!(
                current_home_idx.is_some(),
                "{label}: missing 'Current home'"
            );
            assert!(settings_idx.is_some(), "{label}: missing 'Settings'");
            assert!(
                current_home_idx < settings_idx,
                "{label}: headings out of order"
            );
            assert!(
                !setup.lines.iter().any(|line| line == "Inspection"),
                "{label}: inspection must be hidden when unavailable"
            );
        }
    }

    #[test]
    fn setup_lines_do_not_start_with_redundant_setup_header() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();

        for view in [SetupView::Dashboard, SetupView::Configuration] {
            let setup = build_dashboard_setup_lines(
                &resolved,
                settings_path,
                true,
                view,
                &summary,
                false,
                None,
            );
            assert_ne!(
                setup.lines.first().map(|s| s.as_str()),
                Some("Setup"),
                "view={view:?}"
            );
        }
    }

    #[test]
    fn setup_lines_dashboard_hides_default_home_when_env_override_active() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/env/home"),
            source: transist::HomeSource::EnvOverride,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();
        let setup = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Dashboard,
            &summary,
            false,
            None,
        );
        assert!(
            !setup.lines.iter().any(|l| l.contains("user-set home")),
            "dashboard should omit user-set home row when env override is active and no persisted home; lines={:?}",
            setup.lines
        );
    }

    #[test]
    fn setup_lines_dashboard_shows_default_home_when_user_config_active() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/config/home/repos/repo-abc123"),
            source: transist::HomeSource::UserConfig,
            persisted_user_home: Some(std::path::PathBuf::from("/config/home")),
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();
        let setup = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Dashboard,
            &summary,
            false,
            None,
        );
        assert!(
            setup
                .lines
                .iter()
                .any(|l| l.contains("user-set home:") && l.contains("/config/home")),
            "dashboard should show user-set home row when UserConfig is the source"
        );
    }

    #[test]
    fn setup_lines_configuration_always_shows_default_home_row() {
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();

        // With Some value
        let resolved_some = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::EnvOverride,
            persisted_user_home: Some(std::path::PathBuf::from("/config/home")),
        };
        let setup_some = build_dashboard_setup_lines(
            &resolved_some,
            settings_path,
            true,
            SetupView::Configuration,
            &summary,
            false,
            None,
        );
        assert!(
            setup_some
                .lines
                .iter()
                .any(|l| l.contains("user-set home:")),
            "Configuration view should show user-set home row when persisted_user_home is Some"
        );

        // With None value
        let resolved_none = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::EnvOverride,
            persisted_user_home: None,
        };
        let setup_none = build_dashboard_setup_lines(
            &resolved_none,
            settings_path,
            true,
            SetupView::Configuration,
            &summary,
            false,
            None,
        );
        assert!(
            setup_none
                .lines
                .iter()
                .any(|l| l.contains("user-set home:")),
            "Configuration view should show user-set home row even when persisted_user_home is None"
        );
    }

    #[test]
    fn setup_lines_configuration_always_shows_preferences() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();

        // Configuration view always shows both preference lines regardless of whether
        // they are set. Dashboard view conditionally shows them only when non-empty;
        // that filter cannot be tested without controlling the user settings file.
        let cfg = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Configuration,
            &summary,
            false,
            None,
        );
        assert!(
            cfg.lines.iter().any(|l| l.contains("gate actor")),
            "Configuration view should always show gate actor"
        );
        assert!(
            cfg.lines.iter().any(|l| l.contains("preferred pager")),
            "Configuration view should always show preferred pager"
        );
        assert!(
            cfg.lines.iter().any(|l| l.contains("preferred editor")),
            "Configuration view should always show preferred editor"
        );
    }

    #[test]
    fn setup_lines_configuration_shows_imported_trust_root_public_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        transist::init_repo(tmp.path()).expect("init repo");
        let effective_home = transist::repo_local_home_dir(tmp.path());
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("admin_tooling")
            .join("test-org.pub");
        let imported =
            transist::import_trust_root_file(&effective_home, &fixture).expect("import trust root");
        let resolved = transist::ResolvedHome {
            effective_home,
            source: transist::HomeSource::CliOverride,
            persisted_user_home: None,
        };

        let setup = build_dashboard_setup_lines(
            &resolved,
            std::path::Path::new("/tmp/settings.yaml"),
            true,
            SetupView::Configuration,
            &empty_summary(),
            false,
            None,
        );

        assert!(
            setup
                .lines
                .iter()
                .any(|line| line == "  imported trust root: test-org")
        );
        let expected_path = format!(
            "    path: {}",
            transist::trust_store_dir(&resolved.effective_home)
                .join(format!("{}.pub", imported.key_id))
                .display()
        );
        assert!(setup.lines.iter().any(|line| line == &expected_path));
        assert!(!setup.lines.iter().any(|line| line.contains("public key:")));
    }

    #[test]
    fn setup_lines_dashboard_collapses_stale_lock_cleanup_at_defaults() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();
        let setup = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Dashboard,
            &summary,
            false,
            None,
        );
        assert!(
            setup
                .lines
                .iter()
                .any(|l| l.contains("stale-lock cleanup:") && l.contains("enabled")),
            "dashboard should show stale-lock cleanup line; lines={:?}",
            setup.lines
        );
        // Dashboard collapses to compact form at defaults — no detailed heartbeat info
        assert!(
            !setup.lines.iter().any(|l| l.contains("lease heartbeat")),
            "dashboard should omit heartbeat detail at default settings; lines={:?}",
            setup.lines
        );
    }

    #[test]
    fn setup_lines_hide_inspection_when_unavailable() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();
        let setup = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Dashboard,
            &summary,
            false,
            None,
        );
        assert!(
            !setup.lines.iter().any(|l| l == "Inspection"),
            "dashboard should omit the inspection section when unavailable"
        );
        assert!(
            !setup.lines.iter().any(|l| l.contains("source id")),
            "dashboard should hide inspection sync details when not configured"
        );
    }

    #[test]
    fn setup_lines_show_inspection_sync_when_available_and_configured() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = configured_summary();

        // Dashboard view shows configured line without details
        let dash = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Dashboard,
            &summary,
            true,
            None,
        );
        assert!(
            dash.lines
                .iter()
                .any(|l| l == "  inspection sync: configured"),
            "dashboard should show 'configured' line"
        );
        assert!(
            !dash.lines.iter().any(|l| l.contains("source id")),
            "dashboard should hide sync details"
        );

        // Configuration view shows all detail rows
        let cfg = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Configuration,
            &summary,
            true,
            None,
        );
        assert!(
            cfg.lines
                .iter()
                .any(|l| l == "  inspection sync: configured"),
            "config should show 'configured' line"
        );
        assert!(
            cfg.lines
                .iter()
                .any(|l| l.contains("source id") && l.contains("src-1")),
            "config should show source id"
        );
        assert!(
            cfg.lines
                .iter()
                .any(|l| l.contains("registry") && l.contains("example.com")),
            "config should show registry url"
        );
        assert!(
            cfg.lines
                .iter()
                .any(|l| l.contains("daemon") && l.contains("running")),
            "config should show daemon status"
        );
        assert!(
            cfg.lines
                .iter()
                .any(|l| l.contains("queue depth") && l.contains("3")),
            "config should show queue depth"
        );
    }

    #[test]
    fn setup_lines_dashboard_shows_next_guidance_when_uninitialized() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/tmp/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();
        let setup = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            false,
            SetupView::Dashboard,
            &summary,
            false,
            None,
        );
        assert!(
            setup
                .lines
                .iter()
                .any(|l| l.contains("Optionally set a user home")),
            "uninitialized dashboard should mention 'user home'; lines={:?}",
            setup.lines
        );
    }

    #[test]
    fn setup_lines_per_user_default_shows_per_user_default_source_label() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/home/user/.local/state/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: None,
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();
        let setup = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Dashboard,
            &summary,
            false,
            None,
        );
        assert!(
            setup.lines.iter().any(|l| l.contains("per-user default")),
            "PerUserDefault source should show 'per-user default' label; lines={:?}",
            setup.lines
        );
    }

    #[test]
    fn setup_lines_per_user_default_wins_but_user_set_home_still_shown_in_configuration_view() {
        let resolved = transist::ResolvedHome {
            effective_home: std::path::PathBuf::from("/home/user/.local/state/transist"),
            source: transist::HomeSource::PerUserDefault,
            persisted_user_home: Some(std::path::PathBuf::from("/shared/transist")),
        };
        let settings_path = std::path::Path::new("/tmp/settings.yaml");
        let summary = empty_summary();
        let setup = build_dashboard_setup_lines(
            &resolved,
            settings_path,
            true,
            SetupView::Configuration,
            &summary,
            false,
            None,
        );
        assert!(
            setup
                .lines
                .iter()
                .any(|l| l.contains("user-set home:") && l.contains("/shared/transist")),
            "Configuration view should reveal persisted user home even when the per-user default won; lines={:?}",
            setup.lines
        );
    }

    #[test]
    fn build_no_home_dashboard_setup_lines_contain_error_message() {
        let cwd = std::path::Path::new("/my/project");
        let data = build_no_home_dashboard(cwd, cwd, None).expect("no_home_dashboard");
        let lines = &data.dashboard_state.setup_lines;
        assert!(
            lines.iter().any(|l| l.contains("No Transist home")),
            "no-home setup lines should mention 'No Transist home'; lines={:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| l.contains("transist home init")),
            "no-home setup lines should suggest 'transist home init'; lines={:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| l.contains("per-user default home")),
            "no-home setup lines should identify the no-flag target; lines={:?}",
            lines
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("transist --home <path> home init")),
            "no-home setup lines should provide an explicit-home command; lines={:?}",
            lines
        );
        assert!(
            !lines.iter().any(|l| l.contains("create `.transist/`")),
            "no-home setup lines must not claim initialization uses the current directory; lines={:?}",
            lines
        );
    }

    #[test]
    fn build_no_home_dashboard_is_not_initialized() {
        let cwd = std::path::Path::new("/my/project");
        let data = build_no_home_dashboard(cwd, cwd, None).expect("no_home_dashboard");
        assert!(
            !data.initialized,
            "no-home dashboard should report initialized=false"
        );
        assert_eq!(
            data.dashboard_state
                .header_lines
                .first()
                .map(String::as_str),
            Some("Home: (none)"),
            "no-home header should say Home: (none)"
        );
    }

    #[test]
    fn runtime_posture_view_reads_mode_and_profile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        transist::init_home_dir_with_mode(tmp.path(), transist::RuntimeOperatingMode::Standalone)
            .expect("init");

        assert_eq!(
            load_runtime_posture_view(tmp.path()).expect("posture"),
            super::RuntimePostureView {
                mode: transist::RuntimeOperatingMode::Standalone,
                policy_profile: "permissive".to_string(),
            }
        );
    }

    #[test]
    fn runtime_posture_view_rejects_invalid_mode_profile_combination() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("config.yaml"),
            "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: permissive\n",
        )
        .expect("seed config");

        let error = load_runtime_posture_view(tmp.path()).expect_err("invalid posture");
        assert!(error.to_string().contains("governed runtime requires"));
    }

    #[test]
    fn fresh_execution_mode_requires_trust_in_governed_home() {
        assert!(FreshExecutionMode::TrustedRequired.trusted_execution());
        assert_eq!(
            FreshExecutionMode::TrustedRequired.status_label(),
            "trusted execution (required)"
        );
    }

    #[test]
    fn fresh_execution_mode_defaults_standard_outside_governed() {
        assert_eq!(
            fresh_execution_mode_picker_items()
                .first()
                .map(String::as_str),
            Some("Standard execution - no signature verification")
        );
        assert_eq!(
            fresh_execution_mode_from_picker_index(0),
            FreshExecutionMode::Standard
        );
        assert!(!FreshExecutionMode::Standard.trusted_execution());
    }

    #[test]
    fn launch_flow_order_includes_execution_mode() {
        let order = [
            super::LaunchStep::PickWorkflow,
            super::LaunchStep::ExecutionMode,
            super::LaunchStep::RunRef,
            super::LaunchStep::Inputs,
            super::LaunchStep::Readiness,
        ];
        assert_eq!(order.len(), 5);
        assert!(matches!(order[1], super::LaunchStep::ExecutionMode));
        assert!(matches!(order[2], super::LaunchStep::RunRef));
        assert!(matches!(order[4], super::LaunchStep::Readiness));
    }

    #[test]
    fn launch_flow_back_transitions_preserve_values() {
        let state = super::LaunchFlowState {
            workflow_ref: Some("demo@v1".to_string()),
            execution_mode: Some(FreshExecutionMode::Trusted),
            run_ref_seed: "run-1".to_string(),
            run_ref: Some(Some("run-1".to_string())),
            inputs_path_seed: "inputs.yaml".to_string(),
            last_inputs: None,
        };
        assert!(state.has_user_data());
        assert_eq!(state.workflow_ref.as_deref(), Some("demo@v1"));
        assert_eq!(state.run_ref_seed, "run-1");
        assert_eq!(state.inputs_path_seed, "inputs.yaml");
    }

    #[test]
    fn governed_launch_back_from_run_ref_returns_to_workflow_selection() {
        assert!(matches!(
            launch_step_before_run_ref(FreshExecutionMode::TrustedRequired),
            super::LaunchStep::PickWorkflow
        ));
        assert!(matches!(
            launch_step_before_run_ref(FreshExecutionMode::Trusted),
            super::LaunchStep::ExecutionMode
        ));
        assert!(matches!(
            launch_step_before_run_ref(FreshExecutionMode::Standard),
            super::LaunchStep::ExecutionMode
        ));
    }

    #[test]
    fn launch_outcome_carries_selected_execution_mode() {
        let outcome = super::LaunchOutcome {
            workflow_ref: "demo@v1".to_string(),
            execution_mode: FreshExecutionMode::Trusted,
            run_ref: None,
            workflow_inputs: std::collections::BTreeMap::new(),
        };
        assert_eq!(outcome.execution_mode, FreshExecutionMode::Trusted);
        assert!(outcome.execution_mode.trusted_execution());
    }

    #[test]
    fn concise_reports_ready_workflow_continues_without_review() {
        let report = readiness_summary(transist::WorkflowReadinessStatus::Ready);

        assert_eq!(
            launch_readiness_action(&report),
            super::LaunchReadinessAction::Continue
        );
        let mut prompt = scripted_prompt(false);
        assert!(
            super::review_launch_readiness(
                &mut prompt,
                |_| Ok(report.clone()),
                |_, _, _| panic!("ready must not authorize")
            )
            .unwrap()
        );
        assert!(prompt.paged_text_presentations.is_empty());
        assert_eq!(prompt.action_confirmations.len(), 1);
    }

    fn scripted_prompt(answer: bool) -> crate::prompt::scripted::ScriptedPrompt {
        crate::prompt::scripted::ScriptedPrompt::from_script(
            true,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![answer],
        )
    }

    fn authorization_summary(
        status: transist::WorkflowAuthorizationStatus,
    ) -> transist::WorkflowAuthorizationSummary {
        let mut readiness = readiness_summary(transist::WorkflowReadinessStatus::NotDeliverable);
        readiness.authorization_eligible = true;
        transist::WorkflowAuthorizationSummary {
            workflow: "demo@v1".into(),
            status,
            readiness,
            policy_output: Some("/policy/exact.yaml".into()),
            policy: Some(serde_json::json!({
                "grant_sets": {"exact": {"allow": {
                    "plugin_grants": [{"plugin": "package@v123456789", "actions": ["exact_action"]}],
                    "capabilities": [{"capability": "filesystem", "resource": format!("file:///{}", "long-directory/".repeat(100)), "operations": ["read", "write"]}]
                }}},
                "workflow_grants": [{"workflow": "demo@v1", "use": ["exact"]}]
            })),
        }
    }

    #[test]
    fn concise_reports_planned_review_and_explicit_decline_prevent_application() {
        let plan = authorization_summary(transist::WorkflowAuthorizationStatus::Planned);
        let mut prompt = scripted_prompt(false);
        assert!(
            !super::review_launch_readiness(
                &mut prompt,
                |_| Ok(plan.readiness.clone()),
                |_, apply, _| {
                    assert!(!apply, "decline must not apply");
                    Ok(plan.clone())
                },
            )
            .unwrap()
        );
        assert!(
            prompt.action_confirmations.is_empty(),
            "explicit No was consumed"
        );
        assert_eq!(
            prompt.paged_text_presentations,
            vec![(
                "Review authorization".into(),
                super::render_authorization_summary(&plan)
            )]
        );
        assert!(
            prompt.paged_text_presentations[0]
                .1
                .contains("Grants to add")
        );
    }

    #[test]
    fn concise_reports_applied_receipt_precedes_recheck_and_failed_recheck_stops() {
        for recheck_ready in [true, false] {
            let plan = authorization_summary(transist::WorkflowAuthorizationStatus::Planned);
            let mut applied = plan.clone();
            applied.status = transist::WorkflowAuthorizationStatus::Applied;
            let mut prompt = scripted_prompt(true);
            let checks = std::cell::Cell::new(0);
            let applications = std::cell::Cell::new(0);
            let rechecked = readiness_summary(if recheck_ready {
                transist::WorkflowReadinessStatus::Ready
            } else {
                transist::WorkflowReadinessStatus::NotReady
            });
            let result = super::review_launch_readiness(
                &mut prompt,
                |prompt| {
                    checks.set(checks.get() + 1);
                    if checks.get() == 1 {
                        return Ok(plan.readiness.clone());
                    }
                    prompt.present_paged_text("Recheck started", "")?;
                    Ok(rechecked.clone())
                },
                |_, apply, _| {
                    if apply {
                        applications.set(applications.get() + 1);
                        Ok(applied.clone())
                    } else {
                        Ok(plan.clone())
                    }
                },
            )
            .unwrap();
            assert_eq!(result, recheck_ready);
            assert_eq!(checks.get(), 2);
            assert_eq!(applications.get(), 1);
            assert!(
                prompt.action_confirmations.is_empty(),
                "explicit Yes was consumed"
            );
            assert_eq!(
                prompt.paged_text_presentations[0].1,
                super::render_authorization_summary(&plan)
            );
            assert_eq!(
                prompt.paged_text_presentations[1],
                (
                    "Workflow authorization".into(),
                    super::render_authorization_summary(&applied)
                )
            );
            assert_eq!(prompt.paged_text_presentations[2].0, "Recheck started");
            assert!(
                !prompt.paged_text_presentations[1]
                    .1
                    .contains("Missing grants")
            );
            if !recheck_ready {
                assert_eq!(
                    prompt.paged_text_presentations[3].1,
                    super::render_check_summary(&rechecked)
                );
            }
        }
    }

    #[test]
    fn concise_reports_already_authorized_rechecks_without_confirmation() {
        let summary =
            authorization_summary(transist::WorkflowAuthorizationStatus::AlreadyAuthorized);
        let mut prompt = scripted_prompt(false);
        let mut checks = 0;
        assert!(
            super::review_launch_readiness(
                &mut prompt,
                |_| {
                    checks += 1;
                    Ok(if checks == 1 {
                        summary.readiness.clone()
                    } else {
                        readiness_summary(transist::WorkflowReadinessStatus::Ready)
                    })
                },
                |_, apply, _| {
                    assert!(!apply);
                    Ok(summary.clone())
                },
            )
            .unwrap()
        );
        assert_eq!(checks, 2);
        assert_eq!(prompt.action_confirmations.len(), 1, "must not ask again");
        assert_eq!(
            prompt.paged_text_presentations[0].1,
            super::render_authorization_summary(&summary)
        );
    }

    #[test]
    fn concise_reports_blocked_authorization_does_not_confirm_or_apply() {
        let summary = authorization_summary(transist::WorkflowAuthorizationStatus::Blocked);
        let mut prompt = scripted_prompt(true);
        assert!(
            !super::review_launch_readiness(
                &mut prompt,
                |_| Ok(summary.readiness.clone()),
                |_, apply, _| {
                    assert!(!apply);
                    Ok(summary.clone())
                },
            )
            .unwrap()
        );
        assert_eq!(prompt.action_confirmations.len(), 1);
        assert!(
            prompt.paged_text_presentations[0]
                .1
                .contains("No grants written")
        );
    }

    #[test]
    fn concise_reports_summary_pager_preserves_exact_authority_at_both_sizes() {
        use super::super::{
            components,
            pager::{PagerState, pager_text_lines},
        };
        use ratatui::{Terminal, backend::TestBackend};
        let summary = authorization_summary(transist::WorkflowAuthorizationStatus::Planned);
        let mut prompt = scripted_prompt(false);
        super::confirm_launch_authorization(&mut prompt, &summary).unwrap();
        let body = &prompt.paged_text_presentations[0].1;
        assert_eq!(body, &super::render_authorization_summary(&summary));
        for (width, height) in [(80, 24), (40, 16)] {
            let lines = pager_text_lines(body, ratatui::layout::Size::new(width, height));
            assert_eq!(
                lines.concat(),
                body.lines().collect::<String>(),
                "no character lost at {width}x{height}"
            );
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = PagerState {
                title: "Review authorization".into(),
                lines,
                offset: 0,
                search_query: None,
                search_matches: vec![],
                search_index: 0,
                pending_g: false,
                status_line: "j/k move  Esc close".into(),
                diagnostic_body: None,
            };
            // Scroll every visual row to the first body row, including all chunks of the resource.
            for offset in 0..state.lines.len() {
                state.offset = offset;
                terminal
                    .draw(|frame| components::draw_pager_overlay(frame, &state))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let visible = (0..height)
                    .map(|y| {
                        (0..width)
                            .map(|x| buffer[(x, y)].symbol())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>();
                assert!(
                    visible.iter().any(|row| row.contains(&state.lines[offset])),
                    "row {offset} missing at {width}x{height}: {:?}",
                    state.lines[offset]
                );
            }
        }
    }

    #[test]
    fn concise_reports_pager_wraps_unicode_authority_by_display_width() {
        use super::super::pager::pager_text_lines;
        let body = format!(
            "file:///{}\n\nexact_action",
            "目录/e\u{301}/👩‍💻/".repeat(100)
        );
        for (width, height) in [(80, 24), (40, 16)] {
            let size = ratatui::layout::Size::new(width, height);
            let overlay = super::super::centered_rect(
                92,
                85,
                ratatui::layout::Rect::from((ratatui::layout::Position::ORIGIN, size)),
            );
            let rows = pager_text_lines(&body, size);
            assert_eq!(rows.concat(), body.lines().collect::<String>());
            assert!(rows.iter().any(String::is_empty), "blank row retained");
            for row in rows {
                assert!(ratatui::text::Line::raw(&row).width() <= usize::from(overlay.width - 2));
                assert!(!row.starts_with('\u{301}'), "combining grapheme split");
                assert!(!row.starts_with('\u{200d}'), "emoji grapheme split");
            }
        }
    }

    #[test]
    fn workflow_validation_fast_path_tui_authorization_decline_apply_and_recheck() {
        for approve in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(tmp.path()).unwrap();
            let home = root.join("home");
            let workspace = root.join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
                .unwrap();
            std::fs::write(home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflows, workflow-authorizations]\n").unwrap();
            std::fs::create_dir_all(home.join("policy/workflows")).unwrap();
            std::fs::create_dir_all(home.join("policy/workflow-authorizations")).unwrap();
            let context = transist::TransistContext::with_home(&workspace, &home);
            let flow = root.join("workflow.yaml");
            std::fs::write(&flow, format!("schema: transist.flow.v1\nid: demo\nversion: v1\nworkspace:\n  path: {}\n  access: read_only\nsteps:\n  - id: review\n    control: control.gate.manual\n    with:\n      gate_id: review\n", workspace.display())).unwrap();
            context.compile_flow_file(&flow).unwrap();
            let mut prompt = scripted_prompt(approve);
            let ready = super::review_launch_readiness(
                &mut prompt,
                |_| {
                    context.workflow_validation_summary("demo@v1", Default::default(), false, false)
                },
                |_, _, reviewed| {
                    if let Some(reviewed) = reviewed {
                        context.apply_reviewed_workflow_authorization_summary(
                            Default::default(),
                            reviewed,
                        )
                    } else {
                        context.authorize_workflow_summary(
                            "demo@v1",
                            Default::default(),
                            None,
                            false,
                        )
                    }
                },
            )
            .unwrap();
            assert_eq!(ready, approve);
            assert!(context.runs_list().unwrap().is_empty());
            assert_eq!(
                std::fs::read_dir(home.join("policy/workflow-authorizations"))
                    .unwrap()
                    .count(),
                usize::from(approve)
            );
            if approve {
                assert!(
                    prompt.paged_text_presentations[1]
                        .1
                        .contains("Grants added")
                );
                assert!(
                    context
                        .workflow_readiness_summary("demo@v1", Default::default(), false, false)
                        .unwrap()
                        .is_ready()
                );
            }
        }
    }

    #[test]
    fn workflow_validation_fast_path_tui_structurally_uses_validation_and_reviewed_plan() {
        let source = include_str!("dashboard.rs");
        let production = source.split("#[cfg(test)]").next().unwrap().to_string()
            + source
                .rsplit_once("\nenum LaunchReadinessAction {")
                .unwrap()
                .1;
        for forbidden in [
            "workflow_readiness_report(",
            "workflow_readiness_report_with_summary(",
            ".authorize_workflow(",
            "WorkflowReadinessReport",
            "enrich_launch_readiness",
            "evidence_fingerprint",
            "serde_json::to_value",
            "serde_json::to_string_pretty",
        ] {
            assert!(
                !production.contains(forbidden),
                "TUI must not use {forbidden}"
            );
        }
        assert!(production.contains("workflow_validation_summary("));
        assert!(!production.contains("workflow_readiness_summary("));
        assert!(production.contains("apply_reviewed_workflow_authorization_summary("));
        assert!(production.contains("authorize_workflow_summary("));
        // Confirmation uses the existing fullscreen default-No backend, not a new default-Yes modal.
        let prompt = include_str!("prompt.rs");
        let modal = prompt
            .split("fn run_confirm_modal(")
            .nth(1)
            .unwrap()
            .split("loop {")
            .next()
            .unwrap();
        assert!(modal.contains("selected_yes: false"));
    }

    #[test]
    fn concise_reports_missing_authority_requires_review_before_launch() {
        let mut report = readiness_summary(transist::WorkflowReadinessStatus::NotDeliverable);
        report.authorization_eligible = true;

        assert_eq!(
            launch_readiness_action(&report),
            super::LaunchReadinessAction::ReviewAuthorization
        );
    }

    #[test]
    fn concise_reports_other_blockers_show_shared_readiness_and_stop() {
        let mut report = readiness_summary(transist::WorkflowReadinessStatus::NotDeliverable);
        report.findings.push(transist::WorkflowCheckFinding {
            code: "plugin_prerequisite".into(),
            status: transist::ValidationStatus::Failed,
            message: "plugin unavailable".into(),
            remediation: Some("install package@v123".into()),
            occurrences: 1000,
            example_step_paths: vec!["child/step".into()],
        });

        assert_eq!(
            launch_readiness_action(&report),
            super::LaunchReadinessAction::Block
        );
        let mut prompt = scripted_prompt(true);
        assert!(
            !super::review_launch_readiness(
                &mut prompt,
                |_| Ok(report.clone()),
                |_, _, _| panic!("blocker must not authorize")
            )
            .unwrap()
        );
        assert_eq!(
            prompt.paged_text_presentations,
            vec![(
                "Workflow readiness".into(),
                super::render_check_summary(&report)
            )]
        );
        assert_eq!(prompt.action_confirmations.len(), 1);
    }

    #[test]
    fn concise_reports_names_missing_and_empty_workspace_inputs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical fixture root");
        let home = root.join("home");
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)
            .expect("init home");
        let context = transist::TransistContext::with_home(&root, &home);
        let flow = root.join("workflow.yaml");
        std::fs::write(
            &flow,
            r#"
schema: transist.flow.v1
id: tui_workspace_demo
version: v1
inputs:
  repo_path:
    type: string
    required: true
workspace:
  path: "{{inputs.repo_path}}"
  access: full
steps:
  - id: finish
    control: control.finish
    with: {}
"#,
        )
        .expect("flow");
        context.compile_flow_file(&flow).expect("compile");
        let workflow_ref = "tui_workspace_demo@v1";

        for (inputs, expected) in [
            (
                std::collections::BTreeMap::new(),
                "Required inputs: repo_path",
            ),
            (
                std::collections::BTreeMap::from([(
                    "repo_path".to_string(),
                    transist::KernelValue::String(String::new()),
                )]),
                "Required inputs: repo_path",
            ),
        ] {
            let report =
                super::load_launch_readiness_summary(&context, workflow_ref, inputs, false)
                    .expect("readiness summary");
            assert!(!report.is_ready());
            let rendered = super::render_check_summary(&report);
            assert!(rendered.contains(expected), "{rendered}");
            assert!(matches!(
                launch_readiness_action(&report),
                super::LaunchReadinessAction::Block
            ));
        }
    }

    #[test]
    fn workflow_run_preparation_failure_withholds_dashboard_runner() {
        let reason = runner_launch_withhold_reason("run-1", "failed", None)
            .expect("failed preparation should withhold the runner");

        assert!(reason.contains("no runner was started"));
    }

    #[test]
    fn workflow_run_lease_warning_withholds_dashboard_runner() {
        let reason =
            runner_launch_withhold_reason("run-1", "prepared", Some("lease release timed out"))
                .expect("lease-release warning should withhold the runner");

        assert!(reason.contains("automatic execution was withheld"));
        assert!(reason.contains("lease release timed out"));
    }

    #[test]
    fn workflow_run_allows_dashboard_runner_after_successful_preparation() {
        assert!(runner_launch_withhold_reason("run-1", "prepared", None).is_none());
    }
}

pub(super) fn dashboard_action_items_with_problem_count(
    problem_count: usize,
) -> Vec<DashboardActionItem> {
    vec![
        DashboardActionItem {
            action: DashboardActionId::LaunchWorkflow,
            label: "1. Run workflow".to_string(),
        },
        DashboardActionItem {
            action: DashboardActionId::ShowRunsList,
            label: "2. Browse runs".to_string(),
        },
        DashboardActionItem {
            action: DashboardActionId::Problems,
            label: format!("3. Problems ({problem_count})"),
        },
        DashboardActionItem {
            action: DashboardActionId::Preferences,
            label: "4. Preferences".to_string(),
        },
        DashboardActionItem {
            action: DashboardActionId::Refresh,
            label: "5. Refresh".to_string(),
        },
        DashboardActionItem {
            action: DashboardActionId::Quit,
            label: "6. Quit".to_string(),
        },
    ]
}

/// Steps of the multi-step LaunchWorkflow dashboard flow. The state machine
/// driver in [`run_launch_workflow_flow`] transitions between these on Back,
/// Forward, and Cancel signals from each step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchStep {
    PickWorkflow,
    ExecutionMode,
    RunRef,
    Inputs,
    Readiness,
}

/// Mutable state collected across the launch flow. Kept local to the
/// dashboard module because no other call site needs it. Each field's
/// purpose:
/// - `workflow_ref`: result of the workflow step, seeded back into the picker on Back.
/// - `execution_mode`: selected fresh-run trust mode, preserved across Back navigation.
/// - `run_ref_seed`: raw text the user typed at the run-reference step, preserved verbatim.
/// - `run_ref`: parsed run-reference result (`None` = "auto-generate").
/// - `inputs_path_seed`: raw text the user typed at the inputs step, preserved verbatim.
/// - `last_inputs`: validated map produced by the inputs step, mostly informational
///   today (the editor doesn't currently re-seed from this on Back, since
///   it always re-derives state from the inputs file path; we keep it so
///   future revisions can rehydrate it).
#[derive(Default)]
struct LaunchFlowState {
    workflow_ref: Option<String>,
    execution_mode: Option<FreshExecutionMode>,
    run_ref_seed: String,
    run_ref: Option<Option<String>>,
    inputs_path_seed: String,
    #[allow(dead_code)]
    last_inputs: Option<std::collections::BTreeMap<String, transist::KernelValue>>,
}

impl LaunchFlowState {
    /// Returns `true` if any user input has been collected. Used by step 1
    /// to decide whether Esc should ask for confirmation before discarding
    /// or cancel directly (no data → no confirmation needed).
    fn has_user_data(&self) -> bool {
        self.workflow_ref.is_some()
            || self.execution_mode.is_some()
            || !self.run_ref_seed.is_empty()
            || self.run_ref.is_some()
            || !self.inputs_path_seed.is_empty()
            || self.last_inputs.is_some()
    }
}

struct LaunchOutcome {
    workflow_ref: String,
    execution_mode: FreshExecutionMode,
    run_ref: Option<String>,
    workflow_inputs: std::collections::BTreeMap<String, transist::KernelValue>,
}

enum LaunchFlowResult {
    Submitted(LaunchOutcome),
    /// User-facing status message describing why the flow ended (cancel,
    /// nothing to launch, etc.).
    Canceled(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchReadinessAction {
    Continue,
    ReviewAuthorization,
    Block,
}

fn launch_readiness_action(report: &transist::WorkflowCheckSummary) -> LaunchReadinessAction {
    if report.is_validation_passed() {
        LaunchReadinessAction::Continue
    } else if report.authorization_eligible {
        LaunchReadinessAction::ReviewAuthorization
    } else {
        LaunchReadinessAction::Block
    }
}

/// Evaluates fast validation and restores declared input-name hints when binding fails
/// before the evaluator can collect them. Reads installed metadata only; never creates a run.
fn load_launch_readiness_summary(
    context: &transist::TransistContext,
    workflow_ref: &str,
    inputs: std::collections::BTreeMap<String, transist::KernelValue>,
    trusted_execution: bool,
) -> Result<transist::WorkflowCheckSummary> {
    let mut summary =
        context.workflow_validation_summary(workflow_ref, inputs, trusted_execution, false)?;
    if summary.readiness_status == Some(transist::WorkflowReadinessStatus::InvalidDefinition)
        && summary.required_input_names.is_empty()
    {
        if let Ok(inspection) = context.inspect_workflow(workflow_ref) {
            summary.required_input_names = inspection
                .summary
                .inputs
                .iter()
                .filter(|declaration| declaration.required)
                .map(|declaration| declaration.name.clone())
                .collect();
        }
    }
    Ok(summary)
}

/// Presents the exact typed plan and asks for explicit mutation confirmation.
/// Non-planned outcomes are displayed without asking for another grant confirmation.
fn confirm_launch_authorization(
    prompt: &mut dyn PromptUi,
    summary: &transist::WorkflowAuthorizationSummary,
) -> Result<bool> {
    prompt.present_paged_text(
        "Review authorization",
        &render_authorization_summary(summary),
    )?;
    if summary.status != transist::WorkflowAuthorizationStatus::Planned {
        return Ok(false);
    }
    if !prompt.confirmation_available() {
        return Ok(false);
    }
    // FullscreenPrompt's confirmation modal explicitly defaults to No.
    prompt.confirm_suggested_action(
        "Apply these workflow grants?",
        "The exact authority and destination shown in the review will be written. Continue?",
    )
}

/// Reviews summary-only validation and authority. Passing review still requires live preparation;
/// displays the applied receipt before rechecking, and never creates a run itself.
fn review_launch_readiness(
    prompt: &mut dyn PromptUi,
    mut readiness: impl FnMut(&mut dyn PromptUi) -> Result<transist::WorkflowCheckSummary>,
    mut authorize: impl FnMut(
        &mut dyn PromptUi,
        bool,
        Option<&transist::WorkflowAuthorizationSummary>,
    ) -> Result<transist::WorkflowAuthorizationSummary>,
) -> Result<bool> {
    let summary = readiness(prompt)?;
    match launch_readiness_action(&summary) {
        LaunchReadinessAction::Continue => return Ok(true),
        LaunchReadinessAction::Block => {
            prompt.present_paged_text("Workflow readiness", &render_check_summary(&summary))?;
            return Ok(false);
        }
        LaunchReadinessAction::ReviewAuthorization => {}
    }
    let authorization = authorize(prompt, false, None)?;
    if confirm_launch_authorization(prompt, &authorization)? {
        let applied = authorize(prompt, true, Some(&authorization))?;
        prompt.present_paged_text(
            "Workflow authorization",
            &render_authorization_summary(&applied),
        )?;
        if !matches!(
            applied.status,
            transist::WorkflowAuthorizationStatus::Applied
                | transist::WorkflowAuthorizationStatus::AlreadyAuthorized
        ) {
            return Ok(false);
        }
    } else if authorization.status != transist::WorkflowAuthorizationStatus::AlreadyAuthorized {
        return Ok(false);
    }
    let rechecked = readiness(prompt)?;
    if rechecked.is_validation_passed() {
        Ok(true)
    } else {
        prompt.present_paged_text("Workflow readiness", &render_check_summary(&rechecked))?;
        Ok(false)
    }
}

/// Drive the LaunchWorkflow flow as a small state machine. The loop only
/// terminates when the user submits readiness (returning `Submitted`) or
/// cancels (returning `Canceled` with a status message).
///
/// Esc semantics:
/// - Step 1 (workflow pick): Esc cancels directly when no data has been
///   entered yet. After a Back from a later step, Esc shows a "Discard
///   entered data?" confirm before unwinding.
/// - Steps 2, 3, 4, and readiness: Esc is "Back" (rewind one step, preserving entered data).
/// - Ctrl+C at any step: hard abort, no confirm.
fn run_launch_workflow_flow(
    prompt: &mut FullscreenPrompt,
    dashboard: &DashboardData,
    cwd: &Path,
) -> Result<LaunchFlowResult> {
    let mut state = LaunchFlowState::default();
    let mut step = LaunchStep::PickWorkflow;

    loop {
        match step {
            LaunchStep::PickWorkflow => {
                let outcome = pick_launchable_workflow_with_back(
                    prompt,
                    &dashboard.workflows,
                    state.workflow_ref.as_deref(),
                )?;
                match outcome {
                    StepOutcome::Ok(workflow_ref) => {
                        state.workflow_ref = Some(workflow_ref);
                        step = LaunchStep::ExecutionMode;
                    }
                    StepOutcome::Cancel => {
                        return Ok(LaunchFlowResult::Canceled(
                            "Run workflow canceled.".to_string(),
                        ));
                    }
                    StepOutcome::Back => {
                        // Step 1 has no prior step. If the user has entered
                        // data on a later step (and then backed up here),
                        // confirm before discarding; otherwise cancel
                        // directly so a casual Esc on an empty picker stays
                        // snappy.
                        if state.has_user_data() {
                            let discard = confirm_cancel_with_data_loss(
                                prompt,
                                "Discard entered run data and return to the dashboard?",
                            )?;
                            if discard {
                                return Ok(LaunchFlowResult::Canceled(
                                    "Run workflow canceled.".to_string(),
                                ));
                            }
                            // Stay on step 1 with the previous selection
                            // re-highlighted on the next iteration.
                            continue;
                        }
                        return Ok(LaunchFlowResult::Canceled(
                            "Workflow selection canceled.".to_string(),
                        ));
                    }
                }
            }
            LaunchStep::ExecutionMode => {
                let outcome = resolve_fresh_execution_mode(
                    prompt,
                    dashboard.context.home_dir(),
                    state.execution_mode,
                )?;
                match outcome {
                    StepOutcome::Ok(execution_mode) => {
                        state.execution_mode = Some(execution_mode);
                        step = LaunchStep::RunRef;
                    }
                    StepOutcome::Back => step = LaunchStep::PickWorkflow,
                    StepOutcome::Cancel => {
                        return Ok(LaunchFlowResult::Canceled(
                            "Run workflow canceled.".to_string(),
                        ));
                    }
                };
            }
            LaunchStep::RunRef => {
                let outcome = prompt_optional_run_ref_with_back(
                    prompt,
                    "Run id (optional)",
                    &state.run_ref_seed,
                    StepHint::Subsequent,
                )?;
                match outcome {
                    StepOutcome::Ok(run_ref) => {
                        // Preserve the verbatim seed (including the empty
                        // case) so revisiting this step redisplays whatever
                        // the user typed.
                        state.run_ref_seed = run_ref.clone().unwrap_or_default();
                        state.run_ref = Some(run_ref);
                        step = LaunchStep::Inputs;
                    }
                    StepOutcome::Back => {
                        step = launch_step_before_run_ref(
                            state
                                .execution_mode
                                .expect("execution_mode must be set before reaching RunRef"),
                        );
                    }
                    StepOutcome::Cancel => {
                        return Ok(LaunchFlowResult::Canceled(
                            "Run workflow canceled.".to_string(),
                        ));
                    }
                }
            }
            LaunchStep::Inputs => {
                let workflow_ref = state
                    .workflow_ref
                    .clone()
                    .expect("workflow_ref must be set before reaching Inputs step");
                let context = dashboard.context.clone();
                let selected_workflow = workflow_ref.clone();
                let inspection =
                    run_with_loading_feedback(prompt, "Loading workflow inputs", move || {
                        context.inspect_workflow(&selected_workflow)
                    })?;
                let outcome = prompt_workflow_inputs_with_back(
                    prompt,
                    &inspection.recipe,
                    cwd,
                    &mut state.inputs_path_seed,
                    StepHint::Subsequent,
                )?;
                match outcome {
                    StepOutcome::Ok(map) => {
                        state.last_inputs = Some(map);
                        step = LaunchStep::Readiness;
                    }
                    StepOutcome::Back => {
                        step = LaunchStep::RunRef;
                    }
                    StepOutcome::Cancel => {
                        return Ok(LaunchFlowResult::Canceled(
                            "Run workflow canceled.".to_string(),
                        ));
                    }
                }
            }
            LaunchStep::Readiness => {
                let workflow_ref = state
                    .workflow_ref
                    .clone()
                    .expect("workflow_ref must be set before readiness");
                let execution_mode = state
                    .execution_mode
                    .expect("execution_mode must be set before readiness");
                let workflow_inputs = state
                    .last_inputs
                    .as_ref()
                    .expect("workflow inputs must be set before readiness");
                let ready = review_launch_readiness(
                    prompt,
                    |prompt| {
                        let context = dashboard.context.clone();
                        let workflow = workflow_ref.clone();
                        let inputs = workflow_inputs.clone();
                        run_with_loading_feedback(
                            prompt,
                            "Validating workflow (live readiness checked at preparation)",
                            move || {
                                load_launch_readiness_summary(
                                    &context,
                                    &workflow,
                                    inputs,
                                    execution_mode.trusted_execution(),
                                )
                            },
                        )
                    },
                    |prompt, apply, reviewed| {
                        let context = dashboard.context.clone();
                        let workflow = workflow_ref.clone();
                        let inputs = workflow_inputs.clone();
                        let reviewed = reviewed.cloned();
                        run_with_loading_feedback(
                            prompt,
                            if apply {
                                "Applying workflow grants"
                            } else {
                                "Planning workflow authorization"
                            },
                            move || {
                                if let Some(reviewed) = reviewed {
                                    context.apply_reviewed_workflow_authorization_summary(
                                        inputs, &reviewed,
                                    )
                                } else {
                                    context
                                        .authorize_workflow_summary(&workflow, inputs, None, false)
                                }
                            },
                        )
                    },
                )?;
                if ready {
                    return Ok(LaunchFlowResult::Submitted(LaunchOutcome {
                        workflow_ref,
                        execution_mode,
                        run_ref: state.run_ref.clone().unwrap_or(None),
                        workflow_inputs: workflow_inputs.clone(),
                    }));
                }
                return Ok(LaunchFlowResult::Canceled(
                    "Workflow launch stopped during readiness or authorization review. No run was created.".to_string(),
                ));
            }
        }
    }
}
