// SPDX-License-Identifier: MPL-2.0
//! Authored package-local module normalization.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::flow::{FLOW_SCHEMA_V1, Flow, FlowSubflowCall};

/// Schema identifier for package-local authored modules.
pub const MODULE_SCHEMA_V1: &str = "transist.module.v1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleDeclaration {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ModuleInvocation {
    Reference(String),
    Call(ModuleCall),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleCall {
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    inputs: BTreeMap<String, yaml_serde::Value>,
    #[serde(default)]
    rerun_mode: Option<String>,
}

/// Parses a workflow-root document and lowers authored module calls to subflow calls.
pub(crate) fn parse_root_flow(raw: &str) -> Result<Flow> {
    parse_flow_document(raw, true)
}

/// Parses a package-local module document.
pub(crate) fn parse_child_flow(raw: &str) -> Result<Flow> {
    parse_flow_document(raw, false)
}

/// Reports whether raw authored bytes identify a package-local module.
pub(crate) fn is_module_document(raw: &str) -> Result<bool> {
    let document: yaml_serde::Value = yaml_serde::from_str(raw).context("invalid workflow YAML")?;
    Ok(document_schema(&document)? == MODULE_SCHEMA_V1)
}

/// Returns every source path declared by a top-level workflow module map.
pub(crate) fn root_module_paths(raw: &str) -> Result<Vec<String>> {
    let document: yaml_serde::Value = yaml_serde::from_str(raw).context("invalid workflow YAML")?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("workflow document must be a mapping"))?;
    if document_schema(&document)? != FLOW_SCHEMA_V1 {
        return Ok(Vec::new());
    }
    let declarations = parse_module_declarations(mapping.get("modules").cloned())?;
    Ok(declarations
        .into_values()
        .map(|declaration| declaration.path)
        .collect())
}

fn parse_flow_document(raw: &str, root: bool) -> Result<Flow> {
    let mut document: yaml_serde::Value =
        yaml_serde::from_str(raw).context("invalid workflow YAML")?;
    let mapping = document
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("workflow document must be a mapping"))?;
    let schema_key = yaml_serde::Value::String("schema".to_string());
    let schema = mapping
        .get(&schema_key)
        .and_then(yaml_serde::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("workflow schema is required"))?
        .to_string();

    reject_retired_authored_fields(mapping)?;
    if !root && mapping.contains_key("documents") {
        bail!("documents declarations belong to the root workflow; modules inherit them lexically");
    }
    if root && mapping.contains_key("outputs") {
        bail!("outputs declarations belong to package-local modules");
    }

    if !root && schema != MODULE_SCHEMA_V1 {
        bail!("declared modules must use schema transist.module.v1")
    }

    // Ordinary flows retain the original parser path and its source locations.
    // Only documents that use the authored module surface are reserialized.
    if schema != MODULE_SCHEMA_V1
        && !mapping.contains_key("modules")
        && !contains_module_step(mapping)
    {
        return yaml_serde::from_str(raw).context("invalid flow YAML");
    }

    let declarations = match schema.as_str() {
        FLOW_SCHEMA_V1 if root => parse_module_declarations(mapping.remove("modules"))?,
        FLOW_SCHEMA_V1 => {
            bail!("declared modules must use schema transist.module.v1")
        }
        MODULE_SCHEMA_V1 => {
            if root {
                bail!(
                    "package-local module definitions cannot be installed or launched independently"
                )
            }
            mapping.insert(
                schema_key,
                yaml_serde::Value::String(FLOW_SCHEMA_V1.to_string()),
            );
            parse_module_declarations(mapping.remove("modules"))?
        }
        _ => BTreeMap::new(),
    };

    let module_calls = lower_module_steps(mapping, &declarations)?;
    let normalized =
        yaml_serde::to_string(&document).context("failed serializing normalized flow")?;
    let mut flow: Flow =
        yaml_serde::from_str(&normalized).context("invalid normalized flow YAML")?;
    for (step, module_call) in flow.steps.iter_mut().zip(module_calls) {
        if let Some(module_call) = module_call {
            step.control = None;
            step.subflow = Some(module_call);
        }
    }
    Ok(flow)
}

fn contains_module_step(document: &yaml_serde::Mapping) -> bool {
    document
        .get("steps")
        .and_then(yaml_serde::Value::as_sequence)
        .is_some_and(|steps| {
            steps.iter().any(|step| {
                step.as_mapping()
                    .is_some_and(|step| step.contains_key("module"))
            })
        })
}

fn parse_module_declarations(
    declarations: Option<yaml_serde::Value>,
) -> Result<BTreeMap<String, ModuleDeclaration>> {
    let Some(declarations) = declarations else {
        return Ok(BTreeMap::new());
    };
    let rendered = yaml_serde::to_string(&declarations)
        .context("failed reading workflow modules declarations")?;
    let declarations: BTreeMap<String, ModuleDeclaration> = yaml_serde::from_str(&rendered)
        .context("workflow modules must be a mapping of names to { path } declarations")?;
    for (name, declaration) in &declarations {
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::StepId, name)
            .map_err(|error| anyhow::anyhow!("workflow module '{name}': {error}"))?;
        if declaration.path.trim().is_empty() {
            bail!("workflow module '{name}' path is required")
        }
    }
    Ok(declarations)
}

fn lower_module_steps(
    document: &mut yaml_serde::Mapping,
    declarations: &BTreeMap<String, ModuleDeclaration>,
) -> Result<Vec<Option<FlowSubflowCall>>> {
    let Some(steps) = document.get_mut("steps") else {
        return Ok(Vec::new());
    };
    let steps = steps
        .as_sequence_mut()
        .ok_or_else(|| anyhow::anyhow!("workflow steps must be a sequence"))?;
    let mut module_calls = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter_mut().enumerate() {
        let step = step
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("workflow step {index} must be a mapping"))?;
        let Some(module) = step.remove("module") else {
            module_calls.push(None);
            continue;
        };
        if step.contains_key("uses") || step.contains_key("control") {
            bail!("workflow step {index} cannot combine module with uses or control")
        }
        let (reference, inputs, rerun_mode) = parse_module_invocation(module)?;
        let declaration = declarations.get(&reference).ok_or_else(|| {
            anyhow::anyhow!("workflow step {index} references undeclared module '{reference}'")
        })?;
        let subflow = FlowSubflowCall {
            path: declaration.path.clone(),
            inputs,
            rerun_mode,
        };
        // Parse the public module syntax first, then attach the child-call
        // representation after deserialization. `subflow` is never accepted in
        // authored YAML but remains an internal compiler representation.
        step.insert(
            "control".into(),
            yaml_serde::Value::String("control.noop".to_string()),
        );
        module_calls.push(Some(subflow));
    }
    Ok(module_calls)
}

fn reject_retired_authored_fields(document: &yaml_serde::Mapping) -> Result<()> {
    reject_retired_step_fields(document.get("steps"), "flow.steps")?;
    reject_retired_step_fields(document.get("cleanup"), "flow.cleanup")?;
    Ok(())
}

fn reject_retired_step_fields(steps: Option<&yaml_serde::Value>, path: &str) -> Result<()> {
    let Some(steps) = steps else {
        return Ok(());
    };
    let steps = steps
        .as_sequence()
        .ok_or_else(|| anyhow::anyhow!("{path} must be a sequence"))?;
    for (index, step) in steps.iter().enumerate() {
        let step = step
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("{path}[{index}] must be a mapping"))?;
        for field in [
            "adapter",
            "plugin",
            "action",
            "plugin_version",
            "execution_kind",
        ] {
            if step.contains_key(field) {
                bail!(
                    "{path}[{index}].{field} is no longer supported; use uses: <adapter>:<plugin>/<action>"
                );
            }
        }
        if step.contains_key("subflow") {
            bail!(
                "{path}[{index}].subflow is no longer supported; declare a top-level module and use module: <name>"
            );
        }
    }
    Ok(())
}

fn parse_module_invocation(
    invocation: yaml_serde::Value,
) -> Result<(String, BTreeMap<String, yaml_serde::Value>, Option<String>)> {
    let rendered =
        yaml_serde::to_string(&invocation).context("failed reading module invocation")?;
    let invocation: ModuleInvocation = yaml_serde::from_str(&rendered)
        .context("module must be a declared module name or { ref, inputs, rerun_mode }")?;
    match invocation {
        ModuleInvocation::Reference(reference) => Ok((reference, BTreeMap::new(), None)),
        ModuleInvocation::Call(call) => Ok((call.reference, call.inputs, call.rerun_mode)),
    }
}

fn document_schema(document: &yaml_serde::Value) -> Result<&str> {
    document
        .as_mapping()
        .and_then(|mapping| mapping.get("schema"))
        .and_then(yaml_serde::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("workflow schema is required"))
}
