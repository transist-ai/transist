// SPDX-License-Identifier: MPL-2.0
//! Shared DTOs for effective-home runtime settings.

use crate::{FilesystemPolicy, NetworkPolicy};
use serde::{Deserialize, Serialize};

/// Runtime policy settings loaded from `<home>/settings.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HomeSettings {
    /// Optional global filesystem policy.
    #[serde(default)]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Optional global network policy.
    #[serde(default)]
    pub network_policy: Option<NetworkPolicy>,
}
