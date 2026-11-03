// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::collections::BTreeMap;
use transist_core::ports::{RunStorePort, RunStoreStepExecution};
use transist_core::{
    KernelRunSnapshot, KernelStepState, RecipeSnapshotRef, RunLineage, RunLineageMode,
    StepExecutionRecord,
};
use transist_storage_local::{LocalRunStore, LocalStorageConfig};

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
        2,
    )
}

fn sample_result_with_usage(usage: serde_json::Value) -> ExecutionResult {
    sample_result_with_usage_and_duration(usage, 250)
}

fn sample_result_with_usage_and_duration(
    usage: serde_json::Value,
    duration_ms: u64,
) -> ExecutionResult {
    serde_json::from_value(serde_json::json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "invoke_1",
        "run_id": "run_1",
        "step_id": "step",
        "execution": 1,
        "status": "success",
        "stdout": "",
        "stderr": "",
        "timing": {
            "started_at": "0",
            "finished_at": "0",
            "duration_ms": duration_ms
        },
        "artifacts": [],
        "evidence": [],
        "metrics": {
            "latency_ms": 250.0
        },
        "output": {},
        "usage": usage,
        "error": null
    }))
    .expect("result")
}

fn persist_execution_record(store: &LocalRunStore, record: StepExecutionRecord) {
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: record.run_id.clone(),
            step_id: record.step_id.clone(),
            execution: record.execution,
            state: serde_json::to_value(record).expect("execution json").into(),
        })
        .expect("execution");
}

fn persist_execution(
    store: &LocalRunStore,
    run_id: &str,
    step_id: &str,
    result: Option<ExecutionResult>,
    metrics_validation_errors: Option<u32>,
) {
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
            execution: 1,
            state: serde_json::to_value(StepExecutionRecord {
                run_id: run_id.to_string(),
                step_id: step_id.to_string(),
                execution: 1,
                status: "success".to_string(),
                started_at_ms: Some(100),
                finished_at_ms: Some(350),
                retry_wait_until_ms: None,
                request: None,
                result,
                metrics_validation_errors,
                state_mutations: Vec::new(),
                document_revisions: Vec::new(),
            })
            .expect("execution json")
            .into(),
        })
        .expect("execution");
}

fn persist_gate_decision(
    home_dir: &Path,
    run_id: &str,
    gate_id: &str,
    step_id: &str,
    decision: &str,
    decided_at_ms: u64,
) {
    let gates_dir = transist_storage_local::run_paths::resolve_run_root(home_dir, run_id)
        .expect("resolve run root")
        .join("gates");
    std::fs::create_dir_all(&gates_dir).expect("create gates dir");
    let payload = transist_protocol::GateDecisionRecord {
        review_request: None,
        schema: "transist.gate_decision.v1".to_string(),
        run_id: run_id.to_string(),
        gate_id: gate_id.to_string(),
        step_id: step_id.to_string(),
        decision: decision.to_string(),
        actor: Some("metrics-user".to_string()),
        actor_source: Some("flag".to_string()),
        reason: None,
        decided_at_ms: Some(decided_at_ms),
    };
    let raw = serde_json::to_vec_pretty(&payload).expect("serialize gate");
    std::fs::write(gates_dir.join(format!("{gate_id}.json")), raw).expect("write gate");
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

#[test]
fn query_rebuilds_metrics_from_persisted_executions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_1");
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
            last_result: Some(
                serde_json::from_value(serde_json::json!({
                    "type": "tep.invoke.result.v1",
                    "protocol_version": "tep.v1",
                    "invoke_id": "invoke_1",
                    "run_id": "run_1",
                    "step_id": "step",
                    "execution": 1,
                    "status": "success",
                    "stdout": "",
                    "stderr": "",
                    "timing": {
                        "started_at": "0",
                        "finished_at": "0",
                        "duration_ms": 250
                    },
                    "artifacts": [],
                    "evidence": [],
                    "metrics": {
                        "latency_ms": 250.0
                    },
                    "output": {},
                    "usage": {
                        "provider": "openai",
                        "model": "gpt-test",
                        "input_tokens": 3,
                        "output_tokens": 4,
                        "total_tokens": 7
                    },
                    "error": null
                }))
                .expect("result"),
            ),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "run_1".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            state: serde_json::json!({
                "run_id": "run_1",
                "step_id": "step",
                "execution": 1,
                "status": "success",
                "started_at_ms": 100,
                "finished_at_ms": 350,
                "request": null,
                "result": record.steps["step"].last_result
            })
            .into(),
        })
        .expect("execution");

    let query = LocalMetricsQuery::new(tmp.path());
    let run = query
        .get_run_metrics("run_1")
        .expect("get run")
        .expect("run metrics");
    assert_eq!(run.total_step_executions, 1);
    assert_eq!(run.terminal_status.as_deref(), Some("succeeded"));
    assert_eq!(run.total_tokens, Some(7));

    let step = query
        .get_step_metrics("run_1", "step")
        .expect("get step")
        .expect("step metrics");
    assert_eq!(step.execution_count, 1);
    assert_eq!(
        step.latest_execution
            .as_ref()
            .and_then(|execution| execution.total_tokens),
        Some(7)
    );

    let prometheus = query.render_prometheus().expect("render");
    assert!(prometheus.contains("transist_runs_created_total"));
    assert!(prometheus.contains("transist_usage_total_tokens_total"));
}

#[test]
fn validation_errors_counter_stays_zero_for_valid_metrics() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_valid");
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
            last_result: Some(sample_result_with_usage(serde_json::json!(null))),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");
    persist_execution(
        &store,
        "run_valid",
        "step",
        record.steps["step"].last_result.clone(),
        None,
    );

    let query = LocalMetricsQuery::new(tmp.path());
    let prometheus = query.render_prometheus().expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_metrics_validation_errors_total"),
        0.0
    );
}

#[test]
fn validation_errors_counter_rebuilds_from_persisted_executions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_invalid");
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
            last_result: Some(sample_result_with_usage(serde_json::json!(null))),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");
    persist_execution(
        &store,
        "run_invalid",
        "step",
        record.steps["step"].last_result.clone(),
        Some(3),
    );

    let query = LocalMetricsQuery::new(tmp.path());
    let prometheus = query.render_prometheus().expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_metrics_validation_errors_total"),
        3.0
    );
}

#[test]
fn started_execution_counter_uses_persisted_step_state_before_completion() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_started");
    record.status = KernelRunStatus::Running;
    record.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Running,
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
    store.create_run(&record).expect("create");

    let query = LocalMetricsQuery::new(tmp.path());
    let step = query
        .get_step_metrics("run_started", "step")
        .expect("get step")
        .expect("step metrics");
    assert_eq!(step.execution_count, 1);

    let prometheus = query.render_prometheus().expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_step_executions_started_total"),
        1.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_step_executions_finished_total"),
        0.0
    );
}

#[test]
fn active_run_duration_is_unavailable_when_start_is_ahead_of_metrics_clock() {
    for status in [
        KernelRunStatus::Running,
        KernelRunStatus::Waiting,
        KernelRunStatus::Blocked,
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        let mut record = sample_record("run_future_start");
        record.status = status;
        let step = record.steps.get_mut("step").expect("step");
        step.status = KernelStepStatus::Running;
        step.executions = 1;
        // Force the clock ordering without sleeps or concurrent writers.
        step.started_at_ms = Some(u64::MAX);
        step.finished_at_ms = None;
        store.create_run(&record).expect("create");

        let query = LocalMetricsQuery::new(tmp.path());
        query.refresh_run("run_future_start").expect("refresh");
        let run = query
            .get_run_metrics("run_future_start")
            .expect("get run")
            .expect("run metrics");
        assert_eq!(run.started_at_ms, Some(u64::MAX));
        assert_eq!(run.total_step_executions, 1);
        assert_eq!(run.total_duration_ms, None);
        assert_eq!(run.wall_clock_duration_ms, None);
        let list = query.run_list_metrics().expect("run list");
        assert_eq!(list["run_future_start"].total_duration_ms, None);
        assert_eq!(list["run_future_start"].wall_clock_duration_ms, None);
    }
}

#[test]
fn usage_with_only_input_tokens_exports_input_counter_and_report_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_input_only");
    record.status = KernelRunStatus::Success;
    let result = sample_result_with_usage(serde_json::json!({
        "provider": "openai",
        "model": "gpt-test",
        "input_tokens": 5
    }));
    record.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(result.clone()),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");
    persist_execution(&store, "run_input_only", "step", Some(result), None);

    let query = LocalMetricsQuery::new(tmp.path());
    let run = query
        .get_run_metrics("run_input_only")
        .expect("get run")
        .expect("run");
    assert_eq!(run.input_tokens, Some(5));
    assert_eq!(run.total_tokens, None);

    let prometheus = query.render_prometheus().expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_input_tokens_total"),
        5.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_output_tokens_total"),
        0.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_total_tokens_total"),
        0.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_reports_total"),
        1.0
    );
}

#[test]
fn usage_with_only_output_tokens_exports_output_counter_and_report_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_output_only");
    record.status = KernelRunStatus::Success;
    let result = sample_result_with_usage(serde_json::json!({
        "provider": "openai",
        "model": "gpt-test",
        "output_tokens": 8
    }));
    record.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(result.clone()),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");
    persist_execution(&store, "run_output_only", "step", Some(result), None);

    let prometheus = query_with_store(tmp.path())
        .render_prometheus()
        .expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_input_tokens_total"),
        0.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_output_tokens_total"),
        8.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_reports_total"),
        1.0
    );
}

#[test]
fn usage_without_total_tokens_still_exports_present_fields_and_http_summary_matches() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_partial");
    record.status = KernelRunStatus::Success;
    let result = sample_result_with_usage(serde_json::json!({
        "provider": "openai",
        "model": "gpt-test",
        "input_tokens": 3,
        "output_tokens": 4
    }));
    record.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(result.clone()),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");
    persist_execution(&store, "run_partial", "step", Some(result), None);

    let query = LocalMetricsQuery::new(tmp.path());
    let run = query
        .get_run_metrics("run_partial")
        .expect("get run")
        .expect("run");
    assert_eq!(run.input_tokens, Some(3));
    assert_eq!(run.output_tokens, Some(4));
    assert_eq!(run.total_tokens, None);

    let step = query
        .get_step_metrics("run_partial", "step")
        .expect("get step")
        .expect("step");
    assert_eq!(step.input_tokens, Some(3));
    assert_eq!(step.output_tokens, Some(4));
    assert_eq!(step.total_tokens, None);

    let prometheus = query.render_prometheus().expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_input_tokens_total"),
        3.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_output_tokens_total"),
        4.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_total_tokens_total"),
        0.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_reports_total"),
        1.0
    );
}

#[test]
fn usage_with_all_fields_exports_all_counters() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_full_usage");
    record.status = KernelRunStatus::Success;
    let result = sample_result_with_usage(serde_json::json!({
        "provider": "openai",
        "model": "gpt-test",
        "input_tokens": 3,
        "output_tokens": 4,
        "total_tokens": 7
    }));
    record.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(result.clone()),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");
    persist_execution(&store, "run_full_usage", "step", Some(result), None);

    let prometheus = LocalMetricsQuery::new(tmp.path())
        .render_prometheus()
        .expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_input_tokens_total"),
        3.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_output_tokens_total"),
        4.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_total_tokens_total"),
        7.0
    );
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_usage_reports_total"),
        1.0
    );
}

fn query_with_store(home_dir: &Path) -> LocalMetricsQuery {
    LocalMetricsQuery::new(home_dir)
}

#[test]
fn incremental_refresh_replaces_existing_run_contribution() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let record = sample_record("run_2");
    store.create_run(&record).expect("create");

    let query = LocalMetricsQuery::new(tmp.path());
    let before = query.render_prometheus().expect("before");
    assert!(before.contains("transist_runs_created_total"));

    let mut updated = record.clone();
    updated.status = KernelRunStatus::Failed;
    updated.resume_count = 2;
    store.update_run_state("run_2", &updated).expect("update");
    query.refresh_run("run_2").expect("refresh");

    let after = query.render_prometheus().expect("after");
    assert!(after.contains("transist_runs_resumed_total"));
}

#[test]
fn replayed_runs_increment_replayed_total() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_replay");
    record.lineage = Some(RunLineage {
        mode: RunLineageMode::Replay,
        source_run_id: Some("run_original".to_string()),
        // Parentless replays are independent physical roots; only their source
        // lineage points at the original run.
        root_run_id: "run_replay".to_string(),
        parent_run_id: None,
        parent_step_id: None,
    });
    store.create_run(&record).expect("create");

    let prometheus = LocalMetricsQuery::new(tmp.path())
        .render_prometheus()
        .expect("render");
    assert_eq!(
        metric_sample_sum(&prometheus, "transist_runs_replayed_total"),
        1.0
    );
}

#[test]
fn corrupt_runs_increment_rebuild_errors_and_do_not_block_other_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    store.create_run(&sample_record("good")).expect("good");
    let corrupt_root = tmp.path().join("runs/run_bad");
    let corrupt_locator =
        transist_storage_local::run_paths::plan_run_locator(tmp.path(), "bad", None)
            .expect("plan corrupt locator");
    transist_storage_local::run_paths::write_run_locator(tmp.path(), &corrupt_locator)
        .expect("write corrupt locator");
    fs::create_dir_all(&corrupt_root).expect("dir");
    fs::write(corrupt_root.join("state.json"), "{not-json").expect("write");

    let query = LocalMetricsQuery::new(tmp.path());
    let run = query
        .get_run_metrics("good")
        .expect("get")
        .expect("good metrics");
    assert_eq!(run.run_id, "good");
    let prometheus = query.render_prometheus().expect("render");
    assert!(prometheus.contains("transist_metrics_rebuild_errors_total"));
}

fn subflow_result_with_child(run_id: &str, step_id: &str, child_run_id: &str) -> ExecutionResult {
    serde_json::from_value(serde_json::json!({
        "type": "tep.invoke.result.v1",
        "protocol_version": "tep.v1",
        "invoke_id": format!("subflow_{}_{}", run_id, step_id),
        "run_id": run_id,
        "step_id": step_id,
        "execution": 1,
        "status": "success",
        "stdout": "",
        "stderr": "",
        "timing": {"started_at": "0", "finished_at": "0", "duration_ms": 0},
        "artifacts": [],
        "evidence": [],
        "metrics": {},
        "output": {
            "child_run_id": child_run_id,
            "subflow_id": "child_wf",
            "status": "succeeded"
        },
        "usage": null,
        "error": null
    }))
    .expect("subflow result")
}

fn persist_run_with_step_result(
    store: &LocalRunStore,
    run_id: &str,
    step_id: &str,
    result: ExecutionResult,
) {
    let mut record = sample_record(run_id);
    record.status = KernelRunStatus::Success;
    record.steps.insert(
        step_id.to_string(),
        KernelStepState {
            step_id: step_id.to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(result.clone()),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create run");
    persist_execution(store, run_id, step_id, Some(result), None);
}

#[test]
fn subtree_token_and_cost_fold_across_nested_subflow_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    let leaf_usage = serde_json::json!({
        "provider": "openai",
        "model": "gpt-x",
        "input_tokens": 10,
        "output_tokens": 20,
        "total_tokens": 30
    });
    persist_run_with_step_result(
        &store,
        "leaf",
        "plugin_step",
        sample_result_with_usage(leaf_usage.clone()),
    );
    persist_run_with_step_result(
        &store,
        "mid",
        "subflow_step",
        subflow_result_with_child("mid", "subflow_step", "leaf"),
    );
    persist_run_with_step_result(
        &store,
        "root",
        "outer_subflow",
        subflow_result_with_child("root", "outer_subflow", "mid"),
    );

    let query = LocalMetricsQuery::new(tmp.path());

    let leaf = query
        .get_run_metrics("leaf")
        .expect("leaf")
        .expect("leaf metrics");
    assert_eq!(leaf.total_tokens, Some(30));

    let mid = query
        .get_run_metrics("mid")
        .expect("mid")
        .expect("mid metrics");
    assert_eq!(mid.total_tokens, Some(30));
    assert_eq!(mid.usage_breakdown.len(), 1);
    assert_eq!(mid.usage_breakdown[0].model.as_deref(), Some("gpt-x"));
    assert_eq!(mid.usage_breakdown[0].total_tokens, Some(30));

    let root = query
        .get_run_metrics("root")
        .expect("root")
        .expect("root metrics");
    assert_eq!(root.input_tokens, Some(10));
    assert_eq!(root.output_tokens, Some(20));
    assert_eq!(root.total_tokens, Some(30));
    assert_eq!(root.usage_breakdown.len(), 1);

    let root_step = query
        .get_step_metrics("root", "outer_subflow")
        .expect("root step")
        .expect("root step metrics");
    assert_eq!(root_step.child_run_id.as_deref(), Some("mid"));
    assert_eq!(root_step.total_tokens, Some(30));

    let run_list = query.run_list_metrics().expect("run list");
    assert_eq!(run_list["root"].total_tokens, Some(30));
}

#[test]
fn subtree_orphan_child_falls_back_to_self() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    persist_run_with_step_result(
        &store,
        "only_root",
        "subflow_step",
        subflow_result_with_child("only_root", "subflow_step", "never_persisted"),
    );

    let query = LocalMetricsQuery::new(tmp.path());
    let root = query
        .get_run_metrics("only_root")
        .expect("root")
        .expect("metrics");
    assert_eq!(root.total_tokens, None);
}

#[test]
fn subtree_sums_sibling_subflow_and_direct_executions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    persist_run_with_step_result(
        &store,
        "child_a",
        "plugin_step",
        sample_result_with_usage(serde_json::json!({
            "model": "model-a",
            "input_tokens": 1,
            "output_tokens": 2,
            "total_tokens": 3
        })),
    );

    let mut root_record = sample_record("root2");
    root_record.status = KernelRunStatus::Success;
    let root_invoke_result = sample_result_with_usage(serde_json::json!({
        "model": "model-b",
        "input_tokens": 4,
        "output_tokens": 5,
        "total_tokens": 9
    }));
    let root_subflow_result = subflow_result_with_child("root2", "nested", "child_a");
    root_record.steps.insert(
        "direct".to_string(),
        KernelStepState {
            step_id: "direct".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(root_invoke_result.clone()),
            started_at_ms: Some(100),
            finished_at_ms: Some(200),
            activation_executions_baseline: 0,
        },
    );
    root_record.steps.insert(
        "nested".to_string(),
        KernelStepState {
            step_id: "nested".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(root_subflow_result.clone()),
            started_at_ms: Some(210),
            finished_at_ms: Some(300),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&root_record).expect("create root2");
    persist_execution(&store, "root2", "direct", Some(root_invoke_result), None);
    persist_execution(&store, "root2", "nested", Some(root_subflow_result), None);

    let query = LocalMetricsQuery::new(tmp.path());
    let root = query
        .get_run_metrics("root2")
        .expect("root")
        .expect("metrics");
    assert_eq!(root.total_tokens, Some(12));
    assert_eq!(root.input_tokens, Some(5));
    assert_eq!(root.output_tokens, Some(7));
    let models: BTreeSet<_> = root
        .usage_breakdown
        .iter()
        .filter_map(|entry| entry.model.clone())
        .collect();
    assert!(models.contains("model-a"));
    assert!(models.contains("model-b"));
}

#[test]
fn subtree_counts_and_artifacts_roll_up_across_subflows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    persist_run_with_step_result(
        &store,
        "child_counts",
        "leaf_step",
        sample_result_with_usage(serde_json::json!({
            "model": "m",
            "total_tokens": 1
        })),
    );
    persist_run_with_step_result(
        &store,
        "root_counts",
        "subflow",
        subflow_result_with_child("root_counts", "subflow", "child_counts"),
    );

    let query = LocalMetricsQuery::new(tmp.path());
    let root = query
        .get_run_metrics("root_counts")
        .expect("root")
        .expect("metrics");
    assert_eq!(root.total_step_executions, 2);
    assert!(root.succeeded_step_executions >= 1);
    assert!(
        root.succeeded_step_executions
            + root.failed_step_executions
            + root.cancelled_step_executions
            <= root.total_step_executions
    );

    let run_list = query.run_list_metrics().expect("run list");
    assert_eq!(run_list["root_counts"].total_step_executions, 2);

    let step = query
        .get_step_metrics("root_counts", "subflow")
        .expect("step")
        .expect("step metrics");
    assert_eq!(step.execution_count, 2);
}

/// Regression: repeated refresh calls must not inflate folded subtree totals.
///
/// Before the pure-fold refactor, `fold_subtree_metrics` mutated
/// `RunContribution::run_summary` / `step_summaries` in place with `saturating_add`,
/// and `refresh_run` / `refresh_changed_runs` invoked it on the already-folded
/// index. Each refresh re-added every child's totals on top of the prior fold,
/// producing `execution_count` in the thousands on a run that only had a handful of
/// real executions (observed: 10832 folded executions on a run with 3 real ones).
///
/// This test pins the fix: the folded view must be idempotent with respect to the
/// number of refresh passes, and consistent with the persisted state.
#[test]
fn repeated_refresh_does_not_inflate_folded_totals() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

    persist_run_with_step_result(
        &store,
        "child_idempotent",
        "leaf_step",
        sample_result_with_usage(serde_json::json!({
            "model": "m",
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15
        })),
    );
    persist_run_with_step_result(
        &store,
        "root_idempotent",
        "subflow",
        subflow_result_with_child("root_idempotent", "subflow", "child_idempotent"),
    );

    let query = LocalMetricsQuery::new(tmp.path());

    let baseline_run = query
        .get_run_metrics("root_idempotent")
        .expect("baseline run")
        .expect("baseline run metrics");
    let baseline_step = query
        .get_step_metrics("root_idempotent", "subflow")
        .expect("baseline step")
        .expect("baseline step metrics");
    let baseline_run_list = query.run_list_metrics().expect("baseline list");

    // Drive several refresh passes — the code path that previously accumulated.
    for _ in 0..10 {
        query.refresh_run("root_idempotent").expect("refresh root");
        query
            .refresh_run("child_idempotent")
            .expect("refresh child");
        query.refresh_changed_runs().expect("refresh changed");
    }

    let after_run = query
        .get_run_metrics("root_idempotent")
        .expect("after run")
        .expect("after run metrics");
    let after_step = query
        .get_step_metrics("root_idempotent", "subflow")
        .expect("after step")
        .expect("after step metrics");
    let after_run_list = query.run_list_metrics().expect("after list");

    assert_eq!(
        after_run, baseline_run,
        "run summary must be stable across refreshes"
    );
    assert_eq!(
        after_step, baseline_step,
        "step summary must be stable across refreshes"
    );
    assert_eq!(
        after_run_list, baseline_run_list,
        "run list summary must be stable across refreshes"
    );

    // And match persisted reality: exactly two executions total (leaf + subflow),
    // one usage report folded into the subtree.
    assert_eq!(after_run.total_step_executions, 2);
    assert_eq!(after_step.execution_count, 2);
    assert_eq!(after_run.total_tokens, Some(15));
    assert_eq!(after_step.total_tokens, Some(15));
    assert_eq!(after_run_list["root_idempotent"].total_step_executions, 2);
}

#[test]
fn active_execution_duration_sums_all_attempts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_retry");
    record.status = KernelRunStatus::Success;
    record.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 2,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(sample_result_with_usage_and_duration(
                serde_json::json!(null),
                350,
            )),
            started_at_ms: Some(100),
            finished_at_ms: Some(700),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");

    persist_execution_record(
        &store,
        StepExecutionRecord {
            run_id: "run_retry".to_string(),
            step_id: "step".to_string(),
            execution: 1,
            status: "failed".to_string(),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            retry_wait_until_ms: None,
            request: None,
            result: Some(sample_result_with_usage_and_duration(
                serde_json::json!(null),
                250,
            )),
            metrics_validation_errors: None,
            state_mutations: Vec::new(),
            document_revisions: Vec::new(),
        },
    );
    persist_execution_record(
        &store,
        StepExecutionRecord {
            run_id: "run_retry".to_string(),
            step_id: "step".to_string(),
            execution: 2,
            status: "success".to_string(),
            started_at_ms: Some(360),
            finished_at_ms: Some(710),
            retry_wait_until_ms: None,
            request: None,
            result: Some(sample_result_with_usage_and_duration(
                serde_json::json!(null),
                350,
            )),
            metrics_validation_errors: None,
            state_mutations: Vec::new(),
            document_revisions: Vec::new(),
        },
    );

    let query = LocalMetricsQuery::new(tmp.path());
    let run = query
        .get_run_metrics("run_retry")
        .expect("run")
        .expect("run metrics");
    let step = query
        .get_step_metrics("run_retry", "step")
        .expect("step")
        .expect("step metrics");
    let list = query.run_list_metrics().expect("run list");

    assert_eq!(run.total_duration_ms, Some(600));
    assert_eq!(run.wall_clock_duration_ms, Some(600));
    assert_eq!(run.active_execution_duration_ms, Some(600));
    assert_eq!(run.blocked_wait_duration_ms, None);
    assert_eq!(step.active_execution_duration_ms, Some(600));
    assert_eq!(step.blocked_wait_duration_ms, None);
    assert_eq!(list["run_retry"].active_execution_duration_ms, Some(600));
}

#[test]
fn blocked_wait_duration_comes_from_gate_decision_intervals() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = sample_record("run_gate");
    record.status = KernelRunStatus::Blocked;
    record.steps.insert(
        "review_gate".to_string(),
        KernelStepState {
            step_id: "review_gate".to_string(),
            status: KernelStepStatus::Blocked,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: Some(sample_result_with_usage_and_duration(
                serde_json::json!(null),
                5,
            )),
            started_at_ms: Some(100),
            finished_at_ms: Some(200),
            activation_executions_baseline: 0,
        },
    );
    store.create_run(&record).expect("create");

    let recipe_yaml = serde_json::to_string(&transist_protocol::Recipe {
        schema: "transist.recipe.v1".to_string(),
        workflow: transist_protocol::WorkflowRef {
            id: "wf".to_string(),
            version: "v1".to_string(),
        },
        source_dir: None,
        repositories: Vec::new(),
        prerequisites: transist_protocol::PrerequisiteSet::default(),
        filesystem_policy: None,
        network_policy: None,
        inputs: BTreeMap::new(),
        workspace: None,
        steps: vec![transist_protocol::RecipeStep {
            id: "review_gate".to_string(),
            execution: transist_protocol::StepExecutionDescriptor::Control {
                primitive: "control.gate.manual".to_string(),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }],
        cleanup: Vec::new(),
        graph: None,
        capabilities: Default::default(),
        capabilities_persisted: false,
        documents: Default::default(),
    })
    .expect("recipe json");
    store
        .write_recipe_snapshot("run_gate", &recipe_yaml)
        .expect("write recipe");

    persist_execution_record(
        &store,
        StepExecutionRecord {
            run_id: "run_gate".to_string(),
            step_id: "review_gate".to_string(),
            execution: 1,
            status: "blocked".to_string(),
            started_at_ms: Some(100),
            finished_at_ms: Some(200),
            retry_wait_until_ms: None,
            request: None,
            result: Some(sample_result_with_usage_and_duration(
                serde_json::json!(null),
                5,
            )),
            metrics_validation_errors: None,
            state_mutations: Vec::new(),
            document_revisions: Vec::new(),
        },
    );
    persist_gate_decision(
        tmp.path(),
        "run_gate",
        "review_gate",
        "review_gate",
        "approved",
        800,
    );

    let query = LocalMetricsQuery::new(tmp.path());
    let run = query
        .get_run_metrics("run_gate")
        .expect("run")
        .expect("run metrics");
    let step = query
        .get_step_metrics("run_gate", "review_gate")
        .expect("step")
        .expect("step metrics");

    assert_eq!(step.blocked_wait_duration_ms, Some(600));
    assert_eq!(run.blocked_wait_duration_ms, Some(600));
    assert_eq!(run.active_execution_duration_ms, Some(5));
    assert!(run.wall_clock_duration_ms.is_some());
}
