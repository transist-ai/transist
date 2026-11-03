// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const WORKFLOW_INSTALL_INDEX_SCHEMA_V1: &str = "transist.workflow_install_index.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DefinitionValidation {
    pub(crate) validator_revision: u32,
    pub(crate) recipe_sha256: String,
    pub(crate) plugin_package_digests: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkflowInstallIndexEntry {
    pub(crate) workflow_id: String,
    pub(crate) workflow_version: String,
    pub(crate) workflow_definition_sha256: String,
    /// SHA-256 digest of the installed recipe bytes.
    ///
    /// This lets callers distinguish a complete, unchanged installation from
    /// an index entry whose recipe was modified outside the runtime.
    #[serde(default)]
    pub(crate) recipe_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) definition_validation: Option<DefinitionValidation>,
    pub(crate) recipe_path: String,
    pub(crate) workflow_source_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) signer_key_id: Option<String>,
    pub(crate) trusted_execution: bool,
    pub(crate) installed_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WorkflowInstallIndexRecord {
    schema: String,
    #[serde(default)]
    entries: BTreeMap<String, WorkflowInstallIndexEntry>,
}

impl Default for WorkflowInstallIndexRecord {
    fn default() -> Self {
        Self {
            schema: WORKFLOW_INSTALL_INDEX_SCHEMA_V1.to_string(),
            entries: BTreeMap::new(),
        }
    }
}

fn index_key(workflow_id: &str, workflow_version: &str) -> String {
    format!("{}@{}", workflow_id, workflow_version)
}

fn install_index_path_in_home(home_dir: &Path) -> PathBuf {
    home_dir
        .join("registry")
        .join("workflow-install-index.json")
}

fn load_index(home_dir: &Path) -> Result<WorkflowInstallIndexRecord> {
    let path = install_index_path_in_home(home_dir);
    if !path.exists() {
        return Ok(WorkflowInstallIndexRecord::default());
    }
    let raw = transist_storage_local::read_regular_file_no_follow(&path)
        .with_context(|| format!("failed reading workflows install index: {}", path.display()))?;
    let record: WorkflowInstallIndexRecord = serde_json::from_slice(&raw)
        .with_context(|| format!("invalid workflows install index: {}", path.display()))?;
    if record.schema != WORKFLOW_INSTALL_INDEX_SCHEMA_V1 {
        bail!(
            "unsupported workflows install index schema '{}' at {}",
            record.schema,
            path.display()
        );
    }
    Ok(record)
}

pub(crate) fn contains_install_entry(home_dir: &Path, workflow_ref: &str) -> Result<bool> {
    Ok(load_index(home_dir)?.entries.contains_key(workflow_ref))
}

/// Reads one workflow's optional install metadata without modifying the home.
pub(crate) fn install_entry(
    home_dir: &Path,
    workflow_ref: &str,
) -> Result<Option<WorkflowInstallIndexEntry>> {
    Ok(load_index(home_dir)?.entries.remove(workflow_ref))
}

fn save_index(home_dir: &Path, record: &WorkflowInstallIndexRecord) -> Result<()> {
    let path = install_index_path_in_home(home_dir);
    transist_storage_local::write_json_atomic(&path, record)
        .with_context(|| format!("failed writing workflows install index: {}", path.display()))
}

fn update_index<T>(
    home_dir: &Path,
    update: impl FnOnce(&mut WorkflowInstallIndexRecord) -> Result<T>,
) -> Result<T> {
    let path = install_index_path_in_home(home_dir);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workflows install index path has no parent"))?;
    transist_storage_local::create_dir_all_durable(parent).with_context(|| {
        format!(
            "failed creating workflows install index dir: {}",
            parent.display()
        )
    })?;
    let _lock = transist_storage_local::lock_directory(parent)?;
    let mut index = load_index(home_dir)?;
    let result = update(&mut index)?;
    save_index(home_dir, &index)?;
    Ok(result)
}

pub(crate) fn record_install_entry(
    home_dir: &Path,
    entry: WorkflowInstallIndexEntry,
) -> Result<(WorkflowInstallIndexEntry, bool, bool)> {
    let index_created = !install_index_path_in_home(home_dir).exists();
    let (entry, entry_created) = update_index(home_dir, |index| {
        let key = index_key(&entry.workflow_id, &entry.workflow_version);
        if let Some(existing) = index.entries.get(&key).cloned() {
            if existing.workflow_definition_sha256 != entry.workflow_definition_sha256 {
                bail!(
                    "workflow '{}' version '{}' was already installed with different content; version strings are immutable and require a workflow version bump",
                    entry.workflow_id,
                    entry.workflow_version
                );
            }
            if existing.recipe_sha256.is_empty()
                || existing.definition_validation != entry.definition_validation
            {
                let mut upgraded = existing;
                upgraded.recipe_sha256 = entry.recipe_sha256;
                upgraded.definition_validation = entry.definition_validation;
                index.entries.insert(key, upgraded.clone());
                return Ok((upgraded, false));
            }
            return Ok((existing, false));
        }
        index.entries.insert(key, entry.clone());
        Ok((entry, true))
    })?;
    Ok((entry, entry_created, index_created))
}

pub(crate) fn remove_install_entry(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
) -> Result<()> {
    if !install_index_path_in_home(home_dir).exists() {
        return Ok(());
    }
    update_index(home_dir, |index| {
        index
            .entries
            .remove(&index_key(workflow_id, workflow_version));
        Ok(())
    })
}

/// Atomically restores a prior entry only when this promotion still owns the current entry.
pub(crate) fn restore_install_entry(
    home: &Path,
    previous: &WorkflowInstallIndexEntry,
    published: &WorkflowInstallIndexEntry,
) -> Result<()> {
    update_index(home, |index| {
        let key = index_key(&previous.workflow_id, &previous.workflow_version);
        if index.entries.get(&key) == Some(previous) {
            return Ok(());
        }
        if index.entries.get(&key) != Some(published) {
            bail!("refusing to restore an install receipt not owned by this promotion");
        }
        index.entries.insert(key, previous.clone());
        Ok(())
    })
}

pub(crate) fn rollback_install_entry(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
    expected_digest: &str,
    remove_empty_index: bool,
) -> Result<()> {
    let path = install_index_path_in_home(home_dir);
    if !path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workflows install index path has no parent"))?;
    let _lock = transist_storage_local::lock_directory(parent)?;
    let mut index = load_index(home_dir)?;
    let key = index_key(workflow_id, workflow_version);
    let removed = if let Some(entry) = index.entries.get(&key) {
        if entry.workflow_definition_sha256 != expected_digest {
            bail!(
                "refusing to roll back workflow install index entry '{}' because its digest is not owned by this promotion",
                key
            );
        }
        index.entries.remove(&key);
        true
    } else {
        false
    };
    if !removed {
        return Ok(());
    }
    if remove_empty_index && index.entries.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => transist_storage_local::sync_directory(parent),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed removing empty workflows install index: {}",
                    path.display()
                )
            }),
        }
    } else {
        save_index(home_dir, &index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;

    fn entry(index: usize) -> WorkflowInstallIndexEntry {
        WorkflowInstallIndexEntry {
            workflow_id: format!("workflow-{index}"),
            workflow_version: "v1".to_string(),
            workflow_definition_sha256: format!("digest-{index}"),
            recipe_sha256: format!("recipe-digest-{index}"),
            definition_validation: None,
            recipe_path: format!("recipe-{index}.yaml"),
            workflow_source_path: format!("workflow-{index}.yaml"),
            signer_key_id: None,
            trusted_execution: false,
            installed_at_ms: index as u64,
        }
    }

    #[test]
    fn concurrent_updates_preserve_every_entry() {
        const WRITERS: usize = 12;
        let home = tempdir().expect("tempdir");
        let barrier = Arc::new(Barrier::new(WRITERS));
        let threads = (0..WRITERS)
            .map(|index| {
                let home = home.path().to_path_buf();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    record_install_entry(&home, entry(index)).expect("record entry");
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("writer thread");
        }

        let index = load_index(home.path()).expect("load index");
        assert_eq!(index.entries.len(), WRITERS);
    }
}
