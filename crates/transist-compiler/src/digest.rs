// SPDX-License-Identifier: MPL-2.0
//! Workflow-definition asset discovery for canonical trust digests.

#![allow(unsafe_code)]

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::ffi::{CStr, CString, OsString};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use transist_trust_digest::{digest_named_entries, normalize_lf, sha256_hex};

use crate::flow::{Flow, FlowCleanupStep, FlowStep};

/// One exact file captured from a source-backed workflow definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDefinitionAsset {
    /// Path relative to the workflow root, normalized with `/` separators.
    pub relative_path: String,
    /// Exact bytes read from the resolved regular file.
    pub bytes: Vec<u8>,
    /// SHA-256 of the exact bytes.
    pub sha256: String,
    /// SHA-256 of LF-normalized bytes used by the signature contract.
    pub normalized_sha256: String,
}

/// Immutable capture of every file included in the workflow definition digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDefinitionSnapshot {
    /// Physical workflow root held while the snapshot was captured.
    pub workflow_root: PathBuf,
    /// Top-level workflow path relative to `workflow_root`.
    pub top_level: String,
    /// Canonical aggregate digest used by detached workflow signatures.
    pub digest: String,
    /// Assets sorted by workflow-relative path.
    pub assets: Vec<WorkflowDefinitionAsset>,
    /// Exact detached signature bytes captured through the workflow root.
    pub signature: Option<Vec<u8>>,
}

/// Computes the canonical workflow-definition digest shared with `transist-admin`.
///
/// The digest covers the full authored workflow definition set: the top-level
/// workflow file plus every referenced subflow file, referenced `prompt_file`
/// target, and `source://` helper script. Each file contributes a LF-normalized
/// SHA-256 content digest, and the final digest combines those per-file digests
/// in sorted path order.
pub fn workflow_definition_sha256(path: &Path) -> Result<String> {
    Ok(capture_workflow_definition(path)?.digest)
}

/// Captures the complete signed definition set in one descriptor-anchored pass.
///
/// Every dependency is resolved relative to the workflow file declaring it. The
/// capture rejects absolute paths, traversal, directories, and symlinks in any
/// component. Callers that verify trust and later persist the definition must
/// retain this value rather than re-reading authored files, so verification and
/// execution use exactly the same bytes.
pub fn capture_workflow_definition(path: &Path) -> Result<WorkflowDefinitionSnapshot> {
    capture_workflow_definition_internal(path, false)
}

/// Captures the complete definition and its optional detached signature through one root.
pub fn capture_workflow_definition_with_signature(
    path: &Path,
) -> Result<WorkflowDefinitionSnapshot> {
    capture_workflow_definition_internal(path, true)
}

fn capture_workflow_definition_internal(
    path: &Path,
    capture_signature: bool,
) -> Result<WorkflowDefinitionSnapshot> {
    let absolute = absolute_path(path)?;
    let workflow_root = absolute
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workflow '{}' has no parent directory", path.display()))?
        .to_path_buf();
    let top_level = absolute
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("workflow '{}' has no file name", path.display()))?;
    let top_level = contained_relative_path(Path::new(top_level))?;

    // Opening the root through every descriptor component rejects a symlinked
    // workflow directory before any authored bytes are parsed or captured.
    let root = open_workflow_root(&workflow_root)?;

    let mut assets = BTreeMap::new();
    let mut visited_flows = BTreeSet::new();
    let top_flow = read_flow_asset(
        &root,
        &workflow_root,
        &top_level,
        "top-level workflow",
        true,
    )?;
    let top_raw = std::str::from_utf8(&top_flow.1)
        .context("top-level workflow is not UTF-8 after capture")?;
    for module_path in crate::modules::root_module_paths(top_raw)? {
        let module_relative = resolve_declared_path(&module_path, &top_level)
            .with_context(|| format!("workflow module asset '{module_path}'"))?;
        let (module, bytes) =
            read_flow_asset(&root, &workflow_root, &module_relative, "module", false)
                .with_context(|| format!("workflow module asset '{module_path}'"))?;
        insert_asset(&mut assets, module_relative.clone(), bytes);
        collect_flow_entries(
            &module,
            &module_relative,
            &root,
            &workflow_root,
            &mut assets,
            &mut visited_flows,
        )?;
    }
    insert_asset(&mut assets, top_level.clone(), top_flow.1);
    collect_flow_entries(
        &top_flow.0,
        &top_level,
        &root,
        &workflow_root,
        &mut assets,
        &mut visited_flows,
    )?;

    let mut digests = BTreeMap::new();
    let mut captured = Vec::with_capacity(assets.len());
    for (relative_path, bytes) in assets {
        let normalized_sha256 = sha256_hex(&normalize_lf(&bytes));
        digests.insert(relative_path.clone(), normalized_sha256.clone());
        captured.push(WorkflowDefinitionAsset {
            relative_path,
            sha256: sha256_hex(&bytes),
            normalized_sha256,
            bytes,
        });
    }
    let mut signature_name = top_level.as_os_str().to_os_string();
    signature_name.push(".sig");
    let signature_path = PathBuf::from(signature_name);
    let signature = if !capture_signature {
        None
    } else {
        match read_workflow_asset(&root, &signature_path) {
            Ok(bytes) => Some(bytes),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed capturing workflow signature: {}",
                        workflow_root.join(&signature_path).display()
                    )
                });
            }
        }
    };
    Ok(WorkflowDefinitionSnapshot {
        workflow_root,
        top_level: top_level.to_string_lossy().replace('\\', "/"),
        digest: digest_named_entries(&digests),
        assets: captured,
        signature,
    })
}

impl WorkflowDefinitionSnapshot {
    /// Parses the top-level workflow from the immutable captured bytes.
    pub fn top_level_flow(&self) -> Result<Flow> {
        let asset = self.asset(&self.top_level)?;
        let raw = std::str::from_utf8(&asset.bytes).with_context(|| {
            format!("workflow snapshot asset '{}' is not UTF-8", self.top_level)
        })?;
        if crate::modules::is_module_document(raw)? {
            bail!("package-local module definitions cannot be installed or launched independently")
        }
        self.flow(&self.top_level)
    }

    /// Parses one captured workflow using its workflow-root-relative path.
    pub fn flow(&self, relative_path: &str) -> Result<Flow> {
        let asset = self.asset(relative_path)?;
        let raw = std::str::from_utf8(&asset.bytes)
            .with_context(|| format!("workflow snapshot asset '{relative_path}' is not UTF-8"))?;
        let mut flow = if relative_path == self.top_level {
            crate::modules::parse_root_flow(raw)
        } else {
            crate::modules::parse_child_flow(raw)
        }
        .with_context(|| format!("invalid workflow snapshot YAML: {relative_path}"))?;
        flow.source_path = Some(self.workflow_root.join(relative_path));
        Ok(flow)
    }

    /// Returns a new immutable snapshot with rewritten top-level bytes.
    pub fn with_top_level_bytes(&self, bytes: Vec<u8>) -> Result<Self> {
        let mut snapshot = self.clone();
        let asset = snapshot
            .assets
            .iter_mut()
            .find(|asset| asset.relative_path == snapshot.top_level)
            .ok_or_else(|| anyhow::anyhow!("workflow snapshot is missing its top-level asset"))?;
        asset.sha256 = sha256_hex(&bytes);
        asset.normalized_sha256 = sha256_hex(&normalize_lf(&bytes));
        asset.bytes = bytes;
        let digests = snapshot
            .assets
            .iter()
            .map(|asset| (asset.relative_path.clone(), asset.normalized_sha256.clone()))
            .collect();
        snapshot.digest = digest_named_entries(&digests);
        Ok(snapshot)
    }

    pub(crate) fn flow_for_source(&self, source_path: &Path) -> Result<Flow> {
        let relative = source_path
            .strip_prefix(&self.workflow_root)
            .with_context(|| {
                format!(
                    "workflow source '{}' is outside captured root '{}'",
                    source_path.display(),
                    self.workflow_root.display()
                )
            })?;
        self.flow(
            &contained_relative_path(relative)?
                .to_string_lossy()
                .replace('\\', "/"),
        )
    }

    pub(crate) fn asset(&self, relative_path: &str) -> Result<&WorkflowDefinitionAsset> {
        self.assets
            .iter()
            .find(|asset| asset.relative_path == relative_path)
            .ok_or_else(|| anyhow::anyhow!("workflow snapshot is missing asset '{relative_path}'"))
    }

    pub(crate) fn resolve_from_source(&self, source_path: &Path, raw: &str) -> Result<String> {
        let declaring = source_path
            .strip_prefix(&self.workflow_root)
            .with_context(|| {
                format!(
                    "workflow source '{}' is outside captured root '{}'",
                    source_path.display(),
                    self.workflow_root.display()
                )
            })?;
        Ok(
            resolve_declared_path(raw, &contained_relative_path(declaring)?)?
                .to_string_lossy()
                .replace('\\', "/"),
        )
    }
}

fn collect_flow_entries(
    flow: &Flow,
    source_relative: &Path,
    root: &std::fs::File,
    workflow_root: &Path,
    assets: &mut BTreeMap<String, Vec<u8>>,
    visited_flows: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if !visited_flows.insert(source_relative.to_path_buf()) {
        return Ok(());
    }

    let flow = flow.with_declarations_applied()?;
    for step in &flow.steps {
        collect_step_assets(step, source_relative, root, assets)?;
        if let Some(subflow) = step.subflow.as_ref() {
            let child_relative = resolve_declared_path(&subflow.path, source_relative)
                .with_context(|| {
                    asset_context(source_relative, &step.id, "subflow", &subflow.path)
                })?;
            let (child, bytes) =
                read_flow_asset(root, workflow_root, &child_relative, "subflow", false)
                    .with_context(|| {
                        asset_context(source_relative, &step.id, "subflow", &subflow.path)
                    })?;
            insert_asset(assets, child_relative.clone(), bytes);
            collect_flow_entries(
                &child,
                &child_relative,
                root,
                workflow_root,
                assets,
                visited_flows,
            )?;
        }
    }
    for cleanup in &flow.cleanup {
        collect_cleanup_assets(cleanup, source_relative, root, assets)?;
    }
    Ok(())
}

fn collect_step_assets(
    step: &FlowStep,
    source_relative: &Path,
    root: &std::fs::File,
    assets: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    collect_source_uris_from_value_map(&step.with, source_relative, root, &step.id, assets)?;
    if let Some(raw) = &step.prompt_file {
        collect_prompt_file_asset(raw, source_relative, root, &step.id, assets)?;
    }
    if let Some(skills) = step.skills.as_deref() {
        for (skill_idx, raw) in skills.iter().enumerate() {
            collect_skill_assets(raw, source_relative, root, assets).with_context(|| {
                asset_context(
                    source_relative,
                    &step.id,
                    "skill",
                    &format!("skills[{skill_idx}] '{raw}'"),
                )
            })?;
        }
    }
    Ok(())
}

fn collect_skill_assets(
    raw: &str,
    source_relative: &Path,
    root: &std::fs::File,
    assets: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let relative = resolve_declared_path(raw, source_relative)?;
    let node = open_relative_no_follow(root, &relative)?;
    let metadata = node.metadata()?;
    let names_skill_file = relative.file_name().and_then(|name| name.to_str()) == Some("SKILL.md");
    if metadata.is_file() {
        if !names_skill_file {
            bail!("skill file path must reference SKILL.md");
        }
        return collect_skill_documents(
            &relative,
            relative.parent().unwrap_or(Path::new("")),
            root,
            assets,
        );
    }
    if !metadata.is_dir() {
        bail!("skill path must resolve to a regular file or directory");
    }
    if names_skill_file {
        bail!("skill file path references a directory where SKILL.md was expected");
    }

    let entries = directory_entries(&node)?;
    if entries.iter().any(|name| name == "SKILL.md") {
        return collect_skill_documents(&relative.join("SKILL.md"), &relative, root, assets);
    }

    let mut found = Vec::new();
    discover_skill_files(&relative, node, &mut found)?;
    if found.is_empty() {
        bail!("skill collection resolves to no skills");
    }
    for skill_file in found {
        let skill_root = skill_file.parent().expect("discovered skill has parent");
        collect_skill_documents(&skill_file, skill_root, root, assets)?;
    }
    Ok(())
}

fn discover_skill_files(
    relative_dir: &Path,
    directory: std::fs::File,
    found: &mut Vec<PathBuf>,
) -> Result<()> {
    for name in directory_entries(&directory)? {
        let child_relative = relative_dir.join(&name);
        let child = open_child_no_follow(directory.as_raw_fd(), &name).with_context(|| {
            format!(
                "skill collection contains an unreadable or symlink entry '{}'",
                child_relative.display()
            )
        })?;
        let metadata = child.metadata()?;
        if metadata.is_dir() {
            let entries = directory_entries(&child)?;
            if entries.iter().any(|entry| entry == "SKILL.md") {
                found.push(child_relative.join("SKILL.md"));
            }
            discover_skill_files(&child_relative, child, found)?;
        }
    }
    found.sort();
    found.dedup();
    Ok(())
}

fn collect_skill_documents(
    document: &Path,
    skill_root: &Path,
    root: &std::fs::File,
    assets: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let mut pending = vec![document.to_path_buf()];
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if !current.starts_with(skill_root) {
            bail!("skill markdown reference escapes its skill root");
        }
        let bytes = read_workflow_asset(root, &current)?;
        let content = std::str::from_utf8(&bytes)
            .with_context(|| format!("skill document '{}' is not UTF-8", current.display()))?;
        let mut links = crate::compile::skills::extract_markdown_links(content);
        insert_asset(assets, current.clone(), bytes);
        links.reverse();
        for link in links {
            let linked = resolve_skill_markdown_path(&current, skill_root, &link)?;
            pending.push(linked);
        }
    }
    Ok(())
}

fn resolve_skill_markdown_path(current: &Path, skill_root: &Path, raw: &str) -> Result<PathBuf> {
    let authored = Path::new(raw);
    if authored.is_absolute() {
        bail!("skill markdown path must be relative");
    }
    if authored
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("skill markdown path must not contain traversal");
    }
    let relative = current
        .parent()
        .ok_or_else(|| anyhow::anyhow!("skill document has no parent"))?
        .join(contained_relative_path(authored)?);
    if !relative.starts_with(skill_root) {
        bail!("skill markdown reference escapes its skill root");
    }
    Ok(relative)
}

fn collect_cleanup_assets(
    cleanup: &FlowCleanupStep,
    source_relative: &Path,
    root: &std::fs::File,
    assets: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let step_path = format!("cleanup/{}", cleanup.id);
    collect_source_uris_from_value_map(&cleanup.with, source_relative, root, &step_path, assets)?;
    if let Some(raw) = &cleanup.prompt_file {
        collect_prompt_file_asset(raw, source_relative, root, &step_path, assets)?;
    }
    if let Some(skills) = cleanup.skills.as_deref() {
        for (skill_idx, raw) in skills.iter().enumerate() {
            collect_skill_assets(raw, source_relative, root, assets).with_context(|| {
                asset_context(
                    source_relative,
                    &step_path,
                    "skill",
                    &format!("skills[{skill_idx}] '{raw}'"),
                )
            })?;
        }
    }
    Ok(())
}

fn collect_prompt_file_asset(
    raw: &str,
    source_relative: &Path,
    root: &std::fs::File,
    step_path: &str,
    assets: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.contains("{{") {
        bail!(
            "{}: prompt_file '{}' contains a template placeholder; only literal paths are supported in signed workflow definitions",
            asset_context(source_relative, step_path, "prompt_file", trimmed),
            trimmed
        );
    }
    let relative = resolve_declared_path(trimmed, source_relative)
        .with_context(|| asset_context(source_relative, step_path, "prompt_file", trimmed))?;
    let bytes = read_workflow_asset(root, &relative)
        .with_context(|| asset_context(source_relative, step_path, "prompt_file", trimmed))?;
    insert_asset(assets, relative, bytes);
    Ok(())
}

fn collect_source_uris_from_value_map(
    values: &BTreeMap<String, yaml_serde::Value>,
    source_relative: &Path,
    root: &std::fs::File,
    step_path: &str,
    assets: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    for value in values.values() {
        collect_source_uris_from_value(value, source_relative, root, step_path, assets)?;
    }
    Ok(())
}

fn collect_source_uris_from_value(
    value: &yaml_serde::Value,
    source_relative: &Path,
    root: &std::fs::File,
    step_path: &str,
    assets: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    match value {
        yaml_serde::Value::String(raw) => {
            if let Some(authored_path) = raw.strip_prefix("source://") {
                let relative =
                    resolve_declared_path(authored_path, source_relative).with_context(|| {
                        asset_context(source_relative, step_path, "source", authored_path)
                    })?;
                let bytes = read_workflow_asset(root, &relative).with_context(|| {
                    asset_context(source_relative, step_path, "source", authored_path)
                })?;
                insert_asset(assets, relative, bytes);
            }
        }
        yaml_serde::Value::Sequence(items) => {
            for item in items {
                collect_source_uris_from_value(item, source_relative, root, step_path, assets)?;
            }
        }
        yaml_serde::Value::Mapping(map) => {
            for item in map.values() {
                collect_source_uris_from_value(item, source_relative, root, step_path, assets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn read_flow_asset(
    root: &std::fs::File,
    workflow_root: &Path,
    relative: &Path,
    kind: &str,
    is_root: bool,
) -> Result<(Flow, Vec<u8>)> {
    let bytes = read_workflow_asset(root, relative)?;
    let raw = std::str::from_utf8(&bytes)
        .with_context(|| format!("{kind} '{}' is not UTF-8", relative.display()))?;
    let mut flow = if is_root {
        crate::modules::parse_root_flow(raw)
    } else {
        crate::modules::parse_child_flow(raw)
    }
    .with_context(|| format!("invalid {kind} YAML: {}", relative.display()))?;
    flow.source_path = Some(workflow_root.join(relative));
    Ok((flow, bytes))
}

fn insert_asset(assets: &mut BTreeMap<String, Vec<u8>>, relative: PathBuf, bytes: Vec<u8>) {
    assets
        .entry(relative.to_string_lossy().replace('\\', "/"))
        .or_insert(bytes);
}

fn resolve_declared_path(raw: &str, declaring_workflow: &Path) -> Result<PathBuf> {
    let authored = Path::new(raw.trim());
    if authored.as_os_str().is_empty() || authored.is_absolute() {
        bail!("workflow asset path must be a non-empty relative path");
    }
    let parent = declaring_workflow.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "declaring workflow '{}' has no parent",
            declaring_workflow.display()
        )
    })?;
    let mut contained = parent.to_path_buf();
    for component in authored.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => contained.push(name),
            Component::ParentDir => {
                bail!("workflow asset path must not contain traversal or special components")
            }
            _ => bail!("workflow asset path escapes the workflow root"),
        }
    }
    if contained.as_os_str().is_empty() {
        bail!("workflow asset path must resolve beneath the workflow root");
    }
    Ok(contained)
}

fn contained_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("workflow asset path must be a non-empty relative path");
    }
    let mut contained = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => contained.push(name),
            _ => bail!("workflow asset path must not contain traversal or special components"),
        }
    }
    Ok(contained)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed resolving current directory for workflow path")?
            .join(path)
    };
    normalize_platform_root_alias(&normalize_absolute_components(&absolute)?)
}

fn normalize_absolute_components(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => normalized.push(name),
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("workflow path escapes the filesystem root");
                }
            }
            Component::Prefix(_) => bail!("workflow path contains an unsupported prefix"),
        }
    }
    Ok(normalized)
}

fn asset_context(
    declaring_workflow: &Path,
    step_path: &str,
    asset_kind: &str,
    authored_path: &str,
) -> String {
    format!(
        "workflow '{}' step '{}' {} asset '{}'",
        declaring_workflow.display(),
        step_path,
        asset_kind,
        authored_path
    )
}

#[cfg(target_os = "macos")]
fn normalize_platform_root_alias(path: &Path) -> Result<PathBuf> {
    for alias in [Path::new("/tmp"), Path::new("/var")] {
        if path.starts_with(alias) && std::fs::symlink_metadata(alias)?.file_type().is_symlink() {
            return Ok(std::fs::canonicalize(alias)?.join(path.strip_prefix(alias)?));
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(not(target_os = "macos"))]
fn normalize_platform_root_alias(path: &Path) -> Result<PathBuf> {
    Ok(path.to_path_buf())
}

#[cfg(unix)]
fn open_workflow_root(path: &Path) -> Result<std::fs::File> {
    open_absolute_directory_no_follow(path)
}

#[cfg(not(unix))]
fn open_workflow_root(_path: &Path) -> Result<std::fs::File> {
    bail!("workflow definition capture requires descriptor-anchored reads on this platform")
}

/// Reads a regular workflow asset through descriptors rooted in a verified
/// workflow directory. Every component is opened with `O_NOFOLLOW`, closing
/// the check-then-read race caused by `canonicalize` followed by `fs::read`.
#[cfg(unix)]
fn read_workflow_asset(root: &std::fs::File, relative: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut file = open_relative_no_follow(root, relative)?;
    if !file.metadata()?.is_file() {
        bail!(
            "workflow asset '{}' is not a regular file",
            relative.display()
        );
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed reading workflow asset '{}'", relative.display()))?;
    Ok(bytes)
}

#[cfg(unix)]
fn open_relative_no_follow(root: &std::fs::File, relative: &Path) -> Result<std::fs::File> {
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => bail!("workflow asset path contains non-normal component"),
        })
        .collect::<Result<Vec<_>>>()?;
    let Some((name, parents)) = components.split_last() else {
        bail!("workflow asset path is empty");
    };
    let mut directory = root.try_clone()?;
    for parent in parents {
        directory = open_subdirectory_no_follow(directory.as_raw_fd(), parent)?;
    }
    open_child_no_follow(directory.as_raw_fd(), name)
        .with_context(|| format!("failed opening workflow asset '{}'", relative.display()))
}

#[cfg(unix)]
fn open_child_no_follow(parent_fd: RawFd, name: &std::ffi::OsStr) -> Result<std::fs::File> {
    let name = CString::new(name.as_bytes()).context("workflow asset path contains NUL")?;
    // SAFETY: `parent_fd` is live and `name` is one NUL-terminated component.
    let fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("no-follow open failed");
    }
    // SAFETY: `fd` is newly owned by this function.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn directory_entries(directory: &std::fs::File) -> Result<Vec<OsString>> {
    let current = CString::new(".").expect("static directory component");
    // SAFETY: opening `.` relative to a held directory creates an independent
    // file description, so enumeration never changes the held anchor's offset.
    let duplicated = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            current.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error()).context("failed duplicating directory");
    }
    // SAFETY: `duplicated` is a valid owned directory descriptor.
    let stream = unsafe { libc::fdopendir(duplicated) };
    if stream.is_null() {
        // SAFETY: `fdopendir` did not take ownership on failure.
        unsafe { libc::close(duplicated) };
        return Err(std::io::Error::last_os_error()).context("failed opening directory stream");
    }
    let mut entries = Vec::new();
    loop {
        // SAFETY: `stream` remains live until `closedir` below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `d_name` is NUL-terminated for the returned directory entry.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            entries.push(OsString::from_vec(name.to_vec()));
        }
    }
    // SAFETY: `stream` is no longer used after this call.
    if unsafe { libc::closedir(stream) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed closing directory stream");
    }
    entries.sort();
    Ok(entries)
}

#[cfg(not(unix))]
fn read_workflow_asset(_root: &std::fs::File, _relative: &Path) -> Result<Vec<u8>> {
    bail!("workflow definition capture requires descriptor-anchored reads on this platform")
}

#[cfg(unix)]
fn open_absolute_directory_no_follow(path: &Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    if !path.is_absolute() {
        bail!("workflow root '{}' must be absolute", path.display());
    }
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = open_subdirectory_no_follow(directory.as_raw_fd(), name)?
            }
            _ => bail!(
                "workflow root '{}' contains special components",
                path.display()
            ),
        }
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_subdirectory_no_follow(
    parent_fd: std::os::fd::RawFd,
    name: &std::ffi::OsStr,
) -> Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes()).context("workflow path component contains NUL")?;
    // SAFETY: `parent_fd` is held by the caller while `openat` resolves the
    // NUL-terminated single path component. The flags are read-only and reject
    // links and non-directories.
    let fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "workflow path contains a missing, non-directory, or symlink component '{}'",
                name.to_string_lossy()
            )
        });
    }
    // SAFETY: `fd` is a fresh descriptor returned by `openat` above.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::{capture_workflow_definition, contained_relative_path, workflow_definition_sha256};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn write(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(path, contents).expect("file");
    }

    #[test]
    fn nested_module_assets_resolve_relative_to_the_declaring_workflow() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        let subflow = root.path().join("flows/review.yaml");
        let helper = root.path().join("flows/helpers/review.sh");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  review:\n    path: flows/review.yaml\nsteps:\n  - id: review\n    module: review\n",
        );
        write(
            &subflow,
            "schema: transist.module.v1\nid: review\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    with:\n      command: source://helpers/review.sh\n",
        );
        write(&helper, "#!/bin/sh\nexit 0\n");

        let snapshot = capture_workflow_definition(&workflow).expect("capture");
        assert!(
            snapshot
                .assets
                .iter()
                .any(|asset| asset.relative_path == "flows/helpers/review.sh")
        );
    }

    #[test]
    fn contained_paths_accept_current_directory_but_reject_escape_and_root() {
        assert_eq!(
            contained_relative_path(Path::new("./child.yaml")).expect("contained path"),
            Path::new("child.yaml")
        );
        assert!(contained_relative_path(Path::new("../child.yaml")).is_err());
        assert!(contained_relative_path(Path::new("/child.yaml")).is_err());
        #[cfg(windows)]
        assert!(contained_relative_path(Path::new(r"C:\child.yaml")).is_err());
    }

    #[test]
    fn capture_accepts_current_directory_module_paths() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  child:\n    path: ./child.yaml\nsteps:\n  - id: child\n    module: child\n",
        );
        write(
            &root.path().join("child.yaml"),
            "schema: transist.module.v1\nid: child\nversion: v1\nsteps: []\n",
        );

        let snapshot = capture_workflow_definition(&workflow).expect("capture");
        assert!(
            snapshot
                .assets
                .iter()
                .any(|asset| asset.relative_path == "child.yaml")
        );
    }

    #[test]
    fn capture_rejects_symlinked_asset_components() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        let outside = tempdir().expect("outside");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: root\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    with:\n      command: source://helpers/run.sh\n",
        );
        write(&outside.path().join("run.sh"), "#!/bin/sh\nexit 0\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("helpers")).expect("symlink");

        let error = capture_workflow_definition(&workflow).expect_err("symlink must fail");
        assert!(
            error
                .to_string()
                .contains("workflow 'workflow.yaml' step 'run' source asset 'helpers/run.sh'")
        );
    }

    #[test]
    fn capture_retains_the_exact_bytes_used_for_the_definition_digest() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: captured\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    with:\n      command: source://helpers/run.sh\n",
        );
        let helper = root.path().join("helpers/run.sh");
        write(&helper, "#!/bin/sh\r\nprintf captured\r\n");

        let capture = capture_workflow_definition(&workflow).expect("capture");
        assert_eq!(
            capture.digest,
            workflow_definition_sha256(&workflow).expect("digest")
        );
        let helper_asset = capture
            .assets
            .iter()
            .find(|asset| asset.relative_path == "helpers/run.sh")
            .expect("helper asset");
        assert_eq!(helper_asset.bytes, fs::read(helper).expect("helper bytes"));
        assert_ne!(helper_asset.sha256, helper_asset.normalized_sha256);
    }

    #[test]
    fn capture_includes_skill_file_root_collection_and_linked_markdown() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: skills\nversion: v1\nsteps:\n  - id: run\n    uses: process:model/run\n    skills:\n      - skills/file/SKILL.md\n      - skills/root\n      - skills/collection\n",
        );
        write(
            &root.path().join("skills/file/SKILL.md"),
            "See [file](file.md).\n",
        );
        write(&root.path().join("skills/file/file.md"), "file\n");
        write(&root.path().join("skills/root/SKILL.md"), "root\n");
        write(
            &root.path().join("skills/collection/one/SKILL.md"),
            "See [nested](refs/nested.md).\n",
        );
        write(
            &root.path().join("skills/collection/one/refs/nested.md"),
            "nested\n",
        );

        let snapshot = capture_workflow_definition(&workflow).expect("capture skills");
        let paths = snapshot
            .assets
            .iter()
            .map(|asset| asset.relative_path.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "skills/file/SKILL.md",
            "skills/file/file.md",
            "skills/root/SKILL.md",
            "skills/collection/one/SKILL.md",
            "skills/collection/one/refs/nested.md",
        ] {
            assert!(paths.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn skill_assets_contribute_to_the_canonical_digest() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: skills\nversion: v1\nsteps:\n  - id: run\n    uses: process:model/run\n    skills: [skills/example]\n",
        );
        let skill = root.path().join("skills/example/SKILL.md");
        write(&skill, "first\n");
        let first = workflow_definition_sha256(&workflow).expect("first digest");
        write(&skill, "second\n");
        let second = workflow_definition_sha256(&workflow).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn profile_referenced_prompt_assets_contribute_to_the_canonical_digest() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        let prompt = root.path().join("prompts/review.md");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: profile-assets\nversion: v1\nprofiles:\n  review:\n    uses: process:model/run\n    prompt_file: prompts/review.md\nsteps:\n  - id: run\n    uses: process:model/run\n    profile: review\n",
        );
        write(&prompt, "first\n");
        let first = workflow_definition_sha256(&workflow).expect("first digest");
        write(&prompt, "second\n");
        let second = workflow_definition_sha256(&workflow).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn capture_rejects_skill_traversal_and_directory_markdown_targets() {
        let root = tempdir().expect("tempdir");
        let workflow = root.path().join("workflow.yaml");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: skills\nversion: v1\nsteps:\n  - id: run\n    uses: process:model/run\n    skills: [../outside/SKILL.md]\n",
        );
        assert!(capture_workflow_definition(&workflow).is_err());

        write(
            &workflow,
            "schema: transist.flow.v1\nid: skills\nversion: v1\nsteps:\n  - id: run\n    uses: process:model/run\n    skills: [skills/SKILL.md]\n",
        );
        fs::create_dir_all(root.path().join("skills/SKILL.md")).expect("skill file directory");
        let error = capture_workflow_definition(&workflow).expect_err("skill file must be a file");
        assert!(format!("{error:#}").contains("directory where SKILL.md was expected"));

        write(
            &workflow,
            "schema: transist.flow.v1\nid: skills\nversion: v1\nsteps:\n  - id: run\n    uses: process:model/run\n    skills: [skills/example]\n",
        );
        write(
            &root.path().join("skills/example/SKILL.md"),
            "See [directory](reference.md).\n",
        );
        fs::create_dir_all(root.path().join("skills/example/reference.md")).expect("directory");
        let error = capture_workflow_definition(&workflow).expect_err("directory must fail");
        assert!(format!("{error:#}").contains("not a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn capture_rejects_symlinked_skill_files_and_collection_entries() {
        let root = tempdir().expect("tempdir");
        let outside = tempdir().expect("outside");
        let workflow = root.path().join("workflow.yaml");
        write(
            &workflow,
            "schema: transist.flow.v1\nid: skills\nversion: v1\nsteps:\n  - id: run\n    uses: process:model/run\n    skills: [skills]\n",
        );
        write(&outside.path().join("SKILL.md"), "outside\n");
        fs::create_dir(root.path().join("skills")).expect("skills");
        std::os::unix::fs::symlink(outside.path(), root.path().join("skills/linked"))
            .expect("symlink");

        let error = capture_workflow_definition(&workflow).expect_err("symlink must fail");
        assert!(format!("{error:#}").contains("symlink entry"));
    }
}
