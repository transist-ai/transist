// SPDX-License-Identifier: MPL-2.0
#[path = "support/mod.rs"]
mod runtime_test_support;

use std::collections::BTreeMap;
use std::fs;

use runtime_test_support::{
    compile_prepare, init_repo, install_fixture_process_plugin, install_shell_plugin,
    read_execution, read_lines, read_state, run_root, write_flow, write_state,
};

#[test]
fn process_lifecycle_persists_evidence_without_success_output_or_automatic_retry() {
    let repo = tempfile::tempdir_in("/tmp").expect("tempdir");
    init_repo(repo.path());
    runtime_test_support::install_process_lifecycle_plugin(repo.path());
    let flow = write_flow(
        repo.path(),
        "lifecycle.flow.yaml",
        r#"
schema: transist.flow.v1
id: process_lifecycle
version: v1
plugins:
  fixture.lifecycle: 1.0.0
steps:
  - id: invoke
    uses: process:fixture.lifecycle/invoke
    with:
      mode: descendant
      retry_max: 3
      retry_wait_ms: 0
"#,
    );
    compile_prepare(
        repo.path(),
        &flow,
        "process_lifecycle@v1",
        "lifecycle-run",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("lifecycle-run")
        .expect("run");
    assert_eq!(report.status, "failed");
    let state = read_state(repo.path(), "lifecycle-run");
    assert_eq!(state["steps"]["invoke"]["executions"], 1);
    assert_eq!(
        state["steps"]["invoke"]["last_result"]["output"],
        serde_json::json!({})
    );
    let execution = read_execution(repo.path(), "lifecycle-run", "invoke", 1);
    assert_eq!(execution["result"]["error"]["code"], "protocol_error");
    assert_eq!(execution["result"]["error"]["retryability"], "none");
    assert_eq!(execution["result"]["exit_code"], 2);
    assert_eq!(execution["result"]["output"], serde_json::json!({}));
    let root = run_root(repo.path(), "lifecycle-run");
    let retained =
        runtime_test_support::read_json(&root.join(
            "artifacts/invoke/execution_001/evidence/transist_process_lifecycle_evidence.json",
        ));
    assert_eq!(retained["original_output"]["stdout"], "terminal-output");
    assert_eq!(retained["cleanup"], "confirmed");
    let failures: Vec<serde_json::Value> = read_lines(&root.join("failures.jsonl"))
        .iter()
        .map(|line| serde_json::from_str(line).expect("failure"))
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["failure_class"], "protocol_error");
    assert_eq!(failures[0]["retryable"], false);
    assert_eq!(failures[0]["safe_to_retry"], false);
    for file in execution["result"]["evidence"].as_array().expect("refs") {
        assert_eq!(file["sha256"].as_str().expect("checksum").len(), 64);
    }
}

#[test]
fn process_lifecycle_invalid_payload_never_promotes_output_or_plugin_hints() {
    for mode in ["invalid_identity", "invalid_output", "failed_empty"] {
        let repo = tempfile::tempdir_in("/tmp").expect("tempdir");
        init_repo(repo.path());
        runtime_test_support::install_process_lifecycle_plugin(repo.path());
        let flow = write_flow(
            repo.path(),
            "invalid-lifecycle.flow.yaml",
            &format!(
                r#"
schema: transist.flow.v1
id: invalid_lifecycle
version: v1
plugins:
  fixture.lifecycle: 1.0.0
steps:
  - id: invoke
    uses: process:fixture.lifecycle/invoke
    with:
      mode: {mode}
"#
            ),
        );
        compile_prepare(
            repo.path(),
            &flow,
            "invalid_lifecycle@v1",
            "invalid-lifecycle",
            BTreeMap::new(),
        );
        let report = transist::TransistContext::repo_local(repo.path())
            .run_started_run("invalid-lifecycle")
            .expect("run");
        assert_eq!(report.status, "failed");
        let execution = read_execution(repo.path(), "invalid-lifecycle", "invoke", 1);
        assert_eq!(
            execution["result"]["error"]["code"],
            if mode == "invalid_output" {
                "validation_error"
            } else {
                "protocol_error"
            }
        );
        assert_eq!(execution["result"]["output"], serde_json::json!({}));
        assert!(
            execution["result"]["artifacts"]
                .as_array()
                .expect("artifacts")
                .is_empty()
        );
        let root = run_root(repo.path(), "invalid-lifecycle");
        let evidence_dir = root.join("artifacts/invoke/execution_001/evidence");
        assert!(!evidence_dir.join("000_terminal-trace.txt").exists());
        let retained = runtime_test_support::read_json(
            &evidence_dir.join("transist_process_lifecycle_evidence.json"),
        );
        assert!(retained.get("original_output").is_none());
        if mode == "invalid_identity" {
            assert!(retained.get("original_terminal_status").is_none());
            assert!(retained.get("original_terminal_error").is_none());
        } else if mode == "failed_empty" {
            assert_eq!(
                retained["original_terminal_error"]["code"],
                "tool_execution_error"
            );
        }
    }
}

#[test]
fn process_lifecycle_protocol_failed_cleanup_does_not_automatically_retry() {
    let repo = tempfile::tempdir_in("/tmp").expect("tempdir");
    init_repo(repo.path());
    runtime_test_support::install_process_lifecycle_plugin(repo.path());
    let flow = write_flow(
        repo.path(),
        "cleanup-lifecycle.flow.yaml",
        r#"
schema: transist.flow.v1
id: cleanup_lifecycle
version: v1
plugins:
  fixture.lifecycle: 1.0.0
steps:
  - id: finish
    control: control.finish
cleanup:
  - id: teardown
    when: [success]
    uses: process:fixture.lifecycle/invoke
    with:
      mode: leader
      retry_max: 3
      retry_wait_ms: 0
"#,
    );
    compile_prepare(
        repo.path(),
        &flow,
        "cleanup_lifecycle@v1",
        "cleanup-lifecycle",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("cleanup-lifecycle")
        .expect("run");
    // Cleanup failures do not rewrite the already-terminal primary run outcome.
    assert_eq!(report.status, "success");
    let state = read_state(repo.path(), "cleanup-lifecycle");
    assert_eq!(state["cleanup"]["state"], "failed");
    let teardown = &state["cleanup"]["steps"]["teardown"];
    assert_eq!(teardown["status"], "failed");
    assert_eq!(teardown["executions"], 1);
    assert_eq!(teardown["last_result"]["error"]["code"], "protocol_error");
    assert!(teardown["retry_wait_until_ms"].is_null());
    let execution = read_execution(repo.path(), "cleanup-lifecycle", "teardown", 1);
    assert_eq!(execution["result"]["error"]["retryability"], "none");
    assert_eq!(execution["result"]["output"], serde_json::json!({}));
    assert!(execution["retry_wait_until_ms"].is_null());
    let root = run_root(repo.path(), "cleanup-lifecycle");
    assert!(
        !root
            .join("step_executions/teardown/execution_002.json")
            .exists()
    );
    let evidence = runtime_test_support::read_json(&root.join(
        "artifacts/teardown/execution_001/evidence/transist_process_lifecycle_evidence.json",
    ));
    assert_eq!(evidence["cleanup"], "confirmed");
    let failures: Vec<serde_json::Value> = read_lines(&root.join("failures.jsonl"))
        .iter()
        .map(|line| serde_json::from_str(line).expect("failure"))
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["failure_class"], "protocol_error");
    assert_eq!(failures[0]["retryable"], false);
}

#[test]
fn malformed_plugin_result_becomes_a_persisted_protocol_failure() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_fixture_process_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "malformed.flow.yaml",
        r#"
schema: transist.flow.v1
id: malformed_demo
version: v1
plugins:
  fixture.process: 1.0.0
steps:
  - id: invoke
    uses: process:fixture.process/malformed_type
    with:
      stdout: ignored
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "malformed_demo@v1",
        "malformed-run",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("malformed-run")
        .expect("run");
    assert_eq!(report.status, "failed");

    let state = read_state(repo.path(), "malformed-run");
    assert_eq!(state["status"], "failed");
    assert_eq!(state["steps"]["invoke"]["status"], "failed");
    assert_eq!(
        state["steps"]["invoke"]["last_result"]["error"]["code"],
        "protocol_error"
    );
    assert!(
        state["steps"]["invoke"]["last_result"]["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("invalid APP result type")
    );

    let execution = read_execution(repo.path(), "malformed-run", "invoke", 1);
    assert_eq!(execution["status"], "failed");
    assert_eq!(execution["result"]["error"]["code"], "protocol_error");
}

#[test]
fn resume_rejects_non_failed_runs_without_mutating_execution_state() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "resume-invalid.flow.yaml",
        r#"
schema: transist.flow.v1
id: resume_invalid_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: echo
    uses: process:shell/echo
    with:
      args: ["still-successful"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "resume_invalid_demo@v1",
        "successful-run",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("successful-run")
        .expect("run");
    assert_eq!(report.status, "success");

    let before = read_state(repo.path(), "successful-run");
    let error = transist::TransistContext::repo_local(repo.path())
        .runs_resume("successful-run")
        .expect_err("resume must fail");
    assert!(
        error
            .to_string()
            .contains("cannot be resumed because status is 'success'")
    );

    let after = read_state(repo.path(), "successful-run");
    assert_eq!(before, after);
    assert_eq!(after["resume_count"], 0);
    assert_eq!(after["steps"]["echo"]["executions"], 1);
}

#[test]
fn replay_rejects_existing_target_run_id_without_creating_new_state() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "replay-invalid.flow.yaml",
        r#"
schema: transist.flow.v1
id: replay_invalid_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: echo
    uses: process:shell/echo
    with:
      args: ["source"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "replay_invalid_demo@v1",
        "source-run",
        BTreeMap::new(),
    );
    transist::TransistContext::repo_local(repo.path())
        .run_started_run("source-run")
        .expect("source run");

    let error = transist::TransistContext::repo_local(repo.path())
        .runs_replay("source-run", Some("source-run"))
        .expect_err("replay");
    assert!(
        error
            .to_string()
            .contains("already exists; replay requires a new run id")
    );

    let runs = transist::TransistContext::repo_local(repo.path())
        .runs_list()
        .expect("runs");
    assert_eq!(runs, vec!["source-run"]);
    let state = read_state(repo.path(), "source-run");
    assert_eq!(state["lineage"]["mode"], "fresh");
}

#[test]
fn rerunning_partial_parent_state_reuses_successful_child_subflow() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let child_dir = repo.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_recovery
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: nested
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "printf '%s\n' child >> child-side-effect.log"]
"#,
    )
    .expect("child flow");

    let parent_flow = write_flow(
        repo.path(),
        "workflow.yaml",
        r#"
schema: transist.flow.v1
id: parent_recovery
version: v1
plugins:
  shell: 0.1.0
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
        "parent_recovery@v1",
        "parent-run",
        BTreeMap::new(),
    );
    let first = transist::TransistContext::repo_local(repo.path())
        .run_started_run("parent-run")
        .expect("first run");
    assert_eq!(first.status, "success");
    assert_eq!(
        read_lines(&repo.path().join("child-side-effect.log")),
        vec!["child"]
    );

    let mut parent_state = read_state(repo.path(), "parent-run");
    parent_state["status"] = serde_json::json!("running");
    parent_state["finished_at_ms"] = serde_json::Value::Null;
    parent_state["steps"]["child"]["status"] = serde_json::json!("runnable");
    parent_state["steps"]["child"]["started_at_ms"] = serde_json::json!(1);
    parent_state["steps"]["child"]["finished_at_ms"] = serde_json::Value::Null;
    parent_state["steps"]["child"]["last_request"] = serde_json::Value::Null;
    parent_state["steps"]["child"]["last_result"] = serde_json::Value::Null;
    parent_state["steps"]["finish"]["status"] = serde_json::json!("pending");
    write_state(repo.path(), "parent-run", &parent_state);

    let recovered = transist::TransistContext::repo_local(repo.path())
        .run_started_run("parent-run")
        .expect("recover parent");
    assert_eq!(recovered.status, "success");

    let child_lines = read_lines(&repo.path().join("child-side-effect.log"));
    assert_eq!(child_lines, vec!["child"]);
    let child_state = read_state(repo.path(), "parent-run__child");
    assert_eq!(child_state["steps"]["nested"]["executions"], 1);
}

#[test]
fn event_stream_persistence_failure_surfaces_without_dispatching_steps() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "storage-failure.flow.yaml",
        r#"
schema: transist.flow.v1
id: storage_failure_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: echo
    uses: process:shell/echo
    with:
      args: ["storage-failure"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "storage_failure_demo@v1",
        "storage-fail",
        BTreeMap::new(),
    );
    let events_path = run_root(repo.path(), "storage-fail").join("events.jsonl");
    if events_path.exists() {
        fs::remove_file(&events_path).expect("remove events file");
    }
    fs::create_dir_all(&events_path).expect("block events path");

    let error = transist::TransistContext::repo_local(repo.path())
        .run_started_run("storage-fail")
        .expect_err("run");
    assert!(
        error.to_string().contains("failed appending event")
            || error.to_string().contains("failed opening append stream")
            || error.to_string().contains("failed repairing event stream"),
        "actual error: {error}"
    );
    assert!(error.to_string().contains("events.jsonl"));

    let state = read_state(repo.path(), "storage-fail");
    assert_eq!(state["status"], "running");
    assert_eq!(state["steps"]["echo"]["executions"], 0);
    assert_eq!(state["steps"]["echo"]["status"], "runnable");
    assert_eq!(state["preflight"]["status"], "passed");
    assert!(
        !run_root(repo.path(), "storage-fail")
            .join("step_executions")
            .exists()
    );
}
