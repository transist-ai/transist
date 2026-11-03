// SPDX-License-Identifier: MPL-2.0
//! Query-oriented inspection over persisted local Transist state.
//!
//! This module is intentionally read-only. It indexes run records, runtime
//! events, gate decisions, compiled recipes, and persisted blobs without
//! changing the on-disk storage layout used by the runtime.

use crate::run_paths::resolve_run_root;
use crate::util::STATE_JSON;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use transist_core::{PersistedRunRecord, StepExecutionRecord};
use transist_protocol::{GateDecisionRecord, Recipe, RuntimeEvent};

mod blobs;
mod cache;
mod events;
mod failures;
mod records;

pub use cache::{
    EventsCacheEntry, FileFingerprint, INSPECTION_CACHE_DISABLE_ENV, InspectionCache,
    InspectionCacheKey, InspectionCacheStats, RunCacheSlot, SummaryFingerprint,
};

/// Persisted blob family exposed through inspection queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlobFamily {
    /// Artifact files written under a step execution.
    Artifact,
    /// Evidence files written under a step execution.
    Evidence,
}

impl BlobFamily {
    /// Returns the on-disk directory name used for this blob family.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Artifact => "artifacts",
            Self::Evidence => "evidence",
        }
    }
}

/// Runtime event paired with the sequence number implied by `events.jsonl`.
#[derive(Debug, Clone)]
pub struct IndexedRuntimeEvent {
    /// One-based event position within the persisted event stream.
    pub sequence: u64,
    /// The decoded runtime event payload.
    pub event: RuntimeEvent,
}

/// Descriptor metadata recorded alongside an artifact or evidence blob.
#[derive(Debug, Clone)]
pub struct BlobDescriptor {
    /// Source path recorded by the producer-side descriptor file.
    pub source_path: String,
    /// Optional SHA-256 checksum copied from the descriptor.
    pub sha256: Option<String>,
    /// Optional byte count copied from the descriptor.
    pub bytes: Option<u64>,
    /// Invocation id associated with the descriptor, when recorded.
    pub invoke_id: Option<String>,
    /// Producer-reported status attached to the descriptor, when recorded.
    pub status: Option<String>,
}

/// Blob discovered under a persisted step-execution artifact or evidence tree.
#[derive(Debug, Clone)]
pub struct IndexedBlob {
    /// Artifact/evidence family that owns the file.
    pub family: BlobFamily,
    /// Step id that owns the execution directory.
    pub step_id: String,
    /// Step execution parsed from the execution directory name.
    pub execution: u32,
    /// Relative file name within the family directory.
    pub relative_path: String,
    /// Absolute path to the persisted file on disk.
    pub absolute_path: PathBuf,
    /// File length in bytes from local filesystem metadata.
    pub bytes: u64,
    /// Creation or modification time in Unix milliseconds, when available.
    pub created_at_ms: Option<u64>,
    /// Optional descriptor metadata loaded from the sibling JSON descriptor file.
    pub descriptor: Option<BlobDescriptor>,
}

/// Read-only inspection store over a local Transist home directory.
#[derive(Debug)]
pub struct LocalInspectionStore {
    root: PathBuf,
    cache: Arc<InspectionCache>,
}

impl LocalInspectionStore {
    /// Creates an inspection store rooted at the given Transist home directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_cache(root, Arc::new(InspectionCache::with_default_capacity(10)))
    }

    /// Creates an inspection store with a shared in-memory inspection cache.
    pub fn with_cache(root: impl Into<PathBuf>, cache: Arc<InspectionCache>) -> Self {
        Self {
            root: root.into(),
            cache,
        }
    }

    /// Returns the Transist home directory this store is rooted at.
    pub fn home_dir(&self) -> &Path {
        &self.root
    }

    /// Returns the shared inspection cache used by this store.
    pub fn cache(&self) -> &Arc<InspectionCache> {
        &self.cache
    }

    pub(crate) fn cache_key(&self, run_id: &str) -> Result<InspectionCacheKey> {
        InspectionCacheKey::new(&self.root, run_id)
    }

    /// Returns whether a persisted run state exists for the given run id.
    pub fn run_exists(&self, run_id: &str) -> bool {
        resolve_run_root(&self.root, run_id)
            .map(|root| root.join(STATE_JSON).exists())
            .unwrap_or(false)
    }
}

fn read_dir_sorted(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed reading directory: {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed iterating directory: {}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn parse_execution_dir_name(raw: &str) -> Option<u32> {
    raw.strip_prefix("execution_")?.parse().ok()
}

fn parse_blob_descriptor(
    path: &Path,
    family: BlobFamily,
) -> Result<Option<(String, BlobDescriptor)>> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading blob descriptor: {}", path.display()))?;
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.get("kind").and_then(serde_json::Value::as_str) != Some(family.dir_name()) {
        return Ok(None);
    }
    let Some(source_path) = object
        .get("source_path")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    let Some(target_relative_path) = Path::new(source_path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    Ok(Some((
        target_relative_path,
        BlobDescriptor {
            source_path: source_path.to_string(),
            sha256: object
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            bytes: object.get("bytes").and_then(serde_json::Value::as_u64),
            invoke_id: object
                .get("invoke_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            status: object
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
    )))
}

fn is_safe_relative_path(path: &str) -> bool {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return false;
    }
    candidate
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

fn system_time_to_ms(value: Option<SystemTime>) -> Option<u64> {
    value.and_then(|time| {
        time.duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis() as u64)
    })
}
