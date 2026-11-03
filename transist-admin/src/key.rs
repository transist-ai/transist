// SPDX-License-Identifier: MPL-2.0

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde::Serialize;
use std::path::Path;
use transist_core::{TRUST_ROOT_SCHEMA_V1, TrustRootRecord};

use crate::signature::{
    ADMIN_KEY_SCHEMA_V1, AdminKeyRecord, load_admin_key, save_admin_key, save_trust_root,
    sha256_hex,
};

/// Summary of a generated admin key pair.
#[derive(Debug, Serialize)]
pub struct GeneratedKeySummary {
    /// Human-readable name assigned to the key.
    pub name: String,
    /// SHA-256 identifier of the verifying (public) key.
    pub key_id: String,
    /// Filesystem path where the key was written.
    pub output: String,
}

/// Summary of an exported public trust root.
#[derive(Debug, Serialize)]
pub struct ExportedPublicKeySummary {
    /// Human-readable name copied from the admin key.
    pub name: String,
    /// SHA-256 identifier of the verifying (public) key.
    pub key_id: String,
    /// Filesystem path where the trust root was written.
    pub output: String,
}

/// Generates a new Ed25519 admin key pair and writes it to `output`.
pub fn generate_key(name: &str, output: &Path) -> Result<GeneratedKeySummary> {
    let name = validate_name(name)?;
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let key_id = sha256_hex(verifying_key.as_bytes());
    let key_file = AdminKeyRecord {
        schema: ADMIN_KEY_SCHEMA_V1.to_string(),
        name: name.to_string(),
        key_id: key_id.clone(),
        private_key: BASE64.encode(signing_key.to_bytes()),
        public_key: BASE64.encode(verifying_key.to_bytes()),
    };
    save_admin_key(output, &key_file)?;
    Ok(GeneratedKeySummary {
        name: name.to_string(),
        key_id,
        output: output.display().to_string(),
    })
}

fn validate_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    anyhow::ensure!(!trimmed.is_empty(), "name must not be empty");
    anyhow::ensure!(
        !trimmed.contains('/') && !trimmed.contains('\\'),
        "name '{}' must not contain path separators",
        name
    );
    Ok(trimmed)
}

/// Exports the public half of an existing admin key as a trust root file.
pub fn export_public_key(key_path: &Path, output: &Path) -> Result<ExportedPublicKeySummary> {
    let key_file = load_admin_key(key_path)?;
    let public_key_file = TrustRootRecord {
        schema: TRUST_ROOT_SCHEMA_V1.to_string(),
        name: key_file.record.name.clone(),
        key_id: key_file.record.key_id.clone(),
        public_key: key_file.record.public_key.clone(),
    };
    save_trust_root(output, &public_key_file)?;
    Ok(ExportedPublicKeySummary {
        name: key_file.record.name,
        key_id: key_file.record.key_id,
        output: output.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn generated_private_key_is_written_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("trust/acme.key");
        generate_key("acme", &key_path).expect("generate key");

        let mode = fs::metadata(&key_path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
