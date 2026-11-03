// SPDX-License-Identifier: MPL-2.0
#[cfg(not(feature = "dev-version-bypass"))]
use crate::is_reserved_test_plugin_version;
#[cfg(feature = "dev-version-bypass")]
use crate::is_test_plugin;
use crate::{verify_plugin_signature, workflow_definition_sha256};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use transist_core::{AssetLockKind, PersistedRunRecord};
use transist_core::{
    SignatureVerification, TRUST_DECISION_SCHEMA_V1, TrustDecisionRecord, TrustSubjectDecision,
};
use transist_protocol::{Recipe, StepExecutionDescriptor, for_each_invoke_step};

const WORKFLOW_INSTALL_SCHEMA_V1: &str = "transist.workflow_trust_install.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WorkflowTrustInstallRecord {
    schema: String,
    workflow_id: String,
    workflow_version: String,
    workflow_source_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature_source_path: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TrustedExecutionResolution {
    pub(crate) mode: bool,
    pub(crate) decision: Option<TrustDecisionRecord>,
    pub(crate) preset_workflow_assets: Option<(Vec<u8>, Vec<transist_core::WorkflowAssetBlob>)>,
}

pub(crate) fn disabled_resolution() -> TrustedExecutionResolution {
    TrustedExecutionResolution {
        mode: false,
        decision: None,
        preset_workflow_assets: None,
    }
}

/// Trust metadata captured during workflow installation verification.
pub(crate) struct WorkflowCompileTrust {
    /// Definition digest that was verified against the detached signature.
    pub(crate) verified_digest: Option<String>,
    /// Signer key id when signature verification succeeded.
    pub(crate) signer_key_id: Option<String>,
}

pub(crate) fn verify_workflow_package_compile(
    home_dir: &Path,
    workflow_path: &Path,
    workflow_id: &str,
    workflow_version: &str,
    trusted_execution: bool,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    signature: Option<&[u8]>,
) -> Result<WorkflowCompileTrust> {
    verify_workflow_compile_with_signature(
        home_dir,
        workflow_path,
        workflow_id,
        workflow_version,
        trusted_execution,
        snapshot,
        signature,
    )
}

#[allow(clippy::too_many_arguments)]
fn verify_workflow_compile_with_signature(
    home_dir: &Path,
    _workflow_path: &Path,
    workflow_id: &str,
    workflow_version: &str,
    trusted_execution: bool,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    signature: Option<&[u8]>,
) -> Result<WorkflowCompileTrust> {
    validate_workflow_trust_identity(workflow_id, workflow_version)?;
    if !trusted_execution {
        return Ok(WorkflowCompileTrust {
            verified_digest: None,
            signer_key_id: None,
        });
    }

    #[cfg(not(feature = "dev-version-bypass"))]
    if transist_compiler::is_reserved_test_workflow_version(workflow_version) {
        bail!("trusted workflows install denied (code=test_version_not_allowed)");
    }

    #[cfg(feature = "dev-version-bypass")]
    if transist_compiler::is_test_workflow(workflow_version) {
        return Ok(WorkflowCompileTrust {
            verified_digest: None,
            signer_key_id: None,
        });
    }

    let digest = snapshot.digest.clone();
    let verification = match signature {
        Some(signature) => {
            crate::trust::verify_workflow_snapshot_signature_bytes(home_dir, snapshot, signature)?
        }
        None => crate::trust::verify_workflow_snapshot_signature(home_dir, snapshot)?,
    };
    if !verification.verified {
        bail!(
            "{}",
            format_trusted_verification_denial(
                "trusted workflows install denied",
                home_dir,
                &verification,
            )
        );
    }
    Ok(WorkflowCompileTrust {
        verified_digest: Some(digest),
        signer_key_id: verification.key_id,
    })
}

/// Publishes or removes workflow trust metadata after its immutable recipe has committed.
pub(crate) fn publish_workflow_compile_trust_metadata(
    home_dir: &Path,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    workflow_id: &str,
    workflow_version: &str,
    trusted_execution: bool,
    signature_source_path: Option<&Path>,
) -> Result<()> {
    let metadata_path = workflow_trust_install_path(home_dir, workflow_id, workflow_version)?;
    if trusted_execution {
        return write_workflow_trust_install_record(
            home_dir,
            workflow_id,
            workflow_version,
            snapshot,
            signature_source_path,
        );
    }
    match fs::remove_file(&metadata_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed removing workflow trust metadata: {}",
                metadata_path.display()
            )
        }),
    }
}

/// Verifies the detached signature for a plugin package against the digest of
/// an immutable staged snapshot of that package.
///
/// The snapshot's bytes are the exact bytes that will be published, so this
/// proves the installed package matches the verified digest and signature. The
/// caller supplies the exact sidecar bytes it will publish, so verification and
/// publication operate on the same bytes and a same-user racer cannot swap the
/// sidecar between a verify read and a publish read.
pub(crate) fn verify_plugin_snapshot_signature(
    home_dir: &Path,
    sidecar_bytes: &[u8],
    snapshot_sha256: &str,
    plugin_id: &str,
    plugin_version: &str,
) -> Result<()> {
    #[cfg(not(feature = "dev-version-bypass"))]
    if is_reserved_test_plugin_version(plugin_version) {
        bail!(
            "trusted plugins install denied for {}@{} (code=test_version_not_allowed)",
            plugin_id,
            plugin_version
        );
    }

    #[cfg(feature = "dev-version-bypass")]
    if is_test_plugin(plugin_version) {
        return Ok(());
    }

    // The snapshot digest is authoritative. The exact sidecar bytes that bind it
    // to a signer are verified in memory here and published verbatim by the
    // caller, so the persisted `.sig` is provably the verified envelope.
    let verification = crate::trust::verify_signed_digest_bytes(
        home_dir,
        transist_core::SignatureSubjectKind::Plugin,
        snapshot_sha256,
        sidecar_bytes,
    )?;
    if !verification.verified {
        bail!(
            "{}",
            format_trusted_verification_denial(
                &format!(
                    "trusted plugins install denied for {}@{}",
                    plugin_id, plugin_version
                ),
                home_dir,
                &verification,
            )
        );
    }
    Ok(())
}

pub(crate) fn resolve_trusted_execution_for_fresh_run(
    repo_root: &Path,
    home_dir: &Path,
    run_id: &str,
    recipe: &Recipe,
    trusted_execution: bool,
) -> Result<TrustedExecutionResolution> {
    resolve_trusted_execution_with_catalog(
        repo_root,
        home_dir,
        run_id,
        recipe,
        trusted_execution,
        None,
    )
}

/// Rechecks current signatures and trust roots while sharing already inspected package bytes.
pub(crate) fn resolve_trusted_execution_with_catalog(
    repo_root: &Path,
    home_dir: &Path,
    run_id: &str,
    recipe: &Recipe,
    trusted_execution: bool,
    catalog: Option<&transist_plugins::ValidatedPluginCatalog>,
) -> Result<TrustedExecutionResolution> {
    if !trusted_execution {
        return Ok(disabled_resolution());
    }
    let (decision, snapshot) =
        build_trust_decision_with_snapshot(repo_root, home_dir, run_id, recipe, None, catalog)?;
    let preset_workflow_assets = match snapshot.as_ref().filter(|_| !decision.denied()) {
        Some(snapshot) => workflow_assets_from_snapshot(recipe, snapshot)?,
        None => None,
    };
    Ok(TrustedExecutionResolution {
        mode: true,
        decision: Some(decision),
        preset_workflow_assets,
    })
}

/// Trust decision resolver composed into the kernel by the root application.
pub(crate) struct LocalTrustDecisionResolver;

impl transist_core::TrustDecisionPort for LocalTrustDecisionResolver {
    fn resolve_run_decision(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        record: &PersistedRunRecord,
        recipe: &Recipe,
    ) -> Result<TrustDecisionRecord> {
        build_trust_decision(repo_root, home_dir, &record.run_id, recipe, Some(record))
    }
}

pub(crate) fn local_trust_decision_resolver() -> LocalTrustDecisionResolver {
    LocalTrustDecisionResolver
}

fn build_trust_decision(
    repo_root: &Path,
    home_dir: &Path,
    run_id: &str,
    recipe: &Recipe,
    record: Option<&PersistedRunRecord>,
) -> Result<TrustDecisionRecord> {
    build_trust_decision_with_snapshot(repo_root, home_dir, run_id, recipe, record, None)
        .map(|(decision, _)| decision)
}

fn build_trust_decision_with_snapshot(
    repo_root: &Path,
    home_dir: &Path,
    run_id: &str,
    recipe: &Recipe,
    record: Option<&PersistedRunRecord>,
    catalog: Option<&transist_plugins::ValidatedPluginCatalog>,
) -> Result<(
    TrustDecisionRecord,
    Option<transist_compiler::WorkflowDefinitionSnapshot>,
)> {
    let (workflow, snapshot) = verify_workflow_subject(repo_root, home_dir, recipe)?;
    let mut plugins = collect_plugin_subjects(home_dir, recipe, record, catalog)?;
    plugins.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.version.cmp(&right.version))
    });

    let workflow_bypassed = workflow.trust_decision == "bypassed";
    let denied_plugin = plugins
        .iter()
        .find(|plugin| plugin.trust_decision == "denied");
    let denied_workflow = (workflow.trust_decision == "denied").then_some(&workflow);
    let decision = if denied_workflow.is_some() || denied_plugin.is_some() {
        "denied"
    } else if workflow_bypassed {
        "bypassed"
    } else {
        "allowed"
    };

    let (reason_code, failed_verification_type) = if let Some(plugin) = denied_plugin {
        (
            plugin.reason_code.clone(),
            Some("plugin_signature".to_string()),
        )
    } else if let Some(subject) = denied_workflow {
        (
            subject.reason_code.clone(),
            Some("workflow_signature".to_string()),
        )
    } else if workflow_bypassed {
        (Some("test_version_bypass".to_string()), None)
    } else {
        (None, None)
    };

    Ok((
        TrustDecisionRecord {
            schema: TRUST_DECISION_SCHEMA_V1.to_string(),
            run_id: run_id.to_string(),
            trusted_execution_mode: true,
            decision: decision.to_string(),
            workflow,
            plugins,
            reason_code,
            failed_verification_type,
            organization_id: None,
            policy_revision: None,
            decided_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        },
        snapshot,
    ))
}

fn verify_workflow_subject(
    repo_root: &Path,
    home_dir: &Path,
    recipe: &Recipe,
) -> Result<(
    TrustSubjectDecision,
    Option<transist_compiler::WorkflowDefinitionSnapshot>,
)> {
    #[cfg(not(feature = "dev-version-bypass"))]
    if transist_compiler::is_reserved_test_workflow_version(&recipe.workflow.version) {
        return Ok((
            TrustSubjectDecision {
                id: recipe.workflow.id.clone(),
                version: recipe.workflow.version.clone(),
                digest: maybe_workflow_definition_digest(repo_root, home_dir, recipe)?
                    .unwrap_or_default(),
                signature_verified: false,
                signer_key_id: None,
                reason_code: Some("test_version_not_allowed".to_string()),
                trust_decision: "denied".to_string(),
            },
            None,
        ));
    }

    #[cfg(feature = "dev-version-bypass")]
    if transist_compiler::is_test_workflow(&recipe.workflow.version) {
        let digest =
            maybe_workflow_definition_digest(repo_root, home_dir, recipe)?.unwrap_or_default();
        return Ok((
            TrustSubjectDecision {
                id: recipe.workflow.id.clone(),
                version: recipe.workflow.version.clone(),
                digest,
                signature_verified: false,
                signer_key_id: None,
                reason_code: Some("test_version_bypass".to_string()),
                trust_decision: "bypassed".to_string(),
            },
            None,
        ));
    }

    let install = load_workflow_trust_install_record(
        home_dir,
        &recipe.workflow.id,
        &recipe.workflow.version,
    )?;
    let workflow_path = resolve_workflow_source_path(repo_root, recipe, &install)?;
    let snapshot = transist_compiler::capture_workflow_definition_with_signature(&workflow_path)?;
    let digest = snapshot.digest.clone();
    let verification = match install.signature_source_path.as_deref() {
        Some(signature_path) => {
            let signature = fs::read(signature_path)
                .with_context(|| format!("failed reading workflow signature '{signature_path}'"))?;
            crate::trust::verify_workflow_snapshot_signature_bytes(home_dir, &snapshot, &signature)?
        }
        None => crate::trust::verify_workflow_snapshot_signature(home_dir, &snapshot)?,
    };
    Ok((
        subject_from_verification(
            &recipe.workflow.id,
            &recipe.workflow.version,
            digest,
            verification,
        ),
        Some(snapshot),
    ))
}

fn workflow_assets_from_snapshot(
    recipe: &Recipe,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
) -> Result<Option<(Vec<u8>, Vec<transist_core::WorkflowAssetBlob>)>> {
    let mut referenced = std::collections::BTreeSet::new();
    collect_recipe_asset_paths(recipe, &snapshot.workflow_root, &mut referenced)?;
    if referenced.is_empty() {
        return Ok(None);
    }
    let assets = snapshot
        .assets
        .iter()
        .map(|asset| (asset.relative_path.as_str(), asset))
        .collect::<std::collections::BTreeMap<_, _>>();
    let selected = referenced
        .iter()
        .map(|relative| {
            assets.get(relative.as_str()).copied().ok_or_else(|| {
                anyhow::anyhow!("verified workflow snapshot is missing asset '{relative}'")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let entries = selected
        .iter()
        .map(|asset| {
            serde_json::json!({
                "path": format!("source/{}", asset.relative_path),
                "sha256": asset.sha256,
            })
        })
        .collect::<Vec<_>>();
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema": "transist.workflow_asset_snapshot.v1",
        "entries": entries,
    }))?;
    let blobs = selected
        .into_iter()
        .map(|asset| {
            (
                asset.sha256.clone(),
                transist_core::WorkflowAssetBlob {
                    sha256: asset.sha256.clone(),
                    bytes: asset.bytes.clone(),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect();
    Ok(Some((manifest, blobs)))
}

fn collect_recipe_asset_paths(
    recipe: &Recipe,
    workflow_root: &Path,
    paths: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    let source_dir = recipe.source_dir.as_deref().ok_or_else(|| {
        anyhow::anyhow!("verified workflow assets require a source-backed recipe")
    })?;
    for step in recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|cleanup| &cleanup.step))
    {
        if let Some(transist_protocol::RecipePromptSource::File { path }) =
            step.prompt_binding.as_ref().map(|binding| &binding.source)
        {
            paths.insert(snapshot_relative_asset_path(
                workflow_root,
                source_dir,
                path,
            )?);
        }
        for value in step.with.values() {
            collect_source_asset_paths(value, workflow_root, source_dir, paths)?;
        }
        if let StepExecutionDescriptor::Subflow { recipe } = &step.execution {
            collect_recipe_asset_paths(recipe, workflow_root, paths)?;
        }
    }
    Ok(())
}

fn collect_source_asset_paths(
    value: &transist_protocol::KernelValue,
    workflow_root: &Path,
    source_dir: &str,
    paths: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    match value {
        transist_protocol::KernelValue::String(raw) => {
            if let Some(relative) = raw.strip_prefix("source://") {
                paths.insert(snapshot_relative_asset_path(
                    workflow_root,
                    source_dir,
                    relative,
                )?);
            }
        }
        transist_protocol::KernelValue::Array(values) => {
            for value in values {
                collect_source_asset_paths(value, workflow_root, source_dir, paths)?;
            }
        }
        transist_protocol::KernelValue::Object(values) => {
            for value in values.values() {
                collect_source_asset_paths(value, workflow_root, source_dir, paths)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn snapshot_relative_asset_path(
    workflow_root: &Path,
    source_dir: &str,
    relative: &str,
) -> Result<String> {
    let source_relative = Path::new(source_dir)
        .strip_prefix(workflow_root)
        .with_context(|| {
            format!(
                "verified workflow source '{}' is outside workflow root '{}'",
                source_dir,
                workflow_root.display()
            )
        })?;
    let authored = Path::new(relative.trim());
    if authored.as_os_str().is_empty() || authored.is_absolute() {
        bail!("verified workflow asset path must be a non-empty relative path");
    }
    let mut contained = source_relative.to_path_buf();
    for component in authored.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => contained.push(name),
            std::path::Component::ParentDir if contained.pop() => {}
            _ => bail!("verified workflow asset path escapes the workflow root"),
        }
    }
    if contained.as_os_str().is_empty() {
        bail!("verified workflow asset path must resolve beneath the workflow root");
    }
    Ok(contained.to_string_lossy().replace('\\', "/"))
}

fn collect_plugin_subjects(
    home_dir: &Path,
    recipe: &Recipe,
    record: Option<&PersistedRunRecord>,
    catalog: Option<&transist_plugins::ValidatedPluginCatalog>,
) -> Result<Vec<TrustSubjectDecision>> {
    let mut subjects = Vec::new();
    if let Some(record) = record.filter(|record| !record.asset_locks.is_empty()) {
        for lock in &record.asset_locks {
            if lock.kind != AssetLockKind::Plugin {
                continue;
            }
            subjects.push((
                lock.ref_id.clone(),
                lock.version.clone().unwrap_or_default(),
                lock.sha256.clone(),
            ));
        }
    }
    if !subjects.is_empty() {
        subjects.sort();
        subjects.dedup();
        return subjects
            .into_iter()
            .map(|(plugin_id, version, compiled_digest)| {
                plugin_subject_from_descriptor(
                    home_dir,
                    plugin_id,
                    version,
                    compiled_digest,
                    catalog,
                )
            })
            .collect();
    }

    for_each_invoke_step(recipe, &mut |step, _| {
        if let StepExecutionDescriptor::Invoke {
            plugin,
            plugin_id,
            plugin_version,
            plugin_sha256,
            ..
        } = &step.execution
        {
            let plugin_id = plugin_id.as_deref().unwrap_or(plugin);
            let version = plugin_version.clone().unwrap_or_default();
            let digest = plugin_sha256.clone().unwrap_or_default();
            subjects.push((plugin_id.to_string(), version, digest));
        }
    });

    subjects.sort();
    subjects.dedup();

    subjects
        .into_iter()
        .map(|(plugin_id, version, compiled_digest)| {
            plugin_subject_from_descriptor(home_dir, plugin_id, version, compiled_digest, catalog)
        })
        .collect()
}

fn plugin_subject_from_descriptor(
    home_dir: &Path,
    plugin_id: String,
    version: String,
    compiled_digest: String,
    catalog: Option<&transist_plugins::ValidatedPluginCatalog>,
) -> Result<TrustSubjectDecision> {
    if version.trim().is_empty() {
        return Ok(TrustSubjectDecision {
            id: plugin_id,
            version,
            digest: compiled_digest,
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("missing_plugin_version".to_string()),
            trust_decision: "denied".to_string(),
        });
    }
    #[cfg(not(feature = "dev-version-bypass"))]
    if is_reserved_test_plugin_version(&version) {
        let digest = if compiled_digest.trim().is_empty() {
            let plugin_dir = home_dir.join("plugins").join(&plugin_id).join(&version);
            transist_plugins::plugin_dir_sha256(&plugin_dir).unwrap_or_default()
        } else {
            compiled_digest
        };
        return Ok(TrustSubjectDecision {
            id: plugin_id,
            version,
            digest,
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("test_version_not_allowed".to_string()),
            trust_decision: "denied".to_string(),
        });
    }

    #[cfg(feature = "dev-version-bypass")]
    if is_test_plugin(&version) {
        let digest = if compiled_digest.trim().is_empty() {
            let plugin_dir = home_dir.join("plugins").join(&plugin_id).join(&version);
            transist_plugins::plugin_dir_sha256(&plugin_dir)?
        } else {
            compiled_digest
        };
        return Ok(TrustSubjectDecision {
            id: plugin_id,
            version,
            digest,
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("test_version_bypass".to_string()),
            trust_decision: "bypassed".to_string(),
        });
    }
    let plugin_dir = home_dir.join("plugins").join(&plugin_id).join(&version);
    let digest = if let Some(catalog) = catalog {
        catalog.load(&plugin_id, &version)?.sha256.clone()
    } else {
        transist_plugins::plugin_dir_sha256(&plugin_dir)?
    };
    let verification = if catalog.is_some() {
        crate::trust::verify_plugin_signature_with_digest(&plugin_dir, home_dir, &digest)?
    } else {
        verify_plugin_signature(&plugin_dir, home_dir)?
    };
    Ok(subject_from_verification(
        &plugin_id,
        &version,
        digest,
        verification,
    ))
}

fn maybe_workflow_definition_digest(
    repo_root: &Path,
    home_dir: &Path,
    recipe: &Recipe,
) -> Result<Option<String>> {
    let path =
        workflow_trust_install_path(home_dir, &recipe.workflow.id, &recipe.workflow.version)?;
    if !path.exists() {
        return Ok(None);
    }
    let install = load_workflow_trust_install_record(
        home_dir,
        &recipe.workflow.id,
        &recipe.workflow.version,
    )?;
    let Ok(workflow_path) = resolve_workflow_source_path(repo_root, recipe, &install) else {
        return Ok(None);
    };
    Ok(Some(workflow_definition_sha256(&workflow_path)?))
}

fn subject_from_verification(
    id: &str,
    version: &str,
    digest: String,
    verification: SignatureVerification,
) -> TrustSubjectDecision {
    TrustSubjectDecision {
        id: id.to_string(),
        version: version.to_string(),
        digest,
        signature_verified: verification.verified,
        signer_key_id: verification.key_id,
        reason_code: verification.reason_code,
        trust_decision: if verification.verified {
            "verified".to_string()
        } else {
            "denied".to_string()
        },
    }
}

fn format_trusted_verification_denial(
    operation: &str,
    home_dir: &Path,
    verification: &SignatureVerification,
) -> String {
    let reason_code = verification
        .reason_code
        .as_deref()
        .unwrap_or("trusted_execution_denied");
    let mut message = format!("{operation} (code={reason_code})");
    if reason_code == "untrusted_key" {
        let signer = verification.key_id.as_deref().unwrap_or("unknown");
        message.push_str(&format!(
            ": signer key '{signer}' is not trusted by the active Transist home '{}'; import the publisher's public trust root with `transist trust import <path>`",
            home_dir.display()
        ));
    }
    message
}

pub(crate) fn workflow_trust_install_path(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
) -> Result<PathBuf> {
    validate_workflow_trust_identity(workflow_id, workflow_version)?;
    Ok(home_dir
        .join("registry")
        .join("trust")
        .join(format!("{workflow_id}-{workflow_version}.json")))
}

fn validate_workflow_trust_identity(workflow_id: &str, workflow_version: &str) -> Result<()> {
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::WorkflowId,
        workflow_id,
    )
    .with_context(|| {
        format!("invalid workflow id '{workflow_id}': cannot access trust metadata")
    })?;
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::WorkflowVersion,
        workflow_version,
    )
    .with_context(|| {
        format!("invalid workflow version '{workflow_version}': cannot access trust metadata")
    })?;
    Ok(())
}

fn write_workflow_trust_install_record(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
    snapshot: &transist_compiler::WorkflowDefinitionSnapshot,
    signature_source_path: Option<&Path>,
) -> Result<()> {
    let path = workflow_trust_install_path(home_dir, workflow_id, workflow_version)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating workflow trust dir: {}", parent.display()))?;
    }
    let record = WorkflowTrustInstallRecord {
        schema: WORKFLOW_INSTALL_SCHEMA_V1.to_string(),
        workflow_id: workflow_id.to_string(),
        workflow_version: workflow_version.to_string(),
        workflow_source_path: snapshot
            .workflow_root
            .join(&snapshot.top_level)
            .display()
            .to_string(),
        signature_source_path: signature_source_path.map(|path| path.display().to_string()),
    };
    transist_storage_local::write_json_atomic(&path, &record)
        .with_context(|| format!("failed writing workflow trust metadata: {}", path.display()))
}

fn load_workflow_trust_install_record(
    home_dir: &Path,
    workflow_id: &str,
    workflow_version: &str,
) -> Result<WorkflowTrustInstallRecord> {
    let path = workflow_trust_install_path(home_dir, workflow_id, workflow_version)?;
    let raw = fs::read_to_string(&path).with_context(|| {
        format!(
            "trusted execution requires workflows install --trusted-execution --approve for {}@{} ({})",
            workflow_id,
            workflow_version,
            path.display()
        )
    })?;
    let record: WorkflowTrustInstallRecord = serde_json::from_str(&raw)
        .with_context(|| format!("invalid workflow trust metadata: {}", path.display()))?;
    if record.schema != WORKFLOW_INSTALL_SCHEMA_V1 {
        bail!(
            "unsupported workflow trust metadata schema '{}'",
            record.schema
        );
    }
    Ok(record)
}

fn resolve_workflow_source_path(
    repo_root: &Path,
    recipe: &Recipe,
    record: &WorkflowTrustInstallRecord,
) -> Result<PathBuf> {
    let recorded_path = Path::new(&record.workflow_source_path);
    let path = if recorded_path.is_absolute() {
        recorded_path.to_path_buf()
    } else if let Some(source_dir) = recipe.source_dir.as_deref() {
        let source_dir = Path::new(source_dir);
        let source_dir = if source_dir.is_absolute() {
            source_dir.to_path_buf()
        } else {
            repo_root.join(source_dir)
        };
        let file_name = recorded_path.file_name().ok_or_else(|| {
            anyhow::anyhow!(
                "trusted execution requires a workflow source file name for {}@{}",
                record.workflow_id,
                record.workflow_version
            )
        })?;
        source_dir.join(file_name)
    } else {
        repo_root.join(recorded_path)
    };
    if !path.exists() {
        bail!(
            "trusted execution requires workflow source '{}' for {}@{}",
            path.display(),
            record.workflow_id,
            record.workflow_version
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{format_trusted_verification_denial, resolve_trusted_execution_for_fresh_run};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use transist_core::ports::RunStorePort;
    use transist_core::{AssetLockKind, SignatureVerification};
    use transist_protocol::{
        KernelValue, Recipe, RecipePromptBinding, RecipePromptSource, RecipeStep,
        StepExecutionDescriptor, WorkflowRef,
    };

    fn control_step(id: &str, prompt: &str, source: &str) -> RecipeStep {
        RecipeStep {
            id: id.to_string(),
            execution: StepExecutionDescriptor::Control {
                primitive: "noop".to_string(),
            },
            with: BTreeMap::from([(
                "helper".to_string(),
                KernelValue::String(source.to_string()),
            )]),
            prompt_binding: Some(RecipePromptBinding {
                inline_field: "prompt".to_string(),
                file_field: Some("prompt_file".to_string()),
                source: RecipePromptSource::File {
                    path: prompt.to_string(),
                },
            }),
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }
    }

    fn recipe(id: &str, source_dir: &Path, steps: Vec<RecipeStep>) -> Recipe {
        Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: id.to_string(),
                version: "v1".to_string(),
            },
            source_dir: Some(source_dir.display().to_string()),
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps,
            cleanup: Vec::new(),
            graph: None,
            capabilities: BTreeMap::new(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    #[test]
    fn untrusted_key_denial_explains_how_to_configure_trust() {
        let verification = SignatureVerification {
            verified: false,
            key_id: Some("key-123".to_string()),
            digest_matched: true,
            reason_code: Some("untrusted_key".to_string()),
        };

        let message = format_trusted_verification_denial(
            "trusted plugins install denied for jira.rest@1.5.1",
            Path::new("/tmp/transist-home"),
            &verification,
        );

        assert!(message.contains("code=untrusted_key"));
        assert!(message.contains("key-123"));
        assert!(message.contains("transist trust import <path>"));
        assert!(!message.contains("Configure Transist"));
    }

    #[test]
    fn non_configuration_denials_keep_their_compact_message() {
        let verification = SignatureVerification {
            verified: false,
            key_id: None,
            digest_matched: false,
            reason_code: Some("digest_mismatch".to_string()),
        };

        assert_eq!(
            format_trusted_verification_denial(
                "trusted workflows install denied",
                Path::new("/tmp/transist-home"),
                &verification,
            ),
            "trusted workflows install denied (code=digest_mismatch)"
        );
    }

    #[test]
    fn trusted_same_directory_subflow_uses_rebased_verified_assets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let home = root.join("home");
        fs::create_dir_all(root.join("prompts")).expect("prompts");
        fs::create_dir_all(root.join("scripts")).expect("scripts");
        fs::write(root.join("prompts/root.md"), "verified root prompt").expect("root prompt");
        fs::write(root.join("prompts/child.md"), "verified child prompt").expect("child prompt");
        fs::write(root.join("scripts/child.sh"), "verified child source").expect("child source");
        fs::write(
            root.join("child.yaml"),
            r#"schema: transist.module.v1
id: trusted_same_dir_child
version: v1
steps:
  - id: child_asset
    control: noop
    prompt_file: prompts/child.md
    with:
      helper: source://scripts/child.sh
"#,
        )
        .expect("child workflow");
        let workflow = root.join("workflow.yaml");
        fs::write(
            &workflow,
            r#"schema: transist.flow.v1
id: trusted_same_dir
version: v1
modules:
  child:
    path: child.yaml
steps:
  - id: root_asset
    control: noop
    prompt_file: prompts/root.md
  - id: child
    module: child
"#,
        )
        .expect("root workflow");

        crate::init_home_dir_with_mode(&home, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init home");
        let key = root.join("signing.key");
        let public_key = root.join("signing.pub");
        transist_admin::generate_key("same-dir", &key).expect("generate key");
        transist_admin::export_public_key(&key, &public_key).expect("export key");
        crate::import_trust_root_file(&home, &public_key).expect("import key");
        transist_admin::sign_workflow(
            &workflow,
            &key,
            &transist_core::signature_sidecar_path(&workflow),
        )
        .expect("sign workflow");
        let snapshot = transist_compiler::capture_workflow_definition_with_signature(&workflow)
            .expect("capture signed snapshot");
        super::write_workflow_trust_install_record(
            &home,
            "trusted_same_dir",
            "v1",
            &snapshot,
            None,
        )
        .expect("write trust metadata");

        let child_recipe = recipe(
            "trusted_same_dir_child",
            &snapshot.workflow_root,
            vec![control_step(
                "child_asset",
                "prompts/child.md",
                "source://scripts/child.sh",
            )],
        );
        let mut child_step = control_step("child", "unused", "literal");
        child_step.prompt_binding = None;
        child_step.with.clear();
        child_step.execution = StepExecutionDescriptor::Subflow {
            recipe: Box::new(child_recipe),
        };
        let root_recipe = recipe(
            "trusted_same_dir",
            &snapshot.workflow_root,
            vec![
                control_step("root_asset", "prompts/root.md", "literal"),
                child_step,
            ],
        );
        let resolution = resolve_trusted_execution_for_fresh_run(
            root,
            &home,
            "same-dir-run",
            &root_recipe,
            true,
        )
        .expect("verify trust and capture assets");

        fs::write(root.join("prompts/child.md"), "mutated child prompt")
            .expect("mutate child prompt");
        fs::write(root.join("scripts/child.sh"), "mutated child source")
            .expect("mutate child source");
        let kernel = crate::composition::default_runtime_kernel(
            &crate::composition::router_cache::RouterCache::local(),
            root,
            &home,
        )
        .expect("kernel");
        let summary = kernel
            .execute_recipe_with_inputs_and_trust_and_assets(
                root,
                &home,
                &root_recipe,
                Some("same-dir-run"),
                BTreeMap::new(),
                resolution.mode,
                resolution.decision,
                false,
                resolution.preset_workflow_assets,
            )
            .expect("execute verified snapshot");
        assert_eq!(summary.status.as_str(), "success", "{summary:?}");

        let store = transist_storage_local::LocalRunStore::new(
            transist_storage_local::LocalStorageConfig::new(&home),
        );
        let child_record = store
            .load_run("same-dir-run__child")
            .expect("load child")
            .expect("child exists");
        let child_prompt = child_record
            .asset_locks
            .iter()
            .find(|lock| lock.kind == AssetLockKind::PromptFile)
            .expect("child prompt lock");
        assert_eq!(
            store
                .read_workflow_asset_blob("same-dir-run__child", &child_prompt.sha256)
                .expect("read child prompt")
                .expect("child prompt blob"),
            b"verified child prompt"
        );
        let manifest = store
            .read_workflow_asset_manifest("same-dir-run__child")
            .expect("read child manifest")
            .expect("child manifest");
        let manifest = String::from_utf8(manifest).expect("manifest UTF-8");
        assert!(manifest.contains("source/scripts/child.sh"));
        assert!(!manifest.contains("source//"));
        assert!(!manifest.contains("source/prompts/root.md"));
    }

    #[test]
    fn trusted_assetless_subflow_starts_without_a_preset_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let home = root.join("home");
        let child_dir = root.join("child");
        fs::create_dir_all(root.join("prompts")).expect("prompts");
        fs::create_dir_all(&child_dir).expect("child directory");
        fs::write(root.join("prompts/root.md"), "verified root prompt").expect("root prompt");
        fs::write(
            child_dir.join("workflow.yaml"),
            "schema: transist.module.v1\nid: trusted_assetless_child\nversion: v1\nsteps:\n  - id: noop\n    control: noop\n",
        )
        .expect("child workflow");
        let workflow = root.join("workflow.yaml");
        fs::write(
            &workflow,
            r#"schema: transist.flow.v1
id: trusted_assetless
version: v1
modules:
  child:
    path: child/workflow.yaml
steps:
  - id: root_asset
    control: noop
    prompt_file: prompts/root.md
  - id: child
    module: child
"#,
        )
        .expect("root workflow");

        crate::init_home_dir_with_mode(&home, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init home");
        let key = root.join("signing.key");
        let public_key = root.join("signing.pub");
        transist_admin::generate_key("assetless", &key).expect("generate key");
        transist_admin::export_public_key(&key, &public_key).expect("export key");
        crate::import_trust_root_file(&home, &public_key).expect("import key");
        transist_admin::sign_workflow(
            &workflow,
            &key,
            &transist_core::signature_sidecar_path(&workflow),
        )
        .expect("sign workflow");
        let snapshot = transist_compiler::capture_workflow_definition_with_signature(&workflow)
            .expect("capture signed snapshot");
        super::write_workflow_trust_install_record(
            &home,
            "trusted_assetless",
            "v1",
            &snapshot,
            None,
        )
        .expect("write trust metadata");

        let mut assetless_step = control_step("noop", "unused", "literal");
        assetless_step.prompt_binding = None;
        assetless_step.with.clear();
        let child_recipe = recipe(
            "trusted_assetless_child",
            &snapshot.workflow_root.join("child"),
            vec![assetless_step],
        );
        let mut child_step = control_step("child", "unused", "literal");
        child_step.prompt_binding = None;
        child_step.with.clear();
        child_step.execution = StepExecutionDescriptor::Subflow {
            recipe: Box::new(child_recipe),
        };
        let root_recipe = recipe(
            "trusted_assetless",
            &snapshot.workflow_root,
            vec![
                control_step("root_asset", "prompts/root.md", "literal"),
                child_step,
            ],
        );
        let resolution = resolve_trusted_execution_for_fresh_run(
            root,
            &home,
            "assetless-run",
            &root_recipe,
            true,
        )
        .expect("verify trust and capture assets");
        let kernel = crate::composition::default_runtime_kernel(
            &crate::composition::router_cache::RouterCache::local(),
            root,
            &home,
        )
        .expect("kernel");
        let summary = kernel
            .execute_recipe_with_inputs_and_trust_and_assets(
                root,
                &home,
                &root_recipe,
                Some("assetless-run"),
                BTreeMap::new(),
                resolution.mode,
                resolution.decision,
                false,
                resolution.preset_workflow_assets,
            )
            .expect("execute assetless child");
        let store = transist_storage_local::LocalRunStore::new(
            transist_storage_local::LocalStorageConfig::new(&home),
        );
        let root_record = store
            .load_run("assetless-run")
            .expect("load root")
            .expect("root exists");
        assert_eq!(
            summary.status.as_str(),
            "success",
            "{:?}",
            root_record.steps["child"].last_result
        );
        let child_record = store
            .load_run("assetless-run__child")
            .expect("load child")
            .expect("child exists");
        assert!(child_record.asset_locks.is_empty());
        assert!(
            store
                .read_workflow_asset_manifest("assetless-run__child")
                .expect("read child manifest")
                .is_none()
        );
    }

    #[test]
    fn trusted_birth_and_nested_birth_use_the_verified_snapshot_after_source_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let home = root.join("home");
        let child_dir = root.join("child");
        fs::create_dir_all(root.join("prompts")).expect("root prompts");
        fs::create_dir_all(root.join("scripts")).expect("root scripts");
        fs::create_dir_all(child_dir.join("prompts")).expect("child prompts");
        fs::create_dir_all(child_dir.join("scripts")).expect("child scripts");
        fs::write(root.join("prompts/root.md"), "verified root prompt").expect("root prompt");
        fs::write(root.join("scripts/root.sh"), "verified root source").expect("root source");
        fs::write(child_dir.join("prompts/child.md"), "verified child prompt")
            .expect("child prompt");
        fs::write(child_dir.join("scripts/child.sh"), "verified child source")
            .expect("child source");
        fs::write(
            child_dir.join("workflow.yaml"),
            r#"schema: transist.module.v1
id: trusted_child
version: v1
steps:
  - id: child_asset
    control: noop
    prompt_file: prompts/child.md
    with:
      helper: source://scripts/child.sh
"#,
        )
        .expect("child workflow");
        let workflow = root.join("workflow.yaml");
        fs::write(
            &workflow,
            r#"schema: transist.flow.v1
id: trusted_race
version: v1
modules:
  child:
    path: child/workflow.yaml
steps:
  - id: root_asset
    control: noop
    prompt_file: prompts/root.md
    with:
      helper: source://scripts/root.sh
  - id: child
    module: child
"#,
        )
        .expect("root workflow");

        crate::init_home_dir_with_mode(&home, transist_core::RuntimeOperatingMode::Standalone)
            .expect("init home");
        let key = root.join("signing.key");
        let public_key = root.join("signing.pub");
        transist_admin::generate_key("snapshot-race", &key).expect("generate key");
        transist_admin::export_public_key(&key, &public_key).expect("export key");
        crate::import_trust_root_file(&home, &public_key).expect("import key");
        transist_admin::sign_workflow(
            &workflow,
            &key,
            &transist_core::signature_sidecar_path(&workflow),
        )
        .expect("sign workflow");
        let snapshot = transist_compiler::capture_workflow_definition_with_signature(&workflow)
            .expect("capture signed snapshot");
        super::write_workflow_trust_install_record(&home, "trusted_race", "v1", &snapshot, None)
            .expect("write trust metadata");

        let child_recipe = recipe(
            "trusted_child",
            &snapshot.workflow_root.join("child"),
            vec![control_step(
                "child_asset",
                "prompts/child.md",
                "source://scripts/child.sh",
            )],
        );
        let mut child_step = control_step("child", "prompts/root.md", "literal");
        child_step.prompt_binding = None;
        child_step.with.clear();
        child_step.execution = StepExecutionDescriptor::Subflow {
            recipe: Box::new(child_recipe),
        };
        let root_recipe = recipe(
            "trusted_race",
            &snapshot.workflow_root,
            vec![
                control_step("root_asset", "prompts/root.md", "source://scripts/root.sh"),
                child_step,
            ],
        );
        let resolution = resolve_trusted_execution_for_fresh_run(
            root,
            &home,
            "verified-race",
            &root_recipe,
            true,
        )
        .expect("verify trust and capture assets");

        fs::write(root.join("prompts/root.md"), "mutated root prompt").expect("mutate root");
        fs::write(child_dir.join("prompts/child.md"), "mutated child prompt")
            .expect("mutate child");
        fs::write(child_dir.join("scripts/child.sh"), "mutated child source")
            .expect("mutate child source");

        let kernel = crate::composition::default_runtime_kernel(
            &crate::composition::router_cache::RouterCache::local(),
            root,
            &home,
        )
        .expect("kernel");
        let summary = kernel
            .execute_recipe_with_inputs_and_trust_and_assets(
                root,
                &home,
                &root_recipe,
                Some("verified-race"),
                BTreeMap::new(),
                resolution.mode,
                resolution.decision,
                false,
                resolution.preset_workflow_assets,
            )
            .expect("execute verified snapshot");
        assert_eq!(summary.status.as_str(), "success");

        let store = transist_storage_local::LocalRunStore::new(
            transist_storage_local::LocalStorageConfig::new(&home),
        );
        let root_record = store
            .load_run("verified-race")
            .expect("load root")
            .expect("root exists");
        let child_run_id = root_record.steps["child"]
            .last_result
            .as_ref()
            .and_then(|result| result.output.get("child_run_id"))
            .and_then(serde_json::Value::as_str)
            .expect("child run id");
        let child_record = store
            .load_run(child_run_id)
            .expect("load child")
            .expect("child exists");
        let child_prompt = child_record
            .asset_locks
            .iter()
            .find(|lock| lock.kind == AssetLockKind::PromptFile)
            .expect("child prompt lock");
        let prompt = store
            .read_workflow_asset_blob(child_run_id, &child_prompt.sha256)
            .expect("read child prompt")
            .expect("child prompt blob");
        assert_eq!(prompt, b"verified child prompt");
        let manifest = store
            .read_workflow_asset_manifest(child_run_id)
            .expect("read child manifest")
            .expect("child manifest");
        let manifest = String::from_utf8(manifest).expect("manifest UTF-8");
        assert!(manifest.contains("source/scripts/child.sh"));
        assert!(!manifest.contains("mutated child"));
    }
}
