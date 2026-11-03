// SPDX-License-Identifier: MPL-2.0
use super::control_flow::{validate_if_block, validate_loop_block};
use super::inputs::{
    validate_input_placeholders_in_yaml_map, validate_set_rhs_placeholders_in_yaml_map,
    validate_supported_binding_placeholders_in_string,
    validate_supported_binding_placeholders_in_yaml_map,
};
use super::{
    PromptValidationMode, count_prompt_sources, ensure_step_action_installed, flow_step_adapter,
    resolve_prompt_action, resolve_skills_action, validate_step_secret_refs_declared,
};
use crate::WorkflowDefinitionSnapshot;
use crate::compile::GATE_DECISION_META_KEY;
use crate::compile::gates::effective_gate_id;
use crate::compile::skills::build_skill_bundle;
use crate::flow::{Flow, FlowStep};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use transist_plugins::AdapterKind;
use transist_protocol::{FilesystemPolicy, NetworkPolicy};

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_step(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    step_index: &BTreeMap<String, usize>,
    block_members: &mut BTreeSet<String>,
    stack: &mut Vec<PathBuf>,
    mode: PromptValidationMode<'_>,
    workflow_filesystem_policy: Option<&FilesystemPolicy>,
    workflow_network_policy: Option<&NetworkPolicy>,
    known_vars: &BTreeSet<String>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<()> {
    if step.with.contains_key(GATE_DECISION_META_KEY) {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define reserved input '{}'",
            step.id,
            GATE_DECISION_META_KEY
        );
    }

    let has_invoke = step.adapter.is_some()
        || step.plugin.is_some()
        || step.action.is_some()
        || step.execution_kind.is_some();
    let has_control = step.control.is_some();
    let has_subflow = step.subflow.is_some();
    let shape_count = usize::from(has_invoke) + usize::from(has_control) + usize::from(has_subflow);

    if shape_count == 0 {
        bail!(
            "flow.steps[{step_idx}] '{}' must define invoke fields, control, or subflow",
            step.id
        );
    }
    if shape_count > 1 {
        bail!(
            "flow.steps[{step_idx}] '{}' mixes invoke, control, and subflow fields",
            step.id
        );
    }

    if step.if_block.is_some() && step.loop_block.is_some() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define both if and loop blocks",
            step.id
        );
    }

    if has_subflow {
        validate_no_prompt_fields(step, step_idx, "subflow")?;
        validate_no_skills_fields(step, step_idx, "subflow")?;
        validate_no_gate_decision_field(step, step_idx, "subflow")?;
        validate_no_set_field(step, step_idx, "subflow")?;
        validate_no_plugin_version_field(step, step_idx, "subflow")?;
        validate_subflow_step(flow, step, step_idx, stack, mode, known_vars, snapshot)?;
    } else if has_control {
        validate_no_prompt_fields(step, step_idx, "control")?;
        validate_no_skills_fields(step, step_idx, "control")?;
        validate_no_gate_decision_field(step, step_idx, "control")?;
        validate_no_set_field(step, step_idx, "control")?;
        validate_no_plugin_version_field(step, step_idx, "control")?;
        let control = step.control.as_deref().unwrap_or_default().trim();
        if control.is_empty() {
            bail!("flow.steps[{step_idx}].control is required");
        }
        if (step.if_block.is_some() || step.loop_block.is_some()) && control != "control.noop" {
            bail!(
                "flow.steps[{step_idx}] '{}' must use control.noop when defining if/loop blocks",
                step.id
            );
        }
    } else {
        require_non_empty(step.adapter.as_deref(), "adapter", step_idx)?;
        require_non_empty(step.plugin.as_deref(), "plugin", step_idx)?;
        require_non_empty(step.action.as_deref(), "action", step_idx)?;
        require_non_empty(step.execution_kind.as_deref(), "execution_kind", step_idx)?;
        if let PromptValidationMode::Home(home_dir) = mode {
            ensure_step_action_installed(home_dir, step, step_idx)?;
            validate_step_secret_refs_declared(home_dir, step, step_idx)?;
        }
        validate_optional_non_empty(step.plugin_version.as_deref(), "plugin_version", step_idx)?;
        validate_optional_non_empty(step.prompt.as_deref(), "prompt", step_idx)?;
        validate_optional_non_empty(step.prompt_file.as_deref(), "prompt_file", step_idx)?;
        validate_optional_skills(step.skills.as_deref(), step_idx)?;
        validate_gate_decision_step(flow, step, step_idx, step_index, mode)?;
        if step.if_block.is_some() || step.loop_block.is_some() {
            bail!(
                "flow.steps[{step_idx}] '{}' cannot attach if/loop blocks to invoke steps",
                step.id
            );
        }
        if let Some(set_map) = step.set.as_ref() {
            if set_map.is_empty() {
                bail!(
                    "flow.steps[{step_idx}] '{}' set block cannot be empty",
                    step.id
                );
            }
            for key in set_map.keys() {
                if key.trim().is_empty() {
                    bail!(
                        "flow.steps[{step_idx}] '{}' set block keys cannot be empty",
                        step.id
                    );
                }
            }
            validate_set_rhs_placeholders_in_yaml_map(set_map, &flow.inputs, step_idx, step_index)
                .with_context(|| format!("invalid set RHS placeholders for '{}'", step.id))?;
        }
    }

    if has_control {
        validate_input_placeholders_in_yaml_map(&step.with, &flow.inputs)
            .with_context(|| format!("invalid step input placeholders for '{}'", step.id))?;
    } else {
        validate_supported_binding_placeholders_in_yaml_map(
            &step.with,
            &flow.inputs,
            known_vars,
            "invoke with",
        )
        .with_context(|| format!("invalid step input placeholders for '{}'", step.id))?;
    }
    if let Some(prompt) = step.prompt.as_deref() {
        validate_supported_binding_placeholders_in_string(
            prompt,
            &flow.inputs,
            known_vars,
            "invoke prompt",
        )
        .with_context(|| format!("invalid prompt placeholders for '{}'", step.id))?;
    }
    if let Some(prompt_file) = step.prompt_file.as_deref() {
        validate_supported_binding_placeholders_in_string(
            prompt_file,
            &flow.inputs,
            known_vars,
            "invoke prompt_file",
        )
        .with_context(|| format!("invalid prompt_file placeholders for '{}'", step.id))?;
    }
    validate_prompt_step(step, step_idx, mode)?;
    validate_skills_step(flow, step, step_idx, mode, snapshot)?;
    // Custom mode is rejected on every step type, including control and
    // subflow steps, so the authored workflow never carries an unsupported
    // policy declaration even when the step does not dispatch an adapter.
    if let Some(policy) = step.filesystem_policy.as_ref() {
        reject_custom_workspace_mode(policy, &format!("flow.steps[{step_idx}].filesystem_policy"))?;
    }
    if !has_control && !has_subflow {
        if let Some(policy) = step.filesystem_policy.as_ref() {
            validate_filesystem_policy(
                policy,
                &format!("flow.steps[{step_idx}].filesystem_policy"),
                &flow.inputs,
            )?;
        }
        if let Some(policy) = step.network_policy.as_ref() {
            validate_network_policy(policy, &format!("flow.steps[{step_idx}].network"))?;
        }
        let _ = FilesystemPolicy::resolve(
            None,
            workflow_filesystem_policy,
            step.filesystem_policy.as_ref(),
        );
        let _ = NetworkPolicy::resolve(None, workflow_network_policy, step.network_policy.as_ref());
    }

    if let Some(if_block) = step.if_block.as_ref() {
        validate_if_block(
            flow,
            step,
            step_idx,
            if_block,
            step_index,
            block_members,
            known_vars,
        )?;
    }
    if let Some(loop_block) = step.loop_block.as_ref() {
        validate_loop_block(
            flow,
            step,
            step_idx,
            loop_block,
            step_index,
            block_members,
            known_vars,
        )?;
    }

    Ok(())
}

fn validate_gate_decision_step(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    step_index: &BTreeMap<String, usize>,
    mode: PromptValidationMode<'_>,
) -> Result<()> {
    let Some(binding) = step.gate_decision.as_ref() else {
        return Ok(());
    };
    if binding.step.trim().is_empty() {
        bail!(
            "flow.steps[{step_idx}] '{}' gate_decision.step cannot be empty",
            step.id
        );
    }
    let Some(gate_idx) = step_index.get(&binding.step).copied() else {
        bail!(
            "flow.steps[{step_idx}] '{}' gate_decision.step '{}' does not exist in this workflow",
            step.id,
            binding.step
        );
    };
    let gate_step = &flow.steps[gate_idx];
    if gate_step.control.as_deref().map(str::trim) != Some("control.gate.manual") {
        bail!(
            "flow.steps[{step_idx}] '{}' gate_decision.step '{}' must reference a control.gate.manual step",
            step.id,
            binding.step
        );
    }
    let gate_id = effective_gate_id(gate_step)?;
    if let Some(conflicting_step) = flow.steps.iter().find(|candidate| {
        candidate.id != gate_step.id
            && candidate.control.as_deref().map(str::trim) == Some("control.gate.manual")
            && effective_gate_id(candidate).ok().as_deref() == Some(gate_id.as_str())
    }) {
        bail!(
            "flow.steps[{step_idx}] '{}' gate_decision.step '{}' resolves to gate_id '{}' which is also used by manual gate step '{}'",
            step.id,
            binding.step,
            gate_id,
            conflicting_step.id
        );
    }
    let PromptValidationMode::Home(home_dir) = mode else {
        return Ok(());
    };
    let manifest_binding = super::resolve_gate_decision_action(home_dir, step, step_idx)?;
    if step.with.contains_key(&manifest_binding.decision_field) {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define with.{} because it is injected by transist_gate_decision_v1",
            step.id,
            manifest_binding.decision_field
        );
    }
    Ok(())
}

pub(crate) fn validate_network_policy(policy: &NetworkPolicy, path: &str) -> Result<()> {
    for (idx, host) in policy.allow_hosts.iter().enumerate() {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            bail!("{path}.allow_hosts[{idx}] cannot be empty");
        }
        if trimmed.contains('\0') {
            bail!("{path}.allow_hosts[{idx}] contains NUL byte");
        }
    }
    for (idx, port) in policy.allow_ports.iter().enumerate() {
        if *port == 0 {
            bail!("{path}.allow_ports[{idx}] must be greater than 0");
        }
    }
    Ok(())
}

fn validate_subflow_step(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    stack: &mut Vec<PathBuf>,
    mode: PromptValidationMode<'_>,
    known_vars: &BTreeSet<String>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<()> {
    let call = step
        .subflow
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("internal error: missing subflow"))?;
    if call.path.trim().is_empty() {
        bail!("flow.steps[{step_idx}].subflow.path is required");
    }
    if let Some(rerun_mode) = call.rerun_mode.as_deref() {
        if !matches!(rerun_mode.trim(), "reuse" | "fresh") {
            bail!("flow.steps[{step_idx}].subflow.rerun_mode must be 'reuse' or 'fresh'");
        }
    }
    if step.if_block.is_some() || step.loop_block.is_some() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define if/loop blocks on a subflow step",
            step.id
        );
    }
    if !step.with.is_empty() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define top-level with on a subflow step; use subflow.inputs",
            step.id
        );
    }
    validate_supported_binding_placeholders_in_yaml_map(
        &call.inputs,
        &flow.inputs,
        known_vars,
        "subflow.inputs",
    )
    .with_context(|| format!("invalid subflow input placeholders for '{}'", step.id))?;
    if let Some(parent_path) = flow.source_path.as_ref() {
        let child_path = crate::io::resolve_relative_path(parent_path, &call.path);
        let mut child_flow = match snapshot {
            Some(snapshot) => snapshot.flow_for_source(&child_path),
            None => crate::io::load_child_flow_unvalidated(&child_path),
        }
        .with_context(|| format!("failed loading subflow for '{}'", step.id))?;
        crate::compile::subflows::inherit_module_scope(flow, step, &mut child_flow)
            .with_context(|| format!("invalid module authority for '{}'", step.id))?;
        super::validate_flow_internal(&child_flow, stack, mode, None, None, snapshot, true)
            .with_context(|| format!("failed validating subflow for '{}'", step.id))?;
        for key in call.inputs.keys() {
            if !child_flow.inputs.contains_key(key) {
                bail!(
                    "flow.steps[{step_idx}] '{}' binds unknown child input '{}'",
                    step.id,
                    key
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn require_non_empty(value: Option<&str>, field: &str, step_idx: usize) -> Result<()> {
    match value.map(str::trim) {
        Some("") | None => bail!("flow.steps[{step_idx}].{field} is required"),
        Some(_) => Ok(()),
    }
}

pub(crate) fn validate_optional_non_empty(
    value: Option<&str>,
    field: &str,
    step_idx: usize,
) -> Result<()> {
    if matches!(value.map(str::trim), Some("")) {
        bail!("flow.steps[{step_idx}].{field} cannot be empty");
    }
    Ok(())
}

fn validate_no_set_field(step: &FlowStep, step_idx: usize, step_kind: &str) -> Result<()> {
    if step.set.is_some() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define set on a {} step",
            step.id,
            step_kind
        );
    }
    Ok(())
}

fn validate_no_prompt_fields(step: &FlowStep, step_idx: usize, step_kind: &str) -> Result<()> {
    if step.prompt.is_some() || step.prompt_file.is_some() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define prompt or prompt_file on a {} step",
            step.id,
            step_kind
        );
    }
    Ok(())
}

fn validate_no_plugin_version_field(
    step: &FlowStep,
    step_idx: usize,
    step_kind: &str,
) -> Result<()> {
    if step.plugin_version.is_some() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define plugin_version on a {} step",
            step.id,
            step_kind
        );
    }
    Ok(())
}

fn validate_no_skills_fields(step: &FlowStep, step_idx: usize, step_kind: &str) -> Result<()> {
    if step.skills.is_some() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define skills on a {} step",
            step.id,
            step_kind
        );
    }
    Ok(())
}

fn validate_no_gate_decision_field(
    step: &FlowStep,
    step_idx: usize,
    step_kind: &str,
) -> Result<()> {
    if step.gate_decision.is_some() {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define gate_decision on a {} step",
            step.id,
            step_kind
        );
    }
    Ok(())
}

pub(crate) fn validate_optional_skills(skills: Option<&[String]>, step_idx: usize) -> Result<()> {
    let Some(skills) = skills else {
        return Ok(());
    };
    if skills.is_empty() {
        bail!("flow.steps[{step_idx}].skills cannot be empty");
    }
    for (skill_idx, skill) in skills.iter().enumerate() {
        if skill.trim().is_empty() {
            bail!("flow.steps[{step_idx}].skills[{skill_idx}] cannot be empty");
        }
        if skill.contains("{{inputs.") {
            bail!(
                "flow.steps[{step_idx}].skills[{skill_idx}] does not support workflow input placeholders"
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_prompt_step(
    step: &FlowStep,
    step_idx: usize,
    mode: PromptValidationMode<'_>,
) -> Result<()> {
    let has_first_class_prompt = step.prompt.is_some() || step.prompt_file.is_some();
    if !has_first_class_prompt {
        return Ok(());
    }

    if let PromptValidationMode::Snapshot = mode {
        if step.prompt.is_some() && step.prompt_file.is_some() {
            bail!(
                "flow.steps[{step_idx}] '{}' defines multiple prompt sources; use only one of prompt or prompt_file",
                step.id
            );
        }
        if step.prompt_file.is_some() && flow_step_adapter(step)? == AdapterKind::Http {
            bail!(
                "flow.steps[{step_idx}] '{}' cannot use prompt_file with adapter 'http'",
                step.id
            );
        }
        return Ok(());
    }

    let PromptValidationMode::Home(home_dir) = mode else {
        bail!(
            "flow.steps[{step_idx}] '{}' uses first-class prompt fields; validate with an effective Transist home",
            step.id
        );
    };

    let (_, action, prompt_binding) = resolve_prompt_action(home_dir, step, step_idx)?;
    let prompt_sources = count_prompt_sources(step, &prompt_binding);
    if prompt_sources > 1 {
        bail!(
            "flow.steps[{step_idx}] '{}' defines multiple prompt sources; use only one of prompt, prompt_file, or the mapped with fields",
            step.id
        );
    }
    if step.prompt_file.is_some() {
        if flow_step_adapter(step)? == AdapterKind::Http {
            bail!(
                "flow.steps[{step_idx}] '{}' cannot use prompt_file with adapter 'http'",
                step.id
            );
        }
        if prompt_binding.file_field.is_none() {
            bail!(
                "flow.steps[{step_idx}] '{}' uses prompt_file but plugin action '{}.{}' does not declare a prompt file binding",
                step.id,
                step.plugin.as_deref().unwrap_or_default(),
                action.name
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_skills_step(
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
    mode: PromptValidationMode<'_>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<()> {
    let Some(skills) = step.skills.as_deref() else {
        return Ok(());
    };
    if skills.is_empty() {
        bail!("flow.steps[{step_idx}].skills cannot be empty");
    }

    if let PromptValidationMode::Snapshot = mode {
        let snapshot = snapshot.ok_or_else(|| {
            anyhow::anyhow!("internal error: snapshot skill validation has no snapshot")
        })?;
        build_skill_bundle(flow, step, step_idx, Some(snapshot))?;
        return Ok(());
    }

    let PromptValidationMode::Home(home_dir) = mode else {
        bail!(
            "flow.steps[{step_idx}] '{}' uses first-class skills fields; validate with an effective Transist home",
            step.id
        );
    };

    let binding = resolve_skills_action(home_dir, step, step_idx)?;
    if step.with.contains_key(binding.bundle_field.as_str())
        || step.with.contains_key(binding.rendered_field.as_str())
    {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define with fields that collide with injected skills fields '{}', '{}'",
            step.id,
            binding.bundle_field,
            binding.rendered_field
        );
    }

    build_skill_bundle(flow, step, step_idx, snapshot)?;
    Ok(())
}

/// Rejects `workspace_access.mode: custom` with the dedicated reason code.
/// Called for every step type so unsupported declarations are caught at
/// compile time regardless of whether the step dispatches an adapter.
pub(crate) fn reject_custom_workspace_mode(policy: &FilesystemPolicy, path: &str) -> Result<()> {
    if let Some(wa) = &policy.workspace_access {
        if matches!(wa.mode, transist_protocol::WorkspaceAccessMode::Custom) {
            bail!(
                "{path}.workspace_access.mode 'custom' is not supported (filesystem_custom_mode_unsupported)"
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_filesystem_policy(
    policy: &FilesystemPolicy,
    path: &str,
    inputs: &std::collections::BTreeMap<String, crate::FlowInputSpec>,
) -> Result<()> {
    reject_custom_workspace_mode(policy, path)?;
    for (idx, pattern) in policy.read_allow_paths.iter().enumerate() {
        validate_read_allow_path(pattern, &format!("{path}.read_allow_paths[{idx}]"), inputs)?;
    }
    for (idx, pattern) in policy.write_allow_paths.iter().enumerate() {
        validate_external_path_pattern(pattern, &format!("{path}.write_allow_paths[{idx}]"))?;
    }
    Ok(())
}

fn validate_read_allow_path(
    pattern: &str,
    path: &str,
    inputs: &std::collections::BTreeMap<String, crate::FlowInputSpec>,
) -> Result<()> {
    if pattern.contains("{{") || pattern.contains("}}") {
        let input = super::exact_input_reference(pattern).ok_or_else(|| {
            anyhow::anyhow!("{path} must be a literal path or exactly '{{{{inputs.<name>}}}}'")
        })?;
        let spec = inputs
            .get(input)
            .ok_or_else(|| anyhow::anyhow!("{path} references undeclared input '{input}'"))?;
        if spec.value_type != "string" {
            bail!("{path} input '{input}' must be a string input");
        }
        return Ok(());
    }
    validate_external_path_pattern(pattern, path)
}

fn validate_external_path_pattern(pattern: &str, path: &str) -> Result<()> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        bail!("{path} must be non-empty");
    }
    if pattern != trimmed {
        bail!("{path} must not contain surrounding whitespace");
    }
    if trimmed.contains('\0') {
        bail!("{path} contains NUL byte");
    }
    if trimmed.contains(|c: char| c.is_control()) {
        bail!("{path} contains control characters");
    }
    if trimmed.contains(['*', '?', '[', ']']) {
        bail!("{path} must be a literal path without wildcard syntax");
    }
    if !trimmed.starts_with('/') && !trimmed.starts_with("~/") {
        bail!("{path} must be absolute (/…) or home-relative (~/…)");
    }
    if trimmed.split('/').any(|segment| segment == "..") {
        bail!("{path} must not contain parent traversal '..'");
    }
    Ok(())
}
