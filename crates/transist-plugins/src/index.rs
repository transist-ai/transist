// SPDX-License-Identifier: MPL-2.0
use crate::paths::install_index_path_in_home;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const INSTALL_INDEX_SCHEMA_V1: &str = "transist.plugin_install_index.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PluginInstallIndexRecord {
    schema: String,
    #[serde(default)]
    entries: BTreeMap<String, String>,
}

impl Default for PluginInstallIndexRecord {
    fn default() -> Self {
        Self {
            schema: INSTALL_INDEX_SCHEMA_V1.to_string(),
            entries: BTreeMap::new(),
        }
    }
}

fn index_key(plugin_id: &str, plugin_version: &str) -> String {
    format!("{plugin_id}@{plugin_version}")
}

fn load_index(home_dir: &Path) -> Result<PluginInstallIndexRecord> {
    let path = install_index_path_in_home(home_dir);
    if !path.exists() {
        return Ok(PluginInstallIndexRecord::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed reading plugins install index: {}", path.display()))?;
    let record: PluginInstallIndexRecord = serde_json::from_str(&raw)
        .with_context(|| format!("invalid plugins install index: {}", path.display()))?;
    if record.schema != INSTALL_INDEX_SCHEMA_V1 {
        bail!(
            "unsupported plugins install index schema '{}' at {}",
            record.schema,
            path.display()
        );
    }
    Ok(record)
}

fn save_index(home_dir: &Path, record: &PluginInstallIndexRecord) -> Result<()> {
    let path = install_index_path_in_home(home_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating plugin index dir: {}", parent.display()))?;
    }
    let bytes =
        serde_json::to_vec_pretty(record).context("failed serializing plugins install index")?;
    atomic_write_replace(&path, &bytes)
        .with_context(|| format!("failed writing plugins install index: {}", path.display()))
}

/// Writes `payload` to `path` atomically by writing a sibling temp file,
/// syncing it, and renaming it over the destination, then syncing the parent
/// directory. A crash at any point leaves either the previous or the new index
/// intact, never a truncated mixture, so the install index remains a durable
/// part of a recoverable publication.
fn atomic_write_replace(path: &Path, payload: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!(
            "index path '{}' is missing a parent directory",
            path.display()
        );
    };
    let Some(name) = path.file_name() else {
        bail!("index path '{}' is missing a file name", path.display());
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("failed creating index dir: {}", parent.display()))?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        fs::write(&temp, payload)
            .with_context(|| format!("failed writing index temp: {}", temp.display()))?;
        fsync_file(&temp)?;
        fs::rename(&temp, path)
            .with_context(|| format!("failed publishing index: {}", path.display()))?;
        fsync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Syncs the exact file so its bytes are durable before rename.
fn fsync_file(path: &Path) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = path;
        return Ok(());
    }
    #[cfg(unix)]
    {
        let file = fs::File::open(path)
            .with_context(|| format!("failed opening file for sync: {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed syncing file: {}", path.display()))
    }
}

/// Syncs `directory` so an entry change inside it is durable.
fn fsync_directory(directory: &Path) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = directory;
        return Ok(());
    }
    #[cfg(unix)]
    {
        let dir = fs::File::open(directory)
            .with_context(|| format!("failed opening directory: {}", directory.display()))?;
        dir.sync_all()
            .with_context(|| format!("failed syncing directory: {}", directory.display()))
    }
}

pub(crate) fn enforce_install_index_immutability(
    home_dir: &Path,
    plugin_id: &str,
    plugin_version: &str,
    plugin_sha256: &str,
) -> Result<()> {
    let index = load_index(home_dir)?;
    let key = index_key(plugin_id, plugin_version);
    if let Some(existing_sha) = index.entries.get(&key) {
        if existing_sha != plugin_sha256 {
            bail!(
                "plugin '{plugin_id}' version '{plugin_version}' was already installed with different content; version strings are immutable and require a plugin version bump"
            );
        }
    }
    Ok(())
}

/// Records the verified snapshot digest for one installed plugin version.
///
/// Returns `true` when the version already had an index entry (an idempotent
/// re-record of the same digest), and `false` when a new entry was added. The
/// caller uses this to decide whether a later publication failure should roll
/// back the just-added entry.
pub(crate) fn record_install_index_entry(
    home_dir: &Path,
    plugin_id: &str,
    plugin_version: &str,
    plugin_sha256: &str,
) -> Result<bool> {
    let mut index = load_index(home_dir)?;
    let key = index_key(plugin_id, plugin_version);
    let existed = index.entries.contains_key(&key);
    index.entries.insert(key, plugin_sha256.to_string());
    save_index(home_dir, &index)?;
    Ok(existed)
}

/// Removes one install-index entry for a plugin version.
pub(crate) fn remove_install_index_entry(
    home_dir: &Path,
    plugin_id: &str,
    plugin_version: &str,
) -> Result<()> {
    let mut index = load_index(home_dir)?;
    index.entries.remove(&index_key(plugin_id, plugin_version));
    save_index(home_dir, &index)
}
