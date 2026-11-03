// SPDX-License-Identifier: MPL-2.0
//! Descriptor-anchored reads for mutable workspace files.

#![allow(unsafe_code)]

use anyhow::{Context, Result, bail};
use std::path::Path;

/// Descriptor-anchored read of a file that lives beneath a persisted run root.
///
/// The traversal starts from `home_dir`, opens `runs/` as a directory
/// descriptor with `O_NOFOLLOW`, then walks every component of the run
/// locator's `run_relative_path` and every component of `file_relative_path`
/// using `openat` with `O_DIRECTORY|O_NOFOLLOW`. The final file is opened with
/// `O_RDONLY|O_NOFOLLOW|O_NONBLOCK`.
///
/// Because every run-tree component is crossed one `openat` at a time with
/// `O_NOFOLLOW`, an intermediate run-tree directory that is replaced with a
/// symlink after the locator is resolved cannot be followed: the matching
/// `openat` rejects the symlink in place.
pub(crate) fn read_relative(
    home_dir: &Path,
    run_relative_path: &str,
    file_relative_path: &str,
) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        read_run_relative_unix(home_dir, run_relative_path, file_relative_path)
    }
    #[cfg(not(unix))]
    {
        let _ = (home_dir, run_relative_path, file_relative_path);
        // Without descriptor-relative no-follow opens, deny reads rather than
        // introduce a check-then-open symlink race on supported platforms.
        bail!("descriptor-safe workflow file reads are unsupported on this platform")
    }
}

/// Descriptor-anchored read of a file beneath an already-trusted single root.
///
/// Used for workspace, repository-state, and repository-source reads where the
/// anchor is a single trusted directory (not a multi-component run-tree path).
/// The root is opened with `O_DIRECTORY|O_NOFOLLOW` and each component of
/// `relative_path` is walked with `openat` + `O_NOFOLLOW`.
pub(crate) fn read_relative_to_root(root: &Path, relative_path: &str) -> Result<Vec<u8>> {
    let relative = Path::new(relative_path);
    #[cfg(unix)]
    {
        read_beneath_unix(root, relative)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative);
        bail!("descriptor-safe workflow file reads are unsupported on this platform")
    }
}

#[cfg(unix)]
fn read_beneath_unix(root: &Path, relative: &Path) -> Result<Vec<u8>> {
    use std::os::fd::AsRawFd;

    let components = collect_normal_components(relative)?;
    let Some((file_name, parents)) = components.split_last() else {
        bail!("workflow file path is empty");
    };

    let mut directory = open_root_descriptor(root, "trusted workspace root")?;
    for parent in parents {
        directory = openat_directory(directory.as_raw_fd(), parent, "workflow directory")?;
    }
    let file = openat_file(directory.as_raw_fd(), file_name)?;
    read_regular_file(file)
}

#[cfg(unix)]
fn read_run_relative_unix(
    home_dir: &Path,
    run_relative_path: &str,
    file_relative_path: &str,
) -> Result<Vec<u8>> {
    use std::os::fd::AsRawFd;

    let run_components = collect_normal_components(Path::new(run_relative_path))?;
    if run_components.is_empty() {
        bail!("run-relative path is empty");
    }
    let file_components = collect_normal_components(Path::new(file_relative_path))?;
    let Some((file_name, file_parents)) = file_components.split_last() else {
        bail!("workflow file path is empty");
    };

    // Anchor at the Transist home directory.
    let mut directory = open_root_descriptor(home_dir, "transist home")?;
    // Cross `runs/` as a single descriptor-anchored step so the run-tree is
    // never opened as one multi-component path.
    directory = openat_directory(
        directory.as_raw_fd(),
        std::ffi::OsStr::new(super_runs_dir_name()),
        "runs directory",
    )?;
    // Walk the run locator's relative path one component at a time. Each step
    // rejects a symlink swap on that exact component, closing the root-swap race.
    for component in &run_components {
        directory = openat_directory(directory.as_raw_fd(), component, "run-tree directory")?;
    }
    // Walk the file's relative path beneath the run root.
    for parent in file_parents {
        directory = openat_directory(directory.as_raw_fd(), parent, "workflow directory")?;
    }
    let file = openat_file(directory.as_raw_fd(), file_name)?;
    read_regular_file(file)
}

/// Top-level runs directory name. Kept as a tiny constant here so the
/// descriptor traversal does not depend on the storage crate's layout, while
/// staying consistent with `transist_storage_local::run_paths::RUNS_DIR_NAME`.
#[cfg(unix)]
fn super_runs_dir_name() -> &'static str {
    "runs"
}

#[cfg(unix)]
fn collect_normal_components(path: &Path) -> Result<Vec<&std::ffi::OsStr>> {
    use std::path::Component;
    path.components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            other => bail!("workflow file path contains non-normal component: {other:?}"),
        })
        .collect()
}

#[cfg(unix)]
fn open_root_descriptor(path: &Path, label: &str) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;

    let absolute = path.is_absolute();
    let mut directory = std::fs::File::open(if absolute { "/" } else { "." })?;
    for (index, component) in path.components().enumerate() {
        let name = match component {
            std::path::Component::Normal(name) => name,
            std::path::Component::RootDir if absolute && index == 0 => continue,
            std::path::Component::CurDir if !absolute && index == 0 => continue,
            _ => bail!("{label} contains an unsafe component: {}", path.display()),
        };
        directory = openat_directory(directory.as_raw_fd(), name, label)?;
    }
    Ok(directory)
}

#[cfg(unix)]
fn openat_directory(
    dir_fd: std::os::fd::RawFd,
    name: &std::ffi::OsStr,
    label: &str,
) -> Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(name.as_bytes()).context("workflow path contains NUL")?;
    // SAFETY: `dir_fd` is a live directory descriptor and `name` is NUL-terminated.
    // O_DIRECTORY|O_NOFOLLOW rejects symlinks and non-directory entries.
    let fd = unsafe {
        libc::openat(
            dir_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed opening {label}"));
    }
    // SAFETY: `fd` is newly returned by openat and ownership transfers once.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn openat_file(dir_fd: std::os::fd::RawFd, name: &std::ffi::OsStr) -> Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(name.as_bytes()).context("workflow path contains NUL")?;
    // O_NONBLOCK prevents a special file from blocking before metadata validation.
    // SAFETY: `dir_fd` is a live directory descriptor and `name` is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            dir_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed opening workflow file");
    }
    // SAFETY: `fd` is newly returned by openat and ownership transfers once.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn read_regular_file(mut file: std::fs::File) -> Result<Vec<u8>> {
    use std::io::Read;
    let metadata = file.metadata().context("failed inspecting workflow file")?;
    if !metadata.is_file() {
        bail!("workflow file is not a regular file");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .context("failed reading workflow file")?;
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{read_relative, read_relative_to_root};
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn rejects_outside_root_and_symlink_swap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let temp_root = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let root = temp_root.join("workspace");
        let outside = temp_root.join("outside");
        fs::create_dir_all(root.join("reports")).expect("workspace");
        fs::create_dir(&outside).expect("outside");
        fs::write(root.join("reports/output.txt"), b"safe").expect("safe file");
        fs::write(outside.join("output.txt"), b"secret").expect("outside file");

        assert_eq!(
            read_relative_to_root(&root, "reports/output.txt").expect("safe read"),
            b"safe"
        );
        assert!(read_relative_to_root(&root, "../outside/output.txt").is_err());
        assert!(
            read_relative_to_root(&root, outside.join("output.txt").to_str().unwrap()).is_err()
        );

        fs::remove_dir_all(root.join("reports")).expect("remove original directory");
        symlink(&outside, root.join("reports")).expect("swap directory for symlink");
        assert!(read_relative_to_root(&root, "reports/output.txt").is_err());

        fs::remove_file(root.join("reports")).ok();
        fs::remove_dir(root.join("reports")).ok();
        fs::create_dir_all(&root).expect("restore workspace");
        fs::write(root.join("output.txt"), b"safe").expect("restore safe file");
        symlink(outside.join("output.txt"), root.join("output-link.txt")).expect("file swap");
        assert!(read_relative_to_root(&root, "output-link.txt").is_err());
    }

    #[test]
    fn root_read_rejects_a_symlink_in_a_root_ancestor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let temp_root = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let real_base = temp_root.join("real-base");
        let linked_base = temp_root.join("linked-base");
        let root = linked_base.join("workspace");
        fs::create_dir_all(real_base.join("workspace")).expect("workspace");
        fs::write(real_base.join("workspace/output.txt"), b"safe").expect("file");
        symlink(&real_base, &linked_base).expect("ancestor symlink");

        assert!(read_relative_to_root(&root, "output.txt").is_err());
    }

    #[test]
    fn run_relative_read_rejects_run_tree_symlink_swap_after_resolution() {
        let temp = tempfile::tempdir().expect("tempdir");
        let temp_root = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let home = temp_root.join(".transist");
        let runs = home.join("runs");
        let run_root = runs.join("run_root-run");
        let outside = temp_root.join("outside");
        fs::create_dir_all(run_root.join("artifacts")).expect("run tree");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(run_root.join("artifacts/output.txt"), b"safe").expect("file");

        // The locator's run-relative path is resolved once (this stands in for
        // `resolve_locator_root`), then the read happens later.
        let run_relative_path = "run_root-run";
        let file_relative_path = "artifacts/output.txt";

        assert_eq!(
            read_relative(&home, run_relative_path, file_relative_path).expect("safe read"),
            b"safe"
        );

        // Swap the run-tree directory for a symlink AFTER the locator was
        // resolved. The descriptor-anchored traversal must reject the swap.
        fs::remove_dir_all(&run_root).expect("remove original run dir");
        symlink(&outside, &run_root).expect("swap run dir for symlink");
        assert!(read_relative(&home, run_relative_path, file_relative_path).is_err());
    }

    #[test]
    fn run_relative_read_rejects_intermediate_file_dir_symlink_swap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let temp_root = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let home = temp_root.join(".transist");
        let runs = home.join("runs");
        let run_root = runs.join("run_root-run");
        let outside = temp_root.join("outside");
        fs::create_dir_all(run_root.join("artifacts")).expect("run tree");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(outside.join("output.txt"), b"secret").expect("outside file");
        fs::write(run_root.join("artifacts/output.txt"), b"safe").expect("file");

        let run_relative_path = "run_root-run";
        let file_relative_path = "artifacts/output.txt";

        assert_eq!(
            read_relative(&home, run_relative_path, file_relative_path).expect("safe read"),
            b"safe"
        );

        // Swap an intermediate directory in the file path for a symlink. The
        // openat traversal must reject it.
        fs::remove_dir_all(run_root.join("artifacts")).expect("remove artifacts dir");
        symlink(&outside, run_root.join("artifacts")).expect("swap artifacts for symlink");
        assert!(read_relative(&home, run_relative_path, file_relative_path).is_err());
    }

    #[test]
    fn run_relative_read_rejects_final_file_symlink_swap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let temp_root = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let home = temp_root.join(".transist");
        let runs = home.join("runs");
        let run_root = runs.join("run_root-run");
        let outside = temp_root.join("outside");
        fs::create_dir_all(run_root.join("artifacts")).expect("run tree");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(outside.join("secret.txt"), b"secret").expect("outside file");
        fs::write(run_root.join("artifacts/output.txt"), b"safe").expect("file");

        let run_relative_path = "run_root-run";
        let file_relative_path = "artifacts/output.txt";

        assert_eq!(
            read_relative(&home, run_relative_path, file_relative_path).expect("safe read"),
            b"safe"
        );

        // Swap the final file for a symlink. O_NOFOLLOW on the final openat must reject it.
        fs::remove_file(run_root.join("artifacts/output.txt")).expect("remove file");
        symlink(
            outside.join("secret.txt"),
            run_root.join("artifacts/output.txt"),
        )
        .expect("swap file for symlink");
        assert!(read_relative(&home, run_relative_path, file_relative_path).is_err());
    }
}
