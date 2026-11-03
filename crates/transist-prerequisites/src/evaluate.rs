// SPDX-License-Identifier: MPL-2.0
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::model::{
    BinaryPrerequisite, CommandPrerequisite, EnvPrerequisite, PathKind, PathPrerequisite,
    PrerequisiteSet, validate_prerequisite_set,
};

/// Stable reason code for executable prerequisite declarations that can no longer run.
pub const LEGACY_EXECUTABLE_PREREQUISITE_UNSUPPORTED: &str =
    "legacy_executable_prerequisite_unsupported";

/// Public error type returned by `transist-prerequisites` APIs.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PrerequisiteError {
    /// Structural validation rejected the declared prerequisite set.
    #[error("prerequisite validation: {0}")]
    Validation(String),
}

/// Stable scope label used for reporting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrerequisiteScope {
    /// Workflow-level prerequisite.
    Workflow,
    /// Plugin-level prerequisite.
    Plugin,
    /// Action-level prerequisite.
    Action,
}

impl PrerequisiteScope {
    /// Returns the stable lowercase string for display and persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
            Self::Plugin => "plugin",
            Self::Action => "action",
        }
    }
}

/// One evaluated prerequisite result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrerequisiteCheck {
    /// Scope that declared the prerequisite.
    pub scope: PrerequisiteScope,
    /// Check family name.
    pub kind: String,
    /// Human-readable target label.
    pub target: String,
    /// Result status.
    pub status: PrerequisiteStatus,
    /// Human-readable diagnostic.
    pub message: String,
}

/// Aggregate status of a prerequisite report.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrerequisiteStatus {
    /// All prerequisite checks passed.
    Passed,
    /// At least one warning occurred, but no blocker failed.
    Warning,
    /// At least one blocking prerequisite failed.
    Failed,
}

/// Aggregate report for evaluated prerequisites.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrerequisiteReport {
    /// Aggregate status.
    pub status: PrerequisiteStatus,
    /// Number of blocking failures.
    pub blocker_count: usize,
    /// Number of warnings.
    pub warning_count: usize,
    /// Individual prerequisite check results.
    #[serde(default)]
    pub checks: Vec<PrerequisiteCheck>,
}

impl PrerequisiteReport {
    /// Builds a report from individual check results.
    pub fn from_checks(checks: Vec<PrerequisiteCheck>) -> Self {
        let blocker_count = checks
            .iter()
            .filter(|check| check.status == PrerequisiteStatus::Failed)
            .count();
        let warning_count = checks
            .iter()
            .filter(|check| check.status == PrerequisiteStatus::Warning)
            .count();
        let status = if blocker_count > 0 {
            PrerequisiteStatus::Failed
        } else if warning_count > 0 {
            PrerequisiteStatus::Warning
        } else {
            PrerequisiteStatus::Passed
        };
        Self {
            status,
            blocker_count,
            warning_count,
            checks,
        }
    }

    /// Returns whether the report contains at least one blocking failure.
    pub fn has_blockers(&self) -> bool {
        self.blocker_count > 0
    }
}

/// Host evaluation context used for path resolution.
#[derive(Debug, Clone)]
pub struct EvaluationContext {
    /// Declaring scope.
    pub scope: PrerequisiteScope,
    /// Base directory used to resolve relative paths.
    pub base_dir: PathBuf,
}

impl EvaluationContext {
    /// Creates a new evaluation context.
    pub fn new(scope: PrerequisiteScope, base_dir: impl Into<PathBuf>) -> Self {
        Self {
            scope,
            base_dir: base_dir.into(),
        }
    }
}

/// Evaluates one prerequisite set on the local host.
pub fn evaluate_prerequisites(
    prerequisites: &PrerequisiteSet,
    context: &EvaluationContext,
) -> std::result::Result<PrerequisiteReport, PrerequisiteError> {
    validate_prerequisite_set(prerequisites)
        .map_err(|error| PrerequisiteError::Validation(error.to_string()))?;

    let mut checks = Vec::new();
    for binary in &prerequisites.binaries {
        checks.push(evaluate_binary(binary, context));
    }
    for env in &prerequisites.env {
        checks.push(evaluate_env(env, context));
    }
    for path in &prerequisites.paths {
        checks.push(evaluate_path(path, context));
    }
    for command in &prerequisites.commands {
        checks.push(evaluate_command(command, context));
    }
    Ok(PrerequisiteReport::from_checks(checks))
}

/// Renders a short summary string for a prerequisite report.
fn summarize_report(report: &PrerequisiteReport) -> String {
    match report.status {
        PrerequisiteStatus::Passed => "all prerequisites passed".to_string(),
        PrerequisiteStatus::Warning => format!(
            "prerequisites passed with {} warning(s)",
            report.warning_count
        ),
        PrerequisiteStatus::Failed => format!(
            "prerequisites failed with {} blocker(s) and {} warning(s)",
            report.blocker_count, report.warning_count
        ),
    }
}

/// Formats one error message for a blocking prerequisite report.
pub fn blocking_error_message(report: &PrerequisiteReport) -> String {
    let mut lines = vec![summarize_report(report)];
    for check in &report.checks {
        if check.status == PrerequisiteStatus::Failed {
            lines.push(format!(
                "- [{}:{}] {}",
                check.scope.as_str(),
                check.kind,
                check.message
            ));
        }
    }
    lines.join("\n")
}

fn evaluate_binary(
    prerequisite: &BinaryPrerequisite,
    context: &EvaluationContext,
) -> PrerequisiteCheck {
    let name = prerequisite.name.trim();
    if prerequisite.version.is_some() || prerequisite.version_check.is_some() {
        return unsupported_executable_prerequisite(
            context.scope,
            "binary_version",
            name,
            "binary version probes must use an invoke step through an installed plugin action",
        );
    }
    let Some(path) = which::which(name).ok() else {
        return failed_or_warning(
            context.scope,
            "binary",
            name,
            prerequisite.required,
            format!("required binary '{name}' is unavailable"),
        );
    };

    PrerequisiteCheck {
        scope: context.scope,
        kind: "binary".to_string(),
        target: name.to_string(),
        status: PrerequisiteStatus::Passed,
        message: format!("binary '{name}' is available at {}", path.display()),
    }
}

fn evaluate_env(prerequisite: &EnvPrerequisite, context: &EvaluationContext) -> PrerequisiteCheck {
    let name = prerequisite.name.trim();
    if std::env::var_os(name).is_some() {
        return PrerequisiteCheck {
            scope: context.scope,
            kind: "env".to_string(),
            target: name.to_string(),
            status: PrerequisiteStatus::Passed,
            message: format!("environment variable '{name}' is available"),
        };
    }
    failed_or_warning(
        context.scope,
        "env",
        name,
        prerequisite.required,
        format!("required environment variable '{name}' is unavailable"),
    )
}

fn evaluate_path(
    prerequisite: &PathPrerequisite,
    context: &EvaluationContext,
) -> PrerequisiteCheck {
    let resolved = resolve_path(&context.base_dir, prerequisite.path.trim());
    let Ok(metadata) = resolved.metadata() else {
        return failed_or_warning(
            context.scope,
            "path",
            prerequisite.path.trim(),
            prerequisite.required,
            format!("required path '{}' is unavailable", resolved.display()),
        );
    };

    let matches_kind = match prerequisite.kind {
        PathKind::Any => true,
        PathKind::File => metadata.is_file(),
        PathKind::Dir => metadata.is_dir(),
    };
    if matches_kind {
        return PrerequisiteCheck {
            scope: context.scope,
            kind: "path".to_string(),
            target: prerequisite.path.trim().to_string(),
            status: PrerequisiteStatus::Passed,
            message: format!("path '{}' is available", resolved.display()),
        };
    }

    failed_or_warning(
        context.scope,
        "path",
        prerequisite.path.trim(),
        prerequisite.required,
        format!(
            "path '{}' does not match required kind {:?}",
            resolved.display(),
            prerequisite.kind
        ),
    )
}

fn evaluate_command(
    prerequisite: &CommandPrerequisite,
    context: &EvaluationContext,
) -> PrerequisiteCheck {
    let command_label = prerequisite.argv.join(" ");
    unsupported_executable_prerequisite(
        context.scope,
        "command",
        &command_label,
        "executable checks must use an invoke step through an installed plugin action",
    )
}

fn unsupported_executable_prerequisite(
    scope: PrerequisiteScope,
    kind: &str,
    target: &str,
    migration: &str,
) -> PrerequisiteCheck {
    PrerequisiteCheck {
        scope,
        kind: kind.to_string(),
        target: target.to_string(),
        status: PrerequisiteStatus::Failed,
        message: format!("{LEGACY_EXECUTABLE_PREREQUISITE_UNSUPPORTED}: {migration}"),
    }
}

fn failed_or_warning(
    scope: PrerequisiteScope,
    kind: &str,
    target: &str,
    required: bool,
    message: String,
) -> PrerequisiteCheck {
    PrerequisiteCheck {
        scope,
        kind: kind.to_string(),
        target: target.to_string(),
        status: if required {
            PrerequisiteStatus::Failed
        } else {
            PrerequisiteStatus::Warning
        },
        message,
    }
}

fn resolve_path(base_dir: &Path, path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn validates_relative_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join("bin")).expect("mkdir");
        let report = evaluate_prerequisites(
            &PrerequisiteSet {
                paths: vec![PathPrerequisite {
                    path: "bin".to_string(),
                    kind: PathKind::Dir,
                    required: true,
                }],
                ..PrerequisiteSet::default()
            },
            &EvaluationContext::new(PrerequisiteScope::Workflow, tmp.path()),
        )
        .expect("report");
        assert_eq!(report.status, PrerequisiteStatus::Passed);
    }

    #[test]
    fn rejects_binary_version_probe_without_executing_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sentinel = tmp.path().join("version-probe-ran");
        let report = evaluate_prerequisites(
            &PrerequisiteSet {
                binaries: vec![BinaryPrerequisite {
                    name: "sh".to_string(),
                    required: true,
                    version: Some(">=1.2.0".to_string()),
                    version_check: Some(transist_protocol::VersionCheck {
                        argv: vec![
                            "sh".to_string(),
                            "-c".to_string(),
                            format!("touch {}", sentinel.display()),
                        ],
                        regex: Some(r"(\d+\.\d+)".to_string()),
                    }),
                }],
                ..PrerequisiteSet::default()
            },
            &EvaluationContext::new(PrerequisiteScope::Plugin, "."),
        )
        .expect("report");
        assert_eq!(report.status, PrerequisiteStatus::Failed);
        assert!(
            report.checks[0]
                .message
                .contains(LEGACY_EXECUTABLE_PREREQUISITE_UNSUPPORTED)
        );
        assert!(!sentinel.exists());
    }

    #[test]
    fn rejects_command_probe_without_executing_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sentinel = tmp.path().join("command-probe-ran");
        let report = evaluate_prerequisites(
            &PrerequisiteSet {
                commands: vec![CommandPrerequisite {
                    argv: vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        format!("touch {}", sentinel.display()),
                    ],
                    timeout_ms: Some(50),
                    required: true,
                }],
                ..PrerequisiteSet::default()
            },
            &EvaluationContext::new(PrerequisiteScope::Workflow, "."),
        )
        .expect("report");
        assert_eq!(report.status, PrerequisiteStatus::Failed);
        assert!(
            report.checks[0]
                .message
                .contains(LEGACY_EXECUTABLE_PREREQUISITE_UNSUPPORTED)
        );
        assert!(!sentinel.exists());
    }

    #[test]
    fn rejects_optional_executable_prerequisites() {
        let report = evaluate_prerequisites(
            &PrerequisiteSet {
                commands: vec![CommandPrerequisite {
                    argv: vec!["optional-check".to_string()],
                    timeout_ms: None,
                    required: false,
                }],
                ..PrerequisiteSet::default()
            },
            &EvaluationContext::new(PrerequisiteScope::Workflow, "."),
        )
        .expect("report");

        assert_eq!(report.status, PrerequisiteStatus::Failed);
        assert_eq!(report.blocker_count, 1);
        assert!(
            report.checks[0]
                .message
                .contains(LEGACY_EXECUTABLE_PREREQUISITE_UNSUPPORTED)
        );
    }

    #[test]
    fn rejects_malformed_legacy_executable_fields_at_evaluation() {
        let cases = [
            PrerequisiteSet {
                commands: vec![CommandPrerequisite {
                    argv: Vec::new(),
                    timeout_ms: Some(0),
                    required: true,
                }],
                ..PrerequisiteSet::default()
            },
            PrerequisiteSet {
                binaries: vec![BinaryPrerequisite {
                    name: "sh".to_string(),
                    required: true,
                    version: None,
                    version_check: Some(transist_protocol::VersionCheck {
                        argv: Vec::new(),
                        regex: None,
                    }),
                }],
                ..PrerequisiteSet::default()
            },
            PrerequisiteSet {
                binaries: vec![BinaryPrerequisite {
                    name: "sh".to_string(),
                    required: true,
                    version: None,
                    version_check: Some(transist_protocol::VersionCheck {
                        argv: vec!["sh".to_string(), "--version".to_string()],
                        regex: Some("[".to_string()),
                    }),
                }],
                ..PrerequisiteSet::default()
            },
        ];

        for prerequisites in cases {
            let report = evaluate_prerequisites(
                &prerequisites,
                &EvaluationContext::new(PrerequisiteScope::Workflow, "."),
            )
            .expect("malformed legacy fields remain parseable");
            assert_eq!(report.status, PrerequisiteStatus::Failed);
            assert!(
                report.checks[0]
                    .message
                    .contains(LEGACY_EXECUTABLE_PREREQUISITE_UNSUPPORTED)
            );
        }
    }
}
