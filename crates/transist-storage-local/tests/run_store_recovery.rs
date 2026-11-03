// SPDX-License-Identifier: MPL-2.0
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use transist_core::ports::{
    EventPort, RunBirthArtifacts, RunStorePort, RunStoreStepExecution, RunStoreStepState,
};
use transist_core::recover_snapshot;
use transist_core::{FailureClass, FailureRecord};
use transist_core::{
    KernelRunSnapshot, KernelRunStatus, KernelStepState, KernelStepStatus, PersistedRunRecord,
    RecipeSnapshotRef, RunLineage, RunLineageMode, WorkflowAssetBlob,
};
use transist_protocol::RuntimeEvent;
use transist_storage_local::fault_injection;
use transist_storage_local::run_paths::{plan_run_locator, resolve_run_root, write_run_locator};
use transist_storage_local::{
    LocalEventSink, LocalInspectionStore, LocalRunQueryService, LocalRunStore, LocalStorageConfig,
};

fn sample_record(run_id: &str, steps: &[String]) -> PersistedRunRecord {
    PersistedRunRecord::from_snapshot(
        KernelRunSnapshot::new(run_id, "wf", "v1", steps),
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
fn pending_gate_query_is_false_when_unreadable_and_true_for_json_decisions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    assert!(!store.has_pending_gate_decision(tmp.path(), "run-1"));

    let gates_dir = tmp.path().join("runs/run_run-1/gates");
    std::fs::create_dir_all(&gates_dir).expect("create gates directory");
    std::fs::write(gates_dir.join("note.txt"), b"not a decision").expect("write note");
    assert!(!store.has_pending_gate_decision(tmp.path(), "run-1"));

    std::fs::write(gates_dir.join("review.json"), b"{}").expect("write decision");
    assert!(store.has_pending_gate_decision(tmp.path(), "run-1"));
}

#[test]
fn run_store_rejects_operations_for_a_different_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let other = tempfile::tempdir().expect("other tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    assert!(store.resolve_run_root(other.path(), "run").is_err());
    assert!(store.list_run_roots(other.path()).is_err());
}

#[test]
fn run_store_persists_snapshot_and_step_state_for_recovery() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record(
            "recoverable",
            &["step_a".to_string(), "step_b".to_string()],
        ))
        .expect("create run");

    let mut snapshot = KernelRunSnapshot::new(
        "recoverable",
        "wf",
        "v1",
        &["step_a".to_string(), "step_b".to_string()],
    );
    snapshot.status = KernelRunStatus::Running;
    snapshot.steps.insert(
        "step_a".to_string(),
        KernelStepState {
            step_id: "step_a".to_string(),
            status: KernelStepStatus::Dispatching,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(10),
            finished_at_ms: None,
            activation_executions_baseline: 0,
        },
    );
    snapshot.steps.insert(
        "step_b".to_string(),
        KernelStepState {
            step_id: "step_b".to_string(),
            status: KernelStepStatus::RetryWait,
            executions: 2,
            retry_wait_until_ms: Some(5_000),
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(20),
            finished_at_ms: Some(30),
            activation_executions_baseline: 0,
        },
    );

    let mut record = sample_record("recoverable", &["step_a".to_string(), "step_b".to_string()]);
    record.status = snapshot.status;
    record.steps = snapshot.steps.clone();
    store
        .update_run_state("recoverable", &record)
        .expect("persist snapshot");

    let state = serde_json::to_value(KernelStepState {
        step_id: "step_b".to_string(),
        status: KernelStepStatus::RetryWait,
        executions: 3,
        retry_wait_until_ms: Some(9_000),
        skip_reason: None,
        last_request: Some(
            serde_json::from_value(serde_json::json!({
                "type": "tep.invoke.request.v1",
                "protocol_version": "tep.v1",
                "invoke_id": "invoke_1",
                "run_id": "recoverable",
                "workflow_id": "wf",
                "step": { "id": "step_b", "execution": 3 },
                "adapter_kind": "process",
                "action": "exec",
                "input": {},
                "paths": {
                    "run_root": "/tmp",
                    "artifacts_dir": "/tmp",
                    "evidence_dir": "/tmp",
                    "workspace_dir": "/tmp"
                },
                "timeout_ms": null,
                "policy": {},
                "metadata": {}
            }))
            .expect("request"),
        ),
        last_result: Some(
            serde_json::from_value(serde_json::json!({
                "type": "tep.invoke.result.v1",
                "protocol_version": "tep.v1",
                "invoke_id": "invoke_1",
                "run_id": "recoverable",
                "step_id": "step_b",
                "execution": 3,
                "status": "failed",
                "stdout": "",
                "stderr": "",
                "timing": {
                    "started_at": "0",
                    "finished_at": "0",
                    "duration_ms": 0
                },
                "artifacts": [],
                "evidence": [],
                "metrics": {},
                "output": {},
                "usage": null,
                "error": null
            }))
            .expect("result"),
        ),
        started_at_ms: Some(20),
        finished_at_ms: Some(30),
        activation_executions_baseline: 0,
    })
    .expect("state")
    .into();
    store
        .store_step_state(&RunStoreStepState {
            run_id: "recoverable".to_string(),
            step_id: "step_b".to_string(),
            state,
        })
        .expect("persist step state");

    let loaded = store
        .load_run("recoverable")
        .expect("load run")
        .expect("record present");
    let loaded_json = serde_json::to_value(&loaded).expect("loaded json");
    assert_eq!(loaded_json["steps"]["step_b"]["status"], "retry_wait");
    assert_eq!(loaded_json["executions"]["step_b"], 3);
    assert_eq!(
        loaded_json["steps"]["step_b"]["last_request"]["invoke_id"],
        "invoke_1"
    );
    assert_eq!(
        loaded_json["steps"]["step_b"]["last_result"]["status"],
        "failed"
    );
}

#[test]
fn persisted_run_state_roundtrips_into_core_recovery() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("recover_roundtrip", &["step".to_string()]))
        .expect("create run");

    let mut snapshot =
        KernelRunSnapshot::new("recover_roundtrip", "wf", "v1", &["step".to_string()]);
    snapshot.status = KernelRunStatus::Running;
    snapshot.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Dispatching,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(100),
            finished_at_ms: None,
            activation_executions_baseline: 0,
        },
    );
    let mut record = sample_record("recover_roundtrip", &["step".to_string()]);
    record.status = snapshot.status;
    record.steps = snapshot.steps.clone();
    store
        .update_run_state("recover_roundtrip", &record)
        .expect("persist");

    let loaded = store
        .load_run("recover_roundtrip")
        .expect("load")
        .expect("record present");
    let mut parsed = loaded.snapshot();
    recover_snapshot(&mut parsed, 1_000);
    assert_eq!(parsed.status, KernelRunStatus::Running);
    assert_eq!(
        parsed.steps.get("step").map(|step| step.status),
        Some(KernelStepStatus::Runnable)
    );
}

#[test]
fn run_store_loads_pre_typed_runstoreport_on_disk_fixture() {
    // Compatibility fixture: a `state.json` written before `RunStorePort` was
    // typed (`load_run`/`update_run_state` previously used `KernelValue`).
    // The on-disk shape is unchanged, so the typed port must continue to load
    // such records without migration.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store
        .create_run(&sample_record("compat_fixture", &["step".to_string()]))
        .expect("create run");
    let path = tmp.path().join("runs/run_compat_fixture/state.json");
    let fixture = serde_json::json!({
        "run_id": "compat_fixture",
        "workflow_id": "wf",
        "workflow_version": "v1",
        "status": "running",
        "steps": {
            "step": {
                "step_id": "step",
                "status": "dispatching",
                "executions": 1,
                "retry_wait_until_ms": null,
                "skip_reason": null,
                "last_request": null,
                "last_result": null,
                "started_at_ms": 100,
                "finished_at_ms": null
            }
        },
        "executions": {"step": 1},
        "recipe_snapshot": {"path": "recipe.yaml", "sha256": "abc"},
        "lineage": {
            "mode": "fresh",
            "source_run_id": null,
            "root_run_id": "compat_fixture",
            "parent_run_id": null,
            "parent_step_id": null
        },
        "workflow_inputs": {},
        "resume_count": 0,
        "created_at_ms": 1,
        "updated_at_ms": 1
    });
    std::fs::write(&path, serde_json::to_string_pretty(&fixture).expect("json"))
        .expect("write fixture");

    let loaded = store
        .load_run("compat_fixture")
        .expect("load fixture")
        .expect("record present");
    assert_eq!(loaded.run_id, "compat_fixture");
    assert_eq!(loaded.status, KernelRunStatus::Running);
    assert_eq!(
        loaded.steps.get("step").map(|step| step.status),
        Some(KernelStepStatus::Dispatching)
    );

    // Round-trip: persist back and reload, contents are preserved.
    store
        .update_run_state("compat_fixture", &loaded)
        .expect("persist");
    let reloaded = store
        .load_run("compat_fixture")
        .expect("reload")
        .expect("record present");
    assert_eq!(reloaded.run_id, loaded.run_id);
    assert_eq!(reloaded.status, loaded.status);
    assert_eq!(
        reloaded.steps.get("step").map(|s| s.status),
        loaded.steps.get("step").map(|s| s.status)
    );
}

// ---------------------------------------------------------------------------
// Crash-durability tests for Step 12.
// ---------------------------------------------------------------------------

/// Serializes all fault-injection tests in this binary so a process-global
/// atomic fault flag cannot leak between parallel test threads.
static FAULT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard that arms a fault point and clears it on drop, even when
/// the test panics.
struct FaultGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl FaultGuard {
    fn arm(point: usize) -> Self {
        let lock = FAULT_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        fault_injection::set(point);
        Self { _lock: lock }
    }
}

impl Drop for FaultGuard {
    fn drop(&mut self) {
        fault_injection::clear();
    }
}

/// Acquires the fault-injection mutex and ensures no fault is armed. Use
/// in tests that call `create_run_staged` but do not inject a fault, so a
/// parallel fault-injection test cannot leak its flag into this test.
struct NoFaultZone {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl NoFaultZone {
    fn enter() -> Self {
        let lock = FAULT_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        fault_injection::clear();
        Self { _lock: lock }
    }
}

fn birth_data(run_id: &str, steps: &[String]) -> BirthData {
    let record = sample_record(run_id, steps);
    let run_inputs_json = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "transist.run_inputs.v1",
        "run_id": run_id,
        "workflow": {"id": "wf", "version": "v1"},
        "values": {},
        "sources": {}
    }))
    .expect("inputs json");
    let provenance_json = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "transist.run_provenance.v1",
        "run_id": run_id,
        "workflow_id": "wf",
        "workflow_version": "v1",
        "workflow_signature_verified": false,
        "workflow_trust_decision": "not_verified",
        "trusted_execution_mode": false,
        "policy_decision": {"allowed": true, "mode": "run_start"},
        "external_transfer_summary": {"count": 0},
        "decision": "allowed",
        "created_at_ms": 1u64
    }))
    .expect("provenance json");
    BirthData {
        record,
        recipe_yaml: "recipe: wf\n".to_string(),
        run_inputs_json,
        provenance_json,
    }
}

struct BirthData {
    record: PersistedRunRecord,
    recipe_yaml: String,
    run_inputs_json: Vec<u8>,
    provenance_json: Vec<u8>,
}

impl BirthData {
    fn artifacts(&self) -> RunBirthArtifacts<'_> {
        RunBirthArtifacts {
            record: &self.record,
            recipe_yaml: &self.recipe_yaml,
            run_inputs_json: &self.run_inputs_json,
            provenance_json: &self.provenance_json,
            workflow_asset_manifest_json: None,
            workflow_asset_blobs: &[],
        }
    }
}

fn write_locator_for(home: &Path, record: &PersistedRunRecord) {
    let locator =
        plan_run_locator(home, &record.run_id, record.lineage.as_ref()).expect("plan locator");
    write_run_locator(home, &locator).expect("write locator");
}

#[test]
fn crash_at_run_birth_after_state_exposes_no_run() {
    let _guard = FaultGuard::arm(fault_injection::FAULT_RUN_BIRTH_AFTER_STATE);
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("crash-state", &["step".to_string()]);
    let artifacts = data.artifacts();
    let err = store.create_run_staged(&artifacts).expect_err("must fail");
    assert!(err.to_string().contains("injected fault"));
    fault_injection::clear();
    // The final run root must not exist because the staging dir was never
    // renamed.
    let locator = plan_run_locator(tmp.path(), "crash-state", data.record.lineage.as_ref())
        .expect("plan locator");
    let final_root = transist_storage_local::run_paths::runs_root(tmp.path())
        .join(locator.relative_path_buf().expect("path"));
    assert!(
        !final_root.join("state.json").exists(),
        "no run should be visible"
    );
    // The run must not be discoverable by the query service.
    assert!(!store.run_exists("crash-state").expect("run_exists"));
}

#[test]
fn staged_run_creation_persists_and_reconciles_workflow_asset_blobs() {
    let home = tempfile::tempdir().expect("home");
    let store = LocalRunStore::new(LocalStorageConfig::new(home.path()));
    let birth = birth_data("asset-birth", &[]);
    let bytes = b"#!/bin/sh\nprintf captured\n".to_vec();
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let blobs = vec![WorkflowAssetBlob {
        sha256: sha256.clone(),
        bytes: bytes.clone(),
    }];
    let manifest = format!(
        r#"{{"schema":"transist.workflow_asset_snapshot.v1","entries":[{{"path":"helper.sh","sha256":"{sha256}"}}]}}"#
    );
    let artifacts = RunBirthArtifacts {
        record: &birth.record,
        recipe_yaml: &birth.recipe_yaml,
        run_inputs_json: &birth.run_inputs_json,
        provenance_json: &birth.provenance_json,
        workflow_asset_manifest_json: Some(manifest.as_bytes()),
        workflow_asset_blobs: &blobs,
    };

    store.create_run_staged(&artifacts).expect("first birth");
    store
        .create_run_staged(&artifacts)
        .expect("reconciled birth");
    let root = home.path().join("runs/run_asset-birth");
    assert_eq!(
        std::fs::read(root.join("workflow_assets/blobs").join(&sha256)).expect("blob"),
        bytes
    );
    assert_eq!(
        std::fs::read(root.join("workflow_assets/manifest.json")).expect("manifest"),
        manifest.as_bytes()
    );
    assert_eq!(
        store
            .read_workflow_asset_blob("asset-birth", &sha256)
            .expect("read blob"),
        Some(bytes)
    );
}

#[test]
fn crash_at_run_birth_before_rename_exposes_no_run() {
    let _guard = FaultGuard::arm(fault_injection::FAULT_RUN_BIRTH_BEFORE_RENAME);
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("crash-rename", &["step".to_string()]);
    let artifacts = data.artifacts();
    let err = store.create_run_staged(&artifacts).expect_err("must fail");
    assert!(err.to_string().contains("injected fault"));
    fault_injection::clear();
    assert!(!store.run_exists("crash-rename").expect("run_exists"));
}

#[test]
fn crash_at_run_birth_after_rename_exposes_complete_run() {
    let _guard = FaultGuard::arm(fault_injection::FAULT_RUN_BIRTH_AFTER_RENAME);
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("crash-after-rename", &["step".to_string()]);
    let artifacts = data.artifacts();
    // The rename succeeds, the run root exists with all artifacts, but the
    // method returns an injected error. A recovery attempt (writing the
    // locator) must succeed because the run root is durable.
    let err = store.create_run_staged(&artifacts).expect_err("must fail");
    assert!(err.to_string().contains("injected fault"));
    fault_injection::clear();
    // The run root should be complete (state.json, recipe.yaml, etc.) even
    // though the method returned an error — the rename already happened.
    write_locator_for(tmp.path(), &data.record);
    assert!(store.run_exists("crash-after-rename").expect("run_exists"));
    let loaded = store
        .load_run("crash-after-rename")
        .expect("load")
        .expect("record present");
    assert_eq!(loaded.run_id, "crash-after-rename");
    let recipe = store
        .load_recipe_snapshot("crash-after-rename")
        .expect("recipe");
    assert!(recipe.contains("recipe: wf"));
}

#[test]
fn post_rename_retry_reconciles_complete_birth_before_locator_publication() {
    let _guard = FaultGuard::arm(fault_injection::FAULT_RUN_BIRTH_AFTER_RENAME);
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("birth-retry", &["step".to_string()]);
    let err = store
        .create_run_staged(&data.artifacts())
        .expect_err("post-rename fault");
    assert!(err.to_string().contains("injected fault"));
    fault_injection::clear();

    store
        .create_run_staged(&data.artifacts())
        .expect("recover birth");
    write_locator_for(tmp.path(), &data.record);
    assert!(store.run_exists("birth-retry").expect("run exists"));
}

#[test]
fn post_rename_retry_with_a_real_clock_reconciles_structurally() {
    // The first attempt crashes after rename (state.json on disk, no
    // locator). The kernel's next real `start_recipe`/`prepare_recipe`
    // attempt reaches this retry with a fresh, real clock: `created_at_ms`
    // and `updated_at_ms` legitimately differ from the original attempt.
    // Reconciliation must succeed structurally rather than requiring a
    // byte-identical `state.json`/`provenance.json`.
    let _guard = FaultGuard::arm(fault_injection::FAULT_RUN_BIRTH_AFTER_RENAME);
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut first_attempt = birth_data("birth-retry-real-clock", &["step".to_string()]);
    first_attempt.record.created_at_ms = 1_000;
    first_attempt.record.updated_at_ms = 1_000;
    let err = store
        .create_run_staged(&first_attempt.artifacts())
        .expect_err("post-rename fault");
    assert!(err.to_string().contains("injected fault"));
    fault_injection::clear();

    let mut retry_attempt = birth_data("birth-retry-real-clock", &["step".to_string()]);
    retry_attempt.record.created_at_ms = 999_999;
    retry_attempt.record.updated_at_ms = 999_999;
    retry_attempt.provenance_json = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "transist.run_provenance.v1",
        "run_id": "birth-retry-real-clock",
        "workflow_id": "wf",
        "workflow_version": "v1",
        "workflow_signature_verified": false,
        "workflow_trust_decision": "not_verified",
        "trusted_execution_mode": false,
        "policy_decision": {"allowed": true, "mode": "run_start"},
        "external_transfer_summary": {"count": 0},
        "decision": "allowed",
        "created_at_ms": 999_999u64
    }))
    .expect("provenance json");

    store
        .create_run_staged(&retry_attempt.artifacts())
        .expect("structural reconciliation must succeed despite differing timestamps");
    write_locator_for(tmp.path(), &retry_attempt.record);
    assert!(
        store
            .run_exists("birth-retry-real-clock")
            .expect("run exists")
    );
    let loaded = store
        .load_run("birth-retry-real-clock")
        .expect("load")
        .expect("record present");
    // The persisted state on disk retains the *original* attempt's values;
    // reconciliation does not overwrite it, it only confirms compatibility
    // and lets the caller republish the locator and proceed.
    assert_eq!(loaded.created_at_ms, 1_000);
}

#[test]
fn post_rename_retry_rejects_genuinely_conflicting_birth() {
    // A conflicting attempted birth for the same run id (different
    // workflow) must still be rejected, even though timestamps are
    // ignored during reconciliation.
    let _guard = FaultGuard::arm(fault_injection::FAULT_RUN_BIRTH_AFTER_RENAME);
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("birth-retry-conflict", &["step".to_string()]);
    let err = store
        .create_run_staged(&data.artifacts())
        .expect_err("post-rename fault");
    assert!(err.to_string().contains("injected fault"));
    fault_injection::clear();

    let mut conflicting = birth_data("birth-retry-conflict", &["step".to_string()]);
    conflicting.record.workflow_id = "different-wf".to_string();

    let err = store
        .create_run_staged(&conflicting.artifacts())
        .expect_err("conflicting birth must be rejected");
    assert!(err.to_string().contains("conflicting run birth artifact"));
}

#[test]
fn locator_publication_retries_after_link_and_directory_sync() {
    for point in [
        fault_injection::FAULT_AFTER_LINK,
        fault_injection::FAULT_AFTER_DIRECTORY_SYNC,
    ] {
        let _zone = NoFaultZone::enter();
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        let data = birth_data("locator-retry", &["step".to_string()]);
        store.create_run_staged(&data.artifacts()).expect("birth");
        let locator_path =
            transist_storage_local::run_paths::run_locator_path(tmp.path(), "locator-retry");
        std::fs::create_dir_all(locator_path.parent().expect("locator parent")).expect("index");

        fault_injection::set(point);
        assert!(
            transist_storage_local::run_paths::write_run_locator(
                tmp.path(),
                &plan_run_locator(tmp.path(), "locator-retry", data.record.lineage.as_ref())
                    .expect("locator"),
            )
            .is_err()
        );
        fault_injection::clear();
        write_locator_for(tmp.path(), &data.record);
        assert_eq!(
            std::fs::read_dir(locator_path.parent().expect("locator parent"))
                .expect("index entries")
                .count(),
            1
        );
    }
}

#[test]
fn strict_execution_record_rejects_retry_after_link_or_directory_sync() {
    for point in [
        fault_injection::FAULT_AFTER_LINK,
        fault_injection::FAULT_AFTER_DIRECTORY_SYNC,
    ] {
        let _zone = NoFaultZone::enter();
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_id = match point {
            fault_injection::FAULT_AFTER_LINK => "strict-link",
            _ => "strict-directory-sync",
        };
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        let data = birth_data(run_id, &["step".to_string()]);
        store.create_run_staged(&data.artifacts()).expect("birth");
        write_locator_for(tmp.path(), &data.record);
        let execution = RunStoreStepExecution {
            run_id: run_id.to_string(),
            step_id: "step".to_string(),
            execution: 1,
            state: serde_json::json!({
                "run_id": run_id,
                "step_id": "step",
                "execution": 1,
                "status": "success"
            })
            .into(),
        };
        let execution_path = resolve_run_root(tmp.path(), run_id)
            .expect("run root")
            .join("step_executions/step/execution_001.json");
        std::fs::create_dir_all(execution_path.parent().expect("execution parent"))
            .expect("execution parent");

        fault_injection::set(point);
        assert!(
            store.store_step_execution(&execution).is_err(),
            "fault at publication point must fail the write"
        );
        fault_injection::clear();
        assert!(
            execution_path.is_file(),
            "ambiguous write leaves one record"
        );
        assert!(
            store.store_step_execution(&execution).is_err(),
            "strict execution publication must reject retry"
        );
    }
}

#[test]
fn birth_recovery_rejects_missing_and_conflicting_artifacts() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("birth-conflict", &["step".to_string()]);
    store.create_run_staged(&data.artifacts()).expect("birth");
    let run_root =
        transist_storage_local::run_paths::runs_root(tmp.path()).join("run_birth-conflict");
    let inputs_path = run_root.join("context/run_inputs.json");
    std::fs::remove_file(&inputs_path).expect("remove inputs");
    assert!(store.create_run_staged(&data.artifacts()).is_err());
    assert!(!inputs_path.exists());

    std::fs::write(&inputs_path, b"conflicting inputs").expect("conflicting inputs");
    assert!(store.create_run_staged(&data.artifacts()).is_err());
    assert_eq!(
        std::fs::read(&inputs_path).expect("read inputs"),
        b"conflicting inputs"
    );
}

#[test]
fn staged_run_creation_produces_complete_run_on_success() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("staged-success", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("staged create");
    assert!(
        !transist_storage_local::run_paths::run_locator_path(tmp.path(), "staged-success").exists(),
        "staged storage must not publish the locator"
    );
    write_locator_for(tmp.path(), &data.record);
    assert!(store.run_exists("staged-success").expect("run_exists"));
    let loaded = store
        .load_run("staged-success")
        .expect("load")
        .expect("record present");
    assert_eq!(loaded.run_id, "staged-success");
    let recipe = store
        .load_recipe_snapshot("staged-success")
        .expect("recipe");
    assert!(recipe.contains("recipe: wf"));
    let run_root = resolve_run_root(tmp.path(), "staged-success").expect("run root");
    assert!(run_root.join("context").join("run_inputs.json").exists());
    assert!(run_root.join("provenance.json").exists());
}

#[test]
fn recipe_sync_recovers_a_consistent_pair_after_each_durable_boundary() {
    let fault_points = [
        fault_injection::FAULT_WRITE_REPLACE_FILE,
        fault_injection::FAULT_AFTER_WRITE,
        fault_injection::FAULT_AFTER_FLUSH,
        fault_injection::FAULT_AFTER_FILE_SYNC,
        fault_injection::FAULT_AFTER_RENAME,
        fault_injection::FAULT_AFTER_DIRECTORY_SYNC,
        fault_injection::FAULT_AFTER_REMOVE_FILE,
    ];
    let old_recipe = "schema: transist.recipe.v1\nworkflow: old\n";
    let new_recipe = "schema: transist.recipe.v1\nworkflow: new\n";

    let mut index = 0;
    for point in fault_points {
        for occurrence in 1..=8 {
            let tmp = tempfile::tempdir().expect("tempdir");
            let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
            let mut old = sample_record(&format!("recipe-sync-{index}"), &["step".to_string()]);
            old.recipe_snapshot = Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: digest(old_recipe),
            });
            store.create_run(&old).expect("create run");
            store
                .write_recipe_snapshot(&old.run_id, old_recipe)
                .expect("write old recipe");
            let run_root = resolve_run_root(tmp.path(), &old.run_id).expect("run root");
            let old_state_bytes = std::fs::read(run_root.join("state.json")).expect("old state");
            let old_state: serde_json::Value =
                serde_json::from_slice(&old_state_bytes).expect("parse old state");

            let mut new = old.clone();
            new.recipe_snapshot = Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: digest(new_recipe),
            });
            fault_injection::set_occurrence(point, occurrence);
            let sync_result = store.sync_recipe_snapshot(&new.run_id, &new, new_recipe);
            fault_injection::clear();
            if sync_result.is_ok() {
                assert!(occurrence > 1, "fault point {point} was never reached");
                break;
            }

            let loaded = store
                .load_run(&new.run_id)
                .expect("recover run")
                .expect("run exists");
            let state_bytes = std::fs::read(run_root.join("state.json")).expect("recovered state");
            let state: serde_json::Value =
                serde_json::from_slice(&state_bytes).expect("parse recovered state");
            let recipe_bytes =
                std::fs::read(run_root.join("recipe.yaml")).expect("recovered recipe bytes");

            // `prepare_recipe_state` updates only these timestamps while
            // serializing the new state. Compare every other state field and
            // require the recipe bytes to belong to that same complete state.
            let mut expected_new = new.clone();
            expected_new.refresh_executions();
            let mut expected_new_state =
                serde_json::to_value(expected_new).expect("serialize expected new state");
            expected_new_state["created_at_ms"] = state["created_at_ms"].clone();
            expected_new_state["updated_at_ms"] = state["updated_at_ms"].clone();
            let old_pair = state == old_state && recipe_bytes == old_recipe.as_bytes();
            let new_pair = state == expected_new_state && recipe_bytes == new_recipe.as_bytes();
            assert!(
                old_pair || new_pair,
                "fault point {point}, occurrence {occurrence} recovered a mixed or incomplete pair"
            );
            assert_eq!(
                serde_json::to_value(loaded).expect("serialize recovered state"),
                state,
                "fault point {point}, occurrence {occurrence} state is not complete"
            );
            assert!(
                !run_root.join(".recipe-sync.intent.json").exists(),
                "recovery must remove intent at fault point {point}, occurrence {occurrence}"
            );
            index += 1;
        }
    }
}

#[test]
fn recipe_sync_rejects_unsupported_revisions_before_repairing_any_files() {
    let _zone = NoFaultZone::enter();
    for field in ["asset_lock_version", "capability_authority_version"] {
        for version in [2, 3, u32::MAX] {
            for unsupported_pair in ["old", "new"] {
                for current_pair in ["old", "new"] {
                    assert_recipe_sync_revision_rejected(
                        field,
                        version,
                        unsupported_pair,
                        current_pair,
                    );
                }
            }
        }
    }
}

fn assert_recipe_sync_revision_rejected(
    field: &str,
    version: u32,
    unsupported_pair: &str,
    current_pair: &str,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("unsupported-sync", &["step".to_string()]);
    record.asset_lock_version = 1;
    record.capability_authority_version = 1;
    store.create_run(&record).expect("create run");
    let root = resolve_run_root(tmp.path(), &record.run_id).expect("run root");
    let old_recipe = "schema: transist.recipe.v1\nworkflow: old\n";
    let new_recipe = "schema: transist.recipe.v1\nworkflow: new\n";
    let mut old = serde_json::to_value(&record).expect("old record");
    old["recipe_snapshot"] =
        serde_json::json!({"path": "recipe.yaml", "sha256": digest(old_recipe)});
    let mut new = old.clone();
    new["recipe_snapshot"]["sha256"] = serde_json::json!(digest(new_recipe));
    if unsupported_pair == "old" {
        old[field] = serde_json::json!(version);
    } else {
        new[field] = serde_json::json!(version);
    }
    let old_json = serde_json::to_string(&old).expect("old JSON");
    let new_json = serde_json::to_string(&new).expect("new JSON");
    let intent = serde_json::to_vec(&serde_json::json!({
        "schema": "transist.recipe-sync-intent.v1",
        "old_state_id": digest(&old_json),
        "new_state_id": digest(&new_json),
        "old_state_json": old_json,
        "new_state_json": new_json,
        "old_recipe_yaml": old_recipe,
        "new_recipe_yaml": new_recipe,
    }))
    .expect("intent JSON");
    let state = if current_pair == "old" {
        &old_json
    } else {
        &new_json
    };
    // Deliberately mixed files require a real repair if the revision guard is absent.
    let recipe = if current_pair == "old" {
        new_recipe
    } else {
        old_recipe
    };
    std::fs::write(root.join("state.json"), state).expect("state");
    std::fs::write(root.join("recipe.yaml"), recipe).expect("recipe");
    std::fs::write(root.join(".recipe-sync.intent.json"), &intent).expect("intent");
    let files = || -> BTreeMap<PathBuf, Vec<u8>> {
        std::fs::read_dir(&root)
            .expect("run files")
            .map(|entry| {
                let path = entry.expect("entry").path();
                let bytes = std::fs::read(&path).expect("file bytes");
                (path, bytes)
            })
            .collect()
    };
    let before = files();
    for error in [
        store
            .load_run(&record.run_id)
            .expect_err("load must reject before recovery"),
        store
            .load_recipe_snapshot(&record.run_id)
            .expect_err("recipe read must reject before recovery"),
    ] {
        let contract = if field == "asset_lock_version" {
            "asset lock"
        } else {
            "capability authority"
        };
        assert!(
            error
                .to_string()
                .contains(&format!("unsupported {contract} version {version}")),
            "{error:#}"
        );
        assert_eq!(
            files(),
            before,
            "{field}={version}, unsupported {unsupported_pair}, current {current_pair}"
        );
    }
}

fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn truncated_final_jsonl_record_preserves_prior_records_on_inspection() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("truncated-events", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    for i in 1..=3u64 {
        events
            .publish(&RuntimeEvent {
                kind: "step.started".to_string(),
                run_id: "truncated-events".to_string(),
                step_id: Some("step".to_string()),
                timestamp_unix_ms: i,
                payload: transist_protocol::KernelValue::Null,
            })
            .expect("publish");
    }

    // Simulate a crash that writes a partial final record (no newline).
    let run_root = resolve_run_root(tmp.path(), "truncated-events").expect("run root");
    let events_path = run_root.join("events.jsonl");
    let mut raw = std::fs::read_to_string(&events_path).expect("read events");
    raw.push_str(r#"{"kind":"step.started","run_id":"truncated-events","#);
    std::fs::write(&events_path, raw).expect("write truncated");

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed = inspection.list_events("truncated-events").expect("list");
    assert_eq!(listed.len(), 3, "the three complete records survive");
}

#[test]
fn tail_events_and_inspection_agree_on_a_truncated_tail() {
    // `LocalRunQueryService::tail_events` and the inspection store's
    // `list_events` must tolerate the same truncated-tail shape identically:
    // both drop a malformed non-newline-terminated final fragment and
    // return exactly the complete, well-formed prior records.
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("tail-events-parity", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    for i in 1..=3u64 {
        events
            .publish(&RuntimeEvent {
                kind: "step.started".to_string(),
                run_id: "tail-events-parity".to_string(),
                step_id: Some("step".to_string()),
                timestamp_unix_ms: i,
                payload: transist_protocol::KernelValue::Null,
            })
            .expect("publish");
    }

    let run_root = resolve_run_root(tmp.path(), "tail-events-parity").expect("run root");
    let events_path = run_root.join("events.jsonl");
    let mut raw = std::fs::read_to_string(&events_path).expect("read events");
    raw.push_str(r#"{"kind":"step.started","run_id":"tail-events-parit"#);
    std::fs::write(&events_path, raw).expect("write truncated");

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed = inspection
        .list_events("tail-events-parity")
        .expect("inspection list_events must tolerate the truncated tail");
    assert_eq!(listed.len(), 3);

    let query = LocalRunQueryService::new(tmp.path());
    let tailed = query
        .tail_events("tail-events-parity", 50)
        .expect("tail_events must tolerate the truncated tail");
    assert_eq!(
        tailed.len(),
        3,
        "tail_events must agree with inspection's list_events on a truncated tail"
    );
}

#[test]
fn tail_events_and_inspection_agree_on_a_valid_final_record_without_a_newline() {
    // A crash that leaves the final record missing only its trailing
    // newline must not destroy the record: both the inspection store's
    // `list_events` and the event-tail surface must retain the same valid
    // final record and return the same set on the same fixture file.
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("tail-events-valid-final", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    for i in 1..=3u64 {
        events
            .publish(&RuntimeEvent {
                kind: "step.started".to_string(),
                run_id: "tail-events-valid-final".to_string(),
                step_id: Some("step".to_string()),
                timestamp_unix_ms: i,
                payload: transist_protocol::KernelValue::Null,
            })
            .expect("publish");
    }

    // Plant a complete final record that only lacks its trailing newline.
    let run_root = resolve_run_root(tmp.path(), "tail-events-valid-final").expect("run root");
    let events_path = run_root.join("events.jsonl");
    let mut raw = std::fs::read_to_string(&events_path).expect("read events");
    raw.push_str(r#"{"kind":"step.started","run_id":"tail-events-valid-final","step_id":"step","timestamp_unix_ms":4,"payload":null}"#);
    std::fs::write(&events_path, raw).expect("write valid no-newline final record");

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed = inspection
        .list_events("tail-events-valid-final")
        .expect("inspection list_events must retain the valid final record");
    assert_eq!(
        listed.len(),
        4,
        "list_events must retain the valid newline-less final record"
    );

    let query = LocalRunQueryService::new(tmp.path());
    let tailed = query
        .tail_events("tail-events-valid-final", 50)
        .expect("tail_events must tolerate and retain the valid final record");
    assert_eq!(
        tailed.len(),
        4,
        "tail_events must agree with inspection's list_events on a valid newline-less final record"
    );
    assert_eq!(
        listed
            .iter()
            .map(|event| event.event.timestamp_unix_ms)
            .collect::<Vec<_>>(),
        tailed
            .iter()
            .map(|event| event.timestamp_unix_ms)
            .collect::<Vec<_>>(),
        "both event surfaces must return the same set of events on the same fixture file"
    );
}

#[test]
fn resume_after_truncated_tail_repairs_and_appends_recovery_event() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("tail-repair", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    for i in 1..=2u64 {
        events
            .publish(&RuntimeEvent {
                kind: "step.started".to_string(),
                run_id: "tail-repair".to_string(),
                step_id: Some("step".to_string()),
                timestamp_unix_ms: i,
                payload: transist_protocol::KernelValue::Null,
            })
            .expect("publish");
    }

    // Plant a truncated tail.
    let run_root = resolve_run_root(tmp.path(), "tail-repair").expect("run root");
    let events_path = run_root.join("events.jsonl");
    let mut raw = std::fs::read_to_string(&events_path).expect("read events");
    raw.push_str(r#"{"kind":"step.started","run_id":"tail-rep"#);
    std::fs::write(&events_path, raw).expect("write truncated");

    // The next publish must repair the tail, append a storage_tail_repaired
    // event, then append the new event.
    events
        .publish(&RuntimeEvent {
            kind: "step.finished".to_string(),
            run_id: "tail-repair".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 99,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect("publish after truncation");

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed = inspection.list_events("tail-repair").expect("list");
    let kinds: Vec<&str> = listed.iter().map(|e| e.event.kind.as_str()).collect();
    assert!(
        kinds.contains(&"storage.tail_repaired"),
        "repair event must be recorded"
    );
    assert!(
        kinds.contains(&"step.finished"),
        "new event must be appended"
    );
    // The truncated partial record must not appear as an event.
    assert_eq!(
        listed
            .iter()
            .filter(|e| e.event.kind == "step.started")
            .count(),
        2,
        "both prior complete records survive"
    );
}

#[test]
fn distinct_tail_repairs_on_the_same_stream_each_record_their_own_event() {
    // A second, later, genuinely distinct tail-repair on the same stream
    // (after the file has grown past the first repair) must be recorded as
    // its own `storage.tail_repaired` event rather than being silently
    // skipped as a duplicate of the first repair's event.
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("repeat-tail-repair", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    events
        .publish(&RuntimeEvent {
            kind: "step.started".to_string(),
            run_id: "repeat-tail-repair".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 1,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect("publish");

    let run_root = resolve_run_root(tmp.path(), "repeat-tail-repair").expect("run root");
    let events_path = run_root.join("events.jsonl");

    // First crash: truncated tail, repaired on the next publish.
    let mut raw = std::fs::read_to_string(&events_path).expect("read events");
    raw.push_str(r#"{"kind":"step.started","run_id":"first-crash"#);
    std::fs::write(&events_path, &raw).expect("write first truncation");
    events
        .publish(&RuntimeEvent {
            kind: "step.progress".to_string(),
            run_id: "repeat-tail-repair".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 2,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect("publish after first truncation");

    // Second crash: a new truncated tail further into the (now larger)
    // stream, repaired on the next publish. This is a distinct occurrence.
    let mut raw = std::fs::read_to_string(&events_path).expect("read events");
    raw.push_str(r#"{"kind":"step.started","run_id":"second-crash"#);
    std::fs::write(&events_path, &raw).expect("write second truncation");
    events
        .publish(&RuntimeEvent {
            kind: "step.finished".to_string(),
            run_id: "repeat-tail-repair".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 3,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect("publish after second truncation");

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed = inspection.list_events("repeat-tail-repair").expect("list");
    let repaired_count = listed
        .iter()
        .filter(|e| e.event.kind == "storage.tail_repaired")
        .count();
    assert_eq!(
        repaired_count, 2,
        "each distinct repair occurrence must be recorded, not deduplicated \
         against an earlier one; listed={listed:?}"
    );
}

#[test]
fn malformed_interior_jsonl_record_fails_inspection() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("malformed-interior", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let run_root = resolve_run_root(tmp.path(), "malformed-interior").expect("run root");
    let events_path = run_root.join("events.jsonl");
    // Write two valid records, then a malformed but newline-terminated
    // (complete) record, then a valid record.
    let valid = r#"{"kind":"a","run_id":"malformed-interior","timestamp_unix_ms":1}"#;
    let malformed = r#"{"kind":"b","run_id":"malformed-interior"}"#; // missing timestamp_unix_ms
    std::fs::write(&events_path, format!("{valid}\n{malformed}\n{valid}\n")).expect("write");

    let inspection = LocalInspectionStore::new(tmp.path());
    let result = inspection.list_events("malformed-interior");
    assert!(
        result.is_err(),
        "malformed complete record must fail inspection"
    );
}

#[test]
fn malformed_complete_recovery_records_fail_closed() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("malformed-recovery", &["step".to_string()]);
    store.create_run_staged(&data.artifacts()).expect("create");
    write_locator_for(tmp.path(), &data.record);
    let run_root = resolve_run_root(tmp.path(), "malformed-recovery").expect("run root");

    std::fs::write(run_root.join("events.jsonl"), b"{}\n").expect("events");
    assert!(
        store.load_run_events("malformed-recovery").is_err(),
        "recovery must reject malformed complete event records"
    );

    std::fs::write(run_root.join("failures.jsonl"), b"{}\n").expect("failures");
    assert!(
        store.load_run_failures("malformed-recovery").is_err(),
        "recovery must reject malformed complete failure records"
    );
}

#[cfg(unix)]
#[test]
fn external_transfer_evidence_rejects_symlinked_directories() {
    use std::os::unix::fs::symlink;

    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("evidence-symlink", &["step".to_string()]);
    store.create_run_staged(&data.artifacts()).expect("create");
    write_locator_for(tmp.path(), &data.record);
    let run_root = resolve_run_root(tmp.path(), "evidence-symlink").expect("run root");
    let artifacts = run_root.join("artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts");
    let outside = tmp.path().join("outside-evidence");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(
        outside.join("transist_external_transfer_outside.json"),
        b"{}",
    )
    .expect("outside record");
    symlink(&outside, artifacts.join("linked-evidence")).expect("symlink");

    assert!(
        store
            .load_external_transfer_evidence(tmp.path(), "evidence-symlink")
            .is_err(),
        "evidence traversal must reject symlinked directories"
    );
}

#[cfg(unix)]
#[test]
fn external_transfer_evidence_rejects_a_symlinked_artifacts_root() {
    use std::os::unix::fs::symlink;

    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("evidence-root-symlink", &["step".to_string()]);
    store.create_run_staged(&data.artifacts()).expect("create");
    write_locator_for(tmp.path(), &data.record);
    let run_root = resolve_run_root(tmp.path(), "evidence-root-symlink").expect("run root");
    let artifacts = run_root.join("artifacts");
    let outside = tmp.path().join("outside-evidence-root");
    std::fs::create_dir_all(&outside).expect("outside");
    symlink(&outside, &artifacts).expect("symlink");

    assert!(
        store
            .load_external_transfer_evidence(tmp.path(), "evidence-root-symlink")
            .is_err(),
        "evidence traversal must reject a symlinked artifacts root"
    );
}

#[test]
fn existing_v1_run_directory_remains_readable_without_migration() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    // Manually create a pre-existing run directory using the old layout:
    // locator + state.json only (no staging).
    let run_id = "legacy-v1";
    let record = sample_record(run_id, &["step".to_string()]);
    let locator = plan_run_locator(tmp.path(), run_id, record.lineage.as_ref()).expect("locator");
    write_run_locator(tmp.path(), &locator).expect("locator write");
    let run_root = transist_storage_local::run_paths::runs_root(tmp.path())
        .join(locator.relative_path_buf().expect("path"));
    std::fs::create_dir_all(&run_root).expect("dir");
    std::fs::write(
        run_root.join("state.json"),
        serde_json::to_string_pretty(&record).expect("json"),
    )
    .expect("state");

    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    assert!(store.run_exists(run_id).expect("exists"));
    let loaded = store
        .load_run(run_id)
        .expect("load")
        .expect("record present");
    assert_eq!(loaded.run_id, run_id);
}

#[test]
fn append_failure_retains_last_valid_stream_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("append-fail", &["step".to_string()]);
    let artifacts = data.artifacts();
    // Setup phase: run birth and two valid publishes under NoFaultZone so a
    // parallel fault-injection test cannot leak its flag into setup.
    {
        let _zone = NoFaultZone::enter();
        store.create_run_staged(&artifacts).expect("create");
        write_locator_for(tmp.path(), &data.record);
        let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
        for i in 1..=2u64 {
            events
                .publish(&RuntimeEvent {
                    kind: "step.started".to_string(),
                    run_id: "append-fail".to_string(),
                    step_id: Some("step".to_string()),
                    timestamp_unix_ms: i,
                    payload: transist_protocol::KernelValue::Null,
                })
                .expect("publish");
        }
    }

    // Fault phase: arm the append fault; the next publish must fail and the
    // stream must retain its prior valid state (two events, no corruption).
    let _guard = FaultGuard::arm(fault_injection::FAULT_APPEND_JSONL);
    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    let err = events
        .publish(&RuntimeEvent {
            kind: "step.finished".to_string(),
            run_id: "append-fail".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 99,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect_err("publish must fail");
    assert!(
        err.to_string().contains("injected fault")
            || err.root_cause().to_string().contains("injected fault"),
        "actual error: {err}"
    );
    fault_injection::clear();

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed = inspection.list_events("append-fail").expect("list");
    assert_eq!(listed.len(), 2, "prior records survive append failure");
    assert_eq!(listed[0].event.kind, "step.started");
    assert_eq!(listed[1].event.kind, "step.started");
}

#[test]
fn repair_failure_prevents_corrupting_append() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("repair-fail", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    events
        .publish(&RuntimeEvent {
            kind: "step.started".to_string(),
            run_id: "repair-fail".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 1,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect("publish");

    // Plant a truncated tail (no trailing newline).
    let run_root = resolve_run_root(tmp.path(), "repair-fail").expect("run root");
    let events_path = run_root.join("events.jsonl");
    let mut raw = std::fs::read_to_string(&events_path).expect("read events");
    raw.push_str(r#"{"kind":"step.started","run_id":"repair-fail","#);
    std::fs::write(&events_path, raw).expect("write truncated");

    // Block the events path with a directory so repair_jsonl_tail fails.
    // Remove the file, create a directory in its place.
    std::fs::remove_file(&events_path).expect("remove file");
    std::fs::create_dir_all(&events_path).expect("block with directory");

    // The publish must fail (repair cannot open the directory) and must NOT
    // proceed to append after a failed repair.
    let err = events
        .publish(&RuntimeEvent {
            kind: "step.finished".to_string(),
            run_id: "repair-fail".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 2,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect_err("publish must fail on repair error");
    assert!(
        err.to_string().contains("failed repairing event stream"),
        "actual error: {err}"
    );

    // Restore the file and verify the stream is not further corrupted: the
    // blocked path is still a directory (no append happened).
    assert!(events_path.is_dir(), "no append should have occurred");
    std::fs::remove_dir_all(&events_path).expect("remove blocking dir");

    // Recreate the events file with just the original valid record.
    let valid = serde_json::to_string(&RuntimeEvent {
        kind: "step.started".to_string(),
        run_id: "repair-fail".to_string(),
        step_id: Some("step".to_string()),
        timestamp_unix_ms: 1,
        payload: transist_protocol::KernelValue::Null,
    })
    .expect("valid event");
    std::fs::write(&events_path, format!("{valid}\n")).expect("restore");

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed = inspection.list_events("repair-fail").expect("list");
    assert_eq!(listed.len(), 1, "only the original valid record survives");
}

#[test]
fn stale_run_birth_staging_dir_is_cleaned_on_next_birth() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    // Plant an orphaned run-birth staging directory with a dead PID.
    let staging_parent = tmp.path().join(".transist-run-birth-staging");
    std::fs::create_dir_all(&staging_parent).expect("staging parent");
    let orphan = staging_parent.join("run-birth-orphan-run-4294967295");
    std::fs::create_dir_all(&orphan).expect("orphan dir");
    std::fs::write(orphan.join("state.json"), b"{}").expect("orphan state");

    // A new run birth must sweep the orphan before creating its own staging.
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("cleanup-test", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    assert!(!orphan.exists(), "orphan staging dir must be removed");
    assert!(store.run_exists("cleanup-test").expect("run exists"));
}

#[test]
fn stale_run_birth_cleanup_syncs_after_removal() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let staging_parent = tmp.path().join(".transist-run-birth-staging");
    let orphan = staging_parent.join("run-birth-orphan-run-4294967295");
    std::fs::create_dir_all(&orphan).expect("orphan dir");

    fault_injection::set(fault_injection::FAULT_AFTER_DIRECTORY_SYNC);
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("cleanup-sync", &["step".to_string()]);
    assert!(store.create_run_staged(&data.artifacts()).is_err());
    fault_injection::clear();
    assert!(
        !orphan.exists(),
        "cleanup removal must happen before parent sync"
    );
}

#[test]
fn pre_existing_run_root_without_state_is_quarantined_not_deleted() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    // Pre-create the final run root with operator data but no state.json,
    // simulating a crashed or partially populated directory.
    let run_id = "quarantine-test";
    let record = sample_record(run_id, &["step".to_string()]);
    let locator = plan_run_locator(tmp.path(), run_id, record.lineage.as_ref()).expect("locator");
    let final_root = transist_storage_local::run_paths::runs_root(tmp.path())
        .join(locator.relative_path_buf().expect("path"));
    std::fs::create_dir_all(&final_root).expect("create final root");
    std::fs::write(final_root.join("runner.log"), b"evidence of prior activity")
        .expect("write runner log");

    // Run birth must succeed by quarantining the pre-existing directory.
    let data = birth_data(run_id, &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");

    // The new run root must contain state.json.
    assert!(
        final_root.join("state.json").exists(),
        "new run root has state.json"
    );

    // The pre-existing data must be preserved in a quarantine directory.
    let staging_parent = tmp.path().join(".transist-run-birth-staging");
    let quarantine_dirs: Vec<_> = std::fs::read_dir(&staging_parent)
        .expect("read staging parent")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("quarantine-"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(quarantine_dirs.len(), 1, "exactly one quarantine dir");
    let quarantined_log = quarantine_dirs[0].join("runner.log");
    assert!(
        quarantined_log.exists(),
        "pre-existing runner.log preserved in quarantine"
    );
    assert_eq!(
        std::fs::read_to_string(&quarantined_log).expect("read"),
        "evidence of prior activity"
    );
}

#[test]
fn run_birth_with_existing_complete_root_and_locator_accepts_identical_retry() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    // Create a valid existing run via the normal path.
    let run_id = "existing-run";
    let data = birth_data(run_id, &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("first create");
    write_locator_for(tmp.path(), &data.record);

    // A retry after locator publication must reconcile the complete identical
    // birth rather than treating the successful prior publication as a conflict.
    store
        .create_run_staged(&artifacts)
        .expect("identical published birth retry");

    // The original run data must be intact.
    assert!(store.run_exists(run_id).expect("exists"));
    let loaded = store
        .load_run(run_id)
        .expect("load")
        .expect("record present");
    assert_eq!(loaded.run_id, run_id);
}

#[test]
fn run_birth_with_existing_conflicting_artifact_or_locator_is_rejected() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let run_id = "conflicting-published-run";
    let data = birth_data(run_id, &["step".to_string()]);
    store
        .create_run_staged(&data.artifacts())
        .expect("first create");
    write_locator_for(tmp.path(), &data.record);

    let run_root = resolve_run_root(tmp.path(), run_id).expect("run root");
    let recipe_path = run_root.join("recipe.yaml");
    std::fs::write(&recipe_path, b"conflicting recipe\n").expect("conflicting recipe");
    assert!(
        store.create_run_staged(&data.artifacts()).is_err(),
        "conflicting birth artifact must be rejected"
    );
    assert_eq!(
        std::fs::read(&recipe_path).expect("recipe"),
        b"conflicting recipe\n"
    );

    std::fs::write(&recipe_path, data.recipe_yaml.as_bytes()).expect("restore recipe");
    let locator_path = transist_storage_local::run_paths::run_locator_path(tmp.path(), run_id);
    std::fs::write(&locator_path, b"conflicting locator\n").expect("conflicting locator");
    assert!(
        store.create_run_staged(&data.artifacts()).is_err(),
        "conflicting locator must be rejected"
    );
    assert_eq!(
        std::fs::read(&locator_path).expect("locator"),
        b"conflicting locator\n"
    );
}

#[cfg(unix)]
#[test]
fn birth_reconciliation_anchors_one_root_descriptor_across_reads() {
    // The reconciliation path holds ONE anchored descriptor on the run root
    // across the state/recipe/inputs/provenance reads. A same-user racer that
    // swaps the run-root path for a symlink (pointing at attacker bytes)
    // between reads must not make reconciliation accept heterogeneous builds:
    // a fresh anchored open of the symlinked path is rejected, and the whole
    // birth fails closed rather than reconciling attacker bytes.
    use std::os::unix::fs::symlink;

    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let run_id = "anchor-swap";
    let data = birth_data(run_id, &["step".to_string()]);
    store.create_run_staged(&data.artifacts()).expect("birth");
    write_locator_for(tmp.path(), &data.record);

    let run_root = resolve_run_root(tmp.path(), run_id).expect("run root");
    let original = run_root.parent().unwrap().join("run_anchor-swap-original");
    let attacker = tmp.path().join("runs").join("run_anchor-swap-attacker");
    std::fs::create_dir_all(attacker.join("context")).expect("attacker context");
    std::fs::write(attacker.join("state.json"), b"attacker state").expect("attacker state");
    std::fs::write(
        attacker.join("context").join("run_inputs.json"),
        b"attacker inputs",
    )
    .expect("attacker inputs");

    // Move the real run root aside and point the run-root path at attacker bytes.
    std::fs::rename(&run_root, &original).expect("move real root aside");
    symlink(&attacker, &run_root).expect("symlink run root");

    // Reconciliation must reject the symlinked run root — never reconcile the
    // attacker-supplied state under the original run id.
    let error = store
        .create_run_staged(&data.artifacts())
        .expect_err("symlinked run root must fail reconciliation");
    assert!(
        format!("{error:#}").contains("symlink")
            || format!("{error:#}").contains("anchored")
            || format!("{error:#}").contains("not a directory"),
        "symlinked run root must be rejected, got: {error:#}"
    );
    assert!(
        std::fs::read_to_string(attacker.join("state.json")).expect("attacker state")
            == "attacker state",
        "attacker bytes must not be reconciled as the run"
    );

    // Restore the real run root and verify an identical retry still reconciles.
    std::fs::remove_file(&run_root).expect("remove symlink");
    std::fs::rename(&original, &run_root).expect("restore real root");
    store
        .create_run_staged(&data.artifacts())
        .expect("identical retry reconciles after restoring real root");
    let loaded = store
        .load_run(run_id)
        .expect("load")
        .expect("record present");
    assert_eq!(loaded.run_id, run_id);
}

fn sample_failure(run_id: &str, step_id: &str, attempt: u32) -> FailureRecord {
    FailureRecord {
        failure_id: format!("failure-{}-{}", step_id, attempt),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution_attempt: attempt,
        last_stable_step_id: None,
        failure_class: FailureClass::ToolError,
        error_code: "tool_execution_error".to_string(),
        message_redacted: "step failed for test".to_string(),
        adapter_kind: "process".to_string(),
        adapter_name: Some("shell".to_string()),
        tool_name: Some("exec".to_string()),
        retryable: true,
        safe_to_retry: true,
        requires_manual_attention: false,
        state_mutation_detected: false,
        first_seen_at_unix_ms: attempt as u64,
        last_seen_at_unix_ms: attempt as u64,
        occurrence_count: 1,
    }
}

#[test]
fn simultaneous_events_and_failures_truncation_repairs_and_stays_inspectable() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("dual-truncate", &["step".to_string(), "exec".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    // Two valid events and two valid failures establish the pre-crash streams.
    let events = LocalEventSink::new(LocalStorageConfig::new(tmp.path()));
    for i in 1..=2u64 {
        events
            .publish(&RuntimeEvent {
                kind: "step.started".to_string(),
                run_id: "dual-truncate".to_string(),
                step_id: Some("step".to_string()),
                timestamp_unix_ms: i,
                payload: transist_protocol::KernelValue::Null,
            })
            .expect("publish");
    }
    store
        .store_failure_record(&sample_failure("dual-truncate", "exec", 1))
        .expect("store failure 1");
    store
        .store_failure_record(&sample_failure("dual-truncate", "exec", 2))
        .expect("store failure 2");

    // Simultaneously truncate BOTH streams with a partial JSON fragment each.
    let run_root = resolve_run_root(tmp.path(), "dual-truncate").expect("run root");
    let events_path = run_root.join("events.jsonl");
    let failures_path = run_root.join("failures.jsonl");
    let mut events_raw = std::fs::read_to_string(&events_path).expect("read events");
    events_raw.push_str(r#"{"kind":"step.started","run_id":"dual"#);
    std::fs::write(&events_path, events_raw).expect("truncate events");
    let mut failures_raw = std::fs::read_to_string(&failures_path).expect("read failures");
    failures_raw.push_str(r#"{"failure_id":"truncat"#);
    std::fs::write(&failures_path, failures_raw).expect("truncate failures");

    // The failures append repairs BOTH streams: `repair_stream_and_note`
    // repairs the failures tail, then repairs the (differently) truncated
    // events tail before recording its `storage.tail_repaired` note, so the
    // note is never glued onto the truncated events fragment.
    store
        .store_failure_record(&sample_failure("dual-truncate", "exec", 3))
        .expect("store failure after simultaneous truncation");

    let inspection = LocalInspectionStore::new(tmp.path());
    let simultaneously_repaired = inspection
        .list_events("dual-truncate")
        .expect("list events after simultaneous repair");
    assert_eq!(
        simultaneously_repaired
            .iter()
            .filter(|e| e.event.kind == "storage.tail_repaired")
            .count(),
        2,
        "both simultaneous repairs must be auditable before any later repair"
    );

    // Truncate the events stream again, then publish: the events-tail repair
    // records a third, distinct audit event.
    let mut events_raw = std::fs::read_to_string(&events_path).expect("read events");
    events_raw.push_str(r#"{"kind":"step.started","run_id":"dual-trunc"#);
    std::fs::write(&events_path, events_raw).expect("truncate events again");
    events
        .publish(&RuntimeEvent {
            kind: "step.finished".to_string(),
            run_id: "dual-truncate".to_string(),
            step_id: Some("step".to_string()),
            timestamp_unix_ms: 99,
            payload: transist_protocol::KernelValue::Null,
        })
        .expect("publish after re-truncation");

    let inspection = LocalInspectionStore::new(tmp.path());
    let listed_events = inspection
        .list_events("dual-truncate")
        .expect("list events");
    let step_started = listed_events
        .iter()
        .filter(|e| e.event.kind == "step.started")
        .count();
    assert_eq!(
        step_started, 2,
        "both prior complete records survive simultaneous truncation"
    );
    assert!(
        listed_events
            .iter()
            .any(|e| e.event.kind == "step.finished"),
        "the post-repair event must be appended"
    );
    let repaired_notes = listed_events
        .iter()
        .filter(|e| e.event.kind == "storage.tail_repaired")
        .count();
    assert_eq!(
        repaired_notes, 3,
        "the two events-tail repairs and failures-tail repair each record \
         their own audit note; listed={listed_events:?}"
    );

    let listed_failures = inspection
        .list_failures("dual-truncate")
        .expect("list failures");
    assert_eq!(
        listed_failures.len(),
        3,
        "all three failure records survive simultaneous truncation"
    );
    // The truncated fragments must never leak through either inspection surface.
    assert!(
        listed_failures
            .iter()
            .all(|f| !f.failure_id.starts_with("truncat")),
        "no truncated failure fragment may be reported"
    );
}

#[test]
fn corrupt_run_state_is_an_error_never_reported_as_not_found() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    // A genuinely absent run is the distinct `Ok(None)` outcome.
    assert!(
        store
            .load_run("missing-run")
            .expect("load absent run")
            .is_none(),
        "absent run must be Ok(None), not an error"
    );

    // A corrupt state.json is an error, never `Ok(None)` (never "not found").
    let data = birth_data("corrupt-run", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);
    let run_root = resolve_run_root(tmp.path(), "corrupt-run").expect("run root");
    std::fs::write(run_root.join("state.json"), b"{ not json").expect("corrupt state");

    let err = store
        .load_run("corrupt-run")
        .expect_err("corrupt state must fail closed");
    assert!(
        format!("{err:#}").contains("invalid run state json"),
        "actual error: {err:#}"
    );
}

#[test]
fn corrupt_run_locator_is_an_error_never_reported_as_absent() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("corrupt-locator", &["step".to_string()]);
    store.create_run_staged(&data.artifacts()).expect("create");
    write_locator_for(tmp.path(), &data.record);

    let locator_path =
        transist_storage_local::run_paths::run_locator_path(tmp.path(), "corrupt-locator");
    std::fs::write(&locator_path, b"{ not json").expect("corrupt locator");

    let err = store
        .load_run("corrupt-locator")
        .expect_err("corrupt locator must fail closed");
    assert!(
        format!("{err:#}").contains("invalid run locator json"),
        "actual error: {err:#}"
    );
    let err = store
        .run_exists("corrupt-locator")
        .expect_err("corrupt locator must not report absence");
    assert!(
        format!("{err:#}").contains("invalid run locator json"),
        "actual error: {err:#}"
    );
}

#[test]
fn loading_state_with_invalid_activation_baseline_fails_closed() {
    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let data = birth_data("bad-baseline", &["step".to_string()]);
    let artifacts = data.artifacts();
    store.create_run_staged(&artifacts).expect("create");
    write_locator_for(tmp.path(), &data.record);

    // A persisted baseline above the lifetime execution count is invalid state
    // and must be rejected on load, not saturated to zero.
    let run_root = resolve_run_root(tmp.path(), "bad-baseline").expect("run root");
    let state_path = run_root.join("state.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read state"))
            .expect("parse state");
    value["steps"]["step"]["executions"] = serde_json::json!(2);
    value["steps"]["step"]["activation_executions_baseline"] = serde_json::json!(7);
    std::fs::write(
        &state_path,
        serde_json::to_string_pretty(&value).expect("json"),
    )
    .expect("write invalid baseline state");

    let err = store
        .load_run("bad-baseline")
        .expect_err("invalid activation baseline must fail closed");
    assert!(
        format!("{err:#}").contains("activation execution baseline"),
        "actual error: {err:#}"
    );
}

#[cfg(unix)]
#[test]
fn birth_recovery_rejects_birth_file_swapped_for_symlink() {
    use std::os::unix::fs::symlink;

    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let run_id = "birth-file-symlink";
    let data = birth_data(run_id, &["step".to_string()]);
    store
        .create_run_staged(&data.artifacts())
        .expect("first create");
    write_locator_for(tmp.path(), &data.record);

    // Swap the birth file for a symlink pointing at attacker bytes. The
    // anchored read must reject the symlink (never follow it), so the retry
    // fails closed instead of reconciling attacker bytes as the birth file.
    let run_root = resolve_run_root(tmp.path(), run_id).expect("run root");
    let recipe_path = run_root.join("recipe.yaml");
    let outside = tmp.path().join("outside-recipe.yaml");
    std::fs::write(&outside, b"attacker-controlled").expect("outside");
    std::fs::remove_file(&recipe_path).expect("remove recipe");
    symlink(&outside, &recipe_path).expect("swap in symlink");

    assert!(
        store.create_run_staged(&data.artifacts()).is_err(),
        "a birth file swapped for a symlink must be rejected"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).expect("outside"),
        "attacker-controlled",
        "the symlink target must never be read as a birth artifact"
    );
}

#[cfg(unix)]
#[test]
fn birth_recovery_rejects_birth_parent_swapped_for_symlink() {
    use std::os::unix::fs::symlink;

    let _zone = NoFaultZone::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let run_id = "birth-parent-symlink";
    let data = birth_data(run_id, &["step".to_string()]);
    store
        .create_run_staged(&data.artifacts())
        .expect("first create");
    write_locator_for(tmp.path(), &data.record);

    // Swap the run root (the parent of the birth files) for a symlink. The
    // anchored parent open must reject the symlink, so reconciliation fails
    // closed rather than descending through the redirect.
    let run_root = resolve_run_root(tmp.path(), run_id).expect("run root");
    let outside = tmp.path().join("outside-run-root");
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::fs::write(outside.join("state.json"), b"attacker-state").expect("outside state");
    let root_parent = run_root.parent().expect("run root parent").to_path_buf();
    let run_dir_name = run_root.file_name().expect("run dir name").to_owned();
    let swapped_root = root_parent.join(run_dir_name);
    std::fs::remove_dir_all(&swapped_root).expect("remove run root");
    symlink(&outside, &swapped_root).expect("swap in symlink");

    assert!(
        store.create_run_staged(&data.artifacts()).is_err(),
        "a birth parent swapped for a symlink must be rejected"
    );
    assert!(
        std::fs::symlink_metadata(&swapped_root)
            .expect("swapped root metadata")
            .file_type()
            .is_symlink(),
        "the swapped symlink must remain"
    );
}
