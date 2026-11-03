// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use lru::LruCache;
use serde_json::Value;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::display::progress::{
    ActionableRunState, StepTimingBounds, StepTimingOverrides, merge_step_timing_bounds,
    persisted_run_execution_timestamps, run_state_step_timestamps,
};

/// Lightweight fingerprint for persisted recipe files,
/// used to detect changes to a persisted `recipe.yaml` without re-parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecipeFileFingerprint {
    mtime_nanos: i128,
    len: u64,
}

impl RecipeFileFingerprint {
    fn from_path(path: &Path) -> Result<Option<Self>> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mtime_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as i128)
            .unwrap_or(0);
        Ok(Some(Self {
            mtime_nanos,
            len: metadata.len(),
        }))
    }
}

/// Parsed `recipe.yaml` payload held by the module-local cache. The structured
/// `Recipe` form is used by step-order helpers; the YAML form is used by
/// `run_state_has_blocked_manual_gate` to inspect raw `control.gate.manual`
/// markers without depending on `Recipe` fields.
#[derive(Debug, Clone)]
struct RecipeCachePayload {
    recipe: Arc<transist::Recipe>,
    yaml: Arc<yaml_serde::Value>,
}

#[derive(Debug, Clone)]
struct RecipeCacheEntry {
    fingerprint: RecipeFileFingerprint,
    payload: RecipeCachePayload,
}

fn recipe_cache_enabled() -> bool {
    !std::env::var(transist::INSPECTION_CACHE_DISABLE_ENV)
        .ok()
        .map(|value| value.trim().eq_ignore_ascii_case("off"))
        .unwrap_or(false)
}

const RECIPE_CACHE_CAPACITY: usize = 32;

/// Owns the process-local recipe memo for one CLI execution/display lifetime.
#[derive(Debug)]
pub(crate) struct RecipeCacheOwner {
    cache: Mutex<LruCache<PathBuf, RecipeCacheEntry>>,
}

impl Default for RecipeCacheOwner {
    fn default() -> Self {
        Self {
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(RECIPE_CACHE_CAPACITY).expect("non-zero recipe cache capacity"),
            )),
        }
    }
}

impl RecipeCacheOwner {
    fn lock(&self) -> MutexGuard<'_, LruCache<PathBuf, RecipeCacheEntry>> {
        match self.cache.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = LruCache::new(
                    NonZeroUsize::new(RECIPE_CACHE_CAPACITY)
                        .expect("non-zero recipe cache capacity"),
                );
                self.cache.clear_poison();
                guard
            }
        }
    }
}

fn canonical_recipe_cache_key(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to canonicalize recipe path: {}", path.display()))
}

/// Loads and parses `recipe.yaml` at `path`, returning both the structured
/// `Recipe` and the raw `yaml_serde::Value` forms. Results are memoized by
/// path keyed on the file's mtime+length fingerprint so subsequent calls in
/// the same process skip the disk read and YAML parse when the file is
/// unchanged. The caching layer can be disabled by setting the env var named
/// in `transist::INSPECTION_CACHE_DISABLE_ENV` to `off`.
fn load_recipe_payload(
    path: &Path,
    cache_owner: &RecipeCacheOwner,
) -> Result<Option<RecipeCachePayload>> {
    let Some(fingerprint) = RecipeFileFingerprint::from_path(path)? else {
        return Ok(None);
    };

    let cache_key = canonical_recipe_cache_key(path)?;
    if recipe_cache_enabled() {
        let mut guard = cache_owner.lock();
        if let Some(entry) = guard.get(&cache_key)
            && entry.fingerprint == fingerprint
        {
            return Ok(Some(entry.payload.clone()));
        }
    }

    let raw = std::fs::read_to_string(path)?;
    let yaml: yaml_serde::Value = yaml_serde::from_str(&raw)?;
    let recipe: transist::Recipe = yaml_serde::from_str(&raw)?;
    let payload = RecipeCachePayload {
        recipe: Arc::new(recipe),
        yaml: Arc::new(yaml),
    };

    if recipe_cache_enabled() {
        let mut guard = cache_owner.lock();
        guard.put(
            cache_key,
            RecipeCacheEntry {
                fingerprint,
                payload: payload.clone(),
            },
        );
    }

    Ok(Some(payload))
}

pub(crate) fn load_run_state(
    context: &transist::TransistContext,
    run_id: &str,
) -> Result<Option<Value>> {
    match context.runs_show(run_id) {
        Ok(run) => Ok(Some(Value::from(run))),
        Err(_) => Ok(None),
    }
}

pub(crate) fn resolve_actionable_run_state(
    context: &transist::TransistContext,
    root_run_id: &str,
    cache_owner: &RecipeCacheOwner,
) -> Result<Option<ActionableRunState>> {
    use std::collections::BTreeSet;
    let mut current_run_id = root_run_id.to_string();
    let mut visited = BTreeSet::new();

    loop {
        if !visited.insert(current_run_id.clone()) {
            return Ok(None);
        }
        let Some(state) = load_run_state(context, &current_run_id)? else {
            return Ok(None);
        };
        let next =
            next_actionable_subflow_run_state(context, &current_run_id, &state, cache_owner)?;
        if let Some(next_run_id) = next {
            current_run_id = next_run_id;
            continue;
        }
        return Ok(Some(ActionableRunState {
            run_id: current_run_id,
            state,
        }));
    }
}

pub(crate) fn next_actionable_subflow_run_state(
    context: &transist::TransistContext,
    run_id: &str,
    run_state: &Value,
    cache_owner: &RecipeCacheOwner,
) -> Result<Option<String>> {
    if run_state_has_blocked_manual_gate(context, run_id, run_state, cache_owner)? {
        return Ok(None);
    }

    let Some(steps) = run_state["steps"].as_object() else {
        return Ok(None);
    };
    let mut preferred = None;
    let mut fallback = None;

    for (step_id, step) in steps {
        let Some(child_run_id) = child_run_id_for_state_step(context, run_id, step_id, step)?
        else {
            continue;
        };
        let Some(child) = load_run_state(context, &child_run_id)? else {
            continue;
        };
        if child["status"].as_str() == Some("success") {
            continue;
        }
        let step_status = step["status"].as_str().unwrap_or_default();
        let child_status = child["status"].as_str().unwrap_or_default();
        if matches!(
            step_status,
            "failed" | "blocked" | "running" | "dispatching" | "retry_wait"
        ) || matches!(
            child_status,
            "failed" | "blocked" | "running" | "dispatching" | "waiting"
        ) {
            preferred = Some(child_run_id);
            break;
        }
        let sort_key = step["finished_at_ms"]
            .as_u64()
            .or_else(|| step["started_at_ms"].as_u64())
            .unwrap_or_default();
        match &fallback {
            Some((best_key, _)) if *best_key >= sort_key => {}
            _ => fallback = Some((sort_key, child_run_id)),
        }
    }

    Ok(preferred.or_else(|| fallback.map(|(_, run_id)| run_id)))
}

pub(crate) fn child_run_id_for_state_step(
    context: &transist::TransistContext,
    run_id: &str,
    step_id: &str,
    step: &Value,
) -> Result<Option<String>> {
    if let Some(child_run_id) = step["last_result"]["output"]["child_run_id"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    {
        return Ok(Some(child_run_id));
    }
    if !matches!(step["status"].as_str(), Some("running" | "dispatching")) {
        return Ok(None);
    }

    let derived_child_run_id = format!("{run_id}__{step_id}");
    Ok(load_run_state(context, &derived_child_run_id)?.map(|_| derived_child_run_id))
}

pub(crate) fn subflow_step_timing_overrides_for_state(
    context: &transist::TransistContext,
    root_run_id: &str,
    root_state: &Value,
) -> Result<StepTimingOverrides> {
    let Some(steps) = root_state["steps"].as_object() else {
        return Ok(BTreeMap::new());
    };
    let mut overrides = BTreeMap::new();
    for (step_id, step) in steps {
        let Some(child_run_id) = child_run_id_for_state_step(context, root_run_id, step_id, step)?
        else {
            continue;
        };
        let Some(child_state) = load_run_state(context, &child_run_id)? else {
            continue;
        };
        let persisted_execution_timing =
            persisted_run_execution_timestamps(context.home_dir(), &child_run_id)?;
        overrides.insert(
            step_id.clone(),
            merge_step_timing_bounds(
                StepTimingBounds {
                    started_at_ms: step["started_at_ms"].as_u64(),
                    finished_at_ms: step["finished_at_ms"].as_u64(),
                },
                merge_step_timing_bounds(
                    run_state_step_timestamps(&child_state),
                    persisted_execution_timing,
                ),
            ),
        );
    }
    Ok(overrides)
}

fn run_state_has_blocked_manual_gate(
    context: &transist::TransistContext,
    run_id: &str,
    run_state: &Value,
    cache_owner: &RecipeCacheOwner,
) -> Result<bool> {
    let Some(steps) = run_state["steps"].as_object() else {
        return Ok(false);
    };
    let Some(recipe) = load_persisted_recipe_yaml(context, run_id, cache_owner)? else {
        return Ok(false);
    };
    let Some(recipe_steps) = recipe.get("steps").and_then(|value| value.as_sequence()) else {
        return Ok(false);
    };
    Ok(recipe_steps.iter().any(|step| {
        let step_id = step.get("id").and_then(|value| value.as_str());
        let is_manual_gate =
            step.get("control").and_then(|value| value.as_str()) == Some("control.gate.manual");
        is_manual_gate
            && step_id
                .and_then(|step_id| steps.get(step_id))
                .and_then(|state| state["status"].as_str())
                == Some("blocked")
    }))
}

fn load_persisted_recipe_yaml(
    context: &transist::TransistContext,
    run_id: &str,
    cache_owner: &RecipeCacheOwner,
) -> Result<Option<Arc<yaml_serde::Value>>> {
    let recipe_path = context.run_root_or_default(run_id)?.join("recipe.yaml");
    Ok(load_recipe_payload(&recipe_path, cache_owner)?.map(|payload| payload.yaml))
}

pub(crate) fn load_recipe_step_order(
    context: &transist::TransistContext,
    run_id: &str,
    cache_owner: &RecipeCacheOwner,
) -> Result<Option<Vec<String>>> {
    let Some(recipe) = load_persisted_recipe(context, run_id, cache_owner)? else {
        return Ok(None);
    };
    Ok(Some(
        recipe.steps.iter().map(|step| step.id.clone()).collect(),
    ))
}

pub(crate) fn load_visible_recipe_step_order(
    context: &transist::TransistContext,
    run_id: &str,
    run_state: &Value,
) -> Result<Option<Vec<String>>> {
    context.visible_step_ids(run_id, &transist::KernelValue::from(run_state.clone()))
}

fn load_persisted_recipe(
    context: &transist::TransistContext,
    run_id: &str,
    cache_owner: &RecipeCacheOwner,
) -> Result<Option<Arc<transist::Recipe>>> {
    let recipe_path = context.run_root_or_default(run_id)?.join("recipe.yaml");
    Ok(load_recipe_payload(&recipe_path, cache_owner)?.map(|payload| payload.recipe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_recipe_step_order_returns_none_when_recipe_snapshot_is_absent() {
        let home = tempfile::tempdir().expect("home");
        let run_root = home.path().join("runs").join("run_run_missing_recipe");
        std::fs::create_dir_all(&run_root).expect("run root");
        let context = transist::TransistContext::with_home(home.path(), home.path());

        assert_eq!(
            load_visible_recipe_step_order(&context, "run_missing_recipe", &serde_json::json!({}),)
                .expect("visible step order"),
            None
        );
    }

    fn recipe_cache_entry() -> RecipeCacheEntry {
        RecipeCacheEntry {
            fingerprint: RecipeFileFingerprint {
                mtime_nanos: 0,
                len: 0,
            },
            payload: RecipeCachePayload {
                recipe: Arc::new(yaml_serde::from_str(
                    "schema: transist.recipe.v1\nworkflow:\n  id: test\n  version: v1\nsteps: []\n",
                )
                .expect("recipe")),
                yaml: Arc::new(yaml_serde::Value::Null),
            },
        }
    }

    #[test]
    fn recipe_cache_has_capacity_32_and_evicts_least_recently_used_entry() {
        let owner = RecipeCacheOwner::default();
        for index in 0..33 {
            owner.lock().put(
                PathBuf::from(format!("recipe-{index}")),
                recipe_cache_entry(),
            );
        }

        let cache = owner.lock();
        assert_eq!(cache.cap().get(), RECIPE_CACHE_CAPACITY);
        assert_eq!(cache.len(), RECIPE_CACHE_CAPACITY);
        assert!(!cache.contains(&PathBuf::from("recipe-0")));
        assert!(cache.contains(&PathBuf::from("recipe-1")));
    }

    #[test]
    fn recipe_cache_owners_are_isolated_and_canonical_key_rejects_missing_paths() {
        let first = RecipeCacheOwner::default();
        let second = RecipeCacheOwner::default();
        first
            .lock()
            .put(PathBuf::from("same"), recipe_cache_entry());
        assert_eq!(first.lock().len(), 1);
        assert_eq!(second.lock().len(), 0);

        let home = tempfile::tempdir().expect("home");
        let nested = home.path().join("nested");
        std::fs::create_dir(&nested).expect("nested");
        let path = home.path().join("recipe.yaml");
        std::fs::write(&path, "recipe").expect("recipe file");
        let alias = nested.join("..").join("recipe.yaml");
        assert_eq!(
            canonical_recipe_cache_key(&path).expect("canonical path"),
            canonical_recipe_cache_key(&alias).expect("canonical alias")
        );
        assert!(canonical_recipe_cache_key(&home.path().join("missing.yaml")).is_err());
    }

    #[test]
    fn recipe_cache_recovers_from_poison_and_supports_concurrent_access() {
        let owner = Arc::new(RecipeCacheOwner::default());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.cache.lock().expect("cache lock");
            panic!("poison recipe cache");
        }));
        assert!(owner.cache.is_poisoned());
        let cache = owner.lock();
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.cap().get(), RECIPE_CACHE_CAPACITY);
        drop(cache);
        assert!(!owner.cache.is_poisoned());

        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let owner = Arc::clone(&owner);
                std::thread::spawn(move || {
                    owner.lock().put(
                        PathBuf::from(format!("worker-{worker}")),
                        recipe_cache_entry(),
                    );
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("worker");
        }
        assert_eq!(owner.lock().len(), 8);
    }
}
