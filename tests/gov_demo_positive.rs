// SPDX-License-Identifier: MPL-2.0
#[path = "support/gov_demo.rs"]
mod gov_demo_support;

use gov_demo_support::{
    approve_gate_with_identity, context, export_evidence, init_gov_repo,
    install_signed_external_plugin, install_signed_local_plugin, kernel_inputs,
    read_latest_gate_history, read_provenance, read_state, sign_workflow, write_positive_workflow,
};

fn launch_blocked_positive_run() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    transist::TransistContext,
) {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    install_signed_external_plugin(&ctx, repo.path());
    install_signed_local_plugin(&ctx, repo.path());

    let workflow = write_positive_workflow(repo.path());
    sign_workflow(&workflow, "gov_demo_positive", "v1").expect("sign workflow");
    ctx.compile_flow_file_with_trust(&workflow, true)
        .expect("trusted workflow installation");

    let report = ctx
        .launch_workflow_with_trust(
            "gov_demo_positive@v1",
            Some("gov-positive-run"),
            kernel_inputs(),
            true,
            false,
        )
        .expect("launch positive workflow");
    assert_eq!(report.status, "blocked");

    (repo, home, ctx)
}

#[test]
fn positive_flow_records_verified_decision() {
    let (_repo, home, _ctx) = launch_blocked_positive_run();

    let state = read_state(&home, "gov-positive-run");
    assert_eq!(state["trusted_execution_mode"], true);
    assert_eq!(state["status"], "blocked");
    assert_eq!(state["trust_decision"]["decision"], "allowed");

    let provenance = read_provenance(&home, "gov-positive-run");
    assert_eq!(provenance["workflow_trust_decision"], "verified");
    assert_eq!(provenance["decision"], "allowed");
    assert_eq!(provenance["providers"][0]["provider_kind"], "external");
    assert_eq!(provenance["providers"][0]["provider"], "openai");
    assert_eq!(provenance["providers"][0]["model"], "gpt-5.4");
}

#[test]
fn positive_flow_records_approver_identity() {
    let (_repo, home, ctx) = launch_blocked_positive_run();

    approve_gate_with_identity(&ctx, "gov-positive-run", "plan-review");

    let gate = read_latest_gate_history(&home, "gov-positive-run", "plan-review");
    assert_eq!(gate["actor"], "test-reviewer <test@example.invalid>");
    assert_eq!(gate["gate_id"], "plan-review");
    assert_eq!(gate["decision"], "approved");
    assert_eq!(gate["reason"], "integration test approval");
    assert_eq!(gate["decided_at_ms"], 42);

    let state = read_state(&home, "gov-positive-run");
    assert_eq!(state["status"], "success");
}

#[test]
fn positive_flow_evidence_export_summarizes_run() {
    let (_repo, home, ctx) = launch_blocked_positive_run();

    approve_gate_with_identity(&ctx, "gov-positive-run", "plan-review");

    let exported = export_evidence(&home, "gov-positive-run");
    assert_eq!(exported.run_id, "gov-positive-run");
    assert_eq!(exported.workflow_id, "gov_demo_positive");
    assert_eq!(exported.workflow_version, "v1");
    assert_eq!(exported.provenance.workflow_trust_decision, "verified");
    assert!(exported.markdown.contains("gov-positive-run"));
    assert!(exported.markdown.contains("gov_demo_positive"));
    assert!(exported.markdown.contains("gpt-5.4"));
    assert!(exported.markdown.contains("verified"));
    assert!(exported.markdown.contains("## Approvals"));
    assert!(
        exported
            .markdown
            .contains("test-reviewer <test@example.invalid>")
    );
    assert!(exported.markdown.contains("integration test approval"));
    assert!(exported.markdown.contains("decided_at_ms=42"));
}
