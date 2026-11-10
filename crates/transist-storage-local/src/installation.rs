// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::util::now_unix_ms;

const INSTALLATION_FILE_NAME: &str = "installation.json";

/// Stable installation identity persisted for one Transist home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installation {
    /// Stable UUIDv7 for this local installation.
    pub id: String,
    /// First creation time of this installation record in unix milliseconds.
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InstallationRecord {
    id: String,
    created_at_unix_ms: u64,
    workspace_salt: String,
}

impl Installation {
    /// Loads the stable installation identity or creates it exactly once.
    pub fn load_or_create(home_dir: &Path) -> Result<Installation> {
        Ok(load_or_create_record(home_dir)?.installation())
    }

    /// Loads the stable installation identity from an already-initialized home.
    ///
    /// Returns an error when `installation.json` is absent rather than creating
    /// the file. Use this on server startup paths that must not modify the home
    /// directory. Run `transist home init` to create a properly initialized home
    /// before starting `transist-server`.
    pub fn load(home_dir: &Path) -> Result<Installation> {
        let path = installation_path(home_dir);
        if !path.exists() {
            anyhow::bail!(
                "no installation record found at {}; \
                 run `transist home init` to initialize a local Transist home, \
                 then restart the server.",
                path.display()
            );
        }
        Ok(read_record(&path)?.installation())
    }
}

/// Computes the stable hostname hash derived from the current hostname and persisted workspace salt.
pub fn installation_hostname_hash(home_dir: &Path) -> Result<String> {
    let record = load_or_create_record(home_dir)?;
    let hostname = hostname::get()
        .context("failed resolving hostname")?
        .to_string_lossy()
        .into_owned();

    let mut hasher = Sha256::new();
    hasher.update(hostname.as_bytes());
    hasher.update(record.workspace_salt.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(hex[..16].to_string())
}

fn load_or_create_record(home_dir: &Path) -> Result<InstallationRecord> {
    let path = installation_path(home_dir);
    if path.exists() {
        return read_record(&path);
    }

    let record = InstallationRecord {
        id: Uuid::now_v7().to_string(),
        created_at_unix_ms: now_unix_ms(),
        workspace_salt: Uuid::new_v4().simple().to_string(),
    };

    match crate::write_json_atomic_create_new(&path, &record) {
        Ok(()) => Ok(record),
        Err(_error) if path.exists() => read_record(&path).with_context(|| {
            format!(
                "failed reading installation record after concurrent create: {}",
                path.display()
            )
        }),
        Err(error) => Err(error)
            .with_context(|| format!("failed creating installation record: {}", path.display())),
    }
}

fn read_record(path: &Path) -> Result<InstallationRecord> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed reading installation record: {}", path.display()))?;

    serde_json::from_str(&raw)
        .with_context(|| format!("invalid installation record json: {}", path.display()))
}

fn installation_path(home_dir: &Path) -> PathBuf {
    home_dir.join(INSTALLATION_FILE_NAME)
}

impl InstallationRecord {
    fn installation(&self) -> Installation {
        Installation {
            id: self.id.clone(),
            created_at_unix_ms: self.created_at_unix_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_create_preserves_installation_identity() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let first = Installation::load_or_create(tmp.path()).expect("first install load");
        let second = Installation::load_or_create(tmp.path()).expect("second install load");

        assert_eq!(first, second);
        assert!(tmp.path().join(INSTALLATION_FILE_NAME).exists());
    }

    #[test]
    fn load_succeeds_when_installation_json_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Create the record first so load() can find it.
        let created = Installation::load_or_create(tmp.path()).expect("create");
        let loaded = Installation::load(tmp.path()).expect("load");
        assert_eq!(created, loaded);
    }

    #[test]
    fn load_fails_with_guidance_when_installation_json_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Do NOT create installation.json.
        let err = Installation::load(tmp.path()).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("transist home init"),
            "error must mention 'transist home init': {msg}"
        );
        // Must not create the file as a side-effect.
        assert!(!tmp.path().join(INSTALLATION_FILE_NAME).exists());
    }

    #[test]
    fn hostname_hash_is_stable_for_same_home() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let first = installation_hostname_hash(tmp.path()).expect("first hostname hash");
        let second = installation_hostname_hash(tmp.path()).expect("second hostname hash");

        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()));
    }
}
