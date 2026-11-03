// SPDX-License-Identifier: MPL-2.0

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use transist_compiler::{WorkflowDefinitionSnapshot, capture_workflow_definition};
use transist_core::SignatureSubjectKind;
use transist_plugins::plugin_dir_sha256;
use transist_plugins::{PLUGIN_MANIFEST_FILE_NAME, load_manifest, load_manifest_file};

use crate::signature::{format_subject_key, kind_label};

/// Describes a signable subject (workflow or plugin) for digesting and signing.
#[derive(Debug, Clone)]
pub struct SubjectDescriptor {
    /// The kind of subject (workflow or plugin).
    pub kind: SignatureSubjectKind,
    /// Identifier of the subject.
    pub id: String,
    /// Version of the subject.
    pub version: String,
    /// SHA-256 content digest of the subject.
    pub digest: String,
}

impl SubjectDescriptor {
    /// Returns the lowercase label for this subject's kind.
    pub fn kind_label(&self) -> &'static str {
        kind_label(self.kind)
    }

    /// Returns the composite `kind:id@version@digest` subject key string.
    pub fn subject_key(&self) -> String {
        format_subject_key(self.kind_label(), &self.id, &self.version, &self.digest)
    }
}

#[derive(Debug, Deserialize)]
struct WorkflowIdentity {
    schema: String,
    id: String,
    version: String,
}

/// Builds a `SubjectDescriptor` for a workflow definition file.
pub fn workflow_subject(path: &Path) -> Result<SubjectDescriptor> {
    let capture = capture_workflow_definition(path)?;
    workflow_subject_from_snapshot(&capture)
}

/// Builds a workflow subject from one immutable captured definition snapshot.
pub fn workflow_subject_from_snapshot(
    capture: &WorkflowDefinitionSnapshot,
) -> Result<SubjectDescriptor> {
    let top_level = &capture.top_level;
    let raw = capture
        .assets
        .iter()
        .find(|asset| asset.relative_path == *top_level)
        .ok_or_else(|| {
            anyhow!(
                "workflow capture is missing top-level asset '{}'",
                top_level
            )
        })?;
    let raw = std::str::from_utf8(&raw.bytes)
        .with_context(|| format!("failed reading workflow '{}' as UTF-8", top_level))?;
    let identity: WorkflowIdentity =
        yaml_serde::from_str(raw).context("failed parsing workflow YAML")?;
    if identity.schema != "transist.flow.v1" {
        bail!("unsupported workflow schema '{}'", identity.schema);
    }
    if identity.id.trim().is_empty() || identity.version.trim().is_empty() {
        bail!("workflow is missing required id/version");
    }
    Ok(SubjectDescriptor {
        kind: SignatureSubjectKind::Workflow,
        id: identity.id,
        version: identity.version,
        digest: capture.digest.clone(),
    })
}

/// Builds a `SubjectDescriptor` for a plugin directory.
pub fn plugin_subject(path: &Path) -> Result<SubjectDescriptor> {
    let plugin_dir = resolve_plugin_dir(path)?;
    let manifest = load_manifest(&plugin_dir)?;
    Ok(SubjectDescriptor {
        kind: SignatureSubjectKind::Plugin,
        id: manifest.id,
        version: manifest.version,
        digest: plugin_dir_sha256(&plugin_dir)?,
    })
}

/// Resolves a path to a plugin directory, accepting either a directory or a manifest file.
pub fn resolve_plugin_dir(path: &Path) -> Result<PathBuf> {
    let canonical = canonicalize_existing_path(path)?;
    if canonical.is_dir() {
        return Ok(canonical);
    }
    let file_name = canonical.file_name().and_then(|value| value.to_str());
    if file_name != Some(PLUGIN_MANIFEST_FILE_NAME) {
        bail!(
            "plugin path '{}' must be a directory or '{}'",
            canonical.display(),
            PLUGIN_MANIFEST_FILE_NAME
        );
    }
    let _ = load_manifest_file(&canonical)?;
    canonical.parent().map(Path::to_path_buf).ok_or_else(|| {
        anyhow!(
            "plugin manifest '{}' has no parent directory",
            canonical.display()
        )
    })
}

fn canonicalize_existing_path(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("failed resolving path '{}'", path.display()))
}
