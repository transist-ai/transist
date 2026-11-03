// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use std::path::Path;
use transist_core::AssetLockKind;
use transist_core::ports::RunStorePort;
use transist_protocol::{Recipe, StepExecutionDescriptor, for_each_invoke_step};

use crate::application::run_protection;

/// Lists active run ids that still reference the given plugin id and optional version.
pub fn find_active_runs_for_plugin_in_home(
    home_dir: &Path,
    plugin_id: &str,
    plugin_version: Option<&str>,
) -> Result<Vec<String>> {
    let store = transist_storage_local::LocalRunStore::new(
        transist_storage_local::LocalStorageConfig::new(home_dir),
    );
    let mut run_ids = Vec::new();
    for (summary, record) in run_protection::protected_runs_in_home(home_dir)? {
        let raw = store.load_recipe_snapshot(&summary.run_id).with_context(|| {
            format!(
                "cannot determine whether protected run '{}' references plugin '{}': persisted recipe snapshot could not be loaded",
                summary.run_id, plugin_id
            )
        })?;
        let recipe = yaml_serde::from_str::<Recipe>(&raw).with_context(|| {
            format!(
                "cannot determine whether protected run '{}' references plugin '{}': persisted recipe snapshot is invalid",
                summary.run_id, plugin_id
            )
        })?;
        if run_record_references_plugin(&record, Some(&recipe), plugin_id, plugin_version) {
            run_ids.push(summary.run_id);
        }
    }
    run_ids.sort();
    run_ids.dedup();
    Ok(run_ids)
}

fn run_record_references_plugin(
    record: &transist_core::PersistedRunRecord,
    recipe: Option<&Recipe>,
    plugin_id: &str,
    plugin_version: Option<&str>,
) -> bool {
    if record.asset_locks.iter().any(|lock| {
        lock.kind == AssetLockKind::Plugin
            && lock.ref_id == plugin_id
            && plugin_version.is_none_or(|version| lock.version.as_deref() == Some(version))
    }) {
        return true;
    }
    recipe.is_some_and(|recipe| recipe_references_plugin(recipe, plugin_id, plugin_version))
}

fn recipe_references_plugin(
    recipe: &Recipe,
    plugin_id: &str,
    plugin_version: Option<&str>,
) -> bool {
    let mut referenced = false;
    for_each_invoke_step(recipe, &mut |step, _is_cleanup| {
        if !referenced && step_references_plugin(&step.execution, plugin_id, plugin_version) {
            referenced = true;
        }
    });
    referenced
}

fn step_references_plugin(
    execution: &StepExecutionDescriptor,
    plugin_id: &str,
    plugin_version: Option<&str>,
) -> bool {
    let StepExecutionDescriptor::Invoke {
        plugin,
        plugin_version: step_plugin_version,
        ..
    } = execution
    else {
        return false;
    };
    if plugin != plugin_id {
        return false;
    }
    match plugin_version {
        Some(version) => step_plugin_version
            .as_deref()
            .is_none_or(|current| current == version),
        None => true,
    }
}
