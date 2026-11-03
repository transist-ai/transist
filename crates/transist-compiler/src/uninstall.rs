// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

fn recipe_path_in_home(home_dir: &Path, workflow_id: &str, workflow_version: &str) -> PathBuf {
    home_dir
        .join("registry")
        .join("recipes")
        .join(format!("{workflow_id}-{workflow_version}.yaml"))
}

/// Removes one compiled workflow recipe from an explicit Transist home.
pub fn remove_recipe_in_home(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
) -> Result<()> {
    let recipe_path = recipe_path_in_home(home_dir, workflow_id, workflow_version);
    if !recipe_path.exists() {
        return Err(anyhow!(
            "compiled workflow recipe does not exist: {}",
            recipe_path.display()
        ));
    }
    std::fs::remove_file(&recipe_path)
        .with_context(|| format!("failed removing compiled recipe: {}", recipe_path.display()))
}
