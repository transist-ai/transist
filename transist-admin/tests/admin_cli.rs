// SPDX-License-Identifier: MPL-2.0

use serde_json::Value;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/admin_tooling")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn admin_bin() -> &'static str {
    env!("CARGO_BIN_EXE_transist-admin")
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

fn select_package_file(plugin_dir: &Path, relative: &str) {
    let manifest_path = plugin_dir.join("plugin.yaml");
    let manifest = fs::read_to_string(&manifest_path).expect("read plugin manifest");
    let marker = "    - schemas/output.json\n";
    let replacement = format!("{marker}    - {relative}\n");
    assert!(manifest.contains(marker), "package file marker not found");
    fs::write(&manifest_path, manifest.replace(marker, &replacement))
        .expect("write plugin manifest");
}

fn run_admin(args: &[&str]) -> Output {
    Command::new(admin_bin())
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run transist-admin")
}

fn parse_stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("parse stdout json")
}

#[cfg(unix)]
fn create_fifo(path: &Path) {
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed: {status}");
}

fn generate_trust_material(temp: &Path, name: &str) -> (PathBuf, PathBuf) {
    let key_path = temp.join(format!("{name}.key"));
    let pub_path = temp.join(format!("{name}.pub"));

    let generate = run_admin(&[
        "key",
        "generate",
        "--name",
        name,
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
    ]);
    assert!(
        generate.status.success(),
        "key generate failed: {}",
        String::from_utf8_lossy(&generate.stderr)
    );

    let export = run_admin(&[
        "key",
        "export-public",
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        export.status.success(),
        "key export-public failed: {}",
        String::from_utf8_lossy(&export.stderr)
    );

    (key_path, pub_path)
}

#[test]
fn key_generate_and_export_public_write_expected_schemas() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (key_path, pub_path) = generate_trust_material(temp.path(), "acme");

    let key: Value = serde_json::from_str(&fs::read_to_string(&key_path).expect("read key"))
        .expect("parse key json");
    let public: Value =
        serde_json::from_str(&fs::read_to_string(&pub_path).expect("read public key"))
            .expect("parse public key json");

    assert_eq!(key["schema"], "transist.admin.key.v1");
    assert_eq!(public["schema"], "transist.trust_root.v1");
    assert_eq!(key["name"], "acme");
    assert_eq!(public["name"], "acme");
    assert_eq!(key["key_id"], public["key_id"]);

    // Private key file must have 0600 permissions on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&key_path)
            .expect("stat key")
            .permissions()
            .mode();
        assert_eq!(
            perms & 0o777,
            0o600,
            "private key file must have 0600 permissions, got {:o}",
            perms & 0o777
        );
    }
}

#[test]
fn workflow_sign_verify_and_inspect_succeed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = temp.path().join("fixture");
    copy_tree(&fixture_root(), &fixture);
    let workflow_path = fixture.join("demo.workflow.yaml");
    let signature_path = temp.path().join("workflow.sig");
    let (key_path, pub_path) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "workflow",
        "sign",
        workflow_path.to_str().expect("workflow path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(
        sign.status.success(),
        "workflow sign failed: {}",
        String::from_utf8_lossy(&sign.stderr)
    );

    let verify = run_admin(&[
        "workflow",
        "verify",
        workflow_path.to_str().expect("workflow path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        verify.status.success(),
        "workflow verify failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let verify_json = parse_stdout_json(&verify);
    assert_eq!(verify_json["verified"], true);
    assert_eq!(verify_json["reason_code"], "verified");
    assert_eq!(verify_json["subject_kind"], "workflow");
    assert_eq!(verify_json["subject_id"], "demo.workflow");

    let inspect = run_admin(&[
        "signature",
        "inspect",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(
        inspect.status.success(),
        "signature inspect failed: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let inspect_json = parse_stdout_json(&inspect);
    assert_eq!(inspect_json["subject_kind"], "workflow");
    assert_eq!(inspect_json["sig_alg"], "ed25519");
    assert_eq!(inspect_json["digest_alg"], "sha256");
    assert!(
        inspect_json["subject_key"]
            .as_str()
            .expect("subject key")
            .starts_with("workflow:demo.workflow@v1@")
    );
}

#[test]
fn plugin_sign_and_verify_succeed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("demo-plugin");
    copy_tree(&fixture_root().join("demo-plugin"), &plugin_dir);
    let signature_path = temp.path().join("plugin.sig");
    let (key_path, pub_path) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "plugin",
        "sign",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(
        sign.status.success(),
        "plugin sign failed: {}",
        String::from_utf8_lossy(&sign.stderr)
    );

    let verify = run_admin(&[
        "plugin",
        "verify",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        verify.status.success(),
        "plugin verify failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let verify_json = parse_stdout_json(&verify);
    assert_eq!(verify_json["verified"], true);
    assert_eq!(verify_json["reason_code"], "verified");
    assert_eq!(verify_json["subject_kind"], "plugin");
    assert_eq!(verify_json["subject_id"], "demo.plugin");
}

#[test]
fn sign_commands_default_to_runtime_sidecar_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = temp.path().join("fixture");
    copy_tree(&fixture_root(), &fixture);
    let workflow_path = fixture.join("demo.workflow.yaml");
    let plugin_dir = fixture.join("demo-plugin");
    let (key_path, _) = generate_trust_material(temp.path(), "acme");

    let workflow = run_admin(&[
        "workflow",
        "sign",
        workflow_path.to_str().expect("workflow path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
    ]);
    assert!(
        workflow.status.success(),
        "workflow default sign failed: {}",
        String::from_utf8_lossy(&workflow.stderr)
    );
    assert!(fixture.join("demo.workflow.yaml.sig").is_file());

    let plugin = run_admin(&[
        "plugin",
        "sign",
        plugin_dir
            .join("plugin.yaml")
            .to_str()
            .expect("plugin path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
    ]);
    assert!(
        plugin.status.success(),
        "plugin default sign failed: {}",
        String::from_utf8_lossy(&plugin.stderr)
    );
    assert!(fixture.join("demo-plugin.sig").is_file());
}

#[test]
fn workflow_verify_rejects_tampering() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = temp.path().join("fixture");
    copy_tree(&fixture_root(), &fixture);
    let workflow_path = fixture.join("demo.workflow.yaml");
    let signature_path = temp.path().join("workflow.sig");
    let (key_path, pub_path) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "workflow",
        "sign",
        workflow_path.to_str().expect("workflow path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(sign.status.success(), "workflow sign failed");

    fs::write(
        &workflow_path,
        "schema: transist.flow.v1\nid: demo.workflow\nversion: v1\nsteps: []\n",
    )
    .expect("tamper workflow");

    let verify = run_admin(&[
        "workflow",
        "verify",
        workflow_path.to_str().expect("workflow path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        !verify.status.success(),
        "workflow verify unexpectedly succeeded"
    );
    let verify_json = parse_stdout_json(&verify);
    assert_eq!(verify_json["reason_code"], "digest_mismatch");
}

#[test]
fn plugin_verify_rejects_tampering() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("demo-plugin");
    copy_tree(&fixture_root().join("demo-plugin"), &plugin_dir);
    let signature_path = temp.path().join("plugin.sig");
    let (key_path, pub_path) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "plugin",
        "sign",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(sign.status.success(), "plugin sign failed");

    fs::write(plugin_dir.join("run.sh"), "#!/bin/sh\necho changed\n").expect("tamper plugin");

    let verify = run_admin(&[
        "plugin",
        "verify",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        !verify.status.success(),
        "plugin verify unexpectedly succeeded"
    );
    let verify_json = parse_stdout_json(&verify);
    assert_eq!(verify_json["reason_code"], "digest_mismatch");
}

#[cfg(unix)]
#[test]
fn plugin_sign_rejects_symlink_fixture() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("demo-plugin");
    copy_tree(&fixture_root().join("demo-plugin"), &plugin_dir);
    symlink(plugin_dir.join("run.sh"), plugin_dir.join("linked.sh")).expect("symlink");
    select_package_file(&plugin_dir, "linked.sh");
    let (key_path, _) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "plugin",
        "sign",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        temp.path()
            .join("plugin.sig")
            .to_str()
            .expect("signature path utf8"),
    ]);
    assert!(!sign.status.success(), "plugin sign unexpectedly succeeded");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&sign.stdout),
        String::from_utf8_lossy(&sign.stderr)
    );
    assert!(output.contains("symlink"), "unexpected output: {output}");
}

#[cfg(unix)]
#[test]
fn plugin_sign_rejects_special_file_fixture() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("demo-plugin");
    copy_tree(&fixture_root().join("demo-plugin"), &plugin_dir);
    create_fifo(&plugin_dir.join("plugin.fifo"));
    select_package_file(&plugin_dir, "plugin.fifo");
    let (key_path, _) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "plugin",
        "sign",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        temp.path()
            .join("plugin.sig")
            .to_str()
            .expect("signature path utf8"),
    ]);
    assert!(!sign.status.success(), "plugin sign unexpectedly succeeded");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&sign.stdout),
        String::from_utf8_lossy(&sign.stderr)
    );
    assert!(
        output.contains("not a regular file"),
        "unexpected output: {output}"
    );
}

#[cfg(unix)]
#[test]
fn plugin_verify_rejects_symlink_fixture() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("demo-plugin");
    copy_tree(&fixture_root().join("demo-plugin"), &plugin_dir);
    fs::write(plugin_dir.join("linked.sh"), "#!/bin/sh\n").expect("linked file");
    select_package_file(&plugin_dir, "linked.sh");
    let signature_path = temp.path().join("plugin.sig");
    let (key_path, pub_path) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "plugin",
        "sign",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(sign.status.success(), "plugin sign failed");
    fs::remove_file(plugin_dir.join("linked.sh")).expect("remove linked file");
    symlink(plugin_dir.join("run.sh"), plugin_dir.join("linked.sh")).expect("symlink");

    let verify = run_admin(&[
        "plugin",
        "verify",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        !verify.status.success(),
        "plugin verify unexpectedly succeeded"
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(output.contains("symlink"), "unexpected output: {output}");
}

#[cfg(unix)]
#[test]
fn plugin_verify_rejects_special_file_fixture() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("demo-plugin");
    copy_tree(&fixture_root().join("demo-plugin"), &plugin_dir);
    fs::write(plugin_dir.join("plugin.fifo"), "regular file").expect("special file placeholder");
    select_package_file(&plugin_dir, "plugin.fifo");
    let signature_path = temp.path().join("plugin.sig");
    let (key_path, pub_path) = generate_trust_material(temp.path(), "acme");

    let sign = run_admin(&[
        "plugin",
        "sign",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--private-key-file",
        key_path.to_str().expect("key path utf8"),
        "--output",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(sign.status.success(), "plugin sign failed");
    fs::remove_file(plugin_dir.join("plugin.fifo")).expect("remove special file placeholder");
    create_fifo(&plugin_dir.join("plugin.fifo"));

    let verify = run_admin(&[
        "plugin",
        "verify",
        plugin_dir.to_str().expect("plugin path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        !verify.status.success(),
        "plugin verify unexpectedly succeeded"
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        output.contains("not a regular file"),
        "unexpected output: {output}"
    );
}

#[test]
fn verify_rejects_untrusted_key() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = temp.path().join("fixture");
    copy_tree(&fixture_root(), &fixture);
    let workflow_path = fixture.join("demo.workflow.yaml");
    let signature_path = temp.path().join("workflow.sig");
    let (signing_key_path, _) = generate_trust_material(temp.path(), "trusted");
    let (_, other_pub_path) = generate_trust_material(temp.path(), "other");

    let sign = run_admin(&[
        "workflow",
        "sign",
        workflow_path.to_str().expect("workflow path utf8"),
        "--private-key-file",
        signing_key_path.to_str().expect("key path utf8"),
        "--output",
        signature_path.to_str().expect("signature path utf8"),
    ]);
    assert!(sign.status.success(), "workflow sign failed");

    let verify = run_admin(&[
        "workflow",
        "verify",
        workflow_path.to_str().expect("workflow path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        other_pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        !verify.status.success(),
        "workflow verify unexpectedly succeeded"
    );
    let verify_json = parse_stdout_json(&verify);
    assert_eq!(verify_json["reason_code"], "untrusted_key");
}

#[test]
fn verify_rejects_malformed_signature() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = temp.path().join("fixture");
    copy_tree(&fixture_root(), &fixture);
    let workflow_path = fixture.join("demo.workflow.yaml");
    let signature_path = temp.path().join("workflow.sig");
    let (_, pub_path) = generate_trust_material(temp.path(), "acme");

    fs::write(&signature_path, "not-json").expect("write malformed signature");

    let verify = run_admin(&[
        "workflow",
        "verify",
        workflow_path.to_str().expect("workflow path utf8"),
        "--signature",
        signature_path.to_str().expect("signature path utf8"),
        "--trust-root-file",
        pub_path.to_str().expect("pub path utf8"),
    ]);
    assert!(
        !verify.status.success(),
        "workflow verify unexpectedly succeeded"
    );
    let verify_json = parse_stdout_json(&verify);
    assert!(
        verify_json["reason_code"]
            .as_str()
            .expect("reason code")
            .starts_with("malformed_signature:")
    );
}

#[test]
fn removed_cryptographic_file_options_are_rejected() {
    let removed_output = format!("--{}", "output");
    let removed_key = format!("--{}", "key");
    let removed_trusted_key = format!("--{}", "trusted-key");
    for args in [
        vec![
            "key",
            "generate",
            "--name",
            "acme",
            &removed_output,
            "acme.key",
        ],
        vec![
            "key",
            "export-public",
            &removed_key,
            "acme.key",
            "--trust-root-file",
            "acme.pub",
        ],
        vec![
            "workflow",
            "sign",
            "workflow.yaml",
            &removed_key,
            "acme.key",
        ],
        vec![
            "workflow",
            "verify",
            "workflow.yaml",
            "--signature",
            "workflow.sig",
            &removed_trusted_key,
            "acme.pub",
        ],
    ] {
        let output = run_admin(&args);
        assert!(
            !output.status.success(),
            "removed options unexpectedly parsed: {args:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unexpected argument"),
            "unexpected error for {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
