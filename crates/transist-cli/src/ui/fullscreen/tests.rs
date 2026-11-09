// SPDX-License-Identifier: MPL-2.0
use super::pager::PagerState;
use super::*;
use crate::display::spinner::LiveProgress;
use crate::display::summary::{SuggestedAction, SuggestedActionExecution};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui_textarea::TextArea;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use transist::InspectionGateDecision;

#[test]
fn textarea_accepts_crossterm_events_and_renders_with_ratatui() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use ratatui::style::{Color, Style};
    use ratatui::widgets::{Block, Borders};

    let mut textarea = TextArea::default();
    textarea.set_block(Block::default().borders(Borders::ALL));
    textarea.set_style(Style::default().fg(Color::Yellow));
    assert!(textarea.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
    assert!(!textarea.input(KeyEvent::new_with_kind(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    )));
    assert_eq!(textarea.lines(), ["x"]);

    let mut terminal = Terminal::new(TestBackend::new(10, 3)).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(&textarea, frame.area()))
        .expect("draw textarea");
    let cell = &terminal.backend().buffer()[(1, 1)];
    assert_eq!(cell.symbol(), "x");
    assert_eq!(cell.style().fg, Some(Color::Yellow));
}

#[test]
fn runs_browser_drills_into_direct_children_and_returns_to_parent_level() {
    let mut state = RunBrowserState::from_summaries(
        vec![
            run_summary("root-b", None, 10),
            run_summary("root-a__child-2", Some("root-a"), 15),
            run_summary("root-a__child-1__nested", Some("root-a__child-1"), 40),
            run_summary("root-a__child-1", Some("root-a"), 30),
            run_summary("root-a", None, 20),
        ],
        None,
        PathBuf::new(),
    );

    assert_eq!(
        state
            .current_items()
            .iter()
            .map(|summary| summary.run_id.as_str())
            .collect::<Vec<_>>(),
        vec!["root-a", "root-b"]
    );

    assert!(state.descend());
    assert_eq!(state.current_parent_run_id.as_deref(), Some("root-a"));
    assert_eq!(
        state
            .current_items()
            .iter()
            .map(|summary| summary.run_id.as_str())
            .collect::<Vec<_>>(),
        vec!["root-a__child-1", "root-a__child-2"]
    );

    assert!(state.descend());
    assert_eq!(
        state.current_parent_run_id.as_deref(),
        Some("root-a__child-1")
    );
    assert_eq!(
        state
            .current_items()
            .iter()
            .map(|summary| summary.run_id.as_str())
            .collect::<Vec<_>>(),
        vec!["root-a__child-1__nested"]
    );

    assert!(state.ascend());
    assert_eq!(state.current_parent_run_id.as_deref(), Some("root-a"));
    assert_eq!(state.selected, 0);

    assert!(state.ascend());
    assert_eq!(state.current_parent_run_id, None);
    assert_eq!(state.selected, 0);
}

#[test]
fn revisiting_a_pending_run_restarts_metrics_debounce() {
    let mut state = RunBrowserState::from_summaries(
        vec![
            run_summary("run-a", None, 20),
            run_summary("run-b", None, 10),
        ],
        None,
        PathBuf::from("/tmp/transist-runs-browser"),
    );

    assert!(state.mark_focused_run_pending());
    state.metrics_cache.insert(
        "run-a".to_string(),
        MetricsCacheEntry::Pending {
            since: std::time::Instant::now() - std::time::Duration::from_secs(5),
        },
    );

    state.selected = 1;
    assert!(state.mark_focused_run_pending());

    state.selected = 0;
    assert!(state.mark_focused_run_pending());

    let Some(MetricsCacheEntry::Pending { since }) = state.metrics_cache.get("run-a") else {
        panic!("expected run-a to stay pending");
    };
    assert!(
        since.elapsed() < std::time::Duration::from_secs(1),
        "expected revisiting run-a to restart its debounce"
    );
}

#[test]
fn stable_focus_does_not_restart_pending_metrics_debounce() {
    let mut state = RunBrowserState::from_summaries(
        vec![
            run_summary("run-a", None, 20),
            run_summary("run-b", None, 10),
        ],
        None,
        PathBuf::from("/tmp/transist-runs-browser"),
    );

    assert!(state.mark_focused_run_pending());
    state.metrics_cache.insert(
        "run-a".to_string(),
        MetricsCacheEntry::Pending {
            since: std::time::Instant::now() - std::time::Duration::from_secs(2),
        },
    );

    assert!(!state.mark_focused_run_pending());

    let Some(MetricsCacheEntry::Pending { since }) = state.metrics_cache.get("run-a") else {
        panic!("expected run-a to stay pending");
    };
    assert!(
        since.elapsed() >= std::time::Duration::from_secs(2),
        "expected unchanged focus to preserve the original debounce timestamp"
    );
}

#[test]
fn run_browser_does_not_show_loading_metrics_before_navigation() {
    let lines = build_run_browser_metrics_lines(&BTreeMap::new(), "run-a");
    assert!(
        lines.is_empty(),
        "expected no metrics placeholder before user starts navigating"
    );
}

#[test]
fn dashboard_picker_overlay_renders_when_active() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let dashboard_state = DashboardState {
        header_lines: vec!["Home: /tmp/.transist".to_string()],
        setup_lines: vec!["Setup".to_string()],
        setup_error_line_range: None,
        inventory_lines: vec!["Inventory".to_string()],
        action_items: vec![DashboardActionItem {
            action: DashboardActionId::LaunchWorkflow,
            label: "4. Run workflow".to_string(),
        }],
        selected_action: 0,
        focused_section: DashboardSection::Actions,
        setup_scroll: 0,
        inventory_scroll: 0,
    };
    let picker_state = ActionPickerState {
        title: "Select workflow".to_string(),
        items: vec!["1. demo@v1".to_string()],
        selected: 0,
        actionable: true,
    };

    terminal
        .draw(|frame| {
            components::draw_base_frame(
                frame,
                &ScreenMetadata::default(),
                Some(&dashboard_state),
                None,
                &[],
                Some(&picker_state),
            );
            components::draw_picker_overlay(frame, &picker_state);
        })
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(rendered.contains("Select workflow"));
    assert!(rendered.contains("demo@v1"));
}

#[test]
fn dashboard_header_running_and_blocked_counts_use_accents() {
    use ratatui::style::Color;

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let dashboard_state = DashboardState {
        header_lines: vec![
            "Home: /tmp/.transist".to_string(),
            "Source: TRANSIST_HOME".to_string(),
            "Initialized: yes".to_string(),
            "Running: 2   Blocked: 1".to_string(),
        ],
        setup_lines: vec!["Setup".to_string()],
        setup_error_line_range: None,
        inventory_lines: vec!["Inventory".to_string()],
        action_items: vec![DashboardActionItem {
            action: DashboardActionId::Refresh,
            label: "1. Refresh dashboard".to_string(),
        }],
        selected_action: 0,
        focused_section: DashboardSection::Actions,
        setup_scroll: 0,
        inventory_scroll: 0,
    };

    terminal
        .draw(|frame| {
            components::draw_base_frame(
                frame,
                &ScreenMetadata::default(),
                Some(&dashboard_state),
                None,
                &[],
                None,
            )
        })
        .expect("draw");

    let buf = terminal.backend().buffer();
    let mut found_running_green = false;
    let mut found_blocked_yellow = false;
    for y in 0..6 {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "R" && cell.style().fg == Some(Color::Green) {
                found_running_green = true;
            }
            if cell.symbol() == "B" && cell.style().fg == Some(Color::Yellow) {
                found_blocked_yellow = true;
            }
        }
    }

    assert!(
        found_running_green,
        "expected 'Running: N' to be green in dashboard header"
    );
    assert!(
        found_blocked_yellow,
        "expected 'Blocked: N' to be yellow in dashboard header"
    );
}

#[test]
fn run_browser_status_prefix_is_colored_and_run_id_is_bold() {
    use ratatui::style::{Color, Modifier};

    let backend = TestBackend::new(140, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let state = RunBrowserState::from_summaries(
        vec![
            transist::InspectionRunSummary {
                run_id: "RUN-ok".to_string(),
                workflow_id: "demo".to_string(),
                workflow_version: "v1".to_string(),
                status: "success".to_string(),
                created_at_ms: 2,
                updated_at_ms: 3,
                started_at_ms: None,
                finished_at_ms: None,
                current_step_id: None,
                has_blocked_gates: false,
                has_artifacts: false,
                has_events: false,
                artifact_count: 0,
                evidence_count: 0,
                root_run_id: "RUN-ok".to_string(),
                parent_run_id: None,
            },
            transist::InspectionRunSummary {
                run_id: "RUN-impl-2".to_string(),
                workflow_id: "demo".to_string(),
                workflow_version: "v1".to_string(),
                status: "blocked".to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                started_at_ms: None,
                finished_at_ms: None,
                current_step_id: None,
                has_blocked_gates: true,
                has_artifacts: false,
                has_events: false,
                artifact_count: 0,
                evidence_count: 0,
                root_run_id: "RUN-impl-2".to_string(),
                parent_run_id: None,
            },
        ],
        None,
        PathBuf::new(),
    );

    terminal
        .draw(|frame| components::draw_run_browser_frame(frame, &state))
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(rendered.contains("[blocked ] RUN-impl-2"));

    let buf = terminal.backend().buffer();
    let mut found_prefix_yellow = false;
    let mut found_run_id_bold = false;
    for y in 0..12 {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "[" && cell.style().fg == Some(Color::Yellow) {
                found_prefix_yellow = true;
            }
            if cell.symbol() == "R"
                && cell.style().add_modifier.contains(Modifier::BOLD)
                && cell.style().fg != Some(Color::Yellow)
            {
                found_run_id_bold = true;
            }
        }
    }

    assert!(
        found_prefix_yellow,
        "expected blocked status prefix to be yellow"
    );
    assert!(found_run_id_bold, "expected run id to be bold");
}

#[test]
fn modal_key_footers_render_visible_hints() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let pager_state = PagerState {
        title: "Run details".to_string(),
        lines: vec!["line 1".to_string(), "line 2".to_string()],
        offset: 0,
        search_query: None,
        search_matches: Vec::new(),
        search_index: 0,
        pending_g: false,
        status_line:
            "j/k move  Ctrl+D/Ctrl+U page  gg/G top/bottom  / search  n/N next/prev  Esc close"
                .to_string(),
        diagnostic_body: None,
    };
    let text_input_state = TextInputState {
        help: "One KEY=VALUE per line. Ctrl+S submit  Esc cancel".to_string(),
        textarea: TextArea::from(vec!["repo_path=./demo".to_string()]),
        submit_on_enter: false,
    };
    let confirm_state = ConfirmState {
        title: "Quit Transist UI?".to_string(),
        message: "Exit the Transist CLI UI and return to the shell?".to_string(),
        selected_yes: false,
    };

    terminal
        .draw(|frame| {
            components::draw_pager_overlay(frame, &pager_state);
            components::draw_text_input_overlay(frame, &text_input_state);
            components::draw_confirm_overlay(frame, &confirm_state);
        })
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(rendered.contains("Ctrl+D/Ctrl+U page"));
    assert!(rendered.contains("Ctrl+S submit"));
    assert!(rendered.contains("Enter confirm"));
    assert!(rendered.contains("Keys"));
}

#[test]
fn dashboard_action_items_are_the_operator_console_actions() {
    let items = dashboard_action_items_with_problem_count(0);

    assert_eq!(items.len(), 6);
    assert_eq!(items[0].action, DashboardActionId::LaunchWorkflow);
    assert_eq!(items[0].label, "1. Run workflow");
    assert_eq!(items[1].action, DashboardActionId::ShowRunsList);
    assert_eq!(items[1].label, "2. Browse runs");
    assert_eq!(items[2].action, DashboardActionId::Problems);
    assert_eq!(items[2].label, "3. Problems (0)");
    assert_eq!(items[3].label, "4. Preferences");
    assert_eq!(items[4].label, "5. Refresh");
    assert_eq!(items[5].label, "6. Quit");
}

#[test]
fn dashboard_footer_describes_operator_actions() {
    let rendered = build_footer_lines(None, None, true)
        .into_iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("run workflows, browse and operate runs"));
    assert!(!rendered.contains("manage workflows"));
}

#[test]
fn legacy_home_without_plugins_dir_still_counts_as_initialized() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("config.yaml"), "version: v1\n").expect("config");
    std::fs::write(
        tmp.path().join("toolchains.yaml"),
        "schema: transist.toolchains.v1\ntoolchains: {}\n",
    )
    .expect("toolchains");
    std::fs::create_dir_all(tmp.path().join("registry/recipes")).expect("registry recipes");
    std::fs::create_dir_all(tmp.path().join("runs")).expect("runs");

    assert!(transist_home_is_initialized(tmp.path()));
    assert!(!tmp.path().join("plugins").exists());
}

#[test]
fn dashboard_error_status_line_renders_in_setup_only() {
    use ratatui::style::Color;

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let dashboard_state = DashboardState {
        header_lines: vec!["Home: /tmp/.transist".to_string()],
        setup_lines: vec![
            "Setup".to_string(),
            String::new(),
            "Status".to_string(),
            "  Error: plugin 'openspec' is not installed".to_string(),
        ],
        setup_error_line_range: Some(3..4),
        inventory_lines: vec!["Inventory".to_string()],
        action_items: vec![DashboardActionItem {
            action: DashboardActionId::Problems,
            label: "3. Problems".to_string(),
        }],
        selected_action: 0,
        focused_section: DashboardSection::Setup,
        setup_scroll: 0,
        inventory_scroll: 0,
    };

    terminal
        .draw(|frame| {
            components::draw_base_frame(
                frame,
                &ScreenMetadata::default(),
                Some(&dashboard_state),
                None,
                &[],
                None,
            )
        })
        .expect("draw");

    let buf = terminal.backend().buffer();
    let mut found_red_e = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "E" && cell.style().fg == Some(Color::Red) {
                found_red_e = true;
                break;
            }
        }
        if found_red_e {
            break;
        }
    }

    assert!(
        found_red_e,
        "expected the error status line to render in red"
    );

    let rendered = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let header = rendered.lines().take(6).collect::<Vec<_>>().join("\n");
    assert!(
        !header.contains("Status: Error:"),
        "expected the Transist header not to show the operation status; rendered={rendered}"
    );
    assert!(
        rendered
            .lines()
            .skip(6)
            .any(|line| line.contains("Error: plugin 'openspec' is not")),
        "expected the operation status to remain in the Setup section; rendered={rendered}"
    );
    assert!(
        rendered.lines().skip(6).any(|line| line.contains("Status")),
        "expected the Setup section to retain the status heading; rendered={rendered}"
    );
}

#[test]
fn text_input_modal_renders_placeholder_in_dark_gray_when_textarea_is_empty() {
    // Path-shaped inputs show a greyed placeholder while the field is empty. Assert that
    // the placeholder text reaches the rendered buffer AND that the cell where
    // it lands is styled DarkGray (the placeholder color), not the default
    // foreground that real user input uses.
    use ratatui::style::Color;

    let mut textarea = TextArea::default();
    textarea.set_block(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .title(ratatui::text::Span::styled(
                "Enter file path",
                ratatui::style::Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )),
    );
    textarea.set_placeholder_text("path/to/file");
    textarea.set_placeholder_style(ratatui::style::Style::default().fg(Color::DarkGray));
    let state = TextInputState {
        help: "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel".to_string(),
        textarea,
        submit_on_enter: true,
    };

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| components::draw_text_input_overlay(frame, &state))
        .expect("draw");

    let buf = terminal.backend().buffer();
    let rendered = buffer_to_string(buf);
    assert!(
        rendered.contains("path/to/file"),
        "placeholder text must be rendered; got=\n{rendered}"
    );
    assert!(
        rendered.contains("Enter file path"),
        "modal title must be rendered; got=\n{rendered}"
    );

    // Locate any cell carrying the leading 'p' of the placeholder and check
    // its foreground style. ratatui-textarea writes the placeholder one character
    // per cell with the configured placeholder style.
    let area = buf.area;
    let mut found_dark_gray_p = false;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "p" && cell.style().fg == Some(Color::DarkGray) {
                found_dark_gray_p = true;
                break;
            }
        }
        if found_dark_gray_p {
            break;
        }
    }
    assert!(
        found_dark_gray_p,
        "expected the placeholder 'p' cell to be styled DarkGray (placeholder); got=\n{rendered}"
    );

    // Title cells should be Cyan + Bold.
    let mut found_cyan_bold_e = false;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "E"
                && cell.style().fg == Some(Color::Cyan)
                && cell
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
            {
                found_cyan_bold_e = true;
                break;
            }
        }
        if found_cyan_bold_e {
            break;
        }
    }
    assert!(
        found_cyan_bold_e,
        "expected modal title leading 'E' to be styled Cyan+Bold"
    );

    // Footer should now route through style_hint_line so 'Enter' and 'Esc'
    // come out yellow + bold.
    let mut yellow_bold_tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_is_token = false;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let is_token = cell.style().fg == Some(Color::Yellow)
                && cell
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD);
            if is_token {
                current.push_str(cell.symbol());
                current_is_token = true;
            } else if current_is_token {
                if !current.is_empty() {
                    yellow_bold_tokens.push(std::mem::take(&mut current));
                }
                current_is_token = false;
            }
        }
        if current_is_token && !current.is_empty() {
            yellow_bold_tokens.push(std::mem::take(&mut current));
            current_is_token = false;
        }
    }
    assert!(
        yellow_bold_tokens.iter().any(|t| t.contains("Enter")),
        "footer should highlight 'Enter' yellow+bold; tokens={yellow_bold_tokens:?}"
    );
    assert!(
        yellow_bold_tokens.iter().any(|t| t.contains("Esc")),
        "footer should highlight 'Esc' yellow+bold; tokens={yellow_bold_tokens:?}"
    );
}

#[test]
fn run_detail_command_items_use_full_cli_commands() {
    let actions = vec![
        SuggestedAction::new(SuggestedActionExecution::RunsCleanup {
            run_id: "demo-run".to_string(),
        }),
        SuggestedAction::new(SuggestedActionExecution::InspectCleanupResult {
            run_id: "demo-run".to_string(),
        }),
        SuggestedAction::new(SuggestedActionExecution::InspectFailedStepDetails {
            run_id: "demo-run".to_string(),
            step_id: "verify".to_string(),
        }),
        SuggestedAction::new(SuggestedActionExecution::InspectFailedStepEvents {
            run_id: "demo-run".to_string(),
            step_id: "verify".to_string(),
            limit: 80,
        }),
    ];

    assert_eq!(
        numbered_run_detail_command_items(&actions),
        vec![
            "1. transist runs cleanup demo-run".to_string(),
            "2. view last cleanup result".to_string(),
            "3. transist runs show demo-run --failed".to_string(),
            "4. transist runs tail demo-run --limit 80".to_string(),
        ]
    );
}

#[test]
fn selected_step_details_show_only_the_selected_step_output() {
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("step-b".to_string()),
            current_step_path: Some("step-b.generate".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(4),
            cycle: Some(2),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(1000),
            finished_at_ms: Some(5000),
            error_code: None,
            exit_code: Some(1),
            checklist: vec![
                crate::display::progress::ProgressChecklistItem {
                    step_id: "step-a".to_string(),
                    state: crate::display::progress::ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                crate::display::progress::ProgressChecklistItem {
                    step_id: "step-b".to_string(),
                    state: crate::display::progress::ChecklistState::Failed,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: vec![
            crate::display::output::AttachedStepOutputBlock {
                lines: vec![
                    "Workflow files:".to_string(),
                    "  '/tmp/step-a.txt'".to_string(),
                ],
                anchor_step_id: Some("step-a".to_string()),
            },
            crate::display::output::AttachedStepOutputBlock {
                lines: vec!["Evidence:".to_string(), "  '/tmp/step-b.json'".to_string()],
                anchor_step_id: Some("step-b".to_string()),
            },
        ],
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(None, None, None, &progress, Some(0), 0);

    assert!(lines.iter().any(|line| line.contains("step-a")));
    assert!(lines.iter().any(|line| line.contains("/tmp/step-a.txt")));
    assert!(!lines.iter().any(|line| line.contains("step-b.generate")));
    assert!(!lines.iter().any(|line| line.contains("/tmp/step-b.json")));
}

#[test]
fn selected_finished_step_details_render_structured_summary() {
    let step = transist::InspectionStep {
        step_id: "review_plan".to_string(),
        step_kind: "control".to_string(),
        status: "success".to_string(),
        execution: Some(3),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: Some(
                serde_json::from_value(serde_json::json!({
                    "source": "step",
                    "workspace_root": "/tmp/workspace",
                    "workspace_access": {
                        "mode": "read_only",
                    },
                    "read_allow_paths": [],
                    "write_allow_paths": [],
                }))
                .expect("step filesystem policy"),
            ),
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("success".to_string()),
            exit_code: Some(0),
            error_code: None,
            error_message: None,
            stderr_tail: None,
            child_run_id: None,
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 1,
            evidence_count: 1,
            artifact_refs: vec![transist::InspectionStepBlobRef {
                step_id: "review_plan".to_string(),
                execution: 3,
                relative_path: "000_stdout.txt".to_string(),
                name: "000_stdout.txt".to_string(),
                bytes: 76,
            }],
            evidence_refs: vec![transist::InspectionStepBlobRef {
                step_id: "review_plan".to_string(),
                execution: 3,
                relative_path: "transist_runtime_invoke_evidence.json".to_string(),
                name: "transist_runtime_invoke_evidence.json".to_string(),
                bytes: 1479,
            }],
            workflow_files: vec![transist::InspectionWorkflowFileRef {
                step_id: "review_plan".to_string(),
                field_name: "plan_file".to_string(),
                locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
                relative_path: "plan.md".to_string(),
                local_path: Some(PathBuf::from("/tmp/plan.md")),
                name: "plan.md".to_string(),
                bytes: 256,
            }],
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: None,
            has_blocked_gates: false,
            has_artifacts: true,
            has_events: false,
            artifact_count: 1,
            evidence_count: 1,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: Some(
            serde_json::from_value(serde_json::json!({
                "source": "global",
                "workspace_root": "/tmp/workspace",
                "workspace_access": {
                    "mode": "read_only",
                },
                "read_allow_paths": [],
                "write_allow_paths": [],
            }))
            .expect("global filesystem policy"),
        ),
        workflow_filesystem_policy: Some(
            serde_json::from_value(serde_json::json!({
                "source": "workflow",
                "workspace_root": "/tmp/workspace",
                "workspace_access": {
                    "mode": "read_only",
                },
                "read_allow_paths": ["/etc/gitconfig"],
                "write_allow_paths": ["~/.cache/transist"],
            }))
            .expect("workflow filesystem policy"),
        ),
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: vec![transist::InspectionGate {
            gate_id: "plan-review".to_string(),
            status: "approved".to_string(),
            step_id: "review_plan".to_string(),
            created_at_ms: None,
            decision: Some(InspectionGateDecision {
                review_request: None,
                state: "request_changes".to_string(),
                actor: Some("reviewer@example.com".to_string()),
                reason: Some("tighten the wording".to_string()),
                decided_at_ms: None,
            }),
            decision_history: Vec::new(),
            request_summary: BTreeMap::new(),
        }],
        steps: vec![step],
        artifact_count: 1,
        evidence_count: 1,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("next_step".to_string()),
            current_step_path: Some("next_step".to_string()),
            current_step_status: Some("pending".to_string()),
            execution: None,
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                crate::display::progress::ProgressChecklistItem {
                    step_id: "review_plan".to_string(),
                    state: crate::display::progress::ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                crate::display::progress::ProgressChecklistItem {
                    step_id: "next_step".to_string(),
                    state: crate::display::progress::ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(
        Some(Path::new("/tmp/.transist")),
        Some(&detail),
        None,
        &progress,
        Some(0),
        0,
    );

    assert!(lines.contains(&"Execution:".to_string()));
    assert!(lines.contains(&"  Step status: success".to_string()));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Filesystem policy: source=step")),
        "lines: {lines:#?}"
    );
    assert!(lines.contains(&"  Workspace root: /tmp/workspace".to_string()));
    assert!(lines.contains(&"  Execution: 3".to_string()));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("plan_file: '/tmp/plan.md'"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("/tmp/.transist/runs/run_demo-run/artifacts/review_plan/execution_003/artifacts/000_stdout.txt"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("/tmp/.transist/runs/run_demo-run/artifacts/review_plan/execution_003/evidence/transist_runtime_invoke_evidence.json"))
    );
    assert!(lines.contains(&"Human gate: plan-review".to_string()));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("transist runs gate approve demo-run --actor"))
    );
    assert!(
        lines
            .iter()
            .any(|line| { line.contains("transist runs gate request-changes demo-run --actor") })
    );
}

#[test]
fn selected_finished_step_details_hide_empty_sections() {
    let step = transist::InspectionStep {
        step_id: "validate_changes".to_string(),
        step_kind: "invoke".to_string(),
        status: "success".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("success".to_string()),
            exit_code: Some(0),
            error_code: None,
            error_message: None,
            stderr_tail: None,
            child_run_id: None,
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "success".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: None,
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "success".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("next_step".to_string()),
            current_step_path: Some("next_step".to_string()),
            current_step_status: Some("pending".to_string()),
            execution: None,
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                crate::display::progress::ProgressChecklistItem {
                    step_id: "validate_changes".to_string(),
                    state: crate::display::progress::ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                crate::display::progress::ProgressChecklistItem {
                    step_id: "next_step".to_string(),
                    state: crate::display::progress::ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(None, Some(&detail), None, &progress, Some(0), 0);

    assert!(!lines.iter().any(|line| line == "Workflow files:"));
    assert!(!lines.iter().any(|line| line == "Artifacts:"));
    assert!(!lines.iter().any(|line| line == "Evidence:"));
}

#[test]
fn selected_failed_step_details_render_failure_summary_before_execution() {
    let failed_step = transist::InspectionStep {
        step_id: "validate_plan".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(2),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: true,
            invoke_id: Some("invoke_validate".to_string()),
            adapter_kind: Some("process".to_string()),
            plugin_id: Some("shell".to_string()),
            action: Some("exec".to_string()),
            input_object_keys: vec!["args".to_string(), "command".to_string()],
            metadata_object_keys: vec!["plugin_id".to_string(), "timeout_ms".to_string()],
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: Some("invoke_validate".to_string()),
            status: Some("failed".to_string()),
            exit_code: Some(2),
            error_code: Some("tool_execution_error".to_string()),
            error_message: Some("child flow failed\nstderr follows".to_string()),
            stderr_tail: None,
            child_run_id: Some("demo-run__child".to_string()),
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: vec![transist::InspectionWorkflowFileRef {
                step_id: "validate_plan".to_string(),
                field_name: "report_file".to_string(),
                locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
                relative_path: "report.md".to_string(),
                local_path: Some(PathBuf::from("/tmp/report.md")),
                name: "report.md".to_string(),
                bytes: 32,
            }],
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let child_detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run__child".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("verify_revision".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: Some("demo-run".to_string()),
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![transist::InspectionStep {
            step_id: "verify_revision".to_string(),
            step_kind: "invoke".to_string(),
            status: "success".to_string(),
            execution: Some(1),
            started_at_ms: None,
            finished_at_ms: None,
            input_summary: transist::InspectionStepInputSummary {
                request_present: false,
                invoke_id: None,
                adapter_kind: None,
                plugin_id: None,
                action: None,
                input_object_keys: Vec::new(),
                metadata_object_keys: Vec::new(),
                filesystem_policy: None,
            },
            output_summary: transist::InspectionStepOutputSummary {
                result_present: true,
                invoke_id: None,
                status: Some("failed".to_string()),
                exit_code: Some(1),
                error_code: Some("validation_error".to_string()),
                error_message: Some("child flow failed".to_string()),
                stderr_tail: Some("missing marker".to_string()),
                child_run_id: None,
                child_failure: None,
                output_object_keys: Vec::new(),
                artifact_count: 0,
                evidence_count: 0,
                artifact_refs: Vec::new(),
                evidence_refs: Vec::new(),
                workflow_files: Vec::new(),
            },
            is_current_active: false,
            latest_output: None,
            executions: Vec::new(),
        }],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("validate_plan".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![failed_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("validate_plan".to_string()),
            current_step_path: Some("validate_plan".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(2),
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "validate_plan".to_string(),
                state: crate::display::progress::ChecklistState::Failed,
                duration_seconds: Some(1),
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let contained = BTreeMap::from([("demo-run__child".to_string(), child_detail)]);

    let lines = build_selected_step_detail_lines(
        None,
        Some(&detail),
        Some(&contained),
        &progress,
        Some(0),
        0,
    );

    let failure_idx = lines
        .iter()
        .position(|line| line == "── Failure ──")
        .expect("failure section");
    let execution_idx = lines
        .iter()
        .position(|line| line == "Execution:")
        .expect("execution section");
    let workflow_idx = lines
        .iter()
        .position(|line| line == "Workflow files:")
        .expect("workflow files section");
    assert!(
        failure_idx < execution_idx && execution_idx < workflow_idx,
        "lines={lines:?}"
    );
    assert!(lines.contains(&"  Action: exec".to_string()));
    assert!(lines.contains(&"Failed child step: verify_revision".to_string()));
    assert!(lines.contains(&"Error: child flow failed".to_string()));
    assert!(lines.contains(&"Stderr: missing marker".to_string()));
}

#[test]
fn run_detail_frame_surfaces_child_failure_on_parent_failed_subflow_step() {
    let failed_step = transist::InspectionStep {
        step_id: "validate_plan".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(2),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: true,
            invoke_id: Some("invoke_validate".to_string()),
            adapter_kind: Some("process".to_string()),
            plugin_id: Some("shell".to_string()),
            action: Some("exec".to_string()),
            input_object_keys: vec!["args".to_string(), "command".to_string()],
            metadata_object_keys: vec!["plugin_id".to_string(), "timeout_ms".to_string()],
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: Some("invoke_validate".to_string()),
            status: Some("failed".to_string()),
            exit_code: Some(2),
            error_code: Some("tool_execution_error".to_string()),
            error_message: Some("child flow failed".to_string()),
            stderr_tail: None,
            child_run_id: Some("demo-run__child".to_string()),
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let child_detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run__child".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("verify_revision".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: Some("demo-run".to_string()),
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![transist::InspectionStep {
            step_id: "verify_revision".to_string(),
            step_kind: "invoke".to_string(),
            status: "success".to_string(),
            execution: Some(1),
            started_at_ms: None,
            finished_at_ms: None,
            input_summary: transist::InspectionStepInputSummary {
                request_present: false,
                invoke_id: None,
                adapter_kind: None,
                plugin_id: None,
                action: None,
                input_object_keys: Vec::new(),
                metadata_object_keys: Vec::new(),
                filesystem_policy: None,
            },
            output_summary: transist::InspectionStepOutputSummary {
                result_present: true,
                invoke_id: None,
                status: Some("failed".to_string()),
                exit_code: Some(1),
                error_code: Some("validation_error".to_string()),
                error_message: Some("child flow failed".to_string()),
                stderr_tail: Some("missing marker".to_string()),
                child_run_id: None,
                child_failure: None,
                output_object_keys: Vec::new(),
                artifact_count: 0,
                evidence_count: 0,
                artifact_refs: Vec::new(),
                evidence_refs: Vec::new(),
                workflow_files: Vec::new(),
            },
            is_current_active: false,
            latest_output: None,
            executions: Vec::new(),
        }],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("validate_plan".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: Some("demo-run__child".to_string()),
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![failed_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run__child".to_string(),
            top_level_step_id: Some("validate_plan".to_string()),
            current_step_path: Some("validate_plan -> verify_revision".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(2),
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "validate_plan".to_string(),
                state: crate::display::progress::ChecklistState::Failed,
                duration_seconds: Some(1),
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let contained = BTreeMap::from([("demo-run__child".to_string(), child_detail)]);
    let state = super::run_detail::RunDetailState {
        focused_section: super::run_detail::RunDetailSection::Details,
        selected_step: 0,
        details_scroll: 0,
        details_cursor: 0,
        selected_action: 0,
        step_pinned_by_user: true,
        pane_cache: None,
    };

    let backend = TestBackend::new(180, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            components::draw_run_detail_frame(
                frame,
                &ScreenMetadata::default(),
                None,
                Some(&progress),
                Some(&detail),
                Some(&contained),
                0,
                Some(&state),
                None,
            );
        })
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(
        rendered.contains("Failed child step: verify_revision"),
        "rendered={rendered}"
    );
    assert!(
        rendered.contains("Error: child flow failed"),
        "rendered={rendered}"
    );
    assert!(
        rendered.contains("Stderr: missing marker"),
        "rendered={rendered}"
    );
}

#[test]
fn parent_failed_subflow_step_surfaces_deepest_descendant_failure() {
    let root_failed_step = transist::InspectionStep {
        step_id: "plan_loop".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("failed".to_string()),
            exit_code: None,
            error_code: Some("child_flow_failed".to_string()),
            error_message: Some("child flow failed".to_string()),
            stderr_tail: None,
            child_run_id: Some("demo-run__plan_loop".to_string()),
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let child_failed_step = transist::InspectionStep {
        step_id: "plan_loop".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("failed".to_string()),
            exit_code: None,
            error_code: Some("child_flow_failed".to_string()),
            error_message: Some("child flow failed".to_string()),
            stderr_tail: None,
            child_run_id: Some("demo-run__plan_loop__plan_loop".to_string()),
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let grandchild_failed_step = transist::InspectionStep {
        step_id: "review_plan".to_string(),
        step_kind: "invoke".to_string(),
        status: "success".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: Some("process".to_string()),
            plugin_id: Some("shell".to_string()),
            action: Some("exec".to_string()),
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("failed".to_string()),
            exit_code: Some(1),
            error_code: Some("validation_error".to_string()),
            error_message: Some("child flow failed".to_string()),
            stderr_tail: Some("missing marker".to_string()),
            child_run_id: None,
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let grandchild_detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run__plan_loop__plan_loop".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("review_plan".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: Some("demo-run__plan_loop".to_string()),
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![grandchild_failed_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let child_detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run__plan_loop".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: Some("demo-run".to_string()),
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![child_failed_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: Some("demo-run__plan_loop".to_string()),
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![root_failed_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> plan_loop -> review_plan".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(1),
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Failed,
                duration_seconds: Some(1),
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let contained = BTreeMap::from([
        ("demo-run__plan_loop".to_string(), child_detail),
        (
            "demo-run__plan_loop__plan_loop".to_string(),
            grandchild_detail,
        ),
    ]);

    let lines = build_selected_step_detail_lines(
        None,
        Some(&detail),
        Some(&contained),
        &progress,
        Some(0),
        0,
    );

    assert!(lines.contains(&"Failed child step: review_plan".to_string()));
    assert!(lines.contains(&"Error code: validation_error".to_string()));
    assert!(lines.contains(&"Stderr: missing marker".to_string()));
}

#[test]
fn parent_step_subflow_details_include_failed_descendant_without_outputs() {
    let parent_step = transist::InspectionStep {
        step_id: "plan_loop".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("failed".to_string()),
            exit_code: None,
            error_code: Some("child_flow_failed".to_string()),
            error_message: Some("child flow failed".to_string()),
            stderr_tail: None,
            child_run_id: Some("demo-run__plan_loop".to_string()),
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let child_detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run__plan_loop".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("maybe_refresh_review_state".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: Some("demo-run".to_string()),
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![transist::InspectionStep {
            step_id: "maybe_refresh_review_state".to_string(),
            step_kind: "invoke".to_string(),
            status: "failed".to_string(),
            execution: Some(1),
            started_at_ms: None,
            finished_at_ms: None,
            input_summary: transist::InspectionStepInputSummary {
                request_present: false,
                invoke_id: None,
                adapter_kind: None,
                plugin_id: None,
                action: None,
                input_object_keys: Vec::new(),
                metadata_object_keys: Vec::new(),
                filesystem_policy: None,
            },
            output_summary: transist::InspectionStepOutputSummary {
                result_present: false,
                invoke_id: None,
                status: None,
                exit_code: None,
                error_code: Some("validation_error".to_string()),
                error_message: Some("missing marker".to_string()),
                stderr_tail: Some("line 42 failed".to_string()),
                child_run_id: None,
                child_failure: None,
                output_object_keys: Vec::new(),
                artifact_count: 0,
                evidence_count: 0,
                artifact_refs: Vec::new(),
                evidence_refs: Vec::new(),
                workflow_files: Vec::new(),
            },
            is_current_active: false,
            latest_output: None,
            executions: Vec::new(),
        }],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: Some("demo-run__plan_loop".to_string()),
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![parent_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> maybe_refresh_review_state".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(1),
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Failed,
                duration_seconds: Some(1),
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let contained = BTreeMap::from([("demo-run__plan_loop".to_string(), child_detail)]);

    let lines = build_selected_step_detail_lines(
        None,
        Some(&detail),
        Some(&contained),
        &progress,
        Some(0),
        0,
    );

    assert!(
        lines.contains(&"  Step: maybe_refresh_review_state  status: failed  FAILED".to_string())
    );
    assert!(lines.contains(&"  Error: missing marker".to_string()));
    assert!(lines.contains(&"  Stderr: line 42 failed".to_string()));
}

#[test]
fn parent_failed_subflow_step_surfaces_child_preflight_failure() {
    // The parent subflow step is failed,
    // its child run's `status` is `failed`, but no child step is failed —
    // the failure is entirely in the child run's preflight
    // (`generate_plan` input schema violated), and every child step is
    // pending/runnable. Previously the parent details pane was silent
    // because failed-child-step lookup returned nothing and the
    // preflight-failure short-circuit only fires for the currently viewed
    // run.
    let parent_step = transist::InspectionStep {
        step_id: "plan_loop".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(8),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: Some("subflow_ISSUE-279-plan_plan_loop".to_string()),
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: Some("subflow_ISSUE-279-plan_plan_loop".to_string()),
            status: Some("failed".to_string()),
            exit_code: Some(1),
            error_code: None,
            error_message: None,
            stderr_tail: None,
            child_run_id: Some("ISSUE-279-plan__plan_loop".to_string()),
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let child_detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "ISSUE-279-plan__plan_loop".to_string(),
            workflow_id: "cursor_backend_development_plan_review_loop".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("maybe_refresh_review_state".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "ISSUE-279-plan".to_string(),
            parent_run_id: Some("ISSUE-279-plan".to_string()),
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: Some(transist::InspectionPreflightSummary {
            status: "failed".to_string(),
            blocker_count: 1,
            warning_count: 0,
            checked_at_ms: 0,
            failed_checks: vec![transist::InspectionPreflightFailedCheck {
                kind: "input_schema".to_string(),
                message: "input schema validation failed at instance path '/reasoning_effort' and schema path '/properties/reasoning_effort/enum'"
                    .to_string(),
                step_ids: vec!["generate_plan".to_string()],
                plugin_id: Some("openspec".to_string()),
                action: Some("generate_plan".to_string()),
            }],
        }),
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![
            transist::InspectionStep {
                step_id: "maybe_refresh_review_state".to_string(),
                step_kind: "invoke".to_string(),
                status: "pending".to_string(),
                execution: Some(0),
                started_at_ms: None,
                finished_at_ms: None,
                input_summary: transist::InspectionStepInputSummary {
                    request_present: false,
                    invoke_id: None,
                    adapter_kind: None,
                    plugin_id: None,
                    action: None,
                    input_object_keys: Vec::new(),
                    metadata_object_keys: Vec::new(),
                    filesystem_policy: None,
                },
                output_summary: transist::InspectionStepOutputSummary {
                    result_present: false,
                    invoke_id: None,
                    status: None,
                    exit_code: None,
                    error_code: None,
                    error_message: None,
                    stderr_tail: None,
                    child_run_id: None,
                    child_failure: None,
                    output_object_keys: Vec::new(),
                    artifact_count: 0,
                    evidence_count: 0,
                    artifact_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    workflow_files: Vec::new(),
                },
                is_current_active: false,
                latest_output: None,
            executions: Vec::new(),
            },
            transist::InspectionStep {
                step_id: "generate_plan".to_string(),
                step_kind: "invoke".to_string(),
                status: "pending".to_string(),
                execution: Some(0),
                started_at_ms: None,
                finished_at_ms: None,
                input_summary: transist::InspectionStepInputSummary {
                    request_present: false,
                    invoke_id: None,
                    adapter_kind: None,
                    plugin_id: None,
                    action: None,
                    input_object_keys: Vec::new(),
                    metadata_object_keys: Vec::new(),
                    filesystem_policy: None,
                },
                output_summary: transist::InspectionStepOutputSummary {
                    result_present: false,
                    invoke_id: None,
                    status: None,
                    exit_code: None,
                    error_code: None,
                    error_message: None,
                    stderr_tail: None,
                    child_run_id: None,
                    child_failure: None,
                    output_object_keys: Vec::new(),
                    artifact_count: 0,
                    evidence_count: 0,
                    artifact_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    workflow_files: Vec::new(),
                },
                is_current_active: false,
                latest_output: None,
            executions: Vec::new(),
            },
        ],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "ISSUE-279-plan".to_string(),
            workflow_id: "cursor_backend_development_plan_delivery".to_string(),
            workflow_version: "v2".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "ISSUE-279-plan".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: Some("ISSUE-279-plan__plan_loop".to_string()),
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![parent_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "ISSUE-279-plan".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "ISSUE-279-plan__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> maybe_refresh_review_state".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(8),
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Failed,
                duration_seconds: Some(1),
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let contained = BTreeMap::from([("ISSUE-279-plan__plan_loop".to_string(), child_detail)]);

    let lines = build_selected_step_detail_lines(
        None,
        Some(&detail),
        Some(&contained),
        &progress,
        Some(0),
        0,
    );

    assert!(
        lines.contains(&"Child run: ISSUE-279-plan__plan_loop".to_string()),
        "lines={lines:?}"
    );
    assert!(
        lines.contains(&"Error: preflight failed before step dispatch".to_string()),
        "lines={lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Preflight blockers: 1 blocker(s), 0 warning(s)")),
        "descendant preflight fallback should surface blocker counts; lines={lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("input_schema (openspec/generate_plan) [generate_plan]")
                && line.contains("instance path '/reasoning_effort'")
        }),
        "descendant preflight fallback should surface failed check details; lines={lines:?}"
    );
}

#[test]
fn parent_failed_subflow_step_surfaces_child_preflight_failure_from_child_failure_descriptor() {
    let parent_step = transist::InspectionStep {
        step_id: "plan_loop".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: Some("subflow_demo_plan_loop".to_string()),
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: Some("subflow_demo_plan_loop".to_string()),
            status: Some("failed".to_string()),
            exit_code: Some(1),
            error_code: None,
            error_message: Some("preflight failed before step dispatch".to_string()),
            stderr_tail: None,
            child_run_id: Some("demo-run__plan_loop".to_string()),
            child_failure: Some(transist::InspectionChildFailure {
                child_run_id: "demo-run__plan_loop".to_string(),
                failed_step_id: None,
                error_code: None,
                error_message: Some("preflight failed before step dispatch".to_string()),
                stderr: None,
                preflight_blocker_count: Some(1),
                preflight_warning_count: Some(0),
                preflight_first_blocker: Some(
                    "input schema validation failed at instance path '/model' and schema path '/properties/model/type'".to_string(),
                ),
            }),
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
    executions: Vec::new(),
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: Some("demo-run__plan_loop".to_string()),
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![parent_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(1),
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Failed,
                duration_seconds: Some(1),
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(None, Some(&detail), None, &progress, Some(0), 0);

    assert!(
        lines.contains(&"Child run: demo-run__plan_loop".to_string()),
        "lines={lines:?}"
    );
    assert!(
        lines.contains(&"Error: preflight failed before step dispatch".to_string()),
        "lines={lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "Error: preflight failed before step dispatch")
            .count(),
        1,
        "lines={lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Preflight blockers: 1 blocker(s), 0 warning(s)")),
        "lines={lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "input schema validation failed at instance path '/model' and schema path '/properties/model/type'"),
        "lines={lines:?}"
    );
}

#[test]
fn parent_failed_subflow_step_prefers_formatted_child_preflight_checks_when_child_detail_exists() {
    let parent_step = transist::InspectionStep {
        step_id: "plan_loop".to_string(),
        step_kind: "subflow".to_string(),
        status: "failed".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: Some("subflow_demo_plan_loop".to_string()),
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: Some("subflow_demo_plan_loop".to_string()),
            status: Some("failed".to_string()),
            exit_code: Some(1),
            error_code: None,
            error_message: Some("preflight failed before step dispatch".to_string()),
            stderr_tail: None,
            child_run_id: Some("demo-run__plan_loop".to_string()),
            child_failure: Some(transist::InspectionChildFailure {
                child_run_id: "demo-run__plan_loop".to_string(),
                failed_step_id: None,
                error_code: None,
                error_message: Some("preflight failed before step dispatch".to_string()),
                stderr: None,
                preflight_blocker_count: Some(1),
                preflight_warning_count: Some(0),
                preflight_first_blocker: Some(
                    "input schema validation failed at instance path '/model' and schema path '/properties/model/type'".to_string(),
                ),
            }),
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
    executions: Vec::new(),
    };
    let child_detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run__plan_loop".to_string(),
            workflow_id: "child".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("generate_plan".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: Some("demo-run".to_string()),
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: Some(transist::InspectionPreflightSummary {
            status: "failed".to_string(),
            blocker_count: 1,
            warning_count: 0,
            checked_at_ms: 0,
            failed_checks: vec![transist::InspectionPreflightFailedCheck {
                kind: "input_schema".to_string(),
                message: "input schema validation failed at instance path '/model' and schema path '/properties/model/type'".to_string(),
                step_ids: vec!["generate_plan".to_string()],
                plugin_id: Some("openspec".to_string()),
                action: Some("generate_plan".to_string()),
            }],
        }),
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![transist::InspectionStep {
            step_id: "generate_plan".to_string(),
            step_kind: "invoke".to_string(),
            status: "pending".to_string(),
            execution: Some(0),
            started_at_ms: None,
            finished_at_ms: None,
            input_summary: transist::InspectionStepInputSummary {
                request_present: false,
                invoke_id: None,
                adapter_kind: None,
                plugin_id: None,
                action: None,
                input_object_keys: Vec::new(),
                metadata_object_keys: Vec::new(),
                filesystem_policy: None,
            },
            output_summary: transist::InspectionStepOutputSummary {
                result_present: false,
                invoke_id: None,
                status: None,
                exit_code: None,
                error_code: None,
                error_message: None,
                stderr_tail: None,
                child_run_id: None,
                child_failure: None,
                output_object_keys: Vec::new(),
                artifact_count: 0,
                evidence_count: 0,
                artifact_refs: Vec::new(),
                evidence_refs: Vec::new(),
                workflow_files: Vec::new(),
            },
            is_current_active: false,
            latest_output: None,
        executions: Vec::new(),
        }],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: Some("demo-run__plan_loop".to_string()),
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![parent_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(1),
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Failed,
                duration_seconds: Some(1),
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let contained = BTreeMap::from([("demo-run__plan_loop".to_string(), child_detail)]);

    let lines = build_selected_step_detail_lines(
        None,
        Some(&detail),
        Some(&contained),
        &progress,
        Some(0),
        0,
    );

    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "Error: preflight failed before step dispatch")
            .count(),
        1,
        "lines={lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line == "  - input_schema (openspec/generate_plan) [generate_plan]: input schema validation failed at instance path '/model' and schema path '/properties/model/type'"
        }),
        "lines={lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == "input schema validation failed at instance path '/model' and schema path '/properties/model/type'"),
        "lines={lines:?}"
    );
}

#[test]
fn preflight_failed_details_render_blockers_even_with_pending_selected_step() {
    let pending_step = transist::InspectionStep {
        step_id: "analyze_tickets".to_string(),
        step_kind: "invoke".to_string(),
        status: "pending".to_string(),
        execution: None,
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: false,
            invoke_id: None,
            status: None,
            exit_code: None,
            error_code: None,
            error_message: None,
            stderr_tail: None,
            child_run_id: None,
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "failed".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("analyze_tickets".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: Some(transist::InspectionPreflightSummary {
            status: "failed".to_string(),
            checked_at_ms: 0,
            blocker_count: 2,
            warning_count: 1,
            failed_checks: vec![transist::InspectionPreflightFailedCheck {
                kind: "input.type_mismatch".to_string(),
                message: "input schema validation failed at instance path '/args/0' and schema path '/properties/args/items/type'".to_string(),
                step_ids: vec!["analyze_tickets".to_string()],
                plugin_id: Some("shell".to_string()),
                action: Some("exec".to_string()),
            }],
        }),
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![pending_step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("analyze_tickets".to_string()),
            current_step_path: Some("analyze_tickets".to_string()),
            current_step_status: Some("pending".to_string()),
            execution: None,
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "analyze_tickets".to_string(),
                state: crate::display::progress::ChecklistState::Pending,
                duration_seconds: None,
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(None, Some(&detail), None, &progress, Some(0), 0);

    assert_eq!(lines.first().map(String::as_str), Some("── Failure ──"));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("preflight failed before step dispatch"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Preflight blockers: 2 blocker(s), 1 warning(s)"))
    );
    assert!(lines.iter().any(|line| {
        line.contains("input.type_mismatch (shell/exec) [analyze_tickets]")
            && line.contains("instance path '/args/0'")
    }));
    assert!(
        !lines.iter().any(|line| line == "Execution:"),
        "lines={lines:?}"
    );
}

#[test]
fn selected_active_step_details_render_structured_execution_block() {
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(2),
            cycle: Some(1),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(crate::terminal::current_unix_ms().saturating_sub(5_000)),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Active,
                duration_seconds: None,
            }],
        },
        preview_stream: Some("stdout".to_string()),
        preview_text: Some("planning the next change".to_string()),
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(None, None, None, &progress, Some(0), 0);

    assert_eq!(lines.first().map(String::as_str), Some("Execution:"));
    assert!(lines.contains(&"  Step status: running".to_string()));
    assert!(lines.contains(&"  Execution: 2".to_string()));
    assert!(lines.contains(&"  Tool: copilot".to_string()));
    // Per UI feedback the Details pane no longer shows a top-level `Elapsed:`
    // line for the active step — duration is already visible on the Steps
    // pane row, and removing it here keeps the body focused on
    // step-content (status, last output, evidence, etc.).
    assert!(!lines.iter().any(|line| line.starts_with("  Elapsed: ")));
    // The Details pane no longer surfaces `Last output:` for the active
    // step — the live preview is intentionally omitted to keep the
    // run-detail screen mostly static and avoid per-tick redraws driven
    // by streaming token output.
    assert!(!lines.iter().any(|line| line.contains("Last output:")));
    // Must NOT duplicate the checklist line that the Steps pane already shows.
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("plan_loop") && line.contains("elapsed"))
    );
}

#[test]
fn selected_active_step_details_use_same_live_elapsed_source_as_steps_row() {
    let now_ms = crate::terminal::current_unix_ms();
    let top_level_started_at_ms = now_ms.saturating_sub(5_000);
    let step_started_at_ms = now_ms.saturating_sub(12_000);
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> review_plan".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(2),
            cycle: Some(2),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(top_level_started_at_ms),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Active,
                duration_seconds: None,
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "running".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: Some(step_started_at_ms),
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: Some(transist::RunCurrentStep {
            step_id: "plan_loop".to_string(),
            status: "running".to_string(),
        }),
        actionable_run_id: Some("demo-run__plan_loop".to_string()),
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![transist::InspectionStep {
            step_id: "plan_loop".to_string(),
            step_kind: "invoke".to_string(),
            status: "running".to_string(),
            execution: Some(2),
            started_at_ms: Some(step_started_at_ms),
            finished_at_ms: None,
            input_summary: transist::InspectionStepInputSummary {
                request_present: false,
                invoke_id: None,
                adapter_kind: None,
                plugin_id: None,
                action: None,
                input_object_keys: Vec::new(),
                metadata_object_keys: Vec::new(),
                filesystem_policy: None,
            },
            output_summary: transist::InspectionStepOutputSummary {
                result_present: false,
                invoke_id: None,
                status: None,
                exit_code: None,
                error_code: None,
                error_message: None,
                stderr_tail: None,
                child_run_id: Some("demo-run__plan_loop".to_string()),
                child_failure: None,
                output_object_keys: Vec::new(),
                artifact_count: 0,
                evidence_count: 0,
                artifact_refs: Vec::new(),
                evidence_refs: Vec::new(),
                workflow_files: Vec::new(),
            },
            is_current_active: true,
            latest_output: None,
            executions: Vec::new(),
        }],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };

    let lines = build_selected_step_detail_lines(None, Some(&detail), None, &progress, Some(0), 0);
    let status_line = lines
        .iter()
        .find(|line| line.starts_with("  Step status: running"))
        .expect("running step status line");
    let rendered_seconds = status_line
        .split('(')
        .nth(1)
        .and_then(|suffix| suffix.strip_suffix("s active)"))
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .expect("rendered active seconds");
    let progress_elapsed = progress
        .state
        .elapsed_seconds()
        .expect("progress elapsed seconds");
    let step_elapsed = crate::terminal::current_unix_ms().saturating_sub(step_started_at_ms) / 1000;

    assert!(
        rendered_seconds.abs_diff(progress_elapsed) <= 1,
        "status_line={status_line:?} progress_elapsed={progress_elapsed}"
    );
    assert!(
        rendered_seconds.abs_diff(step_elapsed) >= 5,
        "status_line={status_line:?} step_elapsed={step_elapsed}"
    );
}

#[test]
fn selected_active_step_details_omit_live_metadata_when_not_current() {
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("other_step".to_string()),
            current_step_path: Some("other_step".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(7),
            cycle: Some(1),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(crate::terminal::current_unix_ms().saturating_sub(2_000)),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                crate::display::progress::ProgressChecklistItem {
                    step_id: "queued_step".to_string(),
                    state: crate::display::progress::ChecklistState::Active,
                    duration_seconds: None,
                },
                crate::display::progress::ProgressChecklistItem {
                    step_id: "other_step".to_string(),
                    state: crate::display::progress::ChecklistState::Active,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: Some("stdout".to_string()),
        preview_text: Some("processing the other step".to_string()),
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(None, None, None, &progress, Some(0), 0);

    assert_eq!(lines.first().map(String::as_str), Some("Execution:"));
    assert!(lines.contains(&"  Step status: running".to_string()));
    // Tool / Elapsed / Last output / Execution must not leak from the
    // currently-running top-level step into the queued step's details.
    assert!(!lines.iter().any(|line| line.starts_with("  Tool:")));
    assert!(!lines.iter().any(|line| line.starts_with("  Elapsed:")));
    assert!(!lines.iter().any(|line| line.starts_with("  Last output:")));
    assert!(!lines.iter().any(|line| line.starts_with("  Execution:")));
}

fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn run_summary(
    run_id: &str,
    parent_run_id: Option<&str>,
    updated_at_ms: u64,
) -> transist::InspectionRunSummary {
    let root_run_id = parent_run_id
        .map(|parent| parent.split("__").next().unwrap_or(run_id))
        .unwrap_or(run_id);
    transist::InspectionRunSummary {
        run_id: run_id.to_string(),
        workflow_id: "demo".to_string(),
        workflow_version: "v1".to_string(),
        status: "success".to_string(),
        created_at_ms: updated_at_ms.saturating_sub(1),
        updated_at_ms,
        started_at_ms: None,
        finished_at_ms: None,
        current_step_id: None,
        has_blocked_gates: false,
        has_artifacts: false,
        has_events: false,
        artifact_count: 0,
        evidence_count: 0,
        root_run_id: root_run_id.to_string(),
        parent_run_id: parent_run_id.map(str::to_string),
    }
}

#[test]
fn validate_workflow_inputs_collects_typed_values_and_strings() {
    let items = vec![
        WorkflowInputItem {
            name: "name".to_string(),
            value_type: "string".to_string(),
            required: true,
            default_value: None,
            comment: None,
            value: Some("alice".to_string()),
            origin: WorkflowInputOrigin::UserEdited,
        },
        WorkflowInputItem {
            name: "port".to_string(),
            value_type: "number".to_string(),
            required: false,
            default_value: None,
            comment: None,
            value: Some("8080".to_string()),
            origin: WorkflowInputOrigin::UserEdited,
        },
        WorkflowInputItem {
            name: "tags".to_string(),
            value_type: "array".to_string(),
            required: false,
            default_value: None,
            comment: None,
            value: Some("[\"a\",\"b\"]".to_string()),
            origin: WorkflowInputOrigin::File,
        },
        WorkflowInputItem {
            name: "skipped".to_string(),
            value_type: "string".to_string(),
            required: false,
            default_value: None,
            comment: None,
            value: None,
            origin: WorkflowInputOrigin::None,
        },
    ];
    let map = validate_and_collect_workflow_inputs(&items).expect("validate");
    assert_eq!(map.len(), 3);
    assert!(matches!(
        map.get("name"),
        Some(transist::KernelValue::String(s)) if s == "alice"
    ));
    assert!(matches!(
        map.get("port"),
        Some(transist::KernelValue::Number(_))
    ));
    assert!(matches!(
        map.get("tags"),
        Some(transist::KernelValue::Array(_))
    ));
    assert!(!map.contains_key("skipped"));
}

#[test]
fn validate_workflow_inputs_reports_required_missing_index() {
    let items = vec![
        WorkflowInputItem {
            name: "first".to_string(),
            value_type: "string".to_string(),
            required: false,
            default_value: None,
            comment: None,
            value: Some("ok".to_string()),
            origin: WorkflowInputOrigin::UserEdited,
        },
        WorkflowInputItem {
            name: "needed".to_string(),
            value_type: "string".to_string(),
            required: true,
            default_value: None,
            comment: None,
            value: None,
            origin: WorkflowInputOrigin::None,
        },
    ];
    let err = validate_and_collect_workflow_inputs(&items).expect_err("should fail");
    assert_eq!(err, 1);
}

#[test]
fn validate_workflow_inputs_rejects_cleared_file_input_without_default() {
    let items = vec![
        WorkflowInputItem {
            name: "with_default".to_string(),
            value_type: "string".to_string(),
            required: true,
            default_value: Some("\"fallback\"".to_string()),
            comment: None,
            value: None,
            origin: WorkflowInputOrigin::None,
        },
        WorkflowInputItem {
            name: "from_file".to_string(),
            value_type: "string".to_string(),
            required: true,
            default_value: None,
            comment: None,
            value: None,
            origin: WorkflowInputOrigin::File,
        },
    ];
    assert_eq!(validate_and_collect_workflow_inputs(&items), Err(1));
    let map = validate_and_collect_workflow_inputs(&items[..1]).expect("defaulted input");
    assert!(map.is_empty());
}

#[test]
fn workflow_inputs_overlay_renders_panes_status_and_origin_tags() {
    let items = vec![
        WorkflowInputItem {
            name: "endpoint".to_string(),
            value_type: "string".to_string(),
            required: true,
            default_value: None,
            comment: Some("Base URL used for API requests".to_string()),
            value: Some("\"https://example.com\"".to_string()),
            origin: WorkflowInputOrigin::File,
        },
        WorkflowInputItem {
            name: "retries".to_string(),
            value_type: "number".to_string(),
            required: true,
            default_value: None,
            comment: None,
            value: None,
            origin: WorkflowInputOrigin::None,
        },
    ];
    let mut editor = ratatui_textarea::TextArea::from(vec!["\"https://example.com\"".to_string()]);
    editor.set_block(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .title("Value (JSON)"),
    );
    let modal_state = WorkflowInputsModalState {
        declared: items,
        selected: 0,
        focused_pane: WorkflowInputsPane::List,
        editor,
        status: Some("input 'retries' is required".to_string()),
        file_label: Some("/tmp/inputs.yaml".to_string()),
    };

    let backend = TestBackend::new(110, 28);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            components::draw_workflow_inputs_overlay(frame, &modal_state);
        })
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(rendered.contains("Workflow inputs"));
    assert!(rendered.contains("/tmp/inputs.yaml"));
    assert!(rendered.contains("Parameters (* required)"));
    assert!(rendered.contains("Value (JSON) — Base URL used for API requests"));
    assert!(rendered.contains("* endpoint:string [file]"));
    assert!(rendered.contains("[file]"));
    assert!(rendered.contains("* retries:number"));
    assert!(rendered.contains("input 'retries' is required"));
    assert!(rendered.contains("Enter save/list"));
    assert!(rendered.contains("Shift+Enter newline"));
    assert!(rendered.contains("Ctrl+S"));
}

#[test]
fn strip_workflow_input_comments_extracts_string_comments_and_removes_metadata_keys() {
    let mut file_inputs = BTreeMap::from([
        (
            "endpoint".to_string(),
            transist::KernelValue::String("https://example.com".to_string()),
        ),
        (
            "_comment_endpoint".to_string(),
            transist::KernelValue::String("Base URL for API calls".to_string()),
        ),
        (
            "_comment_retries".to_string(),
            transist::KernelValue::Number(serde_json::Number::from(3)),
        ),
        (
            "_comment_".to_string(),
            transist::KernelValue::String("ignored empty input name".to_string()),
        ),
    ]);

    let comments = strip_workflow_input_comments(&mut file_inputs);

    assert_eq!(
        comments.get("endpoint").map(String::as_str),
        Some("Base URL for API calls")
    );
    assert!(!comments.contains_key("retries"));
    assert!(!file_inputs.contains_key("_comment_endpoint"));
    assert!(!file_inputs.contains_key("_comment_retries"));
    assert!(!file_inputs.contains_key("_comment_"));
    assert!(file_inputs.contains_key("endpoint"));
}

fn workflow_inputs_recipe() -> transist::Recipe {
    yaml_serde::from_str(
        r#"
schema: transist.recipe.v1
workflow: {id: inputs-regression, version: v1}
inputs:
  text: {type: string, required: true, default: fallback}
  flag: {type: boolean, required: true, default: false}
  count: {type: number, required: true, default: 0}
  tags: {type: array, required: true, default: []}
  config: {type: object, required: true, default: {}}
  multiline:
    type: string
    required: true
    default: |-
      first line
      second line
  nullable: {type: string, required: true, default: not-null}
"#,
    )
    .expect("recipe with typed defaults")
}

fn workflow_inputs_modal(items: Vec<WorkflowInputItem>) -> WorkflowInputsModalState {
    let editor = make_workflow_inputs_editor(items.first().and_then(|item| item.editor_value()));
    WorkflowInputsModalState {
        declared: items,
        selected: 0,
        focused_pane: WorkflowInputsPane::Editor,
        editor,
        status: None,
        file_label: None,
    }
}

fn render_workflow_inputs(state: &WorkflowInputsModalState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).expect("terminal");
    terminal
        .draw(|frame| components::draw_workflow_inputs_overlay(frame, state))
        .expect("draw workflow inputs");
    buffer_to_string(terminal.backend().buffer())
}

#[test]
fn workflow_inputs_partial_yaml_preserves_defaults_and_file_values_on_untouched_commit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("inputs.yaml");
    std::fs::write(
        &path,
        "text: from-file\nnullable: null\nextra: {nested: [1, false]}\n_comment_text: File description\n_comment_count: 12\n_comment_: ignored\n",
    )
    .expect("write partial inputs file");
    let file_inputs = crate::input::load_inputs_file(&path).expect("load YAML inputs");
    let mut state = workflow_inputs_modal(
        build_workflow_input_items(&workflow_inputs_recipe(), file_inputs).expect("build inputs"),
    );
    state.file_label = Some(path.display().to_string());
    assert_eq!(state.declared.len(), 8, "metadata must not become inputs");
    assert_eq!(
        state
            .declared
            .iter()
            .find(|item| item.name == "text")
            .unwrap()
            .comment
            .as_deref(),
        Some("File description")
    );

    let defaults = serde_json::json!({
        "text": "fallback",
        "flag": false,
        "count": 0,
        "tags": [],
        "config": {},
        "multiline": "first line\nsecond line",
        "nullable": "not-null"
    });
    for selected in 0..state.declared.len() {
        state.selected = selected;
        let item = &state.declared[selected];
        if let Some(expected) = defaults.get(&item.name) {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(item.default_value.as_deref().unwrap())
                    .expect("serialized JSON default"),
                *expected
            );
        } else {
            assert_eq!(item.name, "extra", "undeclared file key must be retained");
            assert!(item.default_value.is_none());
        }
        let before = (item.value.clone(), item.origin);
        let name = item.name.clone();
        let value_type = item.value_type.clone();
        let required = item.required;
        state.editor = make_workflow_inputs_editor(item.editor_value());
        state.commit_editor();
        state.editor.move_cursor(ratatui_textarea::CursorMove::End);
        state.editor.move_cursor(ratatui_textarea::CursorMove::Head);
        state.commit_editor();
        assert_eq!(
            (
                state.declared[selected].value.clone(),
                state.declared[selected].origin
            ),
            before,
            "untouched text and cursor motion must preserve {name}'s provenance"
        );
        let rendered = render_workflow_inputs(&state);
        assert!(rendered.contains("Parameters (* required)"));
        let tag = if before.0.is_some() {
            "[file]"
        } else {
            "[default]"
        };
        let marker = if required { "* " } else { "" };
        assert!(
            rendered.contains(&format!("{marker}{name}:{value_type} {tag}")),
            "expected required marker and origin for {name}; rendered={rendered}"
        );
        assert!(!rendered.contains("[edited]"));
    }

    assert_eq!(
        serde_json::to_value(validate_and_collect_workflow_inputs(&state.declared).unwrap())
            .unwrap(),
        serde_json::json!({
            "text": "from-file",
            "nullable": null,
            "extra": {"nested": [1, false]}
        }),
        "submission contains explicit file overrides, including null, but no defaults"
    );
}

#[test]
fn workflow_inputs_actual_edits_submit_typed_overrides_and_render_edited_markers() {
    let mut state = workflow_inputs_modal(
        build_workflow_input_items(&workflow_inputs_recipe(), BTreeMap::new()).unwrap(),
    );
    let edits = serde_json::json!({
        "text": "edited text",
        "flag": true,
        "count": 42,
        "tags": ["a", "b"],
        "config": {"enabled": false},
        "multiline": "edited first\nedited second"
    });
    for (name, expected) in edits.as_object().unwrap() {
        state.selected = state
            .declared
            .iter()
            .position(|item| &item.name == name)
            .unwrap();
        state.editor = make_workflow_inputs_editor(state.declared[state.selected].editor_value());
        // Exercise the textarea edit path, rather than assigning the override directly.
        state.editor.select_all();
        state
            .editor
            .insert_str(serde_json::to_string(expected).unwrap());
        state.commit_editor();
        let item = &state.declared[state.selected];
        assert_eq!(item.origin, WorkflowInputOrigin::UserEdited);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(item.value.as_deref().unwrap()).unwrap(),
            *expected
        );
        let rendered = render_workflow_inputs(&state);
        assert!(
            rendered.contains(&format!("* {}:{} [edited]", item.name, item.value_type)),
            "edited required input keeps its marker; rendered={rendered}"
        );
        let before = item.value.clone();
        state.commit_editor();
        assert_eq!(state.declared[state.selected].value, before);
        assert_eq!(
            state.declared[state.selected].origin,
            WorkflowInputOrigin::UserEdited
        );
    }
    assert_eq!(
        serde_json::to_value(validate_and_collect_workflow_inputs(&state.declared).unwrap())
            .unwrap(),
        edits,
        "typed edits are submitted and untouched nullable default is omitted"
    );
}

#[test]
fn workflow_inputs_clearing_overrides_restores_defaults_or_reports_missing_required() {
    let mut recipe = workflow_inputs_recipe();
    recipe.inputs.insert(
        "needed".to_string(),
        yaml_serde::from_str("type: string\nrequired: true\n").unwrap(),
    );
    let file_inputs = BTreeMap::from([
        (
            "text".to_string(),
            transist::KernelValue::String("file text".to_string()),
        ),
        (
            "needed".to_string(),
            transist::KernelValue::String("file required".to_string()),
        ),
    ]);
    let mut state =
        workflow_inputs_modal(build_workflow_input_items(&recipe, file_inputs).unwrap());
    let text_index = state
        .declared
        .iter()
        .position(|item| item.name == "text")
        .unwrap();
    state.selected = text_index;
    assert!(render_workflow_inputs(&state).contains("* text:string [file]"));

    // Both file-provided and subsequently user-edited overrides can be cleared.
    for edited in [false, true] {
        if edited {
            state.editor = make_workflow_inputs_editor(Some("\"user text\""));
            state.commit_editor();
            assert!(render_workflow_inputs(&state).contains("* text:string [edited]"));
        }
        state.editor = make_workflow_inputs_editor(state.declared[text_index].editor_value());
        state.editor.select_all();
        state.editor.cut();
        state.commit_editor();
        let item = &state.declared[text_index];
        assert!(item.value.is_none());
        assert_eq!(item.origin, WorkflowInputOrigin::None);
        assert_eq!(item.editor_value(), Some("\"fallback\""));
        assert_eq!(state.editor.lines().join("\n"), "\"fallback\"");
        assert!(render_workflow_inputs(&state).contains("* text:string [default]"));
        state.commit_editor();
        assert!(state.declared[text_index].value.is_none());
        assert_eq!(
            serde_json::to_value(validate_and_collect_workflow_inputs(&state.declared).unwrap())
                .unwrap(),
            serde_json::json!({"needed": "file required"})
        );
    }

    let needed_index = state
        .declared
        .iter()
        .position(|item| item.name == "needed")
        .unwrap();
    state.selected = needed_index;
    assert!(render_workflow_inputs(&state).contains("* needed:string [file]"));
    state.editor = make_workflow_inputs_editor(None);
    state.commit_editor();
    assert!(state.declared[needed_index].value.is_none());
    assert_eq!(
        state.declared[needed_index].origin,
        WorkflowInputOrigin::None
    );
    assert_eq!(state.declared[needed_index].editor_value(), None);
    assert_eq!(
        validate_and_collect_workflow_inputs(&state.declared),
        Err(needed_index)
    );
    let rendered = render_workflow_inputs(&state);
    assert!(rendered.contains("* needed:string"));
    assert!(!rendered.contains("needed:string [file]"));
    assert!(!rendered.contains("needed:string [default]"));
}

#[test]
fn run_detail_frame_footer_advertises_ctrl_c_cancel() {
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("step-a".to_string()),
            current_step_path: Some("step-a.tool".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(1),
            cycle: Some(1),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(1000),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "step-a".to_string(),
                state: crate::display::progress::ChecklistState::Active,
                duration_seconds: None,
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let backend = TestBackend::new(140, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            components::draw_run_detail_frame(
                frame,
                &ScreenMetadata::default(),
                None,
                Some(&progress),
                None,
                None,
                0,
                None,
                None,
            );
        })
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(
        rendered.contains("Ctrl+C"),
        "run-detail footer should advertise Ctrl+C as a cancel keybinding; rendered={rendered}"
    );
    assert!(
        !rendered.contains("Recent output"),
        "running run should use the unified Steps|Details layout, not the legacy Recent output sidebar; rendered={rendered}"
    );
}

#[test]
fn live_running_view_routes_through_run_detail_frame_layout() {
    // The 'running' fullscreen view (set_progress without run_detail_screen_active)
    // must now render through the unified run-detail layout: Steps + Details panes,
    // and crucially must NOT render the legacy Recent output sidebar.
    //
    // This test exercises draw_run_detail_frame directly (which is what
    // FullscreenPrompt::draw() now dispatches to whenever progress is Some).
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("plan".to_string()),
            current_step_path: Some("plan.draft".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(1),
            cycle: Some(1),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(0),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                crate::display::progress::ProgressChecklistItem {
                    step_id: "plan".to_string(),
                    state: crate::display::progress::ChecklistState::Active,
                    duration_seconds: None,
                },
                crate::display::progress::ProgressChecklistItem {
                    step_id: "implement".to_string(),
                    state: crate::display::progress::ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let backend = TestBackend::new(140, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            components::draw_run_detail_frame(
                frame,
                &ScreenMetadata::default(),
                None,
                Some(&progress),
                None,
                None,
                0,
                None,
                None,
            );
        })
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(rendered.contains("Steps"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("plan"));
    assert!(rendered.contains("implement"));
    assert!(
        !rendered.contains("Recent output"),
        "running run must use unified Steps|Details layout; rendered={rendered}"
    );
}

#[test]
fn live_running_view_renders_finished_step_artifacts_when_finished_step_is_selected() {
    // Regression: while a run is still executing, the operator must be able
    // to Tab into the Details pane and select an already-completed step in
    // Steps to inspect its workflow files / artifacts / evidence. This used
    // to be impossible because `set_progress` cleared `current_run_detail`,
    // so the Details pane could not source the finished-step output. The
    // live-loop now feeds the snapshot through `update_run_detail_context`
    // and `set_progress` keeps the data; this test exercises the rendering
    // contract: when a finished step is selected and `run_detail` carries
    // its artifact ref, the rendered Details pane must show the artifact
    // path even while the run keeps executing.
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("implement".to_string()),
            current_step_path: Some("implement".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(1),
            cycle: Some(1),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(0),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                crate::display::progress::ProgressChecklistItem {
                    step_id: "plan".to_string(),
                    state: crate::display::progress::ChecklistState::Success,
                    duration_seconds: Some(2),
                },
                crate::display::progress::ProgressChecklistItem {
                    step_id: "implement".to_string(),
                    state: crate::display::progress::ChecklistState::Active,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let finished_step = transist::InspectionStep {
        step_id: "plan".to_string(),
        step_kind: "invoke".to_string(),
        status: "success".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("success".to_string()),
            exit_code: Some(0),
            error_code: None,
            error_message: None,
            stderr_tail: None,
            child_run_id: None,
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 1,
            evidence_count: 0,
            artifact_refs: vec![transist::InspectionStepBlobRef {
                step_id: "plan".to_string(),
                execution: 1,
                relative_path: "000_stdout.txt".to_string(),
                name: "000_stdout.txt".to_string(),
                bytes: 42,
            }],
            evidence_refs: Vec::new(),
            workflow_files: vec![transist::InspectionWorkflowFileRef {
                step_id: "plan".to_string(),
                field_name: "plan_file".to_string(),
                locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
                relative_path: "plan.md".to_string(),
                local_path: Some(PathBuf::from("/tmp/plan.md")),
                name: "plan.md".to_string(),
                bytes: 7,
            }],
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "running".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("implement".to_string()),
            has_blocked_gates: false,
            has_artifacts: true,
            has_events: false,
            artifact_count: 1,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: Vec::new(),
        steps: vec![finished_step],
        artifact_count: 1,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };

    // Operator has Tab'd into Steps and selected the first (finished) step.
    let (lines, openables) = super::run_detail::build_selected_step_detail_lines_with_openables(
        Some(Path::new("/tmp/.transist")),
        Some(&detail),
        None,
        &progress,
        Some(0),
        0,
    );
    let file_row = lines
        .iter()
        .position(|line| line.contains("plan_file: '/tmp/plan.md'"))
        .expect("absolute workflow file row in live details");
    assert_eq!(openables[file_row], Some(PathBuf::from("/tmp/plan.md")));
    assert!(openables.iter().flatten().all(|path| path.is_absolute()));

    let run_detail_state = RunDetailState {
        focused_section: RunDetailSection::Steps,
        selected_step: 0,
        details_scroll: 0,
        details_cursor: 0,
        selected_action: 0,
        step_pinned_by_user: true,
        pane_cache: None,
    };

    let backend = TestBackend::new(160, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            components::draw_run_detail_frame(
                frame,
                &ScreenMetadata::default(),
                Some(Path::new("/tmp/.transist")),
                Some(&progress),
                Some(&detail),
                None,
                0,
                Some(&run_detail_state),
                None,
            );
        })
        .expect("draw");

    let rendered = buffer_to_string(terminal.backend().buffer());
    assert!(
        rendered.contains("plan_file"),
        "Details pane should expose the finished step's workflow file field name; rendered={rendered}"
    );
    assert!(
        rendered.contains("/tmp/plan.md"),
        "Details pane should expose the finished step's workflow file path; rendered={rendered}"
    );
    assert!(
        rendered.contains("000_stdout.txt"),
        "Details pane should expose the finished step's artifact name; rendered={rendered}"
    );
}

#[test]
fn live_running_view_step_navigation_helpers_cycle_focus_and_clamp_selection() {
    // The live-loop must let the operator cycle Tab/Shift+Tab focus between
    // Steps and Details (Actions is not present during a live run) and move
    // the selected step bounded by the current checklist length.
    use super::run_detail::{RunDetailSection, run_detail_move_focus};

    // Tab forward without actions cycles Steps -> Details -> Steps.
    assert_eq!(
        run_detail_move_focus(RunDetailSection::Steps, true, false),
        RunDetailSection::Details
    );
    assert_eq!(
        run_detail_move_focus(RunDetailSection::Details, true, false),
        RunDetailSection::Steps
    );
    // Shift+Tab cycles in the opposite direction with the same two-section
    // topology when there are no actions.
    assert_eq!(
        run_detail_move_focus(RunDetailSection::Details, false, false),
        RunDetailSection::Steps
    );
    assert_eq!(
        run_detail_move_focus(RunDetailSection::Steps, false, false),
        RunDetailSection::Details
    );
    // Stale Actions focus is recovered to Details (defensive: if a previous
    // run-detail-screen session had focused Actions, the live loop snaps
    // back to a section that exists during a live run).
    assert_eq!(
        run_detail_move_focus(RunDetailSection::Actions, true, false),
        RunDetailSection::Details
    );
}

#[test]
fn default_run_detail_state_focuses_actions_for_blocked_runs_and_steps_otherwise() {
    let actions = vec![SuggestedAction::new(SuggestedActionExecution::RunsShow {
        run_id: "demo-run".to_string(),
    })];

    let blocked = default_run_detail_state(&actions, 2, 0, "blocked");
    assert_eq!(blocked.focused_section, RunDetailSection::Actions);

    let running = default_run_detail_state(&actions, 2, 0, "running");
    assert_eq!(running.focused_section, RunDetailSection::Steps);

    let success = default_run_detail_state(&actions, 2, 0, "success");
    assert_eq!(success.focused_section, RunDetailSection::Steps);
}

#[test]
fn run_detail_frame_clears_body_so_prior_layout_does_not_bleed_through() {
    // Regression: transitioning from the 3-column base layout (Steps |
    // Details | Sidebar) to the 2-column run-detail layout (Steps |
    // Details) used to leave the right-most sidebar cells of the previous
    // frame visible because no widget in the new layout repainted those
    // cells. Asserting via a sentinel string ("ghostly_sidebar_marker")
    // that gets rendered into the sidebar's history pane on frame 1 and
    // must not appear anywhere on frame 2.
    let history_lines = vec!["ghostly_sidebar_marker".to_string()];
    let progress_running = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("plan".to_string()),
            current_step_path: Some("plan.draft".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(1),
            cycle: Some(1),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(0),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan".to_string(),
                state: crate::display::progress::ChecklistState::Active,
                duration_seconds: None,
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let backend = TestBackend::new(140, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // Frame 1: 3-column base frame populates the sidebar with the marker.
    terminal
        .draw(|frame| {
            components::draw_base_frame(
                frame,
                &ScreenMetadata::default(),
                None,
                None,
                &history_lines,
                None,
            );
        })
        .expect("draw frame 1");
    let frame1 = buffer_to_string(terminal.backend().buffer());
    assert!(
        frame1.contains("ghostly_sidebar_marker"),
        "frame 1 should render the marker into the sidebar; rendered={frame1}"
    );

    // Frame 2: 2-column run-detail frame must not let the sidebar marker
    // bleed through. The new layout has no sidebar, so any leftover cells
    // would be visual artifacts.
    terminal
        .draw(|frame| {
            components::draw_run_detail_frame(
                frame,
                &ScreenMetadata::default(),
                None,
                Some(&progress_running),
                None,
                None,
                0,
                None,
                None,
            );
        })
        .expect("draw frame 2");
    let frame2 = buffer_to_string(terminal.backend().buffer());
    assert!(
        !frame2.contains("ghostly_sidebar_marker"),
        "previous-frame sidebar text must not bleed into the run-detail layout; rendered={frame2}"
    );
}

#[test]
fn run_detail_pane_clears_right_pane_so_picker_history_does_not_bleed_through() {
    // Regression: the right-pane sub-split of `draw_run_detail_pane`
    // (Details + Commands) shrinks/grows when picker_state appears or
    // disappears. Without an explicit Clear, fragments of the prior
    // commands list (or its highlight row) remained visible. Asserting
    // via a sentinel command label that survives across frame 1 (with
    // picker) and is gone on frame 2 (without picker).
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("plan".to_string()),
            current_step_path: Some("plan.draft".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(1),
            cycle: Some(1),
            tool_name: Some("copilot".to_string()),
            started_at_ms: Some(0),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan".to_string(),
                state: crate::display::progress::ChecklistState::Active,
                duration_seconds: None,
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let picker = ActionPickerState {
        title: "Commands".to_string(),
        items: vec!["ghostly_command_marker".to_string()],
        selected: 0,
        actionable: true,
    };

    let backend = TestBackend::new(140, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // Frame 1: picker visible.
    terminal
        .draw(|frame| {
            components::draw_run_detail_frame(
                frame,
                &ScreenMetadata::default(),
                None,
                Some(&progress),
                None,
                None,
                0,
                None,
                Some(&picker),
            );
        })
        .expect("draw frame 1");
    let frame1 = buffer_to_string(terminal.backend().buffer());
    assert!(
        frame1.contains("ghostly_command_marker"),
        "frame 1 should render the picker command; rendered={frame1}"
    );

    // Frame 2: picker hidden — the previous commands sub-rect must be
    // cleared, not left as stale buffer cells.
    terminal
        .draw(|frame| {
            components::draw_run_detail_frame(
                frame,
                &ScreenMetadata::default(),
                None,
                Some(&progress),
                None,
                None,
                0,
                None,
                None,
            );
        })
        .expect("draw frame 2");
    let frame2 = buffer_to_string(terminal.backend().buffer());
    assert!(
        !frame2.contains("ghostly_command_marker"),
        "picker command text must not bleed through after picker is hidden; rendered={frame2}"
    );
}

#[test]
fn gate_request_summary_path_strings_are_surfaced_under_gate_request_heading() {
    // The plan_loop-style gate hands a structured request_summary describing
    // what the reviewer should look at. The Details pane renders those path-
    // shaped values verbatim under a `Gate request:` heading so a reviewer
    // does not have to drop to `runs show --json` to see them.
    let mut request_summary = BTreeMap::new();
    request_summary.insert(
        "plan_file".to_string(),
        transist::KernelValue::String("/tmp/plan.md".to_string()),
    );
    request_summary.insert(
        "attachments".to_string(),
        transist::KernelValue::Array(vec![
            transist::KernelValue::String("/tmp/diff.patch".to_string()),
            transist::KernelValue::String("/tmp/log.txt".to_string()),
        ]),
    );

    let step = transist::InspectionStep {
        step_id: "plan_loop".to_string(),
        step_kind: "human_gate".to_string(),
        status: "blocked".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: false,
            invoke_id: None,
            status: None,
            exit_code: None,
            error_code: None,
            error_message: None,
            stderr_tail: None,
            child_run_id: None,
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: Vec::new(),
        },
        is_current_active: true,
        latest_output: None,
        executions: Vec::new(),
    };
    let detail = transist::InspectionRunDetail {
        summary: transist::InspectionRunSummary {
            run_id: "demo-run".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "blocked".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: Some("plan_loop".to_string()),
            has_blocked_gates: true,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            root_run_id: "demo-run".to_string(),
            parent_run_id: None,
        },
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: None,
        blocked_gates: vec![transist::InspectionGate {
            gate_id: "plan-review".to_string(),
            status: "blocked".to_string(),
            step_id: "plan_loop".to_string(),
            created_at_ms: None,
            decision: None,
            decision_history: Vec::new(),
            request_summary,
        }],
        steps: vec![step],
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    };
    let progress = LiveProgress {
        state: crate::display::progress::ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "blocked".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop".to_string()),
            current_step_status: Some("blocked".to_string()),
            execution: None,
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![crate::display::progress::ProgressChecklistItem {
                step_id: "plan_loop".to_string(),
                state: crate::display::progress::ChecklistState::Active,
                duration_seconds: None,
            }],
        },
        preview_stream: None,
        preview_text: None,
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = build_selected_step_detail_lines(None, Some(&detail), None, &progress, Some(0), 0);

    let joined = lines.join("\n");
    assert!(
        lines.iter().any(|line| line == "  Gate request:"),
        "expected `Gate request:` heading; got:\n{}",
        joined
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "    plan_file: '/tmp/plan.md'"),
        "expected scalar path entry; got:\n{}",
        joined
    );
    assert!(
        lines.iter().any(|line| line == "    attachments:"),
        "expected attachments header; got:\n{}",
        joined
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "      [0]: '/tmp/diff.patch'"),
        "expected first array entry; got:\n{}",
        joined
    );
    assert!(
        lines.iter().any(|line| line == "      [1]: '/tmp/log.txt'"),
        "expected second array entry; got:\n{}",
        joined
    );
}

#[test]
fn format_unix_ms_iso8601_renders_utc_zulu_timestamps() {
    use super::run_detail::format_unix_ms_iso8601;
    // 2024-05-04T12:34:56Z
    let unix_ms: u64 = 1_714_826_096_000;
    assert_eq!(format_unix_ms_iso8601(unix_ms), "2024-05-04T12:34:56Z");
    // Epoch.
    assert_eq!(format_unix_ms_iso8601(0), "1970-01-01T00:00:00Z");
    // Sub-second milliseconds truncate to the floor second to keep the
    // header compact; the helper is documented as second-precision.
    assert_eq!(format_unix_ms_iso8601(1_500), "1970-01-01T00:00:01Z");
}

#[test]
fn extract_openable_path_recognises_artifact_and_workflow_file_rows() {
    use super::run_detail::extract_openable_path;
    use std::path::PathBuf;
    // Workflow-file row shape: `  <field>: '<path>' (<n> bytes)`.
    assert_eq!(
        extract_openable_path("  proposal_md: '/tmp/openspec/proposal.md' (4096 bytes)"),
        Some(PathBuf::from("/tmp/openspec/proposal.md"))
    );
    // Artifact / evidence row shape: `  '<path>' (<n> bytes)` (no field
    // name prefix, just the quoted path under the heading).
    assert_eq!(
        extract_openable_path("  '/var/transist/runs/ISSUE-238-1/artifacts/plan.md' (1024 bytes)"),
        Some(PathBuf::from(
            "/var/transist/runs/ISSUE-238-1/artifacts/plan.md"
        ))
    );
    // Subflow per-step block adds extra indent — same shape, deeper indent.
    assert_eq!(
        extract_openable_path("    proposal_md: '/tmp/p.md' (12 bytes)"),
        Some(PathBuf::from("/tmp/p.md"))
    );
    // Heading rows must not be tagged.
    assert_eq!(extract_openable_path("Workflow files:"), None);
    assert_eq!(extract_openable_path("Artifacts:"), None);
    assert_eq!(
        extract_openable_path("  'reports/plan.md' (12 bytes)"),
        None
    );
    assert_eq!(
        extract_openable_path("  report_file: '/tmp/user's workspace/report.md' (12 bytes)"),
        Some(PathBuf::from("/tmp/user's workspace/report.md"))
    );
    // Gate request summary lines render resolved absolute paths as
    // `  <key>: '<absolute-path>'` (single-quoted, no byte count).
    // These are now openable via the secondary shape.
    assert_eq!(
        extract_openable_path("  plan_file: '/tmp/plan.md'"),
        Some(PathBuf::from("/tmp/plan.md"))
    );
    assert_eq!(
        extract_openable_path("    [1]: '/home/user/.transist/runs/run_1/ISSUE-1/report.md'"),
        Some(PathBuf::from(
            "/home/user/.transist/runs/run_1/ISSUE-1/report.md"
        ))
    );
    // Non-absolute single-quoted values (e.g. gate IDs, short strings) must
    // not be tagged as openable by the secondary shape.
    assert_eq!(extract_openable_path("  gate_id: 'my-gate'"), None);
    assert_eq!(extract_openable_path("  status: 'blocked'"), None);
}

#[test]
fn follow_cursor_with_scroll_keeps_cursor_inside_visible_window() {
    use super::format::follow_cursor_with_scroll;
    let mut offset = 0usize;
    // Cursor inside the window: scroll untouched.
    follow_cursor_with_scroll(&mut offset, 3, 10, 50);
    assert_eq!(offset, 0);

    // Cursor below the bottom of the window: scroll forward so cursor is
    // the last visible row.
    follow_cursor_with_scroll(&mut offset, 12, 10, 50);
    assert_eq!(offset, 3);

    // Cursor jumps above the top: scroll up to the cursor.
    follow_cursor_with_scroll(&mut offset, 1, 10, 50);
    assert_eq!(offset, 1);

    // Tail clamp: cursor near the end of the buffer must not push offset
    // past `total - visible`.
    let mut offset = 0usize;
    follow_cursor_with_scroll(&mut offset, 49, 10, 50);
    assert_eq!(offset, 40);
}

fn inspection_run_summary(run_id: &str, status: &str) -> transist::InspectionRunSummary {
    transist::InspectionRunSummary {
        run_id: run_id.to_string(),
        workflow_id: "demo".to_string(),
        workflow_version: "v1".to_string(),
        status: status.to_string(),
        created_at_ms: 0,
        updated_at_ms: 0,
        started_at_ms: None,
        finished_at_ms: None,
        current_step_id: None,
        has_blocked_gates: false,
        has_artifacts: false,
        has_events: false,
        artifact_count: 0,
        evidence_count: 0,
        root_run_id: run_id.to_string(),
        parent_run_id: None,
    }
}

fn inspection_run_detail_with_cleanup(
    run_id: &str,
    status: &str,
    cleanup: transist::InspectionCleanupSummary,
) -> transist::InspectionRunDetail {
    transist::InspectionRunDetail {
        summary: inspection_run_summary(run_id, status),
        workflow_inputs: BTreeMap::new(),
        global_default_filesystem_policy: None,
        workflow_filesystem_policy: None,
        current_step: None,
        actionable_run_id: None,
        active_lease: None,
        terminal_outcome: None,
        recovery_state: None,
        preflight: None,
        cleanup: Some(cleanup),
        blocked_gates: Vec::new(),
        steps: Vec::new(),
        artifact_count: 0,
        evidence_count: 0,
        event_count: 0,
        latest_event_id: None,
        lineage: None,
        contained_runs: Vec::new(),
        step_counts: BTreeMap::new(),
        trusted_execution_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        trust: transist::InspectionTrustSummary {
            trusted_execution_mode: false,
            workflow: None,
            plugins: Vec::new(),
            decision: None,
            policy_decision: None,
        },
        locality: transist::InspectionLocalityView {
            steps: Vec::new(),
            external_transfers: Vec::new(),
        },
        evidence_export: transist::InspectionEvidenceExportView {
            location: None,
            command: "transist runs evidence export run-1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    }
}

fn workflow_file_detail(local_path: Option<PathBuf>) -> transist::InspectionRunDetail {
    let mut detail = inspection_run_detail_with_cleanup(
        "demo-run",
        "success",
        cleanup_summary("success", "succeeded"),
    );
    detail.cleanup = None;
    detail.steps.push(transist::InspectionStep {
        step_id: "plan".to_string(),
        step_kind: "invoke".to_string(),
        status: "success".to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: transist::InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: transist::InspectionStepOutputSummary {
            result_present: true,
            invoke_id: None,
            status: Some("success".to_string()),
            exit_code: Some(0),
            error_code: None,
            error_message: None,
            stderr_tail: None,
            child_run_id: None,
            child_failure: None,
            output_object_keys: Vec::new(),
            artifact_count: 0,
            evidence_count: 0,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            workflow_files: vec![transist::InspectionWorkflowFileRef {
                step_id: "plan".to_string(),
                field_name: "plan_file".to_string(),
                locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
                relative_path: "reports/plan.md".to_string(),
                local_path,
                name: "plan.md".to_string(),
                bytes: 7,
            }],
        },
        is_current_active: false,
        latest_output: None,
        executions: Vec::new(),
    });
    detail
}

#[test]
fn no_progress_workflow_files_use_absolute_local_paths_for_display_and_opening() {
    for grouped in [false, true] {
        let mut detail =
            workflow_file_detail(Some(PathBuf::from("/tmp/workspace/reports/plan.md")));
        if grouped {
            let files = &mut detail.steps[0].output_summary.workflow_files;
            let mut report = files[0].clone();
            report.field_name = "report_file".to_string();
            report.relative_path = "reports/review.md".to_string();
            report.local_path = Some(PathBuf::from("/tmp/workspace/reports/review.md"));
            report.name = "review.md".to_string();
            files.push(report);
        }

        let (lines, openables) =
            super::run_detail::build_selected_step_detail_lines_without_progress_with_openables(
                None,
                &detail,
                None,
                Some(0),
            );

        assert_eq!(lines.len(), openables.len());
        for file in &detail.steps[0].output_summary.workflow_files {
            let path = file.local_path.as_ref().expect("local workflow file path");
            let row = lines
                .iter()
                .position(|line| line.contains(&format!("'{}' (7 bytes)", path.display())))
                .expect("absolute workflow file row");
            assert_eq!(openables[row].as_ref(), Some(path));
        }
        assert_eq!(
            openables.iter().flatten().count(),
            if grouped { 2 } else { 1 }
        );
        assert!(openables.iter().flatten().all(|path| path.is_absolute()));
    }
}

#[test]
fn nested_workflow_files_open_absolute_local_paths_without_progress() {
    let mut root = workflow_file_detail(None);
    root.steps[0].step_kind = "subflow".to_string();
    root.steps[0].output_summary.workflow_files.clear();
    root.steps[0].output_summary.child_run_id = Some("demo-run__child".to_string());
    let mut child = root.clone();
    child.summary.run_id = "demo-run__child".to_string();
    child.summary.parent_run_id = Some("demo-run".to_string());
    child.steps[0].output_summary.child_run_id = Some("demo-run__child__nested".to_string());
    let path = PathBuf::from("/tmp/workspace/reports/plan.md");
    let mut nested = workflow_file_detail(Some(path.clone()));
    nested.summary.run_id = "demo-run__child__nested".to_string();
    nested.summary.parent_run_id = Some("demo-run__child".to_string());
    nested.summary.root_run_id = "demo-run".to_string();
    let contained = BTreeMap::from([
        (child.summary.run_id.clone(), child),
        (nested.summary.run_id.clone(), nested),
    ]);

    let (lines, openables) =
        super::run_detail::build_selected_step_detail_lines_without_progress_with_openables(
            None,
            &root,
            Some(&contained),
            Some(0),
        );

    let row = lines
        .iter()
        .position(|line| line.contains("plan_file: '/tmp/workspace/reports/plan.md'"))
        .expect("nested absolute workflow file row");
    assert_eq!(openables[row], Some(path));
    assert_eq!(openables.iter().flatten().count(), 1);
    assert!(openables.iter().flatten().all(|path| path.is_absolute()));
}

#[test]
fn no_progress_workflow_files_without_local_paths_are_not_openable() {
    for grouped in [false, true] {
        let mut detail = workflow_file_detail(None);
        if grouped {
            let files = &mut detail.steps[0].output_summary.workflow_files;
            let mut report = files[0].clone();
            report.field_name = "report_file".to_string();
            report.relative_path = "reports/review.md".to_string();
            report.name = "review.md".to_string();
            files.push(report);
        }

        let (lines, openables) =
            super::run_detail::build_selected_step_detail_lines_without_progress_with_openables(
                Some(Path::new("/tmp/.transist")),
                &detail,
                None,
                Some(0),
            );

        assert_eq!(lines.len(), openables.len());
        for file in &detail.steps[0].output_summary.workflow_files {
            let row = lines
                .iter()
                .position(|line| line.contains(&format!("{} (path unavailable)", file.name)))
                .expect("workflow file name with unavailable path");
            assert_eq!(openables[row], None);
            assert!(!lines.iter().any(|line| line.contains(&file.relative_path)));
        }
        assert!(openables.iter().all(Option::is_none));
    }
}

fn cleanup_summary(trigger: &str, state: &str) -> transist::InspectionCleanupSummary {
    transist::InspectionCleanupSummary {
        trigger: trigger.to_string(),
        state: state.to_string(),
        started_at_ms: None,
        finished_at_ms: None,
        preflight: None,
        steps: Vec::new(),
    }
}

#[test]
fn selected_step_details_do_not_render_cleanup_summary_without_virtual_row() {
    let cleanup = transist::InspectionCleanupSummary {
        trigger: "success".to_string(),
        state: "succeeded".to_string(),
        started_at_ms: Some(1_000),
        finished_at_ms: Some(2_000),
        preflight: None,
        steps: vec![transist::InspectionCleanupStep {
            step_id: "cleanup_ok".to_string(),
            status: "success".to_string(),
            execution: Some(1),
            started_at_ms: Some(1_100),
            finished_at_ms: Some(1_900),
            error_message: None,
        }],
    };
    let detail = inspection_run_detail_with_cleanup("demo-run", "success", cleanup);

    let (lines, _openables) =
        super::run_detail::build_selected_step_detail_lines_without_progress_with_openables(
            None,
            &detail,
            None,
            Some(detail.steps.len()),
        );

    assert!(
        !lines.iter().any(|line| line == "Cleanup:"),
        "cleanup should not render as a synthetic details entry: {lines:?}",
    );
}

#[test]
fn canceled_run_actions_include_cleanup_result_viewer() {
    let detail = inspection_run_detail_with_cleanup(
        "demo-run",
        "canceled",
        cleanup_summary("canceled", "succeeded"),
    );
    let actionable = crate::actionable::ActionableRun {
        root_run_id: "demo-run".to_string(),
        run_id: "demo-run".to_string(),
        detail: detail.clone(),
    };

    let actions = super::run_detail::build_run_detail_actions(
        &transist::TransistContext::with_home(
            std::env::current_dir().expect("cwd"),
            std::env::current_dir().expect("cwd"),
        ),
        &detail,
        &actionable,
        "demo-run",
        "canceled",
    )
    .expect("actions build");

    assert!(
        actions.iter().any(|action| matches!(
            action.execution,
            SuggestedActionExecution::InspectCleanupResult { .. }
        )),
        "expected cleanup result viewer action, got {actions:?}",
    );
}

#[test]
fn failed_capability_preflight_includes_remediation_action() {
    let mut detail = inspection_run_detail_with_cleanup(
        "demo-run",
        "failed",
        cleanup_summary("failure", "succeeded"),
    );
    detail.preflight = Some(transist::InspectionPreflightSummary {
        status: "failed".to_string(),
        checked_at_ms: 1,
        blocker_count: 1,
        warning_count: 0,
        failed_checks: vec![transist::InspectionPreflightFailedCheck {
            kind: "step_policy".to_string(),
            message: "code=capability_not_granted: policy.allow.capabilities is missing"
                .to_string(),
            step_ids: vec!["deploy".to_string()],
            plugin_id: None,
            action: None,
        }],
    });
    let actionable = crate::actionable::ActionableRun {
        root_run_id: "demo-run".to_string(),
        run_id: "demo-run".to_string(),
        detail: detail.clone(),
    };

    let actions = super::run_detail::build_run_detail_actions(
        &transist::TransistContext::with_home(
            std::env::current_dir().expect("cwd"),
            std::env::current_dir().expect("cwd"),
        ),
        &detail,
        &actionable,
        "demo-run",
        "failed",
    )
    .expect("actions build");

    assert!(matches!(
        actions.first().map(|action| &action.execution),
        Some(SuggestedActionExecution::ViewFullDiagnostic { run_id }) if run_id == "demo-run"
    ));
    assert!(actions.iter().any(|action| matches!(
        action.execution,
        SuggestedActionExecution::RunsCapabilities { ref run_id } if run_id == "demo-run"
    )));
}
