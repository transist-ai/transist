// SPDX-License-Identifier: MPL-2.0
use std::fmt;

/// Structured uninstall failure raised when installed plugins are still referenced by protected runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginUninstallError {
    /// The plugin is still referenced by one or more protected runs.
    InUse {
        /// Plugin id that cannot be removed yet.
        plugin_id: String,
        /// Optional installed version for a version-scoped removal.
        plugin_version: Option<String>,
        /// Protected runs that still reference the plugin.
        run_ids: Vec<String>,
    },
}

impl fmt::Display for PluginUninstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InUse {
                plugin_id,
                plugin_version,
                run_ids,
            } => match plugin_version {
                Some(version) => write!(
                    f,
                    "cannot uninstall plugin '{}' version '{}' while protected runs still reference it: {}",
                    plugin_id,
                    version,
                    run_ids.join(", ")
                ),
                None => write!(
                    f,
                    "cannot uninstall plugin '{}' while protected runs still reference it: {}",
                    plugin_id,
                    run_ids.join(", ")
                ),
            },
        }
    }
}

impl std::error::Error for PluginUninstallError {}
