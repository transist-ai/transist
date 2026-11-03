// SPDX-License-Identifier: MPL-2.0
//! Inspection queries backed by append-only classified failure records.

use super::*;
use crate::run_paths::resolve_run_root;
use crate::util::FAILURES_JSONL;
use transist_core::FailureRecord;

impl LocalInspectionStore {
    /// Lists classified failure records from the run's `failures.jsonl` stream.
    pub fn list_failures(&self, run_id: &str) -> Result<Vec<FailureRecord>> {
        let path = resolve_run_root(&self.root, run_id)?.join(FAILURES_JSONL);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let records = crate::atomic_io::read_jsonl_records(&path)?;
        let mut failures = Vec::new();
        for raw in records {
            let failure = serde_json::from_slice::<FailureRecord>(&raw)
                .with_context(|| format!("invalid failure line in {}", path.display()))?;
            failures.push(failure);
        }
        Ok(failures)
    }

    /// Loads one classified failure record by id from the run's failure stream.
    pub fn get_failure(&self, run_id: &str, failure_id: &str) -> Result<Option<FailureRecord>> {
        Ok(self
            .list_failures(run_id)?
            .into_iter()
            .find(|failure| failure.failure_id == failure_id))
    }
}
