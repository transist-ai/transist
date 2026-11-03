// SPDX-License-Identifier: MPL-2.0
#[path = "support/gov_demo.rs"]
mod gov_demo_support;

#[cfg(feature = "dev-version-bypass")]
use gov_demo_support::{
    context, init_gov_repo, install_test_plugin_v0, kernel_inputs, read_provenance, read_state,
    write_v0_bypass_workflow,
};

#[cfg(feature = "dev-version-bypass")]
#[test]
fn v0_test_version_bypass_records_bypassed_not_denied() {
    let repo = tempfile::tempdir().expect("tempdir");
    let home = init_gov_repo(repo.path());
    let ctx = context(repo.path(), &home);
    install_test_plugin_v0(&ctx, repo.path());

    let workflow = write_v0_bypass_workflow(repo.path());
    ctx.compile_flow_file_with_trust(&workflow, true)
        .expect("trusted compile should bypass v0");

    let report = ctx
        .launch_workflow_with_trust(
            "gov_demo_bypass@v0",
            Some("gov-bypass-run"),
            kernel_inputs(),
            true,
            false,
        )
        .expect("launch v0 bypass run");
    assert_eq!(report.status, "success");

    let state = read_state(&home, "gov-bypass-run");
    assert_eq!(state["trust_decision"]["decision"], "bypassed");
    assert_eq!(
        state["trust_decision"]["reason_code"],
        "test_version_bypass"
    );
    assert_ne!(state["trust_decision"]["decision"], "denied");

    let provenance = read_provenance(&home, "gov-bypass-run");
    assert_eq!(provenance["workflow_trust_decision"], "bypassed");
    assert_eq!(provenance["decision"], "allowed");
    assert_eq!(provenance["plugins"][0]["trust_decision"], "bypassed");
}
