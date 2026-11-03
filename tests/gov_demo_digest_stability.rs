// SPDX-License-Identifier: MPL-2.0
use std::fs;

use transist::workflow_definition_sha256;

fn write_digest_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let modules = root.join("modules");
    let prompts = root.join("prompts");
    let scripts = root.join("scripts");
    fs::create_dir_all(&modules).expect("modules");
    fs::create_dir_all(&prompts).expect("prompts");
    fs::create_dir_all(&scripts).expect("scripts");

    fs::write(
        modules.join("review.yaml"),
        r#"schema: transist.module.v1
id: digest_child
version: v1
steps:
  - id: review_gate
    control: control.gate.manual
    with:
      gate_id: digest-review
"#,
    )
    .expect("child flow");
    fs::write(prompts.join("task.md"), "Review the generated plan.\n").expect("prompt");
    fs::write(scripts.join("hello.sh"), "#!/bin/sh\nprintf 'hello\\n'\n").expect("script");

    let workflow = root.join("workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: digest_demo
version: v1
plugins:
  shell: 0.1.0
modules:
  review:
    path: modules/review.yaml
steps:
  - id: review
    module: review
  - id: run_script
    uses: process:shell/exec
    prompt_file: prompts/task.md
    with:
      command: source://scripts/hello.sh
"#,
    )
    .expect("workflow");
    workflow
}

#[test]
fn workflow_definition_digest_stable_across_module_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workflow = write_digest_fixture(temp.path());

    let first = workflow_definition_sha256(&workflow).expect("first digest");
    let second = workflow_definition_sha256(&workflow).expect("second digest");

    assert_eq!(first, second);
}

#[test]
fn workflow_definition_digest_changes_on_stray_comment() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workflow = write_digest_fixture(temp.path());

    let first = workflow_definition_sha256(&workflow).expect("first digest");
    fs::write(
        temp.path().join("modules/review.yaml"),
        r#"schema: transist.module.v1
id: digest_child
version: v1
# tampered comment
steps:
  - id: review_gate
    control: control.gate.manual
    with:
      gate_id: digest-review
"#,
    )
    .expect("tamper child flow");

    let second = workflow_definition_sha256(&workflow).expect("second digest");
    assert_ne!(first, second);
}
