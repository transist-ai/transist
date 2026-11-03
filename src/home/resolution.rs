// SPDX-License-Identifier: MPL-2.0
use super::settings::load_user_home_path;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

/// Source that selected the effective Transist home directory.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeSource {
    /// The caller passed an explicit home directory on the command line or API boundary.
    CliOverride,
    /// No `--home`, `TRANSIST_HOME`, or persisted user home was available; the
    /// per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`) was used.
    PerUserDefault,
    /// The `TRANSIST_HOME` environment variable selected the home directory.
    EnvOverride,
    /// The home directory was derived from the user's persisted configuration.
    UserConfig,
}

/// Error produced when no Transist home can be resolved and no fallback exists.
#[derive(Debug)]
pub enum HomeResolutionError {
    /// No `--home` flag, `TRANSIST_HOME`, persisted user home, and no per-user
    /// default could be computed (neither `XDG_STATE_HOME` nor `HOME` set).
    NotSetAndNoLocalDir {
        /// The current working directory at resolution time, for diagnostic messages.
        cwd: PathBuf,
    },
}

impl fmt::Display for HomeResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HomeResolutionError::NotSetAndNoLocalDir { cwd } => write!(
                f,
                "no Transist home resolved for {} — run `transist home init` to initialize the \
                 per-user default home, or `transist home set <path>` to point at an existing \
                 location",
                cwd.display()
            ),
        }
    }
}

impl std::error::Error for HomeResolutionError {}

/// Result of resolving the effective Transist home directory for a command or API call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedHome {
    /// Absolute path to the home directory that should be used.
    pub effective_home: PathBuf,
    /// Which resolution rule produced `effective_home`.
    pub source: HomeSource,
    /// The persisted user-set home from settings, if any.
    ///
    /// Present even when a higher-priority source (e.g. `PerUserDefault`) won, so the
    /// Configuration screen can show "I set X, but local won".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted_user_home: Option<PathBuf>,
}

/// Returns the per-repository Transist home outside the repository workspace.
///
/// `$HOME` (or `temp_dir()` as a fallback) is read on every call rather than cached
/// in a `OnceLock`. In production `$HOME` is stable so this is equivalent; in tests
/// that temporarily mutate `$HOME` under a lock, per-call reads prevent a single
/// racing call from permanently poisoning a static root with a volatile temp-dir path.
pub fn repo_local_home_dir(repo_root: &Path) -> PathBuf {
    let identity = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let mut digest = Sha256::new();
    digest.update(identity.to_string_lossy().as_bytes());
    let suffix = digest
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(std::env::temp_dir);
    let candidate = root.join(".transist").join("repos").join(&suffix);
    if !candidate.starts_with(&identity) {
        return candidate;
    }

    let fallback_root = identity
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("/"));
    fallback_root.join(".transist").join("repos").join(suffix)
}

/// Resolves the effective Transist home using explicit, environment, config, then the
/// per-user default.
///
/// Precedence: `--home` flag > `TRANSIST_HOME` env > persisted user-set >
/// per-user default (`${XDG_STATE_HOME:-$HOME/.local/state}/transist`) >
/// [`HomeResolutionError::NotSetAndNoLocalDir`] when even the per-user default cannot be
/// computed (no `HOME` and no `XDG_STATE_HOME`).
pub fn resolve_home(
    _repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
) -> Result<ResolvedHome> {
    let env_home = std::env::var_os("TRANSIST_HOME").map(PathBuf::from);
    let persisted_user_home = load_user_home_path()?;
    let per_user_default = per_user_default_home();
    resolve_home_with_overrides(
        cwd,
        explicit_home,
        env_home.as_deref(),
        persisted_user_home.as_deref(),
        per_user_default.as_deref(),
    )
    .map_err(anyhow::Error::new)
}

/// Returns `${XDG_STATE_HOME:-$HOME/.local/state}/transist`, or `None` when neither
/// `XDG_STATE_HOME` nor `HOME` is set (or `HOME` is not an absolute path).
pub fn per_user_default_home() -> Option<PathBuf> {
    if let Some(xdg_state_home) = std::env::var_os("XDG_STATE_HOME") {
        let path = PathBuf::from(xdg_state_home);
        if path.is_absolute() {
            return Some(path.join("transist"));
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|home| home.join(".local").join("state").join("transist"))
}

/// Resolves the effective Transist home from already-supplied override values.
///
/// Returns [`HomeResolutionError::NotSetAndNoLocalDir`] when no source can provide a home.
pub fn resolve_home_with_overrides(
    cwd: &Path,
    explicit_home: Option<&Path>,
    env_home: Option<&Path>,
    persisted_user_home: Option<&Path>,
    per_user_default: Option<&Path>,
) -> std::result::Result<ResolvedHome, HomeResolutionError> {
    if let Some(path) = explicit_home {
        return Ok(ResolvedHome {
            effective_home: absolute_from(cwd, path),
            source: HomeSource::CliOverride,
            persisted_user_home: persisted_user_home.map(Path::to_path_buf),
        });
    }

    if let Some(path) = env_home.filter(|path| !path.as_os_str().is_empty()) {
        return Ok(ResolvedHome {
            effective_home: absolute_from(cwd, path),
            source: HomeSource::EnvOverride,
            persisted_user_home: persisted_user_home.map(Path::to_path_buf),
        });
    }

    if let Some(root) = persisted_user_home {
        return Ok(ResolvedHome {
            effective_home: root.to_path_buf(),
            source: HomeSource::UserConfig,
            persisted_user_home: Some(root.to_path_buf()),
        });
    }

    if let Some(default_home) = per_user_default {
        return Ok(ResolvedHome {
            effective_home: default_home.to_path_buf(),
            source: HomeSource::PerUserDefault,
            persisted_user_home: None,
        });
    }

    Err(HomeResolutionError::NotSetAndNoLocalDir {
        cwd: cwd.to_path_buf(),
    })
}

/// Returns the same home resolution payload used by CLI-facing settings commands.
pub fn resolve_settings_payload(
    repo_root: &Path,
    cwd: &Path,
    explicit_home: Option<&Path>,
) -> Result<ResolvedHome> {
    resolve_home(repo_root, cwd, explicit_home)
}

pub(super) fn absolute_from(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}
