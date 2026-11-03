// SPDX-License-Identifier: MPL-2.0
//! Catalog releases must follow ordinary trust enforcement in every build mode.

use std::fs;
use std::path::{Path, PathBuf};
use transist::TransistContext;
use transist_admin::{key, sign};

/// Copy an unsigned package into disposable storage and select its release.
fn release_plugin(root: &Path, version: &str) -> PathBuf {
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/admin_tooling/demo-plugin");
    let plugin = root.join("plugin");
    for relative in [
        "plugin.yaml",
        "run.sh",
        "schemas/input.json",
        "schemas/output.json",
    ] {
        let target = plugin.join(relative);
        fs::create_dir_all(target.parent().expect("parent")).expect("create directory");
        fs::copy(source.join(relative), target).expect("copy fixture");
    }
    let manifest = plugin.join("plugin.yaml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .expect("read manifest")
            .replace("version: v1", &format!("version: {version}")),
    )
    .expect("select release");
    plugin
}

/// Sign test bytes administratively and import only the public root into the home.
fn sign_test_package(root: &Path, plugin: &Path, home: &Path) {
    let private_key = root.join("admin/test-only.key");
    let public_root = root.join("admin/test-only.pub");
    key::generate_key("catalog-release-test-only", &private_key).expect("test key");
    key::export_public_key(&private_key, &public_root).expect("public root");
    sign::sign_plugin(
        plugin,
        &private_key,
        &transist::signature_sidecar_path(plugin),
    )
    .expect("sign test plugin");
    transist::import_trust_root_file(home, &public_root).expect("import public root");
}

#[test]
fn signed_catalog_release_installs_and_verifies_without_bypass() {
    let temp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
    let home = temp.path().join("home");
    let plugin = release_plugin(temp.path(), "0.1.0");
    transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Governed)
        .expect("governed home");
    sign_test_package(temp.path(), &plugin, &home);
    let context = TransistContext::with_home(temp.path(), &home);
    context
        .install_plugin_dir_with_trust(&plugin, true)
        .expect("signed release installation");
    let installed = home.join("plugins/demo.plugin/0.1.0");
    let verified = transist::verify_plugin_signature(&installed, &home).expect("verify installed");
    assert!(verified.verified && verified.digest_matched, "{verified:?}");
    assert!(!transist::is_test_plugin("0.1.0"));
}

#[test]
fn unsigned_catalog_release_is_rejected_even_with_development_bypass() {
    let temp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
    let home = temp.path().join("home");
    let plugin = release_plugin(temp.path(), "0.1.0");
    transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Governed)
        .expect("governed home");
    let context = TransistContext::with_home(temp.path(), &home);
    let error = context
        .install_plugin_dir_with_trust(&plugin, true)
        .expect_err("unsigned release must not bypass trust");
    assert!(format!("{error:#}").contains("missing_plugin_signature"));
    assert!(context.list_plugins().expect("list plugins").is_empty());
    assert!(!home.join("plugins/demo.plugin/0.1.0").exists());
}

#[test]
fn tampered_catalog_release_is_rejected_even_with_development_bypass() {
    let temp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
    let home = temp.path().join("home");
    let plugin = release_plugin(temp.path(), "0.1.0");
    transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Governed)
        .expect("governed home");
    sign_test_package(temp.path(), &plugin, &home);
    fs::write(plugin.join("run.sh"), "#!/bin/sh\necho tampered\n").expect("tamper package");
    let context = TransistContext::with_home(temp.path(), &home);
    let error = context
        .install_plugin_dir_with_trust(&plugin, true)
        .expect_err("tampered release must not bypass trust");
    assert!(format!("{error:#}").contains("digest_mismatch"));
    assert!(context.list_plugins().expect("list plugins").is_empty());
    assert!(!home.join("plugins/demo.plugin/0.1.0").exists());
}

#[test]
fn explicit_plugin_test_markers_are_feature_gated() {
    for marker in ["0", "v0"] {
        let temp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
        let home = temp.path().join("home");
        let plugin = release_plugin(temp.path(), marker);
        transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Governed)
            .expect("governed home");
        let context = TransistContext::with_home(temp.path(), &home);
        let result = context.install_plugin_dir_with_trust(&plugin, true);
        if cfg!(feature = "dev-version-bypass") {
            result.expect("explicit test marker bypass");
        } else {
            let error = result.expect_err("production rejects explicit test markers");
            assert!(format!("{error:#}").contains("reserved test version"));
        }
        // Public authored pins remain SemVer except for explicit v0 test flows.
        let workflow = temp.path().join("workflow.yaml");
        fs::write(
            &workflow,
            format!("schema: transist.flow.v1\nid: test.marker\nversion: 0.1.0\nplugins:\n  demo.plugin: {marker}\nsteps:\n  - id: call\n    uses: process:demo.plugin/run\n"),
        )
        .expect("release workflow");
        let snapshot = transist_compiler::capture_workflow_definition(&workflow).expect("capture");
        assert!(transist_compiler::compile_workflow_snapshot(&snapshot).is_err());
    }
}

#[test]
fn module_release_metadata_compiles_and_changes_the_root_digest() {
    let temp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
    let workflow = temp.path().join("workflow.yaml");
    let module = temp.path().join("module.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: release.root\nversion: 0.1.0\nmodules:\n  child:\n    path: module.yaml\nsteps:\n  - id: child\n    module: child\n",
    )
    .expect("workflow");
    let module_bytes = "schema: transist.module.v1\nid: child\nversion: 0.1.0\nsteps:\n  - id: finish\n    control: control.finish\n";
    fs::write(&module, module_bytes).expect("module");
    let snapshot = transist_compiler::capture_workflow_definition(&workflow).expect("capture");
    let recipe = transist_compiler::compile_workflow_snapshot(&snapshot).expect("compile");
    let transist_protocol::StepExecutionDescriptor::Subflow { recipe: child } =
        &recipe.steps[0].execution
    else {
        panic!("expected child recipe");
    };
    assert_eq!(child.workflow.version, "0.1.0");
    // This deliberately non-catalog release is a digest sensitivity fixture.
    fs::write(&module, module_bytes.replace("0.1.0", "0.1.1")).expect("change release");
    let changed = transist_compiler::workflow_definition_sha256(&workflow).expect("digest");
    assert_ne!(snapshot.digest, changed);
}
