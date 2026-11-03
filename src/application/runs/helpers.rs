// SPDX-License-Identifier: MPL-2.0
use super::{RunReport, execution};
use crate::composition::router_cache::RouterCache;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use transist_core::RunStorePort;
use transist_protocol::Recipe;
use transist_storage_local::run_paths::{
    RunRootOutcome, resolve_root_run_id, resolve_run_root_outcome,
};
use transist_storage_local::{LocalRunStore, LocalStorageConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActiveExecutionResolution {
    pub(super) run_id: String,
    pub(super) step_id: String,
}

pub(super) fn resolve_actionable_failed_run_path(
    home_dir: &Path,
    run_id: &str,
) -> Result<Vec<String>> {
    let mut current_run_id = run_id.to_string();
    let mut run_path = vec![current_run_id.clone()];
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(current_run_id.clone()) {
            bail!(
                "run '{}' contains a cycle while resolving actionable failed subflows",
                run_id
            );
        }
        let Some(next_run_id) = next_actionable_subflow_run(home_dir, &current_run_id)? else {
            return Ok(run_path);
        };
        current_run_id = next_run_id.clone();
        run_path.push(next_run_id);
    }
}

pub(super) fn resolve_active_execution(
    home_dir: &Path,
    run_id: &str,
) -> Result<Option<ActiveExecutionResolution>> {
    let root_run_id = resolve_root_run_id(home_dir, run_id)?;
    let mut visited = std::collections::BTreeSet::new();
    resolve_active_execution_recursive(home_dir, &root_run_id, &mut visited)
}

pub(super) fn next_actionable_subflow_run(home_dir: &Path, run_id: &str) -> Result<Option<String>> {
    let run = serde_json::Value::from(execution::show_run(home_dir, run_id)?);
    let steps = run["steps"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("run '{}' state is missing steps", run_id))?;
    let recipe = load_persisted_recipe(home_dir, run_id)?;
    let mut preferred = None;
    let mut fallback = None;
    for step in recipe.steps.iter().filter(|step| {
        matches!(
            step.execution,
            transist_protocol::StepExecutionDescriptor::Subflow { .. }
        )
    }) {
        let step_state = steps.get(&step.id).unwrap_or(&serde_json::Value::Null);
        let Some(child_run_id) = child_run_id_for_step_state(step_state) else {
            continue;
        };
        let Ok(child) = execution::show_run(home_dir, &child_run_id) else {
            continue;
        };
        let child = serde_json::Value::from(child);
        if child["status"].as_str() == Some("success") {
            continue;
        }
        let step_status = step_state["status"].as_str().unwrap_or_default();
        let child_status = child["status"].as_str().unwrap_or_default();
        if matches!(
            step_status,
            "failed" | "blocked" | "running" | "dispatching" | "canceled"
        ) || matches!(
            child_status,
            "failed" | "blocked" | "running" | "waiting" | "canceled"
        ) {
            preferred = Some(child_run_id);
            break;
        }
        let sort_key = step_sort_key(step_state);
        match &fallback {
            Some((best_key, _)) if *best_key >= sort_key => {}
            _ => fallback = Some((sort_key, child_run_id)),
        }
    }
    Ok(preferred.or_else(|| fallback.map(|(_, child_run_id)| child_run_id)))
}

fn resolve_active_execution_recursive(
    home_dir: &Path,
    run_id: &str,
    visited: &mut std::collections::BTreeSet<String>,
) -> Result<Option<ActiveExecutionResolution>> {
    if !visited.insert(run_id.to_string()) {
        bail!(
            "run '{}' contains a cycle while resolving an active cancellation target",
            run_id
        );
    }
    let run = serde_json::Value::from(execution::show_run(home_dir, run_id)?);
    // Repeated cancellation fulfills the same root operation; its cleanup is
    // not another primary invocation to interrupt, even from a contained id.
    if run["status"].as_str() == Some("canceled")
        && run["cleanup"]["trigger"].as_str() == Some("canceled")
    {
        return Ok(None);
    }
    let steps = run["steps"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("run '{}' state is missing steps", run_id))?;
    let Some((step_id, _)) = active_step_entry(steps) else {
        return Ok(None);
    };
    let recipe = load_persisted_recipe(home_dir, run_id)?;
    let step = recipe
        .steps
        .iter()
        .find(|step| step.id == *step_id)
        .ok_or_else(|| {
            anyhow::anyhow!("active step '{}' is missing from persisted recipe", step_id)
        })?;
    if matches!(
        step.execution,
        transist_protocol::StepExecutionDescriptor::Subflow { .. }
    ) && let Some(child_run_id) =
        active_child_run_id_for_step_state(home_dir, run_id, step_id, &steps[step_id])?
    {
        // An unpublished child is a normal in-flight subflow state, not corruption.
        if run_exists(home_dir, &child_run_id)? {
            let child = serde_json::Value::from(execution::show_run(home_dir, &child_run_id)?);
            let child_status = child["status"].as_str().unwrap_or_default();
            if !matches!(child_status, "success" | "failed" | "canceled")
                && let Some(resolution) =
                    resolve_active_execution_recursive(home_dir, &child_run_id, visited)?
            {
                return Ok(Some(resolution));
            }
        }
    }
    if matches!(
        step.execution,
        transist_protocol::StepExecutionDescriptor::Invoke { .. }
    ) && steps[step_id]["last_request"]["invoke_id"]
        .as_str()
        .is_some()
    {
        return Ok(Some(ActiveExecutionResolution {
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
        }));
    }
    Ok(None)
}

pub(super) fn child_run_id_for_step_state(step_state: &serde_json::Value) -> Option<String> {
    step_state["last_result"]["output"]["child_run_id"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn active_child_run_id_for_step_state(
    home_dir: &Path,
    run_id: &str,
    step_id: &str,
    step_state: &serde_json::Value,
) -> Result<Option<String>> {
    if let Some(child_run_id) = child_run_id_for_step_state(step_state) {
        return Ok(Some(child_run_id));
    }
    if !matches!(
        step_state["status"].as_str(),
        Some("running" | "dispatching")
    ) {
        return Ok(None);
    }

    // Legacy child runs have no published child locator. Probe their expected
    // location only when it exists; absence is an ordinary in-flight state.
    let legacy_child_run_id = format!("{run_id}__{step_id}");
    run_exists(home_dir, &legacy_child_run_id).map(|exists| exists.then_some(legacy_child_run_id))
}

fn run_exists(home_dir: &Path, run_id: &str) -> Result<bool> {
    let run_root = match resolve_run_root_outcome(home_dir, run_id)? {
        RunRootOutcome::Resolved(path) | RunRootOutcome::LegacyAbsent(path) => path,
    };
    let state_path = run_root.join("state.json");
    match fs::metadata(&state_path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(anyhow::anyhow!(
            "run state path is not a regular file: {}",
            state_path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("failed inspecting run state: {}", state_path.display())),
    }
}

pub(super) fn reenter_run_path_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_path: &[String],
) -> Result<RunReport> {
    let Some(root_run_id) = run_path.first() else {
        bail!("cannot re-enter an empty run path")
    };
    let mut latest = None;
    for run_id in run_path.iter().rev() {
        latest = Some(resume_or_reenter_run_with_cache(
            repo_root,
            home_dir,
            router_cache,
            run_id,
        )?);
    }
    if latest.as_ref().map(|report| &report.run_id) == Some(root_run_id) {
        return latest.ok_or_else(|| anyhow::anyhow!("re-entry produced no run report"));
    }
    execution::report_for_run(home_dir, root_run_id)
}

pub(super) fn resume_or_reenter_run_with_cache(
    repo_root: &Path,
    home_dir: &Path,
    router_cache: &RouterCache,
    run_id: &str,
) -> Result<RunReport> {
    match run_status(home_dir, run_id)?.as_str() {
        "failed" | "canceled" => execution::resume_run_with_trust_and_cache(
            repo_root,
            home_dir,
            router_cache,
            run_id,
            false,
            false,
        ),
        "created" | "planned" | "running" | "waiting" | "blocked" => {
            execution::run_started_run_with_trust_and_cache(
                repo_root,
                home_dir,
                router_cache,
                run_id,
                false,
                false,
            )
        }
        "success" => execution::report_for_run(home_dir, run_id),
        other => bail!(
            "run '{}' cannot be re-entered from unexpected status '{}'",
            run_id,
            other
        ),
    }
}

pub(super) fn run_status(home_dir: &Path, run_id: &str) -> Result<String> {
    let run = serde_json::Value::from(execution::show_run(home_dir, run_id)?);
    Ok(run["status"].as_str().unwrap_or_default().to_string())
}

fn step_sort_key(step: &serde_json::Value) -> u64 {
    step["finished_at_ms"]
        .as_u64()
        .or_else(|| step["started_at_ms"].as_u64())
        .unwrap_or_default()
}

fn active_step_entry(
    steps: &serde_json::Map<String, serde_json::Value>,
) -> Option<(&String, &serde_json::Value)> {
    steps
        .iter()
        .filter(|(_, step)| is_cancellable_active_status(step["status"].as_str()))
        .max_by_key(|(_, step)| {
            (
                step["started_at_ms"].as_u64().unwrap_or_default(),
                step["executions"].as_u64().unwrap_or_default(),
            )
        })
}

fn is_cancellable_active_status(status: Option<&str>) -> bool {
    matches!(status, Some("running" | "dispatching"))
}

pub(super) fn load_persisted_recipe(home_dir: &Path, run_id: &str) -> Result<Recipe> {
    let recipe_raw = LocalRunStore::new(LocalStorageConfig::new(home_dir))
        .load_recipe_snapshot(run_id)
        .context("failed reading persisted recipe")?;
    yaml_serde::from_str(&recipe_raw).context("invalid persisted recipe")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_persisted_recipe_reads_legacy_run_without_locator() {
        let home_dir = tempfile::tempdir().expect("home");
        let run_root = home_dir.path().join("runs").join("run_legacy-run");
        fs::create_dir_all(&run_root).expect("legacy run root");
        fs::write(
            run_root.join("recipe.yaml"),
            "schema: transist.recipe.v1\nworkflow:\n  id: legacy\n  version: v1\n",
        )
        .expect("persist recipe");

        let recipe = load_persisted_recipe(home_dir.path(), "legacy-run").expect("load recipe");

        assert_eq!(recipe.workflow.id, "legacy");
        assert_eq!(recipe.workflow.version, "v1");
    }
}
