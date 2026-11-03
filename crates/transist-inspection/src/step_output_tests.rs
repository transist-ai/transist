// SPDX-License-Identifier: MPL-2.0
//! Hermetic persisted-output projection regressions.

use super::*;
use transist_core::{KernelRunSnapshot, StepExecutionRecord};
use transist_storage_local::run_paths::{plan_run_locator, resolve_run_root, write_run_locator};

fn result(execution: u32, output: serde_json::Value) -> ExecutionResult {
    ExecutionResult {
        step_id: "step".to_string(),
        execution,
        output,
        ..super::tests::result_with_stderr("", "success", None)
    }
}

fn execution(ordinal: u32, result: Option<ExecutionResult>) -> StepExecutionRecord {
    StepExecutionRecord {
        run_id: "run_1".to_string(),
        step_id: "step".to_string(),
        execution: ordinal,
        status: "failed".to_string(),
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        retry_wait_until_ms: None,
        request: None,
        result,
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: Vec::new(),
    }
}

fn state(
    ordinal: u32,
    status: KernelStepStatus,
    result: Option<ExecutionResult>,
) -> KernelStepState {
    KernelStepState {
        executions: ordinal,
        status,
        last_result: result,
        ..KernelStepState::pending("step")
    }
}

fn project(
    state: KernelStepState,
    records: &[(u32, StepExecutionRecord)],
) -> Result<InspectionStep> {
    let tmp = tempfile::tempdir_in("/tmp")?;
    let home = fs::canonicalize(tmp.path())?;
    let locator = plan_run_locator(&home, "run_1", None)?;
    write_run_locator(&home, &locator)?;
    let root = resolve_run_root(&home, "run_1")?;
    let executions = root.join("step_executions/step");
    fs::create_dir_all(&executions)?;
    let mut run = PersistedRunRecord::from_snapshot(
        KernelRunSnapshot::new("run_1", "wf", "v1", &["step".to_string()]),
        None,
        None,
        BTreeMap::new(),
        0,
        1,
        1,
    );
    run.steps.insert("step".to_string(), state);
    let state_bytes = serde_json::to_vec(&run)?;
    fs::write(root.join("state.json"), &state_bytes)?;
    let mut persisted = Vec::new();
    for (ordinal, record) in records {
        let path = executions.join(format!("execution_{ordinal:03}.json"));
        let bytes = serde_json::to_vec(record)?;
        fs::write(&path, &bytes)?;
        persisted.push((path, bytes));
    }
    let steps = build_steps(
        &LocalInspectionStore::new(&home),
        &run,
        "run_1",
        &[],
        &BTreeMap::new(),
    );
    assert_eq!(fs::read(root.join("state.json"))?, state_bytes);
    for (path, bytes) in persisted {
        assert_eq!(fs::read(path)?, bytes);
    }
    Ok(steps?.remove(0))
}

#[test]
fn step_outputs_projection_record_precedence_and_history() {
    let stale = result(
        1,
        serde_json::json!({"response_text": "Fictional earlier advice"}),
    );
    let latest = result(
        2,
        serde_json::json!({"response_text": "Fictional diagnostic advice"}),
    );
    let step = project(
        state(1, KernelStepStatus::Succeeded, Some(stale.clone())),
        &[
            (1, execution(1, Some(stale))),
            (2, execution(2, Some(latest.clone()))),
        ],
    )
    .unwrap();
    let snapshot = step.latest_output.unwrap();
    assert_eq!(snapshot.execution, Some(2));
    assert_eq!(snapshot.status, "failed"); // Record status, independently of state/result status.
    assert_eq!(snapshot.source, "execution_record");
    assert!(snapshot.result_present);
    assert_eq!(snapshot.value, Some(latest.output));
    assert_eq!(
        step.executions
            .iter()
            .map(|record| record.execution)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn step_outputs_projection_in_flight_never_reuses_earlier_result() {
    for status in [
        KernelStepStatus::Running,
        KernelStepStatus::Dispatching,
        KernelStepStatus::RetryWait,
    ] {
        let stale = result(
            1,
            serde_json::json!({"response_text": "Fictional stale advice"}),
        );
        let step = project(
            state(2, status, Some(stale.clone())),
            &[(1, execution(1, Some(stale)))],
        )
        .unwrap();
        let snapshot = step.latest_output.unwrap();
        assert_eq!(snapshot.execution, Some(2));
        assert_eq!(snapshot.status, step_status_name(status));
        assert_eq!(snapshot.source, "state_snapshot");
        assert!(!snapshot.result_present);
        assert!(snapshot.value.is_none());
    }
}

#[test]
fn step_outputs_projection_record_without_result_never_falls_back() {
    let step = project(
        state(
            2,
            KernelStepStatus::Failed,
            Some(result(2, serde_json::json!("stale"))),
        ),
        &[(2, execution(2, None))],
    )
    .unwrap();
    let snapshot = step.latest_output.unwrap();
    assert_eq!(snapshot.source, "execution_record");
    assert_eq!(snapshot.execution, Some(2));
    assert!(!snapshot.result_present);
    assert!(snapshot.value.is_none());
}

#[test]
fn step_outputs_projection_legacy_matching_results() {
    for status in [
        KernelStepStatus::Succeeded,
        KernelStepStatus::Failed,
        KernelStepStatus::RetryWait,
        KernelStepStatus::Canceled,
    ] {
        let value = serde_json::json!({"response_text": "Fictional legacy advice"});
        let snapshot = project(state(3, status, Some(result(3, value.clone()))), &[])
            .unwrap()
            .latest_output
            .unwrap();
        assert_eq!(snapshot.execution, Some(3));
        assert_eq!(snapshot.status, step_status_name(status));
        assert_eq!(snapshot.source, "state_snapshot");
        assert!(snapshot.result_present);
        assert_eq!(snapshot.value, Some(value));
    }
}

#[test]
fn step_outputs_projection_state_identity_mismatches_are_excluded() {
    for field in ["run", "step", "execution"] {
        let mut result = result(1, serde_json::json!(false));
        match field {
            "run" => result.run_id = "other".to_string(),
            "step" => result.step_id = "other".to_string(),
            _ => result.execution = 2,
        }
        let snapshot = project(state(1, KernelStepStatus::Failed, Some(result)), &[])
            .unwrap()
            .latest_output
            .unwrap();
        assert!(!snapshot.result_present);
        assert!(snapshot.value.is_none());
    }
}

#[test]
fn step_outputs_projection_nonterminal_state_results_are_excluded() {
    for status in [
        KernelStepStatus::Pending,
        KernelStepStatus::Runnable,
        KernelStepStatus::Running,
        KernelStepStatus::Dispatching,
        KernelStepStatus::Blocked,
        KernelStepStatus::Skipped,
    ] {
        let snapshot = project(state(1, status, Some(result(1, serde_json::json!(0)))), &[])
            .unwrap()
            .latest_output
            .unwrap();
        assert!(!snapshot.result_present);
    }
}

#[test]
fn step_outputs_projection_empty_and_falsy_values_survive_roundtrip() {
    for value in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!([]),
        serde_json::json!(false),
        serde_json::json!(0),
        serde_json::json!(""),
    ] {
        for from_record in [true, false] {
            let records = if from_record {
                vec![(1, execution(1, Some(result(1, value.clone()))))]
            } else {
                Vec::new()
            };
            let snapshot = project(
                state(
                    1,
                    KernelStepStatus::Succeeded,
                    Some(result(1, value.clone())),
                ),
                &records,
            )
            .unwrap()
            .latest_output
            .unwrap();
            assert!(snapshot.result_present);
            assert_eq!(snapshot.value, Some(value.clone()));
            let json = serde_json::to_value(&snapshot).unwrap();
            assert_eq!(json.get("value"), Some(&value));
            let decoded: InspectionStepOutputSnapshot = serde_json::from_value(json).unwrap();
            assert_eq!(decoded.value, Some(value.clone()));
        }
    }
}

#[test]
fn step_outputs_projection_no_attempt_and_no_result_envelopes() {
    let step = project(KernelStepState::pending("step"), &[]).unwrap();
    assert!(step.latest_output.is_none());
    let mut json = serde_json::to_value(&step).unwrap();
    json.as_object_mut().unwrap().remove("latest_output");
    assert!(
        serde_json::from_value::<InspectionStep>(json)
            .unwrap()
            .latest_output
            .is_none()
    );
    let snapshot = project(state(1, KernelStepStatus::Running, None), &[])
        .unwrap()
        .latest_output
        .unwrap();
    assert_eq!(snapshot.execution, Some(1));
    assert!(!snapshot.result_present);
    let decoded: InspectionStepOutputSnapshot =
        serde_json::from_value(serde_json::to_value(snapshot).unwrap()).unwrap();
    assert!(decoded.value.is_none());
    for ordinal in [0, 1] {
        let snapshot = project(
            state(
                ordinal,
                KernelStepStatus::Running,
                Some(result(0, serde_json::Value::Null)),
            ),
            &[],
        )
        .unwrap()
        .latest_output
        .unwrap();
        assert_eq!(snapshot.execution, (ordinal > 0).then_some(ordinal));
        assert!(!snapshot.result_present);
        assert!(
            serde_json::to_value(snapshot)
                .unwrap()
                .get("value")
                .is_none()
        );
    }
}

#[test]
fn step_outputs_projection_record_identity_mismatches_are_storage_errors() {
    for field in [
        "run",
        "step",
        "ordinal",
        "result_run",
        "result_step",
        "result_execution",
        "request",
    ] {
        let mut record = execution(1, Some(result(1, serde_json::json!({}))));
        match field {
            "run" => record.run_id = "other".to_string(),
            "step" => record.step_id = "other".to_string(),
            "ordinal" => record.execution = 2,
            "result_run" => record.result.as_mut().unwrap().run_id = "other".to_string(),
            "result_step" => record.result.as_mut().unwrap().step_id = "other".to_string(),
            "result_execution" => record.result.as_mut().unwrap().execution = 2,
            _ => record.request = Some(super::tests::inspection_request(serde_json::json!({}))),
        }
        let error = project(state(1, KernelStepStatus::Failed, None), &[(1, record)]).unwrap_err();
        assert!(
            error.to_string().contains("identity disagrees"),
            "{field}: {error:#}"
        );
    }
}

#[test]
fn step_outputs_projection_malformed_record_is_storage_error() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let home = fs::canonicalize(tmp.path()).unwrap();
    let locator = plan_run_locator(&home, "run_1", None).unwrap();
    write_run_locator(&home, &locator).unwrap();
    let root = resolve_run_root(&home, "run_1")
        .unwrap()
        .join("step_executions/step");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("execution_001.json"), b"{not json").unwrap();
    let error = LocalInspectionStore::new(&home)
        .list_step_executions("run_1", "step")
        .unwrap_err();
    assert!(error.to_string().contains("invalid step execution json"));
}
