// SPDX-License-Identifier: MPL-2.0
//! Atomic plugin package staging and publication.
//!
//! A plugin package can be mutated while it is being installed by a racer that
//! shares the owner account, so installation never treats the source directory
//! as the verified artifact. Instead the source package is copied into a private
//! staging snapshot whose every component is traversed through directory
//! descriptors (`openat` with `O_NOFOLLOW`), the snapshot digest is computed,
//! and only the byte-identical verified snapshot is atomically renamed into the
//! final install directory. The install index records the exact snapshot digest.
//!
//! Verification (signature checks, capability checks) is the caller's
//! responsibility: publish only after the snapshot has been verified.

// SAFETY: OS-integration: plugin staging uses libc openat/fstatat/from_raw_fd for descriptor-safe file ops.
#![allow(unsafe_code)]

use crate::digest::{collect_plugin_files, plugin_dir_sha256};
use crate::index::{
    enforce_install_index_immutability, record_install_index_entry, remove_install_index_entry,
};
use crate::manifest::{InstalledPlugin, PluginInstallReport, PluginManifest, load_manifest_file};
use crate::paths::plugins_root_in_home;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use transist_prerequisites::{
    EvaluationContext, PrerequisiteReport, PrerequisiteScope, blocking_error_message,
    evaluate_prerequisites,
};

/// Name of the staging directory beneath the plugin root.
///
/// The staging directory never appears as a plugin id: publication renames it
/// to `<plugins>/<id>/<version>`, and listing walks only that final shape.
const STAGING_DIR_NAME: &str = ".staging";

/// Fault point: publication fails after the trusted signature sidecar has been
/// written but before the install-index entry is recorded. Unit-test only.
pub(crate) const FAULT_PLUGIN_AFTER_SIDECAR: usize = 1;
/// Fault point: publication fails after the install-index entry is recorded
/// but before the staged snapshot is renamed into place. Unit-test only.
pub(crate) const FAULT_PLUGIN_AFTER_INDEX: usize = 2;
/// Fault point: publication fails after the staged snapshot has been renamed
/// into place but before the parent directory is synced. Unit-test only.
pub(crate) const FAULT_PLUGIN_AFTER_RENAME: usize = 3;

#[cfg(test)]
mod fault_hooks {
    use std::cell::Cell;

    thread_local! {
        static ACTIVE: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn set(point: usize) {
        ACTIVE.with(|value| value.set(point));
    }

    pub(crate) fn clear() {
        ACTIVE.with(|value| value.set(0));
    }

    pub(crate) fn active() -> usize {
        ACTIVE.with(|value| value.get())
    }
}

#[cfg(test)]
fn check_test_fault(point: usize) -> anyhow::Result<()> {
    if fault_hooks::active() == point {
        fault_hooks::clear();
        anyhow::bail!("injected plugin publication fault at point {point}");
    }
    Ok(())
}

#[cfg(not(test))]
fn check_test_fault(_point: usize) -> anyhow::Result<()> {
    Ok(())
}

/// A plugin package that has been copied into a private immutable staging
/// snapshot but not yet published.
///
/// Dropping the value removes the staging snapshot unless it was published, so
/// a failed verification or a caller that abandons the staged package never
/// leaks a partial plugin tree.
#[derive(Debug)]
pub struct StagedPluginPackage {
    home_dir: PathBuf,
    staging_dir: PathBuf,
    target_dir: PathBuf,
    snapshot_sha256: String,
    manifest: PluginManifest,
    prerequisites: PrerequisiteReport,
    published: bool,
}

impl Drop for StagedPluginPackage {
    fn drop(&mut self) {
        if !self.published {
            cleanup_snapshot(&self.staging_dir);
        }
    }
}

/// Removes a staged snapshot and the now-empty private staging root.
fn cleanup_snapshot(snapshot_dir: &Path) {
    let _ = fs::remove_dir_all(snapshot_dir);
    if let Some(staging_root) = snapshot_dir.parent() {
        let is_empty = staging_root
            .file_name()
            .is_some_and(|name| name == STAGING_DIR_NAME)
            && match fs::read_dir(staging_root) {
                Ok(mut entries) => entries.next().is_none(),
                Err(_) => false,
            };
        if is_empty {
            let _ = fs::remove_dir(staging_root);
        }
    }
}

impl StagedPluginPackage {
    /// Returns the private staged snapshot directory. Byte-for-byte contents
    /// become the install directory on [`publish_staged_plugin`].
    pub fn snapshot_dir(&self) -> &Path {
        &self.staging_dir
    }

    /// Returns the exact SHA-256 digest of the staged snapshot.
    pub fn snapshot_sha256(&self) -> &str {
        &self.snapshot_sha256
    }

    /// Returns the manifest of the staged package.
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Returns the final install directory that publication targets.
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }
}

/// Stages a plugin package from its manifest file into a private snapshot.
///
/// The source package is read exclusively through directory descriptors with
/// `O_NOFOLLOW`, so a same-user racer cannot redirect a component to a symlink
/// after it has been validated. Symlinks, special files, and any mutation of
/// the source tree during the snapshot copy are rejected before the snapshot
/// digest is computed.
pub fn stage_plugin_package_in_home(
    home_dir: &Path,
    manifest_file: &Path,
) -> Result<StagedPluginPackage> {
    let manifest = load_manifest_file(manifest_file)?;
    let source_dir = manifest_file
        .parent()
        .with_context(|| format!("manifest path has no parent: {}", manifest_file.display()))?
        .to_path_buf();
    let prerequisites = evaluate_prerequisites(
        &manifest.prerequisites,
        &EvaluationContext::new(PrerequisiteScope::Plugin, &source_dir),
    )?;
    if prerequisites.has_blockers() {
        bail!(blocking_error_message(&prerequisites));
    }

    let staging_root = plugins_root_in_home(home_dir).join(STAGING_DIR_NAME);
    let snapshot_dir = staging_root.join(format!(
        "{}-{}-{}",
        manifest.id,
        manifest.version,
        unique_staging_suffix()
    ));
    let target_dir = plugins_root_in_home(home_dir)
        .join(&manifest.id)
        .join(&manifest.version);

    // Capture the source digest before and after the copy. If the source is
    // mutated while the snapshot is being taken, the two digests differ and
    // the match fails before anything is published.
    let before_sha256 = plugin_dir_sha256(&source_dir).map_err(|error| {
        anyhow::anyhow!(
            "failed computing plugin digest for '{}' version '{}': {error}",
            manifest.id,
            manifest.version
        )
    })?;
    if let Some(parent) = snapshot_dir.parent() {
        create_private_dir(parent).with_context(|| {
            format!("failed creating plugin staging root: {}", parent.display())
        })?;
    }
    copy_plugin_files_descriptor_anchored(&source_dir, &snapshot_dir)?;
    let after_sha256 = plugin_dir_sha256(&source_dir).map_err(|error| {
        anyhow::anyhow!(
            "failed computing plugin digest for '{}' version '{}': {error}",
            manifest.id,
            manifest.version
        )
    })?;
    if before_sha256 != after_sha256 {
        cleanup_snapshot(&snapshot_dir);
        bail!(
            "plugin '{}' version '{}' source package changed during install (code=plugin_toctou_detected)",
            manifest.id,
            manifest.version
        );
    }
    let snapshot_sha256 = plugin_dir_sha256(&snapshot_dir).map_err(|error| {
        cleanup_snapshot(&snapshot_dir);
        anyhow::anyhow!(
            "failed computing staged plugin digest for '{}' version '{}': {error}",
            manifest.id,
            manifest.version
        )
    })?;
    if snapshot_sha256 != before_sha256 {
        cleanup_snapshot(&snapshot_dir);
        bail!(
            "plugin '{}' version '{}' staged snapshot does not match the source package (code=plugin_snapshot_mismatch)",
            manifest.id,
            manifest.version
        );
    }

    Ok(StagedPluginPackage {
        home_dir: home_dir.to_path_buf(),
        staging_dir: snapshot_dir,
        target_dir,
        snapshot_sha256,
        manifest,
        prerequisites,
        published: false,
    })
}

/// Atomically publishes a staged plugin package into its final install
/// directory and records the exact verified digest in the install index.
///
/// The snapshot is renamed (never re-copied), so the published bytes are exactly
/// the bytes that were digested and verified in [`stage_plugin_package_in_home`].
/// Version immutability is enforced against the snapshot digest, and the target
/// is never touched unless every check passes.
pub fn publish_staged_plugin(mut staged: StagedPluginPackage) -> Result<PluginInstallReport> {
    let result = publish_staged_plugin_inner(&staged, None);
    match result {
        Ok(report) => {
            staged.published = true;
            Ok(report)
        }
        Err(error) => Err(error),
    }
}

/// Atomically publishes a staged plugin package together with an optional
/// detached signature sidecar so the package, the sidecar, and the
/// install-index entry are committed as one recoverable unit.
///
/// The sidecar bytes (typically the verified `transist.signature.v1` envelope)
/// are written to the `<version>.sig` sibling of the install directory before
/// the snapshot is renamed, so a trusted install can never be observed
/// installed-but-unsigned. Pass `Some(bytes)` when the package was verified in
/// trusted mode and `None` for an unsigned (or dev test-version bypass) install.
pub fn publish_staged_plugin_with_sidecar(
    mut staged: StagedPluginPackage,
    sidecar: Option<Vec<u8>>,
) -> Result<PluginInstallReport> {
    let result = publish_staged_plugin_inner(&staged, sidecar.as_deref());
    match result {
        Ok(report) => {
            staged.published = true;
            Ok(report)
        }
        Err(error) => Err(error),
    }
}

fn publish_staged_plugin_inner(
    staged: &StagedPluginPackage,
    sidecar: Option<&[u8]>,
) -> Result<PluginInstallReport> {
    enforce_install_index_immutability(
        &staged.home_dir,
        &staged.manifest.id,
        &staged.manifest.version,
        &staged.snapshot_sha256,
    )?;
    if staged.target_dir.exists() {
        bail!(
            "plugin '{}' version '{}' is already installed",
            staged.manifest.id,
            staged.manifest.version
        );
    }

    // The trusted signature sidecar is published before the package directory
    // appears, so a crash can never leave an installed-but-unsigned package.
    let target_sig = signature_sibling(&staged.target_dir);
    if let Some(bytes) = sidecar {
        publish_no_clobber_sidecar(&target_sig, bytes)?;
        check_test_fault(FAULT_PLUGIN_AFTER_SIDECAR)?;
    }

    // Record the install-index entry before the rename so a crash can never
    // leave an installed-but-unindexed package. If publication fails before the
    // directive is committed and this entry is new, it is rolled back so no
    // phantom entry remains.
    let index_existed = record_install_index_entry(
        &staged.home_dir,
        &staged.manifest.id,
        &staged.manifest.version,
        &staged.snapshot_sha256,
    )?;
    let mut rename_committed = false;
    let outcome = (|| -> Result<()> {
        check_test_fault(FAULT_PLUGIN_AFTER_INDEX)?;

        // Make every staged file durable and materialize the install parent
        // *before* the final digest, so the re-digest is immediately followed
        // by the rename with no intervening writes to the staging tree. This
        // shrinks the same-user swap window to the minimum achievable for a
        // directory publication.
        sync_snapshot_tree(&staged.staging_dir)?;
        if let Some(parent) = staged.target_dir.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating plugin root: {}", parent.display()))?;
        }

        // Re-digest the staged snapshot immediately before publication and
        // abort on any mismatch: the installed bytes must always equal the
        // verified, index-recorded digest.
        let current_sha = plugin_dir_sha256(&staged.staging_dir).map_err(|error| {
            anyhow::anyhow!(
                "failed re-digesting staged plugin '{}' version '{}': {error}",
                staged.manifest.id,
                staged.manifest.version
            )
        })?;
        if current_sha != staged.snapshot_sha256 {
            bail!(
                "plugin '{}' version '{}' staged snapshot changed during publication (code=plugin_staging_mutated)",
                staged.manifest.id,
                staged.manifest.version
            );
        }

        // Descriptor-anchored, no-clobber rename from the private staging root
        // into the final install location. A same-user racer that swaps a
        // parent path for a symlink cannot redirect the rename, and an existing
        // destination is never silently replaced.
        match rename_staged_dir_no_replace(&staged.staging_dir, &staged.target_dir) {
            Ok(()) => {
                // Confirm the *published* bytes still equal the verified digest.
                // A racer that mutated the tree in the minimal window between
                // the re-digest and the rename is detected here and the
                // publication is rolled back, so the install-index digest
                // contract holds at completion.
                let published_sha = plugin_dir_sha256(&staged.target_dir).map_err(|error| {
                    anyhow::anyhow!(
                        "failed verifying published plugin '{}' version '{}': {error}",
                        staged.manifest.id,
                        staged.manifest.version
                    )
                })?;
                if published_sha != staged.snapshot_sha256 {
                    let _ = fs::remove_dir_all(&staged.target_dir);
                    bail!(
                        "plugin '{}' version '{}' published bytes changed during publication; rolled back (code=plugin_staging_mutated)",
                        staged.manifest.id,
                        staged.manifest.version
                    );
                }
                rename_committed = true;
            }
            Err(error) if is_already_exists(&error) => {
                // A retry of an interrupted publication where the target already
                // exists. Reconcile only when it is byte-identical to the
                // verified snapshot; any other content fails closed.
                reconcile_existing_target(staged)?;
                rename_committed = true;
            }
            Err(error) => return Err(error),
        }
        check_test_fault(FAULT_PLUGIN_AFTER_RENAME)?;

        // Sync the final parent(s) so the rename is durable.
        if let Some(parent) = staged.target_dir.parent() {
            fsync_directory(parent)
                .with_context(|| format!("failed syncing plugin root: {}", parent.display()))?;
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        if !rename_committed && !index_existed {
            let _ = remove_install_index_entry(
                &staged.home_dir,
                &staged.manifest.id,
                &staged.manifest.version,
            );
        }
        return Err(error);
    }
    let report = PluginInstallReport {
        installed: InstalledPlugin {
            id: staged.manifest.id.clone(),
            version: staged.manifest.version.clone(),
            dir: staged.target_dir.clone(),
        },
        prerequisites: staged.prerequisites.clone(),
    };
    Ok(report)
}

/// Verifies that a concurrently-published target is byte-identical to the
/// verified snapshot, so an interrupted publication can be reconciled without
/// ever accepting attacker or racer bytes.
fn reconcile_existing_target(staged: &StagedPluginPackage) -> Result<()> {
    let installed_sha = plugin_dir_sha256(&staged.target_dir).map_err(|error| {
        anyhow::anyhow!(
            "failed digesting concurrently installed plugin '{}' version '{}': {error}",
            staged.manifest.id,
            staged.manifest.version
        )
    })?;
    if installed_sha != staged.snapshot_sha256 {
        bail!(
            "plugin '{}' version '{}' was concurrently installed with different content; refusing to replace it (code=plugin_publish_clobber_rejected)",
            staged.manifest.id,
            staged.manifest.version
        );
    }
    Ok(())
}

/// Copies a plugin source package into `dst` reading every component through
/// directory descriptors opened with `O_NOFOLLOW`.
///
/// On platforms without `openat` the copy fails closed: a package can never be
/// installed by a traversal that cannot anchor every component.
fn copy_plugin_files_descriptor_anchored(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir(dst)
        .with_context(|| format!("failed creating snapshot dir: {}", dst.display()))?;
    let entries = collect_plugin_files(src)?;
    for entry in &entries {
        let target_path = dst.join(&entry.relative_path);
        let bytes = read_plugin_file_descriptor_anchored(src, &entry.relative_path)?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating snapshot dir: {}", parent.display()))?;
        }
        fs::write(&target_path, &bytes)
            .with_context(|| format!("failed writing snapshot file: {}", target_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::symlink_metadata(&entry.source_path)
                .map(|metadata| metadata.permissions().mode() & 0o7777)
                .unwrap_or(0o644);
            fs::set_permissions(&target_path, fs::Permissions::from_mode(mode)).with_context(
                || {
                    format!(
                        "failed setting snapshot permissions: {}",
                        target_path.display()
                    )
                },
            )?;
        }
    }
    Ok(())
}

/// Reads one regular plugin file through descriptor-anchored traversal,
/// verifying that every parent component is a directory and the final
/// component is a regular file, never a symlink or special object.
fn read_plugin_file_descriptor_anchored(root: &Path, relative: &str) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let components: Vec<&std::ffi::OsStr> = Path::new(relative)
            .components()
            .map(|component| match component {
                std::path::Component::Normal(name) => Ok(name),
                other => bail!("plugin path contains non-normal component: {other:?}"),
            })
            .collect::<Result<_>>()?;
        let Some((last, parents)) = components.split_last() else {
            bail!("plugin path is empty");
        };

        let mut dir_fd = open_dir_no_follow(root)?;
        for parent in parents {
            dir_fd = open_subdir_no_follow(dir_fd.as_raw_fd(), parent)?;
        }

        let name = CString::new(last.as_bytes())
            .map_err(|_| anyhow::anyhow!("plugin path component is not NUL-free"))?;
        let mut file = open_at_no_follow(dir_fd.as_raw_fd(), &name, libc::O_NONBLOCK)?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed inspecting plugin file: {}", relative))?;
        if metadata.file_type().is_symlink() {
            bail!("plugin package contains unsupported symlink: {}", relative);
        }
        if !metadata.is_file() {
            bail!(
                "plugin package contains unsupported non-regular file: {}",
                relative
            );
        }
        use std::io::Read;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes)
            .with_context(|| format!("failed reading plugin file: {}", relative))?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative);
        bail!(
            "plugin package staging requires descriptor-based traversal and is unsupported on this platform"
        )
    }
}

#[cfg(unix)]
fn open_at_no_follow(
    dir_fd: libc::c_int,
    name: &std::ffi::CStr,
    extra_flags: libc::c_int,
) -> Result<fs::File> {
    use std::os::unix::io::FromRawFd;

    // SAFETY: openat resolves `name` relative to the real directory descriptor
    // `dir_fd` (opened by open_dir_no_follow) and `name` is a NUL-terminated C
    // string built via StringC, so both arguments are valid. The read-only
    // O_RDONLY|O_NOFOLLOW|O_CLOEXEC flags cannot redirect or linger, and a
    // negative return is handled by the caller.
    let fd = unsafe {
        libc::openat(
            dir_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | extra_flags,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed opening plugin path component: {}",
                name.to_string_lossy()
            )
        });
    }
    // SAFETY: reached only when fd >= 0 (the caller returns early on fd < 0), so
    // `fd` is a fresh, still-uncwrapped OS descriptor and File::from_raw_fd takes
    // sole ownership of it; nothing else holds or duplicates the descriptor.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_dir_no_follow(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let file = options.open(path).with_context(|| {
        format!(
            "plugin package directory is missing or is a symlink: {}",
            path.display()
        )
    })?;
    verify_open_dir(&file, path)?;
    Ok(file)
}

#[cfg(unix)]
fn open_subdir_no_follow(dir_fd: libc::c_int, name: &std::ffi::OsStr) -> Result<fs::File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    let name = CString::new(name.as_bytes())
        .map_err(|_| anyhow::anyhow!("plugin directory component is not NUL-free"))?;
    // SAFETY: openat resolves `name` relative to the real directory descriptor
    // `dir_fd`, and `name` is a valid NUL-free C string produced by CString::new.
    // The O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC flags only read, refuse symlinks,
    // and close-on-exec; a negative return is handled by the caller.
    let fd = unsafe {
        libc::openat(
            dir_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed opening plugin subdirectory: {}",
                name.to_string_lossy()
            )
        });
    }
    // SAFETY: reached only after the caller returns on fd < 0, so `fd` is a fresh
    // descriptor not yet wrapped elsewhere; File::from_raw_fd takes sole ownership
    // of it, and verify_open_dir below consumes the resulting File.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    verify_open_dir(&file, Path::new(&name.to_string_lossy().as_ref()))?;
    Ok(file)
}

#[cfg(unix)]
fn verify_open_dir(file: &fs::File, path: &Path) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("failed inspecting plugin directory: {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "plugin package contains unsupported symlink: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "plugin package directory component is not a directory: {}",
            path.display()
        );
    }
    Ok(())
}

/// Synces `directory` so a rename inside it is durable. On non-Unix targets
/// this is a no-op because directory `sync_all` is not portable.
fn fsync_directory(directory: &Path) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = directory;
        return Ok(());
    }
    #[cfg(unix)]
    {
        let dir = fs::File::open(directory)
            .with_context(|| format!("failed opening directory: {}", directory.display()))?;
        dir.sync_all()
            .with_context(|| format!("failed syncing directory: {}", directory.display()))
    }
}

/// Syncs exactly one regular file so its bytes are durable before publication.
fn fsync_file(path: &Path) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = path;
        return Ok(());
    }
    #[cfg(unix)]
    {
        let file = fs::File::open(path)
            .with_context(|| format!("failed opening file for sync: {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed syncing file: {}", path.display()))
    }
}

/// Syncs every regular file in a directory tree followed by the tree itself, so
/// the renamed snapshot is durable from the moment its parent is published.
fn sync_snapshot_tree(dir: &Path) -> Result<()> {
    for entry in collect_plugin_files(dir)? {
        fsync_file(&entry.source_path)?;
    }
    fsync_directory(dir)
}

/// Returns the detached-signature sibling path for a plugin directory,
/// matching `signature_sidecar_path` semantics: `<dir>.sig` beside the dir.
fn signature_sibling(dir: &Path) -> PathBuf {
    let name = dir
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.with_file_name(format!("{name}.sig"))
}

/// Publishes `bytes` to `path` without ever clobbering an existing file with
/// different content. An existing byte-identical file is reconciled (the retry
/// of an interrupted publication); anything else fails closed. The write goes
/// through a synced sibling temp file and an atomic rename plus directory sync.
fn publish_no_clobber_sidecar(path: &Path, bytes: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!(
            "plugin signature sidecar '{}' has no parent directory",
            path.display()
        );
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("failed creating plugin signature dir: {}", parent.display()))?;
    if path.exists() {
        let existing = fs::read(path)
            .with_context(|| format!("failed reading existing signature: {}", path.display()))?;
        if existing != bytes {
            bail!(
                "plugin signature sidecar already exists with different content: {}",
                path.display()
            );
        }
        return Ok(());
    }
    let Some(name) = path.file_name() else {
        bail!(
            "plugin signature sidecar '{}' has no file name",
            path.display()
        );
    };
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        fs::write(&temp, bytes)
            .with_context(|| format!("failed writing signature temp: {}", temp.display()))?;
        fsync_file(&temp)?;
        fs::rename(&temp, path)
            .with_context(|| format!("failed publishing plugin signature '{}'", path.display()))?;
        fsync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Descriptor-anchored, no-clobber rename of the staged snapshot directory into
/// its final install location.
///
/// Both the source parent (`.staging/`) and the destination parent
/// (`<plugins>/<id>/`) are opened as real directories (`O_DIRECTORY |
/// O_NOFOLLOW`) and the rename is performed relative to those descriptors with
/// no-replace semantics, so a same-user racer that swaps a parent path for a
/// symlink cannot redirect the rename and an existing destination is never
/// silently overwritten.
fn rename_staged_dir_no_replace(staging_dir: &Path, target_dir: &Path) -> Result<()> {
    let src_parent = staging_dir.parent().ok_or_else(|| {
        anyhow::anyhow!("staged snapshot has no parent: {}", staging_dir.display())
    })?;
    let dst_parent = target_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target has no parent: {}", target_dir.display()))?;
    let src_name = staging_dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("staged snapshot has no name: {}", staging_dir.display()))?;
    let dst_name = target_dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("target has no name: {}", target_dir.display()))?;

    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::AsRawFd;

        let src_fd = open_dir_no_follow(src_parent)?;
        let dst_fd = open_dir_no_follow(dst_parent)?;
        let src_c = CString::new(src_name.as_bytes()).map_err(|_| {
            anyhow::anyhow!(
                "staged snapshot name is not NUL-free: {}",
                staging_dir.display()
            )
        })?;
        let dst_c = CString::new(dst_name.as_bytes()).map_err(|_| {
            anyhow::anyhow!("target name is not NUL-free: {}", target_dir.display())
        })?;

        #[cfg(target_os = "linux")]
        {
            // SAFETY: syscall(SYS_renameat2, ...) renames relative to the two raw
            // directory descriptors returned by open_dir_no_follow, using the two
            // NUL-free C strings. RENAME_NOREPLACE forbids overwriting an existing
            // target, and the caller checks rc for the no-replace failure result.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    src_fd.as_raw_fd(),
                    src_c.as_ptr(),
                    dst_fd.as_raw_fd(),
                    dst_c.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if rc < 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!(
                        "failed publishing plugin directory '{}'",
                        target_dir.display()
                    )
                });
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: renameatx_np moves the two names relative to the two directory
            // descriptors and the NUL-free C strings; RENAME_EXCL forbids overwriting an
            // existing destination, and the return value is checked against zero.
            let rc = unsafe {
                libc::renameatx_np(
                    src_fd.as_raw_fd(),
                    src_c.as_ptr(),
                    dst_fd.as_raw_fd(),
                    dst_c.as_ptr(),
                    libc::RENAME_EXCL,
                )
            };
            if rc < 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!(
                        "failed publishing plugin directory '{}'",
                        target_dir.display()
                    )
                });
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Portable fallback without an atomic no-replace primitive: verify
            // through the anchored descriptor that the destination is absent,
            // then rename relative to the two opened parents.
            if dir_entry_exists(dst_fd.as_raw_fd(), &dst_c) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("target '{}' already exists", target_dir.display()),
                )
                .into());
            }
            // SAFETY: renameat is relative to the two valid directory descriptors and
            // NUL-free C strings; the destination was already confirmed absent by the
            // anchored dir_entry_exists check above, and the result is checked.
            let rc = unsafe {
                libc::renameat(
                    src_fd.as_raw_fd(),
                    src_c.as_ptr(),
                    dst_fd.as_raw_fd(),
                    dst_c.as_ptr(),
                )
            };
            if rc < 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!(
                        "failed publishing plugin directory '{}'",
                        target_dir.display()
                    )
                });
            }
            Ok(())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (src_name, dst_name);
        bail!(
            "plugin publication requires descriptor-anchored rename and is unsupported on this platform"
        )
    }
}

/// Reports whether an `io::Error` represents a destination-already-exists
/// condition (the no-clobber publication refused to overwrite an entry).
fn is_already_exists(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn dir_entry_exists(dir_fd: libc::c_int, name: &std::ffi::CStr) -> bool {
    // SAFETY: zeroed() fully initializes the libc::stat buffer before fstatat writes
    // into it, so no uninitialized memory is read when the result is inspected.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstatat resolves `name` relative to the valid directory fd and the
    // AT_SYMLINK_NOFOLLOW flag prevents following a symlink; `stat` is a correctly
    // sized output buffer and no ownership is transferred.
    let rc = unsafe { libc::fstatat(dir_fd, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
    rc == 0
}

fn unique_staging_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed creating directory: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed setting private mode on: {}", path.display()))?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_fixture(dir: &Path) {
        fs::create_dir_all(dir.join("schemas")).expect("schemas");
        fs::write(dir.join("schemas/input.json"), "{}").expect("input");
        fs::write(dir.join("schemas/output.json"), "{}").expect("output");
        fs::write(dir.join("plugin.sh"), "#!/bin/sh\nexit 0\n").expect("script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join("plugin.sh"), fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        fs::write(
            dir.join("plugin.yaml"),
            r#"schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: plugin.sh
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
    }

    #[test]
    fn staged_snapshot_digest_matches_source_and_publishes_exact_bytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        let staged = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage package");
        assert_eq!(
            staged.snapshot_sha256(),
            plugin_dir_sha256(fixture.path()).expect("source digest")
        );
        let report = publish_staged_plugin(staged).expect("publish staged");
        assert_eq!(report.installed.id, "shell");
        assert!(report.installed.dir.join("plugin.yaml").exists());
        assert_eq!(
            plugin_dir_sha256(&report.installed.dir).expect("installed digest"),
            plugin_dir_sha256(fixture.path()).expect("source digest")
        );
        // The install index records the exact verified snapshot digest.
        let index_path = home.join("plugins/install-index.json");
        let index: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&index_path).expect("read index"))
                .expect("parse index");
        assert_eq!(
            index["entries"]["shell@v1"],
            staged_snapshot_digest_after_publish(report.installed.dir.as_path()),
        );
    }

    fn staged_snapshot_digest_after_publish(dir: &Path) -> serde_json::Value {
        serde_json::Value::String(plugin_dir_sha256(dir).expect("installed digest"))
    }

    #[cfg(unix)]
    #[test]
    fn staged_install_rejects_source_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        symlink(
            fixture.path().join("plugin.sh"),
            fixture.path().join("linked.sh"),
        )
        .expect("symlink");
        let manifest = fs::read_to_string(fixture.path().join("plugin.yaml")).expect("manifest");
        fs::write(
            fixture.path().join("plugin.yaml"),
            manifest.replace(
                "    - schemas/input.json\n",
                "    - schemas/input.json\n    - linked.sh\n",
            ),
        )
        .expect("manifest");

        let error = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect_err("symlink must be rejected");
        assert!(format!("{error:#}").contains("symlink"));
        // Nothing is published when staging fails.
        assert!(!home.join("plugins/shell/v1").exists());
        assert_eq!(crate::list_plugins_in_home(&home).expect("list").len(), 0);
    }

    #[test]
    fn staging_cleans_up_on_abandon() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        let staged = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage package");
        assert!(staged.snapshot_dir().exists());
        // Dropping without publishing removes the private snapshot.
        drop(staged);
        assert!(!home.join("plugins/.staging").exists());
        assert_eq!(crate::list_plugins_in_home(&home).expect("list").len(), 0);
    }

    #[test]
    fn publish_rejects_already_installed_version() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        let manifest = fixture.path().join("plugin.yaml");
        let first = stage_plugin_package_in_home(&home, &manifest).expect("stage first");
        publish_staged_plugin(first).expect("publish first");

        let second = stage_plugin_package_in_home(&home, &manifest).expect("stage second");
        let error = publish_staged_plugin(second).expect_err("duplicate must fail");
        assert!(error.to_string().contains("already installed"));
        // The abandoned snapshot is cleaned up.
        assert!(!home.join("plugins/.staging").exists());
    }

    fn index_entry(home: &Path, id: &str, version: &str) -> Option<String> {
        let index_path = home.join("plugins/install-index.json");
        let Ok(raw) = fs::read_to_string(&index_path) else {
            return None;
        };
        let index: serde_json::Value = serde_json::from_str(&raw).expect("parse index");
        index["entries"][format!("{id}@{version}")]
            .as_str()
            .map(|value| value.to_string())
    }

    fn installed_digest(home: &Path) -> String {
        crate::plugin_dir_sha256(&home.join("plugins/shell/v1")).expect("installed digest")
    }

    #[test]
    fn publish_rejects_post_staging_mutation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        let staged = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage package");
        let expected = staged.snapshot_sha256().to_string();
        // A same-user racer mutates the private staged snapshot after the digest
        // and signature were verified.
        fs::write(staged.snapshot_dir().join("plugin.sh"), "tampered").expect("mutate staged");

        let error = publish_staged_plugin(staged).expect_err("mutated snapshot must be rejected");
        assert!(error.to_string().contains("plugin_staging_mutated"));

        // Nothing is installed and no index entry survives the failed publish.
        assert!(!home.join("plugins/shell/v1").exists());
        assert!(index_entry(&home, "shell", "v1").is_none());
        assert_eq!(crate::list_plugins_in_home(&home).expect("list").len(), 0);
        // Sanity: an untouched snapshot of the same source publishes fine.
        let clean = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("restage");
        assert_eq!(clean.snapshot_sha256(), expected);
    }

    #[test]
    fn trusted_publish_recovers_from_crash_after_sidecar() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        let sidecar = b"transist.signature.v1 envelope".to_vec();

        let first = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage first");
        fault_hooks::set(FAULT_PLUGIN_AFTER_SIDECAR);
        let error =
            publish_staged_plugin_with_sidecar(first, Some(sidecar.clone())).expect_err("crash");
        assert!(
            error
                .to_string()
                .contains("injected plugin publication fault")
        );

        // Crash after the sidecar was written but before index/package: the
        // sidecar exists but the package is not installed and is not indexed.
        assert!(home.join("plugins/shell/v1.sig").exists());
        assert!(!home.join("plugins/shell/v1").exists());
        assert!(index_entry(&home, "shell", "v1").is_none());

        // A retry reconciles to a fully consistent, signed install.
        let second = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage second");
        let report = publish_staged_plugin_with_sidecar(second, Some(sidecar.clone()))
            .expect("retry must reconcile");
        assert!(report.installed.dir.join("plugin.yaml").exists());
        assert!(home.join("plugins/shell/v1.sig").exists());
        assert_eq!(
            fs::read(home.join("plugins/shell/v1.sig")).expect("sidecar"),
            sidecar
        );
        assert_eq!(
            index_entry(&home, "shell", "v1").expect("indexed"),
            installed_digest(&home)
        );
    }

    #[test]
    fn trusted_publish_recovers_from_crash_after_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        let sidecar = b"envelope".to_vec();

        let first = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage first");
        fault_hooks::set(FAULT_PLUGIN_AFTER_INDEX);
        let error =
            publish_staged_plugin_with_sidecar(first, Some(sidecar.clone())).expect_err("crash");
        assert!(
            error
                .to_string()
                .contains("injected plugin publication fault")
        );

        // Crash after the index was recorded but before the rename: the fresh
        // index entry is rolled back so no phantom entry survives.
        assert!(!home.join("plugins/shell/v1").exists());
        assert!(index_entry(&home, "shell", "v1").is_none());

        // A retry reconciles cleanly.
        let second = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage second");
        publish_staged_plugin_with_sidecar(second, Some(sidecar)).expect("retry must reconcile");
        assert!(home.join("plugins/shell/v1/plugin.yaml").exists());
        assert!(home.join("plugins/shell/v1.sig").exists());
        assert_eq!(
            index_entry(&home, "shell", "v1").expect("indexed"),
            installed_digest(&home)
        );
    }

    #[test]
    fn trusted_publish_crash_after_rename_leaves_consistent_install() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        let sidecar = b"envelope".to_vec();

        let first = stage_plugin_package_in_home(&home, &fixture.path().join("plugin.yaml"))
            .expect("stage first");
        fault_hooks::set(FAULT_PLUGIN_AFTER_RENAME);
        let error =
            publish_staged_plugin_with_sidecar(first, Some(sidecar.clone())).expect_err("crash");

        // The rename had already committed: even though the call returned an
        // error, the package is installed AND indexed AND signed. No
        // installed-but-unindexed or installed-but-unsigned state can exist.
        assert!(home.join("plugins/shell/v1/plugin.yaml").exists());
        assert!(home.join("plugins/shell/v1.sig").exists());
        assert_eq!(
            fs::read(home.join("plugins/shell/v1.sig")).expect("sidecar"),
            sidecar
        );
        assert_eq!(
            index_entry(&home, "shell", "v1").expect("indexed"),
            installed_digest(&home)
        );
        assert!(
            error
                .to_string()
                .contains("injected plugin publication fault")
        );
    }
}
