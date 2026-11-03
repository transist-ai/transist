// SPDX-License-Identifier: MPL-2.0

use std::fs;
use tempfile::tempdir;
use transist_admin::{generate_key, package_workflow, verify_workflow};

#[test]
fn package_copies_captured_assets_and_signs_the_reversioned_workflow() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    let script = root.path().join("scripts/run.sh");
    fs::create_dir_all(script.parent().expect("script parent")).expect("script parent");
    fs::write(&script, "#!/bin/sh\nprintf packaged\n").expect("script");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: packaged.demo\nversion: v1\nplugins:\n  shell: 0.1.0\nsteps:\n  - id: run\n    uses: process:shell/exec\n    with:\n      command: source://scripts/run.sh\n",
    )
    .expect("workflow");
    let key = root.path().join("admin.key");
    let public = root.path().join("admin.pub");
    generate_key("packaging-test", &key).expect("key");
    transist_admin::export_public_key(&key, &public).expect("public key");

    let output = root.path().join("package");
    let package = package_workflow(&workflow, "v2", &key, &output).expect("package");

    assert_eq!(package.workflow, output.join("workflow.yaml"));
    assert!(output.join("scripts/run.sh").is_file());
    assert!(
        fs::read_to_string(&package.workflow)
            .expect("workflow")
            .contains("version: v2")
    );
    assert!(
        verify_workflow(&package.workflow, &package.signature, &public)
            .expect("verification")
            .verified
    );
    assert!(package_workflow(&workflow, "v3", &key, &output).is_err());
}

#[test]
fn package_rejects_semantically_invalid_workflow_and_noncanonical_version() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: invalid\nversion: v1\nsteps: []\n",
    )
    .expect("workflow");
    let key = root.path().join("admin.key");
    generate_key("packaging-test", &key).expect("key");

    let semantic_error = package_workflow(&workflow, "v2", &key, &root.path().join("semantic"))
        .expect_err("semantic validation must fail");
    assert!(semantic_error.to_string().contains("semantic validation"));
    let version_error = package_workflow(&workflow, " v2", &key, &root.path().join("version"))
        .expect_err("noncanonical version must fail");
    assert!(
        version_error
            .to_string()
            .contains("canonical workflow package version")
    );
}

#[test]
fn package_accepts_prompt_and_skills_without_installed_plugin_metadata() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    fs::create_dir_all(root.path().join("skills/review/references")).expect("skill directory");
    fs::write(
        root.path().join("skills/review/SKILL.md"),
        "---\nname: review\n---\nSee [rules](references/rules.md).\n",
    )
    .expect("skill");
    fs::write(
        root.path().join("skills/review/references/rules.md"),
        "Review carefully.\n",
    )
    .expect("skill reference");
    fs::write(root.path().join("prompt.md"), "Review this change.\n").expect("prompt");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: packaged.ai\nversion: v1\nplugins:\n  not-installed.model: 1.0.0\nsteps:\n  - id: review\n    uses: process:not-installed.model/review\n    prompt_file: prompt.md\n    skills: [skills/review]\n",
    )
    .expect("workflow");
    let key = root.path().join("admin.key");
    generate_key("packaging-assets", &key).expect("key");
    let output = root.path().join("package-assets");

    package_workflow(&workflow, "v2", &key, &output).expect("package");

    assert!(output.join("prompt.md").is_file());
    assert!(output.join("skills/review/SKILL.md").is_file());
    assert!(output.join("skills/review/references/rules.md").is_file());
}

#[test]
fn package_still_rejects_package_independent_prompt_rules() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    fs::write(root.path().join("prompt.md"), "file prompt\n").expect("prompt");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: invalid.prompt\nversion: v1\nplugins:\n  not-installed.model: 1.0.0\nsteps:\n  - id: review\n    uses: process:not-installed.model/review\n    prompt: inline\n    prompt_file: prompt.md\n",
    )
    .expect("workflow");
    let key = root.path().join("admin.key");
    generate_key("packaging-semantics", &key).expect("key");

    let error = package_workflow(&workflow, "v2", &key, &root.path().join("package"))
        .expect_err("multiple prompt sources must fail");
    assert!(format!("{error:#}").contains("multiple prompt sources"));
}

#[test]
fn concurrent_package_publication_never_clobbers() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: raced\nversion: v1\nsteps:\n  - id: noop\n    control: control.noop\n",
    )
    .expect("workflow");
    let key = root.path().join("admin.key");
    generate_key("packaging-race", &key).expect("key");
    let output = root.path().join("package-race");

    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| package_workflow(&workflow, "v2", &key, &output));
        let second = scope.spawn(|| package_workflow(&workflow, "v2", &key, &output));
        [
            first.join().expect("first thread"),
            second.join().expect("second thread"),
        ]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert!(output.join("workflow.yaml").is_file());
    assert!(output.join("workflow.yaml.sig").is_file());
}

#[cfg(unix)]
#[test]
fn package_rejects_symlinked_destination_components() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: symlinked\nversion: v1\nsteps:\n  - id: noop\n    control: control.noop\n",
    )
    .expect("workflow");
    let key = root.path().join("admin.key");
    generate_key("packaging-symlink", &key).expect("key");
    let real = root.path().join("real");
    fs::create_dir(&real).expect("real output parent");
    std::os::unix::fs::symlink(&real, root.path().join("selected")).expect("symlink");

    let error = package_workflow(&workflow, "v2", &key, &root.path().join("selected/package"))
        .expect_err("symlinked output parent must fail");
    assert!(error.to_string().contains("symlink component"));
    assert!(!real.join("package").exists());
}
