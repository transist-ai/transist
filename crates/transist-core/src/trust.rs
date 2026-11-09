// SPDX-License-Identifier: MPL-2.0
//! Signature schemas and pure detached-signature verification primitives.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Stable schema identifier for detached signature sidecars.
pub const SIGNATURE_SCHEMA_V1: &str = "transist.signature.v1";
/// Stable schema identifier for imported trust-root public keys.
pub const TRUST_ROOT_SCHEMA_V1: &str = "transist.trust_root.v1";

/// One detached-signature subject descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignatureSubject {
    /// Subject kind recorded by the signer.
    pub kind: SignatureSubjectKind,
    /// Stable workflow or plugin identifier.
    pub id: String,
    /// Stable workflow or plugin version.
    pub version: String,
    /// Signed SHA-256 digest in lowercase hex.
    pub digest: String,
}

/// Supported detached-signature subject kinds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignatureSubjectKind {
    /// One authored workflow definition file.
    Workflow,
    /// One installed or packaged plugin directory.
    Plugin,
}

/// `transist.signature.v1` detached sidecar envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignatureEnvelope {
    /// Envelope schema identifier.
    pub schema: String,
    /// Signature algorithm label.
    pub sig_alg: String,
    /// Digest algorithm label.
    pub digest_alg: String,
    /// Signed subject identity and digest.
    pub subject: SignatureSubject,
    /// SHA-256 of the raw public key bytes.
    pub key_id: String,
    /// Millisecond timestamp emitted by the signer.
    pub signed_at_ms: u64,
    /// Base64-encoded Ed25519 signature over the raw digest bytes.
    pub signature: String,
}

/// `transist.trust_root.v1` public-key record imported into a runtime trust store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustRootRecord {
    /// Envelope schema identifier.
    pub schema: String,
    /// Human label for the imported trust root.
    pub name: String,
    /// SHA-256 of the raw public key bytes.
    pub key_id: String,
    /// Base64-encoded Ed25519 public key bytes.
    pub public_key: String,
}

/// Stored trust-root metadata surfaced by `trust list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportedTrustRoot {
    /// Human label recorded in the imported `.pub` file.
    pub name: String,
    /// Stable trust-root key identifier.
    pub key_id: String,
}

/// Shared signature-verification result contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignatureVerification {
    /// Whether the signature was verified with a trusted public key.
    pub verified: bool,
    /// Signer key id when the envelope parsed successfully.
    pub key_id: Option<String>,
    /// Whether the recomputed digest matched the envelope digest.
    pub digest_matched: bool,
    /// Denial reason code when verification failed.
    pub reason_code: Option<String>,
}

impl SignatureVerification {
    fn verified(key_id: String) -> Self {
        Self {
            verified: true,
            key_id: Some(key_id),
            digest_matched: true,
            reason_code: None,
        }
    }

    fn denied(key_id: Option<String>, digest_matched: bool, reason_code: &str) -> Self {
        Self {
            verified: false,
            key_id,
            digest_matched,
            reason_code: Some(reason_code.to_string()),
        }
    }
}

/// Returns the detached signature sidecar path for a subject path.
pub fn signature_sidecar_path(subject_path: &Path) -> PathBuf {
    let file_name = subject_path
        .file_name()
        .map(|name| {
            let mut name = name.to_os_string();
            name.push(".sig");
            name
        })
        .unwrap_or_else(|| std::ffi::OsString::from(".sig"));
    subject_path.with_file_name(file_name)
}

/// Parses and validates a detached signature envelope without accessing storage.
pub fn parse_signature_envelope(raw: &str) -> Result<SignatureEnvelope> {
    let envelope: SignatureEnvelope =
        serde_json::from_str(raw).context("failed parsing signature envelope")?;
    if envelope.schema != SIGNATURE_SCHEMA_V1 {
        bail!("unsupported signature schema '{}'", envelope.schema);
    }
    if envelope.sig_alg != "ed25519" {
        bail!("unsupported signature algorithm '{}'", envelope.sig_alg);
    }
    if envelope.digest_alg != "sha256" {
        bail!("unsupported digest algorithm '{}'", envelope.digest_alg);
    }
    if envelope.key_id.trim().is_empty()
        || envelope.subject.id.trim().is_empty()
        || envelope.subject.version.trim().is_empty()
        || envelope.subject.digest.trim().is_empty()
        || envelope.signature.trim().is_empty()
    {
        bail!("signature envelope is missing required fields");
    }
    Ok(envelope)
}

/// Parses and validates a trust-root record without accessing storage.
pub fn parse_trust_root(raw: &str) -> Result<TrustRootRecord> {
    let record: TrustRootRecord = serde_json::from_str(raw).context("failed parsing trust root")?;
    if record.schema != TRUST_ROOT_SCHEMA_V1 {
        bail!("unsupported trust root schema '{}'", record.schema);
    }
    if record.name.trim().is_empty()
        || record.key_id.trim().is_empty()
        || record.public_key.trim().is_empty()
    {
        bail!("trust root is missing required fields");
    }
    let public_key_bytes = BASE64
        .decode(record.public_key.as_bytes())
        .context("invalid trust root public_key")?;
    let canonical_key_id = sha256_hex(&public_key_bytes);
    if canonical_key_id != record.key_id {
        bail!(
            "trust root key_id '{}' does not match public key digest '{}'",
            record.key_id,
            canonical_key_id
        );
    }
    let public_key: [u8; 32] = public_key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid Ed25519 public key length"))?;
    VerifyingKey::from_bytes(&public_key).context("invalid Ed25519 public key")?;
    Ok(record)
}

/// Verifies a detached signature envelope against a recomputed subject digest.
///
/// The trust root has already been loaded by the caller's storage port. This
/// function performs no filesystem access and treats an absent or mismatched
/// trust root as the existing `untrusted_key` denial.
pub fn verify_signed_digest_bytes(
    subject_kind: SignatureSubjectKind,
    expected_digest: &str,
    raw: &[u8],
    trust_root: Option<&TrustRootRecord>,
) -> Result<SignatureVerification> {
    let Ok(raw) = std::str::from_utf8(raw) else {
        return Ok(SignatureVerification::denied(None, false, "malformed_sig"));
    };
    let envelope = match parse_signature_envelope(raw) {
        Ok(envelope) => envelope,
        Err(_) => return Ok(SignatureVerification::denied(None, false, "malformed_sig")),
    };

    if envelope.subject.kind != subject_kind {
        return Ok(SignatureVerification::denied(
            Some(envelope.key_id),
            false,
            "malformed_sig",
        ));
    }
    if envelope.subject.digest != expected_digest {
        return Ok(SignatureVerification::denied(
            Some(envelope.key_id),
            false,
            "digest_mismatch",
        ));
    }
    let Some(trust_root) = trust_root.filter(|root| root.key_id == envelope.key_id) else {
        return Ok(SignatureVerification::denied(
            Some(envelope.key_id),
            true,
            "untrusted_key",
        ));
    };

    let digest_bytes = match decode_sha256_hex(expected_digest) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(SignatureVerification::denied(
                Some(envelope.key_id),
                false,
                "malformed_sig",
            ));
        }
    };
    let signature_bytes = match BASE64.decode(envelope.signature.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(SignatureVerification::denied(
                Some(envelope.key_id),
                true,
                "malformed_sig",
            ));
        }
    };
    let signature = match Signature::from_slice(&signature_bytes) {
        Ok(signature) => signature,
        Err(_) => {
            return Ok(SignatureVerification::denied(
                Some(envelope.key_id),
                true,
                "malformed_sig",
            ));
        }
    };
    let verifying_key = decode_verifying_key(trust_root)
        .map_err(|_| anyhow::anyhow!("malformed trust root '{}'", trust_root.key_id))?;
    if verifying_key.verify(&digest_bytes, &signature).is_err() {
        return Ok(SignatureVerification::denied(
            Some(trust_root.key_id.clone()),
            true,
            "bad_signature",
        ));
    }
    Ok(SignatureVerification::verified(trust_root.key_id.clone()))
}

fn decode_verifying_key(record: &TrustRootRecord) -> Result<VerifyingKey> {
    let public_key_bytes = BASE64
        .decode(record.public_key.as_bytes())
        .context("invalid trust root public_key")?;
    VerifyingKey::from_bytes(
        &public_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid Ed25519 public key length"))?,
    )
    .context("invalid Ed25519 public key")
}

fn decode_sha256_hex(value: &str) -> Result<Vec<u8>> {
    if value.len() != 64 || value.len() % 2 != 0 {
        bail!("invalid sha256 hex length");
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        let high = decode_hex_nibble(raw[index])?;
        let low = decode_hex_nibble(raw[index + 1])?;
        bytes.push((high << 4) | low);
        index += 2;
    }
    Ok(bytes)
}

fn decode_hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid hex digit"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_KEY_ID: &str = "eebb50e73c06b23b1bba2f9ccfe73436c92824056b11ee145d24aa28e1259aab";

    fn fixture_trust_root() -> TrustRootRecord {
        parse_trust_root(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/admin_tooling/test-org.pub"
        )))
        .expect("parse root")
    }

    #[test]
    fn signature_sidecar_appends_sig_to_file_name() {
        assert_eq!(
            signature_sidecar_path(Path::new("/tmp/demo.workflow.yaml")),
            PathBuf::from("/tmp/demo.workflow.yaml.sig")
        );
        assert_eq!(
            signature_sidecar_path(Path::new("/tmp/demo-plugin")),
            PathBuf::from("/tmp/demo-plugin.sig")
        );
    }

    #[test]
    fn malformed_signature_returns_reason_code() {
        let verification = verify_signed_digest_bytes(
            SignatureSubjectKind::Workflow,
            &"00".repeat(32),
            b"{not-json",
            None,
        )
        .expect("verify malformed sig");
        assert_eq!(verification.reason_code.as_deref(), Some("malformed_sig"));
        assert!(!verification.digest_matched);
    }

    #[test]
    fn fixture_signature_verifies_with_loaded_trust_root() {
        let root = fixture_trust_root();
        let raw = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/admin_tooling/demo.workflow.yaml.sig"
        ));
        let digest = "52292daa92268539b0c43c4514b6187656fe5712c469080eec8f0d2f4311b96e";
        let verification =
            verify_signed_digest_bytes(SignatureSubjectKind::Workflow, digest, raw, Some(&root))
                .expect("verify");
        assert_eq!(
            verification,
            SignatureVerification::verified(FIXTURE_KEY_ID.to_string())
        );
    }

    #[test]
    fn mismatched_subject_kind_returns_malformed_sig() {
        let root = fixture_trust_root();
        let raw = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/admin_tooling/demo.workflow.yaml.sig"
        ));
        let digest = "52292daa92268539b0c43c4514b6187656fe5712c469080eec8f0d2f4311b96e";
        let verification =
            verify_signed_digest_bytes(SignatureSubjectKind::Plugin, digest, raw, Some(&root))
                .expect("verify");
        assert_eq!(verification.reason_code.as_deref(), Some("malformed_sig"));
    }

    #[test]
    fn missing_trust_root_is_reported_as_untrusted() {
        let raw = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/admin_tooling/demo.workflow.yaml.sig"
        ));
        let digest = "52292daa92268539b0c43c4514b6187656fe5712c469080eec8f0d2f4311b96e";
        let verification =
            verify_signed_digest_bytes(SignatureSubjectKind::Workflow, digest, raw, None)
                .expect("verify");
        assert_eq!(verification.reason_code.as_deref(), Some("untrusted_key"));
        assert!(!verification.verified);
    }
}
