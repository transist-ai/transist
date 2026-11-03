// SPDX-License-Identifier: MPL-2.0
use crate::atomic_io::create_dir_all_durable;
use crate::inspection::{BlobFamily, InspectionCache, LocalInspectionStore, SummaryFingerprint};
use crate::run_paths::{list_run_roots, resolve_run_root, run_id_from_run_root};
use crate::util::{STATE_JSON, write_json};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use transist_core::{KernelStepState, KernelStepStatus, PersistedRunRecord};
use transist_protocol::{Recipe, RuntimeEvent, StepExecutionDescriptor};

const SUMMARY_JSON: &str = "summary.json";

/// Persisted lightweight run summary used by inspection list surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedRunSummaryRecord {
    /// Stable run identifier.
    pub run_id: String,
    /// Workflow id referenced by the run.
    pub workflow_id: String,
    /// Workflow version referenced by the run.
    pub workflow_version: String,
    /// Current local run status as a lowercase string.
    pub status: String,
    /// Run creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Last update timestamp in Unix milliseconds.
    pub updated_at_ms: u64,
    /// First started-at timestamp observed for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Final finished-at timestamp observed for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Current local step pointer when the run is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step_id: Option<String>,
    /// Whether the run is currently blocked on a manual gate.
    #[serde(default)]
    pub has_blocked_gates: bool,
    /// Whether the run has recorded any artifacts.
    #[serde(default)]
    pub has_artifacts: bool,
    /// Whether the run has recorded any runtime events.
    #[serde(default)]
    pub has_events: bool,
    /// Total persisted artifact count.
    #[serde(default)]
    pub artifact_count: usize,
    /// Total persisted evidence count.
    #[serde(default)]
    pub evidence_count: usize,
    /// Total persisted event count.
    #[serde(default)]
    pub event_count: usize,
    /// Root run id for the lineage tree containing this run.
    pub root_run_id: String,
    /// Optional direct parent run id for embedded runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Optional parent step id when this run is nested beneath a parent step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_step_id: Option<String>,
    /// Manual gate step ids declared by the persisted recipe snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_gate_step_ids: Vec<String>,
}

/// Filesystem-backed storage for per-run summary sidecars.
#[derive(Debug, Clone)]
pub struct LocalRunSummaryStore {
    root: PathBuf,
    cache: Arc<InspectionCache>,
}

impl LocalRunSummaryStore {
    /// Creates a run-summary store rooted at the given Transist home directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_cache(root, Arc::new(InspectionCache::with_default_capacity(10)))
    }

    /// Creates a run-summary store rooted at the given Transist home directory and cache.
    pub fn with_cache(root: impl Into<PathBuf>, cache: Arc<InspectionCache>) -> Self {
        Self {
            root: root.into(),
            cache,
        }
    }

    fn run_root(&self, run_id: &str) -> Result<PathBuf> {
        resolve_run_root(&self.root, run_id)
    }

    fn cache_key(&self, run_id: &str) -> Result<crate::inspection::InspectionCacheKey> {
        crate::inspection::InspectionCacheKey::new(&self.root, run_id)
    }

    fn summary_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self.run_root(run_id)?.join(SUMMARY_JSON))
    }

    fn run_state_exists(&self, run_id: &str) -> bool {
        self.run_root(run_id)
            .map(|root| root.join(STATE_JSON).exists())
            .unwrap_or(false)
    }

    /// Loads one persisted run summary when the sidecar exists.
    pub fn load_run_summary(&self, run_id: &str) -> Result<Option<PersistedRunSummaryRecord>> {
        let path = self.summary_path(run_id)?;
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(self.load_summary_at(run_id, &path)?))
    }

    /// Reads one atomic summary snapshot and caches its parsed projection by
    /// content. Hashing and parsing use the same captured bytes, so independent
    /// readers observe replacements even when length and mtime are unchanged.
    fn load_summary_at(&self, run_id: &str, path: &Path) -> Result<PersistedRunSummaryRecord> {
        let raw = fs::read(path)
            .with_context(|| format!("failed reading run summary: {}", path.display()))?;
        let fingerprint = SummaryFingerprint::from_bytes(&raw);
        let cache_key = self.cache_key(run_id)?;
        let cached = self.cache.get_summary(&cache_key, fingerprint, || {
            let value: serde_json::Value = serde_json::from_slice(&raw)
                .with_context(|| format!("invalid run summary json: {}", path.display()))?;
            let summary = serde_json::from_value(value.clone())
                .with_context(|| format!("invalid run summary json: {}", path.display()))?;
            self.reconstruct_legacy_counts(run_id, summary, &value)
        })?;
        Ok((*cached).clone())
    }

    /// Lists every persisted run summary currently present under the local home.
    pub fn list_run_summaries(&self) -> Result<Vec<PersistedRunSummaryRecord>> {
        let mut out = Vec::new();
        for run_root in list_run_roots(&self.root)? {
            let path = run_root.join(SUMMARY_JSON);
            if !path.exists() {
                continue;
            }
            let Some(run_id) = run_id_from_run_root(&run_root) else {
                continue;
            };
            out.push(self.load_summary_at(&run_id, &path)?);
        }
        Ok(out)
    }

    fn reconstruct_legacy_counts(
        &self,
        run_id: &str,
        mut summary: PersistedRunSummaryRecord,
        raw: &serde_json::Value,
    ) -> Result<PersistedRunSummaryRecord> {
        let needs_artifact_count = raw.get("artifact_count").is_none()
            || (summary.has_artifacts && summary.artifact_count == 0);
        let needs_evidence_count =
            raw.get("evidence_count").is_none() || summary.evidence_count == 0;
        let needs_event_count =
            raw.get("event_count").is_none() || (summary.has_events && summary.event_count == 0);
        if !(needs_artifact_count || needs_evidence_count || needs_event_count) {
            return Ok(summary);
        }

        // The summary cache loader must not recursively borrow its own cache
        // while deriving counts from the underlying inspection records.
        let inspection = LocalInspectionStore::with_cache(
            self.root.clone(),
            Arc::new(InspectionCache::with_default_capacity(10)),
        );
        if needs_artifact_count {
            summary.artifact_count = inspection.list_blobs(run_id, BlobFamily::Artifact)?.len();
            summary.has_artifacts |= summary.artifact_count > 0;
        }
        if needs_evidence_count {
            summary.evidence_count = inspection.list_blobs(run_id, BlobFamily::Evidence)?.len();
        }
        if needs_event_count {
            summary.event_count = inspection.list_events(run_id)?.len();
            summary.has_events |= summary.event_count > 0;
        }
        Ok(summary)
    }

    /// Writes one persisted run summary sidecar.
    pub fn write_run_summary(&self, summary: &PersistedRunSummaryRecord) -> Result<()> {
        let path = self.summary_path(&summary.run_id)?;
        if let Some(parent) = path.parent() {
            create_dir_all_durable(parent)
                .with_context(|| format!("failed creating summary dir: {}", parent.display()))?;
        }
        write_json(&path, summary)
    }

    /// Rebuilds one run summary from persisted state, recipe, and blob trees.
    pub fn refresh_run(&self, run_id: &str) -> Result<PersistedRunSummaryRecord> {
        // Rebuild inputs are authoritative, not read-through projections. The
        // writer's shared cache may have been warmed by another inspection read,
        // and equal-size atomic state replacements can share its mtime tick.
        // Bypass all input caches here; keep content-validated summary parsing
        // cached independently for ordinary readers and event-count retention.
        let inspection =
            LocalInspectionStore::with_cache(self.root.clone(), Arc::new(InspectionCache::new(0)));
        let record = inspection.load_run_record(run_id)?;
        let existing = self.load_run_summary(run_id)?;
        let recipe = inspection.load_run_recipe(run_id)?;
        let artifact_count = inspection.list_blobs(run_id, BlobFamily::Artifact)?.len();
        let evidence_count = inspection.list_blobs(run_id, BlobFamily::Evidence)?.len();
        let (started_at_ms, finished_at_ms) = step_timestamps(record.steps.values());
        let root_run_id = effective_root_run_id(self, &record);
        let parent_run_id = embedded_parent_run_id(self, &record, &root_run_id);
        let parent_step_id = embedded_parent_step_id(&record, parent_run_id.as_deref());
        let manual_gate_step_ids = recipe
            .as_ref()
            .map(manual_gate_step_ids)
            .unwrap_or_default();
        let manual_gate_steps = manual_gate_step_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let has_blocked_gates = record.steps.iter().any(|(step_id, step)| {
            step.status == KernelStepStatus::Blocked && manual_gate_steps.contains(step_id)
        });
        let event_count = existing
            .as_ref()
            .map(|summary| summary.event_count)
            .unwrap_or(0);
        let summary = PersistedRunSummaryRecord {
            run_id: record.run_id.clone(),
            workflow_id: record.workflow_id.clone(),
            workflow_version: record.workflow_version.clone(),
            status: record.status.as_str().to_string(),
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
            started_at_ms,
            finished_at_ms,
            current_step_id: (!record.status.is_terminal())
                .then(|| current_step_id(&record.steps))
                .flatten(),
            has_blocked_gates,
            has_artifacts: artifact_count > 0,
            has_events: event_count > 0,
            artifact_count,
            evidence_count,
            event_count,
            root_run_id,
            parent_run_id,
            parent_step_id,
            manual_gate_step_ids,
        };
        self.write_run_summary(&summary)?;
        Ok(summary)
    }

    /// Records one published event in the persisted run summary sidecar.
    pub fn note_event(&self, event: &RuntimeEvent) -> Result<()> {
        let mut summary = self
            .load_run_summary(&event.run_id)?
            .with_context(|| format!("run summary for '{}' was not found", event.run_id))?;
        summary.event_count = summary.event_count.saturating_add(1);
        summary.has_events = true;
        summary.updated_at_ms = summary.updated_at_ms.max(event.timestamp_unix_ms);
        self.write_run_summary(&summary)
    }
}

fn step_timestamps<'a>(
    steps: impl IntoIterator<Item = &'a KernelStepState>,
) -> (Option<u64>, Option<u64>) {
    let mut started: Option<u64> = None;
    let mut finished: Option<u64> = None;
    for step in steps {
        started = match (started, step.started_at_ms) {
            (Some(current), Some(candidate)) => Some(current.min(candidate)),
            (None, Some(candidate)) => Some(candidate),
            (current, None) => current,
        };
        finished = match (finished, step.finished_at_ms) {
            (Some(current), Some(candidate)) => Some(current.max(candidate)),
            (None, Some(candidate)) => Some(candidate),
            (current, None) => current,
        };
    }
    (started, finished)
}

fn current_step_id(steps: &std::collections::BTreeMap<String, KernelStepState>) -> Option<String> {
    steps
        .iter()
        .max_by(|left, right| {
            step_priority(left.1.status)
                .cmp(&step_priority(right.1.status))
                .then(left.1.started_at_ms.cmp(&right.1.started_at_ms))
        })
        .and_then(|(step_id, state)| (step_priority(state.status) > 0).then_some(step_id.clone()))
}

fn step_priority(status: KernelStepStatus) -> u8 {
    match status {
        KernelStepStatus::Running => 6,
        KernelStepStatus::Dispatching => 5,
        KernelStepStatus::Blocked => 4,
        KernelStepStatus::RetryWait => 3,
        KernelStepStatus::Runnable => 2,
        KernelStepStatus::Pending => 1,
        _ => 0,
    }
}

fn manual_gate_step_ids(recipe: &Recipe) -> Vec<String> {
    recipe
        .steps
        .iter()
        .filter(|step| {
            matches!(
                &step.execution,
                StepExecutionDescriptor::Control { primitive }
                    if primitive == "control.gate.manual"
            )
        })
        .map(|step| step.id.clone())
        .collect()
}

fn effective_root_run_id(store: &LocalRunSummaryStore, record: &PersistedRunRecord) -> String {
    let persisted_root = record
        .lineage
        .as_ref()
        .map(|lineage| lineage.root_run_id.trim())
        .filter(|root_run_id| !root_run_id.is_empty())
        .map(ToOwned::to_owned);

    match persisted_root {
        Some(root_run_id) if root_run_id != record.run_id => root_run_id,
        Some(_) | None => infer_embedded_root_run_id(store, &record.run_id)
            .unwrap_or_else(|| record.run_id.clone()),
    }
}

fn embedded_parent_run_id(
    store: &LocalRunSummaryStore,
    record: &PersistedRunRecord,
    root_run_id: &str,
) -> Option<String> {
    if let Some(parent_run_id) = record
        .lineage
        .as_ref()
        .and_then(|lineage| lineage.parent_run_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if store.run_state_exists(parent_run_id) {
            return Some(parent_run_id.to_string());
        }
    }
    if record.run_id == root_run_id {
        return None;
    }
    let prefix = format!("{root_run_id}__");
    if !record.run_id.starts_with(&prefix) {
        return None;
    }
    let (parent_run_id, _) = record.run_id.rsplit_once("__")?;
    if parent_run_id.is_empty() || !store.run_state_exists(parent_run_id) {
        return None;
    }
    Some(parent_run_id.to_string())
}

fn embedded_parent_step_id(
    record: &PersistedRunRecord,
    parent_run_id: Option<&str>,
) -> Option<String> {
    if let Some(parent_step_id) = record
        .lineage
        .as_ref()
        .and_then(|lineage| lineage.parent_step_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(parent_step_id.to_string());
    }
    direct_embedded_step_id(parent_run_id?, &record.run_id)
}

fn infer_embedded_root_run_id(store: &LocalRunSummaryStore, run_id: &str) -> Option<String> {
    let (root_run_id, _) = run_id.split_once("__")?;
    if root_run_id.is_empty() || root_run_id == run_id || !store.run_state_exists(root_run_id) {
        return None;
    }
    Some(root_run_id.to_string())
}

fn direct_embedded_step_id(parent_run_id: &str, child_run_id: &str) -> Option<String> {
    let prefix = format!("{parent_run_id}__");
    let remainder = child_run_id.strip_prefix(&prefix)?;
    (!remainder.is_empty() && !remainder.contains("__")).then_some(remainder.to_string())
}
