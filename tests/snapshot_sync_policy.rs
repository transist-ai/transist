// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;

use transist::{
    InspectionSnapshotBuilder, InspectionSnapshotRequest, SyncPolicyConfig, SyncPolicyMode,
    TransistInspectionContext, TransistMetricsContext,
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
fn metadata_only_filters_event_payloads_and_omits_workflow_values() {
    let (inspection, metrics) = sample_store(
        "run-meta",
        serde_json::json!({"status": "ok", "custom": "value"}),
        "boom",
    );
    let request = base_request("run-meta");

    let snapshot = InspectionSnapshotBuilder::new(&inspection, &metrics)
        .build(&request)
        .expect("build snapshot")
        .expect("snapshot");

    assert_eq!(
        snapshot.sync_policy.expect("sync policy").mode,
        SyncPolicyMode::MetadataOnly
    );

    let run = &snapshot.runs[0];
    assert!(run.run_detail_summary.workflow_input_values.is_none());
    assert!(run.run_detail_summary.run_detail_full.is_none());
    match &run.events[0].summary {
        transist::KernelValue::Object(summary) => {
            assert_eq!(
                summary.get("status"),
                Some(&transist::KernelValue::String("ok".to_string()))
            );
            assert!(summary.get("custom").is_none());
        }
        other => panic!("unexpected event summary: {other:?}"),
    }
}

#[test]
fn extended_policy_includes_workflow_values_and_event_payloads() {
    let (inspection, metrics) = sample_store(
        "run-extended",
        serde_json::json!({"status": "ok", "custom": "value"}),
        "boom",
    );
    let mut request = base_request("run-extended");
    request.sync_policy = Some(SyncPolicyConfig {
        mode: SyncPolicyMode::Extended,
        include_workflow_input_values: true,
        include_event_payloads: true,
        include_run_detail_full: true,
        include_stderr_tail: false,
        include_gate_request_summary: false,
    });

    let snapshot = InspectionSnapshotBuilder::new(&inspection, &metrics)
        .build(&request)
        .expect("build snapshot")
        .expect("snapshot");

    let run = &snapshot.runs[0];
    assert_eq!(
        run.run_detail_summary.workflow_input_values,
        Some(BTreeMap::from([(
            "ticket".to_string(),
            serde_json::json!("TICKET-123").into(),
        )]))
    );
    assert!(run.run_detail_summary.run_detail_full.is_some());
    match &run.events[0].summary {
        transist::KernelValue::Object(summary) => {
            assert_eq!(
                summary.get("custom"),
                Some(&transist::KernelValue::String("value".to_string()))
            );
            assert_eq!(
                summary.get("status"),
                Some(&transist::KernelValue::String("ok".to_string()))
            );
        }
        other => panic!("unexpected event summary: {other:?}"),
    }
}

#[test]
fn extended_policy_still_rejects_forbidden_keys() {
    let (inspection, metrics) = sample_store(
        "run-forbidden",
        serde_json::json!({"nested": {"prompt": "secret"}}),
        "boom",
    );
    let mut request = base_request("run-forbidden");
    request.sync_policy = Some(SyncPolicyConfig {
        mode: SyncPolicyMode::Extended,
        include_workflow_input_values: true,
        include_event_payloads: true,
        include_run_detail_full: true,
        include_stderr_tail: true,
        include_gate_request_summary: true,
    });

    let error = InspectionSnapshotBuilder::new(&inspection, &metrics)
        .build(&request)
        .expect_err("forbidden key should fail");

    assert!(error.to_string().contains("metadata contract"));
}

fn sample_store(
    run_id: &str,
    event_payload: serde_json::Value,
    stderr: &str,
) -> (TransistInspectionContext, TransistMetricsContext) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.keep();
    let store = LocalRunStore::new(LocalStorageConfig::new(&home_dir));
    let summaries = LocalRunSummaryStore::new(&home_dir);
    let events = LocalEventSink::new(LocalStorageConfig::new(&home_dir));

    let result = sample_result(run_id, "step", "success", stderr);
    let record = sample_record(
        run_id,
        BTreeMap::from([(
            "ticket".to_string(),
            transist::KernelValue::String("TICKET-123".to_string()),
        )]),
        Some(result.clone()),
    );

    store.create_run(&record).expect("create run");
    persist_execution(&store, run_id, "step", result);
    summaries.refresh_run(run_id).expect("refresh summary");
    persist_event(
        &events,
        &summaries,
        runtime_event(run_id, 1, "step_finished", event_payload),
    );

    (
        TransistInspectionContext::with_home(&home_dir, &home_dir),
        TransistMetricsContext::with_home(&home_dir),
    )
}

fn base_request(run_id: &str) -> InspectionSnapshotRequest {
    InspectionSnapshotRequest {
        run_id: run_id.to_string(),
        source_id: "src_example".to_string(),
        workspace_id: "example/workflows".to_string(),
        operator_label: None,
        produced_at_unix_ms: Some(5_000),
        tree_version: 1,
        event_after_by_run: BTreeMap::new(),
        max_payload_bytes: Some(transist::DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES),
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
    workflow_inputs: BTreeMap<String, transist::KernelValue>,
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
        payload: transist::KernelValue::from_json(payload),
    }
}

fn persist_event(sink: &LocalEventSink, summaries: &LocalRunSummaryStore, event: RuntimeEvent) {
    sink.publish(&event).expect("publish event");
    summaries.note_event(&event).expect("note event");
}
