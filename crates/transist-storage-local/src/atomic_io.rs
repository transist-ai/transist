// SPDX-License-Identifier: MPL-2.0
//! Atomic file-write helpers for the local storage backend.

// SAFETY: OS-integration: atomic file I/O uses libc fallocate/fstat/fsync for durability.
#![allow(unsafe_code)]

use crate::fault_injection::{
    FAULT_AFTER_DIRECTORY_SYNC, FAULT_AFTER_FILE_SYNC, FAULT_AFTER_FLUSH, FAULT_AFTER_LINK,
    FAULT_AFTER_RENAME, FAULT_AFTER_WRITE, FAULT_APPEND_JSONL, FAULT_WRITE_NEW_FILE,
    FAULT_WRITE_REPLACE_FILE, check_fault, run_after_temp_create_hook,
};
use anyhow::{Context, Result, bail};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Identity/version witness for an untrusted capture, excluding read-only atime.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    links: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

/// Returns an open descriptor's identity/version without resolving a host path.
#[cfg(unix)]
pub(crate) fn capture_fingerprint(file: &fs::File) -> Result<CaptureFingerprint> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(CaptureFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        links: metadata.nlink(),
        mode: metadata.mode(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

/// Reads one already-anchored regular source descriptor within the byte bound;
/// rejects hard links and returns the retained version witness for a later fence.
#[cfg(unix)]
pub(crate) fn read_capture_file(
    file: &mut fs::File,
    max_bytes: u64,
) -> Result<(Vec<u8>, CaptureFingerprint)> {
    let fingerprint = capture_fingerprint(file)?;
    if !file.metadata()?.is_file() || fingerprint.links != 1 || fingerprint.size > max_bytes {
        bail!("capture source is not a bounded single-link regular file");
    }
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != fingerprint.size || capture_fingerprint(file)? != fingerprint {
        bail!("file changed during bounded capture");
    }
    Ok((bytes, fingerprint))
}

/// Exclusive advisory lock held on a directory descriptor.
pub struct DirectoryLock {
    #[cfg(unix)]
    directory: fs::File,
    #[cfg(not(unix))]
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(not(unix))]
static DIRECTORY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Locks `directory` until the returned guard is dropped.
pub fn lock_directory(directory: &Path) -> Result<DirectoryLock> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let directory_file = open_directory_anchor(directory)
            .with_context(|| format!("failed opening lock directory: {}", directory.display()))?;
        // SAFETY: flock borrows the live descriptor; the guard retains ownership.
        if unsafe { libc::flock(directory_file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed locking directory: {}", directory.display()));
        }
        Ok(DirectoryLock {
            directory: directory_file,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(DirectoryLock {
            _guard: DIRECTORY_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        })
    }
}

#[cfg(unix)]
impl Drop for DirectoryLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor remains owned by this guard during unlock.
        let _ = unsafe { libc::flock(self.directory.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn unique_temp_path(parent: &Path, target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("transist-tmp");
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        timestamp ^ u128::from(counter)
    ))
}

/// Syncs exactly `directory` so its entry updates become durable. On
/// non-Unix targets this is a no-op because directory `sync_all` is not
/// portable.
pub fn sync_directory(directory: &Path) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = directory;
        return Ok(());
    }
    #[cfg(unix)]
    {
        let dir = open_directory_anchor(directory)
            .with_context(|| format!("failed opening directory: {}", directory.display()))?;
        dir.sync_all()
            .with_context(|| format!("failed syncing directory: {}", directory.display()))?;
        check_fault(FAULT_AFTER_DIRECTORY_SYNC)
    }
}

/// Opens `path` as a real directory through `O_DIRECTORY | O_NOFOLLOW`,
/// rejecting a symlink or a non-directory at `path`.
///
/// This anchors a directory so a same-user racer that replaces `path` with a
/// symlink after we open it cannot redirect subsequent name resolution: every
/// following `openat`/`renameat`/`unlinkat` resolves relative to the held
/// descriptor.
#[cfg(unix)]
fn open_directory_anchor(path: &Path) -> std::io::Result<fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let anchor_path = normalize_platform_path(path)?;
    let absolute = anchor_path.is_absolute();
    let base = if absolute { "/" } else { "." };
    let mut dir = fs::File::open(base)?;
    for (index, component) in anchor_path.components().enumerate() {
        let name = match component {
            std::path::Component::Normal(name) => name,
            std::path::Component::RootDir if absolute && index == 0 => continue,
            std::path::Component::CurDir if !absolute && index == 0 => continue,
            std::path::Component::ParentDir
            | std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::CurDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "directory path contains an unsafe component: {}",
                        path.display()
                    ),
                ));
            }
        };
        let name = CString::new(name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "directory path contains NUL",
            )
        })?;
        // SAFETY: `dir` is a live directory descriptor and `name` is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is newly returned by openat and ownership transfers once.
        dir = unsafe { fs::File::from_raw_fd(fd) };
    }
    let metadata = dir
        .metadata()
        .map_err(|error| std::io::Error::new(error.kind(), "failed inspecting directory"))?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("directory is a symlink: {}", path.display()),
        ));
    }
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path is not a directory: {}", path.display()),
        ));
    }
    Ok(dir)
}

#[cfg(unix)]
fn normalize_platform_path(path: &Path) -> std::io::Result<PathBuf> {
    // macOS exposes the temporary directory below `/var`, which is a
    // system-managed symlink to `/private/var`. Resolve that fixed prefix
    // before no-follow walks; symlinks below the prefix remain rejected.
    #[cfg(target_os = "macos")]
    {
        let macos_var = Path::new("/var");
        if path.starts_with(macos_var) && fs::symlink_metadata(macos_var)?.file_type().is_symlink()
        {
            return Ok(fs::canonicalize(macos_var)?.join(
                path.strip_prefix(macos_var)
                    .expect("path starts with the macOS var prefix"),
            ));
        }
    }

    Ok(path.to_path_buf())
}

/// Creates missing directories and syncs every newly-created entry from the
/// leaf back to the first existing ancestor.
pub fn create_dir_all_durable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;

        let anchor_path = normalize_platform_path(path)?;
        let absolute = anchor_path.is_absolute();
        let mut current = fs::File::open(if absolute { "/" } else { "." })?;
        for (index, component) in anchor_path.components().enumerate() {
            let name = match component {
                std::path::Component::Normal(name) => name,
                std::path::Component::RootDir if absolute && index == 0 => continue,
                std::path::Component::CurDir if !absolute && index == 0 => continue,
                _ => bail!(
                    "directory path contains an unsafe component: {}",
                    path.display()
                ),
            };
            let name = std::ffi::CString::new(name.as_bytes())
                .map_err(|_| anyhow::anyhow!("directory path contains NUL"))?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            // SAFETY: `current` is a live directory descriptor and `name` is NUL-terminated.
            let mut fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
            let created = if fd < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
            {
                let mkdir_result =
                    // SAFETY: `current` is anchored and `name` is one normal path component.
                    unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
                let created = if mkdir_result < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error).with_context(|| {
                            format!(
                                "failed creating directory component beneath {}",
                                path.display()
                            )
                        });
                    }
                    false
                } else {
                    current.sync_all().with_context(|| {
                        format!(
                            "failed syncing parent of newly created directory: {}",
                            path.display()
                        )
                    })?;
                    true
                };
                // SAFETY: the same anchored descriptor and component are used after mkdirat.
                fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
                created
            } else {
                false
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("failed opening directory: {}", path.display()));
            }
            // SAFETY: `fd` is newly returned by openat and transferred once.
            current = unsafe { fs::File::from_raw_fd(fd) };
            // SAFETY: `current` owns the newly created directory descriptor;
            // `fchmod` receives that live descriptor and a valid mode.
            if created && unsafe { libc::fchmod(current.as_raw_fd(), 0o700) } != 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("failed setting private directory mode: {}", path.display())
                });
            }
            if created {
                current.sync_all().with_context(|| {
                    format!("failed syncing newly created directory: {}", path.display())
                })?;
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
            .with_context(|| format!("failed creating directory: {}", path.display()))?;
        set_mode(path, 0o700)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }
}

/// Writes a new file and fails when the destination already exists.
pub fn write_new_file(path: &Path, payload: &[u8]) -> Result<()> {
    check_fault(FAULT_WRITE_NEW_FILE)?;
    let Some(parent) = path.parent() else {
        bail!("path '{}' is missing a parent directory", path.display());
    };
    create_dir_all_durable(parent)?;
    let Some(target_name) = path.file_name() else {
        bail!("path '{}' is missing a file name", path.display());
    };
    let temp_path = unique_temp_path(parent, path);
    let temp_name = temp_path
        .file_name()
        .expect("temp path is built with a file name");

    #[cfg(unix)]
    {
        // Anchor the parent directory BEFORE creating the temp file and hold
        // the descriptor through the entire write-publish cycle. Every step
        // — temp creation, write/sync, publication, cleanup, parent sync —
        // resolves relative to this one descriptor, so a same-user racer that
        // replaces the parent path with a symlink between temp creation and
        // publication cannot redirect the publication at a different directory.
        let parent_dir = open_directory_anchor(parent).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed opening publication parent as a directory: {}",
                    parent.display()
                ),
            )
        })?;
        let result = (|| -> Result<()> {
            let mut file = create_temp_anchored(&parent_dir, temp_name)
                .with_context(|| format!("failed creating temp file: {}", temp_path.display()))?;
            file.write_all(payload)
                .with_context(|| format!("failed writing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_WRITE)?;
            file.flush()
                .with_context(|| format!("failed flushing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FLUSH)?;
            file.sync_all()
                .with_context(|| format!("failed syncing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FILE_SYNC)?;
            // Test seam: a same-user racer swaps the parent path for a symlink
            // here. The held `parent_dir` descriptor anchors publication to
            // the original directory, so the swap cannot redirect the link.
            run_after_temp_create_hook();
            // Publish through the same anchored parent descriptor: `linkat`
            // relative to the held directory, so a same-user racer that replaces
            // the parent path with a symlink cannot redirect the publication.
            descriptor_anchor_link_at(&parent_dir, &file, temp_name, target_name).with_context(
                || {
                    format!(
                        "failed publishing new file '{}' from temp file",
                        path.display()
                    )
                },
            )?;
            check_fault(FAULT_AFTER_LINK)?;
            parent_dir.sync_all().with_context(|| {
                format!("failed syncing parent directory: {}", parent.display())
            })?;
            crate::fault_injection::run_durable_publication_pause(path)?;
            check_fault(FAULT_AFTER_DIRECTORY_SYNC)
        })();
        if result.is_ok() {
            cleanup_temp_anchored(&parent_dir, temp_name)?;
        } else {
            let _ = cleanup_temp_anchored(&parent_dir, temp_name);
        }
        result
    }
    #[cfg(not(unix))]
    {
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .with_context(|| format!("failed creating temp file: {}", temp_path.display()))?;
            set_mode(&temp_path, 0o600)?;
            file.write_all(payload)
                .with_context(|| format!("failed writing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_WRITE)?;
            file.flush()
                .with_context(|| format!("failed flushing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FLUSH)?;
            file.sync_all()
                .with_context(|| format!("failed syncing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FILE_SYNC)?;
            // Publish through the same anchored parent descriptor: `linkat`
            // relative to the held directory, so a same-user racer that replaces
            // the parent path with a symlink cannot redirect the publication.
            descriptor_anchor_link(parent, temp_name, target_name).with_context(|| {
                format!(
                    "failed publishing new file '{}' from temp file",
                    path.display()
                )
            })?;
            check_fault(FAULT_AFTER_LINK)?;
            sync_directory(parent)
        })();
        let _ = unlink_temp_no_follow(parent, &temp_path);
        result
    }
}

/// Publishes a new file without replacing an existing entry.
///
/// At explicitly retry-safe publication sites, an existing regular file is
/// accepted only when its bytes are identical to `payload`. Directories,
/// symlinks, and differing files remain errors.
pub fn write_new_file_if_identical(path: &Path, payload: &[u8]) -> Result<()> {
    check_fault(FAULT_WRITE_NEW_FILE)?;
    let Some(parent) = path.parent() else {
        bail!("path '{}' is missing a parent directory", path.display());
    };
    create_dir_all_durable(parent)?;
    let Some(target_name) = path.file_name() else {
        bail!("path '{}' is missing a file name", path.display());
    };
    let temp_path = unique_temp_path(parent, path);
    let temp_name = temp_path
        .file_name()
        .expect("temp path is built with a file name");

    #[cfg(unix)]
    {
        // Anchor the parent directory BEFORE creating the temp file and hold
        // the descriptor through the entire write-publish cycle (see
        // [`write_new_file`]).
        let parent_dir = open_directory_anchor(parent).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed opening publication parent as a directory: {}",
                    parent.display()
                ),
            )
        })?;
        let result = (|| -> Result<()> {
            let mut file = create_temp_anchored(&parent_dir, temp_name)
                .with_context(|| format!("failed creating temp file: {}", temp_path.display()))?;
            file.write_all(payload)
                .with_context(|| format!("failed writing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_WRITE)?;
            file.flush()
                .with_context(|| format!("failed flushing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FLUSH)?;
            file.sync_all()
                .with_context(|| format!("failed syncing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FILE_SYNC)?;
            // Test seam: see `write_new_file`. The held `parent_dir` descriptor
            // anchors publication and reconciliation to the original directory.
            run_after_temp_create_hook();

            match descriptor_anchor_link_at(&parent_dir, &file, temp_name, target_name) {
                Ok(()) => {
                    check_fault(FAULT_AFTER_LINK)?;
                    parent_dir.sync_all().with_context(|| {
                        format!("failed syncing parent directory: {}", parent.display())
                    })?;
                    crate::fault_injection::run_durable_publication_pause(path)?;
                    check_fault(FAULT_AFTER_DIRECTORY_SYNC)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Reconciliation reads the existing destination through an
                    // anchored parent descriptor (`openat` with `O_NOFOLLOW`),
                    // so the bytes that are compared are provably the bytes of
                    // the regular file that lives in an opened directory —
                    // never a symlink or a file redirected from a replaced
                    // parent. If the parent path was swapped for a symlink
                    // after we anchored, the read opens the (now symlink)
                    // parent path with `O_NOFOLLOW` and fails closed.
                    let existing = read_regular_file_at(&parent_dir, target_name, path)?;
                    if existing != payload {
                        bail!("existing publication differs: {}", path.display());
                    }
                    // Reconciliation still makes the directory entry durable
                    // for a retry that follows an ambiguous prior publication.
                    parent_dir.sync_all().with_context(|| {
                        format!("failed syncing parent directory: {}", parent.display())
                    })?;
                    check_fault(FAULT_AFTER_DIRECTORY_SYNC)?;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed publishing new file '{}' from temp file",
                            path.display()
                        )
                    });
                }
            }
            Ok(())
        })();
        if result.is_ok() {
            cleanup_temp_anchored(&parent_dir, temp_name)?;
        } else {
            let _ = cleanup_temp_anchored(&parent_dir, temp_name);
        }
        result
    }
    #[cfg(not(unix))]
    {
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .with_context(|| format!("failed creating temp file: {}", temp_path.display()))?;
            set_mode(&temp_path, 0o600)?;
            file.write_all(payload)
                .with_context(|| format!("failed writing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_WRITE)?;
            file.flush()
                .with_context(|| format!("failed flushing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FLUSH)?;
            file.sync_all()
                .with_context(|| format!("failed syncing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FILE_SYNC)?;

            match descriptor_anchor_link(parent, temp_name, target_name) {
                Ok(()) => {
                    check_fault(FAULT_AFTER_LINK)?;
                    sync_directory(parent)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = read_regular_file_no_follow(path)?;
                    if existing != payload {
                        bail!("existing publication differs: {}", path.display());
                    }
                    sync_directory(parent)?;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed publishing new file '{}' from temp file",
                            path.display()
                        )
                    });
                }
            }
            Ok(())
        })();
        let _ = unlink_temp_no_follow(parent, &temp_path);
        result
    }
}

/// Anchored rename relative to an already-open parent directory descriptor.
///
/// The publication is performed with `renameat(parent_fd, ..., parent_fd,
/// ...)`, so a same-user racer that replaces the parent path with a symlink
/// after we open it cannot redirect the rename: both names resolve relative to
/// the descriptor we hold. The temp source is verified to be a regular,
/// non-symlink file through that same descriptor before the rename. Callers
/// that already hold the parent descriptor (so the parent cannot be swapped
/// for a symlink between temp creation and publication) call this directly,
/// avoiding a second `open` that would re-resolve the parent path.
#[cfg(unix)]
fn descriptor_anchor_rename_at(
    dir: &fs::File,
    source: &fs::File,
    temp_name: &std::ffi::OsStr,
    target_name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let source_identity = regular_file_identity(source, "publication temp source")?;
    ensure_anchored_name_identity(dir, temp_name, source_identity, "publication temp source")?;

    let temp_c = std::ffi::CString::new(temp_name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "temp name has NUL"))?;
    let target_c = std::ffi::CString::new(target_name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target name has NUL")
    })?;
    // SAFETY: `dir` is a live open directory fd and `temp_c`/`target_c` are NUL-terminated CStrings built from the path names (embedded NULs rejected by `CString::new`), so `renameat` reads only valid fd/pointer args and writes no process memory.
    let rc = unsafe {
        libc::renameat(
            dir.as_raw_fd(),
            temp_c.as_ptr(),
            dir.as_raw_fd(),
            target_c.as_ptr(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Atomically renames the directory `old_name` under `old_parent` to `new_name`
/// under `new_parent`, resolving both names through anchored parent directory
/// descriptors.
///
/// Both parents are opened with `open(O_DIRECTORY | O_NOFOLLOW)` and the rename
/// is performed with `renameat(old_parent_fd, old_name, new_parent_fd,
/// new_name)`, so a same-user racer that swaps either direct parent for a
/// symlink after we open it cannot redirect the rename. The source is verified
/// to be a real directory (never a symlink) through the anchored descriptor
/// before the rename. On non-Unix targets this falls back to a path rename.
pub fn rename_dir_anchored(
    old_parent: &Path,
    old_name: &std::ffi::OsStr,
    new_parent: &Path,
    new_name: &std::ffi::OsStr,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::AsRawFd;

        let old_dir = open_directory_anchor(old_parent).with_context(|| {
            format!(
                "failed opening rename source parent as a directory: {}",
                old_parent.display()
            )
        })?;
        let new_dir = open_directory_anchor(new_parent).with_context(|| {
            format!(
                "failed opening rename destination parent as a directory: {}",
                new_parent.display()
            )
        })?;
        // The source must be a real directory, never a symlink swapped in.
        let old_name_c = std::ffi::CString::new(old_name.as_bytes())
            .map_err(|_| anyhow::anyhow!("rename source name has NUL"))?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `old_dir` is a live open directory fd, `old_name_c` is a NUL-terminated CString, and `stat.as_mut_ptr()` points at `size_of::<libc::stat>` of writable uninitialized memory that `fstatat` fully initializes on success (the buffer is never read when rc < 0).
        let rc = unsafe {
            libc::fstatat(
                old_dir.as_raw_fd(),
                old_name_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("failed inspecting rename source: {}", old_parent.display())
            });
        }
        // SAFETY: `fstatat` succeeded (rc == 0 checked above), which the libc contract guarantees fully wrote the `stat` buffer, so `assume_init` reads only initialized bytes.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            bail!(
                "rename source is not a directory: {}",
                old_parent.join(std::path::Path::new(old_name)).display()
            );
        }
        let new_name_c = std::ffi::CString::new(new_name.as_bytes())
            .map_err(|_| anyhow::anyhow!("rename destination name has NUL"))?;
        // SAFETY: `old_dir` and `new_dir` are live open directory fds and `old_name_c`/`new_name_c` are NUL-terminated CStrings, so `renameat` reads only valid fd/pointer args and writes no process memory.
        let rc = unsafe {
            libc::renameat(
                old_dir.as_raw_fd(),
                old_name_c.as_ptr(),
                new_dir.as_raw_fd(),
                new_name_c.as_ptr(),
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed renaming directory '{}' to '{}'",
                    old_parent.join(std::path::Path::new(old_name)).display(),
                    new_parent.join(std::path::Path::new(new_name)).display()
                )
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let old_path = old_parent.join(old_name);
        let new_path = new_parent.join(new_name);
        fs::rename(&old_path, &new_path).with_context(|| {
            format!(
                "failed renaming directory '{}' to '{}'",
                old_path.display(),
                new_path.display()
            )
        })
    }
}

/// Atomically replaces a file by writing to a sibling temp file and renaming it into place.
pub fn write_replace_file(path: &Path, payload: &[u8]) -> Result<()> {
    check_fault(FAULT_WRITE_REPLACE_FILE)?;
    let Some(parent) = path.parent() else {
        bail!("path '{}' is missing a parent directory", path.display());
    };
    create_dir_all_durable(parent)?;
    let temp_path = unique_temp_path(parent, path);
    let temp_name = temp_path
        .file_name()
        .expect("temp path is built with a file name");
    let target_name = path
        .file_name()
        .expect("target path is built with a file name");

    #[cfg(unix)]
    {
        // Anchor the parent directory BEFORE creating the temp file and hold
        // the descriptor through the entire write-publish cycle (see
        // [`write_new_file`]).
        let parent_dir = open_directory_anchor(parent).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed opening publication parent as a directory: {}",
                    parent.display()
                ),
            )
        })?;
        let result = (|| -> Result<()> {
            let mut file = create_temp_anchored(&parent_dir, temp_name)
                .with_context(|| format!("failed creating temp file: {}", temp_path.display()))?;
            file.write_all(payload)
                .with_context(|| format!("failed writing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_WRITE)?;
            file.flush()
                .with_context(|| format!("failed flushing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FLUSH)?;
            file.sync_all()
                .with_context(|| format!("failed syncing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FILE_SYNC)?;
            // Test seam: see `write_new_file`. The held `parent_dir` descriptor
            // anchors the replacement to the original directory.
            run_after_temp_create_hook();
            // Replace through the same anchored parent descriptor so a
            // same-user racer that swaps the parent for a symlink after we
            // write the temp cannot redirect the replacement; the temp source
            // must be a regular file.
            descriptor_anchor_rename_at(&parent_dir, &file, temp_name, target_name).with_context(
                || {
                    format!(
                        "failed renaming temp file '{}' to '{}'",
                        temp_path.display(),
                        path.display()
                    )
                },
            )?;
            check_fault(FAULT_AFTER_RENAME)?;
            parent_dir.sync_all().with_context(|| {
                format!("failed syncing parent directory: {}", parent.display())
            })?;
            check_fault(FAULT_AFTER_DIRECTORY_SYNC)
        })();
        if result.is_err() {
            let _ = cleanup_temp_anchored(&parent_dir, temp_name);
        }
        result
    }
    #[cfg(not(unix))]
    {
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .with_context(|| format!("failed creating temp file: {}", temp_path.display()))?;
            set_mode(&temp_path, 0o600)?;
            file.write_all(payload)
                .with_context(|| format!("failed writing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_WRITE)?;
            file.flush()
                .with_context(|| format!("failed flushing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FLUSH)?;
            file.sync_all()
                .with_context(|| format!("failed syncing temp file: {}", temp_path.display()))?;
            check_fault(FAULT_AFTER_FILE_SYNC)?;
            fs::rename(&temp_path, path).with_context(|| {
                format!(
                    "failed renaming temp file '{}' to '{}'",
                    temp_path.display(),
                    path.display()
                )
            })?;
            check_fault(FAULT_AFTER_RENAME)?;
            sync_directory(parent)
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }
}

/// Appends one complete JSONL record (the caller supplies the trailing
/// newline), flushes, and `sync_all`s the file. When the file is newly
/// created, the parent directory is also synced so the stream's name is
/// durable. Use this for every append to `events.jsonl` and
/// `failures.jsonl` so a crash never leaves a partially written record
/// that inspection cannot distinguish from a complete one.
pub fn append_jsonl_durable(path: &Path, line: &[u8]) -> Result<()> {
    check_fault(FAULT_APPEND_JSONL)?;
    let Some(parent) = path.parent() else {
        bail!("path '{}' is missing a parent directory", path.display());
    };
    create_dir_all_durable(parent)?;
    let created = !path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed opening append stream: {}", path.display()))?;
    set_mode(path, 0o600)?;
    file.write_all(line)
        .with_context(|| format!("failed appending record: {}", path.display()))?;
    check_fault(FAULT_AFTER_WRITE)?;
    file.flush()
        .with_context(|| format!("failed flushing append stream: {}", path.display()))?;
    check_fault(FAULT_AFTER_FLUSH)?;
    file.sync_all()
        .with_context(|| format!("failed syncing append stream: {}", path.display()))?;
    check_fault(FAULT_AFTER_FILE_SYNC)?;
    if created {
        sync_directory(parent)?;
    }
    Ok(())
}

/// Detects and repairs a single malformed non-newline-terminated final
/// JSONL fragment at the tail of `path`. If the file does not end with a
/// newline, the trailing fragment is inspected: when it parses as a
/// complete, well-formed JSON value, only the missing trailing newline is
/// appended (the record itself is preserved — a crash between writing the
/// bytes and the newline must not destroy a valid record). When the
/// fragment does not parse as a complete JSON value, it is truncated back
/// to the last complete newline-terminated record boundary. The file is
/// flushed and `sync_all`ed, and the parent directory is synced, in either
/// case. Returns `true` when a repair was performed. Returns `false` when
/// the file is absent, empty, or already well-formed. Malformed complete
/// or interior records (newline-terminated but unparseable) are not
/// repaired here; callers must reject them.
pub fn repair_jsonl_tail(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed opening stream for tail repair: {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("failed reading stream metadata: {}", path.display()))?
        .len();
    if len == 0 {
        return Ok(false);
    }
    let read_from = len.saturating_sub(1);
    file.seek(SeekFrom::Start(read_from))
        .with_context(|| format!("failed seeking stream tail: {}", path.display()))?;
    let mut last = [0u8; 1];
    let n = file.read(&mut last)?;
    if n == 0 || last[0] == b'\n' {
        return Ok(false);
    }
    // The tail is not newline-terminated. Read the whole file to find the
    // last complete newline-terminated record boundary and inspect the
    // trailing fragment.
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed seeking stream start: {}", path.display()))?;
    let mut bytes = Vec::with_capacity(len as usize);
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed reading stream for tail repair: {}", path.display()))?;
    let boundary = match bytes.iter().rposition(|&b| b == b'\n') {
        Some(pos) => pos + 1,
        None => 0,
    };
    let fragment = &bytes[boundary..];
    let fragment_is_complete_json = !fragment.iter().all(u8::is_ascii_whitespace)
        && serde_json::from_slice::<serde_json::Value>(fragment).is_ok();
    if fragment_is_complete_json {
        // Preserve the record; only the trailing newline is missing.
        file.seek(SeekFrom::End(0))
            .with_context(|| format!("failed seeking stream end: {}", path.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("failed appending missing newline: {}", path.display()))?;
    } else {
        file.set_len(boundary as u64)
            .with_context(|| format!("failed truncating stream tail: {}", path.display()))?;
    }
    check_fault(FAULT_AFTER_WRITE)?;
    file.flush()
        .with_context(|| format!("failed flushing repaired stream: {}", path.display()))?;
    check_fault(FAULT_AFTER_FLUSH)?;
    file.sync_all()
        .with_context(|| format!("failed syncing repaired stream: {}", path.display()))?;
    check_fault(FAULT_AFTER_FILE_SYNC)?;
    drop(file);
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(true)
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    let _ = (path, mode);
    Ok(())
}

/// Reads a JSONL stream and returns every record as a raw byte slice.
/// Tolerates at most one non-newline-terminated final fragment (the last
/// byte is not a newline): when that fragment parses as a complete,
/// well-formed JSON value it is retained as a record (a crash between
/// writing the bytes and the newline must not destroy a valid record);
/// when it does not parse as a complete JSON value it is discarded.
/// Newline-terminated but unparseable complete or interior records are NOT
/// dropped here; the caller gets all newline-separated slices and is
/// responsible for rejecting malformed ones.
pub fn read_jsonl_records(path: &Path) -> Result<Vec<Vec<u8>>> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed reading jsonl stream: {}", path.display()))?;
    let mut records = Vec::new();
    let mut last_end = 0usize;
    for (idx, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' {
            let record = &bytes[last_end..idx];
            if !record.iter().all(|b| b.is_ascii_whitespace()) {
                records.push(record.to_vec());
            }
            last_end = idx + 1;
        }
    }
    if last_end < bytes.len() {
        let fragment = &bytes[last_end..];
        if !fragment.iter().all(|b| b.is_ascii_whitespace())
            && serde_json::from_slice::<serde_json::Value>(fragment).is_ok()
        {
            // Preserve a complete JSON value that only lost its trailing
            // newline; discard a genuinely incomplete trailing fragment.
            records.push(fragment.to_vec());
        }
    }
    Ok(records)
}

/// Creates a fresh private temp file `temp_name` inside the anchored parent
/// directory `parent_dir` through `openat(parent_fd, temp_name, O_WRONLY |
/// O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)`.
///
/// Because the temp file is created relative to the already-open parent
/// descriptor, a same-user racer that replaces the parent *path* with a
/// symlink between the parent open and this call cannot redirect the temp
/// creation: the name resolves inside the directory we hold. `O_EXCL` rejects
/// a pre-existing entry; `O_NOFOLLOW` rejects a symlink swapped in at the temp
/// name. The mode is defensively re-asserted with `fchmod` (mirroring
/// [`set_mode`]); umask cannot widen `0o600` because it owns no group/other
/// bits, but the re-assertion keeps the contract explicit.
#[cfg(unix)]
fn create_temp_anchored(
    parent_dir: &fs::File,
    temp_name: &std::ffi::OsStr,
) -> std::io::Result<fs::File> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let name_c = std::ffi::CString::new(temp_name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "temp name has NUL"))?;
    // SAFETY: `parent_dir` is a live open directory fd and `name_c` is a NUL-terminated CString, so `openat` gets valid fd/pointer/mode args; it returns a new owned fd or a negative error value (checked below) and writes no process memory.
    let fd = unsafe {
        libc::openat(
            parent_dir.as_raw_fd(),
            name_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is the non-negative descriptor returned by the `openat` above, so it is a uniquely owned new descriptor; this `File` takes ownership and closes it on drop.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    // SAFETY: `file` owns the live descriptor just created; `fchmod` reads only the fd and the mode constant.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

/// Best-effort removal of a leftover temp file through the anchored parent
/// directory descriptor. Temp cleanup is best-effort: publication already
/// succeeded or failed before this runs, so a failed cleanup must not mask the
/// real result. The `unlinkat` resolves relative to the held descriptor, so a
/// racer that swaps the parent path for a symlink cannot redirect the cleanup
/// at a different directory.
#[cfg(unix)]
fn cleanup_temp_anchored(
    parent_dir: &fs::File,
    temp_name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let Ok(name_c) = std::ffi::CString::new(temp_name.as_bytes()) else {
        return Ok(());
    };
    // SAFETY: `parent_dir` is a live open directory fd and `name_c` is a NUL-terminated CString, so `unlinkat` receives valid fd/pointer args and a valid flag value, writing no process memory. The return value is intentionally ignored: a missing temp (already published or never created) is not an error here.
    let rc = unsafe { libc::unlinkat(parent_dir.as_raw_fd(), name_c.as_ptr(), 0) };
    if rc == 0 {
        parent_dir.sync_all()?;
    }
    Ok(())
}

/// Atomically links `temp` into `parent` as `target` through an opened parent
/// directory descriptor.
///
/// The parent is anchored with `open(O_DIRECTORY | O_NOFOLLOW)` and the
/// publication is performed with `linkat(parent_fd, ...)`, so a same-user
/// racer that replaces the parent path with a symlink after we open it cannot
/// redirect the link: the name is resolved relative to the descriptor we hold,
/// and a symlink inserted where `parent` used to be has no effect on `linkat`.
///
/// The temp source is inspected through that same anchored descriptor before
/// linking: it must be a regular file, never a symlink or a directory. This
/// rejects a temp source that a racer replaced with a symlink pointing at
/// attacker-controlled bytes, so publication can never hard-link such a
/// symlink into place.
///
/// Returns an `io::Error` with kind `AlreadyExists` when `target` exists, so
/// `write_new_file_if_identical` can reconcile through the same anchored parent.
///
/// On Unix this is a thin path-based wrapper around
/// [`descriptor_anchor_link_at`] retained for the non-Unix `fs::hard_link`
/// fallback and for tests that exercise the anchored publication by path; the
/// production Unix write paths call `descriptor_anchor_link_at` directly with a
/// descriptor held across temp creation.
#[allow(dead_code)]
fn descriptor_anchor_link(
    parent: &Path,
    temp_name: &std::ffi::OsStr,
    target_name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let dir = open_directory_anchor(parent).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed opening publication parent as a directory: {}",
                    parent.display()
                ),
            )
        })?;
        let source = open_regular_file_at(&dir, temp_name)?;
        descriptor_anchor_link_at(&dir, &source, temp_name, target_name)
    }
    #[cfg(not(unix))]
    {
        let temp_path = parent.join(temp_name);
        let target_path = parent.join(target_name);
        if temp_path.is_symlink() || !temp_path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "publication temp source is not a regular file: {}",
                    temp_path.display()
                ),
            ));
        }
        fs::hard_link(&temp_path, &target_path)
    }
}

/// Anchored `linkat` relative to an already-open parent directory descriptor.
///
/// This is the fd-based core of [`descriptor_anchor_link`]: callers that
/// already hold the parent descriptor (so the parent cannot be swapped for a
/// symlink between temp creation and publication) call this directly, avoiding
/// a second `open` that would re-resolve the parent path.
#[cfg(unix)]
fn descriptor_anchor_link_at(
    dir: &fs::File,
    source: &fs::File,
    temp_name: &std::ffi::OsStr,
    target_name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let source_identity = regular_file_identity(source, "publication temp source")?;
    ensure_anchored_name_identity(dir, temp_name, source_identity, "publication temp source")?;

    let temp_c = std::ffi::CString::new(temp_name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "temp name has NUL"))?;
    let target_c = std::ffi::CString::new(target_name.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target name has NUL")
    })?;
    // Both names resolve relative to `parent_fd`. `linkat(..., 0)` does not
    // reject a symlink *source* on its own — the pre-check above establishes
    // that `temp_name` is a regular file, and the post-link identity check
    // below proves the object that actually landed under `target_name` is
    // that exact inode, so a swap of the temp source in the window between
    // the pre-check and this `linkat` is detected and rolled back.
    // SAFETY: `dir` is a live open directory fd and `temp_c`/`target_c` are NUL-terminated CStrings, so `linkat(dir_fd, ..., dir_fd, ..., 0)` reads only valid fd/pointer args plus a valid flag value and writes no process memory.
    let rc = unsafe {
        libc::linkat(
            dir.as_raw_fd(),
            temp_c.as_ptr(),
            dir.as_raw_fd(),
            target_c.as_ptr(),
            0,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if let Err(error) =
        verify_published_identity(dir, target_name, source_identity, "published file")
    {
        // A concurrent swap landed a different object under `target_name`.
        // Roll the publication back through the same anchored descriptor so
        // no attacker-controlled bytes remain in place, then fail closed.
        // SAFETY: `dir` is a live open directory fd and `target_c` is a NUL-terminated CString, so `unlinkat` receives valid fd and pointer args and writes no process memory.
        unsafe {
            libc::unlinkat(dir.as_raw_fd(), target_c.as_ptr(), 0);
        }
        return Err(error);
    }
    Ok(())
}

/// Stable identity of a filesystem object: `(st_dev, st_ino)`. A hard link
/// shares this identity with its source, while a symlink or a different file
/// swapped in has a different identity. Used to prove that the object actually
/// published is the exact regular temp source that was verified.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
impl FileIdentity {
    /// Extracts the `(st_dev, st_ino)` identity from a `stat` buffer. The casts
    /// are platform-dependent (`st_dev`/`st_ino` are `i32`/`u64` on macOS and
    /// `u64`/`u64` on Linux), so the redundant-cast lint is suppressed here.
    #[allow(clippy::unnecessary_cast)]
    fn from_stat(stat: &libc::stat) -> Self {
        Self {
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
        }
    }
}

/// Verifies, through an anchored directory descriptor, that `name` resolves to
/// a regular file that is not a symlink, and returns its `(st_dev, st_ino)`
/// identity. `label` names the role of the file in error messages.
///
/// The lookup uses `fstatat(dir_fd, name, AT_SYMLINK_NOFOLLOW)`, so a symlink
/// (or any non-regular entry) inserted at `name` is rejected rather than
/// resolved. This closes the publication-time swap where a temp source that was
/// created as a regular file is replaced with a symlink before the link.
///
/// The returned identity lets the caller confirm — after the link/rename — that
/// the object that landed in place is the exact inode inspected here, so a swap
/// in the narrow window between this check and the publish syscall is detected
/// and fails closed rather than publishing attacker-controlled bytes.
#[cfg(unix)]
fn ensure_anchored_regular_source(
    dir: &fs::File,
    name: &std::ffi::OsStr,
    label: &str,
) -> std::io::Result<FileIdentity> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let name_c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "name has NUL"))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` is a live open directory fd, `name_c` is a NUL-terminated CString, and `stat.as_mut_ptr()` points at one `libc::stat` of writable uninitialized memory that `fstatat` fully initializes on success (never read when rc < 0).
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name_c.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fstatat` succeeded (rc == 0 checked above), which the libc contract guarantees fully initializes the `stat` buffer, so `assume_init` reads only initialized bytes.
    let stat = unsafe { stat.assume_init() };
    let mode = stat.st_mode & libc::S_IFMT;
    if mode != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{label} is not a regular file: {}",
                String::from_utf8_lossy(name.as_bytes())
            ),
        ));
    }
    Ok(FileIdentity::from_stat(&stat))
}

#[cfg(unix)]
fn regular_file_identity(file: &fs::File, label: &str) -> std::io::Result<FileIdentity> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `file` owns a live descriptor and `stat` points to writable storage.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded and initialized the buffer.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} is not a regular file"),
        ));
    }
    Ok(FileIdentity::from_stat(&stat))
}

#[cfg(unix)]
fn ensure_anchored_name_identity(
    dir: &fs::File,
    name: &std::ffi::OsStr,
    expected: FileIdentity,
    label: &str,
) -> std::io::Result<()> {
    if ensure_anchored_regular_source(dir, name, label)? != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} identity changed before publication (concurrent swap detected)"),
        ));
    }
    Ok(())
}

/// Confirms, through the held parent descriptor, that `name` now resolves to a
/// regular file whose `(st_dev, st_ino)` identity equals `expected`. Rejects a
/// symlink or any object whose identity differs from the verified temp source.
///
/// Called immediately after `linkat`/`renameat`. If a same-user racer swapped
/// the temp source for a symlink or a different file in the sub-syscall window
/// after the pre-check, the object that landed under `name` has a different
/// identity (or is a symlink), and this fails closed so the caller can roll the
/// publication back.
#[cfg(unix)]
fn verify_published_identity(
    dir: &fs::File,
    name: &std::ffi::OsStr,
    expected: FileIdentity,
    label: &str,
) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let name_c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "name has NUL"))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` is a live open directory fd, `name_c` is a NUL-terminated CString, and `stat.as_mut_ptr()` points at one `libc::stat` of writable uninitialized memory that `fstatat` fully initializes on success (never read when rc < 0).
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name_c.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fstatat` succeeded (rc == 0 checked above), which the libc contract guarantees fully initializes the `stat` buffer, so `assume_init` reads only initialized bytes.
    let stat = unsafe { stat.assume_init() };
    let actual = FileIdentity::from_stat(&stat);
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || actual != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} identity changed during publication (concurrent swap detected)"),
        ));
    }
    Ok(())
}

/// Reads a regular file through an anchored parent descriptor.
///
/// The direct parent directory is opened with `open(O_DIRECTORY | O_NOFOLLOW)`
/// and the file name is resolved with `openat(parent_fd, name, O_RDONLY |
/// O_NOFOLLOW | O_NONBLOCK)`. Every opened component is `fstat`-verified (the
/// parent is a real directory, the final component is a regular file that is
/// never a symlink), so the returned bytes are provably the bytes of the object
/// that lives in the opened directory. A parent replaced with a symlink after
/// the descriptor is opened cannot redirect the read.
pub fn read_regular_file_no_follow(path: &Path) -> Result<Vec<u8>> {
    let Some(parent) = path.parent() else {
        bail!("path '{}' is missing a parent directory", path.display());
    };
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("path '{}' is missing a file name", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::{AsRawFd, FromRawFd};

        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let dir = options.open(parent).with_context(|| {
            format!(
                "failed opening parent directory (missing or symlink): {}",
                parent.display()
            )
        })?;
        let metadata = dir
            .metadata()
            .with_context(|| format!("failed inspecting parent directory: {}", parent.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("publication parent is a symlink: {}", parent.display());
        }
        if !metadata.is_dir() {
            bail!(
                "publication parent is not a directory: {}",
                parent.display()
            );
        }

        let name_c = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| anyhow::anyhow!("file name contains NUL byte"))?;
        // SAFETY: `dir` is a live open directory fd (verified above to be a real directory) and `name_c` is a NUL-terminated CString, so `openat` gets valid args; it returns a new owned fd or a negative error value and writes no process memory.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("failed opening publication target '{}'", path.display())
            });
        }
        // SAFETY: `fd` is the descriptor returned by the `openat` above, checked non-negative, so it is a uniquely owned new descriptor; transferring it into `File` moves that ownership and the `File` closes it on drop.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let file_metadata = file
            .metadata()
            .with_context(|| format!("failed inspecting publication target: {}", path.display()))?;
        if file_metadata.file_type().is_symlink() {
            bail!("publication target is a symlink: {}", path.display());
        }
        if !file_metadata.is_file() {
            bail!(
                "publication target is not a regular file: {}",
                path.display()
            );
        }
        use std::io::Read;
        let mut bytes = Vec::with_capacity(file_metadata.len() as usize);
        file.take(file_metadata.len())
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed reading publication target: {}", path.display()))?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed inspecting file: {}", path.display()))?;
        if !metadata.file_type().is_file() {
            bail!("path is not a regular file: {}", path.display());
        }
        fs::read(path).with_context(|| format!("failed reading file: {}", path.display()))
    }
}

#[cfg(unix)]
fn read_regular_file_at(
    dir: &fs::File,
    name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = open_regular_file_at(dir, name).with_context(|| {
        format!(
            "failed opening publication target '{}'",
            display_path.display()
        )
    })?;
    let metadata = file.metadata().with_context(|| {
        format!(
            "failed inspecting publication target: {}",
            display_path.display()
        )
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(metadata.len())
        .read_to_end(&mut bytes)
        .with_context(|| {
            format!(
                "failed reading publication target: {}",
                display_path.display()
            )
        })?;
    Ok(bytes)
}

#[cfg(unix)]
fn open_regular_file_at(dir: &fs::File, name: &std::ffi::OsStr) -> std::io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "file name has NUL"))?;
    // SAFETY: `dir` is a live directory descriptor and `name` is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is newly returned by openat and transferred once.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    regular_file_identity(&file, "publication target")?;
    Ok(file)
}

/// Holds one open directory descriptor and reads files beneath it through an
/// anchored walk.
///
/// The root is opened once with `open(O_DIRECTORY | O_NOFOLLOW)` and every
/// subsequent lookup resolves relative to that held descriptor with
/// `openat(..., O_NOFOLLOW)` per component. This keeps a single root anchor
/// across many reads (e.g. all the birth files reconciled in one recovery), so
/// a same-user racer that replaces a parent or any intermediate component with
/// a symlink between reads cannot redirect any individual read: every component
/// of every path is resolved against the descriptor we held from the start and
/// verified to be a real, non-symlink directory (or, for the leaf, a regular
/// non-symlink file).
#[derive(Debug)]
pub struct AnchoredDir {
    #[cfg(unix)]
    file: fs::File,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl AnchoredDir {
    /// Borrows an already-open real directory as an independent anchor. Relative
    /// operations stay beneath that descriptor even if its host path is replaced.
    #[cfg(unix)]
    pub(crate) fn from_file(file: &fs::File) -> Result<Self> {
        if !file.metadata()?.is_dir() {
            bail!("capture anchor must be a real directory");
        }
        Ok(Self {
            file: dup_fd(file)?,
        })
    }
    /// Opens `root` as an anchored directory for subsequent relative reads.
    pub fn open(root: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            let file = open_directory_anchor(root)
                .with_context(|| format!("failed opening anchored root: {}", root.display()))?;
            Ok(Self { file })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                path: root.to_path_buf(),
            })
        }
    }

    /// Reads the regular file at `relative` beneath this anchored root.
    ///
    /// `relative` must be a path of normal components (no `..`, `.`, or
    /// platform separators beyond the platform separator). Every intermediate
    /// component is opened with `O_DIRECTORY | O_NOFOLLOW` relative to the held
    /// root descriptor and verified to be a real directory; the final component
    /// is opened with `O_NOFOLLOW` and verified to be a regular file. Any
    /// symlink at any level (including one swapped in after the root was
    /// opened) is rejected.
    pub fn read_regular(&self, relative: &Path) -> Result<Vec<u8>> {
        self.read_regular_bounded(relative, u64::MAX, false)
    }

    /// Reads at most `max_bytes` from a no-follow regular file. When requested,
    /// rejects hard links; growth/shrinkage during capture also fails closed.
    pub fn read_regular_bounded(
        &self,
        relative: &Path,
        max_bytes: u64,
        reject_hardlinks: bool,
    ) -> Result<Vec<u8>> {
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(part) => parts.push(part),
                _ => bail!(
                    "anchored read path must use only normal components: {}",
                    relative.display()
                ),
            }
        }
        if parts.is_empty() {
            bail!("anchored read path must name at least one component");
        }
        #[cfg(unix)]
        {
            use std::io::Read;
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::io::{AsRawFd, FromRawFd};

            let mut current = dup_fd(&self.file)?;
            for (index, part) in parts.iter().enumerate() {
                let is_leaf = index + 1 == parts.len();
                let name = std::ffi::CString::new(part.as_bytes())
                    .map_err(|_| anyhow::anyhow!("anchored read path component has NUL"))?;
                let flags = if is_leaf {
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC
                } else {
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
                };
                // SAFETY: `current` is a live open directory fd (a dup of the held root anchor) and `name` is a NUL-terminated CString, so `openat` gets valid args; it returns a new owned fd or a negative error value and writes no process memory.
                let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error()).with_context(|| {
                        format!(
                            "failed opening anchored component '{}' beneath '{}'",
                            String::from_utf8_lossy(part.as_bytes()),
                            relative.display()
                        )
                    });
                }
                // SAFETY: `fd` is the non-negative descriptor returned by the `openat` above, so it is a uniquely owned new descriptor; this `File` takes ownership and closes it on drop.
                let next = unsafe { fs::File::from_raw_fd(fd) };
                let metadata = next.metadata().with_context(|| {
                    format!(
                        "failed inspecting anchored component '{}'",
                        String::from_utf8_lossy(part.as_bytes())
                    )
                })?;
                if metadata.file_type().is_symlink() {
                    bail!(
                        "anchored component is a symlink: '{}'",
                        String::from_utf8_lossy(part.as_bytes())
                    );
                }
                if is_leaf {
                    if !metadata.is_file() {
                        bail!(
                            "anchored leaf is not a regular file: '{}'",
                            String::from_utf8_lossy(part.as_bytes())
                        );
                    }
                    use std::os::unix::fs::MetadataExt;
                    if metadata.len() > max_bytes || (reject_hardlinks && metadata.nlink() != 1) {
                        bail!("file exceeds document bounds or is hard-linked");
                    }
                    let mut bytes = Vec::new();
                    let mut reader = next.take(max_bytes.saturating_add(1));
                    reader.read_to_end(&mut bytes).with_context(|| {
                        format!("failed reading anchored leaf '{}'", relative.display())
                    })?;
                    let after = reader.get_ref().metadata()?;
                    if bytes.len() as u64 != metadata.len()
                        || bytes.len() as u64 > max_bytes
                        || (reject_hardlinks && after.nlink() != 1)
                        || after.len() != metadata.len()
                        || (reject_hardlinks
                            && (after.mtime() != metadata.mtime()
                                || after.mtime_nsec() != metadata.mtime_nsec()
                                || after.ctime() != metadata.ctime()
                                || after.ctime_nsec() != metadata.ctime_nsec()))
                    {
                        bail!("file changed during bounded capture");
                    }
                    return Ok(bytes);
                }
                if !metadata.is_dir() {
                    bail!(
                        "anchored intermediate is not a directory: '{}'",
                        String::from_utf8_lossy(part.as_bytes())
                    );
                }
                current = next;
            }
            unreachable!("loop always returns on the leaf")
        }
        #[cfg(not(unix))]
        {
            let full = self.path.join(relative);
            let metadata = fs::symlink_metadata(&full)
                .with_context(|| format!("failed inspecting anchored file: {}", full.display()))?;
            if !metadata.file_type().is_file() {
                bail!("anchored file is not a regular file: {}", full.display());
            }
            if reject_hardlinks || metadata.len() > max_bytes {
                bail!("bounded hard-link-safe capture is unavailable on this platform");
            }
            fs::read(&full)
                .with_context(|| format!("failed reading anchored file: {}", full.display()))
        }
    }

    /// Opens a relative file/directory without following any symlink. Used for
    /// bounded enumeration; callers inspect metadata before processing bytes.
    #[cfg(unix)]
    pub(crate) fn open_relative(&self, relative: &Path) -> Result<fs::File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let mut current = dup_fd(&self.file)?;
        let components = relative.components().collect::<Vec<_>>();
        if components.is_empty() {
            // Reopen the held directory itself so enumeration has an independent
            // offset; dup alone would share the anchor's open-file description.
            // SAFETY: current is a live directory fd; the constant name is NUL-terminated.
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    c".".as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: openat returned a new owned descriptor, transferred exactly once.
            return Ok(unsafe { fs::File::from_raw_fd(fd) });
        }
        for (index, component) in components.iter().enumerate() {
            let std::path::Component::Normal(name) = component else {
                bail!("unsafe relative document path");
            };
            let name = std::ffi::CString::new(name.as_bytes())?;
            let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
            if index + 1 < components.len() {
                flags |= libc::O_DIRECTORY;
            }
            // SAFETY: live directory descriptor and valid single-component CString.
            let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("document path is not accessible without following links");
            }
            // SAFETY: new owned descriptor from openat transferred exactly once.
            current = unsafe { fs::File::from_raw_fd(fd) };
        }
        Ok(current)
    }

    /// Lists up to `limit` child names beneath an anchored real directory. The
    /// enumeration itself is descriptor-relative, including when parents move.
    #[cfg(unix)]
    pub(crate) fn list_bounded(
        &self,
        relative: &Path,
        limit: usize,
    ) -> Result<Vec<std::ffi::OsString>> {
        use std::os::fd::IntoRawFd;
        use std::os::unix::ffi::OsStringExt;
        let directory = self.open_relative(relative)?;
        if !directory.metadata()?.is_dir() {
            bail!("document enumeration requires a real directory");
        }
        let fd = directory.into_raw_fd();
        // SAFETY: fd ownership transfers to DIR on success, closed explicitly on failure.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = std::io::Error::last_os_error();
            // SAFETY: fdopendir failed, so we still own fd.
            unsafe { libc::close(fd) };
            return Err(error.into());
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                // SAFETY: this guard owns the live DIR and closes it exactly once.
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut entries = Vec::new();
        loop {
            // Reset errno to distinguish end-of-directory from an enumeration error.
            #[cfg(target_os = "macos")]
            // SAFETY: __error returns a valid pointer to this thread's errno cell.
            unsafe {
                *libc::__error() = 0;
            }
            #[cfg(target_os = "linux")]
            // SAFETY: __errno_location returns this thread's valid, writable errno cell.
            unsafe {
                *libc::__errno_location() = 0;
            }
            // SAFETY: stream is live; returned name remains valid until next readdir.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error().unwrap_or(0) != 0 {
                        return Err(error.into());
                    }
                }
                break;
            }
            // SAFETY: readdir supplies a NUL-terminated d_name in a valid dirent.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if entries.len() >= limit {
                bail!("document directory exceeds entry bound");
            }
            entries.push(std::ffi::OsString::from_vec(name.to_vec()));
        }
        entries.sort();
        Ok(entries)
    }

    /// Creates a new durable file beneath this anchored directory.
    ///
    /// Every parent component is opened with `O_DIRECTORY | O_NOFOLLOW`; a
    /// missing parent is created with `mkdirat` and then opened through the
    /// same held descriptor. The leaf is created with `O_EXCL | O_NOFOLLOW`,
    /// written, flushed, and synced before its containing directory is synced.
    pub fn write_new(&self, relative: &Path, payload: &[u8]) -> Result<()> {
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(part) => parts.push(part),
                _ => bail!(
                    "anchored write path must use only normal components: {}",
                    relative.display()
                ),
            }
        }
        let (leaf, parents) = parts
            .split_last()
            .ok_or_else(|| anyhow::anyhow!("anchored write path must name a file"))?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::io::{AsRawFd, FromRawFd};

            let mut current = dup_fd(&self.file)?;
            for parent in parents {
                let name = std::ffi::CString::new(parent.as_bytes())
                    .map_err(|_| anyhow::anyhow!("anchored directory component has NUL"))?;
                let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
                // SAFETY: `current` is a live open directory fd (a dup of the held root anchor) and `name` is a NUL-terminated CString, so `openat` gets valid args; it returns a new owned fd or a negative error value (picked up by the check below).
                let mut fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
                if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
                    // SAFETY: `current` is a live open directory fd and `name` is a NUL-terminated CString, so `mkdirat` reads only the valid dirfd/pointer/mode and writes no process memory.
                    let rc = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
                    if rc < 0 {
                        return Err(std::io::Error::last_os_error()).with_context(|| {
                            format!("failed creating anchored directory component {:?}", parent)
                        });
                    }
                    current
                        .sync_all()
                        .context("failed syncing anchored parent directory")?;
                    // SAFETY: `current` is a live open directory fd and `name` is a NUL-terminated CString, so `openat` gets valid args; it returns a new owned fd or a negative error value (checked below).
                    fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
                }
                if fd < 0 {
                    return Err(std::io::Error::last_os_error()).with_context(|| {
                        format!("failed opening anchored directory component {:?}", parent)
                    });
                }
                // SAFETY: `fd` is a non-negative descriptor from one of the two `openat` calls above (each returns a uniquely owned new fd or a negative error), so this `File` takes ownership and closes it on drop.
                let next = unsafe { fs::File::from_raw_fd(fd) };
                if !next.metadata()?.is_dir() {
                    bail!("anchored write parent is not a directory: {:?}", parent);
                }
                current = next;
            }

            let name = std::ffi::CString::new(leaf.as_bytes())
                .map_err(|_| anyhow::anyhow!("anchored file name has NUL"))?;
            // SAFETY: `current` is a live open directory fd and `name` is a NUL-terminated CString, so `openat` gets valid fd/pointer/mode args; it returns a new owned fd or a negative error value (checked below) and writes no process memory.
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("failed creating anchored file {}", relative.display())
                });
            }
            // SAFETY: `fd` is the non-negative descriptor returned by the `openat` above, so it is a uniquely owned new descriptor; this `File` takes ownership and closes it on drop.
            let mut file = unsafe { fs::File::from_raw_fd(fd) };
            file.write_all(payload)?;
            file.flush()?;
            file.sync_all()?;
            current.sync_all()?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let full = self.path.join(relative);
            if let Some(parent) = full.parent() {
                create_dir_all_durable(parent)?;
            }
            write_new_file(&full, payload)
        }
    }
}

#[cfg(unix)]
fn dup_fd(file: &fs::File) -> std::io::Result<fs::File> {
    use std::os::unix::io::{AsRawFd, FromRawFd};
    // SAFETY: `file.as_raw_fd()` is the live open descriptor owned by `file` (required by `dup`), which returns a new fd owned by this process or a negative error value checked below; it writes no process memory.
    let duped = unsafe { libc::dup(file.as_raw_fd()) };
    if duped < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `duped` is the non-negative descriptor returned by `dup` above, so it is a uniquely owned new fd; this `File` takes ownership and closes it on drop.
    Ok(unsafe { fs::File::from_raw_fd(duped) })
}

/// Removes a leftover temp file through the anchored parent directory when
/// possible. Temp cleanup is best-effort: publication already succeeded or
/// failed before this runs, so a failed cleanup must not mask the real result.
///
/// Retained for the non-Unix write paths; the Unix write paths clean up through
/// the held parent descriptor via [`cleanup_temp_anchored`].
#[allow(dead_code)]
fn unlink_temp_no_follow(parent: &Path, temp_path: &Path) -> std::io::Result<()> {
    let Some(temp_name) = temp_path.file_name() else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let Ok(dir) = options.open(parent) else {
            return Ok(());
        };
        let temp_c = match std::ffi::CString::new(temp_name.as_bytes()) {
            Ok(value) => value,
            Err(_) => return Ok(()),
        };
        // SAFETY: `dir` is a live open directory fd and `temp_c` is a NUL-terminated CString (embedded NULs rejected by `CString::new` via the early return above), so `unlinkat` gets valid fd/pointer args and writes no process memory.
        unsafe {
            libc::unlinkat(dir.as_raw_fd(), temp_c.as_ptr(), 0);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        match fs::remove_file(temp_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Removes a regular file through an anchored parent directory descriptor.
///
/// The parent is opened with `open(O_DIRECTORY | O_NOFOLLOW)` and the removal
/// is performed with `unlinkat(parent_fd, name, 0)` after the target is
/// verified through that same descriptor to be a regular file (never a
/// symlink). A same-user racer that replaces the parent path with a symlink
/// after the descriptor is opened cannot redirect the deletion, and a target
/// swapped for a symlink is rejected instead of removed.
///
/// Returns `io::Error` with kind `NotFound` when `name` does not exist.
pub fn remove_file_anchored(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("path '{}' is missing a parent directory", path.display());
    };
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("path '{}' is missing a file name", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::AsRawFd;

        let dir = open_directory_anchor(parent).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed opening removal parent as a directory: {}",
                    parent.display()
                ),
            )
        })?;
        // The target must be a real regular file, never a symlink swapped in.
        ensure_anchored_regular_source(&dir, name, "removal target").map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("failed verifying removal target: {}", path.display()),
            )
        })?;
        let name_c = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "name has NUL"))?;
        // SAFETY: `dir` is a live open directory fd (anchored above) and `name_c` is a NUL-terminated CString, so `unlinkat` gets valid fd/pointer args and a valid flag value, writing no process memory.
        let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), name_c.as_ptr(), 0) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed removing file through anchored parent: {}",
                    path.display()
                )
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::remove_file(path).with_context(|| format!("failed removing file: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnchoredDir, create_dir_all_durable, descriptor_anchor_link, descriptor_anchor_rename_at,
        read_jsonl_records, remove_file_anchored, repair_jsonl_tail, write_new_file,
        write_new_file_if_identical, write_replace_file,
    };
    use crate::fault_injection;
    use crate::fault_injection::{
        FAULT_AFTER_DIRECTORY_SYNC, FAULT_AFTER_FILE_SYNC, FAULT_AFTER_FLUSH, FAULT_AFTER_LINK,
        FAULT_AFTER_RENAME, FAULT_AFTER_WRITE,
    };
    use std::fs;

    #[test]
    fn replacement_faults_leave_a_complete_projection() {
        for point in [
            FAULT_AFTER_WRITE,
            FAULT_AFTER_FLUSH,
            FAULT_AFTER_FILE_SYNC,
            FAULT_AFTER_RENAME,
            FAULT_AFTER_DIRECTORY_SYNC,
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join("state.json");
            std::fs::write(&path, b"old").expect("seed projection");

            fault_injection::set(point);
            let result = write_replace_file(&path, b"new");
            fault_injection::clear();

            assert!(result.is_err(), "fault point {point} must fail");
            assert!(
                matches!(std::fs::read(&path).as_deref(), Ok(b"old") | Ok(b"new")),
                "fault point {point} must leave a complete projection"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn durable_helpers_set_private_modes() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("nested").join("state");
        let path = directory.join("state.json");
        write_new_file(&path, b"state").expect("write state");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn durable_directory_creation_rejects_a_symlinked_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).expect("root");
        fs::create_dir_all(&outside).expect("outside");
        symlink(&outside, root.join("redirect")).expect("component symlink");

        assert!(create_dir_all_durable(&root.join("redirect/new")).is_err());
        assert!(!outside.join("new").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn directory_anchor_accepts_macos_system_var_prefix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("nested");
        fs::create_dir_all(&directory).expect("directory");

        super::open_directory_anchor(&directory).expect("open temporary directory");
    }

    #[test]
    fn create_new_fault_does_not_leave_a_retry_blocking_file() {
        for point in [FAULT_AFTER_WRITE, FAULT_AFTER_FLUSH, FAULT_AFTER_FILE_SYNC] {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join("record.json");
            fault_injection::set(point);
            assert!(write_new_file(&path, b"record").is_err());
            fault_injection::clear();
            assert!(!path.exists(), "fault point {point} left a partial file");
            write_new_file(&path, b"record").expect("retry create-new");
        }
    }

    #[test]
    fn occurrence_fault_targets_the_selected_matching_operation() {
        let temp = tempfile::tempdir().expect("tempdir");
        fault_injection::set_occurrence(FAULT_AFTER_LINK, 2);

        write_new_file(&temp.path().join("first"), b"first").expect("first write");
        assert!(write_new_file(&temp.path().join("second"), b"second").is_err());
        fault_injection::clear();
        write_new_file(&temp.path().join("third"), b"third").expect("third write");
    }

    #[test]
    fn identical_create_new_reconciles_but_different_content_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("publication");

        write_new_file_if_identical(&path, b"same").expect("initial publication");
        write_new_file_if_identical(&path, b"same").expect("identical retry");
        assert!(write_new_file_if_identical(&path, b"different").is_err());
        assert_eq!(std::fs::read(&path).expect("read publication"), b"same");
    }

    #[test]
    fn repair_jsonl_tail_preserves_a_valid_final_record_missing_only_a_newline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("events.jsonl");
        std::fs::write(&path, b"{\"a\":1}\n{\"b\":2}").expect("write jsonl");

        let repaired = repair_jsonl_tail(&path).expect("repair");
        assert!(repaired, "missing trailing newline must be repaired");

        let contents = std::fs::read_to_string(&path).expect("read jsonl");
        assert_eq!(
            contents, "{\"a\":1}\n{\"b\":2}\n",
            "the valid final record must survive; only the newline is appended"
        );
        let records = read_jsonl_records(&path).expect("read records");
        assert_eq!(
            records.len(),
            2,
            "both records must be readable after repair"
        );
    }

    #[test]
    fn repair_jsonl_tail_truncates_a_genuinely_malformed_final_fragment() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("events.jsonl");
        std::fs::write(&path, b"{\"a\":1}\n{\"b\":2, \"incomplete").expect("write jsonl");

        let repaired = repair_jsonl_tail(&path).expect("repair");
        assert!(repaired, "malformed trailing fragment must be repaired");

        let contents = std::fs::read_to_string(&path).expect("read jsonl");
        assert_eq!(
            contents, "{\"a\":1}\n",
            "the malformed trailing fragment must be truncated, not preserved"
        );
    }

    #[test]
    fn repair_jsonl_tail_is_a_noop_for_a_well_formed_stream() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("events.jsonl");
        std::fs::write(&path, b"{\"a\":1}\n{\"b\":2}\n").expect("write jsonl");

        let repaired = repair_jsonl_tail(&path).expect("repair");
        assert!(!repaired, "a well-formed stream must not be modified");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read jsonl"),
            "{\"a\":1}\n{\"b\":2}\n"
        );
    }

    #[test]
    fn read_jsonl_records_retains_a_valid_final_record_missing_only_a_newline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("events.jsonl");
        std::fs::write(&path, b"{\"a\":1}\n{\"b\":2}").expect("write jsonl");

        let records = read_jsonl_records(&path).expect("read records");
        assert_eq!(records.len(), 2, "the valid final record must be retained");
        assert_eq!(records[1], b"{\"b\":2}".to_vec());
    }

    #[test]
    fn read_jsonl_records_discards_a_genuinely_malformed_final_fragment() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("events.jsonl");
        std::fs::write(&path, b"{\"a\":1}\n{\"b\":2, \"incomplete").expect("write jsonl");

        let records = read_jsonl_records(&path).expect("read records");
        assert_eq!(
            records.len(),
            1,
            "the malformed trailing fragment is discarded"
        );
        assert_eq!(records[0], b"{\"a\":1}".to_vec());
    }

    #[test]
    fn read_jsonl_records_pushes_malformed_interior_records_for_the_caller() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("events.jsonl");
        std::fs::write(&path, b"{\"a\":1}\nnot-json\n{\"b\":2}\n").expect("write jsonl");

        let records = read_jsonl_records(&path).expect("read records");
        assert_eq!(
            records.len(),
            3,
            "newline-terminated interior records must reach the caller"
        );
        assert_eq!(records[1], b"not-json".to_vec());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_publication_rejects_symlinked_target_never_reconciles_symlink_bytes() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("artifacts");
        fs::create_dir_all(&dir).expect("dir");
        let target = dir.join("out.bin");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"attacker-controlled").expect("outside");

        // Publish the legitimate file first.
        write_new_file_if_identical(&target, b"legit").expect("initial publication");

        // A retry that reconciles an existing regular file must still succeed
        // byte-for-byte.
        write_new_file_if_identical(&target, b"legit").expect("identical retry");

        // Replace the destination with a symlink pointing at attacker bytes.
        // `read_regular_file_no_follow` must reject the symlink (never read the
        // target), so the reconciliation fails closed instead of acknowledging
        // attacker bytes as the published artifact.
        fs::remove_file(&target).expect("remove target");
        symlink(&outside, &target).expect("swap in symlink");

        let error = write_new_file_if_identical(&target, b"legit")
            .expect_err("symlinked destination must not reconcile");
        assert!(
            error.to_string().contains("symlink")
                || error.to_string().contains("opening publication target"),
            "symlinked target must be rejected, got: {error}"
        );
        assert!(
            fs::read(&outside).expect("outside").as_slice() == b"attacker-controlled",
            "outside bytes must never be read as the publication"
        );
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_publication_rejects_non_regular_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("pub");
        fs::create_dir_all(&dir).expect("parent dir");
        // A directory (not a file) at the destination must be rejected by the
        // regular-file check of the anchored read.
        let dir_target = dir.join("subdir");
        fs::create_dir_all(&dir_target).expect("subdir");

        let error = write_new_file_if_identical(&dir_target, b"payload")
            .expect_err("directory destination must not reconcile");
        assert!(
            error.to_string().contains("not a regular file")
                || error.to_string().contains("is not a regular file")
                || error.to_string().contains("opening publication target"),
            "non-regular target must be rejected, got: {error}"
        );

        // A symlink at the destination is likewise rejected.
        let file_target = dir.join("file.txt");
        let outside = temp.path().join("outside");
        fs::write(&outside, b"x").expect("outside");
        symlink(&outside, &file_target).expect("symlink");
        let error = write_new_file_if_identical(&file_target, b"payload")
            .expect_err("symlink destination must not reconcile");
        assert!(
            error.to_string().contains("symlink")
                || error.to_string().contains("opening publication target"),
            "symlink target must be rejected, got: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_new_file_rejects_a_symlinked_temp_source() {
        // The temp file is created by the caller-free write path, but a
        // same-user racer may replace it with a symlink after creation and
        // before the anchored link. `descriptor_anchor_link` must reject that
        // temp source rather than hard-link the symlink into the destination.
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("pub");
        fs::create_dir_all(&dir).expect("parent dir");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"attacker-controlled").expect("outside");

        // Simulate a temp file that a racer replaced with a symlink.
        let temp_name = std::ffi::OsStr::new("sneaky.tmp");
        let symlink_path = dir.join(temp_name);
        symlink(&outside, &symlink_path).expect("symlink temp");
        let error = descriptor_anchor_link(&dir, temp_name, std::ffi::OsStr::new("out.bin"))
            .expect_err("symlinked temp source must be rejected");
        assert!(
            error.to_string().contains("not a regular file")
                || error
                    .to_string()
                    .contains("Too many levels of symbolic links"),
            // O_NOFOLLOW can reject the swapped symlink directly with ELOOP.
            "symlinked temp source must be rejected, got: {error}"
        );
        assert!(
            !dir.join("out.bin").exists(),
            "a symlink must never be linked as the publication"
        );
        // A regular temp source still links fine.
        fs::remove_file(&symlink_path).expect("remove symlink");
        fs::write(&symlink_path, b"legit").expect("regular temp");
        descriptor_anchor_link(&dir, temp_name, std::ffi::OsStr::new("out.bin"))
            .expect("regular temp links");
    }

    #[cfg(unix)]
    #[test]
    fn write_replace_sync_rejects_a_swapped_parent_between_rename_and_sync() {
        // After the rename but before the directory sync, a same-user racer
        // swaps the parent path for a symlink pointing at another directory.
        // Because `write_replace_file` anchors the parent and `sync_directory`
        // opens through `O_DIRECTORY | O_NOFOLLOW`, the sync must fail (never
        // fsync a redirected directory) instead of silently syncing attacker
        // state, and the destination must remain the real file.
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("state");
        fs::create_dir_all(&parent).expect("parent dir");
        let path = parent.join("state.json");
        let seed_path = parent.join("seed");
        let deeper_path = parent.join("deeper");
        fs::write(&seed_path, b"seed").expect("seed in parent");
        fs::create_dir_all(&deeper_path).expect("deeper dir");

        // Arm a fault after the rename (before the directory sync).
        fault_injection::set(FAULT_AFTER_RENAME);
        let err = write_replace_file(&path, b"new-state");
        fault_injection::clear();
        assert!(
            err.is_err(),
            "fault after rename must fail the replace write"
        );

        // The destination was already renamed into the real parent; now swap
        // the parent itself for a symlink to an attacker directory and try
        // syncing the real directory through that (now broken) path.
        let attacker = temp.path().join("attacker");
        fs::create_dir_all(&attacker).expect("attacker dir");
        let real_parent = temp.path().join("real-state");
        fs::rename(&parent, &real_parent).expect("move real parent");
        symlink(&attacker, &parent).expect("symlink parent");

        // A fresh write whose rename targets the symlinked parent must fail
        // closed (the anchored parent open rejects the symlink), so attacker
        // state is never published as the projection.
        let sync_err = write_replace_file(&path, b"other-state");
        assert!(
            sync_err.is_err(),
            "replace into a symlinked parent must fail"
        );
        assert!(
            !attacker.join("state.json").exists(),
            "attacker directory must never receive the projection"
        );
        // The real projection (moved aside) is intact.
        let real_state = real_parent.join("state.json");
        assert_eq!(fs::read(&real_state).expect("real state"), b"new-state");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_anchor_rename_rejects_a_symlinked_temp_source() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("pub");
        fs::create_dir_all(&dir).expect("parent dir");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"attacker-controlled").expect("outside");

        // A temp-source swap that would otherwise be a same-user race: the
        // regular temp file is replaced with a symlink before the anchored
        // rename. `descriptor_anchor_rename_at` must reject it, never
        // replacing the destination through the symlink.
        let temp_name = std::ffi::OsStr::new("sneaky.tmp");
        let symlink_path = dir.join(temp_name);
        fs::write(&symlink_path, b"legit").expect("regular temp");
        let anchored = super::open_directory_anchor(&dir).expect("open anchored parent");
        let source = super::open_regular_file_at(&anchored, temp_name).expect("open source");
        fs::remove_file(&symlink_path).expect("remove regular temp");
        symlink(&outside, &symlink_path).expect("swap temp for symlink");
        let error = descriptor_anchor_rename_at(
            &anchored,
            &source,
            temp_name,
            std::ffi::OsStr::new("state.json"),
        )
        .expect_err("symlinked temp source must be rejected");
        assert!(
            error.to_string().contains("not a regular file"),
            "symlinked temp source must be rejected for rename, got: {error}"
        );
        assert!(
            !dir.join("state.json").exists(),
            "destination must never be replaced from a symlinked temp source"
        );
        // A regular temp source still renames fine.
        fs::remove_file(&symlink_path).expect("remove symlink");
        fs::write(&symlink_path, b"legit").expect("regular temp");
        let source = super::open_regular_file_at(&anchored, temp_name).expect("open source");
        descriptor_anchor_rename_at(
            &anchored,
            &source,
            temp_name,
            std::ffi::OsStr::new("state.json"),
        )
        .expect("regular temp renames");
        let renamed = dir.join("state.json");
        assert_eq!(fs::read(&renamed).expect("read"), b"legit");
    }

    #[cfg(unix)]
    #[test]
    fn anchored_dir_holds_one_root_descriptor_across_reads() {
        // Opening the root once and reading through that single descriptor must
        // keep all reads anchored to the original object: a racer that swaps
        // the run-root *path* for a symlink after the first read cannot
        // redirect a later read to attacker bytes.
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let context_dir = root.join("context");
        let state_path = root.join("state.json");
        let inputs_path = context_dir.join("inputs.json");
        fs::create_dir_all(&context_dir).expect("context dir");
        fs::write(&state_path, b"state-bytes").expect("state");
        fs::write(&inputs_path, b"inputs-bytes").expect("inputs");

        let anchored = AnchoredDir::open(&root).expect("open anchored root");
        assert_eq!(
            anchored
                .read_regular(std::path::Path::new("state.json"))
                .expect("read"),
            b"state-bytes"
        );

        // Swap the run-root *path* for a symlink pointing at an attacker dir
        // that carries conflicting bytes, while the original directory (still
        // held by the anchored descriptor) is moved aside intact.
        let attacker = temp.path().join("attacker");
        let attacker_context = attacker.join("context");
        let attacker_state = attacker.join("state.json");
        let attacker_inputs = attacker_context.join("inputs.json");
        fs::create_dir_all(&attacker_context).expect("attacker context dir");
        fs::write(&attacker_state, b"attacker-state").expect("attacker state");
        fs::write(&attacker_inputs, b"attacker-inputs").expect("attacker inputs");
        let original = temp.path().join("original-root");
        fs::rename(&root, &original).expect("move original root aside");
        symlink(&attacker, &root).expect("symlink root path");

        // A fresh open of the swapped path must be rejected (root resolves to
        // a symlink now), not silently followed to attacker bytes.
        let reopened_error = AnchoredDir::open(&root).expect_err("symlinked root must be rejected");
        assert!(
            reopened_error.to_string().contains("symlink")
                || reopened_error.to_string().contains("opening anchored root")
                || reopened_error.to_string().contains("not a directory"),
            "symlinked root must be rejected, got: {reopened_error:#}"
        );

        // The original held descriptor must still resolve to the original
        // object's bytes (moved aside intact), proving one root anchor is held
        // across reads and the path swap cannot redirect it.
        assert_eq!(
            anchored
                .read_regular(std::path::Path::new("state.json"))
                .expect("held descriptor read"),
            b"state-bytes"
        );
        assert_eq!(
            anchored
                .read_regular(std::path::Path::new("context/inputs.json"))
                .expect("held descriptor read 2"),
            b"inputs-bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_new_file_anchors_publication_to_held_parent_despite_mid_write_symlink_swap() {
        // Finding 5: the parent descriptor is opened BEFORE the temp file is
        // created and held through the entire write-publish cycle. A same-user
        // racer that replaces the parent *path* with a symlink between temp
        // creation and publication cannot redirect the publication: `linkat`
        // resolves relative to the held descriptor (the original real
        // directory), never the symlinked attacker path. The attacker directory
        // is closed off — it receives nothing — and the publication lands in the
        // real directory (here moved aside by the racer).
        use std::os::unix::fs::symlink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("state");
        fs::create_dir_all(&parent).expect("parent dir");
        let target = parent.join("state.json");

        let attacker = temp.path().join("attacker");
        fs::create_dir_all(&attacker).expect("attacker dir");
        let attacker_target = attacker.join("state.json");
        fs::write(&attacker_target, b"attacker").expect("attacker seed");

        let real_moved = temp.path().join("real-state");
        let parent_for_hook = parent.clone();
        let attacker_for_hook = attacker.clone();
        let real_moved_for_hook = real_moved.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();
        let _hook = fault_injection::after_temp_create_hook(move || {
            if fired_clone.swap(true, Ordering::SeqCst) {
                return;
            }
            // Swap the parent path for a symlink to the attacker directory,
            // moving the real parent aside. This is the racer window the fix
            // closes: between temp creation and publication.
            fs::rename(&parent_for_hook, &real_moved_for_hook).expect("move real parent aside");
            symlink(&attacker_for_hook, &parent_for_hook).expect("symlink parent path at attacker");
        });

        let result = write_new_file(&target, b"legit");

        assert!(fired.load(Ordering::SeqCst), "race hook must have fired");
        // The publication resolved through the held descriptor (the original
        // real dir, now moved aside), so it landed there — never in the
        // attacker dir reachable through the swapped symlink.
        assert_eq!(
            fs::read(real_moved.join("state.json")).expect("real dir received publication"),
            b"legit"
        );
        assert_eq!(
            fs::read(&attacker_target).expect("attacker target untouched"),
            b"attacker",
            "the symlinked attacker path must never receive the publication"
        );
        // No leftover temp file leaked into the attacker dir.
        assert!(
            !attacker
                .read_dir()
                .expect("read attacker dir")
                .any(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
            "no temp file may leak into the attacker directory"
        );
        assert!(
            result.is_ok(),
            "publication must succeed into the held real dir, got: {:?}",
            result.err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_replace_file_anchors_publication_to_held_parent_despite_mid_write_symlink_swap() {
        // Finding 5 (replace path): the held parent descriptor anchors the
        // rename to the original directory, so a mid-write parent→symlink swap
        // cannot redirect the replacement into the attacker directory.
        use std::os::unix::fs::symlink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("state");
        fs::create_dir_all(&parent).expect("parent dir");
        let target = parent.join("state.json");
        fs::write(&target, b"old").expect("seed projection");

        let attacker = temp.path().join("attacker");
        fs::create_dir_all(&attacker).expect("attacker dir");
        let attacker_target = attacker.join("state.json");
        fs::write(&attacker_target, b"attacker").expect("attacker seed");

        let real_moved = temp.path().join("real-state");
        let parent_for_hook = parent.clone();
        let attacker_for_hook = attacker.clone();
        let real_moved_for_hook = real_moved.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();
        let _hook = fault_injection::after_temp_create_hook(move || {
            if fired_clone.swap(true, Ordering::SeqCst) {
                return;
            }
            fs::rename(&parent_for_hook, &real_moved_for_hook).expect("move real parent aside");
            symlink(&attacker_for_hook, &parent_for_hook).expect("symlink parent path at attacker");
        });

        let result = write_replace_file(&target, b"new-state");

        assert!(fired.load(Ordering::SeqCst), "race hook must have fired");
        assert_eq!(
            fs::read(real_moved.join("state.json")).expect("real dir received replacement"),
            b"new-state"
        );
        assert_eq!(
            fs::read(&attacker_target).expect("attacker target untouched"),
            b"attacker",
            "the symlinked attacker path must never receive the replacement"
        );
        assert!(
            !attacker
                .read_dir()
                .expect("read attacker dir")
                .any(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
            "no temp file may leak into the attacker directory"
        );
        assert!(
            result.is_ok(),
            "replacement must succeed into the held real dir, got: {:?}",
            result.err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn remove_file_anchored_rejects_a_symlink_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("state");
        fs::create_dir_all(&dir).expect("parent dir");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"outside").expect("outside");

        // A symlink at the removal target must be rejected, never unlinked.
        let symlink_target = dir.join("decision.json");
        symlink(&outside, &symlink_target).expect("symlink target");
        let error = remove_file_anchored(&symlink_target).expect_err("symlink target rejected");
        assert!(
            error.to_string().contains("not a regular file")
                || error
                    .to_string()
                    .contains("failed verifying removal target"),
            "symlinked removal target must be rejected, got: {error:#}"
        );
        assert!(
            symlink_target.exists(),
            "a symlink must never be removed by the anchored delete"
        );
        assert_eq!(
            fs::read_to_string(&outside).expect("outside intact"),
            "outside"
        );

        // A regular file at the target is removed through the anchored parent.
        let regular_target = dir.join("regular.json");
        fs::write(&regular_target, b"payload").expect("regular target");
        remove_file_anchored(&regular_target).expect("regular file removed");
        assert!(
            !regular_target.exists(),
            "regular file must be removed through the anchored parent"
        );
    }
}
