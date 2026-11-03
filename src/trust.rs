// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use transist_core::ports::TrustStoragePort;
use transist_core::{
    ImportedTrustRoot, SignatureSubjectKind, SignatureVerification, TrustRootRecord,
};
use transist_storage_local::LocalTrustStore;

/// Computes the canonical workflow-definition digest shared with `transist-admin`.
///
/// The digest covers the full authored workflow definition set: the top-level
/// workflow file plus every referenced subflow file, referenced `prompt_file`
/// targets, and `source://` helper scripts resolved beneath the workflow root.
/// Each file contributes a LF-normalized SHA-256 content digest, and the final
/// digest combines those per-file digests in sorted path order.
///
/// Templated `prompt_file` paths (containing `{{...}}`) are not resolvable at
/// signing time and are rejected with an explicit error. Use a literal path for
/// any `prompt_file` that participates in a signed workflow definition.
pub fn workflow_definition_sha256(path: &Path) -> Result<String> {
    transist_compiler::workflow_definition_sha256(path)
}

/// Returns whether a plugin version is an explicit non-SemVer test marker.
///
/// Ordinary SemVer releases, including `0.x.y` and prereleases, always use the
/// normal trust path. This predicate must not turn a catalog release into a
/// signing bypass when `dev-version-bypass` is enabled.
pub fn is_reserved_test_plugin_version(version: &str) -> bool {
    matches!(version.trim(), "0" | "v0")
}

/// Returns whether a plugin version enables development-only test behavior.
pub fn is_test_plugin(version: &str) -> bool {
    cfg!(feature = "dev-version-bypass") && is_reserved_test_plugin_version(version)
}

pub(crate) fn ensure_plugin_version_allowed(plugin_id: &str, version: &str) -> Result<()> {
    #[cfg(feature = "dev-version-bypass")]
    let _ = (plugin_id, version);

    #[cfg(not(feature = "dev-version-bypass"))]
    if is_reserved_test_plugin_version(version) {
        anyhow::bail!(
            "plugin '{}' uses reserved test version '{}'; rebuild with dev-version-bypass to use development-only test plugins",
            plugin_id,
            version
        );
    }
    Ok(())
}

/// Computes the workflow digest and verifies its detached `.sig` sidecar.
pub fn verify_workflow_definition_signature(
    workflow_path: &Path,
    home_dir: &Path,
) -> Result<transist_core::SignatureVerification> {
    let snapshot = transist_compiler::capture_workflow_definition_with_signature(workflow_path)?;
    verify_workflow_snapshot_signature(home_dir, &snapshot)
}

/// Verifies a detached signature against an already captured workflow snapshot.
pub(crate) fn verify_workflow_snapshot_signature(
    home_dir: &Path,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
) -> Result<transist_core::SignatureVerification> {
    let Some(signature) = snapshot.signature.as_deref() else {
        return Ok(SignatureVerification {
            verified: false,
            key_id: None,
            digest_matched: false,
            reason_code: Some("unsigned".to_string()),
        });
    };
    verify_workflow_snapshot_signature_bytes(home_dir, snapshot, signature)
}

pub(crate) fn verify_workflow_snapshot_signature_bytes(
    home_dir: &Path,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    signature: &[u8],
) -> Result<transist_core::SignatureVerification> {
    verify_signed_digest_bytes(
        home_dir,
        SignatureSubjectKind::Workflow,
        &snapshot.digest,
        signature,
    )
}

/// Computes the plugin directory digest and verifies its detached `.sig` sidecar.
pub fn verify_plugin_signature(
    plugin_dir: &Path,
    home_dir: &Path,
) -> Result<transist_core::SignatureVerification> {
    let digest = transist_plugins::plugin_dir_sha256(plugin_dir)?;
    verify_signed_digest(home_dir, plugin_dir, SignatureSubjectKind::Plugin, &digest)
}

/// Verifies the current sidecar and trust roots against an operation's inspected digest, without rehashing.
pub(crate) fn verify_plugin_signature_with_digest(
    plugin_dir: &Path,
    home_dir: &Path,
    digest: &str,
) -> Result<transist_core::SignatureVerification> {
    verify_signed_digest(home_dir, plugin_dir, SignatureSubjectKind::Plugin, digest)
}

/// Imports one public trust root into the local runtime trust store.
pub fn import_trust_root_file(home_dir: &Path, pubkey_path: &Path) -> Result<ImportedTrustRoot> {
    LocalTrustStore::new(home_dir).import_trust_root(pubkey_path)
}

/// Lists public trust roots in the local runtime trust store.
pub fn list_trust_roots(home_dir: &Path) -> Result<Vec<ImportedTrustRoot>> {
    LocalTrustStore::new(home_dir).list_trust_roots()
}

/// Loads one public trust root from the local runtime trust store.
pub fn inspect_trust_root(home_dir: &Path, key_id: &str) -> Result<TrustRootRecord> {
    LocalTrustStore::new(home_dir).inspect_trust_root(key_id)
}

/// Removes one public trust root from the local runtime trust store.
pub fn remove_trust_root(home_dir: &Path, key_id: &str) -> Result<ImportedTrustRoot> {
    LocalTrustStore::new(home_dir).remove_trust_root(key_id)
}

/// Returns the local runtime trust-store directory.
pub fn trust_store_dir(home_dir: &Path) -> PathBuf {
    transist_storage_local::trust_store_dir(home_dir)
}

fn verify_signed_digest(
    home_dir: &Path,
    subject_path: &Path,
    subject_kind: SignatureSubjectKind,
    expected_digest: &str,
) -> Result<SignatureVerification> {
    let sig_path = transist_core::signature_sidecar_path(subject_path);
    let raw = match fs::read(&sig_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SignatureVerification {
                verified: false,
                key_id: None,
                digest_matched: false,
                reason_code: Some("unsigned".to_string()),
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed reading signature file: {}", sig_path.display()));
        }
    };
    verify_signed_digest_bytes(home_dir, subject_kind, expected_digest, &raw)
}

/// Verifies exact signature bytes using a trust root loaded by local storage.
pub(crate) fn verify_signed_digest_bytes(
    home_dir: &Path,
    subject_kind: SignatureSubjectKind,
    expected_digest: &str,
    raw: &[u8],
) -> Result<SignatureVerification> {
    let trust_root = match std::str::from_utf8(raw)
        .ok()
        .and_then(|text| transist_core::parse_signature_envelope(text).ok())
    {
        Some(envelope) => LocalTrustStore::new(home_dir).load_trust_root(&envelope.key_id)?,
        None => None,
    };
    transist_core::verify_signed_digest_bytes(
        subject_kind,
        expected_digest,
        raw,
        trust_root.as_ref(),
    )
}

#[cfg(test)]
fn normalize_lf(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                normalized.push(b'\n');
                if index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                    index += 1;
                }
            }
            byte => normalized.push(byte),
        }
        index += 1;
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use std::fs;

    fn legacy_single_file_digest(path: &Path) -> String {
        let bytes = fs::read(path).expect("read workflow");
        format!("{:x}", sha2::Sha256::digest(normalize_lf(&bytes)))
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, content).expect("write file");
    }

    #[test]
    fn workflow_digest_normalizes_crlf() {
        let temp = tempfile::tempdir().expect("tempdir");
        let lf = temp.path().join("lf/workflow.yaml");
        let crlf = temp.path().join("crlf/workflow.yaml");
        write_file(
            &lf,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps: []\n",
        );
        if let Some(parent) = crlf.parent() {
            fs::create_dir_all(parent).expect("create crlf parent");
        }
        fs::write(
            &crlf,
            "schema: transist.flow.v1\r\nid: demo\r\nversion: v1\r\nsteps: []\r\n",
        )
        .expect("write crlf");

        assert_eq!(
            workflow_definition_sha256(&lf).expect("lf digest"),
            workflow_definition_sha256(&crlf).expect("crlf digest")
        );
    }

    #[test]
    fn workflow_digest_changes_on_top_level_byte_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workflow = temp.path().join("workflow.yaml");
        write_file(
            &workflow,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps: []\n",
        );
        let first = workflow_definition_sha256(&workflow).expect("first digest");
        write_file(
            &workflow,
            "schema: transist.flow.v1\nid: demo\nversion: v2\nsteps: []\n",
        );
        let second = workflow_definition_sha256(&workflow).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn workflow_digest_changes_when_module_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("workflow.yaml");
        let child = temp.path().join("modules/review.yaml");
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nmodules:\n  review:\n    path: modules/review.yaml\nsteps:\n  - id: child\n    module: review\n",
        );
        write_file(
            &child,
            "schema: transist.module.v1\nid: review\nversion: v1\nsteps: []\n",
        );
        let first = workflow_definition_sha256(&root).expect("first digest");
        write_file(
            &child,
            "schema: transist.module.v1\nid: review\nversion: v2\nsteps: []\n",
        );
        let second = workflow_definition_sha256(&root).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn workflow_digest_changes_when_source_script_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("workflow.yaml");
        let script = temp.path().join("scripts/run.sh");
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    with:\n      command: source://scripts/run.sh\n",
        );
        write_file(&script, "#!/bin/sh\necho one\n");
        let first = workflow_definition_sha256(&root).expect("first digest");
        write_file(&script, "#!/bin/sh\necho two\n");
        let second = workflow_definition_sha256(&root).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn workflow_digest_changes_when_prompt_file_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("workflow.yaml");
        let prompt = temp.path().join("prompts/task.md");
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    prompt_file: prompts/task.md\n    with:\n      command: echo\n",
        );
        write_file(&prompt, "first\n");
        let first = workflow_definition_sha256(&root).expect("first digest");
        write_file(&prompt, "second\n");
        let second = workflow_definition_sha256(&root).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn workflow_digest_includes_cleanup_prompt_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("workflow.yaml");
        let prompt = temp.path().join("prompts/cleanup.md");
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps:\n  - id: run\n    control: control.noop\ncleanup:\n  - id: notify\n    when: [success]\n    uses: process:shell/exec\n    prompt_file: prompts/cleanup.md\n    with:\n      command: echo\n",
        );
        write_file(&prompt, "cleanup prompt v1\n");
        let first = workflow_definition_sha256(&root).expect("first digest");
        write_file(&prompt, "cleanup prompt v2\n");
        let second = workflow_definition_sha256(&root).expect("second digest");
        assert_ne!(first, second);
    }

    #[test]
    fn workflow_digest_rejects_templated_prompt_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("workflow.yaml");
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    prompt_file: \"prompts/{{inputs.env}}.md\"\n    with:\n      command: echo\n",
        );
        let err = workflow_definition_sha256(&root).expect_err("templated path should fail");
        assert!(
            err.to_string().contains("template placeholder"),
            "error was: {err}"
        );
    }

    #[test]
    fn workflow_digest_rejects_missing_prompt_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("workflow.yaml");
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    prompt_file: prompts/missing.md\n    with:\n      command: echo\n",
        );
        let err = workflow_definition_sha256(&root).expect_err("missing prompt file should fail");
        assert!(err.to_string().contains(
            "workflow 'workflow.yaml' step 'run' prompt_file asset 'prompts/missing.md'"
        ));
    }

    #[test]
    fn workflow_digest_rejects_missing_source_script() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("workflow.yaml");
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    with:\n      command: source://scripts/missing.sh\n",
        );
        let err = workflow_definition_sha256(&root).expect_err("missing script should fail");
        assert!(
            err.to_string()
                .contains("workflow 'workflow.yaml' step 'run' source asset 'scripts/missing.sh'")
        );
    }

    #[test]
    fn workflow_digest_rejects_escape_beyond_workflow_root() {
        // Create two peer directories. The workflow is in `inner/`, and
        // `../outside.yaml` truly lives outside it. The canonical-root guard
        // must still reject this even though the `..` syntactic check is gone.
        let temp = tempfile::tempdir().expect("tempdir");
        let inner = temp.path().join("inner");
        fs::create_dir_all(&inner).expect("create inner");
        let root = inner.join("workflow.yaml");
        // Write a sibling file that resolves outside `inner/`
        let outside = temp.path().join("outside.yaml");
        write_file(
            &outside,
            "schema: transist.flow.v1\nid: outside\nversion: v1\nsteps: []\n",
        );
        write_file(
            &root,
            "schema: transist.flow.v1\nid: demo\nversion: v1\nmodules:\n  child:\n    path: ../outside.yaml\nsteps:\n  - id: child\n    module: child\n",
        );
        let err = workflow_definition_sha256(&root).expect_err("escape should fail");
        assert!(
            err.to_string()
                .contains("workflow module asset '../outside.yaml'"),
            "error was: {err}"
        );
    }

    #[test]
    fn workflow_digest_rejects_parent_traversal_even_when_the_target_is_under_root() {
        let temp = tempfile::tempdir().expect("tempdir");

        // Allowed: sign from the enclosing directory.
        // workflows/main.yaml references sibling/workflow.yaml directly.
        let wf_dir = temp.path().join("workflows");
        let sibling_dir = wf_dir.join("sibling");
        let main_wf = wf_dir.join("main.yaml");
        let sibling_wf = sibling_dir.join("workflow.yaml");
        write_file(
            &main_wf,
            "schema: transist.flow.v1\nid: main\nversion: v1\nmodules:\n  review:\n    path: sibling/workflow.yaml\nsteps:\n  - id: review\n    module: review\n",
        );
        write_file(
            &sibling_wf,
            "schema: transist.module.v1\nid: sibling\nversion: v1\nsteps: []\n",
        );
        // Direct contained paths remain valid.
        let digest = workflow_definition_sha256(&main_wf)
            .expect("sibling inside workflow root should succeed");
        assert!(!digest.is_empty());

        // Changing the sibling content changes the digest.
        write_file(
            &sibling_wf,
            "schema: transist.module.v1\nid: sibling\nversion: v2\nsteps: []\n",
        );
        let digest2 =
            workflow_definition_sha256(&main_wf).expect("sibling change should still succeed");
        assert_ne!(digest, digest2, "sibling change must change the digest");

        // Parent traversal is rejected before the path can be resolved, even if
        // the physical target would otherwise stay below the package root.
        write_file(
            &main_wf,
            "schema: transist.flow.v1\nid: main\nversion: v1\nmodules:\n  review:\n    path: sibling/../sibling/workflow.yaml\nsteps:\n  - id: review\n    module: review\n",
        );
        let err = workflow_definition_sha256(&main_wf).expect_err("traversal must fail");
        assert!(
            err.to_string()
                .contains("workflow module asset 'sibling/../sibling/workflow.yaml'"),
            "error was: {err}"
        );
    }

    #[test]
    fn workflow_digest_is_distinct_from_legacy_single_file_hash() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workflow = temp.path().join("demo.workflow.yaml");
        write_file(
            &workflow,
            "schema: transist.flow.v1\nid: demo.workflow\nversion: v1\nsteps: []\n",
        );
        let definition_digest = workflow_definition_sha256(&workflow).expect("definition digest");
        assert_ne!(definition_digest, legacy_single_file_digest(&workflow));
    }

    #[test]
    fn workflow_digest_is_stable_for_identical_definition_sets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a_root = temp.path().join("a/workflow.yaml");
        let a_script = temp.path().join("a/scripts/run.sh");
        let b_root = temp.path().join("b/workflow.yaml");
        let b_script = temp.path().join("b/scripts/run.sh");
        let workflow = "schema: transist.flow.v1\nid: demo\nversion: v1\nsteps:\n  - id: run\n    uses: process:shell/exec\n    with:\n      command: source://scripts/run.sh\n";
        write_file(&a_root, workflow);
        write_file(&a_script, "#!/bin/sh\nexit 0\n");
        write_file(&b_root, workflow);
        write_file(&b_script, "#!/bin/sh\nexit 0\n");
        assert_eq!(
            workflow_definition_sha256(&a_root).expect("digest a"),
            workflow_definition_sha256(&b_root).expect("digest b")
        );
    }

    #[test]
    fn reserved_test_plugin_versions_are_detected_narrowly() {
        assert!(is_reserved_test_plugin_version("0"));
        assert!(is_reserved_test_plugin_version("v0"));
        assert!(is_reserved_test_plugin_version(" v0 "));
        for release in ["0.1.0", "0.0.0", "0.0.1-beta", "0.12.0+build.1"] {
            assert!(!is_reserved_test_plugin_version(release), "{release}");
            assert!(!is_test_plugin(release), "{release}");
        }
        assert!(!is_reserved_test_plugin_version("0.foo"));
        assert!(!is_reserved_test_plugin_version("0."));
        assert!(!is_reserved_test_plugin_version("0.x"));
        assert!(!is_reserved_test_plugin_version("0beta"));
        assert!(!is_reserved_test_plugin_version(""));
        assert!(!is_reserved_test_plugin_version("1.0.0"));
        assert!(!is_reserved_test_plugin_version("v1"));
    }

    #[test]
    fn test_plugin_behavior_is_feature_gated() {
        assert_eq!(is_test_plugin("0"), cfg!(feature = "dev-version-bypass"));
        assert_eq!(is_test_plugin("v0"), cfg!(feature = "dev-version-bypass"));
        assert!(!is_test_plugin("0.1.0"));
        assert!(!is_test_plugin("1.0.0"));
    }

    #[test]
    fn normalize_lf_handles_mixed_line_endings() {
        let normalized = normalize_lf(b"a\r\nb\rc\n");
        assert_eq!(normalized, b"a\nb\nc\n");
    }
}
