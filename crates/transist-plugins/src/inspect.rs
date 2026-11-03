// SPDX-License-Identifier: MPL-2.0
use crate::list::list_plugins_in_home;
use crate::manifest::{PluginManifest, load_manifest_file};
use crate::paths::manifest_path;
use anyhow::{Result, bail};
use std::path::Path;

/// Loads manifests for all installed versions of `plugin_id` in an explicit home.
pub fn inspect_plugin_in_home(home_dir: &Path, plugin_id: &str) -> Result<Vec<PluginManifest>> {
    let mut manifests = Vec::new();
    for installed in list_plugins_in_home(home_dir)? {
        if installed.id == plugin_id {
            manifests.push(load_manifest_file(&manifest_path(&installed.dir))?);
        }
    }
    if manifests.is_empty() {
        bail!("plugin '{plugin_id}' is not installed");
    }
    manifests.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(manifests)
}
