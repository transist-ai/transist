// SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::path::{Path, PathBuf};

use transist::{
    import_trust_root_file, remove_trust_root, signature_sidecar_path,
    verify_workflow_definition_signature,
};
use transist_admin::{key, sign};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("admin_tooling")
}

fn copy_workflow_definition(src: &Path, dst: &Path) {
    for path in [
        "demo.workflow.yaml",
        "modules/review.yaml",
        "prompts/task.md",
        "scripts/hello.sh",
    ] {
        let destination = dst.join(path);
        fs::create_dir_all(destination.parent().expect("workflow asset parent"))
            .expect("create workflow asset parent");
        fs::copy(src.join(path), destination).expect("copy workflow definition asset");
    }
}

#[test]
fn removing_trust_root_blocks_future_verification_without_touching_existing_run_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home_dir = temp.path().join("home");
    let workflow_dir = temp.path().join("workflow");
    copy_workflow_definition(&fixture_root(), &workflow_dir);
    let workflow = workflow_dir.join("demo.workflow.yaml");
    let key_path = temp.path().join("test-only-trust/lifecycle.key");
    let trust_root = temp.path().join("test-only-trust/lifecycle.pub");
    key::generate_key("trust-lifecycle-test", &key_path).expect("generate test-only key");
    key::export_public_key(&key_path, &trust_root).expect("export test-only trust root");
    sign::sign_workflow(&workflow, &key_path, &signature_sidecar_path(&workflow))
        .expect("sign copied workflow");

    let imported = import_trust_root_file(&home_dir, &trust_root).expect("import trust root");
    let before =
        verify_workflow_definition_signature(&workflow, &home_dir).expect("verify before remove");
    assert!(before.verified);

    let run_state_path = home_dir.join("runs/run-trust-lifecycle/state.json");
    fs::create_dir_all(run_state_path.parent().expect("run state parent")).expect("create run dir");
    let sentinel =
        r#"{"status":"prepared","trust_decision":{"decision":"allowed","reason_code":null}}"#;
    fs::write(&run_state_path, sentinel).expect("write state");

    let removed = remove_trust_root(&home_dir, &imported.key_id).expect("remove trust root");
    assert_eq!(removed, imported);

    let after =
        verify_workflow_definition_signature(&workflow, &home_dir).expect("verify after remove");
    assert!(!after.verified);
    assert_eq!(after.reason_code.as_deref(), Some("untrusted_key"));
    assert_eq!(
        fs::read_to_string(&run_state_path).expect("read state after remove"),
        sentinel
    );
}
