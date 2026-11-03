// SPDX-License-Identifier: MPL-2.0
//! Filesystem policy types for workspace-scoped filesystem enforcement.

use serde::{Deserialize, Serialize};

/// Workspace access mode for a filesystem policy.
///
/// `Custom` is retained as a deserializable variant so the runtime can reject
/// it with the dedicated `filesystem_custom_mode_unsupported` reason code
/// instead of a generic serde error. It is never accepted by the compiler,
/// preflight, or any sandbox backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAccessMode {
    /// Full workspace access is allowed.
    #[default]
    Full,
    /// Workspace reads are allowed but mutations are denied.
    ReadOnly,
    /// Unsupported workspace access mode; always rejected with
    /// `filesystem_custom_mode_unsupported`.
    Custom,
}

/// Workspace-scoped access policy.
///
/// Only `mode` is meaningful: `full` grants read-write access inside the
/// workspace root and `read_only` denies workspace mutations. Workspace-
/// relative glob allow-lists are not supported; the `custom` mode is
/// rejected before dispatch with `filesystem_custom_mode_unsupported`.
/// External path grants outside the workspace root are expressed by the
/// top-level `read_allow_paths` / `write_allow_paths` lists on
/// [`FilesystemPolicy`], not here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceAccessPolicy {
    /// Workspace access mode.
    pub mode: WorkspaceAccessMode,
}

impl Default for WorkspaceAccessPolicy {
    fn default() -> Self {
        Self {
            mode: WorkspaceAccessMode::Full,
        }
    }
}

impl WorkspaceAccessPolicy {
    /// Returns true when this is the builtin workspace access default.
    pub fn is_default(&self) -> bool {
        self.mode == WorkspaceAccessMode::Full
    }
}

/// Filesystem policy attached to a workflow or step.
///
/// Two distinct concerns are separated:
///
/// - `workspace_access`: controls how much of the sandbox workspace_root the
///   step can see. Access composes monotonically across global, workflow, and
///   step levels: any `read_only` declaration keeps the effective policy
///   read-only, so a narrower scope can never widen a broader restriction.
///
/// - `read_allow_paths` / `write_allow_paths` (top-level): grant **additive**
///   access to paths that live **outside** `workspace_root`, such as system
///   configuration files or the user's home directory. These lists are merged
///   (union) across global → workflow → step so that more specific levels can
///   extend rather than replace the grants from broader levels. Every entry
///   must be an absolute path (`/…`) or a home-relative path (`~/…`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FilesystemPolicy {
    /// Workspace access controls. `None` means "not specified" — the resolver
    /// falls through to the workflow/global/builtin level so that a step
    /// declaring only external path grants does not unintentionally replace
    /// an inherited `read_only` workspace mode with `full`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_access: Option<WorkspaceAccessPolicy>,
    /// Additional read-only paths outside the workspace root.
    /// Each entry must be absolute (`/etc/gitconfig`) or home-relative
    /// (`~/.gitconfig`). Merged additively across global/workflow/step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_allow_paths: Vec<String>,
    /// Additional writable paths outside the workspace root.
    /// Each entry must be absolute or home-relative (`~/`-prefixed).
    /// Merged additively across global/workflow/step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_allow_paths: Vec<String>,
}

impl FilesystemPolicy {
    /// Returns true when the policy is the builtin workspace default.
    pub fn is_default(&self) -> bool {
        self.workspace_access.is_none()
            && self.read_allow_paths.is_empty()
            && self.write_allow_paths.is_empty()
    }

    /// Resolves the effective policy for one step.
    ///
    /// `workspace_access` selects the narrowest declaration across global,
    /// workflow, and step levels. The top-level external path lists
    /// (`read_allow_paths`, `write_allow_paths`) are **merged** — the
    /// effective list is the union of all levels that provided one, with
    /// duplicates removed while preserving first-seen order.
    pub fn resolve(
        global: Option<&Self>,
        workflow: Option<&Self>,
        step: Option<&Self>,
    ) -> (Self, FilesystemPolicySource) {
        let (workspace_access, source) = Self::resolve_workspace_access(global, workflow, step);

        // External path lists: additive union across all levels.
        let read_allow_paths = merge_external_paths([
            global.map(|p| p.read_allow_paths.as_slice()),
            workflow.map(|p| p.read_allow_paths.as_slice()),
            step.map(|p| p.read_allow_paths.as_slice()),
        ]);
        let write_allow_paths = merge_external_paths([
            global.map(|p| p.write_allow_paths.as_slice()),
            workflow.map(|p| p.write_allow_paths.as_slice()),
            step.map(|p| p.write_allow_paths.as_slice()),
        ]);

        (
            Self {
                workspace_access,
                read_allow_paths,
                write_allow_paths,
            },
            source,
        )
    }

    /// Resolves the narrowest workspace access declaration and its source.
    fn resolve_workspace_access(
        global: Option<&Self>,
        workflow: Option<&Self>,
        step: Option<&Self>,
    ) -> (Option<WorkspaceAccessPolicy>, FilesystemPolicySource) {
        let levels = [
            (global, FilesystemPolicySource::Global),
            (workflow, FilesystemPolicySource::Workflow),
            (step, FilesystemPolicySource::Step),
        ];
        let mut selected = None;
        for (policy, source) in levels {
            if let Some(access) = policy.and_then(|policy| policy.workspace_access.as_ref()) {
                match access.mode {
                    WorkspaceAccessMode::Custom => {
                        return (Some(access.clone()), source);
                    }
                    WorkspaceAccessMode::ReadOnly => {
                        if !matches!(selected, Some((WorkspaceAccessMode::ReadOnly, _))) {
                            selected = Some((access.mode, source));
                        }
                    }
                    WorkspaceAccessMode::Full if selected.is_none() => {
                        selected = Some((access.mode, source));
                    }
                    WorkspaceAccessMode::Full => {}
                }
            }
        }
        selected.map_or((None, FilesystemPolicySource::Builtin), |(mode, source)| {
            (Some(WorkspaceAccessPolicy { mode }), source)
        })
    }
}

/// Merges external path slices from multiple levels, preserving first-seen
/// order and deduplicating exact string matches.
fn merge_external_paths<const N: usize>(levels: [Option<&[String]>; N]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for level in levels.into_iter().flatten() {
        for path in level {
            if seen.insert(path.clone()) {
                merged.push(path.clone());
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(
        mode: Option<WorkspaceAccessMode>,
        ext_reads: &[&str],
        ext_writes: &[&str],
    ) -> FilesystemPolicy {
        FilesystemPolicy {
            workspace_access: mode.map(|m| WorkspaceAccessPolicy { mode: m }),
            read_allow_paths: ext_reads.iter().map(|s| s.to_string()).collect(),
            write_allow_paths: ext_writes.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn resolve_builtin_when_nothing_is_set() {
        let (p, src) = FilesystemPolicy::resolve(None, None, None);
        assert_eq!(src, FilesystemPolicySource::Builtin);
        assert!(p.workspace_access.is_none());
        assert!(p.read_allow_paths.is_empty());
        assert!(p.write_allow_paths.is_empty());
    }

    #[test]
    fn workspace_binding_access_composes_monotonically() {
        let global = policy(Some(WorkspaceAccessMode::ReadOnly), &[], &[]);
        let workflow = policy(Some(WorkspaceAccessMode::Full), &[], &[]);
        let step = policy(Some(WorkspaceAccessMode::ReadOnly), &[], &[]);

        let (p, src) = FilesystemPolicy::resolve(Some(&global), Some(&workflow), Some(&step));
        assert_eq!(src, FilesystemPolicySource::Global);
        assert_eq!(
            p.workspace_access.as_ref().unwrap().mode,
            WorkspaceAccessMode::ReadOnly
        );

        let (p, src) = FilesystemPolicy::resolve(Some(&global), Some(&workflow), None);
        assert_eq!(src, FilesystemPolicySource::Global);
        assert_eq!(
            p.workspace_access.as_ref().unwrap().mode,
            WorkspaceAccessMode::ReadOnly
        );

        // global wins when no workflow/step
        let (p, src) = FilesystemPolicy::resolve(Some(&global), None, None);
        assert_eq!(src, FilesystemPolicySource::Global);
        assert_eq!(
            p.workspace_access.as_ref().unwrap().mode,
            WorkspaceAccessMode::ReadOnly
        );
    }

    #[test]
    fn workspace_binding_step_cannot_widen_workflow_access() {
        let workflow = policy(Some(WorkspaceAccessMode::ReadOnly), &[], &[]);
        let step = policy(Some(WorkspaceAccessMode::Full), &[], &[]);

        let (resolved, source) = FilesystemPolicy::resolve(None, Some(&workflow), Some(&step));

        assert_eq!(source, FilesystemPolicySource::Workflow);
        assert_eq!(
            resolved.workspace_access.unwrap().mode,
            WorkspaceAccessMode::ReadOnly
        );
    }

    #[test]
    fn resolve_external_paths_are_merged_across_all_levels() {
        let global = policy(
            Some(WorkspaceAccessMode::Full),
            &["/etc/gitconfig", "~/.gitconfig"],
            &[],
        );
        let workflow = policy(
            Some(WorkspaceAccessMode::Full),
            &["~/.config/git"],
            &["~/.cache/git"],
        );
        let step = policy(Some(WorkspaceAccessMode::Full), &["~/.ssh"], &[]);

        let (p, _) = FilesystemPolicy::resolve(Some(&global), Some(&workflow), Some(&step));
        assert_eq!(
            p.read_allow_paths,
            vec!["/etc/gitconfig", "~/.gitconfig", "~/.config/git", "~/.ssh"]
        );
        assert_eq!(p.write_allow_paths, vec!["~/.cache/git"]);
    }

    #[test]
    fn resolve_external_paths_deduplicates_preserving_first_seen_order() {
        let global = policy(
            Some(WorkspaceAccessMode::Full),
            &["/etc/gitconfig", "~/.gitconfig"],
            &[],
        );
        let workflow = policy(
            Some(WorkspaceAccessMode::Full),
            &["/etc/gitconfig", "~/.config/git"],
            &[],
        );

        let (p, _) = FilesystemPolicy::resolve(Some(&global), Some(&workflow), None);
        // /etc/gitconfig appears in both; only the first occurrence is kept.
        assert_eq!(
            p.read_allow_paths,
            vec!["/etc/gitconfig", "~/.gitconfig", "~/.config/git"]
        );
    }

    #[test]
    fn resolve_external_paths_merged_even_when_workspace_access_uses_builtin() {
        let global = policy(Some(WorkspaceAccessMode::Full), &["/etc/gitconfig"], &[]);

        let (p, src) = FilesystemPolicy::resolve(Some(&global), None, None);
        assert_eq!(src, FilesystemPolicySource::Global);
        assert_eq!(p.read_allow_paths, vec!["/etc/gitconfig"]);
    }

    #[test]
    fn is_default_false_when_external_paths_present() {
        let p = policy(Some(WorkspaceAccessMode::Full), &["/etc/gitconfig"], &[]);
        assert!(!p.is_default());
    }

    #[test]
    fn grant_only_step_inherits_read_only_from_workflow() {
        // A step that declares only external read_allow_paths (no
        // workspace_access) must inherit the workflow's read_only mode rather
        // than silently widening to full.
        let workflow = policy(Some(WorkspaceAccessMode::ReadOnly), &[], &[]);
        let step = policy(None, &["/etc/gitconfig"], &[]);

        let (p, src) = FilesystemPolicy::resolve(None, Some(&workflow), Some(&step));
        assert_eq!(src, FilesystemPolicySource::Workflow);
        assert_eq!(
            p.workspace_access.as_ref().unwrap().mode,
            WorkspaceAccessMode::ReadOnly
        );
        assert_eq!(p.read_allow_paths, vec!["/etc/gitconfig"]);
    }

    #[test]
    fn grant_only_step_inherits_read_only_from_global() {
        let global = policy(Some(WorkspaceAccessMode::ReadOnly), &[], &[]);
        let workflow = policy(None, &["~/.config/git"], &[]);
        let step = policy(None, &["~/.ssh"], &[]);

        let (p, src) = FilesystemPolicy::resolve(Some(&global), Some(&workflow), Some(&step));
        assert_eq!(src, FilesystemPolicySource::Global);
        assert_eq!(
            p.workspace_access.as_ref().unwrap().mode,
            WorkspaceAccessMode::ReadOnly
        );
        assert_eq!(p.read_allow_paths, vec!["~/.config/git", "~/.ssh"]);
    }

    #[test]
    fn grant_only_step_without_inherited_mode_resolves_to_builtin() {
        // No global or workflow workspace_access declared; step has only
        // external paths. Should resolve to builtin (None = full at dispatch).
        let step = policy(None, &["/etc/gitconfig"], &[]);
        let (p, src) = FilesystemPolicy::resolve(None, None, Some(&step));
        assert_eq!(src, FilesystemPolicySource::Builtin);
        assert!(p.workspace_access.is_none());
        assert_eq!(p.read_allow_paths, vec!["/etc/gitconfig"]);
    }
}

/// Source of an effective filesystem policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemPolicySource {
    /// Builtin default policy.
    Builtin,
    /// Policy loaded from `transist/settings.yaml`.
    Global,
    /// Top-level workflow policy.
    Workflow,
    /// Step-level workflow policy.
    Step,
}
