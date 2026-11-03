// SPDX-License-Identifier: MPL-2.0
//! Inspection queries backed by persisted run and recipe records.

use super::*;
use crate::run_paths::{list_run_roots, resolve_run_root, run_id_from_run_root};
use crate::util::{STATE_JSON, STEP_EXECUTIONS_DIR, load_record_from_path};

impl LocalInspectionStore {
    fn ensure_recipe_sync_complete(&self, run_id: &str) -> Result<()> {
        let intent = resolve_run_root(&self.root, run_id)?.join(".recipe-sync.intent.json");
        match fs::symlink_metadata(&intent) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!("failed inspecting recipe sync intent: {}", intent.display())
            }),
            Ok(_) => anyhow::bail!(
                "run '{}' has an incomplete recipe synchronization transaction",
                run_id
            ),
        }
    }

    /// Lists persisted run-state records found under the local `runs/` directory.
    pub fn list_run_records(&self) -> Result<Vec<PersistedRunRecord>> {
        let mut records = Vec::new();
        for run_root in list_run_roots(&self.root)? {
            let run_id = run_id_from_run_root(&run_root).ok_or_else(|| {
                anyhow::anyhow!("invalid persisted run root: {}", run_root.display())
            })?;
            self.ensure_recipe_sync_complete(&run_id)?;
            let state_path = run_root.join(STATE_JSON);
            if !state_path.exists() {
                continue;
            }
            records.push(load_record_from_path(&state_path)?);
        }
        Ok(records)
    }

    /// Loads a single persisted run-state record by run id.
    pub fn load_run_record(&self, run_id: &str) -> Result<PersistedRunRecord> {
        self.ensure_recipe_sync_complete(run_id)?;
        let path = resolve_run_root(&self.root, run_id)?.join(STATE_JSON);
        let fingerprint = FileFingerprint::from_path(&path)?
            .with_context(|| format!("missing run state file: {}", path.display()))?;
        let cache_key = self.cache_key(run_id)?;
        let cached = self
            .cache
            .get_state(&cache_key, fingerprint, || load_record_from_path(&path))?;
        Ok((*cached).clone())
    }

    /// Loads the compiled recipe snapshot recorded for a persisted run, if present.
    pub fn load_run_recipe(&self, run_id: &str) -> Result<Option<Recipe>> {
        self.ensure_recipe_sync_complete(run_id)?;
        let path = resolve_run_root(&self.root, run_id)?.join("recipe.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed reading recipe snapshot: {}", path.display()))?;
        let recipe = yaml_serde::from_str(&raw)
            .with_context(|| format!("invalid recipe snapshot: {}", path.display()))?;
        Ok(Some(recipe))
    }

    /// Lists persisted step-execution records for a specific run step.
    pub fn list_step_executions(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<StepExecutionRecord>> {
        let root = resolve_run_root(&self.root, run_id)?
            .join(STEP_EXECUTIONS_DIR)
            .join(step_id);
        if !root.exists() {
            return Ok(Vec::new());
        }

        // Collect and sort by parsed execution number rather than filename
        // because execution_NNN.json uses minimum-width formatting, so
        // execution_1000.json would sort before execution_999.json lexically.
        let mut entries: Vec<(u32, std::path::PathBuf)> = Vec::new();
        for entry in fs::read_dir(&root)
            .with_context(|| format!("failed reading directory: {}", root.display()))?
            .filter_map(|entry| entry.ok())
        {
            if !entry.path().is_file() {
                continue;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy().into_owned();
            if name_str.starts_with('.') || name_str.ends_with(".tmp") {
                continue;
            }
            let num: u32 = name_str
                .strip_prefix("execution_")
                .and_then(|s| s.strip_suffix(".json"))
                .and_then(|s| s.parse().ok())
                .with_context(|| {
                    format!(
                        "invalid step execution filename: {}",
                        entry.path().display()
                    )
                })?;
            entries.push((num, entry.path()));
        }
        entries.sort_by_key(|(num, _)| *num);

        let mut executions = Vec::new();
        for (ordinal, path) in entries {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed reading step execution: {}", path.display()))?;
            let execution: StepExecutionRecord = serde_json::from_str(&raw)
                .with_context(|| format!("invalid step execution json: {}", path.display()))?;
            anyhow::ensure!(
                execution.run_id == run_id
                    && execution.step_id == step_id
                    && execution.execution == ordinal,
                "step execution identity disagrees with requested run/step/ordinal"
            );
            executions.push(execution);
        }
        Ok(executions)
    }

    /// Loads only the latest step-execution record for a specific run step.
    ///
    /// Unlike [`Self::list_step_executions`], this reads exactly one file — the execution
    /// with the highest execution number — without parsing all historical cycles.
    /// Use this in display paths where only the current state of a step is needed.
    ///
    /// Returns `None` if no execution records exist for the step.
    pub fn load_latest_step_execution(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<StepExecutionRecord>> {
        let root = resolve_run_root(&self.root, run_id)?
            .join(STEP_EXECUTIONS_DIR)
            .join(step_id);
        if !root.exists() {
            return Ok(None);
        }

        // Find the file with the highest execution number without reading all of them.
        // Files are named execution_{N:03}.json; parse N from the name and take the max.
        let latest_path = fs::read_dir(&root)
            .with_context(|| format!("failed reading directory: {}", root.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .filter_map(|entry| {
                let name = entry.file_name();
                let name_str = name.to_string_lossy().into_owned();
                let num: u32 = name_str
                    .strip_prefix("execution_")?
                    .strip_suffix(".json")?
                    .parse()
                    .ok()?;
                Some((num, entry.path()))
            })
            .max_by_key(|(num, _)| *num)
            .map(|(_, path)| path);

        let Some(path) = latest_path else {
            return Ok(None);
        };

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed reading step execution: {}", path.display()))?;
        let execution = serde_json::from_str(&raw)
            .with_context(|| format!("invalid step execution json: {}", path.display()))?;
        Ok(Some(execution))
    }

    /// Lists compiled recipes persisted in the local recipe registry.
    pub fn list_compiled_recipes(&self) -> Result<Vec<Recipe>> {
        let root = self.root.join("registry").join("recipes");
        if !root.exists() {
            return Ok(Vec::new());
        }

        let mut recipes = Vec::new();
        for entry in read_dir_sorted(&root)? {
            if !entry.path().is_file() {
                continue;
            }
            let raw = fs::read_to_string(entry.path()).with_context(|| {
                format!("failed reading compiled recipe: {}", entry.path().display())
            })?;
            let recipe = yaml_serde::from_str(&raw).with_context(|| {
                format!("invalid compiled recipe yaml: {}", entry.path().display())
            })?;
            recipes.push(recipe);
        }
        recipes.sort_by(|left: &Recipe, right: &Recipe| {
            left.workflow
                .id
                .cmp(&right.workflow.id)
                .then(left.workflow.version.cmp(&right.workflow.version))
        });
        Ok(recipes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_paths::{plan_run_locator, write_run_locator};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use transist_core::{KernelRunSnapshot, PersistedRunRecord};

    /// Ensures a locator exists for `run_id` so `resolve_run_root` succeeds.
    /// Mirrors what the kernel does when persisting a real run.
    fn seed_locator(home: &std::path::Path, run_id: &str) {
        let locator = plan_run_locator(home, run_id, None).unwrap();
        write_run_locator(home, &locator).unwrap();
    }

    fn sample_record(run_id: &str) -> PersistedRunRecord {
        PersistedRunRecord::from_snapshot(
            KernelRunSnapshot::new(run_id, "wf", "v1", &["step".to_string()]),
            None,
            None,
            BTreeMap::new(),
            0,
            1,
            1,
        )
    }

    fn write_state(home: &std::path::Path, run_id: &str) {
        seed_locator(home, run_id);
        let run_root = resolve_run_root(home, run_id).unwrap();
        std::fs::create_dir_all(&run_root).unwrap();
        crate::util::write_json(&run_root.join(STATE_JSON), &sample_record(run_id)).unwrap();
    }

    #[test]
    fn state_cache_hit_when_mtime_and_len_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(tmp.path(), "run-1");
        let cache = Arc::new(InspectionCache::new(10));
        let store = LocalInspectionStore::with_cache(tmp.path(), Arc::clone(&cache));

        let _ = store.load_run_record("run-1").unwrap();
        let _ = store.load_run_record("run-1").unwrap();

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn state_cache_miss_when_len_changes() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(tmp.path(), "run-1");
        let cache = Arc::new(InspectionCache::new(10));
        let store = LocalInspectionStore::with_cache(tmp.path(), Arc::clone(&cache));
        let run_root = resolve_run_root(tmp.path(), "run-1").unwrap();
        let path = run_root.join(STATE_JSON);

        let _ = store.load_run_record("run-1").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{raw}\n")).unwrap();
        let _ = store.load_run_record("run-1").unwrap();

        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 2);
    }
}
