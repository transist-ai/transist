// SPDX-License-Identifier: MPL-2.0
use crate::manifest::{PluginInstallReport, load_manifest_file};
use crate::paths::{manifest_path, plugins_root_in_home};
use crate::staged::{StagedPluginPackage, publish_staged_plugin, stage_plugin_package_in_home};
use anyhow::Result;
use std::path::Path;

/// Installs a plugin from its manifest file into the repository-local Transist home.
///
/// `manifest_file` must point at a `plugin.yaml` document. The plugin's source
/// directory is derived from the manifest's parent and only its declared
/// payload is copied, mirroring `workflows install --path workflow.yaml --approve`
/// semantics. Prefer this entrypoint over [`install_plugin`] for new callers.
pub fn install_plugin_file(repo_root: &Path, manifest_file: &Path) -> Result<PluginInstallReport> {
    install_plugin_file_in_home(&repo_root.join(".transist"), manifest_file)
}

/// Installs a plugin from its manifest file into an explicit Transist home.
///
/// See [`install_plugin_file`] for path semantics.
pub fn install_plugin_file_in_home(
    home_dir: &Path,
    manifest_file: &Path,
) -> Result<PluginInstallReport> {
    install_plugin_file_in_home_with_target(home_dir, manifest_file).map(|(report, _)| report)
}

/// Installs a plugin from its manifest file and returns the concrete install directory.
pub fn install_plugin_file_in_home_with_target(
    home_dir: &Path,
    manifest_file: &Path,
) -> Result<(PluginInstallReport, std::path::PathBuf)> {
    let target_dir = {
        let manifest = load_manifest_file(manifest_file)?;
        plugins_root_in_home(home_dir)
            .join(&manifest.id)
            .join(&manifest.version)
    };
    let staged = stage_plugin_package_in_home(home_dir, manifest_file)?;
    let report = publish_staged_plugin(staged)?;
    Ok((report, target_dir))
}

/// Stages a plugin package so a caller can verify it before it is published.
///
/// The trusted-install flow uses this to verify the snapshot's digest and
/// signature before [`publish_staged_plugin`].
pub fn stage_plugin_package(home_dir: &Path, manifest_file: &Path) -> Result<StagedPluginPackage> {
    stage_plugin_package_in_home(home_dir, manifest_file)
}

/// Installs a plugin directory into the repository-local Transist home.
///
/// Directory-based compatibility wrapper around [`install_plugin_file`]. New
/// callers should prefer passing the plugin's `plugin.yaml` path with
/// [`install_plugin_file`].
pub fn install_plugin(repo_root: &Path, plugin_dir: &Path) -> Result<PluginInstallReport> {
    install_plugin_in_home(&repo_root.join(".transist"), plugin_dir)
}

/// Installs a plugin directory into an explicit Transist home.
///
/// Directory-based compatibility wrapper around
/// [`install_plugin_file_in_home`]. New callers should prefer passing the
/// plugin's `plugin.yaml` path with [`install_plugin_file_in_home`].
pub fn install_plugin_in_home(home_dir: &Path, plugin_dir: &Path) -> Result<PluginInstallReport> {
    install_plugin_file_in_home(home_dir, &manifest_path(plugin_dir))
}
