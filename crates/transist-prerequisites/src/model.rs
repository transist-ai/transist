// SPDX-License-Identifier: MPL-2.0
use anyhow::{Result, bail};

pub use transist_protocol::prerequisites::{
    BinaryPrerequisite, CommandPrerequisite, EnvPrerequisite, PathKind, PathPrerequisite,
    PrerequisiteSet, VersionCheck,
};

/// Validates the structural correctness of a prerequisite set.
pub fn validate_prerequisite_set(prerequisites: &PrerequisiteSet) -> Result<()> {
    for binary in &prerequisites.binaries {
        validate_binary(binary)?;
    }
    for env in &prerequisites.env {
        validate_env(env)?;
    }
    for path in &prerequisites.paths {
        validate_path(path)?;
    }
    Ok(())
}

fn validate_binary(prerequisite: &BinaryPrerequisite) -> Result<()> {
    if prerequisite.name.trim().is_empty() {
        bail!("binary prerequisite name cannot be empty");
    }
    Ok(())
}

fn validate_env(prerequisite: &EnvPrerequisite) -> Result<()> {
    if prerequisite.name.trim().is_empty() {
        bail!("environment prerequisite name cannot be empty");
    }
    Ok(())
}

fn validate_path(prerequisite: &PathPrerequisite) -> Result<()> {
    if prerequisite.path.trim().is_empty() {
        bail!("path prerequisite path cannot be empty");
    }
    Ok(())
}
