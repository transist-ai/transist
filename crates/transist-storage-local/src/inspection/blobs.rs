// SPDX-License-Identifier: MPL-2.0
//! Inspection queries backed by persisted artifacts and evidence blobs.

use super::*;
use crate::run_paths::resolve_run_root;

impl LocalInspectionStore {
    /// Lists persisted artifact or evidence blobs for a run, joined with descriptor metadata.
    pub fn list_blobs(&self, run_id: &str, family: BlobFamily) -> Result<Vec<IndexedBlob>> {
        let artifacts_root = resolve_run_root(&self.root, run_id)?.join("artifacts");
        if !artifacts_root.exists() {
            return Ok(Vec::new());
        }
        let fingerprint = FileFingerprint::from_path(&artifacts_root)?.with_context(|| {
            format!(
                "failed collecting blob listing fingerprint: {}",
                artifacts_root.display()
            )
        })?;
        let cache_key = self.cache_key(run_id)?;
        let listing = self.cache.get_blob_listing(
            &cache_key,
            matches!(family, BlobFamily::Artifact),
            fingerprint,
            || list_blobs_uncached(run_id, family, &self.root),
        )?;
        Ok((*listing).clone())
    }

    /// Reads the raw bytes for a persisted artifact or evidence blob after path validation.
    pub fn read_blob_content(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Vec<u8>> {
        if !is_safe_relative_path(relative_path) {
            anyhow::bail!("unsafe blob path");
        }

        let family_root = resolve_run_root(&self.root, run_id)?
            .join("artifacts")
            .join(step_id)
            .join(format!("execution_{execution:03}"))
            .join(family.dir_name());
        let target = family_root.join(relative_path);
        let canonical_root = fs::canonicalize(&family_root)
            .with_context(|| format!("failed resolving family root: {}", family_root.display()))?;
        let canonical_target = fs::canonicalize(&target)
            .with_context(|| format!("failed resolving blob content: {}", target.display()))?;
        if !canonical_target.starts_with(&canonical_root) {
            anyhow::bail!("blob path escapes the run artifact root");
        }
        fs::read(&canonical_target).with_context(|| {
            format!(
                "failed reading blob content: {}",
                canonical_target.display()
            )
        })
    }
}

fn list_blobs_uncached(
    run_id: &str,
    family: BlobFamily,
    root: &std::path::Path,
) -> Result<Vec<IndexedBlob>> {
    let artifacts_root = resolve_run_root(root, run_id)?.join("artifacts");
    let mut descriptors = BTreeMap::new();
    let mut blobs = Vec::new();

    for step_entry in read_dir_sorted(&artifacts_root)? {
        if !step_entry.path().is_dir() {
            continue;
        }
        let Some(step_id) = step_entry.file_name().to_str().map(str::to_string) else {
            continue;
        };

        for execution_entry in read_dir_sorted(&step_entry.path())? {
            if !execution_entry.path().is_dir() {
                continue;
            }
            let Some(execution) =
                parse_execution_dir_name(execution_entry.file_name().to_string_lossy().as_ref())
            else {
                continue;
            };
            let family_dir = execution_entry.path().join(family.dir_name());
            if !family_dir.exists() {
                continue;
            }

            for file_entry in read_dir_sorted(&family_dir)? {
                if !file_entry.path().is_file() {
                    continue;
                }
                if let Some((target, descriptor)) =
                    parse_blob_descriptor(&file_entry.path(), family)?
                {
                    descriptors.insert((step_id.clone(), execution, target), descriptor);
                    continue;
                }

                let metadata = file_entry.metadata().with_context(|| {
                    format!(
                        "failed reading blob metadata: {}",
                        file_entry.path().display()
                    )
                })?;
                let relative_path = file_entry.file_name().to_string_lossy().to_string();
                blobs.push(IndexedBlob {
                    family,
                    step_id: step_id.clone(),
                    execution,
                    relative_path,
                    absolute_path: file_entry.path(),
                    bytes: metadata.len(),
                    created_at_ms: system_time_to_ms(
                        metadata.created().ok().or_else(|| metadata.modified().ok()),
                    ),
                    descriptor: None,
                });
            }
        }
    }

    for blob in &mut blobs {
        let key = (
            blob.step_id.clone(),
            blob.execution,
            blob.relative_path.clone(),
        );
        blob.descriptor = descriptors.remove(&key);
    }
    blobs.sort_by(|left, right| {
        left.step_id
            .cmp(&right.step_id)
            .then(left.execution.cmp(&right.execution))
            .then(left.relative_path.cmp(&right.relative_path))
    });
    Ok(blobs)
}
