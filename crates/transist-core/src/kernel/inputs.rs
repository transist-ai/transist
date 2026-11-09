// SPDX-License-Identifier: MPL-2.0
//! Persisted run-input records and artifact-summary helpers.

use super::*;
use crate::{ChildAuthorityPreparationError, bind_recipe_inputs};
use std::path::{Path, PathBuf};
use transist_protocol::NetworkPolicySource;
use transist_protocol::{
    CapabilityName, CapabilityProjection, CapabilityRequest, CapabilitySource,
    CapabilitySourceDependency,
};

/// Read-only materialization of a workflow before runtime preparation.
///
/// The returned recipe carries resolved authority, effective policies, and
/// capability projections. This type deliberately contains no run identifier
/// or persistence state, so callers can validate a bound workflow without
/// creating a run or invoking external adapters.
#[derive(Debug, Clone)]
pub struct BoundWorkflow {
    /// Recipe with resolved workspace authority, effective policies, and capabilities.
    pub recipe: Recipe,
    /// Inputs after declared defaults have been applied.
    pub inputs: BTreeMap<String, KernelValue>,
    /// Canonical declared workspace selected for the workflow.
    pub workspace_dir: PathBuf,
}

/// Binds inputs and materializes all authority known before a workflow run.
///
/// This function reads home settings, plugin metadata, and declared workspace
/// paths only. It does not resolve secrets, allocate a run, write state, or
/// invoke adapters.
pub fn bind_and_materialize_recipe(
    home_dir: &Path,
    legacy_repo_root: &Path,
    recipe: &Recipe,
    provided_inputs: BTreeMap<String, KernelValue>,
    local_only_mode: bool,
    settings_port: &dyn crate::ports::HomeSettingsPort,
    plugin_metadata: &dyn crate::ports::PluginMetadataPort,
) -> Result<BoundWorkflow> {
    let inputs = bind_recipe_inputs(recipe, provided_inputs)?;
    let mut recipe = recipe.clone();
    let workspace_dir = resolve_recipe_workspace(home_dir, legacy_repo_root, &mut recipe, &inputs)?;
    resolve_recipe_filesystem_policies(home_dir, settings_port, &mut recipe)?;
    resolve_recipe_network_policies(home_dir, settings_port, &mut recipe)?;
    populate_recipe_capabilities_with_settings(
        home_dir,
        &workspace_dir,
        &mut recipe,
        &inputs,
        local_only_mode,
        settings_port,
        plugin_metadata,
    )?;
    Ok(BoundWorkflow {
        recipe,
        inputs,
        workspace_dir,
    })
}

/// Serializes the `transist.run_inputs.v1` record for a run. Used at
/// run birth so the kernel can pass the bytes to the staged run
/// creation port.
pub(crate) fn serialize_run_inputs_record(
    run_id: &str,
    recipe: &Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Result<Vec<u8>> {
    let record = RunInputsRecord {
        schema: "transist.run_inputs.v1".to_string(),
        run_id: run_id.to_string(),
        workflow: recipe.workflow.clone(),
        values: workflow_inputs.clone(),
        sources: workflow_input_sources(recipe, workflow_inputs),
    };
    serde_json::to_vec_pretty(&record).context("failed serializing run inputs")
}

/// Derives per-input source labels from the recipe spec and final bound values.
///
/// Labels: `caller_provided` (value differs from the declared default or the
/// spec has no default), `default_materialized` (value matches the declared
/// default), `unset_null` (spec has no default and the bound value is null).
///
/// Note: a caller-provided value that happens to equal the declared default
/// is indistinguishable from a default-materialized value after binding. This
/// is an acceptable limitation given that the original caller inputs are not
/// threaded through to this point.
fn workflow_input_sources(
    recipe: &Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> BTreeMap<String, String> {
    let mut sources = BTreeMap::new();
    for (name, value) in workflow_inputs {
        let spec = recipe.inputs.get(name);
        let source = match spec {
            None => "caller_provided".to_string(),
            Some(spec) => {
                if let Some(default) = spec.default.as_ref() {
                    if default == value {
                        "default_materialized".to_string()
                    } else {
                        "caller_provided".to_string()
                    }
                } else if matches!(value, KernelValue::Null) {
                    "unset_null".to_string()
                } else {
                    "caller_provided".to_string()
                }
            }
        };
        sources.insert(name.clone(), source);
    }
    sources
}

pub(crate) fn serialize_recipe_snapshot(recipe: &Recipe) -> Result<String> {
    yaml_serde::to_string(recipe).context("failed serializing recipe yaml")
}

/// Binds birth-known child inputs while retaining explicitly unresolved ordinary data.
/// Deferred values are not treated as instances of the child's input schema. Authority
/// aliases must resolve from immutable inputs before any parent run can be born.
pub fn bind_deferred_subflow_inputs(
    child: &Recipe,
    bindings: &BTreeMap<String, KernelValue>,
    inputs: &BTreeMap<String, KernelValue>,
    vars: &BTreeMap<String, KernelValue>,
) -> Result<BTreeMap<String, KernelValue>> {
    Ok(
        bind_deferred_subflow_inputs_in_scope(child, bindings, inputs, vars, &BTreeSet::new())?
            .values,
    )
}

/// Partially bound module inputs; symbolic names are explicit and never persisted.
#[derive(Debug, Clone)]
pub struct PartialChildInputs {
    /// Known values, defaults, and retained authored deferred bindings.
    pub values: BTreeMap<String, KernelValue>,
    /// Input names that cannot yet be validated as concrete schema instances.
    pub deferred_names: BTreeSet<String>,
}

/// Binds child inputs using the enclosing module's explicit partial input scope.
/// Authority aliases are rejected before birth; known values are type-checked now.
pub fn bind_deferred_subflow_inputs_in_scope(
    child: &Recipe,
    bindings: &BTreeMap<String, KernelValue>,
    inputs: &BTreeMap<String, KernelValue>,
    vars: &BTreeMap<String, KernelValue>,
    parent_deferred: &BTreeSet<String>,
) -> Result<PartialChildInputs> {
    let authored = subflows::subflow_provided_inputs(bindings);
    let partial = crate::resolve_partial_action_value(
        &KernelValue::Object(authored),
        inputs,
        vars,
        parent_deferred,
    )?;
    let KernelValue::Object(mut provided) = partial.value else {
        unreachable!("object bindings")
    };
    let mut known_recipe = child.clone();
    let mut deferred = BTreeMap::new();
    let authority_names = authority_input_names(child);
    for (name, value) in &provided {
        if partial.deferred_paths.iter().any(|path| {
            path == &format!("/{}", transist_protocol::binding_pointer_segment(name))
                || path.starts_with(&format!(
                    "/{}/",
                    transist_protocol::binding_pointer_segment(name)
                ))
        }) {
            if !child.inputs.contains_key(name) {
                bail!("unknown child input '{name}'");
            }
            if authority_names.contains(name) {
                bail!("child authority input '{name}' depends on a post-birth value");
            }
            deferred.insert(name.clone(), value.clone());
            known_recipe.inputs.remove(name);
        }
    }
    for name in deferred.keys() {
        provided.remove(name);
    }
    let mut bound = bind_recipe_inputs(&known_recipe, provided)?;
    let deferred_names = deferred.keys().cloned().collect();
    bound.extend(deferred);
    Ok(PartialChildInputs {
        values: bound,
        deferred_names,
    })
}

/// Resolves and stores the effective filesystem policy for every step (and
/// cleanup step) in the recipe by merging global, workflow, and step-level
/// policies. Called once at prepare/start time so the persisted recipe
/// snapshot carries the effective policy for resume and replay.
///
/// Steps that already carry an `effective_filesystem_policy_source` (e.g.
/// from a replayed recipe snapshot) are left untouched.
pub(crate) fn resolve_recipe_filesystem_policies(
    home_dir: &Path,
    settings_port: &dyn crate::ports::HomeSettingsPort,
    recipe: &mut Recipe,
) -> Result<()> {
    let needs_resolution = recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|c| &c.step))
        .any(|step| step.effective_filesystem_policy_source.is_none());
    if !needs_resolution {
        return Ok(());
    }
    let global = settings_port.load_settings(home_dir)?.filesystem_policy;
    let workflow = recipe.filesystem_policy.as_ref();
    let workspace_read_only = matches!(
        recipe.workspace.as_ref().map(|binding| binding.access),
        Some(transist_protocol::WorkspaceAccessMode::ReadOnly)
    );
    for step in &mut recipe.steps {
        if step.effective_filesystem_policy_source.is_some() {
            continue;
        }
        let (effective, source) = transist_protocol::FilesystemPolicy::resolve(
            global.as_ref(),
            workflow,
            step.filesystem_policy.as_ref(),
        );
        step.effective_filesystem_policy = cap_workspace_access(workspace_read_only, effective);
        step.effective_filesystem_policy_source = Some(source);
    }
    for cleanup in &mut recipe.cleanup {
        let step = &mut cleanup.step;
        if step.effective_filesystem_policy_source.is_some() {
            continue;
        }
        let (effective, source) = transist_protocol::FilesystemPolicy::resolve(
            global.as_ref(),
            workflow,
            step.filesystem_policy.as_ref(),
        );
        step.effective_filesystem_policy = cap_workspace_access(workspace_read_only, effective);
        step.effective_filesystem_policy_source = Some(source);
    }
    Ok(())
}

fn cap_workspace_access(
    workspace_read_only: bool,
    mut policy: transist_protocol::FilesystemPolicy,
) -> transist_protocol::FilesystemPolicy {
    if workspace_read_only {
        policy.workspace_access = Some(transist_protocol::WorkspaceAccessPolicy {
            mode: transist_protocol::WorkspaceAccessMode::ReadOnly,
        });
    }
    policy
}

/// Resolves and stores the effective network policy for every step (and
/// cleanup step) in the recipe by merging global, workflow, and step-level
/// policies. Called once at prepare/start time so the persisted recipe snapshot
/// carries the effective policy for resume and replay.
pub(crate) fn resolve_recipe_network_policies(
    home_dir: &Path,
    settings_port: &dyn crate::ports::HomeSettingsPort,
    recipe: &mut Recipe,
) -> Result<()> {
    let needs_resolution = recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|c| &c.step))
        .any(|step| step.effective_network_policy_source.is_none());
    if !needs_resolution {
        return Ok(());
    }
    let global = settings_port.load_settings(home_dir)?.network_policy;
    let workflow = recipe.network_policy.as_ref();
    for step in &mut recipe.steps {
        if step.effective_network_policy_source.is_some() {
            continue;
        }
        let (effective, source) = transist_protocol::NetworkPolicy::resolve(
            global.as_ref(),
            workflow,
            step.network_policy.as_ref(),
        );
        step.effective_network_policy = effective;
        step.effective_network_policy_source = Some(source);
    }
    for cleanup in &mut recipe.cleanup {
        let step = &mut cleanup.step;
        if step.effective_network_policy_source.is_some() {
            continue;
        }
        let (effective, source) = transist_protocol::NetworkPolicy::resolve(
            global.as_ref(),
            workflow,
            step.network_policy.as_ref(),
        );
        step.effective_network_policy = effective;
        step.effective_network_policy_source = Some(source);
    }
    Ok(())
}

/// Resolves every authored field that can contribute to capability generation
/// using immutable run inputs. The same helper is the extension point for new
/// authority-bearing fields; ordinary action data continues to use the normal
/// mutable input/variable resolver.
pub(crate) fn resolve_recipe_authority_fields(
    recipe: &mut Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Result<()> {
    fn resolve_policy<T: serde::Serialize + for<'de> serde::Deserialize<'de>>(
        policy: &mut Option<T>,
        inputs: &BTreeMap<String, KernelValue>,
    ) -> Result<()> {
        let Some(current) = policy.as_ref() else {
            return Ok(());
        };
        let value = KernelValue::from_json(serde_json::to_value(current)?);
        let resolved = resolve_authority_value(&value, inputs)?;
        *policy = Some(serde_json::from_value(serde_json::to_value(resolved)?)?);
        Ok(())
    }

    fn resolve_filesystem_policy(
        policy: &mut Option<transist_protocol::FilesystemPolicy>,
        inputs: &BTreeMap<String, KernelValue>,
    ) -> Result<()> {
        let Some(current) = policy.as_mut() else {
            return Ok(());
        };
        for path in &mut current.read_allow_paths {
            if let Some(name) = exact_authority_input_reference(path)
                && matches!(inputs.get(name), Some(KernelValue::Null))
            {
                path.clear();
                continue;
            }
            let resolved = resolve_authority_value(&KernelValue::String(path.clone()), inputs)?;
            let KernelValue::String(resolved) = resolved else {
                bail!("filesystem read_allow_paths entries must resolve to strings");
            };
            *path = resolved;
        }
        current.read_allow_paths.retain(|path| !path.is_empty());
        let value = KernelValue::from_json(serde_json::to_value(&*current)?);
        let resolved = resolve_authority_value(&value, inputs)?;
        *current = serde_json::from_value(serde_json::to_value(resolved)?)?;
        Ok(())
    }

    resolve_filesystem_policy(&mut recipe.filesystem_policy, workflow_inputs)?;
    resolve_policy(&mut recipe.network_policy, workflow_inputs)?;
    resolve_policy(&mut recipe.workspace, workflow_inputs)?;
    for step in &mut recipe.steps {
        resolve_filesystem_policy(&mut step.filesystem_policy, workflow_inputs)?;
        resolve_policy(&mut step.network_policy, workflow_inputs)?;
    }
    for cleanup in &mut recipe.cleanup {
        resolve_filesystem_policy(&mut cleanup.step.filesystem_policy, workflow_inputs)?;
        resolve_policy(&mut cleanup.step.network_policy, workflow_inputs)?;
    }
    Ok(())
}

/// Resolves a recipe's declared workspace or preserves the legacy CLI-root
/// fallback when no workspace was authored.
pub(crate) fn resolve_recipe_workspace(
    home_dir: &Path,
    legacy_repo_root: &Path,
    recipe: &mut Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Result<PathBuf> {
    resolve_recipe_authority_fields(recipe, workflow_inputs)?;
    let Some(workspace) = recipe.workspace.as_mut() else {
        return std::fs::canonicalize(legacy_repo_root).with_context(|| {
            format!(
                "failed canonicalizing legacy workspace '{}'",
                legacy_repo_root.display()
            )
        });
    };
    if workspace.path.trim() != workspace.path || !Path::new(&workspace.path).is_absolute() {
        bail!("workspace.path must resolve to an absolute, unpadded path");
    }
    if workspace.access == transist_protocol::WorkspaceAccessMode::Custom {
        bail!("workspace.access must be 'full' or 'read_only'");
    }
    let canonical = std::fs::canonicalize(&workspace.path)
        .with_context(|| format!("workspace.path '{}' does not exist", workspace.path))?;
    if !canonical.is_dir() {
        bail!(
            "workspace.path '{}' is not a directory",
            canonical.display()
        );
    }
    let home = std::fs::canonicalize(home_dir).with_context(|| {
        format!(
            "failed canonicalizing Transist home '{}'",
            home_dir.display()
        )
    })?;
    if canonical == home || canonical.starts_with(&home) || home.starts_with(&canonical) {
        bail!(
            "workspace.path '{}' overlaps Transist home '{}'",
            canonical.display(),
            home.display()
        );
    }
    workspace.path = canonical.display().to_string();
    Ok(canonical)
}

/// Recanonicalizes the workspace persisted at run birth, retaining the legacy
/// caller-root fallback only for records that predate declared workspaces.
pub(crate) fn persisted_workspace_or_legacy(
    home_dir: &Path,
    legacy_repo_root: &Path,
    record: &PersistedRunRecord,
) -> Result<PathBuf> {
    crate::app::run_persistence::persisted_workspace_path(
        home_dir,
        legacy_repo_root,
        record.workspace.as_ref(),
    )
}

/// Materializes the complete capability request set into the recipe snapshot.
///
/// The request is derived only from pinned recipe data and effective policy,
/// so re-entry paths evaluate the same authority request as the initial run.
pub(crate) fn populate_recipe_capabilities(
    home_dir: &Path,
    repo_root: &Path,
    recipe: &mut Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    local_only_mode: bool,
    settings_port: &dyn crate::ports::HomeSettingsPort,
    plugin_metadata: &dyn crate::ports::PluginMetadataPort,
) -> Result<()> {
    populate_recipe_capabilities_with_settings(
        home_dir,
        repo_root,
        recipe,
        workflow_inputs,
        local_only_mode,
        settings_port,
        plugin_metadata,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn populate_recipe_capabilities_with_settings(
    home_dir: &Path,
    repo_root: &Path,
    recipe: &mut Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    local_only_mode: bool,
    settings_port: &dyn crate::ports::HomeSettingsPort,
    plugin_metadata: &dyn crate::ports::PluginMetadataPort,
) -> Result<()> {
    populate_recipe_capabilities_in_scope(
        home_dir,
        repo_root,
        recipe,
        workflow_inputs,
        local_only_mode,
        settings_port,
        plugin_metadata,
        &BTreeSet::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn populate_recipe_capabilities_in_scope(
    home_dir: &Path,
    repo_root: &Path,
    recipe: &mut Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    local_only_mode: bool,
    settings_port: &dyn crate::ports::HomeSettingsPort,
    plugin_metadata: &dyn crate::ports::PluginMetadataPort,
    deferred_names: &BTreeSet<String>,
) -> Result<()> {
    resolve_recipe_authority_fields(recipe, workflow_inputs)?;
    let mut capabilities = BTreeMap::new();
    for index in 0..recipe.steps.len() {
        let step = recipe.steps[index].clone();
        let projection = authority_projection_for_recipe_step(
            home_dir,
            repo_root,
            recipe,
            &step,
            workflow_inputs,
            local_only_mode,
            plugin_metadata,
        )?;
        if !projection.requests.is_empty() {
            capabilities.insert(step.id.clone(), projection.requests);
        }
        if let StepExecutionDescriptor::Subflow { recipe: child } = &step.execution {
            let mut child = child.clone();
            let child_scope = match bind_deferred_subflow_inputs_in_scope(
                &child,
                &step.with,
                workflow_inputs,
                &BTreeMap::new(),
                deferred_names,
            ) {
                Ok(bound_child_inputs) => bound_child_inputs,
                Err(error) if !authority_input_names(&child).is_empty() => {
                    return Err(anyhow::Error::new(ChildAuthorityPreparationError {
                        parent_step_id: step.id.clone(),
                        child_workflow_ref: format!(
                            "{}@{}",
                            child.workflow.id, child.workflow.version
                        ),
                        message: error.to_string(),
                    }));
                }
                Err(_) => {
                    // Preflight owns the structured child-input diagnostic. The
                    // parent still receives a complete projection for every
                    // child that can be prepared, rather than silently treating
                    // a failed child as authority-free.
                    continue;
                }
            };
            let bound_child_inputs = child_scope.values;
            resolve_recipe_authority_fields(&mut child, &bound_child_inputs).map_err(|error| {
                anyhow::Error::new(ChildAuthorityPreparationError {
                    parent_step_id: step.id.clone(),
                    child_workflow_ref: format!("{}@{}", child.workflow.id, child.workflow.version),
                    message: error.to_string(),
                })
            })?;
            let inherited_access =
                effective_subflow_workspace_access(recipe, &step).map_err(|error| {
                    anyhow::Error::new(ChildAuthorityPreparationError {
                        parent_step_id: step.id.clone(),
                        child_workflow_ref: format!(
                            "{}@{}",
                            child.workflow.id, child.workflow.version
                        ),
                        message: error.to_string(),
                    })
                })?;
            inherit_subflow_workspace(
                home_dir,
                repo_root,
                recipe.workspace.is_some(),
                inherited_access,
                &mut child,
                &bound_child_inputs,
            )
            .map_err(|error| {
                anyhow::Error::new(ChildAuthorityPreparationError {
                    parent_step_id: step.id.clone(),
                    child_workflow_ref: format!("{}@{}", child.workflow.id, child.workflow.version),
                    message: error.to_string(),
                })
            })?;
            resolve_recipe_filesystem_policies(home_dir, settings_port, &mut child)?;
            resolve_recipe_network_policies(home_dir, settings_port, &mut child)?;
            populate_recipe_capabilities_in_scope(
                home_dir,
                repo_root,
                &mut child,
                &bound_child_inputs,
                local_only_mode,
                settings_port,
                plugin_metadata,
                &child_scope.deferred_names,
            )
            .map_err(|error| {
                if error
                    .downcast_ref::<ChildAuthorityPreparationError>()
                    .is_some()
                {
                    error
                } else {
                    anyhow::Error::new(ChildAuthorityPreparationError {
                        parent_step_id: step.id.clone(),
                        child_workflow_ref: format!(
                            "{}@{}",
                            child.workflow.id, child.workflow.version
                        ),
                        message: error.to_string(),
                    })
                }
            })?;
            if let StepExecutionDescriptor::Subflow { recipe: persisted } =
                &mut recipe.steps[index].execution
            {
                *persisted = child.clone();
            }
            for (step_id, requests) in &child.capabilities {
                capabilities.insert(format!("{}/{}", step.id, step_id), requests.clone());
            }
        }
    }
    for cleanup in &recipe.cleanup {
        let projection = authority_projection_for_recipe_step(
            home_dir,
            repo_root,
            recipe,
            &cleanup.step,
            workflow_inputs,
            local_only_mode,
            plugin_metadata,
        )?;
        if !projection.requests.is_empty() {
            capabilities.insert(cleanup.step.id.clone(), projection.requests);
        }
    }
    recipe.capabilities = capabilities;
    recipe.capabilities_persisted = true;
    Ok(())
}

fn authority_projection_for_recipe_step(
    home_dir: &Path,
    repo_root: &Path,
    recipe: &Recipe,
    step: &transist_protocol::RecipeStep,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    local_only_mode: bool,
    plugin_metadata: &dyn crate::ports::PluginMetadataPort,
) -> Result<CapabilityProjection> {
    let documents = transist_protocol::step_document_bindings(step).map_err(anyhow::Error::msg)?;
    if !documents.is_empty()
        && !step
            .with
            .contains_key(transist_protocol::DOCUMENT_BINDINGS_KEY)
    {
        bail!("$document bindings require explicit compiled metadata");
    }
    for location in documents {
        if !recipe.documents.contains_key(&location.binding.document) {
            bail!("undeclared $document '{}'", location.binding.document);
        }
    }
    let mut sources = Vec::new();
    collect_authority_sources(recipe, step, &mut sources)?;
    reject_mutable_authority_sources(&sources, &step.id)?;
    match &step.execution {
        StepExecutionDescriptor::Invoke {
            adapter,
            plugin_id,
            plugin,
            action,
            plugin_version,
            ..
        } => {
            let (_resolved_with, secret_refs) = resolve_authority_with(step, workflow_inputs)?;
            crate::dispatch::validate_secret_refs_against_action(
                step,
                plugin_id.as_deref().unwrap_or(plugin),
                plugin_version.as_deref(),
                action,
                &secret_refs,
                plugin_metadata,
            )?;
            let provider = crate::dispatch::resolve_effective_provider_kind(
                plugin_metadata,
                plugin_id.as_deref().unwrap_or(plugin),
                plugin_version.as_deref(),
                action,
            );
            let (network_policy, _) = crate::dispatch::effective_network_policy_for_dispatch(
                *adapter,
                &provider,
                step.effective_network_policy.clone(),
                step.effective_network_policy_source
                    .unwrap_or(NetworkPolicySource::Builtin),
                local_only_mode,
            );
            let network_origins =
                if network_policy.mode != transist_protocol::NetworkAccessMode::None {
                    recipe_network_origins(
                        plugin_id.as_deref().unwrap_or(plugin),
                        plugin_version.as_deref(),
                        &network_policy,
                        plugin_metadata,
                    )?
                } else {
                    Vec::new()
                };
            let requests = transist_protocol::invoke_capabilities(
                *adapter,
                plugin_id.as_deref().unwrap_or(plugin),
                plugin_version.as_deref(),
                action,
                provider.as_str(),
                &secret_refs,
                &step.effective_filesystem_policy,
                repo_root,
                home_dir,
                &network_origins,
            )
            .map_err(|error| anyhow::anyhow!("invalid recipe capability request: {error}"))?;
            Ok(CapabilityProjection { requests, sources })
        }
        StepExecutionDescriptor::Control { primitive }
            if primitive == crate::ControlStepKind::GateManual.as_wire_str() =>
        {
            let gate_id = step
                .with
                .get("gate_id")
                .and_then(|value| match value {
                    KernelValue::String(value) if !value.trim().is_empty() => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| step.id.clone());
            let resource = format!("{}/{}", recipe.workflow.id, gate_id);
            let request = transist_protocol::CapabilityRequest::new(
                transist_protocol::CapabilityName::Approval,
                resource,
                ["request"],
            )?;
            let decide = transist_protocol::CapabilityRequest::new(
                transist_protocol::CapabilityName::Approval,
                format!("{}/{}", recipe.workflow.id, gate_id),
                ["decide"],
            )?;
            Ok(CapabilityProjection {
                requests: vec![request, decide],
                sources,
            })
        }
        _ => Ok(CapabilityProjection {
            requests: Vec::new(),
            sources,
        }),
    }
}

fn collect_authority_sources(
    recipe: &Recipe,
    step: &transist_protocol::RecipeStep,
    sources: &mut Vec<CapabilitySource>,
) -> Result<()> {
    if let Some(secret_refs) = step.with.get("secret_refs") {
        let dependencies =
            authority_dependencies(secret_refs, &format!("steps.{}.with.secret_refs", step.id))?;
        sources.push(CapabilitySource {
            field: format!("steps.{}.with.secret_refs", step.id),
            dependencies,
        });
    }
    if let Some(policy) = step.filesystem_policy.as_ref() {
        let value = KernelValue::from_json(serde_json::to_value(policy)?);
        sources.push(CapabilitySource {
            field: format!("steps.{}.filesystem_policy", step.id),
            dependencies: authority_dependencies(
                &value,
                &format!("steps.{}.filesystem_policy", step.id),
            )?,
        });
    }
    if let Some(policy) = step.network_policy.as_ref() {
        let value = KernelValue::from_json(serde_json::to_value(policy)?);
        sources.push(CapabilitySource {
            field: format!("steps.{}.network_policy", step.id),
            dependencies: authority_dependencies(
                &value,
                &format!("steps.{}.network_policy", step.id),
            )?,
        });
    }
    if let Some(policy) = recipe.filesystem_policy.as_ref() {
        let value = KernelValue::from_json(serde_json::to_value(policy)?);
        sources.push(CapabilitySource {
            field: "workflow.filesystem_policy".to_string(),
            dependencies: authority_dependencies(&value, "workflow.filesystem_policy")?,
        });
    }
    if let Some(policy) = recipe.network_policy.as_ref() {
        let value = KernelValue::from_json(serde_json::to_value(policy)?);
        sources.push(CapabilitySource {
            field: "workflow.network_policy".to_string(),
            dependencies: authority_dependencies(&value, "workflow.network_policy")?,
        });
    }
    if let Some(workspace) = recipe.workspace.as_ref() {
        let value = KernelValue::String(workspace.path.clone());
        sources.push(CapabilitySource {
            field: "workspace.path".to_string(),
            dependencies: authority_dependencies(&value, "workspace.path")?,
        });
    }
    Ok(())
}

fn authority_dependencies(
    value: &KernelValue,
    field: &str,
) -> Result<Vec<CapabilitySourceDependency>> {
    let mut dependencies = Vec::new();
    collect_authority_dependencies(value, field, &mut dependencies)?;
    dependencies.sort();
    dependencies.dedup();
    Ok(dependencies)
}

fn collect_authority_dependencies(
    value: &KernelValue,
    field: &str,
    dependencies: &mut Vec<CapabilitySourceDependency>,
) -> Result<()> {
    match value {
        KernelValue::String(raw) => {
            if !raw.contains("{{") && !raw.contains("}}") {
                dependencies.push(CapabilitySourceDependency::Literal);
                return Ok(());
            }
            let mut rest = raw.as_str();
            let mut found = false;
            while let Some(start) = rest.find("{{") {
                found = true;
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else {
                    dependencies.push(CapabilitySourceDependency::PostBirth {
                        reference: format!("{field}: malformed template"),
                    });
                    return Ok(());
                };
                let expression = after[..end].trim();
                if let Some(name) = expression.strip_prefix("inputs.") {
                    let name = name.trim();
                    if name.is_empty() {
                        dependencies.push(CapabilitySourceDependency::PostBirth {
                            reference: format!("{field}: empty input"),
                        });
                    } else {
                        dependencies.push(CapabilitySourceDependency::WorkflowInput {
                            name: name.to_string(),
                        });
                    }
                } else if let Some(name) = expression.strip_prefix("vars.") {
                    dependencies.push(CapabilitySourceDependency::WorkflowVariable {
                        name: name.trim().to_string(),
                    });
                } else if let Some(reference) = expression.strip_prefix("steps.") {
                    dependencies.push(CapabilitySourceDependency::StepOutput {
                        reference: reference.trim().to_string(),
                    });
                } else {
                    dependencies.push(CapabilitySourceDependency::PostBirth {
                        reference: expression.to_string(),
                    });
                }
                rest = &after[end + 2..];
            }
            if !found || !rest.is_empty() && rest.contains("{{") {
                dependencies.push(CapabilitySourceDependency::PostBirth {
                    reference: format!("{field}: malformed template"),
                });
            }
        }
        KernelValue::Array(values) => {
            for value in values {
                collect_authority_dependencies(value, field, dependencies)?;
            }
        }
        KernelValue::Object(values) => {
            for value in values.values() {
                collect_authority_dependencies(value, field, dependencies)?;
            }
        }
        _ => dependencies.push(CapabilitySourceDependency::Literal),
    }
    Ok(())
}

fn reject_mutable_authority_sources(sources: &[CapabilitySource], step_id: &str) -> Result<()> {
    for source in sources {
        for dependency in &source.dependencies {
            if !matches!(
                dependency,
                CapabilitySourceDependency::Literal
                    | CapabilitySourceDependency::WorkflowInput { .. }
            ) {
                bail!(
                    "step '{}' authority field '{}' depends on a post-birth value: {:?}",
                    step_id,
                    source.field,
                    dependency
                );
            }
        }
    }
    Ok(())
}

fn authority_input_names(recipe: &Recipe) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(policy) = recipe.filesystem_policy.as_ref() {
        if let Ok(value) = serde_json::to_value(policy) {
            collect_authority_input_names(&KernelValue::from_json(value), &mut names);
        }
    }
    if let Some(policy) = recipe.network_policy.as_ref() {
        if let Ok(value) = serde_json::to_value(policy) {
            collect_authority_input_names(&KernelValue::from_json(value), &mut names);
        }
    }
    if let Some(workspace) = recipe.workspace.as_ref()
        && let Ok(value) = serde_json::to_value(workspace)
    {
        collect_authority_input_names(&KernelValue::from_json(value), &mut names);
    }
    for step in recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|item| &item.step))
    {
        if let Some(transist_protocol::RecipePromptBinding {
            source: transist_protocol::RecipePromptSource::File { path },
            ..
        }) = &step.prompt_binding
        {
            collect_authority_input_names(&KernelValue::String(path.clone()), &mut names);
        }
        for (key, value) in &step.with {
            if !key.starts_with("__transist_") {
                collect_source_asset_input_names(value, &mut names);
            }
        }
        if let Some(value) = step.with.get("secret_refs") {
            collect_authority_input_names(value, &mut names);
        }
        if let Some(policy) = step.filesystem_policy.as_ref() {
            if let Ok(value) = serde_json::to_value(policy) {
                collect_authority_input_names(&KernelValue::from_json(value), &mut names);
            }
        }
        if let Some(policy) = step.network_policy.as_ref() {
            if let Ok(value) = serde_json::to_value(policy) {
                collect_authority_input_names(&KernelValue::from_json(value), &mut names);
            }
        }
        if let StepExecutionDescriptor::Subflow { recipe: child } = &step.execution {
            for name in authority_input_names(child) {
                if let Some(value) = step.with.get(&name) {
                    collect_authority_input_names(value, &mut names);
                }
            }
        }
    }
    names
}

fn collect_source_asset_input_names(value: &KernelValue, names: &mut BTreeSet<String>) {
    match value {
        KernelValue::String(raw) if raw.starts_with("source://") => {
            collect_authority_input_names(value, names)
        }
        KernelValue::Array(values) => {
            for v in values {
                collect_source_asset_input_names(v, names);
            }
        }
        KernelValue::Object(values) => {
            for v in values.values() {
                collect_source_asset_input_names(v, names);
            }
        }
        _ => {}
    }
}

fn collect_authority_input_names(value: &KernelValue, names: &mut BTreeSet<String>) {
    match value {
        KernelValue::String(raw) => {
            let mut rest = raw.as_str();
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else {
                    break;
                };
                let expression = after[..end].trim();
                if let Some(name) = expression.strip_prefix("inputs.") {
                    if !name.trim().is_empty() {
                        names.insert(name.trim().to_string());
                    }
                }
                rest = &after[end + 2..];
            }
        }
        KernelValue::Array(values) => {
            for value in values {
                collect_authority_input_names(value, names);
            }
        }
        KernelValue::Object(values) => {
            for value in values.values() {
                collect_authority_input_names(value, names);
            }
        }
        _ => {}
    }
}

fn resolve_authority_with(
    step: &transist_protocol::RecipeStep,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Result<(BTreeMap<String, KernelValue>, Vec<String>)> {
    let mut resolved = step.with.clone();
    let secret_refs = match step.with.get("secret_refs") {
        None => Vec::new(),
        Some(value) => {
            let KernelValue::Array(values) = value else {
                bail!(
                    "step '{}' authority field 'with.secret_refs' must be an array",
                    step.id
                );
            };
            let mut refs = Vec::new();
            let mut resolved_values = Vec::new();
            for value in values {
                let resolved_value = resolve_authority_value(value, workflow_inputs)?;
                let KernelValue::String(secret) = &resolved_value else {
                    bail!(
                        "step '{}' secret_refs entries must resolve to strings",
                        step.id
                    );
                };
                if secret.trim().is_empty() {
                    bail!("step '{}' secret_refs entries cannot be empty", step.id);
                }
                refs.push(secret.clone());
                resolved_values.push(resolved_value);
            }
            resolved.insert(
                "secret_refs".to_string(),
                KernelValue::Array(resolved_values),
            );
            refs
        }
    };
    Ok((resolved, secret_refs))
}

fn resolve_authority_value(
    value: &KernelValue,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> Result<KernelValue> {
    match value {
        KernelValue::String(raw) => {
            if let Some(inner) = raw
                .trim()
                .strip_prefix("{{")
                .and_then(|v| v.strip_suffix("}}"))
            {
                let expression = inner.trim();
                if let Some(name) = expression.strip_prefix("inputs.") {
                    return Ok(KernelValue::String(authority_input_to_string(
                        workflow_inputs.get(name.trim()),
                        name.trim(),
                    )?));
                }
            }
            if !raw.contains("{{") && !raw.contains("}}") {
                return Ok(KernelValue::String(raw.clone()));
            }
            let mut output = String::new();
            let mut rest = raw.as_str();
            while let Some(start) = rest.find("{{") {
                output.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else {
                    bail!("malformed authority template");
                };
                let expression = after[..end].trim();
                let Some(name) = expression.strip_prefix("inputs.") else {
                    bail!("authority fields may depend only on workflow inputs");
                };
                output.push_str(&authority_input_to_string(
                    workflow_inputs.get(name.trim()),
                    name.trim(),
                )?);
                rest = &after[end + 2..];
            }
            if rest.contains("{{") || rest.contains("}}") {
                bail!("malformed authority template");
            }
            output.push_str(rest);
            Ok(KernelValue::String(output))
        }
        KernelValue::Array(values) => values
            .iter()
            .map(|value| resolve_authority_value(value, workflow_inputs))
            .collect::<Result<Vec<_>>>()
            .map(KernelValue::Array),
        KernelValue::Object(values) => values
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    resolve_authority_value(value, workflow_inputs)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()
            .map(KernelValue::Object),
        _ => Ok(value.clone()),
    }
}

fn exact_authority_input_reference(value: &str) -> Option<&str> {
    let name = value.strip_prefix("{{inputs.")?.strip_suffix("}}")?;
    (!name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(name)
}

fn inherit_subflow_workspace(
    home_dir: &Path,
    parent_workspace: &Path,
    parent_workspace_declared: bool,
    inherited_access: transist_protocol::WorkspaceAccessMode,
    child: &mut Recipe,
    child_inputs: &BTreeMap<String, KernelValue>,
) -> Result<()> {
    let parent_workspace = std::fs::canonicalize(parent_workspace).with_context(|| {
        format!(
            "failed canonicalizing parent workspace '{}'",
            parent_workspace.display()
        )
    })?;
    narrow_workspace_access(
        inherited_access,
        transist_protocol::WorkspaceAccessMode::Full,
    )?;
    if child.workspace.is_none() && parent_workspace_declared {
        child.workspace = Some(transist_protocol::WorkspaceBinding {
            path: parent_workspace.display().to_string(),
            access: inherited_access,
        });
    } else if child.workspace.is_some() {
        let parent_path = parent_workspace.display().to_string();
        let child_workspace = child.workspace.as_ref().expect("workspace present");
        if child_workspace.path != parent_path {
            let child_workspace =
                resolve_recipe_workspace(home_dir, &parent_workspace, child, child_inputs)?;
            if child_workspace != parent_workspace {
                bail!(
                    "subflow workspace '{}' must equal parent workspace '{}'",
                    child_workspace.display(),
                    parent_workspace.display()
                );
            }
        }
        let child_access = child.workspace.as_ref().expect("workspace resolved").access;
        child.workspace.as_mut().expect("workspace resolved").access =
            narrow_workspace_access(inherited_access, child_access)?;
    }

    let effective_access = child
        .workspace
        .as_ref()
        .map_or(inherited_access, |workspace| workspace.access);
    if effective_access == transist_protocol::WorkspaceAccessMode::ReadOnly {
        for step in child
            .steps
            .iter_mut()
            .chain(child.cleanup.iter_mut().map(|cleanup| &mut cleanup.step))
        {
            step.effective_filesystem_policy =
                cap_workspace_access(true, step.effective_filesystem_policy.clone());
        }
    }
    Ok(())
}

fn effective_subflow_workspace_access(
    parent_recipe: &Recipe,
    subflow_step: &transist_protocol::RecipeStep,
) -> Result<transist_protocol::WorkspaceAccessMode> {
    let binding_access = parent_recipe
        .workspace
        .as_ref()
        .map_or(transist_protocol::WorkspaceAccessMode::Full, |binding| {
            binding.access
        });
    let policy_access = subflow_step
        .effective_filesystem_policy
        .workspace_access
        .as_ref()
        .map_or(transist_protocol::WorkspaceAccessMode::Full, |access| {
            access.mode
        });
    narrow_workspace_access(binding_access, policy_access)
}

fn narrow_workspace_access(
    left: transist_protocol::WorkspaceAccessMode,
    right: transist_protocol::WorkspaceAccessMode,
) -> Result<transist_protocol::WorkspaceAccessMode> {
    use transist_protocol::WorkspaceAccessMode::{Custom, Full, ReadOnly};
    match (left, right) {
        (Custom, _) | (_, Custom) => {
            bail!("subflow workspace access must be 'full' or 'read_only'")
        }
        (ReadOnly, _) | (_, ReadOnly) => Ok(ReadOnly),
        (Full, Full) => Ok(Full),
    }
}

fn authority_input_to_string(value: Option<&KernelValue>, name: &str) -> Result<String> {
    let value = value.ok_or_else(|| anyhow::anyhow!("workflow input '{}' is not bound", name))?;
    match value {
        KernelValue::String(value) => {
            if !transist_protocol::collect_deferred_bindings(
                &KernelValue::String(value.clone()),
                "",
            )
            .map_err(anyhow::Error::msg)?
            .is_empty()
            {
                bail!("authority input '{name}' depends on a post-birth value");
            }
            Ok(value.clone())
        }
        KernelValue::Bool(value) => Ok(value.to_string()),
        KernelValue::Number(value) => Ok(value.to_string()),
        KernelValue::Null => bail!("workflow input '{}' is null", name),
        KernelValue::Array(_) | KernelValue::Object(_) => {
            bail!(
                "workflow input '{}' must be scalar in an authority field",
                name
            )
        }
    }
}

/// Returns the exact persisted authority subset for one execution checkpoint.
///
/// This is the sole enforcement source for dispatch, gate decisions, and any
/// other checkpoint that consults persisted authority: callers must not
/// regenerate a capability request from live recipe/step data and must
/// treat a missing, malformed, or operation-mismatched persisted subset as a
/// denial rather than falling back to a freshly derived request.
pub fn persisted_capabilities_for_step(
    recipe: &Recipe,
    step_id: &str,
    operation: Option<&str>,
) -> Result<Vec<CapabilityRequest>> {
    if !recipe.capabilities_persisted {
        // Legacy/synthetic records are handled by the compatibility path. A
        // current run is rejected earlier by `validate_persisted_authority`.
        return Ok(Vec::new());
    }
    let requests = recipe.capabilities.get(step_id).ok_or_else(|| {
        anyhow::anyhow!(
            "persisted authority is missing for checkpoint '{}'; execution is denied",
            step_id
        )
    })?;
    let mut selected = requests.clone();
    if let Some(operation) = operation {
        selected.retain(|request| {
            request.capability == CapabilityName::Approval
                && request.operations.len() == 1
                && request.operations[0] == operation
        });
        if selected.len() != 1 {
            bail!(
                "persisted authority for checkpoint '{}' does not contain exactly one approval '{}' request",
                step_id,
                operation
            );
        }
    }
    if selected.is_empty() {
        bail!("persisted authority for checkpoint '{}' is empty", step_id);
    }
    Ok(selected)
}

/// Verifies that current runtime inputs still produce the persisted authority.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_persisted_authority(
    home_dir: &Path,
    repo_root: &Path,
    recipe: &Recipe,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    local_only_mode: bool,
    require_materialized: bool,
    settings_port: &dyn crate::ports::HomeSettingsPort,
    plugin_metadata: &dyn crate::ports::PluginMetadataPort,
) -> Result<()> {
    let mut current = recipe.clone();
    if require_materialized && !recipe.capabilities_persisted {
        bail!(
            "current run is missing its persisted capability authority projection; replay is required"
        );
    }
    resolve_recipe_authority_fields(&mut current, workflow_inputs)?;
    let execution_workspace =
        resolve_recipe_workspace(home_dir, repo_root, &mut current, workflow_inputs)?;
    populate_recipe_capabilities(
        home_dir,
        &execution_workspace,
        &mut current,
        workflow_inputs,
        local_only_mode,
        settings_port,
        plugin_metadata,
    )?;
    if recipe.capabilities_persisted && current.capabilities != recipe.capabilities {
        bail!(
            "persisted authority does not match the current recipe projection; replay is required"
        );
    }
    Ok(())
}

pub(crate) fn recipe_network_origins(
    plugin_id: &str,
    plugin_version: Option<&str>,
    policy: &transist_protocol::NetworkPolicy,
    plugin_metadata: &dyn crate::ports::PluginMetadataPort,
) -> Result<Vec<String>> {
    let base_url = plugin_version
        .filter(|version| !version.trim().is_empty())
        .map(|version| plugin_metadata.resolve_network_origin(plugin_id, version))
        .transpose()?
        .flatten();
    transist_protocol::network_origins(policy, base_url.as_deref())
        .map_err(|error| anyhow::anyhow!("invalid generated network origin: {error}"))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn sorted_json_object_keys(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    object.keys().cloned().collect()
}

pub(crate) fn runtime_blob_ref_summary(file_ref: &FileRef, family_dir: &Path) -> serde_json::Value {
    let path = Path::new(&file_ref.path);
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&file_ref.path)
        .to_string();
    let relative_path = path
        .strip_prefix(family_dir)
        .ok()
        .map(|value| value.to_string_lossy().to_string());
    let mut summary = serde_json::Map::from_iter([
        ("name".to_string(), serde_json::Value::String(name)),
        (
            "path".to_string(),
            serde_json::Value::String(file_ref.path.clone()),
        ),
        ("bytes".to_string(), serde_json::Value::from(file_ref.bytes)),
    ]);
    if let Some(relative_path) = relative_path {
        summary.insert(
            "relative_path".to_string(),
            serde_json::Value::String(relative_path),
        );
    }
    if !file_ref.sha256.trim().is_empty() {
        summary.insert(
            "sha256".to_string(),
            serde_json::Value::String(file_ref.sha256.clone()),
        );
    }
    serde_json::Value::Object(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{ExecutionRequestContext, make_execution_request};
    use crate::ports::HomeSettingsPort;
    use transist_protocol::HomeSettings;
    use transist_protocol::{AdapterKind, RecipeStep, StepExecutionDescriptor, WorkflowRef};

    struct DefaultHomeSettings;

    impl HomeSettingsPort for DefaultHomeSettings {
        fn load_settings(&self, _home_dir: &Path) -> Result<HomeSettings> {
            Ok(HomeSettings::default())
        }
    }

    struct NetworkNoneHomeSettings;

    impl HomeSettingsPort for NetworkNoneHomeSettings {
        fn load_settings(&self, _home_dir: &Path) -> Result<HomeSettings> {
            Ok(HomeSettings {
                network_policy: Some(transist_protocol::NetworkPolicy {
                    mode: transist_protocol::NetworkAccessMode::None,
                    allow_hosts: Vec::new(),
                    allow_ports: Vec::new(),
                    tls_required: true,
                    max_redirects: 0,
                }),
                ..HomeSettings::default()
            })
        }
    }

    #[test]
    fn persisted_capabilities_match_dispatch_capabilities() {
        let temp = tempfile::tempdir().expect("temp home");
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("shell".to_string()),
                plugin: "shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(
                transist_protocol::FilesystemPolicySource::Builtin,
            ),
            effective_network_policy: Default::default(),
            effective_network_policy_source: Some(transist_protocol::NetworkPolicySource::Builtin),
        };
        let mut recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "demo".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps: vec![step],
            cleanup: Vec::new(),
            graph: None,
            capabilities: BTreeMap::new(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        populate_recipe_capabilities(
            temp.path(),
            repo_root,
            &mut recipe,
            &BTreeMap::new(),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("populate capabilities");
        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root,
                home_dir: temp.path(),
                recipe_source_dir: None,
                run_id: "run-parity",
                workflow_id: "demo",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke-parity",
                local_only_mode: false,
            },
            &recipe.steps[0],
        )
        .expect("prepare request")
        .expect("invoke request");
        assert_eq!(
            recipe.capabilities.get("invoke"),
            Some(&prepared.request.requested_capabilities)
        );
    }

    #[test]
    fn static_workflow_validation_materializes_without_creating_run_state() {
        let home = tempfile::tempdir().expect("home");
        let workspace = tempfile::tempdir().expect("workspace");
        let recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "static-demo".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps: vec![RecipeStep {
                id: "invoke".to_string(),
                execution: StepExecutionDescriptor::Invoke {
                    adapter: AdapterKind::Process,
                    plugin_id: Some("shell".to_string()),
                    plugin: "shell".to_string(),
                    action: "exec".to_string(),
                    execution_kind: "external".to_string(),
                    plugin_version: None,
                    plugin_sha256: None,
                },
                with: BTreeMap::new(),
                prompt_binding: None,
                skills_binding: None,
                vars: BTreeMap::new(),
                filesystem_policy: None,
                network_policy: None,
                effective_filesystem_policy: Default::default(),
                effective_filesystem_policy_source: None,
                effective_network_policy: Default::default(),
                effective_network_policy_source: None,
            }],
            cleanup: Vec::new(),
            graph: None,
            capabilities: BTreeMap::new(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        let before = std::fs::read_dir(home.path()).expect("read home").count();

        let bound = bind_and_materialize_recipe(
            home.path(),
            workspace.path(),
            &recipe,
            BTreeMap::new(),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("materialize bound workflow");

        assert_eq!(
            bound.workspace_dir,
            std::fs::canonicalize(workspace.path()).unwrap()
        );
        assert!(bound.recipe.capabilities.contains_key("invoke"));
        assert_eq!(
            std::fs::read_dir(home.path()).expect("read home").count(),
            before
        );
    }

    #[test]
    fn authority_projection_allows_immutable_input_secret_refs() {
        let temp = tempfile::tempdir().expect("temp home");
        let repo = tempfile::tempdir().expect("repo");
        let mut step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("shell".to_string()),
                plugin: "shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::from([(
                "secret_refs".to_string(),
                KernelValue::Array(vec![KernelValue::String(
                    "{{ inputs.secret_name }}".to_string(),
                )]),
            )]),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(
                transist_protocol::FilesystemPolicySource::Builtin,
            ),
            effective_network_policy: Default::default(),
            effective_network_policy_source: Some(transist_protocol::NetworkPolicySource::Builtin),
        };
        let mut recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "authority-input".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::from([(
                "secret_name".to_string(),
                transist_protocol::RecipeInputSpec {
                    value_type: "string".to_string(),
                    required: true,
                    default: None,
                    description: None,
                },
            )]),
            steps: vec![step.clone()],
            cleanup: Vec::new(),
            graph: None,
            capabilities: BTreeMap::new(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        let mut sources = Vec::new();
        collect_authority_sources(&recipe, &step, &mut sources).expect("authority sources");
        assert!(matches!(
            sources[0].dependencies.first(),
            Some(CapabilitySourceDependency::WorkflowInput { name }) if name == "secret_name"
        ));
        populate_recipe_capabilities(
            temp.path(),
            repo.path(),
            &mut recipe,
            &BTreeMap::from([(
                "secret_name".to_string(),
                KernelValue::String("API_TOKEN".to_string()),
            )]),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("input-bound authority");
        assert!(recipe.capabilities["invoke"].iter().any(|request| {
            request.capability == CapabilityName::Secret && request.resource == "API_TOKEN"
        }));
        step.with.insert(
            "argument".to_string(),
            KernelValue::String("{{vars.mutable}}".to_string()),
        );
        recipe.steps = vec![step];
        populate_recipe_capabilities(
            temp.path(),
            repo.path(),
            &mut recipe,
            &BTreeMap::from([(
                "secret_name".to_string(),
                KernelValue::String("API_TOKEN".to_string()),
            )]),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("ordinary mutable action data is allowed");
    }

    #[test]
    fn authority_projection_rejects_mutable_and_step_output_dependencies() {
        let temp = tempfile::tempdir().expect("temp home");
        let repo = tempfile::tempdir().expect("repo");
        for dependency in ["{{vars.secret}}", "{{steps.previous.out.secret}}"] {
            let mut recipe = test_authority_recipe(KernelValue::String(dependency.to_string()));
            let error = populate_recipe_capabilities(
                temp.path(),
                repo.path(),
                &mut recipe,
                &BTreeMap::new(),
                false,
                &DefaultHomeSettings,
                &crate::dispatch::DefaultTestPluginMetadata,
            )
            .expect_err("mutable authority dependency must fail before birth");
            assert!(error.to_string().contains("post-birth value"));
        }
    }

    #[test]
    fn child_authority_preparation_failure_is_structured_and_not_skipped() {
        let temp = tempfile::tempdir().expect("temp home");
        let repo = tempfile::tempdir().expect("repo");
        let mut child =
            test_authority_recipe(KernelValue::String("{{inputs.secret_name}}".to_string()));
        child.inputs.insert(
            "secret_name".to_string(),
            transist_protocol::RecipeInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: None,
            },
        );
        let parent_step = transist_protocol::RecipeStep {
            id: "child".to_string(),
            execution: StepExecutionDescriptor::Subflow {
                recipe: Box::new(child),
            },
            with: BTreeMap::from([(
                "secret_name".to_string(),
                KernelValue::String("{{vars.secret_name}}".to_string()),
            )]),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let mut parent = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        parent.steps = vec![parent_step];
        let error = populate_recipe_capabilities(
            temp.path(),
            repo.path(),
            &mut parent,
            &BTreeMap::new(),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect_err("child authority preparation must not be skipped");
        let structured = error
            .downcast_ref::<ChildAuthorityPreparationError>()
            .expect("structured child authority error");
        assert_eq!(structured.parent_step_id, "child");
        assert_eq!(structured.child_workflow_ref, "authority-test@v1");
        assert!(structured.message.contains("secret_name"));
        assert!(structured.message.contains("post-birth value"));
    }

    #[test]
    fn nested_capabilities_inherit_global_network_none_policy() {
        let temp = tempfile::tempdir().expect("temp home");
        let repo = tempfile::tempdir().expect("repo");
        let mut child = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        child.steps[0].effective_network_policy_source = None;
        let mut parent = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        parent.steps = vec![RecipeStep {
            id: "child".to_string(),
            execution: StepExecutionDescriptor::Subflow {
                recipe: Box::new(child),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }];

        resolve_recipe_network_policies(temp.path(), &NetworkNoneHomeSettings, &mut parent)
            .expect("resolve parent network policy");
        populate_recipe_capabilities(
            temp.path(),
            repo.path(),
            &mut parent,
            &BTreeMap::new(),
            false,
            &NetworkNoneHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("project nested capabilities");

        let StepExecutionDescriptor::Subflow { recipe: child } = &parent.steps[0].execution else {
            panic!("expected embedded child recipe");
        };
        assert_eq!(
            child.steps[0].effective_network_policy.mode,
            transist_protocol::NetworkAccessMode::None
        );
        assert_eq!(
            child.steps[0].effective_network_policy_source,
            Some(NetworkPolicySource::Global)
        );
        assert!(
            parent.capabilities["child/invoke"]
                .iter()
                .all(|request| request.capability != CapabilityName::Network)
        );
    }

    #[test]
    fn manual_gate_persists_request_and_decide_authority_separately() {
        let temp = tempfile::tempdir().expect("temp home");
        let repo = tempfile::tempdir().expect("repo");
        let mut recipe = test_gate_recipe();
        populate_recipe_capabilities(
            temp.path(),
            repo.path(),
            &mut recipe,
            &BTreeMap::new(),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("gate authority");
        let requests = &recipe.capabilities["review"];
        assert!(
            requests
                .iter()
                .any(|request| request.operations == ["request"])
        );
        assert!(
            requests
                .iter()
                .any(|request| request.operations == ["decide"])
        );
        let mut combined = recipe.clone();
        combined.capabilities.get_mut("review").unwrap().remove(1);
        combined.capabilities.get_mut("review").unwrap()[0]
            .operations
            .push("decide".to_string());
        assert!(persisted_capabilities_for_step(&combined, "review", Some("request")).is_err());
    }

    #[test]
    fn persisted_authority_rejects_removed_requests_and_workspace_drift() {
        let temp = tempfile::tempdir().expect("temp home");
        let repo_a = tempfile::tempdir().expect("repo a");
        let repo_b = tempfile::tempdir().expect("repo b");
        let mut recipe = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        populate_recipe_capabilities(
            temp.path(),
            repo_a.path(),
            &mut recipe,
            &BTreeMap::new(),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("authority");
        let mut removed = recipe.clone();
        removed
            .capabilities
            .get_mut("invoke")
            .expect("invoke authority")
            .pop();
        assert!(
            validate_persisted_authority(
                temp.path(),
                repo_a.path(),
                &removed,
                &BTreeMap::new(),
                false,
                false,
                &DefaultHomeSettings,
                &crate::dispatch::DefaultTestPluginMetadata,
            )
            .is_err()
        );
        assert!(
            validate_persisted_authority(
                temp.path(),
                repo_b.path(),
                &recipe,
                &BTreeMap::new(),
                false,
                false,
                &DefaultHomeSettings,
                &crate::dispatch::DefaultTestPluginMetadata,
            )
            .is_err()
        );
    }

    fn test_authority_recipe(secret_ref: KernelValue) -> Recipe {
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("shell".to_string()),
                plugin: "shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::from([(
                "secret_refs".to_string(),
                KernelValue::Array(vec![secret_ref]),
            )]),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(
                transist_protocol::FilesystemPolicySource::Builtin,
            ),
            effective_network_policy: Default::default(),
            effective_network_policy_source: Some(transist_protocol::NetworkPolicySource::Builtin),
        };
        Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "authority-test".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps: vec![step],
            cleanup: Vec::new(),
            graph: None,
            capabilities: BTreeMap::new(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    fn test_gate_recipe() -> Recipe {
        let mut recipe = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        recipe.steps = vec![RecipeStep {
            id: "review".to_string(),
            execution: StepExecutionDescriptor::Control {
                primitive: crate::ControlStepKind::GateManual.as_wire_str().to_string(),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }];
        recipe
    }

    #[test]
    fn workspace_binding_resolves_input_to_canonical_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut recipe = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        recipe.workspace = Some(transist_protocol::WorkspaceBinding {
            path: "{{inputs.repo_path}}".to_string(),
            access: transist_protocol::WorkspaceAccessMode::Full,
        });
        let inputs = BTreeMap::from([(
            "repo_path".to_string(),
            KernelValue::String(workspace.display().to_string()),
        )]);

        let resolved = resolve_recipe_workspace(&home, temp.path(), &mut recipe, &inputs)
            .expect("workspace resolves");

        assert_eq!(
            resolved,
            std::fs::canonicalize(&workspace).expect("canonical workspace")
        );
        assert_eq!(
            recipe
                .workspace
                .as_ref()
                .map(|binding| binding.path.as_str()),
            Some(resolved.to_str().expect("utf-8 path"))
        );
    }

    #[test]
    fn workspace_binding_drops_empty_resolved_read_allow_paths() {
        let mut recipe = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        recipe.filesystem_policy = Some(transist_protocol::FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec![
                "{{inputs.optional_path}}".to_string(),
                "{{inputs.empty_path}}".to_string(),
                "/etc/gitconfig".to_string(),
            ],
            write_allow_paths: Vec::new(),
        });
        let inputs = BTreeMap::from([
            ("optional_path".to_string(), KernelValue::Null),
            ("empty_path".to_string(), KernelValue::String(String::new())),
        ]);

        resolve_recipe_authority_fields(&mut recipe, &inputs).expect("resolve policy");

        assert_eq!(
            recipe.filesystem_policy.unwrap().read_allow_paths,
            vec!["/etc/gitconfig"]
        );
    }

    #[test]
    fn workspace_binding_workspace_path_is_an_authority_source_and_input() {
        let mut recipe = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        recipe.workspace = Some(transist_protocol::WorkspaceBinding {
            path: "{{inputs.repo_path}}".to_string(),
            access: transist_protocol::WorkspaceAccessMode::Full,
        });
        let mut sources = Vec::new();

        collect_authority_sources(&recipe, &recipe.steps[0], &mut sources)
            .expect("authority sources");

        assert!(authority_input_names(&recipe).contains("repo_path"));
        assert!(sources.iter().any(|source| {
            source.field == "workspace.path"
                && source.dependencies
                    == [CapabilitySourceDependency::WorkflowInput {
                        name: "repo_path".to_string(),
                    }]
        }));
    }

    #[test]
    fn workspace_binding_subflow_inherits_parent_and_may_narrow_access() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let canonical = std::fs::canonicalize(&workspace).expect("canonical workspace");
        let parent_workspace = Some(transist_protocol::WorkspaceBinding {
            path: canonical.display().to_string(),
            access: transist_protocol::WorkspaceAccessMode::Full,
        });
        let mut child = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        child.workspace = Some(transist_protocol::WorkspaceBinding {
            path: canonical.display().to_string(),
            access: transist_protocol::WorkspaceAccessMode::ReadOnly,
        });

        inherit_subflow_workspace(
            &home,
            &canonical,
            true,
            transist_protocol::WorkspaceAccessMode::Full,
            &mut child,
            &BTreeMap::new(),
        )
        .expect("child may narrow access");
        assert_eq!(
            child.workspace.unwrap().access,
            transist_protocol::WorkspaceAccessMode::ReadOnly
        );

        let mut inherited = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        inherit_subflow_workspace(
            &home,
            &canonical,
            true,
            transist_protocol::WorkspaceAccessMode::Full,
            &mut inherited,
            &BTreeMap::new(),
        )
        .expect("child inherits workspace");
        assert_eq!(inherited.workspace, parent_workspace);

        let mut legacy_child = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        inherit_subflow_workspace(
            &home,
            &canonical,
            false,
            transist_protocol::WorkspaceAccessMode::Full,
            &mut legacy_child,
            &BTreeMap::new(),
        )
        .expect("legacy child keeps caller-root fallback");
        assert!(legacy_child.workspace.is_none());
    }

    #[test]
    fn workspace_binding_subflow_rejects_path_mismatch_and_caps_access_widening() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let parent_path = temp.path().join("parent");
        let child_path = temp.path().join("child");
        for path in [&home, &parent_path, &child_path] {
            std::fs::create_dir_all(path).expect("directory");
        }
        let parent_path = std::fs::canonicalize(parent_path).expect("parent canonical");
        let mut child = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        child.workspace = Some(transist_protocol::WorkspaceBinding {
            path: parent_path.display().to_string(),
            access: transist_protocol::WorkspaceAccessMode::Full,
        });
        inherit_subflow_workspace(
            &home,
            &parent_path,
            true,
            transist_protocol::WorkspaceAccessMode::ReadOnly,
            &mut child,
            &BTreeMap::new(),
        )
        .expect("inherited read-only caps child access");
        assert_eq!(
            child.workspace.as_ref().unwrap().access,
            transist_protocol::WorkspaceAccessMode::ReadOnly
        );
        assert_eq!(
            child.steps[0]
                .effective_filesystem_policy
                .workspace_access
                .as_ref()
                .unwrap()
                .mode,
            transist_protocol::WorkspaceAccessMode::ReadOnly
        );

        child.workspace = Some(transist_protocol::WorkspaceBinding {
            path: child_path.display().to_string(),
            access: transist_protocol::WorkspaceAccessMode::ReadOnly,
        });
        assert!(
            inherit_subflow_workspace(
                &home,
                &parent_path,
                true,
                transist_protocol::WorkspaceAccessMode::ReadOnly,
                &mut child,
                &BTreeMap::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn subflow_read_only_is_materialized_before_nested_capability_projection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let workspace = std::fs::canonicalize(workspace).expect("canonical workspace");
        let mut child = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        child.network_policy = Some(transist_protocol::NetworkPolicy {
            mode: transist_protocol::NetworkAccessMode::Allowlist,
            allow_hosts: vec!["api.example.com".to_string()],
            allow_ports: vec![443],
            tls_required: true,
            max_redirects: 0,
        });
        child.steps[0].effective_network_policy_source = None;
        let mut subflow = RecipeStep {
            id: "child".to_string(),
            execution: StepExecutionDescriptor::Subflow {
                recipe: Box::new(child),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(
                transist_protocol::FilesystemPolicySource::Workflow,
            ),
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        subflow.effective_filesystem_policy.workspace_access =
            Some(transist_protocol::WorkspaceAccessPolicy {
                mode: transist_protocol::WorkspaceAccessMode::ReadOnly,
            });
        let mut parent = test_authority_recipe(KernelValue::String("TOKEN".to_string()));
        parent.steps = vec![subflow];
        parent.workspace = Some(transist_protocol::WorkspaceBinding {
            path: workspace.display().to_string(),
            access: transist_protocol::WorkspaceAccessMode::Full,
        });

        populate_recipe_capabilities(
            &home,
            &workspace,
            &mut parent,
            &BTreeMap::new(),
            false,
            &DefaultHomeSettings,
            &crate::dispatch::DefaultTestPluginMetadata,
        )
        .expect("project nested capabilities");

        let filesystem = parent.capabilities["child/invoke"]
            .iter()
            .find(|request| {
                request.capability == CapabilityName::Filesystem
                    && request.resource.starts_with("workspace://")
            })
            .expect("workspace capability");
        assert_eq!(filesystem.operations, ["list", "read"]);
        assert!(parent.capabilities["child/invoke"].iter().any(|request| {
            request.capability == CapabilityName::Network
                && request.resource == "https://api.example.com:443"
        }));
        let StepExecutionDescriptor::Subflow { recipe: child } = &parent.steps[0].execution else {
            panic!("subflow recipe");
        };
        assert_eq!(
            child.steps[0]
                .effective_filesystem_policy
                .workspace_access
                .as_ref()
                .unwrap()
                .mode,
            transist_protocol::WorkspaceAccessMode::ReadOnly
        );
    }
}
