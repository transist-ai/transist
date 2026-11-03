// SPDX-License-Identifier: MPL-2.0
//! Filesystem fingerprinting helpers for incremental metrics refresh.

use super::*;
use crate::index::RunFingerprint;
use transist_storage_local::run_paths::{list_run_roots, resolve_run_root, run_id_from_run_root};

pub(crate) fn list_run_ids(home_dir: &Path) -> Result<Vec<String>> {
    let mut run_ids = Vec::new();
    for run_root in list_run_roots(home_dir)? {
        if let Some(run_id) = run_id_from_run_root(&run_root) {
            run_ids.push(run_id);
        }
    }
    run_ids.sort();
    Ok(run_ids)
}

pub(crate) fn run_fingerprint(home_dir: &Path, run_id: &str) -> Result<Option<RunFingerprint>> {
    let root = match run_root(home_dir, run_id) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    if !root.exists() {
        return Ok(None);
    }
    let mut files = Vec::new();
    for relative in ["state.json", "recipe.yaml"] {
        let path = root.join(relative);
        if path.exists() {
            files.push(file_fingerprint(&root, &path)?);
        }
    }
    for dir_name in ["step_executions", "gates", "artifacts"] {
        let path = root.join(dir_name);
        if path.exists() {
            collect_fingerprint_files(&root, &path, &mut files)?;
        }
    }
    files.sort();
    Ok(Some(RunFingerprint { files }))
}

fn collect_fingerprint_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, u64, u64)>,
) -> Result<()> {
    for entry in fs::read_dir(current).with_context(|| {
        format!(
            "failed reading metrics fingerprint dir: {}",
            current.display()
        )
    })? {
        let entry = entry?;
        if entry.path().is_dir() {
            collect_fingerprint_files(root, &entry.path(), out)?;
        } else {
            out.push(file_fingerprint(root, &entry.path())?);
        }
    }
    Ok(())
}

fn file_fingerprint(root: &Path, path: &Path) -> Result<(String, u64, u64)> {
    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "failed reading metrics fingerprint metadata: {}",
            path.display()
        )
    })?;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|timestamp| timestamp.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    Ok((
        path.strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string(),
        modified_ms,
        metadata.len(),
    ))
}

pub(crate) fn run_root(home_dir: &Path, run_id: &str) -> Result<PathBuf> {
    resolve_run_root(home_dir, run_id)
}
