// SPDX-License-Identifier: MPL-2.0
//! Stable prerequisite schema shared by workflow and plugin manifests.

use serde::{Deserialize, Serialize};

/// One prerequisite collection attached to a workflow, plugin, or action.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrerequisiteSet {
    /// Required or optional host binaries.
    #[serde(default)]
    pub binaries: Vec<BinaryPrerequisite>,
    /// Required or optional host environment variables.
    #[serde(default)]
    pub env: Vec<EnvPrerequisite>,
    /// Required or optional host paths.
    #[serde(default)]
    pub paths: Vec<PathPrerequisite>,
    /// Required or optional host command probes.
    #[serde(default)]
    pub commands: Vec<CommandPrerequisite>,
}

impl PrerequisiteSet {
    /// Returns whether the set has no declared checks.
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
            && self.env.is_empty()
            && self.paths.is_empty()
            && self.commands.is_empty()
    }
}

/// One binary expected to exist on the host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BinaryPrerequisite {
    /// Binary name resolved through `PATH` when relative.
    pub name: String,
    /// Whether this prerequisite blocks execution.
    #[serde(default = "default_required")]
    pub required: bool,
    /// Legacy semver requirement string. It remains parseable for migration but
    /// fails with `legacy_executable_prerequisite_unsupported` during evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Legacy executable probe. It remains parseable for migration but fails
    /// with `legacy_executable_prerequisite_unsupported` during evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_check: Option<VersionCheck>,
}

/// One environment variable expected to be present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvPrerequisite {
    /// Environment variable name.
    pub name: String,
    /// Whether this prerequisite blocks execution.
    #[serde(default = "default_required")]
    pub required: bool,
}

/// Whether a required path may be any entry, a file, or a directory.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// Any filesystem entry is acceptable.
    #[default]
    Any,
    /// The path must resolve to a file.
    File,
    /// The path must resolve to a directory.
    Dir,
}

/// One host path that must exist.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathPrerequisite {
    /// Absolute or scope-relative filesystem path.
    pub path: String,
    /// Path kind that must exist at the target path.
    #[serde(default)]
    pub kind: PathKind,
    /// Whether this prerequisite blocks execution.
    #[serde(default = "default_required")]
    pub required: bool,
}

/// Optional custom probe used to extract a version string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VersionCheck {
    /// Legacy command argv. It remains parseable for migration but is never
    /// executed; evaluation returns `legacy_executable_prerequisite_unsupported`.
    pub argv: Vec<String>,
    /// Optional regex used to extract the version substring from stdout/stderr.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

/// One legacy custom command probe. It remains parseable but is never
/// executed; evaluation returns `legacy_executable_prerequisite_unsupported`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandPrerequisite {
    /// Command argv that would have probed the environment.
    pub argv: Vec<String>,
    /// Optional timeout for the probe command in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Whether this prerequisite blocks execution.
    #[serde(default = "default_required")]
    pub required: bool,
}

fn default_required() -> bool {
    true
}
