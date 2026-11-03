// SPDX-License-Identifier: MPL-2.0
//! Immutable packaging of signed workflow definition snapshots.

#![allow(unsafe_code)]

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::Builder;
use transist_compiler::{capture_workflow_definition, is_reserved_test_workflow_version};

use crate::{export_public_key, sign_workflow_snapshot, verify_workflow_snapshot};

/// Result of creating one immutable, signed workflow package.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowPackageResult {
    /// Directory atomically published for this package.
    pub output: PathBuf,
    /// Packaged top-level workflow definition.
    pub workflow: PathBuf,
    /// Detached signature covering the complete package asset graph.
    pub signature: PathBuf,
    /// Stable workflow identifier.
    pub workflow_id: String,
    /// Immutable packaged workflow version.
    pub workflow_version: String,
    /// Canonical definition digest signed for the package.
    pub digest: String,
}

/// Captures, versions, signs, self-verifies, and atomically publishes a workflow package.
///
/// The source workflow is never modified. The output contains only the validated
/// dependency graph and a detached signature sidecar for its top-level workflow.
pub fn package_workflow(
    workflow: &Path,
    version: &str,
    key: &Path,
    output: &Path,
) -> Result<WorkflowPackageResult> {
    validate_package_version(version)?;
    if output.exists() {
        bail!(
            "workflow package destination already exists: {}",
            output.display()
        );
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "workflow package destination has no parent: {}",
                output.display()
            )
        })?;
    #[cfg(unix)]
    let _parent_anchor = create_dir_all_no_follow(parent)?;
    #[cfg(not(unix))]
    fs::create_dir_all(parent)
        .with_context(|| format!("failed creating package parent: {}", parent.display()))?;

    let capture = capture_workflow_definition(workflow)?;
    let source_name = workflow
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("workflow '{}' has no file name", workflow.display()))?;
    let source_name = source_name.to_string_lossy().to_string();
    let top_level = capture
        .assets
        .iter()
        .find(|asset| asset.relative_path == source_name)
        .ok_or_else(|| {
            anyhow::anyhow!("workflow capture is missing top-level asset '{source_name}'")
        })?;
    let rewritten =
        capture.with_top_level_bytes(rewrite_top_level_version(&top_level.bytes, version)?)?;
    let rewritten_flow = rewritten.top_level_flow()?;
    if rewritten_flow.version != version {
        bail!("rewritten workflow version is not canonical '{version}'");
    }
    transist_compiler::validate_workflow_snapshot(&rewritten)
        .context("packaged workflow failed semantic validation")?;
    let workflow_id = rewritten_flow.id;

    let temporary = Builder::new()
        .prefix(".transist-workflow-package-")
        .tempdir_in(parent)
        .context("failed creating workflow package staging directory")?;
    let staged = temporary.path().join("package");
    fs::create_dir(&staged).context("failed creating workflow package staging area")?;
    for asset in &rewritten.assets {
        let target = staged.join(&asset.relative_path);
        let target_parent = target
            .parent()
            .expect("relative package asset has a parent");
        fs::create_dir_all(target_parent).with_context(|| {
            format!(
                "failed creating package asset directory: {}",
                target_parent.display()
            )
        })?;
        write_and_sync(&target, &asset.bytes)?;
    }

    let staged_workflow = staged.join(&source_name);
    let signature = transist_core::signature_sidecar_path(&staged_workflow);
    let signed = sign_workflow_snapshot(&rewritten, key, &signature)?;
    sync_file(&signature)?;

    let verification_dir = Builder::new()
        .prefix(".transist-workflow-package-verify-")
        .tempdir_in(parent)
        .context("failed creating package verification directory")?;
    let trusted_key = verification_dir.path().join("package.pub");
    export_public_key(key, &trusted_key)?;
    let verification = verify_workflow_snapshot(&rewritten, &signature, &trusted_key)?;
    if !verification.verified {
        bail!(
            "packaged workflow signature self-verification failed: {}",
            verification.reason_code
        );
    }
    sync_tree_directories(&staged)?;
    rename_staged_dir_no_replace(&staged, output).with_context(|| {
        format!(
            "failed atomically publishing workflow package to {}",
            output.display()
        )
    })?;
    sync_directory(parent)?;

    Ok(WorkflowPackageResult {
        output: output.to_path_buf(),
        workflow: output.join(source_name),
        signature: output.join(signature.file_name().expect("signature has filename")),
        workflow_id,
        workflow_version: version.to_string(),
        digest: signed.digest,
    })
}

fn validate_package_version(version: &str) -> Result<()> {
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::WorkflowVersion,
        version,
    )
    .context("invalid canonical workflow package version")?;
    if is_reserved_test_workflow_version(version) {
        bail!("workflow package refuses reserved test version '{version}'");
    }
    Ok(())
}

fn rewrite_top_level_version(bytes: &[u8], version: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes).context("workflow is not UTF-8")?;
    let mut replaced = false;
    let mut output = String::with_capacity(text.len() + version.len());
    for line in text.split_inclusive('\n') {
        if !replaced && line.strip_prefix("version:").is_some() {
            output.push_str("version: ");
            output.push_str(version);
            output.push('\n');
            replaced = true;
        } else {
            output.push_str(line);
        }
    }
    if !replaced {
        bail!("workflow package requires a top-level version field");
    }
    Ok(output.into_bytes())
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("failed creating package asset: {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed writing package asset: {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed syncing package asset: {}", path.display()))
}

fn sync_file(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("failed opening package file for sync: {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed syncing package file: {}", path.display()))
}

fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| {
            format!(
                "failed opening package directory for sync: {}",
                path.display()
            )
        })?
        .sync_all()
        .with_context(|| format!("failed syncing package directory: {}", path.display()))
}

fn sync_tree_directories(root: &Path) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_directories(&entry.path(), &mut directories)?;
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn collect_directories(path: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    output.push(path.to_path_buf());
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_directories(&entry.path(), output)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn rename_staged_dir_no_replace(source: &Path, target: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let source_parent = source
        .parent()
        .context("package staging directory has no parent")?;
    let target_parent = target
        .parent()
        .context("package destination has no parent")?;
    let source_dir = open_dir_no_follow(source_parent)?;
    let target_dir = open_dir_no_follow(target_parent)?;
    let source_name = CString::new(
        source
            .file_name()
            .context("package staging name missing")?
            .as_bytes(),
    )?;
    let target_name = CString::new(
        target
            .file_name()
            .context("package destination name missing")?
            .as_bytes(),
    )?;

    #[cfg(target_os = "linux")]
    // SAFETY: both directory descriptors and NUL-terminated names remain live for the syscall.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_dir.as_raw_fd(),
            source_name.as_ptr(),
            target_dir.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as libc::c_int
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both directory descriptors and NUL-terminated names remain live for the call.
    let result = unsafe {
        libc::renameatx_np(
            source_dir.as_raw_fd(),
            source_name.as_ptr(),
            target_dir.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    compile_error!("workflow package publication requires atomic no-replace rename support");

    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .context("atomic no-clobber package publication failed");
    }
    Ok(())
}

#[cfg(unix)]
fn open_dir_no_follow(path: &Path) -> Result<fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    #[cfg(target_os = "macos")]
    let absolute = {
        let mut normalized = absolute;
        for alias in [Path::new("/tmp"), Path::new("/var")] {
            if normalized.starts_with(alias)
                && fs::symlink_metadata(alias)?.file_type().is_symlink()
            {
                normalized = fs::canonicalize(alias)?.join(normalized.strip_prefix(alias)?);
                break;
            }
        }
        normalized
    };
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => normalized.push(name),
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    bail!("package directory path escapes the filesystem root");
                }
            }
            std::path::Component::Prefix(_) => {
                bail!("package directory path contains an unsupported prefix")
            }
        }
    }
    let mut directory = fs::File::open("/")?;
    for component in normalized.components() {
        let name = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => name,
            _ => bail!(
                "package directory path contains an unsafe component: {}",
                path.display()
            ),
        };
        let name = CString::new(name.as_bytes()).context("package directory path contains NUL")?;
        // SAFETY: `directory` is live and `name` is one NUL-terminated path component.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "package directory path contains a missing, non-directory, or symlink component: {}",
                    path.display()
                )
            });
        }
        // SAFETY: `fd` is a fresh owned descriptor returned by `openat` above.
        directory = unsafe { fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(unix)]
fn create_dir_all_no_follow(path: &Path) -> Result<fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    #[cfg(target_os = "macos")]
    let absolute = {
        let mut normalized = absolute;
        for alias in [Path::new("/tmp"), Path::new("/var")] {
            if normalized.starts_with(alias)
                && fs::symlink_metadata(alias)?.file_type().is_symlink()
            {
                normalized = fs::canonicalize(alias)?.join(normalized.strip_prefix(alias)?);
                break;
            }
        }
        normalized
    };
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => normalized.push(name),
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    bail!("package directory path escapes the filesystem root");
                }
            }
            std::path::Component::Prefix(_) => {
                bail!("package directory path contains an unsupported prefix")
            }
        }
    }
    let mut directory = fs::File::open("/")?;
    for component in normalized.components() {
        let name = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => name,
            std::path::Component::ParentDir => unreachable!("path was lexically normalized"),
            std::path::Component::Prefix(_) => {
                bail!("package directory path contains an unsupported prefix")
            }
        };
        let name = CString::new(name.as_bytes()).context("package directory path contains NUL")?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `directory` is live and `name` is one NUL-terminated path component.
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            // SAFETY: `directory` is anchored and `name` is one normal path component.
            let created = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o777) };
            if created != 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!(
                        "failed creating package directory '{}': {}",
                        name.to_string_lossy(),
                        path.display()
                    )
                });
            }
            directory.sync_all()?;
            // SAFETY: `directory` is live and `name` is one NUL-terminated path component.
            fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "package directory path contains a missing, non-directory, or symlink component: {}",
                    path.display()
                )
            });
        }
        // SAFETY: `fd` is a fresh owned descriptor returned by `openat` above.
        directory = unsafe { fs::File::from_raw_fd(fd) };
        directory.sync_all()?;
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn rename_staged_dir_no_replace(_source: &Path, _target: &Path) -> Result<()> {
    bail!("workflow package publication requires atomic no-replace rename support")
}
