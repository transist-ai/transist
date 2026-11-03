// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;

use transist::{
    DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES, INSPECTION_SNAPSHOT_SCHEMA_VERSION,
    InspectionSnapshotBuilder, InspectionSnapshotRequest, KernelValue, TransistInspectionContext,
    TransistMetricsContext,
};
use transist_core::ports::{EventPort, RunStorePort, RunStoreStepExecution};
use transist_core::{
    KernelRunSnapshot, KernelRunStatus, KernelStepState, KernelStepStatus, PersistedRunRecord,
};
use transist_protocol::{
    ExecutionRequest, ExecutionResult, RuntimeEvent, TEP_INVOKE_REQUEST_TYPE_V1,
    TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
};
use transist_storage_local::{
    LocalEventSink, LocalRunStore, LocalRunSummaryStore, LocalStorageConfig,
};

#[test]
fn snapshot_uses_run_detail_summary_without_shipping_input_values() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path();
    let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    let summaries = LocalRunSummaryStore::new(home_dir);
    let events = LocalEventSink::new(LocalStorageConfig::new(home_dir));

    let result = sample_result(
        "safe-run",
        "step",
        "success",
        "stderr with [REDACTED]\nPROMPT TEXT",
    );
    let record = sample_record(
        "safe-run",
        BTreeMap::from([
            (
                "prompt".to_string(),
                KernelValue::String("PROMPT TEXT".to_string()),
            ),
            (
                "ticket".to_string(),
                KernelValue::String("TICKET-123: customer issue details".to_string()),
            ),
        ]),
        Some(result.clone()),
    );

    store.create_run(&record).expect("create run");
    persist_execution(&store, "safe-run", "step", result);
    summaries.refresh_run("safe-run").expect("refresh summary");
    persist_event(
        &events,
        &summaries,
        runtime_event(
            "safe-run",
            1,
            "step_finished",
            serde_json::json!({"kind": "step_finished"}),
        ),
    );

    let inspection = TransistInspectionContext::with_home(home_dir, home_dir);
    let metrics = TransistMetricsContext::with_home(home_dir);
    let snapshot = InspectionSnapshotBuilder::new(&inspection, &metrics)
        .build(&sample_request("safe-run"))
        .expect("build snapshot")
        .expect("snapshot");

    assert_eq!(snapshot.schema_version, INSPECTION_SNAPSHOT_SCHEMA_VERSION);

    let snapshot_json = serde_json::to_value(&snapshot).expect("snapshot json");
    let run = &snapshot_json["runs"][0];
    assert!(run.get("run_detail").is_none());
    assert_eq!(
        run["run_detail_summary"]["input_keys"],
        serde_json::json!(["prompt", "ticket"])
    );

    let rendered = serde_json::to_string(&snapshot_json).expect("serialize snapshot json");
    assert!(!rendered.contains("TICKET-123: customer issue details"));
    assert!(!rendered.contains("PROMPT TEXT"));
}

#[test]
fn no_stderr_text_leaks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path();
    let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    let summaries = LocalRunSummaryStore::new(home_dir);
    let events = LocalEventSink::new(LocalStorageConfig::new(home_dir));

    let result = sample_result(
        "failed-run",
        "step",
        "failed",
        "TICKET-123: customer email leaked",
    );
    let record = sample_record("failed-run", BTreeMap::new(), Some(result.clone()));

    store.create_run(&record).expect("create run");
    persist_execution(&store, "failed-run", "step", result);
    summaries
        .refresh_run("failed-run")
        .expect("refresh summary");
    persist_event(
        &events,
        &summaries,
        runtime_event(
            "failed-run",
            1,
            "step_finished",
            serde_json::json!({"kind": "step_finished", "status": "failed"}),
        ),
    );

    let inspection = TransistInspectionContext::with_home(home_dir, home_dir);
    let metrics = TransistMetricsContext::with_home(home_dir);
    let snapshot = InspectionSnapshotBuilder::new(&inspection, &metrics)
        .build(&sample_request("failed-run"))
        .expect("build snapshot")
        .expect("snapshot");

    let rendered = serde_json::to_string(&snapshot).expect("serialize snapshot");
    assert!(!rendered.contains("TICKET-123: customer email leaked"));
}

#[test]
fn snapshot_filters_event_summaries_to_allowlisted_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path();
    let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    let summaries = LocalRunSummaryStore::new(home_dir);
    let events = LocalEventSink::new(LocalStorageConfig::new(home_dir));

    let record = sample_record("unsafe-run", BTreeMap::new(), None);
    store.create_run(&record).expect("create run");
    summaries
        .refresh_run("unsafe-run")
        .expect("refresh summary");

    persist_event(
        &events,
        &summaries,
        runtime_event(
            "unsafe-run",
            1,
            "step_finished",
            serde_json::json!({"stdout": "secret", "status": "ok"}),
        ),
    );

    let inspection = TransistInspectionContext::with_home(home_dir, home_dir);
    let metrics = TransistMetricsContext::with_home(home_dir);
    let snapshot = InspectionSnapshotBuilder::new(&inspection, &metrics)
        .build(&sample_request("unsafe-run"))
        .expect("build snapshot")
        .expect("snapshot");

    assert_eq!(
        snapshot.runs[0].events[0].summary,
        KernelValue::Object(BTreeMap::from([(
            "status".to_string(),
            KernelValue::String("ok".to_string()),
        )]))
    );
}

fn sample_request(run_id: &str) -> InspectionSnapshotRequest {
    InspectionSnapshotRequest {
        run_id: run_id.to_string(),
        source_id: "src_example".to_string(),
        workspace_id: "example/workflows".to_string(),
        operator_label: None,
        produced_at_unix_ms: Some(5_000),
        tree_version: 1,
        event_after_by_run: BTreeMap::new(),
        max_payload_bytes: Some(DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES),
        sync_policy: None,
        extensions: BTreeMap::new(),
        provider_facts: BTreeMap::new(),
        policy_facts: BTreeMap::new(),
        runtime_id: None,
        organization_id: None,
        policy_revision: None,
        trusted_execution_mode: None,
        workflow_digest: None,
        workflow_signature_status: None,
        workflow_signer_key_id: None,
        plugin_trust: None,
        trust_decision: None,
        external_transfer_summary: None,
        approver_identity: None,
        locality: None,
        evidence_export: None,
    }
}

fn sample_record(
    run_id: &str,
    workflow_inputs: BTreeMap<String, KernelValue>,
    last_result: Option<ExecutionResult>,
) -> PersistedRunRecord {
    let mut snapshot = KernelRunSnapshot::new(run_id, "wf", "v1", &["step".to_string()]);
    snapshot.status = KernelRunStatus::Success;
    snapshot.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: Some(sample_request_payload(run_id, "step")),
            last_result,
            started_at_ms: Some(100),
            finished_at_ms: Some(200),
            activation_executions_baseline: 0,
        },
    );

    PersistedRunRecord::from_snapshot(snapshot, None, None, workflow_inputs, 0, 100, 200)
}

fn sample_request_payload(run_id: &str, step_id: &str) -> ExecutionRequest {
    serde_json::from_value(serde_json::json!({
        "type": TEP_INVOKE_REQUEST_TYPE_V1,
        "protocol_version": TEP_PROTOCOL_VERSION_V1,
        "invoke_id": format!("invoke_{run_id}_{step_id}"),
        "run_id": run_id,
        "workflow_id": "wf",
        "step": {"id": step_id, "execution": 1},
        "adapter_kind": "process",
        "action": "exec",
        "input": {"safe": true},
        "paths": {
            "run_root": format!("/tmp/runs/{run_id}"),
            "artifacts_dir": format!("/tmp/runs/{run_id}/artifacts"),
            "evidence_dir": format!("/tmp/runs/{run_id}/evidence"),
            "workspace_dir": "/tmp/workspace"
        },
        "timeout_ms": null,
        "max_timeout_ms": null,
        "policy": null,
        "metadata": null
    }))
    .expect("request")
}

fn sample_result(run_id: &str, step_id: &str, status: &str, stderr: &str) -> ExecutionResult {
    serde_json::from_value(serde_json::json!({
        "type": TEP_INVOKE_RESULT_TYPE_V1,
        "protocol_version": TEP_PROTOCOL_VERSION_V1,
        "invoke_id": format!("invoke_{run_id}_{step_id}"),
        "run_id": run_id,
        "step_id": step_id,
        "execution": 1,
        "status": status,
        "stdout": "",
        "stderr": stderr,
        "timing": {
            "started_at": "0",
            "finished_at": "1",
            "duration_ms": 25
        },
        "artifacts": [],
        "evidence": [],
        "metrics": {"latency_ms": 25.0},
        "output": {},
        "usage": null,
        "error": null
    }))
    .expect("result")
}

fn persist_execution(store: &LocalRunStore, run_id: &str, step_id: &str, result: ExecutionResult) {
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
            execution: 1,
            state: serde_json::json!({
                "run_id": run_id,
                "step_id": step_id,
                "execution": 1,
                "status": result.status,
                "started_at_ms": 100,
                "finished_at_ms": 200,
                "request": sample_request_payload(run_id, step_id),
                "result": result,
                "metrics_validation_errors": null
            })
            .into(),
        })
        .expect("persist execution");
}

fn runtime_event(run_id: &str, seq: u64, kind: &str, payload: serde_json::Value) -> RuntimeEvent {
    RuntimeEvent {
        kind: kind.to_string(),
        run_id: run_id.to_string(),
        step_id: Some("step".to_string()),
        timestamp_unix_ms: 1_000 + seq,
        payload: KernelValue::from_json(payload),
    }
}

fn persist_event(sink: &LocalEventSink, summaries: &LocalRunSummaryStore, event: RuntimeEvent) {
    sink.publish(&event).expect("publish event");
    summaries.note_event(&event).expect("note event");
}
