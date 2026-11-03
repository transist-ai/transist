// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::path::Path;

use crate::application::run_protection;

/// Lists protected run ids that still reference the given workflow.
pub fn find_active_runs_for_workflow_in_home(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
) -> Result<Vec<String>> {
    let mut run_ids = run_protection::protected_runs_in_home(home_dir)?
        .into_iter()
        .filter(|(summary, _record)| {
            summary.workflow_id == workflow_id && summary.workflow_version == workflow_version
        })
        .map(|(summary, _record)| summary.run_id)
        .collect::<Vec<_>>();
    run_ids.sort();
    run_ids.dedup();
    Ok(run_ids)
}
