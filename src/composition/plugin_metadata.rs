// SPDX-License-Identifier: MPL-2.0
//! Local plugin metadata implementation for kernel ports.

use anyhow::Result;
use std::path::PathBuf;
use transist_core::ports::{PluginActionMetadata, PluginMetadataPort, PluginSecretInput};
use transist_plugins::{
    PluginEntrypoint, PluginVersionSelector, resolve_plugin_with_selector_in_home,
};

/// Resolves installed plugin metadata from one effective Transist home.
pub(crate) struct LocalPluginMetadata {
    home_dir: PathBuf,
    catalog: Option<std::sync::Arc<transist_plugins::ValidatedPluginCatalog>>,
}

impl LocalPluginMetadata {
    /// Borrows the operation-scoped catalog for partial schema validation.
    pub(crate) fn catalog(&self) -> Option<&transist_plugins::ValidatedPluginCatalog> {
        self.catalog.as_deref()
    }
    /// Creates a metadata resolver rooted at the effective home directory.
    pub(crate) fn new(home_dir: PathBuf) -> Self {
        Self {
            home_dir,
            catalog: None,
        }
    }

    /// Shares one read-only evaluation catalog across all metadata lookups.
    pub(crate) fn with_catalog(
        home_dir: PathBuf,
        catalog: std::sync::Arc<transist_plugins::ValidatedPluginCatalog>,
    ) -> Self {
        Self {
            home_dir,
            catalog: Some(catalog),
        }
    }

    fn load(
        &self,
        plugin_id: &str,
        plugin_version: &str,
    ) -> Result<transist_plugins::PluginManifest> {
        if let Some(catalog) = &self.catalog {
            return Ok(catalog.load(plugin_id, plugin_version)?.manifest.clone());
        }
        let (manifest, _) = resolve_plugin_with_selector_in_home(
            &self.home_dir,
            plugin_id,
            &PluginVersionSelector::Exact(plugin_version.to_string()),
        )?;
        Ok(manifest)
    }
}

impl PluginMetadataPort for LocalPluginMetadata {
    fn resolve_action(
        &self,
        plugin_id: &str,
        plugin_version: &str,
        action: &str,
    ) -> Result<Option<PluginActionMetadata>> {
        let manifest = self.load(plugin_id, plugin_version)?;
        let Some(action) = manifest
            .actions
            .iter()
            .find(|candidate| candidate.name == action)
        else {
            return Ok(None);
        };
        let provider_kind = action.effective_provider_kind(manifest.default_provider_kind);
        Ok(Some(PluginActionMetadata {
            provider_kind,
            secret_inputs: action
                .secret_inputs
                .iter()
                .map(|secret| PluginSecretInput {
                    name: secret.name.clone(),
                    required: secret.required,
                })
                .collect(),
        }))
    }

    fn resolve_network_origin(
        &self,
        plugin_id: &str,
        plugin_version: &str,
    ) -> Result<Option<String>> {
        let manifest = self.load(plugin_id, plugin_version)?;
        Ok(match manifest.entrypoint {
            PluginEntrypoint::Http { base_url, .. } => Some(base_url),
            PluginEntrypoint::Process { .. } | PluginEntrypoint::Mcp { .. } => None,
        })
    }
}
