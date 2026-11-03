// SPDX-License-Identifier: MPL-2.0
use transist::TransistContext;

#[test]
fn module_definition_cannot_be_compiled_or_installed_as_a_workflow() {
    let root = tempfile::tempdir().expect("tempdir");
    let module = root.path().join("review.yaml");
    std::fs::write(
        &module,
        "schema: transist.module.v1\nid: review\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n",
    )
    .expect("module");
    let context = TransistContext::with_home(root.path(), root.path().join("home"));
    context.init().expect("initialize home");

    for result in [
        context.compile_flow_file(&module).map(|_| ()),
        context
            .install_flow_file_with_trust(&module, false)
            .map(|_| ()),
    ] {
        let error = result.expect_err("module must be rejected");
        assert!(
            format!("{error:#}").contains("cannot be installed or launched independently"),
            "{error:#}"
        );
    }
}
