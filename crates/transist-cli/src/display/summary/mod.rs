// SPDX-License-Identifier: MPL-2.0
mod actions;
mod advice;
mod next_step;
mod rendering;

#[allow(unused_imports)]
pub(crate) use actions::{
    ResolvedSuggestedAction, RunsShowStepFilter, SuggestedAction, SuggestedActionExecution,
    SuggestedActionOutcome, dedupe_suggested_actions, format_runs_show_command,
    gate_picker_suggested_actions, gate_suggested_actions, resolve_selected_action,
};
#[allow(unused_imports)]
pub(crate) use advice::{
    RunStatusAdvice, SummaryContent, actionable_subflow_label, build_compact_run_status_advice,
    build_compact_run_status_advice_from_details, compact_run_status_lines,
    format_failed_preflight_check_line, render_compact_run_status_advice,
};
#[allow(unused_imports)]
pub(crate) use next_step::{build_blocked_summary_content, build_failed_summary_content};
#[allow(unused_imports)]
pub(crate) use rendering::{
    build_current_step_summary_progress, build_run_metadata_lines, emit_current_step_summary,
    emit_final_run_summary, emit_operator_summary, summary_action_only_lines, summary_detail_lines,
};
