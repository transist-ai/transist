// SPDX-License-Identifier: MPL-2.0
#[path = "support/gov_demo.rs"]
mod gov_demo_support;

use std::fs;

use gov_demo_support::{
    context, init_gov_repo, install_signed_external_plugin, install_signed_local_plugin,
    kernel_inputs, sign_workflow, write_positive_workflow, write_unsigned_external_plugin,
};

#[test]
fn t1_unsigned_workflow_denied() {
    let repo = tempfile::tempdir().expect("tempdir");
    let _home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &_home);
    install_signed_external_plugin(&ctx, repo.path());

    let workflow = repo.path().join("unsigned.workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_unsigned
version: v1
plugins:
  gov.external: 1.0.0
steps:
  - id: collect_external
    uses: process:gov.external/emit
    with: {}
"#,
    )
    .expect("write unsigned workflow");
    let _error = ctx
        .compile_flow_file_with_trust(&workflow, true)
        .expect_err("unsigned workflow should deny");
}

#[test]
fn t2_untrusted_plugin_denied() {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    let plugin_dir = write_unsigned_external_plugin(repo.path());
    ctx.install_plugin_dir(&plugin_dir)
        .expect("plain install unsigned plugin");

    let workflow = repo.path().join("signed.workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_untrusted_plugin
version: v1
plugins:
  gov.unsigned: 1.0.0
steps:
  - id: collect_external
    uses: process:gov.unsigned/emit
    with: {}
"#,
    )
    .expect("write signed workflow");
    sign_workflow(&workflow, "gov_demo_untrusted_plugin", "v1").expect("sign workflow");
    ctx.compile_flow_file_with_trust(&workflow, true)
        .expect("trusted compile");

    let error = ctx
        .prepare_workflow_with_trust(
            "gov_demo_untrusted_plugin@v1",
            Some("gov-negative-t2"),
            kernel_inputs(),
            true,
            false,
        )
        .expect_err("unsigned installed plugin should deny");
    assert!(
        error
            .to_string()
            .contains("deterministic pre-dispatch blockers"),
        "unexpected preparation error: {error}"
    );
}

#[test]
fn t3_modified_workflow_denied() {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    install_signed_external_plugin(&ctx, repo.path());
    install_signed_local_plugin(&ctx, repo.path());

    let workflow = write_positive_workflow(repo.path());
    sign_workflow(&workflow, "gov_demo_positive", "v1").expect("sign workflow");
    ctx.compile_flow_file_with_trust(&workflow, true)
        .expect("trusted compile");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_positive
version: v1
plugins:
  gov.external: 1.0.0
  gov.local: 1.0.0
steps:
  - id: collect_external
    uses: process:gov.external/emit
    with: {}
  - id: extra_step
    uses: process:gov.local/emit
    with: {}
"#,
    )
    .expect("tamper workflow");

    let error = ctx
        .prepare_workflow_with_trust(
            "gov_demo_positive@v1",
            Some("gov-negative-t3"),
            kernel_inputs(),
            true,
            false,
        )
        .expect_err("tampered workflow should deny");
    assert!(
        error
            .to_string()
            .contains("deterministic pre-dispatch blockers"),
        "unexpected preparation error: {error}"
    );
}

#[test]
fn t4_modified_plugin_denied() {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    install_signed_external_plugin(&ctx, repo.path());

    let workflow = repo.path().join("signed.workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_tampered_plugin
version: v1
plugins:
  gov.external: 1.0.0
steps:
  - id: collect_external
    uses: process:gov.external/emit
    with: {}
"#,
    )
    .expect("write workflow");
    sign_workflow(&workflow, "gov_demo_tampered_plugin", "v1").expect("sign workflow");
    ctx.compile_flow_file_with_trust(&workflow, true)
        .expect("trusted compile");
    fs::write(
        home.join("plugins/gov.external/1.0.0/plugin.py"),
        "#!/usr/bin/env python3\nprint('tampered')\n",
    )
    .expect("tamper installed plugin");

    let error = ctx
        .prepare_workflow_with_trust(
            "gov_demo_tampered_plugin@v1",
            Some("gov-negative-t4"),
            kernel_inputs(),
            true,
            false,
        )
        .expect_err("tampered plugin should deny");
    assert!(
        error.to_string().contains(
            "plugin_pin_mismatch: package 'gov.external@1.0.0' differs from compiled digest"
        ),
        "unexpected preparation error: {error}"
    );
    assert!(
        ctx.runs_list().expect("list runs").is_empty(),
        "tampered plugin must be rejected before run creation"
    );
}

#[test]
fn t5_external_workflow_denied_in_local_only() {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    install_signed_external_plugin(&ctx, repo.path());
    install_signed_local_plugin(&ctx, repo.path());

    let workflow = write_positive_workflow(repo.path());
    sign_workflow(&workflow, "gov_demo_positive", "v1").expect("sign workflow");
    ctx.compile_flow_file_with_trust(&workflow, true)
        .expect("trusted compile");

    let error = ctx
        .prepare_workflow_with_trust(
            "gov_demo_positive@v1",
            Some("gov-negative-t5"),
            kernel_inputs(),
            true,
            true,
        )
        .expect_err("local-only external workflow should be blocked before dispatch");
    assert!(
        error
            .to_string()
            .contains("deterministic pre-dispatch blockers"),
        "unexpected preparation error: {error}"
    );
}
