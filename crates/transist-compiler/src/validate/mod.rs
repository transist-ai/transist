// SPDX-License-Identifier: MPL-2.0
mod cleanup;
mod control_flow;
mod dataflow;
mod inputs;
mod steps;

use crate::WorkflowDefinitionSnapshot;
use crate::flow::{FLOW_SCHEMA_V1, Flow, FlowStep, UNDECLARED_PLUGIN_MARKER};
use crate::settings::load_home_settings;
use anyhow::{Context, Result, bail};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::fmt;
use std::path::{Path, PathBuf};
use transist_plugins::{
    ActionDescriptor, ActionGateDecisionBinding, ActionPromptBinding, ActionSkillsBinding,
    AdapterKind, PluginVersionSelector, resolve_plugin_in_home,
    resolve_plugin_with_selector_in_home,
};
use transist_prerequisites::PrerequisiteSet;
use transist_prerequisites::validate_prerequisite_set;
use transist_protocol::WorkspaceAccessMode;

/// Ordered static diagnostics found while validating one authored workflow.
///
/// Parsing and I/O errors remain fail-fast because no workflow model is
/// available to inspect. Once a flow is loaded, independent semantic checks
/// are collected so authors can fix them in one pass.
#[derive(Debug)]
struct FlowValidationErrors {
    diagnostics: Vec<String>,
}

impl fmt::Display for FlowValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "{} workflow validation error(s):",
            self.diagnostics.len()
        )?;
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            writeln!(formatter, "  {}. {diagnostic}", index + 1)?;
        }
        Ok(())
    }
}

impl StdError for FlowValidationErrors {}

fn collect_diagnostic(diagnostics: &mut Vec<String>, result: Result<()>) {
    if let Err(error) = result {
        let aggregate = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<FlowValidationErrors>());
        if let Some(aggregate) = aggregate {
            let context = error
                .chain()
                .take_while(|cause| cause.downcast_ref::<FlowValidationErrors>().is_none())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(": ");
            for diagnostic in &aggregate.diagnostics {
                if context.is_empty() {
                    diagnostics.push(diagnostic.clone());
                } else {
                    diagnostics.push(format!("{context}: {diagnostic}"));
                }
            }
        } else {
            diagnostics.push(format!("{error:#}"));
        }
    }
}

fn finish_diagnostics(diagnostics: Vec<String>) -> Result<()> {
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(anyhow::Error::new(FlowValidationErrors { diagnostics }))
    }
}

/// Validates an authored flow without consulting local plugin installation state.
pub fn validate_flow(flow: &Flow) -> Result<()> {
    validate_flow_internal(
        flow,
        &mut Vec::new(),
        PromptValidationMode::Generic,
        None,
        None,
        None,
        false,
    )
}

pub(crate) fn validate_step_secret_refs_declared(
    home_dir: &Path,
    step: &FlowStep,
    step_idx: usize,
) -> Result<()> {
    let secret_refs = authored_secret_refs(&step.with);
    if secret_refs.is_empty() && step.plugin_version.is_none() {
        return Ok(());
    }

    let (_, action) = resolve_action_descriptor(home_dir, step, step_idx)?;
    let plugin_id = step.plugin.as_deref().unwrap_or_default();
    let action_name = step.action.as_deref().unwrap_or_default();
    let declared_secret_refs: BTreeSet<_> = action
        .secret_inputs
        .iter()
        .map(|secret| secret.name.as_str())
        .collect();
    let has_dynamic_secret_ref = secret_refs
        .iter()
        .any(|secret_ref| is_input_template(secret_ref));

    for secret_ref in &secret_refs {
        if !is_input_template(secret_ref) && !declared_secret_refs.contains(secret_ref.as_str()) {
            bail!(
                "flow.steps[{step_idx}] '{}' references undeclared secret ref '{}' for action '{}.{}'",
                step.id,
                secret_ref,
                plugin_id,
                action_name
            );
        }
    }

    for required_secret in action.secret_inputs.iter().filter(|secret| secret.required) {
        if !has_dynamic_secret_ref
            && !secret_refs
                .iter()
                .any(|secret_ref| secret_ref == &required_secret.name)
        {
            bail!(
                "flow.steps[{step_idx}] '{}' is missing required secret ref '{}' for action '{}.{}'",
                step.id,
                required_secret.name,
                plugin_id,
                action_name
            );
        }
    }

    Ok(())
}

/// Validates an authored flow using a concrete home directory for plugin-aware checks.
pub fn validate_flow_in_home(home_dir: &Path, flow: &Flow) -> Result<()> {
    let settings = load_home_settings(home_dir)?;
    validate_flow_internal(
        flow,
        &mut Vec::new(),
        PromptValidationMode::Home(home_dir),
        settings.filesystem_policy.as_ref(),
        settings.network_policy.as_ref(),
        None,
        false,
    )
}

pub(crate) fn validate_flow_snapshot(
    flow: &Flow,
    snapshot: &WorkflowDefinitionSnapshot,
) -> Result<()> {
    validate_flow_internal(
        flow,
        &mut Vec::new(),
        PromptValidationMode::Snapshot,
        None,
        None,
        Some(snapshot),
        false,
    )
}

/// Validates package-independent semantics and canonical assets from captured bytes.
pub fn validate_workflow_snapshot(snapshot: &WorkflowDefinitionSnapshot) -> Result<()> {
    let flow = snapshot.top_level_flow()?;
    validate_flow_snapshot(&flow, snapshot)
}

/// Validates an immutable captured workflow using local plugin and settings state.
///
/// Semantic validation, subflow traversal, and authored asset checks consume only
/// bytes retained by `snapshot`; definition files are never re-read.
pub fn validate_workflow_snapshot_in_home(
    home_dir: &Path,
    snapshot: &WorkflowDefinitionSnapshot,
) -> Result<()> {
    let flow = snapshot.top_level_flow()?;
    let settings = load_home_settings(home_dir)?;
    validate_flow_internal(
        &flow,
        &mut Vec::new(),
        PromptValidationMode::Home(home_dir),
        settings.filesystem_policy.as_ref(),
        settings.network_policy.as_ref(),
        Some(snapshot),
        false,
    )
}

#[derive(Clone, Copy)]
pub(crate) enum PromptValidationMode<'a> {
    Generic,
    Snapshot,
    Home(&'a Path),
}

pub(crate) fn validate_flow_internal(
    flow: &Flow,
    stack: &mut Vec<PathBuf>,
    mode: PromptValidationMode<'_>,
    global_filesystem_policy: Option<&transist_protocol::FilesystemPolicy>,
    global_network_policy: Option<&transist_protocol::NetworkPolicy>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
    is_module: bool,
) -> Result<()> {
    let declared_flow = flow.with_declarations_applied()?;
    transist_plugins::record_validation_work(
        transist_plugins::ValidationWork::DefinitionValidation,
    );
    let flow = &declared_flow;
    if !is_module && !flow.outputs.is_empty() {
        bail!("outputs declarations belong to package-local modules");
    }
    dataflow::validate_dataflow(flow, mode, snapshot)?;
    let pushed_source = push_flow_source(flow, stack)?;
    let result = {
        let mut diagnostics = Vec::new();
        if flow.schema != FLOW_SCHEMA_V1 {
            diagnostics.push(format!(
                "unsupported flow schema '{}'; expected '{}'",
                flow.schema, FLOW_SCHEMA_V1
            ));
        }
        collect_diagnostic(
            &mut diagnostics,
            transist_protocol::validate_identifier(
                transist_protocol::IdentifierKind::WorkflowId,
                &flow.id,
            )
            .map_err(|error| anyhow::anyhow!("flow id: {error}")),
        );
        collect_diagnostic(
            &mut diagnostics,
            transist_protocol::validate_identifier(
                transist_protocol::IdentifierKind::WorkflowVersion,
                &flow.version,
            )
            .map_err(|error| anyhow::anyhow!("flow version: {error}")),
        );
        if flow.steps.is_empty() {
            diagnostics.push("flow must contain at least one step".to_string());
        }

        collect_diagnostic(&mut diagnostics, inputs::validate_inputs(&flow.inputs));
        collect_diagnostic(
            &mut diagnostics,
            validate_declared_plugins(flow, snapshot, !is_module),
        );
        collect_diagnostic(&mut diagnostics, validate_workspace_binding(flow));
        collect_diagnostic(
            &mut diagnostics,
            validate_workflow_prerequisites(&flow.prerequisites),
        );
        if let Some(policy) = flow.filesystem_policy.as_ref() {
            collect_diagnostic(
                &mut diagnostics,
                steps::validate_filesystem_policy(policy, "flow.filesystem_policy", &flow.inputs),
            );
        }
        if let Some(policy) = flow.network_policy.as_ref() {
            collect_diagnostic(
                &mut diagnostics,
                steps::validate_network_policy(policy, "flow.network"),
            );
        }
        if let Some(global_policy) = global_filesystem_policy {
            collect_diagnostic(
                &mut diagnostics,
                steps::validate_filesystem_policy(
                    global_policy,
                    "transist/settings.yaml.filesystem_policy",
                    &std::collections::BTreeMap::new(),
                ),
            );
        }
        if let Some(global_policy) = global_network_policy {
            collect_diagnostic(
                &mut diagnostics,
                steps::validate_network_policy(
                    global_policy,
                    "transist/settings.yaml.network_policy",
                ),
            );
        }

        let mut ids = BTreeSet::new();
        let mut step_index = BTreeMap::new();
        for (idx, step) in flow.steps.iter().enumerate() {
            if step.id.trim().is_empty() {
                diagnostics.push("flow step id cannot be empty".to_string());
            }
            collect_diagnostic(
                &mut diagnostics,
                transist_protocol::validate_identifier(
                    transist_protocol::IdentifierKind::StepId,
                    &step.id,
                )
                .map_err(|error| anyhow::anyhow!("flow.steps[{idx}]: {error}")),
            );
            if !ids.insert(step.id.clone()) {
                diagnostics.push(format!("duplicate flow step id '{}'", step.id));
            }
            step_index.insert(step.id.clone(), idx);
        }
        for cleanup_step in &flow.cleanup {
            if cleanup_step.id.trim().is_empty() {
                diagnostics.push("flow cleanup step id cannot be empty".to_string());
            }
            collect_diagnostic(
                &mut diagnostics,
                transist_protocol::validate_identifier(
                    transist_protocol::IdentifierKind::CleanupId,
                    &cleanup_step.id,
                )
                .map_err(|error| anyhow::anyhow!("flow.cleanup: {error}")),
            );
            if !ids.insert(cleanup_step.id.clone()) {
                diagnostics.push(format!("duplicate flow step id '{}'", cleanup_step.id));
            }
        }

        let mut block_members = BTreeSet::new();
        let mut defined_vars: BTreeSet<String> = BTreeSet::new();
        for (idx, step) in flow.steps.iter().enumerate() {
            collect_diagnostic(
                &mut diagnostics,
                steps::validate_step(
                    flow,
                    step,
                    idx,
                    &step_index,
                    &mut block_members,
                    stack,
                    mode,
                    flow.filesystem_policy.as_ref(),
                    flow.network_policy.as_ref(),
                    &defined_vars,
                    snapshot,
                ),
            );
            // Accumulate vars written by this step so subsequent steps can reference them.
            if let Some(set_map) = step.set.as_ref() {
                for key in set_map.keys() {
                    defined_vars.insert(key.clone());
                }
            }
        }
        for (idx, step) in flow.cleanup.iter().enumerate() {
            collect_diagnostic(
                &mut diagnostics,
                cleanup::validate_cleanup_step(
                    flow,
                    step,
                    idx,
                    mode,
                    flow.filesystem_policy.as_ref(),
                    flow.network_policy.as_ref(),
                    &defined_vars,
                    snapshot,
                ),
            );
        }

        finish_diagnostics(diagnostics)
    };
    if pushed_source {
        stack.pop();
    }
    result
}

fn validate_declared_plugins(
    flow: &Flow,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
    reject_unused: bool,
) -> Result<()> {
    if flow.plugins.is_empty() {
        if flow
            .steps
            .iter()
            .any(|step| step.plugin_version.as_deref() == Some(UNDECLARED_PLUGIN_MARKER))
            || flow
                .cleanup
                .iter()
                .any(|step| step.plugin_version.as_deref() == Some(UNDECLARED_PLUGIN_MARKER))
        {
            bail!(
                "workflow uses '<adapter>:<plugin>/<action>' but declares no plugins; add plugins.<plugin>: <exact-semver>"
            );
        }
        return Ok(());
    }

    let mut referenced = BTreeSet::new();
    for step in &flow.steps {
        if let Some(plugin) = step.plugin.as_deref() {
            referenced.insert(plugin.to_string());
        }
    }
    for step in &flow.cleanup {
        if !step.plugin.trim().is_empty() {
            referenced.insert(step.plugin.clone());
        }
    }
    collect_module_plugin_references(flow, snapshot, &mut referenced, &mut BTreeSet::new())?;

    for (plugin, version) in &flow.plugins {
        if plugin.trim().is_empty() {
            bail!("flow.plugins cannot declare an empty plugin id");
        }
        // Preserve explicit test-marker fixtures without making ordinary
        // pre-1.0 releases eligible for development trust bypass.
        let explicit_test_pin = cfg!(feature = "dev-version-bypass")
            && flow.version == "v0"
            && matches!(version.as_str(), "0" | "v0");
        if !explicit_test_pin {
            Version::parse(version).with_context(|| {
                format!(
                    "flow.plugins.{} must declare an exact semantic version; replace '{}' with a value such as '1.2.3'",
                    plugin, version
                )
            })?;
        }
        if reject_unused && !referenced.contains(plugin) {
            bail!(
                "flow.plugins.{} is unused; remove it or reference it with uses: <adapter>:{}/<action>",
                plugin,
                plugin
            );
        }
    }

    for plugin in referenced {
        if !flow.plugins.contains_key(&plugin) {
            bail!(
                "workflow step references undeclared plugin '{}'; add plugins.{} with its exact installed version",
                plugin,
                plugin
            );
        }
    }
    Ok(())
}

fn collect_module_plugin_references(
    flow: &Flow,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
    referenced: &mut BTreeSet<String>,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let Some(parent_path) = flow.source_path.as_ref() else {
        return Ok(());
    };
    for step in &flow.steps {
        let Some(call) = step.subflow.as_ref() else {
            continue;
        };
        let child_path = crate::io::resolve_relative_path(parent_path, &call.path);
        if !visited.insert(child_path.clone()) {
            continue;
        }
        let mut child = match snapshot {
            Some(snapshot) => snapshot.flow_for_source(&child_path)?,
            None => crate::io::load_child_flow_unvalidated(&child_path)?,
        };
        crate::compile::subflows::inherit_module_scope(flow, step, &mut child)?;
        for child_step in &child.steps {
            if let Some(plugin) = child_step.plugin.as_deref()
                && flow.plugins.contains_key(plugin)
            {
                referenced.insert(plugin.to_string());
            }
        }
        collect_module_plugin_references(&child, snapshot, referenced, visited)?;
    }
    Ok(())
}

pub(crate) fn resolve_prompt_action(
    home_dir: &Path,
    step: &FlowStep,
    step_idx: usize,
) -> Result<(AdapterKind, ActionDescriptor, ActionPromptBinding)> {
    let (adapter, action) = resolve_action_descriptor(home_dir, step, step_idx)?;
    let plugin_id = step.plugin.as_deref().unwrap_or_default();
    let action_name = step.action.as_deref().unwrap_or_default();
    let prompt_binding = action.prompt_binding()?.ok_or_else(|| {
        anyhow::anyhow!(
            "flow.steps[{step_idx}] '{}' uses first-class prompt fields but plugin action '{}.{}' is not prompt-aware",
            step.id,
            plugin_id,
            action_name
        )
    })?;
    Ok((adapter, action, prompt_binding))
}

pub(crate) fn resolve_skills_action(
    home_dir: &Path,
    step: &FlowStep,
    step_idx: usize,
) -> Result<ActionSkillsBinding> {
    let (_, action) = resolve_action_descriptor(home_dir, step, step_idx)?;
    let plugin_id = step.plugin.as_deref().unwrap_or_default();
    let action_name = step.action.as_deref().unwrap_or_default();
    action.skills_binding()?.ok_or_else(|| {
        anyhow::anyhow!(
            "flow.steps[{step_idx}] '{}' uses first-class skills fields but plugin action '{}.{}' is not skills-aware",
            step.id,
            plugin_id,
            action_name
        )
    })
}

/// Resolves the manifest binding used to inject an immutable manual-gate decision.
pub(crate) fn resolve_gate_decision_action(
    home_dir: &Path,
    step: &FlowStep,
    step_idx: usize,
) -> Result<ActionGateDecisionBinding> {
    let (_, action) = resolve_action_descriptor(home_dir, step, step_idx)?;
    let plugin_id = step.plugin.as_deref().unwrap_or_default();
    let action_name = step.action.as_deref().unwrap_or_default();
    action.gate_decision_binding()?.ok_or_else(|| {
        anyhow::anyhow!(
            "flow.steps[{step_idx}] '{}' uses gate_decision but plugin action '{}.{}' does not declare transist_gate_decision_v1",
            step.id,
            plugin_id,
            action_name
        )
    })
}

pub(crate) fn ensure_step_action_installed(
    home_dir: &Path,
    step: &FlowStep,
    step_idx: usize,
) -> Result<()> {
    let plugin_id = step
        .plugin
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("flow.steps[{step_idx}].plugin is required"))?;
    let action_name = step
        .action
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("flow.steps[{step_idx}].action is required"))?;

    let (manifest_adapter, _) = resolve_action_descriptor(home_dir, step, step_idx)?;
    let declared_adapter = flow_step_adapter(step)?;
    if declared_adapter != manifest_adapter {
        bail!(
            "flow.steps[{step_idx}] '{}' declares adapter '{}' but plugin '{}' is installed for adapter '{}'; change uses to '{}:{}/{}'",
            step.id,
            declared_adapter,
            plugin_id,
            manifest_adapter,
            manifest_adapter,
            plugin_id,
            action_name,
        );
    }
    Ok(())
}

pub(crate) fn count_prompt_sources(step: &FlowStep, prompt_binding: &ActionPromptBinding) -> usize {
    let mut count = usize::from(step.prompt.is_some()) + usize::from(step.prompt_file.is_some());
    count += usize::from(step.with.contains_key(prompt_binding.inline_field.as_str()));
    if let Some(file_field) = prompt_binding.file_field.as_deref() {
        count += usize::from(step.with.contains_key(file_field));
    }
    count
}

pub(crate) fn flow_step_adapter(step: &FlowStep) -> Result<AdapterKind> {
    let raw = step.adapter.as_deref().unwrap_or_default();
    raw.parse::<AdapterKind>()
        .map_err(|_| anyhow::anyhow!("unsupported adapter '{}'", raw))
}

fn resolve_action_descriptor(
    home_dir: &Path,
    step: &FlowStep,
    step_idx: usize,
) -> Result<(AdapterKind, ActionDescriptor)> {
    let plugin_id = step
        .plugin
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("flow.steps[{step_idx}].plugin is required"))?;
    let action_name = step
        .action
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("flow.steps[{step_idx}].action is required"))?;
    let (manifest, _) = if let Some(selector) = step
        .plugin_version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let selector = if selector == "latest" {
            PluginVersionSelector::Latest
        } else {
            PluginVersionSelector::Exact(selector.to_string())
        };
        resolve_plugin_with_selector_in_home(home_dir, plugin_id, &selector)
    } else {
        resolve_plugin_in_home(home_dir, plugin_id)
    }
    .with_context(|| {
        format!(
            "flow.steps[{step_idx}] '{}' references plugin '{}' which is not installed; install it or pin an installed plugin_version",
            step.id, plugin_id
        )
    })?;
    let action = manifest
        .actions
        .iter()
        .find(|candidate| candidate.name == action_name)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "flow.steps[{step_idx}] '{}' references unknown action '{}.{}'",
                step.id,
                plugin_id,
                action_name
            )
        })?;
    Ok((manifest.adapter, action))
}

fn authored_secret_refs(with: &BTreeMap<String, yaml_serde::Value>) -> Vec<String> {
    let Some(yaml_serde::Value::Sequence(values)) = with.get("secret_refs") else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            yaml_serde::Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
            _ => None,
        })
        .collect()
}

fn is_input_template(value: &str) -> bool {
    value.replace(' ', "").contains("{{inputs.")
}

fn push_flow_source(flow: &Flow, stack: &mut Vec<PathBuf>) -> Result<bool> {
    let Some(source_path) = flow.source_path.as_ref() else {
        return Ok(false);
    };
    if stack.contains(source_path) {
        let mut chain = stack
            .iter()
            .map(|entry| entry.display().to_string())
            .collect::<Vec<_>>();
        chain.push(source_path.display().to_string());
        bail!("workflow subflow cycle detected: {}", chain.join(" -> "));
    }
    stack.push(source_path.clone());
    Ok(true)
}

fn validate_workflow_prerequisites(prerequisites: &PrerequisiteSet) -> Result<()> {
    validate_prerequisite_set(prerequisites).context("invalid workflow prerequisites")
}

fn validate_workspace_binding(flow: &Flow) -> Result<()> {
    let Some(workspace) = flow.workspace.as_ref() else {
        return Ok(());
    };
    if flow
        .filesystem_policy
        .as_ref()
        .and_then(|policy| policy.workspace_access.as_ref())
        .is_some()
    {
        bail!("workspace cannot coexist with flow.filesystem_policy.workspace_access");
    }
    if workspace.access == WorkspaceAccessMode::Custom {
        bail!("workspace.access must be 'full' or 'read_only'");
    }
    if workspace.path.trim() != workspace.path || workspace.path.is_empty() {
        bail!("workspace.path must be a non-empty unpadded path");
    }
    if workspace.path.contains("{{") || workspace.path.contains("}}") {
        let Some(input) = exact_input_reference(&workspace.path) else {
            bail!("workspace.path must be an absolute literal or exactly '{{{{inputs.<name>}}}}'");
        };
        let spec = flow.inputs.get(input).ok_or_else(|| {
            anyhow::anyhow!("workspace.path references undeclared input '{input}'")
        })?;
        if !spec.required || spec.value_type != "string" {
            bail!("workspace.path input '{input}' must be a required string input");
        }
        return Ok(());
    }
    if std::path::Path::new(&workspace.path).is_absolute() {
        return Ok(());
    }
    let Some(input) = exact_input_reference(&workspace.path) else {
        bail!("workspace.path must be an absolute path or exactly '{{{{inputs.<name>}}}}'");
    };
    let spec = flow
        .inputs
        .get(input)
        .ok_or_else(|| anyhow::anyhow!("workspace.path references undeclared input '{input}'"))?;
    if !spec.required || spec.value_type != "string" {
        bail!("workspace.path input '{input}' must be a required string input");
    }
    Ok(())
}

pub(crate) fn exact_input_reference(value: &str) -> Option<&str> {
    let input = value.strip_prefix("{{inputs.")?.strip_suffix("}}")?;
    (!input.is_empty()
        && !input.contains(['{', '}', '/', '\\'])
        && input
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(input)
}

#[cfg(test)]
mod tests {
    use super::{FlowValidationErrors, collect_diagnostic, validate_workspace_binding};
    use crate::{Flow, FlowInputSpec};
    use anyhow::Context;
    use std::collections::BTreeMap;
    use transist_protocol::{WorkspaceAccessMode, WorkspaceBinding};

    #[test]
    fn flattens_nested_validation_diagnostics_with_context() {
        let nested = Err(anyhow::Error::new(FlowValidationErrors {
            diagnostics: vec!["first error".to_string(), "second error".to_string()],
        }))
        .context("failed validating subflow for 'child'");
        let mut diagnostics = Vec::new();

        collect_diagnostic(&mut diagnostics, nested);

        assert_eq!(
            diagnostics,
            vec![
                "failed validating subflow for 'child': first error",
                "failed validating subflow for 'child': second error",
            ]
        );
    }

    #[test]
    fn workspace_binding_accepts_exact_required_string_input() {
        let flow = Flow {
            schema: "transist.flow.v1".to_string(),
            id: "workspace-test".to_string(),
            version: "v1".to_string(),
            plugins: BTreeMap::new(),
            prerequisites: Default::default(),
            inputs: BTreeMap::from([(
                "repo_path".to_string(),
                FlowInputSpec {
                    value_type: "string".to_string(),
                    required: true,
                    default: None,
                    description: None,
                },
            )]),
            context: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            documents: Default::default(),
            profiles: BTreeMap::new(),
            workspace: Some(WorkspaceBinding {
                path: "{{inputs.repo_path}}".to_string(),
                access: WorkspaceAccessMode::Full,
            }),
            filesystem_policy: None,
            network_policy: None,
            steps: Vec::new(),
            cleanup: Vec::new(),
            source_path: None,
        };

        validate_workspace_binding(&flow).expect("workspace binding is valid");
    }

    #[test]
    fn workspace_binding_rejects_interpolation() {
        let flow = Flow {
            schema: "transist.flow.v1".to_string(),
            id: "workspace-test".to_string(),
            version: "v1".to_string(),
            plugins: BTreeMap::new(),
            prerequisites: Default::default(),
            inputs: BTreeMap::new(),
            context: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            documents: Default::default(),
            profiles: BTreeMap::new(),
            workspace: Some(WorkspaceBinding {
                path: "/tmp/{{inputs.repo_path}}".to_string(),
                access: WorkspaceAccessMode::Full,
            }),
            filesystem_policy: None,
            network_policy: None,
            steps: Vec::new(),
            cleanup: Vec::new(),
            source_path: None,
        };

        assert!(validate_workspace_binding(&flow).is_err());
    }
}
