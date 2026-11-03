// SPDX-License-Identifier: MPL-2.0
use crate::manifest::InstalledPlugin;
use crate::paths::plugins_root_in_home;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Lists installed plugins in an explicit Transist home.
pub fn list_plugins_in_home(home_dir: &Path) -> Result<Vec<InstalledPlugin>> {
    let root = plugins_root_in_home(home_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut plugins = Vec::new();
    for id_entry in fs::read_dir(&root)
        .with_context(|| format!("failed reading plugin root: {}", root.display()))?
    {
        let id_entry = id_entry?;
        if !id_entry.path().is_dir() {
            continue;
        }
        let id = id_entry.file_name().to_string_lossy().to_string();
        for version_entry in fs::read_dir(id_entry.path())
            .with_context(|| format!("failed reading plugin versions for '{id}'"))?
        {
            let version_entry = version_entry?;
            if !version_entry.path().is_dir() {
                continue;
            }
            plugins.push(InstalledPlugin {
                id: id.clone(),
                version: version_entry.file_name().to_string_lossy().to_string(),
                dir: version_entry.path(),
            });
        }
    }
    plugins.sort_by(|a, b| (&a.id, &a.version).cmp(&(&b.id, &b.version)));
    Ok(plugins)
}
