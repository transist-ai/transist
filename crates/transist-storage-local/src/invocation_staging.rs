// SPDX-License-Identifier: MPL-2.0
//! Private per-invocation file staging and atomic promotion.

// SAFETY: OS-integration: staging uses libc openat/mkfifo/kill for FIFO and process management.
#![allow(unsafe_code)]

use crate::atomic_io::{create_dir_all_durable, sync_directory, write_new_file_if_identical};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use tempfile::{Builder, TempDir};
use transist_core::Redactor;
use transist_core::ports::InvocationStagingPort;
use transist_protocol::{ExecutionRequest, ExecutionResult, FileRef};

mod documents;

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const STAGING_DIR: &str = ".transist-staging";
const OWNER_FILE: &str = ".owner.pid";
const STAGING_ROOT_METADATA_KEY: &str = "__transist_staging_dir";
const RUNTIME_TEMP_DIR_METADATA_KEY: &str = "__transist_runtime_temp_dir";
const RUNTIME_METADATA_FILE_KEY: &str = "runtime_metadata_file";
const RUNTIME_METADATA_FILE_NAME: &str = "transist-runtime-metadata.json";
const RUNTIME_TEMP_PREFIX: &str = "transist-runtime-temp-";
const RUNTIME_TEMP_OWNER_SUFFIX: &str = ".owner.lock";
const RUNTIME_TEMP_OWNER_INIT_PREFIX: &str = ".transist-runtime-owner-init-";

/// Safe current-run identity in the unversioned runtime metadata file.
/// This is data, not a filesystem locator, root-lineage claim, or authority.
#[derive(serde::Serialize)]
struct RuntimeRunMetadata<'a> {
    /// Exact owning `ExecutionRequest.run_id`, including for contained runs.
    id: &'a str,
}

/// Owns a runtime temporary directory and a sibling advisory lock that is not
/// visible inside the plugin sandbox. The lock is released automatically after
/// a crash, allowing a later runtime process to reap the abandoned directory.
struct RuntimeTempRoot {
    root: PathBuf,
    owner_path: PathBuf,
    _owner_lock: File,
}

impl RuntimeTempRoot {
    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for RuntimeTempRoot {
    fn drop(&mut self) {
        if fs::remove_dir_all(&self.root).is_ok() {
            let _ = fs::remove_file(&self.owner_path);
        }
    }
}

/// Owns private artifact and evidence directories for one adapter invocation.
pub struct InvocationStaging {
    _root: TempDir,
    runtime_temp_root: RuntimeTempRoot,
    artifacts_dir: PathBuf,
    evidence_dir: PathBuf,
    runtime_metadata_file: PathBuf,
    source_dir: PathBuf,
    final_artifacts_dir: PathBuf,
    final_evidence_dir: PathBuf,
    documents: std::cell::RefCell<Vec<documents::StagedDocument>>,
}

impl InvocationStaging {
    /// Creates a private, mode-0700 staging directory outside the final run tree.
    pub fn new(home_dir: &Path, request: &ExecutionRequest) -> Result<Self> {
        transist_core::validate_runtime_temp_parent(&runtime_temp_parent(), home_dir, request)?;
        cleanup_stale_staging(home_dir)?;
        cleanup_stale_runtime_temp_roots()?;
        let parent = home_dir.join(STAGING_DIR);
        fs::create_dir_all(&parent)
            .with_context(|| format!("failed creating staging directory: {}", parent.display()))?;
        set_mode(&parent, 0o700)?;
        let root = Builder::new()
            .prefix("invoke-")
            .tempdir_in(&parent)
            .with_context(|| {
                format!(
                    "failed creating invocation staging directory in {}",
                    parent.display()
                )
            })?;
        set_mode(root.path(), 0o700)?;
        fs::write(root.path().join(OWNER_FILE), std::process::id().to_string())?;
        let runtime_temp_root = create_runtime_temp_root()?;
        let artifacts_dir = root.path().join("artifacts");
        let evidence_dir = root.path().join("evidence");
        let source_dir = root.path().join("source");
        fs::create_dir_all(&artifacts_dir)?;
        fs::create_dir_all(&evidence_dir)?;
        fs::create_dir_all(&source_dir)?;
        set_mode(&artifacts_dir, 0o700)?;
        set_mode(&evidence_dir, 0o700)?;
        set_mode(&source_dir, 0o700)?;
        let runtime_metadata_file = root.path().join(RUNTIME_METADATA_FILE_NAME);
        let plugin = request
            .metadata
            .get("plugin_id")
            .and_then(serde_json::Value::as_str)
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "version": request.metadata.get("plugin_version").and_then(serde_json::Value::as_str),
                })
            });
        let payload = serde_json::json!({
            "runtime": { "version": env!("CARGO_PKG_VERSION") },
            "workflow": { "id": request.workflow_id },
            "run": RuntimeRunMetadata { id: &request.run_id },
            "plugins": plugin.into_iter().collect::<Vec<_>>(),
        });
        fs::write(&runtime_metadata_file, serde_json::to_vec_pretty(&payload)?)?;
        Ok(Self {
            _root: root,
            runtime_temp_root,
            artifacts_dir,
            evidence_dir,
            runtime_metadata_file,
            source_dir,
            final_artifacts_dir: PathBuf::from(&request.paths.artifacts_dir),
            final_evidence_dir: PathBuf::from(&request.paths.evidence_dir),
            documents: Default::default(),
        })
    }

    /// Replaces all plugin-visible runtime paths with the private staging root.
    pub fn apply_to_request(&self, request: &mut ExecutionRequest) {
        request.paths.run_root = self._root.path().display().to_string();
        request.paths.artifacts_dir = self.artifacts_dir.display().to_string();
        request.paths.evidence_dir = self.evidence_dir.display().to_string();
        rewrite_source_uris(&mut request.input, &self.source_dir);
        if !request.metadata.is_object() {
            request.metadata = serde_json::json!({});
        }
        let metadata = request
            .metadata
            .as_object_mut()
            .expect("metadata was normalized to an object");
        metadata.insert(
            RUNTIME_METADATA_FILE_KEY.to_string(),
            serde_json::Value::String(self.runtime_metadata_file.display().to_string()),
        );
        metadata.insert(
            STAGING_ROOT_METADATA_KEY.to_string(),
            serde_json::Value::String(self._root.path().display().to_string()),
        );
        metadata.insert(
            RUNTIME_TEMP_DIR_METADATA_KEY.to_string(),
            serde_json::Value::String(self.runtime_temp_root.path().display().to_string()),
        );
    }

    /// Validates, redacts, hashes, and atomically promotes plugin files.
    pub fn promote(&self, result: &mut ExecutionResult, redactor: &Redactor) -> Result<()> {
        let mut files = Vec::new();
        let mut targets = BTreeSet::new();
        collect_files(
            &self.artifacts_dir,
            &self.final_artifacts_dir,
            &result.artifacts,
            redactor,
            &mut targets,
            &mut files,
            FileFamily::Artifacts,
            &|text| self.normalize_document_text(text),
        )?;
        collect_files(
            &self.evidence_dir,
            &self.final_evidence_dir,
            &result.evidence,
            redactor,
            &mut targets,
            &mut files,
            FileFamily::Evidence,
            &|text| self.normalize_document_text(text),
        )?;

        for file in &files {
            write_promoted_file(file)?;
        }

        for file in files {
            let destination = file.target.display().to_string();
            let hash = sha256_hex(&file.bytes);
            let destination_ref = match file.family {
                FileFamily::Artifacts => result.artifacts.get_mut(file.index),
                FileFamily::Evidence => result.evidence.get_mut(file.index),
            };
            if let Some(destination_ref) = destination_ref {
                destination_ref.path = destination;
                destination_ref.sha256 = hash;
                destination_ref.bytes = file.bytes.len() as u64;
            }
        }
        Ok(())
    }
}

impl InvocationStagingPort for InvocationStaging {
    fn materialize_documents(
        &self,
        request: &mut ExecutionRequest,
        store: &dyn transist_core::ports::DocumentStorePort,
    ) -> Result<()> {
        self.stage_documents(request, store)
    }

    fn document_outputs(&self) -> Vec<transist_core::ports::DocumentOutput> {
        self.documents
            .borrow()
            .iter()
            .filter(|doc| doc.binding.access != transist_protocol::DocumentAccess::Read)
            .map(|doc| transist_core::ports::DocumentOutput {
                document: doc.binding.document.clone(),
                path: doc.host_path.clone(),
            })
            .collect()
    }

    fn normalize_result(&self, result: &mut ExecutionResult) -> Result<()> {
        if self.documents.borrow().is_empty() {
            return Ok(());
        }
        let mut output = result.output.clone();
        self.normalize_document_value(&mut output);
        let mut value = serde_json::to_value(&*result)?;
        self.normalize_text_value(&mut value);
        *result = serde_json::from_value(value)?;
        result.output = output;
        Ok(())
    }
    fn materialize_source_assets(&self, assets: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        for (relative, bytes) in assets {
            let relative_path = validate_source_relative_path(relative)?;
            let target = self.source_dir.join(relative_path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
                set_mode(parent, 0o700)?;
            }
            fs::write(&target, bytes).with_context(|| {
                format!(
                    "failed materializing workflow source asset: {}",
                    target.display()
                )
            })?;
            set_mode(&target, 0o500)?;
        }
        Ok(())
    }

    fn apply_to_request(&self, request: &mut ExecutionRequest) {
        Self::apply_to_request(self, request);
    }

    fn promote(&self, result: &mut ExecutionResult, redactor: &Redactor) -> Result<()> {
        Self::promote(self, result, redactor)
    }

    fn transient_redaction_values(&self) -> Vec<String> {
        let host_path = self.runtime_temp_root.path().display().to_string();
        #[cfg(target_os = "linux")]
        {
            let sandbox_path = self
                .runtime_temp_root
                .path()
                .file_name()
                .map(|name| Path::new("/tmp").join(name).display().to_string());
            sandbox_path
                .into_iter()
                .chain([host_path, self._root.path().display().to_string()])
                .collect()
        }
        #[cfg(not(target_os = "linux"))]
        {
            vec![host_path, self._root.path().display().to_string()]
        }
    }
}

fn validate_source_relative_path(relative: &str) -> Result<&Path> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("workflow source asset path must be relative: {relative}");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("workflow source asset path is not contained: {relative}");
        }
    }
    Ok(path)
}

fn rewrite_source_uris(value: &mut serde_json::Value, source_dir: &Path) {
    match value {
        serde_json::Value::String(raw) => {
            let Some(relative) = raw.strip_prefix("source://") else {
                return;
            };
            let Ok(relative) = validate_source_relative_path(relative) else {
                return;
            };
            *raw = source_dir.join(relative).display().to_string();
        }
        serde_json::Value::Array(values) => {
            for value in values {
                rewrite_source_uris(value, source_dir);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                rewrite_source_uris(value, source_dir);
            }
        }
        _ => {}
    }
}

struct PreparedFile {
    family: FileFamily,
    index: usize,
    target: PathBuf,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
enum FileFamily {
    Artifacts,
    Evidence,
}

#[allow(clippy::too_many_arguments)]
fn collect_files(
    staging_dir: &Path,
    final_dir: &Path,
    refs: &[FileRef],
    redactor: &Redactor,
    targets: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PreparedFile>,
    family: FileFamily,
    normalize_text: &dyn Fn(&str) -> String,
) -> Result<()> {
    for (index, file_ref) in refs.iter().enumerate() {
        let source = Path::new(&file_ref.path);
        let relative = source.strip_prefix(staging_dir).with_context(|| {
            format!(
                "artifact/evidence path escaped staging: {}",
                source.display()
            )
        })?;
        validate_relative_path(relative, redactor)?;
        let raw = open_and_read_staged_file(staging_dir, relative)?;
        let bytes = match std::str::from_utf8(&raw) {
            Ok(text) => redactor.redact_str(&normalize_text(text)).into_bytes(),
            Err(_) if redactor.contains_secret_bytes(&raw) => {
                bail!(
                    "binary artifact/evidence contains a known secret: {}",
                    source.display()
                )
            }
            Err(_) => raw,
        };
        let target = final_dir.join(relative);
        if !targets.insert(target.clone()) {
            bail!(
                "duplicate artifact/evidence destination: {}",
                target.display()
            );
        }
        files.push(PreparedFile {
            family,
            index,
            target,
            bytes,
        });
    }
    Ok(())
}

fn validate_relative_path(relative: &Path, redactor: &Redactor) -> Result<()> {
    if relative.as_os_str().is_empty() {
        bail!("artifact/evidence path is empty")
    }
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("artifact/evidence path must contain only normal relative components")
        };
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("artifact/evidence path is not valid UTF-8"))?;
        if redactor.contains_secret(name) {
            bail!("artifact/evidence path contains a known secret")
        }
    }
    Ok(())
}

/// Reads a staged artifact/evidence file whose every path component has been
/// verified as a non-symlink object, traversing the staging tree exclusively
/// through directory descriptors.
///
/// The staging root is anchored with `open(O_DIRECTORY | O_NOFOLLOW)`. Each
/// intermediate component is resolved with `openat(dir_fd, name,
/// O_DIRECTORY | O_NOFOLLOW)` and the final component with
/// `openat(dir_fd, name, O_NOFOLLOW)`. Every opened component is verified
/// with `fstat` on its descriptor (directories for parents, a regular file
/// for the final component), and the bytes are read through that same
/// descriptor.
///
/// Because every lookup is relative to an already-open descriptor and
/// `O_NOFOLLOW` applies to each `openat` name, a same-user racer that
/// renames an intermediate directory or inserts a symlink between two
/// lookups cannot redirect promotion: renaming a component whose descriptor
/// we already hold does not affect subsequent `openat` calls, and a symlink
/// inserted at the next component fails with `ELOOP` at open time. The
/// object whose bytes are returned is provably the object whose identity
/// was verified.
fn open_and_read_staged_file(root: &Path, relative: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        let names = normal_component_names(relative)?;
        let (last_name, parent_names) = names
            .split_last()
            .ok_or_else(|| anyhow::anyhow!("artifact/evidence path is empty"))?;

        let mut dir_fd = open_staging_root(root)?;
        #[cfg(test)]
        race_hook::fire();
        for parent_name in parent_names {
            dir_fd = open_staging_parent(dir_fd.as_raw_fd(), parent_name)?;
            #[cfg(test)]
            race_hook::fire();
        }

        let mut file = open_staging_file(dir_fd.as_raw_fd(), last_name)?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed inspecting staged file: {}", relative.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "symlinked artifact/evidence paths are rejected: {}",
                relative.display()
            )
        }
        if !metadata.is_file() {
            bail!(
                "artifact/evidence path is not a regular file: {}",
                relative.display()
            )
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        std::io::Read::read_to_end(&mut file, &mut bytes)
            .with_context(|| format!("failed reading staged file: {}", relative.display()))?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative);
        bail!(
            "artifact/evidence promotion requires descriptor-based traversal and is unsupported on this platform"
        )
    }
}

/// Collects the normal relative component names of a staged path.
#[cfg(unix)]
fn normal_component_names(relative: &Path) -> Result<Vec<&std::ffi::OsStr>> {
    let mut names = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("artifact/evidence path must contain only normal relative components")
        };
        names.push(name);
    }
    if names.is_empty() {
        bail!("artifact/evidence path is empty")
    }
    Ok(names)
}

/// Anchors the staging traversal with `open(O_DIRECTORY | O_NOFOLLOW)` and
/// verifies the descriptor identifies a directory.
#[cfg(unix)]
fn open_staging_root(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let file = options.open(path).with_context(|| {
        format!(
            "staged artifact/evidence directory is missing or is a symlink: {}",
            path.display()
        )
    })?;
    verify_directory(&file, path)?;
    Ok(file)
}

/// Resolves one intermediate component with `openat(dir_fd, name,
/// O_DIRECTORY | O_NOFOLLOW)` and verifies the descriptor identifies a
/// directory.
#[cfg(unix)]
fn open_staging_parent(dir_fd: RawFd, name: &std::ffi::OsStr) -> Result<std::fs::File> {
    let owned = openat_no_follow(dir_fd, name, libc::O_RDONLY | libc::O_DIRECTORY)?;
    let file: std::fs::File = owned.into();
    verify_directory(&file, &PathBuf::from(name))?;
    Ok(file)
}

/// Opens the final component with `openat(dir_fd, name, O_NOFOLLOW)`.
///
/// `O_NONBLOCK` prevents a non-regular object such as a FIFO from blocking
/// the open while this process waits for a writer; it has no effect on
/// regular files and the descriptor is `fstat`-verified to be a regular file
/// before any bytes are read.
#[cfg(unix)]
fn open_staging_file(dir_fd: RawFd, name: &std::ffi::OsStr) -> Result<std::fs::File> {
    let owned = openat_no_follow(dir_fd, name, libc::O_RDONLY | libc::O_NONBLOCK)?;
    Ok(owned.into())
}

/// Wraps `openat` with `O_NOFOLLOW` plus `O_CLOEXEC`, returning an owned
/// descriptor. Callers verify the descriptor with `fstat` via
/// [`std::fs::File::metadata`].
#[cfg(unix)]
fn openat_no_follow(dir_fd: RawFd, name: &std::ffi::OsStr, flags: libc::c_int) -> Result<OwnedFd> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| anyhow::anyhow!("artifact/evidence path component is not NUL-free"))?;
    // SAFETY: `dir_fd` is a live open directory descriptor from an OwnedFd/File that stays alive through the call, and `name` is a NUL-terminated CString; `openat` reads only those args, returning a new owned fd or a negative error value, and writes no process memory.
    let fd = unsafe {
        libc::openat(
            dir_fd,
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed resolving staged artifact/evidence component: {}",
                name.to_string_lossy()
            )
        });
    }
    // SAFETY: `fd` is the non-negative descriptor returned by `openat` above (negative values already returned early), so it is a uniquely owned new descriptor; `from_raw_fd` transfers that ownership and closes it on drop.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Verifies an already-open descriptor identifies a directory, not a symlink.
#[cfg(unix)]
fn verify_directory(file: &std::fs::File, path: &Path) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("failed inspecting staging path: {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "symlinked artifact/evidence paths are rejected: {}",
            path.display()
        )
    }
    if !metadata.is_dir() {
        bail!(
            "artifact/evidence path component is not a directory: {}",
            path.display()
        )
    }
    Ok(())
}

fn write_promoted_file(file: &PreparedFile) -> Result<()> {
    let parent = file
        .target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("promoted file is missing a parent directory"))?;
    create_dir_all_durable(parent).with_context(|| {
        format!(
            "failed creating final artifact/evidence directory: {}",
            parent.display()
        )
    })?;
    write_new_file_if_identical(&file.target, &file.bytes).with_context(|| {
        format!(
            "failed promoting artifact/evidence (target may already exist): {}",
            file.target.display()
        )
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Removes staging directories whose owner process no longer exists.
pub fn cleanup_stale_staging(home_dir: &Path) -> Result<()> {
    let parent = home_dir.join(STAGING_DIR);
    if !parent.exists() {
        return Ok(());
    }
    let mut removed = false;
    for entry in fs::read_dir(&parent)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir()
            || !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("invoke-"))
        {
            continue;
        }
        let Some(owner) = fs::read_to_string(path.join(OWNER_FILE))
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        else {
            // A newly-created invocation directory can be observed before its
            // owner marker is written. Never delete an unowned directory here.
            continue;
        };
        if !process_is_alive(owner) {
            fs::remove_dir_all(&path).with_context(|| {
                format!(
                    "failed removing stale invocation staging: {}",
                    path.display()
                )
            })?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&parent)?;
    }
    Ok(())
}

fn create_runtime_temp_root() -> Result<RuntimeTempRoot> {
    let parent = runtime_temp_parent();
    let mut unpublished_owner = Builder::new()
        .prefix(RUNTIME_TEMP_OWNER_INIT_PREFIX)
        .tempfile_in(&parent)
        .with_context(|| {
            format!(
                "failed creating unpublished runtime temporary owner lock in {}",
                parent.display()
            )
        })?;
    set_mode(unpublished_owner.path(), 0o600)?;
    lock_runtime_temp_owner(unpublished_owner.as_file())?;
    unpublished_owner.write_all(std::process::id().to_string().as_bytes())?;
    unpublished_owner.as_file().sync_all()?;
    let token = unpublished_owner
        .path()
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_prefix(RUNTIME_TEMP_OWNER_INIT_PREFIX))
        .context("unpublished runtime temporary owner lock has an invalid name")?;
    let root = parent.join(format!("{RUNTIME_TEMP_PREFIX}{token}"));
    let owner_path = runtime_temp_owner_path(&root)?;
    let owner_lock = unpublished_owner
        .persist_noclobber(&owner_path)
        .with_context(|| {
            format!(
                "failed publishing runtime temporary owner lock: {}",
                owner_path.display()
            )
        })?;
    if let Err(error) = fs::create_dir(&root).with_context(|| {
        format!(
            "failed creating runtime temporary directory: {}",
            root.display()
        )
    }) {
        drop(owner_lock);
        let _ = fs::remove_file(&owner_path);
        return Err(error);
    }
    let runtime_temp_root = RuntimeTempRoot {
        root,
        owner_path,
        _owner_lock: owner_lock,
    };
    set_mode(runtime_temp_root.path(), 0o700)?;
    Ok(runtime_temp_root)
}

fn runtime_temp_root_from_owner_path(owner_path: &Path) -> Result<PathBuf> {
    let parent = owner_path
        .parent()
        .context("runtime temporary owner lock has no parent")?;
    let owner_name = owner_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("runtime temporary owner lock has no UTF-8 name")?;
    let root_name = owner_name
        .strip_prefix('.')
        .and_then(|value| value.strip_suffix(RUNTIME_TEMP_OWNER_SUFFIX))
        .filter(|value| value.starts_with(RUNTIME_TEMP_PREFIX))
        .context("runtime temporary owner lock has an invalid name")?;
    Ok(parent.join(root_name))
}

fn cleanup_stale_runtime_temp_roots() -> Result<()> {
    let parent = runtime_temp_parent();
    cleanup_stale_runtime_temp_roots_in(&parent)
}

fn cleanup_stale_runtime_temp_roots_in(parent: &Path) -> Result<()> {
    let mut removed = false;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir()
            || !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(RUNTIME_TEMP_PREFIX))
        {
            continue;
        }
        let owner_path = runtime_temp_owner_path(&path)?;
        let Some(owner_lock) = open_existing_runtime_temp_owner(Some(&path), &owner_path)? else {
            continue;
        };
        if try_lock_runtime_temp_owner(&owner_lock)? {
            let root_removed =
                stale_entry_removed(fs::remove_dir_all(&path)).with_context(|| {
                    format!(
                        "failed removing stale runtime temporary directory: {}",
                        path.display()
                    )
                })?;
            let owner_removed =
                stale_entry_removed(fs::remove_file(&owner_path)).with_context(|| {
                    format!(
                        "failed removing stale runtime temporary owner lock: {}",
                        owner_path.display()
                    )
                })?;
            removed |= root_removed || owner_removed;
        }
    }
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let owner_path = entry.path();
        let Some(owner_name) = owner_path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !owner_name.starts_with(&format!(".{RUNTIME_TEMP_PREFIX}"))
            || !owner_name.ends_with(RUNTIME_TEMP_OWNER_SUFFIX)
        {
            continue;
        }
        let root = runtime_temp_root_from_owner_path(&owner_path)?;
        // Only a definitely absent root makes an owner record orphaned.
        if !matches!(fs::symlink_metadata(&root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        let Some(owner_lock) = open_existing_runtime_temp_owner(None, &owner_path)? else {
            continue;
        };
        if try_lock_runtime_temp_owner(&owner_lock)? {
            let owner_removed =
                stale_entry_removed(fs::remove_file(&owner_path)).with_context(|| {
                    format!(
                        "failed removing orphaned runtime temporary owner lock: {}",
                        owner_path.display()
                    )
                })?;
            removed |= owner_removed;
        }
    }
    if removed {
        sync_directory(parent)?;
    }
    Ok(())
}

fn stale_entry_removed(result: std::io::Result<()>) -> std::io::Result<bool> {
    // Verified stale records may disappear after this sweep opened the owner
    // inode. Only NotFound is idempotent; ownership and other I/O errors retain
    // their existing fail-closed behavior. True retains directory-sync evidence.
    match result {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn runtime_temp_owner_path(root: &Path) -> Result<PathBuf> {
    let parent = root
        .parent()
        .context("runtime temporary root has no parent")?;
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .context("runtime temporary root has no UTF-8 name")?;
    Ok(parent.join(format!(".{name}{RUNTIME_TEMP_OWNER_SUFFIX}")))
}

fn open_existing_runtime_temp_owner(
    root: Option<&Path>,
    owner_path: &Path,
) -> Result<Option<File>> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid reads the effective UID of the current process without dereferencing pointers.
        let current_uid = unsafe { libc::geteuid() };
        open_existing_runtime_temp_owner_checked(root, owner_path, current_uid)
    }
    #[cfg(not(unix))]
    {
        open_existing_runtime_temp_owner_checked(root, owner_path)
    }
}

fn open_existing_runtime_temp_owner_checked(
    root: Option<&Path>,
    owner_path: &Path,
    #[cfg(unix)] current_uid: u32,
) -> Result<Option<File>> {
    let root_metadata = match root.map(fs::symlink_metadata).transpose() {
        Ok(metadata) => metadata,
        Err(error) if runtime_temp_entry_unavailable(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if root_metadata
        .as_ref()
        .is_some_and(|metadata| !metadata.file_type().is_dir())
    {
        return Ok(None);
    }
    // Shared temporary parents contain other users' records. Inspect without
    // following links before attempting a read/write open of any candidate.
    let owner_path_metadata = match fs::symlink_metadata(owner_path) {
        Ok(metadata) => metadata,
        Err(error) if runtime_temp_entry_unavailable(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !owner_path_metadata.file_type().is_file() {
        return Ok(None);
    }
    #[cfg(unix)]
    if root_metadata
        .as_ref()
        .is_some_and(|metadata| !runtime_temp_metadata_is_private(metadata, current_uid))
        || !runtime_temp_metadata_is_private(&owner_path_metadata, current_uid)
        || owner_path_metadata.permissions().mode() & 0o600 != 0o600
    {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let owner_lock = match options.open(owner_path) {
        Ok(file) => file,
        // A replacement can become inaccessible, a symlink, or a nonregular
        // object after the path check. Retain it rather than fail other work.
        Err(error) if runtime_temp_entry_unavailable(&error) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed opening runtime temporary owner lock: {}",
                    owner_path.display()
                )
            });
        }
    };
    let owner_metadata = owner_lock.metadata()?;
    if !owner_metadata.file_type().is_file() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        // The descriptor must still identify the private regular file inspected
        // above. O_NONBLOCK also prevents a replacement FIFO from stalling open.
        if !runtime_temp_metadata_is_private(&owner_metadata, current_uid)
            || owner_metadata.permissions().mode() & 0o600 != 0o600
            || owner_metadata.dev() != owner_path_metadata.dev()
            || owner_metadata.ino() != owner_path_metadata.ino()
        {
            return Ok(None);
        }
    }
    Ok(Some(owner_lock))
}

#[cfg(unix)]
fn runtime_temp_metadata_is_private(metadata: &fs::Metadata, current_uid: u32) -> bool {
    metadata.uid() == current_uid && metadata.permissions().mode() & 0o077 == 0
}

fn runtime_temp_entry_unavailable(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::NotADirectory
            | std::io::ErrorKind::IsADirectory
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENXIO | libc::ENODEV)
    ) {
        return true;
    }
    false
}

fn lock_runtime_temp_owner(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: file owns a valid open descriptor for the duration of the flock call.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed locking runtime temporary owner file");
        }
    }
    let _ = file;
    Ok(())
}

#[cfg(unix)]
fn try_lock_runtime_temp_owner(file: &File) -> Result<bool> {
    // SAFETY: file owns a valid open descriptor for the duration of the flock call.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(false);
    }
    Err(error).context("failed probing runtime temporary owner lock")
}

#[cfg(not(unix))]
fn try_lock_runtime_temp_owner(file: &File) -> Result<bool> {
    let _ = file;
    Ok(false)
}

fn runtime_temp_parent() -> PathBuf {
    transist_core::runtime_temp_parent()
}

fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid > i32::MAX as u32 {
            return false;
        }
        // SAFETY: `pid` was bounded to `i32::MAX` above so the cast is lossless and yields a valid `pid_t`; `kill(pid, 0)` only probes signal delivery (signal 0 is sent), reading no process memory and requiring no other state.
        let result = unsafe { libc::kill(pid as i32, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod race_hook {
    use std::cell::RefCell;

    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    }

    /// Arms a closure invoked by the descriptor traversal between open steps.
    /// The closure runs after each opened directory (the staging root and
    /// every parent) and can mutate the traversed tree to simulate a
    /// same-user race. In production builds this module does not exist and
    /// the `fire()` call sites compile away.
    pub fn set(hook: impl FnMut() + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    /// Disarms any armed hook.
    pub fn clear() {
        HOOK.with(|slot| *slot.borrow_mut() = None);
    }

    /// Invokes the armed hook, if any.
    pub fn fire() {
        HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{
        AdapterKind, InvokePaths, InvokeStep, TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
    };

    fn request(home: &Path, final_root: &Path) -> ExecutionRequest {
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
            action: "action".to_string(),
            input: serde_json::json!({}),
            paths: InvokePaths {
                run_root: home.join("runs/run_run-1").display().to_string(),
                artifacts_dir: final_root.join("artifacts").display().to_string(),
                evidence_dir: final_root.join("evidence").display().to_string(),
                workspace_dir: home.display().to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: None,
            max_timeout_ms: None,
            policy: serde_json::json!({}),
            metadata: serde_json::json!({}),
        }
    }

    fn result(file_ref: FileRef, family: &str) -> ExecutionResult {
        serde_json::from_value(serde_json::json!({
            "type": TEP_INVOKE_RESULT_TYPE_V1,
            "protocol_version": TEP_PROTOCOL_VERSION_V1,
            "invoke_id": "invoke-1",
            "run_id": "run-1",
            "step_id": "step-1",
            "execution": 1,
            "status": "success",
            "exit_code": 0,
            "stdout": "",
            "stderr": "",
            "timing": {"started_at": "0", "finished_at": "0", "duration_ms": 0},
            "artifacts": if family == "artifacts" { vec![file_ref.clone()] } else { vec![] },
            "evidence": if family == "evidence" { vec![file_ref] } else { vec![] },
            "metrics": {},
            "output": {},
            "metrics_validation_errors": 0,
            "usage": null,
            "error": null
        }))
        .expect("valid execution result")
    }

    #[test]
    fn runtime_metadata_file_contains_only_allowlisted_data_and_exact_current_run_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("runs/run-path-is-not-identity");
        for run_id in ["actual-root-1", "actual-child-2"] {
            let mut request = request(temp.path(), &final_root);
            request.run_id = run_id.into();
            request.input = serde_json::json!({"run_id": "authored-id", "secret": "raw-secret"});
            request.metadata = serde_json::json!({
                "plugin_id": "fixture.documents", "plugin_version": "0.1.0",
                "run": {"id": "spoofed-id"}, "root_run_id": "guessed-root",
                "secret": "raw-secret", "home": temp.path(), "policy": {"allow": true}
            });
            let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
            staging.apply_to_request(&mut request);
            let path = PathBuf::from(request.metadata["runtime_metadata_file"].as_str().unwrap());
            assert!(path.starts_with(&request.paths.run_root));
            assert!(!path.starts_with(&final_root));
            assert_eq!(request.run_id, run_id);
            let payload: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).expect("actual stored metadata")).unwrap();
            assert_eq!(
                payload,
                serde_json::json!({
                    "runtime": {"version": env!("CARGO_PKG_VERSION")},
                    "workflow": {"id": "workflow-1"},
                    "run": {"id": run_id},
                    "plugins": [{"name": "fixture.documents", "version": "0.1.0"}]
                })
            );
            #[cfg(unix)]
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            drop(staging);
            assert!(!path.exists(), "metadata must remain invocation-private");
        }
    }

    #[test]
    fn runtime_metadata_run_identity_does_not_bypass_promotion_redaction() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let mut request = request(temp.path(), &final_root);
        // Even an identity colliding with a known secret has no redaction exemption.
        request.run_id = "raw-secret".into();
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        staging.apply_to_request(&mut request);
        let source = Path::new(&request.paths.artifacts_dir).join("metadata.json");
        fs::copy(&staging.runtime_metadata_file, &source).expect("metadata echo");
        let mut result = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        staging
            .promote(&mut result, &Redactor::from_secrets(["raw-secret"]))
            .expect("redacted promotion");
        let payload: serde_json::Value = serde_json::from_slice(
            &fs::read(&result.artifacts[0].path).expect("promoted metadata"),
        )
        .unwrap();
        assert_eq!(payload["run"]["id"], "[REDACTED]");
        assert_eq!(payload["plugins"], serde_json::json!([]));
    }

    #[test]
    fn promotes_text_after_redaction_and_recomputes_hash() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run/artifacts/step/execution_001");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let source = Path::new(&adapter_request.paths.artifacts_dir).join("result.txt");
        let encoded_secret = ["cmF3", "LXNl", "Y3JldA=="].concat();
        fs::write(&source, format!("before=raw-secret after={encoded_secret}")).expect("source");
        let mut result = result(
            FileRef {
                path: source.display().to_string(),
                sha256: "untrusted".to_string(),
                bytes: 0,
            },
            "artifacts",
        );

        staging
            .promote(&mut result, &Redactor::from_secrets(["raw-secret"]))
            .expect("promote");

        let target = final_root.join("artifacts/result.txt");
        assert_eq!(
            fs::read_to_string(&target).expect("target"),
            "before=[REDACTED] after=[REDACTED]"
        );
        assert_eq!(result.artifacts[0].path, target.display().to_string());
        assert_eq!(
            result.artifacts[0].bytes,
            fs::metadata(target).expect("metadata").len()
        );
        assert_ne!(result.artifacts[0].sha256, "untrusted");
    }

    #[test]
    fn materializes_source_assets_and_rewrites_source_uris() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let mut request = request(temp.path(), &final_root);
        request.input = serde_json::json!({
            "command": "source://helpers/run.sh",
            "args": ["source://data/input.txt"]
        });
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        staging
            .materialize_source_assets(&BTreeMap::from([
                (
                    "helpers/run.sh".to_string(),
                    b"#!/bin/sh\nprintf ok\n".to_vec(),
                ),
                ("data/input.txt".to_string(), b"input\n".to_vec()),
            ]))
            .expect("materialize");

        staging.apply_to_request(&mut request);

        let command = request.input["command"].as_str().expect("command");
        assert!(command.contains("/source/helpers/run.sh"));
        assert_eq!(
            fs::read_to_string(command).expect("script"),
            "#!/bin/sh\nprintf ok\n"
        );
        let argument = request.input["args"][0].as_str().expect("argument");
        assert_eq!(fs::read_to_string(argument).expect("input"), "input\n");
    }

    #[test]
    fn promotion_failure_keeps_already_linked_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let source = Path::new(&adapter_request.paths.artifacts_dir).join("result.txt");
        fs::write(&source, "result").expect("source");
        let mut result = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 6,
            },
            "artifacts",
        );

        crate::fault_injection::set(crate::fault_injection::FAULT_AFTER_LINK);
        let error = staging
            .promote(&mut result, &Redactor::from_secrets([]))
            .expect_err("injected promotion failure");
        crate::fault_injection::clear();
        assert!(format!("{error:#}").contains("injected fault"));
        assert!(final_root.join("artifacts/result.txt").is_file());
    }

    #[test]
    fn promotion_retries_identical_files_after_link_and_directory_sync_faults() {
        for point in [
            crate::fault_injection::FAULT_AFTER_LINK,
            crate::fault_injection::FAULT_AFTER_DIRECTORY_SYNC,
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let final_root = temp.path().join("run");
            let request = request(temp.path(), &final_root);
            let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
            let mut adapter_request = request.clone();
            staging.apply_to_request(&mut adapter_request);
            let source = Path::new(&adapter_request.paths.artifacts_dir).join("result.txt");
            fs::write(&source, "result").expect("source");
            let mut result = result(
                FileRef {
                    path: source.display().to_string(),
                    sha256: String::new(),
                    bytes: 6,
                },
                "artifacts",
            );

            crate::fault_injection::set(point);
            assert!(
                staging
                    .promote(&mut result, &Redactor::from_secrets([]))
                    .is_err()
            );
            crate::fault_injection::clear();
            staging
                .promote(&mut result, &Redactor::from_secrets([]))
                .expect("identical retry");
            assert_eq!(
                fs::read(final_root.join("artifacts/result.txt")).expect("target"),
                b"result"
            );
        }
    }

    #[test]
    fn promotion_continues_after_an_identical_file_and_rejects_differing_content() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let first_source = Path::new(&adapter_request.paths.artifacts_dir).join("first.txt");
        let second_source = Path::new(&adapter_request.paths.artifacts_dir).join("second.txt");
        fs::write(&first_source, "first").expect("first source");
        fs::write(&second_source, "second").expect("second source");
        let first_target = final_root.join("artifacts/first.txt");
        fs::create_dir_all(first_target.parent().expect("target parent")).expect("target dir");
        fs::write(&first_target, "first").expect("existing identical target");

        let mut first_result = result(
            FileRef {
                path: first_source.display().to_string(),
                sha256: String::new(),
                bytes: 5,
            },
            "artifacts",
        );
        first_result.artifacts.push(FileRef {
            path: second_source.display().to_string(),
            sha256: String::new(),
            bytes: 6,
        });
        staging
            .promote(&mut first_result, &Redactor::from_secrets([]))
            .expect("promotion continues");
        assert_eq!(
            fs::read(final_root.join("artifacts/second.txt")).expect("second target"),
            b"second"
        );

        fs::write(&first_target, "conflict").expect("conflicting target");
        let mut conflicting_result = result(
            FileRef {
                path: first_source.display().to_string(),
                sha256: String::new(),
                bytes: 5,
            },
            "artifacts",
        );
        conflicting_result.artifacts.push(FileRef {
            path: second_source.display().to_string(),
            sha256: String::new(),
            bytes: 6,
        });
        assert!(
            staging
                .promote(&mut conflicting_result, &Redactor::from_secrets([]))
                .is_err()
        );
        assert_eq!(
            fs::read(&first_target).expect("conflict preserved"),
            b"conflict"
        );
    }

    #[test]
    fn rejects_binary_secret_without_promoting_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let source = Path::new(&adapter_request.paths.evidence_dir).join("binary.bin");
        fs::write(
            &source,
            [
                0, 159, b'r', b'a', b'w', b'-', b's', b'e', b'c', b'r', b'e', b't',
            ],
        )
        .expect("source");
        let mut result = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 12,
            },
            "evidence",
        );

        assert!(
            staging
                .promote(&mut result, &Redactor::from_secrets(["raw-secret"]))
                .is_err()
        );
        assert!(!final_root.join("evidence/binary.bin").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_staged_files() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let source = Path::new(&adapter_request.paths.artifacts_dir).join("link.txt");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "secret").expect("outside");
        symlink(&outside, &source).expect("symlink");
        let mut result = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );

        assert!(
            staging
                .promote(&mut result, &Redactor::from_secrets(["secret"]))
                .is_err()
        );
        assert!(!final_root.join("artifacts/link.txt").exists());
    }

    #[test]
    fn rejects_a_symlink_swapped_in_after_component_validation() {
        // Regression test for the check-then-read TOCTOU: a path that was a
        // regular file (and would have passed a separate "is this a regular
        // file" check by path) but is a symlink by the time promotion
        // actually reads it must never have the symlink target's bytes
        // promoted. The fix opens the final component with `O_NOFOLLOW` and
        // reads through that same descriptor, so there is no window between
        // an identity check and the read in which the path can be swapped.
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "attacker-controlled").expect("outside");

        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let source = Path::new(&adapter_request.paths.artifacts_dir).join("swap.txt");

        // First establish the path as a regular file, then replace it with a
        // symlink before promotion ever reads it — simulating a swap that
        // occurs after any earlier validation of the path's identity.
        fs::write(&source, "legit").expect("write regular file");
        fs::remove_file(&source).expect("remove regular file");
        symlink(&outside, &source).expect("swap to symlink");

        let mut result = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        let promotion = staging.promote(&mut result, &Redactor::from_secrets(["never-matches"]));
        assert!(
            promotion.is_err(),
            "a symlinked staged path must be rejected, never promoted"
        );
        assert!(
            !final_root.join("artifacts/swap.txt").exists(),
            "the symlink target must never be promoted as a final artifact"
        );
    }

    #[cfg(unix)]
    struct RaceGuard;

    #[cfg(unix)]
    impl Drop for RaceGuard {
        fn drop(&mut self) {
            race_hook::clear();
        }
    }

    #[test]
    fn promotes_deeply_nested_artifacts_and_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);

        let artifact_source =
            Path::new(&adapter_request.paths.artifacts_dir).join("a/b/c/result.txt");
        fs::create_dir_all(artifact_source.parent().expect("artifact parent"))
            .expect("artifact dir");
        fs::write(&artifact_source, "artifact").expect("artifact source");
        let evidence_source =
            Path::new(&adapter_request.paths.evidence_dir).join("x/y/z/evidence.txt");
        fs::create_dir_all(evidence_source.parent().expect("evidence parent"))
            .expect("evidence dir");
        fs::write(&evidence_source, "evidence").expect("evidence source");

        let mut outcome = result(
            FileRef {
                path: artifact_source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        outcome.evidence.push(FileRef {
            path: evidence_source.display().to_string(),
            sha256: String::new(),
            bytes: 0,
        });

        staging
            .promote(&mut outcome, &Redactor::from_secrets([]))
            .expect("nested promotion");

        assert_eq!(
            fs::read(final_root.join("artifacts/a/b/c/result.txt")).expect("artifact"),
            b"artifact"
        );
        assert_eq!(
            fs::read(final_root.join("evidence/x/y/z/evidence.txt")).expect("evidence"),
            b"evidence"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_directory_parent_component() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let artifacts_dir = Path::new(&adapter_request.paths.artifacts_dir);
        fs::write(artifacts_dir.join("a"), "not a directory").expect("file parent");
        let source = artifacts_dir.join("a/b/file.txt");

        let mut outcome = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        assert!(
            staging
                .promote(&mut outcome, &Redactor::from_secrets([]))
                .is_err()
        );
        assert!(!final_root.join("artifacts/a/b/file.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_parent_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let artifacts_dir = Path::new(&adapter_request.paths.artifacts_dir);
        let outside = temp.path().join("outside-dir");
        fs::create_dir_all(&outside).expect("outside dir");
        // The symlink target holds the file the racer wants promoted. If the
        // traversal followed the symlink, `link/file.txt` would be promoted
        // to the final tree and the assertion below would fail.
        fs::write(outside.join("file.txt"), "attacker-controlled").expect("outside file");
        symlink(&outside, artifacts_dir.join("link")).expect("symlink parent");
        let source = artifacts_dir.join("link/file.txt");

        let mut outcome = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        assert!(
            staging
                .promote(&mut outcome, &Redactor::from_secrets([]))
                .is_err()
        );
        assert!(!final_root.join("artifacts/link/file.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_regular_final_component() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let artifacts_dir = Path::new(&adapter_request.paths.artifacts_dir);
        let source = artifacts_dir.join("not-a-file");
        fs::create_dir_all(&source).expect("final directory");

        let mut outcome = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        assert!(
            staging
                .promote(&mut outcome, &Redactor::from_secrets([]))
                .is_err()
        );
        assert!(!final_root.join("artifacts/not-a-file").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fifo_final_component_without_blocking() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let artifacts_dir = Path::new(&adapter_request.paths.artifacts_dir);
        let source = artifacts_dir.join("named-pipe");
        let c_path =
            std::ffi::CString::new(source.as_os_str().as_bytes()).expect("path has no NUL");
        // SAFETY: `c_path` is a NUL-terminated CString (interior NULs rejected by the `expect` above), so `mkfifo` receives a valid terminated path pointer plus a valid `mode_t`, and writes no process memory.
        let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600 as libc::mode_t) };
        assert_eq!(
            created,
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );

        let mut outcome = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        assert!(
            staging
                .promote(&mut outcome, &Redactor::from_secrets([]))
                .is_err()
        );
        assert!(!final_root.join("artifacts/named-pipe").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_final_component_swapped_for_symlink_after_parent_open() {
        use std::os::unix::fs::symlink;

        let _guard = RaceGuard;
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let artifacts_dir = Path::new(&adapter_request.paths.artifacts_dir);
        let source = artifacts_dir.join("dir/swap.txt");
        fs::create_dir_all(source.parent().expect("parent")).expect("parent dir");
        fs::write(&source, "legit").expect("placeholder");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "attacker-controlled").expect("outside");

        race_hook::set({
            let source = source.clone();
            let outside = outside.clone();
            let calls = std::cell::Cell::new(0usize);
            move || {
                let n = calls.get() + 1;
                calls.set(n);
                if n != 2 {
                    return;
                }
                fs::remove_file(&source).expect("swap out regular file");
                symlink(&outside, &source).expect("swap in symlink");
            }
        });

        let mut outcome = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        let promotion = staging.promote(&mut outcome, &Redactor::from_secrets([]));
        assert!(
            promotion.is_err(),
            "a final component swapped for a symlink after the parent open must be rejected"
        );
        assert!(
            !final_root.join("artifacts/dir/swap.txt").exists(),
            "the symlink target must never be promoted as a final artifact"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_intermediate_directory_renamed_and_replaced_with_symlink() {
        use std::os::unix::fs::symlink;

        let _guard = RaceGuard;
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
        let mut adapter_request = request.clone();
        staging.apply_to_request(&mut adapter_request);
        let artifacts_dir = Path::new(&adapter_request.paths.artifacts_dir);
        let source = artifacts_dir.join("a/b/file.txt");
        fs::create_dir_all(source.parent().expect("parent")).expect("parent dir");
        fs::write(&source, "legit").expect("placeholder");
        let outside = temp.path().join("outside-dir");
        fs::create_dir_all(&outside).expect("outside dir");
        // The swapped-in symlink points at a directory that already holds the
        // file the racer wants promoted. If the traversal followed the
        // intermediate symlink, `file.txt` would be promoted to the final
        // tree and the assertion below would fail.
        fs::write(outside.join("file.txt"), "attacker-controlled").expect("outside file");

        race_hook::set({
            let b_dir = artifacts_dir.join("a/b");
            let a_dir = artifacts_dir.join("a");
            let outside = outside.clone();
            let calls = std::cell::Cell::new(0usize);
            move || {
                let n = calls.get() + 1;
                calls.set(n);
                if n != 2 {
                    return;
                }
                fs::rename(&b_dir, a_dir.join("b_stash")).expect("rename away");
                symlink(&outside, &b_dir).expect("insert symlink");
            }
        });

        let mut outcome = result(
            FileRef {
                path: source.display().to_string(),
                sha256: String::new(),
                bytes: 0,
            },
            "artifacts",
        );
        let promotion = staging.promote(&mut outcome, &Redactor::from_secrets([]));
        assert!(
            promotion.is_err(),
            "an intermediate directory swapped for a symlink after its parent open must be rejected"
        );
        assert!(
            !final_root.join("artifacts/a/b/file.txt").exists(),
            "a redirected intermediate path must never be promoted"
        );
    }

    #[test]
    fn startup_cleanup_removes_staging_owned_by_dead_processes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let stale = temp.path().join(STAGING_DIR).join("invoke-stale");
        fs::create_dir_all(&stale).expect("stale staging");
        fs::write(stale.join(OWNER_FILE), "4294967295").expect("owner");

        cleanup_stale_staging(temp.path()).expect("cleanup");

        assert!(!stale.exists());
    }

    #[test]
    fn startup_cleanup_keeps_staging_without_owner_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let incomplete = temp.path().join(STAGING_DIR).join("invoke-incomplete");
        fs::create_dir_all(&incomplete).expect("incomplete staging");

        cleanup_stale_staging(temp.path()).expect("cleanup");

        assert!(incomplete.exists());
    }

    #[test]
    fn runtime_temp_root_is_private_and_removed_with_invocation_staging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("run");
        let request = request(temp.path(), &final_root);
        let root = {
            let staging = InvocationStaging::new(temp.path(), &request).expect("staging");
            let mut adapter_request = request.clone();
            staging.apply_to_request(&mut adapter_request);
            let root = PathBuf::from(
                adapter_request.metadata[RUNTIME_TEMP_DIR_METADATA_KEY]
                    .as_str()
                    .expect("runtime temp metadata"),
            );
            assert!(root.is_dir());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&root).expect("metadata").permissions().mode() & 0o777,
                    0o700
                );
            }
            root
        };

        assert!(!root.exists(), "temporary root must be removed on unwind");
    }

    #[test]
    fn runtime_temp_cleanup_reaps_stale_roots_without_removing_live_siblings() {
        let parent = runtime_temp_parent();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let stale_root = parent.join(format!("{RUNTIME_TEMP_PREFIX}stale-{unique}"));
        fs::create_dir(&stale_root).expect("stale root");
        set_mode(&stale_root, 0o700).expect("stale root mode");
        let stale_owner = runtime_temp_owner_path(&stale_root).expect("stale owner path");
        let mut stale_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&stale_owner)
            .expect("stale owner");
        set_mode(&stale_owner, 0o600).expect("stale owner mode");
        stale_lock.write_all(b"stale").expect("stale metadata");
        drop(stale_lock);
        let orphan_owner = parent.join(format!(
            ".{RUNTIME_TEMP_PREFIX}orphan-{unique}{RUNTIME_TEMP_OWNER_SUFFIX}"
        ));
        fs::write(&orphan_owner, b"orphan").expect("orphan owner");
        set_mode(&orphan_owner, 0o600).expect("orphan owner mode");
        let publishing_owner = parent.join(format!(
            ".{RUNTIME_TEMP_PREFIX}publishing-{unique}{RUNTIME_TEMP_OWNER_SUFFIX}"
        ));
        // Match real publication: acquire the lock under an unpublished name
        // before making the owner discoverable to concurrent cleanup scans.
        let publishing_lock = Builder::new()
            .prefix(RUNTIME_TEMP_OWNER_INIT_PREFIX)
            .tempfile_in(&parent)
            .expect("publishing owner");
        set_mode(publishing_lock.path(), 0o600).expect("publishing owner mode");
        lock_runtime_temp_owner(publishing_lock.as_file()).expect("publishing owner lock");
        let publishing_lock = publishing_lock
            .persist_noclobber(&publishing_owner)
            .expect("publish locked owner");
        let live = create_runtime_temp_root().expect("live root");
        let live_root = live.path().to_path_buf();
        let live_owner = live.owner_path.clone();

        cleanup_stale_runtime_temp_roots().expect("cleanup");

        assert!(!stale_root.exists(), "stale root must be reaped");
        assert!(!stale_owner.exists(), "stale owner lock must be reaped");
        assert!(!orphan_owner.exists(), "orphan owner lock must be reaped");
        assert!(
            publishing_owner.exists(),
            "locked unpublished root must be kept"
        );
        assert!(live_root.exists(), "live sibling must not be reaped");
        assert!(live_owner.exists(), "live owner lock must remain");
        drop(publishing_lock);
        cleanup_stale_runtime_temp_roots().expect("reap unlocked owner");
        assert!(!publishing_owner.exists(), "unlocked owner must be reaped");
        drop(live);
        assert!(!live_root.exists(), "live root must be removed on drop");
        assert!(
            !live_owner.exists(),
            "live owner lock must be removed on drop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_temp_cleanup_ignores_invalid_owner_records() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let parent = tempfile::tempdir_in("/tmp").expect("isolated temporary parent");
        let target = parent.path().join("untouched-target");
        fs::write(&target, b"not runtime data").expect("symlink target");
        set_mode(&target, 0o000).expect("target mode");
        let mut records = Vec::new();
        let mut listeners = Vec::new();
        for orphan in [false, true] {
            for kind in ["symlink", "dangling", "directory", "socket", "inaccessible"] {
                let root = parent
                    .path()
                    .join(format!("{RUNTIME_TEMP_PREFIX}{kind}-{orphan}"));
                if !orphan {
                    fs::create_dir(&root).expect("root");
                    set_mode(&root, 0o700).expect("root mode");
                    fs::write(root.join("keep"), b"keep").expect("root contents");
                }
                let owner = runtime_temp_owner_path(&root).expect("owner path");
                match kind {
                    "symlink" => symlink(&target, &owner).expect("symlink owner"),
                    "dangling" => symlink(parent.path().join("missing"), &owner)
                        .expect("dangling symlink owner"),
                    "directory" => fs::create_dir(&owner).expect("directory owner"),
                    "socket" => listeners.push(UnixListener::bind(&owner).expect("socket owner")),
                    "inaccessible" => {
                        fs::write(&owner, b"keep owner").expect("inaccessible owner");
                        set_mode(&owner, 0o000).expect("inaccessible owner mode");
                    }
                    _ => unreachable!(),
                }
                assert!(
                    open_existing_runtime_temp_owner((!orphan).then_some(root.as_path()), &owner)
                        .expect("invalid record must not fail owner lookup")
                        .is_none(),
                    "{kind}, orphan={orphan}"
                );
                records.push((root, owner, orphan));
            }
        }
        let stale = parent.path().join(format!("{RUNTIME_TEMP_PREFIX}stale"));
        fs::create_dir(&stale).expect("stale root");
        set_mode(&stale, 0o700).expect("stale root mode");
        let stale_owner = runtime_temp_owner_path(&stale).expect("stale owner path");
        fs::write(&stale_owner, b"stale").expect("stale owner");
        set_mode(&stale_owner, 0o600).expect("stale owner mode");

        // Descriptor-anchored sync requires the physical spelling of macOS's
        // symlinked /tmp parent, just like production runtime_temp_parent().
        let parent_path = parent.path().canonicalize().expect("physical temp parent");
        cleanup_stale_runtime_temp_roots_in(&parent_path).expect("cleanup must continue");

        assert!(!stale.exists(), "eligible stale root must still be reaped");
        assert!(!stale_owner.exists(), "eligible owner must still be reaped");
        for (root, owner, orphan) in records {
            assert!(
                fs::symlink_metadata(&owner).is_ok(),
                "owner must be retained"
            );
            if !orphan {
                assert_eq!(fs::read(root.join("keep")).expect("retained root"), b"keep");
            }
        }
        assert_eq!(
            fs::metadata(&target)
                .expect("target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o000,
            "cleanup must not change the symlink target"
        );
        set_mode(&target, 0o600).expect("restore target mode");
        assert_eq!(
            fs::read(&target).expect("retained target"),
            b"not runtime data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_temp_owner_lookup_rejects_foreign_uid() {
        let parent = tempfile::tempdir_in("/tmp").expect("isolated temporary parent");
        set_mode(parent.path(), 0o700).expect("root mode");
        let owner = parent.path().join("owner");
        fs::write(&owner, b"owner").expect("owner");
        set_mode(&owner, 0o600).expect("owner mode");
        let owner_uid = fs::symlink_metadata(&owner).expect("owner metadata").uid();
        for root in [None, Some(parent.path())] {
            assert!(
                open_existing_runtime_temp_owner_checked(root, &owner, owner_uid)
                    .expect("owned record lookup")
                    .is_some()
            );
            // Model another caller's UID through the production lookup body,
            // without chown, credential changes, or test-only unsafe syscalls.
            assert!(
                open_existing_runtime_temp_owner_checked(root, &owner, owner_uid.wrapping_add(1))
                    .expect("foreign record must not fail owner lookup")
                    .is_none()
            );
        }
        assert_eq!(fs::read(&owner).expect("retained owner"), b"owner");
    }

    #[test]
    fn runtime_temp_cleanup_disappearance_is_idempotent_but_io_errors_propagate() {
        let parent = tempfile::tempdir().expect("isolated parent");
        let root = parent.path().join("already-removed-root");
        let owner = parent.path().join("already-removed-owner");
        assert!(!stale_entry_removed(fs::remove_dir_all(root)).expect("absent root"));
        assert!(!stale_entry_removed(fs::remove_file(owner)).expect("absent owner"));
        assert_eq!(
            stale_entry_removed(Err(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            )))
            .expect_err("permission failures must propagate")
            .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
