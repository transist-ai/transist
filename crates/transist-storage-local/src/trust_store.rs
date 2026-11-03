// SPDX-License-Identifier: MPL-2.0
//! Local filesystem-backed trust-root storage.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use transist_core::ports::TrustStoragePort;
use transist_core::{ImportedTrustRoot, TrustRootRecord, parse_trust_root};
use transist_protocol::ids::{IdentifierKind, validate_identifier};

/// Local trust-root store rooted at one resolved Transist home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTrustStore {
    home_dir: PathBuf,
}

impl LocalTrustStore {
    /// Creates a local trust-root store for `home_dir`.
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
        }
    }
}

/// Returns the trust-root directory under one resolved Transist home.
pub fn trust_store_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("trust").join("keys")
}

impl TrustStoragePort for LocalTrustStore {
    fn import_trust_root(&self, pubkey_path: &Path) -> Result<ImportedTrustRoot> {
        let raw = fs::read_to_string(pubkey_path).with_context(|| {
            format!("failed reading trust root file: {}", pubkey_path.display())
        })?;
        let record = parse_trust_root(&raw)?;
        let trust_dir = trust_store_dir(&self.home_dir);
        fs::create_dir_all(&trust_dir)
            .with_context(|| format!("failed creating trust dir: {}", trust_dir.display()))?;
        let stored_path = trust_root_path(&self.home_dir, &record.key_id)?;
        let payload =
            serde_json::to_vec_pretty(&record).context("failed serializing trust root")?;
        crate::atomic_io::write_replace_file(&stored_path, &payload)
            .with_context(|| format!("failed writing trust root: {}", stored_path.display()))?;
        Ok(imported_root(&record))
    }

    fn list_trust_roots(&self) -> Result<Vec<ImportedTrustRoot>> {
        let trust_dir = trust_store_dir(&self.home_dir);
        if !trust_dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries = fs::read_dir(&trust_dir)
            .with_context(|| format!("failed reading trust dir: {}", trust_dir.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("failed reading trust dir: {}", trust_dir.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        let mut imported = Vec::new();
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("pub") {
                continue;
            }
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed reading trust root: {}", path.display()))?;
            let record = parse_trust_root(&raw)?;
            imported.push(imported_root(&record));
        }
        Ok(imported)
    }

    fn inspect_trust_root(&self, key_id: &str) -> Result<TrustRootRecord> {
        let path = trust_root_path(&self.home_dir, key_id)?;
        self.load_trust_root(key_id)?
            .ok_or_else(|| anyhow::anyhow!("failed reading trust root: {}", path.display()))
    }

    fn remove_trust_root(&self, key_id: &str) -> Result<ImportedTrustRoot> {
        let record = self.inspect_trust_root(key_id)?;
        let path = trust_root_path(&self.home_dir, key_id)?;
        fs::remove_file(&path)
            .with_context(|| format!("failed removing trust root: {}", path.display()))?;
        crate::fault_injection::check_fault(crate::fault_injection::FAULT_AFTER_REMOVE_FILE)?;
        crate::atomic_io::sync_directory(&trust_store_dir(&self.home_dir)).with_context(|| {
            format!(
                "failed syncing trust dir: {}",
                trust_store_dir(&self.home_dir).display()
            )
        })?;
        Ok(imported_root(&record))
    }

    fn load_trust_root(&self, key_id: &str) -> Result<Option<TrustRootRecord>> {
        // Verification treats malformed or unknown envelope key ids as an
        // untrusted key rather than turning a hostile signature into I/O error.
        let Ok(path) = trust_root_path(&self.home_dir, key_id) else {
            return Ok(None);
        };
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed reading trust root: {}", path.display()));
            }
        };
        parse_trust_root(&raw)
            .with_context(|| format!("invalid trust root: {}", path.display()))
            .map(Some)
    }
}

fn imported_root(record: &TrustRootRecord) -> ImportedTrustRoot {
    ImportedTrustRoot {
        name: record.name.clone(),
        key_id: record.key_id.clone(),
    }
}

fn trust_root_path(home_dir: &Path, key_id: &str) -> Result<PathBuf> {
    validate_identifier(IdentifierKind::TrustKeyId, key_id).context("invalid trust key id")?;
    Ok(trust_store_dir(home_dir).join(format!("{key_id}.pub")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_core::ports::TrustStoragePort;

    const FIXTURE_KEY_ID: &str = "eebb50e73c06b23b1bba2f9ccfe73436c92824056b11ee145d24aa28e1259aab";

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("admin_tooling")
    }

    #[test]
    fn import_list_inspect_and_remove_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LocalTrustStore::new(temp.path());
        let source = fixture_root().join("test-org.pub");
        let imported = store.import_trust_root(&source).expect("import trust root");
        assert_eq!(imported.key_id, FIXTURE_KEY_ID);
        let source_record = transist_core::parse_trust_root(
            &std::fs::read_to_string(source).expect("read source root"),
        )
        .expect("parse source root");
        assert_eq!(
            std::fs::read(trust_store_dir(temp.path()).join(format!("{FIXTURE_KEY_ID}.pub")))
                .expect("read stored root"),
            serde_json::to_vec_pretty(&source_record).expect("serialize source root")
        );
        assert_eq!(
            store.list_trust_roots().expect("list"),
            vec![imported.clone()]
        );
        assert_eq!(
            store
                .inspect_trust_root(FIXTURE_KEY_ID)
                .expect("inspect")
                .schema,
            transist_core::TRUST_ROOT_SCHEMA_V1
        );
        assert_eq!(
            store.remove_trust_root(FIXTURE_KEY_ID).expect("remove"),
            imported
        );
        assert!(
            store
                .list_trust_roots()
                .expect("list after remove")
                .is_empty()
        );
    }

    #[test]
    fn invalid_key_ids_are_rejected_for_operator_operations() {
        let store = LocalTrustStore::new(tempfile::tempdir().expect("tempdir").path());
        for invalid in [
            "../escape",
            "/absolute",
            "has space",
            "café",
            "-flag",
            ".hidden",
        ] {
            let inspect_err = store
                .inspect_trust_root(invalid)
                .expect_err("inspect must reject invalid key id");
            assert!(inspect_err.to_string().contains("invalid trust key id"));
            let remove_err = store
                .remove_trust_root(invalid)
                .expect_err("remove must reject invalid key id");
            assert!(remove_err.to_string().contains("invalid trust key id"));
        }
    }

    #[test]
    fn verification_lookup_returns_none_for_invalid_or_missing_key() {
        let store = LocalTrustStore::new(tempfile::tempdir().expect("tempdir").path());
        assert!(
            store
                .load_trust_root("../escape")
                .expect("lookup")
                .is_none()
        );
        assert!(
            store
                .load_trust_root(FIXTURE_KEY_ID)
                .expect("lookup")
                .is_none()
        );
    }

    #[test]
    fn remove_fault_leaves_key_absent_on_retry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LocalTrustStore::new(temp.path());
        store
            .import_trust_root(&fixture_root().join("test-org.pub"))
            .expect("import trust root");
        crate::fault_injection::set(crate::fault_injection::FAULT_AFTER_REMOVE_FILE);
        let result = store.remove_trust_root(FIXTURE_KEY_ID);
        crate::fault_injection::clear();
        assert!(result.is_err());
        assert!(store.list_trust_roots().expect("list").is_empty());
        assert!(store.inspect_trust_root(FIXTURE_KEY_ID).is_err());
    }
}
