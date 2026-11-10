// SPDX-License-Identifier: MPL-2.0
//! Pure authoring inspection over captured definitions and exact local contracts.
//! No runtime context, policy evaluator, secret resolver, or adapter is used.
mod bindings;
mod compare;
pub use compare::compare_explanations;
pub use transist_protocol::explanation::*;

use crate::{Flow, FlowStep, WorkflowDefinitionSnapshot};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use transist_plugins::ValidatedPluginCatalog;
use transist_protocol::{KernelValue, Recipe, RecipeStep, StepExecutionDescriptor};
use transist_trust_digest::sha256_hex;

/// Explanations describe declarations, never validation, authorization, or consent.
pub const EXPLANATION_NOTICE: &str = "Static authoring explanation only: not validation, authorization, readiness, or execution consent. External actions, scripts, and prompts are opaque; no effects are inferred. Authority is requested, not granted. No document revisions or approvals are created.";

/// Captures and explains authored source using only an optional read-only local catalog.
/// Missing homes/packages remain unresolved. Inputs are ordinary data, never secrets.
/// Does not install, initialize, inspect policy, probe prerequisites, or dispatch work.
pub fn explain_workflow(
    path: &Path,
    home: Option<&Path>,
    inputs: &BTreeMap<String, KernelValue>,
) -> Result<WorkflowExplanation> {
    let snapshot = crate::capture_workflow_definition(path)?;
    let catalog = home.map(|home| ValidatedPluginCatalog::new(home.to_path_buf(), BTreeMap::new()));
    explain_snapshot(&snapshot, catalog.as_ref(), inputs)
}

/// Explains immutable compiler-captured bytes against one operation-scoped catalog.
/// The resulting symbolic recipe is private and cannot be used for execution.
pub fn explain_snapshot(
    snapshot: &WorkflowDefinitionSnapshot,
    catalog: Option<&ValidatedPluginCatalog>,
    inputs: &BTreeMap<String, KernelValue>,
) -> Result<WorkflowExplanation> {
    let flow = snapshot.top_level_flow()?;
    crate::validate_workflow_snapshot(snapshot)?;
    let recipe = crate::compile::compile_flow_internal_mode(
        &flow,
        None,
        crate::compile::inputs::serialize_yaml_value_default,
        Some(snapshot),
        true,
    )?;
    let mut explanation = WorkflowExplanation {
        schema: "transist.workflow_explanation.v1".into(),
        notice: EXPLANATION_NOTICE.into(),
        workflow: recipe.workflow.clone(),
        definition_sha256: snapshot.digest.clone(),
        assets: snapshot
            .assets
            .iter()
            .map(|asset| {
                (
                    asset.relative_path.clone(),
                    ExplanationAsset {
                        sha256: asset.sha256.clone(),
                        normalized_sha256: asset.normalized_sha256.clone(),
                    },
                )
            })
            .collect(),
        pins: BTreeMap::new(),
        documents: BTreeMap::new(),
        scopes: BTreeMap::new(),
        steps: BTreeMap::new(),
        comparison: None,
    };
    for (name, declaration) in &flow.documents {
        let mut value = serde_json::to_value(declaration)?;
        if let Some(initial) = value
            .as_object_mut()
            .and_then(|object| object.remove("initial"))
        {
            value["initial_sha256"] =
                json!(sha256_hex(initial.as_str().unwrap_or_default().as_bytes()));
        }
        value["revision_state"] = json!("not_created");
        explanation.documents.insert(name.clone(), value);
    }
    for (id, version) in &flow.plugins {
        let pin = match catalog.map(|catalog| catalog.load(id, version)) {
            Some(Ok(package)) => ExplanationPin {
                status: "resolved".into(),
                sha256: Some(package.sha256.clone()),
            },
            Some(Err(error)) => ExplanationPin {
                status: if error.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                }) {
                    "unresolved_missing"
                } else {
                    "unresolved_invalid"
                }
                .into(),
                sha256: None,
            },
            None => ExplanationPin {
                status: "unresolved_missing".into(),
                sha256: None,
            },
        };
        explanation.pins.insert(format!("{id}@{version}"), pin);
    }
    let inputs = inputs
        .iter()
        .map(|(name, value)| (name.clone(), value.clone().to_json()))
        .collect();
    Builder {
        snapshot,
        catalog,
        explanation: &mut explanation,
    }
    .scope(&flow, &recipe, "root", &inputs, &BTreeSet::new())?;
    Ok(explanation)
}

struct Builder<'a> {
    snapshot: &'a WorkflowDefinitionSnapshot,
    catalog: Option<&'a ValidatedPluginCatalog>,
    explanation: &'a mut WorkflowExplanation,
}

impl Builder<'_> {
    fn scope(
        &mut self,
        flow: &Flow,
        recipe: &Recipe,
        path: &str,
        supplied: &BTreeMap<String, Value>,
        symbolic_inputs: &BTreeSet<String>,
    ) -> Result<()> {
        let declared = flow.with_declarations_applied()?;
        let input_states = bindings::inputs(&declared, supplied, symbolic_inputs)?;
        let known = input_states
            .iter()
            .filter(|(_, input)| input.state != "symbolic")
            .filter_map(|(name, input)| {
                input
                    .value
                    .as_ref()
                    .map(|value| (name.clone(), value.clone()))
            })
            .collect();
        let source = self.snapshot.resolve_from_source(
            flow.source_path.as_deref().expect("captured source"),
            flow.source_path
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .expect("captured filename"),
        )?;
        let outputs = serde_json::to_value(&declared.outputs)?;
        self.explanation.scopes.insert(
            path.into(),
            ExplanationScope {
                source,
                inputs: input_states,
                outputs,
                authority_requests: authority(recipe, None, &known)?,
                prerequisites: serde_json::to_value(&recipe.prerequisites)?,
                step_order: recipe
                    .steps
                    .iter()
                    .map(|step| format!("{path}/steps/{}", step.id))
                    .collect(),
                cleanup_order: recipe
                    .cleanup
                    .iter()
                    .map(|step| format!("{path}/cleanup/{}", step.step.id))
                    .collect(),
                graph: recipe.graph.clone(),
            },
        );
        for (authored, compiled) in declared.steps.iter().zip(&recipe.steps) {
            let step_path = format!("{path}/steps/{}", compiled.id);
            let mut explained = self.step(&declared, recipe, authored, compiled, &known)?;
            if let StepExecutionDescriptor::Subflow {
                recipe: child_recipe,
            } = &compiled.execution
            {
                let call = authored.subflow.as_ref().expect("compiled module");
                let relative = self.snapshot.resolve_from_source(
                    declared.source_path.as_deref().expect("source"),
                    &call.path,
                )?;
                let mut child = self.snapshot.flow(&relative)?;
                crate::compile::subflows::inherit_module_scope(&declared, authored, &mut child)?;
                let child_path = format!("{step_path}/module");
                let supplied = explained
                    .bound_bindings
                    .as_object()
                    .expect("compiled bindings")
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let symbolic_inputs = explained
                    .bindings
                    .as_object()
                    .expect("bindings")
                    .iter()
                    .filter_map(|(name, value)| {
                        let mut dependencies = Vec::new();
                        bindings::dependencies(value, "", &mut dependencies);
                        dependencies
                            .iter()
                            .any(|dependency| {
                                dependency
                                    .selector
                                    .strip_prefix("inputs.")
                                    .is_none_or(|input| !known.contains_key(input))
                            })
                            .then(|| name.clone())
                    })
                    .collect();
                self.scope(
                    &child,
                    child_recipe,
                    &child_path,
                    &supplied,
                    &symbolic_inputs,
                )?;
                explained.module = Some(child_path);
            }
            self.explanation.steps.insert(step_path, explained);
        }
        for (authored, compiled) in declared.cleanup.iter().zip(&recipe.cleanup) {
            let mut explained = self.step(
                &declared,
                recipe,
                &authored.as_flow_step(),
                &compiled.step,
                &known,
            )?;
            explained.cleanup_when = compiled
                .when
                .iter()
                .map(|when| {
                    match when {
                        transist_protocol::CleanupTrigger::Success => "success",
                        transist_protocol::CleanupTrigger::Canceled => "canceled",
                        transist_protocol::CleanupTrigger::Failed => "failed",
                    }
                    .into()
                })
                .collect();
            self.explanation
                .steps
                .insert(format!("{path}/cleanup/{}", compiled.step.id), explained);
        }
        Ok(())
    }

    fn step(
        &self,
        flow: &Flow,
        recipe: &Recipe,
        authored: &FlowStep,
        compiled: &RecipeStep,
        inputs: &BTreeMap<String, Value>,
    ) -> Result<ExplanationStep> {
        let bindings = serde_json::to_value(
            compiled
                .with
                .iter()
                .filter(|(key, _)| !key.starts_with("__transist_"))
                .collect::<BTreeMap<_, _>>(),
        )?;
        let mut result = ExplanationStep {
            kind: "control".into(),
            action: None,
            pin: None,
            contract_status: "not_applicable".into(),
            contract: None,
            behavior: "kernel_control".into(),
            bound_bindings: bindings::materialize(&bindings, inputs)
                .with_context(|| format!("step '{}' action bindings", compiled.id))?,
            bindings,
            variable_assignments: serde_json::to_value(&compiled.vars)?,
            dependencies: Vec::new(),
            documents: transist_protocol::step_document_bindings(compiled)
                .map_err(anyhow::Error::msg)?,
            gate: None,
            gate_decision: authored
                .gate_decision
                .as_ref()
                .map(|binding| ExplanationGateDecision {
                    step: binding.step.clone(),
                    decision_state: "not_created".into(),
                    contract_binding: None,
                }),
            branch: authored
                .if_block
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
            loop_limits: authored
                .loop_block
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
            cleanup_when: Vec::new(),
            module: None,
            prompt: None,
            skills: authored.skills.clone().unwrap_or_default(),
            authority_requests: authority(recipe, Some(compiled), inputs)?,
        };
        if recipe
            .graph
            .as_ref()
            .and_then(|graph| graph.loop_routing_version)
            == Some(1)
        {
            if let Some(limits) = result.loop_limits.as_mut().and_then(Value::as_object_mut) {
                limits.insert("exit_on_max_iters".into(), json!(true));
            }
        }
        match &compiled.execution {
            StepExecutionDescriptor::Invoke {
                adapter,
                plugin,
                action,
                plugin_version,
                ..
            } => {
                result.kind = "invoke".into();
                result.behavior = "opaque_external_action".into();
                result.action = Some(format!("{adapter}:{plugin}/{action}"));
                let version = plugin_version.as_deref().expect("exact declared pin");
                let pin = format!("{plugin}@{version}");
                result.contract_status = self.explanation.pins[&pin].status.clone();
                result.pin = Some(pin);
                if result.contract_status == "resolved" {
                    let package = self
                        .catalog
                        .expect("resolved catalog")
                        .load(plugin, version)?;
                    if package.manifest.adapter != *adapter {
                        result.contract_status = "unresolved_adapter".into();
                    } else if let Some(descriptor) = package
                        .manifest
                        .actions
                        .iter()
                        .find(|candidate| candidate.name == *action)
                    {
                        result.contract = Some(ExplanationContract {
                            action: serde_json::to_value(descriptor)?,
                            provider_kind: descriptor
                                .effective_provider_kind(package.manifest.default_provider_kind),
                            package_prerequisites: package.manifest.prerequisites.clone(),
                        });
                        if authored.prompt.is_some() || authored.prompt_file.is_some() {
                            result.prompt = Some(self.prompt(
                                flow,
                                authored,
                                descriptor.prompt_binding()?,
                                inputs,
                            )?);
                        }
                        if let Some(gate_decision) = result.gate_decision.as_mut() {
                            gate_decision.contract_binding = descriptor
                                .gate_decision_binding()?
                                .map(serde_json::to_value)
                                .transpose()?;
                        }
                    } else {
                        result.contract_status = "unresolved_action".into();
                    }
                }
            }
            StepExecutionDescriptor::Control { primitive } => {
                result.action = Some(primitive.clone());
                if primitive == "control.gate.manual" {
                    result.gate = Some(ExplanationGate {
                        gate_id: crate::compile::gates::effective_gate_id(authored)?,
                        review_documents: serde_json::from_value(
                            result
                                .bindings
                                .get("review_documents")
                                .cloned()
                                .unwrap_or(json!([])),
                        )?,
                        decision_state: "not_created".into(),
                        revision_state: "not_created".into(),
                    });
                }
            }
            StepExecutionDescriptor::Subflow { .. } => {
                result.kind = "module".into();
                result.behavior = "compiled_module".into();
            }
        }
        if result.prompt.is_none() && (authored.prompt.is_some() || authored.prompt_file.is_some())
        {
            result.prompt = Some(self.prompt(flow, authored, None, inputs)?);
        }
        // Dependencies are collected from declared templates, not materialized
        // input strings. User data must never introduce new template semantics.
        for (field, value) in [
            ("/bindings", &result.bindings),
            ("/variable_assignments", &result.variable_assignments),
            ("/branch", &serde_json::to_value(&result.branch)?),
            ("/loop_limits", &serde_json::to_value(&result.loop_limits)?),
            (
                "/authority_requests/declared",
                &result.authority_requests["declared"],
            ),
        ] {
            bindings::dependencies(value, field, &mut result.dependencies);
        }
        if let Some(prompt) = &authored.prompt {
            bindings::dependencies(&json!(prompt), "/prompt", &mut result.dependencies);
        }
        // Guards use bare expression selectors as well as templated bindings.
        for (field, expr) in authored
            .if_block
            .iter()
            .map(|block| ("/branch/guard", block.guard.as_str()))
            .chain(authored.loop_block.iter().flat_map(|block| {
                [
                    block
                        .while_expr
                        .as_deref()
                        .map(|expr| ("/loop_limits/while", expr)),
                    block
                        .until
                        .as_deref()
                        .map(|expr| ("/loop_limits/until", expr)),
                ]
                .into_iter()
                .flatten()
            }))
        {
            bindings::expression_dependencies(expr, field, &mut result.dependencies);
        }
        result.dependencies.sort();
        result.dependencies.dedup();
        Ok(result)
    }

    fn prompt(
        &self,
        flow: &Flow,
        step: &FlowStep,
        contract_binding: Option<transist_plugins::ActionPromptBinding>,
        inputs: &BTreeMap<String, Value>,
    ) -> Result<ExplanationPrompt> {
        let contract_binding = contract_binding.map(serde_json::to_value).transpose()?;
        if let Some(prompt) = &step.prompt {
            let mut dependencies = Vec::new();
            bindings::dependencies(&json!(prompt), "", &mut dependencies);
            let symbolic = dependencies.iter().any(|dependency| {
                dependency
                    .selector
                    .strip_prefix("inputs.")
                    .is_none_or(|name| !inputs.contains_key(name))
            });
            let bound = bindings::materialize(&json!(prompt), inputs)?;
            if bound.is_null()
                && !dependencies
                    .iter()
                    .any(|dependency| dependency.selector.starts_with("steps."))
            {
                bail!("prompt cannot resolve to null");
            }
            let bound = bindings::scalar(&bound)?;
            if bound.trim().is_empty() {
                bail!("prompt cannot resolve to an empty string");
            }
            Ok(ExplanationPrompt::Inline {
                behavior: "opaque_prompt".into(),
                binding_state: if symbolic { "symbolic" } else { "birth_known" }.into(),
                sha256: sha256_hex(prompt.as_bytes()),
                bound_sha256: sha256_hex(bound.as_bytes()),
                contract_binding,
            })
        } else {
            let path = self.snapshot.resolve_from_source(
                flow.source_path.as_deref().expect("captured source"),
                step.prompt_file.as_deref().expect("prompt file"),
            )?;
            Ok(ExplanationPrompt::File {
                behavior: "opaque_prompt".into(),
                sha256: self.snapshot.asset(&path)?.sha256.clone(),
                path,
                contract_binding,
            })
        }
    }
}

fn authority(
    recipe: &Recipe,
    step: Option<&RecipeStep>,
    inputs: &BTreeMap<String, Value>,
) -> Result<Value> {
    let raw = json!({
        "status": "requested_not_authorized", "resource_resolution": "declared_not_canonicalized",
        "workspace": recipe.workspace,
        "workflow_filesystem": recipe.filesystem_policy,
        "workflow_network": recipe.network_policy,
        "step_filesystem": step.and_then(|step| step.filesystem_policy.as_ref()),
        "step_network": step.and_then(|step| step.network_policy.as_ref()),
        "secret_refs": step.and_then(|step| step.with.get("secret_refs")).map(|value| value.clone().to_json()).unwrap_or(json!([])),
    });
    // Preserve declaration dependencies separately from the bound exact request.
    Ok(json!({"declared": raw, "bound": bindings::materialize(&raw, inputs)?}))
}

/// Produces readable deterministic text containing every public explanation field.
/// Uses the same typed view as JSON; never runs validation or infers opaque effects.
pub fn render_explanation_text(explanation: &WorkflowExplanation) -> Result<String> {
    let mut text = format!(
        "Workflow: {}@{}\n{}\n",
        explanation.workflow.id, explanation.workflow.version, explanation.notice
    );
    for (scope, details) in &explanation.scopes {
        if details
            .graph
            .as_ref()
            .and_then(|graph| graph.loop_routing_version)
            == Some(1)
        {
            text.push_str(&format!("\nLoop routing ({scope}, version 1): successful wrappers stop entering the body at the activation-local max_iters cap and take the normal successor when present, even if the predicate still requests continuation. Below the cap, guards are evaluated freshly and unknown/invalid predicates authorize neither branch. Cap exhaustion is not approval.\n"));
        }
    }
    // Structured sections preserve exact bindings and limits instead of lossy
    // natural-language summaries that might imply more than the compiler knows.
    let value = serde_json::to_value(explanation)?;
    let object = value.as_object().expect("typed explanation");
    for key in [
        "definition_sha256",
        "pins",
        "documents",
        "scopes",
        "steps",
        "assets",
        "comparison",
    ] {
        if let Some(value) = object.get(key) {
            text.push_str(&format!(
                "\n{key}:\n{}\n",
                serde_json::to_string_pretty(value)?
            ));
        }
    }
    Ok(text)
}
