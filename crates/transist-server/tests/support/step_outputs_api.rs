// SPDX-License-Identifier: MPL-2.0
//! Hermetic execution-output detail contract tests using local storage and Axum.

use super::*;

const DETAIL: &str = "/api/v1/runs/inspect-run/steps/collect";

pub(super) fn execution_path(fixture: &Fixture, ordinal: u32) -> PathBuf {
    fixture.home_dir.join(format!(
        "runs/run_inspect-run/step_executions/collect/execution_{ordinal:03}.json"
    ))
}

pub(super) fn record(fixture: &Fixture, ordinal: u32, status: &str, value: Option<Value>) {
    let mut result = sample_collect_result();
    result.execution = ordinal;
    result.status = status.to_string();
    result.output = value.clone().unwrap_or(Value::Null);
    let record = StepExecutionRecord {
        run_id: "inspect-run".to_string(),
        step_id: "collect".to_string(),
        execution: ordinal,
        status: status.to_string(),
        started_at_ms: Some(TS_COLLECT_STARTED),
        finished_at_ms: Some(TS_COLLECT_FINISHED),
        retry_wait_until_ms: None,
        request: None,
        result: value.map(|_| result),
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: Vec::new(),
    };
    fs::write(
        execution_path(fixture, ordinal),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
}

pub(super) fn state(
    fixture: &mut Fixture,
    ordinal: u32,
    status: KernelStepStatus,
    result: Option<transist_protocol::ExecutionResult>,
) {
    let store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
    let mut run = store.load_run("inspect-run").unwrap().unwrap();
    let step = run.steps.get_mut("collect").unwrap();
    step.executions = ordinal;
    step.status = status;
    step.last_result = result;
    // Fixture setup may model legacy state with a lower counter. Write the
    // synthetic record directly rather than violating the runtime update API.
    fs::write(
        fixture.home_dir.join("runs/run_inspect-run/state.json"),
        serde_json::to_vec(&run).unwrap(),
    )
    .unwrap();
    // The embedded inspection-only server deliberately has no metrics backend.
    fixture.app = app_with_query(Arc::new(transist::TransistInspectionContext::with_home(
        fixture.repo_root.clone(),
        fixture.home_dir.clone(),
    )));
}

async fn response(fixture: &Fixture, path: &str, status: StatusCode) -> Value {
    let response = fixture.request("GET", path, Body::empty()).await;
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap();
    assert_contract_http_response(
        &load_openapi_contract(),
        "GET",
        path,
        axum::response::Response::from_parts(parts, Body::from(bytes)),
        status,
    )
    .await;
    value
}

#[tokio::test]
async fn step_outputs_api_latest_and_history_have_independent_attribution() {
    let mut fixture = Fixture::new();
    record(
        &fixture,
        1,
        "failed",
        Some(json!({"response_text": "Fictional diagnostic advice"})),
    );
    record(
        &fixture,
        2,
        "success",
        Some(json!({"response_text": "Fictional revised advice"})),
    );
    // Counter lags records; state result disagrees with the immutable record.
    state(
        &mut fixture,
        1,
        KernelStepStatus::Succeeded,
        Some(sample_collect_result()),
    );
    let before = snapshot_tree(&fixture.home_dir);
    for suffix in ["", "?execution=latest", "?execution=2", "?execution=0002"] {
        let body = response(&fixture, &format!("{DETAIL}{suffix}"), StatusCode::OK).await;
        assert_eq!(
            body["output"],
            json!({
                "execution": 2, "status": "success", "source": "execution_record",
                "result_present": true, "value": {"response_text": "Fictional revised advice"}
            })
        );
        assert_eq!(body["output_executions"], json!([1, 2]));
        assert!(body.get("metrics").is_none());
    }
    let historical = response(&fixture, &format!("{DETAIL}?execution=1"), StatusCode::OK).await;
    assert_eq!(historical["status"], "success");
    assert_eq!(historical["output"]["status"], "failed");
    assert_eq!(
        historical["output"]["value"]["response_text"],
        "Fictional diagnostic advice"
    );
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_api_newer_uncommitted_attempt_does_not_reuse_output() {
    let mut fixture = Fixture::new();
    state(
        &mut fixture,
        2,
        KernelStepStatus::Running,
        Some(sample_collect_result()),
    );
    let body = response(&fixture, DETAIL, StatusCode::OK).await;
    assert_eq!(
        body["output"],
        json!({
            "execution": 2, "status": "running", "source": "state_snapshot", "result_present": false
        })
    );
    assert_eq!(body["output_executions"], json!([1]));
    let error = response(
        &fixture,
        &format!("{DETAIL}?execution=2"),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(error["error"]["code"], "resource_not_found");
    assert_eq!(
        response(&fixture, &format!("{DETAIL}?execution=1"), StatusCode::OK).await["output"]["result_present"],
        true
    );
}

#[tokio::test]
async fn step_outputs_api_no_result_record_wins_over_matching_state() {
    let mut fixture = Fixture::new();
    record(&fixture, 1, "interrupted", None);
    state(
        &mut fixture,
        1,
        KernelStepStatus::Succeeded,
        Some(sample_collect_result()),
    );
    for suffix in ["", "?execution=1"] {
        let body = response(&fixture, &format!("{DETAIL}{suffix}"), StatusCode::OK).await;
        assert_eq!(
            body["output"],
            json!({
                "execution": 1, "status": "interrupted", "source": "execution_record", "result_present": false
            })
        );
        assert_eq!(body["output_executions"], json!([1]));
    }
}

#[tokio::test]
async fn step_outputs_api_legacy_fallback_requires_matching_identity_and_status() {
    for status in [
        KernelStepStatus::Succeeded,
        KernelStepStatus::Failed,
        KernelStepStatus::RetryWait,
        KernelStepStatus::Canceled,
    ] {
        let mut fixture = Fixture::new();
        fs::remove_file(execution_path(&fixture, 1)).unwrap();
        state(&mut fixture, 1, status, Some(sample_collect_result()));
        for suffix in ["", "?execution=1"] {
            let body = response(&fixture, &format!("{DETAIL}{suffix}"), StatusCode::OK).await;
            assert_eq!(body["output"]["source"], "state_snapshot");
            assert_eq!(body["output"]["result_present"], true);
            assert_eq!(body["output_executions"], json!([1]));
            assert!(body.get("metrics").is_none());
        }
    }
    for mismatch in ["run", "step", "ordinal", "status"] {
        let mut fixture = Fixture::new();
        fs::remove_file(execution_path(&fixture, 1)).unwrap();
        let mut result = sample_collect_result();
        let mut status = KernelStepStatus::Succeeded;
        match mismatch {
            "run" => result.run_id = "other".into(),
            "step" => result.step_id = "other".into(),
            "ordinal" => result.execution = 2,
            _ => status = KernelStepStatus::Running,
        }
        state(&mut fixture, 1, status, Some(result));
        let body = response(&fixture, DETAIL, StatusCode::OK).await;
        assert_eq!(body["output"]["result_present"], false);
        assert!(body["output"].get("value").is_none());
        assert_eq!(body["output_executions"], json!([]));
        response(
            &fixture,
            &format!("{DETAIL}?execution=1"),
            StatusCode::NOT_FOUND,
        )
        .await;
    }
}

#[tokio::test]
async fn step_outputs_api_pending_and_falsy_results_preserve_presence() {
    let mut fixture = Fixture::new();
    fs::remove_file(execution_path(&fixture, 1)).unwrap();
    state(&mut fixture, 0, KernelStepStatus::Pending, None);
    let body = response(&fixture, DETAIL, StatusCode::OK).await;
    assert_eq!(
        body["output"],
        json!({
            "execution": null, "status": "pending", "source": "state_snapshot", "result_present": false
        })
    );
    assert_eq!(body["output_executions"], json!([]));
    assert!(body.get("metrics").is_none());
    for value in [
        Value::Null,
        json!({}),
        json!([]),
        json!(false),
        json!(0),
        json!(""),
    ] {
        record(&fixture, 1, "failed", Some(value.clone()));
        state(&mut fixture, 1, KernelStepStatus::Failed, None);
        let body = response(&fixture, DETAIL, StatusCode::OK).await;
        assert_eq!(body["output"]["result_present"], true);
        assert_eq!(body["output"].get("value"), Some(&value));
        assert!(body.get("metrics").is_none());
    }
}

#[tokio::test]
async fn step_outputs_api_strict_selector_errors_use_problem_envelope() {
    let fixture = Fixture::new();
    let before = snapshot_tree(&fixture.home_dir);
    for selector in [
        "",
        "%20",
        "1%20",
        "%091",
        "+1",
        "%2B1",
        "-1",
        "0",
        "000",
        "one",
        "LATEST",
        "1.0",
        "4294967296",
        "99999999999999999999999",
        "%D9%A1",
        "latest&execution=1",
    ] {
        let body = response(
            &fixture,
            &format!("{DETAIL}?execution={selector}"),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(body["error"]["code"], "invalid_request", "{selector}");
    }
    response(
        &fixture,
        &format!("{DETAIL}?execution=4294967295"),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_api_missing_resources_return_not_found() {
    let fixture = Fixture::new();
    for path in [
        "/api/v1/runs/missing/steps/collect",
        "/api/v1/runs/inspect-run/steps/missing",
        "/api/v1/runs/inspect-run/steps/collect?execution=3",
    ] {
        let body = response(&fixture, path, StatusCode::NOT_FOUND).await;
        assert_eq!(body["error"]["code"], "resource_not_found");
    }
}

#[tokio::test]
async fn step_outputs_api_selectable_ordinals_are_sorted_deduplicated_and_u32_bounded() {
    let mut fixture = Fixture::new();
    record(
        &fixture,
        10,
        "failed",
        Some(json!({"response_text": "Fictional earlier advice"})),
    );
    record(&fixture, u32::MAX, "success", Some(Value::Null));
    // Legacy alternate padding names can project the same positive ordinal.
    fs::copy(
        execution_path(&fixture, 1),
        execution_path(&fixture, 1).with_file_name("execution_0001.json"),
    )
    .unwrap();
    state(&mut fixture, 1, KernelStepStatus::Succeeded, None);
    let before = snapshot_tree(&fixture.home_dir);
    let body = response(&fixture, DETAIL, StatusCode::OK).await;
    assert_eq!(body["output_executions"], json!([1, 10, u32::MAX]));
    assert_eq!(body["output"]["execution"], u32::MAX);
    for suffix in ["?execution=4294967295", "?execution=0004294967295"] {
        let selected = response(&fixture, &format!("{DETAIL}{suffix}"), StatusCode::OK).await;
        assert_eq!(selected["output"], body["output"]);
        assert_eq!(selected["output"].get("value"), Some(&Value::Null));
    }
    response(
        &fixture,
        &format!("{DETAIL}?execution=9"),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[test]
fn step_outputs_api_openapi_enforces_presence_and_omission_rules() {
    let contract = load_openapi_contract();
    let mut defs = contract["components"]["schemas"].clone();
    replace_component_refs(&mut defs);
    normalize_openapi_nullable(&mut defs);
    let root = json!({"$defs": defs, "$ref": "#/$defs/StepOutput"});
    let validator = jsonschema::Validator::new(&root).unwrap();
    let no_result = json!({"execution": null, "status": "pending", "source": "state_snapshot", "result_present": false});
    assert!(validator.is_valid(&no_result));
    let mut invalid = no_result.clone();
    invalid["value"] = Value::Null;
    assert!(!validator.is_valid(&invalid));
    let mut present = no_result;
    present["result_present"] = json!(true);
    assert!(!validator.is_valid(&present));
    present["value"] = Value::Null;
    assert!(validator.is_valid(&present));
    present["execution"] = json!(0);
    assert!(!validator.is_valid(&present));
    present["execution"] = json!(4294967296u64);
    assert!(!validator.is_valid(&present));
}

#[tokio::test]
async fn step_outputs_api_transport_redacts_paths_and_preserves_complete_values() {
    let value = json!({
        "response_text": "Fictional advice\nSecond line",
        "nested": [{"/tmp/private/key": "/home/testuser/output", "C:\\private\\key": "~/private", "traversal": "../private"}],
        "url": "https://example.test/a?path=/tmp/private&x=1&x=2#frag",
        "http": "http://example.test/a",
        "large": "🦀".repeat(20_000),
    });
    let mut fixture = Fixture::new();
    record(&fixture, 1, "success", Some(value.clone()));
    state(&mut fixture, 1, KernelStepStatus::Succeeded, None);
    let before = snapshot_tree(&fixture.home_dir);
    let body = response(&fixture, DETAIL, StatusCode::OK).await;
    let output = &body["output"]["value"];
    assert_eq!(output["response_text"], value["response_text"]);
    assert_eq!(output["large"], value["large"]);
    assert_eq!(output["url"], value["url"]);
    assert_eq!(output["http"], value["http"]);
    assert_eq!(
        output["nested"][0],
        json!({
            "[path redacted]": "[path redacted]",
            "[path redacted] #2": "[path redacted]",
            "traversal": "[path redacted]"
        })
    );
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_api_compact_lists_keep_existing_shapes() {
    let fixture = Fixture::new();
    let before = snapshot_tree(&fixture.home_dir);
    let steps = response(&fixture, "/api/v1/runs/inspect-run/steps", StatusCode::OK).await;
    for step in steps["data"].as_array().unwrap() {
        for field in ["output", "output_executions", "latest_output"] {
            assert!(step.get(field).is_none());
        }
        assert!(step["output_summary"].get("value").is_none());
        if let Some(executions) = step["executions"].as_array() {
            for execution in executions {
                assert!(execution.get("result").is_none());
                assert!(execution.get("output").is_none());
            }
        }
    }
    let runs = response(&fixture, "/api/v1/runs", StatusCode::OK).await;
    assert!(runs["data"][0].get("output").is_none());
    assert!(runs["data"][0].get("latest_output").is_none());
    let detail = response(&fixture, DETAIL, StatusCode::OK).await;
    assert!(detail.get("metrics").is_some());
    let mut compact_detail = detail.as_object().unwrap().clone();
    for key in ["output", "output_executions", "metrics"] {
        compact_detail.remove(key);
    }
    assert_eq!(Value::Object(compact_detail), steps["data"][0]);
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_api_corrupt_and_unreadable_storage_is_sanitized() {
    for corruption in [
        "json",
        "run",
        "step",
        "ordinal",
        "result_run",
        "result_step",
        "result_ordinal",
        "request",
        "unreadable",
        "state",
    ] {
        let fixture = Fixture::new();
        let path = execution_path(&fixture, 1);
        if corruption == "json" {
            fs::write(&path, b"malformed /tmp/private/storage").unwrap();
        } else if corruption == "unreadable" {
            fs::remove_file(&path).unwrap();
            let directory = path.parent().unwrap();
            fs::remove_dir(directory).unwrap();
            fs::write(directory, b"not a readable execution directory").unwrap();
        } else if corruption == "state" {
            fs::write(
                fixture.home_dir.join("runs/run_inspect-run/state.json"),
                b"invalid",
            )
            .unwrap();
        } else {
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            match corruption {
                "run" => value["run_id"] = json!("other"),
                "step" => value["step_id"] = json!("other"),
                "ordinal" => value["execution"] = json!(2),
                "result_run" => value["result"]["run_id"] = json!("other"),
                "result_step" => value["result"]["step_id"] = json!("other"),
                "result_ordinal" => value["result"]["execution"] = json!(2),
                _ => value["request"]["step"]["execution"] = json!(2),
            }
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let before = snapshot_tree(&fixture.home_dir);
        for suffix in ["", "?execution=1"] {
            let body = response(
                &fixture,
                &format!("{DETAIL}{suffix}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .await;
            assert_eq!(
                body,
                json!({"error": {"code": "storage_query_failure", "message": "failed to load inspection data"}})
            );
        }
        assert_eq!(snapshot_tree(&fixture.home_dir), before);
    }
}
