// SPDX-License-Identifier: MPL-2.0

use anyhow::Result;
use ed25519_dalek::Verifier;
use serde::Serialize;
use std::path::Path;
use transist_core::SignatureSubjectKind;

use crate::digest::{
    SubjectDescriptor, plugin_subject, workflow_subject, workflow_subject_from_snapshot,
};
use crate::signature::{
    decode_sha256_hex, decode_signature, kind_label, load_signature_envelope, load_trust_root,
    verifying_key_from_trust_root,
};

/// Outcome of verifying a signature against a trusted key.
#[derive(Debug, Serialize)]
pub struct SignatureVerification {
    /// Whether the signature passed all verification checks.
    pub verified: bool,
    /// Whether the envelope digest matches the recomputed subject digest.
    pub digest_matched: bool,
    /// Whether the signing key matches the provided trusted root.
    pub trusted_key: bool,
    /// Machine-readable reason code explaining the verification outcome.
    pub reason_code: String,
    /// Key identifier from the signature envelope, when available.
    pub key_id: Option<String>,
    /// Kind label of the signed subject, when available.
    pub subject_kind: Option<String>,
    /// Identifier of the signed subject, when available.
    pub subject_id: Option<String>,
    /// Version of the signed subject, when available.
    pub subject_version: Option<String>,
    /// SHA-256 content digest of the signed subject, when available.
    pub digest: Option<String>,
}

/// Verifies a workflow signature against a trusted public key.
pub fn verify_workflow(
    workflow: &Path,
    signature: &Path,
    trusted_key: &Path,
) -> Result<SignatureVerification> {
    verify_subject(
        workflow_subject(workflow)?,
        signature,
        trusted_key,
        SignatureSubjectKind::Workflow,
    )
}

/// Verifies a signature against an immutable captured workflow definition.
pub fn verify_workflow_snapshot(
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    signature: &Path,
    trusted_key: &Path,
) -> Result<SignatureVerification> {
    verify_subject(
        workflow_subject_from_snapshot(snapshot)?,
        signature,
        trusted_key,
        SignatureSubjectKind::Workflow,
    )
}

/// Verifies a plugin signature against a trusted public key.
pub fn verify_plugin(
    plugin_dir: &Path,
    signature: &Path,
    trusted_key: &Path,
) -> Result<SignatureVerification> {
    verify_subject(
        plugin_subject(plugin_dir)?,
        signature,
        trusted_key,
        SignatureSubjectKind::Plugin,
    )
}

fn verify_subject(
    subject: SubjectDescriptor,
    signature_path: &Path,
    trusted_key_path: &Path,
    expected_kind: SignatureSubjectKind,
) -> Result<SignatureVerification> {
    let envelope = match load_signature_envelope(signature_path) {
        Ok(envelope) => envelope,
        Err(error) => return Ok(failed("malformed_signature", Some(error.to_string()))),
    };
    let subject_kind = Some(kind_label(envelope.subject.kind).to_string());
    let subject_id = Some(envelope.subject.id.clone());
    let subject_version = Some(envelope.subject.version.clone());
    let digest = Some(envelope.subject.digest.clone());

    if envelope.sig_alg != "ed25519" || envelope.digest_alg != "sha256" {
        return Ok(SignatureVerification {
            verified: false,
            digest_matched: false,
            trusted_key: false,
            reason_code: "unsupported_signature_format".to_string(),
            key_id: Some(envelope.key_id),
            subject_kind,
            subject_id,
            subject_version,
            digest,
        });
    }

    if envelope.subject.kind != expected_kind {
        return Ok(SignatureVerification {
            verified: false,
            digest_matched: false,
            trusted_key: false,
            reason_code: "subject_kind_mismatch".to_string(),
            key_id: Some(envelope.key_id),
            subject_kind,
            subject_id,
            subject_version,
            digest,
        });
    }

    let digest_matched = envelope.subject.id == subject.id
        && envelope.subject.version == subject.version
        && envelope.subject.digest == subject.digest;
    if !digest_matched {
        return Ok(SignatureVerification {
            verified: false,
            digest_matched: false,
            trusted_key: false,
            reason_code: "digest_mismatch".to_string(),
            key_id: Some(envelope.key_id),
            subject_kind,
            subject_id,
            subject_version,
            digest,
        });
    }

    let trust_root = match load_trust_root(trusted_key_path) {
        Ok(trust_root) => trust_root,
        Err(error) => return Ok(failed("trusted_key_read_failed", Some(error.to_string()))),
    };
    if trust_root.key_id != envelope.key_id {
        return Ok(SignatureVerification {
            verified: false,
            digest_matched: true,
            trusted_key: false,
            reason_code: "untrusted_key".to_string(),
            key_id: Some(envelope.key_id),
            subject_kind,
            subject_id,
            subject_version,
            digest,
        });
    }

    let verifying_key = match verifying_key_from_trust_root(&trust_root) {
        Ok(verifying_key) => verifying_key,
        Err(error) => return Ok(failed("trusted_key_invalid", Some(error.to_string()))),
    };
    let signature = match decode_signature(&envelope.signature) {
        Ok(signature) => signature,
        Err(error) => return Ok(failed("malformed_signature", Some(error.to_string()))),
    };
    let digest_bytes = match decode_sha256_hex(&subject.digest) {
        Ok(digest_bytes) => digest_bytes,
        Err(error) => return Ok(failed("digest_decode_failed", Some(error.to_string()))),
    };
    if let Err(error) = verifying_key.verify(&digest_bytes, &signature) {
        return Ok(failed("signature_invalid", Some(error.to_string())));
    }

    Ok(SignatureVerification {
        verified: true,
        digest_matched: true,
        trusted_key: true,
        reason_code: "verified".to_string(),
        key_id: Some(envelope.key_id),
        subject_kind,
        subject_id,
        subject_version,
        digest,
    })
}

fn failed(reason_code: &str, detail: Option<String>) -> SignatureVerification {
    SignatureVerification {
        verified: false,
        digest_matched: false,
        trusted_key: false,
        reason_code: detail
            .map(|detail| format!("{reason_code}: {detail}"))
            .unwrap_or_else(|| reason_code.to_string()),
        key_id: None,
        subject_kind: None,
        subject_id: None,
        subject_version: None,
        digest: None,
    }
}
