// SPDX-License-Identifier: MPL-2.0
use std::fs;
use transist_compiler::{
    capture_workflow_definition, compile_workflow_snapshot, workflow_definition_sha256,
};
use transist_protocol::StepExecutionDescriptor;

fn write(path: &std::path::Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, contents).expect("write file");
}

#[test]
fn package_local_module_lowers_to_an_embedded_child_recipe() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: 0.1.0\nmodules:\n  review:\n    path: modules/review.yaml\nsteps:\n  - id: review\n    module:\n      ref: review\n      inputs:\n        title: release\n",
    );
    write(
        &root.path().join("modules/review.yaml"),
        "schema: transist.module.v1\nid: review\nversion: 0.1.0\ninputs:\n  title:\n    type: string\n    required: true\nsteps:\n  - id: finish\n    control: control.finish\n",
    );

    let snapshot = capture_workflow_definition(&workflow).expect("capture workflow");
    let recipe = compile_workflow_snapshot(&snapshot).expect("compile workflow");

    assert!(matches!(
        &recipe.steps[0].execution,
        StepExecutionDescriptor::Subflow { recipe }
            if recipe.workflow.id == "root-module-review" && recipe.workflow.version == "0.1.0"
    ));
}

#[test]
fn package_local_module_can_declare_a_nested_module() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  parent:\n    path: modules/parent.yaml\nsteps:\n  - id: parent\n    module: parent\n",
    );
    write(
        &root.path().join("modules/parent.yaml"),
        "schema: transist.module.v1\nid: parent\nversion: v1\nmodules:\n  child:\n    path: child.yaml\nsteps:\n  - id: child\n    module: child\n",
    );
    write(
        &root.path().join("modules/child.yaml"),
        "schema: transist.module.v1\nid: child\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n",
    );

    let snapshot = capture_workflow_definition(&workflow).expect("capture workflow");
    let recipe = compile_workflow_snapshot(&snapshot).expect("compile workflow");

    let StepExecutionDescriptor::Subflow { recipe: parent } = &recipe.steps[0].execution else {
        panic!("root step must call the parent module");
    };
    let StepExecutionDescriptor::Subflow { recipe: child } = &parent.steps[0].execution else {
        panic!("parent step must call the child module");
    };
    assert_eq!(child.workflow.id, "root-module-parent-module-child");
}

#[test]
fn package_local_module_cannot_widen_root_authority() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nworkspace:\n  path: '{{inputs.repo_path}}'\n  access: read_only\nnetwork:\n  mode: allowlist\n  allow_hosts: [example.com]\n  allow_ports: [443]\n  tls_required: true\n  max_redirects: 1\ninputs:\n  repo_path:\n    type: string\n    required: true\nmodules:\n  review:\n    path: modules/review.yaml\nsteps:\n  - id: review\n    module: review\n",
    );
    write(
        &root.path().join("modules/review.yaml"),
        "schema: transist.module.v1\nid: review\nversion: v1\nworkspace:\n  path: '{{inputs.repo_path}}'\n  access: full\nnetwork:\n  mode: allowlist\n  allow_hosts: [example.com, attacker.example]\n  allow_ports: [443]\n  tls_required: true\n  max_redirects: 1\ninputs:\n  repo_path:\n    type: string\n    required: true\nsteps:\n  - id: finish\n    control: control.finish\n",
    );

    let snapshot = capture_workflow_definition(&workflow).expect("capture workflow");
    let error = compile_workflow_snapshot(&snapshot).expect_err("module authority widening");
    assert!(
        format!("{error:#}").contains("cannot widen read-only root workspace access"),
        "{error:#}"
    );
}

#[test]
fn package_local_module_inherits_root_declarations() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v2\nplugins:\n  shell: 0.1.0\nworkspace:\n  path: '{{inputs.repo_path}}'\n  access: full\ninputs:\n  repo_path:\n    type: string\n    required: true\n  ticket:\n    type: string\ncontext:\n  ticket: '{{inputs.ticket}}'\nartifacts:\n  result:\n    scope: run\n    path: result.json\nmodules:\n  review:\n    path: modules/review.yaml\nsteps:\n  - id: review\n    module: review\n",
    );
    write(
        &root.path().join("modules/review.yaml"),
        "schema: transist.module.v1\nid: review\nversion: v1\nsteps:\n  - id: echo\n    uses: process:shell/echo\n    with:\n      args: ['{{inputs.ticket}}']\n",
    );

    let snapshot = capture_workflow_definition(&workflow).expect("capture workflow");
    let recipe = compile_workflow_snapshot(&snapshot).expect("compile workflow");
    let StepExecutionDescriptor::Subflow { recipe: child } = &recipe.steps[0].execution else {
        panic!("root step must call module");
    };
    assert_eq!(child.workflow.version, "v2");
    assert_eq!(child.workspace, recipe.workspace);
    assert!(child.inputs.contains_key("repo_path"));
    assert!(child.inputs.contains_key("ticket"));
    assert_eq!(
        recipe.steps[0].with.get("repo_path"),
        Some(&transist_protocol::KernelValue::String(
            "{{inputs.repo_path}}".to_string()
        ))
    );
    assert_eq!(
        recipe.steps[0].with.get("ticket"),
        Some(&transist_protocol::KernelValue::String(
            "{{inputs.ticket}}".to_string()
        ))
    );
}

#[test]
fn module_source_and_assets_are_included_in_the_definition_digest() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    let module = root.path().join("modules/run.yaml");
    let helper = root.path().join("modules/helpers/run.sh");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  run:\n    path: modules/run.yaml\nsteps:\n  - id: run\n    module: run\n",
    );
    write(
        &module,
        "schema: transist.module.v1\nid: run\nversion: v1\nsteps:\n  - id: shell\n    uses: process:shell/exec\n    with:\n      command: source://helpers/run.sh\n",
    );
    write(&helper, "#!/bin/sh\nprintf first\n");

    let first = workflow_definition_sha256(&workflow).expect("first digest");
    let captured = capture_workflow_definition(&workflow).expect("capture workflow");
    assert!(
        captured
            .assets
            .iter()
            .any(|asset| asset.relative_path == "modules/run.yaml")
    );
    assert!(
        captured
            .assets
            .iter()
            .any(|asset| asset.relative_path == "modules/helpers/run.sh")
    );

    write(&helper, "#!/bin/sh\nprintf second\n");
    let second = workflow_definition_sha256(&workflow).expect("second digest");
    assert_ne!(first, second);
}

#[test]
fn unused_declared_module_is_included_in_the_definition_digest() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    let module = root.path().join("modules/unused.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  unused:\n    path: modules/unused.yaml\nsteps:\n  - id: finish\n    control: control.finish\n",
    );
    write(
        &module,
        "schema: transist.module.v1\nid: unused\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n",
    );

    let first = workflow_definition_sha256(&workflow).expect("first digest");
    let captured = capture_workflow_definition(&workflow).expect("capture workflow");
    assert!(
        captured
            .assets
            .iter()
            .any(|asset| asset.relative_path == "modules/unused.yaml")
    );

    write(
        &module,
        "schema: transist.module.v1\nid: unused\nversion: v1\nsteps:\n  - id: changed\n    control: control.finish\n",
    );
    let second = workflow_definition_sha256(&workflow).expect("second digest");
    assert_ne!(first, second);
}

#[test]
fn package_local_module_cannot_be_captured_as_a_root_workflow() {
    let root = tempfile::tempdir().expect("tempdir");
    let module = root.path().join("module.yaml");
    write(
        &module,
        "schema: transist.module.v1\nid: review\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n",
    );

    let error =
        capture_workflow_definition(&module).expect_err("module must not be a root workflow");
    assert!(
        format!("{error:#}").contains("cannot be installed or launched independently"),
        "{error:#}"
    );
}

#[test]
fn package_local_module_requires_version_with_stable_diagnostic() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  review:\n    path: modules/review.yaml\nsteps:\n  - id: review\n    module: review\n",
    );
    write(
        &root.path().join("modules/review.yaml"),
        "schema: transist.module.v1\nid: review\nsteps:\n  - id: finish\n    control: control.finish\n",
    );

    let error = capture_workflow_definition(&workflow).expect_err("module version must fail");
    assert!(
        format!("{error:#}").contains("missing field `version`"),
        "{error:#}"
    );
}

#[test]
fn module_declaration_rejects_an_ordinary_workflow_document() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nmodules:\n  child:\n    path: child.yaml\nsteps:\n  - id: child\n    module: child\n",
    );
    write(
        &root.path().join("child.yaml"),
        "schema: transist.flow.v1\nid: child\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n",
    );

    let error = capture_workflow_definition(&workflow)
        .expect_err("an independently installable workflow must not lower as a module");
    assert!(
        format!("{error:#}").contains("declared modules must use schema transist.module.v1"),
        "{error:#}"
    );
}

#[test]
fn uses_rejects_non_canonical_whitespace() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nsteps:\n  - id: run\n    uses: ' process:shell/echo '\n",
    );

    let error = capture_workflow_definition(&workflow).expect_err("whitespace must fail");
    assert!(
        format!("{error:#}").contains("uses must not contain leading or trailing whitespace"),
        "{error:#}"
    );
}

#[test]
fn authored_flows_reject_retired_invoke_fields_with_stable_errors() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");

    for (field, value) in [
        ("adapter", "process"),
        ("plugin", "shell"),
        ("action", "echo"),
        ("plugin_version", "1.0.0"),
        ("execution_kind", "external"),
    ] {
        write(
            &workflow,
            &format!(
                "schema: transist.flow.v1\nid: root\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/echo\n    {field}: {value}\n"
            ),
        );
        let error = capture_workflow_definition(&workflow).expect_err("retired field must fail");
        assert!(
            format!("{error:#}").contains(&format!(
                "flow.steps[0].{field} is no longer supported; use uses: <adapter>:<plugin>/<action>"
            )),
            "{error:#}"
        );
    }
}

#[test]
fn authored_flows_reject_retired_subflow_syntax_with_a_stable_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    write(
        &workflow,
        "schema: transist.flow.v1\nid: root\nversion: v1\nsteps:\n  - id: child\n    subflow:\n      path: child.yaml\n",
    );

    let error = capture_workflow_definition(&workflow).expect_err("subflow must fail");
    assert!(
        format!("{error:#}").contains(
            "flow.steps[0].subflow is no longer supported; declare a top-level module and use module: <name>"
        ),
        "{error:#}"
    );
}

#[test]
fn authored_cleanup_rejects_retired_invoke_fields_with_stable_errors() {
    let root = tempfile::tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");

    for (field, value) in [
        ("adapter", "process"),
        ("plugin", "shell"),
        ("action", "echo"),
        ("plugin_version", "1.0.0"),
        ("execution_kind", "external"),
    ] {
        write(
            &workflow,
            &format!(
                "schema: transist.flow.v1\nid: root\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\ncleanup:\n  - id: cleanup\n    when: [success]\n    uses: process:shell/echo\n    {field}: {value}\n"
            ),
        );
        let error = capture_workflow_definition(&workflow).expect_err("retired field must fail");
        assert!(
            format!("{error:#}").contains(&format!(
                "flow.cleanup[0].{field} is no longer supported; use uses: <adapter>:<plugin>/<action>"
            )),
            "{error:#}"
        );
    }
}
