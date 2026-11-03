// SPDX-License-Identifier: MPL-2.0
use crate::atomic_io::AnchoredDir;
use crate::run_paths::{
    resolve_run_root, validate_artifact_logical_path, validate_artifact_step_id,
};
use crate::run_store::LocalStorageConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;
use transist_core::ports::{ArtifactPort, ArtifactReadRequest, ArtifactWriteRequest};
use transist_protocol::KernelValue;

/// Filesystem-backed implementation of artifact persistence.
#[derive(Debug)]
pub struct LocalArtifactStore {
    config: LocalStorageConfig,
}

impl LocalArtifactStore {
    /// Creates a local artifact store.
    pub fn new(config: LocalStorageConfig) -> Self {
        Self { config }
    }

    /// Returns the execution directory (the descriptor anchor) for a request.
    ///
    /// The request's `step_id` and `logical_path` are validated before any path
    /// is built so no caller can escape the execution artifact directory. The
    /// run root is resolved strictly from the persisted locator; a corrupt or
    /// absent locator fails closed rather than falling back.
    fn execution_dir(&self, run_id: &str, step_id: &str, execution: u32) -> Result<PathBuf> {
        validate_artifact_step_id(step_id)?;
        let run_root = resolve_run_root(&self.config.root, run_id).with_context(|| {
            format!(
                "artifact path requires persisted run locator for run '{}'",
                run_id
            )
        })?;
        Ok(run_root
            .join("artifacts")
            .join(step_id)
            .join(format!("execution_{:03}", execution)))
    }

    /// Computes the full artifact path for a write request.
    fn artifact_path(&self, request: &ArtifactWriteRequest) -> Result<PathBuf> {
        validate_artifact_logical_path(&request.logical_path)?;
        Ok(self
            .execution_dir(&request.run_id, &request.step_id, request.execution)?
            .join(&request.logical_path))
    }

    /// Computes the full artifact path for a read request.
    fn artifact_read_path(&self, request: &ArtifactReadRequest) -> Result<PathBuf> {
        validate_artifact_logical_path(&request.logical_path)?;
        Ok(self
            .execution_dir(&request.run_id, &request.step_id, request.execution)?
            .join(&request.logical_path))
    }
}

impl ArtifactPort for LocalArtifactStore {
    fn write_artifact(&self, request: &ArtifactWriteRequest) -> Result<PathBuf> {
        let path = self.artifact_path(request)?;
        let run_root = resolve_run_root(&self.config.root, &request.run_id)?;
        let relative = path
            .strip_prefix(&run_root)
            .context("artifact path escaped run root")?;
        let json: serde_json::Value = request.payload.clone().into();
        let bytes = serde_json::to_vec_pretty(&json)?;
        AnchoredDir::open(&run_root)?
            .write_new(relative, &bytes)
            .with_context(|| format!("failed writing artifact: {}", path.display()))?;
        Ok(path)
    }

    fn read_artifact(&self, request: &ArtifactReadRequest) -> Result<KernelValue> {
        let path = self.artifact_read_path(request)?;
        let run_root = resolve_run_root(&self.config.root, &request.run_id)?;
        let relative = path
            .strip_prefix(&run_root)
            .context("artifact path escaped run root")?;
        let raw = AnchoredDir::open(&run_root)?
            .read_regular(relative)
            .with_context(|| format!("failed reading artifact: {}", path.display()))?;
        let parsed: serde_json::Value = serde_json::from_slice(&raw)
            .with_context(|| format!("invalid artifact json: {}", path.display()))?;
        Ok(parsed.into())
    }
}
