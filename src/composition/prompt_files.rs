// SPDX-License-Identifier: MPL-2.0
//! Local prompt-file implementation for the kernel port.

#![allow(unsafe_code)]

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use transist_core::ports::{PromptFilePort, PromptFileReference};

/// Resolves and reads prompt files from workflow source trees.
pub(crate) struct LocalPromptFiles;

impl LocalPromptFiles {
    /// Creates a local prompt-file resolver.
    pub(crate) fn new() -> Self {
        Self
    }
}

impl PromptFilePort for LocalPromptFiles {
    fn resolve_prompt_file(
        &self,
        raw_path: &str,
        recipe_source_dir: Option<&Path>,
        step_id: &str,
    ) -> Result<PromptFileReference> {
        let path = PathBuf::from(raw_path);
        let absolute = if path.is_absolute() {
            path
        } else {
            let source_dir = recipe_source_dir.ok_or_else(|| {
                anyhow::anyhow!(
                    "step '{}' requires recipe source_dir to resolve relative prompt_file paths",
                    step_id
                )
            })?;
            source_dir.join(path)
        };
        let metadata = fs::symlink_metadata(&absolute).with_context(|| {
            format!(
                "step '{}' prompt_file '{}' does not exist",
                step_id,
                absolute.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "step '{}' prompt_file '{}' is not a regular file",
                step_id,
                absolute.display()
            );
        }
        let bytes = read_prompt_file_no_follow(&absolute)?;
        let content = String::from_utf8(bytes.clone()).with_context(|| {
            format!(
                "step '{}' prompt_file '{}' is not UTF-8",
                step_id,
                absolute.display()
            )
        })?;
        Ok(PromptFileReference {
            path: absolute,
            content,
            sha256: Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        })
    }

    fn read_source_asset(
        &self,
        raw_path: &str,
        recipe_source_dir: Option<&Path>,
        step_id: &str,
    ) -> Result<Vec<u8>> {
        let path = Path::new(raw_path);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            bail!("step '{step_id}' source asset '{raw_path}' is not a contained relative path");
        }
        let source_dir = recipe_source_dir.ok_or_else(|| {
            anyhow::anyhow!(
                "step '{step_id}' requires recipe source_dir to resolve source asset paths"
            )
        })?;
        let absolute = source_dir.join(path);
        let metadata = fs::symlink_metadata(&absolute).with_context(|| {
            format!("step '{step_id}' source asset '{raw_path}' does not exist")
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("step '{step_id}' source asset '{raw_path}' is not a regular file");
        }
        read_prompt_file_no_follow(&absolute)
    }
}

fn read_prompt_file_no_follow(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        if !path.is_absolute() {
            bail!("prompt_file path must be absolute after resolution");
        }
        let mut directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        let components = path
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(name) => Some(Ok(name)),
                std::path::Component::RootDir => None,
                _ => Some(Err(anyhow::anyhow!(
                    "prompt_file path contains non-normal component"
                ))),
            })
            .collect::<Result<Vec<_>>>()?;
        let Some((name, parents)) = components.split_last() else {
            bail!("prompt_file path is empty");
        };
        for parent in parents {
            let name = CString::new(parent.as_bytes()).context("prompt_file path contains NUL")?;
            // SAFETY: `directory` is a live directory descriptor and `name` is NUL-terminated.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("failed opening prompt_file directory '{}'", path.display())
                });
            }
            // SAFETY: `openat` returned a new owned descriptor after the negative check above.
            directory = unsafe { fs::File::from_raw_fd(fd) };
        }
        let name = CString::new(name.as_bytes()).context("prompt_file path contains NUL")?;
        // SAFETY: `directory` is a live directory descriptor and `name` is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed opening prompt_file '{}'", path.display()));
        }
        // SAFETY: `openat` returned a new owned descriptor after the negative check above.
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() {
            bail!("prompt_file '{}' is not a regular file", path.display());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .with_context(|| format!("failed reading prompt_file '{}'", path.display()))?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        fs::read(path).with_context(|| format!("failed reading prompt_file '{}'", path.display()))
    }
}
