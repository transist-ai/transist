// SPDX-License-Identifier: MPL-2.0
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;
use transist_core::ports::{EventPort, RunStorePort, RunStoreStepExecution};
use transist_core::{FailureClass, FailureRecord};
use transist_core::{
    KernelRunSnapshot, KernelRunStatus, KernelStepState, KernelStepStatus,
    PersistedPreflightRecord, PersistedRunRecord, PreflightCheck, PreflightStatus,
    RUN_PROVENANCE_SCHEMA_V1, RecipeSnapshotRef, RunApprovalProvenance, RunExternalTransferSummary,
    RunLineage, RunLineageMode, RunProvenanceDenied, RunProvenancePolicyDecision,
    RunProvenanceRecord, RunProvenanceSubject, StepExecutionRecord, TrustDecisionRecord,
    TrustSubjectDecision,
};
use transist_protocol::{InvokeUsage, KernelValue, RuntimeEvent};
use transist_server::{app_for_paths, app_with_query};
use transist_storage_local::{
    LocalEventSink, LocalRunStore, LocalRunSummaryStore, LocalStorageConfig,
};

const TS_CREATED: u64 = 1_735_689_600_000;
const TS_COLLECT_STARTED: u64 = 1_735_689_601_000;
const TS_COLLECT_FINISHED: u64 = 1_735_689_602_000;
const TS_GATE_STARTED: u64 = 1_735_689_603_000;
const TS_UPDATED: u64 = 1_735_689_604_000;

#[path = "support/step_outputs_api.rs"]
mod step_outputs_api;

#[path = "support/step_outputs_ui.rs"]
mod step_outputs_ui;

#[path = "support/step_outputs_preview.rs"]
mod step_outputs_preview;

#[path = "support/step_outputs_end_to_end.rs"]
mod step_outputs_end_to_end;

#[path = "support/document_reviews.rs"]
mod document_reviews;

fn test_tempdir() -> tempfile::TempDir {
    // macOS exposes its system temporary directory through symlinked `/var`.
    // Storage tests intentionally reject such anchors, so keep fixtures beneath
    // the physical workspace tree.
    let root = std::env::current_dir()
        .expect("current directory")
        .join("target/test-tmp");
    fs::create_dir_all(&root).expect("create test temp root");
    tempfile::tempdir_in(root).expect("tempdir")
}

#[tokio::test]
async fn health_endpoint_reports_read_only_server_status() {
    let fixture = Fixture::new();
    let response = fixture.request("GET", "/health", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(
        body,
        json!({
            "status": "ok",
            "service": "transist-server",
            "mode": "read_only",
            "api_version": "v1"
        })
    );
}

#[tokio::test]
async fn workflow_file_metadata_survives_without_workspace_authority_but_content_is_denied() {
    let fixture = Fixture::new();
    let query = Arc::new(transist::TransistInspectionContext::with_home(
        fixture.repo_root.clone(),
        fixture.home_dir.clone(),
    ));
    let app = app_with_query(query);
    let steps = read_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/runs/inspect-run/steps")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response"),
    )
    .await;
    let workflow_file_id = steps["data"][0]["output_summary"]["workflow_files"][0]
        .get("workflow_file_id")
        .and_then(Value::as_str)
        .expect("host-independent workflow file id");

    let content = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}/content"
                ))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(content.status(), StatusCode::NOT_FOUND);
}

#[cfg(unix)]
#[tokio::test]
async fn workflow_file_content_rejects_directory_symlink_swap_outside_workspace() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/steps", Body::empty())
            .await,
    )
    .await;
    let workflow_file_id =
        steps["data"][0]["output_summary"]["workflow_files"][0]["workflow_file_id"]
            .as_str()
            .expect("workflow file id");
    let reports = fixture.repo_root.join("server_demo/reports");
    let outside = fixture.repo_root.parent().unwrap().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::create_dir(outside.join("nested")).expect("outside nested directory");
    fs::write(outside.join("nested/collect.md"), b"secret").expect("outside content");
    fs::remove_dir_all(&reports).expect("remove reports directory");
    symlink(&outside, &reports).expect("replace reports with symlink");

    let response = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_ne!(read_body(response).await, b"secret".to_vec());
}

#[tokio::test]
async fn stale_workflow_file_locator_is_not_retrievable() {
    let fixture = Fixture::new();
    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/steps", Body::empty())
            .await,
    )
    .await;
    let workflow_file_id =
        steps["data"][0]["output_summary"]["workflow_files"][0]["workflow_file_id"]
            .as_str()
            .expect("workflow file id");
    fs::remove_file(
        fixture
            .repo_root
            .join("server_demo/reports/nested/collect.md"),
    )
    .expect("remove workflow file");

    let response = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn repo_workflow_file_uses_persisted_relative_repo_root_not_workspace_shadow() {
    let fixture = Fixture::new();
    let repository = fixture.repo_root.join("project");
    fs::create_dir_all(repository.join(".transist")).expect("repository state");
    fs::create_dir_all(fixture.repo_root.join(".transist")).expect("workspace shadow state");
    fs::write(
        repository.join(".transist/spec.md"),
        b"requested repository",
    )
    .expect("repository file");
    fs::write(
        fixture.repo_root.join(".transist/spec.md"),
        b"wrong root shadow",
    )
    .expect("shadow file");
    replace_collect_workflow_file_request(
        &fixture,
        Value::String("project".to_string()),
        "repo://spec.md",
    );

    let (locator_kind, content) = read_only_workflow_file(&fixture, "spec_files").await;
    assert_eq!(locator_kind, "repository_state");
    assert_eq!(content, b"requested repository");
}

#[tokio::test]
async fn repo_workflow_file_rejects_persisted_absolute_repo_root() {
    let fixture = Fixture::new();
    let repository = fixture
        .repo_root
        .parent()
        .unwrap()
        .join("external-repository");
    fs::create_dir_all(repository.join(".transist")).expect("repository state");
    fs::write(repository.join(".transist/spec.md"), b"absolute repository")
        .expect("repository file");
    replace_collect_workflow_file_request(
        &fixture,
        Value::String(repository.display().to_string()),
        "repo://spec.md",
    );

    let response = fixture
        .request(
            "GET",
            "/api/v1/runs/inspect-run/steps/collect",
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert!(
        body["output_summary"]["workflow_files"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
}

#[tokio::test]
async fn child_run_workflow_file_reads_from_lineage_root() {
    let mut fixture = Fixture::with_nested_subflow_runs();
    let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
    let child_run_id = "inspect-run__plan_loop";
    let child_root =
        transist_storage_local::run_paths::resolve_run_root(&fixture.home_dir, child_run_id)
            .expect("child root");
    let root =
        transist_storage_local::run_paths::resolve_run_root(&fixture.home_dir, "inspect-run")
            .expect("lineage root");
    fs::write(root.join("lineage-plan.md"), b"lineage root file").expect("root file");
    fs::write(child_root.join("lineage-plan.md"), b"wrong child shadow").expect("child shadow");

    let mut request = sample_collect_request();
    request.run_id = child_run_id.to_string();
    request.step.id = "child".to_string();
    request.paths.run_root = child_root.display().to_string();
    request.paths.workspace_dir = fixture.repo_root.display().to_string();
    request.input = json!({"response_file": "run://lineage-plan.md"});
    let mut result = sample_collect_result();
    result.run_id = child_run_id.to_string();
    result.step_id = "child".to_string();
    let mut record = run_store
        .load_run(child_run_id)
        .expect("load child")
        .expect("child run");
    let step = record.steps.get_mut("child").expect("child step");
    step.last_request = Some(request);
    step.last_result = Some(result);
    run_store
        .update_run_state(child_run_id, &record)
        .expect("update child state");
    fixture.app = app_for_paths(fixture.repo_root.clone(), fixture.home_dir.clone());

    let steps = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/inspect-run__plan_loop/steps",
                Body::empty(),
            )
            .await,
    )
    .await;
    let file_id = steps["data"][0]["output_summary"]["workflow_files"][0]["workflow_file_id"]
        .as_str()
        .expect("workflow file id");
    let response = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/{child_run_id}/workflow-files/{file_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_body(response).await, b"lineage root file");
}

#[tokio::test]
async fn run_detail_reports_the_actual_latest_event_id() {
    let fixture = Fixture::new();

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["event_summary"]["count"], 3);
    assert_eq!(run["event_summary"]["latest_event_id"], "3");

    let latest_event = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/inspect-run/events?order=desc&limit=1",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(latest_event["data"][0]["event_id"], "3");
}

#[tokio::test]
async fn legacy_run_summary_counts_are_derived_without_rewriting_the_sidecar() {
    let fixture = Fixture::new();
    let summary_path = fixture.home_dir.join("runs/run_inspect-run/summary.json");
    let original = fs::read(&summary_path).expect("summary bytes");
    let mut legacy: Value = serde_json::from_slice(&original).expect("summary json");
    let object = legacy.as_object_mut().expect("summary object");
    object.remove("artifact_count");
    object.remove("evidence_count");
    object.remove("event_count");
    let legacy_bytes = serde_json::to_vec_pretty(&legacy).expect("legacy summary json");
    fs::write(&summary_path, &legacy_bytes).expect("write legacy summary");

    let runs = read_json(fixture.request("GET", "/api/v1/runs", Body::empty()).await).await;
    assert_eq!(runs["data"][0]["artifact_count"], 1);
    assert_eq!(runs["data"][0]["evidence_count"], 2);
    assert_eq!(runs["data"][0]["has_artifacts"], true);
    assert_eq!(runs["data"][0]["has_events"], true);

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["artifact_summary"]["count"], 1);
    assert_eq!(run["evidence_summary"]["count"], 2);
    assert_eq!(run["event_summary"]["count"], 3);
    assert_eq!(
        fs::read(&summary_path).expect("summary after read"),
        legacy_bytes
    );
}

#[tokio::test]
async fn read_endpoints_expose_richer_steps_and_resource_specific_ids() {
    let fixture = Fixture::new();

    let runs = read_json(fixture.request("GET", "/api/v1/runs", Body::empty()).await).await;
    assert_eq!(runs["data"][0]["run_id"], "inspect-run");
    assert_eq!(runs["data"][0]["status"], "blocked");
    assert_eq!(runs["data"][0]["has_blocked_gates"], true);
    assert_eq!(runs["data"][0]["has_artifacts"], true);
    assert_eq!(runs["data"][0]["has_events"], true);
    assert_eq!(runs["data"][0]["metrics"]["total_tokens"], 18);

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["run_id"], "inspect-run");
    assert_eq!(run["workflow_id"], "server_demo");
    assert_eq!(run["current_step"]["step_id"], "review");
    assert_eq!(run["artifact_summary"]["count"], 1);
    assert_eq!(run["evidence_summary"]["count"], 2);
    assert_eq!(run["metrics"]["total_step_executions"], 2);
    assert_eq!(run["metrics"]["total_tokens"], 18);
    assert_eq!(run["metrics"]["usage_breakdown"][0]["provider"], "openai");
    assert_eq!(run["trusted_execution_mode"], true);
    assert_eq!(run["trust"]["workflow"]["digest"], "workflow-digest-123");
    assert_eq!(run["trust"]["workflow"]["signature_status"], "verified");
    assert_eq!(run["trust"]["decision"]["outcome"], "verified");
    assert_eq!(run["locality"]["steps"][0]["provider"], "openai");
    assert_eq!(run["locality"]["steps"][0]["provider_kind"], "external");
    assert_eq!(
        run["locality"]["external_transfers"][0]["destination"],
        "api.openai.com"
    );
    assert_eq!(
        run["evidence_export"]["command"],
        "transist runs evidence export inspect-run"
    );
    // Run-detail metrics expose the consolidated timer trio: wall clock
    // covers the run's outer window, active is the summed step execution
    // time, and blocked-wait is omitted while the gate is still open
    // (review step is blocked and has no resolution timestamp yet).
    assert_eq!(run["metrics"]["wall_clock_duration_ms"], 1000);
    assert_eq!(run["metrics"]["active_execution_duration_ms"], 1000);
    assert!(
        run["metrics"]["blocked_wait_duration_ms"].is_null(),
        "blocked_wait_duration_ms should be omitted while the gate is open, got {:?}",
        run["metrics"]["blocked_wait_duration_ms"]
    );
    // The run-list summary mirrors the same fields so the runs index page
    // can render `wall clock / active / blocked` without round-tripping
    // to per-run detail.
    assert_eq!(runs["data"][0]["metrics"]["wall_clock_duration_ms"], 1000);
    assert_eq!(
        runs["data"][0]["metrics"]["active_execution_duration_ms"],
        1000
    );
    assert!(
        runs["data"][0]["metrics"]["blocked_wait_duration_ms"].is_null(),
        "run-list blocked_wait_duration_ms should be omitted while the gate is open, got {:?}",
        runs["data"][0]["metrics"]["blocked_wait_duration_ms"]
    );

    let events = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/inspect-run/events?order=asc",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(events["data"][0]["event_id"], "1");
    assert_eq!(events["data"][0]["event_type"], "run_created");

    let artifacts = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/artifacts", Body::empty())
            .await,
    )
    .await;
    let artifact_id = artifacts["data"][0]["artifact_id"]
        .as_str()
        .expect("artifact id");
    assert_eq!(artifacts["data"][0]["name"], "000_output.txt");
    assert_eq!(artifacts["data"][0]["media_type"], "text/plain");

    let evidence = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/evidence", Body::empty())
            .await,
    )
    .await;
    let evidence_id = evidence["data"]
        .as_array()
        .expect("evidence rows")
        .iter()
        .find(|row| row["name"] == "000_trace.log")
        .and_then(|row| row["evidence_id"].as_str())
        .expect("trace evidence id");
    let evidence_names = evidence["data"]
        .as_array()
        .expect("evidence rows")
        .iter()
        .map(|row| row["name"].as_str().expect("evidence name"))
        .collect::<Vec<_>>();
    assert!(evidence_names.contains(&"000_trace.log"));
    assert!(evidence_names.contains(&"transist_external_transfer_001.json"));

    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/steps", Body::empty())
            .await,
    )
    .await;
    assert_eq!(steps["data"][0]["step_id"], "collect");
    assert_eq!(steps["data"][1]["step_id"], "review");
    assert_eq!(steps["data"][1]["is_current_active"], true);
    assert_eq!(steps["data"][0]["input_summary"]["request_present"], true);
    assert_eq!(
        steps["data"][0]["input_summary"]["invoke_id"],
        "invoke_collect"
    );
    assert_eq!(steps["data"][0]["input_summary"]["adapter_kind"], "process");
    assert_eq!(steps["data"][0]["input_summary"]["plugin_id"], "shell");
    assert_eq!(steps["data"][0]["input_summary"]["action"], "exec");
    assert_eq!(
        steps["data"][0]["input_summary"]["filesystem_policy"]["workspace_root"],
        "."
    );
    let filesystem_json = steps["data"][0]["input_summary"]["filesystem_policy"].to_string();
    assert!(!filesystem_json.contains("/tmp/") && !filesystem_json.contains("~/"));
    assert_eq!(
        steps["data"][0]["input_summary"]["input_object_keys"],
        json!(["output_file", "prompt", "report_file"])
    );
    assert_eq!(
        steps["data"][0]["input_summary"]["metadata_object_keys"],
        json!(["plugin_id", "step_kind"])
    );
    assert_eq!(steps["data"][0]["output_summary"]["result_present"], true);
    assert_eq!(
        steps["data"][0]["output_summary"]["invoke_id"],
        "invoke_collect"
    );
    assert_eq!(steps["data"][0]["output_summary"]["status"], "success");
    assert_eq!(steps["data"][0]["output_summary"]["exit_code"], 0);
    assert_eq!(
        steps["data"][0]["output_summary"]["output_object_keys"],
        json!(["message", "status"])
    );
    assert_eq!(steps["data"][0]["output_summary"]["artifact_count"], 1);
    assert_eq!(steps["data"][0]["output_summary"]["evidence_count"], 2);
    assert_eq!(
        steps["data"][0]["output_summary"]["artifact_refs"][0]["artifact_id"],
        artifact_id
    );
    let evidence_refs = steps["data"][0]["output_summary"]["evidence_refs"]
        .as_array()
        .expect("evidence refs");
    assert!(
        evidence_refs
            .iter()
            .any(|row| row["evidence_id"] == evidence_id),
        "trace evidence id should be present in evidence refs"
    );
    let workflow_file_id =
        steps["data"][0]["output_summary"]["workflow_files"][0]["workflow_file_id"]
            .as_str()
            .expect("workflow file id");
    let workflow_files = steps["data"][0]["output_summary"]["workflow_files"]
        .as_array()
        .expect("workflow files");
    assert_eq!(workflow_files.len(), 2);
    let report_file = workflow_files
        .iter()
        .find(|file| file["field_name"] == "report_file")
        .expect("report file");
    let report_file_id = report_file["workflow_file_id"]
        .as_str()
        .expect("report file id");
    assert_ne!(workflow_file_id, report_file_id);
    assert_eq!(
        steps["data"][0]["output_summary"]["workflow_files"][0]["field_name"],
        "output_file"
    );
    assert_eq!(
        steps["data"][0]["output_summary"]["workflow_files"][0]["locator_kind"],
        "workspace"
    );
    assert_eq!(
        steps["data"][0]["output_summary"]["workflow_files"][0]["name"],
        "collect.md"
    );
    assert_eq!(
        steps["data"][0]["output_summary"]["workflow_files"][0]["relative_path"],
        "server_demo/reports/nested/collect.md"
    );
    assert!(
        !workflow_file_id.contains("/tmp/")
            && !workflow_file_id.contains("\\")
            && !workflow_file_id.contains("server_demo/reports")
    );

    let gates = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/gates", Body::empty())
            .await,
    )
    .await;
    assert_eq!(gates["data"][0]["gate_id"], "security-review");
    assert_eq!(gates["data"][0]["status"], "blocked");
    assert_eq!(gates["data"][0]["decision"]["actor"], "alice@example.com");

    let trust = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/trust", Body::empty())
            .await,
    )
    .await;
    assert_eq!(trust["trusted_execution_mode"], true);
    assert_eq!(trust["workflow"]["signer_key_id"], "wkid_demo");
    assert_eq!(trust["plugins"][0]["id"], "shell");
    assert_eq!(trust["policy_decision"]["mode"], "run_start");

    let locality = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/locality", Body::empty())
            .await,
    )
    .await;
    assert_eq!(locality["steps"][0]["model"], "gpt-test");
    assert_eq!(
        locality["external_transfers"][0]["provider_kind"],
        "external"
    );
    assert_eq!(
        locality["external_transfers"][0]["policy_mode"],
        "trusted_execution"
    );

    let step_detail = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/inspect-run/steps/collect",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(step_detail["step_id"], "collect");
    assert_eq!(step_detail["metrics"]["execution_count"], 1);
    assert_eq!(
        step_detail["metrics"]["latest_execution"]["duration_ms"],
        1000
    );
    assert_eq!(
        step_detail["metrics"]["latest_execution"]["adapter_metrics"]["latency_ms"],
        1000.0
    );
    assert_eq!(
        step_detail["metrics"]["usage_breakdown"][0]["total_tokens"],
        18
    );

    let artifact = read_json(
        fixture
            .request(
                "GET",
                &format!("/api/v1/runs/inspect-run/artifacts/{artifact_id}"),
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(artifact["artifact_id"], artifact_id);
    assert_eq!(artifact["descriptor"]["status"], "success");

    let artifact_content = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/artifacts/{artifact_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(artifact_content.status(), StatusCode::OK);
    assert_eq!(
        artifact_content
            .headers()
            .get("content-type")
            .expect("content-type"),
        "text/plain"
    );
    assert_eq!(
        read_body(artifact_content).await,
        b"hello from artifact".to_vec()
    );

    let evidence_content = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/evidence/{evidence_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(evidence_content.status(), StatusCode::OK);
    assert_eq!(read_body(evidence_content).await, b"trace line".to_vec());

    let workflow_file = read_json(
        fixture
            .request(
                "GET",
                &format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}"),
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(workflow_file["workflow_file_id"], workflow_file_id);
    assert_eq!(workflow_file["field_name"], "output_file");
    assert_eq!(workflow_file["locator_kind"], "workspace");
    assert!(
        workflow_file["relative_path"]
            .as_str()
            .expect("workflow path")
            == "server_demo/reports/nested/collect.md"
    );

    let workflow_file_content = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(workflow_file_content.status(), StatusCode::OK);
    assert_eq!(
        read_body(workflow_file_content).await,
        b"hello from workflow file".to_vec()
    );

    let report_file = read_json(
        fixture
            .request(
                "GET",
                &format!("/api/v1/runs/inspect-run/workflow-files/{report_file_id}"),
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(report_file["workflow_file_id"], report_file_id);
    assert_eq!(report_file["field_name"], "report_file");
    let report_file_content = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/workflow-files/{report_file_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(report_file_content.status(), StatusCode::OK);
    assert_eq!(
        read_body(report_file_content).await,
        b"hello from workflow file".to_vec()
    );

    let workflows = read_json(
        fixture
            .request("GET", "/api/v1/workflows", Body::empty())
            .await,
    )
    .await;
    let workflow_rows = workflows["data"].as_array().expect("workflow rows");
    assert!(workflow_rows.iter().any(|row| {
        row["workflow_ref"] == "server_demo@v1"
            && row["compiled_recipe_available"] == json!(true)
            && row["root_step_count"] == json!(2)
            && row["is_subflow"] == json!(false)
    }));
    assert!(workflow_rows.iter().any(|row| {
        row["workflow_ref"] == "graph_demo@v1"
            && row["compiled_recipe_available"] == json!(true)
            && row["is_subflow"] == json!(false)
    }));
    assert!(workflow_rows.iter().any(|row| {
        row["workflow_ref"] == "compiled_only@v1" && row["compiled_recipe_available"] == json!(true)
    }));

    let workflow = read_json(
        fixture
            .request("GET", "/api/v1/workflows/server_demo@v1", Body::empty())
            .await,
    )
    .await;
    assert_eq!(workflow["workflow_ref"], "server_demo@v1");
    assert_eq!(workflow["is_subflow"], false);
    assert_eq!(workflow["step_graph_summary"][1]["step_id"], "review");
    assert_eq!(workflow["validation"]["status"], "unknown");
    assert_eq!(
        workflow["prompt_references"]
            .as_array()
            .expect("prompts")
            .len(),
        0
    );
}

#[tokio::test]
async fn run_detail_exposes_replay_lineage_for_secondary_inspection() {
    let fixture = Fixture::with_replayed_run();

    let replay = read_json(
        fixture
            .request("GET", "/api/v1/runs/replay-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(replay["run_id"], "replay-run");
    assert_eq!(replay["status"], "success");
    assert_eq!(replay["lineage"]["mode"], "replay");
    assert_eq!(replay["lineage"]["source_run_id"], "inspect-run");
    assert_eq!(replay["lineage"]["root_run_id"], "replay-run");
}

#[tokio::test]
async fn workflow_detail_reports_validation_status_for_invalid_and_recipe_only_sources() {
    let fixture = Fixture::new();

    let compiled_only = read_json(
        fixture
            .request("GET", "/api/v1/workflows/compiled_only@v1", Body::empty())
            .await,
    )
    .await;
    assert_eq!(compiled_only["validation"]["status"], "unknown");
    assert!(compiled_only["validation"]["errors"].is_null());
    assert_eq!(compiled_only["compiled_recipe_available"], true);
}

#[tokio::test]
async fn inspect_root_redirects_to_runs_list() {
    let fixture = Fixture::new();
    let response = fixture.request("GET", "/inspect", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get("location")
        .expect("location header")
        .to_str()
        .expect("location utf-8");
    assert_eq!(location, "/inspect/runs");
}

#[tokio::test]
async fn inspect_runs_list_renders_html_with_run_row_and_filters() {
    let fixture = Fixture::new();
    let response = fixture.request("GET", "/inspect/runs", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .expect("content-type"),
        "text/html; charset=utf-8"
    );
    let body = read_text(response).await;
    assert!(body.contains("<main"), "main landmark present");
    assert!(body.contains("<h1>Runs</h1>"));
    assert!(body.contains("inspect-run"));
    assert!(body.contains("/inspect/runs/inspect-run"));
    assert!(body.contains("name=\"status\""));
    assert!(body.contains("/inspect/static/inspect.css"));
    // The runs list now exposes the consolidated timer trio under a single
    // `Duration` column, rendering `wall clock / active / blocked` per row
    // instead of the legacy `Elapsed (wall-clock)` header.
    assert!(
        body.contains("<th scope=\"col\">Duration</th>"),
        "runs list should render the consolidated `Duration` column header"
    );
    assert!(
        !body.contains("Elapsed (wall-clock)"),
        "runs list should no longer carry the legacy `Elapsed (wall-clock)` label"
    );
    assert!(
        body.contains("wall clock") && body.contains("active") && body.contains("blocked"),
        "runs list row should expose wall-clock/active/blocked breakdown"
    );
}

#[tokio::test]
async fn inspect_runs_list_supports_status_filter_and_pagination() {
    let fixture = Fixture::new();
    let body = read_text(
        fixture
            .request("GET", "/inspect/runs?status=blocked", Body::empty())
            .await,
    )
    .await;
    assert!(body.contains("inspect-run"));

    let body = read_text(
        fixture
            .request("GET", "/inspect/runs?status=canceled", Body::empty())
            .await,
    )
    .await;
    assert!(
        body.contains("No runs match"),
        "expected empty state when filter excludes all rows"
    );

    let response = fixture
        .request("GET", "/inspect/runs?status=not-a-status", Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn inspect_run_detail_tabs_render_against_fixture_data() {
    let fixture = Fixture::new();

    let overview = read_text(
        fixture
            .request("GET", "/inspect/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    assert!(overview.contains("Run"));
    assert!(overview.contains("inspect-run"));
    assert!(overview.contains("Metrics"));
    assert!(overview.contains("Trust &amp; Provenance"));
    assert!(overview.contains("workflow-digest-123"));
    assert!(overview.contains("api.openai.com"));
    assert!(overview.contains("transist runs evidence export inspect-run"));
    // The headline strip now surfaces the run's wall-clock duration under the
    // `Wall clock` label, and the metrics block exposes the full timer trio
    // under `Duration`.
    assert!(
        overview.contains("<dt>Wall clock</dt>"),
        "run overview headline should label run duration as `Wall clock`"
    );
    assert!(
        overview.contains("<dt>Duration</dt>"),
        "run overview metrics block should carry the consolidated `Duration` label"
    );
    assert!(
        !overview.contains("Elapsed (wall-clock)"),
        "run overview should no longer carry the legacy `Elapsed (wall-clock)` label"
    );
    assert!(
        overview.contains("href=\"/inspect/workflows/server_demo@v1\""),
        "run overview should link the workflow headline to the workflow detail page"
    );

    for tab in [
        "steps",
        "events",
        "gates",
        "failures",
        "artifacts",
        "evidence",
        "workflow-files",
        "children",
    ] {
        let body = read_text(
            fixture
                .request(
                    "GET",
                    &format!("/inspect/runs/inspect-run?tab={tab}"),
                    Body::empty(),
                )
                .await,
        )
        .await;
        assert!(
            body.contains("aria-label=\"Run tabs\""),
            "tab {tab} should still render tab navigation"
        );
        if matches!(tab, "artifacts" | "evidence" | "workflow-files") {
            assert!(
                body.contains("all iterations") && body.contains("last iteration"),
                "tab {tab} should render iteration filter"
            );
        }
        if tab == "gates" {
            assert!(body.contains("alice@example.com"));
            assert!(body.contains("reviewed generated plan"));
        }
    }
}

#[tokio::test]
async fn failure_records_are_exposed_via_api_and_html() {
    let fixture = Fixture::with_failed_invoke_step();

    let failures = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/failed-invoke-run/failures",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(failures["data"].as_array().expect("failures").len(), 1);
    let failure_id = failures["data"][0]["failure_id"]
        .as_str()
        .expect("failure id")
        .to_string();
    assert_eq!(failures["data"][0]["failure_class"], "tool_error");
    assert_eq!(failures["data"][0]["safe_to_retry"], true);

    let failure = read_json(
        fixture
            .request(
                "GET",
                &format!("/api/v1/runs/failed-invoke-run/failures/{failure_id}"),
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(failure["failure_id"], failure_id);
    assert_eq!(failure["adapter_name"], "shell");

    let redirect = fixture
        .request(
            "GET",
            "/inspect/runs/failed-invoke-run/failures",
            Body::empty(),
        )
        .await;
    assert_eq!(redirect.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        redirect
            .headers()
            .get("location")
            .expect("location")
            .to_str()
            .expect("location utf-8"),
        "/inspect/runs/failed-invoke-run?tab=failures"
    );

    let html = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/failed-invoke-run?tab=failures",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(html.contains("tool_error"));
    assert!(html.contains("shell exited with code 2"));
}

#[tokio::test]
async fn inspect_run_detail_filters_iteration_scopes_on_blob_tabs() {
    let fixture = Fixture::new();

    let artifacts_all = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/inspect-run?tab=artifacts&iteration=all",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(artifacts_all.contains("<option value=\"all\" selected>"));

    let artifacts_specific = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/inspect-run?tab=artifacts&iteration=1",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(artifacts_specific.contains("<option value=\"1\" selected>iteration 1</option>"));

    let workflow_files_specific = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/inspect-run?tab=workflow-files&iteration=1",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(workflow_files_specific.contains("<th>Exec</th>"));
    assert!(workflow_files_specific.contains("<option value=\"1\" selected>iteration 1</option>"));
}

#[tokio::test]
async fn inspect_unknown_run_returns_404_html() {
    let fixture = Fixture::new();
    let response = fixture
        .request("GET", "/inspect/runs/does-not-exist", Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = read_text(response).await;
    assert!(body.contains("resource_not_found"));
}

#[tokio::test]
async fn inspect_step_detail_renders_for_known_step() {
    let fixture = Fixture::new();
    let body = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/inspect-run/steps/collect",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(body.contains("Step"));
    assert!(body.contains("collect"));
    assert!(body.contains("Latest step output summary"));
    assert!(body.contains("Workflow files"));
    assert!(body.contains("Artifact/evidence iteration filter"));
    assert!(body.contains("all iterations"));
    assert!(body.contains("last iteration"));
    // The step-detail metrics block is rendered as JSON only on this view, so
    // the consolidated timer trio is verified at the API level (see
    // `read_endpoints_expose_richer_steps_and_resource_specific_ids`). No HTML
    // label assertion fits here because the fixture has no child-run overview.
    assert!(
        !body.contains("Elapsed (wall-clock)"),
        "step detail should no longer carry the legacy `Elapsed (wall-clock)` label"
    );
    // The structured summary grid replaces a raw JSON dump for the most useful
    // fields, so users don't have to expand the JSON to see them.
    assert!(
        body.contains("Result present"),
        "missing structured 'Result present' field"
    );
    assert!(
        body.contains("Request present"),
        "missing structured 'Request present' field"
    );
    // No child run on this step → no child-run CTA should be rendered.
    assert!(
        !body.contains("View child run"),
        "unexpected child-run banner on non-subflow step"
    );
}

#[tokio::test]
async fn inspect_step_detail_accepts_iteration_filter_query() {
    let fixture = Fixture::new();
    let body = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/inspect-run/steps/collect?iteration=all",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(body.contains("<option value=\"all\" selected>"));

    let body = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/inspect-run/steps/collect?iteration=1",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(body.contains("<option value=\"1\" selected>iteration 1</option>"));
}

#[tokio::test]
async fn inspect_step_detail_surfaces_failure_context_for_failed_step() {
    let fixture = Fixture::with_failed_invoke_step();
    let body = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/failed-invoke-run/steps/exec",
                Body::empty(),
            )
            .await,
    )
    .await;

    // CLI-parity failure context: Plugin/Adapter/Action/Exit code/Error
    // code/Error must surface inline in the top kv-grid so users see why
    // the step failed without expanding the raw JSON.
    assert!(body.contains("<dt>Plugin</dt>"), "missing Plugin row");
    assert!(body.contains("shell"), "missing plugin_id value");
    assert!(body.contains("<dt>Adapter</dt>"), "missing Adapter row");
    assert!(body.contains("<dt>Action</dt>"), "missing Action row");
    assert!(body.contains("<dt>Exit code</dt>"), "missing Exit code row");
    assert!(
        body.contains("<dt>Error code</dt>"),
        "missing Error code row"
    );
    assert!(
        body.contains("tool_execution_error"),
        "missing error_code value"
    );
    assert!(body.contains("<dt>Error</dt>"), "missing Error row");
    assert!(
        body.contains("shell exited with code 2"),
        "missing error_message value"
    );

    // The standalone error_message aside must be gone now that the
    // structured rows replace it.
    assert!(
        !body.contains("status-banner-failed"),
        "standalone error aside should be replaced by inline kv-grid rows"
    );
}

#[tokio::test]
async fn inspect_run_detail_surfaces_preflight_blockers_when_run_failed_before_any_step() {
    let fixture = Fixture::with_failed_preflight_run();
    let body = read_text(
        fixture
            .request("GET", "/inspect/runs/preflight-failed-run", Body::empty())
            .await,
    )
    .await;

    // Banner headline acknowledges preflight as the failure phase.
    assert!(
        body.contains("This run failed during preflight."),
        "preflight-aware headline missing"
    );
    // Preflight blocker list is rendered inline.
    assert!(
        body.contains("status-banner-preflight-list"),
        "preflight blocker list not rendered"
    );
    // Both blocker messages and their kinds surface verbatim.
    assert!(
        body.contains("secret_ref"),
        "missing secret_ref blocker kind"
    );
    assert!(
        body.contains("GITHUB_TOKEN"),
        "missing secret_ref blocker message"
    );
    assert!(
        body.contains("plugin_action"),
        "missing plugin_action blocker kind"
    );
    assert!(
        body.contains("create_release"),
        "missing plugin_action blocker message"
    );
    // Plugin / action metadata is exposed so operators can identify the offender.
    assert!(body.contains("shell"), "missing shell plugin id");
    assert!(body.contains("github"), "missing github plugin id");
    // Per-check step ids surface so operators can jump to the right step.
    assert!(body.contains("collect"), "missing collect step id");
    assert!(body.contains("publish"), "missing publish step id");
}

#[tokio::test]
async fn run_detail_api_exposes_preflight_failed_checks() {
    let fixture = Fixture::with_failed_preflight_run();
    let body = read_text(
        fixture
            .request("GET", "/api/v1/runs/preflight-failed-run", Body::empty())
            .await,
    )
    .await;
    let value: Value = serde_json::from_str(&body).expect("run detail json");
    let preflight = value
        .get("preflight")
        .expect("preflight summary present on run detail");
    assert_eq!(preflight["status"], "failed");
    assert_eq!(preflight["blocker_count"], 2);
    let failed_checks = preflight["failed_checks"]
        .as_array()
        .expect("failed_checks array");
    assert_eq!(failed_checks.len(), 2);
    let kinds: Vec<&str> = failed_checks
        .iter()
        .map(|c| c["kind"].as_str().unwrap_or(""))
        .collect();
    assert!(kinds.contains(&"secret_ref"));
    assert!(kinds.contains(&"plugin_action"));
    let secret_ref = failed_checks
        .iter()
        .find(|c| c["kind"] == "secret_ref")
        .expect("secret_ref entry");
    assert_eq!(secret_ref["plugin_id"], "shell");
    assert_eq!(secret_ref["action"], "exec");
    assert_eq!(secret_ref["step_ids"][0], "collect");
    assert!(
        secret_ref["message"]
            .as_str()
            .unwrap_or("")
            .contains("GITHUB_TOKEN")
    );
}

#[tokio::test]
async fn inspect_step_detail_surfaces_preflight_blockers_for_pending_step() {
    let fixture = Fixture::with_failed_preflight_run();
    let body = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/preflight-failed-run/steps/collect",
                Body::empty(),
            )
            .await,
    )
    .await;

    // Step-scoped preflight banner surfaces only the check(s) that name this step.
    assert!(
        body.contains("This step is blocked by preflight."),
        "step-scoped preflight headline missing"
    );
    assert!(
        body.contains("status-banner-preflight-list"),
        "preflight blocker list not rendered on step page"
    );
    // The blocker that names 'collect' surfaces here.
    assert!(body.contains("secret_ref"), "missing secret_ref kind");
    assert!(
        body.contains("GITHUB_TOKEN"),
        "missing secret_ref blocker message"
    );
    assert!(body.contains("shell"), "missing plugin id for blocker");
    // The unrelated 'publish' blocker must NOT leak into this step's view.
    assert!(
        !body.contains("create_release"),
        "unrelated blocker for another step should not surface here"
    );
    assert!(
        !body.contains("plugin_action"),
        "unrelated blocker kind for another step should not surface here"
    );
}

#[tokio::test]
async fn inspect_run_detail_surfaces_descendant_failure_for_failed_subflow_parent() {
    let fixture = Fixture::with_failed_subflow_with_descendant_error();
    let body = read_text(
        fixture
            .request("GET", "/inspect/runs/parent-run", Body::empty())
            .await,
    )
    .await;

    // Headline still references the local failed step…
    assert!(
        body.contains("This run failed at step &#39;nested_loop&#39;.")
            || body.contains("This run failed at step 'nested_loop'."),
        "headline missing or wrong"
    );
    // …but the detail line surfaces the descendant's actual error message,
    // including its run id and step id. The plain "Exit code 1." fallback
    // must NOT be the only thing shown.
    assert!(
        body.contains("no pull requests found for branch"),
        "descendant error message not surfaced in banner detail"
    );
    assert!(
        body.contains("check_pr_exit_approvals"),
        "descendant step id not surfaced in banner detail"
    );
    assert!(
        body.contains("parent-run__nested_loop"),
        "descendant run id not surfaced in banner detail"
    );
    assert!(
        body.contains("tool_execution_error"),
        "descendant error code not surfaced in banner detail"
    );
    // Primary link should deep-link to the root-cause step on the descendant run.
    assert!(
        body.contains("/inspect/runs/parent-run__nested_loop/steps/check_pr_exit_approvals"),
        "primary link should deep-link to descendant failed step"
    );
    assert!(
        body.contains("View root-cause step"),
        "primary link label should reflect root-cause walk"
    );
}

#[tokio::test]
async fn api_run_detail_exposes_actionable_run_id_for_failed_subflow_parent() {
    let fixture = Fixture::with_failed_subflow_with_descendant_error();
    let body = read_text(
        fixture
            .request("GET", "/api/v1/runs/parent-run", Body::empty())
            .await,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("parse api json");
    assert_eq!(
        v["actionable_run_id"].as_str(),
        Some("parent-run__nested_loop"),
        "actionable_run_id should walk to descendant child run"
    );
}

#[tokio::test]
async fn api_steps_expose_persisted_child_failure_for_failed_subflow_parent() {
    let fixture = Fixture::with_failed_subflow_carrying_persisted_child_failure();
    let steps = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/persisted-child-failure/steps",
                Body::empty(),
            )
            .await,
    )
    .await;
    let output_summary = &steps["data"][0]["output_summary"];
    let child_failure = &output_summary["child_failure"];
    assert_eq!(
        child_failure["child_run_id"].as_str(),
        Some("persisted-child-failure__nested_loop"),
        "child_run_id should surface from persisted output.failure"
    );
    assert_eq!(
        child_failure["failed_step_id"].as_str(),
        Some("check_pr_exit_approvals"),
        "failed_step_id should surface from persisted output.failure"
    );
    assert_eq!(
        child_failure["error_code"].as_str(),
        Some("tool_execution_error"),
        "error_code should surface from persisted output.failure"
    );
    assert_eq!(
        child_failure["error_message"].as_str(),
        Some("no pull requests found for branch \"master\""),
        "error_message should surface from persisted output.failure"
    );
    assert_eq!(
        child_failure["stderr"].as_str(),
        Some("no pull requests found for branch \"master\""),
        "stderr should surface from persisted output.failure"
    );
    assert_eq!(child_failure["preflight_blocker_count"], 2);
    assert_eq!(child_failure["preflight_warning_count"], 1);
    assert_eq!(
        child_failure["preflight_first_blocker"],
        "child input schema rejected model"
    );
}

#[tokio::test]
async fn api_inspection_fields_are_complete_and_paths_are_host_independent() {
    let fixture = Fixture::new();
    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    for field in ["runtime_mode", "policy_profile", "capability_enforcement"] {
        assert!(run.get(field).is_some(), "run detail lost field {field}");
    }
    assert_eq!(run["runtime_mode"], "unknown");
    assert_eq!(run["policy_profile"], "unknown");
    assert_eq!(run["capability_enforcement"], "unknown");

    let paths = [
        "/api/v1/runs",
        "/api/v1/runs/inspect-run",
        "/api/v1/runs/inspect-run/steps",
        "/api/v1/workflows",
        "/api/v1/workflows/server_demo@v1",
    ];
    let host_paths = [
        fixture.repo_root.display().to_string(),
        fixture.home_dir.display().to_string(),
    ];
    for path in paths {
        let body = read_body(fixture.request("GET", path, Body::empty()).await).await;
        let body = String::from_utf8(body).expect("json response");
        for host_path in &host_paths {
            assert!(
                !body.contains(host_path),
                "{path} leaked local host path {host_path}"
            );
        }
    }
}

#[tokio::test]
async fn arbitrary_persisted_json_is_recursively_sanitized_at_the_api_boundary() {
    let fixture = Fixture::new();

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["workflow_input_summary"]["safe"], "preserve-me");
    assert_eq!(run["workflow_input_summary"]["[path redacted]"], 1);
    assert_eq!(run["workflow_input_summary"]["[path redacted] #2"], 2);
    assert_eq!(run["workflow_input_summary"]["[path redacted] #3"], 3);
    for value in [
        &run["workflow_input_summary"]["paths"]["posix"],
        &run["workflow_input_summary"]["paths"]["windows"],
        &run["workflow_input_summary"]["paths"]["home"],
        &run["workflow_input_summary"]["paths"]["traversal"],
    ] {
        assert_eq!(*value, "[path redacted]");
    }

    let events = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/events", Body::empty())
            .await,
    )
    .await;
    assert_eq!(events["data"][1]["summary"]["safe"], "event-kept");
    assert_eq!(
        events["data"][1]["summary"]["nested"]["path"],
        "[path redacted]"
    );

    let gates = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/gates", Body::empty())
            .await,
    )
    .await;
    assert_eq!(gates["data"][0]["request_summary"]["safe"], "gate-kept");
    assert_eq!(
        gates["data"][0]["request_summary"]["nested"]["path"],
        "[path redacted]"
    );

    let failed = Fixture::with_failed_invoke_step();
    let failures = read_json(
        failed
            .request(
                "GET",
                "/api/v1/runs/failed-invoke-run/failures",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(
        failures["data"][0]["message_redacted"],
        "shell failed at [path redacted]"
    );
}

#[tokio::test]
async fn inspect_step_detail_shows_child_run_overview_for_subflow_step() {
    let fixture = Fixture::with_subflow_step();
    let body = read_text(
        fixture
            .request(
                "GET",
                "/inspect/runs/subflow-run/steps/plan_loop",
                Body::empty(),
            )
            .await,
    )
    .await;
    // Child run banner and kv-row must both appear.
    assert!(
        body.contains("View child run"),
        "missing 'View child run' banner"
    );
    assert!(
        body.contains("Child run overview"),
        "missing embedded child run overview section"
    );
    // The child run id should appear in the page (kv-grid and overview).
    assert!(
        body.contains("subflow-run__plan_loop"),
        "missing child run id"
    );
    // A direct link to the child run inspect page must be present.
    assert!(
        body.contains("/inspect/runs/subflow-run__plan_loop"),
        "missing href to child run"
    );
}

#[tokio::test]
async fn inspect_artifact_detail_links_to_existing_content_endpoint() {
    let fixture = Fixture::new();
    let artifacts = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/artifacts", Body::empty())
            .await,
    )
    .await;
    let artifact_id = artifacts["data"][0]["artifact_id"]
        .as_str()
        .expect("artifact id");

    let body = read_text(
        fixture
            .request(
                "GET",
                &format!("/inspect/runs/inspect-run/artifacts/{artifact_id}"),
                Body::empty(),
            )
            .await,
    )
    .await;
    let content_url = format!("/api/v1/runs/inspect-run/artifacts/{artifact_id}/content");
    assert!(body.contains(&content_url), "missing content url link");
    let content_response = fixture.request("GET", &content_url, Body::empty()).await;
    assert_eq!(content_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn inspect_workflows_list_and_detail_render() {
    let fixture = Fixture::new();
    let body = read_text(
        fixture
            .request("GET", "/inspect/workflows", Body::empty())
            .await,
    )
    .await;
    assert!(body.contains("server_demo@v1"));

    let body = read_text(
        fixture
            .request("GET", "/inspect/workflows/server_demo@v1", Body::empty())
            .await,
    )
    .await;
    assert!(body.contains("server_demo@v1"));
    assert!(body.contains("Steps"));
    assert!(body.contains("Plugins"));
    assert!(body.contains("Inputs"));

    let plugins = read_text(
        fixture
            .request(
                "GET",
                "/inspect/workflows/server_demo@v1?tab=plugins",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(plugins.contains("No plugins used by this workflow."));

    let inputs = read_text(
        fixture
            .request(
                "GET",
                "/inspect/workflows/server_demo@v1?tab=inputs",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(inputs.contains("prompt"));

    let source = read_text(
        fixture
            .request(
                "GET",
                "/inspect/workflows/server_demo@v1/source",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(source.contains("Source"));

    let compiled = read_text(
        fixture
            .request(
                "GET",
                "/inspect/workflows/server_demo@v1/compiled",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert!(compiled.contains("Compiled recipe"));
}

#[tokio::test]
async fn inspect_static_assets_are_served_with_expected_content_types() {
    let fixture = Fixture::new();
    let css = fixture
        .request("GET", "/inspect/static/inspect.css", Body::empty())
        .await;
    assert_eq!(css.status(), StatusCode::OK);
    assert_eq!(
        css.headers().get("content-type").expect("content-type"),
        "text/css; charset=utf-8"
    );
    assert!(read_text(css).await.contains(".status-badge"));

    let js = fixture
        .request("GET", "/inspect/static/inspect.js", Body::empty())
        .await;
    assert_eq!(js.status(), StatusCode::OK);
    assert_eq!(
        js.headers().get("content-type").expect("content-type"),
        "text/javascript; charset=utf-8"
    );
    assert!(read_text(js).await.contains("auto-refresh-toggle"));

    let missing = fixture
        .request("GET", "/inspect/static/missing.txt", Body::empty())
        .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn workflow_content_endpoints_return_real_source_and_compiled_recipe_content() {
    let fixture = Fixture::new();

    let source = fixture
        .request(
            "GET",
            "/api/v1/workflows/server_demo@v1/source",
            Body::empty(),
        )
        .await;
    assert_eq!(source.status(), StatusCode::OK);
    assert_eq!(
        source.headers().get("content-type").expect("content-type"),
        "text/yaml; charset=utf-8"
    );
    let source_text = read_text(source).await;
    assert!(source_text.contains("schema: transist.flow.v1"));
    assert!(!source_text.contains(&("/".to_string() + "Users/alice/gate-request.json")));
    assert!(source_text.contains("[path redacted]"));

    let compiled = fixture
        .request(
            "GET",
            "/api/v1/workflows/server_demo@v1/compiled-recipe",
            Body::empty(),
        )
        .await;
    assert_eq!(compiled.status(), StatusCode::OK);
    assert_eq!(
        compiled
            .headers()
            .get("content-type")
            .expect("content-type"),
        "text/yaml; charset=utf-8"
    );
    let compiled_text = read_text(compiled).await;
    assert!(compiled_text.contains("server_demo"));
    assert!(!compiled_text.contains(&("/".to_string() + "Users/alice/gate-request.json")));
}

#[tokio::test]
async fn workflow_content_endpoints_reject_invalid_utf8_without_source_bytes() {
    let invalid_bytes = b"not text: \xff private source";
    for (path, invalid_path) in [
        (
            "/api/v1/workflows/server_demo@v1/source",
            "server_demo.flow.yaml",
        ),
        (
            "/api/v1/workflows/server_demo@v1/compiled-recipe",
            "registry/recipes/server_demo-v1.yaml",
        ),
    ] {
        let fixture = Fixture::new();
        let invalid_path = if path.ends_with("/source") {
            fixture.repo_root.join(invalid_path)
        } else {
            fixture.home_dir.join(invalid_path)
        };
        fs::write(invalid_path, invalid_bytes).expect("write invalid content");
        let response = fixture.request("GET", path, Body::empty()).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}"
        );
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .expect("content type"),
            "application/json"
        );
        let body = read_body(response).await;
        assert!(
            !body
                .windows(invalid_bytes.len())
                .any(|window| window == invalid_bytes)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("problem JSON"),
            json!({
                "error": {
                    "code": "invalid_text_encoding",
                    "message": "requested content is not valid UTF-8 text"
                }
            })
        );
    }
}

#[tokio::test]
async fn workflow_content_endpoints_report_unavailable_representations_and_bad_refs() {
    let fixture = Fixture::new();

    let missing_source = fixture
        .request(
            "GET",
            "/api/v1/workflows/compiled_only@v1/source",
            Body::empty(),
        )
        .await;
    assert_eq!(missing_source.status(), StatusCode::NOT_FOUND);

    let missing_recipe = fixture
        .request(
            "GET",
            "/api/v1/workflows/source_only@v1/compiled-recipe",
            Body::empty(),
        )
        .await;
    assert_eq!(missing_recipe.status(), StatusCode::NOT_FOUND);

    let bad_ref = fixture
        .request("GET", "/api/v1/workflows/not-a-ref/source", Body::empty())
        .await;
    assert_eq!(bad_ref.status(), StatusCode::BAD_REQUEST);
    let body = read_json(bad_ref).await;
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn every_companion_route_response_matches_its_openapi_contract() {
    let fixture = Fixture::new();
    let contract = load_openapi_contract();

    let artifacts = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/artifacts", Body::empty())
            .await,
    )
    .await;
    let evidence = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/evidence", Body::empty())
            .await,
    )
    .await;
    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/steps", Body::empty())
            .await,
    )
    .await;
    let artifact_id = artifacts["data"][0]["artifact_id"]
        .as_str()
        .expect("artifact id");
    let evidence_id = evidence["data"][0]["evidence_id"]
        .as_str()
        .expect("evidence id");
    let workflow_file_id =
        steps["data"][0]["output_summary"]["workflow_files"][0]["workflow_file_id"]
            .as_str()
            .expect("workflow file id");

    for path in [
        "/api/v1/runs",
        "/api/v1/runs/inspect-run",
        "/api/v1/runs/inspect-run/trust",
        "/api/v1/runs/inspect-run/locality",
        "/api/v1/runs/inspect-run/events",
        "/api/v1/runs/inspect-run/steps",
        "/api/v1/runs/inspect-run/steps/collect",
        "/api/v1/runs/inspect-run/gates",
        "/api/v1/runs/inspect-run/failures",
        "/api/v1/runs/inspect-run/artifacts",
        "/api/v1/runs/inspect-run/evidence",
        "/api/v1/workflows",
        "/api/v1/workflows/server_demo@v1",
    ] {
        assert_contract_response(&fixture, &contract, "GET", path, StatusCode::OK).await;
    }
    for path in [
        format!("/api/v1/runs/inspect-run/artifacts/{artifact_id}"),
        format!("/api/v1/runs/inspect-run/artifacts/{artifact_id}/content"),
        format!("/api/v1/runs/inspect-run/evidence/{evidence_id}"),
        format!("/api/v1/runs/inspect-run/evidence/{evidence_id}/content"),
        format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}"),
        format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}/content"),
    ] {
        assert_contract_response(&fixture, &contract, "GET", &path, StatusCode::OK).await;
    }

    assert_contract_response(
        &fixture,
        &contract,
        "GET",
        "/api/v1/workflows/server_demo@v1/source",
        StatusCode::OK,
    )
    .await;
    assert_contract_response(
        &fixture,
        &contract,
        "GET",
        "/api/v1/workflows/server_demo@v1/compiled-recipe",
        StatusCode::OK,
    )
    .await;

    // Exercise representative handler errors, including every response family
    // whose status or content type is easy to regress independently.
    for (path, status) in [
        ("/api/v1/runs?limit=not-a-number", StatusCode::BAD_REQUEST),
        ("/api/v1/runs/missing", StatusCode::NOT_FOUND),
        (
            "/api/v1/runs/inspect-run/events?order=invalid",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/runs/inspect-run/artifacts?scope=invalid",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/runs/inspect-run/artifacts/not-base64",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/runs/inspect-run/evidence?scope=invalid",
            StatusCode::BAD_REQUEST,
        ),
        ("/api/v1/workflows/not-a-ref", StatusCode::BAD_REQUEST),
        (
            "/api/v1/workflows/not-a-ref/source",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/workflows/not-a-ref/compiled-recipe",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_contract_response(&fixture, &contract, "GET", path, status).await;
    }

    let failing = app_with_query(Arc::new(FailingInspectionQuery));
    let response = failing
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runs")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_contract_http_response(
        &contract,
        "GET",
        "/api/v1/runs",
        response,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;

    let failed_fixture = Fixture::with_failed_invoke_step();
    assert_contract_response(
        &failed_fixture,
        &contract,
        "GET",
        "/api/v1/runs/failed-invoke-run/failures",
        StatusCode::OK,
    )
    .await;
    assert_contract_response(
        &failed_fixture,
        &contract,
        "GET",
        "/api/v1/runs/failed-invoke-run/failures/failure-exec-1",
        StatusCode::OK,
    )
    .await;
}

fn load_openapi_contract() -> Value {
    yaml_serde::from_str(
        &fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../schemas/http/companion-api.v1.openapi.yaml"),
        )
        .expect("read OpenAPI contract"),
    )
    .expect("parse OpenAPI contract")
}

async fn assert_contract_response(
    fixture: &Fixture,
    contract: &Value,
    method: &str,
    path: &str,
    status: StatusCode,
) {
    let response = fixture.request(method, path, Body::empty()).await;
    assert_contract_http_response(contract, method, path, response, status).await;
}

async fn assert_contract_http_response(
    contract: &Value,
    method: &str,
    path: &str,
    response: axum::response::Response,
    expected_status: StatusCode,
) {
    assert_eq!(response.status(), expected_status, "{method} {path}");
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("{method} {path} has no content type"));
    assert_content_type_declared(contract, method, path, expected_status, &content_type);
    let body = read_body(response).await;
    let schema = response_schema(contract, method, path, expected_status, &content_type);
    let mut defs = contract["components"]["schemas"].clone();
    replace_component_refs(&mut defs);
    normalize_openapi_nullable(&mut defs);
    let mut resolved_schema = schema;
    replace_component_refs(&mut resolved_schema);
    normalize_openapi_nullable(&mut resolved_schema);
    let mut root = json!({"$defs": defs});
    root.as_object_mut().expect("schema object").extend(
        resolved_schema
            .as_object()
            .expect("response schema object")
            .clone(),
    );
    let validator = jsonschema::JSONSchema::compile(&root)
        .unwrap_or_else(|error| panic!("compile {method} {path} schema: {error}"));

    let response_is_json = resolved_schema["type"].as_str() != Some("string");
    if response_is_json {
        let payload: Value = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("decode {method} {path} JSON response: {error}"));
        if let Err(errors) = validator.validate(&payload) {
            panic!(
                "{method} {path} payload violates its OpenAPI response schema: {:?}",
                errors.collect::<Vec<_>>()
            );
        }
    } else {
        let text = String::from_utf8_lossy(&body);
        if let Err(errors) = validator.validate(&Value::String(text.into_owned())) {
            panic!(
                "{method} {path} content violates its OpenAPI response schema: {:?}",
                errors.collect::<Vec<_>>()
            );
        }
    }
}

/// Returns the media type part of a `Content-Type` header value, stripping any
/// parameters such as `charset=utf-8` and trimming/casing.
fn media_part(value: &str) -> String {
    value
        .split(';')
        .next()
        .expect("media type")
        .trim()
        .to_ascii_lowercase()
}

/// Verifies the response's `Content-Type` media type is literally declared as a
/// `content` media-type key of the matching OpenAPI response. A literal `*/*`
/// declaration accepts any media type; otherwise the media part must match
/// exactly (wildcard suffix matching is intentionally not applied here).
fn assert_content_type_declared(
    contract: &Value,
    method: &str,
    actual_path: &str,
    status: StatusCode,
    content_type: &str,
) {
    let path_without_query = actual_path.split('?').next().expect("path");
    let paths = contract["paths"].as_object().expect("OpenAPI paths object");
    let (template, path_item) = paths
        .iter()
        .find(|(template, _)| path_matches(template.as_str(), path_without_query))
        .unwrap_or_else(|| panic!("OpenAPI has no path matching {path_without_query}"));
    let method = method.to_ascii_lowercase();
    let operation = path_item[method.as_str()]
        .as_object()
        .unwrap_or_else(|| panic!("OpenAPI has no {method} operation for {template:?}"));
    let response = operation["responses"][status.as_u16().to_string()].clone();
    let response = dereference_response(contract, &response);
    let actual_media = media_part(content_type);
    let declared: Vec<String> = response["content"]
        .as_object()
        .map(|content| content.keys().map(|key| media_part(key)).collect())
        .unwrap_or_default();
    let matches = declared
        .iter()
        .any(|declared| declared == "*/*" || declared == &actual_media);
    assert!(
        matches,
        "{method} {actual_path} {status}: Content-Type '{content_type}' is not declared; \
         declared={declared:?}"
    );
}

fn response_schema(
    contract: &Value,
    method: &str,
    actual_path: &str,
    status: StatusCode,
    content_type: &str,
) -> Value {
    let path_without_query = actual_path.split('?').next().expect("path");
    let paths = contract["paths"].as_object().expect("OpenAPI paths object");
    let (template, path_item) = paths
        .iter()
        .find(|(template, _)| path_matches(template.as_str(), path_without_query))
        .unwrap_or_else(|| panic!("OpenAPI has no path matching {path_without_query}"));
    let method = method.to_ascii_lowercase();
    let operation = path_item[method.as_str()]
        .as_object()
        .unwrap_or_else(|| panic!("OpenAPI has no {method} operation for {template:?}"));
    let responses = operation
        .get("responses")
        .and_then(Value::as_object)
        .expect("OpenAPI responses object");
    let response = responses
        .get(&status.as_u16().to_string())
        .unwrap_or_else(|| {
            panic!(
                "OpenAPI has no {status} response for {method} {actual_path}; available={:?}",
                responses.keys().collect::<Vec<_>>()
            )
        });
    let response = dereference_response(contract, response);
    let content = response["content"].as_object().expect("response content");
    let (media_type, media) = content
        .iter()
        .find(|(declared, _)| media_type_matches(declared.as_str(), content_type))
        .unwrap_or_else(|| {
            panic!("OpenAPI has no content type {content_type} for {method} {actual_path} {status}")
        });
    let _ = media_type;
    dereference_schema(contract, &media["schema"])
}

fn path_matches(template: &str, actual: &str) -> bool {
    let template_parts: Vec<_> = template.split('/').collect();
    let actual_parts: Vec<_> = actual.split('/').collect();
    template_parts.len() == actual_parts.len()
        && template_parts
            .iter()
            .zip(actual_parts)
            .all(|(expected, actual)| expected.starts_with('{') || *expected == actual)
}

fn media_type_matches(declared: &str, actual: &str) -> bool {
    let declared = declared.to_ascii_lowercase();
    let actual = actual
        .split(';')
        .next()
        .expect("content type")
        .trim()
        .to_ascii_lowercase();
    declared == actual
        || declared == "*/*"
        || (declared.ends_with("/*") && actual.starts_with(declared.trim_end_matches('*')))
        || declared
            .split(';')
            .next()
            .expect("declared content type")
            .trim()
            == actual
}

fn dereference_response(contract: &Value, value: &Value) -> Value {
    let mut value = value.clone();
    while let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/components/responses/")
            .expect("response reference");
        value = contract["components"]["responses"][name].clone();
    }
    value
}

fn dereference_schema(contract: &Value, value: &Value) -> Value {
    let mut value = value.clone();
    while let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .expect("schema reference");
        value = contract["components"]["schemas"][name].clone();
    }
    value
}

fn replace_component_refs(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for child in object.values_mut() {
                replace_component_refs(child);
            }
            if let Some(Value::String(reference)) = object.get_mut("$ref") {
                *reference = reference.replace("#/components/schemas/", "#/$defs/");
            }
        }
        Value::Array(values) => {
            for child in values {
                replace_component_refs(child);
            }
        }
        _ => {}
    }
}

fn normalize_openapi_nullable(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for child in object.values_mut() {
                normalize_openapi_nullable(child);
            }
            if object.remove("nullable") == Some(Value::Bool(true)) {
                if let Some(Value::String(type_name)) = object.remove("type") {
                    object.insert(
                        "type".to_string(),
                        Value::Array(vec![
                            Value::String(type_name),
                            Value::String("null".to_string()),
                        ]),
                    );
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                normalize_openapi_nullable(child);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn metrics_endpoint_exposes_prometheus_text_without_high_cardinality_labels() {
    let fixture = Fixture::new();
    let response = fixture.request("GET", "/metrics", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .expect("content-type"),
        "text/plain; version=0.0.4"
    );
    let body = String::from_utf8(read_body(response).await).expect("utf8");
    assert!(body.contains("transist_runs_created_total"));
    assert!(body.contains("transist_usage_total_tokens_total"));
    assert!(!body.contains("run_id="));
    assert!(!body.contains("step_id="));
    assert_eq!(
        metric_sample_sum(&body, "transist_step_executions_started_total"),
        3.0
    );
    assert_eq!(
        metric_sample_sum(&body, "transist_step_executions_finished_total"),
        1.0
    );
}

#[tokio::test]
async fn metrics_endpoint_is_available_for_an_empty_home() {
    let fixture = Fixture::empty();
    let response = fixture.request("GET", "/metrics", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(read_body(response).await).expect("utf8");
    assert!(body.contains("# HELP transist_runs_created_total"));
    assert!(body.contains("# TYPE transist_runs_created_total counter"));
    assert_eq!(metric_sample_sum(&body, "transist_runs_created_total"), 0.0);
    assert_eq!(
        metric_sample_sum(&body, "transist_runs_finished_total"),
        0.0
    );
}

#[tokio::test]
async fn metrics_endpoint_rebuilds_partial_usage_and_validation_counters_consistently() {
    let mut result = sample_collect_result();
    result.usage = Some(InvokeUsage {
        provider: Some("openai".to_string()),
        model: Some("gpt-test".to_string()),
        input_tokens: Some(11),
        output_tokens: None,
        total_tokens: None,
    });
    let fixture = Fixture::with_collect_result(result, Some(2));

    let response = fixture.request("GET", "/metrics", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(read_body(response).await).expect("utf8");
    assert_eq!(
        metric_sample_sum(&body, "transist_usage_input_tokens_total"),
        11.0
    );
    assert_eq!(
        metric_sample_sum(&body, "transist_usage_output_tokens_total"),
        0.0
    );
    assert_eq!(
        metric_sample_sum(&body, "transist_usage_total_tokens_total"),
        0.0
    );
    assert_eq!(
        metric_sample_sum(&body, "transist_usage_reports_total"),
        1.0
    );
    assert_eq!(
        metric_sample_sum(&body, "transist_metrics_validation_errors_total"),
        2.0
    );

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["metrics"]["input_tokens"], 11);
    assert!(run["metrics"]["output_tokens"].is_null());
    assert!(run["metrics"]["total_tokens"].is_null());

    let step = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/inspect-run/steps/collect",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(step["metrics"]["input_tokens"], 11);
    assert!(step["metrics"]["output_tokens"].is_null());
    assert!(step["metrics"]["total_tokens"].is_null());
}

#[tokio::test]
async fn list_runs_filters_and_pagination_are_deterministic() {
    let fixture = Fixture::new();

    let first_page = read_json(
        fixture
            .request("GET", "/api/v1/runs?limit=1&offset=0", Body::empty())
            .await,
    )
    .await;
    assert_eq!(first_page["data"][0]["run_id"], "inspect-run");
    assert_eq!(first_page["pagination"]["has_more"], true);

    let second_page = read_json(
        fixture
            .request("GET", "/api/v1/runs?limit=1&offset=1", Body::empty())
            .await,
    )
    .await;
    assert_eq!(second_page["data"][0]["run_id"], "finished-run");

    let blocked = read_json(
        fixture
            .request("GET", "/api/v1/runs?status=blocked", Body::empty())
            .await,
    )
    .await;
    assert_eq!(blocked["data"].as_array().expect("runs").len(), 1);
    assert_eq!(blocked["data"][0]["run_id"], "inspect-run");

    let workflow = read_json(
        fixture
            .request("GET", "/api/v1/runs?workflow=server_demo@v1", Body::empty())
            .await,
    )
    .await;
    assert_eq!(workflow["data"].as_array().expect("runs").len(), 1);
    assert_eq!(workflow["data"][0]["run_id"], "inspect-run");

    let time_filtered = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs?created_after=2024-12-31T23:59:50Z",
                Body::empty(),
            )
            .await,
    )
    .await;
    assert_eq!(time_filtered["data"].as_array().expect("runs").len(), 2);
}

#[tokio::test]
async fn list_runs_ignores_runs_without_summary_sidecars() {
    let fixture = Fixture::new();
    let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
    let mut snapshot =
        KernelRunSnapshot::new("legacy-run", "legacy_demo", "v1", &["noop".to_string()]);
    snapshot.status = KernelRunStatus::Success;
    snapshot.steps.insert(
        "noop".to_string(),
        KernelStepState {
            step_id: "noop".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED - 100),
            finished_at_ms: Some(TS_CREATED - 50),
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "legacy-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED - 200,
        TS_CREATED - 25,
    );
    run_store.create_run(&record).expect("create legacy run");

    let runs = read_json(
        fixture
            .request("GET", "/api/v1/runs?limit=20", Body::empty())
            .await,
    )
    .await;
    assert!(
        runs["data"]
            .as_array()
            .expect("run rows")
            .iter()
            .all(|run| run["run_id"] != "legacy-run")
    );
}

#[tokio::test]
async fn list_runs_shows_only_top_level_runs_and_root_detail_contains_embedded_children() {
    let fixture = Fixture::with_nested_subflow_runs();

    let all_runs = read_json(
        fixture
            .request("GET", "/api/v1/runs?limit=10", Body::empty())
            .await,
    )
    .await;
    let all_ids = all_runs["data"]
        .as_array()
        .expect("all runs")
        .iter()
        .filter_map(|row| row["run_id"].as_str())
        .collect::<Vec<_>>();
    assert!(all_ids.contains(&"inspect-run"));
    assert!(all_ids.contains(&"finished-run"));
    assert!(all_ids.contains(&"replay-run"));
    assert!(!all_ids.contains(&"inspect-run__plan_loop"));
    assert!(!all_ids.contains(&"inspect-run__plan_loop__review_loop"));
    let inspect_root_row = all_runs["data"]
        .as_array()
        .expect("run rows")
        .iter()
        .find(|row| row["run_id"] == "inspect-run")
        .expect("inspect-run row");
    assert_eq!(inspect_root_row["metrics"]["total_artifacts_produced"], 2);
    assert_eq!(
        inspect_root_row["metrics"]["total_evidence_files_produced"],
        4
    );

    let all_scoped_runs = read_json(
        fixture
            .request("GET", "/api/v1/runs?limit=20&scope=all", Body::empty())
            .await,
    )
    .await;
    let all_scoped_ids = all_scoped_runs["data"]
        .as_array()
        .expect("all scoped runs")
        .iter()
        .filter_map(|row| row["run_id"].as_str())
        .collect::<Vec<_>>();
    assert!(all_scoped_ids.contains(&"inspect-run__plan_loop"));
    assert!(all_scoped_ids.contains(&"inspect-run__plan_loop__review_loop"));

    let root_run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(root_run["artifact_summary"]["count"], 1);
    assert_eq!(root_run["evidence_summary"]["count"], 2);
    assert_eq!(root_run["actionable_run_id"], Value::Null);
    let contained = root_run["contained_runs"]
        .as_array()
        .expect("contained runs");
    assert!(contained.iter().any(|row| {
        row["run_id"] == "inspect-run__plan_loop"
            && row["parent_run_id"] == "inspect-run"
            && row["artifact_count"] == 1
            && row["evidence_count"] == 1
    }));
    assert!(contained.iter().any(|row| {
        row["run_id"] == "inspect-run__plan_loop__review_loop"
            && row["parent_run_id"] == "inspect-run__plan_loop"
            && row["artifact_count"] == 0
            && row["evidence_count"] == 1
    }));
    assert!(!contained.iter().any(|row| row["run_id"] == "replay-run"));

    let child_run = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run__plan_loop", Body::empty())
            .await,
    )
    .await;
    assert_eq!(
        child_run["actionable_run_id"],
        "inspect-run__plan_loop__review_loop"
    );
    assert_eq!(child_run["root_run_id"], "inspect-run");
    assert_eq!(child_run["parent_run_id"], "inspect-run");
    assert_eq!(child_run["artifact_summary"]["count"], 1);
    assert_eq!(child_run["evidence_summary"]["count"], 1);
    let child_contained = child_run["contained_runs"]
        .as_array()
        .expect("child contained runs");
    assert!(child_contained.iter().any(|row| {
        row["run_id"] == "inspect-run__plan_loop__review_loop"
            && row["parent_run_id"] == "inspect-run__plan_loop"
            && row["artifact_count"] == 0
            && row["evidence_count"] == 1
    }));
}

#[tokio::test]
async fn failed_parent_with_resumed_child_projects_as_running_in_run_browser() {
    let fixture = Fixture::with_failed_parent_and_running_child();

    let runs = read_json(
        fixture
            .request("GET", "/api/v1/runs?status=running", Body::empty())
            .await,
    )
    .await;
    assert!(
        runs["data"]
            .as_array()
            .expect("running runs")
            .iter()
            .any(|row| { row["run_id"] == "resumed-root" && row["status"] == "running" })
    );

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/resumed-root", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["status"], "running");
    assert_eq!(run["current_step"]["step_id"], "plan_loop");
    assert!(run["terminal_outcome"].is_null());
    assert_eq!(run["actionable_run_id"], "resumed-root__plan_loop");
    assert!(
        run["contained_runs"]
            .as_array()
            .expect("contained runs")
            .iter()
            .any(|row| {
                row["run_id"] == "resumed-root__plan_loop" && row["status"] == "running"
            })
    );

    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/resumed-root/steps", Body::empty())
            .await,
    )
    .await;
    let plan_loop = steps["data"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|step| step["step_id"] == "plan_loop")
        .expect("plan_loop step");
    assert_eq!(plan_loop["status"], "running");
    assert_eq!(plan_loop["is_current_active"], true);
}

#[tokio::test]
async fn invalid_filters_fail_with_json_errors() {
    let fixture = Fixture::new();

    let bad_status = fixture
        .request("GET", "/api/v1/runs?status=unknown", Body::empty())
        .await;
    assert_eq!(bad_status.status(), StatusCode::BAD_REQUEST);
    let body = read_json(bad_status).await;
    assert_eq!(body["error"]["code"], "unsupported_query_value");

    let bad_time = fixture
        .request("GET", "/api/v1/runs?created_after=nope", Body::empty())
        .await;
    assert_eq!(bad_time.status(), StatusCode::BAD_REQUEST);
    let body = read_json(bad_time).await;
    assert_eq!(body["error"]["code"], "invalid_request");

    let bad_order = fixture
        .request(
            "GET",
            "/api/v1/runs/inspect-run/events?order=sideways",
            Body::empty(),
        )
        .await;
    assert_eq!(bad_order.status(), StatusCode::BAD_REQUEST);
    let body = read_json(bad_order).await;
    assert_eq!(body["error"]["code"], "unsupported_query_value");

    let bad_run_scope = fixture
        .request("GET", "/api/v1/runs?scope=sideways", Body::empty())
        .await;
    assert_eq!(bad_run_scope.status(), StatusCode::BAD_REQUEST);
    let body = read_json(bad_run_scope).await;
    assert_eq!(body["error"]["code"], "unsupported_query_value");

    let bad_blob_scope = fixture
        .request(
            "GET",
            "/api/v1/runs/inspect-run/artifacts?scope=sideways",
            Body::empty(),
        )
        .await;
    assert_eq!(bad_blob_scope.status(), StatusCode::BAD_REQUEST);
    let body = read_json(bad_blob_scope).await;
    assert_eq!(body["error"]["code"], "unsupported_query_value");
}

#[tokio::test]
async fn subtree_blob_scope_returns_descendant_outputs_with_owner_run_ids() {
    let fixture = Fixture::with_nested_subflow_runs();

    let direct_artifacts = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/artifacts", Body::empty())
            .await,
    )
    .await;
    assert_eq!(
        direct_artifacts["data"]
            .as_array()
            .expect("artifacts")
            .len(),
        1
    );
    assert_eq!(direct_artifacts["data"][0]["run_id"], "inspect-run");

    let subtree_artifacts = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/inspect-run/artifacts?scope=subtree",
                Body::empty(),
            )
            .await,
    )
    .await;
    let subtree_artifact_rows = subtree_artifacts["data"]
        .as_array()
        .expect("subtree artifacts");
    assert!(
        subtree_artifact_rows
            .iter()
            .any(|row| { row["run_id"] == "inspect-run" && row["producing_step_id"] == "collect" })
    );
    assert!(subtree_artifact_rows.iter().any(|row| {
        row["run_id"] == "inspect-run__plan_loop" && row["producing_step_id"] == "child"
    }));

    let direct_evidence = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/evidence", Body::empty())
            .await,
    )
    .await;
    assert_eq!(
        direct_evidence["data"].as_array().expect("evidence").len(),
        2
    );
    assert_eq!(direct_evidence["data"][0]["run_id"], "inspect-run");

    let subtree_evidence = read_json(
        fixture
            .request(
                "GET",
                "/api/v1/runs/inspect-run/evidence?scope=subtree",
                Body::empty(),
            )
            .await,
    )
    .await;
    let subtree_evidence_rows = subtree_evidence["data"]
        .as_array()
        .expect("subtree evidence");
    assert!(
        subtree_evidence_rows
            .iter()
            .any(|row| { row["run_id"] == "inspect-run" && row["producing_step_id"] == "collect" })
    );
    assert!(subtree_evidence_rows.iter().any(|row| {
        row["run_id"] == "inspect-run__plan_loop" && row["producing_step_id"] == "child"
    }));
    assert!(subtree_evidence_rows.iter().any(|row| {
        row["run_id"] == "inspect-run__plan_loop__review_loop"
            && row["producing_step_id"] == "child"
    }));
}

#[tokio::test]
async fn missing_resources_return_not_found() {
    let fixture = Fixture::new();

    let missing_run = fixture
        .request("GET", "/api/v1/runs/missing", Body::empty())
        .await;
    assert_eq!(missing_run.status(), StatusCode::NOT_FOUND);

    let missing_workflow = fixture
        .request("GET", "/api/v1/workflows/missing@v1", Body::empty())
        .await;
    assert_eq!(missing_workflow.status(), StatusCode::NOT_FOUND);

    let missing_artifact = fixture
        .request(
            "GET",
            &format!(
                "/api/v1/runs/inspect-run/artifacts/{}",
                encoded_resource_id("artifacts", "collect", 1, "missing.txt")
            ),
            Body::empty(),
        )
        .await;
    assert_eq!(missing_artifact.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn malformed_and_mismatched_resource_ids_fail_cleanly() {
    let fixture = Fixture::new();

    let malformed_artifact = fixture
        .request(
            "GET",
            "/api/v1/runs/inspect-run/artifacts/not-base64",
            Body::empty(),
        )
        .await;
    assert_eq!(malformed_artifact.status(), StatusCode::BAD_REQUEST);
    let body = read_json(malformed_artifact).await;
    assert_eq!(body["error"]["code"], "invalid_request");

    let wrong_family = fixture
        .request(
            "GET",
            &format!(
                "/api/v1/runs/inspect-run/artifacts/{}",
                encoded_resource_id("evidence", "collect", 1, "000_trace.log")
            ),
            Body::empty(),
        )
        .await;
    assert_eq!(wrong_family.status(), StatusCode::BAD_REQUEST);
    let body = read_json(wrong_family).await;
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn artifact_and_evidence_access_cannot_escape_the_trusted_model() {
    let fixture = Fixture::new();
    let attack_id = encoded_resource_id("artifacts", "collect", 1, "../state.json");

    let response = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/artifacts/{attack_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn injected_query_failures_are_sanitized_in_http_responses() {
    let app = app_with_query(Arc::new(FailingInspectionQuery));
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runs")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "storage_query_failure");
    assert_eq!(body["error"]["message"], "failed to load inspection data");
    let body_text = body.to_string();
    assert!(!body_text.contains("/tmp/hidden/state.json"));
    assert!(!body_text.contains("invalid serde payload"));
}

#[tokio::test]
async fn corrupted_persisted_state_returns_sanitized_storage_failure() {
    let fixture = Fixture::new();
    let corrupt_root = fixture.home_dir.join("runs/run_corrupt-run");
    let corrupt_locator =
        transist_storage_local::run_paths::plan_run_locator(&fixture.home_dir, "corrupt-run", None)
            .expect("plan corrupt locator");
    transist_storage_local::run_paths::write_run_locator(&fixture.home_dir, &corrupt_locator)
        .expect("write corrupt locator");
    fs::create_dir_all(&corrupt_root).expect("corrupt root");
    fs::write(corrupt_root.join("state.json"), b"{not-json").expect("corrupt state");

    let response = fixture
        .request("GET", "/api/v1/runs/corrupt-run", Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "storage_query_failure");
    assert_eq!(body["error"]["message"], "failed to load inspection data");
    assert!(!body.to_string().contains("corrupt-run/state.json"));
}

#[tokio::test]
async fn corrupted_events_return_sanitized_storage_failure() {
    let fixture = Fixture::new();
    fs::write(
        fixture.home_dir.join("runs/run_inspect-run/events.jsonl"),
        "{\"kind\":\"ok\"}\nnot-json\n",
    )
    .expect("corrupt events");

    let response = fixture
        .request("GET", "/api/v1/runs/inspect-run/events", Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "storage_query_failure");
    assert_eq!(body["error"]["message"], "failed to load inspection data");
    assert!(!body.to_string().contains("events.jsonl"));
}

#[tokio::test]
async fn read_requests_do_not_mutate_persisted_state() {
    let fixture = Fixture::new();
    // Use the snapshot taken before server app construction so any writes
    // during startup are also caught, not only writes from GET handlers.
    let before = fixture.pre_startup_snapshot.clone();

    let artifacts = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/artifacts", Body::empty())
            .await,
    )
    .await;
    let artifact_id = artifacts["data"][0]["artifact_id"]
        .as_str()
        .expect("artifact id");

    let evidence = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/evidence", Body::empty())
            .await,
    )
    .await;
    let evidence_id = evidence["data"][0]["evidence_id"]
        .as_str()
        .expect("evidence id");
    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/steps", Body::empty())
            .await,
    )
    .await;
    let workflow_file_id =
        steps["data"][0]["output_summary"]["workflow_files"][0]["workflow_file_id"]
            .as_str()
            .expect("workflow file id");

    let requests = [
        ("/health", StatusCode::OK),
        ("/metrics", StatusCode::OK),
        ("/inspect", StatusCode::SEE_OTHER),
        ("/inspect/runs", StatusCode::OK),
        ("/inspect/runs/inspect-run", StatusCode::OK),
        ("/inspect/runs/inspect-run?tab=steps", StatusCode::OK),
        ("/inspect/runs/inspect-run?tab=events", StatusCode::OK),
        ("/inspect/runs/inspect-run?tab=gates", StatusCode::OK),
        ("/inspect/runs/inspect-run?tab=artifacts", StatusCode::OK),
        ("/inspect/runs/inspect-run?tab=evidence", StatusCode::OK),
        (
            "/inspect/runs/inspect-run?tab=workflow-files",
            StatusCode::OK,
        ),
        ("/inspect/runs/inspect-run?tab=children", StatusCode::OK),
        ("/inspect/static/inspect.css", StatusCode::OK),
        ("/inspect/static/inspect.js", StatusCode::OK),
        ("/inspect/workflows", StatusCode::OK),
        ("/inspect/workflows/server_demo@v1", StatusCode::OK),
        ("/inspect/workflows/server_demo@v1/source", StatusCode::OK),
        ("/inspect/workflows/server_demo@v1/compiled", StatusCode::OK),
        ("/api/v1/runs", StatusCode::OK),
        ("/api/v1/runs/inspect-run", StatusCode::OK),
        ("/api/v1/runs/inspect-run/trust", StatusCode::OK),
        ("/api/v1/runs/inspect-run/locality", StatusCode::OK),
        ("/api/v1/runs/inspect-run/events", StatusCode::OK),
        ("/api/v1/runs/inspect-run/steps", StatusCode::OK),
        ("/api/v1/runs/inspect-run/steps/collect", StatusCode::OK),
        ("/api/v1/runs/inspect-run/gates", StatusCode::OK),
        ("/api/v1/runs/inspect-run/artifacts", StatusCode::OK),
        ("/api/v1/runs/inspect-run/evidence", StatusCode::OK),
        ("/api/v1/workflows", StatusCode::OK),
        ("/api/v1/workflows/server_demo@v1", StatusCode::OK),
        ("/api/v1/workflows/server_demo@v1/source", StatusCode::OK),
        (
            "/api/v1/workflows/server_demo@v1/compiled-recipe",
            StatusCode::OK,
        ),
    ];
    for (uri, expected) in requests {
        let response = fixture.request("GET", uri, Body::empty()).await;
        assert_eq!(response.status(), expected, "uri={uri}");
    }
    // Per-id JSON endpoints still must return 200 — keep the existing assertion intact.
    for uri in [
        format!("/api/v1/runs/inspect-run/artifacts/{artifact_id}"),
        format!("/api/v1/runs/inspect-run/artifacts/{artifact_id}/content"),
        format!("/api/v1/runs/inspect-run/evidence/{evidence_id}"),
        format!("/api/v1/runs/inspect-run/evidence/{evidence_id}/content"),
        format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}"),
        format!("/api/v1/runs/inspect-run/workflow-files/{workflow_file_id}/content"),
    ] {
        let response = fixture.request("GET", &uri, Body::empty()).await;
        assert_eq!(response.status(), StatusCode::OK, "uri={uri}");
    }

    let after = snapshot_tree(&fixture.home_dir);
    assert_eq!(before, after);
}

#[test]
fn server_load_fails_on_uninitialized_home_with_guidance() {
    // An uninitialized home (no installation.json) must cause Installation::load
    // to return an actionable error instead of silently creating the file.
    let tmp = test_tempdir();
    let home_dir = tmp.path().join("uninitialized");
    fs::create_dir_all(&home_dir).expect("create dir");
    // Deliberately do NOT call init_home_dir_with_mode so installation.json is absent.
    let err = transist_storage_local::Installation::load(&home_dir)
        .expect_err("should fail on missing record");
    let msg = err.to_string();
    assert!(
        msg.contains("transist home init"),
        "error must mention 'transist home init' to guide the user: {msg}"
    );
    // Confirm load did not create installation.json as a side-effect.
    assert!(
        !home_dir.join("installation.json").exists(),
        "load must not create installation.json"
    );
}

#[test]
fn openapi_object_schemas_forbid_additional_properties() {
    // additionalProperties: false (or a typed map for JsonValue).
    // This ensures that if a Rust DTO gains a new field not present in the
    // OpenAPI spec, the schema validation in assert_contract_http_response
    // will catch it — i.e., DTO/OpenAPI field drift fails CI.
    let contract = load_openapi_contract();
    let schemas = contract["components"]["schemas"]
        .as_object()
        .expect("OpenAPI components.schemas");
    let mut violations: Vec<String> = Vec::new();
    for (name, schema) in schemas {
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            continue;
        }
        let ap = schema.get("additionalProperties");
        // Allow true/false or a typed schema ($ref map for JsonValue).
        // Reject when the key is absent (additionalProperties defaults to true,
        // which means extra DTO fields would silently pass validation).
        if ap.is_none() {
            violations.push(name.clone());
        }
    }
    assert!(
        violations.is_empty(),
        "OpenAPI object schemas missing additionalProperties declaration (DTO drift would be \
         undetected): {violations:?}\n\
         Set additionalProperties: false on each schema to enforce DTO/OpenAPI parity."
    );
}

#[tokio::test]
async fn every_declared_openapi_route_and_status_is_exercised() {
    // Systematically exercise every (path, status) pair declared in the
    // OpenAPI spec. If a new route or status code is added to the spec but
    // not covered by this test, it will fail here.
    let fixture = Fixture::new();
    let documents = tokio::task::spawn_blocking(document_reviews::Documents::new)
        .await
        .unwrap();
    let document_id = documents.revisions[0]["revision_id"].as_str().unwrap();
    let failed_fixture = Fixture::with_failed_invoke_step();
    let invalid_source_fixture = Fixture::new();
    let invalid_text = b"not text: \xff private source";
    fs::write(
        invalid_source_fixture
            .repo_root
            .join("server_demo.flow.yaml"),
        invalid_text,
    )
    .expect("write invalid workflow source");
    let invalid_recipe_fixture = Fixture::new();
    fs::write(
        invalid_recipe_fixture
            .home_dir
            .join("registry/recipes/server_demo-v1.yaml"),
        invalid_text,
    )
    .expect("write invalid compiled recipe");
    let failing = app_with_query(Arc::new(FailingInspectionQuery));
    let contract = load_openapi_contract();
    let paths = contract["paths"].as_object().expect("OpenAPI paths");

    // Collect all (path_template, status_str) pairs declared in the spec.
    // For routes that require dynamic path parameters we build concrete
    // request paths from the fixture's known resource IDs.
    let artifacts = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/artifacts", Body::empty())
            .await,
    )
    .await;
    let artifact_id = artifacts["data"][0]["artifact_id"]
        .as_str()
        .expect("artifact id");
    let evidence = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/evidence", Body::empty())
            .await,
    )
    .await;
    let evidence_id = evidence["data"][0]["evidence_id"]
        .as_str()
        .expect("evidence id");
    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/steps", Body::empty())
            .await,
    )
    .await;
    let workflow_file_id =
        steps["data"][0]["output_summary"]["workflow_files"][0]["workflow_file_id"]
            .as_str()
            .expect("workflow file id");

    // Map every OpenAPI path template to a concrete status test
    // matrix, substituting known fixture IDs for path parameters. `Source`
    // selects which app serves the request: the seeded fixture, the failed-run
    // fixture (for failure-specific 200s), or the always-failing query app
    // (for every declared 500, so no status goes unexercised).
    let run_id = "inspect-run";
    let failed_run_id = "failed-invoke-run";
    let missing_run_id = "missing-run-xyz";
    let bad_artifact = "not-base64-xyz";
    let workflow_ref = "server_demo@v1";
    let bad_workflow_ref = "not-a-ref";
    let missing_workflow_ref = "missing@v1";

    enum Source<'a> {
        Documents(&'a document_reviews::Documents),
        Fixture(&'a Fixture),
        FailedFixture(&'a Fixture),
        InvalidTextFixture(&'a Fixture),
        Failing,
    }

    // For each path template in the spec, assert every declared status code.
    for (template, path_item) in paths {
        let responses = match path_item.get("get").and_then(|op| op.get("responses")) {
            Some(r) => r.as_object().expect("responses object"),
            None => continue,
        };
        let declared_statuses: Vec<u16> = responses
            .keys()
            .filter_map(|s| s.parse::<u16>().ok())
            .collect();

        for status_code in &declared_statuses {
            let expected = StatusCode::from_u16(*status_code).expect("valid status");

            // Build the concrete path and pick a fixture that produces this status.
            let (concrete_path, source): (String, Source) = match (template.as_str(), *status_code)
            {
                // --- /api/v1/runs ---
                ("/api/v1/runs", 200) => ("/api/v1/runs".into(), Source::Fixture(&fixture)),
                ("/api/v1/runs", 400) => (
                    "/api/v1/runs?limit=not-a-number".into(),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs", 500) => ("/api/v1/runs".into(), Source::Failing),
                // --- /api/v1/runs/{run_id} ---
                ("/api/v1/runs/{run_id}", 200) => {
                    (format!("/api/v1/runs/{run_id}"), Source::Fixture(&fixture))
                }
                ("/api/v1/runs/{run_id}", 404) => (
                    format!("/api/v1/runs/{missing_run_id}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}", 500) => {
                    (format!("/api/v1/runs/{run_id}"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/trust ---
                ("/api/v1/runs/{run_id}/trust", 200) => (
                    format!("/api/v1/runs/{run_id}/trust"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/trust", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/trust"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/trust", 500) => {
                    (format!("/api/v1/runs/{run_id}/trust"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/locality ---
                ("/api/v1/runs/{run_id}/locality", 200) => (
                    format!("/api/v1/runs/{run_id}/locality"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/locality", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/locality"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/locality", 500) => {
                    (format!("/api/v1/runs/{run_id}/locality"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/events ---
                ("/api/v1/runs/{run_id}/events", 200) => (
                    format!("/api/v1/runs/{run_id}/events"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/events", 400) => (
                    format!("/api/v1/runs/{run_id}/events?order=invalid"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/events", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/events"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/events", 500) => {
                    (format!("/api/v1/runs/{run_id}/events"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/steps ---
                ("/api/v1/runs/{run_id}/steps", 200) => (
                    format!("/api/v1/runs/{run_id}/steps"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/steps", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/steps"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/steps", 500) => {
                    (format!("/api/v1/runs/{run_id}/steps"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/steps/{step_id} ---
                ("/api/v1/runs/{run_id}/steps/{step_id}", 200) => (
                    format!("/api/v1/runs/{run_id}/steps/collect"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/steps/{step_id}", 400) => (
                    format!("/api/v1/runs/{run_id}/steps/collect?execution=0"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/steps/{step_id}", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/steps/collect"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/steps/{step_id}", 500) => (
                    format!("/api/v1/runs/{run_id}/steps/collect"),
                    Source::Failing,
                ),
                // --- /api/v1/runs/{run_id}/gates ---
                ("/api/v1/runs/{run_id}/gates", 200) => (
                    format!("/api/v1/runs/{run_id}/gates"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/gates", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/gates"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/gates", 500) => {
                    (format!("/api/v1/runs/{run_id}/gates"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/failures ---
                ("/api/v1/runs/{run_id}/failures", 200) => (
                    format!("/api/v1/runs/{run_id}/failures"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/failures", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/failures"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/failures", 500) => {
                    (format!("/api/v1/runs/{run_id}/failures"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/failures/{failure_id} ---
                ("/api/v1/runs/{run_id}/failures/{failure_id}", 200) => (
                    format!("/api/v1/runs/{failed_run_id}/failures/failure-exec-1"),
                    Source::FailedFixture(&failed_fixture),
                ),
                ("/api/v1/runs/{run_id}/failures/{failure_id}", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/failures/no-such"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/failures/{failure_id}", 500) => (
                    format!("/api/v1/runs/{run_id}/failures/failure-exec-1"),
                    Source::Failing,
                ),
                // --- /api/v1/runs/{run_id}/artifacts ---
                ("/api/v1/runs/{run_id}/artifacts", 200) => (
                    format!("/api/v1/runs/{run_id}/artifacts"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts", 400) => (
                    format!("/api/v1/runs/{run_id}/artifacts?scope=invalid"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/artifacts"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts", 500) => {
                    (format!("/api/v1/runs/{run_id}/artifacts"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/artifacts/{artifact_id} ---
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}", 200) => (
                    format!("/api/v1/runs/{run_id}/artifacts/{artifact_id}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}", 400) => (
                    format!("/api/v1/runs/{run_id}/artifacts/{bad_artifact}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/artifacts/{artifact_id}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}", 500) => (
                    format!("/api/v1/runs/{run_id}/artifacts/{artifact_id}"),
                    Source::Failing,
                ),
                // --- /api/v1/runs/{run_id}/artifacts/{artifact_id}/content ---
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}/content", 200) => (
                    format!("/api/v1/runs/{run_id}/artifacts/{artifact_id}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}/content", 400) => (
                    format!("/api/v1/runs/{run_id}/artifacts/{bad_artifact}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}/content", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/artifacts/{artifact_id}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/artifacts/{artifact_id}/content", 500) => (
                    format!("/api/v1/runs/{run_id}/artifacts/{artifact_id}/content"),
                    Source::Failing,
                ),
                // --- /api/v1/runs/{run_id}/evidence ---
                ("/api/v1/runs/{run_id}/evidence", 200) => (
                    format!("/api/v1/runs/{run_id}/evidence"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence", 400) => (
                    format!("/api/v1/runs/{run_id}/evidence?scope=invalid"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/evidence"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence", 500) => {
                    (format!("/api/v1/runs/{run_id}/evidence"), Source::Failing)
                }
                // --- /api/v1/runs/{run_id}/evidence/{evidence_id} ---
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}", 200) => (
                    format!("/api/v1/runs/{run_id}/evidence/{evidence_id}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}", 400) => (
                    format!("/api/v1/runs/{run_id}/evidence/{bad_artifact}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/evidence/{evidence_id}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}", 500) => (
                    format!("/api/v1/runs/{run_id}/evidence/{evidence_id}"),
                    Source::Failing,
                ),
                // --- /api/v1/runs/{run_id}/evidence/{evidence_id}/content ---
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}/content", 200) => (
                    format!("/api/v1/runs/{run_id}/evidence/{evidence_id}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}/content", 400) => (
                    format!("/api/v1/runs/{run_id}/evidence/{bad_artifact}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}/content", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/evidence/{evidence_id}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/evidence/{evidence_id}/content", 500) => (
                    format!("/api/v1/runs/{run_id}/evidence/{evidence_id}/content"),
                    Source::Failing,
                ),
                // --- /api/v1/runs/{run_id}/workflow-files/{workflow_file_id} ---
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}", 200) => (
                    format!("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}", 400) => (
                    format!("/api/v1/runs/{run_id}/workflow-files/{bad_artifact}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}", 404) => (
                    format!("/api/v1/runs/{missing_run_id}/workflow-files/{workflow_file_id}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}", 500) => (
                    format!("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}"),
                    Source::Failing,
                ),
                // --- /api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content ---
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content", 200) => (
                    format!("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content", 400) => (
                    format!("/api/v1/runs/{run_id}/workflow-files/{bad_artifact}/content"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content", 404) => (
                    format!(
                        "/api/v1/runs/{missing_run_id}/workflow-files/{workflow_file_id}/content"
                    ),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content", 500) => (
                    format!("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}/content"),
                    Source::Failing,
                ),
                // --- /api/v1/workflows ---
                ("/api/v1/workflows", 200) => {
                    ("/api/v1/workflows".into(), Source::Fixture(&fixture))
                }
                ("/api/v1/workflows", 400) => (
                    "/api/v1/workflows?limit=nan".into(),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows", 500) => ("/api/v1/workflows".into(), Source::Failing),
                // --- /api/v1/workflows/{workflow_ref} ---
                ("/api/v1/workflows/{workflow_ref}", 200) => (
                    format!("/api/v1/workflows/{workflow_ref}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}", 400) => (
                    format!("/api/v1/workflows/{bad_workflow_ref}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}", 404) => (
                    format!("/api/v1/workflows/{missing_workflow_ref}"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}", 500) => {
                    (format!("/api/v1/workflows/{workflow_ref}"), Source::Failing)
                }
                // --- /api/v1/workflows/{workflow_ref}/source ---
                ("/api/v1/workflows/{workflow_ref}/source", 200) => (
                    format!("/api/v1/workflows/{workflow_ref}/source"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/source", 400) => (
                    format!("/api/v1/workflows/{bad_workflow_ref}/source"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/source", 422) => (
                    format!("/api/v1/workflows/{workflow_ref}/source"),
                    Source::InvalidTextFixture(&invalid_source_fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/source", 404) => (
                    format!("/api/v1/workflows/{missing_workflow_ref}/source"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/source", 500) => (
                    format!("/api/v1/workflows/{workflow_ref}/source"),
                    Source::Failing,
                ),
                // --- /api/v1/workflows/{workflow_ref}/compiled-recipe ---
                ("/api/v1/workflows/{workflow_ref}/compiled-recipe", 200) => (
                    format!("/api/v1/workflows/{workflow_ref}/compiled-recipe"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/compiled-recipe", 400) => (
                    format!("/api/v1/workflows/{bad_workflow_ref}/compiled-recipe"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/compiled-recipe", 422) => (
                    format!("/api/v1/workflows/{workflow_ref}/compiled-recipe"),
                    Source::InvalidTextFixture(&invalid_recipe_fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/compiled-recipe", 404) => (
                    format!("/api/v1/workflows/{missing_workflow_ref}/compiled-recipe"),
                    Source::Fixture(&fixture),
                ),
                ("/api/v1/workflows/{workflow_ref}/compiled-recipe", 500) => (
                    format!("/api/v1/workflows/{workflow_ref}/compiled-recipe"),
                    Source::Failing,
                ),
                ("/api/v1/runs/{run_id}/documents/{revision_id}", code)
                | ("/api/v1/runs/{run_id}/documents/{revision_id}/content", code) => {
                    let suffix = if template.ends_with("/content") {
                        "/content"
                    } else {
                        ""
                    };
                    match code {
                        200 => (
                            format!("/api/v1/runs/review-run/documents/{document_id}{suffix}"),
                            Source::Documents(&documents),
                        ),
                        400 => (
                            format!("/api/v1/runs/review-run/documents/invalid{suffix}"),
                            Source::Documents(&documents),
                        ),
                        404 => (
                            format!("/api/v1/runs/missing-run/documents/{document_id}{suffix}"),
                            Source::Documents(&documents),
                        ),
                        500 => (
                            format!("/api/v1/runs/review-run/documents/{document_id}{suffix}"),
                            Source::Failing,
                        ),
                        _ => panic!("unmapped document status {code}"),
                    }
                }
                (path, code) => panic!(
                    "No test mapping for OpenAPI route ({path}, {code}). \
                         Add a case to every_declared_openapi_route_and_status_is_exercised."
                ),
            };

            match source {
                Source::Documents(fx) => {
                    let response = fx.request("GET", &concrete_path).await;
                    assert_contract_http_response(
                        &contract,
                        "GET",
                        &concrete_path,
                        response,
                        expected,
                    )
                    .await;
                }
                Source::Fixture(fx)
                | Source::FailedFixture(fx)
                | Source::InvalidTextFixture(fx) => {
                    assert_contract_response(fx, &contract, "GET", &concrete_path, expected).await;
                }
                Source::Failing => {
                    let response = failing
                        .clone()
                        .oneshot(
                            Request::builder()
                                .method("GET")
                                .uri(&concrete_path)
                                .body(Body::empty())
                                .expect("request"),
                        )
                        .await
                        .expect("response");
                    assert_contract_http_response(
                        &contract,
                        "GET",
                        &concrete_path,
                        response,
                        expected,
                    )
                    .await;
                }
            }
        }
    }
}

#[tokio::test]
async fn mutable_routes_are_absent() {
    let fixture = Fixture::new();
    let routes = [
        "/runs/start",
        "/runs/inspect-run/run",
        "/runs/inspect-run/resume",
        "/runs/inspect-run/replay",
        "/runs/inspect-run/gates/security-review/approve",
        "/runs/inspect-run/gates/security-review/reject",
        "/runs/inspect-run/gates/security-review/request-changes",
    ];
    for route in routes {
        let response = fixture.request("POST", route, Body::empty()).await;
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "route={route} status={}",
            response.status()
        );
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo_root: PathBuf,
    home_dir: PathBuf,
    /// Snapshot of the home directory taken after data seeding but before the
    /// server app is constructed. Used by the no-mutation test to assert that
    /// neither app construction nor GET request handling writes to the home.
    pre_startup_snapshot: BTreeMap<String, Vec<u8>>,
    app: axum::Router,
}

fn replace_collect_workflow_file_request(fixture: &Fixture, repo_path: Value, workflow_file: &str) {
    let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
    let mut record = run_store
        .load_run("inspect-run")
        .expect("load run")
        .expect("run");
    let step = record.steps.get_mut("collect").expect("collect step");
    let request = step.last_request.as_mut().expect("collect request");
    request.input = json!({
        "repo_path": repo_path,
        "response_file": workflow_file,
    });
    let result = step.last_result.as_mut().expect("collect result");
    result.output = json!({"spec_files": [workflow_file]});
    let request = step.last_request.clone();
    let result = step.last_result.clone();
    run_store
        .update_run_state("inspect-run", &record)
        .expect("update run state");

    let execution_path =
        transist_storage_local::run_paths::resolve_run_root(&fixture.home_dir, "inspect-run")
            .expect("run root")
            .join("step_executions/collect/execution_001.json");
    let mut execution: StepExecutionRecord =
        serde_json::from_slice(&fs::read(&execution_path).expect("read collect execution"))
            .expect("parse collect execution");
    execution.request = request;
    execution.result = result;
    fs::write(
        execution_path,
        serde_json::to_vec_pretty(&execution).expect("serialize collect execution"),
    )
    .expect("write collect execution");
}

async fn read_only_workflow_file(fixture: &Fixture, field_name: &str) -> (String, Vec<u8>) {
    let steps = read_json(
        fixture
            .request("GET", "/api/v1/runs/inspect-run/steps", Body::empty())
            .await,
    )
    .await;
    let file = steps["data"][0]["output_summary"]["workflow_files"]
        .as_array()
        .expect("workflow files")
        .iter()
        .find(|file| file["field_name"] == field_name)
        .expect("requested workflow file");
    let locator_kind = file["locator_kind"]
        .as_str()
        .expect("locator kind")
        .to_string();
    let file_id = file["workflow_file_id"].as_str().expect("workflow file id");
    let response = fixture
        .request(
            "GET",
            &format!("/api/v1/runs/inspect-run/workflow-files/{file_id}/content"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    (locator_kind, read_body(response).await)
}

impl Fixture {
    fn new() -> Self {
        Self::with_collect_result(sample_collect_result(), None)
    }

    fn empty() -> Self {
        let tmp = test_tempdir();
        let repo_root = tmp.path().join("repo");
        let home_dir = tmp.path().join("home");
        fs::create_dir_all(&repo_root).expect("repo root");
        transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
            .expect("init home");

        // Snapshot the home before the server app is constructed.
        let pre_startup_snapshot = snapshot_tree(&home_dir);

        Self {
            repo_root: repo_root.clone(),
            app: app_for_paths(repo_root, home_dir.clone()),
            pre_startup_snapshot,
            home_dir,
            _tmp: tmp,
        }
    }

    fn with_collect_result(
        result: transist_protocol::ExecutionResult,
        metrics_validation_errors: Option<u32>,
    ) -> Self {
        let tmp = test_tempdir();
        let repo_root = tmp.path().join("repo");
        let home_dir = tmp.path().join("home");
        fs::create_dir_all(&repo_root).expect("repo root");
        transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
            .expect("init home");

        let demo_flow = repo_root.join("server_demo.flow.yaml");
        fs::write(
            &demo_flow,
            r#"
schema: transist.flow.v1
id: server_demo
version: v1
inputs:
  prompt:
    type: string
    required: false
steps:
  - id: collect
    control: control.noop
  - id: review
    control: control.gate.manual
    with:
      gate_id: security-review
      safe: gate-kept
      nested:
         path: /__USER_HOME__/alice/gate-request.json
"#
            .replace("__USER_HOME__", "Users"),
        )
        .expect("demo flow");
        let graph_child_flow = repo_root.join("modules/graph_child.yaml");
        fs::create_dir_all(graph_child_flow.parent().expect("module parent"))
            .expect("graph module directory");
        fs::write(
            &graph_child_flow,
            r#"
schema: transist.module.v1
id: graph_child
version: v1
inputs:
  child_ticket:
    type: string
    required: false
steps:
  - id: child_collect
    control: control.noop
"#,
        )
        .expect("graph child flow");
        let graph_demo_flow = repo_root.join("graph_demo.flow.yaml");
        fs::write(
            &graph_demo_flow,
            r#"
schema: transist.flow.v1
id: graph_demo
version: v1
modules:
  graph_child:
    path: modules/graph_child.yaml
inputs:
  jira_ticket:
    type: string
    required: true
  dry_run:
    type: boolean
    required: false
steps:
  - id: branch
    control: control.noop
    with:
      ticket: "{{inputs.jira_ticket}}"
    if:
      guard: inputs.dry_run == true
      then:
        - ref: dry_path
      else:
        - ref: live_path
  - id: dry_path
    control: control.noop
  - id: live_path
    module:
      ref: graph_child
      inputs:
        child_ticket: "{{inputs.jira_ticket}}"
  - id: review_loop
    control: control.noop
    with:
      mode: gated
    loop:
      while: inputs.dry_run == false
      body:
        - ref: revise
      max_iters: 3
      max_duration: 5m
  - id: revise
    control: control.noop
"#,
        )
        .expect("graph demo flow");
        let source_only_flow = repo_root.join("source_only.flow.yaml");
        fs::write(
            &source_only_flow,
            r#"
schema: transist.flow.v1
id: source_only
version: v1
steps:
  - id: noop
    control: control.noop
"#,
        )
        .expect("source-only flow");
        let invalid_flow = repo_root.join("invalid_source.flow.yaml");
        fs::write(
            &invalid_flow,
            r#"
schema: transist.flow.v1
id: invalid_source
version: v1
plugins:
  codex.cli: 0.1.0
steps:
  - id: ask
    uses: process:codex.cli/exec_task
    prompt: "say hello"
"#,
        )
        .expect("invalid flow");
        let compiled_only_flow = repo_root.join("compiled_only.flow.yaml");
        fs::write(
            &compiled_only_flow,
            r#"
schema: transist.flow.v1
id: compiled_only
version: v1
steps:
  - id: noop
    control: control.noop
    with: {}
"#,
        )
        .expect("compiled-only flow");

        let context = transist::TransistContext::with_home(&repo_root, &home_dir);
        context.compile_flow_file(&demo_flow).expect("compile");
        context
            .compile_flow_file(&graph_demo_flow)
            .expect("compile graph demo");
        context
            .compile_flow_file(&compiled_only_flow)
            .expect("compile compiled-only");
        fs::remove_file(&compiled_only_flow).expect("remove compiled-only source");

        let cfg = LocalStorageConfig::new(&home_dir);
        let run_store = LocalRunStore::new(cfg.clone());
        let events = LocalEventSink::new(cfg);
        let summaries = LocalRunSummaryStore::new(&home_dir);
        seed_blocked_run(
            &run_store,
            &events,
            &summaries,
            &repo_root,
            &home_dir,
            result,
            metrics_validation_errors,
        );
        seed_finished_run(&run_store, &summaries);

        // Snapshot the home after data seeding but before the server app is
        // constructed, so the no-mutation test covers both app startup and
        // GET request handling.
        let pre_startup_snapshot = snapshot_tree(&home_dir);

        Self {
            repo_root: repo_root.clone(),
            app: app_for_paths(repo_root, home_dir.clone()),
            pre_startup_snapshot,
            home_dir,
            _tmp: tmp,
        }
    }

    fn with_replayed_run() -> Self {
        let fixture = Self::with_collect_result(sample_collect_result(), None);
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_replayed_run(&run_store, &summaries);
        fixture
    }

    /// Fixture with a root run ("subflow-run") that has a "plan_loop" step
    /// whose result links to a child run ("subflow-run__plan_loop"). Used to
    /// test the embedded child-run overview on the subflow step detail page.
    fn with_subflow_step() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_run_with_subflow_step(&run_store, &summaries);
        seed_child_run(
            &run_store,
            &summaries,
            "subflow-run__plan_loop",
            "plan_loop_wf",
            "v1",
            KernelRunStatus::Blocked,
            TS_UPDATED + 1_000,
        );
        fixture
    }

    fn with_nested_subflow_runs() -> Self {
        let mut fixture = Self::with_replayed_run();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_child_run(
            &run_store,
            &summaries,
            "inspect-run__plan_loop",
            "server_demo_plan_loop",
            "v1",
            KernelRunStatus::Failed,
            TS_UPDATED + 1_000,
        );
        seed_child_run(
            &run_store,
            &summaries,
            "inspect-run__plan_loop__review_loop",
            "server_demo_review_loop",
            "v1",
            KernelRunStatus::Running,
            TS_UPDATED + 2_000,
        );
        seed_run_blob(
            &fixture.home_dir,
            SeedRunBlob {
                run_id: "inspect-run__plan_loop",
                step_id: "child",
                execution: 1,
                family: "artifacts",
                file_name: "000_plan.md",
                contents: b"child plan",
                invoke_id: "invoke_child_plan",
                sha256: "child-plan-sha",
            },
        );
        seed_run_blob(
            &fixture.home_dir,
            SeedRunBlob {
                run_id: "inspect-run__plan_loop",
                step_id: "child",
                execution: 1,
                family: "evidence",
                file_name: "transist_runtime_invoke_evidence.json",
                contents: br#"{"schema":"transist.runtime.invoke_evidence.v1","status":"failed"}"#,
                invoke_id: "invoke_child_plan",
                sha256: "child-plan-evidence-sha",
            },
        );
        seed_run_blob(
            &fixture.home_dir,
            SeedRunBlob {
                run_id: "inspect-run__plan_loop__review_loop",
                step_id: "child",
                execution: 1,
                family: "evidence",
                file_name: "transist_runtime_invoke_evidence.json",
                contents: br#"{"schema":"transist.runtime.invoke_evidence.v1","status":"running"}"#,
                invoke_id: "invoke_review_loop",
                sha256: "review-loop-evidence-sha",
            },
        );
        summaries
            .refresh_run("inspect-run__plan_loop")
            .expect("refresh plan loop summary");
        summaries
            .refresh_run("inspect-run__plan_loop__review_loop")
            .expect("refresh review loop summary");
        fixture.app = app_for_paths(fixture.repo_root.clone(), fixture.home_dir.clone());
        fixture
    }

    fn with_failed_parent_and_running_child() -> Self {
        let fixture = Self::new();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_failed_parent_with_running_child(&run_store, &summaries);
        fixture
    }

    /// Fixture with a root run ("failed-invoke-run") containing a single
    /// failed `invoke` step ("exec") with both `last_request` and
    /// `last_result` populated so plugin/adapter/action and error
    /// code/message flow through to the inspection layer. Used to test the
    /// failure-first step detail view.
    fn with_failed_invoke_step() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_failed_invoke_step(&run_store, &summaries);
        fixture
    }

    fn with_failed_preflight_run() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_failed_preflight_run(&run_store, &summaries);
        fixture
    }

    /// Seeds a 2-run tree where the parent run has a failed *subflow* step
    /// whose own `last_result.error` is empty, but a descendant child run
    /// contains the real root-cause failure with a populated `error` block.
    /// Used to exercise the actionable-descendant walk on the run-detail
    /// status banner.
    fn with_failed_subflow_with_descendant_error() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_failed_subflow_with_descendant_error(&run_store, &summaries);
        fixture
    }

    /// Used to exercise the durable `output.failure` mapping path that the
    /// kernel emits alongside the parent subflow step result. The child run
    /// still holds the raw execution error, but the parent's own step output
    /// already carries the compact `failure` descriptor so the inspection
    /// surfaces can render it without recomputation.
    fn with_failed_subflow_carrying_persisted_child_failure() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_failed_subflow_with_persisted_child_failure(&run_store, &summaries);
        fixture
    }

    fn with_bypassed_trust_run() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_bypassed_trust_run(&run_store, &summaries, &fixture.home_dir);
        fixture
    }

    fn with_denied_trust_run() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_denied_trust_run(&run_store, &summaries, &fixture.home_dir);
        fixture
    }

    fn with_local_only_run() -> Self {
        let fixture = Self::empty();
        let run_store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
        let summaries = LocalRunSummaryStore::new(&fixture.home_dir);
        seed_local_only_run(&run_store, &summaries, &fixture.home_dir);
        fixture
    }

    async fn request(&self, method: &str, uri: &str, body: Body) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(body)
                    .expect("request"),
            )
            .await
            .expect("response")
    }
}

#[tokio::test]
async fn bypassed_trust_decision_is_distinct_from_verified_and_denied() {
    let fixture = Fixture::with_bypassed_trust_run();

    let trust = read_json(
        fixture
            .request("GET", "/api/v1/runs/bypass-run/trust", Body::empty())
            .await,
    )
    .await;
    assert_eq!(trust["trusted_execution_mode"], true);
    assert_eq!(trust["workflow"]["signature_status"], "bypassed");
    assert_eq!(trust["decision"]["outcome"], "bypassed");
    assert_ne!(trust["decision"]["outcome"], "verified");
    assert_ne!(trust["decision"]["outcome"], "denied");
    assert_eq!(trust["plugins"][0]["signature_status"], "bypassed");
    assert_eq!(
        trust["policy_decision"]["reason_code"],
        "test_version_bypass"
    );

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/bypass-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["trust"]["decision"]["outcome"], "bypassed");

    let overview = read_text(
        fixture
            .request("GET", "/inspect/runs/bypass-run", Body::empty())
            .await,
    )
    .await;
    assert!(
        overview.contains("bypassed"),
        "SSR should render the bypass outcome"
    );
}

#[tokio::test]
async fn denied_trust_decision_surfaces_reason_and_verification_type() {
    let fixture = Fixture::with_denied_trust_run();

    let trust = read_json(
        fixture
            .request("GET", "/api/v1/runs/denied-run/trust", Body::empty())
            .await,
    )
    .await;
    assert_eq!(trust["decision"]["outcome"], "denied");
    assert_eq!(trust["decision"]["reason"], "workflow_digest_mismatch");
    assert_eq!(
        trust["decision"]["failed_verification_type"],
        "workflow_signature"
    );
    assert_eq!(trust["workflow"]["signature_status"], "denied");
    assert_eq!(trust["policy_decision"]["allowed"], false);

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/denied-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(run["trust"]["decision"]["outcome"], "denied");
    assert_eq!(
        run["trust"]["decision"]["reason"],
        "workflow_digest_mismatch"
    );
}

#[tokio::test]
async fn local_only_run_shows_empty_external_transfers() {
    let fixture = Fixture::with_local_only_run();

    let locality = read_json(
        fixture
            .request("GET", "/api/v1/runs/local-only-run/locality", Body::empty())
            .await,
    )
    .await;
    assert_eq!(
        locality["external_transfers"]
            .as_array()
            .expect("external_transfers array")
            .len(),
        0
    );
    assert_eq!(locality["steps"][0]["provider_kind"], "local");
    assert_eq!(locality["steps"][0]["provider"], "local-tool");

    let run = read_json(
        fixture
            .request("GET", "/api/v1/runs/local-only-run", Body::empty())
            .await,
    )
    .await;
    assert_eq!(
        run["locality"]["external_transfers"]
            .as_array()
            .expect("external_transfers array")
            .len(),
        0
    );

    let overview = read_text(
        fixture
            .request("GET", "/inspect/runs/local-only-run", Body::empty())
            .await,
    )
    .await;
    assert!(
        overview.contains("external_transfers: []"),
        "SSR should render the empty-transfers proof text"
    );
}

#[derive(Default)]
struct FailingInspectionQuery;

const FAILING_QUERY_MESSAGE: &str = "failed reading /tmp/hidden/state.json: invalid serde payload";

impl FailingInspectionQuery {
    fn err<T>() -> anyhow::Result<T> {
        Err(anyhow::anyhow!(FAILING_QUERY_MESSAGE))
    }
}

impl transist::InspectionQueryPort for FailingInspectionQuery {
    fn get_document(
        &self,
        _run_id: &str,
        _revision_id: &str,
    ) -> anyhow::Result<Option<transist::InspectionDocument>> {
        Self::err()
    }

    fn read_document_content(
        &self,
        _run_id: &str,
        _revision_id: &str,
        _member_id: Option<&str>,
    ) -> anyhow::Result<Option<transist::InspectionBlobContent>> {
        Self::err()
    }
    fn list_runs(
        &self,
        _query: &transist::RunListQuery,
    ) -> anyhow::Result<transist::Page<transist::InspectionRunSummary>> {
        Self::err()
    }

    fn get_run(&self, _run_id: &str) -> anyhow::Result<Option<transist::InspectionRunDetail>> {
        Self::err()
    }

    fn list_events(
        &self,
        _run_id: &str,
        _query: &transist::EventListQuery,
    ) -> anyhow::Result<Option<transist::Page<transist::InspectionEvent>>> {
        Self::err()
    }

    fn list_steps(&self, _run_id: &str) -> anyhow::Result<Option<Vec<transist::InspectionStep>>> {
        Self::err()
    }

    fn get_step(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> anyhow::Result<Option<transist::InspectionStep>> {
        Self::err()
    }

    fn list_step_executions(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> anyhow::Result<Option<Vec<transist::InspectionStepExecution>>> {
        Self::err()
    }

    fn list_gates(&self, _run_id: &str) -> anyhow::Result<Option<Vec<transist::InspectionGate>>> {
        Self::err()
    }

    fn list_failures(&self, _run_id: &str) -> anyhow::Result<Option<Vec<transist::FailureRecord>>> {
        Self::err()
    }

    fn get_failure(
        &self,
        _run_id: &str,
        _failure_id: &str,
    ) -> anyhow::Result<Option<transist::FailureRecord>> {
        Self::err()
    }

    fn list_blobs(
        &self,
        _run_id: &str,
        _family: transist::BlobFamily,
        _scope: transist::BlobListScope,
    ) -> anyhow::Result<Option<Vec<transist::InspectionBlob>>> {
        Self::err()
    }

    fn get_blob(
        &self,
        _run_id: &str,
        _family: transist::BlobFamily,
        _step_id: &str,
        _execution: u32,
        _relative_path: &str,
    ) -> anyhow::Result<Option<transist::InspectionBlob>> {
        Self::err()
    }

    fn read_blob_content(
        &self,
        _run_id: &str,
        _family: transist::BlobFamily,
        _step_id: &str,
        _execution: u32,
        _relative_path: &str,
    ) -> anyhow::Result<Option<transist::InspectionBlobContent>> {
        Self::err()
    }

    fn get_workflow_file(
        &self,
        _run_id: &str,
        _step_id: &str,
        _locator_kind: transist::InspectionWorkflowFileLocatorKind,
        _relative_path: &str,
    ) -> anyhow::Result<Option<transist::InspectionWorkflowFileRef>> {
        Self::err()
    }

    fn read_workflow_file_content(
        &self,
        _run_id: &str,
        _step_id: &str,
        _locator_kind: transist::InspectionWorkflowFileLocatorKind,
        _relative_path: &str,
        _workspace_root: Option<&std::path::Path>,
    ) -> anyhow::Result<Option<transist::InspectionWorkflowFileContent>> {
        Self::err()
    }

    fn list_workflows(
        &self,
        _query: &transist::PaginationQuery,
    ) -> anyhow::Result<transist::Page<transist::InspectionWorkflowSummary>> {
        Self::err()
    }

    fn get_workflow(
        &self,
        _workflow_ref: &str,
    ) -> anyhow::Result<Option<transist::InspectionWorkflowDetail>> {
        Self::err()
    }

    fn read_workflow_source(
        &self,
        _workflow_ref: &str,
    ) -> anyhow::Result<Option<transist::InspectionWorkflowContent>> {
        Self::err()
    }

    fn read_workflow_compiled_recipe(
        &self,
        _workflow_ref: &str,
    ) -> anyhow::Result<Option<transist::InspectionWorkflowContent>> {
        Self::err()
    }
}

fn seed_blocked_run(
    run_store: &LocalRunStore,
    events: &LocalEventSink,
    summaries: &LocalRunSummaryStore,
    repo_root: &Path,
    home_dir: &Path,
    result: transist_protocol::ExecutionResult,
    metrics_validation_errors: Option<u32>,
) {
    let run_root = home_dir.join("runs/run_inspect-run");
    let artifacts_dir = run_root.join("artifacts/collect/execution_001/artifacts");
    let evidence_dir = run_root.join("artifacts/collect/execution_001/evidence");
    let mut request = sample_collect_request();
    request.paths.run_root = run_root.display().to_string();
    request.paths.artifacts_dir = artifacts_dir.display().to_string();
    request.paths.evidence_dir = evidence_dir.display().to_string();
    request.paths.workspace_dir = repo_root.display().to_string();
    request.policy = json!({
        "filesystem": {
            "source": "workflow",
            "workspace_root": run_root.display().to_string(),
            "workspace_access": { "mode": "read_only" }
        }
    });
    if let Some(input) = request.input.as_object_mut() {
        let workflow_file_path = repo_root.join("server_demo/reports/nested/collect.md");
        input.insert(
            "output_file".to_string(),
            Value::String(workflow_file_path.display().to_string()),
        );
        input.insert(
            "report_file".to_string(),
            Value::String(workflow_file_path.display().to_string()),
        );
    }

    let mut snapshot = KernelRunSnapshot::new(
        "inspect-run",
        "server_demo",
        "v1",
        &["collect".to_string(), "review".to_string()],
    );
    snapshot.status = KernelRunStatus::Blocked;
    snapshot.steps.insert(
        "collect".to_string(),
        KernelStepState {
            step_id: "collect".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: Some(request.clone()),
            last_result: Some(result.clone()),
            started_at_ms: Some(TS_COLLECT_STARTED),
            finished_at_ms: Some(TS_COLLECT_FINISHED),
            activation_executions_baseline: 0,
        },
    );
    snapshot.steps.insert(
        "review".to_string(),
        KernelStepState {
            step_id: "review".to_string(),
            status: KernelStepStatus::Blocked,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_GATE_STARTED),
            finished_at_ms: None,
            activation_executions_baseline: 0,
        },
    );
    let mut record = PersistedRunRecord::from_snapshot(
        snapshot,
        Some(RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: "recipe-sha".to_string(),
        }),
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "inspect-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::from([
            (
                "prompt".to_string(),
                KernelValue::String("hello".to_string()),
            ),
            (
                "safe".to_string(),
                KernelValue::String("preserve-me".to_string()),
            ),
            (
                ("/".to_string() + "Users/alice/key"),
                KernelValue::Number(1.into()),
            ),
            (
                "C:\\Users\\alice\\key".to_string(),
                KernelValue::Number(2.into()),
            ),
            ("[path redacted]".to_string(), KernelValue::Number(3.into())),
            (
                "paths".to_string(),
                KernelValue::Object(BTreeMap::from([
                    (
                        "posix".to_string(),
                        KernelValue::String("/".to_string() + "Users/alice/private/token"),
                    ),
                    (
                        "windows".to_string(),
                        KernelValue::String("./C:\\Users\\alice\\secret.txt".to_string()),
                    ),
                    (
                        "home".to_string(),
                        KernelValue::String("./~/.ssh/id_rsa".to_string()),
                    ),
                    (
                        "traversal".to_string(),
                        KernelValue::String("./../secrets/token".to_string()),
                    ),
                ])),
            ),
        ]),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    record.trusted_execution_mode = true;
    record.trust_decision = Some(TrustDecisionRecord {
        schema: "transist.trust_decision.v1".to_string(),
        run_id: "inspect-run".to_string(),
        trusted_execution_mode: true,
        decision: "allowed".to_string(),
        workflow: TrustSubjectDecision {
            id: "server_demo".to_string(),
            version: "v1".to_string(),
            digest: "workflow-digest-123".to_string(),
            signature_verified: true,
            signer_key_id: Some("wkid_demo".to_string()),
            reason_code: None,
            trust_decision: "verified".to_string(),
        },
        plugins: vec![TrustSubjectDecision {
            id: "shell".to_string(),
            version: "1.0.0".to_string(),
            digest: "plugin-digest-456".to_string(),
            signature_verified: true,
            signer_key_id: Some("pkid_shell".to_string()),
            reason_code: None,
            trust_decision: "verified".to_string(),
        }],
        reason_code: None,
        failed_verification_type: None,
        organization_id: None,
        policy_revision: None,
        decided_at_ms: TS_COLLECT_FINISHED,
    });
    run_store.create_run(&record).expect("create blocked run");
    let recipe = fs::read_to_string(home_dir.join("registry/recipes/server_demo-v1.yaml"))
        .expect("compiled recipe");
    run_store
        .write_recipe_snapshot("inspect-run", &recipe)
        .expect("write recipe snapshot");

    let execution_record = StepExecutionRecord {
        run_id: "inspect-run".to_string(),
        step_id: "collect".to_string(),
        execution: 1,
        status: "success".to_string(),
        started_at_ms: Some(TS_COLLECT_STARTED),
        finished_at_ms: Some(TS_COLLECT_FINISHED),
        retry_wait_until_ms: None,
        request: Some(request),
        result: Some(result),
        metrics_validation_errors,
        state_mutations: Vec::new(),
        document_revisions: Vec::new(),
    };
    run_store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "inspect-run".to_string(),
            step_id: "collect".to_string(),
            execution: 1,
            state: serde_json::to_value(execution_record)
                .expect("step execution json")
                .into(),
        })
        .expect("store execution");

    fs::create_dir_all(&artifacts_dir).expect("artifact dir");
    let artifact_path = artifacts_dir.join("000_output.txt");
    fs::write(&artifact_path, b"hello from artifact").expect("artifact bytes");
    fs::write(
        artifacts_dir.join("000_000_output.txt.json"),
        serde_json::to_vec_pretty(&json!({
            "kind": "artifacts",
            "source_path": artifact_path.display().to_string(),
            "sha256": "artifact-sha",
            "bytes": 19,
            "status": "success",
            "invoke_id": "invoke_collect",
            "execution": 1
        }))
        .expect("artifact descriptor"),
    )
    .expect("artifact descriptor write");

    fs::create_dir_all(&evidence_dir).expect("evidence dir");
    let evidence_path = evidence_dir.join("000_trace.log");
    fs::write(&evidence_path, b"trace line").expect("evidence bytes");
    fs::write(
        evidence_dir.join("000_000_trace.log.json"),
        serde_json::to_vec_pretty(&json!({
            "kind": "evidence",
            "source_path": evidence_path.display().to_string(),
            "sha256": "evidence-sha",
            "bytes": 10,
            "status": "success",
            "invoke_id": "invoke_collect",
            "execution": 1
        }))
        .expect("evidence descriptor"),
    )
    .expect("evidence descriptor write");
    let transfer_path = evidence_dir.join("transist_external_transfer_001.json");
    fs::write(
        &transfer_path,
        serde_json::to_vec_pretty(&json!({
            "schema": "transist.external_transfer.v1",
            "run_id": "inspect-run",
            "step_id": "collect",
            "execution": 1,
            "timestamp_ms": TS_COLLECT_FINISHED,
            "destination": "api.openai.com",
            "adapter_kind": "process",
            "provider": "openai",
            "provider_kind": "external",
            "content_hash": "payload-hash-789",
            "redaction_applied": false,
            "policy_decision": {
                "allowed": true,
                "mode": "trusted_execution"
            }
        }))
        .expect("external transfer evidence"),
    )
    .expect("external transfer write");
    fs::write(
        evidence_dir.join("000_transist_external_transfer_001.json.json"),
        serde_json::to_vec_pretty(&json!({
            "kind": "evidence",
            "source_path": transfer_path.display().to_string(),
            "sha256": "external-transfer-sha",
            "bytes": fs::metadata(&transfer_path).expect("external transfer metadata").len(),
            "status": "success",
            "invoke_id": "invoke_collect",
            "execution": 1
        }))
        .expect("external transfer descriptor"),
    )
    .expect("external transfer descriptor write");

    let workflow_file_path = repo_root.join("server_demo/reports/nested/collect.md");
    fs::create_dir_all(workflow_file_path.parent().expect("workflow file parent"))
        .expect("workflow file dir");
    fs::write(&workflow_file_path, b"hello from workflow file").expect("workflow file bytes");

    let gates_dir = run_root.join("gates");
    fs::create_dir_all(&gates_dir).expect("gates dir");
    fs::write(
        gates_dir.join("security-review.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "transist.gate_decision.v1",
            "run_id": "inspect-run",
            "gate_id": "security-review",
            "step_id": "review",
            "decision": "approved",
            "actor": "alice@example.com",
            "actor_source": "flag",
            "reason": "reviewed generated plan",
            "decided_at_ms": TS_UPDATED
        }))
        .expect("gate decision json"),
    )
    .expect("gate decision write");

    summaries
        .refresh_run("inspect-run")
        .expect("refresh inspect-run summary");
    write_provenance(
        home_dir,
        &RunProvenanceRecord {
            schema: RUN_PROVENANCE_SCHEMA_V1.to_string(),
            run_id: "inspect-run".to_string(),
            workflow_id: "server_demo".to_string(),
            workflow_version: "v1".to_string(),
            workflow_digest: Some("workflow-digest-123".to_string()),
            workflow_signature_verified: true,
            workflow_signer_key_id: Some("wkid_demo".to_string()),
            workflow_trust_decision: "verified".to_string(),
            plugins: vec![RunProvenanceSubject {
                id: "shell".to_string(),
                version: "1.0.0".to_string(),
                digest: "plugin-digest-456".to_string(),
                signature_verified: true,
                signer_key_id: Some("pkid_shell".to_string()),
                trust_decision: "verified".to_string(),
            }],
            trusted_execution_mode: true,
            local_only_mode: false,
            runtime_mode: "governed".to_string(),
            policy_profile: "allowlist".to_string(),
            capability_enforcement: "enforced".to_string(),
            policy_decision: RunProvenancePolicyDecision {
                allowed: true,
                reason_code: Some("trusted_execution_verified".to_string()),
                message: Some("workflow and plugin signatures verified".to_string()),
                mode: "run_start".to_string(),
            },
            providers: vec![transist_core::RunProviderProvenance {
                step_id: "collect".to_string(),
                provider: Some("openai".to_string()),
                model: Some("gpt-test".to_string()),
                provider_kind: Some("external".to_string()),
            }],
            external_transfer_summary: RunExternalTransferSummary {
                count: 1,
                external_transfers: Vec::new(),
            },
            approval_summary: vec![RunApprovalProvenance {
                gate_id: "security-review".to_string(),
                decision: "approved".to_string(),
                actor: Some("alice@example.com".to_string()),
                reason: Some("reviewed generated plan".to_string()),
                decided_at_ms: Some(TS_UPDATED),
            }],
            decision: "allowed".to_string(),
            denied: None,
            organization_id: None,
            policy_revision: None,
            created_at_ms: TS_CREATED,
            finalized_at_ms: Some(TS_UPDATED),
        },
    );

    for event in [
        RuntimeEvent {
            kind: "run_created".to_string(),
            run_id: "inspect-run".to_string(),
            step_id: None,
            timestamp_unix_ms: TS_CREATED,
            payload: KernelValue::Null,
        },
        RuntimeEvent {
            kind: "step_succeeded".to_string(),
            run_id: "inspect-run".to_string(),
            step_id: Some("collect".to_string()),
            timestamp_unix_ms: TS_COLLECT_FINISHED,
            payload: KernelValue::Object(BTreeMap::from([
                (
                    "safe".to_string(),
                    KernelValue::String("event-kept".to_string()),
                ),
                (
                    "nested".to_string(),
                    KernelValue::Object(BTreeMap::from([(
                        "path".to_string(),
                        KernelValue::String("./C:\\Users\\alice\\event.json".to_string()),
                    )])),
                ),
            ])),
        },
        RuntimeEvent {
            kind: "step_blocked".to_string(),
            run_id: "inspect-run".to_string(),
            step_id: Some("review".to_string()),
            timestamp_unix_ms: TS_GATE_STARTED,
            payload: KernelValue::Null,
        },
    ] {
        events.publish(&event).expect("publish event");
        summaries
            .note_event(&event)
            .expect("update summary event count");
    }

    let events_path = run_root.join("events.jsonl");
    let events_raw = fs::read_to_string(&events_path).expect("events raw");
    let lines = events_raw.lines().collect::<Vec<_>>();
    fs::write(
        &events_path,
        format!("{}\n{}\n\n{}\n", lines[0], lines[1], lines[2]),
    )
    .expect("rewrite events");
}

fn seed_finished_run(run_store: &LocalRunStore, summaries: &LocalRunSummaryStore) {
    let mut snapshot =
        KernelRunSnapshot::new("finished-run", "source_only", "v1", &["noop".to_string()]);
    snapshot.status = KernelRunStatus::Success;
    snapshot.steps.insert(
        "noop".to_string(),
        KernelStepState {
            step_id: "noop".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED - 2_000),
            finished_at_ms: Some(TS_CREATED - 1_000),
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "finished-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED - 5_000,
        TS_CREATED - 500,
    );
    run_store.create_run(&record).expect("create finished run");
    summaries
        .refresh_run("finished-run")
        .expect("refresh finished run summary");
}

fn seed_replayed_run(run_store: &LocalRunStore, summaries: &LocalRunSummaryStore) {
    let mut snapshot =
        KernelRunSnapshot::new("replay-run", "server_demo", "v1", &["collect".to_string()]);
    snapshot.status = KernelRunStatus::Success;
    snapshot.steps.insert(
        "collect".to_string(),
        KernelStepState {
            step_id: "collect".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED - 4_000),
            finished_at_ms: Some(TS_CREATED - 3_000),
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Replay,
            source_run_id: Some("inspect-run".to_string()),
            root_run_id: "replay-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED - 8_000,
        TS_CREATED - 2_500,
    );
    run_store.create_run(&record).expect("create replayed run");
    summaries
        .refresh_run("replay-run")
        .expect("refresh replayed run summary");
}

fn seed_failed_parent_with_running_child(
    run_store: &LocalRunStore,
    summaries: &LocalRunSummaryStore,
) {
    let run_id = "resumed-root";
    let mut snapshot = KernelRunSnapshot::new(
        run_id,
        "resume_demo",
        "v1",
        &[
            "fetch".to_string(),
            "plan_loop".to_string(),
            "finish".to_string(),
        ],
    );
    snapshot.status = KernelRunStatus::Failed;
    snapshot.steps.insert(
        "fetch".to_string(),
        KernelStepState {
            step_id: "fetch".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED + 10),
            finished_at_ms: Some(TS_CREATED + 20),
            activation_executions_baseline: 0,
        },
    );
    snapshot.steps.insert(
        "plan_loop".to_string(),
        KernelStepState {
            step_id: "plan_loop".to_string(),
            status: KernelStepStatus::Failed,
            executions: 2,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED + 30),
            finished_at_ms: Some(TS_CREATED + 40),
            activation_executions_baseline: 0,
        },
    );
    snapshot.steps.insert(
        "finish".to_string(),
        KernelStepState {
            step_id: "finish".to_string(),
            status: KernelStepStatus::Pending,
            executions: 0,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: None,
            finished_at_ms: None,
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: run_id.to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED + 5,
        TS_UPDATED + 10,
    );
    run_store.create_run(&record).expect("create resumed root");
    summaries
        .refresh_run(run_id)
        .expect("refresh resumed root summary");

    seed_child_run(
        run_store,
        summaries,
        "resumed-root__plan_loop",
        "resume_demo_plan_loop",
        "v1",
        KernelRunStatus::Running,
        TS_UPDATED + 2_000,
    );
}

/// Seeds a root run ("subflow-run") with a single "plan_loop" step whose
/// Seeds a failed root run ("failed-invoke-run") with one failed `invoke`
/// step ("exec") that carries both a `last_request` (so adapter/plugin/action
/// flow through) and a `last_result` with `exit_code`/`error_code`/error
/// message populated. Used by `Fixture::with_failed_invoke_step`.
fn seed_failed_invoke_step(run_store: &LocalRunStore, summaries: &LocalRunSummaryStore) {
    let request: transist_protocol::ExecutionRequest = serde_json::from_value(json!({
        "type": "tep.invoke.request.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "invoke_exec",
        "run_id": "failed-invoke-run",
        "workflow_id": "failed_demo",
        "step": { "id": "exec", "execution": 1 },
        "adapter_kind": "process",
        "action": "exec",
        "input": { "prompt": "boom" },
        "paths": {
            "run_root": "/tmp/run",
            "artifacts_dir": "/tmp/run/artifacts",
            "evidence_dir": "/tmp/run/evidence",
            "workspace_dir": "/tmp/workspace"
        },
        "timeout_ms": 1000,
        "policy": {},
        "metadata": {
            "plugin_id": "shell",
            "provider_kind": "external",
            "step_kind": "invoke"
        }
    }))
    .expect("failed invoke request");
    let result: transist_protocol::ExecutionResult = serde_json::from_value(json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "invoke_exec",
        "run_id": "failed-invoke-run",
        "step_id": "exec",
        "execution": 1,
        "status": "failed",
        "exit_code": 2,
        "stdout": "",
        "stderr": "boom\n",
        "timing": {
            "started_at": "2025-01-01T00:00:01Z",
            "finished_at": "2025-01-01T00:00:02Z",
            "duration_ms": 1000
        },
        "artifacts": [],
        "evidence": [],
        "metrics": {},
        "output": null,
        "usage": null,
        "error": {
            "code": "tool_execution_error",
            "class": "tool",
            "message": "shell exited with code 2",
            "retryability": "none"
        }
    }))
    .expect("failed invoke result");

    let mut snapshot = KernelRunSnapshot::new(
        "failed-invoke-run",
        "failed_demo",
        "v1",
        &["exec".to_string()],
    );
    snapshot.status = KernelRunStatus::Failed;
    snapshot.steps.insert(
        "exec".to_string(),
        KernelStepState {
            step_id: "exec".to_string(),
            status: KernelStepStatus::Failed,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: Some(request),
            last_result: Some(result),
            started_at_ms: Some(TS_CREATED + 10),
            finished_at_ms: Some(TS_CREATED + 20),
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "failed-invoke-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    run_store
        .create_run(&record)
        .expect("create failed-invoke-run run");
    run_store
        .store_failure_record(&FailureRecord {
            failure_id: "failure-exec-1".to_string(),
            run_id: "failed-invoke-run".to_string(),
            step_id: "exec".to_string(),
            execution_attempt: 1,
            last_stable_step_id: None,
            failure_class: FailureClass::ToolError,
            error_code: "tool_execution_error".to_string(),
            message_redacted: "shell failed at /".to_string() + "Users/alice/failure.log",
            adapter_kind: "process".to_string(),
            adapter_name: Some("shell".to_string()),
            tool_name: Some("exec".to_string()),
            retryable: true,
            safe_to_retry: true,
            requires_manual_attention: false,
            state_mutation_detected: false,
            first_seen_at_unix_ms: TS_CREATED + 20,
            last_seen_at_unix_ms: TS_CREATED + 20,
            occurrence_count: 1,
        })
        .expect("store failure record");
    summaries
        .refresh_run("failed-invoke-run")
        .expect("refresh failed-invoke-run summary");
}

/// Seeds a 2-run tree where the parent run has a failed subflow step whose
/// own `last_result` carries no `error` (mirrors how runtime persists subflow
/// failures), but a descendant child run contains the real root-cause failure
/// with a populated `error` block. The inspection layer's actionable-run
/// walker should chase the parent_run_id chain and land on the descendant so
/// the run-detail banner can surface the real error message.
fn seed_failed_subflow_with_descendant_error(
    run_store: &LocalRunStore,
    summaries: &LocalRunSummaryStore,
) {
    // --- Parent run with a failed subflow step whose own last_result has no
    //     error (mirrors how runtime persists subflow failures). Must be
    //     created BEFORE the child so the run-store index records the parent.
    let parent_subflow_result: transist_protocol::ExecutionResult = serde_json::from_value(json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "subflow_parent-run_nested_loop",
        "run_id": "parent-run",
        "step_id": "nested_loop",
        "execution": 1,
        "status": "failed",
        "exit_code": 1,
        "stdout": "",
        "stderr": "",
        "timing": {
            "started_at": "2025-01-01T00:00:01Z",
            "finished_at": "2025-01-01T00:00:02Z",
            "duration_ms": 1000
        },
        "artifacts": [],
        "evidence": [],
        "metrics": {},
        "output": {
            "child_run_id": "parent-run__nested_loop",
            "subflow_id": "nested_loop_wf",
            "status": "failed"
        },
        "usage": null,
        "error": null
    }))
    .expect("parent subflow result");

    let mut parent_snapshot = KernelRunSnapshot::new(
        "parent-run",
        "parent_wf",
        "v1",
        &["nested_loop".to_string()],
    );
    parent_snapshot.status = KernelRunStatus::Failed;
    parent_snapshot.steps.insert(
        "nested_loop".to_string(),
        KernelStepState {
            step_id: "nested_loop".to_string(),
            status: KernelStepStatus::Failed,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(parent_subflow_result),
            started_at_ms: Some(TS_CREATED + 10),
            finished_at_ms: Some(TS_CREATED + 50),
            activation_executions_baseline: 0,
        },
    );
    let parent_record = PersistedRunRecord::from_snapshot(
        parent_snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "parent-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    run_store
        .create_run(&parent_record)
        .expect("create parent run");
    summaries
        .refresh_run("parent-run")
        .expect("refresh parent summary");

    // --- Descendant child run with the real root-cause failure ---
    let child_request: transist_protocol::ExecutionRequest = serde_json::from_value(json!({
        "type": "tep.invoke.request.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "child_invoke_check",
        "run_id": "parent-run__nested_loop",
        "workflow_id": "nested_loop_wf",
        "step": { "id": "check_pr_exit_approvals", "execution": 1 },
        "adapter_kind": "process",
        "action": "check_pr_approvals",
        "input": {},
        "paths": {
            "run_root": "/tmp/run",
            "artifacts_dir": "/tmp/run/artifacts",
            "evidence_dir": "/tmp/run/evidence",
            "workspace_dir": "/tmp/workspace"
        },
        "timeout_ms": 1000,
        "policy": {},
        "metadata": {
            "plugin_id": "github.cli",
            "step_kind": "invoke"
        }
    }))
    .expect("child failed invoke request");
    let child_result: transist_protocol::ExecutionResult = serde_json::from_value(json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "child_invoke_check",
        "run_id": "parent-run__nested_loop",
        "step_id": "check_pr_exit_approvals",
        "execution": 1,
        "status": "failed",
        "exit_code": 1,
        "stdout": "",
        "stderr": "no pull requests found for branch \"master\"\n",
        "timing": {
            "started_at": "2025-01-01T00:00:01Z",
            "finished_at": "2025-01-01T00:00:02Z",
            "duration_ms": 1000
        },
        "artifacts": [],
        "evidence": [],
        "metrics": {},
        "output": null,
        "usage": null,
        "error": {
            "code": "tool_execution_error",
            "class": "tool",
            "message": "no pull requests found for branch \"master\"",
            "retryability": "safe"
        }
    }))
    .expect("child failed invoke result");

    let mut child_snapshot = KernelRunSnapshot::new(
        "parent-run__nested_loop",
        "nested_loop_wf",
        "v1",
        &["check_pr_exit_approvals".to_string()],
    );
    child_snapshot.status = KernelRunStatus::Failed;
    child_snapshot.steps.insert(
        "check_pr_exit_approvals".to_string(),
        KernelStepState {
            step_id: "check_pr_exit_approvals".to_string(),
            status: KernelStepStatus::Failed,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: Some(child_request),
            last_result: Some(child_result),
            started_at_ms: Some(TS_CREATED + 30),
            finished_at_ms: Some(TS_CREATED + 40),
            activation_executions_baseline: 0,
        },
    );
    let child_record = PersistedRunRecord::from_snapshot(
        child_snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "parent-run".to_string(),
            parent_run_id: Some("parent-run".to_string()),
            parent_step_id: Some("nested_loop".to_string()),
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    run_store
        .create_run(&child_record)
        .expect("create child run");
    summaries
        .refresh_run("parent-run__nested_loop")
        .expect("refresh child summary");
}

/// Seeds a 2-run tree where the parent run's failed subflow step already
/// carries the durable `output.failure` descriptor the kernel emits (Step 1
/// of the subflow-failure-surfacing plan). Companion server tests should see
/// the compact reason + pointer surface without walking the child run.
fn seed_failed_subflow_with_persisted_child_failure(
    run_store: &LocalRunStore,
    summaries: &LocalRunSummaryStore,
) {
    let parent_subflow_result: transist_protocol::ExecutionResult = serde_json::from_value(json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "subflow_persisted-child-failure_nested_loop",
        "run_id": "persisted-child-failure",
        "step_id": "nested_loop",
        "execution": 1,
        "status": "failed",
        "exit_code": 1,
        "stdout": "",
        "stderr": "",
        "timing": {
            "started_at": "2025-01-01T00:00:01Z",
            "finished_at": "2025-01-01T00:00:02Z",
            "duration_ms": 1000
        },
        "artifacts": [],
        "evidence": [],
        "metrics": {},
        "output": {
            "child_run_id": "persisted-child-failure__nested_loop",
            "subflow_id": "nested_loop_wf",
            "status": "failed",
            "failure": {
                "child_run_id": "persisted-child-failure__nested_loop",
                "failed_step_id": "check_pr_exit_approvals",
                "error_code": "tool_execution_error",
                "error_message": "no pull requests found for branch \"master\"",
                "stderr": "no pull requests found for branch \"master\"",
                "preflight_blocker_count": 2,
                "preflight_warning_count": 1,
                "preflight_first_blocker": "child input schema rejected model"
            }
        },
        "usage": null,
        "error": null
    }))
    .expect("parent subflow result");

    let mut parent_snapshot = KernelRunSnapshot::new(
        "persisted-child-failure",
        "parent_wf",
        "v1",
        &["nested_loop".to_string()],
    );
    parent_snapshot.status = KernelRunStatus::Failed;
    parent_snapshot.steps.insert(
        "nested_loop".to_string(),
        KernelStepState {
            step_id: "nested_loop".to_string(),
            status: KernelStepStatus::Failed,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(parent_subflow_result),
            started_at_ms: Some(TS_CREATED + 10),
            finished_at_ms: Some(TS_CREATED + 50),
            activation_executions_baseline: 0,
        },
    );
    let parent_record = PersistedRunRecord::from_snapshot(
        parent_snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "persisted-child-failure".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    run_store
        .create_run(&parent_record)
        .expect("create parent run");
    summaries
        .refresh_run("persisted-child-failure")
        .expect("refresh parent summary");
}

/// Seeds a top-level run that failed during preflight, before any step started.
/// All steps remain `Pending` and the run carries a `PersistedPreflightRecord`
/// with one failed `secret_ref` check and one failed `plugin_action` check so
/// the inspection layer can project them as banner blockers.
fn seed_failed_preflight_run(run_store: &LocalRunStore, summaries: &LocalRunSummaryStore) {
    let mut snapshot = KernelRunSnapshot::new(
        "preflight-failed-run",
        "preflight_demo",
        "v1",
        &["collect".to_string(), "publish".to_string()],
    );
    snapshot.status = KernelRunStatus::Failed;
    // Both steps stay pending — failure happened before execution.
    for step_id in ["collect", "publish"] {
        snapshot.steps.insert(
            step_id.to_string(),
            KernelStepState {
                step_id: step_id.to_string(),
                status: KernelStepStatus::Pending,
                executions: 0,
                retry_wait_until_ms: None,
                skip_reason: None,
                last_request: None,
                last_result: None,
                started_at_ms: None,
                finished_at_ms: None,
                activation_executions_baseline: 0,
            },
        );
    }

    let preflight = PersistedPreflightRecord::from_checks(
        TS_CREATED + 5,
        vec![
            PreflightCheck {
                kind: "secret_ref".to_string(),
                status: PreflightStatus::Failed,
                step_ids: vec!["collect".to_string()],
                adapter_kind: Some("process".to_string()),
                plugin_id: Some("shell".to_string()),
                action: Some("exec".to_string()),
                message: "secret 'GITHUB_TOKEN' is not available in the secret store".to_string(),
            },
            PreflightCheck {
                kind: "plugin_action".to_string(),
                status: PreflightStatus::Failed,
                step_ids: vec!["publish".to_string()],
                adapter_kind: Some("http".to_string()),
                plugin_id: Some("github".to_string()),
                action: Some("create_release".to_string()),
                message: "plugin 'github' does not declare action 'create_release'".to_string(),
            },
        ],
    );

    let mut record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "preflight-failed-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    record.preflight = Some(preflight);

    run_store
        .create_run(&record)
        .expect("create preflight-failed-run");
    summaries
        .refresh_run("preflight-failed-run")
        .expect("refresh preflight-failed-run summary");
}

/// `last_result` contains a `child_run_id` pointing to
/// "subflow-run__plan_loop". Used by `Fixture::with_subflow_step`.
fn seed_run_with_subflow_step(run_store: &LocalRunStore, summaries: &LocalRunSummaryStore) {
    let subflow_result: transist_protocol::ExecutionResult = serde_json::from_value(json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "subflow_subflow-run_plan_loop",
        "run_id": "subflow-run",
        "step_id": "plan_loop",
        "execution": 1,
        "status": "blocked",
        "stdout": "",
        "stderr": "",
        "timing": {
            "started_at": "2025-01-01T00:00:01Z",
            "finished_at": "2025-01-01T00:00:02Z",
            "duration_ms": 0
        },
        "artifacts": [],
        "evidence": [],
        "metrics": {},
        "output": {
            "child_run_id": "subflow-run__plan_loop",
            "subflow_id": "plan_loop_wf",
            "status": "blocked"
        },
        "usage": null,
        "error": null
    }))
    .expect("subflow result");

    let mut snapshot = KernelRunSnapshot::new(
        "subflow-run",
        "subflow_demo",
        "v1",
        &["plan_loop".to_string()],
    );
    snapshot.status = KernelRunStatus::Blocked;
    snapshot.steps.insert(
        "plan_loop".to_string(),
        KernelStepState {
            step_id: "plan_loop".to_string(),
            status: KernelStepStatus::Blocked,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(subflow_result),
            started_at_ms: Some(TS_UPDATED),
            finished_at_ms: None,
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "subflow-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    run_store
        .create_run(&record)
        .expect("create subflow-run run");
    summaries
        .refresh_run("subflow-run")
        .expect("refresh subflow-run summary");
}

fn seed_child_run(
    run_store: &LocalRunStore,
    summaries: &LocalRunSummaryStore,
    run_id: &str,
    workflow_id: &str,
    workflow_version: &str,
    status: KernelRunStatus,
    updated_at_ms: u64,
) {
    let mut snapshot = KernelRunSnapshot::new(
        run_id,
        workflow_id,
        workflow_version,
        &["child".to_string()],
    );
    snapshot.status = status;
    snapshot.steps.insert(
        "child".to_string(),
        KernelStepState {
            step_id: "child".to_string(),
            status: match status {
                KernelRunStatus::Created | KernelRunStatus::Planned => KernelStepStatus::Pending,
                KernelRunStatus::Running | KernelRunStatus::Waiting => KernelStepStatus::Running,
                KernelRunStatus::Blocked => KernelStepStatus::Blocked,
                KernelRunStatus::Success => KernelStepStatus::Succeeded,
                KernelRunStatus::Failed => KernelStepStatus::Failed,
                KernelRunStatus::Canceled => KernelStepStatus::Canceled,
            },
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(updated_at_ms.saturating_sub(750)),
            finished_at_ms: matches!(
                status,
                KernelRunStatus::Success | KernelRunStatus::Failed | KernelRunStatus::Canceled
            )
            .then_some(updated_at_ms),
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        Some(RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: format!("recipe-{run_id}"),
        }),
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: inferred_root_run_id(run_id),
            parent_run_id: inferred_parent_run_id(run_id),
            parent_step_id: inferred_parent_step_id(run_id),
        }),
        BTreeMap::new(),
        0,
        updated_at_ms.saturating_sub(1_000),
        updated_at_ms,
    );
    run_store.create_run(&record).expect("create child run");
    summaries
        .refresh_run(run_id)
        .expect("refresh child summary");
}

fn inferred_root_run_id(run_id: &str) -> String {
    run_id
        .split_once("__")
        .map(|(root_run_id, _)| root_run_id.to_string())
        .unwrap_or_else(|| run_id.to_string())
}

fn inferred_parent_run_id(run_id: &str) -> Option<String> {
    let (parent_run_id, _) = run_id.rsplit_once("__")?;
    (!parent_run_id.is_empty()).then(|| parent_run_id.to_string())
}

fn inferred_parent_step_id(run_id: &str) -> Option<String> {
    let (_, parent_step_id) = run_id.rsplit_once("__")?;
    (!parent_step_id.is_empty()).then(|| parent_step_id.to_string())
}

struct SeedRunBlob<'a> {
    run_id: &'a str,
    step_id: &'a str,
    execution: u32,
    family: &'a str,
    file_name: &'a str,
    contents: &'a [u8],
    invoke_id: &'a str,
    sha256: &'a str,
}

fn seed_run_blob(home_dir: &Path, seed: SeedRunBlob<'_>) {
    let family_dir = transist_storage_local::run_paths::resolve_run_root(home_dir, seed.run_id)
        .expect("resolve seeded run root")
        .join(format!(
            "artifacts/{}/execution_{:03}/{}",
            seed.step_id, seed.execution, seed.family
        ));
    fs::create_dir_all(&family_dir).expect("blob family dir");
    let blob_path = family_dir.join(seed.file_name);
    fs::write(&blob_path, seed.contents).expect("blob bytes");
    fs::write(
        family_dir.join(format!("000_{}.json", seed.file_name)),
        serde_json::to_vec_pretty(&json!({
            "kind": seed.family,
            "source_path": blob_path.display().to_string(),
            "sha256": seed.sha256,
            "bytes": seed.contents.len(),
            "status": "success",
            "invoke_id": seed.invoke_id,
            "execution": seed.execution
        }))
        .expect("blob descriptor"),
    )
    .expect("blob descriptor write");
}

/// Seeds a run whose workflow is `@v0` (test version) so trust verification
/// is bypassed. The persisted `TrustDecisionRecord` and `provenance.json`
/// both record `trust_decision = "bypassed"` so the inspection surface must
/// show that outcome distinctly from `verified` and `denied`.
fn seed_bypassed_trust_run(
    run_store: &LocalRunStore,
    summaries: &LocalRunSummaryStore,
    home_dir: &Path,
) {
    let mut snapshot =
        KernelRunSnapshot::new("bypass-run", "demo_workflow", "v0", &["step".to_string()]);
    snapshot.status = KernelRunStatus::Success;
    snapshot.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED),
            finished_at_ms: Some(TS_UPDATED),
            activation_executions_baseline: 0,
        },
    );
    let mut record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "bypass-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    record.trusted_execution_mode = true;
    record.trust_decision = Some(TrustDecisionRecord {
        schema: "transist.trust_decision.v1".to_string(),
        run_id: "bypass-run".to_string(),
        trusted_execution_mode: true,
        decision: "bypassed".to_string(),
        workflow: TrustSubjectDecision {
            id: "demo_workflow".to_string(),
            version: "v0".to_string(),
            digest: "v0-digest".to_string(),
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("test_version_bypass".to_string()),
            trust_decision: "bypassed".to_string(),
        },
        plugins: vec![TrustSubjectDecision {
            id: "demo_plugin".to_string(),
            version: "0.1.0".to_string(),
            digest: "v0-plugin-digest".to_string(),
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("test_version_bypass".to_string()),
            trust_decision: "bypassed".to_string(),
        }],
        reason_code: Some("test_version_bypass".to_string()),
        failed_verification_type: None,
        organization_id: None,
        policy_revision: None,
        decided_at_ms: TS_CREATED,
    });
    run_store
        .create_run(&record)
        .expect("create bypassed-trust run");
    summaries
        .refresh_run("bypass-run")
        .expect("refresh bypassed-trust summary");
    write_provenance(
        home_dir,
        &RunProvenanceRecord {
            schema: RUN_PROVENANCE_SCHEMA_V1.to_string(),
            run_id: "bypass-run".to_string(),
            workflow_id: "demo_workflow".to_string(),
            workflow_version: "v0".to_string(),
            workflow_digest: Some("v0-digest".to_string()),
            workflow_signature_verified: false,
            workflow_signer_key_id: None,
            workflow_trust_decision: "bypassed".to_string(),
            plugins: vec![RunProvenanceSubject {
                id: "demo_plugin".to_string(),
                version: "0.1.0".to_string(),
                digest: "v0-plugin-digest".to_string(),
                signature_verified: false,
                signer_key_id: None,
                trust_decision: "bypassed".to_string(),
            }],
            trusted_execution_mode: true,
            local_only_mode: false,
            runtime_mode: "development".to_string(),
            policy_profile: "permissive".to_string(),
            capability_enforcement: "bypassed".to_string(),
            policy_decision: RunProvenancePolicyDecision {
                allowed: true,
                reason_code: Some("test_version_bypass".to_string()),
                message: Some("test version bypass".to_string()),
                mode: "run_start".to_string(),
            },
            providers: Vec::new(),
            external_transfer_summary: RunExternalTransferSummary {
                count: 0,
                external_transfers: Vec::new(),
            },
            approval_summary: Vec::new(),
            decision: "allowed".to_string(),
            denied: None,
            organization_id: None,
            policy_revision: None,
            created_at_ms: TS_CREATED,
            finalized_at_ms: Some(TS_UPDATED),
        },
    );
}

/// Seeds a denied trusted run. The workflow signature is `denied` because
/// the digest did not match, so the run-level decision is `denied` with a
/// reason code and failed-verification type.
fn seed_denied_trust_run(
    run_store: &LocalRunStore,
    summaries: &LocalRunSummaryStore,
    home_dir: &Path,
) {
    let mut snapshot =
        KernelRunSnapshot::new("denied-run", "demo_workflow", "v1", &["step".to_string()]);
    snapshot.status = KernelRunStatus::Failed;
    snapshot.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Pending,
            executions: 0,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: None,
            finished_at_ms: None,
            activation_executions_baseline: 0,
        },
    );
    let mut record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "denied-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    record.trusted_execution_mode = true;
    record.trust_decision = Some(TrustDecisionRecord {
        schema: "transist.trust_decision.v1".to_string(),
        run_id: "denied-run".to_string(),
        trusted_execution_mode: true,
        decision: "denied".to_string(),
        workflow: TrustSubjectDecision {
            id: "demo_workflow".to_string(),
            version: "v1".to_string(),
            digest: "tampered-digest".to_string(),
            signature_verified: false,
            signer_key_id: Some("wkid_demo".to_string()),
            reason_code: Some("workflow_digest_mismatch".to_string()),
            trust_decision: "denied".to_string(),
        },
        plugins: Vec::new(),
        reason_code: Some("workflow_digest_mismatch".to_string()),
        failed_verification_type: Some("workflow_signature".to_string()),
        organization_id: None,
        policy_revision: None,
        decided_at_ms: TS_CREATED,
    });
    run_store
        .create_run(&record)
        .expect("create denied-trust run");
    summaries
        .refresh_run("denied-run")
        .expect("refresh denied-trust summary");
    write_provenance(
        home_dir,
        &RunProvenanceRecord {
            schema: RUN_PROVENANCE_SCHEMA_V1.to_string(),
            run_id: "denied-run".to_string(),
            workflow_id: "demo_workflow".to_string(),
            workflow_version: "v1".to_string(),
            workflow_digest: Some("tampered-digest".to_string()),
            workflow_signature_verified: false,
            workflow_signer_key_id: Some("wkid_demo".to_string()),
            workflow_trust_decision: "denied".to_string(),
            plugins: Vec::new(),
            trusted_execution_mode: true,
            local_only_mode: false,
            runtime_mode: "governed".to_string(),
            policy_profile: "allowlist".to_string(),
            capability_enforcement: "enforced".to_string(),
            policy_decision: RunProvenancePolicyDecision {
                allowed: false,
                reason_code: Some("trusted_execution_denied".to_string()),
                message: Some("workflow signature verification failed".to_string()),
                mode: "run_start".to_string(),
            },
            providers: Vec::new(),
            external_transfer_summary: RunExternalTransferSummary {
                count: 0,
                external_transfers: Vec::new(),
            },
            approval_summary: Vec::new(),
            decision: "denied".to_string(),
            denied: Some(RunProvenanceDenied {
                reason: "workflow_digest_mismatch".to_string(),
                offending_kind: "workflow".to_string(),
                offending_id: "demo_workflow".to_string(),
                failed_verification_type: Some("workflow_signature".to_string()),
                at_ms: TS_CREATED,
            }),
            organization_id: None,
            policy_revision: None,
            created_at_ms: TS_CREATED,
            finalized_at_ms: Some(TS_UPDATED),
        },
    );
}

/// Seeds a local-only run. No external-transfer evidence files are written,
/// so `external_transfers` must be `[]`. The provenance `providers[]` carries
/// `provider_kind = "local"` so the locality view surfaces it.
fn seed_local_only_run(
    run_store: &LocalRunStore,
    summaries: &LocalRunSummaryStore,
    home_dir: &Path,
) {
    let mut snapshot =
        KernelRunSnapshot::new("local-only-run", "local_demo", "v1", &["step".to_string()]);
    snapshot.status = KernelRunStatus::Success;
    snapshot.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED),
            finished_at_ms: Some(TS_UPDATED),
            activation_executions_baseline: 0,
        },
    );
    let record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "local-only-run".to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    run_store
        .create_run(&record)
        .expect("create local-only run");
    summaries
        .refresh_run("local-only-run")
        .expect("refresh local-only summary");
    write_provenance(
        home_dir,
        &RunProvenanceRecord {
            schema: RUN_PROVENANCE_SCHEMA_V1.to_string(),
            run_id: "local-only-run".to_string(),
            workflow_id: "local_demo".to_string(),
            workflow_version: "v1".to_string(),
            workflow_digest: None,
            workflow_signature_verified: false,
            workflow_signer_key_id: None,
            workflow_trust_decision: "not_verified".to_string(),
            plugins: Vec::new(),
            trusted_execution_mode: false,
            local_only_mode: true,
            runtime_mode: "standalone".to_string(),
            policy_profile: "permissive".to_string(),
            capability_enforcement: "bypassed".to_string(),
            policy_decision: RunProvenancePolicyDecision {
                allowed: true,
                reason_code: None,
                message: None,
                mode: "run_start".to_string(),
            },
            providers: vec![transist_core::RunProviderProvenance {
                step_id: "step".to_string(),
                provider: Some("local-tool".to_string()),
                model: Some("local-model".to_string()),
                provider_kind: Some("local".to_string()),
            }],
            external_transfer_summary: RunExternalTransferSummary {
                count: 0,
                external_transfers: Vec::new(),
            },
            approval_summary: Vec::new(),
            decision: "allowed".to_string(),
            denied: None,
            organization_id: None,
            policy_revision: None,
            created_at_ms: TS_CREATED,
            finalized_at_ms: Some(TS_UPDATED),
        },
    );
}

fn write_provenance(home_dir: &Path, provenance: &RunProvenanceRecord) {
    let path = transist_storage_local::run_paths::run_provenance_path(home_dir, &provenance.run_id)
        .expect("provenance path");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("provenance dir");
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(provenance).expect("serialize provenance"),
    )
    .expect("write provenance");
}

fn sample_collect_request() -> transist_protocol::ExecutionRequest {
    serde_json::from_value(json!({
        "type": "tep.invoke.request.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "invoke_collect",
        "run_id": "inspect-run",
        "workflow_id": "server_demo",
        "step": { "id": "collect", "execution": 1 },
        "adapter_kind": "process",
        "action": "exec",
        "input": {
            "output_file": "run://server_demo/reports/nested/collect.md",
            "prompt": "hello",
            "report_file": "run://server_demo/reports/nested/collect.md"
        },
        "paths": {
            "run_root": "/tmp/run",
            "artifacts_dir": "/tmp/run/artifacts",
            "evidence_dir": "/tmp/run/evidence",
            "workspace_dir": "/tmp/workspace"
        },
        "timeout_ms": 1000,
        "policy": {},
        "metadata": {
            "plugin_id": "shell",
            "step_kind": "invoke"
        }
    }))
    .expect("request")
}

fn sample_collect_result() -> transist_protocol::ExecutionResult {
    serde_json::from_value(json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "invoke_collect",
        "run_id": "inspect-run",
        "step_id": "collect",
        "execution": 1,
        "status": "success",
        "exit_code": 0,
        "stdout": "hello",
        "stderr": "",
        "timing": {
            "started_at": "2025-01-01T00:00:01Z",
            "finished_at": "2025-01-01T00:00:02Z",
            "duration_ms": 1000
        },
        "artifacts": [
            {
                "path": "/tmp/run/artifacts/000_output.txt",
                "sha256": "artifact-sha",
                "bytes": 19
            }
        ],
        "evidence": [
            {
                "path": "/tmp/run/evidence/000_trace.log",
                "sha256": "evidence-sha",
                "bytes": 10
            }
        ],
        "metrics": {
            "latency_ms": 1000.0
        },
        "output": {
            "message": "hello",
            "status": "done"
        },
        "usage": {
            "provider": "openai",
            "model": "gpt-test",
            "input_tokens": 11,
            "output_tokens": 7,
            "total_tokens": 18
        },
        "error": null
    }))
    .expect("result")
}

fn encoded_resource_id(family: &str, step_id: &str, execution: u32, relative_path: &str) -> String {
    URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "family": family,
            "step_id": step_id,
            "execution": execution,
            "relative_path": relative_path
        }))
        .expect("resource id"),
    )
}

fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect_tree(root, root, &mut files);
    files
}

fn collect_tree(root: &Path, current: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    let mut entries = fs::read_dir(current)
        .expect("read dir")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("entries");
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_tree(root, &path, files);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("relative path")
                .display()
                .to_string();
            files.insert(relative, fs::read(&path).expect("file bytes"));
        }
    }
}

async fn read_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

async fn read_body(response: axum::response::Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec()
}

async fn read_text(response: axum::response::Response) -> String {
    String::from_utf8(read_body(response).await).expect("utf8 body")
}

fn metric_sample_sum(body: &str, metric_name: &str) -> f64 {
    body.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let (sample, value) = line.rsplit_once(' ')?;
            (sample == metric_name || sample.starts_with(&format!("{metric_name}{{")))
                .then(|| value.parse::<f64>().expect("metric value"))
        })
        .sum()
}
