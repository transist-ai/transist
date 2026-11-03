// SPDX-License-Identifier: MPL-2.0
//! Isolated readiness-validation helpers for installed workflow checks.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// A bound validation performed against an isolated copy of the active home.
pub(crate) struct StagedValidation {
    pub(crate) report: Option<Value>,
    pub(crate) summary: Option<transist::WorkflowCheckSummary>,
}

/// Installs and validates a package in an isolated home without mutating the
/// active home or authorizing any capability.
pub(crate) fn stage_bound_validation(
    repo_root: &Path,
    home_dir: &Path,
    package: &transist::WorkflowPackageSnapshot,
    inputs: std::collections::BTreeMap<String, transist::KernelValue>,
    trusted_execution: bool,
    local_only: bool,
    full: bool,
) -> Result<StagedValidation> {
    let scratch = ScratchHome::new()?;
    copy_validation_home(home_dir, scratch.path())?;
    let package_dir = scratch.path().join("captured-package");
    let package = package.materialize(&package_dir)?;
    let context = transist::TransistContext::with_home(repo_root, scratch.path());
    let install_lock = transist::lock_workflow_installation(scratch.path())?;
    let install = context.install_workflow_package_snapshot_with_trust(
        &package,
        trusted_execution,
        &install_lock,
    )?;
    let workflow_ref = format!("{}@{}", install.workflow_id, install.workflow_version);
    if full {
        let validation = context.validate_bound_workflow(
            &workflow_ref,
            inputs,
            trusted_execution,
            local_only,
        )?;
        Ok(StagedValidation {
            report: Some(serde_json::to_value(&validation)?),
            summary: None,
        })
    } else {
        Ok(StagedValidation {
            report: None,
            summary: Some(context.validate_bound_workflow_summary(
                &workflow_ref,
                inputs,
                trusted_execution,
                local_only,
            )?),
        })
    }
}

fn copy_validation_home(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for name in [
        "config.yaml",
        "settings.yaml",
        "toolchains.yaml",
        "installation.json",
        "trust",
        "plugins",
        "policy",
    ] {
        let source_path = source.join(name);
        if source_path.exists() {
            copy_tree(&source_path, &destination.join(name))?;
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        bail!("validation staging refuses symlink: {}", source.display());
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        return Ok(());
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

struct ScratchHome(PathBuf);

impl ScratchHome {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("transist-readiness-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path)?;
        Ok(Self(fs::canonicalize(path)?))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn serialized_validation<T: Serialize>(validation: &T) -> Result<Value> {
    serde_json::to_value(validation).context("failed serializing bound validation report")
}
