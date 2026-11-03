// SPDX-License-Identifier: MPL-2.0
use super::{ExplanationDependency, ExplanationInput};
use crate::Flow;
use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use transist_protocol::{binding_pointer_segment, binding_selectors};

pub(super) fn inputs(
    flow: &Flow,
    supplied: &BTreeMap<String, Value>,
    symbolic_inputs: &BTreeSet<String>,
) -> Result<BTreeMap<String, ExplanationInput>> {
    for key in supplied.keys() {
        if !flow.inputs.contains_key(key) {
            bail!("unknown workflow input '{key}'");
        }
    }
    flow.inputs
        .iter()
        .map(|(name, spec)| {
            let (value, state) = if let Some(value) = supplied.get(name) {
                (Some(value.clone()), "provided")
            } else if let Some(default) = &spec.default {
                (Some(serde_json::to_value(default)?), "default")
            } else if !spec.required {
                // The runtime binds omitted optional inputs to actual JSON null.
                (Some(Value::Null), "unset_null")
            } else {
                (None, "missing")
            };
            // Provenance comes from the caller's declared module bindings, never
            // from scanning external input values for template-shaped text.
            let symbolic = symbolic_inputs.contains(name);
            if let Some(value) = &value {
                let compatible = symbolic
                    || value.is_null()
                    || match spec.value_type.as_str() {
                        "string" => value.is_string(),
                        "number" => value.is_number(),
                        "boolean" => value.is_boolean(),
                        "object" => value.is_object(),
                        "array" => value.is_array(),
                        _ => false,
                    };
                if !compatible {
                    bail!("input '{name}' must have type '{}'", spec.value_type);
                }
            }
            Ok((
                name.clone(),
                ExplanationInput {
                    value_type: spec.value_type.clone(),
                    required: spec.required,
                    state: if symbolic { "symbolic" } else { state }.into(),
                    value,
                },
            ))
        })
        .collect()
}

pub(super) fn materialize(value: &Value, inputs: &BTreeMap<String, Value>) -> Result<Value> {
    materialize_in_array(value, inputs, false)
}

// Mirror the execution resolver's legacy input/variable scalar coercion. The
// flag propagates through objects nested beneath arrays, but external values
// returned by a lookup are opaque and must never be traversed or templated.
fn materialize_in_array(
    value: &Value,
    inputs: &BTreeMap<String, Value>,
    array_scalar: bool,
) -> Result<Value> {
    match value {
        Value::String(raw) => {
            let selectors = binding_selectors(raw).map_err(anyhow::Error::msg)?;
            let lookup = |selector: &str| -> Result<Value> {
                if let Some(name) = selector.strip_prefix("inputs.") {
                    return Ok(inputs
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| Value::String(format!("{{{{{selector}}}}}"))));
                }
                if selector.starts_with("steps.") || selector.starts_with("vars.") {
                    return Ok(Value::String(format!("{{{{{selector}}}}}")));
                }
                bail!("unsupported binding selector '{selector}'")
            };
            if selectors.len() == 1 && raw.trim() == format!("{{{{{}}}}}", selectors[0]) {
                let resolved = lookup(selectors[0])?;
                if array_scalar
                    && !selectors[0].starts_with("steps.")
                    && (resolved.is_boolean() || resolved.is_number())
                {
                    return Ok(Value::String(scalar(&resolved)?));
                }
                return Ok(resolved);
            }
            let mut rendered = String::new();
            let mut rest = raw.as_str();
            while let Some(start) = rest.find("{{") {
                rendered.push_str(&rest[..start]);
                let end = rest[start..].find("}}").expect("parsed template") + start + 2;
                let selector = rest[start + 2..end - 2].trim();
                let resolved = lookup(selector)?;
                if resolved.is_null() && !selector.starts_with("steps.") {
                    bail!(
                        "input or variable '{selector}' is null and cannot be interpolated into a larger string"
                    );
                }
                rendered.push_str(&scalar(&resolved)?);
                rest = &rest[end..];
            }
            rendered.push_str(rest);
            Ok(Value::String(rendered))
        }
        Value::Array(values) => values
            .iter()
            .map(|value| materialize_in_array(value, inputs, true))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    materialize_in_array(value, inputs, array_scalar)?,
                ))
            })
            .collect::<Result<_>>()
            .map(Value::Object),
        _ => Ok(value.clone()),
    }
}

pub(super) fn scalar(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value.to_string()),
        _ => bail!("structured output can only be used as a whole-value reference"),
    }
}

pub(super) fn dependencies(value: &Value, path: &str, out: &mut Vec<ExplanationDependency>) {
    match value {
        Value::String(raw) => {
            for selector in binding_selectors(raw).unwrap_or_default() {
                out.push(ExplanationDependency {
                    path: path.into(),
                    selector: selector.into(),
                });
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                dependencies(value, &format!("{path}/{index}"), out);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                dependencies(
                    value,
                    &format!("{path}/{}", binding_pointer_segment(key)),
                    out,
                );
            }
        }
        _ => {}
    }
}

pub(super) fn expression_dependencies(raw: &str, path: &str, out: &mut Vec<ExplanationDependency>) {
    for word in raw.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))) {
        if word.starts_with("inputs.") || word.starts_with("vars.") || word.starts_with("steps.") {
            out.push(ExplanationDependency {
                path: path.into(),
                selector: word.into(),
            });
        }
    }
}
