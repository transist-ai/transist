// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Runtime trust-store operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustService {
    pub(crate) home_dir: PathBuf,
}

impl TrustService {
    pub(crate) fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }

    /// Imports one public trust-root record into this context's home directory.
    pub fn import_trust_root(&self, pubkey_path: &Path) -> Result<crate::ImportedTrustRoot> {
        super::import_trust_root_in_home(&self.home_dir, pubkey_path)
    }

    /// Lists imported trust roots from this context's home directory.
    pub fn list_trust_roots(&self) -> Result<Vec<crate::ImportedTrustRoot>> {
        super::list_trust_roots_in_home(&self.home_dir)
    }

    /// Loads one imported trust root from this context's home directory.
    pub fn inspect_trust_root(&self, key_id: &str) -> Result<crate::TrustRootRecord> {
        super::inspect_trust_root_in_home(&self.home_dir, key_id)
    }

    /// Removes one imported trust root from this context's home directory.
    pub fn remove_trust_root(&self, key_id: &str) -> Result<crate::ImportedTrustRoot> {
        super::remove_trust_root_in_home(&self.home_dir, key_id)
    }
}
