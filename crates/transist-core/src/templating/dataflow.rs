// SPDX-License-Identifier: MPL-2.0
//! Strict action-data resolution. External values are opaque, never templates.

use crate::{KernelRunSnapshot, KernelStepStatus};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use transist_protocol::{
    KernelValue, binding_selectors, split_output_selector, split_output_selector_for_ids,
};

/// Resolves one action value against immutable inputs, variables and current results.
/// Partial mode retains genuine post-birth bindings as authored strings; full mode
/// rejects missing, failed, skipped, stale, or incomplete producer values.
pub fn resolve_action_value(
    value: &KernelValue,
    inputs: &BTreeMap<String, KernelValue>,
    vars: &BTreeMap<String, KernelValue>,
    snapshot: Option<&KernelRunSnapshot>,
    partial: bool,
) -> Result<KernelValue> {
    resolve(value, inputs, vars, snapshot, partial, false)
}

/// Partial action data with explicit unknown locations, separate from JSON values.
#[derive(Debug, Clone)]
pub struct PartialActionValue {
    /// Birth-known data and retained authored bindings.
    pub value: KernelValue,
    /// JSON pointers whose constraints must be rechecked after materialization.
    pub deferred_paths: Vec<String>,
}

/// Resolves birth-known data without treating literal external text as a template.
/// `deferred_inputs` identifies symbolic module inputs supplied by the enclosing
/// partial scope; no fabricated values or JSON marker objects are introduced.
pub fn resolve_partial_action_value(
    value: &KernelValue,
    inputs: &BTreeMap<String, KernelValue>,
    vars: &BTreeMap<String, KernelValue>,
    deferred_inputs: &BTreeSet<String>,
) -> Result<PartialActionValue> {
    let mut deferred_paths = Vec::new();
    collect_unknown_paths(value, vars, deferred_inputs, "", &mut deferred_paths)?;
    Ok(PartialActionValue {
        value: resolve_action_value(value, inputs, vars, None, true)?,
        deferred_paths,
    })
}

fn collect_unknown_paths(
    value: &KernelValue,
    vars: &BTreeMap<String, KernelValue>,
    deferred_inputs: &BTreeSet<String>,
    path: &str,
    paths: &mut Vec<String>,
) -> Result<()> {
    match value {
        KernelValue::String(raw) => {
            if binding_selectors(raw)
                .map_err(anyhow::Error::msg)?
                .iter()
                .any(|selector| {
                    selector.starts_with("steps.")
                        || selector
                            .strip_prefix("vars.")
                            .is_some_and(|name| !vars.contains_key(name))
                        || selector
                            .strip_prefix("inputs.")
                            .is_some_and(|name| deferred_inputs.contains(name))
                })
            {
                paths.push(path.into());
            }
        }
        KernelValue::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_unknown_paths(
                    value,
                    vars,
                    deferred_inputs,
                    &format!("{path}/{index}"),
                    paths,
                )?;
            }
        }
        KernelValue::Object(values) => {
            for (key, value) in values {
                collect_unknown_paths(
                    value,
                    vars,
                    deferred_inputs,
                    &format!("{path}/{}", transist_protocol::binding_pointer_segment(key)),
                    paths,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve(
    value: &KernelValue,
    inputs: &BTreeMap<String, KernelValue>,
    vars: &BTreeMap<String, KernelValue>,
    snapshot: Option<&KernelRunSnapshot>,
    partial: bool,
    array_scalar: bool,
) -> Result<KernelValue> {
    match value {
        KernelValue::String(raw) => {
            let selectors = binding_selectors(raw).map_err(anyhow::Error::msg)?;
            if selectors.is_empty() {
                return Ok(value.clone());
            }
            let lookup = |selector: &str| -> Result<KernelValue> {
                if selector.starts_with("steps.") {
                    split_output_selector(selector).map_err(anyhow::Error::msg)?;
                    if partial {
                        return Ok(KernelValue::String(format!("{{{{{selector}}}}}")));
                    }
                    let snapshot = snapshot.ok_or_else(|| {
                        anyhow::anyhow!("output '{selector}' has no current snapshot")
                    })?;
                    return current_output(selector, snapshot);
                }
                if let Some(name) = selector.strip_prefix("inputs.") {
                    return Ok(inputs.get(name).cloned().unwrap_or(KernelValue::Null));
                }
                if let Some(name) = selector.strip_prefix("vars.") {
                    return match vars.get(name) {
                        Some(value) => Ok(value.clone()),
                        None if partial => Ok(KernelValue::String(format!("{{{{{selector}}}}}"))),
                        None => Ok(KernelValue::Null),
                    };
                }
                bail!("unsupported binding selector '{selector}'")
            };
            if selectors.len() == 1 && raw.trim() == format!("{{{{{}}}}}", selectors[0]) {
                let resolved = lookup(selectors[0])?;
                if array_scalar
                    && !selectors[0].starts_with("steps.")
                    && matches!(resolved, KernelValue::Bool(_) | KernelValue::Number(_))
                {
                    return Ok(KernelValue::String(scalar(&resolved)?));
                }
                return Ok(resolved);
            }
            let mut rendered = String::new();
            let mut rest = raw.as_str();
            while let Some(start) = rest.find("{{") {
                rendered.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                let end = after.find("}}").expect("validated template");
                let selector = after[..end].trim();
                let resolved = lookup(selector)?;
                if resolved == KernelValue::Null && !selector.starts_with("steps.") {
                    bail!(
                        "input or variable '{selector}' is null and cannot be interpolated into a larger string"
                    );
                }
                rendered.push_str(&scalar(&resolved)?);
                rest = &after[end + 2..];
            }
            rendered.push_str(rest);
            Ok(KernelValue::String(rendered))
        }
        KernelValue::Array(values) => values
            .iter()
            .map(|v| resolve(v, inputs, vars, snapshot, partial, true))
            .collect::<Result<Vec<_>>>()
            .map(KernelValue::Array),
        KernelValue::Object(values) => values
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    resolve(value, inputs, vars, snapshot, partial, array_scalar)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()
            .map(KernelValue::Object),
        _ => Ok(value.clone()),
    }
}

fn current_output(selector: &str, snapshot: &KernelRunSnapshot) -> Result<KernelValue> {
    let (id, path) =
        split_output_selector_for_ids(selector, snapshot.steps.keys().map(String::as_str))
            .map_err(anyhow::Error::msg)?;
    let state = &snapshot.steps[id];
    let result = state.last_result.as_ref().filter(|result| {
        state.status == KernelStepStatus::Succeeded && result.status == "success"
            && result.run_id == snapshot.run_id && result.step_id == id
            && result.execution == state.executions && state.executions > state.activation_executions_baseline
    }).ok_or_else(|| anyhow::anyhow!("output '{selector}' is unavailable from the producer's current successful execution"))?;
    let mut selected = &result.output;
    for segment in path.split('.') {
        selected = match selected {
            serde_json::Value::Array(values)
                if segment.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                segment.parse::<usize>().ok().and_then(|i| values.get(i))
            }
            serde_json::Value::Object(values) => values.get(segment),
            _ => None,
        }
        .ok_or_else(|| anyhow::anyhow!("missing output '{selector}' in current execution"))?;
    }
    Ok(KernelValue::from_json(selected.clone()))
}

pub(crate) fn scalar(value: &KernelValue) -> Result<String> {
    match value {
        KernelValue::String(s) => Ok(s.clone()),
        KernelValue::Null => Ok("null".into()),
        KernelValue::Bool(v) => Ok(v.to_string()),
        KernelValue::Number(v) => Ok(v.to_string()),
        _ => bail!("structured output can only be used as a whole-value reference"),
    }
}
