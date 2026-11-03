// SPDX-License-Identifier: MPL-2.0
use std::path::{Path, PathBuf};

/// Returns the plugin installation root for an explicit Transist home.
pub(crate) fn plugins_root_in_home(home_dir: &Path) -> PathBuf {
    home_dir.join("plugins")
}

/// Returns the immutable plugins install index path for an explicit home.
pub(crate) fn install_index_path_in_home(home_dir: &Path) -> PathBuf {
    plugins_root_in_home(home_dir).join("install-index.json")
}

/// Returns the expected manifest path inside a plugin directory.
pub(crate) fn manifest_path(plugin_dir: &Path) -> PathBuf {
    plugin_dir.join("plugin.yaml")
}
