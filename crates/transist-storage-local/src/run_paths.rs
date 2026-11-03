// SPDX-License-Identifier: MPL-2.0
//! Shared helpers for resolving persisted run paths under the Transist home.

#![allow(unsafe_code)]

use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use transist_core::{PersistedRunRecord, RunLineage};
pub use transist_protocol::{ActiveInvocationRecord, RunLocator};

pub use transist_core::{
    LIVE_PROGRESS_RECORD_SCHEMA_V1, RUN_PROVENANCE_RECORD_SCHEMA_V1, RUN_STATE_RECORD_SCHEMA_V1,
};
pub use transist_protocol::{ACTIVE_INVOCATION_RECORD_SCHEMA_V1, RUN_LOCATOR_SCHEMA_V1};

/// Top-level runs directory name inside the Transist home.
pub const RUNS_DIR_NAME: &str = "runs";
/// Internal per-run locator index directory name.
pub const RUN_INDEX_DIR_NAME: &str = "_index";
/// Nested child-run container directory name.
pub const CONTAINED_RUNS_DIR_NAME: &str = "contained_runs";
/// Run directory prefix used on disk.
pub const RUN_DIR_PREFIX: &str = "run_";
/// Directory under a run root where adapters publish active invocation records.
pub const ACTIVE_INVOCATIONS_DIR_NAME: &str = "active_invocations";
/// Per-run provenance file name.
pub const RUN_PROVENANCE_FILE_NAME: &str = "provenance.json";

const MAX_PARENT_LOCATOR_DEPTH: usize = 256;

/// Returns the `<home>/runs` directory.
pub fn runs_root(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNS_DIR_NAME)
}

/// Returns the internal run-locator index directory.
pub fn run_index_root(home_dir: &Path) -> PathBuf {
    runs_root(home_dir).join(RUN_INDEX_DIR_NAME)
}

/// Returns the on-disk directory name for a run id.
pub fn run_dir_name(run_id: &str) -> String {
    format!("{RUN_DIR_PREFIX}{run_id}")
}

/// Returns the per-run locator file path.
pub fn run_locator_path(home_dir: &Path, run_id: &str) -> PathBuf {
    run_index_root(home_dir).join(format!("{}.json", run_dir_name(run_id)))
}

/// Plans the persisted locator for a run using its lineage metadata.
pub fn plan_run_locator(
    home_dir: &Path,
    run_id: &str,
    lineage: Option<&RunLineage>,
) -> Result<RunLocator> {
    validate_run_id(run_id)?;
    let parent_run_id = lineage
        .and_then(|lineage| lineage.parent_run_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let parent_locator = parent_run_id
        .as_deref()
        .map(|parent_run_id| load_run_locator(home_dir, parent_run_id))
        .transpose()?;
    let root_run_id = parent_locator
        .as_ref()
        .map(|locator| locator.root_run_id.clone())
        .unwrap_or_else(|| run_id.to_string());
    if let Some(lineage_root_run_id) = lineage
        .map(|lineage| lineage.root_run_id.trim())
        .filter(|value| !value.is_empty())
        && lineage_root_run_id != root_run_id
    {
        bail!(
            "run '{}' lineage root '{}' does not match physical root '{}'",
            run_id,
            lineage_root_run_id,
            root_run_id
        );
    }

    let relative_path = if let Some(parent_locator) = parent_locator {
        parent_locator
            .relative_path_buf()?
            .join(CONTAINED_RUNS_DIR_NAME)
            .join(run_dir_name(run_id))
    } else {
        PathBuf::from(run_dir_name(run_id))
    };
    validate_safe_relative_path(&relative_path)?;

    Ok(RunLocator {
        schema: RUN_LOCATOR_SCHEMA_V1.to_string(),
        run_id: run_id.to_string(),
        root_run_id,
        parent_run_id,
        relative_path: relative_path.to_string_lossy().to_string(),
    })
}

/// Persists a run locator record.
pub fn write_run_locator(home_dir: &Path, locator: &RunLocator) -> Result<()> {
    validate_run_id(&locator.run_id)?;
    let path = run_locator_path(home_dir, &locator.run_id);
    if path.parent().is_some() {
        let payload = serde_json::to_vec_pretty(locator)
            .with_context(|| format!("failed serializing run locator: {}", path.display()))?;
        crate::atomic_io::write_new_file_if_identical(&path, &payload)
            .with_context(|| format!("failed writing run locator: {}", path.display()))?;
        return Ok(());
    }
    bail!(
        "run locator path '{}' is missing a parent directory",
        path.display()
    )
}

/// Loads a persisted run locator record.
pub fn load_run_locator(home_dir: &Path, run_id: &str) -> Result<RunLocator> {
    validate_run_id(run_id)?;
    let path = run_locator_path(home_dir, run_id);
    let Some(raw) = read_run_locator_bytes(home_dir, run_id)? else {
        bail!(
            "failed reading run locator: {}: file does not exist",
            path.display()
        );
    };
    let raw = String::from_utf8(raw)
        .with_context(|| format!("run locator is not UTF-8: {}", path.display()))?;
    let locator: RunLocator = serde_json::from_str(&raw)
        .with_context(|| format!("invalid run locator json: {}", path.display()))?;
    validate_run_locator(home_dir, &locator, run_id, &path)?;
    Ok(locator)
}

/// Resolves the run-root directory for a persisted run id.
pub fn resolve_run_root(home_dir: &Path, run_id: &str) -> Result<PathBuf> {
    let locator = load_run_locator(home_dir, run_id)?;
    resolve_locator_root(home_dir, &locator)
}

/// Recomputes the per-execution `artifacts/<step>/execution_<NNN>` directory.
///
/// `run_root` must already be the rebased absolute path under the live home.
pub fn execution_dir_path(run_root: &Path, step_id: &str, execution: u32) -> PathBuf {
    run_root
        .join("artifacts")
        .join(step_id)
        .join(format!("execution_{execution:03}"))
}

/// Rebases the four absolute-path fields of a persisted [`transist_protocol::InvokePaths`] in place.
///
/// All four fields (`run_root`, `artifacts_dir`, `evidence_dir`, `workspace_dir`)
/// are deterministically recomputable from the live `(home_dir, repo_root,
/// run_id, step_id, execution)` tuple. After persisted state crosses a host or
/// is read from a moved repo, callers must rebase before the kernel or any
/// inspection helper consumes the request.
pub fn rebase_invoke_paths(
    paths: &mut transist_protocol::InvokePaths,
    home_dir: &Path,
    repo_root: &Path,
    run_id: &str,
    step_id: &str,
    execution: u32,
) -> Result<()> {
    let run_root = resolve_run_root_or_default(home_dir, run_id)?;
    let execution_dir = execution_dir_path(&run_root, step_id, execution);
    paths.run_root = run_root.display().to_string();
    paths.artifacts_dir = execution_dir.join("artifacts").display().to_string();
    paths.evidence_dir = execution_dir.join("evidence").display().to_string();
    paths.workspace_dir = repo_root.display().to_string();
    Ok(())
}

/// Rebases every persisted `last_request` in a run record so paths point at the
/// live home and repo. Safe no-op for steps with no persisted request.
pub fn rebase_persisted_run_record(
    record: &mut PersistedRunRecord,
    home_dir: &Path,
    repo_root: &Path,
) -> Result<()> {
    for (step_id, step) in record.steps.iter_mut() {
        let Some(request) = step.last_request.as_mut() else {
            continue;
        };
        rebase_invoke_paths(
            &mut request.paths,
            home_dir,
            repo_root,
            &request.run_id,
            step_id,
            request.step.execution,
        )?;
    }
    Ok(())
}

/// Outcome of resolving a persisted run root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunRootOutcome {
    /// The run locator exists and resolved to its exact run-root path.
    Resolved(PathBuf),
    /// No locator record exists (legacy layout); the caller may fall back to
    /// the deterministic `runs/run_<id>` top-level path.
    LegacyAbsent(PathBuf),
}

/// Resolves a persisted run root with an explicit outcome, distinguishing a
/// genuine legacy locator absence from a corrupt or unreadable locator.
///
/// A missing locator is the only case that reports [`RunRootOutcome::LegacyAbsent`];
/// a locator that exists but fails to parse, mismatches the requested run id, or
/// names an unsafe relative path is a corruption and fails closed.
pub fn resolve_run_root_outcome(home_dir: &Path, run_id: &str) -> Result<RunRootOutcome> {
    validate_run_id(run_id)?;
    let locator_path = run_locator_path(home_dir, run_id);
    let Some(raw) = read_run_locator_bytes(home_dir, run_id)? else {
        return Ok(RunRootOutcome::LegacyAbsent(
            runs_root(home_dir).join(run_dir_name(run_id)),
        ));
    };
    let raw = String::from_utf8(raw)
        .with_context(|| format!("run locator is not UTF-8: {}", locator_path.display()))?;
    let locator: RunLocator = serde_json::from_str(&raw)
        .with_context(|| format!("invalid run locator json: {}", locator_path.display()))?;
    validate_run_locator(home_dir, &locator, run_id, &locator_path)?;
    Ok(RunRootOutcome::Resolved(resolve_locator_root(
        home_dir, &locator,
    )?))
}

fn validate_run_locator(
    home_dir: &Path,
    locator: &RunLocator,
    run_id: &str,
    path: &Path,
) -> Result<()> {
    let mut current = locator.clone();
    let mut current_run_id = run_id.to_string();
    let mut current_path = path.to_path_buf();
    let mut visited = HashSet::new();
    let mut depth = 0;

    loop {
        if !visited.insert(current_run_id.clone()) {
            bail!("run locator parent chain contains a cycle at '{current_run_id}'");
        }
        if current.schema != RUN_LOCATOR_SCHEMA_V1 {
            bail!(
                "run locator '{}' uses unsupported schema '{}'",
                current_path.display(),
                current.schema
            );
        }
        if current.run_id != current_run_id {
            bail!(
                "run locator '{}' does not match requested run id '{}'",
                current_path.display(),
                current_run_id
            );
        }
        let relative_path = current.relative_path_buf()?;
        if relative_path.file_name() != Some(OsStr::new(&run_dir_name(&current_run_id))) {
            bail!(
                "run locator '{}' path does not terminate in '{}': {}",
                current_path.display(),
                run_dir_name(&current_run_id),
                relative_path.display()
            );
        }
        let Some(parent_run_id) = current.parent_run_id.clone() else {
            if relative_path.components().count() != 1 {
                bail!(
                    "root run locator '{}' must use a top-level run path",
                    current_path.display()
                );
            }
            if current.root_run_id != current.run_id {
                bail!(
                    "root run locator '{}' has mismatched root run id",
                    current_path.display()
                );
            }
            return Ok(());
        };

        validate_run_id(&parent_run_id)?;
        if visited.contains(&parent_run_id) {
            bail!("run locator parent chain contains a cycle at '{parent_run_id}'");
        }
        if depth == MAX_PARENT_LOCATOR_DEPTH {
            bail!("run locator parent chain exceeds maximum depth of {MAX_PARENT_LOCATOR_DEPTH}");
        }
        let parent_path = run_locator_path(home_dir, &parent_run_id);
        let parent_locator = read_run_locator_bytes(home_dir, &parent_run_id)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "failed reading run locator: {}: file does not exist",
                    parent_path.display()
                )
            })
            .and_then(|raw| {
                String::from_utf8(raw)
                    .with_context(|| format!("run locator is not UTF-8: {}", parent_path.display()))
            })
            .and_then(|raw| {
                serde_json::from_str::<RunLocator>(&raw)
                    .with_context(|| format!("invalid run locator json: {}", parent_path.display()))
            })
            .with_context(|| {
                format!("invalid parent locator for child run '{}'", current_run_id)
            })?;
        if let Some(next_parent_run_id) = parent_locator.parent_run_id.as_deref()
            && visited.contains(next_parent_run_id)
        {
            bail!("run locator parent chain contains a cycle at '{next_parent_run_id}'");
        }
        let expected = parent_locator
            .relative_path_buf()?
            .join(CONTAINED_RUNS_DIR_NAME)
            .join(run_dir_name(&current_run_id));
        if relative_path != expected {
            bail!(
                "child run locator '{}' does not match declared parent '{}'",
                current_path.display(),
                parent_run_id
            );
        }
        if current.root_run_id != parent_locator.root_run_id {
            bail!(
                "child run locator '{}' root run id does not match its parent",
                current_path.display()
            );
        }
        current = parent_locator;
        current_run_id = parent_run_id;
        current_path = parent_path;
        depth += 1;
    }
}

fn validate_run_id(run_id: &str) -> Result<()> {
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::RunRef, run_id)
        .context("invalid run id")
}

fn resolve_locator_root(home_dir: &Path, locator: &RunLocator) -> Result<PathBuf> {
    let relative = locator.relative_path_buf()?;
    let mut current = runs_root(home_dir);
    reject_symlink_or_non_directory(&current)?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("run locator path contains a non-normal component");
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("run-root component is a symlink: {}", current.display());
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!(
                    "run-root component is not a directory: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Birth planning may resolve the final directory before it is published.
                break;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed inspecting run-root component: {}",
                        current.display()
                    )
                });
            }
        }
    }
    Ok(runs_root(home_dir).join(relative))
}

fn reject_symlink_or_non_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("run-tree directory is a symlink: {}", path.display())
        }
        Ok(metadata) if !metadata.is_dir() => {
            bail!("run-tree path is not a directory: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed inspecting run-tree path: {}", path.display())),
    }
}

#[cfg(unix)]
fn read_run_locator_bytes(home_dir: &Path, run_id: &str) -> Result<Option<Vec<u8>>> {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;

    validate_run_id(run_id)?;
    let locator_path = run_locator_path(home_dir, run_id);
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let home = match options.open(home_dir) {
        Ok(home) => home,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed opening run-locator home directory: {}",
                    home_dir.display()
                )
            });
        }
    };
    let mut directory = home;
    for component in [RUNS_DIR_NAME, RUN_INDEX_DIR_NAME] {
        let name = CString::new(component).expect("run directory names contain no NUL");
        // SAFETY: `directory` is a live directory descriptor and `name` is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error)
                .with_context(|| format!("failed opening run-locator directory: {}", component));
        }
        // SAFETY: `fd` is newly returned by openat and ownership transfers once.
        directory = unsafe { fs::File::from_raw_fd(fd) };
    }
    let file_name = format!("{}.json", run_dir_name(run_id));
    let name = CString::new(file_name).context("run id contains NUL")?;
    // SAFETY: `directory` is a live directory descriptor and `name` is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error)
            .with_context(|| format!("failed reading run locator: {}", locator_path.display()));
    }
    // SAFETY: `fd` is newly returned by openat and ownership transfers once.
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        bail!(
            "run locator '{}' is not a regular file",
            locator_path.display()
        );
    }
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)
        .with_context(|| format!("failed reading run locator: {}", locator_path.display()))?;
    Ok(Some(raw))
}

#[cfg(not(unix))]
fn read_run_locator_bytes(home_dir: &Path, run_id: &str) -> Result<Option<Vec<u8>>> {
    validate_run_id(run_id)?;
    let path = run_locator_path(home_dir, run_id);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("run locator '{}' must not be a symlink", path.display());
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("run locator '{}' is not a regular file", path.display());
        }
        Ok(_) => fs::read(&path)
            .map(Some)
            .with_context(|| format!("failed reading run locator: {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("failed inspecting run locator: {}", path.display()))
        }
    }
}

/// Resolves the run-root directory, falling back to the top-level root-run path
/// shape only when the locator is genuinely absent (legacy layout). A corrupt
/// locator is not a valid fallback and fails closed.
pub fn resolve_run_root_or_default(home_dir: &Path, run_id: &str) -> Result<PathBuf> {
    match resolve_run_root_outcome(home_dir, run_id)? {
        RunRootOutcome::Resolved(path) | RunRootOutcome::LegacyAbsent(path) => Ok(path),
    }
}

/// Resolves the path to the per-run provenance record.
pub fn run_provenance_path(home_dir: &Path, run_id: &str) -> Result<PathBuf> {
    Ok(resolve_run_root_or_default(home_dir, run_id)?.join(RUN_PROVENANCE_FILE_NAME))
}

/// Resolves the per-run `gates` directory.
pub fn gates_dir(home_dir: &Path, run_id: &str) -> Result<PathBuf> {
    Ok(resolve_run_root_or_default(home_dir, run_id)?.join("gates"))
}

/// Resolves the active manual-gate decision record path for an exact validated `gate_id`.
///
/// The gate id is validated with [`transist_protocol::IdentifierKind::GateId`] so
/// every on-disk decision is stored under the exact authored id. No lossy slug
/// normalization is applied: `review-a`, `review_a`, and `review.a` stay distinct.
pub fn gate_decision_path(home_dir: &Path, run_id: &str, gate_id: &str) -> Result<PathBuf> {
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, gate_id)
        .context("invalid gate id")?;
    Ok(gates_dir(home_dir, run_id)?.join(format!("{gate_id}.json")))
}

/// Resolves the durable gate decision history record path for an exact validated
/// `gate_id`, decision value, and decision timestamp.
pub fn gate_decision_history_path(
    home_dir: &Path,
    run_id: &str,
    gate_id: &str,
    decision: &str,
    decided_at_ms: u64,
) -> Result<PathBuf> {
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, gate_id)
        .context("invalid gate id")?;
    Ok(gates_dir(home_dir, run_id)?
        .join("history")
        .join(format!("{gate_id}-{decided_at_ms}-{decision}.json")))
}

/// Resolves the immutable manual-gate decision history directory for a run.
pub fn gate_decision_history_dir(home_dir: &Path, run_id: &str) -> Result<PathBuf> {
    Ok(gates_dir(home_dir, run_id)?.join("history"))
}

/// Resolves the root run id for a persisted run using lineage first and the locator index as a fallback.
pub fn resolve_root_run_id(home_dir: &Path, run_id: &str) -> Result<String> {
    let outcome = resolve_run_root_outcome(home_dir, run_id)?;
    let run_root = match &outcome {
        RunRootOutcome::Resolved(root) | RunRootOutcome::LegacyAbsent(root) => root,
    };
    let state_path = run_root.join("state.json");
    match fs::metadata(&state_path) {
        Ok(metadata) if metadata.is_file() => {
            let raw = fs::read_to_string(&state_path)
                .with_context(|| format!("failed reading run state: {}", state_path.display()))?;
            let value: serde_json::Value = serde_json::from_str(&raw)
                .with_context(|| format!("invalid run state json: {}", state_path.display()))?;
            if let Some(root_run_id) = value
                .get("lineage")
                .and_then(|lineage| lineage.get("root_run_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Ok(root_run_id.to_string());
            }
        }
        Ok(_) => bail!(
            "run state path is not a regular file: {}",
            state_path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed inspecting run state: {}", state_path.display()));
        }
    }

    if matches!(outcome, RunRootOutcome::LegacyAbsent(_)) {
        return Ok(run_id.to_string());
    }
    let locator = load_run_locator(home_dir, run_id)?;
    if locator.root_run_id.trim().is_empty() {
        bail!(
            "run '{}' is missing lineage.root_run_id and locator root_run_id metadata",
            run_id
        );
    }
    Ok(locator.root_run_id)
}

/// Lists all persisted run-root directories, including nested child runs.
pub fn list_run_roots(home_dir: &Path) -> Result<Vec<PathBuf>> {
    let root = runs_root(home_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    collect_run_roots(&root, &mut out, false)?;
    out.sort_by_key(|path| path.display().to_string());
    Ok(out)
}

/// Lists the selected physical root and its contained runs without inspecting
/// sibling trees. The selected run must be a root; malformed contained-run
/// directories fail closed through the same traversal used by whole-home lists.
pub fn list_run_tree_roots(home_dir: &Path, root_run_id: &str) -> Result<Vec<PathBuf>> {
    let root = resolve_run_root(home_dir, root_run_id)?;
    if lineage_root_run_dir_from_run_root(&root)? != root {
        bail!("run '{}' is not a physical root run", root_run_id);
    }
    let mut out = vec![root.clone()];
    collect_contained_run_roots(&root, &mut out, true)?;
    out.sort();
    Ok(out)
}

/// Returns the run id encoded in a persisted run-root directory name.
pub fn run_id_from_run_root(run_root: &Path) -> Option<String> {
    run_root
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_prefix(RUN_DIR_PREFIX))
        .map(str::to_string)
}

/// Resolves the Transist home directory from a run-root path.
pub fn home_dir_from_run_root(run_root: &Path) -> Result<PathBuf> {
    let runs_dir = run_root
        .ancestors()
        .find(|candidate| candidate.file_name() == Some(OsStr::new(RUNS_DIR_NAME)))
        .ok_or_else(|| anyhow::anyhow!("run root is missing '{}' ancestor", RUNS_DIR_NAME))?;
    runs_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
        anyhow::anyhow!(
            "run root '{}' is missing home directory parent",
            run_root.display()
        )
    })
}

/// Resolves the lineage-root run directory from any nested run-root path.
pub fn lineage_root_run_dir_from_run_root(run_root: &Path) -> Result<PathBuf> {
    run_root
        .ancestors()
        .find(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.starts_with(RUN_DIR_PREFIX))
                .unwrap_or(false)
                && candidate.parent().and_then(Path::file_name) == Some(OsStr::new(RUNS_DIR_NAME))
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "run root '{}' does not belong to a lineage rooted under '{}'",
                run_root.display(),
                RUNS_DIR_NAME
            )
        })
}

fn collect_run_roots(current: &Path, out: &mut Vec<PathBuf>, strict: bool) -> Result<()> {
    let mut entries = fs::read_dir(current)
        .with_context(|| format!("failed reading directory: {}", current.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed iterating directory: {}", current.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let metadata = fs::symlink_metadata(entry.path()).with_context(|| {
            format!(
                "failed inspecting run-tree entry: {}",
                entry.path().display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            if strict && name.starts_with(RUN_DIR_PREFIX) {
                bail!(
                    "selected run-tree entry is not a real directory: {}",
                    entry.path().display()
                );
            }
            continue;
        }
        if name == RUN_INDEX_DIR_NAME || !name.starts_with(RUN_DIR_PREFIX) {
            continue;
        }
        out.push(entry.path());
        collect_contained_run_roots(&entry.path(), out, strict)?;
    }
    Ok(())
}

fn collect_contained_run_roots(
    run_root: &Path,
    out: &mut Vec<PathBuf>,
    strict: bool,
) -> Result<()> {
    let nested = run_root.join(CONTAINED_RUNS_DIR_NAME);
    match fs::symlink_metadata(&nested) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "contained-runs directory is a symlink: {}",
                nested.display()
            );
        }
        Ok(metadata) if metadata.is_dir() => collect_run_roots(&nested, out, strict)?,
        Ok(_) => bail!(
            "contained-runs path is not a directory: {}",
            nested.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed inspecting contained-runs path: {}",
                    nested.display()
                )
            });
        }
    }
    Ok(())
}

/// Validates a logical artifact path before it is used to read or write a file
/// under an execution's artifact directory.
///
/// The path must be relative and non-empty, and may contain only normal
/// components. It rejects empty paths, absolute paths, any `.` or `..`
/// component, root components, drive prefixes, and separator-heavy inputs. A
/// caller that passes an unvalidated `logical_path` or `step_id` would otherwise
/// be able to escape the execution artifact directory.
pub fn validate_artifact_logical_path(raw: &str) -> Result<()> {
    let path = PathBuf::from(raw);
    if path.as_os_str().is_empty() {
        bail!("artifact logical path must not be empty");
    }
    validate_safe_relative_path(&path).context("invalid artifact logical path")?;
    Ok(())
}

/// Validates an artifact `step_id` before it is joined into an artifact path.
///
/// The step id must be a single validated step identifier (no separators, no
/// `.`/`..`, no absolute or root components).
pub fn validate_artifact_step_id(step_id: &str) -> Result<()> {
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::StepId, step_id)
        .context("invalid artifact step id")?;
    Ok(())
}

fn validate_safe_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        bail!("run locator path must be relative: {}", path.display());
    }
    if path.as_os_str().is_empty() {
        bail!("run locator path must not be empty");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!(
                "run locator path contains unsupported component '{}'",
                path.display()
            );
        }
    }
    Ok(())
}

/// Returns `<run_root>/active_invocations`.
pub fn active_invocations_dir(run_root: &Path) -> PathBuf {
    run_root.join(ACTIVE_INVOCATIONS_DIR_NAME)
}

/// Returns the per-invocation record path inside `<run_root>/active_invocations/`.
pub fn active_invocation_record_path(run_root: &Path, invoke_id: &str) -> PathBuf {
    active_invocations_dir(run_root).join(format!("{}.json", invoke_id))
}

/// Returns the per-invocation cancellation sentinel path inside
/// `<run_root>/active_invocations/`.
///
/// The sentinel is created by an out-of-process cancel before it signals the
/// recorded process group, and is observed by the executing adapter so that
/// the resulting failure-or-cancellation decision treats the outcome as
/// "canceled" rather than "failed". Sentinels are removed alongside the
/// associated record file.
pub fn active_invocation_cancel_sentinel_path(run_root: &Path, invoke_id: &str) -> PathBuf {
    active_invocations_dir(run_root).join(format!("{}.cancel", invoke_id))
}

/// Atomically creates the cancel sentinel file. Errors are returned but the
/// file content is intentionally empty; callers only check for existence.
pub fn write_active_invocation_cancel_sentinel(run_root: &Path, invoke_id: &str) -> Result<()> {
    let dir = active_invocations_dir(run_root);
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating active invocations dir {}", dir.display()))?;
    let path = active_invocation_cancel_sentinel_path(run_root, invoke_id);
    fs::write(&path, b"").with_context(|| format!("writing cancel sentinel {}", path.display()))
}

/// Removes the cancel sentinel for `invoke_id`. Missing files are ignored.
pub fn remove_active_invocation_cancel_sentinel(run_root: &Path, invoke_id: &str) -> Result<()> {
    let path = active_invocation_cancel_sentinel_path(run_root, invoke_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => {
            Err(err).with_context(|| format!("removing cancel sentinel {}", path.display()))
        }
    }
}

/// Returns `true` if a cancel sentinel exists for `invoke_id`.
pub fn active_invocation_cancel_sentinel_exists(run_root: &Path, invoke_id: &str) -> bool {
    active_invocation_cancel_sentinel_path(run_root, invoke_id).exists()
}

/// Atomically writes an active invocation record under the run root.
pub fn write_active_invocation_record(
    run_root: &Path,
    record: &ActiveInvocationRecord,
) -> Result<()> {
    let dir = active_invocations_dir(run_root);
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating active invocations dir {}", dir.display()))?;
    let path = active_invocation_record_path(run_root, &record.invoke_id);
    let payload = serde_json::to_vec_pretty(record)
        .with_context(|| "serializing active invocation record".to_string())?;
    crate::atomic_io::write_replace_file(&path, &payload)
        .with_context(|| format!("persisting active invocation record {}", path.display()))
}

/// Removes the active invocation record for `invoke_id`. Missing files are not an error.
pub fn remove_active_invocation_record(run_root: &Path, invoke_id: &str) -> Result<()> {
    let path = active_invocation_record_path(run_root, invoke_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("removing active invocation record {}", path.display())),
    }
}

/// Lists all active invocation records under the run root, sorted by file path.
///
/// Corrupt records are reported rather than skipped so callers cannot report a
/// successful cancellation while an uninspected invocation remains active.
pub fn list_active_invocation_records(run_root: &Path) -> Result<Vec<ActiveInvocationRecord>> {
    let dir = active_invocations_dir(run_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("reading active invocations dir {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("iterating active invocations dir {}", dir.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect();
    entries.sort();
    let mut out = Vec::new();
    for path in entries {
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspecting active invocation record {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "active invocation record is not a regular file: {}",
                path.display()
            );
        }
        let raw = crate::atomic_io::read_regular_file_no_follow(&path)
            .with_context(|| format!("reading active invocation record {}", path.display()))?;
        let record = serde_json::from_slice::<ActiveInvocationRecord>(&raw)
            .with_context(|| format!("invalid active invocation record {}", path.display()))?;
        if record.schema != ACTIVE_INVOCATION_RECORD_SCHEMA_V1 {
            bail!(
                "unsupported active invocation record schema '{}' at {}",
                record.schema,
                path.display()
            );
        }
        validate_safe_identifier(&record.invoke_id, "active invocation invoke id")?;
        transist_protocol::validate_identifier(
            transist_protocol::IdentifierKind::RunRef,
            &record.run_id,
        )
        .context("invalid active invocation run id")?;
        transist_protocol::validate_identifier(
            transist_protocol::IdentifierKind::StepId,
            &record.step_id,
        )
        .context("invalid active invocation step id")?;
        let expected_name = format!("{}.json", record.invoke_id);
        if path.file_name() != Some(OsStr::new(&expected_name)) {
            bail!(
                "active invocation record filename does not match invoke id: {}",
                path.display()
            );
        }
        out.push(record);
    }
    Ok(out)
}

fn validate_safe_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("{label} contains unsafe characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use transist_core::RunLineageMode;

    fn write_locator(home_dir: &Path, run_id: &str, lineage: Option<&RunLineage>) -> RunLocator {
        let locator = plan_run_locator(home_dir, run_id, lineage).expect("plan locator");
        write_run_locator(home_dir, &locator).expect("write locator");
        locator
    }

    fn write_unvalidated_locator(home_dir: &Path, locator: &RunLocator) {
        fs::create_dir_all(run_index_root(home_dir)).expect("create locator index");
        fs::write(
            run_locator_path(home_dir, &locator.run_id),
            serde_json::to_vec(locator).expect("serialize locator"),
        )
        .expect("write locator fixture");
    }

    #[test]
    fn plans_nested_relative_paths_from_parent_lineage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let root = write_locator(&home_dir, "root-run", None);
        assert_eq!(root.relative_path, "run_root-run");

        let child = write_locator(
            &home_dir,
            "root-run__child",
            Some(&RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root-run".to_string(),
                parent_run_id: Some("root-run".to_string()),
                parent_step_id: Some("child".to_string()),
            }),
        );
        assert_eq!(
            child.relative_path,
            "run_root-run/contained_runs/run_root-run__child"
        );

        let grandchild = write_locator(
            &home_dir,
            "root-run__child__nested",
            Some(&RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root-run".to_string(),
                parent_run_id: Some("root-run__child".to_string()),
                parent_step_id: Some("nested".to_string()),
            }),
        );
        assert_eq!(
            grandchild.relative_path,
            "run_root-run/contained_runs/run_root-run__child/contained_runs/run_root-run__child__nested"
        );
    }

    #[test]
    fn resolves_a_valid_nested_run_locator_chain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        write_locator(&home_dir, "root-run", None);
        write_locator(
            &home_dir,
            "child-run",
            Some(&RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root-run".to_string(),
                parent_run_id: Some("root-run".to_string()),
                parent_step_id: Some("child".to_string()),
            }),
        );
        let grandchild = write_locator(
            &home_dir,
            "grandchild-run",
            Some(&RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root-run".to_string(),
                parent_run_id: Some("child-run".to_string()),
                parent_step_id: Some("grandchild".to_string()),
            }),
        );

        assert_eq!(
            resolve_run_root(&home_dir, "grandchild-run").expect("resolve nested run"),
            runs_root(&home_dir).join(grandchild.relative_path_buf().expect("grandchild path"))
        );
    }

    #[test]
    fn rejects_a_self_referential_parent_locator() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let locator = RunLocator {
            schema: RUN_LOCATOR_SCHEMA_V1.to_string(),
            run_id: "self-run".to_string(),
            root_run_id: "self-run".to_string(),
            parent_run_id: Some("self-run".to_string()),
            relative_path: "run_self-run".to_string(),
        };
        write_unvalidated_locator(tmp.path(), &locator);

        let error = resolve_run_root(tmp.path(), "self-run").expect_err("self-cycle rejected");
        assert!(error.to_string().contains("cycle"), "got: {error:#}");
    }

    #[test]
    fn rejects_a_two_node_parent_locator_cycle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for (run_id, parent_run_id) in [("first-run", "second-run"), ("second-run", "first-run")] {
            write_unvalidated_locator(
                tmp.path(),
                &RunLocator {
                    schema: RUN_LOCATOR_SCHEMA_V1.to_string(),
                    run_id: run_id.to_string(),
                    root_run_id: "first-run".to_string(),
                    parent_run_id: Some(parent_run_id.to_string()),
                    relative_path: format!("run_{run_id}"),
                },
            );
        }

        let error = resolve_run_root(tmp.path(), "first-run").expect_err("cycle rejected");
        assert!(error.to_string().contains("cycle"), "got: {error:#}");
    }

    #[test]
    fn rejects_parent_locator_chains_deeper_than_the_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut relative_path = PathBuf::from(run_dir_name("run-0"));
        write_unvalidated_locator(
            tmp.path(),
            &RunLocator {
                schema: RUN_LOCATOR_SCHEMA_V1.to_string(),
                run_id: "run-0".to_string(),
                root_run_id: "run-0".to_string(),
                parent_run_id: None,
                relative_path: relative_path.to_string_lossy().to_string(),
            },
        );
        for index in 1..=257 {
            let run_id = format!("run-{index}");
            let parent_run_id = format!("run-{}", index - 1);
            relative_path = relative_path
                .join(CONTAINED_RUNS_DIR_NAME)
                .join(run_dir_name(&run_id));
            write_unvalidated_locator(
                tmp.path(),
                &RunLocator {
                    schema: RUN_LOCATOR_SCHEMA_V1.to_string(),
                    run_id: run_id.clone(),
                    root_run_id: "run-0".to_string(),
                    parent_run_id: Some(parent_run_id),
                    relative_path: relative_path.to_string_lossy().to_string(),
                },
            );
        }

        let error = resolve_run_root(tmp.path(), "run-257").expect_err("depth limit rejected");
        assert!(
            error.to_string().contains("maximum depth"),
            "got: {error:#}"
        );
    }

    #[test]
    fn plans_parentless_replay_locator_as_its_own_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let source_locator = write_locator(&home_dir, "source-run", None);
        let replay_lineage = RunLineage {
            mode: RunLineageMode::Replay,
            source_run_id: Some("source-run".to_string()),
            root_run_id: "replay-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        };

        let replay_locator = write_locator(&home_dir, "replay-run", Some(&replay_lineage));

        assert_eq!(source_locator.root_run_id, "source-run");
        assert_eq!(replay_locator.run_id, "replay-run");
        assert_eq!(replay_locator.root_run_id, "replay-run");
        assert_eq!(replay_locator.parent_run_id, None);
        assert_eq!(replay_locator.relative_path, "run_replay-run");
        assert_eq!(
            resolve_run_root(&home_dir, "replay-run").expect("resolve replay root"),
            home_dir.join("runs/run_replay-run")
        );
    }

    #[test]
    fn rejects_child_lineage_root_that_differs_from_parent_locator() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        write_locator(&home_dir, "root-run", None);

        let error = plan_run_locator(
            &home_dir,
            "root-run__child",
            Some(&RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "different-root".to_string(),
                parent_run_id: Some("root-run".to_string()),
                parent_step_id: Some("child".to_string()),
            }),
        )
        .expect_err("mismatched child lineage root must fail");

        assert!(error.to_string().contains("does not match physical root"));
    }

    #[test]
    fn lists_nested_run_roots_recursively() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let root = write_locator(&home_dir, "root-run", None);
        let child = write_locator(
            &home_dir,
            "root-run__child",
            Some(&RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root-run".to_string(),
                parent_run_id: Some("root-run".to_string()),
                parent_step_id: Some("child".to_string()),
            }),
        );

        fs::create_dir_all(runs_root(&home_dir).join(root.relative_path_buf().expect("root path")))
            .expect("root dir");
        fs::create_dir_all(
            runs_root(&home_dir).join(child.relative_path_buf().expect("child path")),
        )
        .expect("child dir");

        let listed = list_run_roots(&home_dir).expect("list run roots");
        assert_eq!(
            listed,
            vec![
                home_dir.join("runs/run_root-run"),
                home_dir.join("runs/run_root-run/contained_runs/run_root-run__child"),
            ]
        );
    }

    #[test]
    fn selected_tree_listing_ignores_unrelated_damage_and_rejects_selected_damage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        write_locator(home, "selected", None);
        write_locator(home, "unrelated", None);
        let selected = home.join("runs/run_selected");
        let unrelated = home.join("runs/run_unrelated");
        fs::create_dir_all(&selected).expect("selected root");
        fs::create_dir_all(&unrelated).expect("unrelated root");
        fs::write(unrelated.join("state.json"), b"not json").expect("corrupt unrelated state");
        fs::write(unrelated.join(CONTAINED_RUNS_DIR_NAME), b"not a directory")
            .expect("corrupt unrelated tree");
        assert!(
            list_run_roots(home).is_err(),
            "whole-home traversal sees unrelated damage"
        );
        assert_eq!(
            list_run_tree_roots(home, "selected").unwrap(),
            vec![selected.clone()]
        );
        fs::write(selected.join(CONTAINED_RUNS_DIR_NAME), b"not a directory")
            .expect("corrupt selected tree");
        assert!(list_run_tree_roots(home, "selected").is_err());
        fs::remove_file(selected.join(CONTAINED_RUNS_DIR_NAME))
            .expect("remove corrupt selected path");
        fs::create_dir(selected.join(CONTAINED_RUNS_DIR_NAME)).expect("selected child directory");
        fs::write(
            selected.join(CONTAINED_RUNS_DIR_NAME).join("run_broken"),
            b"not a directory",
        )
        .expect("corrupt selected child");
        assert!(list_run_tree_roots(home, "selected").is_err());
    }

    #[test]
    fn resolves_home_and_root_from_nested_run_root() {
        let run_root =
            Path::new("/tmp/.transist/runs/run_root-run/contained_runs/run_root-run__child");
        assert_eq!(
            home_dir_from_run_root(run_root).expect("home dir"),
            PathBuf::from("/tmp/.transist")
        );
        assert_eq!(
            lineage_root_run_dir_from_run_root(run_root).expect("root run dir"),
            PathBuf::from("/tmp/.transist/runs/run_root-run")
        );
    }

    #[test]
    fn rebase_invoke_paths_overwrites_all_four_path_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        fs::create_dir_all(&home_dir).expect("create home");
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).expect("create repo");

        let mut paths = transist_protocol::InvokePaths {
            run_root: "/old/absolute/run_root".to_string(),
            artifacts_dir: "/old/absolute/artifacts".to_string(),
            evidence_dir: "/old/absolute/evidence".to_string(),
            workspace_dir: "/old/absolute/workspace".to_string(),
        };

        rebase_invoke_paths(&mut paths, &home_dir, &repo_root, "run-1", "step_a", 2)
            .expect("rebase invoke paths");

        let expected_run_root = home_dir.join("runs").join("run_run-1");
        assert_eq!(paths.run_root, expected_run_root.display().to_string());
        let expected_exec_dir = expected_run_root
            .join("artifacts")
            .join("step_a")
            .join("execution_002");
        assert_eq!(
            paths.artifacts_dir,
            expected_exec_dir.join("artifacts").display().to_string()
        );
        assert_eq!(
            paths.evidence_dir,
            expected_exec_dir.join("evidence").display().to_string()
        );
        assert_eq!(paths.workspace_dir, repo_root.display().to_string());
    }

    #[test]
    fn resolve_run_root_outcome_distinguishes_found_from_legacy_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        fs::create_dir_all(runs_root(&home_dir)).expect("create runs root");

        // No locator exists -> legacy absent with the top-level path.
        let absent = resolve_run_root_outcome(&home_dir, "run-missing").expect("resolve missing");
        assert_eq!(
            absent,
            RunRootOutcome::LegacyAbsent(home_dir.join("runs/run_run-missing"))
        );

        // A valid locator -> resolved to its exact relative path.
        write_run_locator(
            &home_dir,
            &plan_run_locator(
                &home_dir,
                "run-nested",
                Some(&RunLineage {
                    mode: RunLineageMode::Fresh,
                    source_run_id: None,
                    root_run_id: "run-nested".to_string(),
                    parent_run_id: None,
                    parent_step_id: None,
                }),
            )
            .expect("plan locator"),
        )
        .expect("write locator");
        let found = resolve_run_root_outcome(&home_dir, "run-nested").expect("resolve found");
        assert_eq!(
            found,
            RunRootOutcome::Resolved(home_dir.join("runs/run_run-nested"))
        );
    }

    #[test]
    fn locator_lookup_rejects_traversal_run_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for run_id in ["../escape", "nested/run", ".", ".."] {
            assert!(resolve_run_root_outcome(tmp.path(), run_id).is_err());
        }
    }

    #[test]
    fn locator_rejects_a_relative_path_for_another_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let locator = RunLocator {
            schema: RUN_LOCATOR_SCHEMA_V1.to_string(),
            run_id: "run-a".to_string(),
            root_run_id: "run-a".to_string(),
            parent_run_id: None,
            relative_path: "run_run-b".to_string(),
        };
        write_run_locator(home, &locator).expect("write malformed locator fixture");
        assert!(resolve_run_root_outcome(home, "run-a").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn run_enumeration_rejects_symlinked_contained_runs() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let run_root = runs_root(home).join("run_root");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&run_root).expect("run root");
        fs::create_dir_all(&outside).expect("outside");
        symlink(&outside, run_root.join(CONTAINED_RUNS_DIR_NAME)).expect("symlink");
        assert!(list_run_roots(home).is_err());
    }

    #[test]
    fn resolve_run_root_outcome_fails_closed_on_corrupt_locator() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        fs::create_dir_all(run_index_root(&home_dir)).expect("create index");

        // Corrupt JSON locator must fail closed, never report legacy absence.
        let corrupt = run_locator_path(&home_dir, "run-corrupt");
        fs::write(&corrupt, b"{not valid json").expect("write corrupt locator");
        assert!(resolve_run_root_outcome(&home_dir, "run-corrupt").is_err());

        // A valid locator for a different run id must also fail closed.
        let wrong = run_locator_path(&home_dir, "run-wrong");
        fs::write(
            &wrong,
            b"{\"schema\":\"transist.run_locator.v1\",\"run_id\":\"someone-else\",\"root_run_id\":\"run-wrong\",\"relative_path\":\"run_run-wrong\"}",
        ).expect("write mismatched locator");
        assert!(resolve_run_root_outcome(&home_dir, "run-wrong").is_err());
    }

    #[test]
    fn resolve_run_root_or_default_only_falls_back_on_legacy_absence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        fs::create_dir_all(run_index_root(&home_dir)).expect("create index");

        // Corrupt locator: the default helper still swallows for display/rebasing
        // read paths, but resolve_run_root (strict) fails closed.
        let corrupt = run_locator_path(&home_dir, "run-bad");
        fs::write(&corrupt, b"{nope").expect("write corrupt locator");
        assert!(resolve_run_root(&home_dir, "run-bad").is_err());
        assert!(resolve_run_root_or_default(&home_dir, "run-bad").is_err());

        // Missing locator -> legacy default path.
        assert_eq!(
            resolve_run_root_or_default(&home_dir, "run-absent").expect("legacy fallback"),
            home_dir.join("runs/run_run-absent")
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_run_root_outcome_rejects_valid_locator_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        fs::create_dir_all(run_index_root(&home_dir)).expect("create index");

        let target = run_locator_path(&home_dir, "run-target");
        let target_locator = plan_run_locator(&home_dir, "run-target", None).expect("plan locator");
        fs::write(
            &target,
            serde_json::to_vec_pretty(&target_locator).expect("serialize locator"),
        )
        .expect("write target locator");
        let link = run_locator_path(&home_dir, "run-link");
        symlink(&target, &link).expect("create locator symlink");

        assert!(resolve_run_root_outcome(&home_dir, "run-link").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_run_root_outcome_rejects_dangling_locator_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        fs::create_dir_all(run_index_root(&home_dir)).expect("create index");
        let link = run_locator_path(&home_dir, "run-dangling");
        symlink(tmp.path().join("missing-locator"), &link).expect("create dangling symlink");

        assert!(resolve_run_root_outcome(&home_dir, "run-dangling").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_run_root_outcome_rejects_external_locator_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        fs::create_dir_all(run_index_root(&home_dir)).expect("create index");

        let external = tmp.path().join("external-locator.json");
        let locator = plan_run_locator(&home_dir, "run-external", None).expect("plan locator");
        fs::write(
            &external,
            serde_json::to_vec_pretty(&locator).expect("serialize locator"),
        )
        .expect("write external locator");
        let link = run_locator_path(&home_dir, "run-external");
        symlink(&external, &link).expect("create external locator symlink");

        assert!(resolve_run_root_outcome(&home_dir, "run-external").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_run_root_outcome_rejects_locator_index_ancestor_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        let external_runs = tmp.path().join("external-runs");
        fs::create_dir_all(external_runs.join(RUN_INDEX_DIR_NAME)).expect("external index");
        fs::create_dir(&home_dir).expect("home");
        symlink(&external_runs, home_dir.join(RUNS_DIR_NAME)).expect("runs symlink");

        assert!(resolve_run_root_outcome(&home_dir, "run-ancestor-link").is_err());
    }
}
