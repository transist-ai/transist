// SPDX-License-Identifier: MPL-2.0
//! Installation receipts attest immutable structure, never current authority.

use super::*;
use install_index::DefinitionValidation;
use std::sync::Arc;
use transist_plugins::ValidatedPluginCatalog;

fn pins(recipe: &Recipe) -> Result<BTreeMap<String, String>> {
    let mut pins = BTreeMap::new();
    let mut failure = None;
    transist_protocol::for_each_invoke_step(recipe, &mut |step, _| {
        if let StepExecutionDescriptor::Invoke {
            plugin,
            plugin_id,
            plugin_version,
            plugin_sha256,
            ..
        } = &step.execution
        {
            let key = format!(
                "{}@{}",
                plugin_id.as_deref().unwrap_or(plugin),
                plugin_version.as_deref().unwrap_or_default()
            );
            let digest = plugin_sha256.clone().unwrap_or_default();
            if !valid_digest(&digest)
                || pins
                    .insert(key, digest.clone())
                    .is_some_and(|old| old != digest)
            {
                failure = Some(anyhow::anyhow!(
                    "plugin_pin_mismatch: missing or conflicting compiled plugin digest"
                ));
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(pins)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Validates all pinned contracts and returns metadata for the installed recipe, without publishing it.
pub(super) fn receipt(home: &Path, recipe: &Recipe, path: &Path) -> Result<DefinitionValidation> {
    let digests = pins(recipe)?;
    let catalog = ValidatedPluginCatalog::new(home.to_path_buf(), BTreeMap::new());
    verify_pins(&catalog, &digests)?;
    Ok(DefinitionValidation {
        validator_revision: 2,
        recipe_sha256: recipe_sha256(path)?,
        plugin_package_digests: digests,
    })
}

fn verify_pins(catalog: &ValidatedPluginCatalog, pins: &BTreeMap<String, String>) -> Result<()> {
    for (key, digest) in pins {
        let (id, version) = key.split_once('@').ok_or_else(|| {
            anyhow::anyhow!("definition_validation_malformed: invalid package pin")
        })?;
        if catalog.load(id, version)?.sha256 != *digest {
            bail!("plugin_pin_mismatch: package '{key}' differs from compiled digest");
        }
    }
    Ok(())
}

/// Checks immutable digests and builds one read-only evaluation catalog, falling back for legacy installs.
pub(super) fn catalog(
    home: &Path,
    recipe: &Recipe,
    digest: &str,
) -> Result<Arc<ValidatedPluginCatalog>> {
    let workflow_ref = format!("{}@{}", recipe.workflow.id, recipe.workflow.version);
    let mut receipts = BTreeMap::new();
    let test = crate::is_test_workflow(&recipe.workflow.version);
    let pinned = if test { BTreeMap::new() } else { pins(recipe)? };
    if !test && let Some(entry) = install_index::install_entry(home, &workflow_ref)? {
        if !entry.recipe_sha256.is_empty() && entry.recipe_sha256 != digest {
            bail!("installed_recipe_mismatch: installed recipe bytes changed");
        }
        if let Some(receipt) = entry.definition_validation {
            if !valid_digest(&receipt.recipe_sha256)
                || receipt
                    .plugin_package_digests
                    .iter()
                    .any(|(key, value)| !key.contains('@') || !valid_digest(value))
            {
                bail!("definition_validation_malformed: invalid receipt digest or pin");
            }
            if receipt.recipe_sha256 != digest || receipt.plugin_package_digests != pinned {
                bail!("definition_validation_mismatch: receipt differs from installed recipe");
            }
            if receipt.validator_revision == 2 {
                receipts = receipt.plugin_package_digests;
            }
        }
    }
    let catalog = Arc::new(ValidatedPluginCatalog::new(home.to_path_buf(), receipts));
    verify_pins(&catalog, &pinned)?;
    Ok(catalog)
}

/// Reads each materialized source asset through the existing contained-source port; retains no bytes.
pub(super) fn check_source_assets(
    value: &serde_json::Value,
    source: Option<&Path>,
    step: &str,
    files: &LocalPromptFiles,
) -> Result<()> {
    match value {
        serde_json::Value::String(value) => {
            if let Some(relative) = value.strip_prefix("source://") {
                let source = source.ok_or_else(|| {
                    anyhow::anyhow!("source asset requires workflow source directory")
                })?;
                transist_core::PromptFilePort::read_source_asset(
                    files,
                    &relative.trim_start_matches('/').replace('\\', "/"),
                    Some(source),
                    step,
                )?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                check_source_assets(value, source, step, files)?;
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                check_source_assets(value, source, step, files)?;
            }
        }
        _ => {}
    }
    Ok(())
}
