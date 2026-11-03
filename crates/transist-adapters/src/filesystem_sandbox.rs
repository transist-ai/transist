// SPDX-License-Identifier: MPL-2.0
//! Workspace-scoped filesystem sandbox capability checks for local adapter launches.
//!
//! Provides backend detection (macOS sandbox-exec / Linux bwrap), sandbox plan
//! construction from the effective filesystem policy, and command wrapping for
//! local subprocess isolation.

// SAFETY: OS-integration: sandbox setup uses libc geteuid/getegid and env mutation in tests.
#![allow(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use transist_protocol::{
    ExecutionRequest, FilesystemPolicy, NetworkAccessMode, NetworkPolicy, WorkspaceAccessMode,
    canonical_external_policy_path,
};

use crate::paths::{
    RUNTIME_TEMP_DIR_METADATA_KEY, home_dir_from_run_root, runtime_temp_visible_root,
};

const LINUX_SYSTEM_BIND_PATHS: &[&str] = &["/usr", "/bin", "/lib", "/lib64", "/sbin"];
const MACOS_RUNTIME_WRITE_PATHS: &[&str] = &["/dev"];
/// Writable bwrap mount parent used for the private network launcher bind.
///
/// `/usr` is mounted read-only in the sandbox, so bwrap cannot create a missing
/// `/usr/libexec/transist` destination there. `/tmp` is a sandbox-owned tmpfs
/// and exists before the launcher bind is applied.
const BWRAP_NETWORK_LAUNCHER_PATH: &str = "/tmp/transist-sandbox-launcher";
const STAGING_ROOT_METADATA_KEY: &str = "__transist_staging_dir";
const RECIPE_SOURCE_DIR_METADATA_KEY: &str = "recipe_source_dir";
const INVOCATION_STAGING_DIR: &str = ".transist-staging";
const RUNTIME_TEMP_PREFIX: &str = "transist-runtime-temp-";

/// Filesystem locations that local sandboxing must allow internally.
#[derive(Debug, Clone)]
pub(crate) struct SandboxContext {
    pub home_dir: Option<PathBuf>,
    pub workspace_dir: PathBuf,
    pub artifacts_dir: PathBuf,
    pub evidence_dir: PathBuf,
    pub staging_dir: Option<PathBuf>,
    /// Runtime-created host-private temporary root for this invocation.
    pub runtime_temp_dir: Option<PathBuf>,
    pub plugin_dir: PathBuf,
    /// Authored workflow source files referenced by `source://` paths.
    pub recipe_source_dir: Option<PathBuf>,
}

/// Builds the sandbox context for one execution request and resolved plugin directory.
pub(crate) fn sandbox_context_from_request(
    request: &ExecutionRequest,
    plugin_dir: &Path,
) -> SandboxContext {
    SandboxContext {
        home_dir: home_dir_from_run_root(Path::new(&request.paths.run_root)).ok(),
        workspace_dir: PathBuf::from(&request.paths.workspace_dir),
        artifacts_dir: PathBuf::from(&request.paths.artifacts_dir),
        evidence_dir: PathBuf::from(&request.paths.evidence_dir),
        staging_dir: request
            .metadata
            .get(STAGING_ROOT_METADATA_KEY)
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
        runtime_temp_dir: request
            .metadata
            .get(RUNTIME_TEMP_DIR_METADATA_KEY)
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
        plugin_dir: plugin_dir.to_path_buf(),
        recipe_source_dir: request
            .metadata
            .get(RECIPE_SOURCE_DIR_METADATA_KEY)
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
    }
}

/// Returns true when the request carries a filesystem policy that requires
/// local sandbox enforcement. The historical name is retained for callers.
pub(crate) fn strict_filesystem_policy_requested(request: &ExecutionRequest) -> bool {
    request
        .policy
        .get("filesystem")
        .is_some_and(|value| !value.is_null())
}

/// Local process and MCP invocations are always sandboxed.
pub(crate) fn strict_local_policy_requested(request: &ExecutionRequest) -> bool {
    let _ = request;
    true
}

/// Human-readable reason for failing closed on local execution.
pub(crate) fn strict_local_sandbox_failure_message() -> String {
    match find_backend() {
        Some(Backend::SandboxExec) => {
            "workspace-scoped filesystem sandboxing: sandbox-exec is available but plan construction failed"
                .to_string()
        }
        Some(Backend::Bwrap) => {
            "workspace-scoped filesystem sandboxing: bwrap is available but plan construction failed"
                .to_string()
        }
        None => {
            if cfg!(target_os = "macos") {
                "workspace-scoped filesystem sandboxing requires /usr/bin/sandbox-exec (macOS only)"
                    .to_string()
            } else if cfg!(target_os = "linux") {
                "workspace-scoped filesystem sandboxing requires bwrap (bubblewrap) on Linux"
                    .to_string()
            } else {
                "workspace-scoped filesystem sandboxing is not supported on this platform"
                    .to_string()
            }
        }
    }
}

/// Extracts the effective filesystem policy from an execution request.
///
/// Returns `Ok(None)` when the request has no filesystem policy. Returns
/// `Ok(Some(...))` for valid workspace-scoped policies. Returns `Err(...)`
/// when policy metadata is malformed so local enforcement can fail closed.
pub(crate) fn filesystem_policy_from_request(
    request: &ExecutionRequest,
) -> Result<Option<FilesystemPolicy>, String> {
    let Some(fs) = request.policy.get("filesystem") else {
        return Ok(None);
    };
    if fs.is_null() {
        return Ok(None);
    }
    let workspace_access = match fs.get("workspace_access") {
        Some(value) if !value.is_null() => Some(
            serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid filesystem.workspace_access: {error}"))?,
        ),
        _ => None,
    };
    let read_allow_paths = fs
        .get("read_allow_paths")
        .cloned()
        .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
        .unwrap_or_default();
    let write_allow_paths = fs
        .get("write_allow_paths")
        .cloned()
        .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
        .unwrap_or_default();
    let policy = FilesystemPolicy {
        workspace_access,
        read_allow_paths,
        write_allow_paths,
    };
    validate_workspace_policy(&policy)?;
    Ok(Some(policy))
}

/// Extracts the effective network policy from an execution request.
pub(crate) fn network_policy_from_request(
    request: &ExecutionRequest,
) -> Result<Option<NetworkPolicy>, String> {
    let Some(network) = request.policy.get("network") else {
        return Ok(None);
    };
    if network.is_null() {
        return Ok(None);
    }
    serde_json::from_value(network.clone())
        .map(Some)
        .map_err(|error| format!("invalid network policy: {error}"))
}

/// A validated sandbox plan that wraps a subprocess command for local isolation.
#[derive(Debug)]
pub(crate) struct SandboxPlan {
    backend: Backend,
    #[allow(dead_code)]
    workspace_dir: PathBuf,
    #[allow(dead_code)]
    policy: FilesystemPolicy,
    profile_body: Option<String>,
    bwrap_args: Vec<String>,
    #[allow(dead_code)]
    guard_dir: Option<PathBuf>,
    /// Strict network mode enforced by the internal Linux launcher. When
    /// present and the launcher cannot be located at command-construction time,
    /// the sandboxed command fails closed. The launcher path itself is resolved
    /// lazily so plan construction (preflight) does not fail on an ephemeral
    /// install layout.
    launcher_network_mode: Option<LinuxNetworkMode>,
    preflight_concurrency_eligible: bool,
}

impl SandboxPlan {
    /// Returns whether this constructed plan proves probe-local write authority.
    pub(crate) fn preflight_concurrency_eligible(&self) -> bool {
        self.preflight_concurrency_eligible
    }
}

/// Internal launcher modes; unrestricted plans never configure an interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxNetworkMode {
    None,
    Loopback,
}

impl LinuxNetworkMode {
    fn argument(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Loopback => "loopback",
        }
    }
}

/// Resolved internal launcher invocation for strict bwrap network modes.
#[derive(Debug)]
struct NetworkLauncherSpec {
    launcher_path: PathBuf,
    target_uid: u32,
    target_gid: u32,
}

/// Builds a sandbox plan from the filesystem policy and request context.
///
/// Returns the plan and a short backend label on success.
/// Returns an error message when the plan cannot be constructed (backend
/// unavailable, path canonicalization fails, or a pattern cannot be enforced).
pub(crate) fn build_sandbox_plan(
    filesystem_policy: &FilesystemPolicy,
    network_policy: &NetworkPolicy,
    context: &SandboxContext,
) -> Result<SandboxPlan, String> {
    build_sandbox_plan_for_backend(find_backend(), filesystem_policy, network_policy, context)
}

/// Validates the same sandbox authority without creating future runtime mounts.
/// Missing artifact/evidence directories are allowed at preflight; existing
/// non-directories still fail. The returned plan is validation-only evidence.
pub(crate) fn validate_sandbox_plan(
    filesystem_policy: &FilesystemPolicy,
    network_policy: &NetworkPolicy,
    context: &SandboxContext,
) -> Result<SandboxPlan, String> {
    build_sandbox_plan_with_mount_policy(
        find_backend(),
        filesystem_policy,
        network_policy,
        context,
        RuntimeMountPolicy::ValidateOnly,
    )
}

#[derive(Clone, Copy)]
enum RuntimeMountPolicy {
    Create,
    ValidateOnly,
}

fn build_sandbox_plan_for_backend(
    backend: Option<Backend>,
    filesystem_policy: &FilesystemPolicy,
    network_policy: &NetworkPolicy,
    context: &SandboxContext,
) -> Result<SandboxPlan, String> {
    build_sandbox_plan_with_mount_policy(
        backend,
        filesystem_policy,
        network_policy,
        context,
        RuntimeMountPolicy::Create,
    )
}

fn build_sandbox_plan_with_mount_policy(
    backend: Option<Backend>,
    filesystem_policy: &FilesystemPolicy,
    network_policy: &NetworkPolicy,
    context: &SandboxContext,
    runtime_mount_policy: RuntimeMountPolicy,
) -> Result<SandboxPlan, String> {
    let backend = backend.ok_or_else(strict_local_sandbox_failure_message)?;
    if workspace_mode_unsupported(filesystem_policy) {
        return Err(format!(
            "{FILESYSTEM_CUSTOM_MODE_UNSUPPORTED}: workspace_access.mode 'custom' is not supported"
        ));
    }
    let canonical_workspace = std::fs::canonicalize(&context.workspace_dir).map_err(|e| {
        format!(
            "failed to canonicalize workspace_dir '{}': {}",
            context.workspace_dir.display(),
            e
        )
    })?;
    if contains_control_chars(&canonical_workspace.display().to_string()) {
        return Err(format!(
            "workspace_dir '{}' contains control characters and cannot be sandboxed safely",
            canonical_workspace.display()
        ));
    }
    let canonical_context = CanonicalSandboxContext::new(context, canonical_workspace.clone())?;
    // External grants must not overlap the workspace; otherwise a
    // `read_only` workspace can be reopened writable via a grant that points
    // at the workspace or one of its ancestors.
    validate_external_grants_against_workspace(filesystem_policy, &canonical_workspace)?;
    validate_external_grants_against_protected_roots(filesystem_policy, &canonical_context)?;
    match backend {
        Backend::SandboxExec => {
            build_sandbox_exec_plan(&canonical_context, filesystem_policy, network_policy)
        }
        Backend::Bwrap => build_bwrap_plan(
            &canonical_context,
            filesystem_policy,
            network_policy,
            runtime_mount_policy,
        ),
    }
}

#[derive(Debug, Clone)]
struct CanonicalSandboxContext {
    home_dir: Option<PathBuf>,
    workspace_dir: PathBuf,
    artifacts_dir: PathBuf,
    evidence_dir: PathBuf,
    staging_dir: Option<PathBuf>,
    runtime_temp_dir: Option<PathBuf>,
    plugin_dir: PathBuf,
    recipe_source_dir: Option<PathBuf>,
}

impl CanonicalSandboxContext {
    fn new(context: &SandboxContext, workspace_dir: PathBuf) -> Result<Self, String> {
        let home_dir = context
            .home_dir
            .as_ref()
            .map(|path| canonicalize_path_allow_missing(path, "home_dir"))
            .transpose()?;
        let artifacts_dir =
            canonicalize_path_allow_missing(&context.artifacts_dir, "artifacts_dir")?;
        let evidence_dir = canonicalize_path_allow_missing(&context.evidence_dir, "evidence_dir")?;
        let staging_dir = context
            .staging_dir
            .as_ref()
            .map(|path| canonicalize_required_path(path, "staging_dir"))
            .transpose()?;
        if let Some(staging_dir) = &staging_dir {
            validate_staging_context(staging_dir, &artifacts_dir, &evidence_dir)?;
        }
        let runtime_temp_dir = context
            .runtime_temp_dir
            .as_ref()
            .map(|path| canonicalize_required_path(path, "runtime_temp_dir"))
            .transpose()?;
        if let Some(runtime_temp_dir) = &runtime_temp_dir {
            validate_runtime_temp_context(runtime_temp_dir)?;
        }
        let plugin_dir = canonicalize_required_path(&context.plugin_dir, "plugin_dir")?;
        let recipe_source_dir = context
            .recipe_source_dir
            .as_ref()
            .map(|path| canonicalize_required_path(path, "recipe_source_dir"))
            .transpose()?;
        Ok(Self {
            home_dir,
            workspace_dir,
            artifacts_dir,
            evidence_dir,
            staging_dir,
            runtime_temp_dir,
            plugin_dir,
            recipe_source_dir,
        })
    }
}

fn validate_runtime_temp_context(path: &Path) -> Result<(), String> {
    let expected_parent = runtime_temp_parent();
    if path.parent() != Some(expected_parent.as_path())
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(RUNTIME_TEMP_PREFIX))
    {
        return Err(format!(
            "runtime_temp_dir '{}' is not a runtime temporary directory",
            path.display()
        ));
    }
    Ok(())
}

fn runtime_temp_parent() -> PathBuf {
    transist_core::runtime_temp_parent()
}

fn validate_runtime_temp_isolation(
    context: &CanonicalSandboxContext,
    policy: &FilesystemPolicy,
) -> Result<(), String> {
    let Some(temporary) = &context.runtime_temp_dir else {
        return Ok(());
    };
    let mut exposed = vec![
        &context.workspace_dir,
        &context.plugin_dir,
        &context.artifacts_dir,
        &context.evidence_dir,
    ];
    exposed.extend(context.staging_dir.iter());
    exposed.extend(context.home_dir.iter());
    for root in exposed {
        if paths_overlap(temporary, root) {
            return Err("runtime_temp_exposed_root_overlap: private temporary root overlaps an exposed workspace, package, home or staging root".into());
        }
    }
    for raw in policy
        .read_allow_paths
        .iter()
        .chain(&policy.write_allow_paths)
    {
        let root = transist_protocol::canonical_external_policy_path(raw, true)
            .map_err(|e| e.to_string())?;
        if paths_overlap(temporary, &root) {
            return Err("runtime_temp_exposed_root_overlap: private temporary root overlaps an external grant".into());
        }
    }
    Ok(())
}

fn source_directory_is_safe(context: &CanonicalSandboxContext, source: &Path) -> bool {
    let temporary_parent = runtime_temp_parent();
    let staging_parent = context.staging_dir.as_ref().and_then(|root| root.parent());
    !paths_overlap(source, &temporary_parent)
        && !context
            .home_dir
            .as_ref()
            .is_some_and(|root| paths_overlap(source, root))
        && !staging_parent.is_some_and(|root| paths_overlap(source, root))
}

fn canonicalize_required_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| {
        format!(
            "failed to canonicalize {label} '{}': {error}",
            path.display()
        )
    })
}

fn canonicalize_path_allow_missing(path: &Path, label: &str) -> Result<PathBuf, String> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Ok(canonical);
    }

    let mut missing_components = Vec::new();
    let mut cursor = path;
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(canonical) => {
                let mut resolved = canonical;
                for component in missing_components.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(_) => {
                let component = cursor.file_name().ok_or_else(|| {
                    format!(
                        "failed to canonicalize {label} '{}': missing ancestor",
                        path.display()
                    )
                })?;
                missing_components.push(component.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    format!(
                        "failed to canonicalize {label} '{}': missing ancestor",
                        path.display()
                    )
                })?;
            }
        }
    }
}

fn validate_staging_context(
    staging_dir: &Path,
    artifacts_dir: &Path,
    evidence_dir: &Path,
) -> Result<(), String> {
    let is_invocation_dir = staging_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("invoke-"));
    let is_staging_parent = staging_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".transist-staging");
    if !is_invocation_dir || !is_staging_parent {
        return Err(format!(
            "staging_dir '{}' is not a runtime staging directory",
            staging_dir.display()
        ));
    }
    if artifacts_dir.parent() != Some(staging_dir) || evidence_dir.parent() != Some(staging_dir) {
        return Err("artifact/evidence directories must be direct staging children".to_string());
    }
    Ok(())
}

pub(crate) fn strict_local_sandbox_backend_label() -> &'static str {
    match find_backend() {
        Some(Backend::SandboxExec) => "sandbox-exec",
        Some(Backend::Bwrap) => "bwrap",
        None => "none",
    }
}

pub(crate) fn stderr_matches_sandbox_wrapper_failure(backend: &str, stderr: &str) -> bool {
    match backend {
        "sandbox-exec" => stderr.contains("sandbox-exec:"),
        "bwrap" => stderr.contains("bwrap:") || stderr.contains("bubblewrap:"),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Backend detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    SandboxExec,
    Bwrap,
}

fn find_backend() -> Option<Backend> {
    #[cfg(target_os = "macos")]
    {
        if Path::new("/usr/bin/sandbox-exec").is_file() {
            return Some(Backend::SandboxExec);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if find_binary("bwrap") {
            return Some(Backend::Bwrap);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn find_binary(name: &str) -> bool {
    ["/usr/bin/", "/usr/local/bin/", "/bin/"]
        .iter()
        .any(|dir| Path::new(&format!("{}{}", dir, name)).is_file())
        || std::env::var_os("PATH")
            .as_ref()
            .is_some_and(|paths| std::env::split_paths(paths).any(|dir| dir.join(name).is_file()))
}

// ---------------------------------------------------------------------------
// macOS sandbox-exec implementation
// ---------------------------------------------------------------------------

/// Build a SandboxPlan using macOS sandbox-exec.
///
/// The profile uses `allow default` so that all system calls, mach IPC, and
/// network operations work without an explicit allowlist. Reads from the
/// Transist home and shared staging parent are denied and reopened only for
/// the current package, workspace, declared external paths, and staging root.
/// Write access is narrowed to the workspace, invocation staging, and
/// explicitly declared external `write_allow_paths`.
///
/// This design avoids the macOS-specific `fork`-safety issue that arises when
/// `git` (which links against `CoreServices.framework`) spawns `ssh` as a child
/// process: the child inherits mach port state from CoreServices initialisation,
/// and a `deny default` profile fails to allow the socket operation that
/// `connectx()` needs in that context, producing a silent errno-0 failure.
/// With `allow default`, all such implicit operations are permitted while the
/// write boundary is still enforced.
fn build_sandbox_exec_plan(
    context: &CanonicalSandboxContext,
    policy: &FilesystemPolicy,
    network_policy: &NetworkPolicy,
) -> Result<SandboxPlan, String> {
    validate_runtime_temp_isolation(context, policy)?;
    let read_protection = build_macos_read_protection(context, policy)?;
    let write_allow_filters = build_macos_write_allow_filters(context, policy)?;
    let network_policy_block = build_macos_network_policy_block(network_policy);
    // In read_only mode, add an explicit deny for the workspace after the
    // allow block. A more specific rule takes precedence in the sandbox policy
    // language.
    let workspace_mode = policy
        .workspace_access
        .as_ref()
        .map(|wa| wa.mode)
        .unwrap_or(WorkspaceAccessMode::Full);
    let mut workspace_deny = if workspace_mode == WorkspaceAccessMode::ReadOnly {
        format!(
            "\n(deny file-write*\n    {})\n",
            subpath_filter(&context.workspace_dir)
        )
    } else {
        String::new()
    };
    if let Some(root) = &context.runtime_temp_dir {
        let read_root = root.join("documents/read");
        if read_root.is_dir() {
            workspace_deny.push_str(&format!(
                "\n(deny file-write*\n    {})\n(deny file-write-unlink\n    {}\n    {})\n",
                subpath_filter(&read_root),
                // Renaming an allowed writable ancestor must not move inputs out
                // from beneath the path-based read-only denial.
                literal_filter(&root.join("documents")),
                literal_filter(root),
            ));
        }
    }
    let profile = format!(
        "(version 3)\n(allow default)\n(import \"dyld-support.sb\")\n{}{}(deny file-write*\n    (subpath \"/\"))\n(allow file-write*\n{})\n{}",
        network_policy_block,
        read_protection,
        render_macos_filters(&write_allow_filters),
        workspace_deny,
    );
    Ok(SandboxPlan {
        backend: Backend::SandboxExec,
        workspace_dir: context.workspace_dir.clone(),
        policy: policy.clone(),
        profile_body: Some(profile),
        bwrap_args: Vec::new(),
        guard_dir: None,
        launcher_network_mode: None,
        // sandbox-exec permits shared runtime writes such as /dev. Stay
        // conservative until the profile can prove a fully private write set.
        preflight_concurrency_eligible: false,
    })
}

fn build_macos_read_protection(
    context: &CanonicalSandboxContext,
    policy: &FilesystemPolicy,
) -> Result<String, String> {
    let staging_parent = context.staging_dir.as_ref().and_then(|root| root.parent());
    let runtime_temp_parent = runtime_temp_parent();
    let mut filters = BTreeSet::new();
    filters.insert(subpath_filter(&context.plugin_dir));
    filters.insert(subpath_filter(&context.workspace_dir));
    if let Some(staging_dir) = &context.staging_dir {
        filters.insert(subpath_filter(staging_dir));
    }
    if let Some(runtime_temp_dir) = &context.runtime_temp_dir {
        filters.insert(subpath_filter(runtime_temp_dir));
    }
    if let Some(recipe_source_dir) = &context.recipe_source_dir
        && source_directory_is_safe(context, recipe_source_dir)
    {
        filters.insert(subpath_filter(recipe_source_dir));
    }
    for pattern in policy
        .read_allow_paths
        .iter()
        .chain(policy.write_allow_paths.iter())
    {
        let expanded = expand_external_path(pattern)
            .map_err(|e| format!("cannot expand external allow-path '{pattern}': {e}"))?;
        let canonical =
            canonicalize_path_allow_missing(&expanded, "external grant").unwrap_or(expanded);
        filters.insert(subpath_filter(&canonical));
    }
    let filters = filters.into_iter().collect::<Vec<_>>();
    let mut protection = macos_read_deny_except(&runtime_temp_parent, &filters);
    for root in context
        .home_dir
        .iter()
        .map(PathBuf::as_path)
        .chain(staging_parent)
    {
        protection.push_str(&macos_read_deny_except(root, &filters));
    }
    Ok(protection)
}

/// Denies reads below a protected parent except for the current invocation's
/// exact canonical roots. A separate child `allow` cannot reliably reopen a
/// path covered by a parent read denial in sandbox-exec. Metadata access to the
/// parent itself remains available so authorized descendants can be traversed;
/// sibling directory listing and file contents remain denied.
fn macos_read_deny_except(protected_parent: &Path, allowed_filters: &[String]) -> String {
    format!(
        "(deny file-read*\n    (require-all\n        {}\n        (require-not\n            (require-any\n{}))))\n(allow file-read-metadata\n    {})\n",
        subpath_filter(protected_parent),
        render_macos_filters(allowed_filters),
        literal_filter(protected_parent),
    )
}

fn build_macos_network_policy_block(network_policy: &NetworkPolicy) -> String {
    match network_policy.mode {
        NetworkAccessMode::Any | NetworkAccessMode::Allowlist => String::new(),
        NetworkAccessMode::Loopback => {
            "(deny network-outbound)\n(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-bind)\n"
                .to_string()
        }
        NetworkAccessMode::None => {
            "(deny network-outbound)\n(allow network-bind)\n".to_string()
        }
    }
}

/// Builds the set of `(subpath …)` and `(regex …)` filters that are explicitly
/// *allowed* for `file-write*` in an `allow default` macOS sandbox profile.
///
/// The profile denies all writes with `(deny file-write* (subpath "/"))` and
/// then re-opens the paths returned here. More-specific rules take precedence
/// over less-specific rules in the sandbox policy language, so the
/// `(allow file-write* …)` rules produced here override the blanket deny.
fn build_macos_write_allow_filters(
    context: &CanonicalSandboxContext,
    policy: &FilesystemPolicy,
) -> Result<Vec<String>, String> {
    let mut filters = BTreeSet::new();

    for path in MACOS_RUNTIME_WRITE_PATHS {
        filters.insert(subpath_filter(Path::new(path)));
    }

    if let Some(staging_dir) = &context.staging_dir {
        filters.insert(subpath_filter(staging_dir));
    }
    if let Some(runtime_temp_dir) = &context.runtime_temp_dir {
        filters.insert(subpath_filter(runtime_temp_dir));
    }

    match policy
        .workspace_access
        .as_ref()
        .map(|wa| wa.mode)
        .unwrap_or(WorkspaceAccessMode::Full)
    {
        WorkspaceAccessMode::Full => {
            filters.insert(subpath_filter(&context.workspace_dir));
        }
        WorkspaceAccessMode::ReadOnly => {
            // No workspace writes; only system temps and run dirs allowed above.
        }
        WorkspaceAccessMode::Custom => {
            // Unreachable: `build_sandbox_plan_for_backend` rejects custom mode
            // before dispatching to a backend-specific builder.
            return Err(format!(
                "{FILESYSTEM_CUSTOM_MODE_UNSUPPORTED}: workspace_access.mode 'custom' is not supported"
            ));
        }
    }

    for pattern in &policy.write_allow_paths {
        let expanded = expand_external_path(pattern)
            .map_err(|e| format!("cannot expand external write allow-path '{pattern}': {e}"))?;
        filters.insert(subpath_filter(&expanded));
    }

    Ok(filters.into_iter().collect())
}

fn render_macos_filters(filters: &[String]) -> String {
    filters
        .iter()
        .map(|filter| format!("    {filter}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn subpath_filter(path: &Path) -> String {
    format!(
        "(subpath \"{}\")",
        escape_spl_string(&path.display().to_string())
    )
}

fn literal_filter(path: &Path) -> String {
    format!(
        "(literal \"{}\")",
        escape_spl_string(&path.display().to_string())
    )
}

/// Escapes a string for use inside a Sandbox Profile Language (Scheme) string
/// literal. Backslashes and double quotes are doubled so the SPL parser
/// preserves them literally, ensuring regex backslash sequences like `\.` are
/// not misinterpreted by the Scheme reader.
fn escape_spl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Linux bwrap implementation
// ---------------------------------------------------------------------------

/// Build a SandboxPlan using Linux bwrap.
///
/// The Linux backend denies outside-workspace access by only bind-mounting the
/// explicit runtime roots. `full` and `read_only` workspace access are
/// supported directly. `custom` mode is rejected by `build_sandbox_plan_for_backend`
/// before this builder runs.
fn build_bwrap_plan(
    context: &CanonicalSandboxContext,
    policy: &FilesystemPolicy,
    network_policy: &NetworkPolicy,
    runtime_mount_policy: RuntimeMountPolicy,
) -> Result<SandboxPlan, String> {
    // Resolve once, then validate and mount these same source/destination pairs.
    // In particular, never re-resolve a host symlink after checking its target.
    let read_grants = policy
        .read_allow_paths
        .iter()
        .map(|path| ExternalGrant::resolve(path))
        .collect::<Result<Vec<_>, _>>()?;
    let write_grants = policy
        .write_allow_paths
        .iter()
        .map(|path| ExternalGrant::resolve(path))
        .collect::<Result<Vec<_>, _>>()?;
    for grant in read_grants.iter().chain(&write_grants) {
        validate_grant_against_workspace(grant, &context.workspace_dir)?;
        validate_grant_against_protected_roots(grant, context)?;
    }
    validate_runtime_temp_isolation(context, policy)?;
    if let Some(staging_dir) = &context.staging_dir {
        ensure_bwrap_staging_mount_dir(staging_dir)?;
    } else {
        for (path, label) in [
            (&context.artifacts_dir, "artifacts_dir"),
            (&context.evidence_dir, "evidence_dir"),
        ] {
            match runtime_mount_policy {
                RuntimeMountPolicy::Create => ensure_bwrap_runtime_mount_dir(path, label, None)?,
                RuntimeMountPolicy::ValidateOnly => validate_bwrap_runtime_mount_dir(path, label)?,
            }
        }
    }

    let mut granted_homes = BTreeSet::new();
    if policy
        .read_allow_paths
        .iter()
        .any(|path| path.starts_with("~/"))
    {
        let home = std::env::var_os("HOME").ok_or("operator HOME is not set")?;
        let lexical = normalize_absolute_grant_path(Path::new(&home))?;
        granted_homes.insert(canonicalize_required_path(&lexical, "operator home")?);
        granted_homes.insert(lexical);
    }
    let mut bwrap_args = vec![
        "--die-with-parent".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ];
    // Home-relative grants need a read-only parent, not a read-only bind of
    // the entire real home (which could reveal runtime state and other files).
    // Populate an empty mount with only the explicitly authorized child mounts.
    for home in &granted_homes {
        bwrap_args.extend(["--tmpfs".to_string(), home.display().to_string()]);
    }
    let launcher_network_mode = match network_policy.mode {
        NetworkAccessMode::None => Some(LinuxNetworkMode::None),
        NetworkAccessMode::Loopback => Some(LinuxNetworkMode::Loopback),
        NetworkAccessMode::Any | NetworkAccessMode::Allowlist => None,
    };
    if launcher_network_mode.is_some() {
        bwrap_args.push("--unshare-net".to_string());
        // Bubblewrap may raise `lo` itself. Both strict modes run the internal
        // launcher as namespace root to explicitly set `lo` up or down
        // (`CAP_NET_ADMIN` inside the fresh namespace), then drop the capability
        // bounding set (`CAP_SETPCAP`) and switch to the operator credentials
        // (`CAP_SETGID` and `CAP_SETUID`) before execing the plugin. Create a
        // user namespace so the launcher starts as uid 0.
        bwrap_args.push("--unshare-user".to_string());
        // Map the invoking operator to namespace uid/gid 0 so capabilities
        // survive exec of the trusted launcher for ordinary non-root operators.
        // This is not host root: bwrap maps only the caller's host credentials.
        bwrap_args.extend(["--uid", "0", "--gid", "0", "--cap-drop", "ALL"].map(str::to_string));
        for capability in ["CAP_NET_ADMIN", "CAP_SETPCAP", "CAP_SETGID", "CAP_SETUID"] {
            bwrap_args.push("--cap-add".to_string());
            bwrap_args.push(capability.to_string());
        }
    }

    for path in LINUX_SYSTEM_BIND_PATHS {
        if Path::new(path).exists() {
            bwrap_args.push("--ro-bind".to_string());
            bwrap_args.push((*path).to_string());
            bwrap_args.push((*path).to_string());
        }
    }

    bind_same_path(&mut bwrap_args, &context.plugin_dir, true);
    if let Some(staging_dir) = &context.staging_dir {
        bind_same_path(&mut bwrap_args, staging_dir, false);
    } else {
        bind_same_path(&mut bwrap_args, &context.artifacts_dir, false);
        bind_same_path(&mut bwrap_args, &context.evidence_dir, false);
    }
    if let Some(runtime_temp_dir) = &context.runtime_temp_dir {
        let visible_path = runtime_temp_visible_root(runtime_temp_dir)
            .map_err(|error| format!("invalid runtime temporary root: {error}"))?;
        bind_path(&mut bwrap_args, runtime_temp_dir, &visible_path, false);
        let read_root = runtime_temp_dir.join("documents/read");
        if read_root.is_dir() {
            // Pin the documents ancestor itself as a mount point, so renaming
            // a writable ancestor cannot invalidate the protected input root.
            bind_path(
                &mut bwrap_args,
                &runtime_temp_dir.join("documents"),
                &visible_path.join("documents"),
                true,
            );
            bind_path(
                &mut bwrap_args,
                &runtime_temp_dir.join("documents/write"),
                &visible_path.join("documents/write"),
                false,
            );
        }
    }
    if let Some(recipe_source_dir) = &context.recipe_source_dir
        && source_directory_is_safe(context, recipe_source_dir)
    {
        bind_same_path(&mut bwrap_args, recipe_source_dir, true);
    }

    match policy
        .workspace_access
        .as_ref()
        .map(|wa| wa.mode)
        .unwrap_or(WorkspaceAccessMode::Full)
    {
        WorkspaceAccessMode::Full => bind_same_path(&mut bwrap_args, &context.workspace_dir, false),
        WorkspaceAccessMode::ReadOnly => {
            bind_same_path(&mut bwrap_args, &context.workspace_dir, true)
        }
        WorkspaceAccessMode::Custom => {
            return Err(format!(
                "{FILESYSTEM_CUSTOM_MODE_UNSUPPORTED}: workspace_access.mode 'custom' is not supported"
            ));
        }
    }

    // External read paths outside workspace_root. A missing target is a policy
    // error: silently omitting the mount would let a grant claim coverage for a
    // path that bwrap never actually binds, so the grant is rejected instead.
    //
    // When a grant is home-relative (~/…), binding the specific file alone is
    // not enough: bwrap auto-creates the intermediate parent directories as a
    // *writable* tmpfs, so `${HOME}` becomes writable inside the sandbox and
    // the "no writes outside the workspace" invariant breaks (the external-read
    // demo depends on this). Populate only granted children of the empty home
    // mount, then make that parent read-only after all child mounts exist.
    for grant in &read_grants {
        if !grant.source.exists() {
            return Err(format!(
                "external read allow-path '{}' target '{}' does not exist",
                grant.pattern,
                grant.source.display()
            ));
        }
        grant.mount(&mut bwrap_args, true);
    }
    // External write paths outside workspace_root.
    for grant in &write_grants {
        if !grant.source.exists() {
            return Err(format!(
                "external write allow-path '{}' target '{}' does not exist",
                grant.pattern,
                grant.source.display()
            ));
        }
        grant.mount(&mut bwrap_args, false);
    }
    // Explicit write-grant mounts remain writable; their previously absent
    // mountpoints must be created before the empty parent becomes read-only.
    for home in granted_homes.iter().rev() {
        bwrap_args.extend(["--remount-ro".to_string(), home.display().to_string()]);
    }

    Ok(SandboxPlan {
        backend: Backend::Bwrap,
        workspace_dir: context.workspace_dir.clone(),
        policy: policy.clone(),
        profile_body: None,
        bwrap_args,
        guard_dir: None,
        launcher_network_mode,
        preflight_concurrency_eligible: policy
            .workspace_access
            .as_ref()
            .is_some_and(|access| access.mode == WorkspaceAccessMode::ReadOnly)
            && policy.write_allow_paths.is_empty()
            && network_policy.mode == NetworkAccessMode::None
            && context.staging_dir.is_some()
            && context.runtime_temp_dir.is_some(),
    })
}

fn ensure_bwrap_runtime_mount_dir(
    path: &Path,
    label: &str,
    required_parent: Option<&Path>,
) -> Result<(), String> {
    if let Some(parent) = required_parent
        && !path.starts_with(parent)
    {
        return Err(format!(
            "{label} '{}' must be inside run_root '{}' for bwrap mount",
            path.display(),
            parent.display()
        ));
    }
    std::fs::create_dir_all(path).map_err(|error| {
        format!(
            "failed creating {label} '{}' for bwrap mount: {error}",
            path.display()
        )
    })
}

fn validate_bwrap_runtime_mount_dir(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!("{label} '{}' is not a directory", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed inspecting {label} '{}' for bwrap mount: {error}",
            path.display()
        )),
    }
}

fn ensure_bwrap_staging_mount_dir(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!(
            "staging_dir '{}' is not an existing directory",
            path.display()
        ));
    }
    Ok(())
}

fn bind_same_path(args: &mut Vec<String>, path: &Path, read_only: bool) {
    bind_path(args, path, path, read_only);
}

fn bind_path(args: &mut Vec<String>, source: &Path, destination: &Path, read_only: bool) {
    args.push(if read_only { "--ro-bind" } else { "--bind" }.to_string());
    args.push(source.display().to_string());
    args.push(destination.display().to_string());
}

fn contains_control_chars(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Expands an external path pattern to an absolute `PathBuf`.
///
/// - `~/…` is expanded using the `HOME` environment variable.
/// - Absolute paths (`/…`) are used as-is.
/// - Relative paths are rejected; external grants must be absolute or
///   home-relative.
fn expand_external_path(pattern: &str) -> Result<PathBuf, String> {
    if pattern == "~" {
        return Err("bare '~' is not supported; use the '~/' prefix".to_string());
    }
    if pattern.starts_with('~') && !pattern.starts_with("~/") {
        return Err(format!(
            "external pattern '{pattern}' uses ~user/ syntax which is not supported"
        ));
    }
    if !pattern.starts_with('/') && !pattern.starts_with("~/") {
        return Err(format!(
            "external pattern '{pattern}' must be absolute (/) or home-relative (~/)"
        ));
    }
    canonical_external_policy_path(pattern, true)
        .map_err(|_| format!("external pattern '{pattern}' is not an unambiguous path"))
}

/// One authorized resource and its exact lexical alias; ancestors are only
/// validation witnesses, never mounts or implicit grants.
struct ExternalGrant {
    pattern: String,
    source: PathBuf,
    destination: PathBuf,
    check_paths: BTreeSet<PathBuf>,
}

impl ExternalGrant {
    /// Captures canonical source and normalized lexical destination from a
    /// policy path. Resolves existing prefixes for private-root/workspace checks.
    fn resolve(pattern: &str) -> Result<Self, String> {
        let lexical = if let Some(relative) = pattern.strip_prefix("~/") {
            PathBuf::from(std::env::var_os("HOME").ok_or("operator HOME is not set")?)
                .join(relative)
        } else {
            PathBuf::from(pattern)
        };
        let destination = normalize_absolute_grant_path(&lexical)?;
        let source = expand_external_path(pattern)?;
        let mut check_paths = BTreeSet::from([source.clone(), destination.clone()]);
        // An alias can escape a private home through a later symlink. Capture
        // each resolved prefix + unresolved suffix, so that location is checked
        // even when the final canonical source lies outside the private root.
        for ancestor in destination.ancestors().skip(1) {
            if let Ok(prefix) = ancestor.canonicalize() {
                check_paths
                    .insert(prefix.join(destination.strip_prefix(ancestor).expect("ancestor")));
            }
        }
        Ok(Self {
            pattern: pattern.into(),
            source,
            destination,
            check_paths,
        })
    }

    /// Adds only the exact captured resource at its canonical and lexical paths.
    /// Does not copy or mutate host symlinks, nor bind any ancestor directory.
    fn mount(&self, args: &mut Vec<String>, read_only: bool) {
        bind_path(args, &self.source, &self.source, read_only);
        if self.destination != self.source {
            bind_path(args, &self.source, &self.destination, read_only);
        }
    }
}

/// Normalizes an absolute alias without following symlinks. Parent traversal and
/// control bytes are rejected rather than canonicalized away into a mount path.
fn normalize_absolute_grant_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || contains_control_chars(&path.to_string_lossy()) {
        return Err("external alias must be an absolute path without control characters".into());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err("external alias must not contain '..' traversal".into());
            }
            std::path::Component::CurDir => {}
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Validates an external path pattern before it is expanded.
///
/// - Must be non-empty.
/// - Must contain no control characters or NUL bytes.
/// - Must start with `/` or `~/`.
/// - Must not contain `..` path traversal in any component.
fn validate_external_path(pattern: &str) -> Result<(), String> {
    if pattern.trim().is_empty() {
        return Err("external allow-path must be non-empty".to_string());
    }
    if pattern != pattern.trim() {
        return Err(format!(
            "external allow-path '{}' contains surrounding whitespace",
            pattern.escape_default()
        ));
    }
    if pattern.contains('\0') {
        return Err(format!(
            "external allow-path '{pattern}' contains NUL and cannot be enforced"
        ));
    }
    if contains_control_chars(pattern) {
        return Err(format!(
            "external allow-path '{}' contains control characters and cannot be enforced",
            pattern.escape_default()
        ));
    }
    let expanded = ExternalGrant::resolve(pattern)?.source;
    if expanded
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "external allow-path '{pattern}' must not contain '..' traversal"
        ));
    }
    Ok(())
}

/// Filesystem policy reason code used when `workspace_access.mode: custom` is
/// rejected. The same code is surfaced by the compiler, preflight, and both
/// sandbox backends so the denial is identical on Linux and macOS.
pub(crate) const FILESYSTEM_CUSTOM_MODE_UNSUPPORTED: &str = "filesystem_custom_mode_unsupported";

/// Returns true when the workspace access mode is unsupported and must be
/// rejected with [`FILESYSTEM_CUSTOM_MODE_UNSUPPORTED`].
fn workspace_mode_unsupported(policy: &FilesystemPolicy) -> bool {
    matches!(
        policy.workspace_access.as_ref().map(|wa| wa.mode),
        Some(WorkspaceAccessMode::Custom)
    )
}

fn validate_workspace_policy(policy: &FilesystemPolicy) -> Result<(), String> {
    if workspace_mode_unsupported(policy) {
        return Err(format!(
            "{FILESYSTEM_CUSTOM_MODE_UNSUPPORTED}: workspace_access.mode 'custom' is not supported"
        ));
    }
    for pattern in &policy.read_allow_paths {
        validate_external_path(pattern)?;
    }
    for pattern in &policy.write_allow_paths {
        validate_external_path(pattern)?;
    }
    Ok(())
}

/// Validates that expanded external grant paths do not overlap the canonical
/// workspace directory. A grant that points at the workspace, an ancestor of
/// the workspace, or a path inside the workspace would let a `read_only`
/// workspace be reopened writable, defeating the workspace write boundary.
fn validate_external_grants_against_workspace(
    policy: &FilesystemPolicy,
    canonical_workspace: &Path,
) -> Result<(), String> {
    for pattern in policy
        .read_allow_paths
        .iter()
        .chain(policy.write_allow_paths.iter())
    {
        validate_grant_against_workspace(&ExternalGrant::resolve(pattern)?, canonical_workspace)?;
    }
    Ok(())
}

/// Rejects any captured source or alias location overlapping the workspace.
fn validate_grant_against_workspace(
    grant: &ExternalGrant,
    canonical_workspace: &Path,
) -> Result<(), String> {
    for path in &grant.check_paths {
        if paths_overlap(path, canonical_workspace) {
            return Err(format!(
                "external allow-path '{}' ('{}') overlaps the workspace ('{}') and cannot be granted",
                grant.pattern,
                path.display(),
                canonical_workspace.display()
            ));
        }
    }
    Ok(())
}

/// Rejects external grants that overlap runtime-owned roots.
///
/// A grant cannot be used to reopen the Transist home, durable run state, trust
/// material, or the shared invocation-staging parent. The current plugin,
/// workspace, and invocation staging roots are reopened explicitly by the
/// sandbox plan instead.
fn validate_external_grants_against_protected_roots(
    policy: &FilesystemPolicy,
    context: &CanonicalSandboxContext,
) -> Result<(), String> {
    for pattern in policy
        .read_allow_paths
        .iter()
        .chain(&policy.write_allow_paths)
    {
        validate_grant_against_protected_roots(&ExternalGrant::resolve(pattern)?, context)?;
    }
    Ok(())
}

/// Rejects canonical resources and lexical alias locations in known private
/// roots, including paths entering through a symlinked ancestor before escaping.
fn validate_grant_against_protected_roots(
    grant: &ExternalGrant,
    context: &CanonicalSandboxContext,
) -> Result<(), String> {
    let staging_parent = context
        .home_dir
        .as_ref()
        .map(|home_dir| home_dir.join(INVOCATION_STAGING_DIR));
    let runtime_temp_parent = runtime_temp_parent();
    let mut protected_roots = Vec::new();
    if let Some(home_dir) = &context.home_dir {
        protected_roots.push(home_dir.as_path());
    }
    if let Some(staging_parent) = &staging_parent {
        protected_roots.push(staging_parent.as_path());
    }
    for canonical_grant in &grant.check_paths {
        // Exact unrelated children of /tmp are ordinary user resources. Deny
        // the shared parent/ancestors and all reserved invocation-temp children,
        // not every user file that happens to live beneath the OS temp parent.
        let reserved_temporary = canonical_grant
            .strip_prefix(&runtime_temp_parent)
            .ok()
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| {
                let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
                name.starts_with(RUNTIME_TEMP_PREFIX)
                    || name.starts_with(&format!(".{RUNTIME_TEMP_PREFIX}"))
                    || name.starts_with(".transist-runtime-owner-init-")
            });
        if runtime_temp_parent.starts_with(canonical_grant) || reserved_temporary {
            return Err(format!(
                "external allow-path '{}' overlaps protected runtime root '{}' and cannot be granted",
                grant.pattern,
                runtime_temp_parent.display()
            ));
        }
        if let Some(protected_root) = protected_roots
            .iter()
            .find(|root| paths_overlap(root, canonical_grant))
        {
            return Err(format!(
                "external allow-path '{}' ('{}') overlaps protected runtime root '{}' and cannot be granted",
                grant.pattern,
                canonical_grant.display(),
                protected_root.display()
            ));
        }
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

/// Wraps a command program + args with the sandbox wrapper.
///
/// Builds a new `Command` that runs the original program under the sandbox.
pub(crate) fn make_sandboxed_command(
    program: &Path,
    args: &[String],
    plan: &SandboxPlan,
) -> Result<Command, String> {
    match plan.backend {
        Backend::SandboxExec => make_sandbox_exec_command(program, args, plan),
        Backend::Bwrap => make_bwrap_command(program, args, plan),
    }
}

fn make_sandbox_exec_command(
    program: &Path,
    args: &[String],
    plan: &SandboxPlan,
) -> Result<Command, String> {
    let profile_body = plan
        .profile_body
        .as_ref()
        .ok_or("sandbox-exec plan missing profile body")?;
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p");
    cmd.arg(profile_body);
    cmd.arg("--");
    cmd.arg(program);
    for arg in args {
        cmd.arg(arg);
    }
    Ok(cmd)
}

fn make_bwrap_command(
    program: &Path,
    args: &[String],
    plan: &SandboxPlan,
) -> Result<Command, String> {
    let mut cmd = Command::new("bwrap");
    for arg in &plan.bwrap_args {
        cmd.arg(arg);
    }
    // Strict modes explicitly configure loopback in the fresh namespace before
    // dropping all capabilities and executing the plugin. The launcher
    // binary is bind-mounted so it is reachable inside the namespace.
    let launcher = if plan.launcher_network_mode.is_some() {
        Some(resolve_network_launcher()?)
    } else {
        None
    };
    if let Some(launcher) = &launcher {
        let launcher_in_sandbox = PathBuf::from(BWRAP_NETWORK_LAUNCHER_PATH);
        cmd.arg("--ro-bind");
        cmd.arg(&launcher.launcher_path);
        cmd.arg(&launcher_in_sandbox);
    }
    cmd.arg("--");
    if let (Some(launcher), Some(mode)) = (&launcher, plan.launcher_network_mode) {
        cmd.arg(BWRAP_NETWORK_LAUNCHER_PATH);
        cmd.arg("--network-mode");
        cmd.arg(mode.argument());
        cmd.arg(launcher.target_uid.to_string());
        cmd.arg(launcher.target_gid.to_string());
    }
    cmd.arg(program);
    for arg in args {
        cmd.arg(arg);
    }
    Ok(cmd)
}

/// Resolves the internal network launcher path and target credentials for the
/// current process.
///
/// The launcher ships next to the runtime binary inside the release archive
/// (`bin/transist-sandbox-launcher`). It can also be overridden with the
/// `TRANSIST_SANDBOX_LAUNCHER` environment variable. When it cannot be found,
/// command construction fails closed because strict network modes cannot be enforced.
///
/// Strict plans map the operator's host credentials to namespace uid/gid 0.
/// The launcher hands off those mapped credentials with all capability sets
/// cleared and no-new-privileges enabled; no host-root identity is granted.
fn resolve_network_launcher() -> Result<NetworkLauncherSpec, String> {
    let launcher_path = if let Some(override_path) = std::env::var_os("TRANSIST_SANDBOX_LAUNCHER") {
        let path = PathBuf::from(&override_path);
        if path.is_file() {
            path
        } else {
            return Err(format!(
                "TRANSIST_SANDBOX_LAUNCHER points at a missing file: {}",
                path.display()
            ));
        }
    } else if let Ok(current_exe) = std::env::current_exe() {
        let sibling = current_exe.with_file_name("transist-sandbox-launcher");
        if sibling.is_file() {
            sibling
        } else {
            find_system_launcher()?
        }
    } else {
        find_system_launcher()?
    };
    let target_uid = 0;
    let target_gid = 0;
    Ok(NetworkLauncherSpec {
        launcher_path,
        target_uid,
        target_gid,
    })
}

fn find_system_launcher() -> Result<PathBuf, String> {
    for candidate in [
        "/usr/libexec/transist/transist-sandbox-launcher",
        "/usr/local/libexec/transist/transist-sandbox-launcher",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(
        "strict network sandboxing requires the internal launcher 'transist-sandbox-launcher' next to the runtime binary or in /usr/libexec/transist; set TRANSIST_SANDBOX_LAUNCHER to override".to_string(),
    )
}

#[cfg(test)]
#[path = "filesystem_sandbox_document_review.rs"]
mod document_review;

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{AdapterKind, ExecutionRequest, InvokePaths, InvokeStep};

    // Ordinary fixtures stay crate-local. Runtime-temporary boundary fixtures
    // explicitly use runtime_temp_parent(), independent of checkout placement.

    fn policy(mode: WorkspaceAccessMode) -> FilesystemPolicy {
        FilesystemPolicy {
            workspace_access: Some(transist_protocol::WorkspaceAccessPolicy { mode }),
            read_allow_paths: Vec::new(),
            write_allow_paths: Vec::new(),
        }
    }

    fn request_with_policy(policy: serde_json::Value) -> ExecutionRequest {
        ExecutionRequest {
            message_type: "tep.invoke.request.v1".to_string(),
            protocol_version: "tep.v1".to_string(),
            invoke_id: "invoke-1".to_string(),
            run_id: "run-1".to_string(),
            workflow_id: "workflow-1".to_string(),
            step: InvokeStep {
                id: "step-1".to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "exec".to_string(),
            input: serde_json::json!({}),
            paths: InvokePaths {
                run_root: "/tmp/run".to_string(),
                artifacts_dir: "/tmp/run/artifacts".to_string(),
                evidence_dir: "/tmp/run/evidence".to_string(),
                workspace_dir: "/tmp/workspace".to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: None,
            max_timeout_ms: None,
            policy,
            metadata: serde_json::json!({}),
        }
    }

    fn network_policy(mode: NetworkAccessMode) -> NetworkPolicy {
        NetworkPolicy {
            mode,
            allow_hosts: Vec::new(),
            allow_ports: Vec::new(),
            tls_required: false,
            max_redirects: 0,
        }
    }

    #[test]
    fn detects_strict_filesystem_policy_requests() {
        let request = request_with_policy(serde_json::json!({
            "filesystem": {
                "workspace_access": {
                    "mode": "full"
                }
            }
        }));
        assert!(strict_filesystem_policy_requested(&request));
    }

    #[test]
    fn ignores_missing_filesystem_policy_requests() {
        let request = request_with_policy(serde_json::json!({
            "execution_kind": "external"
        }));
        assert!(!strict_filesystem_policy_requested(&request));
    }

    #[test]
    fn detects_strict_network_policy_requests() {
        let request = request_with_policy(serde_json::json!({
            "network": {
                "mode": "loopback"
            }
        }));
        assert!(strict_local_policy_requested(&request));
    }

    #[test]
    fn network_mode_any_still_requests_local_sandbox() {
        let request = request_with_policy(serde_json::json!({
            "network": {
                "mode": "any"
            }
        }));
        assert!(strict_local_policy_requested(&request));
    }

    #[test]
    fn network_mode_allowlist_still_requests_local_sandbox() {
        let request = request_with_policy(serde_json::json!({
            "network": {
                "mode": "allowlist",
                "allow_hosts": ["api.example.com"],
                "allow_ports": [443]
            }
        }));
        assert!(strict_local_policy_requested(&request));
    }

    #[test]
    fn filesystem_policy_from_request_parses_full_policy() {
        let request = request_with_policy(serde_json::json!({
            "filesystem": {
                "workspace_access": {
                    "mode": "full"
                }
            }
        }));
        let policy = filesystem_policy_from_request(&request)
            .expect("parse policy")
            .expect("policy");
        assert_eq!(
            policy.workspace_access.as_ref().unwrap().mode,
            WorkspaceAccessMode::Full
        );
    }

    #[test]
    fn filesystem_policy_from_request_accepts_missing_workspace_access() {
        let request = request_with_policy(serde_json::json!({
            "filesystem": { "source": "builtin" }
        }));
        let policy = filesystem_policy_from_request(&request)
            .expect("parse policy")
            .expect("policy");
        assert!(policy.workspace_access.is_none());
    }

    #[test]
    fn filesystem_policy_from_request_rejects_invalid_workspace_access() {
        let request = request_with_policy(serde_json::json!({
            "filesystem": { "workspace_access": "full" }
        }));
        let err = filesystem_policy_from_request(&request).expect_err("invalid policy");
        assert!(err.contains("invalid filesystem.workspace_access"));
    }

    #[test]
    fn filesystem_policy_from_request_rejects_custom_mode() {
        let request = request_with_policy(serde_json::json!({
            "filesystem": {
                "workspace_access": {
                    "mode": "custom"
                }
            }
        }));
        let err = filesystem_policy_from_request(&request).expect_err("custom unsupported");
        assert!(err.contains(FILESYSTEM_CUSTOM_MODE_UNSUPPORTED));
    }

    #[test]
    fn network_policy_from_request_parses_policy() {
        let request = request_with_policy(serde_json::json!({
            "network": {
                "mode": "allowlist",
                "allow_hosts": ["api.example.com"],
                "allow_ports": [443],
                "tls_required": true,
                "max_redirects": 5
            }
        }));
        let policy = network_policy_from_request(&request)
            .expect("parse policy")
            .expect("policy");
        assert_eq!(policy.mode, NetworkAccessMode::Allowlist);
        assert_eq!(policy.allow_hosts, vec!["api.example.com"]);
        assert_eq!(policy.allow_ports, vec![443]);
        assert!(policy.tls_required);
        assert_eq!(policy.max_redirects, 5);
    }

    #[cfg(target_os = "linux")]
    fn require_bwrap_backend(test_name: &str) {
        assert_eq!(
            find_backend(),
            Some(Backend::Bwrap),
            "{test_name}: bwrap missing"
        );
        let output = Command::new("bwrap")
            .args([
                "--die-with-parent",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/tmp",
                "--ro-bind",
                "/",
                "/",
                "/bin/true",
            ])
            .output()
            .unwrap_or_else(|error| panic!("{test_name}: failed to run bwrap probe: {error}"));
        assert!(
            output.status.success(),
            "{test_name}: bwrap backend unusable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_plan_rejects_bwrap_custom_mode() {
        require_bwrap_backend("build_plan_rejects_bwrap_custom_mode");
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        let err = build_sandbox_plan(
            &policy(WorkspaceAccessMode::Custom),
            &NetworkPolicy::default(),
            &context,
        )
        .expect_err("custom mode unsupported on bwrap");
        assert!(err.contains(FILESYSTEM_CUSTOM_MODE_UNSUPPORTED));
    }

    #[test]
    fn build_sandbox_plan_for_backend_rejects_custom_mode_uniformly() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        let err = build_sandbox_plan_for_backend(
            find_backend(),
            &policy(WorkspaceAccessMode::Custom),
            &NetworkPolicy::default(),
            &context,
        )
        .expect_err("custom mode must be rejected before backend dispatch");
        assert!(err.contains(FILESYSTEM_CUSTOM_MODE_UNSUPPORTED));
    }

    #[test]
    fn build_sandbox_plan_rejects_external_grant_overlapping_workspace() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace.clone(),
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        // A write grant pointing at the workspace itself must be rejected.
        let workspace_policy = FilesystemPolicy {
            workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                mode: WorkspaceAccessMode::ReadOnly,
            }),
            read_allow_paths: Vec::new(),
            write_allow_paths: vec![workspace.display().to_string()],
        };
        let err = build_sandbox_plan_for_backend(
            find_backend(),
            &workspace_policy,
            &NetworkPolicy::default(),
            &context,
        )
        .expect_err("external grant overlapping workspace must be rejected");
        assert!(err.contains("overlaps the workspace"));
    }

    #[test]
    fn build_plan_rejects_external_grant_overlapping_runtime_home() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let home_dir = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&home_dir).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: Some(home_dir.clone()),
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        let runtime_policy = FilesystemPolicy {
            workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                mode: WorkspaceAccessMode::Full,
            }),
            read_allow_paths: vec![home_dir.join("settings.yaml").display().to_string()],
            write_allow_paths: Vec::new(),
        };
        let err = build_sandbox_plan_for_backend(
            Some(Backend::SandboxExec),
            &runtime_policy,
            &NetworkPolicy::default(),
            &context,
        )
        .expect_err("runtime-home grant must be rejected");
        assert!(err.contains("protected runtime root"));
    }

    #[test]
    fn build_plan_rejects_external_grant_overlapping_runtime_temp_parent() {
        // Exercise the protected-root validator directly: a checkout itself may
        // live under /tmp, where the separate workspace guard also rejects it.
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("workspace fixture");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        let canonical = CanonicalSandboxContext::new(
            &context,
            std::fs::canonicalize(&context.workspace_dir).unwrap(),
        )
        .unwrap();
        for grant in [
            runtime_temp_parent(),
            runtime_temp_parent().join(format!("{RUNTIME_TEMP_PREFIX}other-invocation/documents")),
            runtime_temp_parent().join(format!(".{RUNTIME_TEMP_PREFIX}other-invocation.owner")),
            runtime_temp_parent().join(".transist-runtime-owner-init-other-invocation"),
            runtime_temp_parent().join("TRANSIST-RUNTIME-TEMP-OTHER-INVOCATION/documents"),
        ] {
            for write in [false, true] {
                let mut policy = policy(WorkspaceAccessMode::Full);
                if write {
                    policy.write_allow_paths.push(grant.display().to_string());
                } else {
                    policy.read_allow_paths.push(grant.display().to_string());
                }
                let err = validate_external_grants_against_protected_roots(&policy, &canonical)
                    .expect_err("runtime temporary parent grant must be rejected");
                assert!(err.contains("protected runtime root"), "{err}");
            }
        }
        let ordinary = tempfile::Builder::new()
            .prefix("transist-user-data-")
            .tempdir_in(runtime_temp_parent())
            .unwrap();
        let mut allowed = policy(WorkspaceAccessMode::Full);
        allowed
            .read_allow_paths
            .push(ordinary.path().display().to_string());
        allowed
            .write_allow_paths
            .push(ordinary.path().display().to_string());
        validate_external_grants_against_protected_roots(&allowed, &canonical)
            .expect("an exact ordinary user child is not a grant to the temporary parent");
    }

    #[test]
    fn build_sandbox_plan_rejects_external_grant_ancestor_of_workspace() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("nested/workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace.clone(),
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        // A write grant pointing at an ancestor of the workspace (the temp
        // parent) would reopen the workspace writable in read_only mode.
        let ancestor_policy = FilesystemPolicy {
            workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                mode: WorkspaceAccessMode::ReadOnly,
            }),
            read_allow_paths: Vec::new(),
            write_allow_paths: vec![tmp.path().display().to_string()],
        };
        let err = build_sandbox_plan_for_backend(
            find_backend(),
            &ancestor_policy,
            &NetworkPolicy::default(),
            &context,
        )
        .expect_err("ancestor grant must be rejected");
        assert!(err.contains("overlaps the workspace"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_plan_creates_missing_bwrap_runtime_mount_dirs() {
        require_bwrap_backend("build_plan_creates_missing_bwrap_runtime_mount_dirs");
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        let artifacts_dir = run_root
            .join("artifacts")
            .join("step")
            .join("execution_001")
            .join("artifacts");
        let evidence_dir = run_root
            .join("artifacts")
            .join("step")
            .join("execution_001")
            .join("evidence");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&run_root).expect("run root");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");

        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: artifacts_dir.clone(),
            evidence_dir: evidence_dir.clone(),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        build_sandbox_plan(
            &policy(WorkspaceAccessMode::Full),
            &NetworkPolicy::default(),
            &context,
        )
        .expect("bwrap plan");

        assert!(artifacts_dir.is_dir());
        assert!(evidence_dir.is_dir());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_plan_allows_runtime_staging_outside_run_root() {
        require_bwrap_backend("build_plan_allows_runtime_staging_outside_run_root");
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let staging_dir = tmp.path().join(".transist-staging/invoke-test");
        let artifacts_dir = staging_dir.join("artifacts");
        let evidence_dir = staging_dir.join("evidence");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&run_root).expect("run root");
        std::fs::create_dir_all(&artifacts_dir).expect("artifacts");
        std::fs::create_dir_all(&evidence_dir).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");

        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir,
            evidence_dir,
            staging_dir: Some(staging_dir.clone()),
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };
        let plan = build_sandbox_plan(
            &policy(WorkspaceAccessMode::Full),
            &NetworkPolicy::default(),
            &context,
        )
        .expect("bwrap staging plan");

        assert!(plan.bwrap_args.windows(3).any(|args| args
            == [
                "--bind".to_string(),
                staging_dir.display().to_string(),
                staging_dir.display().to_string()
            ]));
    }

    #[test]
    fn strict_network_policy_fails_closed_when_backend_missing() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };

        let err = build_sandbox_plan_for_backend(
            None,
            &policy(WorkspaceAccessMode::Full),
            &network_policy(NetworkAccessMode::None),
            &context,
        )
        .expect_err("missing backend must fail closed");
        assert!(
            err.contains("sandboxing") || err.contains("bwrap") || err.contains("sandbox-exec")
        );
    }

    #[test]
    fn runtime_mount_dir_guard_rejects_paths_outside_run_root() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let run_root = tmp.path().join("run");
        let outside = tmp.path().join("outside");

        let err = ensure_bwrap_runtime_mount_dir(&outside, "artifacts_dir", Some(&run_root))
            .expect_err("outside path rejected");

        assert!(err.contains("must be inside run_root"));
        assert!(!outside.exists());
    }

    #[test]
    fn make_sandboxed_command_with_plan_sets_sandbox_exec() {
        let plan = SandboxPlan {
            backend: Backend::SandboxExec,
            workspace_dir: PathBuf::from("/ws"),
            policy: policy(WorkspaceAccessMode::Full),
            profile_body: Some("(version 3)".to_string()),
            bwrap_args: Vec::new(),
            guard_dir: None,
            launcher_network_mode: None,
            preflight_concurrency_eligible: false,
        };
        let cmd = make_sandboxed_command(Path::new("/bin/echo"), &["hello".to_string()], &plan)
            .expect("command");
        let dbg = format!("{:?}", cmd);
        assert!(
            dbg.contains("sandbox-exec"),
            "expected sandbox-exec in command, got: {}",
            dbg
        );
    }

    #[test]
    fn make_sandboxed_command_with_plan_sets_bwrap() {
        let plan = SandboxPlan {
            backend: Backend::Bwrap,
            workspace_dir: PathBuf::from("/ws"),
            policy: policy(WorkspaceAccessMode::Full),
            profile_body: None,
            bwrap_args: vec![
                "--ro-bind".to_string(),
                "/ws".to_string(),
                "/ws".to_string(),
            ],
            guard_dir: None,
            launcher_network_mode: None,
            preflight_concurrency_eligible: false,
        };
        let cmd = make_sandboxed_command(Path::new("/bin/echo"), &["hello".to_string()], &plan)
            .expect("command");
        let dbg = format!("{:?}", cmd);
        assert!(
            dbg.contains("bwrap"),
            "expected bwrap in command, got: {}",
            dbg
        );
    }

    #[test]
    fn escape_spl_string_doubles_backslash_and_quote() {
        assert_eq!(escape_spl_string(r"\.env"), r"\\.env");
        assert_eq!(escape_spl_string(r#"say "hi""#), r#"say \"hi\""#);
    }

    // -----------------------------------------------------------------------
    // expand_external_path tests
    // -----------------------------------------------------------------------

    #[test]
    fn expand_external_path_absolute_pass_through() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let result = expand_external_path(&tmp.path().display().to_string()).expect("expand");
        assert_eq!(
            result,
            tmp.path().canonicalize().expect("canonical tempdir")
        );
    }

    #[test]
    fn expand_external_path_tilde_slash_expands_home() {
        // SAFETY: single-threaded test; no other threads read HOME concurrently.
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        let result = expand_external_path("~/.gitconfig").expect("expand");
        assert!(result.ends_with("home/testuser/.gitconfig"));
        assert!(result.is_absolute());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn build_plan_denies_all_outbound_including_loopback_for_none_network_policy() {
        assert_eq!(
            find_backend(),
            Some(Backend::SandboxExec),
            "sandbox-exec backend unavailable"
        );
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };

        let plan = build_sandbox_plan(
            &policy(WorkspaceAccessMode::Full),
            &network_policy(NetworkAccessMode::None),
            &context,
        )
        .expect("sandbox plan");
        let profile = plan.profile_body.expect("profile body");
        assert!(profile.contains("(deny network-outbound)"));
        assert!(
            !profile.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
            "network.mode: none must not carry a loopback allowance: {profile}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn build_plan_allows_loopback_only_for_loopback_network_policy() {
        assert_eq!(
            find_backend(),
            Some(Backend::SandboxExec),
            "sandbox-exec backend unavailable"
        );
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };

        let plan = build_sandbox_plan(
            &policy(WorkspaceAccessMode::Full),
            &network_policy(NetworkAccessMode::Loopback),
            &context,
        )
        .expect("sandbox plan");
        let profile = plan.profile_body.expect("profile body");
        assert!(profile.contains("(deny network-outbound)"));
        assert!(profile.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_reopens_only_dev_and_invocation_staging_for_runtime_writes() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        let recipe_source_dir = tmp.path().join("workflow");
        let staging_dir = home.join(".transist-staging/invoke-test");
        let plugin_dir = home.join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&recipe_source_dir).expect("recipe source");
        std::fs::create_dir_all(staging_dir.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(staging_dir.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: Some(home.clone()),
            workspace_dir: workspace,
            artifacts_dir: staging_dir.join("artifacts"),
            evidence_dir: staging_dir.join("evidence"),
            staging_dir: Some(staging_dir.clone()),
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: Some(recipe_source_dir.clone()),
        };

        let plan = build_sandbox_plan(
            &policy(WorkspaceAccessMode::ReadOnly),
            &NetworkPolicy::default(),
            &context,
        )
        .expect("sandbox plan");
        let profile = plan.profile_body.expect("profile body");
        let canonical_staging = std::fs::canonicalize(&staging_dir).expect("canonical staging");
        let canonical_home = std::fs::canonicalize(&home).expect("canonical home");
        assert!(profile.contains("(subpath \"/dev\")"));
        assert!(profile.contains(&format!(
            "(deny file-read*\n    (require-all\n        (subpath \"{}\")",
            canonical_home.display()
        )));
        assert!(profile.contains(&format!(
            "(require-all\n        (subpath \"{}/.transist-staging\")",
            canonical_home.display()
        )));
        assert!(profile.contains("(require-not\n            (require-any"));
        assert!(profile.contains(&format!(
            "(allow file-read-metadata\n    (literal \"{}\"))",
            canonical_home.display()
        )));
        assert!(profile.contains("(allow file-read-metadata\n    (literal \"/private/tmp\"))"));
        assert!(profile.contains(&format!("(subpath \"{}\")", canonical_staging.display())));
        let canonical_recipe_source =
            std::fs::canonicalize(&recipe_source_dir).expect("canonical recipe source");
        assert!(profile.contains(&format!(
            "(subpath \"{}\")",
            canonical_recipe_source.display()
        )));
        assert!(!profile.contains("(subpath \"/tmp\")"));
        assert!(!profile.contains("(subpath \"/var/tmp\")"));
        assert!(!profile.contains("(subpath \"/var/folders\")"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_allows_active_runtime_temp_and_denies_sibling() {
        assert_eq!(
            find_backend(),
            Some(Backend::SandboxExec),
            "sandbox-exec backend unavailable"
        );
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let staging_dir = tmp.path().join(".transist-staging/invoke-test");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(staging_dir.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(staging_dir.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");

        let active = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .expect("active runtime temp");
        let sibling = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .expect("sibling runtime temp");
        let sibling_secret = sibling.path().join("sibling-secret.txt");
        std::fs::write(&sibling_secret, "sibling-secret").expect("sibling sentinel");

        let context = SandboxContext {
            home_dir: Some(tmp.path().to_path_buf()),
            workspace_dir: workspace,
            artifacts_dir: staging_dir.join("artifacts"),
            evidence_dir: staging_dir.join("evidence"),
            staging_dir: Some(staging_dir),
            runtime_temp_dir: Some(active.path().to_path_buf()),
            plugin_dir,
            recipe_source_dir: None,
        };
        let plan = build_sandbox_plan(
            &policy(WorkspaceAccessMode::ReadOnly),
            &NetworkPolicy::default(),
            &context,
        )
        .expect("sandbox plan");
        let active_sentinel = active.path().join("active-sentinel.txt");
        let script = format!(
            "printf active-sentinel > '{}' && test \"$(cat '{}')\" = active-sentinel && ! cat '{}' >/dev/null 2>&1 && ! ls '{}' >/dev/null 2>&1",
            active_sentinel.display(),
            active_sentinel.display(),
            sibling_secret.display(),
            runtime_temp_parent().display(),
        );
        let command =
            make_sandboxed_command(Path::new("/bin/sh"), &["-c".to_string(), script], &plan)
                .expect("sandbox command");
        let output = Command::new(command.get_program())
            .args(command.get_args())
            .output()
            .expect("run sandbox probe");
        assert!(
            output.status.success(),
            "active runtime temp must be readable and writable while its sibling remains unreadable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bwrap_plan_allows_active_runtime_temp_and_denies_sibling() {
        assert_eq!(
            find_backend(),
            Some(Backend::Bwrap),
            "bwrap backend unavailable"
        );
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let staging_dir = tmp.path().join(".transist-staging/invoke-test");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(staging_dir.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(staging_dir.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");

        let active = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .expect("active runtime temp");
        let sibling = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .expect("sibling runtime temp");
        let sibling_secret = sibling.path().join("sibling-secret.txt");
        std::fs::write(&sibling_secret, "sibling-secret").expect("sibling sentinel");

        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: staging_dir.join("artifacts"),
            evidence_dir: staging_dir.join("evidence"),
            staging_dir: Some(staging_dir),
            runtime_temp_dir: Some(active.path().to_path_buf()),
            plugin_dir,
            recipe_source_dir: None,
        };
        let plan = build_sandbox_plan(
            &policy(WorkspaceAccessMode::ReadOnly),
            &NetworkPolicy::default(),
            &context,
        )
        .expect("sandbox plan");
        let active_sentinel = active.path().join("active-sentinel.txt");
        let script = format!(
            "printf active-sentinel > '{}' && test \"$(cat '{}')\" = active-sentinel && ! cat '{}' >/dev/null 2>&1",
            active_sentinel.display(),
            active_sentinel.display(),
            sibling_secret.display(),
        );
        let command =
            make_sandboxed_command(Path::new("/bin/sh"), &["-c".to_string(), script], &plan)
                .expect("sandbox command");
        let output = Command::new(command.get_program())
            .args(command.get_args())
            .output()
            .expect("run sandbox probe");
        assert!(
            output.status.success(),
            "active runtime temp must be readable and writable while its sibling remains unreadable: {}; bwrap args: {:?}",
            String::from_utf8_lossy(&output.stderr),
            plan.bwrap_args
        );
    }

    #[test]
    fn managed_document_sandbox_roots_pin_read_ancestors_and_writable_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let plugin = temp.path().join("plugin");
        let staging = temp.path().join(".transist-staging/invoke-documents");
        for path in [
            &workspace,
            &plugin,
            &staging.join("artifacts"),
            &staging.join("evidence"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let active = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .unwrap();
        std::fs::create_dir_all(active.path().join("documents/read")).unwrap();
        std::fs::create_dir_all(active.path().join("documents/write")).unwrap();
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace.clone(),
            artifacts_dir: staging.join("artifacts"),
            evidence_dir: staging.join("evidence"),
            staging_dir: Some(staging),
            runtime_temp_dir: Some(active.path().to_path_buf()),
            plugin_dir: plugin,
            recipe_source_dir: None,
        };
        let context =
            CanonicalSandboxContext::new(&context, std::fs::canonicalize(workspace).unwrap())
                .unwrap();
        let policy = policy(WorkspaceAccessMode::ReadOnly);
        let network = NetworkPolicy::default();
        let bwrap = build_bwrap_plan(
            &context,
            &policy,
            &network,
            RuntimeMountPolicy::ValidateOnly,
        )
        .unwrap();
        let host = context.runtime_temp_dir.as_ref().unwrap().join("documents");
        let visible = runtime_temp_visible_root(context.runtime_temp_dir.as_ref().unwrap())
            .unwrap()
            .join("documents");
        for (flag, source, target) in [
            ("--ro-bind", host.clone(), visible.clone()),
            ("--bind", host.join("write"), visible.join("write")),
        ] {
            assert!(bwrap.bwrap_args.windows(3).any(|args| args
                == [
                    flag.to_string(),
                    source.display().to_string(),
                    target.display().to_string()
                ]));
        }
        let macos = build_sandbox_exec_plan(&context, &policy, &network).unwrap();
        let profile = macos.profile_body.unwrap();
        assert!(profile.contains("(deny file-write-unlink"));
        assert!(profile.contains(&literal_filter(&host)));
        assert!(profile.contains(&literal_filter(context.runtime_temp_dir.as_ref().unwrap())));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_plan_adds_unshare_net_for_strict_network_policy() {
        require_bwrap_backend("build_plan_adds_unshare_net_for_strict_network_policy");
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let run_root = tmp.path().join("run");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: run_root.join("artifacts"),
            evidence_dir: run_root.join("evidence"),
            staging_dir: None,
            runtime_temp_dir: None,
            plugin_dir,
            recipe_source_dir: None,
        };

        let plan = build_sandbox_plan(
            &policy(WorkspaceAccessMode::Full),
            &network_policy(NetworkAccessMode::Loopback),
            &context,
        )
        .expect("bwrap plan");
        assert!(plan.bwrap_args.iter().any(|arg| arg == "--unshare-net"));
    }

    #[test]
    fn expand_external_path_rejects_relative() {
        let err = expand_external_path("etc/gitconfig").expect_err("relative rejected");
        assert!(err.contains("must be absolute") || err.contains("absolute (/)"));
    }

    #[test]
    fn expand_external_path_rejects_bare_tilde() {
        let err = expand_external_path("~").expect_err("bare tilde rejected");
        assert!(err.contains("bare '~'") || err.contains("not supported"));
    }

    #[test]
    fn expand_external_path_rejects_tilde_user() {
        let err = expand_external_path("~root/foo").expect_err("~user rejected");
        assert!(err.contains("~user/") || err.contains("not supported"));
    }

    // -----------------------------------------------------------------------
    // validate_external_path tests
    // -----------------------------------------------------------------------

    #[test]
    fn validate_external_path_accepts_absolute() {
        assert!(validate_external_path("/etc/gitconfig").is_ok());
    }

    #[test]
    fn validate_external_path_accepts_tilde_slash() {
        // SAFETY: single-threaded test; no other threads read HOME concurrently.
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        assert!(validate_external_path("~/.ssh").is_ok());
    }

    #[test]
    fn validate_external_path_rejects_empty() {
        let err = validate_external_path("").expect_err("empty rejected");
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn validate_external_path_rejects_nul() {
        let err = validate_external_path("/etc/foo\0bar").expect_err("NUL rejected");
        assert!(err.contains("NUL"));
    }

    #[test]
    fn validate_external_path_rejects_control_chars() {
        let err = validate_external_path("/etc/foo\nbar").expect_err("control chars rejected");
        assert!(err.contains("control characters"));
    }

    #[test]
    fn validate_external_path_rejects_traversal() {
        // SAFETY: single-threaded test; no other threads read HOME concurrently.
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        let err = validate_external_path("~/../etc/passwd").expect_err("traversal rejected");
        assert!(err.contains(".."));
    }

    #[test]
    fn validate_external_path_rejects_relative() {
        let err = validate_external_path("relative/path").expect_err("relative rejected");
        assert!(err.contains("absolute") || err.contains("home-relative"));
    }

    #[test]
    fn validate_external_path_rejects_surrounding_whitespace() {
        let err = validate_external_path(" /etc/gitconfig").expect_err("whitespace rejected");
        assert!(err.contains("surrounding whitespace"));
    }

    // -----------------------------------------------------------------------
    // filesystem_policy_from_request parses external paths
    // -----------------------------------------------------------------------

    #[test]
    fn filesystem_policy_from_request_parses_external_paths() {
        let request = request_with_policy(serde_json::json!({
            "filesystem": {
                "workspace_access": { "mode": "full" },
                "read_allow_paths": ["/etc/gitconfig", "~/.gitconfig"],
                "write_allow_paths": ["~/.cache/git"]
            }
        }));
        let policy = filesystem_policy_from_request(&request)
            .expect("parse policy")
            .expect("policy");
        assert_eq!(
            policy.read_allow_paths,
            vec!["/etc/gitconfig", "~/.gitconfig"]
        );
        assert_eq!(policy.write_allow_paths, vec!["~/.cache/git"]);
    }

    #[test]
    fn filesystem_policy_from_request_defaults_external_paths_to_empty() {
        let request = request_with_policy(serde_json::json!({
            "filesystem": {
                "workspace_access": { "mode": "full" }
            }
        }));
        let policy = filesystem_policy_from_request(&request)
            .expect("parse policy")
            .expect("policy");
        assert!(policy.read_allow_paths.is_empty());
        assert!(policy.write_allow_paths.is_empty());
    }

    // -----------------------------------------------------------------------
    // Step 3: missing external grant targets and loopback enforcement
    // -----------------------------------------------------------------------

    fn bwrap_context(
        tmp: &Path,
        network_mode: NetworkAccessMode,
    ) -> (SandboxContext, FilesystemPolicy, NetworkPolicy) {
        let workspace = tmp.join("workspace");
        let run_root = tmp.join("run");
        let plugin_dir = tmp.join("plugin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        (
            SandboxContext {
                home_dir: None,
                workspace_dir: workspace,
                artifacts_dir: run_root.join("artifacts"),
                evidence_dir: run_root.join("evidence"),
                staging_dir: None,
                runtime_temp_dir: None,
                plugin_dir,
                recipe_source_dir: None,
            },
            policy(WorkspaceAccessMode::Full),
            network_policy(network_mode),
        )
    }

    #[test]
    fn preflight_concurrency_eligibility_requires_private_read_only_offline_plan() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let plugin_dir = tmp.path().join("plugin");
        let staging = tmp.path().join(".transist-staging/invoke-test");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&plugin_dir).expect("plugin");
        std::fs::create_dir_all(staging.join("artifacts")).expect("artifacts");
        std::fs::create_dir_all(staging.join("evidence")).expect("evidence");
        let runtime_temp = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .expect("runtime temp");
        let context = SandboxContext {
            home_dir: None,
            workspace_dir: workspace,
            artifacts_dir: staging.join("artifacts"),
            evidence_dir: staging.join("evidence"),
            staging_dir: Some(staging),
            runtime_temp_dir: Some(runtime_temp.path().to_path_buf()),
            plugin_dir,
            recipe_source_dir: None,
        };
        let read_only = policy(WorkspaceAccessMode::ReadOnly);
        let denied_network = network_policy(NetworkAccessMode::None);

        let eligible = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &read_only,
            &denied_network,
            &context,
        )
        .expect("isolated plan");
        assert!(eligible.preflight_concurrency_eligible());

        let full_workspace = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &policy(WorkspaceAccessMode::Full),
            &denied_network,
            &context,
        )
        .expect("full plan");
        assert!(!full_workspace.preflight_concurrency_eligible());

        let networked = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &read_only,
            &network_policy(NetworkAccessMode::Any),
            &context,
        )
        .expect("networked plan");
        assert!(!networked.preflight_concurrency_eligible());

        let shared_write = tmp.path().join("shared-write");
        std::fs::create_dir(&shared_write).expect("shared write");
        let mut writable_host = read_only.clone();
        writable_host.write_allow_paths = vec![shared_write.display().to_string()];
        let writable_host = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &writable_host,
            &denied_network,
            &context,
        )
        .expect("host write plan");
        assert!(!writable_host.preflight_concurrency_eligible());

        let mut shared_staging = context.clone();
        shared_staging.staging_dir = None;
        let shared_staging = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &read_only,
            &denied_network,
            &shared_staging,
        )
        .expect("unproven staging plan");
        assert!(!shared_staging.preflight_concurrency_eligible());

        let mut shared_runtime_temp = context.clone();
        shared_runtime_temp.runtime_temp_dir = None;
        let shared_runtime_temp = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &read_only,
            &denied_network,
            &shared_runtime_temp,
        )
        .expect("unproven runtime temp plan");
        assert!(!shared_runtime_temp.preflight_concurrency_eligible());

        let sandbox_exec = build_sandbox_plan_for_backend(
            Some(Backend::SandboxExec),
            &read_only,
            &denied_network,
            &context,
        )
        .expect("macOS plan");
        assert!(!sandbox_exec.preflight_concurrency_eligible());
    }

    #[test]
    fn bwrap_plan_rejects_missing_external_read_target() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let (context, mut filesystem_policy, network_policy) =
            bwrap_context(tmp.path(), NetworkAccessMode::Any);
        let missing = tmp.path().join("missing-read-target");
        assert!(!missing.exists());
        filesystem_policy.read_allow_paths = vec![missing.display().to_string()];

        let error = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &filesystem_policy,
            &network_policy,
            &context,
        )
        .expect_err("missing external read target must be rejected");
        assert!(error.contains("does not exist"));
    }

    #[test]
    fn bwrap_plan_rejects_missing_external_write_target() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let (context, mut filesystem_policy, network_policy) =
            bwrap_context(tmp.path(), NetworkAccessMode::Any);
        let missing = tmp.path().join("missing-write-target");
        assert!(!missing.exists());
        filesystem_policy.write_allow_paths = vec![missing.display().to_string()];

        let error = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &filesystem_policy,
            &network_policy,
            &context,
        )
        .expect_err("missing external write target must be rejected");
        assert!(error.contains("does not exist"));
    }

    #[test]
    fn bwrap_plan_accepts_existing_external_read_target() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let (context, mut filesystem_policy, network_policy) =
            bwrap_context(tmp.path(), NetworkAccessMode::Any);
        let existing = tmp.path().join("existing-read-target");
        std::fs::write(&existing, b"x").expect("write existing");
        filesystem_policy.read_allow_paths = vec![existing.display().to_string()];

        let plan = build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &filesystem_policy,
            &network_policy,
            &context,
        )
        .expect("existing external target accepted");
        let canonical_existing = existing.canonicalize().expect("canonical external target");
        assert!(plan.bwrap_args.windows(3).any(|args| {
            args[0] == "--ro-bind"
                && args[1] == canonical_existing.display().to_string()
                && args[2] == canonical_existing.display().to_string()
        }));
    }

    #[test]
    fn bwrap_static_validation_does_not_create_future_run_directories() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("fixture");
        let (mut context, filesystem_policy, network_policy) =
            bwrap_context(tmp.path(), NetworkAccessMode::Any);
        let future = tmp.path().join("unpublished-run");
        context.artifacts_dir = future.join("artifacts");
        context.evidence_dir = future.join("evidence");
        build_sandbox_plan_with_mount_policy(
            Some(Backend::Bwrap),
            &filesystem_policy,
            &network_policy,
            &context,
            RuntimeMountPolicy::ValidateOnly,
        )
        .expect("prospective runtime mounts are valid");
        assert!(
            !future.exists(),
            "static evidence must not materialize a run"
        );

        build_sandbox_plan_for_backend(
            Some(Backend::Bwrap),
            &filesystem_policy,
            &network_policy,
            &context,
        )
        .expect("execution plan retains runtime mount preparation");
        assert!(context.artifacts_dir.is_dir());
        assert!(context.evidence_dir.is_dir());
    }

    #[test]
    fn bwrap_static_validation_rejects_non_directory_runtime_mounts() {
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("fixture");
        let (mut context, filesystem_policy, network_policy) =
            bwrap_context(tmp.path(), NetworkAccessMode::Any);
        let artifact_file = tmp.path().join("not-a-directory");
        std::fs::write(&artifact_file, b"preserve").expect("fixture file");
        context.artifacts_dir = artifact_file.clone();
        let error = build_sandbox_plan_with_mount_policy(
            Some(Backend::Bwrap),
            &filesystem_policy,
            &network_policy,
            &context,
            RuntimeMountPolicy::ValidateOnly,
        )
        .expect_err("an existing file cannot become a runtime mount");
        assert!(error.contains("is not a directory"), "{error}");
        assert_eq!(
            std::fs::read(artifact_file).expect("untouched file"),
            b"preserve"
        );
    }

    /// RAII guard that restores (or removes) an environment variable on drop.
    struct EnvGuard {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }
    // All tests that resolve or override the launcher must hold this lock so
    // the wiring fixture cannot substitute its fake launcher into a live probe.
    static LAUNCHER_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: test-only env mutation; cleaned up deterministically via Drop.
            unsafe {
                match &self.old {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    /// Requires the configured, profile-local, or installed sandbox launcher.
    /// Coverage uses a separate target directory while CI installs the matching
    /// launcher in the production system location before running these tests.
    #[cfg(target_os = "linux")]
    fn find_sandbox_launcher_for_test() -> std::path::PathBuf {
        if let Some(path) = std::env::var_os("TRANSIST_SANDBOX_LAUNCHER") {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "configured sandbox launcher missing: {path:?}"
            );
            return path;
        }
        if let Ok(exe) = std::env::current_exe() {
            let sibling = exe.with_file_name("transist-sandbox-launcher");
            if sibling.is_file() {
                return sibling;
            }
            if let Some(profile_dir) = exe.parent().and_then(|p| p.parent()) {
                let candidate = profile_dir.join("transist-sandbox-launcher");
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        find_system_launcher().expect(
            "required transist-sandbox-launcher missing; build/install it or set TRANSIST_SANDBOX_LAUNCHER",
        )
    }

    #[test]
    fn strict_network_modes_wire_internal_launcher_into_command() {
        let _launcher_lock = LAUNCHER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let launcher = tmp.path().join("transist-sandbox-launcher");
        std::fs::write(&launcher, b"#!/bin/sh\nexit 0\n").expect("write launcher");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))
                .expect("chmod launcher");
        }
        // Save the current value and restore it on drop so this test does not
        // leak TRANSIST_SANDBOX_LAUNCHER into concurrently-running tests.
        let _env_guard = EnvGuard {
            key: "TRANSIST_SANDBOX_LAUNCHER",
            old: std::env::var_os("TRANSIST_SANDBOX_LAUNCHER"),
        };
        // SAFETY: launcher tests share a lock; env guard restores the value on drop.
        unsafe { std::env::set_var("TRANSIST_SANDBOX_LAUNCHER", &launcher) };

        for (mode, internal_mode) in [
            (NetworkAccessMode::None, LinuxNetworkMode::None),
            (NetworkAccessMode::Loopback, LinuxNetworkMode::Loopback),
        ] {
            let (context, filesystem_policy, network_policy) = bwrap_context(tmp.path(), mode);
            let plan = build_sandbox_plan_for_backend(
                Some(Backend::Bwrap),
                &filesystem_policy,
                &network_policy,
                &context,
            )
            .expect("strict network plan");
            assert_eq!(plan.launcher_network_mode, Some(internal_mode));
            for flag in ["--die-with-parent", "--unshare-net", "--unshare-user"] {
                assert!(plan.bwrap_args.iter().any(|arg| arg == flag), "{flag}");
            }
            for capability in ["CAP_NET_ADMIN", "CAP_SETPCAP", "CAP_SETGID", "CAP_SETUID"] {
                assert!(
                    plan.bwrap_args
                        .windows(2)
                        .any(|args| args == ["--cap-add", capability])
                );
            }
            let cmd = make_sandboxed_command(Path::new("/bin/echo"), &["hello".to_string()], &plan)
                .expect("sandboxed command");
            let args: Vec<_> = cmd
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(args.windows(3).any(|args| args
                == [
                    "--ro-bind",
                    launcher.to_str().unwrap(),
                    BWRAP_NETWORK_LAUNCHER_PATH
                ]));
            let separator = args.iter().position(|arg| arg == "--").expect("separator");
            let spec = resolve_network_launcher().expect("launcher credentials");
            assert_eq!(
                &args[separator + 1..],
                &[
                    BWRAP_NETWORK_LAUNCHER_PATH.to_string(),
                    "--network-mode".to_string(),
                    internal_mode.argument().to_string(),
                    spec.target_uid.to_string(),
                    spec.target_gid.to_string(),
                    "/bin/echo".to_string(),
                    "hello".to_string(),
                ]
            );
        }
    }

    #[test]
    fn unrestricted_modes_do_not_configure_network_or_require_launcher() {
        let _launcher_lock = LAUNCHER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let _env_guard = EnvGuard {
            key: "TRANSIST_SANDBOX_LAUNCHER",
            old: std::env::var_os("TRANSIST_SANDBOX_LAUNCHER"),
        };
        // SAFETY: launcher tests share a lock and restore the environment.
        unsafe { std::env::set_var("TRANSIST_SANDBOX_LAUNCHER", tmp.path().join("missing")) };
        for mode in [
            NetworkAccessMode::Any,
            NetworkAccessMode::Allowlist,
            NetworkAccessMode::None,
            NetworkAccessMode::Loopback,
        ] {
            let (context, filesystem_policy, network_policy) = bwrap_context(tmp.path(), mode);
            let plan = build_sandbox_plan_for_backend(
                Some(Backend::Bwrap),
                &filesystem_policy,
                &network_policy,
                &context,
            )
            .expect("network plan");
            let command =
                make_sandboxed_command(Path::new("/bin/echo"), &["hello".to_string()], &plan);
            if matches!(mode, NetworkAccessMode::None | NetworkAccessMode::Loopback) {
                assert!(
                    command
                        .expect_err("missing launcher must fail closed")
                        .contains("missing file")
                );
            } else {
                assert_eq!(plan.launcher_network_mode, None);
                for flag in ["--unshare-net", "--unshare-user", "--cap-add"] {
                    assert!(!plan.bwrap_args.iter().any(|arg| arg == flag));
                }
                let command = command.expect("unrestricted command needs no launcher");
                let args: Vec<_> = command.get_args().collect();
                assert_eq!(
                    &args[args.len() - 3..],
                    ["--", "/bin/echo", "hello"].map(std::ffi::OsStr::new)
                );
            }
        }
    }

    // Inspect the executed payload, not launcher state before exec. In addition
    // to flags and connectivity, ensure the plugin cannot raise lo again.
    #[cfg(target_os = "linux")]
    const NETWORK_PROBE_PY: &str = r#"
import fcntl, json, os, socket, struct
def observe_network():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    flags = struct.unpack_from('H', fcntl.ioctl(sock, 0x8913, struct.pack('16sH22x', b'lo', 0)), 16)[0]
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.bind(('127.0.0.1', 0))
    srv.listen(1)
    connected, error = False, None
    try:
        with socket.create_connection(srv.getsockname(), timeout=2):
            connected = True
    except OSError as e:
        error = e.errno
    srv.close()
    capabilities = {}
    with open('/proc/self/status') as status:
        for line in status:
            key, _, value = line.partition(':')
            if key in ('CapInh', 'CapPrm', 'CapEff', 'CapBnd', 'CapAmb', 'NoNewPrivs'):
                capabilities[key] = int(value.strip(), 16)
    raise_errno = None
    try:
        fcntl.ioctl(sock, 0x8914, struct.pack('16sH22x', b'lo', flags | 1))
    except OSError as e:
        raise_errno = e.errno
    sock.close()
    return {'netns': os.readlink('/proc/self/ns/net'), 'lo_flags': flags,
            'connected': connected, 'errno': error, 'capabilities': capabilities,
            'raise_errno': raise_errno}
"#;

    #[cfg(target_os = "linux")]
    fn assert_network_observation(observed: &serde_json::Value, mode: NetworkAccessMode) {
        let expected_connected = mode != NetworkAccessMode::None;
        let host_netns = std::fs::read_link("/proc/self/ns/net").expect("host netns");
        let netns = observed["netns"].as_str().expect("plugin netns");
        if mode == NetworkAccessMode::Any {
            assert_eq!(netns, host_netns.to_str().unwrap());
        } else {
            assert_ne!(netns, host_netns.to_str().unwrap());
            for capability in ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"] {
                assert_eq!(
                    observed["capabilities"][capability].as_u64(),
                    Some(0),
                    "{mode:?}: {observed}"
                );
            }
            assert_eq!(observed["capabilities"]["NoNewPrivs"].as_u64(), Some(1));
            assert_eq!(observed["raise_errno"].as_i64(), Some(libc::EPERM as i64));
        }
        assert_eq!(
            observed["connected"].as_bool(),
            Some(expected_connected),
            "{mode:?}: {observed}"
        );
        assert_eq!(
            observed["lo_flags"].as_u64().expect("loopback flags") & libc::IFF_UP as u64 != 0,
            expected_connected,
            "{mode:?}: {observed}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn loopback_mode_reaches_local_listener_but_none_does_not() {
        let _launcher_lock = LAUNCHER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        require_bwrap_backend("loopback_mode_reaches_local_listener_but_none_does_not");

        // The real launcher is needed to configure loopback inside the sandbox
        // netns. Locate it next to the test executable (one level above the
        // `deps/` directory where test binaries live).
        let launcher = find_sandbox_launcher_for_test();

        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");

        // Self-contained probe: binds a TCP server on the sandbox-internal
        // 127.0.0.1 and connects to it. The listener is INSIDE the sandbox
        // network namespace so it is reachable only when loopback is UP.
        // The documented contract requires `lo` up in `loopback` and down in
        // `none`. Inspect flags as well as connectivity: --unshare-net alone
        // does not prove that the wrapper leaves loopback down.
        //
        // `/usr` is bind-mounted into the bwrap sandbox via LINUX_SYSTEM_BIND_PATHS,
        // so python3 is available inside the sandbox without extra binding.
        let probe_script = format!(
            "#!/usr/bin/env python3\n{NETWORK_PROBE_PY}\nprint(json.dumps(observe_network()), flush=True)\n"
        );
        let probe = plugin_dir.join("probe.py");
        std::fs::write(&probe, probe_script).expect("probe script");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755))
            .expect("chmod probe");

        // Point TRANSIST_SANDBOX_LAUNCHER at the real binary so strict modes
        // find it. Restore the value on drop.
        let _env_guard = EnvGuard {
            key: "TRANSIST_SANDBOX_LAUNCHER",
            old: std::env::var_os("TRANSIST_SANDBOX_LAUNCHER"),
        };
        // SAFETY: launcher tests share a lock; env guard restores the value on drop.
        unsafe { std::env::set_var("TRANSIST_SANDBOX_LAUNCHER", &launcher) };

        let plan_for = |mode: NetworkAccessMode| -> SandboxPlan {
            let (context, filesystem_policy, network_policy) = bwrap_context(tmp.path(), mode);
            build_sandbox_plan_for_backend(
                Some(Backend::Bwrap),
                &filesystem_policy,
                &network_policy,
                &context,
            )
            .expect("bwrap plan")
        };

        for mode in [
            NetworkAccessMode::Loopback,
            NetworkAccessMode::None,
            NetworkAccessMode::Any,
        ] {
            let plan = plan_for(mode);
            let mut cmd = make_sandboxed_command(&probe, &[], &plan).expect("probe command");
            let out = cmd.output().expect("run network probe");
            assert!(
                out.status.success(),
                "{mode:?}: probe must execute, not fail in the wrapper: {cmd:?}; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let observed: serde_json::Value =
                serde_json::from_slice(&out.stdout).expect("network probe output");
            eprintln!("{mode:?}: {cmd:?}; observed: {observed}");
            assert_network_observation(&observed, mode);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn loopback_mode_cannot_reach_non_loopback() {
        let _launcher_lock = LAUNCHER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        require_bwrap_backend("loopback_mode_cannot_reach_non_loopback");

        let launcher = find_sandbox_launcher_for_test();

        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");

        // Probe: attempt to reach a non-loopback address. With --unshare-net,
        // the sandbox has no routing to external addresses, so any connect
        // attempt to a non-loopback IP must fail regardless of network mode.
        // Loopback mode raises 127.0.0.0/8 only; it does not add external routes.
        let probe_script = "\
#!/usr/bin/env python3
import json, socket
try:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(1)
    # Use a well-known address unlikely to have an active listener in the
    # sandbox netns; the important outcome is network unreachability, not
    # connection-refused.
    s.connect(('192.0.2.1', 80))
    s.close()
    # Should never reach here; connecting to a non-loopback address from
    # inside an isolated netns must fail.
    print(json.dumps({'connected': True, 'errno': None}), flush=True)
except OSError as e:
    print(json.dumps({'connected': False, 'errno': e.errno}), flush=True)
";
        let probe = plugin_dir.join("probe_nonloopback.py");
        std::fs::write(&probe, probe_script).expect("probe script");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755))
            .expect("chmod probe");

        let _env_guard = EnvGuard {
            key: "TRANSIST_SANDBOX_LAUNCHER",
            old: std::env::var_os("TRANSIST_SANDBOX_LAUNCHER"),
        };
        // SAFETY: launcher tests share a lock; env guard restores the value on drop.
        unsafe { std::env::set_var("TRANSIST_SANDBOX_LAUNCHER", &launcher) };

        for mode in [NetworkAccessMode::Loopback, NetworkAccessMode::None] {
            let (context, filesystem_policy, network_policy) = bwrap_context(tmp.path(), mode);
            let plan = build_sandbox_plan_for_backend(
                Some(Backend::Bwrap),
                &filesystem_policy,
                &network_policy,
                &context,
            )
            .expect("isolated network plan");
            let mut cmd = make_sandboxed_command(&probe, &[], &plan).expect("command");
            let out = cmd.output().expect("run non-loopback probe");
            assert!(
                out.status.success(),
                "{mode:?}: probe must execute: {cmd:?}; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let observed: serde_json::Value =
                serde_json::from_slice(&out.stdout).expect("non-loopback probe output");
            eprintln!("{mode:?}: non-loopback probe: {observed}");
            assert_eq!(observed["connected"].as_bool(), Some(false));
            assert_eq!(
                observed["errno"].as_i64(),
                Some(libc::ENETUNREACH as i64),
                "{mode:?}: require network unreachability, not a refused connection or wrapper failure: {observed}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn network_launcher_ioctl_failure_never_executes_plugin() {
        let _launcher_lock = LAUNCHER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        require_bwrap_backend("network_launcher_ioctl_failure_never_executes_plugin");
        let launcher = find_sandbox_launcher_for_test();
        for flag in ["--help", "--version"] {
            let output = Command::new(&launcher)
                .arg(flag)
                .output()
                .expect("launcher information");
            assert!(output.status.success(), "{flag}: {output:?}");
            let text = String::from_utf8(output.stdout).expect("launcher output");
            if flag == "--version" {
                assert_eq!(
                    text.trim(),
                    format!("transist-sandbox-launcher {}", env!("CARGO_PKG_VERSION"))
                );
            } else {
                assert!(text.contains("--network-mode <none|loopback>"));
            }
        }
        let _env_guard = EnvGuard {
            key: "TRANSIST_SANDBOX_LAUNCHER",
            old: std::env::var_os("TRANSIST_SANDBOX_LAUNCHER"),
        };
        // SAFETY: launcher tests share a lock and restore the environment.
        unsafe { std::env::set_var("TRANSIST_SANDBOX_LAUNCHER", &launcher) };
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        for mode in [NetworkAccessMode::None, NetworkAccessMode::Loopback] {
            let (context, filesystem_policy, network_policy) = bwrap_context(tmp.path(), mode);
            let marker = context.workspace_dir.join("plugin-executed");
            let mut plan = build_sandbox_plan_for_backend(
                Some(Backend::Bwrap),
                &filesystem_policy,
                &network_policy,
                &context,
            )
            .expect("network plan");
            // Namespace-root exec may inherit this capability from bwrap's
            // bounding set; explicitly drop it to force a real ioctl denial.
            plan.bwrap_args
                .extend(["--cap-drop".to_string(), "CAP_NET_ADMIN".to_string()]);
            let mut command = make_sandboxed_command(
                Path::new("/bin/sh"),
                &["-c".to_string(), format!("touch '{}'", marker.display())],
                &plan,
            )
            .expect("sandbox command");
            let output = command.output().expect("launcher failure probe");
            assert_eq!(
                output.status.code(),
                Some(20),
                "{mode:?}: {command:?}: {output:?}"
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("SIOCSIFFLAGS failed"));
            assert!(
                !marker.exists(),
                "{mode:?}: plugin must not execute after setup failure"
            );
        }
    }

    /// Exercises real APP and MCP dispatch with the same namespace/capability
    /// observations as the direct wrapper probe. These checks always run in CI.
    #[cfg(target_os = "linux")]
    #[test]
    fn process_and_mcp_enforce_strict_network_modes_after_launcher_handoff() {
        use std::collections::BTreeMap;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use transist_core::ports::{ExecutionInvocation, ExecutionPort, RemoteSecretPolicy};
        use transist_plugins::install_plugin_file_in_home;
        use transist_storage_local::LocalActiveRecordStore;

        let _launcher_lock = LAUNCHER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        require_bwrap_backend(
            "process_and_mcp_enforce_strict_network_modes_after_launcher_handoff",
        );
        let launcher = find_sandbox_launcher_for_test();
        let _env_guard = EnvGuard {
            key: "TRANSIST_SANDBOX_LAUNCHER",
            old: std::env::var_os("TRANSIST_SANDBOX_LAUNCHER"),
        };
        // SAFETY: launcher tests share a lock and restore the environment.
        unsafe { std::env::set_var("TRANSIST_SANDBOX_LAUNCHER", launcher) };
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        let staging = home.join(".transist-staging/invoke-network-test");
        for directory in [
            &home,
            &workspace,
            &staging.join("artifacts"),
            &staging.join("evidence"),
        ] {
            std::fs::create_dir_all(directory).expect("fixture directory");
        }
        let runtime_temp = tempfile::Builder::new()
            .prefix(RUNTIME_TEMP_PREFIX)
            .tempdir_in(runtime_temp_parent())
            .expect("runtime temp");
        let shell_manifest =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell/plugin.yaml");
        install_plugin_file_in_home(&home, &shell_manifest).expect("install shell");

        let mcp_source = tmp.path().join("mcp-network-probe");
        std::fs::create_dir_all(&mcp_source).expect("mcp source");
        let server = format!("#!/usr/bin/env python3\n{NETWORK_PROBE_PY}\n")
            + r#"
import sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}},
                  'serverInfo': {'name': 'network-probe', 'version': '1.0.0'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'probe'}]}
    elif method == 'tools/call':
        result = {'content': [], 'structuredContent': observe_network()}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'result': result}), flush=True)
    if method == 'tools/call':
        break
"#;
        std::fs::write(mcp_source.join("server.py"), server).expect("server");
        std::fs::set_permissions(
            mcp_source.join("server.py"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod server");
        std::fs::write(mcp_source.join("schema.json"), "{}").expect("schema");
        std::fs::write(
            mcp_source.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: mcp.network-probe
version: 1.0.0
package:
  files: [server.py, schema.json]
adapter: mcp
entrypoint:
  type: mcp
  command: server.py
actions:
  - name: probe
    input_schema: schema.json
    output_schema: schema.json
    bindings:
      tool_name: probe
capabilities:
  supports_cancel: false
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        install_plugin_file_in_home(&home, &mcp_source.join("plugin.yaml"))
            .expect("install mcp probe");

        for mode in [NetworkAccessMode::None, NetworkAccessMode::Loopback] {
            for adapter in [AdapterKind::Process, AdapterKind::Mcp] {
                let mut request = request_with_policy(serde_json::json!({
                    "network": network_policy(mode),
                    "filesystem": policy(WorkspaceAccessMode::ReadOnly),
                }));
                request.adapter_kind = adapter;
                request.paths = InvokePaths {
                    run_root: staging.display().to_string(),
                    artifacts_dir: staging.join("artifacts").display().to_string(),
                    evidence_dir: staging.join("evidence").display().to_string(),
                    workspace_dir: workspace.display().to_string(),
                };
                request.timeout_ms = Some(5_000);
                request.metadata = serde_json::json!({
                    "plugin_id": if adapter == AdapterKind::Process { "shell" } else { "mcp.network-probe" },
                    STAGING_ROOT_METADATA_KEY: staging,
                    RUNTIME_TEMP_DIR_METADATA_KEY: runtime_temp.path(),
                });
                if adapter == AdapterKind::Process {
                    request.action = "exec".to_string();
                    request.input = serde_json::json!({"command": "/usr/bin/python3", "args": ["-c", format!("{NETWORK_PROBE_PY}\nprint(json.dumps(observe_network()), flush=True)")]});
                } else {
                    request.action = "probe".to_string();
                }
                let invocation = ExecutionInvocation {
                    request,
                    secret_values: BTreeMap::new(),
                    remote_secret_policy: RemoteSecretPolicy::default(),
                };
                let store = Arc::new(LocalActiveRecordStore);
                let result = if adapter == AdapterKind::Process {
                    crate::ProcessAdapter::new(store).execute_step(&invocation)
                } else {
                    crate::McpAdapter::new(store).execute_step(&invocation)
                }
                .expect("adapter execute");
                assert_eq!(result.status, "success", "{adapter:?}/{mode:?}: {result:?}");
                let observed = if adapter == AdapterKind::Process {
                    serde_json::from_str(result.output["stdout"].as_str().expect("probe stdout"))
                        .expect("probe JSON")
                } else {
                    result.output
                };
                eprintln!("{adapter:?}/{mode:?}: {observed}");
                assert_network_observation(&observed, mode);
            }
        }
    }
}
