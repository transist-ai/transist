// SPDX-License-Identifier: MPL-2.0
//! Local effective-home settings implementation for the kernel port.

use anyhow::Result;
use transist_core::ports::HomeSettingsPort;
use transist_protocol::HomeSettings;

/// Loads runtime policy settings from one effective Transist home.
pub(crate) struct LocalHomeSettings {}

impl LocalHomeSettings {
    /// Creates a settings port rooted at the effective home directory.
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl HomeSettingsPort for LocalHomeSettings {
    fn load_settings(&self, home_dir: &std::path::Path) -> Result<HomeSettings> {
        transist_compiler::settings::load_home_settings(home_dir)
    }
}
