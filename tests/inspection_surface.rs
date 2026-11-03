// SPDX-License-Identifier: MPL-2.0
#[path = "support/mod.rs"]
mod runtime_test_support;

use std::collections::BTreeMap;
use std::fs;

use runtime_test_support::{
    compile_prepare, init_repo, install_fixture_process_plugin, install_shell_plugin, write_flow,
};
use serde_json::Value;
use transist::{BlobFamily, BlobListScope, TransistInspectionContext};

#[test]
fn cli_first_inspection_answers_what_failed_where_and_what_outputs_exist() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_fixture_process_plugin(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "inspection-surface.flow.yaml",
        r#"
schema: transist.flow.v1
id: inspection_surface_demo
version: v1
plugins:
  fixture.process: 1.0.0
  shell: 0.1.0
steps:
  - id: collect
    uses: process:fixture.process/emit
    with:
      stdout: collected-ok
      artifact_contents: artifact-payload
      evidence_contents: evidence-payload
  - id: verify_marker
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "test -f .inspection-ok"]
"#,
    );

    compile_prepare(
        repo.path(),
        &flow,
        "inspection_surface_demo@v1",
        "inspection-failed-run",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("inspection-failed-run")
        .expect("run");
    assert_eq!(report.status, "failed");

    let run = Value::from(
        transist::TransistContext::repo_local(repo.path())
            .runs_show("inspection-failed-run")
            .expect("show failed run"),
    );
    assert_eq!(run["status"], "failed");
    assert_eq!(run["steps"]["collect"]["status"], "succeeded");
    assert_eq!(run["steps"]["verify_marker"]["status"], "failed");
    assert_eq!(
        run["steps"]["verify_marker"]["last_result"]["error"]["code"],
        "tool_execution_error"
    );
    assert!(
        run["steps"]["collect"]["last_request"]["paths"]["artifacts_dir"]
            .as_str()
            .expect("artifacts dir")
            .contains("artifacts/collect/execution_001/artifacts")
    );
    assert!(
        run["steps"]["collect"]["last_request"]["paths"]["evidence_dir"]
            .as_str()
            .expect("evidence dir")
            .contains("artifacts/collect/execution_001/evidence")
    );

    let tail = transist::TransistContext::repo_local(repo.path())
        .runs_tail("inspection-failed-run", 20)
        .expect("tail");
    assert!(tail.iter().any(|event| event.kind == "step_failed"));
    assert_eq!(tail.last().expect("last event").kind, "run_stopped");

    let inspection = TransistInspectionContext::repo_local(repo.path());
    let summary = inspection
        .get_run("inspection-failed-run")
        .expect("inspection query")
        .expect("run detail");
    assert_eq!(summary.summary.workflow_id, "inspection_surface_demo");
    assert_eq!(summary.summary.status, "failed");

    let artifacts = inspection
        .list_blobs(
            "inspection-failed-run",
            BlobFamily::Artifact,
            BlobListScope::SelfOnly,
        )
        .expect("artifact query")
        .expect("artifact list");
    let evidence = inspection
        .list_blobs(
            "inspection-failed-run",
            BlobFamily::Evidence,
            BlobListScope::SelfOnly,
        )
        .expect("evidence query")
        .expect("evidence list");
    assert!(artifacts.iter().any(|blob| blob.step_id == "collect"));
    assert!(evidence.iter().any(|blob| blob.step_id == "collect"));
}

#[test]
fn cli_visible_resume_and_replay_state_is_persisted_for_inspection() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    install_shell_plugin(repo.path());

    let flow = write_flow(
        repo.path(),
        "inspection-recovery.flow.yaml",
        r#"
schema: transist.flow.v1
id: inspection_recovery_demo
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
        "inspection_recovery_demo@v1",
        "inspection-resume-source",
        BTreeMap::new(),
    );
    let failed = transist::TransistContext::repo_local(repo.path())
        .run_started_run("inspection-resume-source")
        .expect("run");
    assert_eq!(failed.status, "failed");

    fs::write(repo.path().join(".resume-ok"), "").expect("resume marker");

    let resumed = transist::TransistContext::repo_local(repo.path())
        .runs_resume("inspection-resume-source")
        .expect("resume");
    assert_eq!(resumed.status, "success");

    let source = Value::from(
        transist::TransistContext::repo_local(repo.path())
            .runs_show("inspection-resume-source")
            .expect("show source"),
    );
    assert_eq!(source["resume_count"], 1);
    assert_eq!(source["steps"]["verify_marker"]["executions"], 2);

    let replay = transist::TransistContext::repo_local(repo.path())
        .runs_replay("inspection-resume-source", Some("inspection-replay-run"))
        .expect("replay");
    assert_eq!(replay.status, "success");

    let replayed = Value::from(
        transist::TransistContext::repo_local(repo.path())
            .runs_show("inspection-replay-run")
            .expect("show replay"),
    );
    assert_eq!(replayed["lineage"]["mode"], "replay");
    assert_eq!(
        replayed["lineage"]["source_run_id"],
        "inspection-resume-source"
    );
    assert_eq!(replayed["lineage"]["root_run_id"], "inspection-replay-run");

    let runs = transist::TransistContext::repo_local(repo.path())
        .runs_list()
        .expect("runs list");
    assert!(
        runs.iter()
            .any(|run_id| run_id == "inspection-resume-source")
    );
    assert!(runs.iter().any(|run_id| run_id == "inspection-replay-run"));
}
