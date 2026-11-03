// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use lru::LruCache;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use transist::InspectionGateDecision;
use transist::{BlobFamily, InspectionGate, InspectionStep, TransistInspectionContext};

use crate::actionable::{
    ActionableRun, child_run_id_for_inspection_step, collect_manual_gate_and_workflow_file_lines,
};
use crate::display::progress::progress_top_level_step_id;
use crate::display::summary::gate_suggested_actions;
use crate::guidance::shell_quote;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachedStepOutputBlock {
    pub(crate) lines: Vec<String>,
    pub(crate) anchor_step_id: Option<String>,
}

pub(crate) type AttachedStepOutputBlocks = Vec<AttachedStepOutputBlock>;

pub(crate) fn build_attached_step_output_blocks(
    step_output_lines: &[String],
    anchor_step_id: Option<String>,
    trailing_lines: Vec<String>,
) -> AttachedStepOutputBlocks {
    let mut blocks = Vec::new();
    if !step_output_lines.is_empty() {
        blocks.push(AttachedStepOutputBlock {
            lines: step_output_lines.to_vec(),
            anchor_step_id: anchor_step_id.clone(),
        });
    }
    blocks.push(AttachedStepOutputBlock {
        lines: if blocks.is_empty() {
            trailing_lines
        } else {
            let mut lines = vec![String::new()];
            lines.extend(trailing_lines);
            lines
        },
        anchor_step_id,
    });
    blocks
}

pub(crate) fn format_suggested_action_list(
    title: &str,
    actions: &[crate::display::summary::SuggestedAction],
    selected: Option<(usize, &str)>,
) -> Vec<String> {
    let mut lines = vec![title.to_string()];
    lines.extend(actions.iter().enumerate().map(|(index, action)| {
        if let Some((selected_index, selected_command)) = selected {
            if index == selected_index {
                return format!("  {}. ✔ (selected) {}", index + 1, selected_command);
            }
        }
        format!("  {}. {}", index + 1, action.display_command)
    }));
    lines
}

pub(crate) fn build_persisted_gate_step_output_blocks(
    context: &transist::TransistContext,
    root_run_id: &str,
    actionable: &ActionableRun,
    gate: &InspectionGate,
) -> Result<AttachedStepOutputBlocks> {
    let Some(decision) = gate.decision.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(anchor_step_id) =
        progress_top_level_step_id(root_run_id, &actionable.run_id, &gate.step_id)
    else {
        return Ok(Vec::new());
    };
    let Some((selected_index, selected_command)) =
        persisted_gate_selected_action(root_run_id, decision)
    else {
        return Ok(Vec::new());
    };

    Ok(build_attached_step_output_blocks(
        &persisted_gate_step_output_lines(context, actionable, gate)?,
        Some(anchor_step_id),
        format_suggested_action_list(
            "Next:",
            &gate_suggested_actions(
                root_run_id,
                None,
                transist::resolve_gate_actor_identity_in_home(context.home_dir())?.as_deref(),
            ),
            Some((selected_index, selected_command.as_str())),
        ),
    ))
}

fn persisted_gate_step_output_lines(
    context: &transist::TransistContext,
    actionable: &ActionableRun,
    gate: &InspectionGate,
) -> Result<Vec<String>> {
    let lines = collect_manual_gate_and_workflow_file_lines(
        context,
        &actionable.root_run_id,
        actionable,
        gate,
    )?;
    Ok(lines)
}

pub(crate) fn build_persisted_workflow_file_step_output_blocks(
    root_run_id: &str,
    actionable: &ActionableRun,
    step: &InspectionStep,
) -> AttachedStepOutputBlocks {
    let Some(anchor_step_id) =
        progress_top_level_step_id(root_run_id, &actionable.run_id, &step.step_id)
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    append_workflow_file_path_lines(
        &mut lines,
        workflow_file_paths(&step.output_summary.workflow_files),
    );
    if lines.is_empty() {
        return Vec::new();
    }
    vec![AttachedStepOutputBlock {
        lines,
        anchor_step_id: Some(anchor_step_id),
    }]
}

pub(crate) fn step_has_historical_workflow_files(step: &InspectionStep) -> bool {
    matches!(step.status.as_str(), "success" | "succeeded")
        && !step.output_summary.workflow_files.is_empty()
}

pub(crate) fn persisted_gate_selected_action(
    root_run_id: &str,
    decision: &InspectionGateDecision,
) -> Option<(usize, String)> {
    match decision.state.as_str() {
        "approved" => Some((
            0,
            crate::guidance::render_gate_command(
                "approve",
                root_run_id,
                &None,
                decision.actor.as_deref(),
                decision.reason.as_deref(),
            ),
        )),
        "request_changes" => Some((
            1,
            crate::guidance::render_gate_command(
                "request-changes",
                root_run_id,
                &None,
                decision.actor.as_deref(),
                decision.reason.as_deref(),
            ),
        )),
        "rejected" => Some((
            2,
            crate::guidance::render_gate_command(
                "reject",
                root_run_id,
                &None,
                decision.actor.as_deref(),
                decision.reason.as_deref(),
            ),
        )),
        _ => None,
    }
}

pub(crate) fn collect_persisted_step_output_blocks(
    context: &transist::TransistContext,
    inspection: &TransistInspectionContext,
    root_run_id: &str,
    cache_owner: &PersistedStepOutputCacheOwner,
) -> Result<AttachedStepOutputBlocks> {
    if !persisted_step_output_cache_enabled() {
        return collect_persisted_step_output_blocks_uncached(context, inspection, root_run_id);
    }

    let fingerprint = compute_persisted_step_output_fingerprint(context, inspection, root_run_id)?;

    let cache_key = PersistedStepOutputCacheKey {
        home_dir: canonical_cache_path(context.home_dir())?,
        root_run_id: root_run_id.to_string(),
    };
    if let Some(entry) = cache_owner.lock().get(&cache_key)
        && entry.fingerprint == fingerprint
    {
        return Ok(entry.blocks.clone());
    }

    let blocks = collect_persisted_step_output_blocks_uncached(context, inspection, root_run_id)?;

    let mut guard = cache_owner.lock();
    guard.put(
        cache_key,
        PersistedStepOutputCacheEntry {
            fingerprint,
            blocks: blocks.clone(),
        },
    );
    Ok(blocks)
}

fn collect_persisted_step_output_blocks_uncached(
    context: &transist::TransistContext,
    inspection: &TransistInspectionContext,
    root_run_id: &str,
) -> Result<AttachedStepOutputBlocks> {
    let mut blocks = Vec::new();
    let mut visited = BTreeSet::new();
    collect_persisted_step_output_blocks_for_run(
        context,
        inspection,
        root_run_id,
        root_run_id,
        &mut visited,
        &mut blocks,
    )?;
    Ok(blocks)
}

/// File-fingerprint snapshot used by the persisted step output memo to detect
/// changes in a single run's `events.jsonl` and `state.json` files cheaply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PersistedRunFingerprint {
    events_mtime_nanos: i128,
    events_len: u64,
    state_mtime_nanos: i128,
    state_len: u64,
}

fn file_metadata_fingerprint(path: &Path) -> (i128, u64) {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let mtime_nanos = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos() as i128)
                .unwrap_or(0);
            (mtime_nanos, metadata.len())
        }
        Err(_) => (0, 0),
    }
}

fn persisted_run_fingerprint(home_dir: &Path, run_id: &str) -> PersistedRunFingerprint {
    let Ok(run_root) =
        transist::TransistContext::with_home(home_dir, home_dir).run_root_or_default(run_id)
    else {
        return PersistedRunFingerprint::default();
    };
    let (events_mtime_nanos, events_len) =
        file_metadata_fingerprint(&run_root.join("events.jsonl"));
    let (state_mtime_nanos, state_len) = file_metadata_fingerprint(&run_root.join("state.json"));
    PersistedRunFingerprint {
        events_mtime_nanos,
        events_len,
        state_mtime_nanos,
        state_len,
    }
}

/// Composite fingerprint capturing the (events, state) file fingerprints for
/// the root run plus every run reported in `detail.contained_runs`. Used as
/// the cache key for `collect_persisted_step_output_blocks` so the recursive
/// traversal and formatting only re-runs when something actually changed on
/// disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PersistedStepOutputFingerprint {
    runs: Vec<(String, PersistedRunFingerprint)>,
}

#[derive(Debug, Clone)]
struct PersistedStepOutputCacheEntry {
    fingerprint: PersistedStepOutputFingerprint,
    blocks: AttachedStepOutputBlocks,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PersistedStepOutputCacheKey {
    home_dir: std::path::PathBuf,
    root_run_id: String,
}

fn persisted_step_output_cache_enabled() -> bool {
    !std::env::var(transist::INSPECTION_CACHE_DISABLE_ENV)
        .ok()
        .map(|value| value.trim().eq_ignore_ascii_case("off"))
        .unwrap_or(false)
}

const PERSISTED_STEP_OUTPUT_CACHE_CAPACITY: usize = 32;

/// Owns the persisted output memo for one CLI display lifetime.
#[derive(Debug)]
pub(crate) struct PersistedStepOutputCacheOwner {
    cache: Mutex<LruCache<PersistedStepOutputCacheKey, PersistedStepOutputCacheEntry>>,
}

impl Default for PersistedStepOutputCacheOwner {
    fn default() -> Self {
        Self {
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(PERSISTED_STEP_OUTPUT_CACHE_CAPACITY)
                    .expect("non-zero step output cache capacity"),
            )),
        }
    }
}

impl PersistedStepOutputCacheOwner {
    fn lock(
        &self,
    ) -> MutexGuard<'_, LruCache<PersistedStepOutputCacheKey, PersistedStepOutputCacheEntry>> {
        match self.cache.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = LruCache::new(
                    NonZeroUsize::new(PERSISTED_STEP_OUTPUT_CACHE_CAPACITY)
                        .expect("non-zero step output cache capacity"),
                );
                self.cache.clear_poison();
                guard
            }
        }
    }
}

fn canonical_cache_path(path: &Path) -> Result<std::path::PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to canonicalize cache path: {}", path.display()))
}

fn compute_persisted_step_output_fingerprint(
    context: &transist::TransistContext,
    inspection: &TransistInspectionContext,
    root_run_id: &str,
) -> Result<PersistedStepOutputFingerprint> {
    let home_dir = context.home_dir();
    let mut runs = Vec::new();
    runs.push((
        root_run_id.to_string(),
        persisted_run_fingerprint(home_dir, root_run_id),
    ));
    if let Some(detail) = inspection.get_run(root_run_id)? {
        for summary in &detail.contained_runs {
            runs.push((
                summary.run_id.clone(),
                persisted_run_fingerprint(home_dir, &summary.run_id),
            ));
        }
    }
    runs.sort_by(|a, b| a.0.cmp(&b.0));
    runs.dedup_by(|a, b| a.0 == b.0);
    Ok(PersistedStepOutputFingerprint { runs })
}

fn collect_persisted_step_output_blocks_for_run(
    context: &transist::TransistContext,
    inspection: &TransistInspectionContext,
    root_run_id: &str,
    run_id: &str,
    visited: &mut BTreeSet<String>,
    blocks: &mut AttachedStepOutputBlocks,
) -> Result<()> {
    if !visited.insert(run_id.to_string()) {
        return Ok(());
    }
    let Some(detail) = inspection.get_run(run_id)? else {
        return Ok(());
    };
    let gates_by_step_id = inspection
        .list_gates(run_id)?
        .unwrap_or_default()
        .into_iter()
        .filter(|gate| gate.decision.is_some())
        .map(|gate| (gate.step_id.clone(), gate))
        .collect::<BTreeMap<_, _>>();
    let actionable = ActionableRun {
        root_run_id: root_run_id.to_string(),
        run_id: run_id.to_string(),
        detail: detail.clone(),
    };

    for step in &detail.steps {
        if let Some(gate) = gates_by_step_id.get(&step.step_id) {
            blocks.extend(build_persisted_gate_step_output_blocks(
                context,
                root_run_id,
                &actionable,
                gate,
            )?);
        } else if step_has_historical_workflow_files(step) {
            blocks.extend(build_persisted_workflow_file_step_output_blocks(
                root_run_id,
                &actionable,
                step,
            ));
        }
        if step.step_kind == "subflow"
            && let Some(child_run_id) = child_run_id_for_inspection_step(step)
        {
            collect_persisted_step_output_blocks_for_run(
                context,
                inspection,
                root_run_id,
                &child_run_id,
                visited,
                blocks,
            )?;
        }
    }

    Ok(())
}

pub(crate) fn append_failed_step_blob_path_lines(
    lines: &mut Vec<String>,
    home_dir: &std::path::Path,
    run_id: &str,
    step: &InspectionStep,
) {
    append_workflow_file_path_lines(
        lines,
        workflow_file_paths(&step.output_summary.workflow_files),
    );

    let artifact_paths = blob_refs_to_paths(
        home_dir,
        run_id,
        BlobFamily::Artifact,
        &step.output_summary.artifact_refs,
    );
    if !artifact_paths.is_empty() {
        lines.push("Artifacts:".to_string());
        lines.extend(
            artifact_paths
                .into_iter()
                .map(|path| format!("  {}", shell_quote(&path))),
        );
    }

    let evidence_paths = blob_refs_to_paths(
        home_dir,
        run_id,
        BlobFamily::Evidence,
        &step.output_summary.evidence_refs,
    );
    if !evidence_paths.is_empty() {
        lines.push("Evidence:".to_string());
        lines.extend(
            evidence_paths
                .into_iter()
                .map(|path| format!("  {}", shell_quote(&path))),
        );
    }
}

pub(crate) fn append_workflow_file_path_lines(lines: &mut Vec<String>, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    lines.push("Workflow files:".to_string());
    lines.extend(
        paths
            .into_iter()
            .map(|path| format!("  {}", shell_quote(&path))),
    );
}

/// Returns a locally resolved absolute file path, never a relative locator.
pub(crate) fn workflow_file_local_path(
    file: &transist::InspectionWorkflowFileRef,
) -> Option<&Path> {
    file.local_path.as_deref().filter(|path| path.is_absolute())
}

/// Lists absolute local file locations for summaries and diagnostic pages.
/// References without a local location remain visible with an unavailable label.
pub(crate) fn workflow_file_paths(refs: &[transist::InspectionWorkflowFileRef]) -> Vec<String> {
    refs.iter()
        .map(|file| {
            workflow_file_local_path(file)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| format!("{} (path unavailable)", file.name))
        })
        .collect()
}

pub(crate) fn blob_refs_to_paths(
    home_dir: &std::path::Path,
    run_id: &str,
    family: BlobFamily,
    refs: &[transist::InspectionStepBlobRef],
) -> Vec<String> {
    let Ok(run_root) =
        transist::TransistContext::with_home(home_dir, home_dir).run_root_or_default(run_id)
    else {
        return Vec::new();
    };
    refs.iter()
        .map(|blob_ref| {
            run_root
                .join("artifacts")
                .join(&blob_ref.step_id)
                .join(format!("execution_{:03}", blob_ref.execution))
                .join(family.dir_name())
                .join(&blob_ref.relative_path)
                .display()
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_file_summaries_keep_unavailable_paths_visible() {
        let file = transist::InspectionWorkflowFileRef {
            step_id: "produce".to_string(),
            field_name: "report_file".to_string(),
            locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
            relative_path: "reports/output.md".to_string(),
            local_path: Some(Path::new("/workspace/reports/output.md").to_path_buf()),
            name: "output.md".to_string(),
            bytes: 6,
        };
        let mut missing = file.clone();
        missing.local_path = None;
        let mut relative = file.clone();
        relative.local_path = Some(Path::new("relative/output.md").to_path_buf());
        let paths = workflow_file_paths(&[file, missing, relative]);
        assert_eq!(
            paths,
            vec![
                "/workspace/reports/output.md",
                "output.md (path unavailable)",
                "output.md (path unavailable)",
            ]
        );
        let mut lines = Vec::new();
        append_workflow_file_path_lines(&mut lines, paths);
        assert_eq!(lines.len(), 4);
        assert!(lines[2].contains("output.md (path unavailable)"));
        assert!(!lines.join("\n").contains("relative/output.md"));
    }

    fn output_cache_entry(index: usize) -> PersistedStepOutputCacheEntry {
        PersistedStepOutputCacheEntry {
            fingerprint: PersistedStepOutputFingerprint {
                runs: vec![(format!("run-{index}"), PersistedRunFingerprint::default())],
            },
            blocks: Vec::new(),
        }
    }

    fn output_cache_key(index: usize) -> PersistedStepOutputCacheKey {
        PersistedStepOutputCacheKey {
            home_dir: Path::new("/home").to_path_buf(),
            root_run_id: format!("run-{index}"),
        }
    }

    #[test]
    fn persisted_output_cache_has_capacity_32_and_evicts_least_recently_used_entry() {
        let owner = PersistedStepOutputCacheOwner::default();
        for index in 0..33 {
            owner
                .lock()
                .put(output_cache_key(index), output_cache_entry(index));
        }

        let cache = owner.lock();
        assert_eq!(cache.cap().get(), PERSISTED_STEP_OUTPUT_CACHE_CAPACITY);
        assert_eq!(cache.len(), PERSISTED_STEP_OUTPUT_CACHE_CAPACITY);
        assert!(!cache.contains(&output_cache_key(0)));
        assert!(cache.contains(&output_cache_key(1)));
    }

    #[test]
    fn persisted_output_cache_owners_are_isolated_and_canonical_key_rejects_missing_paths() {
        let first = PersistedStepOutputCacheOwner::default();
        let second = PersistedStepOutputCacheOwner::default();
        first.lock().put(output_cache_key(1), output_cache_entry(1));
        assert_eq!(first.lock().len(), 1);
        assert_eq!(second.lock().len(), 0);

        let home = tempfile::tempdir().expect("home");
        let nested = home.path().join("nested");
        std::fs::create_dir(&nested).expect("nested");
        let path = home.path().join("state.json");
        std::fs::write(&path, "state").expect("state file");
        let alias = nested.join("..").join("state.json");
        assert_eq!(
            canonical_cache_path(&path).expect("canonical path"),
            canonical_cache_path(&alias).expect("canonical alias")
        );
        assert!(canonical_cache_path(&home.path().join("missing.json")).is_err());
    }

    #[test]
    fn persisted_output_cache_recovers_from_poison_and_supports_concurrent_access() {
        let owner = std::sync::Arc::new(PersistedStepOutputCacheOwner::default());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.cache.lock().expect("cache lock");
            panic!("poison persisted output cache");
        }));
        assert!(owner.cache.is_poisoned());
        let cache = owner.lock();
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.cap().get(), PERSISTED_STEP_OUTPUT_CACHE_CAPACITY);
        drop(cache);
        assert!(!owner.cache.is_poisoned());

        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let owner = std::sync::Arc::clone(&owner);
                std::thread::spawn(move || {
                    owner
                        .lock()
                        .put(output_cache_key(worker), output_cache_entry(worker));
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("worker");
        }
        assert_eq!(owner.lock().len(), 8);
    }
}
