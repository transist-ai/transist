// SPDX-License-Identifier: MPL-2.0
//! Typed adapter kind shared across the protocol and adapter routing.
//!
//! The wire format is a stable lowercase string (`process`, `http`, `mcp`).
//! Centralizing the enum here keeps compiler validation, TEP request routing,
//! compiled recipe invoke descriptors, and adapter dispatch on a single
//! exhaustively-matched type instead of stringly-typed comparisons.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Adapter family that hosts a plugin action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    /// Adapter implemented as a local process.
    Process,
    /// Adapter implemented over HTTP.
    Http,
    /// Adapter implemented through the Model Context Protocol.
    Mcp,
}

impl AdapterKind {
    /// Returns the stable adapter label used in manifests and runtime records.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Http => "http",
            Self::Mcp => "mcp",
        }
    }

    /// All adapter kinds in stable order, useful for diagnostics.
    pub fn all() -> &'static [AdapterKind] {
        &[Self::Process, Self::Http, Self::Mcp]
    }
}

impl fmt::Display for AdapterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string cannot be parsed into an [`AdapterKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAdapterKind(pub String);

impl fmt::Display for UnknownAdapterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown adapter kind '{}'", self.0)
    }
}

impl std::error::Error for UnknownAdapterKind {}

impl FromStr for AdapterKind {
    type Err = UnknownAdapterKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "process" => Ok(Self::Process),
            "http" => Ok(Self::Http),
            "mcp" => Ok(Self::Mcp),
            other => Err(UnknownAdapterKind(other.to_string())),
        }
    }
}
