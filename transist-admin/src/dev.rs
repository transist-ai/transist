// SPDX-License-Identifier: MPL-2.0

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fs;
use std::path::Path;

use crate::digest::{plugin_subject, workflow_subject};
use crate::key::{export_public_key, generate_key};
use crate::sign::{sign_plugin, sign_workflow};
use crate::signature::load_admin_key;

/// Report returned by regenerating the admin tooling test fixtures.
#[derive(Debug, Serialize)]
pub struct FixtureRegenerationReport {
    /// Filesystem root of the regenerated fixtures.
    pub fixture_root: String,
    /// SHA-256 identifier of the ephemeral signing key used.
    pub key_id: String,
    /// Subject key string for the regenerated workflow fixture.
    pub workflow_subject_key: String,
    /// Subject key string for the regenerated plugin fixture.
    pub plugin_subject_key: String,
}

#[derive(Debug, Serialize)]
struct FixtureMetadata<'a> {
    key_id: &'a str,
    workflow: FixtureSubject<'a>,
    plugin: FixtureSubject<'a>,
}

#[derive(Debug, Serialize)]
struct FixtureSubject<'a> {
    subject_key: &'a str,
    digest: &'a str,
}

/// Regenerates the admin tooling test fixtures (keys, signatures, and metadata) under `fixture_root`.
pub fn regenerate_fixtures(fixture_root: &Path) -> Result<FixtureRegenerationReport> {
    let key_dir = tempfile::tempdir().context("create ephemeral fixture key directory")?;
    let key_path = key_dir.path().join("test-org.key");
    let public_key_path = fixture_root.join("test-org.pub");
    let workflow_path = fixture_root.join("demo.workflow.yaml");
    let workflow_sig_path = fixture_root.join("demo.workflow.yaml.sig");
    let plugin_dir = fixture_root.join("demo-plugin");
    let plugin_sig_path = fixture_root.join("demo-plugin.sig");

    for path in [&workflow_path, &plugin_dir.join("plugin.yaml")] {
        if !path.exists() {
            bail!("fixture source is missing: {}", path.display());
        }
    }

    generate_key("test-org", &key_path)?;
    export_public_key(&key_path, &public_key_path)?;
    sign_workflow(&workflow_path, &key_path, &workflow_sig_path)?;
    sign_plugin(&plugin_dir, &key_path, &plugin_sig_path)?;

    let key_file = load_admin_key(&key_path)?;
    let workflow = workflow_subject(&workflow_path)?;
    let plugin = plugin_subject(&plugin_dir)?;

    fs::write(
        fixture_root.join("digest.expected"),
        format!("{}\n", workflow.digest),
    )
    .with_context(|| format!("write workflow digest: {}", fixture_root.display()))?;

    let metadata = FixtureMetadata {
        key_id: &key_file.record.key_id,
        workflow: FixtureSubject {
            subject_key: &workflow.subject_key(),
            digest: &workflow.digest,
        },
        plugin: FixtureSubject {
            subject_key: &plugin.subject_key(),
            digest: &plugin.digest,
        },
    };
    let metadata_body =
        serde_json::to_vec_pretty(&metadata).context("serialize fixture metadata")?;
    fs::write(fixture_root.join("fixture-metadata.json"), metadata_body)
        .with_context(|| format!("write fixture metadata: {}", fixture_root.display()))?;

    Ok(FixtureRegenerationReport {
        fixture_root: fixture_root.display().to_string(),
        key_id: key_file.record.key_id,
        workflow_subject_key: workflow.subject_key(),
        plugin_subject_key: plugin.subject_key(),
    })
}
