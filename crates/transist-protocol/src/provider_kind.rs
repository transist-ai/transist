// SPDX-License-Identifier: MPL-2.0
//! Declared provider locality for a plugin action.
//!
//! Locality is **declared** by the plugin manifest (`provider_kind: local |
//! external`), not inferred from adapter kind. A `process` adapter that calls
//! an external LLM API is still `external`; a `process` adapter that only
//! reads local files is `local`. This lets the runtime prove whether data left
//! the machine without inspecting adapter internals.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Whether an action's provider is local or external.
///
/// Stored on the plugin manifest at the action level with a manifest-level
/// default. When both are absent the effective value is [`ProviderKind::External`]
/// (fail-safe): an undeclared action is treated as external in local-only mode.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// The action runs entirely on the local machine and does not send data
    /// to any external service.
    Local,
    /// The action may send data to an external service (network API, remote
    /// model provider, cloud endpoint).
    #[default]
    External,
}

impl ProviderKind {
    /// Returns the stable label used in manifests and runtime records.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::External => "external",
        }
    }

    /// Returns `true` when the provider is local.
    pub fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }

    /// Returns `true` when the provider is external.
    pub fn is_external(self) -> bool {
        matches!(self, Self::External)
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string cannot be parsed into a [`ProviderKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownProviderKind(pub String);

impl fmt::Display for UnknownProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown provider_kind '{}'", self.0)
    }
}

impl std::error::Error for UnknownProviderKind {}

impl FromStr for ProviderKind {
    type Err = UnknownProviderKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "external" => Ok(Self::External),
            other => Err(UnknownProviderKind(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trips() {
        for kind in [ProviderKind::Local, ProviderKind::External] {
            let json = serde_json::to_string(&kind).expect("serialize");
            let parsed: ProviderKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(kind, parsed);
        }
    }

    #[test]
    fn wire_labels_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&ProviderKind::Local).unwrap(),
            "\"local\""
        );
        assert_eq!(
            serde_json::to_string(&ProviderKind::External).unwrap(),
            "\"external\""
        );
    }

    #[test]
    fn default_is_external_fail_safe() {
        assert_eq!(ProviderKind::default(), ProviderKind::External);
    }

    #[test]
    fn from_str_round_trips() {
        assert_eq!(
            "local".parse::<ProviderKind>().unwrap(),
            ProviderKind::Local
        );
        assert_eq!(
            "external".parse::<ProviderKind>().unwrap(),
            ProviderKind::External
        );
        assert!("unknown".parse::<ProviderKind>().is_err());
    }
}
