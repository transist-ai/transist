// SPDX-License-Identifier: MPL-2.0
//! Inspection queries backed by append-only runtime event and gate history.

use super::*;
use crate::run_paths::resolve_run_root;
use crate::util::EVENTS_JSONL;
use std::sync::Arc;

impl LocalInspectionStore {
    /// Lists decoded runtime events from the run's `events.jsonl` stream.
    pub fn list_events(&self, run_id: &str) -> Result<Vec<IndexedRuntimeEvent>> {
        let path = resolve_run_root(&self.root, run_id)?.join(EVENTS_JSONL);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let fingerprint = FileFingerprint::from_path(&path)?
            .with_context(|| format!("missing run events file: {}", path.display()))?;
        let cache_key = self.cache_key(run_id)?;
        let parsed = self.cache.with_run_slot(
            &cache_key,
            |slot| {
                slot.events.as_ref().and_then(|entry| {
                    if entry.fingerprint == fingerprint {
                        Some(Arc::clone(&entry.parsed))
                    } else {
                        None
                    }
                })
            },
            |slot| {
                let previous = slot.events.clone();
                let mut events = if let Some(previous) = previous {
                    maybe_tail_parse_events(&path, previous, fingerprint)?
                } else {
                    parse_events_full(&path)?
                };
                events.sort_by_key(|event| event.sequence);
                let parsed = Arc::new(events);
                slot.events = Some(EventsCacheEntry {
                    fingerprint,
                    parsed: Arc::clone(&parsed),
                });
                Ok(parsed)
            },
        )?;
        Ok((*parsed).clone())
    }

    /// Lists unique gate decisions recorded for the run, including history entries.
    pub fn list_gate_decisions(&self, run_id: &str) -> Result<Vec<GateDecisionRecord>> {
        let root = resolve_run_root(&self.root, run_id)?.join("gates");
        if !root.exists() {
            return Ok(Vec::new());
        }

        let mut decisions = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in read_dir_sorted(&root)? {
            if !entry.path().is_file() {
                continue;
            }
            let raw = fs::read_to_string(entry.path()).with_context(|| {
                format!("failed reading gate decision: {}", entry.path().display())
            })?;
            if !seen.insert(raw.clone()) {
                continue;
            }
            let decision = serde_json::from_str(&raw).with_context(|| {
                format!("invalid gate decision json: {}", entry.path().display())
            })?;
            decisions.push(decision);
        }
        let history_dir = root.join("history");
        if history_dir.exists() {
            for entry in read_dir_sorted(&history_dir)? {
                if !entry.path().is_file() {
                    continue;
                }
                let raw = fs::read_to_string(entry.path()).with_context(|| {
                    format!("failed reading gate decision: {}", entry.path().display())
                })?;
                if !seen.insert(raw.clone()) {
                    continue;
                }
                let decision = serde_json::from_str(&raw).with_context(|| {
                    format!("invalid gate decision json: {}", entry.path().display())
                })?;
                decisions.push(decision);
            }
        }
        Ok(decisions)
    }
}

fn parse_events_full(path: &std::path::Path) -> Result<Vec<IndexedRuntimeEvent>> {
    let records = crate::atomic_io::read_jsonl_records(path)?;
    let mut events = Vec::new();
    let mut sequence = 0u64;
    for raw in records {
        let event = serde_json::from_slice::<RuntimeEvent>(&raw)
            .with_context(|| format!("invalid event line in {}", path.display()))?;
        sequence = sequence.saturating_add(1);
        events.push(IndexedRuntimeEvent { sequence, event });
    }
    Ok(events)
}

fn parse_events_lines(
    path: &std::path::Path,
    raw: &str,
    start_sequence: u64,
) -> Result<Vec<IndexedRuntimeEvent>> {
    let mut events = Vec::new();
    let mut sequence = start_sequence;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<RuntimeEvent>(line)
            .with_context(|| format!("invalid event line in {}", path.display()))?;
        sequence = sequence.saturating_add(1);
        events.push(IndexedRuntimeEvent { sequence, event });
    }
    Ok(events)
}

fn maybe_tail_parse_events(
    path: &std::path::Path,
    previous: EventsCacheEntry,
    current: FileFingerprint,
) -> Result<Vec<IndexedRuntimeEvent>> {
    if current.len < previous.fingerprint.len
        || current.mtime_nanos < previous.fingerprint.mtime_nanos
    {
        return parse_events_full(path);
    }
    if current.len == previous.fingerprint.len {
        // Equal length alone does not prove equal content: a truncate
        // followed by a rewrite of the same byte length (e.g. tail repair
        // replacing a malformed fragment, or a step re-emitting an event of
        // identical size) changes `mtime` without changing `len`. Only
        // return the cached parse when both `len` and `mtime` are
        // unchanged; otherwise fall back to a full reparse.
        if current.mtime_nanos == previous.fingerprint.mtime_nanos {
            return Ok((*previous.parsed).clone());
        }
        return parse_events_full(path);
    }
    let bytes =
        fs::read(path).with_context(|| format!("failed reading run events: {}", path.display()))?;
    let start = previous.fingerprint.len as usize;
    if start > bytes.len() {
        return parse_events_full(path);
    }
    let tail = &bytes[start..];
    let tail_str = match std::str::from_utf8(tail) {
        Ok(value) => value,
        Err(_) => return parse_events_full(path),
    };
    if !tail_str.is_empty() && !tail_str.ends_with('\n') {
        return parse_events_full(path);
    }
    let start_sequence = previous
        .parsed
        .last()
        .map(|event| event.sequence)
        .unwrap_or(0);
    let mut out = (*previous.parsed).clone();
    let mut appended = parse_events_lines(path, tail_str, start_sequence)?;
    out.append(&mut appended);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_paths::{plan_run_locator, resolve_run_root, write_run_locator};
    use std::sync::Arc;
    use transist_protocol::{KernelValue, RuntimeEvent};

    /// Ensures a locator exists for `run_id` so `resolve_run_root` succeeds.
    /// Mirrors what the kernel does when persisting a real run.
    fn seed_locator(home: &std::path::Path, run_id: &str) {
        let locator = plan_run_locator(home, run_id, None).unwrap();
        write_run_locator(home, &locator).unwrap();
    }

    fn event_line(ts: u64) -> String {
        serde_json::to_string(&RuntimeEvent {
            run_id: "run-1".to_string(),
            timestamp_unix_ms: ts,
            step_id: Some("step".to_string()),
            kind: "step.started".to_string(),
            payload: KernelValue::Null,
        })
        .unwrap()
    }

    fn write_events(path: &std::path::Path, timestamps: &[u64]) {
        let mut lines = Vec::new();
        for ts in timestamps {
            lines.push(event_line(*ts));
        }
        std::fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
    }

    #[test]
    fn events_tail_only_parses_new_lines() {
        let tmp = tempfile::tempdir().unwrap();
        seed_locator(tmp.path(), "run-1");
        let run_root = resolve_run_root(tmp.path(), "run-1").unwrap();
        std::fs::create_dir_all(&run_root).unwrap();
        let events_path = run_root.join(EVENTS_JSONL);
        write_events(&events_path, &[1, 2]);

        let cache = Arc::new(InspectionCache::new(10));
        let store = LocalInspectionStore::with_cache(tmp.path(), Arc::clone(&cache));
        let first = store.list_events("run-1").unwrap();
        assert_eq!(first.len(), 2);

        let mut raw = std::fs::read_to_string(&events_path).unwrap();
        raw.push_str(&format!("{}\n", event_line(3)));
        std::fs::write(&events_path, raw).unwrap();

        let second = store.list_events("run-1").unwrap();
        assert_eq!(second.len(), 3);
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn events_full_reparse_on_shrink_or_mtime_rewind() {
        let tmp = tempfile::tempdir().unwrap();
        seed_locator(tmp.path(), "run-1");
        let run_root = resolve_run_root(tmp.path(), "run-1").unwrap();
        std::fs::create_dir_all(&run_root).unwrap();
        let events_path = run_root.join(EVENTS_JSONL);
        write_events(&events_path, &[1, 2, 3]);

        let cache = Arc::new(InspectionCache::new(10));
        let store = LocalInspectionStore::with_cache(tmp.path(), Arc::clone(&cache));
        let _ = store.list_events("run-1").unwrap();

        write_events(&events_path, &[1]);
        let second = store.list_events("run-1").unwrap();
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn events_full_reparse_on_same_length_but_changed_mtime() {
        // A same-length rewrite (e.g. a truncate-then-rewrite tail repair, or
        // an equal-size event replacing another) must not be served from a
        // stale cached parse just because the byte length is unchanged.
        let tmp = tempfile::tempdir().unwrap();
        seed_locator(tmp.path(), "run-1");
        let run_root = resolve_run_root(tmp.path(), "run-1").unwrap();
        std::fs::create_dir_all(&run_root).unwrap();
        let events_path = run_root.join(EVENTS_JSONL);
        write_events(&events_path, &[1, 2, 3]);

        let cache = Arc::new(InspectionCache::new(10));
        let store = LocalInspectionStore::with_cache(tmp.path(), Arc::clone(&cache));
        let first = store.list_events("run-1").unwrap();
        assert_eq!(
            first
                .iter()
                .map(|e| e.event.timestamp_unix_ms)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        // Same total byte length (single-digit timestamps), different
        // content, and a forced later mtime.
        write_events(&events_path, &[4, 5, 6]);
        let same_len = std::fs::metadata(&events_path).unwrap().len();
        assert_eq!(
            same_len,
            std::fs::metadata(&events_path).unwrap().len(),
            "fixture must keep the byte length constant across the rewrite"
        );
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&events_path)
            .unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .expect("bump mtime forward");

        let second = store.list_events("run-1").unwrap();
        assert_eq!(
            second
                .iter()
                .map(|e| e.event.timestamp_unix_ms)
                .collect::<Vec<_>>(),
            vec![4, 5, 6],
            "a same-length, different-content rewrite must not be served from a stale cache"
        );
    }
}
