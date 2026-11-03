// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use transist::{
    SignatureVerification, import_trust_root_file, verify_plugin_signature,
    verify_workflow_definition_signature,
};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("admin_tooling")
}

fn copy_fixture_subjects(root: &Path) -> Result<(PathBuf, PathBuf)> {
    let workflow = root.join("demo.workflow.yaml");
    fs::copy(fixture_root().join("demo.workflow.yaml"), &workflow)?;
    fs::copy(
        fixture_root().join("demo.workflow.yaml.sig"),
        transist::signature_sidecar_path(&workflow),
    )?;

    // Copy associated workflow assets so workflow_definition_sha256 can resolve them.
    for relative in ["modules/review.yaml", "scripts/hello.sh", "prompts/task.md"] {
        let src = fixture_root().join(relative);
        let dst = root.join(relative);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }

    let plugin_dir = root.join("demo-plugin");
    fs::create_dir_all(&plugin_dir)?;
    for relative in [
        "plugin.yaml",
        "run.sh",
        "schemas/input.json",
        "schemas/output.json",
    ] {
        let src = fixture_root().join("demo-plugin").join(relative);
        let dst = plugin_dir.join(relative);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    fs::copy(
        fixture_root().join("demo-plugin.sig"),
        transist::signature_sidecar_path(&plugin_dir),
    )?;
    Ok((workflow, plugin_dir))
}

#[test]
#[cfg(not(target_env = "musl"))]
fn imported_fixture_key_detects_the_unresigned_workflow_fixture() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    import_trust_root_file(&home, &fixture_root().join("test-org.pub")).expect("import trust root");
    let (workflow, plugin_dir) = copy_fixture_subjects(temp.path()).expect("copy fixture subjects");

    let workflow_verification =
        verify_workflow_definition_signature(&workflow, &home).expect("verify workflow");
    let plugin_verification = verify_plugin_signature(&plugin_dir, &home).expect("verify plugin");

    assert!(!workflow_verification.verified);
    assert!(!workflow_verification.digest_matched);
    assert_eq!(
        workflow_verification.reason_code.as_deref(),
        Some("digest_mismatch")
    );
    assert!(plugin_verification.verified);
    assert!(plugin_verification.digest_matched);
}

#[test]
fn unsigned_workflow_returns_unsigned_reason() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    let workflow = temp.path().join("unsigned.workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps: []\n",
    )
    .expect("write workflow");

    let verification = verify_workflow_definition_signature(&workflow, &home).expect("verify");
    assert_eq!(
        verification,
        SignatureVerification {
            verified: false,
            key_id: None,
            digest_matched: false,
            reason_code: Some("unsigned".to_string()),
        }
    );
}

#[test]
fn unsigned_plugin_returns_unsigned_reason() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    let plugin_dir = temp.path().join("unsigned-plugin");
    fs::create_dir_all(plugin_dir.join("schemas")).expect("plugin dir");
    fs::write(plugin_dir.join("plugin.yaml"), "schema: transist.plugin.v1\nid: demo.plugin\nversion: v1\nadapter: process\nentrypoint:\n  command: sh\npackage:\n  files: [run.sh, schemas/input.json, schemas/output.json]\nactions: []\n").expect("manifest");
    fs::write(plugin_dir.join("run.sh"), "#!/bin/sh\nexit 0\n").expect("script");
    fs::write(plugin_dir.join("schemas/input.json"), "{}").expect("input schema");
    fs::write(plugin_dir.join("schemas/output.json"), "{}").expect("output schema");

    let verification = verify_plugin_signature(&plugin_dir, &home).expect("verify");
    assert_eq!(verification.reason_code.as_deref(), Some("unsigned"));
    assert!(!verification.verified);
}

#[test]
fn modified_workflow_after_signing_returns_digest_mismatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    import_trust_root_file(&home, &fixture_root().join("test-org.pub")).expect("import trust root");
    let (workflow, _) = copy_fixture_subjects(temp.path()).expect("copy fixture subjects");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: demo.workflow\nversion: v2\nsteps: []\n",
    )
    .expect("rewrite workflow");

    let verification = verify_workflow_definition_signature(&workflow, &home).expect("verify");
    assert_eq!(verification.reason_code.as_deref(), Some("digest_mismatch"));
    assert!(!verification.digest_matched);
}

#[test]
fn modified_plugin_after_signing_returns_digest_mismatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    import_trust_root_file(&home, &fixture_root().join("test-org.pub")).expect("import trust root");
    let (_, plugin_dir) = copy_fixture_subjects(temp.path()).expect("copy fixture subjects");
    fs::write(plugin_dir.join("run.sh"), "#!/bin/sh\necho changed\n").expect("rewrite plugin");

    let verification = verify_plugin_signature(&plugin_dir, &home).expect("verify");
    assert_eq!(verification.reason_code.as_deref(), Some("digest_mismatch"));
    assert!(!verification.digest_matched);
}

#[test]
fn unresigned_workflow_returns_digest_mismatch_before_trust_lookup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    let (workflow, _) = copy_fixture_subjects(temp.path()).expect("copy fixture subjects");

    let verification = verify_workflow_definition_signature(&workflow, &home).expect("verify");
    assert_eq!(verification.reason_code.as_deref(), Some("digest_mismatch"));
    assert!(!verification.digest_matched);
}

#[test]
fn mismatched_signed_digest_returns_digest_mismatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    import_trust_root_file(&home, &fixture_root().join("test-org.pub")).expect("import trust root");
    let (workflow, _) = copy_fixture_subjects(temp.path()).expect("copy fixture subjects");
    let sig_path = transist::signature_sidecar_path(&workflow);
    let mut sig: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&sig_path).expect("read sig")).expect("parse sig");
    sig["subject"]["digest"] = serde_json::Value::String("00".repeat(32));
    fs::write(
        &sig_path,
        serde_json::to_vec_pretty(&sig).expect("serialize sig"),
    )
    .expect("write sig");

    let verification = verify_workflow_definition_signature(&workflow, &home).expect("verify");
    assert_eq!(verification.reason_code.as_deref(), Some("digest_mismatch"));
    assert!(!verification.digest_matched);
}
