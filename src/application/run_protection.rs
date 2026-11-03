// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::path::Path;
use transist_core::{KernelRunRecoveryState, PersistedRunRecord};
use transist_storage_local::{
    LocalInspectionStore, LocalRunSummaryStore, PersistedRunSummaryRecord,
};

/// Returns protected runs in the current home that Transist-managed lifecycle
/// commands must not invalidate.
pub(crate) fn protected_runs_in_home(
    home_dir: &Path,
) -> Result<Vec<(PersistedRunSummaryRecord, PersistedRunRecord)>> {
    let inspection = LocalInspectionStore::new(home_dir);
    let mut protected = Vec::new();
    for summary in LocalRunSummaryStore::new(home_dir).list_run_summaries()? {
        let record = inspection.load_run_record(&summary.run_id)?;
        if is_protected_run(&record) {
            protected.push((summary, record));
        }
    }
    Ok(protected)
}

pub(crate) fn is_protected_run(record: &PersistedRunRecord) -> bool {
    match record.status {
        transist_core::KernelRunStatus::Success => false,
        transist_core::KernelRunStatus::Failed => {
            record.recovery_state != Some(KernelRunRecoveryState::CleanupCompleted)
        }
        transist_core::KernelRunStatus::Created
        | transist_core::KernelRunStatus::Planned
        | transist_core::KernelRunStatus::Running
        | transist_core::KernelRunStatus::Waiting
        | transist_core::KernelRunStatus::Blocked
        | transist_core::KernelRunStatus::Canceled => true,
    }
}
