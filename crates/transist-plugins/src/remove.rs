// SPDX-License-Identifier: MPL-2.0
use crate::paths::plugins_root_in_home;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

/// Removes one installed plugin version from an explicit Transist home.
pub fn remove_plugin_in_home(home_dir: &Path, plugin_id: &str, plugin_version: &str) -> Result<()> {
    let dir = plugins_root_in_home(home_dir)
        .join(plugin_id)
        .join(plugin_version);
    if !dir.exists() {
        bail!("plugin '{plugin_id}' version '{plugin_version}' is not installed");
    }
    fs::remove_dir_all(&dir)
        .with_context(|| format!("failed removing plugin dir: {}", dir.display()))?;
    let id_dir = plugins_root_in_home(home_dir).join(plugin_id);
    if id_dir.exists() && fs::read_dir(&id_dir)?.next().is_none() {
        fs::remove_dir(&id_dir).with_context(|| {
            format!("failed removing empty plugin id dir: {}", id_dir.display())
        })?;
    }
    Ok(())
}

/// Removes all installed versions of a plugin from an explicit Transist home.
///
/// Returns the list of version strings that were removed. Errors if the plugin
/// is not installed at all.
pub fn uninstall_plugin_in_home(home_dir: &Path, plugin_id: &str) -> Result<Vec<String>> {
    let id_dir = plugins_root_in_home(home_dir).join(plugin_id);
    if !id_dir.exists() {
        bail!("plugin '{plugin_id}' is not installed");
    }
    let mut versions: Vec<String> = fs::read_dir(&id_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    versions.sort();
    fs::remove_dir_all(&id_dir)
        .with_context(|| format!("failed removing plugin dir: {}", id_dir.display()))?;
    Ok(versions)
}
