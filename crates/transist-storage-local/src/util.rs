// SPDX-License-Identifier: MPL-2.0

use crate::run_paths::{home_dir_from_run_root, load_run_locator, run_id_from_run_root};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use transist_core::{PersistedRunRecord, RunLineage, RunLineageMode};
use transist_protocol::{KernelValue, RuntimeEvent};

/// File name for the persisted run state document within a run directory.
pub(crate) const STATE_JSON: &str = "state.json";
/// File name for the append-only runtime event stream within a run directory.
pub(crate) const EVENTS_JSONL: &str = "events.jsonl";
/// File name for the append-only classified failure stream within a run directory.
pub(crate) const FAILURES_JSONL: &str = "failures.jsonl";
/// Directory name for persisted step execution records within a run directory.
pub(crate) const STEP_EXECUTIONS_DIR: &str = "step_executions";
/// Hidden top-level directory used by [`LocalRunStore::create_run_staged`]
/// to assemble run-birth artifacts before atomically renaming the staging
/// directory to the final run root.
pub(crate) const RUN_BIRTH_STAGING_DIR: &str = ".transist-run-birth-staging";

/// Prefix for individual run-birth staging directory names.
const RUN_BIRTH_STAGING_PREFIX: &str = "run-birth-";

/// Removes orphaned run-birth staging directories whose owning process is
/// no longer alive. Call this at the start of `create_run_staged` so a
/// crash from a previous run birth does not accumulate stale directories.
/// Directories whose owner PID cannot be determined are left untouched.
pub(crate) fn cleanup_stale_run_birth_staging(home_dir: &Path) -> Result<()> {
    let parent = home_dir.join(RUN_BIRTH_STAGING_DIR);
    if !parent.exists() {
        return Ok(());
    }
    let mut removed = false;
    for entry in fs::read_dir(&parent)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(RUN_BIRTH_STAGING_PREFIX) {
            continue;
        }
        // Directory name: run-birth-<run_id>-<pid>. The PID is the last
        // component after the final dash; run ids may contain dashes.
        let Some(pid_str) = name.rsplit('-').next() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if !process_is_alive(pid) {
            fs::remove_dir_all(&path).with_context(|| {
                format!(
                    "failed removing stale run-birth staging: {}",
                    path.display()
                )
            })?;
            removed = true;
        }
    }
    if removed {
        crate::atomic_io::sync_directory(&parent)?;
    }
    Ok(())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn process_is_alive(pid: u32) -> bool {
    if pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: `kill(pid, 0)` checks liveness without sending a signal.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Repairs a single malformed non-newline-terminated final JSONL fragment
/// in `stream_path`. When a repair is performed, a durable
/// `storage.tail_repaired` runtime event is appended to `events_path` so
/// the recovery is auditable. Returns `true` when the stream was repaired.
pub(crate) fn repair_stream_and_note(
    stream_path: &Path,
    events_path: &Path,
    run_id: &str,
    stream_name: &str,
) -> Result<bool> {
    let marker_path = tail_repair_marker_path(stream_path);
    let pending_length = fs::read_to_string(&marker_path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok());
    let current_length = fs::metadata(stream_path)
        .ok()
        .map(|metadata| metadata.len());
    let should_repair = match (pending_length, current_length) {
        (Some(original_length), Some(current_length)) => current_length >= original_length,
        (Some(_), None) => false,
        (None, _) => stream_has_unterminated_tail(stream_path)?,
    };
    if should_repair && pending_length.is_none() {
        let original_length = current_length.unwrap_or_default();
        write_bytes_atomic(&marker_path, original_length.to_string().as_bytes())?;
    }
    // The occurrence identity for this repair attempt: the pre-repair length
    // recorded by the marker (freshly written above when this is a new
    // attempt, or read from disk when this call is retrying a still-pending
    // attempt). This distinguishes a retry of the *same* interrupted repair
    // (matched and deduplicated below) from a later, distinct repair of the
    // same stream after the file has grown further (a different occurrence,
    // which must still be recorded).
    let occurrence_length = pending_length.or(current_length).unwrap_or_default();
    let repaired = if should_repair {
        let changed = crate::atomic_io::repair_jsonl_tail(stream_path)?;
        changed || pending_length.is_some()
    } else {
        pending_length.is_some()
    };
    if repaired {
        let event = RuntimeEvent {
            kind: "storage.tail_repaired".to_string(),
            run_id: run_id.to_string(),
            step_id: None,
            timestamp_unix_ms: now_unix_ms(),
            payload: KernelValue::Object(BTreeMap::from([
                (
                    "stream".to_string(),
                    KernelValue::String(stream_name.to_string()),
                ),
                (
                    "path".to_string(),
                    KernelValue::String(stream_path.display().to_string()),
                ),
                (
                    "occurrence_length".to_string(),
                    KernelValue::Number(occurrence_length.into()),
                ),
            ])),
        };
        if !recovery_event_exists(events_path, stream_name, stream_path, occurrence_length)? {
            // The recovery note is appended to `events_path`. When that is a
            // stream other than the one just repaired (e.g. a failures-stream
            // repair whose note is recorded into `events.jsonl`), the events
            // stream itself must be repaired first so the note is not glued
            // onto a truncated final fragment inside the events stream. When
            // the repaired stream IS the events stream, it was already
            // repaired above. `repair_jsonl_tail` is a no-op for absent,
            // empty, or well-formed streams.
            if events_path != stream_path {
                // Repair the audit stream through this helper as well, so a
                // simultaneous events-stream repair gets its own durable note.
                repair_stream_and_note(events_path, events_path, run_id, "events")?;
            }
            let mut line = serde_json::to_vec(&event)?;
            line.push(b'\n');
            crate::atomic_io::append_jsonl_durable(events_path, &line)?;
        }
        fs::remove_file(&marker_path).with_context(|| {
            format!(
                "failed removing tail-repair marker: {}",
                marker_path.display()
            )
        })?;
        if let Some(parent) = marker_path.parent() {
            crate::atomic_io::sync_directory(parent)?;
        }
    }
    Ok(repaired)
}

fn tail_repair_marker_path(stream_path: &Path) -> std::path::PathBuf {
    let mut marker = stream_path.as_os_str().to_os_string();
    marker.push(".tail-repair.pending");
    std::path::PathBuf::from(marker)
}

fn stream_has_unterminated_tail(path: &Path) -> Result<bool> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(!bytes.is_empty() && bytes.last() != Some(&b'\n'))
}

fn recovery_event_exists(
    events_path: &Path,
    stream_name: &str,
    stream_path: &Path,
    occurrence_length: u64,
) -> Result<bool> {
    let raw = match fs::read_to_string(events_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for line in raw.lines() {
        let Ok(event) = serde_json::from_str::<RuntimeEvent>(line) else {
            continue;
        };
        if event.kind != "storage.tail_repaired" {
            continue;
        }
        let KernelValue::Object(payload) = event.payload else {
            continue;
        };
        let stream_matches =
            payload.get("stream") == Some(&KernelValue::String(stream_name.to_string()));
        let path_matches =
            payload.get("path") == Some(&KernelValue::String(stream_path.display().to_string()));
        // Match on the specific repair occurrence (the pre-repair stream
        // length), not just stream+path, so a later, distinct repair of the
        // same stream after it has grown further is recorded as its own
        // event instead of being treated as a duplicate of an earlier one.
        // Events recorded before this field existed have no
        // `occurrence_length`; treat those as matching only when the
        // requested occurrence is also unrecorded (defaults to `0`), which
        // preserves old dedup behavior for legacy events without silently
        // swallowing a new, distinct repair.
        let occurrence_matches = match payload.get("occurrence_length") {
            Some(KernelValue::Number(recorded)) => recorded.as_u64() == Some(occurrence_length),
            _ => occurrence_length == 0,
        };
        if stream_matches && path_matches && occurrence_matches {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    write_bytes_atomic(path, &serde_json::to_vec_pretty(value)?)
}

pub(crate) fn write_json_create_new(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    write_bytes_atomic_create_new(path, &serde_json::to_vec_pretty(value)?)
}

pub(crate) fn write_string_atomic(path: &Path, value: &str) -> Result<()> {
    write_bytes_atomic(path, value.as_bytes())
}

pub(crate) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::atomic_io::write_replace_file(path, bytes)
}

pub(crate) fn write_bytes_atomic_create_new(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::atomic_io::write_new_file(path, bytes)
}

pub(crate) fn load_record_from_path(path: &Path) -> Result<PersistedRunRecord> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading run state: {}", path.display()))?;
    let mut record: PersistedRunRecord = serde_json::from_str(&raw)
        .with_context(|| format!("invalid run state json: {}", path.display()))?;
    if record.schema.is_empty() {
        record.schema = crate::run_paths::RUN_STATE_RECORD_SCHEMA_V1.to_string();
    }
    if record.schema != crate::run_paths::RUN_STATE_RECORD_SCHEMA_V1 {
        anyhow::bail!(
            "unsupported run state schema '{}' at {} (expected '{}')",
            record.schema,
            path.display(),
            crate::run_paths::RUN_STATE_RECORD_SCHEMA_V1
        );
    }
    if record.lineage.is_none() {
        let root_run_id =
            infer_root_run_id_from_state_path(path).unwrap_or_else(|| record.run_id.clone());
        record.lineage = Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id,
            parent_run_id: None,
            parent_step_id: None,
        });
    }
    if record.created_at_ms == 0 {
        record.created_at_ms = now_unix_ms();
    }
    // Enforce the activation-baseline invariant on read: no step may carry an
    // `activation_executions_baseline` above its lifetime `executions` count.
    // Invalid persisted baselines fail closed instead of being silently
    // saturated to zero by `activation_executions()`.
    for (step_id, step) in &record.steps {
        if step.activation_executions_baseline > step.executions {
            anyhow::bail!(
                "invalid activation execution baseline for run '{}' step '{}': {} > {}",
                record.run_id,
                step_id,
                step.activation_executions_baseline,
                step.executions
            );
        }
    }
    Ok(record)
}

fn infer_root_run_id_from_state_path(path: &Path) -> Option<String> {
    if path.file_name() != Some(OsStr::new(STATE_JSON)) {
        return None;
    }
    let run_root = path.parent()?;
    let run_id = run_id_from_run_root(run_root)?;
    let home_dir = home_dir_from_run_root(run_root).ok()?;
    load_run_locator(&home_dir, &run_id)
        .ok()
        .map(|locator| locator.root_run_id)
        .filter(|root_run_id| !root_run_id.trim().is_empty())
}
