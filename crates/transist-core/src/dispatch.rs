// SPDX-License-Identifier: MPL-2.0
use crate::ports::{PluginMetadataPort, PromptFilePort, RunStorePort};
use crate::types::{AssetLock, AssetLockKind};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use transist_protocol::{
    AdapterKind, CapabilityRequest, ExecutionRequest, FilesystemPolicySource, InvokePaths,
    InvokeStep, KernelValue, NetworkPolicySource, RecipePromptSource, RecipeStep,
    StepExecutionDescriptor, TEP_INVOKE_REQUEST_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
    invoke_capabilities,
};

const GATE_DECISION_META_KEY: &str = "__transist_gate_decision";

#[derive(Debug, Clone, PartialEq, Eq)]
struct GateDecisionBinding {
    decision_field: String,
    gate_step_id: String,
    gate_id: String,
}

/// Materialized invoke payload and its declared secret references.
#[derive(Debug, Clone)]
pub struct PreparedExecution {
    /// Fully resolved protocol request ready for static validation or dispatch.
    pub request: ExecutionRequest,
    /// Secret reference names declared by the materialized action input.
    pub secret_refs: Vec<String>,
    /// Explicit unknown JSON pointers; empty for a fully materialized request.
    pub deferred_paths: Vec<String>,
}

/// Immutable context needed to materialize one invoke request.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionRequestContext<'a> {
    /// Canonical workspace exposed to the step.
    pub repo_root: &'a Path,
    /// Effective Transist home directory.
    pub home_dir: &'a Path,
    /// Optional source directory of the compiled recipe.
    pub recipe_source_dir: Option<&'a Path>,
    /// Logical run identifier used in request metadata.
    pub run_id: &'a str,
    /// Workflow identifier used in request metadata.
    pub workflow_id: &'a str,
    /// Fully bound immutable workflow inputs.
    pub workflow_inputs: &'a BTreeMap<String, KernelValue>,
    /// Runtime variables visible to the step.
    pub workflow_vars: &'a BTreeMap<String, KernelValue>,
    /// Step execution ordinal.
    pub execution: u32,
    /// Logical invocation identifier.
    pub invoke_id: &'a str,
    /// Whether local-only restrictions apply to the request.
    pub local_only_mode: bool,
}

/// Materializes an invoke request for read-only workflow validation.
///
/// The returned paths are inert labels used solely to validate the sandbox
/// plan. This function never creates a run directory or resolves source assets
/// from persisted run storage.
pub fn make_static_execution_request(
    context: ExecutionRequestContext<'_>,
    step: &RecipeStep,
    workspace_declared: bool,
    paths: InvokePaths,
    plugin_metadata: &dyn PluginMetadataPort,
    prompt_files: &dyn PromptFilePort,
) -> Result<Option<PreparedExecution>> {
    make_static_execution_request_in_scope(
        context,
        step,
        workspace_declared,
        paths,
        plugin_metadata,
        prompt_files,
        &BTreeSet::new(),
    )
}

/// Materializes a static request in an explicit symbolic module-input scope.
/// Reads only birth-known sources and returns deferred locations separately from JSON.
#[allow(clippy::too_many_arguments)]
pub fn make_static_execution_request_in_scope(
    context: ExecutionRequestContext<'_>,
    step: &RecipeStep,
    workspace_declared: bool,
    paths: InvokePaths,
    plugin_metadata: &dyn PluginMetadataPort,
    prompt_files: &dyn PromptFilePort,
    deferred_inputs: &BTreeSet<String>,
) -> Result<Option<PreparedExecution>> {
    make_execution_request_unchecked(
        context,
        step,
        workspace_declared,
        0,
        false,
        &[],
        &|_home_dir, _run_id, _step_id, _execution| Ok(paths.clone()),
        plugin_metadata,
        prompt_files,
        None,
        None,
        true,
        deferred_inputs,
    )
}

#[cfg(test)]
pub(crate) struct DefaultTestPluginMetadata;

#[cfg(test)]
impl PluginMetadataPort for DefaultTestPluginMetadata {
    fn resolve_action(
        &self,
        plugin_id: &str,
        _plugin_version: &str,
        action: &str,
    ) -> Result<Option<crate::ports::PluginActionMetadata>> {
        if !action.is_empty() {
            Ok(Some(crate::ports::PluginActionMetadata {
                provider_kind: if plugin_id.starts_with("local.") {
                    transist_protocol::ProviderKind::Local
                } else {
                    transist_protocol::ProviderKind::External
                },
                secret_inputs: if plugin_id == "shell" && action == "exec_with_secret_env" {
                    vec![crate::ports::PluginSecretInput {
                        name: "MY_API_TOKEN".to_string(),
                        required: true,
                    }]
                } else {
                    Vec::new()
                },
            }))
        } else {
            Ok(None)
        }
    }

    fn resolve_network_origin(
        &self,
        _plugin_id: &str,
        _plugin_version: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
struct DefaultTestPromptFiles;

#[cfg(test)]
impl PromptFilePort for DefaultTestPromptFiles {
    fn resolve_prompt_file(
        &self,
        raw_path: &str,
        recipe_source_dir: Option<&Path>,
        step_id: &str,
    ) -> Result<crate::ports::PromptFileReference> {
        if raw_path == "missing.md" {
            bail!("step '{step_id}' prompt_file '{raw_path}' does not exist");
        }
        let path = PathBuf::from(raw_path);
        let path = if path.is_absolute() {
            path
        } else {
            recipe_source_dir
                .ok_or_else(|| anyhow::anyhow!("missing prompt source directory"))?
                .join(path)
        };
        let content = match raw_path {
            "prompts/task.md" => "Hello from file\n",
            path if Path::new(path).is_absolute() => "Absolute prompt fixture\n",
            _ => bail!("step '{step_id}' prompt_file '{raw_path}' does not exist"),
        };
        Ok(crate::ports::PromptFileReference {
            content: content.to_string(),
            path,
            sha256: String::new(),
        })
    }
}

/// Convenience wrapper used only by the in-module test suite.
#[cfg(test)]
pub fn make_execution_request(
    context: ExecutionRequestContext<'_>,
    step: &RecipeStep,
) -> Result<Option<PreparedExecution>> {
    make_execution_request_unchecked(
        context,
        step,
        false,
        0,
        false,
        &[],
        &|home_dir, run_id, step_id, execution| {
            Ok(tests::test_invocation_paths(
                context.repo_root,
                home_dir,
                run_id,
                step_id,
                execution,
            ))
        },
        &DefaultTestPluginMetadata,
        &DefaultTestPromptFiles,
        None,
        None,
        false,
        &BTreeSet::new(),
    )
}

/// Prepares an invoke request and proves that its generated authority exactly
/// matches the persisted checkpoint subset. The persisted values are then
/// forwarded unchanged; regeneration is verification only.
#[allow(clippy::too_many_arguments)] // Port-backed dispatch dependencies stay explicit.
pub(crate) fn make_execution_request_with_capabilities(
    context: ExecutionRequestContext<'_>,
    step: &RecipeStep,
    workspace_declared: bool,
    persisted: &[CapabilityRequest],
    record: &crate::types::PersistedRunRecord,
    run_store: &dyn RunStorePort,
    plugin_metadata: &dyn PluginMetadataPort,
    prompt_files: &dyn PromptFilePort,
    snapshot: Option<&crate::KernelRunSnapshot>,
    partial: bool,
    deferred_inputs: &BTreeSet<String>,
) -> Result<Option<PreparedExecution>> {
    record.validate_contract_versions()?;
    let Some(mut prepared) = make_execution_request_unchecked(
        context,
        step,
        workspace_declared,
        record.asset_lock_version,
        record.uses_workflow_asset_snapshot(),
        &record.asset_locks,
        &|home_dir, run_id, step_id, execution| {
            run_store.resolve_invocation_paths(
                home_dir,
                context.repo_root,
                run_id,
                step_id,
                execution,
            )
        },
        plugin_metadata,
        prompt_files,
        Some(run_store),
        snapshot,
        partial,
        deferred_inputs,
    )?
    else {
        return Ok(None);
    };
    if persisted.is_empty() {
        // Recipes written before the authority marker retain the historical
        // regeneration path. All runs born by the current kernel carry a
        // materialized map and never take this compatibility branch.
        return Ok(Some(prepared));
    }
    if prepared.request.requested_capabilities != persisted {
        bail!(
            "persisted authority does not match generated authority for step '{}'; execution is denied",
            step.id
        );
    }
    prepared.request.requested_capabilities = persisted.to_vec();
    Ok(Some(prepared))
}

/// Reports whether request metadata requires captured workflow assets without probing storage.
pub(crate) fn request_uses_workflow_asset_snapshot(request: &ExecutionRequest) -> bool {
    request
        .metadata
        .get("workflow_asset_snapshot")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

/// Loads the `source://` assets referenced by one snapshot-backed invocation from its
/// immutable run snapshot.
pub(crate) fn workflow_source_assets_for_request(
    run_store: &dyn RunStorePort,
    run_id: &str,
    request: &ExecutionRequest,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut relative_paths = BTreeSet::new();
    collect_source_uris(&request.input, &mut relative_paths)?;
    if relative_paths.is_empty() {
        return Ok(BTreeMap::new());
    }
    let manifest = run_store
        .read_workflow_asset_manifest(run_id)?
        .ok_or_else(|| anyhow::anyhow!("run '{run_id}' is missing its workflow asset manifest"))?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest).context("workflow asset manifest is not valid JSON")?;
    let entries = manifest
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("workflow asset manifest has no entries array"))?;
    let mut digests = BTreeMap::new();
    for entry in entries {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("workflow asset manifest entry has no path"))?;
        let sha256 = entry
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("workflow asset manifest entry has no sha256"))?;
        if let Some(relative) = path.strip_prefix("source/") {
            digests.insert(relative.to_string(), sha256.to_string());
        }
    }
    let mut assets = BTreeMap::new();
    for relative in relative_paths {
        let sha256 = digests.get(&relative).ok_or_else(|| {
            anyhow::anyhow!("run '{run_id}' is missing source asset 'source://{relative}'")
        })?;
        let bytes = run_store
            .read_workflow_asset_blob(run_id, sha256)?
            .ok_or_else(|| anyhow::anyhow!("run '{run_id}' is missing asset blob '{sha256}'"))?;
        assets.insert(relative, bytes);
    }
    Ok(assets)
}

fn collect_source_uris(value: &serde_json::Value, paths: &mut BTreeSet<String>) -> Result<()> {
    match value {
        serde_json::Value::String(raw) => {
            if let Some(relative) = raw.strip_prefix("source://") {
                let path = Path::new(relative);
                if path.is_absolute()
                    || path
                        .components()
                        .any(|component| !matches!(component, std::path::Component::Normal(_)))
                {
                    bail!("source:// path is not a contained relative path: {raw}");
                }
                paths.insert(relative.to_string());
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_source_uris(value, paths)?;
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_source_uris(value, paths)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Port-backed dispatch dependencies stay explicit.
fn make_execution_request_unchecked(
    context: ExecutionRequestContext<'_>,
    step: &RecipeStep,
    workspace_declared: bool,
    asset_lock_version: u32,
    workflow_asset_snapshot: bool,
    asset_locks: &[AssetLock],
    resolve_invocation_paths: &dyn Fn(&Path, &str, &str, u32) -> Result<InvokePaths>,
    plugin_metadata: &dyn PluginMetadataPort,
    prompt_files: &dyn PromptFilePort,
    run_store: Option<&dyn RunStorePort>,
    snapshot: Option<&crate::KernelRunSnapshot>,
    partial: bool,
    deferred_inputs: &BTreeSet<String>,
) -> Result<Option<PreparedExecution>> {
    if asset_lock_version > 1 {
        bail!(
            "unsupported asset lock version {}; expected version 0 or 1",
            asset_lock_version
        );
    }
    let StepExecutionDescriptor::Invoke {
        adapter,
        plugin_id,
        plugin,
        action,
        execution_kind,
        plugin_version,
        plugin_sha256,
    } = &step.execution
    else {
        return Ok(None);
    };
    validate_home_workspace_boundary(context.home_dir, context.repo_root)?;

    // Compiler-owned metadata is not action input and must never participate in
    // workflow placeholder resolution. Extract it before resolving `with:` so
    // the manifest-defined destination remains frozen in the compiled recipe.
    let mut unresolved_with = step.with.clone();
    let document_bindings =
        transist_protocol::step_document_bindings(step).map_err(anyhow::Error::msg)?;
    let gate_decision_binding = take_gate_decision_binding(&mut unresolved_with)?;
    unresolved_with.remove(transist_protocol::DEFERRED_BINDINGS_KEY);
    unresolved_with.remove(transist_protocol::DOCUMENT_BINDINGS_KEY);
    let authored = KernelValue::Object(unresolved_with);
    let (resolved, mut deferred_paths) = if partial {
        let resolved = crate::resolve_partial_action_value(
            &authored,
            context.workflow_inputs,
            context.workflow_vars,
            deferred_inputs,
        )?;
        (resolved.value, resolved.deferred_paths)
    } else {
        (
            crate::resolve_action_value(
                &authored,
                context.workflow_inputs,
                context.workflow_vars,
                snapshot,
                false,
            )?,
            Vec::new(),
        )
    };
    let KernelValue::Object(mut resolved_with) = resolved else {
        unreachable!("object resolution")
    };
    if partial && let Some(binding) = &step.prompt_binding {
        let value = match &binding.source {
            RecipePromptSource::Inline { value } => value,
            RecipePromptSource::File { path } => path,
        };
        if !crate::resolve_partial_action_value(
            &KernelValue::String(value.clone()),
            context.workflow_inputs,
            context.workflow_vars,
            deferred_inputs,
        )?
        .deferred_paths
        .is_empty()
        {
            deferred_paths.push(format!(
                "/{}",
                transist_protocol::binding_pointer_segment(&binding.inline_field)
            ));
        }
    }
    inject_prompt_binding(
        &mut resolved_with,
        step,
        &context,
        prompt_files,
        prompt_file_lock_for_dispatch(step, asset_lock_version, asset_locks)?,
        workflow_asset_snapshot,
        run_store,
        snapshot,
        partial,
        deferred_inputs,
    )?;
    inject_skills_binding(&mut resolved_with, step, &context)?;
    inject_gate_decision_binding(
        &mut resolved_with,
        &context,
        run_store,
        gate_decision_binding,
    )?;
    let input =
        serde_json::to_value(to_json_map(&resolved_with)).context("serialize invoke input")?;
    let materialized_bindings =
        transist_protocol::collect_document_bindings(&input).map_err(anyhow::Error::msg)?;
    if materialized_bindings != document_bindings {
        bail!(
            "$document objects cannot be introduced by input, variable, prompt or step-output references"
        );
    }
    if partial {
        deferred_paths.extend(
            document_bindings
                .iter()
                .map(|location| location.path.clone()),
        );
    }
    let invocation_paths = resolve_invocation_paths(
        context.home_dir,
        context.run_id,
        &step.id,
        context.execution,
    )?;
    let secret_refs = secret_refs(&resolved_with);
    validate_secret_refs_against_action(
        step,
        plugin_id.as_deref().unwrap_or(plugin),
        plugin_version.as_deref(),
        action,
        &secret_refs,
        plugin_metadata,
    )?;
    let filesystem_policy = step.effective_filesystem_policy.clone();
    let filesystem_policy_source = step
        .effective_filesystem_policy_source
        .unwrap_or(FilesystemPolicySource::Builtin);
    let provider_kind = resolve_effective_provider_kind(
        plugin_metadata,
        plugin_id.as_deref().unwrap_or(plugin),
        plugin_version.as_deref(),
        action,
    );
    let (network_policy, network_policy_source) = effective_network_policy_for_dispatch(
        *adapter,
        &provider_kind,
        step.effective_network_policy.clone(),
        step.effective_network_policy_source
            .unwrap_or(NetworkPolicySource::Builtin),
        context.local_only_mode,
    );
    let network_origins = if network_policy.mode != transist_protocol::NetworkAccessMode::None {
        crate::kernel::recipe_network_origins(
            plugin_id.as_deref().unwrap_or(plugin),
            plugin_version.as_deref(),
            &network_policy,
            plugin_metadata,
        )?
        .into_iter()
        .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let requested_capabilities = invoke_capabilities(
        *adapter,
        plugin_id.as_deref().unwrap_or(plugin),
        plugin_version.as_deref(),
        action,
        provider_kind.as_str(),
        &secret_refs,
        &filesystem_policy,
        context.repo_root,
        context.home_dir,
        &network_origins,
    )
    .map_err(|error| anyhow::anyhow!("invalid generated capability request: {error}"))?;
    Ok(Some(PreparedExecution {
        deferred_paths,
        request: ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: context.invoke_id.to_string(),
            run_id: context.run_id.to_string(),
            workflow_id: context.workflow_id.to_string(),
            step: InvokeStep {
                id: step.id.clone(),
                execution: context.execution,
            },
            adapter_kind: *adapter,
            action: action.clone(),
            input,
            paths: invocation_paths,
            requested_capabilities,
            timeout_ms: timeout_ms(&resolved_with),
            max_timeout_ms: max_timeout_ms(&resolved_with),
            policy: {
                let mut policy = serde_json::json!({
                    "execution_kind": execution_kind,
                    "adapter": adapter,
                    "workspace_declared": workspace_declared,
                });
                let workspace_root = context.repo_root.display().to_string();
                policy["filesystem"] = serde_json::json!({
                    "source": filesystem_policy_source,
                    "workspace_root": workspace_root,
                    "workspace_access": filesystem_policy.workspace_access,
                    "read_allow_paths": filesystem_policy.read_allow_paths,
                    "write_allow_paths": filesystem_policy.write_allow_paths,
                });
                policy["network"] = serde_json::json!({
                    "source": network_policy_source,
                    "mode": network_policy.mode,
                    "allow_hosts": network_policy.allow_hosts,
                    "allow_ports": network_policy.allow_ports,
                    "tls_required": network_policy.tls_required,
                    "max_redirects": network_policy.max_redirects,
                });
                policy
            },
            metadata: {
                let mut meta = serde_json::json!({
                    "step_id": step.id,
                    "workflow_id": context.workflow_id,
                    "plugin_id": plugin_id.as_deref().unwrap_or(plugin),
                    "secret_refs": secret_refs,
                });
                if !document_bindings.is_empty() {
                    meta["document_bindings"] = serde_json::to_value(&document_bindings)?;
                }
                if let Some(ver) = plugin_version {
                    meta["plugin_version"] = serde_json::Value::String(ver.clone());
                }
                if let Some(sha) = plugin_sha256 {
                    meta["plugin_sha256"] = serde_json::Value::String(sha.clone());
                }
                if workflow_asset_snapshot {
                    meta["asset_lock_version"] = serde_json::json!(1);
                    meta["workflow_asset_snapshot"] = serde_json::json!(true);
                } else if let Some(source_dir) = context.recipe_source_dir {
                    meta["recipe_source_dir"] =
                        serde_json::Value::String(source_dir.display().to_string());
                }
                meta["provider_kind"] =
                    serde_json::Value::String(provider_kind.as_str().to_string());
                meta
            },
        },
        secret_refs,
    }))
}

fn inject_gate_decision_binding(
    input: &mut BTreeMap<String, KernelValue>,
    context: &ExecutionRequestContext<'_>,
    run_store: Option<&dyn RunStorePort>,
    binding: Option<GateDecisionBinding>,
) -> Result<()> {
    let Some(binding) = binding else {
        return Ok(());
    };
    // Static recipe validation has no durable run store. It still removes the
    // compiler-only metadata so the plugin schema sees the initial iteration
    // shape, where the optional decision field is absent.
    let Some(run_store) = run_store else {
        return Ok(());
    };
    let Some(record) = crate::kernel::load_latest_gate_decision_history_record(
        run_store,
        context.home_dir,
        context.run_id,
        &binding.gate_step_id,
        &binding.gate_id,
    )?
    else {
        return Ok(());
    };
    inject_gate_decision_record(input, binding, record)
}

fn inject_gate_decision_record(
    input: &mut BTreeMap<String, KernelValue>,
    binding: GateDecisionBinding,
    record: transist_protocol::GateDecisionRecord,
) -> Result<()> {
    if input.contains_key(&binding.decision_field) {
        bail!(
            "gate decision destination '{}' is already populated",
            binding.decision_field
        );
    }
    input.insert(
        binding.decision_field,
        KernelValue::from(serde_json::to_value(record).context("serialize gate decision")?),
    );
    Ok(())
}

fn take_gate_decision_binding(
    input: &mut BTreeMap<String, KernelValue>,
) -> Result<Option<GateDecisionBinding>> {
    let Some(raw) = input.remove(GATE_DECISION_META_KEY) else {
        return Ok(None);
    };
    let KernelValue::Object(values) = raw else {
        bail!("compiled gate decision binding must be an object");
    };
    let required_string = |key: &str| match values.get(key) {
        Some(KernelValue::String(value)) if !value.trim().is_empty() => Ok(value.clone()),
        _ => bail!("compiled gate decision binding is missing non-empty '{key}'"),
    };
    Ok(Some(GateDecisionBinding {
        decision_field: required_string("decision_field")?,
        gate_step_id: required_string("gate_step_id")?,
        gate_id: required_string("gate_id")?,
    }))
}

pub(crate) fn effective_network_policy_for_dispatch(
    adapter: AdapterKind,
    provider_kind: &transist_protocol::ProviderKind,
    mut network_policy: transist_protocol::NetworkPolicy,
    mut source: NetworkPolicySource,
    local_only_mode: bool,
) -> (transist_protocol::NetworkPolicy, NetworkPolicySource) {
    if local_only_mode
        && ((adapter == AdapterKind::Http
            && network_policy.mode != transist_protocol::NetworkAccessMode::None)
            || (provider_kind.is_local()
                && matches!(
                    network_policy.mode,
                    transist_protocol::NetworkAccessMode::Any
                        | transist_protocol::NetworkAccessMode::Allowlist
                )))
    {
        network_policy.mode = transist_protocol::NetworkAccessMode::Loopback;
        source = NetworkPolicySource::Builtin;
    }
    (network_policy, source)
}

/// Rejects runtime homes mounted inside the workspace exposed to an adapter.
pub fn validate_home_workspace_boundary(home_dir: &Path, workspace_dir: &Path) -> Result<()> {
    let home = canonicalize_path_allow_missing(home_dir, "home_dir")?;
    let workspace = canonicalize_path_allow_missing(workspace_dir, "workspace_dir")?;
    if home == workspace || home.starts_with(&workspace) {
        bail!(
            "runtime_home_inside_workspace: Transist home '{}' is inside workspace '{}'",
            home.display(),
            workspace.display()
        );
    }
    if workspace.starts_with(&home) {
        bail!(
            "runtime_workspace_inside_home: adapter workspace '{}' is inside Transist home '{}'",
            workspace.display(),
            home.display()
        );
    }
    Ok(())
}

/// Canonicalizes a literal filesystem path, retaining missing normal descendants.
/// Reads path metadata only; `label` identifies failures without applying grant syntax.
pub(crate) fn canonicalize_path_allow_missing(path: &Path, label: &str) -> Result<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Ok(canonical);
    }
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        match fs::canonicalize(cursor) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(_) => {
                let component = cursor.file_name().ok_or_else(|| {
                    anyhow::anyhow!(
                        "failed to canonicalize {label} '{}': missing ancestor",
                        path.display()
                    )
                })?;
                missing.push(component.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    anyhow::anyhow!(
                        "failed to canonicalize {label} '{}': missing ancestor",
                        path.display()
                    )
                })?;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // Storage, pinned assets and partial data scope remain explicit.
fn inject_prompt_binding(
    resolved_with: &mut BTreeMap<String, KernelValue>,
    step: &RecipeStep,
    context: &ExecutionRequestContext<'_>,
    prompt_files: &dyn PromptFilePort,
    prompt_file_lock: Option<&AssetLock>,
    workflow_asset_snapshot: bool,
    run_store: Option<&dyn RunStorePort>,
    snapshot: Option<&crate::KernelRunSnapshot>,
    partial: bool,
    deferred_inputs: &BTreeSet<String>,
) -> Result<()> {
    let Some(prompt_binding) = step.prompt_binding.as_ref() else {
        return Ok(());
    };
    match &prompt_binding.source {
        RecipePromptSource::Inline { value } => {
            let resolved = crate::resolve_action_value(
                &KernelValue::String(value.clone()),
                context.workflow_inputs,
                context.workflow_vars,
                snapshot,
                partial,
            )?;
            if resolved == KernelValue::Null
                && !transist_protocol::binding_selectors(value)
                    .map_err(anyhow::Error::msg)?
                    .iter()
                    .any(|selector| selector.starts_with("steps."))
            {
                bail!("prompt cannot resolve to null");
            }
            let prompt = crate::templating::dataflow::scalar(&resolved)?;
            if prompt.trim().is_empty() {
                bail!("prompt cannot resolve to an empty string");
            }
            resolved_with.insert(
                prompt_binding.inline_field.clone(),
                KernelValue::String(prompt),
            );
        }
        RecipePromptSource::File { path } => {
            if partial
                && !crate::resolve_partial_action_value(
                    &KernelValue::String(path.clone()),
                    context.workflow_inputs,
                    context.workflow_vars,
                    deferred_inputs,
                )?
                .deferred_paths
                .is_empty()
            {
                resolved_with.insert(
                    prompt_binding.inline_field.clone(),
                    KernelValue::String(path.clone()),
                );
                return Ok(());
            }
            prompt_binding.file_field.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "step '{}' compiled a prompt file source without a file_field binding",
                    step.id
                )
            })?;
            let resolved = resolve_string_input(
                path,
                context.workflow_inputs,
                context.workflow_vars,
                "prompt_file",
            )?;
            let content = if workflow_asset_snapshot {
                let lock = prompt_file_lock.ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing snapshot prompt_file asset lock for step '{}'",
                        step.id
                    )
                })?;
                let bytes = run_store
                    .ok_or_else(|| anyhow::anyhow!("snapshot prompt_file storage is unavailable"))?
                    .read_workflow_asset_blob(context.run_id, &lock.sha256)?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "run '{}' is missing prompt_file snapshot blob '{}'",
                            context.run_id,
                            lock.sha256
                        )
                    })?;
                let actual: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                if actual != lock.sha256 {
                    bail!(
                        "run '{}' prompt_file snapshot blob '{}' has changed; found '{}'",
                        context.run_id,
                        lock.sha256,
                        actual
                    );
                }
                String::from_utf8(bytes).with_context(|| {
                    format!(
                        "run '{}' prompt_file snapshot blob '{}' is not UTF-8",
                        context.run_id, lock.sha256
                    )
                })?
            } else {
                let prompt_path = prompt_files.resolve_prompt_file(
                    &resolved,
                    context.recipe_source_dir,
                    &step.id,
                )?;
                // Source-package prompts are captured workflow inputs, not
                // adapter-visible filesystem access.
                if !is_workflow_source_asset(&prompt_path.path, context.recipe_source_dir) {
                    guard_prompt_file(
                        &step.effective_filesystem_policy,
                        step.effective_filesystem_policy_source
                            .unwrap_or(FilesystemPolicySource::Builtin),
                        &prompt_path.path,
                        &step.id,
                        context.repo_root,
                    )?;
                }
                if let Some(lock) = prompt_file_lock {
                    let injected_sha256: String = Sha256::digest(prompt_path.content.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    if injected_sha256 != lock.sha256 {
                        bail!(
                            "prompt_file '{}' locked by step '{}' has changed; expected digest '{}' but found '{}'",
                            prompt_path.path.display(),
                            lock.step_id,
                            lock.sha256,
                            injected_sha256
                        );
                    }
                }
                prompt_path.content
            };
            resolved_with.insert(
                prompt_binding.inline_field.clone(),
                KernelValue::String(content),
            );
        }
    }
    Ok(())
}

fn prompt_file_lock_for_dispatch<'a>(
    step: &RecipeStep,
    asset_lock_version: u32,
    asset_locks: &'a [AssetLock],
) -> Result<Option<&'a AssetLock>> {
    if asset_lock_version == 0
        || !matches!(
            step.prompt_binding.as_ref().map(|binding| &binding.source),
            Some(RecipePromptSource::File { .. })
        )
    {
        return Ok(None);
    }

    let prompt_path = match &step.prompt_binding.as_ref().expect("checked above").source {
        RecipePromptSource::File { path } => path,
        RecipePromptSource::Inline { .. } => unreachable!("checked above"),
    };
    let step_locks: Vec<_> = asset_locks
        .iter()
        .filter(|lock| lock.step_id == step.id)
        .collect();
    let prompt_locks: Vec<_> = step_locks
        .iter()
        .copied()
        .filter(|lock| lock.kind == AssetLockKind::PromptFile)
        .collect();
    if prompt_locks.len() > 1 {
        bail!("duplicate prompt_file asset locks for step '{}'", step.id);
    }
    if let Some(lock) = step_locks
        .iter()
        .copied()
        .find(|lock| lock.ref_id == prompt_path.as_str())
        && lock.kind != AssetLockKind::PromptFile
    {
        bail!(
            "prompt_file asset lock for step '{}' has wrong kind",
            step.id
        );
    }
    let Some(lock) = prompt_locks.first().copied() else {
        if asset_locks
            .iter()
            .any(|lock| lock.ref_id == prompt_path.as_str())
        {
            bail!(
                "prompt_file asset lock '{}' has wrong step; expected '{}'",
                prompt_path,
                step.id
            );
        }
        bail!("missing prompt_file asset lock for step '{}'", step.id);
    };
    if lock.ref_id != prompt_path.as_str() {
        bail!(
            "prompt_file asset lock '{}' does not match authored path for step '{}'",
            lock.ref_id,
            step.id
        );
    }
    Ok(Some(lock))
}

fn inject_skills_binding(
    resolved_with: &mut BTreeMap<String, KernelValue>,
    step: &RecipeStep,
    context: &ExecutionRequestContext<'_>,
) -> Result<()> {
    let Some(skills_binding) = step.skills_binding.as_ref() else {
        return Ok(());
    };
    if resolved_with.contains_key(skills_binding.bundle_field.as_str())
        || resolved_with.contains_key(skills_binding.rendered_field.as_str())
    {
        bail!(
            "step '{}' input already contains injected skills fields '{}', '{}'",
            step.id,
            skills_binding.bundle_field,
            skills_binding.rendered_field
        );
    }
    guard_skill_bundle_paths(
        &skills_binding.bundle,
        &step.effective_filesystem_policy,
        step.effective_filesystem_policy_source
            .unwrap_or(FilesystemPolicySource::Builtin),
        &step.id,
        context.repo_root,
        context.recipe_source_dir,
    )?;
    let bundle = serde_json::to_value(&skills_binding.bundle)
        .context("failed serializing embedded skills bundle")?;
    resolved_with.insert(
        skills_binding.bundle_field.clone(),
        KernelValue::from_json(bundle),
    );
    resolved_with.insert(
        skills_binding.rendered_field.clone(),
        KernelValue::String(skills_binding.rendered.clone()),
    );
    Ok(())
}

/// Resolves immutable input and variable placeholders in an action payload.
pub fn resolve_input_placeholders(
    value: &BTreeMap<String, KernelValue>,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
) -> Result<BTreeMap<String, KernelValue>> {
    value
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                resolve_kernel_value(value, workflow_inputs, workflow_vars)?,
            ))
        })
        .collect()
}

fn timeout_ms(with: &BTreeMap<String, KernelValue>) -> Option<u64> {
    with.get("timeout_ms").and_then(|value| match value {
        KernelValue::Number(number) => number.as_u64(),
        _ => None,
    })
}

fn max_timeout_ms(with: &BTreeMap<String, KernelValue>) -> Option<u64> {
    with.get("max_timeout_ms").and_then(|value| match value {
        KernelValue::Number(number) => number.as_u64(),
        _ => None,
    })
}

fn secret_refs(with: &BTreeMap<String, KernelValue>) -> Vec<String> {
    let Some(KernelValue::Array(values)) = with.get("secret_refs") else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            KernelValue::String(secret) if !secret.trim().is_empty() => Some(secret.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn validate_secret_refs_against_action(
    step: &RecipeStep,
    plugin_id: &str,
    plugin_version: Option<&str>,
    action_name: &str,
    secret_refs: &[String],
    plugin_metadata: &dyn PluginMetadataPort,
) -> Result<()> {
    let Some(plugin_version) = plugin_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let action = plugin_metadata
        .resolve_action(plugin_id, plugin_version, action_name)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "step '{}' references unknown action '{}.{}'",
                step.id,
                plugin_id,
                action_name
            )
        })?;

    let declared_secret_refs: BTreeSet<_> = action
        .secret_inputs
        .iter()
        .map(|secret| secret.name.as_str())
        .collect();
    for secret_ref in secret_refs {
        if !declared_secret_refs.contains(secret_ref.as_str()) {
            bail!(
                "step '{}' references undeclared secret ref '{}' for action '{}.{}'",
                step.id,
                secret_ref,
                plugin_id,
                action_name
            );
        }
    }

    for required_secret in action.secret_inputs.iter().filter(|secret| secret.required) {
        if !secret_refs
            .iter()
            .any(|secret_ref| secret_ref == &required_secret.name)
        {
            bail!(
                "step '{}' is missing required secret ref '{}' for action '{}.{}'",
                step.id,
                required_secret.name,
                plugin_id,
                action_name
            );
        }
    }

    Ok(())
}

fn to_json_map(with: &BTreeMap<String, KernelValue>) -> serde_json::Map<String, serde_json::Value> {
    with.iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::from(value.clone())))
        .collect()
}

pub(crate) fn resolve_effective_provider_kind(
    plugin_metadata: &dyn PluginMetadataPort,
    plugin_id: &str,
    plugin_version: Option<&str>,
    action_name: &str,
) -> transist_protocol::ProviderKind {
    use transist_protocol::ProviderKind;
    let Some(plugin_version) = plugin_version.filter(|version| !version.is_empty()) else {
        return ProviderKind::External;
    };
    if plugin_id.is_empty() {
        return ProviderKind::External;
    }
    plugin_metadata
        .resolve_action(plugin_id, plugin_version, action_name)
        .ok()
        .flatten()
        .map(|metadata| metadata.provider_kind)
        .unwrap_or(ProviderKind::External)
}

fn guard_prompt_file(
    policy: &transist_protocol::FilesystemPolicy,
    source: FilesystemPolicySource,
    path: &Path,
    step_id: &str,
    workspace_root: &Path,
) -> Result<()> {
    crate::filesystem_policy::check_path(
        policy,
        source,
        crate::filesystem_policy::FilesystemOperation::Read,
        path,
        step_id,
        "prompt_file",
        workspace_root,
    )
    .map_err(anyhow::Error::new)
}

fn is_workflow_source_asset(path: &Path, recipe_source_dir: Option<&Path>) -> bool {
    let Some(recipe_source_dir) = recipe_source_dir else {
        return false;
    };
    let (Ok(source_dir), Ok(path)) = (
        std::fs::canonicalize(recipe_source_dir),
        std::fs::canonicalize(path),
    ) else {
        return false;
    };
    // Child recipes resolve relative assets from their conventional
    // `subflows/` directory, while their package assets remain rooted one
    // level above it.
    let package_root = source_dir
        .file_name()
        .filter(|name| *name == "subflows")
        .and_then(|_| source_dir.parent())
        .unwrap_or(&source_dir);
    path.starts_with(package_root)
}

fn guard_skill_bundle_paths(
    bundle: &transist_protocol::RecipeSkillBundle,
    policy: &transist_protocol::FilesystemPolicy,
    source: FilesystemPolicySource,
    step_id: &str,
    workspace_root: &Path,
    recipe_source_dir: Option<&Path>,
) -> Result<()> {
    for authored_ref in &bundle.authored_refs {
        let path = Path::new(&authored_ref.resolved_path);
        if !is_workflow_source_asset(path, recipe_source_dir) {
            crate::filesystem_policy::check_path(
                policy,
                source,
                crate::filesystem_policy::FilesystemOperation::Read,
                path,
                step_id,
                "skills.authored_ref",
                workspace_root,
            )
            .map_err(anyhow::Error::new)?;
        }
    }
    for root in &bundle.roots {
        let skill_root = Path::new(&root.skill_root);
        if !is_workflow_source_asset(skill_root, recipe_source_dir) {
            crate::filesystem_policy::check_path(
                policy,
                source,
                crate::filesystem_policy::FilesystemOperation::List,
                skill_root,
                step_id,
                "skills.skill_root",
                workspace_root,
            )
            .map_err(anyhow::Error::new)?;
        }
        let skill_file = Path::new(&root.skill_file);
        if !is_workflow_source_asset(skill_file, recipe_source_dir) {
            crate::filesystem_policy::check_path(
                policy,
                source,
                crate::filesystem_policy::FilesystemOperation::Read,
                skill_file,
                step_id,
                "skills.skill_file",
                workspace_root,
            )
            .map_err(anyhow::Error::new)?;
        }
        for document in &root.documents {
            let path = Path::new(&document.resolved_path);
            if !is_workflow_source_asset(path, recipe_source_dir) {
                crate::filesystem_policy::check_path(
                    policy,
                    source,
                    crate::filesystem_policy::FilesystemOperation::Read,
                    path,
                    step_id,
                    "skills.document",
                    workspace_root,
                )
                .map_err(anyhow::Error::new)?;
            }
        }
    }
    Ok(())
}

fn resolve_kernel_value(
    value: &KernelValue,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
) -> Result<KernelValue> {
    resolve_kernel_value_inner(value, workflow_inputs, workflow_vars, false)
}

/// Walk a `with:` value and substitute `{{inputs.*}}` / `{{vars.*}}`
/// placeholders.
///
/// `stringify_scalars_in_exact_placeholder` controls how a single
/// `{{inputs.x}}` placeholder resolves when `x` is a scalar (Number/Bool):
/// - `false` (top-level `with:` map values): preserve the source type. This
///   matches the documented type-preservation contract for `with:` fields
///   like `timeout_ms` that downstream code reads as `KernelValue::Number`.
/// - `true` (inside arrays): coerce scalars to string. Plugin input
///   schemas typically declare list-typed fields such as `args: string[]`,
///   and the YAML author wrote a string scalar template
///   (`args: ['{{inputs.required_pr_approvals}}']`) — so the contract for
///   array elements is "string". Without this coercion, a numeric input
///   plumbed through an exact placeholder slips through as a JSON number
///   and fails preflight.
fn resolve_kernel_value_inner(
    value: &KernelValue,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
    stringify_scalars_in_exact_placeholder: bool,
) -> Result<KernelValue> {
    match value {
        KernelValue::String(raw) => resolve_string_value(
            raw,
            workflow_inputs,
            workflow_vars,
            stringify_scalars_in_exact_placeholder,
        ),
        KernelValue::Array(values) => values
            .iter()
            .map(|value| resolve_kernel_value_inner(value, workflow_inputs, workflow_vars, true))
            .collect::<Result<Vec<_>>>()
            .map(KernelValue::Array),
        KernelValue::Object(entries) => entries
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    resolve_kernel_value_inner(
                        value,
                        workflow_inputs,
                        workflow_vars,
                        stringify_scalars_in_exact_placeholder,
                    )?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()
            .map(KernelValue::Object),
        _ => Ok(value.clone()),
    }
}

fn resolve_string_value(
    raw: &str,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
    stringify_scalars_in_exact_placeholder: bool,
) -> Result<KernelValue> {
    // Fast path: single exact {{inputs.<key>}} placeholder.
    //
    // Non-string values (Array/Object) are returned as-is so callers can pass
    // structured data through templates. For scalars, behavior depends on the
    // caller-supplied flag (see `resolve_kernel_value_inner` doc comment):
    // top-level `with:` values preserve source type; array elements coerce
    // to string to match plugin input schemas like `args: string[]`.
    if let Some(input_key) = exact_input_placeholder(raw) {
        let value = workflow_inputs
            .get(&input_key)
            .cloned()
            .unwrap_or(KernelValue::Null);
        return coerce_exact_placeholder_value(
            value,
            &input_key,
            stringify_scalars_in_exact_placeholder,
        );
    }

    // Fast path: single exact {{vars.<key>}} placeholder. Same coercion rules
    // as the inputs fast path above.
    if let Some(var_key) = exact_vars_placeholder(raw) {
        let value = workflow_vars
            .get(&var_key)
            .cloned()
            .unwrap_or(KernelValue::Null);
        return coerce_exact_placeholder_value(
            value,
            &var_key,
            stringify_scalars_in_exact_placeholder,
        );
    }

    if !raw.contains("{{inputs.") && !raw.contains("{{vars.") {
        return Ok(KernelValue::String(raw.to_string()));
    }

    // Multi-placeholder slow path: substitute {{inputs.*}} and {{vars.*}} in
    // document order, whichever comes first.
    let mut rendered = String::new();
    let mut rest = raw;
    loop {
        let inputs_pos = rest.find("{{inputs.");
        let vars_pos = rest.find("{{vars.");

        if inputs_pos.is_none() && vars_pos.is_none() {
            break;
        }

        let use_inputs = match (inputs_pos, vars_pos) {
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(i), Some(v)) => i <= v,
            (None, None) => break,
        };

        if use_inputs {
            let i = inputs_pos.unwrap();
            rendered.push_str(&rest[..i]);
            let after = &rest[i + "{{inputs.".len()..];
            let Some(end_idx) = after.find("}}") else {
                bail!("invalid input placeholder '{}': missing closing '}}'", raw);
            };
            let key = after[..end_idx].trim();
            let value = workflow_inputs
                .get(key)
                .cloned()
                .unwrap_or(KernelValue::Null);
            rendered.push_str(&stringify_scalar_input(&value, key)?);
            rest = &after[end_idx + 2..];
        } else {
            let v = vars_pos.unwrap();
            rendered.push_str(&rest[..v]);
            let after = &rest[v + "{{vars.".len()..];
            let Some(end_idx) = after.find("}}") else {
                bail!("invalid var placeholder '{}': missing closing '}}'", raw);
            };
            let key = after[..end_idx].trim();
            let value = workflow_vars.get(key).cloned().unwrap_or(KernelValue::Null);
            rendered.push_str(&stringify_scalar_input(&value, key)?);
            rest = &after[end_idx + 2..];
        }
    }
    rendered.push_str(rest);
    Ok(KernelValue::String(rendered))
}

pub(crate) fn resolve_string_input(
    raw: &str,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    workflow_vars: &BTreeMap<String, KernelValue>,
    label: &str,
) -> Result<String> {
    // String-typed contexts (prompt content, file paths): stringify scalars
    // so a numeric input templated in via `{{inputs.x}}` doesn't blow up with
    // "must resolve to a string".
    match resolve_string_value(raw, workflow_inputs, workflow_vars, true)? {
        KernelValue::String(value) if !value.trim().is_empty() => Ok(value),
        KernelValue::String(_) => bail!("{} cannot resolve to an empty string", label),
        other => bail!(
            "{} must resolve to a string, received '{}'",
            label,
            kernel_value_type_name(&other)
        ),
    }
}

fn exact_input_placeholder(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("{{inputs.")?.strip_suffix("}}")?;
    // Reject anything that contains additional template syntax (e.g.
    // "{{inputs.a}}/{{inputs.b}}") so the multi-placeholder slow path is
    // taken instead. Without this guard the entire raw string is treated
    // as a single key lookup, the lookup misses, and the value silently
    // resolves to `Null` — surfacing later as `received null` from the
    // input-schema validator with no hint that templating was the cause.
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    let inner = inner.trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

/// Matches a string that is exactly `{{vars.<key>}}` (whole-value, no extra content).
fn exact_vars_placeholder(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("{{vars.")?.strip_suffix("}}")?;
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    let inner = inner.trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

fn stringify_scalar_input(value: &KernelValue, key: &str) -> Result<String> {
    match value {
        KernelValue::Null => bail!(
            "input '{}' is null and cannot be interpolated into a larger string",
            key
        ),
        KernelValue::Bool(value) => Ok(value.to_string()),
        KernelValue::Number(value) => Ok(value.to_string()),
        KernelValue::String(value) => Ok(value.clone()),
        KernelValue::Array(_) | KernelValue::Object(_) => {
            bail!(
                "input '{}' must be scalar when interpolated into a larger string",
                key
            )
        }
    }
}

/// Coerce the value of a single exact `{{inputs.<k>}}` / `{{vars.<k>}}`
/// placeholder.
///
/// `stringify_scalars`:
/// - `false` — preserve the source `KernelValue` for all types. Used at
///   the top-level of a `with:` map so that fields like `timeout_ms`
///   stay numeric (see `vars_placeholder_in_with_resolves_with_type_preservation`).
/// - `true` — coerce `Number`/`Bool`/`String` scalars to `KernelValue::String`.
///   Used when the placeholder appears inside an array element, because
///   the YAML author wrote a string scalar template and plugin input
///   schemas typically require array-of-string for fields like
///   `args` (a numeric workflow input previously
///   slipped through as JSON number and failed the shell plugin
///   `exec.input.schema.json` preflight check).
///
/// `Array`/`Object` values are always returned as-is so callers can pass
/// structured data through templates regardless of context.
fn coerce_exact_placeholder_value(
    value: KernelValue,
    key: &str,
    stringify_scalars: bool,
) -> Result<KernelValue> {
    match value {
        KernelValue::Null => Ok(KernelValue::Null),
        KernelValue::Bool(_) | KernelValue::Number(_) | KernelValue::String(_) => {
            if stringify_scalars {
                Ok(KernelValue::String(stringify_scalar_input(&value, key)?))
            } else {
                Ok(value)
            }
        }
        KernelValue::Array(_) | KernelValue::Object(_) => Ok(value),
    }
}

fn kernel_value_type_name(value: &KernelValue) -> &'static str {
    match value {
        KernelValue::Null => "null",
        KernelValue::Bool(_) => "boolean",
        KernelValue::Number(_) => "number",
        KernelValue::String(_) => "string",
        KernelValue::Array(_) => "array",
        KernelValue::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use transist_protocol::{
        AdapterKind, RecipePromptBinding, RecipePromptSource, RecipeSkillBundle,
        RecipeSkillDocument, RecipeSkillRef, RecipeSkillRoot, RecipeSkillsBinding,
    };

    fn seed_run_locator(home_dir: &Path, run_id: &str) {
        fs::create_dir_all(home_dir.join("runs")).expect("create runs root");
        fs::create_dir_all(home_dir.join("runs").join(format!("run_{run_id}")))
            .expect("create run root");
    }

    fn temp_home_dir() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let home_dir = temp.path().join("home");
        (temp, home_dir)
    }

    #[test]
    fn gate_decision_metadata_is_removed_and_exact_history_is_injected() {
        let mut input = BTreeMap::from([(
            GATE_DECISION_META_KEY.to_string(),
            KernelValue::Object(BTreeMap::from([
                (
                    "decision_field".to_string(),
                    KernelValue::String("gate_decision".to_string()),
                ),
                (
                    "gate_step_id".to_string(),
                    KernelValue::String("review".to_string()),
                ),
                (
                    "gate_id".to_string(),
                    KernelValue::String("implementation-review".to_string()),
                ),
            ])),
        )]);
        let binding = take_gate_decision_binding(&mut input)
            .expect("binding parses")
            .expect("binding exists");
        assert!(!input.contains_key(GATE_DECISION_META_KEY));

        inject_gate_decision_record(
            &mut input,
            binding,
            transist_protocol::GateDecisionRecord {
                review_request: None,
                schema: "transist.gate_decision.v1".to_string(),
                run_id: "run-1".to_string(),
                gate_id: "implementation-review".to_string(),
                step_id: "review".to_string(),
                decision: "request_changes".to_string(),
                actor: None,
                actor_source: None,
                reason: Some("keep the regression case".to_string()),
                decided_at_ms: Some(42),
            },
        )
        .expect("record injects");

        assert_eq!(
            serde_json::Value::from(input.remove("gate_decision").expect("decision"))["reason"],
            "keep the regression case"
        );
    }

    pub(super) fn test_invocation_paths(
        workspace_dir: &Path,
        home_dir: &Path,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> InvokePaths {
        let run_root = home_dir.join("runs").join(format!("run_{run_id}"));
        let execution_dir = run_root
            .join("artifacts")
            .join(step_id)
            .join(format!("execution_{execution:03}"));
        InvokePaths {
            run_root: run_root.display().to_string(),
            artifacts_dir: execution_dir.join("artifacts").display().to_string(),
            evidence_dir: execution_dir.join("evidence").display().to_string(),
            workspace_dir: workspace_dir.display().to_string(),
        }
    }

    fn install_shell_plugin(home_dir: &Path) {
        let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell/plugin.yaml");
        let source = plugin.parent().expect("shell plugin source");
        copy_plugin_dir(source, &home_dir.join("plugins/shell/0.1.0"));
    }

    fn copy_plugin_dir(source: &Path, target: &Path) {
        fs::create_dir_all(target).expect("plugin target");
        for entry in fs::read_dir(source).expect("plugin source") {
            let entry = entry.expect("plugin entry");
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            if source_path.is_dir() {
                copy_plugin_dir(&source_path, &target_path);
            } else {
                fs::copy(source_path, target_path).expect("plugin file");
            }
        }
    }

    #[test]
    fn make_execution_request_returns_none_for_control_step() {
        let step = RecipeStep {
            id: "control".to_string(),
            execution: StepExecutionDescriptor::Control {
                primitive: "noop".to_string(),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let request = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("."),
                home_dir: Path::new("./.transist"),
                recipe_source_dir: None,
                run_id: "run_1",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_1",
                local_only_mode: false,
            },
            &step,
        );
        assert!(request.expect("request").is_none());
        for version in [2, 3, u32::MAX] {
            let error = make_execution_request_unchecked(
                ExecutionRequestContext {
                    repo_root: Path::new("."),
                    home_dir: Path::new("./.transist"),
                    recipe_source_dir: None,
                    run_id: "run_1",
                    workflow_id: "wf",
                    workflow_inputs: &BTreeMap::new(),
                    workflow_vars: &BTreeMap::new(),
                    execution: 1,
                    invoke_id: "invoke_1",
                    local_only_mode: false,
                },
                &step,
                false,
                version,
                true,
                &[],
                &|_, _, _, _| panic!("unsupported revision must not resolve invocation paths"),
                &DefaultTestPluginMetadata,
                &DefaultTestPromptFiles,
                None,
                None,
                false,
                &BTreeSet::new(),
            )
            .expect_err("unsupported dispatch revision");
            assert!(
                error
                    .to_string()
                    .contains(&format!("unsupported asset lock version {version}"))
            );
        }
    }

    #[test]
    fn rejects_home_inside_workspace_for_invoke_dispatch() {
        let tmp = tempfile::tempdir().expect("workspace");
        let error = validate_home_workspace_boundary(&tmp.path().join(".transist"), tmp.path())
            .expect_err("workspace-nested home must be rejected");
        assert!(error.to_string().contains("runtime_home_inside_workspace"));
    }

    #[test]
    fn rejects_workspace_inside_home_for_invoke_dispatch() {
        let tmp = tempfile::tempdir().expect("home");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace dir");
        let error = validate_home_workspace_boundary(tmp.path(), &workspace)
            .expect_err("home-nested workspace must be rejected");
        assert!(error.to_string().contains("runtime_workspace_inside_home"));
    }

    #[test]
    fn make_execution_request_serializes_builtin_full_filesystem_policy() {
        let (_temp, home_dir) = temp_home_dir();
        seed_run_locator(&home_dir, "run_builtin");
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
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_builtin",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_builtin",
                local_only_mode: false,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["filesystem"]["source"], "builtin");
        assert_eq!(prepared.request.policy["network"]["source"], "builtin");
    }

    #[test]
    fn make_execution_request_emits_non_default_network_policy() {
        let (_temp, home_dir) = temp_home_dir();
        seed_run_locator(&home_dir, "run_network");
        let network_policy = transist_protocol::NetworkPolicy {
            mode: transist_protocol::NetworkAccessMode::Allowlist,
            allow_hosts: vec!["api.example.com".to_string()],
            allow_ports: vec![443],
            tls_required: true,
            max_redirects: 5,
        };
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
            filesystem_policy: Default::default(),
            network_policy: Some(network_policy.clone()),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: network_policy,
            effective_network_policy_source: Some(transist_protocol::NetworkPolicySource::Step),
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_network",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_network",
                local_only_mode: false,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["network"]["source"], "step");
        assert_eq!(prepared.request.policy["network"]["mode"], "allowlist");
        assert_eq!(
            prepared.request.policy["network"]["allow_hosts"][0],
            "api.example.com"
        );
        assert_eq!(prepared.request.policy["network"]["allow_ports"][0], 443);
        assert_eq!(prepared.request.policy["network"]["tls_required"], true);
        assert_eq!(prepared.request.policy["network"]["max_redirects"], 5);
    }

    #[test]
    fn make_execution_request_defaults_local_only_local_plugins_to_network_loopback() {
        let (_temp, home_dir) = temp_home_dir();
        let plugin_dir = home_dir.join("plugins").join("local.shell").join("1.0.0");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: local.shell
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
actions:
  - name: exec
    input_schema: input.json
    output_schema: output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        seed_run_locator(&home_dir, "run_local_only_network");
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("local.shell".to_string()),
                plugin: "local.shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: Some("1.0.0".to_string()),
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: Default::default(),
            effective_network_policy_source: Some(NetworkPolicySource::Builtin),
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_local_only_network",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_local_only_network",
                local_only_mode: true,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["network"]["mode"], "loopback");
        assert_eq!(prepared.request.policy["network"]["source"], "builtin");
    }

    #[test]
    fn make_execution_request_local_only_local_process_caps_authored_allowlist() {
        let (_temp, home_dir) = temp_home_dir();
        let plugin_dir = home_dir.join("plugins").join("local.shell").join("1.0.0");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: local.shell
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
actions:
  - name: exec
    input_schema: input.json
    output_schema: output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        seed_run_locator(&home_dir, "run_local_only_allowlist");
        let network_policy = transist_protocol::NetworkPolicy {
            mode: transist_protocol::NetworkAccessMode::Allowlist,
            allow_hosts: vec!["api.example.com".to_string()],
            allow_ports: vec![443],
            tls_required: true,
            max_redirects: 5,
        };
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("local.shell".to_string()),
                plugin: "local.shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: Some("1.0.0".to_string()),
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Some(network_policy.clone()),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: network_policy,
            effective_network_policy_source: Some(NetworkPolicySource::Step),
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_local_only_allowlist",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_local_only_allowlist",
                local_only_mode: true,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["network"]["mode"], "loopback");
        assert_eq!(prepared.request.policy["network"]["source"], "builtin");
        assert_eq!(
            prepared.request.policy["network"]["allow_hosts"][0],
            "api.example.com"
        );
    }

    #[test]
    fn make_execution_request_local_only_local_process_caps_authored_any_with_metadata() {
        let (_temp, home_dir) = temp_home_dir();
        let plugin_dir = home_dir.join("plugins").join("local.shell").join("1.0.0");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: local.shell
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
actions:
  - name: exec
    input_schema: input.json
    output_schema: output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        seed_run_locator(&home_dir, "run_local_only_any_meta");
        let network_policy = transist_protocol::NetworkPolicy {
            mode: transist_protocol::NetworkAccessMode::Any,
            allow_hosts: vec!["evil.example.com".to_string()],
            allow_ports: vec![443],
            tls_required: false,
            max_redirects: 0,
        };
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("local.shell".to_string()),
                plugin: "local.shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: Some("1.0.0".to_string()),
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Some(network_policy.clone()),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: network_policy,
            effective_network_policy_source: Some(NetworkPolicySource::Step),
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_local_only_any_meta",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_local_only_any_meta",
                local_only_mode: true,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["network"]["mode"], "loopback");
        assert_eq!(prepared.request.policy["network"]["source"], "builtin");
    }

    #[test]
    fn make_execution_request_local_only_local_process_preserves_authored_none() {
        let (_temp, home_dir) = temp_home_dir();
        let plugin_dir = home_dir.join("plugins").join("local.shell").join("1.0.0");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: local.shell
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
actions:
  - name: exec
    input_schema: input.json
    output_schema: output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        seed_run_locator(&home_dir, "run_local_only_none");
        let network_policy = transist_protocol::NetworkPolicy {
            mode: transist_protocol::NetworkAccessMode::None,
            allow_hosts: vec![],
            allow_ports: vec![],
            tls_required: false,
            max_redirects: 0,
        };
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("local.shell".to_string()),
                plugin: "local.shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: Some("1.0.0".to_string()),
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Some(network_policy.clone()),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: network_policy,
            effective_network_policy_source: Some(NetworkPolicySource::Step),
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_local_only_none",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_local_only_none",
                local_only_mode: true,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["network"]["mode"], "none");
        assert_eq!(prepared.request.policy["network"]["source"], "step");
    }

    #[test]
    fn make_execution_request_local_only_local_process_preserves_authored_loopback() {
        let (_temp, home_dir) = temp_home_dir();
        let plugin_dir = home_dir.join("plugins").join("local.shell").join("1.0.0");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: local.shell
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
actions:
  - name: exec
    input_schema: input.json
    output_schema: output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        seed_run_locator(&home_dir, "run_local_only_loopback");
        let network_policy = transist_protocol::NetworkPolicy {
            mode: transist_protocol::NetworkAccessMode::Loopback,
            allow_hosts: vec![],
            allow_ports: vec![],
            tls_required: false,
            max_redirects: 0,
        };
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("local.shell".to_string()),
                plugin: "local.shell".to_string(),
                action: "exec".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: Some("1.0.0".to_string()),
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Some(network_policy.clone()),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: network_policy,
            effective_network_policy_source: Some(NetworkPolicySource::Step),
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_local_only_loopback",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_local_only_loopback",
                local_only_mode: true,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["network"]["mode"], "loopback");
        assert_eq!(prepared.request.policy["network"]["source"], "step");
    }

    #[test]
    fn make_execution_request_local_only_http_keeps_authored_network_narrowing() {
        let (_temp, home_dir) = temp_home_dir();
        let plugin_dir = home_dir.join("http.policy");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: http.policy
version: 1.0.0
adapter: http
entrypoint:
  type: http
  base_url: http://127.0.0.1:8080
actions:
  - name: echo
    input_schema: input.json
    output_schema: output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        fs::write(plugin_dir.join("input.json"), "{}").expect("input schema");
        fs::write(plugin_dir.join("output.json"), "{}").expect("output schema");
        copy_plugin_dir(&plugin_dir, &home_dir.join("plugins/http.policy/1.0.0"));
        seed_run_locator(&home_dir, "run_local_only_http_network");
        let network_policy = transist_protocol::NetworkPolicy {
            mode: transist_protocol::NetworkAccessMode::Allowlist,
            allow_hosts: vec!["127.0.0.1".to_string()],
            allow_ports: vec![8080],
            tls_required: false,
            max_redirects: 2,
        };
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Http,
                plugin_id: Some("http.policy".to_string()),
                plugin: "http.policy".to_string(),
                action: "echo".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Some(network_policy.clone()),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: network_policy,
            effective_network_policy_source: Some(NetworkPolicySource::Step),
        };

        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_local_only_http_network",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_local_only_http_network",
                local_only_mode: true,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");

        assert_eq!(prepared.request.policy["network"]["mode"], "loopback");
        assert_eq!(
            prepared.request.policy["network"]["allow_hosts"][0],
            "127.0.0.1"
        );
        assert_eq!(prepared.request.policy["network"]["allow_ports"][0], 8080);
    }

    #[test]
    fn make_execution_request_populates_new_identity() {
        let (_temp, home_dir) = temp_home_dir();
        seed_run_locator(&home_dir, "run_2");
        let mut with = BTreeMap::new();
        with.insert(
            "timeout_ms".to_string(),
            KernelValue::Number(serde_json::Number::from(250_u64)),
        );
        with.insert(
            "max_timeout_ms".to_string(),
            KernelValue::Number(serde_json::Number::from(1_500_u64)),
        );
        with.insert(
            "secret_refs".to_string(),
            KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
        );
        with.insert(
            "message".to_string(),
            KernelValue::String("hello".to_string()),
        );
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
            with,
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: Some(Path::new("/tmp/workflow")),
                run_id: "run_2",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 2,
                invoke_id: "invoke_2",
                local_only_mode: false,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");
        assert_eq!(prepared.request.action, "exec");
        assert_eq!(prepared.request.adapter_kind, AdapterKind::Process);
        assert_eq!(prepared.request.timeout_ms, Some(250));
        assert_eq!(prepared.request.max_timeout_ms, Some(1_500));
        assert_eq!(prepared.secret_refs, vec!["API_TOKEN".to_string()]);
        assert_eq!(prepared.request.metadata["plugin_id"], "shell");
        assert_eq!(
            prepared.request.metadata["recipe_source_dir"],
            "/tmp/workflow"
        );
        assert_eq!(prepared.request.step.execution, 2);
        assert_eq!(prepared.request.paths.workspace_dir, "/tmp/repo");
        assert_eq!(
            prepared.request.paths.run_root,
            home_dir
                .join("runs")
                .join("run_run_2")
                .display()
                .to_string()
        );
    }

    #[test]
    fn make_execution_request_rejects_undeclared_secret_refs_for_pinned_actions() {
        let (_temp, home_dir) = temp_home_dir();
        fs::create_dir_all(&home_dir).expect("home dir");
        install_shell_plugin(&home_dir);
        seed_run_locator(&home_dir, "run_undeclared_secret");
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("shell".to_string()),
                plugin: "shell".to_string(),
                action: "echo".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: Some("0.1.0".to_string()),
                plugin_sha256: None,
            },
            with: BTreeMap::from([(
                "secret_refs".to_string(),
                KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
            )]),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };

        let error = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_undeclared_secret",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_undeclared_secret",
                local_only_mode: false,
            },
            &step,
        )
        .expect_err("undeclared secret ref must fail");
        assert!(
            error
                .to_string()
                .contains("undeclared secret ref 'API_TOKEN'")
        );
    }

    #[test]
    fn make_execution_request_rejects_missing_required_secret_refs_for_pinned_actions() {
        let (_temp, home_dir) = temp_home_dir();
        fs::create_dir_all(&home_dir).expect("home dir");
        install_shell_plugin(&home_dir);
        seed_run_locator(&home_dir, "run_missing_secret");
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("shell".to_string()),
                plugin: "shell".to_string(),
                action: "exec_with_secret_env".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: Some("0.1.0".to_string()),
                plugin_sha256: None,
            },
            with: BTreeMap::from([
                ("command".to_string(), KernelValue::String("sh".to_string())),
                (
                    "args".to_string(),
                    KernelValue::Array(vec![KernelValue::String("-c".to_string())]),
                ),
            ]),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };

        let error = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_missing_secret",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_missing_secret",
                local_only_mode: false,
            },
            &step,
        )
        .expect_err("missing required secret ref must fail");
        assert!(
            error
                .to_string()
                .contains("missing required secret ref 'MY_API_TOKEN'")
        );
    }

    #[test]
    fn make_execution_request_resolves_exact_input_placeholders() {
        let (_temp, home_dir) = temp_home_dir();
        seed_run_locator(&home_dir, "run_2");
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
                "ticket".to_string(),
                KernelValue::String("{{inputs.ticket}}".to_string()),
            )]),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_2",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::from([(
                    "ticket".to_string(),
                    KernelValue::String("JIRA-1".to_string()),
                )]),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_3",
                local_only_mode: false,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");
        assert_eq!(prepared.request.input["ticket"], "JIRA-1");
    }

    #[test]
    fn make_execution_request_injects_prompt_binding() {
        let (_temp, home_dir) = temp_home_dir();
        seed_run_locator(&home_dir, "run_3");
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("codex.cli".to_string()),
                plugin: "codex.cli".to_string(),
                action: "exec_task".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: Some(RecipePromptBinding {
                inline_field: "prompt".to_string(),
                file_field: Some("prompt_file".to_string()),
                source: RecipePromptSource::Inline {
                    value: "{{inputs.prompt}}".to_string(),
                },
            }),
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_3",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::from([(
                    "prompt".to_string(),
                    KernelValue::String("Say hello".to_string()),
                )]),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_4",
                local_only_mode: false,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");
        assert_eq!(prepared.request.input["prompt"], "Say hello");
    }

    #[test]
    fn make_execution_request_resolves_prompt_file_relative_to_recipe_source_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_temp = tempfile::tempdir().expect("home tempdir");
        let home_dir = home_temp.path().join("home");
        std::fs::create_dir_all(tmp.path().join("prompts")).expect("prompt directory");
        std::fs::write(tmp.path().join("prompts/task.md"), "Hello from file\n")
            .expect("prompt fixture");
        seed_run_locator(&home_dir, "run_4");
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("goose.cli".to_string()),
                plugin: "goose.cli".to_string(),
                action: "run_task".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: Some(RecipePromptBinding {
                inline_field: "text".to_string(),
                file_field: Some("text_file".to_string()),
                source: RecipePromptSource::File {
                    path: "prompts/task.md".to_string(),
                },
            }),
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: tmp.path(),
                home_dir: &home_dir,
                recipe_source_dir: Some(tmp.path()),
                run_id: "run_4",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_5",
                local_only_mode: false,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");
        assert_eq!(prepared.request.input["text"], "Hello from file\n");
    }

    #[test]
    fn make_execution_request_rejects_missing_prompt_file() {
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("goose.cli".to_string()),
                plugin: "goose.cli".to_string(),
                action: "run_task".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: Some(RecipePromptBinding {
                inline_field: "text".to_string(),
                file_field: Some("text_file".to_string()),
                source: RecipePromptSource::File {
                    path: "missing.md".to_string(),
                },
            }),
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let err = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: Path::new("/tmp/home"),
                recipe_source_dir: Some(Path::new("/tmp/source")),
                run_id: "run_5",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_6",
                local_only_mode: false,
            },
            &step,
        )
        .expect_err("missing prompt file");
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn make_execution_request_rejects_ignored_prompt_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_temp = tempfile::tempdir().expect("home tempdir");
        let home_dir = home_temp.path().join("home");
        seed_run_locator(&home_dir, "run_5b");
        let source_dir = tmp.path().join("workflow-source");
        let prompt_dir = tmp.path().join("runtime-prompts");
        std::fs::create_dir_all(&source_dir).expect("source dir");
        std::fs::create_dir_all(&prompt_dir).expect("prompt dir");
        let prompt_path = prompt_dir.join("secret.md");
        std::fs::write(&prompt_path, "Hidden prompt\n").expect("prompt file");

        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("goose.cli".to_string()),
                plugin: "goose.cli".to_string(),
                action: "run_task".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: Some(RecipePromptBinding {
                inline_field: "text".to_string(),
                file_field: Some("text_file".to_string()),
                source: RecipePromptSource::File {
                    path: prompt_path.display().to_string(),
                },
            }),
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Some(transist_protocol::FilesystemPolicy {
                workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                    mode: transist_protocol::WorkspaceAccessMode::Custom,
                }),
                read_allow_paths: Vec::new(),
                write_allow_paths: Vec::new(),
            }),
            network_policy: Default::default(),
            effective_filesystem_policy: transist_protocol::FilesystemPolicy {
                workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
                    mode: transist_protocol::WorkspaceAccessMode::Custom,
                }),
                read_allow_paths: Vec::new(),
                write_allow_paths: Vec::new(),
            },
            effective_filesystem_policy_source: Some(
                transist_protocol::FilesystemPolicySource::Step,
            ),
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };

        let err = make_execution_request(
            ExecutionRequestContext {
                repo_root: tmp.path(),
                home_dir: &home_dir,
                recipe_source_dir: Some(&source_dir),
                run_id: "run_5b",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_6b",
                local_only_mode: false,
            },
            &step,
        )
        .expect_err("ignored prompt file");
        let denial = err
            .downcast_ref::<crate::filesystem_policy::FilesystemDenial>()
            .expect("filesystem denial");
        assert_eq!(denial.step_id, "invoke");
        assert_eq!(denial.surface, "prompt_file");
        assert_eq!(denial.source, FilesystemPolicySource::Step);
        assert_eq!(denial.mode, transist_protocol::WorkspaceAccessMode::Custom);
        assert_eq!(
            denial.operation,
            crate::filesystem_policy::FilesystemOperation::Read
        );
        assert_eq!(
            denial.matched_pattern,
            "<filesystem_custom_mode_unsupported>"
        );
        assert_eq!(denial.workspace_relative_path, "runtime-prompts/secret.md");
    }

    #[test]
    fn make_execution_request_injects_skills_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home_temp = tempfile::tempdir().expect("home tempdir");
        let home_dir = home_temp.path().join("home");
        seed_run_locator(&home_dir, "run_6");
        let workspace_dir = temp.path().join("workspace");
        let source_dir = temp.path().join("workflow-source");
        let skill_dir = source_dir.join("skill");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(skill_dir.join("SKILL.md"), "Use the skill").expect("skill file");
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("codex.cli".to_string()),
                plugin: "codex.cli".to_string(),
                action: "exec_task".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: Some(RecipeSkillsBinding {
                bundle_field: "skills_bundle".to_string(),
                rendered_field: "skills_rendered".to_string(),
                bundle: RecipeSkillBundle {
                    version: "v1".to_string(),
                    authored_refs: vec![RecipeSkillRef {
                        raw: "./skill".to_string(),
                        resolved_path: skill_dir.display().to_string(),
                        resolution_kind: "skill_root".to_string(),
                    }],
                    roots: vec![RecipeSkillRoot {
                        skill_name: "example".to_string(),
                        skill_root: skill_dir.display().to_string(),
                        skill_file: skill_dir.join("SKILL.md").display().to_string(),
                        documents: vec![RecipeSkillDocument {
                            logical_path: "SKILL.md".to_string(),
                            resolved_path: skill_dir.join("SKILL.md").display().to_string(),
                            content: "Use the skill".to_string(),
                        }],
                    }],
                },
                rendered: "# Workflow Skills\n".to_string(),
            }),
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let prepared = make_execution_request(
            ExecutionRequestContext {
                repo_root: &workspace_dir,
                home_dir: &home_dir,
                recipe_source_dir: Some(&source_dir),
                run_id: "run_6",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_7",
                local_only_mode: false,
            },
            &step,
        )
        .expect("invoke request")
        .expect("invoke");
        assert_eq!(prepared.request.input["skills_bundle"]["version"], "v1");
        assert_eq!(
            prepared.request.input["skills_rendered"],
            "# Workflow Skills\n"
        );
    }

    #[test]
    fn make_execution_request_rejects_outside_workspace_skill_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home_temp = tempfile::tempdir().expect("home tempdir");
        let home_dir = home_temp.path().join("home");
        seed_run_locator(&home_dir, "run_6b");
        let outside = tempfile::tempdir().expect("outside skill dir");
        std::fs::write(outside.path().join("SKILL.md"), "Use the outside skill")
            .expect("outside skill file");
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("codex.cli".to_string()),
                plugin: "codex.cli".to_string(),
                action: "exec_task".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: Some(RecipeSkillsBinding {
                bundle_field: "skills_bundle".to_string(),
                rendered_field: "skills_rendered".to_string(),
                bundle: RecipeSkillBundle {
                    version: "v1".to_string(),
                    authored_refs: vec![RecipeSkillRef {
                        raw: "../outside".to_string(),
                        resolved_path: outside.path().display().to_string(),
                        resolution_kind: "skill_root".to_string(),
                    }],
                    roots: vec![RecipeSkillRoot {
                        skill_name: "outside".to_string(),
                        skill_root: outside.path().display().to_string(),
                        skill_file: outside.path().join("SKILL.md").display().to_string(),
                        documents: vec![RecipeSkillDocument {
                            logical_path: "SKILL.md".to_string(),
                            resolved_path: outside.path().join("SKILL.md").display().to_string(),
                            content: "Use the outside skill".to_string(),
                        }],
                    }],
                },
                rendered: "# Workflow Skills\n".to_string(),
            }),
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: Some(FilesystemPolicySource::Builtin),
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let err = make_execution_request(
            ExecutionRequestContext {
                repo_root: temp.path(),
                home_dir: &home_dir,
                recipe_source_dir: None,
                run_id: "run_6b",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_7b",
                local_only_mode: false,
            },
            &step,
        )
        .expect_err("outside workspace skill bundle");
        let denial = err
            .downcast_ref::<crate::filesystem_policy::FilesystemDenial>()
            .expect("filesystem denial");
        assert_eq!(denial.surface, "skills.authored_ref");
        assert_eq!(denial.source, FilesystemPolicySource::Builtin);
        assert_eq!(denial.mode, transist_protocol::WorkspaceAccessMode::Full);
        assert_eq!(
            denial.operation,
            crate::filesystem_policy::FilesystemOperation::Read
        );
        assert_eq!(denial.matched_pattern, "<outside_workspace>");
    }

    #[test]
    fn make_execution_request_rejects_skills_binding_collisions() {
        let step = RecipeStep {
            id: "invoke".to_string(),
            execution: StepExecutionDescriptor::Invoke {
                adapter: AdapterKind::Process,
                plugin_id: Some("codex.cli".to_string()),
                plugin: "codex.cli".to_string(),
                action: "exec_task".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                plugin_sha256: None,
            },
            with: BTreeMap::from([(
                "skills_bundle".to_string(),
                KernelValue::String("already present".to_string()),
            )]),
            prompt_binding: None,
            skills_binding: Some(RecipeSkillsBinding {
                bundle_field: "skills_bundle".to_string(),
                rendered_field: "skills_rendered".to_string(),
                bundle: RecipeSkillBundle {
                    version: "v1".to_string(),
                    authored_refs: Vec::new(),
                    roots: Vec::new(),
                },
                rendered: "skills".to_string(),
            }),
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        let err = make_execution_request(
            ExecutionRequestContext {
                repo_root: Path::new("/tmp/repo"),
                home_dir: Path::new("/tmp/home"),
                recipe_source_dir: None,
                run_id: "run_7",
                workflow_id: "wf",
                workflow_inputs: &BTreeMap::new(),
                workflow_vars: &BTreeMap::new(),
                execution: 1,
                invoke_id: "invoke_8",
                local_only_mode: false,
            },
            &step,
        )
        .expect_err("skills collision");
        assert!(err.to_string().contains("injected skills fields"));
    }

    #[test]
    fn resolve_string_value_concatenates_two_placeholders_with_separator() {
        let inputs: BTreeMap<String, KernelValue> = BTreeMap::from([
            (
                "workspace_root".to_string(),
                KernelValue::String("/tmp/root".to_string()),
            ),
            (
                "context_repo".to_string(),
                KernelValue::String("hardware".to_string()),
            ),
        ]);
        let rendered = resolve_string_value(
            "{{inputs.workspace_root}}/{{inputs.context_repo}}",
            &inputs,
            &BTreeMap::new(),
            true,
        )
        .expect("render");
        assert_eq!(
            rendered,
            KernelValue::String("/tmp/root/hardware".to_string())
        );
    }

    #[test]
    fn resolve_string_value_concatenates_two_adjacent_placeholders() {
        let inputs: BTreeMap<String, KernelValue> = BTreeMap::from([
            ("a".to_string(), KernelValue::String("foo".to_string())),
            ("b".to_string(), KernelValue::String("bar".to_string())),
        ]);
        let rendered =
            resolve_string_value("{{inputs.a}}{{inputs.b}}", &inputs, &BTreeMap::new(), true)
                .expect("render");
        assert_eq!(rendered, KernelValue::String("foobar".to_string()));
    }

    #[test]
    fn resolve_string_value_renders_literal_with_one_placeholder() {
        let inputs: BTreeMap<String, KernelValue> = BTreeMap::from([(
            "context_repo".to_string(),
            KernelValue::String("hardware".to_string()),
        )]);
        let rendered = resolve_string_value(
            "/tmp/root/{{inputs.context_repo}}",
            &inputs,
            &BTreeMap::new(),
            true,
        )
        .expect("render");
        assert_eq!(
            rendered,
            KernelValue::String("/tmp/root/hardware".to_string())
        );
    }

    #[test]
    fn resolve_string_value_single_exact_placeholder_preserves_structured_values() {
        // The single-exact-placeholder fast path keeps returning the raw
        // KernelValue for structured inputs (arrays, objects) so callers can
        // pass them through templates without losing type. Scalars are
        // stringified by `coerce_exact_placeholder_value` and covered by the
        // dedicated tests below.
        let inputs: BTreeMap<String, KernelValue> = BTreeMap::from([(
            "tags".to_string(),
            KernelValue::Array(vec![KernelValue::String("alpha".to_string())]),
        )]);
        let rendered = resolve_string_value("{{inputs.tags}}", &inputs, &BTreeMap::new(), true)
            .expect("render");
        assert_eq!(
            rendered,
            KernelValue::Array(vec![KernelValue::String("alpha".to_string())])
        );
    }

    #[test]
    fn resolve_string_value_single_exact_placeholder_stringifies_number() {
        // Regression: an exact `{{inputs.<numeric>}}` placeholder used to
        // preserve `KernelValue::Number`, which then failed plugin input
        // schemas requiring `args: string[]` at preflight.
        // Now scalars are coerced to string so the YAML author's
        // `args: ['{{inputs.required_pr_approvals}}']` works whether the
        // workflow input is typed as number or string.
        let inputs: BTreeMap<String, KernelValue> = BTreeMap::from([(
            "required_pr_approvals".to_string(),
            KernelValue::Number(serde_json::Number::from(1)),
        )]);
        let rendered = resolve_string_value(
            "{{inputs.required_pr_approvals}}",
            &inputs,
            &BTreeMap::new(),
            true,
        )
        .expect("render");
        assert_eq!(rendered, KernelValue::String("1".to_string()));
    }

    #[test]
    fn resolve_string_value_single_exact_placeholder_stringifies_bool() {
        // Same coercion contract as for numbers: an exact placeholder
        // resolving to a boolean is stringified to "true" / "false".
        let inputs: BTreeMap<String, KernelValue> =
            BTreeMap::from([("dry_run".to_string(), KernelValue::Bool(true))]);
        let rendered = resolve_string_value("{{inputs.dry_run}}", &inputs, &BTreeMap::new(), true)
            .expect("render");
        assert_eq!(rendered, KernelValue::String("true".to_string()));
    }

    #[test]
    fn resolve_string_value_single_exact_placeholder_passes_through_null() {
        // Null in the fast path remains a passthrough so optional `with:`
        // fields can keep using `'{{inputs.optional_thing}}'` as a value
        // without erroring at preflight. Bare-string contexts that disallow
        // null get a separate, louder error via `stringify_scalar_input`
        // (see `resolve_string_value_errors_on_null_input_inside_larger_string`).
        let inputs: BTreeMap<String, KernelValue> =
            BTreeMap::from([("missing".to_string(), KernelValue::Null)]);
        let rendered = resolve_string_value("{{inputs.missing}}", &inputs, &BTreeMap::new(), true)
            .expect("null fast-path placeholder should pass through");
        assert_eq!(rendered, KernelValue::Null);
    }

    #[test]
    fn resolve_string_value_errors_on_null_input_inside_larger_string() {
        // A missing/null input interpolated into a larger string used to
        // silently render the literal substring "null"; that hides authoring
        // mistakes and produces broken paths. It must now fail loudly so the
        // problem surfaces at preflight rather than at runtime.
        let inputs: BTreeMap<String, KernelValue> =
            BTreeMap::from([("context_repo".to_string(), KernelValue::Null)]);
        let err = resolve_string_value(
            "/tmp/{{inputs.context_repo}}/x",
            &inputs,
            &BTreeMap::new(),
            true,
        )
        .expect_err("null interpolation should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("context_repo") && msg.contains("null"),
            "error should name the offending input and explain the null cause: {msg}"
        );
    }
}
