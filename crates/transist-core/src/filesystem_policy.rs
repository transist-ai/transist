// SPDX-License-Identifier: MPL-2.0
//! Runtime filesystem policy enforcement: canonicalization, glob matching,
//! workspace scoping, and denial diagnostics.

use std::fmt;
use std::path::{Path, PathBuf};
use transist_protocol::{
    FilesystemPolicy, FilesystemPolicySource, WorkspaceAccessMode, canonical_external_policy_path,
};

const OUTSIDE_WORKSPACE_PATTERN: &str = "<outside_workspace>";
const UNRESOLVABLE_PATH_PATTERN: &str = "<unresolvable>";
const READ_ONLY_PATTERN: &str = "<read_only>";
const CUSTOM_UNSUPPORTED_PATTERN: &str = "<filesystem_custom_mode_unsupported>";

/// Filesystem operation being checked against policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemOperation {
    /// Read a file.
    Read,
    /// List a directory.
    List,
    /// Create or write a file.
    Write,
    /// Delete a file or directory.
    Delete,
    /// Rename a file or directory.
    Rename,
}

impl FilesystemOperation {
    fn requires_write(self) -> bool {
        matches!(self, Self::Write | Self::Delete | Self::Rename)
    }
}

/// Diagnostic information about a filesystem policy denial.
#[derive(Debug, Clone)]
pub struct FilesystemDenial {
    /// The step whose file access was denied.
    pub step_id: String,
    /// The surface where the denial occurred.
    pub surface: String,
    /// The filesystem policy source at the time of denial.
    pub source: FilesystemPolicySource,
    /// The filesystem operation that was denied.
    pub operation: FilesystemOperation,
    /// The workspace access mode at the time of denial.
    pub mode: WorkspaceAccessMode,
    /// The glob pattern or sentinel that matched the denied path.
    pub matched_pattern: String,
    /// The workspace-relative path that was denied.
    pub workspace_relative_path: String,
}

impl FilesystemDenial {
    /// Serialize this denial into a JSON event payload.
    pub fn to_event_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "step_id": self.step_id,
            "surface": self.surface,
            "source": self.source,
            "operation": self.operation,
            "mode": self.mode,
            "matched_pattern": self.matched_pattern,
            "path": self.workspace_relative_path,
        })
    }
}

impl fmt::Display for FilesystemDenial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "step '{}' {} '{}' denied by filesystem policy ({:?}, pattern '{}')",
            self.step_id,
            self.surface,
            self.workspace_relative_path,
            self.operation,
            self.matched_pattern
        )
    }
}

impl std::error::Error for FilesystemDenial {}

/// Checks a path against the effective filesystem policy.
pub fn check_path(
    policy: &FilesystemPolicy,
    source: FilesystemPolicySource,
    operation: FilesystemOperation,
    path: &Path,
    step_id: &str,
    surface: &str,
    workspace_root: &Path,
) -> std::result::Result<(), FilesystemDenial> {
    if matches!(
        policy.workspace_access.as_ref().map(|access| access.mode),
        Some(WorkspaceAccessMode::Custom)
    ) {
        let path = if path.is_absolute() {
            diagnostic_path(path, workspace_root)
        } else {
            normalize_relative_path(path)
        };
        return Err(build_denial(
            policy,
            source,
            operation,
            step_id,
            surface,
            CUSTOM_UNSUPPORTED_PATTERN,
            &path,
        ));
    }
    let Some(canonical_root) = canonicalize_path(workspace_root) else {
        return Err(build_denial(
            policy,
            source,
            operation,
            step_id,
            surface,
            UNRESOLVABLE_PATH_PATTERN,
            ".",
        ));
    };
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        canonical_root.join(path)
    };
    let Some(canonical_path) = canonicalize_path_for_operation(&absolute_path, operation) else {
        return Err(build_denial(
            policy,
            source,
            operation,
            step_id,
            surface,
            UNRESOLVABLE_PATH_PATTERN,
            &diagnostic_path(&absolute_path, &canonical_root),
        ));
    };
    let Ok(relative) = canonical_path.strip_prefix(&canonical_root) else {
        if is_allowed_external_path(policy, operation, &canonical_path) {
            return Ok(());
        }
        return Err(build_denial(
            policy,
            source,
            operation,
            step_id,
            surface,
            OUTSIDE_WORKSPACE_PATTERN,
            &normalize_relative_path(&canonical_path),
        ));
    };
    let relative = normalize_relative_path(relative);
    check_relative_path(policy, source, operation, &relative, step_id, surface)
}

fn is_allowed_external_path(
    policy: &FilesystemPolicy,
    operation: FilesystemOperation,
    path: &Path,
) -> bool {
    let matches = |raw: &String, allow_missing| {
        canonical_external_policy_path(raw, allow_missing)
            .is_ok_and(|allowed| path.starts_with(allowed))
    };
    let writable = || {
        policy
            .write_allow_paths
            .iter()
            .any(|raw| matches(raw, true))
    };
    match operation {
        FilesystemOperation::Read | FilesystemOperation::List => {
            policy
                .read_allow_paths
                .iter()
                .any(|raw| matches(raw, false))
                || writable()
        }
        FilesystemOperation::Write | FilesystemOperation::Delete | FilesystemOperation::Rename => {
            writable()
        }
    }
}

/// Checks a workspace-relative path against the effective filesystem policy.
///
/// `custom` mode is unsupported and always denies: the compiler, preflight,
/// and sandbox backends reject it before dispatch with
/// `filesystem_custom_mode_unsupported`. This guard keeps enforcement
/// fail-closed if a persisted recipe carrying `custom` reaches the runtime
/// result-file guard.
pub fn check_relative_path(
    policy: &FilesystemPolicy,
    source: FilesystemPolicySource,
    operation: FilesystemOperation,
    workspace_relative: &str,
    step_id: &str,
    surface: &str,
) -> std::result::Result<(), FilesystemDenial> {
    let mode = policy
        .workspace_access
        .as_ref()
        .map(|wa| wa.mode)
        .unwrap_or(WorkspaceAccessMode::Full);
    match mode {
        WorkspaceAccessMode::Full => Ok(()),
        WorkspaceAccessMode::ReadOnly if operation.requires_write() => Err(build_denial(
            policy,
            source,
            operation,
            step_id,
            surface,
            READ_ONLY_PATTERN,
            workspace_relative,
        )),
        WorkspaceAccessMode::ReadOnly => Ok(()),
        WorkspaceAccessMode::Custom => Err(build_denial(
            policy,
            source,
            operation,
            step_id,
            surface,
            CUSTOM_UNSUPPORTED_PATTERN,
            workspace_relative,
        )),
    }
}

/// Resolves a path to its canonical form following symlinks.
fn canonicalize_path(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

fn canonicalize_path_for_operation(path: &Path, operation: FilesystemOperation) -> Option<PathBuf> {
    match operation {
        FilesystemOperation::Read | FilesystemOperation::List | FilesystemOperation::Delete => {
            canonicalize_path(path)
        }
        FilesystemOperation::Write | FilesystemOperation::Rename => {
            canonicalize_path(path).or_else(|| canonicalize_path_allow_missing(path))
        }
    }
}

fn canonicalize_path_allow_missing(path: &Path) -> Option<PathBuf> {
    let mut missing_components = Vec::new();
    let mut cursor = path;

    loop {
        if let Some(canonical) = canonicalize_path(cursor) {
            let mut resolved = canonical;
            for component in missing_components.iter().rev() {
                resolved.push(component);
            }
            return Some(resolved);
        }

        let component = cursor.file_name()?.to_os_string();
        missing_components.push(component);
        cursor = cursor.parent()?;
    }
}

fn build_denial(
    policy: &FilesystemPolicy,
    source: FilesystemPolicySource,
    operation: FilesystemOperation,
    step_id: &str,
    surface: &str,
    matched_pattern: &str,
    workspace_relative_path: &str,
) -> FilesystemDenial {
    FilesystemDenial {
        step_id: step_id.to_string(),
        surface: surface.to_string(),
        source,
        operation,
        mode: policy
            .workspace_access
            .as_ref()
            .map(|wa| wa.mode)
            .unwrap_or(WorkspaceAccessMode::Full),
        matched_pattern: matched_pattern.to_string(),
        workspace_relative_path: workspace_relative_path.to_string(),
    }
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn diagnostic_path(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root)
        .map(normalize_relative_path)
        .unwrap_or_else(|_| {
            path.file_name()
                .and_then(|name| name.to_str())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(UNRESOLVABLE_PATH_PATTERN)
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn custom_policy() -> FilesystemPolicy {
        FilesystemPolicy {
            workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                mode: WorkspaceAccessMode::Custom,
            }),
            read_allow_paths: Vec::new(),
            write_allow_paths: Vec::new(),
        }
    }

    #[test]
    fn read_only_denies_write_operations() {
        let policy = FilesystemPolicy {
            workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                mode: WorkspaceAccessMode::ReadOnly,
            }),
            read_allow_paths: Vec::new(),
            write_allow_paths: Vec::new(),
        };
        let denial = check_relative_path(
            &policy,
            FilesystemPolicySource::Step,
            FilesystemOperation::Write,
            "reports/out.txt",
            "step-1",
            "artifact",
        )
        .expect_err("expected denial");
        assert_eq!(denial.matched_pattern, READ_ONLY_PATTERN);
    }

    #[test]
    fn custom_policy_always_denies() {
        let policy = custom_policy();
        let denial = check_relative_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            "secrets/token.txt",
            "step-1",
            "artifact",
        )
        .expect_err("expected denial");
        assert_eq!(denial.matched_pattern, CUSTOM_UNSUPPORTED_PATTERN);
    }

    #[test]
    fn check_path_denies_outside_workspace_path() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let file = tmp.path().join("outside.txt");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::write(&file, b"outside").expect("write file");

        let policy = FilesystemPolicy::default();
        let denial = check_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            &file,
            "step-1",
            "evidence",
            &workspace,
        )
        .expect_err("expected denial");
        assert_eq!(denial.matched_pattern, OUTSIDE_WORKSPACE_PATTERN);
    }

    #[test]
    fn check_path_allows_read_under_external_allow_path() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let external_root = tmp.path().join("workflow-assets");
        let prompt = external_root.join("prompts/task.md");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::create_dir_all(prompt.parent().expect("prompt parent")).expect("mkdir prompts");
        fs::write(&prompt, b"prompt").expect("write prompt");

        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec![external_root.display().to_string()],
            write_allow_paths: Vec::new(),
        };
        check_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            &prompt,
            "step-1",
            "prompt_file",
            &workspace,
        )
        .expect("external prompt under read grant should be allowed");
    }

    #[test]
    fn check_path_denies_external_allow_path_sibling() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let allowed = tmp.path().join("allowed");
        let sibling = tmp.path().join("allowed-sibling/secret.txt");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::create_dir_all(&allowed).expect("mkdir allowed");
        fs::create_dir_all(sibling.parent().expect("sibling parent")).expect("mkdir sibling");
        fs::write(&sibling, b"secret").expect("write sibling");
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec![allowed.display().to_string()],
            write_allow_paths: Vec::new(),
        };

        let denial = check_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            &sibling,
            "step-1",
            "prompt_file",
            &workspace,
        )
        .expect_err("a similarly prefixed sibling must remain denied");
        assert_eq!(denial.matched_pattern, OUTSIDE_WORKSPACE_PATTERN);
    }

    #[test]
    fn check_path_denies_symlink_escape_from_external_allow_path() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::create_dir_all(&allowed).expect("mkdir allowed");
        fs::create_dir_all(&outside).expect("mkdir outside");
        fs::write(outside.join("secret.txt"), b"secret").expect("write secret");
        symlink(&outside, allowed.join("escape")).expect("create symlink");
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec![allowed.display().to_string()],
            write_allow_paths: Vec::new(),
        };

        let denial = check_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            &allowed.join("escape/secret.txt"),
            "step-1",
            "prompt_file",
            &workspace,
        )
        .expect_err("a symlink must not escape an external grant");
        assert_eq!(denial.matched_pattern, OUTSIDE_WORKSPACE_PATTERN);
    }

    #[test]
    fn check_path_rejects_custom_mode_before_external_grants() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let external = tmp.path().join("external.txt");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::write(&external, b"external").expect("write external");
        let mut policy = custom_policy();
        policy.read_allow_paths = vec![external.display().to_string()];

        let denial = check_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            &external,
            "step-1",
            "prompt_file",
            &workspace,
        )
        .expect_err("custom mode must fail closed");
        assert_eq!(denial.matched_pattern, CUSTOM_UNSUPPORTED_PATTERN);
    }

    #[test]
    fn check_path_allows_reads_under_external_write_grants() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let writable = tmp.path().join("writable");
        let file = writable.join("existing.txt");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::create_dir_all(&writable).expect("mkdir writable");
        fs::write(&file, b"existing").expect("write file");
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: Vec::new(),
            write_allow_paths: vec![writable.display().to_string()],
        };

        check_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            &file,
            "step-1",
            "artifact",
            &workspace,
        )
        .expect("writable external mounts are readable");
    }

    #[test]
    fn check_path_rejects_external_grants_with_surrounding_whitespace() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let external = tmp.path().join("external.txt");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::write(&external, b"external").expect("write external");
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec![format!(" {}", external.display())],
            write_allow_paths: Vec::new(),
        };

        check_path(
            &policy,
            FilesystemPolicySource::Workflow,
            FilesystemOperation::Read,
            &external,
            "step-1",
            "prompt_file",
            &workspace,
        )
        .expect_err("ambiguous external grant must fail closed");
    }

    #[test]
    fn write_allows_missing_target_inside_workspace() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(workspace.join("reports")).expect("mkdir reports");

        let policy = FilesystemPolicy::default();
        let target = workspace.join("reports/new/output.txt");
        check_path(
            &policy,
            FilesystemPolicySource::Step,
            FilesystemOperation::Write,
            &target,
            "step-1",
            "artifact",
            &workspace,
        )
        .expect("missing write target inside workspace should be allowed");
    }

    #[test]
    fn check_path_denies_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        let secret = outside.join("secret.txt");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::create_dir_all(&outside).expect("mkdir outside");
        fs::write(&secret, b"secret").expect("write secret");
        symlink(&outside, workspace.join("linked_outside")).expect("create symlink");

        let policy = FilesystemPolicy::default();
        let denial = check_path(
            &policy,
            FilesystemPolicySource::Builtin,
            FilesystemOperation::Read,
            &workspace.join("linked_outside/secret.txt"),
            "step-1",
            "prompt_file",
            &workspace,
        )
        .expect_err("expected symlink escape denial");
        assert_eq!(denial.matched_pattern, OUTSIDE_WORKSPACE_PATTERN);
    }

    #[test]
    fn check_path_denies_symlink_escape_in_read_only_mode() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        let secret = outside.join("secret.txt");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::create_dir_all(&outside).expect("mkdir outside");
        fs::write(&secret, b"secret").expect("write secret");
        symlink(&outside, workspace.join("linked_outside")).expect("create symlink");

        let policy = FilesystemPolicy {
            workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                mode: WorkspaceAccessMode::ReadOnly,
            }),
            read_allow_paths: Vec::new(),
            write_allow_paths: Vec::new(),
        };
        let denial = check_path(
            &policy,
            FilesystemPolicySource::Step,
            FilesystemOperation::Read,
            &workspace.join("linked_outside/secret.txt"),
            "step-1",
            "artifact",
            &workspace,
        )
        .expect_err("expected symlink escape denial under read_only");
        assert_eq!(denial.matched_pattern, OUTSIDE_WORKSPACE_PATTERN);
    }
}
