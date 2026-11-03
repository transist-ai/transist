// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::fs;
use transist_core::ports::RunStorePort;
use transist_core::{
    KernelRunSnapshot, PersistedRunRecord, RecipeSnapshotRef, RunLineage, RunLineageMode,
};
use transist_storage_local::{
    LocalRunQueryService, LocalRunStore, LocalRunSummaryStore, LocalStorageConfig,
    plan_run_locator, write_run_locator,
};

fn sample_record(run_id: &str) -> PersistedRunRecord {
    PersistedRunRecord::from_snapshot(
        KernelRunSnapshot::new(run_id, "wf", "v1", &["step".to_string()]),
        Some(RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: "abc".to_string(),
        }),
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: run_id.to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        1,
        1,
    )
}

#[test]
fn local_run_store_and_query_service_integrate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = LocalStorageConfig::new(tmp.path());
    let store = LocalRunStore::new(config);
    let record = sample_record("integration");
    store.create_run(&record).expect("create");

    let query = LocalRunQueryService::new(tmp.path());
    let runs = query.list_runs().expect("list");
    assert_eq!(runs, vec!["integration".to_string()]);
    let run = query.show_run("integration").expect("show");
    let run_json = serde_json::Value::from(run);
    assert_eq!(run_json["run_id"], "integration");
    assert_eq!(run_json["recipe_snapshot"]["path"], "recipe.yaml");
}

#[test]
fn legacy_evidence_only_summary_reconstructs_count_without_rewriting_fixture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_legacy-evidence-only");
    let summary_path = run_root.join("summary.json");
    let evidence_path = run_root.join("artifacts/collect/execution_001/evidence/000_trace.log");
    let locator = plan_run_locator(tmp.path(), "legacy-evidence-only", None).expect("locator");
    write_run_locator(tmp.path(), &locator).expect("write locator");
    fs::create_dir_all(evidence_path.parent().expect("evidence parent")).expect("directories");

    let fixture = include_str!("fixtures/legacy_evidence_only_summary.json");
    fs::write(&summary_path, fixture).expect("legacy summary");
    fs::write(evidence_path, b"trace").expect("evidence");

    let summary = LocalRunSummaryStore::new(tmp.path())
        .load_run_summary("legacy-evidence-only")
        .expect("load summary")
        .expect("summary exists");

    assert_eq!(summary.evidence_count, 1);
    assert_eq!(
        fs::read_to_string(summary_path).expect("read summary"),
        fixture
    );
}
