// SPDX-License-Identifier: MPL-2.0
use crate::run_paths::{list_run_roots, resolve_run_root, run_id_from_run_root};
use crate::util::{EVENTS_JSONL, STATE_JSON};
use crate::{LocalRunStore, LocalStorageConfig};
use anyhow::{Context, Result};
use std::path::PathBuf;
use transist_protocol::{KernelValue, RuntimeEvent};

/// Lightweight local query helpers used by the root facade.
#[derive(Debug)]
pub struct LocalRunQueryService {
    root: PathBuf,
}

impl LocalRunQueryService {
    fn recover_recipe_sync(&self, run_id: &str) -> Result<()> {
        LocalRunStore::new(LocalStorageConfig::new(&self.root)).recover_recipe_sync(run_id)
    }

    /// Creates a run query service rooted at a local storage directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Lists run IDs available in local storage.
    pub fn list_runs(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for run_root in list_run_roots(&self.root)? {
            if let Some(run_id) = run_id_from_run_root(&run_root) {
                out.push(run_id);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Loads full run state for `run_id`.
    pub fn show_run(&self, run_id: &str) -> Result<KernelValue> {
        self.recover_recipe_sync(run_id)?;
        let path = resolve_run_root(&self.root, run_id)?.join(STATE_JSON);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed reading run state: {}", path.display()))?;
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("invalid run state json: {}", path.display()))?;
        Ok(parsed.into())
    }

    /// Returns up to `limit` latest events for `run_id`.
    ///
    /// Tolerates at most one non-newline-terminated final fragment
    /// (retained when it is a complete JSON value, dropped otherwise),
    /// consistent with the inspection store's event reader
    /// (`read_jsonl_records`/`parse_events_full`), so both surfaces agree
    /// on a truncated tail instead of this surface erroring where the
    /// inspection store would tolerate it.
    pub fn tail_events(&self, run_id: &str, limit: usize) -> Result<Vec<RuntimeEvent>> {
        self.recover_recipe_sync(run_id)?;
        let path = resolve_run_root(&self.root, run_id)?.join(EVENTS_JSONL);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let records = crate::atomic_io::read_jsonl_records(&path)?;
        let mut events = Vec::new();
        for raw in records {
            let event = serde_json::from_slice::<RuntimeEvent>(&raw)
                .with_context(|| format!("invalid event line in {}", path.display()))?;
            events.push(event);
        }
        if events.len() <= limit {
            return Ok(events);
        }
        Ok(events[events.len() - limit..].to_vec())
    }
}
