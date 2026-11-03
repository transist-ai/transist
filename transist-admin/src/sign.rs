// SPDX-License-Identifier: MPL-2.0

use anyhow::{Context, Result};
use ed25519_dalek::Signer;
use serde::Serialize;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use transist_core::{SIGNATURE_SCHEMA_V1, SignatureEnvelope, SignatureSubject};

use crate::digest::{
    SubjectDescriptor, plugin_subject, workflow_subject, workflow_subject_from_snapshot,
};
use crate::signature::{
    decode_sha256_hex, encode_signature, load_admin_key, save_signature_envelope,
};

/// Result of signing a workflow or plugin.
#[derive(Debug, Serialize)]
pub struct SignResult {
    /// Filesystem path where the signature envelope was written.
    pub output: String,
    /// SHA-256 identifier of the signing key.
    pub key_id: String,
    /// Kind label of the signed subject (`workflow` or `plugin`).
    pub subject_kind: String,
    /// Identifier of the signed subject.
    pub subject_id: String,
    /// Version of the signed subject.
    pub subject_version: String,
    /// SHA-256 content digest of the signed subject.
    pub digest: String,
}

/// Signs a workflow definition file with the given admin key.
pub fn sign_workflow(workflow: &Path, key: &Path, output: &Path) -> Result<SignResult> {
    sign_subject(workflow_subject(workflow)?, key, output)
}

/// Signs an immutable captured workflow definition without reading source files again.
pub fn sign_workflow_snapshot(
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    key: &Path,
    output: &Path,
) -> Result<SignResult> {
    sign_subject(workflow_subject_from_snapshot(snapshot)?, key, output)
}

/// Signs a plugin directory with the given admin key.
pub fn sign_plugin(plugin_dir: &Path, key: &Path, output: &Path) -> Result<SignResult> {
    sign_subject(plugin_subject(plugin_dir)?, key, output)
}

fn sign_subject(subject: SubjectDescriptor, key: &Path, output: &Path) -> Result<SignResult> {
    let key = load_admin_key(key)?;
    let digest_bytes = decode_sha256_hex(&subject.digest)?;
    let signature = key.signing_key.sign(&digest_bytes);
    let envelope = SignatureEnvelope {
        schema: SIGNATURE_SCHEMA_V1.to_string(),
        sig_alg: "ed25519".to_string(),
        digest_alg: "sha256".to_string(),
        subject: SignatureSubject {
            kind: subject.kind,
            id: subject.id.clone(),
            version: subject.version.clone(),
            digest: subject.digest.clone(),
        },
        key_id: key.record.key_id.clone(),
        signed_at_ms: now_ms()?,
        signature: encode_signature(&signature),
    };
    save_signature_envelope(output, &envelope)?;
    Ok(SignResult {
        output: output.display().to_string(),
        key_id: key.record.key_id,
        subject_kind: subject.kind_label().to_string(),
        subject_id: subject.id,
        subject_version: subject.version,
        digest: subject.digest,
    })
}

fn now_ms() -> Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?;
    u64::try_from(duration.as_millis()).context("current timestamp exceeds u64")
}
