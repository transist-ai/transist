// SPDX-License-Identifier: MPL-2.0

use std::fs;
use tempfile::tempdir;
use transist_compiler::{capture_workflow_definition, validate_workflow_snapshot_in_home};

fn write(path: &std::path::Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, contents).expect("write asset");
}

#[test]
fn captures_nested_assets_relative_to_the_declaring_module() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  child:\n    path: modules/child.yaml\nsteps:\n  - id: child\n    module: child\n",
    );
    write(
        &root.path().join("modules/child.yaml"),
        "schema: transist.module.v1\nid: child\nversion: v1\nsteps:\n  - id: shell\n    uses: process:shell/exec\n    with:\n      command: source://assets/child.sh\n",
    );
    write(
        &root.path().join("modules/assets/child.sh"),
        "#!/bin/sh\nexit 0\n",
    );

    let capture = capture_workflow_definition(&workflow).expect("capture workflow");
    assert!(
        capture
            .assets
            .iter()
            .any(|asset| asset.relative_path == "modules/assets/child.sh")
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_symlink_in_a_referenced_asset_path() {
    let root = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nsteps:\n  - id: shell\n    uses: process:shell/exec\n    with:\n      command: source://assets/child.sh\n",
    );
    write(&outside.path().join("child.sh"), "#!/bin/sh\nexit 0\n");
    std::os::unix::fs::symlink(outside.path(), root.path().join("assets")).expect("create symlink");

    let error = capture_workflow_definition(&workflow).expect_err("symlink must fail");
    assert!(
        error
            .to_string()
            .contains("workflow 'workflow.yaml' step 'shell' source asset 'assets/child.sh'")
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_symlink_in_the_operator_selected_workflow_path() {
    let root = tempdir().expect("tempdir");
    let real = root.path().join("real");
    write(
        &real.join("workflow.yaml"),
        "schema: transist.flow.v1\nid: root\nversion: v1\nsteps:\n  - id: noop\n    control: control.noop\n",
    );
    std::os::unix::fs::symlink(&real, root.path().join("selected")).expect("create symlink");

    let error = capture_workflow_definition(&root.path().join("selected/workflow.yaml"))
        .expect_err("operator-selected symlink component must fail");
    assert!(error.to_string().contains("symlink component"));
}

#[test]
fn snapshot_compilation_uses_captured_bytes_after_source_changes() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: captured\nversion: v1\nsteps:\n  - id: noop\n    control: control.noop\n",
    );
    let snapshot = capture_workflow_definition(&workflow).expect("capture workflow");
    write(&workflow, "not: a valid workflow\n");

    let recipe = transist_compiler::compile_workflow_snapshot(&snapshot)
        .expect("compile immutable snapshot");
    assert_eq!(recipe.workflow.id, "captured");
    assert_eq!(recipe.workflow.version, "v1");
}

#[test]
fn snapshot_validation_uses_captured_module_bytes_after_source_changes() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    let child = root.path().join("modules/child.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: captured_validation\nversion: v1\nmodules:\n  child:\n    path: modules/child.yaml\nsteps:\n  - id: child\n    module: child\n",
    );
    write(
        &child,
        "schema: transist.module.v1\nid: child\nversion: v1\nsteps:\n  - id: noop\n    control: control.noop\n",
    );
    let snapshot = capture_workflow_definition(&workflow).expect("capture workflow");
    write(&child, "not: a valid workflow\n");

    validate_workflow_snapshot_in_home(&root.path().join("home"), &snapshot)
        .expect("validate immutable snapshot");
}
