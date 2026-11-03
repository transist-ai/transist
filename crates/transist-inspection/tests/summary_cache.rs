// SPDX-License-Identifier: MPL-2.0
//! Reader-visible summary coherence with production inspection composition.

use std::collections::BTreeMap;
use std::fs::{self, File, FileTimes};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use transist_core::{KernelRunSnapshot, KernelRunStatus, PersistedRunRecord, RunStorePort};
use transist_inspection::{InspectionQueryPort, LocalInspectionQuery, RunListScope};
use transist_storage_local::inspection::{FileFingerprint, InspectionCache, LocalInspectionStore};
use transist_storage_local::{LocalRunStore, LocalRunSummaryStore, LocalStorageConfig};

#[test]
fn summary_rebuild_uses_fresh_inputs_after_same_tick_state_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(&home));
    let mut record = PersistedRunRecord::from_snapshot(
        KernelRunSnapshot::new("refresh-collision", "wf", "0.1.0", &["step".into()]),
        None,
        None,
        BTreeMap::new(),
        0,
        1,
        1_735_689_604_000,
    );
    store.create_run(&record).unwrap();
    let run_root = home.join("runs/run_refresh-collision");
    let state_path = run_root.join("state.json");
    let tick = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    File::options()
        .write(true)
        .open(&state_path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(tick))
        .unwrap();
    let old_state_fingerprint = FileFingerprint::from_path(&state_path).unwrap();
    let old_state_bytes = fs::read(&state_path).unwrap();
    for family in ["artifacts", "evidence"] {
        let directory = run_root.join("artifacts/step/execution_001").join(family);
        fs::create_dir_all(&directory).unwrap();
        for index in 0..8 {
            fs::write(directory.join(format!("{index:03}.txt")), b"fixture").unwrap();
        }
    }

    let writer_cache = Arc::new(InspectionCache::new(10));
    let reader_cache = Arc::new(InspectionCache::new(10));
    assert!(!Arc::ptr_eq(&writer_cache, &reader_cache));
    let writer = LocalRunSummaryStore::with_cache(&home, Arc::clone(&writer_cache));
    let query = LocalInspectionQuery::with_cache(&workspace, &home, Arc::clone(&reader_cache));
    let reader = LocalRunSummaryStore::with_cache(&home, Arc::clone(&reader_cache));
    let initial = writer.refresh_run(&record.run_id).unwrap();
    assert_eq!(initial.status, "created");
    assert_eq!((initial.artifact_count, initial.evidence_count), (8, 8));
    let rows = query.list_all_runs(RunListScope::All, None).unwrap();
    assert_eq!(rows[0].status, "created");
    assert_eq!((rows[0].artifact_count, rows[0].evidence_count), (8, 8));

    // Warm state outside refresh, after the previous summary publication. Merely
    // invalidating the writer slot after write_run_summary cannot fix this case.
    let state_reader = LocalInspectionStore::with_cache(&home, writer_cache);
    assert_eq!(
        state_reader.load_run_record(&record.run_id).unwrap().status,
        KernelRunStatus::Created
    );
    record.status = KernelRunStatus::Running;
    store.update_run_state(&record.run_id, &record).unwrap();
    File::options()
        .write(true)
        .open(&state_path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(tick))
        .unwrap();
    assert_eq!(
        FileFingerprint::from_path(&state_path).unwrap(),
        old_state_fingerprint
    );
    assert_eq!(fs::read(&state_path).unwrap().len(), old_state_bytes.len());
    assert_ne!(fs::read(&state_path).unwrap(), old_state_bytes);
    for family in ["artifacts", "evidence"] {
        fs::write(
            run_root
                .join("artifacts/step/execution_001")
                .join(family)
                .join("008.txt"),
            b"fixture",
        )
        .unwrap();
    }

    let refreshed = writer.refresh_run(&record.run_id).unwrap();
    assert_eq!(
        refreshed.status, "running",
        "a rebuild must read authoritative state, not the warmed input cache"
    );
    assert_eq!((refreshed.artifact_count, refreshed.evidence_count), (9, 9));
    let rows = query.list_all_runs(RunListScope::All, None).unwrap();
    assert_eq!(rows[0].status, "running");
    assert_eq!((rows[0].artifact_count, rows[0].evidence_count), (9, 9));
    reader.load_run_summary(&record.run_id).unwrap();
    let parsed = reader_cache.stats();
    query.list_all_runs(RunListScope::All, None).unwrap();
    reader.load_run_summary(&record.run_id).unwrap();
    assert_eq!(reader_cache.stats().misses, parsed.misses);
    assert_eq!(reader_cache.stats().hits, parsed.hits + 2);
}

#[test]
fn independent_summary_reader_detects_same_tick_atomic_replacement_and_reuses_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(&home));
    let mut record = PersistedRunRecord::from_snapshot(
        KernelRunSnapshot::new("summary-collision", "wf", "0.1.0", &[]),
        None,
        None,
        BTreeMap::new(),
        0,
        1,
        1,
    );
    store.create_run(&record).unwrap();

    // The runtime writer and inspection reader own independent caches, exactly
    // as default runtime composition and LocalInspectionQuery do in production.
    let writer_cache = Arc::new(InspectionCache::new(10));
    let reader_cache = Arc::new(InspectionCache::new(10));
    assert!(!Arc::ptr_eq(&writer_cache, &reader_cache));
    let writer = LocalRunSummaryStore::with_cache(&home, writer_cache);
    let reader = LocalRunSummaryStore::with_cache(&home, Arc::clone(&reader_cache));
    let query = LocalInspectionQuery::with_cache(&workspace, &home, Arc::clone(&reader_cache));
    let mut summary = writer.refresh_run(&record.run_id).unwrap();
    summary.artifact_count = 8;
    summary.evidence_count = 8;
    summary.event_count = 8;
    summary.has_artifacts = true;
    summary.has_events = true;
    writer.write_run_summary(&summary).unwrap();

    let path = home.join("runs/run_summary-collision/summary.json");
    let tick = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(tick))
        .unwrap();
    let old_metadata = FileFingerprint::from_path(&path).unwrap();
    let old_bytes = fs::read(&path).unwrap();

    let rows = query.list_all_runs(RunListScope::All, None).unwrap();
    assert_eq!(rows[0].artifact_count, 8);
    assert_eq!(rows[0].evidence_count, 8);
    assert_eq!(
        reader
            .load_run_summary(&record.run_id)
            .unwrap()
            .unwrap()
            .event_count,
        8
    );
    let warm = reader_cache.stats();
    query.list_all_runs(RunListScope::All, None).unwrap();
    reader.load_run_summary(&record.run_id).unwrap();
    assert_eq!(reader_cache.stats().misses, warm.misses);
    assert_eq!(reader_cache.stats().hits, warm.hits + 2);

    record.status = KernelRunStatus::Running;
    store.update_run_state(&record.run_id, &record).unwrap();
    summary.status = "running".to_string(); // Same length as "created".
    summary.artifact_count = 9;
    summary.evidence_count = 9;
    summary.event_count = 9;
    writer.write_run_summary(&summary).unwrap();
    // Deterministically simulate a coarse filesystem tick, without a sleep or
    // a metadata/mock shortcut. The new file is the writer's real atomic rename.
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(tick))
        .unwrap();
    assert_eq!(FileFingerprint::from_path(&path).unwrap(), old_metadata);
    assert_eq!(fs::read(&path).unwrap().len(), old_bytes.len());
    assert_ne!(fs::read(&path).unwrap(), old_bytes);

    let rows = query.list_all_runs(RunListScope::All, None).unwrap();
    assert_eq!(rows[0].status, "running");
    assert_eq!(rows[0].artifact_count, 9);
    assert_eq!(rows[0].evidence_count, 9);
    assert_eq!(
        reader
            .load_run_summary(&record.run_id)
            .unwrap()
            .unwrap()
            .event_count,
        9
    );
    let refreshed = reader_cache.stats();
    assert_eq!(refreshed.misses, warm.misses + 1);
    query.list_all_runs(RunListScope::All, None).unwrap();
    reader.load_run_summary(&record.run_id).unwrap();
    assert_eq!(reader_cache.stats().misses, refreshed.misses);
    assert_eq!(reader_cache.stats().hits, refreshed.hits + 2);
}
