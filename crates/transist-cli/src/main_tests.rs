// SPDX-License-Identifier: MPL-2.0
use crate::actionable::*;
use crate::cli::Cli;
use crate::commands::dispatch::{
    add_authorization_diagnostics, add_empty_required_input_diagnostics, add_readiness_diagnostics,
    deterministic_pre_dispatch_readiness, dispatch, empty_required_workflow_inputs,
    filter_runs_show_steps, missing_required_workflow_inputs, pre_dispatch_readiness,
    readiness_authorization_available, readiness_check_group, readiness_fingerprint,
    render_authorization_text, render_readiness_text, render_runs_show_request_command,
    structured_bound_validation_output, workflow_readiness_evidence, workflow_readiness_output,
    write_readiness_file,
};
use crate::display::output::*;
use crate::display::progress::*;
use crate::display::spinner::*;
use crate::display::summary::*;
use crate::execution::*;
use crate::guidance::*;
use crate::pager::*;
use crate::prompt::dialoguer::*;
use crate::prompt::scripted::*;
use crate::terminal::*;
use clap::Parser;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use transist::{
    InspectionGate, InspectionRunDetail, InspectionRunSummary, InspectionStep,
    InspectionStepInputSummary, InspectionStepOutputSummary, KernelValue,
    TransistInspectionContext,
};
use transist::{InspectionPreflightFailedCheck, InspectionPreflightSummary, RunCurrentStep};

#[test]
fn fatal_exit_code_in_standalone_mode_stays_one() {
    let usage = anyhow::anyhow!("bad args");
    assert_eq!(
        super::fatal_exit_code_for_mode(transist::ManagementMode::Standalone, &usage),
        1
    );
}

#[test]
fn cli_command_uses_build_specific_version() {
    let command = super::cli_command();
    let version = command.get_version().expect("cli version");
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            let _ = chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn strip_ansi_lines(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| strip_ansi(&line)).collect()
}

fn init_standalone_home(home_dir: &std::path::Path) {
    transist::init_home_dir_with_mode(home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init standalone test home");
}

#[test]
fn command_detaches_runner_identifies_detaching_commands() {
    let workflow_run = Cli::parse_from([
        "transist",
        "workflows",
        "run",
        "wf@v1",
        "--run-ref",
        "run-1",
    ]);
    assert!(super::command_detaches_runner(&workflow_run));

    let run = Cli::parse_from(["transist", "runs", "start", "run-1"]);
    assert!(super::command_detaches_runner(&run));

    let resume = Cli::parse_from(["transist", "runs", "resume", "run-1"]);
    assert!(super::command_detaches_runner(&resume));

    let retry = Cli::parse_from(["transist", "runs", "retry", "run-1"]);
    assert!(super::command_detaches_runner(&retry));

    let replay = Cli::parse_from(["transist", "runs", "replay", "run-1"]);
    assert!(super::command_detaches_runner(&replay));
}

#[test]
fn command_detaches_runner_ignores_non_detaching_commands() {
    let list = Cli::parse_from(["transist", "runs", "list"]);
    assert!(!super::command_detaches_runner(&list));

    let status = Cli::parse_from(["transist", "runs", "status", "run-1"]);
    assert!(!super::command_detaches_runner(&status));

    let run_runner = Cli::parse_from(["transist", "runs", "start", "--runner", "run-1"]);
    assert!(!super::command_detaches_runner(&run_runner));
}

#[test]
fn filter_runs_show_steps_keeps_non_step_fields_and_matches_failed_only() {
    let filtered = filter_runs_show_steps(
        serde_json::json!({
            "run_id": "demo-run",
            "status": "failed",
            "steps": {
                "prepare": { "status": "succeeded" },
                "verify": { "status": "failed", "executions": 2 },
                "finish": { "status": "pending" }
            }
        }),
        RunsShowStepFilter::Failed,
    );

    assert_eq!(filtered["run_id"], "demo-run");
    assert_eq!(filtered["status"], "failed");
    assert!(filtered["steps"].get("prepare").is_none());
    assert_eq!(filtered["steps"]["verify"]["status"], "failed");
    assert_eq!(filtered["steps"]["verify"]["executions"], 2);
    assert!(filtered["steps"].get("finish").is_none());
}

#[test]
fn filter_runs_show_steps_matches_success_and_succeeded_statuses() {
    let filtered = filter_runs_show_steps(
        serde_json::json!({
            "steps": {
                "control": { "status": "success" },
                "invoke": { "status": "succeeded" },
                "verify": { "status": "failed" }
            }
        }),
        RunsShowStepFilter::Success,
    );

    assert_eq!(
        filtered["steps"].as_object().expect("steps object").len(),
        2
    );
    assert_eq!(filtered["steps"]["control"]["status"], "success");
    assert_eq!(filtered["steps"]["invoke"]["status"], "succeeded");
    assert!(filtered["steps"].get("verify").is_none());
}

#[test]
fn filter_runs_show_steps_leaves_unusual_payloads_unchanged() {
    let no_steps = serde_json::json!({ "run_id": "demo-run" });
    let non_object_steps = serde_json::json!({ "steps": [] });

    assert_eq!(
        filter_runs_show_steps(no_steps.clone(), RunsShowStepFilter::Failed),
        no_steps
    );
    assert_eq!(
        filter_runs_show_steps(non_object_steps.clone(), RunsShowStepFilter::Success),
        non_object_steps
    );
}

#[test]
fn render_runs_show_request_command_appends_explicit_all_flag() {
    assert_eq!(
        render_runs_show_request_command("demo-run", RunsShowStepFilter::All, false),
        "transist runs show demo-run"
    );
    assert_eq!(
        render_runs_show_request_command("demo-run", RunsShowStepFilter::All, true),
        "transist runs show demo-run --all"
    );
    assert_eq!(
        render_runs_show_request_command("demo-run", RunsShowStepFilter::Failed, false),
        "transist runs show demo-run --failed"
    );
    assert_eq!(
        render_runs_show_request_command("demo-run", RunsShowStepFilter::Success, false),
        "transist runs show demo-run --success"
    );
}

#[test]
fn running_focus_dimming_targets_only_other_step_lines() {
    assert!(should_dim_line_for_running_focus(
        Some("implementation_loop"),
        Some("plan_loop"),
    ));
    assert!(should_dim_line_for_running_focus(
        Some("implementation_loop"),
        Some("finish"),
    ));
    assert!(!should_dim_line_for_running_focus(
        Some("implementation_loop"),
        Some("implementation_loop"),
    ));
    assert!(!should_dim_line_for_running_focus(
        Some("implementation_loop"),
        None,
    ));
    assert!(!should_dim_line_for_running_focus(None, Some("plan_loop")));
}

#[test]
fn preview_text_is_capped_by_requested_limit() {
    let preview = format_preview_text("abcdefghijklmnopqrstuvwxyz", 12).expect("preview text");
    assert_eq!(preview, "abcdefghijkl...");
}

#[test]
fn preview_body_limit_accounts_for_tool_prefix() {
    let limit = preview_body_limit("github.copilot.cli");
    assert!(limit <= 150);
    assert!(limit >= 12);
}

#[test]
fn step_tool_name_from_run_falls_back_to_preflight_checks() {
    let run = serde_json::json!({
        "steps": {
            "wait_step": {
                "status": "running"
            }
        },
        "preflight": {
            "checks": [
                {
                    "plugin_id": "shell",
                    "step_ids": ["wait_step"]
                }
            ]
        }
    });

    assert_eq!(
        step_tool_name_from_run(&run, "wait_step").as_deref(),
        Some("shell")
    );
}

#[test]
fn safe_terminal_columns_reserves_one_column_for_redraw_stability() {
    assert_eq!(safe_terminal_columns(80), 79);
    assert_eq!(safe_terminal_columns(20), 20);
}

#[test]
fn rendered_terminal_rows_accounts_for_wrapped_ansi_lines() {
    let lines = vec![
        "\u{1b}[32m[✔]\u{1b}[0m fetch_jira_issue (1s)".to_string(),
        "      '/tmp/very-long-artifact-path.md'".to_string(),
        String::new(),
    ];

    assert_eq!(rendered_terminal_rows_for_columns(&lines, 20), 5);
}

#[test]
fn wrap_terminal_render_line_preserves_indentation_across_chunks() {
    let wrapped = wrap_terminal_render_line("    123456789", 8);

    assert_eq!(
        wrapped,
        vec![
            "    1234".to_string(),
            "    5678".to_string(),
            "    9".to_string(),
        ]
    );
}

#[test]
fn wrap_attached_step_output_line_preserves_single_quoted_paths() {
    let line = "      '/tmp/very/long/path/validation_report.md'";

    assert_eq!(
        wrap_attached_step_output_line(line, 12),
        vec![line.to_string()]
    );
}

#[test]
fn wrap_attached_step_output_line_still_wraps_non_path_text() {
    assert_eq!(
        wrap_attached_step_output_line("    Workflow files:", 8),
        vec![
            "    Work".to_string(),
            "    flow".to_string(),
            "     fil".to_string(),
            "    es:".to_string(),
        ]
    );
}

#[test]
fn wrap_attached_step_output_line_preserves_embedded_quoted_path() {
    // Lines that embed a single-quoted absolute path (e.g.
    // `  workflow.spec_path: '/tmp/very/long/path/spec.md' (123 bytes)`)
    // must stay intact; hard-wrapping mid-character would split the path
    // and break visual scan plus any future click-to-open behavior.
    let line = "  workflow.spec_path: '/tmp/very/long/path/spec.md' (123 bytes)";
    assert_eq!(
        wrap_attached_step_output_line(line, 16),
        vec![line.to_string()]
    );
}

#[test]
fn wrap_pager_body_preserves_lines_with_embedded_quoted_paths() {
    let body =
        "intro line\n  workflow.spec_path: '/tmp/very/long/path/spec.md' (123 bytes)\nfinal line";
    let wrapped = crate::pager::wrap_pager_body(body, 16);
    assert!(
        wrapped.contains(
            &"  workflow.spec_path: '/tmp/very/long/path/spec.md' (123 bytes)".to_string()
        ),
        "expected path-bearing line to be preserved verbatim, got: {:?}",
        wrapped
    );
    // And non-path lines that still exceed the width must still be wrapped.
    assert!(wrapped.iter().any(|line| line == "intro line"));
}

#[test]
fn picker_dismissed_by_interrupt_only_marks_interrupted_reads() {
    assert!(picker_dismissed_by_interrupt(&io::Error::from(
        io::ErrorKind::Interrupted
    )));
    assert!(!picker_dismissed_by_interrupt(&io::Error::from(
        io::ErrorKind::UnexpectedEof
    )));
}

#[test]
fn render_plain_interactive_progress_keeps_only_the_live_tail_visible() {
    let progress = LiveProgress {
        state: ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> review_plan".to_string()),
            current_step_status: Some("running".to_string()),
            execution: None,
            cycle: None,
            tool_name: Some("openspec".to_string()),
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "fetch_jira_issue".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "plan_loop".to_string(),
                    state: ChecklistState::Active,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: Some("status".to_string()),
        preview_text: Some("Applying selected command".to_string()),
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    assert_eq!(
        strip_ansi_lines(render_plain_interactive_progress(&progress)),
        vec![
            "[>] plan_loop".to_string(),
            "    openspec: \"Applying selected command...\"".to_string(),
        ]
    );
}

#[test]
fn running_checklist_row_shows_elapsed_seconds() {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let progress = LiveProgress {
        state: ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "demo-run".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> review_plan".to_string()),
            current_step_status: Some("running".to_string()),
            execution: None,
            cycle: None,
            tool_name: Some("openspec".to_string()),
            started_at_ms: Some(now_ms.saturating_sub(42_000)),
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "plan_loop".to_string(),
                    state: ChecklistState::Active,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: Some("status".to_string()),
        preview_text: Some("Applying selected command".to_string()),
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };

    let lines = strip_ansi_lines(render_plain_interactive_progress(&progress));
    assert!(lines[0].starts_with("[>] plan_loop ("));
    assert!(lines[0].contains(" active)"));
}

#[test]
fn blocked_without_gate_guidance_explains_run_command() {
    let lines = blocked_without_gate_guidance("demo-run");

    assert_eq!(
        lines[0],
        "Outcome: the run is blocked before the actionable subflow reached a manual gate."
    );
    assert_eq!(
        lines[1],
        "No approve/request-changes/reject action is available yet."
    );
    assert_eq!(
        lines[2],
        "Continue execution without recording another decision:"
    );
    assert_eq!(lines[3], "  transist runs start demo-run");
    assert_eq!(
        lines[4],
        "This command just keeps the run moving until the next gate or terminal state."
    );
}

#[test]
fn blocked_gate_summary_builds_interactive_picker_actions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    init_standalone_home(&home_dir);
    let mut home_config = fs::read_to_string(home_dir.join("config.yaml")).expect("home config");
    home_config.push_str("\nmanual_gates:\n  actor: \"Reviewer <reviewer@example.com>\"\n");
    fs::write(home_dir.join("config.yaml"), home_config).expect("configure gate actor");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: demo
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: finish
    control: control.finish
"#,
    )
    .expect("workflow");
    context.compile_flow_file(&flow).expect("compile");
    let mut actionable = actionable_run(
        "demo-run",
        "demo-run",
        "blocked",
        vec![
            inspection_step("review", "blocked"),
            inspection_step("finish", "pending"),
        ],
        Some("review"),
    );
    actionable.detail.blocked_gates = vec![inspection_gate(
        "review",
        BTreeMap::from([(
            "operator_guidance".to_string(),
            KernelValue::Object(BTreeMap::from([(
                "conditions".to_string(),
                KernelValue::String("Check the draft".to_string()),
            )])),
        )]),
    )];

    let summary =
        build_blocked_summary_content(&context, &actionable.detail, "demo-run", &actionable)
            .expect("summary");

    assert!(
        summary
            .body_lines
            .contains(&"Blocked on gate: review".to_string())
    );
    assert!(
        summary
            .step_output_lines
            .contains(&"Conditions:".to_string())
    );
    assert!(summary.interactive_action_lines.contains(
        &"  approve: accept this gate and continue the run, then finish this workflow".to_string()
    ));
    assert!(
        summary
            .interactive_action_lines
            .contains(&"Exit:".to_string())
    );
    assert!(
        summary
            .interactive_action_lines
            .contains(&"  leave this blocked run as-is and return to the shell".to_string())
    );
    assert_eq!(summary.suggested_actions.len(), 4);
    assert_eq!(
        summary.suggested_actions[0].display_command,
        "transist runs gate approve demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"Reviewed output and approved\""
    );
    assert_eq!(
        summary.suggested_actions[1].display_command,
        "transist runs gate request-changes demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"...\""
    );
    assert_eq!(
        summary.suggested_actions[2].display_command,
        "transist runs gate reject demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"stop the run\""
    );
    assert_eq!(summary.suggested_actions[3].display_command, "exit the run");
}

#[test]
fn blocked_gate_summary_keeps_workflow_files_attached_with_guidance() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let context = transist::TransistContext::with_home(tmp.path(), tmp.path().join(".transist"));
    let mut review_step = inspection_step("review", "blocked");
    review_step.output_summary.workflow_files = vec![transist::InspectionWorkflowFileRef {
        step_id: "review".to_string(),
        field_name: "report_file".to_string(),
        locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
        relative_path: "review_plan.md".to_string(),
        local_path: Some(std::path::PathBuf::from("/tmp/review_plan.md")),
        name: "review_plan.md".to_string(),
        bytes: 128,
    }];
    let mut actionable = actionable_run(
        "demo-run",
        "demo-run",
        "blocked",
        vec![review_step],
        Some("review"),
    );
    actionable.detail.blocked_gates = vec![inspection_gate(
        "review",
        BTreeMap::from([(
            "operator_guidance".to_string(),
            KernelValue::Object(BTreeMap::from([(
                "conditions".to_string(),
                KernelValue::String("Check the draft".to_string()),
            )])),
        )]),
    )];

    let summary =
        build_blocked_summary_content(&context, &actionable.detail, "demo-run", &actionable)
            .expect("summary");

    let conditions_index = summary
        .step_output_lines
        .iter()
        .position(|line| line == "Conditions:")
        .expect("conditions header");
    assert!(
        summary
            .step_output_lines
            .get(conditions_index + 1)
            .map(|line| line == "  Check the draft")
            .unwrap_or(false),
        "missing rendered guidance: {:#?}",
        summary.step_output_lines
    );
    assert!(
        summary
            .step_output_lines
            .contains(&"Workflow files:".to_string())
    );
    assert!(
        summary
            .step_output_lines
            .iter()
            .any(|line| line.contains("/tmp/review_plan.md"))
    );
}

#[test]
fn blocked_child_gate_summary_includes_prior_child_step_workflow_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    init_standalone_home(&home_dir);
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: demo
version: v1
steps:
  - id: plan_loop
    control: control.noop
    loop:
      body:
        - ref: generate_plan
        - ref: review_plan
      until: 'steps.review_plan.out.decision == "approved"'
      max_iters: 4
      max_duration: 1h
  - id: generate_plan
    control: control.noop
  - id: review_plan
    control: control.gate.manual
    with:
      gate_id: plan-review
  - id: finish
    control: control.finish
"#,
    )
    .expect("workflow");
    context.compile_flow_file(&flow).expect("compile");
    let mut generate_plan = inspection_step("generate_plan", "succeeded");
    generate_plan.output_summary.workflow_files = vec![
        transist::InspectionWorkflowFileRef {
            step_id: "generate_plan".to_string(),
            field_name: "response_file".to_string(),
            locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
            relative_path: "plan.md".to_string(),
            local_path: Some(std::path::PathBuf::from("/tmp/plan.md")),
            name: "plan.md".to_string(),
            bytes: 256,
        },
        transist::InspectionWorkflowFileRef {
            step_id: "generate_plan".to_string(),
            field_name: "change_metadata_file".to_string(),
            locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
            relative_path: "openspec_change.json".to_string(),
            local_path: Some(std::path::PathBuf::from("/tmp/openspec_change.json")),
            name: "openspec_change.json".to_string(),
            bytes: 256,
        },
    ];
    let review_plan = inspection_step("review_plan", "blocked");
    let finish = inspection_step("finish", "pending");
    let mut actionable = actionable_run(
        "demo-run",
        "demo-run__plan_loop",
        "blocked",
        vec![generate_plan, review_plan, finish],
        Some("review_plan"),
    );
    let mut gate = inspection_gate(
        "plan-review",
        BTreeMap::from([(
            "operator_guidance".to_string(),
            KernelValue::Object(BTreeMap::from([(
                "next".to_string(),
                KernelValue::Array(vec![KernelValue::String(
                    "approve: accept this gate and continue the run".to_string(),
                )]),
            )])),
        )]),
    );
    gate.step_id = "review_plan".to_string();
    actionable.detail.blocked_gates = vec![gate];

    let summary =
        build_blocked_summary_content(&context, &actionable.detail, "demo-run", &actionable)
            .expect("summary");

    assert!(
        summary
            .step_output_lines
            .contains(&"Workflow files:".to_string())
    );
    assert!(
        summary
            .step_output_lines
            .iter()
            .any(|line| line.contains("/tmp/plan.md"))
    );
    assert!(
        summary
            .interactive_action_lines
            .contains(&"  approve: accept this cycle and finish the loop".to_string())
    );
    assert!(summary.interactive_action_lines.contains(
        &"  request-changes: rerun this cycle with an optional reason, then continue at generate_plan".to_string()
    ));
    assert!(
        summary
            .step_output_lines
            .iter()
            .any(|line| line.contains("/tmp/openspec_change.json"))
    );
}

#[test]
fn failed_summary_deduplicates_resume_command_for_picker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let context = transist::TransistContext::with_home(tmp.path(), tmp.path().join(".transist"));
    let mut root_detail = inspection_run_detail(vec![inspection_step("verify", "failed")]);
    root_detail.summary.status = "failed".to_string();
    let actionable = actionable_run(
        "demo-run",
        "demo-run",
        "failed",
        vec![inspection_step("verify", "failed")],
        Some("verify"),
    );

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    assert!(
        summary
            .interactive_action_lines
            .contains(&"Retry:".to_string())
    );
    assert!(
        summary
            .interactive_action_lines
            .contains(&"Inspect:".to_string())
    );
    assert!(
        summary
            .interactive_action_lines
            .contains(&"Exit:".to_string())
    );
    assert_eq!(summary.suggested_actions.len(), 5);
    assert_eq!(
        summary.suggested_actions[0].display_command,
        "transist runs retry demo-run"
    );
    assert_eq!(
        summary.suggested_actions[1].display_command,
        "transist runs cleanup demo-run"
    );
    assert_eq!(
        summary.suggested_actions[2].display_command,
        "inspect failed step details"
    );
    assert_eq!(
        summary.suggested_actions[3].display_command,
        "inspect failed step events"
    );
    assert_eq!(summary.suggested_actions[4].display_command, "exit the run");
}

#[test]
fn failed_summary_swaps_events_for_stderr_action_when_stderr_artifact_exists() {
    // When the failed step produced a `stderr.txt` artifact, the picker should
    // surface a direct stderr inspector instead of the generic events pager —
    // that's almost always the most useful next click for a failed process.
    let tmp = tempfile::tempdir().expect("tempdir");
    let context = transist::TransistContext::with_home(tmp.path(), tmp.path().join(".transist"));
    let mut step = inspection_step("verify", "failed");
    step.output_summary.artifact_count = 1;
    step.output_summary.artifact_refs = vec![transist::InspectionStepBlobRef {
        step_id: "verify".to_string(),
        execution: 1,
        relative_path: "stderr.txt".to_string(),
        name: "stderr.txt".to_string(),
        bytes: 42,
    }];
    let mut root_detail = inspection_run_detail(vec![step.clone()]);
    root_detail.summary.status = "failed".to_string();
    let actionable = actionable_run("demo-run", "demo-run", "failed", vec![step], Some("verify"));

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    let labels: Vec<&str> = summary
        .suggested_actions
        .iter()
        .map(|action| action.display_command.as_str())
        .collect();
    assert!(
        labels.contains(&"inspect failed step stderr"),
        "expected stderr action, got labels: {:?}",
        labels
    );
    assert!(
        !labels.contains(&"inspect failed step events"),
        "expected events action to be swapped out, got labels: {:?}",
        labels
    );
}

#[test]
fn failed_summary_keeps_events_action_when_no_stderr_artifact() {
    // Sanity check the negative case: a failed step with no stderr-style
    // artifact must still get the generic events pager, never lose the
    // inspection action entirely.
    let tmp = tempfile::tempdir().expect("tempdir");
    let context = transist::TransistContext::with_home(tmp.path(), tmp.path().join(".transist"));
    let mut step = inspection_step("verify", "failed");
    step.output_summary.artifact_count = 1;
    step.output_summary.artifact_refs = vec![transist::InspectionStepBlobRef {
        step_id: "verify".to_string(),
        execution: 1,
        relative_path: "result.json".to_string(),
        name: "result.json".to_string(),
        bytes: 7,
    }];
    let mut root_detail = inspection_run_detail(vec![step.clone()]);
    root_detail.summary.status = "failed".to_string();
    let actionable = actionable_run("demo-run", "demo-run", "failed", vec![step], Some("verify"));

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    let labels: Vec<&str> = summary
        .suggested_actions
        .iter()
        .map(|action| action.display_command.as_str())
        .collect();
    assert!(labels.contains(&"inspect failed step events"));
    assert!(!labels.contains(&"inspect failed step stderr"));
}

#[test]
fn failed_summary_omits_resume_when_cleanup_already_completed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let context = transist::TransistContext::with_home(tmp.path(), tmp.path().join(".transist"));
    let mut root_detail = inspection_run_detail(vec![inspection_step("verify", "failed")]);
    root_detail.summary.status = "failed".to_string();
    root_detail.recovery_state = Some("cleanup_completed".to_string());
    let actionable = actionable_run(
        "demo-run",
        "demo-run",
        "failed",
        vec![inspection_step("verify", "failed")],
        Some("verify"),
    );

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    assert!(
        summary.body_lines.iter().any(
            |line| line.contains("Cleanup is complete; the failed run is no longer resumable.")
        )
    );
    assert!(
        !summary
            .interactive_action_lines
            .contains(&"Retry:".to_string())
    );
    assert_eq!(
        summary
            .suggested_actions
            .iter()
            .map(|action| action.display_command.as_str())
            .collect::<Vec<_>>(),
        vec![
            "inspect failed step details",
            "inspect failed step events",
            "exit the run"
        ]
    );
}

#[test]
fn failed_summary_recommends_cleanup_retry_when_cleanup_is_required() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let context = transist::TransistContext::with_home(tmp.path(), tmp.path().join(".transist"));
    let mut root_detail = inspection_run_detail(vec![inspection_step("verify", "failed")]);
    root_detail.summary.status = "failed".to_string();
    root_detail.recovery_state = Some("cleanup_required".to_string());
    let actionable = actionable_run(
        "demo-run",
        "demo-run",
        "failed",
        vec![inspection_step("verify", "failed")],
        Some("verify"),
    );

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    assert!(
        summary
            .body_lines
            .iter()
            .any(|line| line.contains("Retry cleanup"))
    );
    assert!(
        !summary
            .interactive_action_lines
            .contains(&"Retry:".to_string())
    );
    assert_eq!(
        summary.suggested_actions[0].display_command,
        "transist runs cleanup demo-run"
    );
}

#[test]
fn compact_status_cleanup_advice_distinguishes_required_and_completed_cleanup() {
    for (recovery_state, expected_cleanup, expected_retry, expected_headline) in [
        (
            "cleanup_required",
            true,
            false,
            "The failed run is no longer resumable because explicit cleanup has begun. Retry cleanup until it succeeds or is skipped.",
        ),
        (
            "cleanup_completed",
            false,
            false,
            "Cleanup is complete; the failed run is no longer resumable.",
        ),
    ] {
        let mut root_detail = inspection_run_detail(vec![inspection_step("verify", "failed")]);
        root_detail.summary.status = "failed".to_string();
        root_detail.recovery_state = Some(recovery_state.to_string());
        let actionable = actionable_run(
            "demo-run",
            "demo-run",
            "failed",
            vec![inspection_step("verify", "failed")],
            Some("verify"),
        );

        let advice =
            build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

        assert_eq!(advice.headline, expected_headline);
        assert_eq!(
            advice
                .suggested_commands
                .iter()
                .any(|command| command.contains("runs cleanup")),
            expected_cleanup,
        );
        assert_eq!(
            advice
                .suggested_commands
                .iter()
                .any(|command| command.contains("runs retry")),
            expected_retry,
        );
    }
}

#[test]
fn preflight_failed_summary_orders_inspect_before_resume_actions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let context = transist::TransistContext::with_home(tmp.path(), tmp.path().join(".transist"));
    let mut root_detail = inspection_run_detail(Vec::new());
    root_detail.summary.status = "failed".to_string();
    root_detail.preflight = Some(InspectionPreflightSummary {
        status: "failed".to_string(),
        checked_at_ms: 0,
        blocker_count: 2,
        warning_count: 1,
        failed_checks: vec![InspectionPreflightFailedCheck {
            kind: "input.type_mismatch".to_string(),
            message: "input schema validation failed at instance path '/args/0' and schema path '/properties/args/items/type'".to_string(),
            step_ids: vec!["analyze_tickets".to_string()],
            plugin_id: Some("shell".to_string()),
            action: Some("exec".to_string()),
        }],
    });
    let actionable = actionable_run("demo-run", "demo-run", "failed", Vec::new(), None);

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    assert_eq!(
        summary
            .suggested_actions
            .iter()
            .map(|action| action.display_command.as_str())
            .collect::<Vec<_>>(),
        vec![
            "transist runs show demo-run --failed",
            "transist runs preflight demo-run",
            "transist runs retry demo-run",
            "exit the run",
        ]
    );
}

#[test]
fn compact_status_for_prepared_run_suggests_starting_commands() {
    let mut root_detail = inspection_run_detail(Vec::new());
    root_detail.summary.status = "planned".to_string();
    root_detail.summary.run_id = "demo-run".to_string();
    root_detail.global_default_filesystem_policy = Some(inspection_filesystem_policy_summary(
        "global",
        "read_only",
        &[],
        &[],
    ));
    root_detail.workflow_filesystem_policy = Some(inspection_filesystem_policy_summary(
        "workflows",
        "read_only",
        &["/etc/gitconfig"],
        &["~/.cache/transist"],
    ));
    let actionable = actionable_run("demo-run", "demo-run", "planned", Vec::new(), None);

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

    assert_eq!(advice.status, "planned");
    assert_eq!(
        advice.detail_lines,
        vec![
            "Global default policy: source=global, mode=read_only".to_string(),
            "Workflow policy: source=workflows, mode=read_only, read=[/etc/gitconfig], write=[~/.cache/transist]"
                .to_string(),
        ]
    );
    assert_eq!(
        advice.suggested_commands,
        vec![
            "transist runs start demo-run",
            "transist runs preflight demo-run",
            "transist runs show demo-run",
        ]
    );
    assert!(advice.headline.contains("ready to execute"));
}

#[test]
fn compact_status_for_running_and_waiting_runs_stays_inspection_only() {
    for (status, step_status, expected_headline) in [
        (
            "running",
            "running",
            "This run is running. Current step: 'work'.",
        ),
        (
            "waiting",
            "retry_wait",
            "This run is waiting (current step: 'work').",
        ),
    ] {
        let mut root_detail = inspection_run_detail(vec![inspection_step("work", step_status)]);
        root_detail.summary.status = status.to_string();
        root_detail.summary.run_id = "demo-run".to_string();
        let actionable = actionable_run(
            "demo-run",
            "demo-run__child",
            status,
            vec![inspection_step("work", step_status)],
            Some("work"),
        );

        let advice =
            build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

        assert_eq!(advice.current_step.as_deref(), Some("work"));
        assert_eq!(advice.headline, expected_headline);
        assert!(advice.detail_lines.is_empty());
        assert_eq!(
            advice.suggested_commands,
            vec![
                "transist runs show demo-run",
                "transist runs tail demo-run__child --limit 80",
            ],
            "unexpected commands for {status}"
        );
        assert!(
            !advice
                .suggested_commands
                .iter()
                .any(|command| command.contains("retry") || command.contains("approve")),
            "active states should not suggest mutating recovery: {:#?}",
            advice.suggested_commands
        );
    }
}

#[test]
fn progress_state_detail_lines_surface_active_metadata() {
    let progress = ProgressState {
        root_run_id: "demo-run".to_string(),
        run_status: "running".to_string(),
        actionable_run_id: "demo-run__child".to_string(),
        top_level_step_id: Some("implement_plan".to_string()),
        current_step_path: Some("implementation_loop -> implement_plan".to_string()),
        current_step_status: Some("running".to_string()),
        execution: Some(2),
        cycle: Some(3),
        tool_name: Some("github.copilot.cli".to_string()),
        started_at_ms: Some(current_unix_ms().saturating_sub(2_000)),
        finished_at_ms: None,
        error_code: Some("timeout_error".to_string()),
        exit_code: Some(124),
        checklist: Vec::new(),
    };

    let lines = format_progress_state_detail_lines(&progress);

    assert_eq!(
        &lines[..5],
        [
            "Path: implementation_loop -> implement_plan".to_string(),
            "Step state: running".to_string(),
            "Tool: github.copilot.cli".to_string(),
            "Execution: 2".to_string(),
            "Cycle: 3".to_string(),
        ]
    );
    assert_eq!(lines.len(), 6);
    assert!(lines[5].starts_with("Active: "));
    assert!(!lines.iter().any(|line| line.starts_with("Error code: ")));
    assert!(!lines.iter().any(|line| line.starts_with("Exit code: ")));
}

#[test]
fn compact_status_for_blocked_gate_lists_decision_and_inspection_commands() {
    let mut root_detail = inspection_run_detail(vec![inspection_step("review", "blocked")]);
    root_detail.summary.status = "blocked".to_string();
    root_detail.summary.run_id = "demo-run".to_string();
    let mut actionable = actionable_run(
        "demo-run",
        "demo-run__review_flow",
        "blocked",
        vec![inspection_step("review", "blocked")],
        Some("review"),
    );
    actionable.detail.blocked_gates = vec![inspection_gate("review", BTreeMap::new())];

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

    assert_eq!(advice.headline, "Blocked on gate: review.");
    assert_eq!(
        advice.suggested_commands,
        vec![
            "transist runs gate approve demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"Reviewed output and approved\"",
            "transist runs gate request-changes demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"...\"",
            "transist runs gate reject demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"stop the run\"",
            "transist runs show demo-run__review_flow",
            "transist runs tail demo-run__review_flow --limit 80",
        ]
    );
}

#[test]
fn compact_status_for_blocked_without_gate_suggests_reentry() {
    let mut root_detail = inspection_run_detail(vec![inspection_step("child", "blocked")]);
    root_detail.summary.status = "blocked".to_string();
    root_detail.summary.run_id = "demo-run".to_string();
    let actionable = actionable_run(
        "demo-run",
        "demo-run__child",
        "blocked",
        vec![inspection_step("start", "blocked")],
        Some("start"),
    );

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

    assert!(advice.headline.contains("before the actionable subflow"));
    assert_eq!(
        advice.suggested_commands,
        vec![
            "transist runs start demo-run",
            "transist runs show demo-run__child",
            "transist runs tail demo-run__child --limit 80",
        ]
    );
}

#[test]
fn compact_status_for_failed_preflight_orders_inspect_resume_and_replay() {
    let mut root_detail = inspection_run_detail(Vec::new());
    root_detail.summary.status = "failed".to_string();
    root_detail.summary.run_id = "demo-run".to_string();
    root_detail.preflight = Some(InspectionPreflightSummary {
        status: "failed".to_string(),
        checked_at_ms: 0,
        blocker_count: 2,
        warning_count: 1,
        failed_checks: vec![InspectionPreflightFailedCheck {
            kind: "input.type_mismatch".to_string(),
            message: "input schema validation failed at instance path '/args/0' and schema path '/properties/args/items/type'".to_string(),
            step_ids: vec!["analyze_tickets".to_string()],
            plugin_id: Some("shell".to_string()),
            action: Some("exec".to_string()),
        }],
    });
    let actionable = actionable_run("demo-run", "demo-run", "failed", Vec::new(), None);

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

    assert_eq!(advice.headline, "Preflight failed before step dispatch.");
    assert_eq!(
        advice.detail_lines,
        vec![
            "Preflight blockers: 2 blocker(s), 1 warning(s)".to_string(),
            "  - input.type_mismatch (shell/exec) [analyze_tickets]: input schema validation failed at instance path '/args/0' and schema path '/properties/args/items/type'".to_string(),
        ]
    );
    assert_eq!(
        advice.suggested_commands,
        vec![
            "transist runs show demo-run --failed",
            "transist runs preflight demo-run",
            "transist runs retry demo-run",
            "transist runs replay demo-run --run-ref <new-run-id>",
        ]
    );
}

#[test]
fn compact_status_for_failed_nested_run_surfaces_child_preflight_blocker_reason() {
    // Parity with the TUI Details pane: when the child failed at preflight,
    // the plain-text "Failure reason" must surface the specific first
    // preflight blocker, not the generic "preflight failed before step
    // dispatch" marker.
    let mut failed_step = inspection_step("plan_loop", "failed");
    failed_step.output_summary.child_failure = Some(transist::InspectionChildFailure {
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
    });
    let mut root_detail = inspection_run_detail(vec![inspection_step("plan_loop", "failed")]);
    root_detail.summary.status = "failed".to_string();
    root_detail.summary.run_id = "demo-run".to_string();
    let actionable = actionable_run(
        "demo-run",
        "demo-run__plan_loop",
        "failed",
        vec![failed_step],
        Some("plan_loop"),
    );

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

    assert_eq!(
        advice.detail_lines,
        vec![
            "Failed step: plan_loop",
            "Child run: demo-run__plan_loop",
            "Failure reason: input schema validation failed at instance path '/model' and schema path '/properties/model/type'",
        ]
    );
}

#[test]
fn compact_status_for_failed_nested_run_resumes_root_and_inspects_child() {
    let mut failed_step = inspection_step("verify", "failed");
    failed_step.output_summary.child_failure = Some(transist::InspectionChildFailure {
        child_run_id: "demo-run__child__review".to_string(),
        failed_step_id: Some("review".to_string()),
        error_code: Some("validation_error".to_string()),
        error_message: Some("missing marker".to_string()),
        stderr: None,
        preflight_blocker_count: None,
        preflight_warning_count: None,
        preflight_first_blocker: None,
    });
    let mut root_detail = inspection_run_detail(vec![inspection_step("child", "failed")]);
    root_detail.summary.status = "failed".to_string();
    root_detail.summary.run_id = "demo-run".to_string();
    let actionable = actionable_run(
        "demo-run",
        "demo-run__child",
        "failed",
        vec![failed_step],
        Some("verify"),
    );

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");

    assert_eq!(advice.current_step.as_deref(), Some("verify"));
    assert!(
        advice
            .headline
            .contains("child subflow run demo-run__child failed")
    );
    assert_eq!(
        advice.detail_lines,
        vec![
            "Failed step: review",
            "Child run: demo-run__child__review",
            "Failure reason: missing marker"
        ]
    );
    assert_eq!(
        advice.suggested_commands,
        vec![
            "transist runs retry demo-run",
            "transist runs cleanup demo-run",
            "transist runs show demo-run__child --failed",
            "transist runs tail demo-run__child --limit 80",
            "transist runs replay demo-run --run-ref <new-run-id>",
        ]
    );
}

#[test]
fn failed_summary_prefers_persisted_child_failure_for_nested_subflows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let mut failed_step = inspection_step("plan_loop", "failed");
    failed_step.output_summary.child_run_id = Some("demo-run__plan_loop".to_string());
    failed_step.output_summary.child_failure = Some(transist::InspectionChildFailure {
        child_run_id: "demo-run__plan_loop__generate_plan".to_string(),
        failed_step_id: Some("generate_plan".to_string()),
        error_code: Some("tool_execution_error".to_string()),
        error_message: Some("deep child failure".to_string()),
        stderr: Some("deep stderr".to_string()),
        preflight_blocker_count: None,
        preflight_warning_count: None,
        preflight_first_blocker: None,
    });
    let mut root_detail = inspection_run_detail(vec![failed_step.clone()]);
    root_detail.summary.status = "failed".to_string();
    let actionable = actionable_run(
        "demo-run",
        "demo-run__plan_loop",
        "failed",
        vec![failed_step],
        Some("plan_loop"),
    );

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    assert_eq!(
        summary.step_output_lines[0..3],
        [
            "Failed step: generate_plan".to_string(),
            "Child run: demo-run__plan_loop__generate_plan".to_string(),
            "Failure reason: deep child failure".to_string(),
        ]
    );
}

#[test]
fn compact_status_matches_operator_summary_for_one_level_nested_failure() {
    // 1-level nesting: parent's failed subflow step points at a direct child
    // run whose own failed step is the root cause. Both `runs status` (advice)
    // and the operator summary must render the same first-line reason and
    // pointer taken from `child_failure`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);

    let mut failed_step = inspection_step("verify", "failed");
    failed_step.output_summary.child_run_id = Some("demo-run__verify".to_string());
    failed_step.output_summary.child_failure = Some(transist::InspectionChildFailure {
        child_run_id: "demo-run__verify".to_string(),
        failed_step_id: Some("check".to_string()),
        error_code: Some("tool_execution_error".to_string()),
        error_message: Some("check failed".to_string()),
        stderr: Some("check stderr".to_string()),
        preflight_blocker_count: None,
        preflight_warning_count: None,
        preflight_first_blocker: None,
    });

    let mut root_detail = inspection_run_detail(vec![failed_step.clone()]);
    root_detail.summary.status = "failed".to_string();
    root_detail.summary.run_id = "demo-run".to_string();

    let actionable = actionable_run(
        "demo-run",
        "demo-run__verify",
        "failed",
        vec![failed_step],
        Some("verify"),
    );

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");
    assert_eq!(
        advice.detail_lines,
        vec![
            "Failed step: check",
            "Child run: demo-run__verify",
            "Failure reason: check failed",
        ]
    );

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");
    assert_eq!(
        summary.step_output_lines[0..3],
        [
            "Failed step: check".to_string(),
            "Child run: demo-run__verify".to_string(),
            "Failure reason: check failed".to_string(),
        ]
    );
}

#[test]
fn compact_status_matches_operator_summary_for_two_level_nested_failure() {
    // 2-level nesting: parent's failed subflow step propagated a nested
    // failure from a grandchild run. Both `runs status` (advice) and the
    // operator summary must reflect the deepest failed step id + grandchild
    // run id verbatim from `child_failure`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);

    let mut failed_step = inspection_step("plan_loop", "failed");
    failed_step.output_summary.child_run_id = Some("demo-run__plan_loop".to_string());
    failed_step.output_summary.child_failure = Some(transist::InspectionChildFailure {
        child_run_id: "demo-run__plan_loop__generate_plan".to_string(),
        failed_step_id: Some("generate_plan".to_string()),
        error_code: Some("tool_execution_error".to_string()),
        error_message: Some("deep child failure".to_string()),
        stderr: Some("deep stderr".to_string()),
        preflight_blocker_count: None,
        preflight_warning_count: None,
        preflight_first_blocker: None,
    });

    let mut root_detail = inspection_run_detail(vec![failed_step.clone()]);
    root_detail.summary.status = "failed".to_string();
    root_detail.summary.run_id = "demo-run".to_string();

    let actionable = actionable_run(
        "demo-run",
        "demo-run__plan_loop",
        "failed",
        vec![failed_step],
        Some("plan_loop"),
    );

    let advice =
        build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");
    assert_eq!(
        advice.detail_lines,
        vec![
            "Failed step: generate_plan",
            "Child run: demo-run__plan_loop__generate_plan",
            "Failure reason: deep child failure",
        ]
    );

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");
    assert_eq!(
        summary.step_output_lines[0..3],
        [
            "Failed step: generate_plan".to_string(),
            "Child run: demo-run__plan_loop__generate_plan".to_string(),
            "Failure reason: deep child failure".to_string(),
        ]
    );
}

#[test]
fn compact_status_for_terminal_runs_suggests_inspection_and_replay() {
    let cases: [(&str, &str, Vec<&str>); 2] = [
        (
            "success",
            "The run completed successfully.",
            vec![
                "transist runs show demo-run",
                "transist runs tail demo-run --limit 80",
                "transist runs replay demo-run --run-ref <new-run-id>",
            ],
        ),
        (
            "canceled",
            "The run was canceled. Use `transist runs retry` to continue from the canceled step.",
            vec![
                "transist runs retry demo-run",
                "transist runs show demo-run",
                "transist runs tail demo-run --limit 80",
                "transist runs replay demo-run --run-ref <new-run-id>",
            ],
        ),
    ];
    for (status, headline, expected_commands) in cases {
        let mut root_detail = inspection_run_detail(Vec::new());
        root_detail.summary.status = status.to_string();
        root_detail.summary.run_id = "demo-run".to_string();
        let actionable = actionable_run("demo-run", "demo-run", status, Vec::new(), None);

        let advice =
            build_compact_run_status_advice_from_details(&root_detail, &actionable, "demo-run");
        let lines = render_compact_run_status_advice(&advice);

        assert_eq!(advice.headline, headline);
        assert_eq!(
            advice.suggested_commands,
            expected_commands
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert!(lines.contains(&format!("Summary: {headline}")));
        assert!(!lines.iter().any(|line| line.contains("Component versions")));
    }
}

#[test]
fn failed_summary_lists_failed_step_artifact_and_evidence_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let mut failed_step = inspection_step("validate_changes", "failed");
    failed_step.output_summary.workflow_files = vec![transist::InspectionWorkflowFileRef {
        step_id: "validate_changes".to_string(),
        field_name: "report_file".to_string(),
        locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
        relative_path: "validation_report.md".to_string(),
        local_path: Some(std::path::PathBuf::from("/tmp/validation_report.md")),
        name: "validation_report.md".to_string(),
        bytes: 128,
    }];
    failed_step.output_summary.artifact_refs = vec![transist::InspectionStepBlobRef {
        step_id: "validate_changes".to_string(),
        execution: 2,
        relative_path: "000_stdout.txt".to_string(),
        name: "000_stdout.txt".to_string(),
        bytes: 10,
    }];
    failed_step.output_summary.evidence_refs = vec![transist::InspectionStepBlobRef {
        step_id: "validate_changes".to_string(),
        execution: 2,
        relative_path: "transist_runtime_invoke_evidence.json".to_string(),
        name: "transist_runtime_invoke_evidence.json".to_string(),
        bytes: 10,
    }];
    let mut root_detail = inspection_run_detail(vec![failed_step.clone()]);
    root_detail.summary.status = "failed".to_string();
    let actionable = actionable_run(
        "demo-run",
        "demo-run",
        "failed",
        vec![failed_step],
        Some("validate_changes"),
    );

    let summary = build_failed_summary_content(&context, &root_detail, &actionable, "demo-run");

    assert!(
        summary
            .step_output_lines
            .contains(&"Workflow files:".to_string())
    );
    assert!(
        summary
            .step_output_lines
            .contains(&"Artifacts:".to_string())
    );
    assert!(summary.step_output_lines.contains(&"Evidence:".to_string()));
    assert!(
        summary
            .step_output_lines
            .iter()
            .any(|line| line.contains("/tmp/validation_report.md"))
    );
    assert!(summary.step_output_lines.iter().any(|line| line.contains(
        "run_demo-run/artifacts/validate_changes/execution_002/artifacts/000_stdout.txt"
    )));
    assert!(summary.step_output_lines.iter().any(|line| line.contains(
            "run_demo-run/artifacts/validate_changes/execution_002/evidence/transist_runtime_invoke_evidence.json"
        )));
}

#[test]
fn resolve_selected_action_applies_prompted_reason() {
    let action = SuggestedAction::new(SuggestedActionExecution::RequestChanges {
        run_id: "demo-run".to_string(),
        gate_id: None,
        actor: None,
        reason: Some("...".to_string()),
    });
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        vec![Some("tighten wording".to_string())],
        Vec::new(),
        vec![true],
    );

    let resolved = resolve_selected_action(&mut prompt, &action)
        .expect("resolve")
        .expect("selection");

    assert_eq!(
        resolved.display_command,
        "transist runs gate request-changes demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"tighten wording\""
    );
    assert!(prompt.action_confirmations.is_empty());
    assert_eq!(
        resolved.execution,
        action
            .execution
            .with_prompted_reason(Some("tighten wording".to_string()))
    );
}

#[test]
fn resolve_selected_action_trims_long_display_reason() {
    let long_reason = "a".repeat(display_reason_limit() + 25);
    let action = SuggestedAction::new(SuggestedActionExecution::RequestChanges {
        run_id: "demo-run".to_string(),
        gate_id: None,
        actor: None,
        reason: Some("...".to_string()),
    });
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        vec![Some(long_reason.clone())],
        Vec::new(),
        vec![true],
    );

    let resolved = resolve_selected_action(&mut prompt, &action)
        .expect("resolve")
        .expect("selection");

    assert!(resolved.display_command.starts_with(
        "transist runs gate request-changes demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \""
    ));
    assert!(resolved.display_command.ends_with("...\""));
    assert!(resolved.display_command.chars().count() < long_reason.chars().count() + 120);
}

#[test]
fn resolve_selected_action_rejects_blank_reason() {
    let action = SuggestedAction::new(SuggestedActionExecution::Reject {
        run_id: "demo-run".to_string(),
        gate_id: None,
        actor: None,
        reason: Some("stop the run".to_string()),
    });
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        vec![Some(String::new())],
        Vec::new(),
        Vec::new(),
    );

    let resolved = resolve_selected_action(&mut prompt, &action).expect("resolve");

    assert!(resolved.is_none());
}

#[test]
fn resolve_selected_action_handles_canceled_reason_prompt() {
    let action = SuggestedAction::new(SuggestedActionExecution::Reject {
        run_id: "demo-run".to_string(),
        gate_id: None,
        actor: None,
        reason: Some("stop the run".to_string()),
    });
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        vec![None],
        Vec::new(),
        Vec::new(),
    );

    let resolved = resolve_selected_action(&mut prompt, &action).expect("resolve");

    assert!(resolved.is_none());
}

#[test]
fn resolve_selected_action_requires_approval_confirmation() {
    let action = SuggestedAction::new(SuggestedActionExecution::Approve {
        run_id: "demo-run".to_string(),
        gate_id: None,
        actor: None,
        reason: Some("Reviewed output and approved".to_string()),
    });
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        vec![Some("Reviewed output and approved".to_string())],
        Vec::new(),
        vec![true],
    );

    let resolved = resolve_selected_action(&mut prompt, &action)
        .expect("resolve")
        .expect("selection");

    assert_eq!(
        resolved.display_command,
        "transist runs gate approve demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"Reviewed output and approved\""
    );
}

#[test]
fn resolve_selected_action_cancels_when_approval_is_not_confirmed() {
    let action = SuggestedAction::new(SuggestedActionExecution::Approve {
        run_id: "demo-run".to_string(),
        gate_id: None,
        actor: None,
        reason: Some("Reviewed output and approved".to_string()),
    });
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![false],
    );

    let resolved = resolve_selected_action(&mut prompt, &action).expect("resolve");

    assert!(resolved.is_none());
}

#[test]
fn resolve_selected_action_cancels_when_request_changes_is_not_confirmed() {
    let action = SuggestedAction::new(SuggestedActionExecution::RequestChanges {
        run_id: "demo-run".to_string(),
        gate_id: Some("review-gate".to_string()),
        actor: Some("reviewer@example.com".to_string()),
        reason: Some("...".to_string()),
    });
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        vec![Some("tighten wording".to_string())],
        Vec::new(),
        vec![false],
    );

    let resolved = resolve_selected_action(&mut prompt, &action).expect("resolve");

    assert!(resolved.is_none());
    assert!(prompt.reasons.is_empty());
    assert!(prompt.action_confirmations.is_empty());
}

#[test]
fn resolve_selected_action_does_not_confirm_request_changes_without_a_reason() {
    let action = SuggestedAction::new(SuggestedActionExecution::RequestChanges {
        run_id: "demo-run".to_string(),
        gate_id: None,
        actor: None,
        reason: Some("...".to_string()),
    });
    for reason in [None, Some(String::new()), Some(" \n\t ".to_string())] {
        let mut prompt = ScriptedPrompt::from_script(
            true,
            Vec::new(),
            Vec::new(),
            vec![reason],
            Vec::new(),
            vec![true],
        );

        let resolved = resolve_selected_action(&mut prompt, &action).expect("resolve");

        assert!(resolved.is_none());
        assert_eq!(prompt.action_confirmations.len(), 1);
    }
}

#[test]
fn format_suggested_action_list_numbers_commands() {
    let lines = format_suggested_action_list(
        "Next:",
        &[
            SuggestedAction::new(SuggestedActionExecution::RunsResume {
                run_id: "demo-run".to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: "demo-run".to_string(),
            }),
        ],
        None,
    );

    assert_eq!(lines[0], "Next:");
    assert_eq!(lines[1], "  1. transist runs retry demo-run");
    assert_eq!(lines[2], "  2. transist runs show demo-run");
}

#[test]
fn format_suggested_action_list_marks_selected_command_inline() {
    let lines = format_suggested_action_list(
        "Next:",
        &[
            SuggestedAction::new(SuggestedActionExecution::RunsResume {
                run_id: "demo-run".to_string(),
            }),
            SuggestedAction::new(SuggestedActionExecution::RunsShow {
                run_id: "demo-run".to_string(),
            }),
        ],
        Some((1, "transist runs show demo-run --limit 80")),
    );

    assert_eq!(lines[0], "Next:");
    assert_eq!(lines[1], "  1. transist runs retry demo-run");
    assert_eq!(
        lines[2],
        "  2. ✔ (selected) transist runs show demo-run --limit 80"
    );
}

#[test]
fn numbered_picker_items_prefix_commands_with_indices() {
    let items = numbered_picker_items(&[
        SuggestedAction::new(SuggestedActionExecution::RunsResume {
            run_id: "demo-run".to_string(),
        }),
        SuggestedAction::new(SuggestedActionExecution::RunsShow {
            run_id: "demo-run".to_string(),
        }),
    ]);

    assert_eq!(items[0], "1. transist runs retry demo-run");
    assert_eq!(items[1], "2. transist runs show demo-run");
}

#[test]
fn suggested_action_shortcut_index_maps_supported_digits() {
    assert_eq!(suggested_action_shortcut_index('1', 3), Some(0));
    assert_eq!(suggested_action_shortcut_index('3', 3), Some(2));
    assert_eq!(suggested_action_shortcut_index('4', 3), None);
    assert_eq!(suggested_action_shortcut_index('0', 3), None);
    assert_eq!(suggested_action_shortcut_index('x', 3), None);
}

#[test]
fn pager_navigation_uses_scrollable_body_height() {
    let page_height = 43;
    let body_height = pager_body_height(page_height);

    assert_eq!(body_height, 41);
    assert_eq!(pager_max_offset(42, body_height), 1);
    assert_eq!(pager_next_offset(0, 1, 42, body_height), 1);
    assert_eq!(pager_next_offset(0, body_height, 42, body_height), 1);
}

#[test]
fn body_exceeds_pager_page_detects_overflow_after_wrapping() {
    assert!(!body_exceeds_pager_page("a\nb\nc\nd", 80, 8));
    assert!(body_exceeds_pager_page("a\nb\nc\nd\ne", 80, 8));
    assert!(body_exceeds_pager_page("abcdefghij", 2, 8));
}

#[test]
fn resolve_inspect_path_under_root_run_dir() {
    let path = resolve_inspect_path(
        "run://software_engineer_ticket_delivery/ISSUE-45/plan.md",
        Some("/tmp/home/runs/run_abc123/contained_runs/run_abc123__plan_loop"),
        Some("/tmp/workspace"),
        Some("/tmp/target"),
    );
    assert_eq!(
        path,
        "/tmp/home/runs/run_abc123/software_engineer_ticket_delivery/ISSUE-45/plan.md"
    );
}

#[test]
fn resolve_inspect_path_under_repo_transist_dir() {
    let path = resolve_inspect_path(
        "repo://software_engineer_ticket_delivery/ISSUE-45/plan.md",
        Some("/tmp/home/runs/run_abc123"),
        Some("/tmp/workspace"),
        Some("../target-repo"),
    );
    assert_eq!(
        path,
        "/tmp/workspace/../target-repo/.transist/software_engineer_ticket_delivery/ISSUE-45/plan.md"
    );
}

#[test]
fn parse_interrupt_confirmation_accepts_yes_no_forms() {
    assert_eq!(parse_interrupt_confirmation("y"), Some(true));
    assert_eq!(parse_interrupt_confirmation("yes"), Some(true));
    assert_eq!(parse_interrupt_confirmation("n"), Some(false));
    assert_eq!(parse_interrupt_confirmation("no"), Some(false));
    assert_eq!(parse_interrupt_confirmation(""), Some(false));
    assert_eq!(parse_interrupt_confirmation("maybe"), None);
}

#[test]
fn inactive_run_cancellation_status_message_includes_current_status() {
    let error = anyhow::Error::new(RunNotActiveForCancellationError::new("demo-run", "failed"));

    assert_eq!(
        inactive_run_cancellation_status_message(&error).as_deref(),
        Some(
            "Run demo-run is not currently active (status: failed), so there is nothing to interrupt. Refreshing run view."
        )
    );
}

#[test]
fn build_progress_checklist_marks_running_root_step_active() {
    let detail = inspection_run_detail(vec![
        inspection_step("plan", "running"),
        inspection_step("finish", "pending"),
    ]);

    let checklist = build_progress_checklist(&detail, None);

    assert_eq!(checklist[0].state, ChecklistState::Active);
    assert_eq!(checklist[1].state, ChecklistState::Pending);
}

#[test]
fn build_progress_checklist_promotes_active_descendant_over_blocked_subflow_step() {
    let detail = inspection_run_detail(vec![
        inspection_step("plan_loop", "blocked"),
        inspection_step("finish", "pending"),
    ]);

    let checklist = build_progress_checklist(
        &detail,
        Some(&ChecklistOverride {
            step_id: "plan_loop".to_string(),
            state: ChecklistState::Active,
        }),
    );

    assert_eq!(checklist[0].state, ChecklistState::Active);
    assert_eq!(checklist[1].state, ChecklistState::Pending);
}

#[test]
fn build_progress_state_marks_failed_descendant_subflow_failed() {
    let root_detail = inspection_run_detail(vec![
        inspection_step("plan_loop", "failed"),
        inspection_step("finish", "pending"),
    ]);
    let actionable = actionable_run(
        "run_1",
        "run_1__plan_loop",
        "failed",
        vec![inspection_step("generate_plan", "failed")],
        Some("generate_plan"),
    );
    let progress = build_progress_state(
        "run_1",
        &root_detail,
        &actionable,
        actionable.detail.steps.first(),
        Some("openspec".to_string()),
        false,
    );

    assert_eq!(progress.checklist[0].state, ChecklistState::Failed);
    assert_eq!(progress.checklist[1].state, ChecklistState::Pending);
}

#[test]
fn terminal_live_progress_clears_last_preview_text_for_terminal_states() {
    let last_progress = LiveProgress {
        state: ProgressState {
            root_run_id: "run_1".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "run_1__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> generate_plan".to_string()),
            current_step_status: Some("running".to_string()),
            execution: Some(2),
            cycle: Some(2),
            tool_name: Some("openspec".to_string()),
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: Vec::new(),
        },
        preview_stream: Some("stdout".to_string()),
        preview_text: Some("Still working".to_string()),
        attached_step_output_blocks: Vec::new(),
        footer_lines: Vec::new(),
    };
    let terminal = terminal_live_progress(
        ProgressState {
            root_run_id: "run_1".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "run_1__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> generate_plan".to_string()),
            current_step_status: Some("failed".to_string()),
            execution: Some(2),
            cycle: Some(2),
            tool_name: Some("openspec".to_string()),
            started_at_ms: None,
            finished_at_ms: None,
            error_code: Some("validation_error".to_string()),
            exit_code: Some(0),
            checklist: Vec::new(),
        },
        Some(&last_progress),
        Vec::new(),
    );

    assert_eq!(terminal.preview_stream, None);
    assert_eq!(terminal.preview_text, None);
    assert!(
        strip_ansi_lines(terminal.render_active_step_lines_plain())
            .into_iter()
            .all(|line| !line.contains("Working..."))
    );
}

#[test]
fn live_progress_attaches_step_output_under_its_step_and_preview_under_running_step() {
    let progress = LiveProgress {
        state: ProgressState {
            root_run_id: "run_1".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "run_1".to_string(),
            top_level_step_id: Some("implementation_loop".to_string()),
            current_step_path: Some("implementation_loop".to_string()),
            current_step_status: Some("running".to_string()),
            execution: None,
            cycle: None,
            tool_name: Some("github.copilot.cli".to_string()),
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "fetch_jira_issue".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "set_jira_in_progress".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "plan_loop".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "implementation_loop".to_string(),
                    state: ChecklistState::Active,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        preview_stream: Some("status".to_string()),
        preview_text: Some("Copilot is still working...".to_string()),
        attached_step_output_blocks: vec![AttachedStepOutputBlock {
            lines: vec![
                "Next:".to_string(),
                "  1. ✔ (selected) transist runs gate approve demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"Reviewed output and approved\"".to_string(),
                "  2. transist runs gate request-changes demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"...\"".to_string(),
            ],
            anchor_step_id: Some("plan_loop".to_string()),
        }],
        footer_lines: Vec::new(),
    };

    assert_eq!(
        strip_ansi_lines(progress.render_lines_plain()),
        vec![
            "[✔] fetch_jira_issue (1s)".to_string(),
            "[✔] set_jira_in_progress (1s)".to_string(),
            "[✔] plan_loop (1s)".to_string(),
            "    Next:".to_string(),
            "      1. ✔ (selected) transist runs gate approve demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"Reviewed output and approved\"".to_string(),
            "      2. transist runs gate request-changes demo-run --actor \"Reviewer <reviewer@example.com>\" --reason \"...\"".to_string(),
            "[>] implementation_loop".to_string(),
            "    github.copilot.cli: \"Copilot is still working...\"".to_string(),
            "[ ] finish".to_string(),
        ]
    );
}

#[test]
fn current_step_summary_progress_renders_only_current_step_details() {
    let summary = SummaryContent {
            body_lines: vec!["Blocked on gate: plan-review".to_string()],
            step_output_lines: vec!["Artifacts:".to_string(), "  '/tmp/plan.md'".to_string()],
            plain_action_lines: Vec::new(),
            interactive_action_lines: vec![
                "Next:".to_string(),
                "  approve: accept this cycle and exit the loop, then continue at implementation_loop".to_string(),
                "  request-changes: rerun this cycle with an optional reason, then continue at generate_plan".to_string(),
                "  reject: stop the run here".to_string(),
            ],
            suggested_actions: vec![
                SuggestedAction::new(SuggestedActionExecution::Approve {
                    run_id: "demo-run".to_string(),
                    gate_id: None,
                    actor: None,
                    reason: Some("Reviewed output and approved".to_string()),
                }),
                SuggestedAction::new(SuggestedActionExecution::RequestChanges {
                    run_id: "demo-run".to_string(),
                    gate_id: None,
                    actor: None,
                    reason: Some("...".to_string()),
                }),
                SuggestedAction::new(SuggestedActionExecution::Reject {
                    run_id: "demo-run".to_string(),
                    gate_id: None,
                    actor: None,
                    reason: Some("stop the run".to_string()),
                }),
            ],
            selected_action_block_title: "Next:".to_string(),
        };
    let progress = build_current_step_summary_progress(
        ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "blocked".to_string(),
            actionable_run_id: "demo-run__plan_loop".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> review_plan".to_string()),
            current_step_status: Some("blocked".to_string()),
            execution: None,
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "fetch_jira_issue".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "set_jira_in_progress".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "create_branch".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "plan_loop".to_string(),
                    state: ChecklistState::Blocked,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "implementation_loop".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        &summary,
        Some("plan_loop".to_string()),
        &summary.interactive_action_lines,
    );

    assert_eq!(
            strip_ansi_lines(progress.render_active_step_lines_plain()),
            vec![
                "[-] plan_loop".to_string(),
                "    Blocked on gate: plan-review".to_string(),
                "    ".to_string(),
                "    Artifacts:".to_string(),
                "      '/tmp/plan.md'".to_string(),
                "    ".to_string(),
                "    Next:".to_string(),
                "      approve: accept this cycle and exit the loop, then continue at implementation_loop".to_string(),
                "      request-changes: rerun this cycle with an optional reason, then continue at generate_plan".to_string(),
                "      reject: stop the run here".to_string(),
            ]
        );
}

#[test]
fn current_step_summary_progress_renders_unanchored_details_when_step_anchor_is_missing() {
    let summary = SummaryContent {
            body_lines: vec![
                "Outcome: preflight failed before step dispatch.".to_string(),
                "Preflight blockers: 2 blocker(s), 1 warning(s)".to_string(),
            ],
            step_output_lines: Vec::new(),
            plain_action_lines: Vec::new(),
            interactive_action_lines: vec![
                "Inspect:".to_string(),
                "  show the persisted run record or rerun preflight for detailed blockers"
                    .to_string(),
                "Retry:".to_string(),
                "  retry this run after you fix the preflight blockers, then continue at ensure_distinct_repos".to_string(),
            ],
            suggested_actions: vec![
                SuggestedAction::new(SuggestedActionExecution::RunsShow {
                    run_id: "demo-run".to_string(),
                }),
                SuggestedAction::new(SuggestedActionExecution::RunsPreflight {
                    run_id: "demo-run".to_string(),
                }),
            ],
            selected_action_block_title: "Next:".to_string(),
        };
    let progress = build_current_step_summary_progress(
        ProgressState {
            root_run_id: "demo-run".to_string(),
            run_status: "failed".to_string(),
            actionable_run_id: "demo-run".to_string(),
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
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "ensure_distinct_repos".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "fetch_jira_issue".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        &summary,
        None,
        &summary.interactive_action_lines,
    );

    assert_eq!(
            strip_ansi_lines(progress.render_active_step_lines_plain()),
            vec![
                "[ ] ensure_distinct_repos".to_string(),
                "    Outcome: preflight failed before step dispatch.".to_string(),
                "    Preflight blockers: 2 blocker(s), 1 warning(s)".to_string(),
                "    ".to_string(),
                "    Inspect:".to_string(),
                "      show the persisted run record or rerun preflight for detailed blockers"
                    .to_string(),
                "    Retry:".to_string(),
                "      retry this run after you fix the preflight blockers, then continue at ensure_distinct_repos".to_string(),
            ]
        );
}

fn assert_loading_feedback_ends_cleanly(events: &[Option<String>], message: &str) {
    let (terminator, feedbacks) = events
        .split_last()
        .expect("loading feedback must end with a clear event");
    assert_eq!(terminator, &None);
    for event in feedbacks {
        assert_eq!(event.as_deref(), Some(message));
    }
}

#[test]
fn execute_suggested_action_pages_run_show_and_reopens_picker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: run_show_pager_demo
version: v1
steps:
  - id: noop
    control: control.noop
    with: {}
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    context
        .prepare_workflow(
            "run_show_pager_demo@v1",
            Some("run-show-demo"),
            BTreeMap::new(),
        )
        .expect("prepare");

    let mut prompt = ScriptedPrompt::default();
    let action = ResolvedSuggestedAction {
        display_command: "transist runs show run-show-demo".to_string(),
        execution: SuggestedActionExecution::RunsShow {
            run_id: "run-show-demo".to_string(),
        },
    };

    let outcome = execute_suggested_action(
        &context,
        &mut prompt,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &action,
        true,
        crate::cli::CliUiMode::Plain,
    )
    .expect("execute");

    assert_eq!(outcome, SuggestedActionOutcome::ReopenPicker);
    assert_eq!(prompt.paged_text_presentations.len(), 1);
    assert_eq!(
        prompt.paged_text_presentations[0].0,
        "Run details: run-show-demo"
    );
    assert!(
        prompt.paged_text_presentations[0]
            .1
            .contains("\"run_id\": \"run-show-demo\"")
    );
    assert_loading_feedback_ends_cleanly(&prompt.loading_events, "Loading run details");
}

#[test]
fn execute_suggested_action_pages_run_preflight_and_reopens_picker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: run_preflight_pager_demo
version: v1
steps:
  - id: noop
    control: control.noop
    with: {}
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    context
        .prepare_workflow(
            "run_preflight_pager_demo@v1",
            Some("run-preflight-demo"),
            BTreeMap::new(),
        )
        .expect("prepare");

    let mut prompt = ScriptedPrompt::default();
    let action = ResolvedSuggestedAction {
        display_command: "transist runs preflight run-preflight-demo".to_string(),
        execution: SuggestedActionExecution::RunsPreflight {
            run_id: "run-preflight-demo".to_string(),
        },
    };

    let outcome = execute_suggested_action(
        &context,
        &mut prompt,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &action,
        true,
        crate::cli::CliUiMode::Plain,
    )
    .expect("execute");

    assert_eq!(outcome, SuggestedActionOutcome::ReopenPicker);
    assert_eq!(prompt.paged_text_presentations.len(), 1);
    assert_eq!(
        prompt.paged_text_presentations[0].0,
        "Run preflight: run-preflight-demo"
    );
    assert!(
        prompt.paged_text_presentations[0]
            .1
            .contains("\"status\": \"passed\"")
    );
    assert_loading_feedback_ends_cleanly(&prompt.loading_events, "Running preflight");
}

#[test]
fn dispatch_runs_preflight_pages_results_through_loading_feedback() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: dispatch_preflight_demo
version: v1
steps:
  - id: noop
    control: control.noop
    with: {}
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    context
        .prepare_workflow(
            "dispatch_preflight_demo@v1",
            Some("dispatch-preflight-demo"),
            BTreeMap::new(),
        )
        .expect("prepare");

    let cli = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        home_dir.display().to_string(),
        "runs".to_string(),
        "preflight".to_string(),
        "dispatch-preflight-demo".to_string(),
    ]);
    let mut prompt = ScriptedPrompt::default();
    let exit_code = dispatch(
        cli,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut prompt,
    )
    .expect("dispatch");

    assert_eq!(exit_code, 0);
    assert_loading_feedback_ends_cleanly(&prompt.loading_events, "Running preflight");
    assert!(prompt.paged_text_presentations.is_empty());
}

#[test]
fn workflow_run_rejects_pre_dispatch_blockers_without_creating_a_run() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: failed_prepare_run
version: v1
prerequisites:
  paths:
    - path: .missing-run-prerequisite
      kind: file
      required: true
steps:
  - id: noop
    control: control.noop
    with: {}
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    let readiness = deterministic_pre_dispatch_readiness(
        &context,
        "failed_prepare_run@v1",
        BTreeMap::new(),
        false,
        false,
    )
    .expect("readiness")
    .expect("deterministic blocker report");
    assert_eq!(readiness["status"], "not_deliverable");
    assert_eq!(readiness["run_created"], false);
    let run_entries_before = fs::read_dir(home_dir.join("runs"))
        .expect("runs storage")
        .count();
    let cli = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        home_dir.display().to_string(),
        "workflows".to_string(),
        "run".to_string(),
        "failed_prepare_run@v1".to_string(),
        "--run-ref".to_string(),
        "failed-prepare-run".to_string(),
    ]);
    let mut prompt = ScriptedPrompt::default();

    let exit_code = dispatch(
        cli,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut prompt,
    )
    .expect("failed prepare is a reported command outcome");

    assert_eq!(exit_code, 1);
    assert!(
        context
            .inspection()
            .get_run("failed-prepare-run")
            .expect("run status")
            .is_none(),
        "deterministic blockers must not publish a run locator"
    );
    assert!(context.runs_list().expect("runs list").is_empty());
    assert!(
        fs::read_dir(home_dir.join("runs"))
            .expect("runs storage")
            .count()
            == run_entries_before,
        "deterministic blockers must not create run storage"
    );
    assert!(!home_dir.join("locks/failed-prepare-run.json").exists());
}

#[test]
fn workflow_check_path_with_input_uses_read_only_bound_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    init_standalone_home(&home_dir);
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: path_bound_validation
version: v1
inputs:
  label:
    type: string
    required: true
steps:
  - id: noop
    control: control.noop
    with: {}
"#,
    )
    .expect("flow");
    let cli = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        home_dir.display().to_string(),
        "workflows".to_string(),
        "check".to_string(),
        "--path".to_string(),
        flow.display().to_string(),
        "--input".to_string(),
        "label=demo".to_string(),
    ]);

    let exit_code = dispatch(
        cli,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut ScriptedPrompt::default(),
    )
    .expect("bound path check");

    assert_eq!(exit_code, 0);
    assert!(
        !home_dir
            .join("registry/recipes/path_bound_validation-v1.yaml")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn workflow_check_path_structured_output_retains_envelope_and_full_report() {
    let report = serde_json::json!({
        "workflow_ref": "demo@v1",
        "workspace_dir": "/tmp/workspace",
        "status": "warning",
        "checks": [{"code": "runtime_only", "status": "not_evaluated"}],
        "policy_suggestion": {"schema": "transist.policy_fragment.v1"},
    });

    let output = structured_bound_validation_output(serde_json::json!("demo@v1"), report.clone());

    assert_eq!(output["validated"], "demo@v1");
    assert_eq!(output["schema"], transist::FLOW_SCHEMA_V1);
    assert_eq!(output["mode"], "bound");
    assert_eq!(output["report_schema"], "transist.workflow_validation.v1");
    assert_eq!(output["workflow"], "demo@v1");
    assert_eq!(output["valid"], true);
    assert_eq!(output["workspace_dir"], "/tmp/workspace");
    assert_eq!(output["checks"], report["checks"]);
    assert_eq!(output["report"], report);
}

#[test]
fn workflow_check_output_aggregates_bound_validation_without_a_run() {
    let validation = serde_json::json!({
        "workflow_ref": "demo@v1",
        "status": "not_evaluated",
        "checks": [{"code": "secret_value", "status": "not_evaluated"}],
    });

    let output = workflow_readiness_output("demo@v1", Ok(validation.clone()));

    assert_eq!(output["schema"], "transist.workflow_readiness.v1");
    assert_eq!(output["workflow"], "demo@v1");
    assert_eq!(output["status"], "not_ready");
    assert_eq!(output["run_created"], false);
    assert_eq!(output["checks"], validation["checks"]);
    assert_eq!(output["validation"], validation);
}

#[test]
fn workflow_check_output_distinguishes_delivery_and_definition_failures() {
    let not_deliverable = workflow_readiness_output(
        "demo@v1",
        Ok(serde_json::json!({"status": "failed", "checks": []})),
    );
    let invalid_definition = workflow_readiness_output(
        "demo@v1",
        Err(anyhow::anyhow!("installed recipe cannot be bound")),
    );

    assert_eq!(not_deliverable["status"], "not_deliverable");
    assert_eq!(invalid_definition["status"], "invalid_definition");
    assert_eq!(invalid_definition["run_created"], false);
    assert_eq!(invalid_definition["checks"][0]["code"], "bound_validation");
}

#[test]
fn readiness_plain_rendering_groups_checks_and_exact_capabilities() {
    let report = serde_json::json!({
        "workflow": "demo@v1",
        "status": "not_deliverable",
        "run_created": false,
        "checks": [
            {"code": "input_missing", "status": "failed", "message": "repo_path is required"},
            {"code": "run_policy", "status": "failed", "message": "capability not granted"},
            {"code": "secret_value", "status": "warning", "message": "secret unavailable"}
        ],
        "validation": {
            "required_capabilities": [{"capability": "filesystem", "resource": "/repo", "operations": ["read", "write"]}],
            "granted_capabilities": [{"capability": "filesystem", "resource": "/repo", "operations": ["read"]}],
            "missing_capabilities": [{"capability": "filesystem", "resource": "/repo", "operations": ["write"]}]
        }
    });

    let text = render_readiness_text(&report);

    assert!(text.contains("Blocking checks: Inputs / action schemas=1 Authority=1"));
    assert!(
        text.contains("Inputs / action schemas:\n- [failed] input_missing: repo_path is required")
    );
    assert!(text.contains("Authority:\n- [failed] run_policy: capability not granted"));
    assert!(text.contains("Secrets:\n- [warning] secret_value: secret unavailable"));
    assert!(text.contains("Missing authority: 1 capabilities."));
    assert!(!text.contains("Required capabilities:"));
    assert!(
        !text.contains("workflows authorize"),
        "input failures must not suggest policy mutation"
    );
    assert!(!readiness_authorization_available(&report));
}

#[test]
fn required_empty_input_hint_uses_effective_overridden_values_only() {
    let spec =
        |name: &str, required: bool, value_type: &str| transist::WorkflowInspectionInputSummary {
            name: name.to_string(),
            value_type: value_type.to_string(),
            required,
            has_default: false,
            description: None,
        };
    let specs = vec![
        spec("repo_path", true, "string"),
        spec("jira_ticket_id", true, "string"),
        spec("branch_name", false, "string"),
        spec("max_iterations", true, "number"),
    ];
    let mut inputs = BTreeMap::from([
        ("repo_path".to_string(), KernelValue::String(String::new())),
        (
            "jira_ticket_id".to_string(),
            KernelValue::String("  ".into()),
        ),
        (
            "branch_name".to_string(),
            KernelValue::String(String::new()),
        ),
    ]);
    inputs.insert(
        "repo_path".into(),
        KernelValue::String("/path/to/repo".into()),
    );
    assert_eq!(
        empty_required_workflow_inputs(&specs, &inputs),
        vec!["jira_ticket_id"]
    );
    inputs.remove("repo_path");
    assert_eq!(
        missing_required_workflow_inputs(&specs, &inputs),
        vec!["repo_path", "max_iterations"]
    );
}

#[test]
fn blocked_authorization_names_empty_required_input_without_value_or_unrelated_hints() {
    let mut report = serde_json::json!({
        "status": "blocked",
        "workflow": "demo@v1",
        "readiness": {
            "workflow": "demo@v1",
            "status": "not_deliverable",
            "checks": [
                {"code": "input_schema", "status": "failed", "message": "instance path '/issue_key' fails minLength"},
                {"code": "capability_not_granted", "status": "failed", "message": "policy missing"}
            ],
            "validation": {"missing_capabilities": [{"capability": "plugin", "resource": "jira", "operations": ["load"]}]}
        }
    });
    add_authorization_diagnostics(&mut report);
    add_empty_required_input_diagnostics(&mut report["readiness"], &["jira_ticket_id".into()]);
    let text = render_authorization_text(&report);
    assert!(text.contains("Required workflow inputs supplied empty (possible cause):"));
    assert!(text.contains("\"jira_ticket_id\": set a non-empty value"));
    assert!(text.contains("--input jira_ticket_id=<value>"));
    assert!(!text.contains("--input repo_path=<value>"));
    assert!(!text.contains("YOUR-REAL-TICKET"));
    assert!(
        !report["readiness"]["diagnostics"]
            .to_string()
            .contains("YOUR-REAL-TICKET")
    );
}

#[test]
fn readiness_plain_rendering_collapses_repeated_action_failures_without_hiding_steps() {
    let report = serde_json::json!({
        "workflow": "demo@v1",
        "status": "not_deliverable",
        "checks": [
            {"code": "input_schema", "status": "failed", "step_path": "one", "message": "instance path '/issue_key' is too short"},
            {"code": "input_schema", "status": "failed", "step_path": "two", "message": "instance path '/issue_key' is too short"},
            {"code": "plugin_contract", "status": "passed", "message": "plugin contract is valid"},
            {"code": "new_unknown_code", "status": "warning", "message": "inspect this later"}
        ],
        "validation": {"missing_capabilities": [{"capability": "plugin", "resource": "shell", "operations": ["load"]}]}
    });
    let text = render_readiness_text(&report);
    assert!(text.contains("Blocking checks: Inputs / action schemas=2"));
    assert_eq!(
        text.matches("instance path '/issue_key' is too short")
            .count(),
        1
    );
    assert!(text.contains("(steps: one, two)"));
    assert!(!text.contains("plugin contract is valid"));
    assert!(text.contains("Other:\n- [warning] new_unknown_code: inspect this later"));
    assert_eq!(readiness_check_group("new_unknown_code"), "Other");
}

#[test]
fn blocked_authorization_explains_non_policy_blockers_before_grants() {
    let report = serde_json::json!({
        "status": "blocked",
        "workflow": "demo@v1",
        "readiness": {
            "workflow": "demo@v1",
            "status": "not_deliverable",
            "checks": [
                {"code": "input_schema", "status": "failed", "step_path": "fetch_issue", "message": "instance path '/issue_key' is too short"},
                {"code": "capability_not_granted", "status": "failed", "message": "capability not granted", "remediation": "apply the exact policy_suggestion fragment from this report"}
            ],
            "validation": {"missing_capabilities": [{"capability": "plugin", "resource": "jira", "operations": ["load"]}]}
        }
    });
    let text = render_authorization_text(&report);
    assert!(text.contains("Workflow authorization: blocked"));
    assert!(text.contains("No grants written."));
    assert!(
        text.contains("transist workflows authorize demo@v1 --inputs-file <corrected-inputs-file>")
    );
    assert!(text.contains("after resolving the non-authority blockers"));
    assert!(text.contains("Reuse any --input overrides from this attempt."));
    assert!(text.contains("Use --dry-run to preview without writing."));
    assert!(text.contains(
        "Grants are added to the effective home's policy, not to the workflow definition."
    ));
    assert!(!text.contains("apply the exact policy_suggestion fragment"));
    assert!(!readiness_authorization_available(&report["readiness"]));
}

#[test]
fn blocked_authorization_without_missing_grants_does_not_suggest_policy_mutation() {
    let report = serde_json::json!({
        "status": "blocked",
        "workflow": "demo@v1",
        "readiness": {
            "workflow": "demo@v1",
            "status": "invalid_definition",
            "checks": [{"code": "bound_validation", "status": "failed", "message": "workspace.path is invalid"}]
        }
    });
    let text = render_authorization_text(&report);
    assert!(!text.contains("To add missing grants"));
    assert!(!text.contains("--dry-run"));
}

#[test]
fn planned_authorization_plain_output_shows_exact_grants_and_destination() {
    let report = serde_json::json!({
        "status": "planned",
        "workflow": "demo@v1",
        "policy_output": "/home/user/policy/workflow-authorizations/demo.yaml",
        "policy": {"workflow_grants": [{"workflow": "demo@v1", "use": ["exact"]}]}
    });
    let text = render_authorization_text(&report);
    assert!(
        text.contains("Policy destination: /home/user/policy/workflow-authorizations/demo.yaml")
    );
    assert!(text.contains("\"workflow\": \"demo@v1\""));
}

#[test]
fn readiness_diagnostics_index_input_and_authority_findings_without_changing_checks() {
    let mut report = serde_json::json!({
        "workflow": "demo@v1",
        "status": "not_deliverable",
        "checks": [
            {"code": "plugin_contract", "status": "passed", "message": "ready"},
            {"code": "input_schema", "status": "failed", "step_path": "fetch_issue", "message": "instance path '/issue_key' invalid"},
            {"code": "capability_not_granted", "status": "failed", "message": "policy missing"},
            {"code": "input_schema", "status": "failed", "step_path": "set_issue", "message": "instance path '/issue_key' invalid"},
            {"code": "unknown_check", "status": "warning", "message": "inspect"}
        ],
        "validation": {"missing_capabilities": [{"capability": "plugin", "resource": "jira", "operations": ["load"]}]}
    });
    let checks = report["checks"].clone();
    add_readiness_diagnostics(&mut report);

    assert_eq!(report["checks"], checks);
    assert_eq!(report["diagnostics"]["groups"]["inputs"]["failed"], 2);
    assert_eq!(
        report["diagnostics"]["groups"]["inputs"]["check_indices"],
        serde_json::json!([1, 3])
    );
    assert_eq!(
        report["diagnostics"]["groups"]["authority"]["check_indices"],
        serde_json::json!([2])
    );
    assert_eq!(report["diagnostics"]["groups"]["other"]["warning"], 1);
    assert_eq!(report["diagnostics"]["authorization"]["eligible"], false);
    assert_eq!(
        report["diagnostics"]["authorization"]["blocking_codes"],
        serde_json::json!(["input_schema"])
    );
    assert_eq!(
        report["diagnostics"]["authorization"]["missing_capabilities"],
        1
    );
    assert!(!report["diagnostics"].to_string().contains("instance path"));
}

#[test]
fn authorization_diagnostics_distinguish_ready_grants_from_unavailable_destination() {
    let mut report = serde_json::json!({
        "status": "blocked",
        "workflow": "demo@v1",
        "policy_output": "/home/user/policy/workflow-authorizations/existing.yaml",
        "readiness": {
            "workflow": "demo@v1",
            "status": "not_deliverable",
            "checks": [{"code": "run_policy", "status": "failed", "message": "capability not granted"}],
            "validation": {"missing_capabilities": [{"capability": "plugin", "resource": "shell", "operations": ["load"]}]}
        }
    });
    add_authorization_diagnostics(&mut report);
    assert_eq!(report["diagnostics"]["authorization"]["eligible"], true);
    assert_eq!(
        report["diagnostics"]["authorization"]["plan_blocker"],
        "policy_destination_unavailable"
    );
    assert_eq!(
        report["readiness"]["diagnostics"]["authorization"]["eligible"],
        true
    );
}

#[test]
fn readiness_plain_rendering_suggests_authorization_only_for_exact_authority_gaps() {
    let report = serde_json::json!({
        "workflow": "demo@v1",
        "status": "not_deliverable",
        "run_created": false,
        "checks": [{"code": "step_policy", "status": "failed", "message": "capability not granted"}],
        "validation": {
            "missing_capabilities": [{"capability": "action", "resource": "shell/exec", "operations": ["execute"]}]
        }
    });

    let text = render_readiness_text(&report);

    assert!(readiness_authorization_available(&report));
    assert!(
        text.contains("Next: review exact missing authority with `transist workflows authorize")
    );
}

#[test]
fn readiness_plain_rendering_explains_runtime_materialization_blockers() {
    let report = serde_json::json!({
        "workflow": "ticket_delivery@v1",
        "status": "not_ready",
        "run_created": false,
        "checks": [
            {"code": "mutable_variable", "status": "not_evaluated", "step_path": "fetch_hardware_context", "message": "step depends on a value unavailable during static validation"},
            {"code": "request_preparation", "status": "not_evaluated", "step_path": "fetch_hardware_context", "message": "request_preparation was not evaluated because materialization depends on runtime data"}
        ]
    });

    let text = render_readiness_text(&report);

    assert!(text.contains(
        "Launch blocked: fetch_hardware_context depends on values produced at runtime, so its request cannot be materialized before run birth."
    ));
    assert!(text.contains(
        "This workflow cannot start until that dependency is removed or made input-bound. No run was created."
    ));
}

#[test]
fn workflow_check_uses_bound_validation_without_creating_a_run() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: ready_demo
version: v1
steps:
  - id: finish
    control: control.finish
    with: {}
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    let run_entries_before = fs::read_dir(home_dir.join("runs"))
        .expect("runs directory")
        .count();
    let readiness_file = tmp.path().join("ready-output.json");

    let cli = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        home_dir.display().to_string(),
        "workflows".to_string(),
        "check".to_string(),
        "ready_demo@v1".to_string(),
        "--output".to_string(),
        readiness_file.display().to_string(),
    ]);
    let exit_code = dispatch(
        cli,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut ScriptedPrompt::default(),
    )
    .expect("readiness check");

    assert_eq!(exit_code, 0);
    let evidence: serde_json::Value =
        serde_json::from_slice(&fs::read(&readiness_file).expect("readiness output"))
            .expect("readiness JSON");
    assert_eq!(evidence["status"], "ready");
    assert!(evidence["fingerprint"].is_string());
    assert_eq!(
        evidence["diagnostics"]["inputs"]["empty_required"],
        serde_json::json!([])
    );
    assert_eq!(
        fs::read_dir(home_dir.join("runs"))
            .expect("runs directory")
            .count(),
        run_entries_before
    );
}

#[test]
fn readiness_evidence_is_canonical_redacted_and_revalidated_before_run_birth() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: readiness_evidence_demo
version: v1
inputs:
  token:
    type: string
    required: true
steps:
  - id: finish
    control: control.finish
    with: {}
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    let inputs = BTreeMap::from([(
        "token".to_string(),
        KernelValue::String("secret-value-must-not-be-written".to_string()),
    )]);
    let evidence = workflow_readiness_evidence(
        &context,
        "readiness_evidence_demo@v1",
        inputs.clone(),
        false,
        false,
    )
    .expect("readiness evidence");
    assert_eq!(evidence["status"], "ready");
    assert!(evidence["fingerprint"].is_string());
    let empty_evidence = workflow_readiness_evidence(
        &context,
        "readiness_evidence_demo@v1",
        BTreeMap::from([("token".to_string(), KernelValue::String(String::new()))]),
        false,
        false,
    )
    .expect("empty input readiness evidence");
    assert_eq!(
        empty_evidence["diagnostics"]["inputs"]["empty_required"],
        serde_json::json!(["token"])
    );
    for field in [
        "workflow",
        "inputs",
        "workspace",
        "dependencies",
        "trust",
        "policy",
        "settings",
        "preflight",
    ] {
        assert!(
            evidence["fingerprints"][field].is_string(),
            "missing {field} fingerprint"
        );
    }
    let readiness_file = tmp.path().join("readiness.json");
    write_readiness_file(&readiness_file, &evidence).expect("write evidence");
    let raw = fs::read(&readiness_file).expect("read evidence");
    assert_eq!(raw, serde_json::to_vec(&evidence).expect("canonical JSON"));
    assert!(
        !String::from_utf8_lossy(&raw).contains("secret-value-must-not-be-written"),
        "readiness evidence must not write bound input values"
    );
    assert!(
        pre_dispatch_readiness(
            &context,
            "readiness_evidence_demo@v1",
            inputs.clone(),
            false,
            false,
            Some(&readiness_file),
            300,
        )
        .expect("valid evidence")
        .is_none()
    );

    fs::write(&readiness_file, [raw.as_slice(), b" "].concat()).expect("modify evidence");
    let modified = pre_dispatch_readiness(
        &context,
        "readiness_evidence_demo@v1",
        inputs.clone(),
        false,
        false,
        Some(&readiness_file),
        300,
    )
    .expect("modified evidence result")
    .expect("modified rejection");
    assert_eq!(modified["run_created"], false);
    assert_eq!(modified["checks"][0]["code"], "readiness_modified");

    let mut stale = evidence.clone();
    stale["issued_at_unix_seconds"] = serde_json::json!(0);
    stale["fingerprint"] =
        serde_json::json!(readiness_fingerprint(&stale).expect("stale fingerprint"));
    write_readiness_file(&readiness_file, &stale).expect("write stale evidence");
    let stale = pre_dispatch_readiness(
        &context,
        "readiness_evidence_demo@v1",
        inputs.clone(),
        false,
        false,
        Some(&readiness_file),
        300,
    )
    .expect("stale evidence result")
    .expect("stale rejection");
    assert_eq!(stale["checks"][0]["code"], "readiness_stale");

    let mut future = evidence.clone();
    future["issued_at_unix_seconds"] = serde_json::json!(4_102_444_800_i64);
    future["fingerprint"] =
        serde_json::json!(readiness_fingerprint(&future).expect("future fingerprint"));
    write_readiness_file(&readiness_file, &future).expect("write future evidence");
    let future = pre_dispatch_readiness(
        &context,
        "readiness_evidence_demo@v1",
        inputs.clone(),
        false,
        false,
        Some(&readiness_file),
        300,
    )
    .expect("future evidence result")
    .expect("future rejection");
    assert_eq!(future["checks"][0]["code"], "readiness_future");

    write_readiness_file(&readiness_file, &evidence).expect("restore evidence");
    fs::write(
        home_dir.join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: deny_all\n",
    )
    .expect("change settings");
    let drift = pre_dispatch_readiness(
        &context,
        "readiness_evidence_demo@v1",
        inputs,
        false,
        false,
        Some(&readiness_file),
        300,
    )
    .expect("drift evidence result")
    .expect("drift rejection");
    assert_eq!(drift["run_created"], false);
    assert_eq!(drift["checks"][0]["code"], "readiness_drift");
}

#[test]
fn readiness_file_cannot_relabel_current_non_ready_evidence_as_ready() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    context
        .install_plugin_file(&root.join("plugins/shell/plugin.yaml"))
        .expect("install shell");
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: forged_readiness_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: echo
    uses: process:shell/echo
    with:
      args: [hello]
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");

    let mut evidence = workflow_readiness_evidence(
        &context,
        "forged_readiness_demo@v1",
        BTreeMap::new(),
        false,
        false,
    )
    .expect("readiness evidence");
    assert_ne!(evidence["status"], "ready");
    evidence["status"] = serde_json::json!("ready");
    evidence["fingerprint"] =
        serde_json::json!(readiness_fingerprint(&evidence).expect("forged fingerprint"));
    let readiness_file = tmp.path().join("forged-readiness.json");
    write_readiness_file(&readiness_file, &evidence).expect("write forged evidence");

    let rejection = pre_dispatch_readiness(
        &context,
        "forged_readiness_demo@v1",
        BTreeMap::new(),
        false,
        false,
        Some(&readiness_file),
        300,
    )
    .expect("readiness check")
    .expect("forged status must be rejected");
    assert_eq!(rejection["checks"][0]["code"], "readiness_drift");
    assert_eq!(rejection["run_created"], false);
}

#[test]
fn workflow_run_rejects_stale_readiness_evidence_without_creating_a_run() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    init_standalone_home(&home_dir);
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: stale_readiness_run
version: v1
steps:
  - id: finish
    control: control.finish
    with: {}
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    let mut evidence = workflow_readiness_evidence(
        &context,
        "stale_readiness_run@v1",
        BTreeMap::new(),
        false,
        false,
    )
    .expect("readiness evidence");
    evidence["issued_at_unix_seconds"] = serde_json::json!(0);
    evidence["fingerprint"] =
        serde_json::json!(readiness_fingerprint(&evidence).expect("fingerprint"));
    let readiness_file = tmp.path().join("stale-readiness.json");
    write_readiness_file(&readiness_file, &evidence).expect("write evidence");
    let cli = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        home_dir.display().to_string(),
        "workflows".to_string(),
        "run".to_string(),
        "stale_readiness_run@v1".to_string(),
        "--run-ref".to_string(),
        "stale-readiness-run".to_string(),
        "--readiness-file".to_string(),
        readiness_file.display().to_string(),
    ]);
    let exit_code = dispatch(
        cli,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut ScriptedPrompt::default(),
    )
    .expect("stale readiness is a reported command outcome");

    assert_eq!(exit_code, 1);
    assert!(
        context
            .inspection()
            .get_run("stale-readiness-run")
            .expect("run status")
            .is_none(),
        "stale readiness evidence must not publish a run locator"
    );
}

#[test]
fn workflow_install_dry_run_and_approve_are_transactional() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let home_dir = tmp.path().join("home");
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Governed)
        .expect("governed home");
    fs::create_dir_all(home_dir.join("policy/workflows")).expect("policy source");
    fs::write(
        home_dir.join("config.yaml"),
        "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [workflows]\n",
    )
    .expect("config");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    transist::import_trust_root_file(
        &home_dir,
        &root.join("examples/quickstart/trust/quickstart-demo.pub"),
    )
    .expect("trust root");
    let context = transist::TransistContext::with_home(&workspace, &home_dir);
    context
        .install_plugin_file_with_trust(&root.join("plugins/shell/plugin.yaml"), true)
        .expect("trusted shell plugin");
    let workflow_path = root.join("examples/quickstart/quickstart.workflow.yaml");
    let package = transist::capture_workflow_package_with_signature(&workflow_path, None)
        .expect("capture signed quickstart package");
    let package_dir = tmp.path().join("quickstart-package");
    package
        .materialize(&package_dir)
        .expect("materialize package");
    let command = |approve: bool| {
        let mut args = vec![
            "transist".to_string(),
            "--home".to_string(),
            home_dir.display().to_string(),
            "workflows".to_string(),
            "install".to_string(),
            "--package".to_string(),
            package_dir.display().to_string(),
        ];
        if approve {
            args.push("--approve".to_string());
        } else {
            args.push("--dry-run".to_string());
        }
        Cli::parse_from(args)
    };

    let exit_code = dispatch(
        command(false),
        crate::cli::CliUiMode::Plain,
        &workspace,
        &workspace,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut ScriptedPrompt::default(),
    )
    .expect("installation dry run");

    assert_eq!(exit_code, 0);
    assert!(home_dir.join("installation.json").exists());
    assert!(
        !home_dir
            .join("registry/recipes/quickstart.demo-0.1.0.yaml")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/trust/quickstart.demo-0.1.0.json")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );

    let apply_exit_code = dispatch(
        command(true),
        crate::cli::CliUiMode::Plain,
        &workspace,
        &workspace,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut ScriptedPrompt::default(),
    )
    .expect("installation applies the validly signed package");

    assert_eq!(apply_exit_code, 0);
    assert!(
        home_dir
            .join("registry/recipes/quickstart.demo-0.1.0.yaml")
            .exists(),
        "approved installation must install after dry-run leaves the active home unchanged"
    );
    assert!(
        home_dir
            .join("registry/trust/quickstart.demo-0.1.0.json")
            .exists()
    );
    assert!(
        home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn workflow_install_decline_leaves_the_standalone_home_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join("home");
    init_standalone_home(&home_dir);
    let workflow = tmp.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: declined_install\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n    with: {}\n",
    )
    .expect("workflow");
    let cli = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        home_dir.display().to_string(),
        "workflows".to_string(),
        "install".to_string(),
        "--path".to_string(),
        workflow.display().to_string(),
    ]);
    let mut prompt = ScriptedPrompt::from_script(
        true,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![false],
    );

    let exit_code = dispatch(
        cli,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut prompt,
    )
    .expect("declined installation is a reported outcome");

    assert_eq!(exit_code, 0);
    assert!(
        !home_dir
            .join("registry/recipes/declined_install-v1.yaml")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn workflow_install_rejects_home_incompatible_flags_before_mutation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workflow = tmp.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: incompatible_install\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n    with: {}\n",
    )
    .expect("workflow");

    let standalone_home = tmp.path().join("standalone");
    init_standalone_home(&standalone_home);
    let standalone = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        standalone_home.display().to_string(),
        "workflows".to_string(),
        "install".to_string(),
        "--path".to_string(),
        workflow.display().to_string(),
        "--policy-output".to_string(),
        "workflows/incompatible-v1.yaml".to_string(),
    ]);
    let standalone_error = dispatch(
        standalone,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut ScriptedPrompt::default(),
    )
    .expect_err("standalone policy output must be rejected");
    assert!(
        standalone_error
            .to_string()
            .contains("--policy-output is no longer supported by workflows install")
    );
    assert!(
        !standalone_home
            .join("registry/recipes/incompatible_install-v1.yaml")
            .exists()
    );

    let governed_home = tmp.path().join("governed");
    transist::init_home_dir_with_mode(&governed_home, transist::RuntimeOperatingMode::Governed)
        .expect("init governed home");
    let governed = Cli::parse_from([
        "transist".to_string(),
        "--home".to_string(),
        governed_home.display().to_string(),
        "workflows".to_string(),
        "install".to_string(),
        "--path".to_string(),
        workflow.display().to_string(),
        "--signature".to_string(),
        "workflow.sig".to_string(),
    ]);
    let governed_error = dispatch(
        governed,
        crate::cli::CliUiMode::Plain,
        tmp.path(),
        tmp.path(),
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut ScriptedPrompt::default(),
    )
    .expect_err("governed signature override must be rejected");
    assert!(
        governed_error
            .to_string()
            .contains("--signature is no longer supported by workflows install")
    );
    assert!(
        !governed_home
            .join("registry/recipes/incompatible_install-v1.yaml")
            .exists()
    );
}

#[test]
fn workflow_install_uses_the_exact_captured_package_after_source_changes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join("home");
    init_standalone_home(&home_dir);
    let workflow = tmp.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: captured_install\nversion: v1\nsteps:\n  - id: noop\n    control: control.noop\n",
    )
    .expect("workflow");
    let package = transist::capture_workflow_package(&workflow).expect("capture package");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: changed_after_capture\nversion: v2\nsteps:\n  - id: noop\n    control: control.noop\n",
    )
    .expect("mutate source");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let lock = transist::lock_workflow_installation(&home_dir).expect("install lock");

    let installed = context
        .install_workflow_package_snapshot_with_trust(&package, false, &lock)
        .expect("install captured package");

    assert_eq!(installed.workflow_id, "captured_install");
    assert_eq!(installed.workflow_version, "v1");
    assert_eq!(
        installed.workflow_definition_sha256,
        package.definition_sha256()
    );
    assert!(
        home_dir
            .join("registry/recipes/captured_install-v1.yaml")
            .is_file()
    );
    assert!(
        !home_dir
            .join("registry/recipes/changed_after_capture-v2.yaml")
            .exists()
    );
}

#[test]
fn successful_workflow_files_remain_attached_under_completed_steps() {
    let mut validate_changes = inspection_step("validate_changes", "success");
    validate_changes.output_summary.workflow_files = vec![transist::InspectionWorkflowFileRef {
        step_id: "validate_changes".to_string(),
        field_name: "report_file".to_string(),
        locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
        relative_path: "validation_report.md".to_string(),
        local_path: Some(std::path::PathBuf::from("/tmp/validation_report.md")),
        name: "validation_report.md".to_string(),
        bytes: 256,
    }];
    let actionable = actionable_run(
        "demo-run",
        "demo-run",
        "running",
        vec![
            validate_changes.clone(),
            inspection_step("commit_implementation", "pending"),
        ],
        Some("commit_implementation"),
    );
    let persisted_blocks = build_persisted_workflow_file_step_output_blocks(
        "demo-run",
        &actionable,
        &validate_changes,
    );

    let rendered = strip_ansi_lines(
        LiveProgress {
            state: ProgressState {
                root_run_id: "demo-run".to_string(),
                run_status: "running".to_string(),
                actionable_run_id: "demo-run".to_string(),
                top_level_step_id: Some("commit_implementation".to_string()),
                current_step_path: Some("commit_implementation".to_string()),
                current_step_status: Some("pending".to_string()),
                execution: None,
                cycle: None,
                tool_name: None,
                started_at_ms: None,
                finished_at_ms: None,
                error_code: None,
                exit_code: None,
                checklist: vec![
                    ProgressChecklistItem {
                        step_id: "validate_changes".to_string(),
                        state: ChecklistState::Success,
                        duration_seconds: Some(1),
                    },
                    ProgressChecklistItem {
                        step_id: "commit_implementation".to_string(),
                        state: ChecklistState::Pending,
                        duration_seconds: None,
                    },
                ],
            },
            preview_stream: None,
            preview_text: None,
            attached_step_output_blocks: persisted_blocks,
            footer_lines: Vec::new(),
        }
        .render_lines_plain(),
    );

    assert_eq!(rendered[0], "[✔] validate_changes (1s)");
    assert!(
        rendered.contains(&"    Workflow files:".to_string()),
        "missing workflow file section: {rendered:#?}"
    );
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("/tmp/validation_report.md")),
        "missing validation report path: {rendered:#?}"
    );
}

#[test]
fn select_bootstrap_progress_step_state_prefers_first_pending_step_by_order() {
    let run_state = serde_json::json!({
        "steps": {
            "finish": { "status": "pending" },
            "plan_loop": { "status": "pending" },
            "fetch_jira_issue": { "status": "success" }
        }
    });

    let (step_id, _) = select_bootstrap_progress_step_state(
        &run_state,
        &[
            "fetch_jira_issue".to_string(),
            "plan_loop".to_string(),
            "finish".to_string(),
        ],
    )
    .expect("bootstrap step");

    assert_eq!(step_id, "plan_loop");
}

#[test]
fn build_progress_checklist_preserves_terminal_states() {
    let detail = inspection_run_detail(vec![
        inspection_step("review", "blocked"),
        inspection_step("implement", "failed"),
        inspection_step("cleanup", "canceled"),
    ]);

    let checklist = build_progress_checklist(&detail, None);

    assert_eq!(checklist[0].state, ChecklistState::Blocked);
    assert_eq!(checklist[1].state, ChecklistState::Failed);
    assert_eq!(checklist[2].state, ChecklistState::Canceled);
}

#[test]
fn build_progress_checklist_omits_skipped_steps() {
    let detail = inspection_run_detail(vec![
        inspection_step("plan", "success"),
        inspection_step("guard", "skipped"),
        inspection_step("apply", "pending"),
    ]);

    let checklist = build_progress_checklist(&detail, None);

    assert_eq!(checklist.len(), 2);
    assert_eq!(checklist[0].step_id, "plan");
    assert_eq!(checklist[0].state, ChecklistState::Success);
    assert_eq!(checklist[1].step_id, "apply");
    assert_eq!(checklist[1].state, ChecklistState::Pending);
}

#[test]
fn terminal_progress_without_live_snapshot_still_renders_checklist() {
    let progress = terminal_live_progress(
        ProgressState {
            root_run_id: "run_1".to_string(),
            run_status: "blocked".to_string(),
            actionable_run_id: "run_1__pr_review_loop".to_string(),
            top_level_step_id: Some("pr_review_loop".to_string()),
            current_step_path: Some("pr_review_loop -> review_cycle_decision".to_string()),
            current_step_status: Some("blocked".to_string()),
            execution: None,
            cycle: Some(2),
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "pr_review_loop".to_string(),
                    state: ChecklistState::Blocked,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        None,
        Vec::new(),
    );

    assert_eq!(
        strip_ansi_lines(progress.render_lines_plain()),
        vec!["[-] pr_review_loop".to_string(), "[ ] finish".to_string(),]
    );
}

#[test]
fn terminal_progress_without_selected_step_omits_unknown_status_line() {
    let progress = terminal_live_progress(
        ProgressState {
            root_run_id: "run_1".to_string(),
            run_status: "success".to_string(),
            actionable_run_id: "run_1".to_string(),
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
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "set_jira_done".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(1),
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Success,
                    duration_seconds: Some(3),
                },
            ],
        },
        None,
        Vec::new(),
    );

    assert_eq!(
        strip_ansi_lines(progress.render_lines_plain()),
        vec![
            "[✔] set_jira_done (1s)".to_string(),
            "[✔] finish (3s)".to_string(),
        ]
    );
}

#[test]
fn build_progress_checklist_persists_success_duration_seconds() {
    let mut detail = inspection_run_detail(vec![
        inspection_step("fetch_jira_issue", "success"),
        inspection_step("plan_loop", "pending"),
    ]);
    detail.steps[0].started_at_ms = Some(1_000);
    detail.steps[0].finished_at_ms = Some(2_500);

    let checklist = build_progress_checklist(&detail, None);

    assert_eq!(checklist[0].state, ChecklistState::Success);
    assert_eq!(checklist[0].duration_seconds, Some(1));
    assert_eq!(checklist[1].duration_seconds, None);
}

#[test]
fn build_progress_checklist_prefers_subflow_timing_override_for_success_duration() {
    let mut detail = inspection_run_detail(vec![inspection_step("plan_loop", "success")]);
    detail.steps[0].step_kind = "subflow".to_string();
    detail.steps[0].started_at_ms = Some(10_000);
    detail.steps[0].finished_at_ms = Some(11_000);

    let checklist = build_progress_checklist_with_timing_overrides(
        &detail,
        None,
        &BTreeMap::from([(
            "plan_loop".to_string(),
            StepTimingBounds {
                started_at_ms: Some(1_000),
                finished_at_ms: Some(13_000),
            },
        )]),
    );

    assert_eq!(checklist[0].state, ChecklistState::Success);
    assert_eq!(checklist[0].duration_seconds, Some(12));
}

#[test]
fn build_progress_state_from_states_uses_current_active_step_start_for_elapsed() {
    let root_state = serde_json::json!({
        "status": "running",
        "steps": {
            "implementation_loop": {
                "status": "running",
                "started_at_ms": 8_000,
                "finished_at_ms": null
            },
            "finish": {
                "status": "pending"
            }
        }
    });
    let actionable = ActionableRunState {
        run_id: "run_1__implementation_loop".to_string(),
        state: serde_json::json!({}),
    };
    let current_step_value = serde_json::json!({
        "status": "running",
        "started_at_ms": 9_000,
        "last_result": {}
    });

    let progress = build_progress_state_from_states_with_timing_overrides(
        ProgressStateFromStatesArgs {
            root_run_id: "run_1",
            root_state: &root_state,
            root_step_order: &["implementation_loop".to_string(), "finish".to_string()],
            actionable: &actionable,
            current_step: CurrentStepStateRef {
                step_id: "implement_plan",
                step: &current_step_value,
            },
            live_active_override: false,
        },
        Some("github.copilot.cli".to_string()),
        &BTreeMap::from([(
            "implementation_loop".to_string(),
            StepTimingBounds {
                started_at_ms: Some(1_000),
                finished_at_ms: None,
            },
        )]),
    );

    assert_eq!(
        progress.top_level_step_id.as_deref(),
        Some("implementation_loop")
    );
    assert_eq!(progress.started_at_ms, Some(9_000));
    assert_eq!(progress.finished_at_ms, None);
}

#[test]
fn build_progress_state_from_states_clears_stale_result_snapshot_for_live_step() {
    let root_state = serde_json::json!({
        "status": "running",
        "steps": {
            "implementation_loop": {
                "status": "running",
                "started_at_ms": 8_000,
                "finished_at_ms": null
            },
            "finish": {
                "status": "pending"
            }
        }
    });
    let actionable = ActionableRunState {
        run_id: "run_1__implementation_loop".to_string(),
        state: serde_json::json!({}),
    };
    let current_step_value = serde_json::json!({
        "status": "running",
        "started_at_ms": 9_000,
        "last_result": {
            "exit_code": 124,
            "error": {
                "code": "timeout_error"
            }
        }
    });

    let progress = build_progress_state_from_states_with_timing_overrides(
        ProgressStateFromStatesArgs {
            root_run_id: "run_1",
            root_state: &root_state,
            root_step_order: &["implementation_loop".to_string(), "finish".to_string()],
            actionable: &actionable,
            current_step: CurrentStepStateRef {
                step_id: "implement_plan",
                step: &current_step_value,
            },
            live_active_override: false,
        },
        Some("github.copilot.cli".to_string()),
        &BTreeMap::from([(
            "implementation_loop".to_string(),
            StepTimingBounds {
                started_at_ms: Some(1_000),
                finished_at_ms: None,
            },
        )]),
    );

    assert_eq!(progress.current_step_status.as_deref(), Some("running"));
    assert_eq!(progress.error_code, None);
    assert_eq!(progress.exit_code, None);
}

#[test]
fn build_progress_state_from_states_hides_guard_bypassed_steps_from_live_checklist() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let run_root =
        transist_storage_local::run_paths::resolve_run_root_or_default(&home_dir, "run_1")
            .expect("resolve run root");
    fs::create_dir_all(&run_root).expect("create run root");
    fs::write(
        run_root.join("recipe.yaml"),
        r#"
schema: transist.recipe.v1
workflow:
  id: wf
  version: v1
steps:
  - id: choose_mode
    execution:
      kind: control
      primitive: noop
    with: {}
  - id: worktree_prepare
    execution:
      kind: control
      primitive: noop
    with: {}
  - id: branch_prepare
    execution:
      kind: control
      primitive: noop
    with: {}
  - id: finish
    execution:
      kind: control
      primitive: finish
    with: {}
graph:
  entry: choose_mode
  edges:
    - from: choose_mode
      to: branch_prepare
      event: on_success
      guard: 'inputs.mode == "branch"'
    - from: choose_mode
      to: worktree_prepare
      event: on_success
      guard: 'NOT (inputs.mode == "branch")'
    - from: branch_prepare
      to: finish
      event: on_success
    - from: worktree_prepare
      to: finish
      event: on_success
"#,
    )
    .expect("write recipe");

    let root_state = serde_json::json!({
        "run_id": "run_1",
        "workflow_id": "wf",
        "workflow_version": "v1",
        "status": "running",
        "workflow_inputs": {
            "mode": "branch"
        },
        "steps": {
            "choose_mode": {
                "step_id": "choose_mode",
                "status": "succeeded",
                "executions": 1,
                "started_at_ms": 1_000,
                "finished_at_ms": 1_001
            },
            "worktree_prepare": {
                "step_id": "worktree_prepare",
                "status": "pending",
                "executions": 0
            },
            "branch_prepare": {
                "step_id": "branch_prepare",
                "status": "running",
                "executions": 1,
                "started_at_ms": 2_000
            },
            "finish": {
                "step_id": "finish",
                "status": "pending",
                "executions": 0
            }
        }
    });
    let actionable = ActionableRunState {
        run_id: "run_1".to_string(),
        state: root_state.clone(),
    };
    let step_order = vec![
        "choose_mode".to_string(),
        "worktree_prepare".to_string(),
        "branch_prepare".to_string(),
        "finish".to_string(),
    ];

    let progress = build_progress_state_from_states(
        &context,
        ProgressStateFromStatesArgs {
            root_run_id: "run_1",
            root_state: &root_state,
            root_step_order: &step_order,
            actionable: &actionable,
            current_step: CurrentStepStateRef {
                step_id: "branch_prepare",
                step: &root_state["steps"]["branch_prepare"],
            },
            live_active_override: true,
        },
        Some("tool".to_string()),
    )
    .expect("progress");

    assert_eq!(
        progress
            .checklist
            .iter()
            .map(|item| item.step_id.as_str())
            .collect::<Vec<_>>(),
        vec!["choose_mode", "branch_prepare", "finish"],
    );
}

#[test]
fn build_progress_state_from_states_uses_aggregate_timing_for_non_running_nested_step() {
    let root_state = serde_json::json!({
        "status": "blocked",
        "steps": {
            "plan_loop": {
                "status": "blocked",
                "started_at_ms": 8_000,
                "finished_at_ms": 14_000
            },
            "finish": {
                "status": "pending"
            }
        }
    });
    let actionable = ActionableRunState {
        run_id: "run_1__plan_loop".to_string(),
        state: serde_json::json!({}),
    };
    let current_step_value = serde_json::json!({
        "status": "blocked",
        "started_at_ms": 9_000,
        "finished_at_ms": 9_000,
        "last_result": {}
    });

    let progress = build_progress_state_from_states_with_timing_overrides(
        ProgressStateFromStatesArgs {
            root_run_id: "run_1",
            root_state: &root_state,
            root_step_order: &["plan_loop".to_string(), "finish".to_string()],
            actionable: &actionable,
            current_step: CurrentStepStateRef {
                step_id: "review_plan",
                step: &current_step_value,
            },
            live_active_override: false,
        },
        None,
        &BTreeMap::from([(
            "plan_loop".to_string(),
            StepTimingBounds {
                started_at_ms: Some(1_000),
                finished_at_ms: Some(13_000),
            },
        )]),
    );

    assert_eq!(progress.top_level_step_id.as_deref(), Some("plan_loop"));
    assert_eq!(progress.started_at_ms, Some(1_000));
    assert_eq!(progress.finished_at_ms, Some(13_000));
}

#[test]
fn persisted_run_execution_timestamps_aggregates_all_execution_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let locator = transist_storage_local::run_paths::plan_run_locator(&home_dir, "child", None)
        .expect("plan locator");
    transist_storage_local::run_paths::write_run_locator(&home_dir, &locator)
        .expect("write locator");
    let executions_dir = transist_storage_local::run_paths::resolve_run_root(&home_dir, "child")
        .expect("run root")
        .join("step_executions")
        .join("generate_plan");
    fs::create_dir_all(&executions_dir).expect("create executions dir");
    fs::write(
        executions_dir.join("execution_001.json"),
        serde_json::json!({
            "started_at_ms": 1_000,
            "finished_at_ms": 5_000
        })
        .to_string(),
    )
    .expect("write execution 1");
    fs::write(
        executions_dir.join("execution_002.json"),
        serde_json::json!({
            "started_at_ms": 8_000,
            "finished_at_ms": 13_000
        })
        .to_string(),
    )
    .expect("write execution 2");

    let bounds = persisted_run_execution_timestamps(&home_dir, "child").expect("bounds");

    assert_eq!(bounds.started_at_ms, Some(1_000));
    assert_eq!(bounds.finished_at_ms, Some(13_000));
}

#[test]
fn subflow_step_timing_overrides_for_state_keeps_first_cycle_start_across_reentry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let root_locator =
        transist_storage_local::run_paths::plan_run_locator(&home_dir, "run_1", None)
            .expect("plan root locator");
    transist_storage_local::run_paths::write_run_locator(&home_dir, &root_locator)
        .expect("write root locator");
    let child_locator = transist_storage_local::run_paths::plan_run_locator(
        &home_dir,
        "run_1__plan_loop",
        Some(&transist::RunLineage {
            mode: transist::RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "run_1".to_string(),
            parent_run_id: Some("run_1".to_string()),
            parent_step_id: Some("plan_loop".to_string()),
        }),
    )
    .expect("plan child locator");
    transist_storage_local::run_paths::write_run_locator(&home_dir, &child_locator)
        .expect("write child locator");
    let child_run_dir =
        transist_storage_local::run_paths::resolve_run_root(&home_dir, "run_1__plan_loop")
            .expect("child run dir");
    fs::create_dir_all(child_run_dir.join("step_executions").join("generate_plan"))
        .expect("create generate_plan executions dir");
    fs::create_dir_all(child_run_dir.join("step_executions").join("review_plan"))
        .expect("create review_plan executions dir");
    fs::write(
        child_run_dir.join("state.json"),
        serde_json::json!({
            "run_id": "run_1__plan_loop",
            "status": "running",
            "steps": {
                "generate_plan": {
                    "status": "running",
                    "started_at_ms": 9_000
                },
                "plan_loop": {
                    "status": "succeeded",
                    "started_at_ms": 8_000,
                    "finished_at_ms": 8_001
                },
                "review_plan": {
                    "status": "pending"
                }
            }
        })
        .to_string(),
    )
    .expect("write child state");
    fs::write(
        child_run_dir
            .join("step_executions")
            .join("generate_plan")
            .join("execution_001.json"),
        serde_json::json!({
            "started_at_ms": 1_000,
            "finished_at_ms": 5_000
        })
        .to_string(),
    )
    .expect("write generate_plan execution 1");
    fs::write(
        child_run_dir
            .join("step_executions")
            .join("review_plan")
            .join("execution_001.json"),
        serde_json::json!({
            "started_at_ms": 5_010,
            "finished_at_ms": 5_020
        })
        .to_string(),
    )
    .expect("write review_plan execution 1");

    let root_state = serde_json::json!({
        "steps": {
            "plan_loop": {
                "status": "running",
                "started_at_ms": 7_000,
                "last_result": {
                    "output": {
                        "child_run_id": "run_1__plan_loop"
                    }
                }
            }
        }
    });

    let overrides =
        subflow_step_timing_overrides_for_state(&context, "run_1", &root_state).expect("overrides");

    assert_eq!(
        overrides["plan_loop"].started_at_ms,
        Some(1_000),
        "should anchor to the first persisted child-run execution rather than the latest cycle",
    );
}

#[test]
fn terminal_progress_marks_current_non_running_step_with_arrow() {
    let progress = terminal_live_progress(
        ProgressState {
            root_run_id: "run_1".to_string(),
            run_status: "running".to_string(),
            actionable_run_id: "run_1".to_string(),
            top_level_step_id: Some("implementation_loop".to_string()),
            current_step_path: Some("implementation_loop".to_string()),
            current_step_status: Some("waiting".to_string()),
            execution: None,
            cycle: None,
            tool_name: None,
            started_at_ms: None,
            finished_at_ms: None,
            error_code: None,
            exit_code: None,
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "implementation_loop".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        None,
        Vec::new(),
    );

    assert_eq!(
        strip_ansi_lines(progress.render_lines_plain()),
        vec![
            "[>] implementation_loop".to_string(),
            "[ ] finish".to_string(),
        ]
    );
}

#[test]
fn terminal_progress_freezes_elapsed_for_current_non_running_step() {
    let progress = terminal_live_progress(
        ProgressState {
            root_run_id: "run_1".to_string(),
            run_status: "blocked".to_string(),
            actionable_run_id: "run_1".to_string(),
            top_level_step_id: Some("plan_loop".to_string()),
            current_step_path: Some("plan_loop -> review_plan".to_string()),
            current_step_status: Some("blocked".to_string()),
            execution: None,
            cycle: None,
            tool_name: None,
            started_at_ms: Some(1_000),
            finished_at_ms: Some(11_000),
            error_code: None,
            exit_code: None,
            checklist: vec![
                ProgressChecklistItem {
                    step_id: "plan_loop".to_string(),
                    state: ChecklistState::Blocked,
                    duration_seconds: None,
                },
                ProgressChecklistItem {
                    step_id: "finish".to_string(),
                    state: ChecklistState::Pending,
                    duration_seconds: None,
                },
            ],
        },
        None,
        Vec::new(),
    );

    assert_eq!(
        strip_ansi_lines(progress.render_lines_plain()),
        vec![
            "[-] plan_loop (blocked for 10s)".to_string(),
            "[ ] finish".to_string(),
        ]
    );
}

#[test]
fn resolve_actionable_run_stops_at_current_review_cycle_gate_after_nested_approval() {
    let (_tmp, context, inspection, root_run_id) = prepare_nested_review_loop_reblocked_context();

    let actionable = resolve_actionable_run(&inspection, &root_run_id)
        .expect("resolve actionable")
        .expect("actionable run");

    assert_eq!(
        actionable.run_id,
        format!("{root_run_id}__pr_review_loop__review_cycle")
    );
    assert_eq!(actionable.detail.blocked_gates.len(), 1);
    assert_eq!(
        actionable.detail.blocked_gates[0].gate_id,
        "review-cycle-decision"
    );

    let summary =
        build_blocked_summary_content(&context, &actionable.detail, &root_run_id, &actionable)
            .expect("summary");
    assert!(
        summary
            .body_lines
            .contains(&"Blocked on gate: review-cycle-decision".to_string()),
        "unexpected blocked summary body: {:#?}",
        summary.body_lines
    );
    assert!(
        summary.body_lines.iter().any(|line| {
            line.contains("Tip: use the root run id") && line.contains(&root_run_id)
        }),
        "summary should explain that the root run id is enough: {:#?}",
        summary.body_lines
    );
    assert!(
        !summary.body_lines.iter().any(|line| line.contains(
            "Outcome: the run is blocked before the actionable subflow reached a manual gate."
        )),
        "summary should not fall back to stale-child guidance: {:#?}",
        summary.body_lines
    );
}

#[test]
fn resolve_actionable_run_state_stops_at_current_review_cycle_gate_after_nested_approval() {
    let (_tmp, context, _inspection, root_run_id) = prepare_nested_review_loop_reblocked_context();

    let recipe_cache = RecipeCacheOwner::default();
    let actionable = resolve_actionable_run_state(&context, &root_run_id, &recipe_cache)
        .expect("resolve actionable state")
        .expect("actionable state");

    assert_eq!(
        actionable.run_id,
        format!("{root_run_id}__pr_review_loop__review_cycle")
    );
    assert_eq!(
        actionable.state["steps"]["review_cycle_decision"]["status"],
        "blocked"
    );
}

fn actionable_run(
    root_run_id: &str,
    run_id: &str,
    status: &str,
    steps: Vec<InspectionStep>,
    current_step_id: Option<&str>,
) -> ActionableRun {
    let mut detail = inspection_run_detail(steps);
    detail.summary.status = status.to_string();
    detail.summary.run_id = run_id.to_string();
    detail.summary.root_run_id = root_run_id.to_string();
    detail.current_step = current_step_id.map(|step_id| RunCurrentStep {
        step_id: step_id.to_string(),
        status: detail
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .map(|step| step.status.clone())
            .unwrap_or_else(|| "unknown".to_string()),
    });
    ActionableRun {
        root_run_id: root_run_id.to_string(),
        run_id: run_id.to_string(),
        detail,
    }
}

fn inspection_run_detail(steps: Vec<InspectionStep>) -> InspectionRunDetail {
    InspectionRunDetail {
        summary: InspectionRunSummary {
            run_id: "run_1".to_string(),
            workflow_id: "demo".to_string(),
            workflow_version: "v1".to_string(),
            status: "running".to_string(),
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
            root_run_id: "run_1".to_string(),
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
        steps,
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
            command: "transist runs evidence export run_1".to_string(),
            format: "markdown".to_string(),
            generated_at_ms: None,
        },
    }
}

fn inspection_step(step_id: &str, status: &str) -> InspectionStep {
    InspectionStep {
        step_id: step_id.to_string(),
        step_kind: "invoke".to_string(),
        status: status.to_string(),
        execution: Some(1),
        started_at_ms: None,
        finished_at_ms: None,
        input_summary: InspectionStepInputSummary {
            request_present: false,
            invoke_id: None,
            adapter_kind: None,
            plugin_id: None,
            action: None,
            input_object_keys: Vec::new(),
            metadata_object_keys: Vec::new(),
            filesystem_policy: None,
        },
        output_summary: InspectionStepOutputSummary {
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
    }
}

fn inspection_gate(
    gate_id: &str,
    request_summary: BTreeMap<String, KernelValue>,
) -> InspectionGate {
    InspectionGate {
        gate_id: gate_id.to_string(),
        status: "blocked".to_string(),
        step_id: gate_id.to_string(),
        created_at_ms: None,
        decision: None,
        decision_history: Vec::new(),
        request_summary,
    }
}

fn inspection_filesystem_policy_summary(
    source: &str,
    mode: &str,
    reads: &[&str],
    writes: &[&str],
) -> transist::InspectionFilesystemPolicySummary {
    serde_json::from_value(serde_json::json!({
        "source": source,
        "workspace_root": "/tmp/workspace",
        "workspace_access": {
            "mode": mode,
        },
        "read_allow_paths": reads,
        "write_allow_paths": writes,
    }))
    .expect("filesystem policy summary")
}

fn prepare_nested_review_loop_reblocked_context() -> (
    tempfile::TempDir,
    transist::TransistContext,
    TransistInspectionContext,
    String,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let context = transist::TransistContext::with_home(tmp.path(), &home_dir);
    let root_run_id = "review-loop-root-run".to_string();
    init_standalone_home(&home_dir);

    let loop_dir = tmp.path().join("loop");
    let cycle_dir = loop_dir.join("cycle");
    let changes_dir = cycle_dir.join("changes");
    fs::create_dir_all(&changes_dir).expect("changes dir");
    fs::write(
        changes_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: review_changes_demo
version: v1
steps:
  - id: review_plan_revision
    control: control.gate.manual
    with:
      gate_id: review-plan-revision
  - id: finish
    control: control.finish
"#,
    )
    .expect("changes flow");

    fs::create_dir_all(&cycle_dir).expect("cycle dir");
    fs::write(
        cycle_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: review_cycle_demo
version: v1
modules:
  review_changes:
    path: changes/workflow.yaml
steps:
  - id: review_cycle_decision
    control: control.gate.manual
    with:
      gate_id: review-cycle-decision
  - id: maybe_review_changes
    control: control.noop
    if:
      guard: 'steps.review_cycle_decision.out.decision == "request_changes"'
      then:
        - ref: review_changes_flow
  - id: review_changes_flow
    module: review_changes
  - id: finish
    control: control.finish
"#,
    )
    .expect("cycle flow");

    fs::create_dir_all(&loop_dir).expect("loop dir");
    fs::write(
        loop_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: pr_review_loop_demo
version: v1
modules:
  review_cycle:
    path: cycle/workflow.yaml
steps:
  - id: review_loop
    control: control.noop
    loop:
      body:
        - ref: review_cycle
      until: 'steps.review_cycle.out.child_steps.review_cycle_decision.out.decision == "approved"'
      max_iters: 4
      max_duration: 1h
  - id: review_cycle
    module: review_cycle
  - id: finish
    control: control.finish
"#,
    )
    .expect("loop flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_review_loop_root_demo
version: v1
modules:
  pr_review_loop:
    path: ./loop/workflow.yaml
steps:
  - id: pr_review_loop
    module: pr_review_loop
  - id: finish
    control: control.finish
"#,
    )
    .expect("root flow");

    context.compile_flow_file(&flow).expect("compile");
    context
        .prepare_workflow(
            "parent_review_loop_root_demo@v1",
            Some(&root_run_id),
            BTreeMap::new(),
        )
        .expect("prepare");

    let blocked = context.run_started_run(&root_run_id).expect("run");
    assert_eq!(blocked.status, "blocked");

    let changed = context
        .request_changes_gate_with_input(
            &root_run_id,
            "review-cycle-decision",
            transist::GateDecisionInput {
                actor: Some("reviewer@example.com".to_string()),
                actor_source: Some("flag".to_string()),
                reason: Some("run one review cycle".to_string()),
                decided_at_ms: None,
            },
        )
        .expect("request changes");
    assert_eq!(changed.status, "blocked");

    let approved = context
        .approve_gate_with_input(
            &root_run_id,
            "review-plan-revision",
            transist::GateDecisionInput {
                actor: Some("reviewer@example.com".to_string()),
                actor_source: Some("flag".to_string()),
                reason: Some("approved nested review gate".to_string()),
                decided_at_ms: None,
            },
        )
        .expect("approve nested review gate");
    assert_eq!(approved.status, "blocked");

    let inspection =
        TransistInspectionContext::with_home(context.repo_root().to_path_buf(), &home_dir);
    (tmp, context, inspection, root_run_id)
}
