// SPDX-License-Identifier: MPL-2.0
//! FullscreenPrompt + supporting state, terminal lifecycle, and prompt helpers.

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

thread_local! {
    /// Number of `FullscreenPrompt` instances currently alive on this
    /// thread. Construction increments it; `Drop` decrements it. Used so
    /// that a nested `FullscreenPrompt` (e.g. constructed by
    /// `attach_run_viewer` while a parent run-detail screen is already on
    /// the call stack) does not tear the parent's raw-mode + alt-screen
    /// terminal down when it unwinds. Without this guard, a child prompt's
    /// `Drop` would call `restore_terminal_state()` while the outer prompt
    /// is still drawing, breaking the parent screen (Esc echoed as `^[`,
    /// keystrokes invisible to crossterm, no redraw).
    static FULLSCREEN_PROMPT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Returns true if at least one `FullscreenPrompt` is currently alive on
/// this thread (i.e. the alt screen + raw mode are owned by the TUI).
/// Callers that print to stdout/stderr (e.g. spawn announcements) must
/// suppress their writes when this returns true to avoid corrupting the
/// alt-screen frame.
pub(crate) fn fullscreen_prompt_active() -> bool {
    FULLSCREEN_PROMPT_DEPTH.with(|c| c.get() > 0)
}

fn fullscreen_prompt_depth_inc() -> usize {
    FULLSCREEN_PROMPT_DEPTH.with(|c| {
        let next = c.get().saturating_add(1);
        c.set(next);
        next
    })
}

fn fullscreen_prompt_depth_dec() -> usize {
    FULLSCREEN_PROMPT_DEPTH.with(|c| {
        let next = c.get().saturating_sub(1);
        c.set(next);
        next
    })
}
use ratatui_textarea::TextArea;

use crate::cli::CliUiMode;
use crate::display::spinner::LiveProgress;
use crate::display::summary::{SuggestedAction, SuggestedActionExecution};
use crate::input::load_inputs_file;
use crate::pager::RenderedTerminalBlock;
use crate::prompt::dialoguer::{numbered_picker_items, suggested_action_shortcut_index};
use crate::prompt::{PickerSelection, PromptInputOutcome, PromptUi};
use crate::terminal::terminal_supports_redraw;

/// Map Ctrl+C to KeyCode::Esc so quit/cancel arms in modal loops can handle
/// Ctrl+C uniformly. crossterm raw mode delivers Ctrl+C as a key event rather
/// than triggering SIGINT, so each loop must opt in.
fn remap_ctrl_c_to_esc(code: KeyCode, modifiers: KeyModifiers) -> KeyCode {
    if matches!(code, KeyCode::Char('c')) && modifiers.contains(KeyModifiers::CONTROL) {
        KeyCode::Esc
    } else {
        code
    }
}

fn normalize_pasted_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\r') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

fn insert_pasted_text(textarea: &mut TextArea<'_>, pasted: &str) -> bool {
    let normalized = normalize_pasted_text(pasted);
    if normalized.is_empty() {
        return false;
    }

    let mut segment_start = 0;
    for (idx, ch) in normalized.char_indices() {
        if ch == '\n' {
            if segment_start < idx {
                textarea.insert_str(&normalized[segment_start..idx]);
            }
            textarea.insert_newline();
            segment_start = idx + ch.len_utf8();
        }
    }

    if segment_start < normalized.len() {
        textarea.insert_str(&normalized[segment_start..]);
    }

    true
}

use super::components;
use super::configuration::ConfigScreenState;
use super::dashboard::DashboardState;
use super::format::{join_text_area_lines, status_color, style_hint_line, truncate_vec};
use super::pager::{PagerState, handle_pager_key, pager_text_lines, pager_visible_count_for_area};
use super::problems::ProblemsViewState;
use super::run_browser::RunBrowserState;
use super::run_detail::RunDetailState;
use super::screens;

pub(super) type FullscreenTerminal = Terminal<CrosstermBackend<io::Stdout>>;

#[derive(Debug, Clone, Default)]
pub(super) struct ScreenMetadata {
    pub(super) run_id: Option<String>,
    pub(super) workflow_ref: Option<String>,
    pub(super) status: Option<String>,
    pub(super) actionable_run_id: Option<String>,
    pub(super) current_step: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ActionPickerState {
    pub(super) title: String,
    pub(super) items: Vec<String>,
    pub(super) selected: usize,
    pub(super) actionable: bool,
}

#[derive(Debug)]
pub(super) struct TextInputState {
    pub(super) help: String,
    pub(super) textarea: TextArea<'static>,
    pub(super) submit_on_enter: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ConfirmState {
    pub(super) title: String,
    pub(super) message: String,
    pub(super) selected_yes: bool,
}

#[derive(Debug, Clone)]
pub(super) struct LoadingState {
    pub(super) message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkflowInputsPane {
    List,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkflowInputOrigin {
    None,
    File,
    UserEdited,
}

#[derive(Debug, Clone)]
pub(super) struct WorkflowInputItem {
    pub(super) name: String,
    pub(super) value_type: String,
    pub(super) required: bool,
    pub(super) default_value: Option<String>,
    pub(super) comment: Option<String>,
    pub(super) value: Option<String>,
    pub(super) origin: WorkflowInputOrigin,
}

impl WorkflowInputItem {
    /// Returns the explicit override or inherited JSON default for display in the editor.
    pub(super) fn editor_value(&self) -> Option<&str> {
        self.value.as_deref().or(self.default_value.as_deref())
    }
}

#[derive(Debug)]
pub(super) struct WorkflowInputsModalState {
    pub(super) declared: Vec<WorkflowInputItem>,
    pub(super) selected: usize,
    pub(super) focused_pane: WorkflowInputsPane,
    pub(super) editor: TextArea<'static>,
    pub(super) status: Option<String>,
    pub(super) file_label: Option<String>,
}

impl WorkflowInputsModalState {
    /// Commits changed editor text as an override; clearing restores the declared default.
    /// Merely viewing a value preserves its origin and does not materialize inherited defaults.
    pub(super) fn commit_editor(&mut self) {
        let text = join_text_area_lines(&self.editor);
        if let Some(item) = self.declared.get_mut(self.selected) {
            if item.editor_value().unwrap_or("") == text {
                return;
            }
            if text.trim().is_empty() {
                item.value = None;
                item.origin = WorkflowInputOrigin::None;
                self.editor = make_workflow_inputs_editor(item.editor_value());
            } else {
                item.value = Some(text);
                item.origin = WorkflowInputOrigin::UserEdited;
            }
        }
    }
}

pub(crate) struct FullscreenPrompt {
    pub(super) terminal: FullscreenTerminal,
    pub(super) mouse_capture_enabled: bool,
    pub(super) flush_transcript_on_drop: bool,
    pub(super) transcript_lines: Vec<String>,
    pub(super) history_lines: Vec<String>,
    pub(super) current_progress: Option<LiveProgress>,
    pub(super) current_home_dir: Option<PathBuf>,
    pub(super) current_run_detail: Option<transist::InspectionRunDetail>,
    /// Eagerly-loaded `InspectionRunDetail` for the actionable run when it
    /// differs from the run displayed in the header (i.e. the user is looking
    /// at a parent whose actionable work lives in a child subflow). Lets the
    /// run-detail Details pane surface child gates and child step artifacts
    /// inline so an operator does not have to drill into the child run with a
    /// separate `runs show` to see what they should review.
    pub(super) current_actionable_run_detail: Option<transist::InspectionRunDetail>,
    /// Eagerly-loaded inspection details for every child run contained under
    /// `current_run_detail`, keyed by child run id. Used by the Details pane
    /// to render parent-step subflow bodies that include the artifacts and
    /// workflow_files produced inside child runs.
    pub(super) current_contained_run_details:
        std::collections::BTreeMap<String, transist::InspectionRunDetail>,
    pub(super) spinner_tick: u64,
    pub(super) metadata: ScreenMetadata,
    pub(super) dashboard_state: Option<DashboardState>,
    pub(super) config_screen_state: Option<ConfigScreenState>,
    pub(super) run_detail_screen_active: bool,
    pub(super) run_detail_state: Option<RunDetailState>,
    pub(super) picker_state: Option<ActionPickerState>,
    pub(super) run_browser_state: Option<RunBrowserState>,
    pub(super) problems_view_state: Option<ProblemsViewState>,
    pub(super) pager_state: Option<PagerState>,
    pub(super) text_input_state: Option<TextInputState>,
    pub(super) select_modal_state: Option<ActionPickerState>,
    pub(super) confirm_state: Option<ConfirmState>,
    pub(super) loading_state: Option<LoadingState>,
    pub(super) workflow_inputs_state: Option<WorkflowInputsModalState>,
    /// Draft text remembered across Esc-out and re-entry of the gate
    /// reason prompt, keyed by `(action_kind, run_id, gate_id?)` so two
    /// different rejects do not bleed text into each other. Submitting
    /// (Enter) drops the entry; Ctrl+C drops the entry; Esc preserves
    /// whatever was in the textarea so the next invocation with the same
    /// key re-seeds the field.
    pub(super) reason_drafts: HashMap<String, String>,
    /// Hash of the inputs that produced the most recently rendered run-detail
    /// frame. Populated by `apply_run_detail_loaded_data` to short-circuit
    /// `prompt.draw()` calls whose data has not changed since the previous
    /// draw — this is the primary CPU saver for the run-detail screen while a
    /// long-running tool produces streaming output that does not affect the
    /// visible frame (live preview lines were removed; only status / counter
    /// transitions matter). Reset (cleared) on screen change, manual redraw,
    /// terminal resize, and any key handler that mutates non-data state.
    pub(super) last_drawn_run_detail_frame_fingerprint: Option<u64>,
}

impl FullscreenPrompt {
    pub(super) fn with_spinner<T>(
        &mut self,
        message: &str,
        operation: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        crate::prompt::run_with_loading_feedback(self, message, operation)
    }

    pub(super) fn new(flush_transcript_on_drop: bool) -> Result<Self> {
        // Only the outermost `FullscreenPrompt` on the thread owns the raw
        // mode + alt-screen lifecycle. Nested constructions (e.g. via
        // `attach_run_viewer` re-entering after a gate decision) reuse the
        // existing terminal state and just allocate a new state struct.
        // Without this guard, a nested prompt's `Drop` would tear the
        // parent screen down when it unwinds — which is the original
        // freeze symptom (Esc echoed as `^[`, keystrokes invisible to
        // crossterm, no redraw).
        let depth_after_inc = fullscreen_prompt_depth_inc();
        let new_owner = depth_after_inc == 1;
        let init_result: Result<FullscreenTerminal> = (|| {
            if new_owner {
                enable_raw_mode().context("enable raw mode for fullscreen CLI")?;
                let mut stdout = io::stdout();
                execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide)
                    .context("enter alternate screen for fullscreen CLI")?;
            }
            let backend = CrosstermBackend::new(io::stdout());
            let mut terminal = Terminal::new(backend).context("create fullscreen terminal")?;
            terminal.clear().context("clear fullscreen terminal")?;
            Ok(terminal)
        })();
        let terminal = match init_result {
            Ok(t) => t,
            Err(err) => {
                // Roll the depth back so the next attempt starts clean and
                // a stale "active" flag does not suppress later announce
                // prints.
                fullscreen_prompt_depth_dec();
                if new_owner {
                    restore_terminal_state();
                }
                return Err(err);
            }
        };
        Ok(Self {
            terminal,
            mouse_capture_enabled: false,
            flush_transcript_on_drop,
            transcript_lines: Vec::new(),
            history_lines: Vec::new(),
            current_progress: None,
            current_home_dir: None,
            current_run_detail: None,
            current_actionable_run_detail: None,
            current_contained_run_details: std::collections::BTreeMap::new(),
            spinner_tick: 0,
            metadata: ScreenMetadata::default(),
            dashboard_state: None,
            config_screen_state: None,
            run_detail_screen_active: false,
            run_detail_state: None,
            picker_state: None,
            run_browser_state: None,
            problems_view_state: None,
            pager_state: None,
            text_input_state: None,
            select_modal_state: None,
            confirm_state: None,
            loading_state: None,
            workflow_inputs_state: None,
            reason_drafts: HashMap::new(),
            last_drawn_run_detail_frame_fingerprint: None,
        })
    }

    pub(super) fn draw(&mut self) -> Result<()> {
        self.draw_inner(false)
    }

    pub(super) fn handle_terminal_resize(&mut self) -> Result<()> {
        // Resize changes the available `details_width` and overall pane
        // geometry, so the cached run-detail frame fingerprint is no longer
        // representative of what's on screen. Force the next data-driven
        // draw to run unconditionally by clearing the cached fingerprint.
        self.last_drawn_run_detail_frame_fingerprint = None;
        self.draw_inner(true)
    }

    pub(super) fn draw_inner(&mut self, clear_first: bool) -> Result<()> {
        if clear_first {
            self.terminal
                .clear()
                .context("clear fullscreen terminal on resize")?;
        }
        let history_lines = self.history_lines.clone();
        let progress = self.current_progress.clone();
        let current_home_dir = self.current_home_dir.clone();
        let current_run_detail = self.current_run_detail.clone();
        let current_contained_run_details = self.current_contained_run_details.clone();
        let metadata = self.metadata.clone();
        let dashboard_state = self.dashboard_state.clone();
        let config_screen_state = self.config_screen_state.clone();
        let run_detail_state = self.run_detail_state.clone();
        let picker_state = self.picker_state.clone();
        let run_browser_state = self.run_browser_state.clone();
        let problems_view_state = self.problems_view_state.clone();
        let pager_state = self.pager_state.clone();
        let run_detail_screen_active = self.run_detail_screen_active;
        let confirm_state = self.confirm_state.clone();
        let loading_state = self.loading_state.clone();
        let select_modal_state = self.select_modal_state.clone();
        let spinner_tick = self.spinner_tick;
        // Live references kept alongside the &mut self.terminal borrow so the
        // textarea widgets render with their built-in cursor.
        let text_input_state = self.text_input_state.as_ref();
        let workflow_inputs_state = self.workflow_inputs_state.as_ref();
        self.terminal.draw(|frame| {
            if let Some(dashboard_state) = dashboard_state.as_ref() {
                components::draw_dashboard_frame(frame, dashboard_state);
                if let Some(state) = picker_state.as_ref() {
                    components::draw_picker_overlay(frame, state);
                }
            } else if let Some(config_screen_state) = config_screen_state.as_ref() {
                components::draw_config_screen_frame(frame, config_screen_state);
            } else if let Some(run_browser_state) = run_browser_state.as_ref() {
                components::draw_run_browser_frame(frame, run_browser_state);
            } else if let Some(problems_view_state) = problems_view_state.as_ref() {
                components::draw_problems_frame(frame, problems_view_state);
            } else if run_detail_screen_active || progress.is_some() {
                components::draw_run_detail_frame(
                    frame,
                    &metadata,
                    current_home_dir.as_deref(),
                    progress.as_ref(),
                    current_run_detail.as_ref(),
                    Some(&current_contained_run_details),
                    spinner_tick,
                    run_detail_state.as_ref(),
                    picker_state.as_ref(),
                );
            } else {
                components::draw_base_frame(
                    frame,
                    &metadata,
                    None,
                    progress.as_ref(),
                    &history_lines,
                    picker_state.as_ref(),
                );
            }
            if let Some(state) = pager_state.as_ref() {
                components::draw_pager_overlay(frame, state);
            }
            if let Some(state) = select_modal_state.as_ref() {
                components::draw_picker_overlay(frame, state);
            }
            if let Some(state) = text_input_state {
                components::draw_text_input_overlay(frame, state);
            }
            if let Some(state) = workflow_inputs_state {
                components::draw_workflow_inputs_overlay(frame, state);
            }
            if let Some(state) = confirm_state.as_ref() {
                components::draw_confirm_overlay(frame, state);
            }
            if let Some(state) = loading_state.as_ref() {
                components::draw_loading_overlay(frame, state);
            }
        })?;
        Ok(())
    }

    pub(super) fn set_mouse_capture_enabled(&mut self, _enabled: bool) -> Result<()> {
        // Mouse capture is intentionally disabled to allow native text
        // selection in the host terminal. No mouse events are consumed by
        // the TUI, so this is a no-op kept for call-site compatibility.
        Ok(())
    }

    pub(super) fn push_history_lines(&mut self, lines: &[String]) {
        for line in lines {
            self.history_lines.push(line.clone());
            self.transcript_lines.push(line.clone());
        }
        truncate_vec(&mut self.history_lines, 400);
        truncate_vec(&mut self.transcript_lines, 4000);
        update_metadata_from_lines(&mut self.metadata, lines);
    }

    pub(super) fn replace_history_lines(&mut self, lines: Vec<String>) {
        self.history_lines = lines;
        truncate_vec(&mut self.history_lines, 400);
        self.metadata.workflow_ref = None;
        update_metadata_from_lines(&mut self.metadata, &self.history_lines);
    }

    pub(super) fn set_dashboard_state(&mut self, dashboard_state: DashboardState) -> Result<()> {
        self.dashboard_state = Some(dashboard_state);
        self.config_screen_state = None;
        self.run_detail_screen_active = false;
        self.run_detail_state = None;
        self.run_browser_state = None;
        self.problems_view_state = None;
        self.picker_state = None;
        self.current_progress = None;
        self.current_home_dir = None;
        self.current_run_detail = None;
        self.current_actionable_run_detail = None;
        self.current_contained_run_details.clear();
        self.loading_state = None;
        self.draw()
    }

    /// Move the selected step / scroll the Details pane during a live run.
    /// `delta` is `1` for Down/j and `-1` for Up/k.
    pub(super) fn live_navigation_step(&mut self, delta: isize) -> bool {
        let Some(state) = self.run_detail_state.as_mut() else {
            return false;
        };
        match state.focused_section {
            super::run_detail::RunDetailSection::Steps => {
                let step_count = self
                    .current_progress
                    .as_ref()
                    .map(|progress| progress.state.checklist.len())
                    .or_else(|| {
                        self.current_run_detail
                            .as_ref()
                            .map(|detail| detail.steps.len())
                    })
                    .unwrap_or(0);
                if step_count == 0 {
                    return false;
                }
                let previous = state.selected_step;
                let next = if delta >= 0 {
                    (previous + delta as usize).min(step_count.saturating_sub(1))
                } else {
                    previous.saturating_sub(delta.unsigned_abs())
                };
                if next == previous {
                    return false;
                }
                state.selected_step = next;
                state.details_scroll = 0;
                state.step_pinned_by_user = true;
                true
            }
            super::run_detail::RunDetailSection::Details => {
                let (_, _, details_width, details_visible, _, _) = self
                    .terminal
                    .size()
                    .map(super::run_detail::run_detail_section_metrics)
                    .unwrap_or((30, 10, 40, 10, 20, 10));
                let detail_line_count = super::run_detail::build_run_detail_history_lines(
                    self.current_home_dir.as_deref(),
                    self.current_run_detail.as_ref(),
                    Some(&self.current_contained_run_details),
                    self.current_progress.as_ref(),
                    Some(state.selected_step),
                    details_width,
                    self.spinner_tick,
                )
                .lines
                .len();
                let previous = state.details_scroll;
                super::format::apply_scroll_delta(
                    &mut state.details_scroll,
                    detail_line_count,
                    details_visible,
                    delta,
                );
                state.details_scroll != previous
            }
            super::run_detail::RunDetailSection::Actions => false,
        }
    }

    /// Cycle the focused section while the modal `select_suggested_action`
    /// loop is active and rendering inside the run-detail layout (i.e. the
    /// Commands sub-pane sits next to Steps and Details).
    ///
    /// Tab cycles `Steps → Details → Actions` (forward = true); Shift+Tab
    /// cycles in the opposite direction. Mirrors the run-detail screen's own
    /// section switcher so the operator's mental model is consistent.
    fn picker_loop_section_switch(&mut self, forward: bool) -> bool {
        let Some(state) = self.run_detail_state.as_mut() else {
            return false;
        };
        let next = super::run_detail::run_detail_move_focus(state.focused_section, forward, true);
        if next == state.focused_section {
            return false;
        }
        state.focused_section = next;
        true
    }

    /// Navigate within the focused section while the modal
    /// `select_suggested_action` loop is active and rendered inline next to
    /// Steps / Details. `delta` is `1` for Down/j and `-1` for Up/k.
    /// In `Actions` focus this advances the picker selection.
    fn picker_loop_navigate_within_section(&mut self, delta: isize) -> bool {
        let focused_section = match self.run_detail_state.as_ref() {
            Some(s) => s.focused_section,
            None => return false,
        };
        match focused_section {
            super::run_detail::RunDetailSection::Steps
            | super::run_detail::RunDetailSection::Details => {
                // Reuse the live-run helper: moves step selection or scrolls
                // Details depending on the focused section.
                self.live_navigation_step(delta)
            }
            super::run_detail::RunDetailSection::Actions => {
                let Some(picker) = self.picker_state.as_mut() else {
                    return false;
                };
                if picker.items.is_empty() {
                    return false;
                }
                let previous = picker.selected;
                let len = picker.items.len();
                let next = if delta >= 0 {
                    (previous + delta as usize) % len
                } else {
                    let step = (delta.unsigned_abs()) % len.max(1);
                    (previous + len.saturating_sub(step)) % len
                };
                picker.selected = next;
                if let Some(state) = self.run_detail_state.as_mut() {
                    state.selected_action = next;
                }
                next != previous
            }
        }
    }

    pub(super) fn run_text_input_modal(
        &mut self,
        title: &str,
        help: &str,
        initial: &str,
        submit_on_enter: bool,
    ) -> Result<Option<String>> {
        self.run_text_input_modal_with_placeholder(title, help, initial, None, submit_on_enter)
    }

    /// Same as `run_text_input_modal` but with an optional greyed-out
    /// placeholder shown while the textarea is empty. The placeholder follows
    /// the standard convention (e.g. `path/to/file`) and is rendered in
    /// `Color::DarkGray` so it is visually distinct from real user input.
    pub(super) fn run_text_input_modal_with_placeholder(
        &mut self,
        title: &str,
        help: &str,
        initial: &str,
        placeholder: Option<&str>,
        submit_on_enter: bool,
    ) -> Result<Option<String>> {
        let mut textarea = TextArea::from(initial.lines().map(str::to_string).collect::<Vec<_>>());
        if textarea.lines().is_empty() {
            textarea.insert_str(initial);
        }
        // Title styled cyan + bold so the modal frame matches the rest of the
        // accent palette (focused content frames, "Transist" outer chrome).
        textarea.set_block(
            Block::default().borders(Borders::ALL).title(Span::styled(
                title.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
        );
        textarea.set_cursor_line_style(Style::default().fg(Color::Yellow));
        if let Some(text) = placeholder {
            textarea.set_placeholder_text(text);
            textarea.set_placeholder_style(Style::default().fg(Color::DarkGray));
        }
        self.text_input_state = Some(TextInputState {
            help: help.to_string(),
            textarea,
            submit_on_enter,
        });
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => self.handle_terminal_resize()?,
                Event::Paste(text) => {
                    if let Some(state) = self.text_input_state.as_mut() {
                        insert_pasted_text(&mut state.textarea, &text);
                    }
                    self.draw()?;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if matches!(remap_ctrl_c_to_esc(key.code, key.modifiers), KeyCode::Esc) {
                        self.text_input_state = None;
                        self.draw()?;
                        return Ok(None);
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('s'))
                    {
                        let state = self.text_input_state.take().expect("text input state");
                        self.draw()?;
                        return Ok(Some(join_text_area_lines(&state.textarea)));
                    }
                    if let Some(state) = self.text_input_state.as_mut() {
                        // Alt+Enter and Shift+Enter both insert a newline so
                        // users can compose multi-line text even when
                        // `submit_on_enter` makes plain Enter the submit
                        // shortcut. We accept both because terminal emulators
                        // disagree about which modifier+Enter combination they
                        // forward to the application.
                        if matches!(key.code, KeyCode::Enter)
                            && (key.modifiers.contains(KeyModifiers::ALT)
                                || key.modifiers.contains(KeyModifiers::SHIFT))
                        {
                            state.textarea.insert_newline();
                            self.draw()?;
                            continue;
                        }
                        if state.submit_on_enter && matches!(key.code, KeyCode::Enter) {
                            let state = self.text_input_state.take().expect("text input state");
                            self.draw()?;
                            return Ok(Some(join_text_area_lines(&state.textarea)));
                        }
                        state.textarea.input(key);
                    }
                    self.draw()?;
                }
                _ => {}
            }
        }
    }

    pub(super) fn run_select_modal(
        &mut self,
        title: &str,
        items: Vec<String>,
        initial: usize,
    ) -> Result<Option<usize>> {
        if items.is_empty() {
            return Ok(None);
        }
        let selected = initial.min(items.len().saturating_sub(1));
        self.select_modal_state = Some(ActionPickerState {
            title: title.to_string(),
            items,
            selected,
            actionable: true,
        });
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => self.handle_terminal_resize()?,
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match remap_ctrl_c_to_esc(key.code, key.modifiers) {
                        KeyCode::Esc => {
                            self.select_modal_state = None;
                            self.draw()?;
                            return Ok(None);
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            let state = self.select_modal_state.take().expect("select modal state");
                            self.draw()?;
                            return Ok(Some(state.selected));
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                if !state.items.is_empty() {
                                    state.selected = (state.selected + 1) % state.items.len();
                                }
                            }
                            self.draw()?;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                if !state.items.is_empty() {
                                    state.selected = (state.selected + state.items.len() - 1)
                                        % state.items.len();
                                }
                            }
                            self.draw()?;
                        }
                        KeyCode::Home | KeyCode::Char('g') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                state.selected = 0;
                            }
                            self.draw()?;
                        }
                        KeyCode::End | KeyCode::Char('G') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                state.selected = state.items.len().saturating_sub(1);
                            }
                            self.draw()?;
                        }
                        KeyCode::Char(ch) if ch.is_ascii_digit() => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                if let Some(index) =
                                    suggested_action_shortcut_index(ch, state.items.len())
                                {
                                    state.selected = index;
                                    let state =
                                        self.select_modal_state.take().expect("select modal state");
                                    self.draw()?;
                                    return Ok(Some(state.selected));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) fn run_confirm_modal(&mut self, title: &str, message: &str) -> Result<bool> {
        self.confirm_state = Some(ConfirmState {
            title: title.to_string(),
            message: message.to_string(),
            selected_yes: false,
        });
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => self.handle_terminal_resize()?,
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(state) = self.confirm_state.as_mut() {
                        match remap_ctrl_c_to_esc(key.code, key.modifiers) {
                            KeyCode::Left | KeyCode::Char('h') => state.selected_yes = true,
                            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                                state.selected_yes = false
                            }
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                self.confirm_state = None;
                                self.draw()?;
                                return Ok(true);
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                self.confirm_state = None;
                                self.draw()?;
                                return Ok(false);
                            }
                            KeyCode::Enter => {
                                let selected_yes = state.selected_yes;
                                self.confirm_state = None;
                                self.draw()?;
                                return Ok(selected_yes);
                            }
                            _ => {}
                        }
                    }
                    self.draw()?;
                }
                _ => {}
            }
        }
    }

    /// Tear down the alternate-screen / raw-mode terminal state so an
    /// external program (e.g. `$PAGER`) can own the terminal. Caller must
    /// pair every call with [`Self::resume_terminal`] once the external
    /// program exits, otherwise the TUI will not redraw correctly.
    ///
    /// After leaving the alternate screen, the main screen buffer is
    /// wiped and the cursor parked at `(0, 0)`. Without this, the
    /// operator would see whatever happened to be in the main buffer
    /// before the TUI launched (typically `cargo run` output, or
    /// whatever shell command produced the binary). For console editors
    /// like vim/nvim this is invisible because they immediately enter
    /// their own alt screen, but for GUI editors invoked with a wait
    /// flag (e.g. `code --wait`) the bare main buffer would remain
    /// visible the entire time the editor window is open. Clearing
    /// keeps the handover clean regardless of what the child does.
    pub(super) fn suspend_terminal(&mut self) -> Result<()> {
        let mut stdout = io::stdout();
        execute!(
            stdout,
            LeaveAlternateScreen,
            DisableBracketedPaste,
            Show,
            crossterm::terminal::Clear(ClearType::All),
            MoveTo(0, 0),
        )
        .context("leave alternate screen to suspend fullscreen TUI")?;
        disable_raw_mode().context("disable raw mode while TUI is suspended")?;
        Ok(())
    }

    /// Re-enter the alternate screen and raw mode after a
    /// [`Self::suspend_terminal`] call. The next [`Self::draw`] redraws the
    /// current screen on top of the freshly cleared alternate buffer.
    pub(super) fn resume_terminal(&mut self) -> Result<()> {
        enable_raw_mode().context("re-enable raw mode after TUI suspend")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide)
            .context("re-enter alternate screen after TUI suspend")?;
        self.mouse_capture_enabled = false;
        self.terminal
            .clear()
            .context("clear fullscreen terminal after TUI resume")?;
        Ok(())
    }

    /// Runs an external terminal operation and attempts to restore and redraw
    /// the fullscreen TUI after every suspend, operation, or restore failure.
    pub(super) fn with_suspended_terminal<T>(
        &mut self,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if let Err(error) = self.suspend_terminal() {
            return match self.resume_terminal().and_then(|()| self.draw()) {
                Ok(()) => Err(error),
                Err(restore_error) => Err(error.context(format!(
                    "fullscreen terminal restoration also failed: {restore_error:#}"
                ))),
            };
        }

        let operation_result = operation();
        let restore_result = self.resume_terminal().and_then(|()| self.draw());
        match (operation_result, restore_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(restore_error)) => Err(error.context(format!(
                "fullscreen terminal restoration also failed: {restore_error:#}"
            ))),
        }
    }

    /// Back-aware text input modal used by multi-step flows. Differs from
    /// [`Self::run_text_input_modal_with_placeholder`] only in its return
    /// type: Esc maps to [`StepOutcome::Back`] (the caller pops a step),
    /// Ctrl+C maps to [`StepOutcome::Cancel`] (the caller aborts the flow),
    /// and submission (Enter when `submit_on_enter`, otherwise Ctrl+S) maps
    /// to [`StepOutcome::Ok`] with the textarea contents joined by '\n'.
    ///
    /// Single-modal call sites must keep using
    /// [`Self::run_text_input_modal_with_placeholder`] so their Esc behavior
    /// remains "cancel".
    pub(super) fn run_text_input_modal_with_back(
        &mut self,
        title: &str,
        help: &str,
        initial: &str,
        placeholder: Option<&str>,
        submit_on_enter: bool,
    ) -> Result<super::flow::StepOutcome<String>> {
        let (outcome, _typed) = self.run_text_input_modal_with_back_preserving(
            title,
            help,
            initial,
            placeholder,
            submit_on_enter,
        )?;
        Ok(outcome)
    }

    /// Variant of [`Self::run_text_input_modal_with_back`] that also
    /// surfaces the textarea contents at the moment Esc/Ctrl+C was pressed.
    /// Multi-step flows that want to preserve a draft (e.g. the gate
    /// reason prompt) call this so a subsequent re-entry can re-seed the
    /// textarea with whatever the user had typed before backing out. The
    /// returned text is `None` only when the modal exits with `Ok` (the
    /// submitted text is already inside the `Ok` payload).
    pub(super) fn run_text_input_modal_with_back_preserving(
        &mut self,
        title: &str,
        help: &str,
        initial: &str,
        placeholder: Option<&str>,
        submit_on_enter: bool,
    ) -> Result<(super::flow::StepOutcome<String>, Option<String>)> {
        let mut textarea = TextArea::from(initial.lines().map(str::to_string).collect::<Vec<_>>());
        if textarea.lines().is_empty() {
            textarea.insert_str(initial);
        }
        textarea.set_block(
            Block::default().borders(Borders::ALL).title(Span::styled(
                title.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
        );
        textarea.set_cursor_line_style(Style::default().fg(Color::Yellow));
        if let Some(text) = placeholder {
            textarea.set_placeholder_text(text);
            textarea.set_placeholder_style(Style::default().fg(Color::DarkGray));
        }
        self.text_input_state = Some(TextInputState {
            help: help.to_string(),
            textarea,
            submit_on_enter,
        });
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => self.handle_terminal_resize()?,
                Event::Paste(text) => {
                    if let Some(state) = self.text_input_state.as_mut() {
                        insert_pasted_text(&mut state.textarea, &text);
                    }
                    self.draw()?;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if is_ctrl_c {
                        // Capture the in-flight text before clearing state
                        // so the caller can preserve a draft if it wants.
                        let typed = self
                            .text_input_state
                            .as_ref()
                            .map(|s| join_text_area_lines(&s.textarea));
                        self.text_input_state = None;
                        self.draw()?;
                        return Ok((super::flow::StepOutcome::Cancel, typed));
                    }
                    if matches!(key.code, KeyCode::Esc) {
                        let typed = self
                            .text_input_state
                            .as_ref()
                            .map(|s| join_text_area_lines(&s.textarea));
                        self.text_input_state = None;
                        self.draw()?;
                        return Ok((super::flow::StepOutcome::Back, typed));
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('s'))
                    {
                        let state = self.text_input_state.take().expect("text input state");
                        self.draw()?;
                        return Ok((
                            super::flow::StepOutcome::Ok(join_text_area_lines(&state.textarea)),
                            None,
                        ));
                    }
                    if let Some(state) = self.text_input_state.as_mut() {
                        // Alt+Enter and Shift+Enter both insert a newline
                        // (see the single-modal loop above for rationale).
                        if matches!(key.code, KeyCode::Enter)
                            && (key.modifiers.contains(KeyModifiers::ALT)
                                || key.modifiers.contains(KeyModifiers::SHIFT))
                        {
                            state.textarea.insert_newline();
                            self.draw()?;
                            continue;
                        }
                        if state.submit_on_enter && matches!(key.code, KeyCode::Enter) {
                            let state = self.text_input_state.take().expect("text input state");
                            self.draw()?;
                            return Ok((
                                super::flow::StepOutcome::Ok(join_text_area_lines(&state.textarea)),
                                None,
                            ));
                        }
                        state.textarea.input(key);
                    }
                    self.draw()?;
                }
                _ => {}
            }
        }
    }

    /// Back-aware select modal. Esc → [`StepOutcome::Back`], Ctrl+C →
    /// [`StepOutcome::Cancel`], Enter/space/digit-shortcut →
    /// [`StepOutcome::Ok`] with the selected index. See
    /// [`Self::run_select_modal`] for the single-modal flavor (Esc cancels
    /// directly).
    pub(super) fn run_select_modal_with_back(
        &mut self,
        title: &str,
        items: Vec<String>,
        initial: usize,
    ) -> Result<super::flow::StepOutcome<usize>> {
        if items.is_empty() {
            return Ok(super::flow::StepOutcome::Cancel);
        }
        let selected = initial.min(items.len().saturating_sub(1));
        self.select_modal_state = Some(ActionPickerState {
            title: title.to_string(),
            items,
            selected,
            actionable: true,
        });
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => self.handle_terminal_resize()?,
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if is_ctrl_c {
                        self.select_modal_state = None;
                        self.draw()?;
                        return Ok(super::flow::StepOutcome::Cancel);
                    }
                    match key.code {
                        KeyCode::Esc => {
                            self.select_modal_state = None;
                            self.draw()?;
                            return Ok(super::flow::StepOutcome::Back);
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            let state = self.select_modal_state.take().expect("select modal state");
                            self.draw()?;
                            return Ok(super::flow::StepOutcome::Ok(state.selected));
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                if !state.items.is_empty() {
                                    state.selected = (state.selected + 1) % state.items.len();
                                }
                            }
                            self.draw()?;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                if !state.items.is_empty() {
                                    state.selected = (state.selected + state.items.len() - 1)
                                        % state.items.len();
                                }
                            }
                            self.draw()?;
                        }
                        KeyCode::Home | KeyCode::Char('g') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                state.selected = 0;
                            }
                            self.draw()?;
                        }
                        KeyCode::End | KeyCode::Char('G') => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                state.selected = state.items.len().saturating_sub(1);
                            }
                            self.draw()?;
                        }
                        KeyCode::Char(ch) if ch.is_ascii_digit() => {
                            if let Some(state) = self.select_modal_state.as_mut() {
                                if let Some(index) =
                                    suggested_action_shortcut_index(ch, state.items.len())
                                {
                                    state.selected = index;
                                    let state =
                                        self.select_modal_state.take().expect("select modal state");
                                    self.draw()?;
                                    return Ok(super::flow::StepOutcome::Ok(state.selected));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    /// Back-aware confirm modal. Esc/`n`/`N` → [`StepOutcome::Back`],
    /// Ctrl+C → [`StepOutcome::Cancel`], `y`/`Y` → `Ok(true)`, Enter →
    /// `Ok(selected_yes)`. The single-modal flavor [`Self::run_confirm_modal`]
    /// instead returns `false` on Esc (no Back semantic).
    #[allow(dead_code)]
    pub(super) fn run_confirm_modal_with_back(
        &mut self,
        title: &str,
        message: &str,
    ) -> Result<super::flow::StepOutcome<bool>> {
        self.confirm_state = Some(ConfirmState {
            title: title.to_string(),
            message: message.to_string(),
            selected_yes: false,
        });
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => self.handle_terminal_resize()?,
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if is_ctrl_c {
                        self.confirm_state = None;
                        self.draw()?;
                        return Ok(super::flow::StepOutcome::Cancel);
                    }
                    if let Some(state) = self.confirm_state.as_mut() {
                        match key.code {
                            KeyCode::Left | KeyCode::Char('h') => state.selected_yes = true,
                            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                                state.selected_yes = false
                            }
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                self.confirm_state = None;
                                self.draw()?;
                                return Ok(super::flow::StepOutcome::Ok(true));
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                self.confirm_state = None;
                                self.draw()?;
                                return Ok(super::flow::StepOutcome::Back);
                            }
                            KeyCode::Enter => {
                                let selected_yes = state.selected_yes;
                                self.confirm_state = None;
                                self.draw()?;
                                return Ok(super::flow::StepOutcome::Ok(selected_yes));
                            }
                            _ => {}
                        }
                    }
                    self.draw()?;
                }
                _ => {}
            }
        }
    }

    pub(super) fn run_pager_modal(&mut self, title: &str, body: &str) -> Result<()> {
        self.run_pager_modal_with_diagnostic(title, body, None)
    }

    pub(super) fn run_diagnostic_pager(&mut self, title: &str, body: &str) -> Result<()> {
        self.run_pager_modal_with_diagnostic(title, body, Some(body.to_string()))
    }

    fn run_pager_modal_with_diagnostic(
        &mut self,
        title: &str,
        body: &str,
        diagnostic_body: Option<String>,
    ) -> Result<()> {
        self.pager_state = Some(PagerState {
            title: title.to_string(),
            lines: pager_text_lines(body, self.terminal.size()?),
            offset: 0,
            search_query: None,
            search_matches: Vec::new(),
            search_index: 0,
            pending_g: false,
            status_line: if diagnostic_body.is_some() {
                "c copy  e external pager  / search  j/k move  Esc close".to_string()
            } else {
                "j/k move  Ctrl+D/Ctrl+U page  gg/G top/bottom  / search  n/N next/prev  Esc close"
                    .to_string()
            },
            diagnostic_body,
        });
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => {
                    self.handle_terminal_resize()?;
                    let lines = pager_text_lines(body, self.terminal.size()?);
                    if let Some(state) = self.pager_state.as_mut() {
                        state.lines = lines;
                        state.offset = 0;
                        state.search_query = None;
                        state.search_matches.clear();
                        state.search_index = 0;
                        state.pending_g = false;
                        state.status_line = "Resized. j/k move  / search  Esc close".into();
                    }
                    self.draw()?;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let area_height = self
                        .terminal
                        .size()
                        .map(pager_visible_count_for_area)
                        .unwrap_or(10)
                        .max(1);
                    if handle_pager_key(self, key, area_height)? {
                        self.pager_state = None;
                        self.draw()?;
                        return Ok(());
                    }
                    self.draw()?;
                }
                Event::Mouse(_) => {}
                _ => {}
            }
        }
    }
}

impl Drop for FullscreenPrompt {
    fn drop(&mut self) {
        // Only the outermost prompt on the call stack owns the terminal
        // lifecycle; a nested prompt that returns must NOT tear down raw
        // mode + alt screen, otherwise the parent screen breaks
        // (keystrokes echoed by the cooked TTY, no redraw, Esc visible as
        // `^[`).
        let remaining_depth = fullscreen_prompt_depth_dec();
        if remaining_depth == 0 {
            restore_terminal_state();
        }
        if !self.flush_transcript_on_drop || self.transcript_lines.is_empty() {
            return;
        }
        // Only flush the transcript once the outermost prompt unwinds.
        // Nested prompts hand their transcript ownership back to the
        // outer prompt's stderr restore path naturally.
        if remaining_depth > 0 {
            return;
        }
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        for line in &self.transcript_lines {
            let _ = writeln!(stderr, "{}", line);
        }
    }
}

impl PromptUi for FullscreenPrompt {
    fn picker_available(&self) -> bool {
        true
    }

    fn confirmation_available(&self) -> bool {
        true
    }

    /// Reuse this active prompt to render the run-detail screen for a
    /// freshly-spawned child runner. Critical for the in-TUI gate flow:
    /// constructing a *new* `FullscreenPrompt` here would cause its
    /// `Drop` to call `restore_terminal_state()` while this outer prompt
    /// is still live, which is the original "TUI freeze + ^[ on Esc"
    /// bug. The dispatched-from-shell path (top-level CLI) keeps using
    /// the free `attach_run_viewer` function — there is no parent prompt
    /// to reuse there.
    fn attach_run_viewer_inline(
        &mut self,
        context: &transist::TransistContext,
        run_id: &str,
    ) -> Result<bool> {
        crate::ui::watch::attach_run_viewer_with_prompt(self, context, run_id)?;
        Ok(true)
    }

    fn select_suggested_action(
        &mut self,
        actions: &[SuggestedAction],
        selected_block_title: &str,
    ) -> Result<PickerSelection> {
        if actions.is_empty() {
            return Ok(PickerSelection::default());
        }
        self.picker_state = Some(ActionPickerState {
            title: selected_block_title.to_string(),
            items: numbered_picker_items(actions),
            selected: 0,
            actionable: true,
        });
        // If the picker will render as the inline "Commands" sub-pane of the
        // run-detail layout (because a live progress / run-detail screen frame
        // is active), seed a `RunDetailState` so Tab/Shift+Tab can cycle the
        // surrounding sections (Steps ↔ Details ↔ Actions) and arrow keys can
        // navigate within the focused section. Without this seed Tab would be
        // a no-op for the operator who can clearly see Steps and Details on
        // screen but has no way to focus them.
        let inline_layout = self.run_detail_screen_active || self.current_progress.is_some();
        if inline_layout && self.run_detail_state.is_none() {
            // Seed Steps selection at the current top-level step so the
            // operator can immediately Tab to Steps and inspect surrounding
            // history without first scrolling.
            let selected_step = self
                .current_progress
                .as_ref()
                .and_then(selected_step_index)
                .unwrap_or(0);
            self.run_detail_state = Some(super::run_detail::RunDetailState {
                focused_section: super::run_detail::RunDetailSection::Actions,
                selected_step,
                details_scroll: 0,
                details_cursor: 0,
                selected_action: 0,
                step_pinned_by_user: false,
                pane_cache: None,
            });
        }
        self.draw()?;
        loop {
            match event::read()? {
                Event::Resize(_, _) => self.handle_terminal_resize()?,
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let code = remap_ctrl_c_to_esc(key.code, key.modifiers);

                    // Inline run-detail layout: route Tab/Shift+Tab to the
                    // section switcher and route arrows/j/k to the focused
                    // section. Enter/space/digit only accept when focus is
                    // Actions.
                    if inline_layout {
                        match code {
                            KeyCode::Tab => {
                                self.picker_loop_section_switch(true);
                                self.draw()?;
                                continue;
                            }
                            KeyCode::BackTab => {
                                self.picker_loop_section_switch(false);
                                self.draw()?;
                                continue;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                self.picker_loop_navigate_within_section(1);
                                self.draw()?;
                                continue;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                self.picker_loop_navigate_within_section(-1);
                                self.draw()?;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    let selected = if let Some(state) = self.picker_state.as_mut() {
                        // Only fall through to picker-item selection when
                        // either the picker is the only widget on screen
                        // (centered overlay path), or — in the inline-layout
                        // path — Actions is currently focused. This prevents
                        // Enter/space/digit from accepting an action while
                        // the operator is reading Steps or Details.
                        let actions_focused = if inline_layout {
                            self.run_detail_state
                                .as_ref()
                                .map(|s| {
                                    matches!(
                                        s.focused_section,
                                        super::run_detail::RunDetailSection::Actions
                                    )
                                })
                                .unwrap_or(false)
                        } else {
                            true
                        };
                        match code {
                            KeyCode::Down | KeyCode::Char('j') if !inline_layout => {
                                state.selected = (state.selected + 1) % state.items.len();
                                self.draw()?;
                                continue;
                            }
                            KeyCode::Up | KeyCode::Char('k') if !inline_layout => {
                                state.selected =
                                    (state.selected + state.items.len() - 1) % state.items.len();
                                self.draw()?;
                                continue;
                            }
                            KeyCode::Esc => {
                                self.picker_state = None;
                                self.draw()?;
                                return Ok(PickerSelection::default());
                            }
                            KeyCode::Enter | KeyCode::Char(' ') if actions_focused => {
                                state.selected
                            }
                            KeyCode::Char(ch) if key.modifiers.is_empty() && actions_focused => {
                                if let Some(index) =
                                    suggested_action_shortcut_index(ch, state.items.len())
                                {
                                    index
                                } else {
                                    self.draw()?;
                                    continue;
                                }
                            }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                self.picker_state = None;
                                self.draw()?;
                                return Ok(PickerSelection {
                                    selected_index: None,
                                    dismissed_by_interrupt: true,
                                    rendered_lines: RenderedTerminalBlock::default(),
                                });
                            }
                            _ => {
                                self.draw()?;
                                continue;
                            }
                        }
                    } else {
                        0
                    };
                    self.picker_state = None;
                    self.draw()?;
                    return Ok(PickerSelection {
                        selected_index: Some(selected),
                        dismissed_by_interrupt: false,
                        rendered_lines: RenderedTerminalBlock::default(),
                    });
                }
                _ => {}
            }
        }
    }

    fn prompt_optional_reason(&mut self, prompt: &str) -> Result<PromptInputOutcome> {
        // Single-modal default: no draft keying. Esc cancels and discards
        // whatever was typed. The keyed variant
        // [`Self::prompt_optional_reason_keyed`] is what the gate flow
        // calls so a prior Esc preserves the draft per (run_id, action).
        self.run_text_input_modal(
            prompt,
            "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
            "",
            true,
        )
        .map(|result| match result {
            Some(reason) => PromptInputOutcome::Submitted(reason),
            None => PromptInputOutcome::Canceled,
        })
    }

    fn prompt_optional_reason_keyed(
        &mut self,
        prompt: &str,
        key: &str,
    ) -> Result<PromptInputOutcome> {
        // Multi-step gate flow: seed the textarea with any draft saved
        // under `key` from a prior Esc, and on Esc preserve whatever the
        // user had typed so a re-visit (same run + action kind) re-seeds
        // the textarea. Submit clears the draft so it cannot leak across
        // unrelated future reason prompts.
        let seed = self.reason_drafts.get(key).cloned().unwrap_or_default();
        let help = if seed.is_empty() {
            "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel".to_string()
        } else {
            "Draft preserved  ·  Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel"
                .to_string()
        };
        let (outcome, typed) =
            self.run_text_input_modal_with_back_preserving(prompt, &help, &seed, None, true)?;
        match outcome {
            super::flow::StepOutcome::Ok(reason) => {
                self.reason_drafts.remove(key);
                Ok(PromptInputOutcome::Submitted(reason))
            }
            super::flow::StepOutcome::Back => {
                // Esc: preserve whatever the user had typed (or fall back
                // to the prior seed when the modal couldn't read it).
                let preserved = typed.unwrap_or(seed);
                if preserved.is_empty() {
                    self.reason_drafts.remove(key);
                } else {
                    self.reason_drafts.insert(key.to_string(), preserved);
                }
                Ok(PromptInputOutcome::Canceled)
            }
            super::flow::StepOutcome::Cancel => {
                // Ctrl+C is unconditional abort: drop the draft.
                self.reason_drafts.remove(key);
                Ok(PromptInputOutcome::Canceled)
            }
        }
    }

    fn confirm_suggested_action(&mut self, title: &str, message: &str) -> Result<bool> {
        self.run_confirm_modal(title, message)
    }

    fn confirm_exit_transist(&mut self) -> Result<bool> {
        self.run_confirm_modal(
            "Exit fullscreen UI?",
            "Interrupt received. Exit the run and return to the shell?",
        )
    }

    fn present_paged_text(&mut self, title: &str, body: &str) -> Result<()> {
        self.run_pager_modal(title, body)
    }

    fn set_loading_message(&mut self, message: Option<&str>) -> Result<()> {
        self.loading_state = message.map(|message| LoadingState {
            message: message.to_string(),
        });
        self.draw()
    }

    fn prefers_pager(&self) -> bool {
        true
    }

    fn show_summary_progress(
        &mut self,
        progress: Option<&LiveProgress>,
        fallback_lines: &[String],
    ) -> Result<()> {
        if let Some(progress) = progress {
            self.current_progress = Some(progress.clone());
            self.spinner_tick = 0;
            self.metadata.run_id = Some(progress.state.root_run_id.clone());
            self.metadata.status = Some(progress.state.run_status.clone());
            self.metadata.actionable_run_id = Some(progress.state.actionable_run_id.clone());
            self.metadata.current_step = progress.state.top_level_step_id.clone();
        }
        if !fallback_lines.is_empty() {
            self.push_history_lines(fallback_lines);
        }
        self.draw()
    }

    fn clear_rendered_frame(&mut self, _block: &RenderedTerminalBlock) -> Result<()> {
        self.draw()
    }

    fn emit_plain_lines(&mut self, lines: &[String]) -> Result<()> {
        self.push_history_lines(lines);
        self.draw()
    }
}

pub(crate) fn maybe_default_fullscreen_prompt(
    ui_mode: CliUiMode,
) -> Result<Option<FullscreenPrompt>> {
    if matches!(
        transist::runtime_management_mode()?,
        transist::ManagementMode::Managed
    ) {
        return Ok(None);
    }

    let should_use_tui = crate::ui::should_use_tui(ui_mode);

    if !should_use_tui {
        return Ok(None);
    }

    let available = fullscreen_ui_available();
    if available {
        FullscreenPrompt::new(true).map(Some)
    } else {
        anyhow::bail!("tui mode requires an interactive terminal with redraw support")
    }
}

pub(crate) fn fullscreen_ui_available() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal() && terminal_supports_redraw()
}

pub(crate) fn restore_terminal_state() {
    let _ = disable_raw_mode();
    let mut stderr = io::stderr();
    if stderr.is_terminal() {
        let _ = execute!(
            stderr,
            DisableBracketedPaste,
            crossterm::terminal::Clear(ClearType::All),
            MoveTo(0, 0),
            LeaveAlternateScreen,
            Show
        );
    }
    let mut stdout = io::stdout();
    if stdout.is_terminal() {
        let _ = execute!(
            stdout,
            DisableBracketedPaste,
            crossterm::terminal::Clear(ClearType::All),
            MoveTo(0, 0),
            LeaveAlternateScreen,
            Show
        );
    }
}

pub(crate) fn run_root_dashboard(
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
    interrupt_flag: &Arc<AtomicBool>,
) -> Result<()> {
    screens::run_root_dashboard(repo_root, cwd, explicit_home, interrupt_flag)
}

pub(super) fn confirm_quit_main_screen(
    prompt: &mut FullscreenPrompt,
    message: &str,
) -> Result<bool> {
    prompt.run_confirm_modal("Quit Transist UI?", message)
}

pub(super) fn input_path_from_cwd(cwd: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw.trim());
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

/// Single-modal picker. Returns the selected index or `None` on Esc/Ctrl+C.
/// Multi-step flows should call [`pick_string_item_with_back`] instead so
/// Esc preserves entered data on prior steps.
#[allow(dead_code)]
pub(super) fn pick_string_item(
    prompt: &mut FullscreenPrompt,
    title: &str,
    items: &[String],
) -> Result<Option<usize>> {
    if items.is_empty() {
        return Ok(None);
    }
    let actions = items
        .iter()
        .map(|item| SuggestedAction {
            display_command: item.clone(),
            execution: SuggestedActionExecution::Exit,
        })
        .collect::<Vec<_>>();
    Ok(prompt
        .select_suggested_action(&actions, title)?
        .selected_index)
}

pub(super) fn workflow_ref(summary: &transist::InspectionWorkflowSummary) -> String {
    format!("{}@{}", summary.workflow_id, summary.workflow_version)
}

pub(super) fn launchable_workflow_refs(
    workflows: &[transist::InspectionWorkflowSummary],
) -> Vec<String> {
    workflows
        .iter()
        .filter(|workflow| workflow.compiled_recipe_available && !workflow.is_subflow)
        .map(workflow_ref)
        .collect()
}

/// Single-modal workflow picker. Multi-step flows should call
/// [`pick_launchable_workflow_with_back`] instead.
#[allow(dead_code)]
pub(super) fn pick_launchable_workflow(
    prompt: &mut FullscreenPrompt,
    workflows: &[transist::InspectionWorkflowSummary],
) -> Result<Option<String>> {
    let refs = launchable_workflow_refs(workflows);
    let Some(index) = pick_string_item(prompt, "Select workflow", &refs)? else {
        return Ok(None);
    };
    Ok(refs.get(index).cloned())
}

/// Back-aware variant of [`pick_string_item`] for use inside multi-step
/// flows. The picker is rendered via [`Self::run_select_modal_with_back`]
/// so Esc maps to [`super::flow::StepOutcome::Back`] instead of cancel.
/// `initial` lets the caller seed the highlighted entry — multi-step flows
/// need this so going Back to step 1 re-highlights the previously chosen
/// workflow / action / item.
pub(super) fn pick_string_item_with_back(
    prompt: &mut FullscreenPrompt,
    title: &str,
    items: &[String],
    initial: usize,
) -> Result<super::flow::StepOutcome<usize>> {
    if items.is_empty() {
        return Ok(super::flow::StepOutcome::Cancel);
    }
    prompt.run_select_modal_with_back(title, items.to_vec(), initial)
}

/// Back-aware variant of [`pick_launchable_workflow`]. Returns the chosen
/// workflow ref string (e.g. `"my-workflow@1.0.0"`) inside `Ok` and
/// preserves the previously chosen workflow on re-entry via `initial_ref`.
pub(super) fn pick_launchable_workflow_with_back(
    prompt: &mut FullscreenPrompt,
    workflows: &[transist::InspectionWorkflowSummary],
    initial_ref: Option<&str>,
) -> Result<super::flow::StepOutcome<String>> {
    let refs = launchable_workflow_refs(workflows);
    if refs.is_empty() {
        return Ok(super::flow::StepOutcome::Cancel);
    }
    let initial = initial_ref
        .and_then(|target| refs.iter().position(|candidate| candidate == target))
        .unwrap_or(0);
    match pick_string_item_with_back(prompt, "Select workflow", &refs, initial)? {
        super::flow::StepOutcome::Ok(index) => Ok(super::flow::StepOutcome::Ok(
            refs.get(index).cloned().unwrap_or_default(),
        )),
        super::flow::StepOutcome::Back => Ok(super::flow::StepOutcome::Back),
        super::flow::StepOutcome::Cancel => Ok(super::flow::StepOutcome::Cancel),
    }
}

/// Single-modal optional run-id prompt. Multi-step flows should call
/// [`prompt_optional_run_ref_with_back`] instead.
#[allow(dead_code)]
pub(super) fn prompt_optional_run_ref(
    prompt: &mut FullscreenPrompt,
    title: &str,
) -> Result<Option<Option<String>>> {
    let Some(raw) = prompt.run_text_input_modal(
        title,
        "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel",
        "",
        true,
    )?
    else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Ok(Some(None))
    } else {
        Ok(Some(Some(trimmed.to_string())))
    }
}

/// Back-aware variant of [`prompt_optional_run_ref`]. `seed` pre-fills the
/// textarea so a user revisiting this step (after pressing Esc on a later
/// step) finds their previously typed run id intact. Returns
/// `StepOutcome::Ok(None)` for an empty submission (caller should treat
/// this as "auto-generate a run id").
pub(super) fn prompt_optional_run_ref_with_back(
    prompt: &mut FullscreenPrompt,
    title: &str,
    seed: &str,
    hint: super::flow::StepHint,
) -> Result<super::flow::StepOutcome<Option<String>>> {
    let help = super::flow::back_aware_help("Enter submit", hint);
    match prompt.run_text_input_modal_with_back(title, &help, seed, None, true)? {
        super::flow::StepOutcome::Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(super::flow::StepOutcome::Ok(None))
            } else {
                Ok(super::flow::StepOutcome::Ok(Some(trimmed.to_string())))
            }
        }
        super::flow::StepOutcome::Back => Ok(super::flow::StepOutcome::Back),
        super::flow::StepOutcome::Cancel => Ok(super::flow::StepOutcome::Cancel),
    }
}

/// Single-modal entry to the workflow-inputs flow. Multi-step flows
/// should call [`prompt_workflow_inputs_with_back`] directly so the path
/// seed is owned by the surrounding state machine.
#[allow(dead_code)]
pub(super) fn prompt_workflow_inputs(
    prompt: &mut FullscreenPrompt,
    recipe: &transist::Recipe,
    cwd: &Path,
) -> Result<Option<std::collections::BTreeMap<String, transist::KernelValue>>> {
    let mut path_seed = String::new();
    match prompt_workflow_inputs_with_back(
        prompt,
        recipe,
        cwd,
        &mut path_seed,
        super::flow::StepHint::First,
    )? {
        super::flow::StepOutcome::Ok(map) => Ok(Some(map)),
        super::flow::StepOutcome::Back | super::flow::StepOutcome::Cancel => Ok(None),
    }
}

/// Back-aware variant of [`prompt_workflow_inputs`] for use as a step in a
/// multi-step flow. `path_seed` is shared mutable state owned by the
/// flow's state machine: the path prompt seeds itself from it, writes
/// successful submissions back into it, and the inputs editor sub-step
/// returning Back rewinds the user into the path prompt with that seed
/// preserved. Returning [`super::flow::StepOutcome::Back`] from this
/// function means the user wants to leave step 3 entirely and rewind to
/// step 2; the caller's flow loop is responsible for that transition.
///
/// `hint` controls the footer text on the path prompt and reflects the
/// flow's outer position: a step-1 caller should pass
/// [`super::flow::StepHint::First`] (Esc cancels the flow), while later
/// steps pass [`super::flow::StepHint::Subsequent`] (Esc backs up).
pub(super) fn prompt_workflow_inputs_with_back(
    prompt: &mut FullscreenPrompt,
    recipe: &transist::Recipe,
    cwd: &Path,
    path_seed: &mut String,
    hint: super::flow::StepHint,
) -> Result<super::flow::StepOutcome<std::collections::BTreeMap<String, transist::KernelValue>>> {
    if recipe.inputs.is_empty() {
        return Ok(super::flow::StepOutcome::Ok(
            std::collections::BTreeMap::new(),
        ));
    }

    // The two sub-steps share state via this loop. Outer Back from the
    // editor sub-step (Esc inside the editor) rewinds to the path prompt
    // with the user's typed path preserved; outer Back from the path
    // prompt (Esc inside the empty path prompt) bubbles up to the caller
    // so they can rewind to step 2.
    let mut error_message: Option<String> = None;
    loop {
        // Step 3a: optional inputs file path; loop until canceled, empty,
        // or successful load. The path sub-loop terminates on user
        // submission (advance to 3b) or on Back/Cancel.
        let (file_inputs, file_label) = loop {
            let base_help = match error_message.as_deref() {
                Some(err) => format!(
                    "{} — Full path to a YAML/JSON inputs file. Empty to skip.",
                    err
                ),
                None => "Full path to a YAML/JSON inputs file. Empty to skip.".to_string(),
            };
            let help = format!(
                "{}  {}",
                base_help,
                super::flow::back_aware_help("Enter submit", hint)
            );
            let outcome = prompt.run_text_input_modal_with_back(
                "Inputs file (optional)",
                &help,
                path_seed,
                Some("path/to/inputs.yaml"),
                true,
            )?;
            match outcome {
                super::flow::StepOutcome::Cancel => return Ok(super::flow::StepOutcome::Cancel),
                super::flow::StepOutcome::Back => return Ok(super::flow::StepOutcome::Back),
                super::flow::StepOutcome::Ok(raw) => {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        path_seed.clear();
                        break (std::collections::BTreeMap::new(), None);
                    }
                    let path = input_path_from_cwd(cwd, trimmed);
                    match load_inputs_file(&path) {
                        Ok(map) => {
                            *path_seed = trimmed.to_string();
                            error_message = None;
                            break (map, Some(path.display().to_string()));
                        }
                        Err(err) => {
                            *path_seed = trimmed.to_string();
                            error_message =
                                Some(format!("error loading '{}': {}", path.display(), err));
                            continue;
                        }
                    }
                }
            }
        };

        // Step 3b: build modal state from declared inputs + parsed file.
        let items = build_workflow_input_items(recipe, file_inputs)?;
        let editor = make_workflow_inputs_editor(items.first().and_then(|i| i.editor_value()));
        let state = WorkflowInputsModalState {
            declared: items,
            selected: 0,
            focused_pane: WorkflowInputsPane::List,
            editor,
            status: None,
            file_label,
        };
        prompt.workflow_inputs_state = Some(state);
        prompt.draw()?;

        let result = workflow_inputs_event_loop_with_back(prompt)?;
        prompt.workflow_inputs_state = None;
        prompt.draw()?;
        match result {
            super::flow::StepOutcome::Ok(map) => return Ok(super::flow::StepOutcome::Ok(map)),
            super::flow::StepOutcome::Cancel => return Ok(super::flow::StepOutcome::Cancel),
            super::flow::StepOutcome::Back => {
                // Rewind to the path prompt with the previously typed path
                // (path_seed already updated above). Loop iterates.
                continue;
            }
        }
    }
}

/// Builds input rows from the installed recipe and file overrides, retaining defaults separately.
/// Input comment metadata is removed; undeclared file keys remain visible for runtime validation.
pub(super) fn build_workflow_input_items(
    recipe: &transist::Recipe,
    mut file_inputs: std::collections::BTreeMap<String, transist::KernelValue>,
) -> Result<Vec<WorkflowInputItem>> {
    let input_comments = strip_workflow_input_comments(&mut file_inputs);
    let mut items = Vec::new();
    for (name, spec) in &recipe.inputs {
        let value = file_inputs
            .remove(name)
            .map(|value| serde_json::to_string(&value))
            .transpose()?;
        let default_value = spec
            .default
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let origin = if value.is_some() {
            WorkflowInputOrigin::File
        } else {
            WorkflowInputOrigin::None
        };
        items.push(WorkflowInputItem {
            name: name.clone(),
            value_type: spec.value_type.clone(),
            required: spec.required,
            default_value,
            comment: input_comments.get(name).cloned(),
            value,
            origin,
        });
    }
    for (name, value) in file_inputs {
        items.push(WorkflowInputItem {
            name,
            value_type: "(undeclared)".to_string(),
            required: false,
            default_value: None,
            comment: None,
            value: Some(serde_json::to_string(&value)?),
            origin: WorkflowInputOrigin::File,
        });
    }
    Ok(items)
}

/// Creates the value editor from JSON text without adding a second pane border.
pub(super) fn make_workflow_inputs_editor(initial: Option<&str>) -> TextArea<'static> {
    let lines: Vec<String> = match initial {
        Some(text) if !text.is_empty() => text.lines().map(str::to_string).collect(),
        _ => Vec::new(),
    };
    let mut textarea = TextArea::from(lines);
    // The bordered "Value (JSON)" block is drawn by the overlay renderer so
    // its border style can reflect pane focus. Don't set a block here.
    textarea.set_cursor_line_style(Style::default().fg(Color::Yellow));
    textarea
}

#[allow(dead_code)]
fn workflow_inputs_event_loop(
    prompt: &mut FullscreenPrompt,
) -> Result<Option<std::collections::BTreeMap<String, transist::KernelValue>>> {
    match workflow_inputs_event_loop_with_back(prompt)? {
        super::flow::StepOutcome::Ok(map) => Ok(Some(map)),
        super::flow::StepOutcome::Back | super::flow::StepOutcome::Cancel => Ok(None),
    }
}

fn workflow_inputs_event_loop_with_back(
    prompt: &mut FullscreenPrompt,
) -> Result<super::flow::StepOutcome<std::collections::BTreeMap<String, transist::KernelValue>>> {
    loop {
        let event = event::read()?;
        match event {
            Event::Resize(_, _) => prompt.handle_terminal_resize()?,
            Event::Paste(text) => {
                if let Some(state) = prompt.workflow_inputs_state.as_mut()
                    && matches!(state.focused_pane, WorkflowInputsPane::Editor)
                {
                    let changed = insert_pasted_text(&mut state.editor, &text);
                    if changed {
                        state.status = None;
                    }
                }
                prompt.draw()?;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
                    && key.modifiers.contains(KeyModifiers::CONTROL);
                if is_ctrl_c {
                    return Ok(super::flow::StepOutcome::Cancel);
                }
                if matches!(key.code, KeyCode::Esc) {
                    return Ok(super::flow::StepOutcome::Back);
                }
                let is_ctrl_s = key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('s'));
                if is_ctrl_s {
                    workflow_inputs_commit_editor(prompt);
                    let state = prompt
                        .workflow_inputs_state
                        .as_ref()
                        .expect("workflow inputs state");
                    match validate_and_collect_workflow_inputs(&state.declared) {
                        Ok(map) => return Ok(super::flow::StepOutcome::Ok(map)),
                        Err(err_idx) => {
                            let bad_name = state.declared[err_idx].name.clone();
                            if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                state.status = Some(format!("input '{}' is required", bad_name));
                                state.selected = err_idx;
                                state.focused_pane = WorkflowInputsPane::List;
                                state.editor = make_workflow_inputs_editor(
                                    state.declared[err_idx].editor_value(),
                                );
                            }
                            prompt.draw()?;
                            continue;
                        }
                    }
                }

                let pane = prompt
                    .workflow_inputs_state
                    .as_ref()
                    .map(|s| s.focused_pane)
                    .unwrap_or(WorkflowInputsPane::List);
                match pane {
                    WorkflowInputsPane::List => {
                        match key.code {
                            KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter => {
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    state.focused_pane = WorkflowInputsPane::Editor;
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                workflow_inputs_commit_editor(prompt);
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    if !state.declared.is_empty() {
                                        state.selected =
                                            (state.selected + 1) % state.declared.len();
                                        state.editor = make_workflow_inputs_editor(
                                            state.declared[state.selected].editor_value(),
                                        );
                                        state.status = None;
                                    }
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                workflow_inputs_commit_editor(prompt);
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    if !state.declared.is_empty() {
                                        state.selected = (state.selected + state.declared.len()
                                            - 1)
                                            % state.declared.len();
                                        state.editor = make_workflow_inputs_editor(
                                            state.declared[state.selected].editor_value(),
                                        );
                                        state.status = None;
                                    }
                                }
                            }
                            KeyCode::Home | KeyCode::Char('g') => {
                                workflow_inputs_commit_editor(prompt);
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    state.selected = 0;
                                    if let Some(item) = state.declared.first() {
                                        state.editor =
                                            make_workflow_inputs_editor(item.editor_value());
                                    }
                                    state.status = None;
                                }
                            }
                            KeyCode::End | KeyCode::Char('G') => {
                                workflow_inputs_commit_editor(prompt);
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    if !state.declared.is_empty() {
                                        state.selected = state.declared.len() - 1;
                                        state.editor = make_workflow_inputs_editor(
                                            state.declared[state.selected].editor_value(),
                                        );
                                    }
                                    state.status = None;
                                }
                            }
                            _ => {}
                        }
                        prompt.draw()?;
                    }
                    WorkflowInputsPane::Editor => {
                        if matches!(key.code, KeyCode::Enter)
                            && (key.modifiers.contains(KeyModifiers::SHIFT)
                                || key.modifiers.contains(KeyModifiers::ALT))
                        {
                            if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                state.editor.insert_newline();
                                state.status = None;
                            }
                            prompt.draw()?;
                            continue;
                        }
                        match key.code {
                            KeyCode::Tab | KeyCode::BackTab => {
                                workflow_inputs_commit_editor(prompt);
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    state.focused_pane = WorkflowInputsPane::List;
                                }
                            }
                            KeyCode::Enter if key.modifiers.is_empty() => {
                                workflow_inputs_commit_editor(prompt);
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    state.focused_pane = WorkflowInputsPane::List;
                                }
                            }
                            _ => {
                                if let Some(state) = prompt.workflow_inputs_state.as_mut() {
                                    let changed = state.editor.input(key);
                                    if changed {
                                        state.status = None;
                                    }
                                }
                            }
                        }
                        prompt.draw()?;
                    }
                }
            }
            _ => {}
        }
    }
}

fn workflow_inputs_commit_editor(prompt: &mut FullscreenPrompt) {
    if let Some(state) = prompt.workflow_inputs_state.as_mut() {
        state.commit_editor();
    }
}

pub(super) fn strip_workflow_input_comments(
    file_inputs: &mut std::collections::BTreeMap<String, transist::KernelValue>,
) -> std::collections::BTreeMap<String, String> {
    let mut comments = std::collections::BTreeMap::new();
    let comment_keys: Vec<String> = file_inputs
        .keys()
        .filter(|key| key.starts_with("_comment_"))
        .cloned()
        .collect();
    for key in comment_keys {
        if let Some(comment_value) = file_inputs.remove(&key)
            && let transist::KernelValue::String(comment) = comment_value
        {
            let input_name = key.trim_start_matches("_comment_");
            if !input_name.is_empty() {
                comments.insert(input_name.to_string(), comment);
            }
        }
    }
    comments
}

/// Validate and collect inputs. Returns Err(item_index) for the first required
/// input that is missing; otherwise the merged kernel map.
pub(super) fn validate_and_collect_workflow_inputs(
    items: &[WorkflowInputItem],
) -> Result<std::collections::BTreeMap<String, transist::KernelValue>, usize> {
    let mut map = std::collections::BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        let raw = item.value.as_deref().unwrap_or("").trim();
        if raw.is_empty() {
            if item.required && item.default_value.is_none() {
                return Err(idx);
            }
            continue;
        }
        let parsed = match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(value) => transist::KernelValue::from_json(value),
            Err(_) => transist::KernelValue::String(raw.to_string()),
        };
        map.insert(item.name.clone(), parsed);
    }
    Ok(map)
}

pub(crate) fn run_runs_watch(context: &transist::TransistContext, run_id: &str) -> Result<()> {
    screens::run_runs_watch(context, run_id)
}

pub(super) fn build_header_lines(
    metadata: &ScreenMetadata,
    progress: Option<&LiveProgress>,
) -> Vec<Line<'static>> {
    let bold_label = Style::default().add_modifier(Modifier::BOLD);
    let mut line1 = vec![Span::styled(
        "Transist CLI UI",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(workflow_ref) = metadata.workflow_ref.as_ref() {
        line1.push(Span::raw("  "));
        line1.push(Span::styled("workflow", bold_label));
        if let Some((id, version)) = workflow_ref.rsplit_once('@') {
            let version_style = if transist::is_test_workflow(version) {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            line1.push(Span::raw(format!(" {}@", id)));
            line1.push(Span::styled(version.to_string(), version_style));
        } else {
            line1.push(Span::raw(format!(" {}", workflow_ref)));
        }
        if metadata
            .workflow_ref
            .as_ref()
            .and_then(|r| r.rsplit_once('@'))
            .map(|(_, v)| transist::is_test_workflow(v))
            .unwrap_or(false)
        {
            line1.push(Span::raw("  "));
            line1.push(Span::styled(
                "(assets integrity checks are off for v0 workflows)",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
    }
    let mut line2 = Vec::new();
    if let Some(run_id) = metadata.run_id.as_ref() {
        line2.push(Span::styled("run", bold_label));
        line2.push(Span::raw(format!(" {}", run_id)));
    }
    if let Some(status) = metadata.status.as_ref() {
        if !line2.is_empty() {
            line2.push(Span::raw("  "));
        }
        line2.push(Span::styled("status", bold_label));
        line2.push(Span::styled(
            format!(" {}", status),
            Style::default().fg(status_color(status)),
        ));
    }
    if let Some(actionable_run_id) = metadata.actionable_run_id.as_ref() {
        if metadata.run_id.as_ref() != Some(actionable_run_id) {
            line2.push(Span::raw("  "));
            line2.push(Span::styled("actionable", bold_label));
            line2.push(Span::raw(format!(" {}", actionable_run_id)));
        }
    }
    let mut line3 = Vec::new();
    if let Some(step) = metadata.current_step.as_ref() {
        line3.push(Span::styled("step", bold_label));
        line3.push(Span::raw(format!(" {}", step)));
    }
    if let Some(progress) = progress
        && let Some(path) = progress.state.current_step_path.as_ref()
    {
        if !line3.is_empty() {
            line3.push(Span::raw("  "));
        }
        line3.push(Span::styled("path", bold_label));
        line3.push(Span::raw(format!(" {}", path)));
    }
    vec![Line::from(line1), Line::from(line2), Line::from(line3)]
}

pub(super) fn build_footer_lines(
    progress: Option<&LiveProgress>,
    picker_state: Option<&ActionPickerState>,
    dashboard: bool,
) -> Vec<Line<'static>> {
    let navigation = if dashboard {
        "Navigation: j/k move  Enter select  r refresh  q quit  1-9 shortcuts, arrows for more"
    } else if picker_state.is_some() {
        "Navigation: j/k move  Enter select  Esc close  1-9 shortcuts"
    } else {
        "Navigation: j/k move in lists and pagers  Enter select  Esc back"
    };
    let context = if dashboard {
        "Actions: run workflows, browse and operate runs, manage local preferences, or refresh"
    } else if picker_state.is_some() {
        "Actions: choose a suggested command, inspect, or leave the current state as-is"
    } else if progress.is_some() {
        "Actions: Ctrl+C cancels; hold Option (iTerm) or Shift (Ghostty) to select text"
    } else {
        "Actions: pager opens inspection output; hold Option (iTerm) or Shift (Ghostty) to select"
    };
    vec![style_hint_line(navigation), style_hint_line(context)]
}

pub(super) fn build_progress_detail_lines(
    progress: &LiveProgress,
    selected_index: Option<usize>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let selected_index = selected_index
        .or_else(|| selected_step_index(progress))
        .unwrap_or(0);
    let selected_step = progress
        .state
        .checklist
        .get(selected_index)
        .map(|item| item.step_id.as_str());
    lines.push(format!("Run id: {}", progress.state.root_run_id));
    if progress.state.actionable_run_id != progress.state.root_run_id {
        lines.push(format!(
            "Actionable run: {}",
            progress.state.actionable_run_id
        ));
    }
    if let Some(step_id) = selected_step {
        lines.push(format!("Step: {}", step_id));
    }
    lines.extend(crate::display::progress::format_progress_state_detail_lines(&progress.state));
    let anchored_lines = attached_output_lines(progress, selected_step);
    if !anchored_lines.is_empty() {
        lines.push(String::new());
        lines.extend(anchored_lines);
    }
    let shared_lines = attached_output_lines(progress, None);
    if !shared_lines.is_empty() {
        lines.push(String::new());
        lines.extend(shared_lines);
    }
    if let Some(preview) = progress.render_active_step_lines_plain().last().cloned() {
        if !lines.contains(&preview) {
            lines.push(String::new());
            lines.push(preview);
        }
    }
    lines
}

pub(super) fn attached_output_lines(
    progress: &LiveProgress,
    anchor_step_id: Option<&str>,
) -> Vec<String> {
    progress
        .attached_step_output_blocks
        .iter()
        .filter(|block| block.anchor_step_id.as_deref() == anchor_step_id)
        .flat_map(|block| block.lines.iter().cloned())
        .collect()
}

pub(super) fn selected_step_index(progress: &LiveProgress) -> Option<usize> {
    progress
        .state
        .top_level_step_id
        .as_deref()
        .and_then(|step_id| {
            progress
                .state
                .checklist
                .iter()
                .position(|item| item.step_id == step_id)
        })
        .or_else(|| {
            progress.state.checklist.iter().position(|item| {
                matches!(
                    item.state,
                    crate::display::progress::ChecklistState::Active
                        | crate::display::progress::ChecklistState::Blocked
                        | crate::display::progress::ChecklistState::Failed
                        | crate::display::progress::ChecklistState::Canceled
                )
            })
        })
        .or_else(|| {
            progress.state.checklist.iter().rposition(|item| {
                !matches!(
                    item.state,
                    crate::display::progress::ChecklistState::Pending
                )
            })
        })
        .or_else(|| (!progress.state.checklist.is_empty()).then_some(0))
}

pub(super) fn update_metadata_from_lines(metadata: &mut ScreenMetadata, lines: &[String]) {
    for line in lines {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("Run id: ") {
            metadata.run_id = Some(value.to_string());
        } else if let Some(value) = trimmed.strip_prefix("Workflow: ") {
            metadata.workflow_ref = Some(value.to_string());
        } else if let Some(value) = trimmed.strip_prefix("Run status: ") {
            metadata.status = Some(value.to_string());
        } else if let Some(value) = trimmed.strip_prefix("Status: ") {
            metadata.status = Some(value.to_string());
        } else if let Some(value) = trimmed.strip_prefix("Current step: ") {
            metadata.current_step = Some(value.to_string());
        } else if trimmed.starts_with("------------[run ") && trimmed.ends_with("]------------") {
            metadata.run_id = trimmed
                .strip_prefix("------------[run ")
                .and_then(|value| value.strip_suffix("]------------"))
                .map(str::to_string)
                .or_else(|| metadata.run_id.clone());
        } else if trimmed.ends_with(':') && trimmed.contains('@') && !trimmed.contains(' ') {
            metadata.workflow_ref = Some(trimmed.trim_end_matches(':').to_string());
        } else if let Some((label, value)) = trimmed.split_once(':') {
            if matches!(
                label,
                "Actionable subflow" | "Blocked subflow" | "Failed subflow"
            ) {
                metadata.actionable_run_id = Some(value.trim().to_string());
            }
        }
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn textarea_input_preserves_word_shortcuts_and_repeat_events() {
        use crossterm::event::KeyEvent;

        let mut textarea = TextArea::from(["one two"]);
        textarea.input(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(textarea.cursor(), (0, 4));
        textarea.input(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(textarea.lines(), ["one "]);
        assert!(textarea.input(KeyEvent::new_with_kind(
            KeyCode::Char('X'),
            KeyModifiers::SHIFT,
            KeyEventKind::Repeat,
        )));
        assert_eq!(textarea.lines(), ["one X"]);
        for code in [KeyCode::F(12), KeyCode::CapsLock] {
            assert!(!textarea.input(KeyEvent::new(code, KeyModifiers::NONE)));
        }
        assert_eq!(textarea.lines(), ["one X"]);
        let back_tab =
            ratatui_textarea::Input::from(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(back_tab.key, ratatui_textarea::Key::Tab);
        assert!(back_tab.shift);
    }

    #[test]
    fn textarea_input_edits_selects_and_undoes_with_upgraded_crossterm() {
        use crossterm::event::KeyEvent;

        let mut textarea = TextArea::default();
        for code in [
            KeyCode::Char('é'),
            KeyCode::Char('界'),
            KeyCode::Left,
            KeyCode::Delete,
        ] {
            textarea.input(KeyEvent::new(code, KeyModifiers::NONE));
        }
        assert_eq!(textarea.lines(), ["é"]);
        textarea.input(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(textarea.lines(), ["é界"]);
        textarea.input(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        textarea.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        textarea.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(textarea.lines(), ["x界"]);
        assert!(!textarea.input(KeyEvent::new_with_kind(
            KeyCode::Char('z'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )));
        assert_eq!(textarea.lines(), ["x界"]);
    }

    #[test]
    fn insert_pasted_text_normalizes_crlf_and_preserves_blank_lines() {
        let mut textarea = TextArea::default();

        assert!(insert_pasted_text(
            &mut textarea,
            "alpha\r\n\r\nbeta\rgamma"
        ));

        assert_eq!(join_text_area_lines(&textarea), "alpha\n\nbeta\ngamma");
    }

    #[test]
    fn insert_pasted_text_preserves_leading_newlines_inside_existing_input() {
        let mut textarea = TextArea::default();
        textarea.insert_str("prefix");

        assert!(insert_pasted_text(&mut textarea, "\nnext line\nthird line"));

        assert_eq!(
            join_text_area_lines(&textarea),
            "prefix\nnext line\nthird line"
        );
    }

    #[test]
    fn progress_details_do_not_repeat_header_run_status() {
        let progress = LiveProgress {
            state: crate::display::progress::ProgressState {
                root_run_id: "run-1".to_string(),
                run_status: "running".to_string(),
                actionable_run_id: "run-1".to_string(),
                top_level_step_id: None,
                current_step_path: None,
                current_step_status: None,
                execution: None,
                cycle: None,
                tool_name: None,
                started_at_ms: None,
                finished_at_ms: None,
                error_code: None,
                exit_code: None,
                checklist: Vec::new(),
            },
            preview_stream: None,
            preview_text: None,
            attached_step_output_blocks: Vec::new(),
            footer_lines: Vec::new(),
        };

        let lines = build_progress_detail_lines(&progress, None);

        assert!(lines.iter().any(|line| line == "Run id: run-1"));
        assert!(!lines.iter().any(|line| line.starts_with("Run status: ")));
    }
}
