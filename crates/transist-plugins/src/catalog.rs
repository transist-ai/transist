// SPDX-License-Identifier: MPL-2.0
//! Read-only, operation-scoped package and schema validation reuse.

use crate::{ActionDescriptor, PluginManifest};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One exact installed package inspected during an evaluation.
#[derive(Clone, Debug)]
pub struct ValidatedPluginPackage {
    /// Parsed, validated package contract.
    pub manifest: PluginManifest,
    /// Exact installed directory.
    pub directory: PathBuf,
    /// Digest of manifest and declared payload, including transitive schemas.
    pub sha256: String,
}

/// Shares package inspection and compiled schemas for one read-only operation.
/// Receipts contain exact `id@version` digests; they never carry authority or inputs.
pub struct ValidatedPluginCatalog {
    home: PathBuf,
    receipts: BTreeMap<String, String>,
    state: Mutex<CatalogState>,
}

#[derive(Default)]
struct CatalogState {
    packages: BTreeMap<String, Arc<ValidatedPluginPackage>>,
    schemas: crate::schema::SchemaCache,
}

impl ValidatedPluginCatalog {
    /// Creates a fresh catalog; reads are lazy and nothing is persisted.
    pub fn new(home: PathBuf, receipts: BTreeMap<String, String>) -> Self {
        Self {
            home,
            receipts,
            state: Mutex::new(CatalogState::default()),
        }
    }

    /// Loads and hashes an exact package once, validating its contract unless its receipt matches.
    pub fn load(&self, id: &str, version: &str) -> Result<Arc<ValidatedPluginPackage>> {
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::PluginId, id)?;
        transist_protocol::validate_identifier(
            transist_protocol::IdentifierKind::PluginVersion,
            version,
        )?;
        let key = format!("{id}@{version}");
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin catalog lock poisoned"))?;
        if let Some(package) = state.packages.get(&key) {
            return Ok(Arc::clone(package));
        }
        let directory = self.home.join("plugins").join(id).join(version);
        crate::record_validation_work(crate::ValidationWork::PackageLoad);
        let raw = std::fs::read(directory.join("plugin.yaml"))
            .context("failed reading pinned plugin manifest")?;
        let manifest: PluginManifest =
            yaml_serde::from_slice(&raw).context("invalid pinned plugin manifest")?;
        if manifest.id != id || manifest.version != version {
            bail!("plugin_pin_mismatch: installed package identity differs from pin '{key}'");
        }
        let sha256 = crate::digest::parsed_package_sha256(&directory, &manifest, raw)?;
        match self.receipts.get(&key) {
            Some(expected) if expected != &sha256 => {
                bail!("plugin_pin_mismatch: package digest differs for '{key}'")
            }
            Some(_) => {}
            None => crate::validate::validate_manifest_with_schemas(
                &directory,
                &manifest,
                &mut state.schemas,
            )?,
        }
        let package = Arc::new(ValidatedPluginPackage {
            manifest,
            directory,
            sha256,
        });
        state.packages.insert(key, Arc::clone(&package));
        Ok(package)
    }

    /// Validates every materialized input, reusing the distinct package/schema root compilation.
    pub fn validate_input(
        &self,
        package: &ValidatedPluginPackage,
        action: &ActionDescriptor,
        input: &serde_json::Value,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin catalog lock poisoned"))?;
        state
            .schemas
            .validate(&package.directory, &action.input_schema, "input", input)
    }

    /// Validates known action input portions using explicit deferred JSON pointers.
    /// Reuses the schema root but never treats unresolved values as valid instances.
    pub fn validate_input_partial(
        &self,
        package: &ValidatedPluginPackage,
        action: &ActionDescriptor,
        input: &serde_json::Value,
        deferred_paths: &[String],
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin catalog lock poisoned"))?;
        state.schemas.validate_partial(
            &package.directory,
            &action.input_schema,
            input,
            deferred_paths,
        )
    }
}
