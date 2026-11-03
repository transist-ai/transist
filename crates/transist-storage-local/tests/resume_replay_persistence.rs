// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::fs;
use transist_core::ports::{
    ArtifactPort, ArtifactReadRequest, ArtifactWriteRequest, RunStorePort, RunStoreStepExecution,
    RunStoreStepState,
};
use transist_core::{
    KernelRunSnapshot, KernelRunStatus, KernelStepState, KernelStepStatus, PersistedRunRecord,
    RecipeSnapshotRef, RunLineage, RunLineageMode,
};
use transist_protocol::KernelValue;
use transist_storage_local::{LocalArtifactStore, LocalRunStore, LocalStorageConfig};

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
        10,
        10,
    )
}

#[test]
fn state_json_preserves_metadata_across_run_creation_and_step_updates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("meta-run");
    record.status = KernelRunStatus::Running;
    store.create_run(&record).expect("create");

    store
        .store_step_state(&RunStoreStepState {
            run_id: "meta-run".to_string(),
            step_id: "step".to_string(),
            state: serde_json::to_value(KernelStepState {
                step_id: "step".to_string(),
                status: KernelStepStatus::Succeeded,
                executions: 1,
                retry_wait_until_ms: None,
                skip_reason: None,
                last_request: None,
                last_result: None,
                started_at_ms: Some(11),
                finished_at_ms: Some(12),
                activation_executions_baseline: 0,
            })
            .expect("step json")
            .into(),
        })
        .expect("store step");

    record.status = KernelRunStatus::Success;
    record.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(11),
            finished_at_ms: Some(12),
            activation_executions_baseline: 0,
        },
    );
    store
        .update_run_state("meta-run", &record)
        .expect("persist final");

    let state = fs::read_to_string(tmp.path().join("runs/run_meta-run/state.json")).expect("state");
    let json: serde_json::Value = serde_json::from_str(&state).expect("json");
    assert_eq!(json["workflow_version"], "v1");
    assert_eq!(json["recipe_snapshot"]["path"], "recipe.yaml");
    assert_eq!(json["lineage"]["mode"], "fresh");
    assert_eq!(json["resume_count"], 0);
    assert_eq!(json["workflow_inputs"], serde_json::json!({}));
    assert!(json["created_at_ms"].as_u64().is_some());
    assert!(json["updated_at_ms"].as_u64().is_some());
}

#[test]
fn recipe_snapshot_is_written_for_new_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("recipe-run"))
        .expect("create");
    store
        .write_recipe_snapshot("recipe-run", "schema: transist.recipe.v1\n")
        .expect("write recipe");
    let recipe_path = tmp.path().join("runs/run_recipe-run/recipe.yaml");
    assert_eq!(
        fs::read_to_string(recipe_path).expect("recipe"),
        "schema: transist.recipe.v1\n"
    );
}

#[test]
fn step_execution_records_are_append_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("execution-run"))
        .expect("create");
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "execution-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            state: serde_json::json!({
                "run_id": "execution-run",
                "step_id": "step",
                "execution": 1,
                "status": "failed",
                "started_at_ms": 1,
                "finished_at_ms": 2,
                "request": null,
                "result": null
            })
            .into(),
        })
        .expect("execution 1");
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "execution-run".to_string(),
            step_id: "step".to_string(),
            execution: 2,
            state: serde_json::json!({
                "run_id": "execution-run",
                "step_id": "step",
                "execution": 2,
                "status": "success",
                "started_at_ms": 3,
                "finished_at_ms": 4,
                "request": null,
                "result": null
            })
            .into(),
        })
        .expect("execution 2");

    assert!(
        tmp.path()
            .join("runs/run_execution-run/step_executions/step/execution_001.json")
            .exists()
    );
    assert!(
        tmp.path()
            .join("runs/run_execution-run/step_executions/step/execution_002.json")
            .exists()
    );
}

#[test]
fn execution_scoped_artifact_and_evidence_paths_do_not_overwrite_prior_executions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("artifact-run"))
        .expect("create");
    let artifacts = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));
    artifacts
        .write_artifact(&ArtifactWriteRequest {
            run_id: "artifact-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            logical_path: "artifacts/000_stdout.txt.json".to_string(),
            payload: KernelValue::from(serde_json::json!({"execution": 1})),
        })
        .expect("artifact 1");
    artifacts
        .write_artifact(&ArtifactWriteRequest {
            run_id: "artifact-run".to_string(),
            step_id: "step".to_string(),
            execution: 2,
            logical_path: "artifacts/000_stdout.txt.json".to_string(),
            payload: KernelValue::from(serde_json::json!({"execution": 2})),
        })
        .expect("artifact 2");
    artifacts
        .write_artifact(&ArtifactWriteRequest {
            run_id: "artifact-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            logical_path: "evidence/000_trace.json".to_string(),
            payload: KernelValue::from(serde_json::json!({"execution": 1})),
        })
        .expect("evidence 1");

    assert!(
        tmp.path()
            .join(
                "runs/run_artifact-run/artifacts/step/execution_001/artifacts/000_stdout.txt.json"
            )
            .exists()
    );
    assert!(
        tmp.path()
            .join(
                "runs/run_artifact-run/artifacts/step/execution_002/artifacts/000_stdout.txt.json"
            )
            .exists()
    );
    assert!(
        tmp.path()
            .join("runs/run_artifact-run/artifacts/step/execution_001/evidence/000_trace.json")
            .exists()
    );
}

#[test]
fn rerun_preserves_prior_execution_records_artifacts_and_evidence() {
    // Rerunning a run under the same run id must never
    // delete prior step state, execution records, artifacts, or evidence. The
    // execution counter is append-only, so a rerun starts at N+1 and every
    // prior execution remains inspectable. This test simulates the runtime
    // rerun path: a step with a prior execution is reset to a fresh dispatch
    // state (status=Pending, no runtime fields) while preserving its execution
    // counter, the run state is persisted, and then the next execution is
    // stored. The prior execution record, artifact, and evidence must still be
    // present, and the next execution must append rather than overwrite.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let artifacts = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("rerun-run"))
        .expect("create");

    // First execution: store the execution record, an artifact, and evidence.
    // Persist the step state with executions=1 to mirror the on-disk state
    // after a completed first execution.
    let mut seeded = store
        .load_run("rerun-run")
        .expect("load record")
        .expect("record present");
    let seeded_step = seeded.steps.get_mut("step").expect("step");
    seeded_step.executions = 1;
    seeded_step.status = KernelStepStatus::Succeeded;
    seeded_step.started_at_ms = Some(1);
    seeded_step.finished_at_ms = Some(2);
    store
        .update_run_state("rerun-run", &seeded)
        .expect("persist executions 1");
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "rerun-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            state: serde_json::json!({
                "run_id": "rerun-run",
                "step_id": "step",
                "execution": 1,
                "status": "success",
                "started_at_ms": 1,
                "finished_at_ms": 2,
                "request": null,
                "result": null
            })
            .into(),
        })
        .expect("execution 1");
    artifacts
        .write_artifact(&ArtifactWriteRequest {
            run_id: "rerun-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            logical_path: "artifacts/000_stdout.txt.json".to_string(),
            payload: KernelValue::from(serde_json::json!({"execution": 1})),
        })
        .expect("artifact 1");
    artifacts
        .write_artifact(&ArtifactWriteRequest {
            run_id: "rerun-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            logical_path: "evidence/000_trace.json".to_string(),
            payload: KernelValue::from(serde_json::json!({"execution": 1})),
        })
        .expect("evidence 1");

    // Simulate the kernel's rerun reset: clear transient runtime fields while
    // preserving the execution counter (N=1). The runtime never resets an
    // existing execution counter to zero.
    let mut record = store
        .load_run("rerun-run")
        .expect("load record")
        .expect("record present");
    let step = record
        .steps
        .get_mut("step")
        .expect("step exists in sample record");
    step.status = KernelStepStatus::Pending;
    step.retry_wait_until_ms = None;
    step.skip_reason = None;
    step.last_request = None;
    step.last_result = None;
    step.started_at_ms = None;
    step.finished_at_ms = None;
    // executions stays at 1; the next dispatch will produce execution 2.
    assert_eq!(step.executions, 1);

    // Persisting the reset state must not delete prior execution records,
    // artifacts, or evidence.
    store
        .update_run_state("rerun-run", &record)
        .expect("persist reset state");

    let execution_001 = tmp
        .path()
        .join("runs/run_rerun-run/step_executions/step/execution_001.json");
    assert!(
        execution_001.exists(),
        "prior execution record must survive rerun state persistence"
    );
    let artifact_001 = tmp
        .path()
        .join("runs/run_rerun-run/artifacts/step/execution_001/artifacts/000_stdout.txt.json");
    assert!(
        artifact_001.exists(),
        "prior artifact must survive rerun state persistence"
    );
    let evidence_001 = tmp
        .path()
        .join("runs/run_rerun-run/artifacts/step/execution_001/evidence/000_trace.json");
    assert!(
        evidence_001.exists(),
        "prior evidence must survive rerun state persistence"
    );

    // The next execution appends at N+1; the invariant guard rejects
    // overwriting execution 1.
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "rerun-run".to_string(),
            step_id: "step".to_string(),
            execution: 2,
            state: serde_json::json!({
                "run_id": "rerun-run",
                "step_id": "step",
                "execution": 2,
                "status": "success",
                "started_at_ms": 3,
                "finished_at_ms": 4,
                "request": null,
                "result": null
            })
            .into(),
        })
        .expect("execution 2");
    let duplicate = store.store_step_execution(&RunStoreStepExecution {
        run_id: "rerun-run".to_string(),
        step_id: "step".to_string(),
        execution: 1,
        state: serde_json::json!({
            "run_id": "rerun-run",
            "step_id": "step",
            "execution": 1,
            "status": "success",
            "started_at_ms": 5,
            "finished_at_ms": 6,
            "request": null,
            "result": null
        })
        .into(),
    });
    assert!(
        duplicate.is_err(),
        "invariant guard must reject rewriting an existing execution record"
    );

    // Both executions remain inspectable in ascending order, with their
    // distinct artifact and evidence bytes intact.
    let loaded_first = artifacts
        .read_artifact(&ArtifactReadRequest {
            run_id: "rerun-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            logical_path: "artifacts/000_stdout.txt.json".to_string(),
        })
        .expect("read artifact 1");
    assert_eq!(
        serde_json::Value::from(loaded_first),
        serde_json::json!({"execution": 1}),
        "execution 1 artifact bytes must be unchanged after rerun"
    );
    assert!(
        tmp.path()
            .join("runs/run_rerun-run/step_executions/step/execution_002.json")
            .exists(),
        "execution 2 record must be appended"
    );
    assert!(
        tmp.path()
            .join("runs/run_rerun-run/step_executions/step/execution_001.json")
            .exists(),
        "execution 1 record must remain after execution 2 is stored"
    );
}

#[test]
fn rerun_under_same_run_id_starts_at_next_execution_number() {
    // A reused child subflow rerun under the same run id must start at N+1.
    // The execution counter is never reset to zero, so the persisted state
    // after a rerun reset preserves the prior count and the next dispatch
    // produces the next execution number.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("counter-run"))
        .expect("create");

    let mut record = store
        .load_run("counter-run")
        .expect("load record")
        .expect("record present");
    let step = record.steps.get_mut("step").expect("step");
    step.executions = 4;
    step.status = KernelStepStatus::Succeeded;
    step.started_at_ms = Some(10);
    step.finished_at_ms = Some(20);
    store
        .update_run_state("counter-run", &record)
        .expect("persist with executions 4");

    // Simulate the kernel's rerun reset: transient fields cleared, executions
    // preserved.
    let mut reset = store
        .load_run("counter-run")
        .expect("reload")
        .expect("record present");
    let step = reset.steps.get_mut("step").expect("step");
    step.status = KernelStepStatus::Pending;
    step.last_request = None;
    step.last_result = None;
    step.started_at_ms = None;
    step.finished_at_ms = None;
    assert_eq!(step.executions, 4, "execution counter must be preserved");
    store
        .update_run_state("counter-run", &reset)
        .expect("persist reset");

    let after = store
        .load_run("counter-run")
        .expect("reload after reset")
        .expect("record present");
    assert_eq!(
        after.steps["step"].executions, 4,
        "persisted execution counter must remain at its prior value after a rerun reset"
    );
}

#[test]
fn artifact_write_rejects_overwriting_existing_execution_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("clobber-run"))
        .expect("create");
    let artifacts = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));
    artifacts
        .write_artifact(&ArtifactWriteRequest {
            run_id: "clobber-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            logical_path: "artifacts/result.json".to_string(),
            payload: KernelValue::from(serde_json::json!({"v": 1})),
        })
        .expect("write 1");
    let duplicate = artifacts.write_artifact(&ArtifactWriteRequest {
        run_id: "clobber-run".to_string(),
        step_id: "step".to_string(),
        execution: 1,
        logical_path: "artifacts/result.json".to_string(),
        payload: KernelValue::from(serde_json::json!({"v": 2})),
    });
    assert!(
        duplicate.is_err(),
        "no-clobber guard must reject overwriting an existing execution-scoped artifact"
    );
    let loaded = artifacts
        .read_artifact(&ArtifactReadRequest {
            run_id: "clobber-run".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            logical_path: "artifacts/result.json".to_string(),
        })
        .expect("read");
    assert_eq!(
        serde_json::Value::from(loaded),
        serde_json::json!({"v": 1}),
        "prior artifact bytes must remain unchanged after rejected overwrite"
    );
}

#[test]
fn update_run_state_rejects_decreasing_execution_counter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("monotonic-run");
    record.status = KernelRunStatus::Running;
    let step = record.steps.get_mut("step").expect("step");
    step.executions = 3;
    step.activation_executions_baseline = 0;
    step.status = KernelStepStatus::Succeeded;
    step.started_at_ms = Some(1);
    step.finished_at_ms = Some(2);
    store.create_run(&record).expect("create");

    // Simulate a stale state write that regresses the counter.
    let mut stale = store
        .load_run("monotonic-run")
        .expect("load")
        .expect("record present");
    stale.steps.get_mut("step").expect("step").executions = 1;
    let result = store.update_run_state("monotonic-run", &stale);
    assert!(
        result.is_err(),
        "monotonic guard must reject decreasing execution counter"
    );
    let current = store
        .load_run("monotonic-run")
        .expect("reload")
        .expect("record present");
    assert_eq!(
        current.steps["step"].executions, 3,
        "prior counter must remain unchanged after rejected decrease"
    );
}

#[test]
fn store_step_state_rejects_decreasing_execution_counter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("monotonic-step-run");
    record.status = KernelRunStatus::Running;
    let step = record.steps.get_mut("step").expect("step");
    step.executions = 3;
    step.activation_executions_baseline = 0;
    step.status = KernelStepStatus::Succeeded;
    step.started_at_ms = Some(1);
    step.finished_at_ms = Some(2);
    store.create_run(&record).expect("create");

    // A stale per-step write must not lower the step's lifetime execution
    // counter, mirroring the guard already enforced by `update_run_state`.
    let mut stale_step = record.steps["step"].clone();
    stale_step.executions = 1;
    let stale_state = RunStoreStepState {
        run_id: "monotonic-step-run".to_string(),
        step_id: "step".to_string(),
        state: serde_json::to_value(&stale_step)
            .expect("serialize stale step")
            .into(),
    };
    let result = store.store_step_state(&stale_state);
    assert!(
        result.is_err(),
        "monotonic guard must reject a decreasing execution counter via store_step_state"
    );
    let current = store
        .load_run("monotonic-step-run")
        .expect("reload")
        .expect("record present");
    assert_eq!(
        current.steps["step"].executions, 3,
        "prior counter must remain unchanged after rejected decrease"
    );
}

#[test]
fn list_step_executions_returns_records_in_ascending_numeric_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("order-run"))
        .expect("create");

    // Write executions 1, 10, 2, 999, 1000 in insertion order to prove
    // the listing sorts by parsed execution number, not filename.
    let executions_to_write = [1u32, 10, 2, 999, 1000];
    for &n in &executions_to_write {
        store
            .store_step_execution(&RunStoreStepExecution {
                run_id: "order-run".to_string(),
                step_id: "step".to_string(),
                execution: n,
                state: serde_json::json!({
                    "run_id": "order-run",
                    "step_id": "step",
                    "execution": n,
                    "status": "success",
                    "started_at_ms": n as u64,
                    "finished_at_ms": n as u64 + 1,
                    "request": null,
                    "result": null
                })
                .into(),
            })
            .unwrap_or_else(|_| panic!("execution {n}"));
    }

    let inspection_store =
        transist_storage_local::inspection::LocalInspectionStore::new(tmp.path().to_path_buf());
    let records = inspection_store
        .list_step_executions("order-run", "step")
        .expect("list");

    assert_eq!(records.len(), 5);
    let nums: Vec<u32> = records.iter().map(|r| r.execution).collect();
    assert_eq!(
        nums,
        vec![1, 2, 10, 999, 1000],
        "executions must be sorted by parsed execution number, not filename"
    );
}

/// Seeds a run whose step carries a lifetime `executions` count of `5` with an
/// `activation_executions_baseline` of `3`, mimicking a live rerun activation.
fn seed_activated_run(store: &LocalRunStore, run_id: &str) {
    let mut record = sample_record(run_id);
    record.status = KernelRunStatus::Running;
    let step = record.steps.get_mut("step").expect("step");
    step.executions = 5;
    step.activation_executions_baseline = 3;
    step.status = KernelStepStatus::Succeeded;
    step.started_at_ms = Some(1);
    step.finished_at_ms = Some(2);
    store.create_run(&record).expect("create");
}

#[test]
fn update_run_state_rejects_activation_baseline_exceeding_executions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    seed_activated_run(&store, "baseline-over-run");

    let mut bad = store
        .load_run("baseline-over-run")
        .expect("load")
        .expect("record present");
    let bad_step = bad.steps.get_mut("step").expect("step");
    bad_step.executions = 5;
    bad_step.activation_executions_baseline = 6;
    let result = store.update_run_state("baseline-over-run", &bad);
    assert!(
        result.is_err(),
        "activation baseline above lifetime executions must be rejected"
    );
    let current = store
        .load_run("baseline-over-run")
        .expect("reload")
        .expect("record present");
    assert_eq!(
        current.steps["step"].activation_executions_baseline, 3,
        "prior baseline must remain unchanged after rejected write"
    );
}

#[test]
fn update_run_state_rejects_activation_baseline_decrease() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    seed_activated_run(&store, "baseline-decrease-run");

    let mut stale = store
        .load_run("baseline-decrease-run")
        .expect("load")
        .expect("record present");
    stale
        .steps
        .get_mut("step")
        .expect("step")
        .activation_executions_baseline = 2;
    let result = store.update_run_state("baseline-decrease-run", &stale);
    assert!(
        result.is_err(),
        "activation baseline decrease below the persisted value must be rejected"
    );
    let current = store
        .load_run("baseline-decrease-run")
        .expect("reload")
        .expect("record present");
    assert_eq!(
        current.steps["step"].activation_executions_baseline, 3,
        "prior baseline must remain unchanged after rejected decrease"
    );
}

#[test]
fn store_step_state_rejects_activation_baseline_invariants() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    seed_activated_run(&store, "baseline-step-run");

    // Baseline above executions fails via store_step_state too.
    let mut over = succeeded_step();
    over.executions = 5;
    over.activation_executions_baseline = 9;
    let over_state = RunStoreStepState {
        run_id: "baseline-step-run".to_string(),
        step_id: "step".to_string(),
        state: serde_json::to_value(&over).expect("serialize").into(),
    };
    assert!(
        store.store_step_state(&over_state).is_err(),
        "step baseline above lifetime executions must be rejected"
    );

    // Baseline decrease below the persisted value fails via store_step_state too.
    let mut decreased = succeeded_step();
    decreased.executions = 5;
    decreased.activation_executions_baseline = 1;
    let decreased_state = RunStoreStepState {
        run_id: "baseline-step-run".to_string(),
        step_id: "step".to_string(),
        state: serde_json::to_value(&decreased).expect("serialize").into(),
    };
    assert!(
        store.store_step_state(&decreased_state).is_err(),
        "step baseline decrease must be rejected"
    );

    let current = store
        .load_run("baseline-step-run")
        .expect("reload")
        .expect("record present");
    assert_eq!(
        current.steps["step"].activation_executions_baseline, 3,
        "prior baseline must remain unchanged after rejected step writes"
    );
}

fn succeeded_step() -> KernelStepState {
    let mut step = KernelStepState::pending("step");
    step.status = KernelStepStatus::Succeeded;
    step.started_at_ms = Some(1);
    step.finished_at_ms = Some(2);
    step
}
