// SPDX-License-Identifier: MPL-2.0

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::Path;
use transist_core::{SIGNATURE_SCHEMA_V1, SignatureEnvelope, TrustRootRecord, parse_trust_root};

/// Schema identifier for `transist-admin` key records.
pub const ADMIN_KEY_SCHEMA_V1: &str = "transist.admin.key.v1";

/// On-disk record for an Ed25519 admin key pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdminKeyRecord {
    /// Schema version of the key record.
    pub schema: String,
    /// Human-readable name assigned to the key.
    pub name: String,
    /// SHA-256 identifier of the verifying (public) key.
    pub key_id: String,
    /// Base64-encoded Ed25519 private key.
    pub private_key: String,
    /// Base64-encoded Ed25519 public key.
    pub public_key: String,
}

/// An admin key loaded and validated into a ready-to-use signing key.
#[derive(Debug)]
pub struct LoadedAdminKey {
    /// The parsed on-disk key record.
    pub record: AdminKeyRecord,
    /// The Ed25519 signing key derived from the private key bytes.
    pub signing_key: SigningKey,
}

/// Human-readable view of a signature envelope for inspection output.
#[derive(Debug, Clone, Serialize)]
pub struct SignatureInspectView {
    /// Kind label of the signed subject (`workflow` or `plugin`).
    pub subject_kind: String,
    /// Identifier of the signed subject.
    pub subject_id: String,
    /// Version of the signed subject.
    pub subject_version: String,
    /// SHA-256 content digest of the signed subject.
    pub digest: String,
    /// SHA-256 identifier of the signing key.
    pub key_id: String,
    /// Unix-millisecond timestamp at which the signature was produced.
    pub signed_at_ms: u64,
    /// Signature algorithm name (e.g. `ed25519`).
    pub sig_alg: String,
    /// Digest algorithm name (e.g. `sha256`).
    pub digest_alg: String,
    /// Composite `kind:id@version@digest` subject key string.
    pub subject_key: String,
}

/// Loads and validates an admin key file from disk.
pub fn load_admin_key(path: &Path) -> Result<LoadedAdminKey> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading admin key '{}'", path.display()))?;
    let record: AdminKeyRecord =
        serde_json::from_str(&raw).context("failed parsing admin key JSON")?;
    if record.schema != ADMIN_KEY_SCHEMA_V1 {
        bail!("unsupported admin key schema '{}'", record.schema);
    }
    if record.name.trim().is_empty() || record.key_id.trim().is_empty() {
        bail!("admin key is missing required fields");
    }

    let private_bytes = BASE64
        .decode(record.private_key.as_bytes())
        .context("invalid admin private_key")?;
    let public_bytes = BASE64
        .decode(record.public_key.as_bytes())
        .context("invalid admin public_key")?;
    let private_array: [u8; 32] = private_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("invalid Ed25519 private key length"))?;
    let public_array: [u8; 32] = public_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("invalid Ed25519 public key length"))?;
    let signing_key = SigningKey::from_bytes(&private_array);
    let verifying_key =
        VerifyingKey::from_bytes(&public_array).context("invalid Ed25519 public key bytes")?;
    if signing_key.verifying_key() != verifying_key {
        bail!("admin key public/private keypair does not match");
    }
    let canonical_key_id = sha256_hex(verifying_key.as_bytes());
    if canonical_key_id != record.key_id {
        bail!(
            "admin key key_id '{}' does not match public key digest '{}'",
            record.key_id,
            canonical_key_id
        );
    }
    Ok(LoadedAdminKey {
        record,
        signing_key,
    })
}

/// Writes an admin key record to disk with restrictive permissions.
pub fn save_admin_key(path: &Path, record: &AdminKeyRecord) -> Result<()> {
    save_private_json_file(path, record)
}

/// Loads and validates a trust root (public key) file from disk.
pub fn load_trust_root(path: &Path) -> Result<TrustRootRecord> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading trust root '{}'", path.display()))?;
    parse_trust_root(&raw).context("failed parsing trust root JSON")
}

/// Writes a trust root record to disk.
pub fn save_trust_root(path: &Path, record: &TrustRootRecord) -> Result<()> {
    save_json_file(path, record)
}

/// Loads and validates a signature envelope file from disk.
pub fn load_signature_envelope(path: &Path) -> Result<SignatureEnvelope> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading signature file '{}'", path.display()))?;
    let envelope: SignatureEnvelope =
        serde_json::from_str(&raw).context("failed parsing signature JSON")?;
    if envelope.schema != SIGNATURE_SCHEMA_V1 {
        bail!("unsupported signature schema '{}'", envelope.schema);
    }
    Ok(envelope)
}

/// Writes a signature envelope to disk.
pub fn save_signature_envelope(path: &Path, envelope: &SignatureEnvelope) -> Result<()> {
    save_json_file(path, envelope)
}

/// Reconstructs an Ed25519 verifying key from a trust root record.
pub fn verifying_key_from_trust_root(record: &TrustRootRecord) -> Result<VerifyingKey> {
    let public_bytes = BASE64
        .decode(record.public_key.as_bytes())
        .context("invalid trust root public_key")?;
    let public_array: [u8; 32] = public_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("invalid Ed25519 public key length"))?;
    VerifyingKey::from_bytes(&public_array).context("invalid Ed25519 public key bytes")
}

/// Base64-encodes an Ed25519 signature into a string.
pub fn encode_signature(signature: &Signature) -> String {
    BASE64.encode(signature.to_bytes())
}

/// Decodes a base64-encoded Ed25519 signature string.
pub fn decode_signature(signature: &str) -> Result<Signature> {
    let signature_bytes = BASE64
        .decode(signature.as_bytes())
        .context("invalid base64 signature")?;
    Signature::from_slice(&signature_bytes).context("invalid Ed25519 signature bytes")
}

/// Decodes a 64-character SHA-256 hex string into a 32-byte array.
pub fn decode_sha256_hex(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 {
        bail!("invalid sha256 hex length");
    }
    let mut output = [0_u8; 32];
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        let high = decode_hex_nibble(raw[index])?;
        let low = decode_hex_nibble(raw[index + 1])?;
        output[index / 2] = (high << 4) | low;
        index += 2;
    }
    Ok(output)
}

/// Computes the lowercase hex SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Formats a subject key string as `kind:id@version@digest`.
pub fn format_subject_key(kind: &str, id: &str, version: &str, digest: &str) -> String {
    format!("{kind}:{id}@{version}@{digest}")
}

/// Builds a `SignatureInspectView` from a loaded signature envelope.
pub fn inspect_view(envelope: &SignatureEnvelope) -> SignatureInspectView {
    SignatureInspectView {
        subject_kind: kind_label(envelope.subject.kind).to_string(),
        subject_id: envelope.subject.id.clone(),
        subject_version: envelope.subject.version.clone(),
        digest: envelope.subject.digest.clone(),
        key_id: envelope.key_id.clone(),
        signed_at_ms: envelope.signed_at_ms,
        sig_alg: envelope.sig_alg.clone(),
        digest_alg: envelope.digest_alg.clone(),
        subject_key: format_subject_key(
            kind_label(envelope.subject.kind),
            &envelope.subject.id,
            &envelope.subject.version,
            &envelope.subject.digest,
        ),
    }
}

/// Returns the lowercase label for a signature subject kind.
pub fn kind_label(kind: transist_core::SignatureSubjectKind) -> &'static str {
    match kind {
        transist_core::SignatureSubjectKind::Workflow => "workflow",
        transist_core::SignatureSubjectKind::Plugin => "plugin",
    }
}

fn save_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating output directory '{}'", parent.display()))?;
    }
    let payload = serde_json::to_vec_pretty(value).context("failed serializing JSON")?;
    fs::write(path, payload).with_context(|| format!("failed writing '{}'", path.display()))
}

#[cfg(unix)]
fn save_private_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating output directory '{}'", parent.display()))?;
    }

    let payload = serde_json::to_vec_pretty(value).context("failed serializing JSON")?;
    let temp_path = private_temp_path(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp_path)
        .with_context(|| {
            format!(
                "failed creating temporary private file '{}'",
                temp_path.display()
            )
        })?;
    if let Err(error) = file.write_all(&payload).with_context(|| {
        format!(
            "failed writing temporary private file '{}'",
            temp_path.display()
        )
    }) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = file.sync_all().with_context(|| {
        format!(
            "failed syncing temporary private file '{}'",
            temp_path.display()
        )
    }) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    drop(file);

    if let Err(error) = fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed moving temporary private file '{}' to '{}'",
            temp_path.display(),
            path.display()
        )
    }) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    set_private_permissions(path)
}

#[cfg(not(unix))]
fn save_private_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    save_json_file(path, value)
}

#[cfg(unix)]
fn private_temp_path(path: &Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("private-key");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{file_name}.{pid}.{nanos}.tmp"))
}

fn decode_hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!(
            "invalid hex digit '{}': expected [0-9a-fA-F]",
            char::from(byte)
        ),
    }
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms)
        .with_context(|| format!("failed setting private permissions on '{}'", path.display()))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
