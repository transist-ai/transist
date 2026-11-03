// SPDX-License-Identifier: MPL-2.0

use assert_cmd::cargo::CommandCargoExt;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;
use transist::{
    InspectionSnapshotEnvelope, KernelValue, SyncPolicyMode, TransistInspectionContext,
    TransistMetricsContext, validate_snapshot_contract,
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
fn snapshot_export_stdout_is_valid_metadata_only_json() {
    let repo = SnapshotRepo::new(
        "run-meta",
        serde_json::json!({"status": "ok", "custom": "value"}),
        "boom",
    );
    let output = repo.run_cli(&[
        "runs",
        "snapshot",
        "export",
        "run-meta",
        "--source-id",
        "src_example",
        "--workspace-id",
        "example/workflows",
    ]);

    assert!(
        output.status.success(),
        "snapshot export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: InspectionSnapshotEnvelope =
        serde_json::from_slice(&output.stdout).expect("parse snapshot stdout");
    validate_snapshot_contract(&snapshot).expect("validate snapshot contract");
    assert_eq!(snapshot.root_run_id, "run-meta");
    assert_eq!(
        snapshot.sync_policy.as_ref().expect("sync policy").mode,
        SyncPolicyMode::MetadataOnly
    );
    assert!(
        snapshot.runs[0]
            .run_detail_summary
            .workflow_input_values
            .is_none()
    );
    match &snapshot.runs[0].events[0].summary {
        KernelValue::Object(event_summary) => {
            assert!(event_summary.get("status").is_some());
            assert!(event_summary.get("custom").is_none());
        }
        other => panic!("unexpected event summary: {other:?}"),
    }
}

#[test]
fn snapshot_export_file_can_include_extended_fields() {
    let repo = SnapshotRepo::new(
        "run-extended",
        serde_json::json!({"status": "ok", "custom": "value"}),
        "boom",
    );
    let output_path = repo.root.path().join("exports/snapshot.json");
    let output = repo.run_cli(&[
        "runs",
        "snapshot",
        "export",
        "run-extended",
        "--source-id",
        "src_example",
        "--workspace-id",
        "example/workflows",
        "--tree-version",
        "7",
        "--output",
        output_path.to_str().expect("output path utf8"),
        "--sync-mode",
        "extended",
        "--include-workflow-input-values",
        "--include-event-payloads",
        "--include-run-detail-full",
        "--include-stderr-tail",
        "--include-gate-request-summary",
    ]);

    assert!(
        output.status.success(),
        "snapshot export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let export_meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse export metadata");
    assert_eq!(export_meta["output"], output_path.display().to_string());

    let snapshot: InspectionSnapshotEnvelope =
        serde_json::from_str(&fs::read_to_string(&output_path).expect("read snapshot file"))
            .expect("parse snapshot file");
    validate_snapshot_contract(&snapshot).expect("validate snapshot contract");
    assert_eq!(snapshot.tree_version, 7);
    let policy = snapshot.sync_policy.as_ref().expect("sync policy");
    assert_eq!(policy.mode, SyncPolicyMode::Extended);
    assert!(policy.include_workflow_input_values);
    assert!(policy.include_event_payloads);
    assert!(
        snapshot.runs[0]
            .run_detail_summary
            .workflow_input_values
            .is_some()
    );
    assert!(
        snapshot.runs[0]
            .run_detail_summary
            .run_detail_full
            .is_some()
    );
    match &snapshot.runs[0].events[0].summary {
        KernelValue::Object(event_summary) => {
            assert_eq!(
                event_summary.get("custom"),
                Some(&KernelValue::String("value".to_string()))
            );
        }
        other => panic!("unexpected event summary: {other:?}"),
    }
}

#[test]
fn snapshot_export_missing_run_fails() {
    let repo = SnapshotRepo::new("run-present", serde_json::json!({"status": "ok"}), "boom");
    let output = repo.run_cli(&[
        "runs",
        "snapshot",
        "export",
        "run-missing",
        "--source-id",
        "src_example",
        "--workspace-id",
        "example/workflows",
    ]);

    assert!(
        !output.status.success(),
        "missing run unexpectedly succeeded"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        combined.contains("run 'run-missing' was not found"),
        "unexpected output: {combined}"
    );
}

#[test]
fn snapshot_export_rejects_tiny_max_payload() {
    let repo = SnapshotRepo::new(
        "run-max",
        serde_json::json!({"status": "ok", "custom": "value"}),
        "boom",
    );
    let output = repo.run_cli(&[
        "runs",
        "snapshot",
        "export",
        "run-max",
        "--source-id",
        "src_example",
        "--workspace-id",
        "example/workflows",
        "--max-payload-bytes",
        "64",
    ]);

    assert!(
        !output.status.success(),
        "tiny max payload unexpectedly succeeded"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        combined.contains("max_payload_bytes"),
        "unexpected output: {combined}"
    );
}

struct SnapshotRepo {
    root: TempDir,
    home_dir: PathBuf,
}

impl SnapshotRepo {
    fn new(run_id: &str, event_payload: serde_json::Value, stderr: &str) -> Self {
        let root = TempDir::new().expect("tempdir");
        let home_dir = root.path().join(".transist");
        transist::init_home_dir(&home_dir).expect("init home");

        let store = LocalRunStore::new(LocalStorageConfig::new(&home_dir));
        let summaries = LocalRunSummaryStore::new(&home_dir);
        let events = LocalEventSink::new(LocalStorageConfig::new(&home_dir));

        let result = sample_result(run_id, "step", "success", stderr);
        let record = sample_record(
            run_id,
            BTreeMap::from([(
                "ticket".to_string(),
                KernelValue::String("TICKET-123".to_string()),
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

        let inspection = TransistInspectionContext::with_home(root.path(), &home_dir);
        let metrics = TransistMetricsContext::with_home(&home_dir);
        let snapshot = transist::InspectionSnapshotBuilder::new(&inspection, &metrics)
            .build(&transist::InspectionSnapshotRequest {
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
            })
            .expect("build snapshot")
            .expect("snapshot");
        validate_snapshot_contract(&snapshot).expect("validate fixture snapshot");

        Self { root, home_dir }
    }

    fn run_cli(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::cargo_bin("transist").expect("transist binary");
        cmd.arg("--home").arg(&self.home_dir);
        cmd.current_dir(self.root.path());
        cmd.args(args);
        cmd.output().expect("run transist")
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

fn persist_event(events: &LocalEventSink, summaries: &LocalRunSummaryStore, event: RuntimeEvent) {
    events.publish(&event).expect("publish event");
    summaries.note_event(&event).expect("note event");
}
