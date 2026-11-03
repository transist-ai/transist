// SPDX-License-Identifier: MPL-2.0
use crate::ports::{RunStorePort, RunStoreStepExecution, RunStoreStepState};
use crate::types::{KernelRunSnapshot, KernelStepState, PersistedRunRecord, StepExecutionRecord};
use anyhow::{Context, Result, bail};

/// Resolves persisted workspace metadata without permitting path drift, files,
/// or overlap with the runtime home.
pub(crate) fn persisted_workspace_path(
    home_dir: &std::path::Path,
    legacy_repo_root: &std::path::Path,
    binding: Option<&transist_protocol::WorkspaceBinding>,
) -> Result<std::path::PathBuf> {
    let Some(binding) = binding else {
        return Ok(legacy_repo_root.to_path_buf());
    };
    let canonical = std::fs::canonicalize(&binding.path)
        .with_context(|| format!("persisted workspace '{}' is unavailable", binding.path))?;
    if canonical.display().to_string() != binding.path {
        bail!("persisted workspace path has drifted: '{}'", binding.path);
    }
    if !canonical.is_dir() {
        bail!("persisted workspace '{}' is not a directory", binding.path);
    }
    let home = std::fs::canonicalize(home_dir).with_context(|| {
        format!(
            "failed canonicalizing Transist home '{}'",
            home_dir.display()
        )
    })?;
    if canonical == home || canonical.starts_with(&home) || home.starts_with(&canonical) {
        bail!(
            "persisted workspace '{}' overlaps Transist home '{}'",
            canonical.display(),
            home.display()
        );
    }
    Ok(canonical)
}

/// Persists run record state as the durable run state record.
pub fn persist_run_record(run_store: &dyn RunStorePort, record: &PersistedRunRecord) -> Result<()> {
    record.validate_contract_versions()?;
    let mut record = record.clone();
    record.refresh_executions();
    run_store.update_run_state(&record.run_id, &record)
}

/// Persists run snapshot state back into an existing durable run record.
pub fn persist_run_snapshot(
    run_store: &dyn RunStorePort,
    record: &PersistedRunRecord,
    snapshot: &KernelRunSnapshot,
) -> Result<()> {
    let mut next = record.clone();
    next.run_id = snapshot.run_id.clone();
    next.workflow_id = snapshot.workflow_id.clone();
    next.workflow_version = snapshot.workflow_version.clone();
    next.status = snapshot.status;
    next.steps = snapshot.steps.clone();
    persist_run_record(run_store, &next)
}

/// Persists a single step snapshot into an existing durable run record.
pub fn persist_step_state(
    run_store: &dyn RunStorePort,
    run_id: &str,
    step_state: &KernelStepState,
) -> Result<()> {
    run_store.store_step_state(&RunStoreStepState {
        run_id: run_id.to_string(),
        step_id: step_state.step_id.clone(),
        state: serde_json::to_value(step_state)?.into(),
    })
}

/// Persists an immutable per-execution step record.
pub fn persist_step_execution(
    run_store: &dyn RunStorePort,
    execution_record: &StepExecutionRecord,
) -> Result<()> {
    let mut execution_record = execution_record.clone();
    if let Some(documents) = run_store.document_store() {
        execution_record.document_revisions = if execution_record.status == "success" {
            documents.prepared_document_revisions(
                &execution_record.run_id,
                &execution_record.step_id,
                execution_record.execution,
            )?
        } else {
            Vec::new()
        };
    } else if !execution_record.document_revisions.is_empty() {
        bail!("document storage unavailable");
    }
    run_store.store_step_execution(&RunStoreStepExecution {
        run_id: execution_record.run_id.clone(),
        step_id: execution_record.step_id.clone(),
        execution: execution_record.execution,
        state: serde_json::to_value(execution_record)?.into(),
    })
}

/// Loads an existing durable run record if present.
///
/// Run absence is a distinct port outcome: `Ok(None)` means no run record
/// exists. Corruption and I/O errors are propagated as `Err` so a corrupt or
/// unreadable run state is never reported as "not found".
pub fn load_run_record(
    run_store: &dyn RunStorePort,
    run_id: &str,
) -> Result<Option<PersistedRunRecord>> {
    let record = run_store.load_run(run_id)?;
    if let Some(record) = &record {
        record.validate_contract_versions()?;
    }
    Ok(record)
}

/// Loads a durable run record and rebases all persisted invoke paths against
/// the live `(home_dir, repo_root)` so resume, replay, cleanup, and inspection
/// behave correctly after a repo or home move on the same host.
///
/// Returns `Ok(None)` when the record is absent.
pub fn load_and_rebase_run_record(
    run_store: &dyn RunStorePort,
    home_dir: &std::path::Path,
    repo_root: &std::path::Path,
    run_id: &str,
) -> Result<Option<PersistedRunRecord>> {
    let mut record = match load_run_record(run_store, run_id)? {
        Some(record) => record,
        None => return Ok(None),
    };
    let workspace = persisted_workspace_path(home_dir, repo_root, record.workspace.as_ref())?;
    run_store.rebase_persisted_run_record(&mut record, home_dir, &workspace)?;
    Ok(Some(record))
}
