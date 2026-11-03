// SPDX-License-Identifier: MPL-2.0
//! Identifier containment enforcement shared across compiler, plugin, and
//! kernel run-birth surfaces.
//!
//! Transist derives filesystem paths (plugins install directories, run
//! directories, workflow/step identifiers surfaced in storage keys) directly
//! from user- and workflow-authored identifiers. Any identifier that is not
//! constrained to a small, predictable alphabet can be used to escape the
//! intended directory (path traversal via `..`, absolute-path injection,
//! separator injection, or non-ASCII homoglyph confusion). This module
//! defines the single slug rule enforced at compile time, install time, and
//! run-birth time, so that no path is ever built from an unvalidated
//! identifier.

use crate::error::{Error, Result};

/// The identifier classes this module validates. Used only to produce a
/// precise, actionable error message; the validation rule itself is
/// identical across all kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierKind {
    /// A workflow id (`transist.flow.v1` `id` field).
    WorkflowId,
    /// A workflow version string.
    WorkflowVersion,
    /// A step id within a workflow or subflow.
    StepId,
    /// A cleanup step id.
    CleanupId,
    /// A gate id (manual approval checkpoint).
    GateId,
    /// An embedded-subflow child workflow id.
    SubflowChildId,
    /// A plugin id (`transist.plugin.v1` manifest `id` field).
    PluginId,
    /// A plugin version string.
    PluginVersion,
    /// A run identifier (`run_ref` / generated run id).
    RunRef,
    /// A trust-root key id (`transist.trust_root.v1` record key).
    TrustKeyId,
}

impl IdentifierKind {
    fn label(self) -> &'static str {
        match self {
            IdentifierKind::WorkflowId => "workflow id",
            IdentifierKind::WorkflowVersion => "workflow version",
            IdentifierKind::StepId => "step id",
            IdentifierKind::CleanupId => "cleanup id",
            IdentifierKind::GateId => "gate id",
            IdentifierKind::SubflowChildId => "subflow child id",
            IdentifierKind::PluginId => "plugin id",
            IdentifierKind::PluginVersion => "plugin version",
            IdentifierKind::RunRef => "run reference",
            IdentifierKind::TrustKeyId => "trust key id",
        }
    }
}

/// Validates `value` against the single identifier containment rule:
/// `^[A-Za-z0-9][A-Za-z0-9._-]*$`.
///
/// This rejects an empty string, a value starting with `.` or `-`
/// (including `.` and `..` themselves), any path separator (`/` or `\`),
/// absolute paths, embedded whitespace, and non-ASCII characters. The rule
/// is deliberately conservative: it allows only ASCII alphanumerics plus
/// `.`, `_`, and `-` after a mandatory alphanumeric first character.
///
/// # Errors
///
/// Returns [`Error`] describing the specific identifier class and the
/// rejected value when `value` does not satisfy the rule.
pub fn validate_identifier(kind: IdentifierKind, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Protocol(format!(
            "{} must not be empty",
            kind.label()
        )));
    }

    let mut chars = value.chars();
    let first = chars.next().expect("checked non-empty above");
    if !first.is_ascii_alphanumeric() {
        return Err(Error::Protocol(format!(
            "{} '{}' must start with an ASCII alphanumeric character",
            kind.label(),
            value
        )));
    }

    for c in chars {
        let allowed = c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
        if !allowed {
            return Err(Error::Protocol(format!(
                "{} '{}' contains an invalid character '{}'; only ASCII letters, digits, '.', '_', and '-' are allowed",
                kind.label(),
                value,
                c
            )));
        }
    }

    if value == "." || value == ".." {
        return Err(Error::Protocol(format!(
            "{} '{}' must not be '.' or '..'",
            kind.label(),
            value
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_slugs() {
        for value in ["a", "workflow-1", "step_id.v2", "Plugin123"] {
            validate_identifier(IdentifierKind::WorkflowId, value)
                .unwrap_or_else(|e| panic!("expected '{value}' to be accepted: {e}"));
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_identifier(IdentifierKind::WorkflowId, "").is_err());
    }

    #[test]
    fn rejects_dot_and_dotdot() {
        assert!(validate_identifier(IdentifierKind::StepId, ".").is_err());
        assert!(validate_identifier(IdentifierKind::StepId, "..").is_err());
    }

    #[test]
    fn rejects_traversal_and_separators() {
        for value in ["../etc/passwd", "a/b", "a\\b", "/abs/path", "a/../b"] {
            assert!(
                validate_identifier(IdentifierKind::PluginId, value).is_err(),
                "expected '{value}' to be rejected"
            );
        }
    }

    #[test]
    fn rejects_leading_dot_or_dash() {
        assert!(validate_identifier(IdentifierKind::GateId, ".hidden").is_err());
        assert!(validate_identifier(IdentifierKind::GateId, "-flag").is_err());
    }

    #[test]
    fn rejects_whitespace_and_non_ascii() {
        assert!(validate_identifier(IdentifierKind::RunRef, "a b").is_err());
        assert!(validate_identifier(IdentifierKind::RunRef, "café").is_err());
        assert!(validate_identifier(IdentifierKind::RunRef, "a\tb").is_err());
    }
}
