// SPDX-License-Identifier: MPL-2.0
use super::inputs::{
    validate_supported_binding_placeholders_in_string,
    validate_supported_binding_placeholders_in_yaml_map,
};
use super::steps::{
    require_non_empty, validate_filesystem_policy, validate_network_policy,
    validate_optional_non_empty, validate_optional_skills, validate_prompt_step,
    validate_skills_step,
};
use super::{
    PromptValidationMode, ensure_step_action_installed, validate_step_secret_refs_declared,
};
use crate::WorkflowDefinitionSnapshot;
use crate::flow::{Flow, FlowCleanupStep};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_cleanup_step(
    flow: &Flow,
    step: &FlowCleanupStep,
    step_idx: usize,
    mode: PromptValidationMode<'_>,
    workflow_filesystem_policy: Option<&transist_protocol::FilesystemPolicy>,
    workflow_network_policy: Option<&transist_protocol::NetworkPolicy>,
    known_vars: &BTreeSet<String>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<()> {
    if step.id.trim().is_empty() {
        bail!("flow.cleanup[{step_idx}].id cannot be empty");
    }
    if step.when.is_empty() {
        bail!("flow.cleanup[{step_idx}].when cannot be empty");
    }
    let mut seen_when = BTreeSet::new();
    for when in &step.when {
        if !seen_when.insert(*when) {
            bail!(
                "flow.cleanup[{step_idx}] '{}' declares duplicate cleanup outcome triggers",
                step.id
            );
        }
    }

    require_non_empty(Some(step.adapter.as_str()), "adapter", step_idx)
        .context("invalid cleanup step")?;
    require_non_empty(Some(step.plugin.as_str()), "plugin", step_idx)
        .context("invalid cleanup step")?;
    require_non_empty(Some(step.action.as_str()), "action", step_idx)
        .context("invalid cleanup step")?;
    require_non_empty(
        Some(step.execution_kind.as_str()),
        "execution_kind",
        step_idx,
    )
    .context("invalid cleanup step")?;
    validate_optional_non_empty(step.plugin_version.as_deref(), "plugin_version", step_idx)
        .context("invalid cleanup step")?;
    validate_optional_non_empty(step.prompt.as_deref(), "prompt", step_idx)
        .context("invalid cleanup step")?;
    validate_optional_non_empty(step.prompt_file.as_deref(), "prompt_file", step_idx)
        .context("invalid cleanup step")?;
    validate_optional_skills(step.skills.as_deref(), step_idx).context("invalid cleanup step")?;

    validate_supported_binding_placeholders_in_yaml_map(
        &step.with,
        &flow.inputs,
        known_vars,
        "cleanup with",
    )
    .with_context(|| format!("invalid cleanup input placeholders for '{}'", step.id))?;
    if let Some(prompt) = step.prompt.as_deref() {
        validate_supported_binding_placeholders_in_string(
            prompt,
            &flow.inputs,
            known_vars,
            "cleanup prompt",
        )
        .with_context(|| format!("invalid cleanup prompt placeholders for '{}'", step.id))?;
    }
    if let Some(prompt_file) = step.prompt_file.as_deref() {
        validate_supported_binding_placeholders_in_string(
            prompt_file,
            &flow.inputs,
            known_vars,
            "cleanup prompt_file",
        )
        .with_context(|| format!("invalid cleanup prompt_file placeholders for '{}'", step.id))?;
    }

    let step_like = step.as_flow_step();
    if let PromptValidationMode::Home(home_dir) = mode {
        ensure_step_action_installed(home_dir, &step_like, step_idx)
            .with_context(|| format!("invalid cleanup plugin configuration for '{}'", step.id))?;
        validate_step_secret_refs_declared(home_dir, &step_like, step_idx)
            .with_context(|| format!("invalid cleanup secret refs for '{}'", step.id))?;
    }
    validate_prompt_step(&step_like, step_idx, mode)
        .with_context(|| format!("invalid cleanup prompt configuration for '{}'", step.id))?;
    validate_skills_step(flow, &step_like, step_idx, mode, snapshot)
        .with_context(|| format!("invalid cleanup skills configuration for '{}'", step.id))?;
    if let Some(policy) = step.filesystem_policy.as_ref() {
        validate_filesystem_policy(
            policy,
            &format!("flow.cleanup[{step_idx}].filesystem_policy"),
            &flow.inputs,
        )?;
    }
    if let Some(policy) = step.network_policy.as_ref() {
        validate_network_policy(policy, &format!("flow.cleanup[{step_idx}].network"))?;
    }
    let _ = transist_protocol::FilesystemPolicy::resolve(
        None,
        workflow_filesystem_policy,
        step.filesystem_policy.as_ref(),
    );
    let _ = transist_protocol::NetworkPolicy::resolve(
        None,
        workflow_network_policy,
        step.network_policy.as_ref(),
    );
    Ok(())
}
