// SPDX-License-Identifier: MPL-2.0
//! Runtime trust-store application operations.
//!
//! This module exposes the verification-only trust-root lifecycle that belongs
//! on runtime machines: importing public keys produced elsewhere plus inspecting,
//! exporting, listing, and removing records from the local trust store.

/// Trust-store service facade for runtime CLI and library callers.
pub mod service;
pub use service::TrustService;

use anyhow::Result;
use std::path::Path;
use transist_core::ports::TrustStoragePort;
use transist_storage_local::LocalTrustStore;

/// Imports one trust-root public key into an explicit Transist home.
pub fn import_trust_root_in_home(
    home_dir: &Path,
    pubkey_path: &Path,
) -> Result<crate::ImportedTrustRoot> {
    LocalTrustStore::new(home_dir).import_trust_root(pubkey_path)
}

/// Lists imported trust roots from an explicit Transist home.
pub fn list_trust_roots_in_home(home_dir: &Path) -> Result<Vec<crate::ImportedTrustRoot>> {
    LocalTrustStore::new(home_dir).list_trust_roots()
}

/// Loads one imported trust root from an explicit Transist home.
pub fn inspect_trust_root_in_home(home_dir: &Path, key_id: &str) -> Result<crate::TrustRootRecord> {
    LocalTrustStore::new(home_dir).inspect_trust_root(key_id)
}

/// Removes one imported trust root from an explicit Transist home.
pub fn remove_trust_root_in_home(
    home_dir: &Path,
    key_id: &str,
) -> Result<crate::ImportedTrustRoot> {
    LocalTrustStore::new(home_dir).remove_trust_root(key_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("admin_tooling")
    }

    #[test]
    fn import_and_list_trust_roots_in_home_uses_runtime_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let imported = import_trust_root_in_home(temp.path(), &fixture_root().join("test-org.pub"))
            .expect("import trust root");
        let listed = list_trust_roots_in_home(temp.path()).expect("list trust roots");
        assert_eq!(listed, vec![imported]);
    }

    #[test]
    fn inspect_and_remove_trust_root_in_home_use_runtime_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fixture = fixture_root().join("test-org.pub");
        let imported = import_trust_root_in_home(temp.path(), &fixture).expect("import trust root");

        let inspected =
            inspect_trust_root_in_home(temp.path(), &imported.key_id).expect("inspect trust root");
        assert_eq!(inspected.name, imported.name);
        assert_eq!(inspected.key_id, imported.key_id);

        let removed =
            remove_trust_root_in_home(temp.path(), &imported.key_id).expect("remove trust root");
        assert_eq!(removed, imported);
        assert!(
            list_trust_roots_in_home(temp.path())
                .expect("list after remove")
                .is_empty()
        );
    }
}
