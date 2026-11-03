// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::path::{Path, PathBuf};
use transist_plugins::{InstalledPlugin, PluginInstallReport, PluginManifest};

/// Plugin install, list, inspect, and remove operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginService {
    pub(crate) home_dir: PathBuf,
}

impl PluginService {
    pub(crate) fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }

    /// Installs a plugin from its manifest file into this context's home directory.
    ///
    /// Prefer this over [`Self::install_plugin_dir`] for new callers; pass the
    /// plugin's `plugin.yaml` path so install semantics match
    /// `workflows install --path --approve`.
    pub fn install_plugin_file(&self, manifest_file: &Path) -> Result<PluginInstallReport> {
        super::install_plugin_file_in_home(&self.home_dir, manifest_file)
    }

    /// Installs a plugin manifest file with optional trusted-execution verification.
    pub fn install_plugin_file_with_trust(
        &self,
        manifest_file: &Path,
        trusted_execution: bool,
    ) -> Result<PluginInstallReport> {
        super::install_plugin_file_in_home_with_trust(
            &self.home_dir,
            manifest_file,
            trusted_execution,
        )
    }

    /// Installs a plugin manifest with optional trusted verification using a
    /// caller-selected detached signature source.
    pub fn install_plugin_file_with_trust_and_signature(
        &self,
        manifest_file: &Path,
        trusted_execution: bool,
        signature_path: Option<&Path>,
    ) -> Result<PluginInstallReport> {
        super::install_plugin_file_in_home_with_trust_and_signature(
            &self.home_dir,
            manifest_file,
            trusted_execution,
            signature_path,
        )
    }

    /// Installs a plugin directory into this context's home directory.
    ///
    /// Directory-based compatibility wrapper around [`Self::install_plugin_file`].
    pub fn install_plugin_dir(&self, plugin_dir: &Path) -> Result<PluginInstallReport> {
        super::install_plugin_dir_in_home(&self.home_dir, plugin_dir)
    }

    /// Installs a plugin directory with optional trusted-execution verification.
    pub fn install_plugin_dir_with_trust(
        &self,
        plugin_dir: &Path,
        trusted_execution: bool,
    ) -> Result<PluginInstallReport> {
        super::install_plugin_dir_in_home_with_trust(&self.home_dir, plugin_dir, trusted_execution)
    }

    /// Installs a plugin directory with optional trusted verification using a
    /// caller-selected detached signature source.
    pub fn install_plugin_dir_with_trust_and_signature(
        &self,
        plugin_dir: &Path,
        trusted_execution: bool,
        signature_path: Option<&Path>,
    ) -> Result<PluginInstallReport> {
        super::install_plugin_dir_in_home_with_trust_and_signature(
            &self.home_dir,
            plugin_dir,
            trusted_execution,
            signature_path,
        )
    }

    /// Lists installed plugins from this context's home directory.
    pub fn list_plugins(&self) -> Result<Vec<InstalledPlugin>> {
        super::list_plugins_in_home(&self.home_dir)
    }

    /// Loads manifests for all installed versions of a plugin id.
    pub fn inspect_plugin(&self, plugin_id: &str) -> Result<Vec<PluginManifest>> {
        super::inspect_plugin_in_home(&self.home_dir, plugin_id)
    }

    /// Removes one installed plugin version from this context's home directory.
    pub fn remove_plugin(&self, plugin_id: &str, plugin_version: &str) -> Result<()> {
        super::remove_plugin_in_home(&self.home_dir, plugin_id, plugin_version)
    }

    /// Removes all installed versions of a plugin from this context's home directory.
    ///
    /// Returns the list of version strings that were removed.
    pub fn uninstall_plugin(&self, plugin_id: &str) -> Result<Vec<String>> {
        super::uninstall_plugin_in_home(&self.home_dir, plugin_id)
    }
}
