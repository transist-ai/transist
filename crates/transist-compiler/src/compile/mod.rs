// SPDX-License-Identifier: MPL-2.0
pub(crate) mod cleanup;
pub(crate) mod gates;
pub(crate) mod graph;
pub(crate) mod inputs;
pub(crate) mod prompts;
pub(crate) mod skills;
pub(crate) mod subflows;

use crate::WorkflowDefinitionSnapshot;
use crate::common::parse_duration_to_ms;
use crate::flow::{Flow, RECIPE_SCHEMA_V1};
use crate::settings::load_home_settings;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use transist_plugins::{
    PluginVersionSelector, list_plugins_in_home, plugin_dir_sha256,
    resolve_plugin_with_selector_in_home,
};
use transist_protocol::{
    KernelValue, Recipe, RecipeRepository, RecipeStep, StepExecutionDescriptor, WorkflowRef,
};

pub(crate) const LOOP_META_KEY: &str = "__transist_loop";
pub(crate) const SUBFLOW_META_KEY: &str = "__transist_subflow";
pub(crate) const GATE_DECISION_META_KEY: &str = "__transist_gate_decision";

/// Validates an authored flow and compiles it into a runtime recipe.
pub fn compile_flow(flow: &Flow) -> Result<Recipe> {
    crate::validate::validate_flow(flow)?;
    compile_flow_internal(flow, None, inputs::serialize_yaml_value_default, None)
}

/// Validates and compiles a flow using a concrete home directory for plugin-aware features.
pub fn compile_flow_in_home(home_dir: &Path, flow: &Flow) -> Result<Recipe> {
    crate::validate::validate_flow_in_home(home_dir, flow)?;
    let _ = load_home_settings(home_dir)?;
    compile_flow_internal(
        flow,
        Some(home_dir),
        inputs::serialize_yaml_value_default,
        None,
    )
}

/// Validates and compiles an immutable captured workflow without re-reading its definition files.
pub fn compile_workflow_snapshot_in_home(
    home_dir: &Path,
    snapshot: &WorkflowDefinitionSnapshot,
) -> Result<Recipe> {
    let flow = snapshot.top_level_flow()?;
    crate::validate::validate_workflow_snapshot_in_home(home_dir, snapshot)?;
    let _ = load_home_settings(home_dir)?;
    compile_flow_internal(
        &flow,
        Some(home_dir),
        inputs::serialize_yaml_value_default,
        Some(snapshot),
    )
}

/// Validates an immutable captured workflow without consulting plugin installation state.
pub fn compile_workflow_snapshot(snapshot: &WorkflowDefinitionSnapshot) -> Result<Recipe> {
    let flow = snapshot.top_level_flow()?;
    crate::validate::validate_flow_snapshot(&flow, snapshot)?;
    compile_flow_internal(
        &flow,
        None,
        inputs::serialize_yaml_value_default,
        Some(snapshot),
    )
}

pub(crate) fn compile_flow_internal(
    flow: &Flow,
    home_dir: Option<&Path>,
    serializer: inputs::YamlJsonSerializer,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<Recipe> {
    compile_flow_internal_mode(flow, home_dir, serializer, snapshot, false)
}

// Explanation alone permits absent contracts. This recipe stays private to the
// renderer and must never be installed, persisted, or used for execution.
pub(crate) fn compile_flow_internal_mode(
    flow: &Flow,
    home_dir: Option<&Path>,
    serializer: inputs::YamlJsonSerializer,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
    symbolic_contracts: bool,
) -> Result<Recipe> {
    let declared_flow = flow.with_declarations_applied()?;
    let flow = &declared_flow;
    if !symbolic_contracts
        && home_dir.is_none()
        && flow.steps.iter().any(|step| step.gate_decision.is_some())
    {
        bail!(
            "workflow '{}' uses gate_decision and requires plugin-aware compilation in a concrete home",
            flow.id
        );
    }
    let mut steps = Vec::with_capacity(flow.steps.len());
    for (step_idx, step) in flow.steps.iter().enumerate() {
        let mut with = inputs::yaml_map_to_kernel_with_serializer(
            &step.with,
            &flow.id,
            &step.id,
            flow.source_path.as_deref(),
            "step with",
            serializer,
        )?;
        if let Some(loop_block) = step.loop_block.as_ref() {
            let mut loop_meta = BTreeMap::new();
            loop_meta.insert(
                "max_iters".to_string(),
                KernelValue::Number(loop_block.max_iters.into()),
            );
            loop_meta.insert(
                "body_steps".to_string(),
                KernelValue::Array(
                    loop_block
                        .body
                        .iter()
                        .map(|entry| KernelValue::String(entry.ref_step.trim().to_string()))
                        .collect(),
                ),
            );
            if let Some(while_expr) = loop_block.while_expr.as_deref() {
                loop_meta.insert(
                    "while".to_string(),
                    KernelValue::String(while_expr.trim().to_string()),
                );
            }
            if let Some(until_expr) = loop_block.until.as_deref() {
                loop_meta.insert(
                    "until".to_string(),
                    KernelValue::String(until_expr.trim().to_string()),
                );
            }
            if let Some(max_duration) = loop_block.max_duration.as_deref() {
                loop_meta.insert(
                    "max_duration_ms".to_string(),
                    KernelValue::Number(parse_duration_to_ms(max_duration)?.into()),
                );
            }
            with.insert(LOOP_META_KEY.to_string(), KernelValue::Object(loop_meta));
        }

        let execution = if let Some(subflow) = step.subflow.as_ref() {
            let (child_recipe, outputs) = subflows::compile_subflow_recipe(
                flow,
                step,
                subflow,
                home_dir,
                snapshot,
                symbolic_contracts,
            )?;
            let subflow_inputs = inputs::compile_subflow_inputs_with_serializer(
                flow,
                &step.id,
                &subflow.inputs,
                serializer,
            )?;
            with.extend(subflow_inputs);
            // Modules execute in the root workflow's lexical scope. Forward
            // inherited root inputs unless the call explicitly overrides one.
            for input_name in flow.inputs.keys() {
                if !subflow.inputs.contains_key(input_name) {
                    with.insert(
                        input_name.clone(),
                        KernelValue::String(format!("{{{{inputs.{input_name}}}}}")),
                    );
                }
            }
            let mut metadata = BTreeMap::new();
            if let Some(rerun_mode) = subflow.rerun_mode.as_deref() {
                metadata.insert(
                    "rerun_mode".into(),
                    KernelValue::String(rerun_mode.trim().into()),
                );
            }
            if !outputs.is_empty() {
                metadata.insert("outputs".into(), KernelValue::Object(outputs));
            }
            if !metadata.is_empty() {
                with.insert(SUBFLOW_META_KEY.into(), KernelValue::Object(metadata));
            }
            StepExecutionDescriptor::Subflow {
                recipe: Box::new(child_recipe),
            }
        } else if let Some(control) = step.control.as_ref() {
            StepExecutionDescriptor::Control {
                primitive: control.trim().to_string(),
            }
        } else {
            let plugin_id = step.plugin.clone().expect("validated plugin");
            let (resolved_plugin_version, plugin_sha256) = resolve_step_plugin_pin(
                home_dir,
                &plugin_id,
                step.plugin_version.as_deref(),
                &step.id,
            )?;
            if let Some(home_dir) = home_dir {
                validate_compiled_step_secret_refs(
                    home_dir,
                    &step.id,
                    &plugin_id,
                    resolved_plugin_version.as_deref(),
                    step.action.as_deref().expect("validated action"),
                    &with,
                )?;
            }
            StepExecutionDescriptor::Invoke {
                adapter: crate::validate::flow_step_adapter(step)?,
                plugin_id: Some(plugin_id.clone()),
                plugin: plugin_id,
                action: step.action.clone().expect("validated action"),
                execution_kind: step
                    .execution_kind
                    .clone()
                    .expect("validated execution kind"),
                plugin_version: resolved_plugin_version,
                plugin_sha256,
            }
        };
        let prompt_binding = match home_dir {
            Some(home_dir) => prompts::compile_prompt_binding(home_dir, step, step_idx)?,
            None => None,
        };
        let skills_binding = match home_dir {
            Some(home_dir) => {
                skills::compile_skills_binding(home_dir, flow, step, step_idx, snapshot)?
            }
            None => None,
        };
        if let Some(home_dir) = home_dir
            && let Some(binding) =
                gates::compile_gate_decision_binding(home_dir, flow, step, step_idx)?
        {
            with.insert(GATE_DECISION_META_KEY.to_string(), binding);
        }

        let vars = step
            .set
            .as_ref()
            .map(|set| {
                inputs::yaml_map_to_kernel_with_serializer(
                    set,
                    &flow.id,
                    &step.id,
                    flow.source_path.as_deref(),
                    "step set",
                    serializer,
                )
            })
            .transpose()?
            .unwrap_or_default();

        let mut compiled = RecipeStep {
            id: step.id.clone(),
            execution,
            with,
            vars,
            prompt_binding,
            skills_binding,
            filesystem_policy: step.filesystem_policy.clone(),
            network_policy: step.network_policy.clone(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        };
        compile_document_bindings(&mut compiled, &flow.documents)?;
        let deferred =
            transist_protocol::step_deferred_bindings(&compiled).map_err(anyhow::Error::msg)?;
        if !deferred.is_empty() {
            compiled.with.insert(
                transist_protocol::DEFERRED_BINDINGS_KEY.into(),
                KernelValue::from_json(serde_json::to_value(deferred)?),
            );
        }
        steps.push(compiled);
    }

    let recipe = Recipe {
        documents: flow.documents.clone(),
        schema: RECIPE_SCHEMA_V1.to_string(),
        workflow: WorkflowRef {
            id: flow.id.clone(),
            version: flow.version.clone(),
        },
        source_dir: flow
            .source_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|path| path.display().to_string()),
        workspace: flow.workspace.clone(),
        repositories: Vec::<RecipeRepository>::new(),
        prerequisites: flow.prerequisites.clone(),
        filesystem_policy: flow.filesystem_policy.clone(),
        network_policy: flow.network_policy.clone(),
        inputs: inputs::compile_recipe_inputs_with_serializer(flow, serializer)?,
        steps,
        cleanup: cleanup::compile_cleanup_steps_with_serializer(
            flow, home_dir, serializer, snapshot,
        )?,
        graph: Some(graph::compile_graph(flow)),
        capabilities: BTreeMap::new(),
        capabilities_persisted: false,
    };
    transist_protocol::validate_document_reviews(&recipe).map_err(anyhow::Error::msg)?;
    transist_protocol::validate_recipe_loop_routing(&recipe).map_err(anyhow::Error::msg)?;
    Ok(recipe)
}

/// Pins root-lexical action-input pointers/access requirements in compiler metadata.
pub(crate) fn compile_document_bindings(
    step: &mut RecipeStep,
    declarations: &BTreeMap<String, transist_protocol::DocumentDeclaration>,
) -> Result<()> {
    let bindings = transist_protocol::step_document_bindings(step).map_err(anyhow::Error::msg)?;
    for location in &bindings {
        if !declarations.contains_key(&location.binding.document) {
            bail!(
                "step '{}' references undeclared $document '{}'",
                step.id,
                location.binding.document
            );
        }
    }
    if !bindings.is_empty() {
        step.with.insert(
            transist_protocol::DOCUMENT_BINDINGS_KEY.into(),
            KernelValue::from_json(serde_json::to_value(bindings)?),
        );
    }
    Ok(())
}

fn validate_compiled_step_secret_refs(
    home_dir: &Path,
    step_id: &str,
    plugin_id: &str,
    plugin_version: Option<&str>,
    action_name: &str,
    with: &BTreeMap<String, KernelValue>,
) -> Result<()> {
    let Some(plugin_version) = plugin_version.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    let (manifest, _) = resolve_plugin_with_selector_in_home(
        home_dir,
        plugin_id,
        &PluginVersionSelector::Exact(plugin_version.to_string()),
    )?;
    let action = manifest
        .actions
        .iter()
        .find(|candidate| candidate.name == action_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "step '{}' references unknown action '{}.{}'",
                step_id,
                plugin_id,
                action_name
            )
        })?;
    let secret_refs = compiled_secret_refs(with);
    let has_dynamic_secret_ref = with
        .get("secret_refs")
        .map(|value| match value {
            KernelValue::Array(values) => values.iter().any(
                |value| matches!(value, KernelValue::String(value) if value.replace(' ', "").contains("{{inputs.")),
            ),
            _ => false,
        })
        .unwrap_or(false);
    let declared_secret_refs: BTreeSet<_> = action
        .secret_inputs
        .iter()
        .map(|secret| secret.name.as_str())
        .collect();
    for secret_ref in &secret_refs {
        if !secret_ref.replace(' ', "").contains("{{inputs.")
            && !declared_secret_refs.contains(secret_ref.as_str())
        {
            bail!(
                "step '{}' references undeclared secret ref '{}' for action '{}.{}'",
                step_id,
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
                "step '{}' is missing required secret ref '{}' for action '{}.{}'",
                step_id,
                required_secret.name,
                plugin_id,
                action_name
            );
        }
    }
    Ok(())
}

fn compiled_secret_refs(with: &BTreeMap<String, KernelValue>) -> Vec<String> {
    let Some(KernelValue::Array(values)) = with.get("secret_refs") else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            KernelValue::String(value) if !value.trim().is_empty() => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// Resolves the authored `plugin_version` selector to a concrete version and
/// pins the installed plugin directory's deterministic SHA-256 digest.
///
/// When `home_dir` is available:
/// - an explicit `"latest"` selector resolves to the highest installed SemVer;
/// - an exact version is validated against installed versions;
/// - an omitted selector infers the version when exactly one is installed, and
///   fails at compile time when multiple versions are installed (guidance:
///   pin a `plugin_version`) or when none is installed.
///
/// The resolved plugin directory digest is materialized into the compiled
/// recipe so runtime re-entry can detect byte-level drift.
///
/// When no home context is available (i.e. the `compile_flow` path), exact
/// selectors pass through unchanged, `"latest"` and omitted selectors resolve
/// to `None`, and no digest is computed.
pub(crate) fn resolve_step_plugin_pin(
    home_dir: Option<&Path>,
    plugin_id: &str,
    plugin_version: Option<&str>,
    step_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    let Some(home) = home_dir else {
        // No home context: exact versions pass through; "latest" and omitted are unresolvable.
        if let Some(selector_str) = plugin_version {
            let selector_str = selector_str.trim();
            if selector_str == "latest" {
                return Ok((None, None));
            }
            return Ok((Some(selector_str.to_string()), None));
        }
        return Ok((None, None));
    };

    let selector_str = plugin_version.map(|s| s.trim()).unwrap_or("");
    let selector = if selector_str.is_empty() {
        // Omitted selector: infer from installed versions.
        let installed: Vec<_> = list_plugins_in_home(home)?
            .into_iter()
            .filter(|p| p.id == plugin_id)
            .collect();
        match installed.len() {
            0 => {
                bail!(
                    "step '{}' references plugin '{}' which is not installed; install it or pin an installed plugin_version",
                    step_id,
                    plugin_id
                )
            }
            1 => {
                let dir = installed[0].dir.clone();
                let version = installed[0].version.clone();
                let sha = plugin_dir_sha256(&dir).with_context(|| {
                    format!(
                        "failed computing plugin digest for '{}' version '{}'",
                        plugin_id, version
                    )
                })?;
                return Ok((Some(version), Some(sha)));
            }
            _ => bail!(
                "step '{}' references plugin '{}' which has multiple installed versions; \
                 pin a single plugin_version on the step",
                step_id,
                plugin_id
            ),
        }
    } else if selector_str == "latest" {
        PluginVersionSelector::Latest
    } else {
        PluginVersionSelector::Exact(selector_str.to_string())
    };

    let (manifest, dir) = resolve_plugin_with_selector_in_home(home, plugin_id, &selector)
        .with_context(|| {
            format!(
                "failed resolving plugin_version '{}' for step '{}'",
                selector_str, step_id
            )
        })?;
    let sha = plugin_dir_sha256(&dir).with_context(|| {
        format!(
            "failed computing plugin digest for '{}' version '{}'",
            plugin_id, manifest.version
        )
    })?;
    Ok((Some(manifest.version), Some(sha)))
}
