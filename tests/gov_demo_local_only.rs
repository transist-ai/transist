// SPDX-License-Identifier: MPL-2.0
#[path = "support/gov_demo.rs"]
mod gov_demo_support;

use std::fs;

use gov_demo_support::{
    context, export_evidence, init_gov_repo, install_signed_local_plugin, kernel_inputs,
    read_provenance, read_state, sign_workflow, write_local_only_workflow,
    write_unsigned_external_plugin,
};

#[test]
fn local_only_companion_records_empty_external_transfers() {
    // This is a required Linux local-only sandbox contract: a missing or
    // broken bwrap loopback handshake fails through the actual execution
    // assertion instead of being silently skipped in CI.
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    install_signed_local_plugin(&ctx, repo.path());

    let workflow = write_local_only_workflow(repo.path());
    sign_workflow(&workflow, "gov_demo_local_only", "v1").expect("sign workflow");
    ctx.compile_flow_file_with_trust(&workflow, true)
        .expect("trusted workflow installation");

    let report = ctx
        .launch_workflow_with_trust(
            "gov_demo_local_only@v1",
            Some("gov-local-only-run"),
            kernel_inputs(),
            true,
            true,
        )
        .expect("launch local-only workflow");
    assert_eq!(report.status, "success");

    let state = read_state(&home, "gov-local-only-run");
    assert_eq!(state["local_only_mode"], true);
    let provenance = read_provenance(&home, "gov-local-only-run");
    assert_eq!(provenance["local_only_mode"], true);
    assert_eq!(provenance["external_transfer_summary"]["count"], 0);
    assert_eq!(
        provenance["external_transfer_summary"]["external_transfers"],
        serde_json::json!([])
    );

    let exported = export_evidence(&home, "gov-local-only-run");
    assert_eq!(exported.provenance.external_transfer_summary.count, 0);
    assert!(
        exported
            .provenance
            .external_transfer_summary
            .external_transfers
            .is_empty()
    );
}

#[test]
fn default_mode_still_works_without_trust_flags() {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    let plugin_dir = write_unsigned_external_plugin(repo.path());
    ctx.install_plugin_dir(&plugin_dir)
        .expect("plain install of unsigned plugin");

    let workflow = repo.path().join("plain-unsigned.workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_plain_mode
version: v1
plugins:
  gov.unsigned: 1.0.0
steps:
  - id: collect_external
    uses: process:gov.unsigned/emit
    with:
      stdout: plain-mode-ok
"#,
    )
    .expect("write plain workflow");

    ctx.compile_flow_file(&workflow).expect("plain compile");
    let report = ctx
        .launch_workflow(
            "gov_demo_plain_mode@v1",
            Some("gov-plain-run"),
            kernel_inputs(),
        )
        .expect("plain launch");
    assert_eq!(report.status, "success");

    let state = read_state(&home, "gov-plain-run");
    assert_eq!(state["status"], "success");
    assert!(state["trust_decision"].is_null());
}
