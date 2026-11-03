// SPDX-License-Identifier: MPL-2.0
//! Local plugin-byte verification for the kernel asset-lock port.

use anyhow::{Context, Result};
use transist_core::AssetLock;
use transist_core::ports::PluginVerificationPort;

/// Verifies plugin asset locks against the installed plugin tree in one home directory.
pub(crate) struct LocalPluginVerifier {}

impl LocalPluginVerifier {
    /// Creates a verifier rooted at the effective Transist home directory.
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl PluginVerificationPort for LocalPluginVerifier {
    fn verify_plugin_digest(&self, home_dir: &std::path::Path, lock: &AssetLock) -> Result<()> {
        let version = lock.version.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "plugin lock for step '{}' ('{}') has no version",
                lock.step_id,
                lock.ref_id
            )
        })?;
        let installed_dir = home_dir.join("plugins").join(&lock.ref_id).join(version);
        if !installed_dir.is_dir() {
            anyhow::bail!(
                "plugin '{}' version '{}' pinned by step '{}' is not installed",
                lock.ref_id,
                version,
                lock.step_id
            );
        }
        let current_sha =
            transist_plugins::plugin_dir_sha256(&installed_dir).with_context(|| {
                format!(
                    "failed computing digest for plugin '{}' version '{}'",
                    lock.ref_id, version
                )
            })?;
        if current_sha != lock.sha256 {
            anyhow::bail!(
                "plugin '{}' version '{}' pinned by step '{}' has changed; \
                 expected digest '{}' but found '{}'",
                lock.ref_id,
                version,
                lock.step_id,
                lock.sha256,
                current_sha
            );
        }
        Ok(())
    }
}
