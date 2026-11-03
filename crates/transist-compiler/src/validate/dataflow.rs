// SPDX-License-Identifier: MPL-2.0
//! Strict recipe-local data selectors and conservative schema compatibility.

use super::PromptValidationMode;
use crate::{Flow, FlowStep, WorkflowDefinitionSnapshot};
use anyhow::{Result, bail};
use std::collections::BTreeSet;
use transist_protocol::{
    KernelValue, binding_selectors, collect_deferred_bindings, split_output_selector_for_ids,
};

pub(super) fn validate_dataflow(
    flow: &Flow,
    mode: PromptValidationMode<'_>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<()> {
    let known_vars = flow
        .steps
        .iter()
        .filter_map(|step| step.set.as_ref())
        .flat_map(|set| set.keys().cloned())
        .collect();
    super::inputs::validate_supported_binding_placeholders_in_yaml_map(
        &flow.outputs,
        &flow.inputs,
        &known_vars,
        "module outputs",
    )?;
    for (name, value) in &flow.outputs {
        if name.contains('.')
            || name.starts_with("__transist_")
            || matches!(
                name.as_str(),
                "subflow_id"
                    | "subflow_version"
                    | "child_run_id"
                    | "status"
                    | "bound_inputs"
                    | "child_steps"
                    | "failure"
            )
        {
            bail!("module export '{name}' uses a reserved output name");
        }
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::StepId, name)?;
        validate_value(flow, value, None, mode, snapshot)?;
    }
    for step in &flow.steps {
        validate_documents(flow, step, mode)?;
        for (key, value) in &step.with {
            if key.starts_with("__transist_") {
                bail!("step '{}' cannot define reserved input '{key}'", step.id);
            }
            if key == "secret_refs" {
                reject_dynamic_authority(&serde_json::to_value(value)?, "secret_refs")?;
            }
            if key == "gate_id"
                && step.control.as_deref() == Some("control.gate.manual")
                && value.as_str().is_some_and(|value| value.contains("{{"))
            {
                bail!("manual gate identity must be a literal, not runtime data");
            }
            validate_value(flow, value, Some((step, key.as_str())), mode, snapshot)?;
        }
        if let Some(skills) = &step.skills {
            for skill in skills {
                reject_dynamic_authority(
                    &serde_json::Value::String(skill.clone()),
                    "skills identity",
                )?;
            }
        }
        if let Some(prompt) = &step.prompt {
            validate_value(
                flow,
                &yaml_serde::Value::String(prompt.clone()),
                None,
                mode,
                snapshot,
            )?;
            validate_inline_output_types(flow, step, prompt, mode, snapshot)?;
        }
        if let Some(call) = &step.subflow {
            for (name, value) in &call.inputs {
                validate_value(flow, value, None, mode, snapshot)?;
                let value = KernelValue::from_json(serde_json::to_value(value)?);
                let bindings = collect_deferred_bindings(&value, "")
                    .map_err(anyhow::Error::msg)?
                    .into_iter()
                    .filter(|binding| {
                        binding.whole_value
                            && binding.path.is_empty()
                            && binding.selector.starts_with("steps.")
                    })
                    .collect::<Vec<_>>();
                if bindings.is_empty() {
                    continue;
                }
                let parent = flow
                    .source_path
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("module requires source"))?;
                let path = crate::io::resolve_relative_path(parent, &call.path);
                let mut child = match snapshot {
                    Some(s) => s.flow_for_source(&path)?,
                    None => crate::io::load_child_flow_unvalidated(&path)?,
                };
                crate::compile::subflows::inherit_module_scope(flow, step, &mut child)?;
                if let Some(expected) = child.inputs.get(name) {
                    for binding in bindings {
                        if binding.whole_value
                            && binding.path.is_empty()
                            && binding.selector.starts_with("steps.")
                        {
                            let (id, path) = split_output_selector_for_ids(
                                &binding.selector,
                                flow.steps.iter().map(|step| step.id.as_str()),
                            )
                            .map_err(anyhow::Error::msg)?;
                            let producer = flow
                                .steps
                                .iter()
                                .find(|s| s.id == id)
                                .ok_or_else(|| anyhow::anyhow!("unknown producer '{id}'"))?;
                            let actual = output_types(flow, producer, path, mode, snapshot, 0)?;
                            if !actual.is_empty()
                                && !actual.contains(&expected.value_type)
                                && (!actual.contains("null") || expected.required)
                            {
                                bail!(
                                    "statically incompatible output '{}' for module '{}' input '{name}'",
                                    binding.selector,
                                    step.id
                                );
                            }
                        }
                    }
                }
            }
        }
        for value in [
            serde_json::to_value(&step.filesystem_policy)?,
            serde_json::to_value(&step.network_policy)?,
        ] {
            reject_dynamic_authority(&value, "step policy")?;
        }
        for value in [
            &step.adapter,
            &step.plugin,
            &step.action,
            &step.plugin_version,
        ]
        .into_iter()
        .flatten()
        {
            reject_dynamic_authority(
                &serde_json::Value::String(value.clone()),
                "executable identity",
            )?;
        }
        if let Some(path) = &step.prompt_file {
            reject_dynamic_authority(&serde_json::Value::String(path.clone()), "prompt_file")?;
        }
    }
    for cleanup in &flow.cleanup {
        let step = cleanup.as_flow_step();
        validate_documents(flow, &step, mode)?;
        for (key, value) in &step.with {
            if key.starts_with("__transist_") {
                bail!("cleanup cannot define reserved input '{key}'");
            }
            if key == "secret_refs" {
                reject_dynamic_authority(&serde_json::to_value(value)?, "cleanup secret_refs")?;
            }
            validate_value(flow, value, Some((&step, key)), mode, snapshot)?;
        }
        if let Some(prompt) = &step.prompt {
            validate_value(
                flow,
                &yaml_serde::Value::String(prompt.clone()),
                None,
                mode,
                snapshot,
            )?;
            validate_inline_output_types(flow, &step, prompt, mode, snapshot)?;
        }
        if let Some(path) = &step.prompt_file {
            reject_dynamic_authority(
                &serde_json::Value::String(path.clone()),
                "cleanup prompt_file",
            )?;
        }
    }
    for value in [
        serde_json::to_value(&flow.workspace)?,
        serde_json::to_value(&flow.filesystem_policy)?,
        serde_json::to_value(&flow.network_policy)?,
    ] {
        reject_dynamic_authority(&value, "workflow authority")?;
    }
    Ok(())
}

fn validate_documents(flow: &Flow, step: &FlowStep, mode: PromptValidationMode<'_>) -> Result<()> {
    let bindings = transist_protocol::collect_document_bindings(&serde_json::to_value(&step.with)?)
        .map_err(anyhow::Error::msg)?;
    if !bindings.is_empty()
        && (step.adapter.as_deref() != Some("process")
            || step.control.is_some()
            || step.subflow.is_some())
    {
        bail!("$document bindings support process invoke actions only; HTTP/MCP are unsupported");
    }
    for location in bindings {
        if !flow.documents.contains_key(&location.binding.document) {
            bail!("undeclared $document '{}'", location.binding.document);
        }
        if let PromptValidationMode::Home(home) = mode {
            let (action, dir) = action_package(home, step)?;
            let path = location
                .path
                .split('/')
                .skip(1)
                .map(|p| p.replace("~1", "/").replace("~0", "~"))
                .collect::<Vec<_>>();
            let types = transist_plugins::schema_types_at_path(&dir, &action.input_schema, &path)?;
            if !types.is_empty() && !types.contains("string") {
                bail!(
                    "statically incompatible $document string path at '{}'",
                    location.path
                );
            }
        }
    }
    Ok(())
}

fn reject_dynamic_authority(value: &serde_json::Value, field: &str) -> Result<()> {
    match value {
        serde_json::Value::String(raw) => {
            for selector in binding_selectors(raw).map_err(anyhow::Error::msg)? {
                if !selector.starts_with("inputs.") {
                    bail!(
                        "{field} authority may depend only on immutable inputs, not '{selector}'"
                    );
                }
            }
        }
        serde_json::Value::Array(values) => {
            for v in values {
                reject_dynamic_authority(v, field)?;
            }
        }
        serde_json::Value::Object(values) => {
            for v in values.values() {
                reject_dynamic_authority(v, field)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_value(
    flow: &Flow,
    value: &yaml_serde::Value,
    consumer: Option<(&FlowStep, &str)>,
    mode: PromptValidationMode<'_>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<()> {
    reject_dynamic_source(value)?;
    let value = KernelValue::from_json(serde_json::to_value(value)?);
    for binding in collect_deferred_bindings(&value, "").map_err(anyhow::Error::msg)? {
        if !binding.selector.starts_with("steps.") {
            continue;
        }
        let (id, path) = split_output_selector_for_ids(
            &binding.selector,
            flow.steps.iter().map(|step| step.id.as_str()),
        )
        .map_err(anyhow::Error::msg)?;
        let producer = flow
            .steps
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown step '{id}' in '{}'", binding.selector))?;
        if consumer.is_some_and(|(step, _)| step.id == id) {
            bail!("step '{id}' cannot consume its own incomplete output");
        }
        let produced = output_types(flow, producer, path, mode, snapshot, 0)?;
        if !binding.whole_value
            && !produced.is_empty()
            && produced.iter().all(|t| t == "array" || t == "object")
        {
            bail!(
                "structured output '{}' cannot be interpolated into a string",
                binding.selector
            );
        }
        if let (Some((consumer, key)), PromptValidationMode::Home(home)) = (consumer, mode) {
            if consumer.subflow.is_some() || consumer.control.is_some() {
                continue;
            }
            let (action, dir) = action_package(home, consumer)?;
            let mut destination = vec![key.to_string()];
            destination.extend(
                binding
                    .path
                    .split('/')
                    .skip(1)
                    .map(|s| s.replace("~1", "/").replace("~0", "~")),
            );
            let expected =
                transist_plugins::schema_types_at_path(&dir, &action.input_schema, &destination)?;
            let actual = if binding.whole_value {
                produced
            } else {
                BTreeSet::from(["string".into()])
            };
            if !actual.is_empty() && !expected.is_empty() && actual.is_disjoint(&expected) {
                bail!(
                    "statically incompatible output '{}' for step '{}' input '{key}': producer {actual:?}, consumer {expected:?}",
                    binding.selector,
                    consumer.id
                );
            }
        }
    }
    Ok(())
}

fn output_types(
    flow: &Flow,
    step: &FlowStep,
    path: &str,
    mode: PromptValidationMode<'_>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
    depth: usize,
) -> Result<BTreeSet<String>> {
    if depth > 8 {
        return Ok(BTreeSet::new());
    }
    if let Some(call) = &step.subflow {
        let parent = flow
            .source_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("module requires source"))?;
        let child_path = crate::io::resolve_relative_path(parent, &call.path);
        let mut child = match snapshot {
            Some(s) => s.flow_for_source(&child_path)?,
            None => crate::io::load_child_flow_unvalidated(&child_path)?,
        };
        crate::compile::subflows::inherit_module_scope(flow, step, &mut child)?;
        let child = child.with_declarations_applied()?;
        let (export, suffix) = path.split_once('.').unwrap_or((path, ""));
        let value = child
            .outputs
            .get(export)
            .ok_or_else(|| anyhow::anyhow!("module step '{}' has no export '{export}'", step.id))?;
        if let Some(raw) = value.as_str() {
            let selectors = binding_selectors(raw).map_err(anyhow::Error::msg)?;
            if selectors.len() == 1 && raw.trim() == format!("{{{{{}}}}}", selectors[0]) {
                if selectors[0].starts_with("steps.") {
                    let (id, selected) = split_output_selector_for_ids(
                        selectors[0],
                        child.steps.iter().map(|step| step.id.as_str()),
                    )
                    .map_err(anyhow::Error::msg)?;
                    let producer = child
                        .steps
                        .iter()
                        .find(|s| s.id == id)
                        .ok_or_else(|| anyhow::anyhow!("unknown export producer '{id}'"))?;
                    let path = if suffix.is_empty() {
                        selected.into()
                    } else {
                        format!("{selected}.{suffix}")
                    };
                    return output_types(&child, producer, &path, mode, snapshot, depth + 1);
                }
                if let Some(input) = selectors[0].strip_prefix("inputs.") {
                    if !suffix.is_empty() {
                        return Ok(BTreeSet::new());
                    }
                    return Ok(child
                        .inputs
                        .get(input)
                        .map(|spec| {
                            let mut types = BTreeSet::from([spec.value_type.clone()]);
                            if !spec.required && spec.default.is_none() {
                                types.insert("null".into());
                            }
                            types
                        })
                        .unwrap_or_default());
                }
            }
        }
        let mut literal = serde_json::to_value(value)?;
        if !suffix.is_empty() {
            for segment in suffix.split('.') {
                literal = match &literal {
                    serde_json::Value::Array(values) => {
                        segment.parse::<usize>().ok().and_then(|i| values.get(i))
                    }
                    serde_json::Value::Object(values) => values.get(segment),
                    _ => None,
                }
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            }
        }
        if let Some(raw) = literal.as_str()
            && !binding_selectors(raw)
                .map_err(anyhow::Error::msg)?
                .is_empty()
        {
            return Ok(BTreeSet::new());
        }
        return Ok(BTreeSet::from([match literal {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
        .into()]));
    }
    if let PromptValidationMode::Home(home) = mode {
        if step.control.is_some() {
            return Ok(BTreeSet::new());
        }
        let (action, dir) = action_package(home, step)?;
        return transist_plugins::schema_types_at_path(
            &dir,
            &action.output_schema,
            &path.split('.').map(str::to_string).collect::<Vec<_>>(),
        );
    }
    Ok(BTreeSet::new())
}

fn reject_dynamic_source(value: &yaml_serde::Value) -> Result<()> {
    match value {
        yaml_serde::Value::String(raw) if raw.starts_with("source://") => reject_dynamic_authority(
            &serde_json::Value::String(raw.clone()),
            "source asset identity",
        )?,
        yaml_serde::Value::Sequence(values) => {
            for value in values {
                reject_dynamic_source(value)?;
            }
        }
        yaml_serde::Value::Mapping(values) => {
            for value in values.values() {
                reject_dynamic_source(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_inline_output_types(
    flow: &Flow,
    step: &FlowStep,
    prompt: &str,
    mode: PromptValidationMode<'_>,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<()> {
    for selector in binding_selectors(prompt).map_err(anyhow::Error::msg)? {
        if !selector.starts_with("steps.") {
            continue;
        }
        let (id, path) =
            split_output_selector_for_ids(selector, flow.steps.iter().map(|step| step.id.as_str()))
                .map_err(anyhow::Error::msg)?;
        if id == step.id {
            bail!("step '{id}' cannot consume its own incomplete output");
        }
        let producer = flow
            .steps
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown producer '{id}'"))?;
        let produced = output_types(flow, producer, path, mode, snapshot, 0)?;
        if !produced.is_empty() && produced.iter().all(|t| t == "array" || t == "object") {
            bail!("structured output '{selector}' cannot be used as inline prompt text");
        }
        if let PromptValidationMode::Home(home) = mode {
            let (action, dir) = action_package(home, step)?;
            if let Some(binding) = action.prompt_binding()? {
                let accepted = transist_plugins::schema_types_at_path(
                    &dir,
                    &action.input_schema,
                    &[binding.inline_field],
                )?;
                if !accepted.is_empty() && !accepted.contains("string") {
                    bail!(
                        "statically incompatible inline prompt binding for step '{}'",
                        step.id
                    );
                }
            }
        }
    }
    Ok(())
}

fn action_package(
    home: &std::path::Path,
    step: &FlowStep,
) -> Result<(transist_plugins::ActionDescriptor, std::path::PathBuf)> {
    let plugin = step.plugin.as_deref().unwrap_or_default();
    let (manifest, dir) = match step.plugin_version.as_deref() {
        None => transist_plugins::resolve_plugin_in_home(home, plugin)?,
        Some("latest") => transist_plugins::resolve_plugin_with_selector_in_home(
            home,
            plugin,
            &transist_plugins::PluginVersionSelector::Latest,
        )?,
        Some(version) => transist_plugins::resolve_plugin_with_selector_in_home(
            home,
            plugin,
            &transist_plugins::PluginVersionSelector::Exact(version.into()),
        )?,
    };
    let action = manifest
        .actions
        .into_iter()
        .find(|a| Some(a.name.as_str()) == step.action.as_deref())
        .ok_or_else(|| anyhow::anyhow!("unknown action for step '{}'", step.id))?;
    Ok((action, dir))
}
