// SPDX-License-Identifier: MPL-2.0
#[path = "support/gov_demo.rs"]
mod gov_demo_support;
#[path = "support/mod.rs"]
mod runtime_test_support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use runtime_test_support::{
    compile_prepare, init_repo, install_fixture_local_process_plugin,
    install_fixture_process_plugin, install_shell_plugin, read_events, read_execution, read_lines,
    read_provenance, read_recipe, read_run_inputs, read_state, run_root, write_flow, write_json,
};

/// Polls persisted run state until `step_id` reports `status`, or panics after
/// a generous deadline. Tolerates a run record that has not been created yet
/// (e.g. a replay whose target run is still being prepared) so concurrent
/// source/replay launches can be observed without racing the run locator.
fn wait_for_step_status(repo_root: &Path, run_id: &str, step_id: &str, status: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(state) = try_read_state(repo_root, run_id) {
            if state["steps"][step_id]["status"].as_str() == Some(status) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for step '{}' in run '{}' to reach status '{}'",
            step_id,
            run_id,
            status
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Best-effort read of persisted run state that returns `None` when the run
/// has not been created yet, instead of panicking like `read_state`.
fn try_read_state(repo_root: &Path, run_id: &str) -> Option<serde_json::Value> {
    let root = transist::TransistContext::repo_local(repo_root)
        .run_root(run_id)
        .ok()?;
    let raw = fs::read_to_string(root.join("state.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

fn wait_for_entered_workers(entered_path: &Path, expected_workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let entered_workers = fs::read_to_string(entered_path)
            .map(|contents| contents.lines().count())
            .unwrap_or_default();
        if entered_workers >= expected_workers {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected_workers} workers to enter blocking fixture"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

struct ActiveRunCleanup {
    ctx: transist::TransistContext,
    run_ids: [&'static str; 2],
    release_path: PathBuf,
    source_handle: Option<thread::JoinHandle<()>>,
    replay_handle: Option<thread::JoinHandle<()>>,
}

impl ActiveRunCleanup {
    fn join_workers(&mut self) {
        self.source_handle
            .take()
            .expect("source worker handle")
            .join()
            .expect("source thread join");
        self.replay_handle
            .take()
            .expect("replay worker handle")
            .join()
            .expect("replay thread join");
    }
}

impl Drop for ActiveRunCleanup {
    fn drop(&mut self) {
        if self.source_handle.is_some() || self.replay_handle.is_some() {
            for run_id in self.run_ids {
                let _ = self.ctx.request_run_cancellation(run_id);
            }
            let _ = fs::write(&self.release_path, "release");
            if let Some(handle) = self.source_handle.take() {
                let _ = handle.join();
            }
            if let Some(handle) = self.replay_handle.take() {
                let _ = handle.join();
            }
        }
    }
}
use serde_json::Value;
use transist::{
    BlobFamily, BlobListScope, GateDecisionInput, KernelValue, TransistInspectionContext,
};

#[test]
fn successful_workflow_execution_persists_state_inputs_recipe_and_events() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "success.flow.yaml",
        r#"
schema: transist.flow.v1
id: success_demo
version: v1
plugins:
  shell: 0.1.0
inputs:
  message:
    type: string
    required: true
steps:
  - id: echo
    uses: process:shell/echo
    with:
      args: ["{{inputs.message}}", "world"]
"#,
    );

    let started = compile_prepare(
        repo.path(),
        &flow,
        "success_demo@v1",
        "success-run",
        BTreeMap::from([(
            "message".to_string(),
            KernelValue::String("hello".to_string()),
        )]),
    );
    assert_eq!(started.status, "planned");

    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("success-run")
        .expect("run");
    assert_eq!(report.run_id, "success-run");
    assert_eq!(report.status, "success");

    let state = read_state(repo.path(), "success-run");
    assert_eq!(state["status"], "success");
    assert_eq!(state["lineage"]["mode"], "fresh");
    assert_eq!(state["recipe_snapshot"]["path"], "recipe.yaml");
    assert_eq!(state["workflow_inputs"]["message"], "hello");
    assert_eq!(state["steps"]["echo"]["status"], "succeeded");
    assert_eq!(state["steps"]["echo"]["executions"], 1);

    let run_inputs = read_run_inputs(repo.path(), "success-run");
    assert_eq!(run_inputs["values"]["message"], "hello");
    assert_eq!(run_inputs["sources"]["message"], "caller_provided");

    let provenance = read_provenance(repo.path(), "success-run");
    assert_eq!(provenance["workflow_id"], "success_demo");
    assert_eq!(provenance["decision"], "allowed");
    assert!(provenance["finalized_at_ms"].as_u64().unwrap_or_default() > 0);

    let recipe = read_recipe(repo.path(), "success-run");
    assert!(recipe.contains("success_demo"));

    let events = read_events(repo.path(), "success-run");
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "run_preflight_started")
    );
    assert_eq!(events.last().expect("last event")["kind"], "run_finished");

    let tail = transist::TransistContext::repo_local(repo.path())
        .runs_tail("success-run", 10)
        .expect("tail");
    assert_eq!(tail.last().expect("tail last").kind, "run_finished");
}

#[test]
fn step_failure_and_resume_keep_append_only_execution_history() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "resume.flow.yaml",
        r#"
schema: transist.flow.v1
id: resume_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: verify_marker
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "test -f .resume-ok"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "resume_demo@v1",
        "resume-run",
        BTreeMap::new(),
    );
    let failed = transist::TransistContext::repo_local(repo.path())
        .run_started_run("resume-run")
        .expect("run failed");
    assert_eq!(failed.status, "failed");

    let failed_state = read_state(repo.path(), "resume-run");
    assert_eq!(failed_state["status"], "failed");
    assert_eq!(failed_state["steps"]["verify_marker"]["status"], "failed");
    assert_eq!(
        failed_state["steps"]["verify_marker"]["last_result"]["error"]["code"],
        "tool_execution_error"
    );
    assert_eq!(failed_state["steps"]["verify_marker"]["executions"], 1);

    let first_execution = read_execution(repo.path(), "resume-run", "verify_marker", 1);
    assert_eq!(first_execution["status"], "failed");
    assert_eq!(first_execution["result"]["status"], "failed");
    assert_eq!(
        first_execution["result"]["error"]["code"],
        "tool_execution_error"
    );

    fs::write(repo.path().join(".resume-ok"), "").expect("marker");

    let resumed = transist::TransistContext::repo_local(repo.path())
        .runs_resume("resume-run")
        .expect("resume");
    assert_eq!(resumed.run_id, "resume-run");
    assert_eq!(resumed.status, "success");

    let resumed_state = read_state(repo.path(), "resume-run");
    assert_eq!(resumed_state["status"], "success");
    assert_eq!(resumed_state["resume_count"], 1);
    assert_eq!(
        resumed_state["steps"]["verify_marker"]["status"],
        "succeeded"
    );
    assert_eq!(resumed_state["steps"]["verify_marker"]["executions"], 2);

    let second_execution = read_execution(repo.path(), "resume-run", "verify_marker", 2);
    assert_eq!(second_execution["status"], "success");
    assert_eq!(second_execution["result"]["status"], "success");
}

#[test]
fn replay_creates_new_lineage_and_reexecutes_from_persisted_inputs() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "replay.flow.yaml",
        r#"
schema: transist.flow.v1
id: replay_demo
version: v1
plugins:
  shell: 0.1.0
inputs:
  marker:
    type: string
    required: true
steps:
  - id: append_marker
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "printf '%s\n' \"$1\" >> replay.log", "--", "{{inputs.marker}}"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "replay_demo@v1",
        "source-run",
        BTreeMap::from([(
            "marker".to_string(),
            KernelValue::String("replayed-input".to_string()),
        )]),
    );
    let source = transist::TransistContext::repo_local(repo.path())
        .run_started_run("source-run")
        .expect("source run");
    assert_eq!(source.status, "success");

    let replay = transist::TransistContext::repo_local(repo.path())
        .runs_replay("source-run", Some("replay-run"))
        .expect("replay run");
    assert_eq!(replay.run_id, "replay-run");
    assert_eq!(replay.status, "success");

    let replayed_lines = read_lines(&repo.path().join("replay.log"));
    assert_eq!(replayed_lines, vec!["replayed-input", "replayed-input"]);

    let source_state = read_state(repo.path(), "source-run");
    let replay_state = read_state(repo.path(), "replay-run");
    assert_eq!(source_state["lineage"]["mode"], "fresh");
    assert_eq!(replay_state["lineage"]["mode"], "replay");
    assert_eq!(replay_state["lineage"]["source_run_id"], "source-run");
    assert_eq!(replay_state["lineage"]["root_run_id"], "replay-run");
    assert_eq!(replay_state["steps"]["append_marker"]["executions"], 1);

    let home_dir = transist::repo_local_home_dir(repo.path());
    let replay_locator =
        transist_storage_local::load_run_locator(&home_dir, "replay-run").expect("replay locator");
    assert_eq!(replay_locator.root_run_id, "replay-run");
    assert_eq!(replay_locator.relative_path, "run_replay-run");
    assert_eq!(
        transist_storage_local::resolve_run_root(&home_dir, "replay-run")
            .expect("resolve replay root"),
        home_dir.join("runs/run_replay-run")
    );
    assert_eq!(
        transist_storage_local::resolve_root_run_id(&home_dir, "replay-run")
            .expect("resolve replay root id"),
        "replay-run"
    );

    let cancellation = transist::TransistContext::repo_local(repo.path())
        .request_run_cancellation("replay-run")
        .expect("request replay cancellation");
    assert_eq!(cancellation.root_run_id, "replay-run");
    assert!(
        home_dir
            .join("control/runs/replay-run/cancel-request.json")
            .is_file()
    );
    assert!(!home_dir.join("control/runs/source-run").exists());

    let source_inputs = read_run_inputs(repo.path(), "source-run");
    let replay_inputs = read_run_inputs(repo.path(), "replay-run");
    assert_eq!(source_inputs["values"], replay_inputs["values"]);
    assert_eq!(
        read_recipe(repo.path(), "source-run"),
        read_recipe(repo.path(), "replay-run")
    );
}

#[test]
fn replay_cancellation_is_isolated_from_its_source_root() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "replay-cancellation.flow.yaml",
        r#"
schema: transist.flow.v1
id: replay_cancellation_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: echo
    uses: process:shell/echo
    with:
      args: ["ok"]
"#,
    );

    // A cancellation request for the source must not stop its replay root.
    compile_prepare(
        repo.path(),
        &flow,
        "replay_cancellation_demo@v1",
        "source-canceled",
        BTreeMap::new(),
    );
    transist::TransistContext::repo_local(repo.path())
        .request_run_cancellation("source-canceled")
        .expect("cancel source");
    let replay = transist::TransistContext::repo_local(repo.path())
        .runs_replay("source-canceled", Some("replay-unaffected"))
        .expect("replay despite source cancellation");
    assert_eq!(replay.status, "success");
    assert_eq!(
        read_state(repo.path(), "replay-unaffected")["status"],
        "success"
    );

    // A cancellation request for the replay must not stop its source root.
    compile_prepare(
        repo.path(),
        &flow,
        "replay_cancellation_demo@v1",
        "source-unaffected",
        BTreeMap::new(),
    );
    transist::TransistContext::repo_local(repo.path())
        .runs_replay("source-unaffected", Some("replay-canceled"))
        .expect("replay");
    transist::TransistContext::repo_local(repo.path())
        .request_run_cancellation("replay-canceled")
        .expect("cancel replay");
    let source = transist::TransistContext::repo_local(repo.path())
        .run_started_run("source-unaffected")
        .expect("source despite replay cancellation");
    assert_eq!(source.status, "success");
    assert_eq!(
        read_state(repo.path(), "source-unaffected")["status"],
        "success"
    );
}

#[test]
fn active_source_cancellation_does_not_stop_concurrent_replay_root() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());
    let entered_path = repo.path().join("entered-workers");
    let release_path = repo.path().join("release-workers");

    let flow = write_flow(
        repo.path(),
        "active-replay-cancellation-source.flow.yaml",
        r#"
schema: transist.flow.v1
id: active_replay_cancellation_demo
version: v1
plugins:
  shell: 0.1.0
inputs:
  entered_path:
    type: string
    required: true
  release_path:
    type: string
    required: true
steps:
  - id: wait_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "printf '%s\\n' \"$$\" >> \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.01; done", "--", "{{inputs.entered_path}}", "{{inputs.release_path}}"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "active_replay_cancellation_demo@v1",
        "source-active",
        BTreeMap::from([
            (
                "entered_path".to_string(),
                KernelValue::String(entered_path.display().to_string()),
            ),
            (
                "release_path".to_string(),
                KernelValue::String(release_path.display().to_string()),
            ),
        ]),
    );

    let ctx = transist::TransistContext::repo_local(repo.path());
    let mut cleanup = ActiveRunCleanup {
        ctx: ctx.clone(),
        run_ids: ["source-active", "replay-unaffected"],
        release_path: release_path.clone(),
        source_handle: None,
        replay_handle: None,
    };
    let source_ctx = ctx.clone();
    let (source_tx, source_rx) = mpsc::channel();
    cleanup.source_handle = Some(thread::spawn(move || {
        source_tx
            .send(source_ctx.run_started_run("source-active"))
            .expect("send source result");
    }));

    wait_for_step_status(repo.path(), "source-active", "wait_step", "running");

    // Start a replay of the still-running source. The replay reuses the
    // source's persisted inputs and recipe but owns an independent root run
    // tree, so a cancellation request for the source root must not reach it.
    let replay_ctx = ctx.clone();
    let (replay_tx, replay_rx) = mpsc::channel();
    cleanup.replay_handle = Some(thread::spawn(move || {
        replay_tx
            .send(replay_ctx.runs_replay("source-active", Some("replay-unaffected")))
            .expect("send replay result");
    }));

    wait_for_step_status(repo.path(), "replay-unaffected", "wait_step", "running");
    wait_for_entered_workers(&entered_path, 2);

    // Both runs are actively executing wait_step when the cancellation is
    // requested for the source root. This is the active-cancellation
    // isolation contract: the cancel must propagate to the source root only.
    let cancellation = ctx
        .request_run_cancellation("source-active")
        .expect("cancel source");
    assert_eq!(cancellation.root_run_id, "source-active");

    wait_for_step_status(repo.path(), "source-active", "wait_step", "canceled");
    fs::write(&release_path, "release").expect("release blocking fixture");
    cleanup.join_workers();

    let source_report = source_rx
        .recv()
        .expect("source thread result")
        .expect("source run report");
    assert_eq!(source_report.status, "canceled");

    let replay_report = replay_rx
        .recv()
        .expect("replay thread result")
        .expect("replay run report");
    assert_eq!(replay_report.run_id, "replay-unaffected");
    assert_eq!(replay_report.status, "success");

    let source_state = read_state(repo.path(), "source-active");
    assert_eq!(source_state["status"], "canceled");
    assert_eq!(source_state["steps"]["wait_step"]["status"], "canceled");

    let replay_state = read_state(repo.path(), "replay-unaffected");
    assert_eq!(replay_state["status"], "success");
    assert_eq!(replay_state["lineage"]["mode"], "replay");
    assert_eq!(replay_state["lineage"]["source_run_id"], "source-active");
    assert_eq!(replay_state["steps"]["wait_step"]["status"], "succeeded");
}

#[test]
fn active_replay_cancellation_does_not_stop_concurrent_source_root() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());
    let entered_path = repo.path().join("entered-workers");
    let release_path = repo.path().join("release-workers");

    let flow = write_flow(
        repo.path(),
        "active-replay-cancellation-replay.flow.yaml",
        r#"
schema: transist.flow.v1
id: active_replay_cancellation_demo
version: v1
plugins:
  shell: 0.1.0
inputs:
  entered_path:
    type: string
    required: true
  release_path:
    type: string
    required: true
steps:
  - id: wait_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "printf '%s\\n' \"$$\" >> \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.01; done", "--", "{{inputs.entered_path}}", "{{inputs.release_path}}"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "active_replay_cancellation_demo@v1",
        "source-unaffected",
        BTreeMap::from([
            (
                "entered_path".to_string(),
                KernelValue::String(entered_path.display().to_string()),
            ),
            (
                "release_path".to_string(),
                KernelValue::String(release_path.display().to_string()),
            ),
        ]),
    );

    let ctx = transist::TransistContext::repo_local(repo.path());
    let mut cleanup = ActiveRunCleanup {
        ctx: ctx.clone(),
        run_ids: ["source-unaffected", "replay-canceled"],
        release_path: release_path.clone(),
        source_handle: None,
        replay_handle: None,
    };
    let source_ctx = ctx.clone();
    let (source_tx, source_rx) = mpsc::channel();
    cleanup.source_handle = Some(thread::spawn(move || {
        source_tx
            .send(source_ctx.run_started_run("source-unaffected"))
            .expect("send source result");
    }));

    wait_for_step_status(repo.path(), "source-unaffected", "wait_step", "running");

    // Start a replay of the still-running source so both runs hold an active
    // wait_step invocation at the moment the replay root is canceled.
    let replay_ctx = ctx.clone();
    let (replay_tx, replay_rx) = mpsc::channel();
    cleanup.replay_handle = Some(thread::spawn(move || {
        replay_tx
            .send(replay_ctx.runs_replay("source-unaffected", Some("replay-canceled")))
            .expect("send replay result");
    }));

    wait_for_step_status(repo.path(), "replay-canceled", "wait_step", "running");
    wait_for_entered_workers(&entered_path, 2);

    // Both runs are actively executing wait_step when the cancellation is
    // requested for the replay root. The cancel must propagate to the replay
    // root only, leaving the source root to complete successfully.
    let cancellation = ctx
        .request_run_cancellation("replay-canceled")
        .expect("cancel replay");
    assert_eq!(cancellation.root_run_id, "replay-canceled");

    wait_for_step_status(repo.path(), "replay-canceled", "wait_step", "canceled");
    fs::write(&release_path, "release").expect("release blocking fixture");
    cleanup.join_workers();

    let replay_report = replay_rx
        .recv()
        .expect("replay thread result")
        .expect("replay run report");
    assert_eq!(replay_report.run_id, "replay-canceled");
    assert_eq!(replay_report.status, "canceled");

    let source_report = source_rx
        .recv()
        .expect("source thread result")
        .expect("source run report");
    assert_eq!(source_report.status, "success");

    let replay_state = read_state(repo.path(), "replay-canceled");
    assert_eq!(replay_state["status"], "canceled");
    assert_eq!(replay_state["lineage"]["mode"], "replay");
    assert_eq!(
        replay_state["lineage"]["source_run_id"],
        "source-unaffected"
    );
    assert_eq!(replay_state["steps"]["wait_step"]["status"], "canceled");

    let source_state = read_state(repo.path(), "source-unaffected");
    assert_eq!(source_state["status"], "success");
    assert_eq!(source_state["steps"]["wait_step"]["status"], "succeeded");
}

#[test]
fn artifacts_and_evidence_can_be_listed_and_read_back_from_persisted_storage() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_fixture_process_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "evidence.flow.yaml",
        r#"
schema: transist.flow.v1
id: evidence_demo
version: v1
plugins:
  fixture.process: 1.0.0
steps:
  - id: collect
    uses: process:fixture.process/emit
    with:
      stdout: fixture-ok
      artifact_contents: artifact-payload
      evidence_contents: evidence-payload
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "evidence_demo@v1",
        "evidence-run",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("evidence-run")
        .expect("run");
    assert_eq!(report.status, "success");

    let state = read_state(repo.path(), "evidence-run");
    assert_eq!(state["status"], "success");
    assert!(
        run_root(repo.path(), "evidence-run")
            .join("events.jsonl")
            .exists()
    );
    assert!(
        run_root(repo.path(), "evidence-run")
            .join("artifacts/collect/execution_001/artifacts/000_stdout.txt")
            .exists()
    );
    assert!(
        run_root(repo.path(), "evidence-run")
            .join("artifacts/collect/execution_001/evidence/000_trace.log")
            .exists()
    );

    let inspection = TransistInspectionContext::with_home(
        repo.path(),
        transist::repo_local_home_dir(repo.path()),
    );
    let run_detail = inspection
        .get_run("evidence-run")
        .expect("get run detail")
        .expect("run detail");
    assert_eq!(run_detail.artifact_count, 1);
    assert_eq!(run_detail.evidence_count, 2);

    let artifacts = inspection
        .list_blobs(
            "evidence-run",
            BlobFamily::Artifact,
            BlobListScope::SelfOnly,
        )
        .expect("artifact list")
        .expect("artifact blobs");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].relative_path, "000_stdout.txt");
    assert_eq!(
        artifacts[0]
            .descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.status.as_deref()),
        Some("success")
    );

    let artifact_content = inspection
        .read_blob_content(
            "evidence-run",
            BlobFamily::Artifact,
            "collect",
            1,
            &artifacts[0].relative_path,
        )
        .expect("artifact content")
        .expect("artifact bytes");
    assert_eq!(
        String::from_utf8(artifact_content.bytes).expect("artifact utf8"),
        "artifact-payload"
    );

    let evidence = inspection
        .list_blobs(
            "evidence-run",
            BlobFamily::Evidence,
            BlobListScope::SelfOnly,
        )
        .expect("evidence list")
        .expect("evidence blobs");
    assert_eq!(evidence.len(), 2);
    let plugin_evidence = evidence
        .iter()
        .find(|blob| blob.relative_path == "000_trace.log")
        .expect("plugin evidence");
    let runtime_evidence = evidence
        .iter()
        .find(|blob| blob.relative_path == "transist_runtime_invoke_evidence.json")
        .expect("runtime evidence");
    assert_eq!(
        plugin_evidence
            .descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.status.as_deref()),
        Some("success")
    );

    let evidence_content = inspection
        .read_blob_content(
            "evidence-run",
            BlobFamily::Evidence,
            "collect",
            1,
            &plugin_evidence.relative_path,
        )
        .expect("evidence content")
        .expect("evidence bytes");
    assert_eq!(
        String::from_utf8(evidence_content.bytes).expect("evidence utf8"),
        "evidence-payload"
    );

    let runtime_evidence_content = inspection
        .read_blob_content(
            "evidence-run",
            BlobFamily::Evidence,
            "collect",
            1,
            &runtime_evidence.relative_path,
        )
        .expect("runtime evidence content")
        .expect("runtime evidence bytes");
    let runtime_evidence_json: Value =
        serde_json::from_slice(&runtime_evidence_content.bytes).expect("runtime evidence json");
    assert_eq!(
        runtime_evidence_json["schema"],
        "transist.runtime.invoke_evidence.v1"
    );
    assert_eq!(runtime_evidence_json["plugin_id"], "fixture.process");
    assert_eq!(
        runtime_evidence_json["artifact_refs"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        runtime_evidence_json["plugin_evidence_refs"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        runtime_evidence_json["producing_workflow"]["id"],
        "evidence_demo"
    );
    assert_eq!(runtime_evidence_json["producing_workflow"]["version"], "v1");
    assert_eq!(
        runtime_evidence_json["producing_plugin"]["id"],
        "fixture.process"
    );
    assert!(runtime_evidence_json.get("usage").is_none());

    let exported = transist::TransistContext::repo_local(repo.path())
        .run_evidence_export("evidence-run")
        .expect("evidence export");
    assert_eq!(exported.provenance.decision, "allowed");
    assert!(exported.markdown.contains("# Run Evidence Report"));
    assert!(exported.markdown.contains("Workflow: `evidence_demo@v1`"));
}

#[test]
fn invoke_steps_produce_baseline_runtime_evidence_without_plugin_evidence_hints() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "baseline-evidence.flow.yaml",
        r#"
schema: transist.flow.v1
id: baseline_evidence_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: invoke
    uses: process:shell/echo
    with:
      args: ["hello", "evidence"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "baseline_evidence_demo@v1",
        "baseline-evidence-run",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("baseline-evidence-run")
        .expect("run");
    assert_eq!(report.status, "success");

    let state = read_state(repo.path(), "baseline-evidence-run");
    let persisted_evidence = state["steps"]["invoke"]["last_result"]["evidence"]
        .as_array()
        .expect("persisted evidence refs");
    assert_eq!(persisted_evidence.len(), 1);
    assert!(
        persisted_evidence[0]["path"]
            .as_str()
            .expect("runtime evidence path")
            .ends_with("/evidence/transist_runtime_invoke_evidence.json")
    );
    assert!(
        run_root(repo.path(), "baseline-evidence-run")
            .join("artifacts/invoke/execution_001/evidence/transist_runtime_invoke_evidence.json")
            .exists()
    );

    let inspection = TransistInspectionContext::repo_local(repo.path());
    let run_detail = inspection
        .get_run("baseline-evidence-run")
        .expect("run detail query")
        .expect("run detail");
    assert_eq!(run_detail.evidence_count, 1);

    let evidence = inspection
        .list_blobs(
            "baseline-evidence-run",
            BlobFamily::Evidence,
            BlobListScope::SelfOnly,
        )
        .expect("evidence list")
        .expect("evidence blobs");
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        evidence[0].relative_path,
        "transist_runtime_invoke_evidence.json"
    );

    let runtime_evidence_content = inspection
        .read_blob_content(
            "baseline-evidence-run",
            BlobFamily::Evidence,
            "invoke",
            1,
            "transist_runtime_invoke_evidence.json",
        )
        .expect("runtime evidence content")
        .expect("runtime evidence bytes");
    let runtime_evidence_json: Value =
        serde_json::from_slice(&runtime_evidence_content.bytes).expect("runtime evidence json");
    assert_eq!(
        runtime_evidence_json["schema"],
        "transist.runtime.invoke_evidence.v1"
    );
    assert_eq!(runtime_evidence_json["plugin_id"], "shell");
    assert_eq!(
        runtime_evidence_json["artifact_refs"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        runtime_evidence_json["plugin_evidence_refs"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
}

#[test]
fn local_only_prepare_fails_reachable_external_provider_steps() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_fixture_process_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "local-only-denied.flow.yaml",
        r#"
schema: transist.flow.v1
id: local_only_denied_demo
version: v1
plugins:
  fixture.process: 1.0.0
steps:
  - id: emit
    uses: process:fixture.process/emit
    with: {}
"#,
    );

    let ctx = transist::TransistContext::repo_local(repo.path());
    ctx.compile_flow_file(&flow).expect("compile flow");
    let validation = ctx
        .validate_bound_workflow("local_only_denied_demo@v1", BTreeMap::new(), false, true)
        .expect("validate local-only workflow");

    assert_eq!(validation.status.as_str(), "failed");
    let checks = &validation.checks;
    assert!(checks.iter().any(|check| {
        check.code == "local_only_denied_external"
            && check.status.as_str() == "failed"
            && check.step_path.as_deref() == Some("emit")
    }));
    assert!(validation.required_capabilities.iter().any(|capability| {
        capability.capability.as_str() == "provider"
            && capability.resource == "external"
            && capability.operations == ["use"]
    }));

    let runs_before = ctx.runs_list().expect("runs list");
    let runs_dir = ctx.home_dir().join("runs");
    let run_storage_entries = || {
        let mut entries = fs::read_dir(&runs_dir)
            .expect("runs storage")
            .map(|entry| {
                entry
                    .expect("runs storage entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    let storage_before = run_storage_entries();

    let error = ctx
        .prepare_workflow_with_trust(
            "local_only_denied_demo@v1",
            Some("local-only-denied-run"),
            BTreeMap::new(),
            false,
            true,
        )
        .expect_err("external provider must be rejected before run creation");
    assert!(
        error
            .to_string()
            .contains("deterministic pre-dispatch blockers"),
        "unexpected preparation error: {error:#}"
    );
    assert_eq!(ctx.runs_list().expect("runs list"), runs_before);
    assert_eq!(run_storage_entries(), storage_before);
    assert!(
        ctx.run_root("local-only-denied-run").is_err(),
        "pre-dispatch rejection must not publish a run locator"
    );
}

#[test]
fn local_only_local_plugin_run_records_zero_external_transfers() {
    // This is a required Linux local-only contract, not an optional
    // best-effort check. A broken bwrap loopback handshake fails naturally.
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_fixture_local_process_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "local-only-local.flow.yaml",
        r#"
schema: transist.flow.v1
id: local_only_local_demo
version: v1
plugins:
  fixture.local: 1.0.0
steps:
  - id: emit
    uses: process:fixture.local/emit
    with: {}
"#,
    );

    let ctx = transist::TransistContext::repo_local(repo.path());
    ctx.compile_flow_file(&flow).expect("compile flow");
    let prepared = ctx
        .prepare_workflow_with_trust(
            "local_only_local_demo@v1",
            Some("local-only-local-run"),
            BTreeMap::new(),
            false,
            true,
        )
        .expect("prepare local-only local run");
    assert_eq!(prepared.preflight.status.as_str(), "passed");

    let report = ctx
        .run_started_run_with_trust("local-only-local-run", false, true)
        .expect("run local-only local plugin");
    assert_eq!(report.status, "success");

    let provenance = read_provenance(repo.path(), "local-only-local-run");
    assert_eq!(provenance["local_only_mode"], true);
    assert_eq!(provenance["external_transfer_summary"]["count"], 0);
    assert_eq!(
        provenance["external_transfer_summary"]["external_transfers"],
        serde_json::json!([])
    );

    let state = read_state(repo.path(), "local-only-local-run");
    let evidence = state["steps"]["emit"]["last_result"]["evidence"]
        .as_array()
        .expect("evidence refs");
    assert!(evidence.iter().all(|file_ref| {
        !file_ref["path"]
            .as_str()
            .unwrap_or_default()
            .contains("transist_external_transfer")
    }));
}

#[test]
fn trusted_external_plugin_run_records_external_transfer_ledger() {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = gov_demo_support::init_gov_repo(repo.path());
    let ctx = transist::TransistContext::with_home(repo.path(), &home);

    let plugin_dir = repo.path().join("fixture-process-plugin");
    runtime_test_support::write_fixture_process_plugin_dir(&plugin_dir);
    gov_demo_support::sign_plugin(&plugin_dir, "fixture.process", "1.0.0").expect("sign plugin");
    ctx.install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("install signed plugin");

    let flow = write_flow(
        repo.path(),
        "trusted-external-transfer.flow.yaml",
        r#"
schema: transist.flow.v1
id: trusted_external_transfer_demo
version: v1
plugins:
  fixture.process: 1.0.0
steps:
  - id: emit
    uses: process:fixture.process/emit
    with:
      stdout: trusted-external-ok
      usage:
        provider: openai
        model: gpt-5.4
"#,
    );
    gov_demo_support::sign_workflow(&flow, "trusted_external_transfer_demo", "v1")
        .expect("sign workflow");
    ctx.compile_flow_file_with_trust(&flow, true)
        .expect("compile trusted workflow");

    let report = ctx
        .launch_workflow_with_trust(
            "trusted_external_transfer_demo@v1",
            Some("trusted-external-transfer-run"),
            BTreeMap::new(),
            true,
            false,
        )
        .expect("launch trusted workflow");
    assert_eq!(report.status, "success");

    let provenance = read_provenance(repo.path(), "trusted-external-transfer-run");
    assert_eq!(provenance["external_transfer_summary"]["count"], 1);
    let transfers = provenance["external_transfer_summary"]["external_transfers"]
        .as_array()
        .expect("transfer list");
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0]["step_id"], "emit");
    assert_eq!(transfers[0]["adapter_kind"], "process");
    assert_eq!(transfers[0]["provider"], "openai");
    assert_eq!(transfers[0]["provider_kind"], "external");
    assert!(transfers[0]["content_hash"].as_str().is_some());
    assert!(transfers[0]["timestamp_ms"].as_u64().is_some());

    let evidence_path = transfers[0]["evidence_path"]
        .as_str()
        .expect("evidence path");
    assert!(
        run_root(repo.path(), "trusted-external-transfer-run")
            .join(evidence_path)
            .is_file()
    );
}

#[test]
fn runtime_invoke_evidence_preserves_optional_usage_without_inference() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_fixture_process_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "usage-evidence.flow.yaml",
        r#"
schema: transist.flow.v1
id: usage_evidence_demo
version: v1
plugins:
  fixture.process: 1.0.0
steps:
  - id: collect
    uses: process:fixture.process/emit
    with:
      stdout: usage-ok
      artifact_contents: artifact-payload
      evidence_contents: evidence-payload
      usage:
        provider: openai
        model: gpt-5.4
        input_tokens: 21
        total_tokens: 34
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "usage_evidence_demo@v1",
        "usage-evidence-run",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("usage-evidence-run")
        .expect("run");
    assert_eq!(report.status, "success");

    let inspection = TransistInspectionContext::repo_local(repo.path());
    let runtime_evidence_content = inspection
        .read_blob_content(
            "usage-evidence-run",
            BlobFamily::Evidence,
            "collect",
            1,
            "transist_runtime_invoke_evidence.json",
        )
        .expect("runtime evidence read")
        .expect("runtime evidence bytes");
    let runtime_evidence_json: Value =
        serde_json::from_slice(&runtime_evidence_content.bytes).expect("runtime evidence json");
    assert_eq!(
        runtime_evidence_json["usage"],
        serde_json::json!({
            "provider": "openai",
            "model": "gpt-5.4",
            "input_tokens": 21,
            "total_tokens": 34
        })
    );
    assert!(
        runtime_evidence_json["usage"]
            .get("output_tokens")
            .is_none()
    );
}

#[test]
fn rerunning_parent_recovers_interrupted_child_subflow_before_gate_completion() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());

    let child_dir = repo.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_demo
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let parent_flow = write_flow(
        repo.path(),
        "workflow.yaml",
        r#"
schema: transist.flow.v1
id: parent_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    );

    compile_prepare(
        repo.path(),
        &parent_flow,
        "parent_demo@v1",
        "parent-run",
        BTreeMap::new(),
    );
    let blocked = transist::TransistContext::repo_local(repo.path())
        .run_started_run("parent-run")
        .expect("blocked run");
    assert_eq!(blocked.status, "blocked");

    let child_state_path = run_root(repo.path(), "parent-run__child").join("state.json");
    let mut child_state = runtime_test_support::read_json(&child_state_path);
    child_state["status"] = serde_json::json!("running");
    child_state["steps"]["review"]["status"] = serde_json::json!("pending");
    child_state["steps"]["review"]["started_at_ms"] = serde_json::Value::Null;
    child_state["steps"]["review"]["finished_at_ms"] = serde_json::Value::Null;
    child_state["steps"]["review"]["last_request"] = serde_json::Value::Null;
    child_state["steps"]["review"]["last_result"] = serde_json::Value::Null;
    write_json(&child_state_path, &child_state);

    let rerun = transist::TransistContext::repo_local(repo.path())
        .run_started_run("parent-run")
        .expect("rerun parent");
    assert_eq!(rerun.status, "blocked");

    let recovered_child = read_state(repo.path(), "parent-run__child");
    assert_eq!(recovered_child["status"], "blocked");
    assert_eq!(recovered_child["steps"]["review"]["status"], "blocked");
    assert_eq!(
        transist::TransistContext::repo_local(repo.path())
            .runs_list()
            .expect("runs"),
        vec!["parent-run", "parent-run__child"]
    );

    let approved = transist::TransistContext::repo_local(repo.path())
        .approve_gate_with_input(
            "parent-run",
            "review",
            GateDecisionInput {
                actor: Some("reviewer@example.com".to_string()),
                actor_source: Some("flag".to_string()),
                reason: Some("approved".to_string()),
                decided_at_ms: None,
            },
        )
        .expect("approve");
    assert_eq!(approved.status, "success");

    let parent_state = read_state(repo.path(), "parent-run");
    assert_eq!(parent_state["status"], "success");
    assert!(!read_events(repo.path(), "parent-run__child").is_empty());
}
