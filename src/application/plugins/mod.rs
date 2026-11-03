// SPDX-License-Identifier: MPL-2.0
//! Plugin-oriented application operations.
//!
//! This module keeps installed-plugin lifecycle commands together so callers do not
//! need to know the on-disk layout used by the local Transist home.

/// Plugin lifecycle service facade for install, list, inspect, and remove operations.
pub mod service;
pub use errors::PluginUninstallError;
pub use service::PluginService;

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use transist_core::ports::LeasePort;
use transist_plugins::{InstalledPlugin, PluginInstallReport, PluginManifest};

mod errors;
mod in_use;

const PLUGIN_LIFECYCLE_LEASE_ID: &str = "plugin-lifecycle-reservation";

/// Installs a plugin from its manifest file into an explicit home directory.
pub fn install_plugin_file_in_home(
    home_dir: &Path,
    manifest_file: &Path,
) -> Result<PluginInstallReport> {
    install_plugin_file_in_home_with_trust(home_dir, manifest_file, false)
}

/// Installs a plugin from its manifest file into an explicit home directory.
pub fn install_plugin_file_in_home_with_trust(
    home_dir: &Path,
    manifest_file: &Path,
    trusted_execution: bool,
) -> Result<PluginInstallReport> {
    install_plugin_file_in_home_with_trust_and_signature(
        home_dir,
        manifest_file,
        trusted_execution,
        None,
    )
}

/// Installs a plugin manifest file with optional trusted verification and a
/// caller-selected detached signature source.
pub fn install_plugin_file_in_home_with_trust_and_signature(
    home_dir: &Path,
    manifest_file: &Path,
    trusted_execution: bool,
    signature_path: Option<&Path>,
) -> Result<PluginInstallReport> {
    with_plugin_lifecycle_reservation(home_dir, || {
        let trusted_execution =
            crate::runtime_policy::trusted_execution_enabled(home_dir, trusted_execution)?;
        let manifest = transist_plugins::load_manifest_file(manifest_file)?;
        crate::trust::ensure_plugin_version_allowed(&manifest.id, &manifest.version)?;
        let plugin_dir = manifest_file.parent().ok_or_else(|| {
            anyhow::anyhow!("manifest path has no parent: {}", manifest_file.display())
        })?;
        // Stage the package into a private immutable snapshot first. The digest and
        // signature are verified against the snapshot, and only the verified
        // snapshot is atomically published. The source tree is never treated as the
        // artifact that gets installed, so a verify-then-copy race cannot publish
        // unverified bytes.
        let staged = transist_plugins::stage_plugin_package(home_dir, manifest_file)?;
        // In trusted mode the detached sidecar is read exactly once, the snapshot
        // signature is verified against those in-memory bytes, and the same bytes
        // are published so package + sidecar + install-index entry commit as one
        // recoverable unit. A missing sidecar fails closed (unless a development
        // test-version bypass applies).
        let sidecar = if trusted_execution {
            let sidecar =
                read_trusted_plugin_sidecar(plugin_dir, &manifest.version, signature_path)?;
            crate::trusted_execution::verify_plugin_snapshot_signature(
                home_dir,
                sidecar.as_deref().unwrap_or(&[]),
                staged.snapshot_sha256(),
                &manifest.id,
                &manifest.version,
            )?;
            sidecar
        } else {
            None
        };
        transist_plugins::publish_staged_plugin_with_sidecar(staged, sidecar)
    })
}

/// Reads the detached signature sidecar for a trusted plugins install.
///
/// In trusted mode every real plugin must supply a `transist.signature.v1`
/// sidecar. The conventional sibling path is used when the caller does not
/// select an explicit source; a missing selected sidecar fails closed. The only
/// exception is a development test-version bypass install, which explicitly
/// installs an unsigned package.
fn read_trusted_plugin_sidecar(
    source_plugin_dir: &Path,
    _plugin_version: &str,
    signature_path: Option<&Path>,
) -> Result<Option<Vec<u8>>> {
    let sig_path = signature_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate::signature_sidecar_path(source_plugin_dir));
    match fs::read(&sig_path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(feature = "dev-version-bypass")]
            if crate::is_test_plugin(_plugin_version) {
                return Ok(None);
            }
            bail!(
                "trusted plugins install refused: signature sidecar '{}' is missing (code=missing_plugin_signature)",
                sig_path.display()
            );
        }
        Err(error) => Err(error)
            .with_context(|| format!("failed reading plugin signature '{}'", sig_path.display())),
    }
}

/// Installs a plugin directory into an explicit home directory.
///
/// Directory-based compatibility wrapper around [`install_plugin_file_in_home`].
pub fn install_plugin_dir_in_home(
    home_dir: &Path,
    plugin_dir: &Path,
) -> Result<PluginInstallReport> {
    install_plugin_dir_in_home_with_trust(home_dir, plugin_dir, false)
}

/// Installs a plugin directory into an explicit home directory with optional trusted verification.
pub fn install_plugin_dir_in_home_with_trust(
    home_dir: &Path,
    plugin_dir: &Path,
    trusted_execution: bool,
) -> Result<PluginInstallReport> {
    install_plugin_dir_in_home_with_trust_and_signature(
        home_dir,
        plugin_dir,
        trusted_execution,
        None,
    )
}

/// Installs a plugin directory with optional trusted verification and a
/// caller-selected detached signature source.
pub fn install_plugin_dir_in_home_with_trust_and_signature(
    home_dir: &Path,
    plugin_dir: &Path,
    trusted_execution: bool,
    signature_path: Option<&Path>,
) -> Result<PluginInstallReport> {
    install_plugin_file_in_home_with_trust_and_signature(
        home_dir,
        &plugin_dir.join("plugin.yaml"),
        trusted_execution,
        signature_path,
    )
}

/// Lists installed plugins from an explicit home directory.
pub fn list_plugins_in_home(home_dir: &Path) -> Result<Vec<InstalledPlugin>> {
    transist_plugins::list_plugins_in_home(home_dir)
}

/// Loads manifests for all installed versions of a plugin id from an explicit home directory.
pub fn inspect_plugin_in_home(home_dir: &Path, plugin_id: &str) -> Result<Vec<PluginManifest>> {
    transist_plugins::inspect_plugin_in_home(home_dir, plugin_id)
}

/// Removes one installed plugin version from an explicit home directory.
pub fn remove_plugin_in_home(home_dir: &Path, plugin_id: &str, plugin_version: &str) -> Result<()> {
    with_plugin_lifecycle_reservation(home_dir, || {
        let run_ids =
            in_use::find_active_runs_for_plugin_in_home(home_dir, plugin_id, Some(plugin_version))?;
        if !run_ids.is_empty() {
            return Err(PluginUninstallError::InUse {
                plugin_id: plugin_id.to_string(),
                plugin_version: Some(plugin_version.to_string()),
                run_ids,
            }
            .into());
        }
        transist_plugins::remove_plugin_in_home(home_dir, plugin_id, plugin_version)
    })
}

/// Removes all installed versions of a plugin from an explicit home directory.
///
/// Returns the list of version strings that were removed.
pub fn uninstall_plugin_in_home(home_dir: &Path, plugin_id: &str) -> Result<Vec<String>> {
    with_plugin_lifecycle_reservation(home_dir, || {
        let run_ids = in_use::find_active_runs_for_plugin_in_home(home_dir, plugin_id, None)?;
        if !run_ids.is_empty() {
            return Err(PluginUninstallError::InUse {
                plugin_id: plugin_id.to_string(),
                plugin_version: None,
                run_ids,
            }
            .into());
        }
        transist_plugins::uninstall_plugin_in_home(home_dir, plugin_id)
    })
}

fn with_plugin_lifecycle_reservation<T>(
    home_dir: &Path,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let store =
        transist_storage_local::LocalLeaseStore::new(home_dir, crate::runtime_owner_identity()?);
    let lease = store.acquire_root_run_tree_lease(PLUGIN_LIFECYCLE_LEASE_ID)?;
    let result = operation();
    match (result, store.release_root_run_tree_lease(&lease)) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error).context("plugin lifecycle reservation release failed"),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(release_error)) => Err(error.context(format!(
            "plugin lifecycle reservation release also failed: {release_error:#}"
        ))),
    }
}
