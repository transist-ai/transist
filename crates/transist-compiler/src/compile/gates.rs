// SPDX-License-Identifier: MPL-2.0
use crate::flow::{Flow, FlowStep};
use anyhow::{Result, bail};
use std::path::Path;
use transist_protocol::KernelValue;

/// Freezes a manifest-declared manual-gate decision binding for one invoke step.
pub(crate) fn compile_gate_decision_binding(
    home_dir: &Path,
    flow: &Flow,
    step: &FlowStep,
    step_idx: usize,
) -> Result<Option<KernelValue>> {
    let Some(authored) = step.gate_decision.as_ref() else {
        return Ok(None);
    };
    let binding = crate::validate::resolve_gate_decision_action(home_dir, step, step_idx)?;
    let gate_step = flow
        .steps
        .iter()
        .find(|candidate| candidate.id == authored.step)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "flow.steps[{step_idx}] '{}' gate_decision.step '{}' does not exist in this workflow",
                step.id,
                authored.step
            )
        })?;
    let primitive = gate_step.control.as_deref().unwrap_or_default().trim();
    if primitive != "control.gate.manual" {
        bail!(
            "flow.steps[{step_idx}] '{}' gate_decision.step '{}' must reference a control.gate.manual step",
            step.id,
            authored.step
        );
    }
    if step.with.contains_key(&binding.decision_field) {
        bail!(
            "flow.steps[{step_idx}] '{}' cannot define with.{} because it is injected by transist_gate_decision_v1",
            step.id,
            binding.decision_field
        );
    }

    Ok(Some(KernelValue::Object(std::collections::BTreeMap::from(
        [
            (
                "decision_field".to_string(),
                KernelValue::String(binding.decision_field),
            ),
            (
                "gate_step_id".to_string(),
                KernelValue::String(gate_step.id.clone()),
            ),
            (
                "gate_id".to_string(),
                KernelValue::String(effective_gate_id(gate_step)?),
            ),
        ],
    ))))
}

/// Resolves and validates the immutable gate identity used by a compiled binding.
pub(crate) fn effective_gate_id(step: &FlowStep) -> Result<String> {
    let gate_id = match step.with.get("gate_id") {
        None => step.id.clone(),
        Some(yaml_serde::Value::String(value)) if !value.trim().is_empty() => value.clone(),
        Some(yaml_serde::Value::String(_)) => {
            bail!(
                "gate step '{}' has an empty or whitespace-only gate_id",
                step.id
            )
        }
        Some(_) => bail!(
            "gate step '{}' has a non-string gate_id (must be a string)",
            step.id
        ),
    };
    if gate_id.contains("{{") || gate_id.contains("}}") {
        bail!(
            "gate step '{}' gate_id must be a literal to support gate_decision bindings",
            step.id
        );
    }
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, &gate_id)
        .map_err(|error| anyhow::anyhow!("gate step '{}' has invalid gate id: {error}", step.id))?;
    Ok(gate_id)
}

#[cfg(test)]
mod tests {
    use super::effective_gate_id;
    use crate::flow::FlowStep;
    use std::collections::BTreeMap;

    #[test]
    fn effective_gate_id_defaults_to_the_step_id() {
        let step = FlowStep {
            id: "review".to_string(),
            adapter: None,
            plugin: None,
            action: None,
            execution_kind: None,
            profile: None,
            control: Some("control.gate.manual".to_string()),
            subflow: None,
            prompt: None,
            prompt_file: None,
            skills: None,
            gate_decision: None,
            plugin_version: None,
            with: BTreeMap::new(),
            if_block: None,
            loop_block: None,
            set: None,
            filesystem_policy: None,
            network_policy: None,
        };

        assert_eq!(effective_gate_id(&step).expect("gate id"), "review");
    }
}
