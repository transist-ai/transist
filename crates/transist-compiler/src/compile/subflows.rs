// SPDX-License-Identifier: MPL-2.0
use crate::WorkflowDefinitionSnapshot;
use crate::flow::{Flow, FlowStep, FlowSubflowCall};
use anyhow::{Result, bail};
use std::collections::BTreeSet;
use std::path::Path;
use transist_protocol::{
    FilesystemPolicy, NetworkAccessMode, NetworkPolicy, Recipe, WorkspaceAccessMode,
};

pub(crate) fn compile_subflow_recipe(
    flow: &Flow,
    step: &FlowStep,
    subflow: &FlowSubflowCall,
    home_dir: Option<&Path>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
    symbolic_contracts: bool,
) -> Result<(
    Recipe,
    std::collections::BTreeMap<String, transist_protocol::KernelValue>,
)> {
    let parent_path = flow.source_path.as_ref().ok_or_else(|| {
        anyhow::anyhow!("subflow step '{}' requires a source-backed flow", step.id)
    })?;
    let child_path = crate::io::resolve_relative_path(parent_path, &subflow.path);
    let mut child_flow = match snapshot {
        Some(snapshot) => snapshot.flow_for_source(&child_path)?,
        None => crate::io::load_child_flow_unvalidated(&child_path)?,
    };
    inherit_module_scope(flow, step, &mut child_flow)?;
    for key in subflow.inputs.keys() {
        if !child_flow.inputs.contains_key(key) {
            bail!(
                "subflow step '{}' binds unknown child input '{}'",
                step.id,
                key
            );
        }
    }
    let declared_child = child_flow.with_declarations_applied()?;
    let outputs = super::inputs::yaml_map_to_kernel_with_serializer(
        &declared_child.outputs,
        &child_flow.id,
        &step.id,
        child_flow.source_path.as_deref(),
        "module outputs",
        super::inputs::serialize_yaml_value_default,
    )?;
    let recipe = super::compile_flow_internal_mode(
        &child_flow,
        home_dir,
        super::inputs::serialize_yaml_value_default,
        snapshot,
        symbolic_contracts,
    )?;
    Ok((recipe, outputs))
}

pub(crate) fn inherit_module_scope(parent: &Flow, step: &FlowStep, child: &mut Flow) -> Result<()> {
    if !child.documents.is_empty() && child.documents != parent.documents {
        bail!(
            "module step '{}' cannot introduce or shadow root documents",
            step.id
        );
    }
    child.documents = parent.documents.clone();
    for (plugin, version) in &child.plugins {
        if parent.plugins.get(plugin) != Some(version) {
            bail!(
                "module step '{}' references plugin '{}@{}' outside the root plugin declarations",
                step.id,
                plugin,
                version
            );
        }
    }
    validate_workspace_narrowing(parent, child, &step.id)?;
    validate_network_narrowing(
        parent.network_policy.as_ref(),
        child.network_policy.as_ref(),
        &step.id,
    )?;
    validate_filesystem_narrowing(
        parent.filesystem_policy.as_ref(),
        child.filesystem_policy.as_ref(),
        &step.id,
    )?;

    child.id = format!("{}-module-{}", parent.id, step.id);
    child.version = parent.version.clone();
    child.plugins = parent.plugins.clone();
    child.workspace = child.workspace.clone().or_else(|| parent.workspace.clone());
    child.network_policy = child
        .network_policy
        .clone()
        .or_else(|| parent.network_policy.clone());
    child.filesystem_policy = child
        .filesystem_policy
        .clone()
        .or_else(|| parent.filesystem_policy.clone());
    for (name, spec) in &parent.inputs {
        child
            .inputs
            .entry(name.clone())
            .or_insert_with(|| spec.clone());
    }
    for (name, value) in &parent.context {
        child
            .context
            .entry(name.clone())
            .or_insert_with(|| value.clone());
    }
    for (name, artifact) in &parent.artifacts {
        child
            .artifacts
            .entry(name.clone())
            .or_insert_with(|| artifact.clone());
    }
    Ok(())
}

fn validate_workspace_narrowing(parent: &Flow, child: &Flow, step_id: &str) -> Result<()> {
    let Some(child) = &child.workspace else {
        return Ok(());
    };
    let Some(parent) = &parent.workspace else {
        bail!("module step '{step_id}' cannot introduce a workspace absent from the root");
    };
    if child.path != parent.path {
        bail!("module step '{step_id}' cannot change the root workspace path");
    }
    if parent.access == WorkspaceAccessMode::ReadOnly
        && child.access != WorkspaceAccessMode::ReadOnly
    {
        bail!("module step '{step_id}' cannot widen read-only root workspace access");
    }
    Ok(())
}

fn validate_network_narrowing(
    parent: Option<&NetworkPolicy>,
    child: Option<&NetworkPolicy>,
    step_id: &str,
) -> Result<()> {
    let (Some(parent), Some(child)) = (parent, child) else {
        return Ok(());
    };
    let mode_allowed = match parent.mode {
        NetworkAccessMode::Any => true,
        NetworkAccessMode::Allowlist => matches!(
            child.mode,
            NetworkAccessMode::Allowlist | NetworkAccessMode::None
        ),
        NetworkAccessMode::Loopback => {
            matches!(
                child.mode,
                NetworkAccessMode::Loopback | NetworkAccessMode::None
            )
        }
        NetworkAccessMode::None => child.mode == NetworkAccessMode::None,
    };
    let parent_hosts = parent.allow_hosts.iter().collect::<BTreeSet<_>>();
    let parent_ports = parent.allow_ports.iter().collect::<BTreeSet<_>>();
    if !mode_allowed
        || (parent.mode != NetworkAccessMode::Any
            && (!child
                .allow_hosts
                .iter()
                .all(|host| parent_hosts.contains(host))
                || !child
                    .allow_ports
                    .iter()
                    .all(|port| parent_ports.contains(port))))
        || (parent.mode != NetworkAccessMode::Any
            && ((parent.tls_required && !child.tls_required)
                || child.max_redirects > parent.max_redirects))
    {
        bail!("module step '{step_id}' cannot widen the root network policy");
    }
    Ok(())
}

fn validate_filesystem_narrowing(
    parent: Option<&FilesystemPolicy>,
    child: Option<&FilesystemPolicy>,
    step_id: &str,
) -> Result<()> {
    let Some(child) = child else {
        return Ok(());
    };
    let parent_read = parent
        .into_iter()
        .flat_map(|policy| &policy.read_allow_paths)
        .collect::<BTreeSet<_>>();
    let parent_write = parent
        .into_iter()
        .flat_map(|policy| &policy.write_allow_paths)
        .collect::<BTreeSet<_>>();
    let parent_read_only = parent
        .and_then(|policy| policy.workspace_access.as_ref())
        .as_ref()
        .is_some_and(|access| access.mode == WorkspaceAccessMode::ReadOnly);
    let child_widens_workspace = parent_read_only
        && child
            .workspace_access
            .as_ref()
            .is_none_or(|access| access.mode != WorkspaceAccessMode::ReadOnly);
    if child_widens_workspace
        || !child
            .read_allow_paths
            .iter()
            .all(|path| parent_read.contains(path))
        || !child
            .write_allow_paths
            .iter()
            .all(|path| parent_write.contains(path))
    {
        bail!("module step '{step_id}' cannot widen the root filesystem policy");
    }
    Ok(())
}
