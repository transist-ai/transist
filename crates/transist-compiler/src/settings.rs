// SPDX-License-Identifier: MPL-2.0
//! Effective-home runtime settings loading.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use transist_protocol::{FilesystemPolicy, HomeSettings, NetworkPolicy};

/// Loads both runtime policies from one effective-home settings file.
///
/// A missing file is equivalent to default settings. Unknown fields and invalid
/// YAML remain errors so callers cannot silently ignore a malformed policy.
pub fn load_home_settings(home_dir: &Path) -> Result<HomeSettings> {
    let path = home_dir.join("settings.yaml");
    if !path.exists() {
        return Ok(HomeSettings::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed reading Transist home settings: {}", path.display()))?;
    yaml_serde::from_str(&raw)
        .with_context(|| format!("invalid Transist home settings yaml: {}", path.display()))
}

/// Loads the global filesystem policy from effective-home settings.
pub fn load_home_filesystem_policy(home_dir: &Path) -> Result<Option<FilesystemPolicy>> {
    Ok(load_home_settings(home_dir)?.filesystem_policy)
}

/// Loads the global network policy from effective-home settings.
pub fn load_home_network_policy(home_dir: &Path) -> Result<Option<NetworkPolicy>> {
    Ok(load_home_settings(home_dir)?.network_policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{NetworkAccessMode, WorkspaceAccessMode};

    #[test]
    fn missing_settings_are_defaulted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            load_home_settings(tmp.path()).expect("settings"),
            HomeSettings::default()
        );
    }

    #[test]
    fn both_policies_are_loaded_from_one_document() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("settings.yaml"),
            "filesystem_policy:\n  workspace_access:\n    mode: read_only\nnetwork_policy:\n  mode: allowlist\n  allow_hosts: [api.example.com]\n",
        )
        .expect("settings");
        let settings = load_home_settings(tmp.path()).expect("settings");
        assert_eq!(
            settings
                .filesystem_policy
                .expect("filesystem")
                .workspace_access
                .expect("workspace")
                .mode,
            WorkspaceAccessMode::ReadOnly
        );
        assert_eq!(
            settings.network_policy.expect("network").mode,
            NetworkAccessMode::Allowlist
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("settings.yaml"), "unknown: true\n").expect("settings");
        assert!(load_home_settings(tmp.path()).is_err());
    }
}
