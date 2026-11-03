// SPDX-License-Identifier: MPL-2.0
use std::fs;
use std::path::{Path, PathBuf};

use transist::{
    import_trust_root_file, signature_sidecar_path, verify_plugin_signature,
    verify_workflow_definition_signature,
};
use transist_admin::{key, sign};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/admin_tooling")
}

fn copy_tree(src: &Path, dst: &Path) {
    let metadata = fs::symlink_metadata(src).expect("source metadata");
    if metadata.file_type().is_symlink() {
        let resolved = fs::canonicalize(src).expect("resolve symlink");
        copy_tree(&resolved, dst);
        return;
    }
    if metadata.is_dir() {
        fs::create_dir_all(dst).expect("create dir");
        for entry in fs::read_dir(src).expect("read dir") {
            let entry = entry.expect("dir entry");
            copy_tree(&entry.path(), &dst.join(entry.file_name()));
        }
        return;
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).expect("create file parent");
    }
    fs::copy(src, dst).expect("copy file");
}

#[test]
fn admin_signed_workflow_verifies_at_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workflow_dir = temp.path().join("workflow");
    copy_tree(&fixture_root(), &workflow_dir);
    let workflow_path = workflow_dir.join("demo.workflow.yaml");
    let signature_path = signature_sidecar_path(&workflow_path);
    let key_path = temp.path().join("trust/test-org.key");
    let pub_path = temp.path().join("trust/test-org.pub");
    let home_dir = temp.path().join("home");
    fs::create_dir_all(&home_dir).expect("create home");

    key::generate_key("test-org", &key_path).expect("generate key");
    key::export_public_key(&key_path, &pub_path).expect("export public key");
    sign::sign_workflow(&workflow_path, &key_path, &signature_path).expect("sign workflow");

    import_trust_root_file(&home_dir, &pub_path).expect("import trust root");

    let verification =
        verify_workflow_definition_signature(&workflow_path, &home_dir).expect("verify workflow");
    assert!(verification.verified);
    assert!(verification.digest_matched);
}

#[test]
fn admin_signed_plugin_verifies_at_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("demo-plugin");
    copy_tree(&fixture_root().join("demo-plugin"), &plugin_dir);
    let signature_path = signature_sidecar_path(&plugin_dir);
    let key_path = temp.path().join("trust/test-org.key");
    let pub_path = temp.path().join("trust/test-org.pub");
    let home_dir = temp.path().join("home");
    fs::create_dir_all(&home_dir).expect("create home");

    key::generate_key("test-org", &key_path).expect("generate key");
    key::export_public_key(&key_path, &pub_path).expect("export public key");
    sign::sign_plugin(&plugin_dir, &key_path, &signature_path).expect("sign plugin");

    import_trust_root_file(&home_dir, &pub_path).expect("import trust root");

    let verification = verify_plugin_signature(&plugin_dir, &home_dir).expect("verify plugin");
    assert!(verification.verified);
    assert!(verification.digest_matched);
}
