// SPDX-License-Identifier: MPL-2.0
//! Run-preview regressions against hermetic storage and the read-only query port.

use super::step_outputs_api::{execution_path, record, state};
use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use transist::InspectionQueryPort;

const RUN: &str = "inspect-run";

async fn run_page(fixture: &Fixture, run_id: &str) -> String {
    let response = fixture
        .request(
            "GET",
            &format!("/inspect/runs/{run_id}?tab=steps"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    read_text(response).await
}

fn preview<'a>(html: &'a str, step_id: &str) -> &'a str {
    html.split("<tr class=\"step-output-preview\">")
        .skip(1)
        .map(|row| row.split("</tr>").next().unwrap())
        .find(|row| row.contains(&format!("<span class=\"mono\">{step_id}</span>")))
        .expect("preview for step")
}

fn escape(text: &str) -> String {
    askama::filters::escape(text, askama::filters::Html)
        .unwrap()
        .to_string()
}

fn seed_output(fixture: &Fixture, run_id: &str, step_id: &str, value: Value) {
    let store = LocalRunStore::new(LocalStorageConfig::new(&fixture.home_dir));
    let mut run = store.load_run(run_id).unwrap().unwrap();
    let mut result = sample_collect_result();
    result.run_id = run_id.into();
    result.step_id = step_id.into();
    result.output = value;
    let step = run.steps.get_mut(step_id).unwrap();
    step.status = KernelStepStatus::Succeeded;
    step.executions = 1;
    step.last_result = Some(result);
    // Model a legacy state-only result without requiring a runtime dispatch.
    let root =
        transist_storage_local::run_paths::resolve_run_root(&fixture.home_dir, run_id).unwrap();
    fs::write(root.join("state.json"), serde_json::to_vec(&run).unwrap()).unwrap();
}

struct CountingQuery {
    inner: transist::TransistInspectionContext,
    step_lists: Mutex<Vec<String>>,
    step_details: AtomicUsize,
    execution_lists: AtomicUsize,
}

// Forward unrelated reads to the real fixture query, retaining its behavior.
macro_rules! forward_reads {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> anyhow::Result<$ret> {
            self.inner.$name($($arg),*)
        })*
    };
}

impl InspectionQueryPort for CountingQuery {
    fn list_steps(&self, run_id: &str) -> anyhow::Result<Option<Vec<transist::InspectionStep>>> {
        self.step_lists.lock().unwrap().push(run_id.into());
        self.inner.list_steps(run_id)
    }

    fn get_step(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> anyhow::Result<Option<transist::InspectionStep>> {
        self.step_details.fetch_add(1, Ordering::SeqCst);
        self.inner.get_step(run_id, step_id)
    }

    fn list_step_executions(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> anyhow::Result<Option<Vec<transist::InspectionStepExecution>>> {
        self.execution_lists.fetch_add(1, Ordering::SeqCst);
        self.inner.list_step_executions(run_id, step_id)
    }

    forward_reads! {
        fn list_runs(query: &transist::RunListQuery) -> transist::Page<transist::InspectionRunSummary>;
        fn get_run(run_id: &str) -> Option<transist::InspectionRunDetail>;
        fn list_events(run_id: &str, query: &transist::EventListQuery) -> Option<transist::Page<transist::InspectionEvent>>;
        fn list_gates(run_id: &str) -> Option<Vec<transist::InspectionGate>>;
        fn list_failures(run_id: &str) -> Option<Vec<transist::FailureRecord>>;
        fn get_failure(run_id: &str, failure_id: &str) -> Option<transist::FailureRecord>;
        fn list_blobs(run_id: &str, family: transist::BlobFamily, scope: transist::BlobListScope) -> Option<Vec<transist::InspectionBlob>>;
        fn get_blob(run_id: &str, family: transist::BlobFamily, step_id: &str, execution: u32, relative_path: &str) -> Option<transist::InspectionBlob>;
        fn read_blob_content(run_id: &str, family: transist::BlobFamily, step_id: &str, execution: u32, relative_path: &str) -> Option<transist::InspectionBlobContent>;
        fn get_workflow_file(run_id: &str, step_id: &str, locator_kind: transist::InspectionWorkflowFileLocatorKind, relative_path: &str) -> Option<transist::InspectionWorkflowFileRef>;
        fn read_workflow_file_content(run_id: &str, step_id: &str, locator_kind: transist::InspectionWorkflowFileLocatorKind, relative_path: &str, workspace_root: Option<&Path>) -> Option<transist::InspectionWorkflowFileContent>;
        fn list_workflows(query: &transist::PaginationQuery) -> transist::Page<transist::InspectionWorkflowSummary>;
        fn get_workflow(workflow_ref: &str) -> Option<transist::InspectionWorkflowDetail>;
        fn read_workflow_source(workflow_ref: &str) -> Option<transist::InspectionWorkflowContent>;
        fn read_workflow_compiled_recipe(workflow_ref: &str) -> Option<transist::InspectionWorkflowContent>;
    }
}

#[tokio::test]
async fn step_outputs_preview_one_query_multiple_steps_and_attribution() {
    let mut fixture = Fixture::new();
    record(
        &fixture,
        2,
        "failed",
        Some(json!({"response_text": "Fictional failed advice"})),
    );
    seed_output(
        &fixture,
        RUN,
        "review",
        json!({"response_text": "Fictional review advice"}),
    );
    let query = Arc::new(CountingQuery {
        inner: transist::TransistInspectionContext::with_home(
            fixture.repo_root.clone(),
            fixture.home_dir.clone(),
        ),
        step_lists: Mutex::new(Vec::new()),
        step_details: AtomicUsize::new(0),
        execution_lists: AtomicUsize::new(0),
    });
    fixture.app = app_with_query(query.clone());
    let before = snapshot_tree(&fixture.home_dir);
    let html = run_page(&fixture, RUN).await;
    assert_eq!(query.step_lists.lock().unwrap().as_slice(), [RUN]);
    assert_eq!(query.step_details.load(Ordering::SeqCst), 0);
    assert_eq!(query.execution_lists.load(Ordering::SeqCst), 0);
    assert_eq!(
        html.matches("<tr class=\"step-output-preview\">").count(),
        2
    );
    for (step, ordinal, status, source, advice) in [
        (
            "collect",
            2,
            "failed",
            "execution_record",
            "Fictional failed advice",
        ),
        (
            "review",
            1,
            "success",
            "state_snapshot",
            "Fictional review advice",
        ),
    ] {
        let row = preview(&html, step);
        assert!(row.contains(&format!("Output execution: {ordinal}")));
        assert!(row.contains(&format!("status-{status}\">{status}")));
        assert!(row.contains(&format!("<code>{source}</code>")));
        assert!(row.contains(advice));
        let link = format!("/inspect/runs/{RUN}/steps/{step}?execution={ordinal}");
        assert!(row.contains(&format!("href=\"{link}\"")));
        let response = fixture.request("GET", &link, Body::empty()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let detail = read_text(response).await;
        assert!(detail.contains(&format!("Selected output execution: {ordinal}")));
        assert!(detail.contains(advice));
    }
    assert!(!preview(&html, "collect").contains("Fictional review advice"));
    assert!(!preview(&html, "review").contains("Fictional failed advice"));
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_preview_no_result_pending_inflight_record_and_legacy() {
    let mut fixture = Fixture::new();
    fs::remove_file(execution_path(&fixture, 1)).unwrap();
    state(&mut fixture, 0, KernelStepStatus::Pending, None);
    let html = run_page(&fixture, RUN).await;
    assert!(preview(&html, "collect").contains("Output execution: none"));
    assert!(preview(&html, "collect").contains("status-pending\">pending"));
    assert!(preview(&html, "collect").contains("No persisted result for this execution."));
    assert!(
        preview(&html, "collect")
            .contains("href=\"/inspect/runs/inspect-run/steps/collect?execution=latest\"")
    );
    record(
        &fixture,
        1,
        "failed",
        Some(json!({"response_text": "Fictional stale advice"})),
    );
    state(
        &mut fixture,
        2,
        KernelStepStatus::Running,
        Some(sample_collect_result()),
    );
    for recorded in [false, true] {
        if recorded {
            record(&fixture, 2, "interrupted", None);
        }
        let before = snapshot_tree(&fixture.home_dir);
        let html = run_page(&fixture, RUN).await;
        let row = preview(&html, "collect");
        assert!(row.contains("Output execution: 2"));
        assert!(row.contains(if recorded {
            "status-interrupted\">interrupted"
        } else {
            "status-running\">running"
        }));
        assert!(row.contains("No persisted result for this execution."));
        assert!(row.contains("href=\"/inspect/runs/inspect-run/steps/collect?execution=latest\""));
        assert!(!row.contains("Fictional stale advice"));
        assert!(!row.contains("Preview truncated"));
        assert_eq!(snapshot_tree(&fixture.home_dir), before);
    }
    fs::remove_file(execution_path(&fixture, 2)).unwrap();
    let mut result = sample_collect_result();
    result.execution = 2;
    result.output = Value::Null;
    state(&mut fixture, 2, KernelStepStatus::Failed, Some(result));
    let html = run_page(&fixture, RUN).await;
    let row = preview(&html, "collect");
    assert!(row.contains("<pre class=\"output-text\">null</pre>"));
    assert!(row.contains("<code>state_snapshot</code>"));
    assert!(row.contains("href=\"/inspect/runs/inspect-run/steps/collect?execution=2\""));
}

#[tokio::test]
async fn step_outputs_preview_unicode_boundaries_sanitization_and_detail_parity() {
    let fixture = Fixture::new();
    let overhead = serde_json::to_string_pretty(&json!({"response_text": ""}))
        .unwrap()
        .chars()
        .count();
    let mut values = Vec::new();
    for size in [511, 512, 513] {
        values.push(json!("🦀".repeat(size - 2)));
        values.push(json!({"response_text": "é".repeat(size - overhead)}));
    }
    values.extend([
        json!({"nested": [{"/tmp/private/key": "/home/testuser/private"}], "response_text": "<script>alert(1)</script> **Markdown**", "url": "https://example.test/a?path=/tmp/private&x=1"}),
        json!(format!("/tmp/{}", "🦀".repeat(600))),
        Value::Null, json!({}), json!([]), json!(false), json!(0), json!(""),
    ]);
    for value in values {
        record(&fixture, 1, "failed", Some(value));
        let before = snapshot_tree(&fixture.home_dir);
        let api = read_json(
            fixture
                .request(
                    "GET",
                    "/api/v1/runs/inspect-run/steps/collect?execution=1",
                    Body::empty(),
                )
                .await,
        )
        .await;
        let pretty = serde_json::to_string_pretty(&api["output"]["value"]).unwrap();
        let expected: String = pretty.chars().take(512).collect();
        let html = run_page(&fixture, RUN).await;
        let row = preview(&html, "collect");
        assert!(row.contains(&format!(
            "<pre class=\"output-text\">{}</pre>",
            escape(&expected)
        )));
        assert_eq!(
            row.contains("Preview truncated"),
            pretty.chars().count() > 512
        );
        assert!(!row.contains("No persisted result"));
        assert!(!row.contains("<script>"));
        assert!(!row.contains("/home/testuser/private"));
        assert!(!row.contains("/tmp/private/key"));
        assert_eq!(run_page(&fixture, RUN).await, html);
        assert_eq!(snapshot_tree(&fixture.home_dir), before);
    }
}

#[tokio::test]
async fn step_outputs_preview_nested_runs_keep_their_own_outputs() {
    let fixture = Fixture::with_nested_subflow_runs();
    let child = "inspect-run__plan_loop";
    let grandchild = "inspect-run__plan_loop__review_loop";
    record(
        &fixture,
        1,
        "failed",
        Some(json!({"response_text": "Fictional parent advice"})),
    );
    seed_output(
        &fixture,
        child,
        "child",
        json!({"response_text": "Fictional child advice"}),
    );
    seed_output(
        &fixture,
        grandchild,
        "child",
        json!({"response_text": "Fictional grandchild advice"}),
    );
    let before = snapshot_tree(&fixture.home_dir);
    for (run, step, advice) in [
        (RUN, "collect", "Fictional parent advice"),
        (child, "child", "Fictional child advice"),
        (grandchild, "child", "Fictional grandchild advice"),
    ] {
        let html = run_page(&fixture, run).await;
        let row = preview(&html, step);
        assert!(row.contains(advice));
        for other in [
            "Fictional parent advice",
            "Fictional child advice",
            "Fictional grandchild advice",
        ] {
            assert_eq!(row.contains(other), other == advice);
        }
        let link = format!("/inspect/runs/{run}/steps/{step}?execution=1");
        assert!(row.contains(&format!("href=\"{link}\"")));
        let response = fixture.request("GET", &link, Body::empty()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(read_text(response).await.contains(advice));
    }
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
}

#[tokio::test]
async fn step_outputs_preview_links_pin_results_and_compact_apis_stay_unchanged() {
    let mut fixture = Fixture::new();
    record(
        &fixture,
        1,
        "failed",
        Some(json!({"response_text": "Fictional original advice"})),
    );
    let paths = [
        "/api/v1/runs",
        "/api/v1/runs/inspect-run",
        "/api/v1/runs/inspect-run/steps",
    ];
    let mut compact = Vec::new();
    for path in paths {
        compact.push(read_json(fixture.request("GET", path, Body::empty()).await).await);
    }
    let before = snapshot_tree(&fixture.home_dir);
    let html = run_page(&fixture, RUN).await;
    assert!(
        preview(&html, "collect")
            .contains("href=\"/inspect/runs/inspect-run/steps/collect?execution=1\"")
    );
    for (path, expected) in paths.into_iter().zip(compact) {
        let actual = read_json(fixture.request("GET", path, Body::empty()).await).await;
        assert_eq!(actual, expected);
        let text = actual.to_string();
        for field in [
            "\"output\":",
            "\"latest_output\":",
            "\"output_executions\":",
            "Fictional original advice",
        ] {
            assert!(!text.contains(field), "{path} expanded compact output");
        }
    }
    assert_eq!(snapshot_tree(&fixture.home_dir), before);
    record(
        &fixture,
        2,
        "success",
        Some(json!({"response_text": "Fictional newer advice"})),
    );
    state(&mut fixture, 2, KernelStepStatus::Succeeded, None);
    let response = fixture
        .request(
            "GET",
            "/inspect/runs/inspect-run/steps/collect?execution=1",
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let historical = read_text(response).await;
    assert!(historical.contains("Fictional original advice"));
    assert!(historical.contains("Selected output execution: 1"));
    assert!(preview(&run_page(&fixture, RUN).await, "collect").contains("Fictional newer advice"));
}
