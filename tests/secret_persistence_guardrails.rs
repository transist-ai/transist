// SPDX-License-Identifier: MPL-2.0
// SAFETY: Test: uses libc env mutation for test isolation.
#![allow(unsafe_code)]

#[path = "support/mod.rs"]
mod runtime_test_support;

use std::collections::BTreeMap;

use runtime_test_support::{
    assert_no_secret_variants, collect_text, compile_prepare, init_repo,
    install_fixture_secret_process_plugin, read_events, read_execution, read_state, run_root,
    write_flow,
};
use transist::{BlobFamily, BlobListScope, TransistInspectionContext};

#[test]
fn process_lifecycle_redacts_terminal_evidence_and_promoted_hints() {
    let repo = tempfile::tempdir_in("/tmp").expect("tempdir");
    init_repo(repo.path());
    let config = transist::repo_local_home_dir(repo.path()).join("config.yaml");
    let text = std::fs::read_to_string(&config).expect("config");
    std::fs::write(&config, text.replace("process: false", "process: true"))
        .expect("enable secrets");
    runtime_test_support::install_process_lifecycle_plugin(repo.path());
    // SAFETY: unique test-only secret name, never used by other test fixtures.
    unsafe {
        std::env::set_var("LIFECYCLE_SECRET", "topsecret");
    }
    let flow = write_flow(
        repo.path(),
        "lifecycle.flow.yaml",
        r#"
schema: transist.flow.v1
id: process_lifecycle_secret
version: v1
plugins:
  fixture.lifecycle: 1.0.0
steps:
  - id: invoke
    uses: process:fixture.lifecycle/invoke
    with:
      mode: failed_descendant
      secret_refs: [LIFECYCLE_SECRET]
"#,
    );
    compile_prepare(
        repo.path(),
        &flow,
        "process_lifecycle_secret@v1",
        "lifecycle-secret",
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo.path())
        .run_started_run("lifecycle-secret")
        .expect("run");
    assert_eq!(report.status, "failed");
    let root = run_root(repo.path(), "lifecycle-secret");
    assert_no_secret_variants(&root, "topsecret");
    let evidence =
        runtime_test_support::read_json(&root.join(
            "artifacts/invoke/execution_001/evidence/transist_process_lifecycle_evidence.json",
        ));
    assert_eq!(evidence["original_terminal_status"], "failed");
    assert!(
        evidence["original_output"]["stdout"]
            .as_str()
            .expect("output")
            .contains("[REDACTED]")
    );
    assert!(
        evidence["original_terminal_error"]["message"]
            .as_str()
            .expect("error")
            .contains("[REDACTED]")
    );
    let execution = read_execution(repo.path(), "lifecycle-secret", "invoke", 1);
    assert_eq!(execution["result"]["output"], serde_json::json!({}));
    assert_eq!(
        execution["result"]["artifacts"]
            .as_array()
            .expect("artifacts")
            .len(),
        1
    );
    assert_eq!(execution["result"]["error"]["code"], "protocol_error");
}

fn run_secret_echo(repo_root: &std::path::Path, run_id: &str) {
    let config = transist::repo_local_home_dir(repo_root).join("config.yaml");
    let config_text = std::fs::read_to_string(&config).expect("read test config");
    std::fs::write(
        &config,
        config_text.replace("process: false", "process: true"),
    )
    .expect("enable local secret delivery for test");
    install_fixture_secret_process_plugin(repo_root);
    let flow = write_flow(
        repo_root,
        "secret-guardrail.flow.yaml",
        r#"
schema: transist.flow.v1
id: secret_guardrail_demo
version: v1
plugins:
  fixture.secret: 1.0.0
steps:
  - id: emit
    uses: process:fixture.secret/emit_secret_echo
    with:
      secret_refs: ["API_TOKEN"]
"#,
    );

    // SAFETY: test-only environment setup that matches existing repo patterns.
    unsafe {
        std::env::set_var("API_TOKEN", "topsecret");
    }

    compile_prepare(
        repo_root,
        &flow,
        "secret_guardrail_demo@v1",
        run_id,
        BTreeMap::new(),
    );
    let report = transist::TransistContext::repo_local(repo_root)
        .run_started_run(run_id)
        .expect("run");
    assert_eq!(report.status, "success");
}

#[test]
fn secret_values_are_redacted_from_persisted_run_state_and_execution_records() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    run_secret_echo(repo.path(), "secret-state-run");

    let state = read_state(repo.path(), "secret-state-run");
    let execution = read_execution(repo.path(), "secret-state-run", "emit", 1);

    let state_text = state.to_string();
    let execution_text = execution.to_string();
    assert_no_secret_variants(&run_root(repo.path(), "secret-state-run"), "topsecret");
    assert!(!state_text.contains("topsecret"));
    assert!(!execution_text.contains("topsecret"));
    assert_eq!(
        state["steps"]["emit"]["last_result"]["stdout"],
        "[REDACTED]"
    );
    assert_eq!(execution["result"]["stdout"], "[REDACTED]");
}

#[test]
fn secret_values_are_redacted_from_artifacts_evidence_and_inspection_reads() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    run_secret_echo(repo.path(), "secret-blob-run");

    let run_root = run_root(repo.path(), "secret-blob-run");
    let persisted_text = collect_text(&run_root);
    assert_no_secret_variants(&run_root, "topsecret");
    assert!(!persisted_text.contains("topsecret"));
    assert!(persisted_text.contains("[REDACTED]"));

    let inspection = TransistInspectionContext::repo_local(repo.path());
    let artifacts = inspection
        .list_blobs(
            "secret-blob-run",
            BlobFamily::Artifact,
            BlobListScope::SelfOnly,
        )
        .expect("artifact query")
        .expect("artifact list");
    let evidence = inspection
        .list_blobs(
            "secret-blob-run",
            BlobFamily::Evidence,
            BlobListScope::SelfOnly,
        )
        .expect("evidence query")
        .expect("evidence list");

    assert_eq!(artifacts.len(), 1);
    assert_eq!(evidence.len(), 2);

    let artifact_content = inspection
        .read_blob_content(
            "secret-blob-run",
            BlobFamily::Artifact,
            &artifacts[0].step_id,
            artifacts[0].execution,
            &artifacts[0].relative_path,
        )
        .expect("artifact read")
        .expect("artifact content");
    let plugin_evidence = evidence
        .iter()
        .find(|blob| blob.relative_path == "000_secret-trace.log")
        .expect("plugin evidence");
    let runtime_evidence = evidence
        .iter()
        .find(|blob| blob.relative_path == "transist_runtime_invoke_evidence.json")
        .expect("runtime evidence");
    let evidence_content = inspection
        .read_blob_content(
            "secret-blob-run",
            BlobFamily::Evidence,
            &plugin_evidence.step_id,
            plugin_evidence.execution,
            &plugin_evidence.relative_path,
        )
        .expect("evidence read")
        .expect("evidence content");
    let runtime_evidence_content = inspection
        .read_blob_content(
            "secret-blob-run",
            BlobFamily::Evidence,
            &runtime_evidence.step_id,
            runtime_evidence.execution,
            &runtime_evidence.relative_path,
        )
        .expect("runtime evidence read")
        .expect("runtime evidence content");

    assert_eq!(artifact_content.bytes, b"[REDACTED]".to_vec());
    assert_eq!(evidence_content.bytes, b"[REDACTED]".to_vec());
    let runtime_text = String::from_utf8(runtime_evidence_content.bytes).expect("runtime utf8");
    assert!(!runtime_text.contains("topsecret"));
}

#[test]
fn secret_values_are_not_present_in_events_file() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    run_secret_echo(repo.path(), "secret-events-run");

    let events = read_events(repo.path(), "secret-events-run");
    assert_no_secret_variants(&run_root(repo.path(), "secret-events-run"), "topsecret");
    assert!(
        !events.is_empty(),
        "at least one event must be recorded for the run"
    );

    for event in &events {
        let event_text = event.to_string();
        assert!(
            !event_text.contains("topsecret"),
            "secret value must not appear in event: {event_text}"
        );
    }
}
