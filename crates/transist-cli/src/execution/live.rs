// SPDX-License-Identifier: MPL-2.0
//! Read-only progress/summary helpers retained after the migration to the
//! detached-runner model. Historically this module also hosted
//! `execute_with_live_status` and the `LiveCommandUiState` plumbing that
//! drove the foreground cooked-mode renderer; those have been deleted now
//! that all mutating commands spawn detached runners and the foreground
//! attaches the fullscreen run-detail viewer instead. The remaining
//! functions are pure inspection helpers used by the fullscreen run-detail
//! viewer (`inspect_bootstrap_progress`) and the spinner/progress display
//! (`inspect_bootstrap_progress_state`).
//!
//! All entry points here are read-only — they never mutate run state or
//! the durable control channel.

use anyhow::Result;

use crate::display::output::AttachedStepOutputBlocks;
use crate::display::progress::{
    CurrentStepStateRef, ProgressState, ProgressStateFromStatesArgs,
    build_progress_state_from_states, select_bootstrap_progress_step_state,
};
use crate::display::spinner::{LiveProgress, step_tool_name_from_run};

use super::run_state::{
    RecipeCacheOwner, load_recipe_step_order, load_run_state, resolve_actionable_run_state,
};

pub(crate) fn inspect_bootstrap_progress(
    context: &transist::TransistContext,
    root_run_id: &str,
    attached_step_output_blocks: AttachedStepOutputBlocks,
    recipe_cache: &RecipeCacheOwner,
) -> Result<Option<LiveProgress>> {
    Ok(
        inspect_bootstrap_progress_state(context, root_run_id, recipe_cache)?.map(|state| {
            LiveProgress {
                state,
                preview_stream: None,
                preview_text: None,
                attached_step_output_blocks,
                footer_lines: Vec::new(),
            }
        }),
    )
}

pub(crate) fn inspect_bootstrap_progress_state(
    context: &transist::TransistContext,
    root_run_id: &str,
    recipe_cache: &RecipeCacheOwner,
) -> Result<Option<ProgressState>> {
    let root_step_order =
        load_recipe_step_order(context, root_run_id, recipe_cache)?.unwrap_or_default();
    let Some(root_state) = load_run_state(context, root_run_id)? else {
        return Ok(None);
    };
    let Some(actionable) = resolve_actionable_run_state(context, root_run_id, recipe_cache)? else {
        return Ok(None);
    };
    let Some((step_id, step)) =
        select_bootstrap_progress_step_state(&actionable.state, &root_step_order)
    else {
        return Ok(None);
    };
    let tool_name = if step["kind"].as_str() == Some("invoke") {
        step_tool_name_from_run(&actionable.state, &step_id)
    } else {
        None
    };
    Ok(Some(build_progress_state_from_states(
        context,
        ProgressStateFromStatesArgs {
            root_run_id,
            root_state: &root_state,
            root_step_order: &root_step_order,
            actionable: &actionable,
            current_step: CurrentStepStateRef {
                step_id: &step_id,
                step,
            },
            live_active_override: false,
        },
        tool_name,
    )?))
}
