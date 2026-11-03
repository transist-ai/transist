// SPDX-License-Identifier: MPL-2.0
use crate::flow::FlowInputSpec;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate_inputs(inputs: &BTreeMap<String, FlowInputSpec>) -> Result<()> {
    for (key, spec) in inputs {
        if key.trim().is_empty() {
            bail!("workflow input keys cannot be empty");
        }
        ensure_supported_input_type(&spec.value_type)
            .with_context(|| format!("invalid input type for '{}'", key))?;
        if let Some(default) = spec.default.as_ref() {
            ensure_yaml_type_matches(&spec.value_type, default).with_context(|| {
                format!(
                    "default value for workflow input '{}' does not match declared type",
                    key
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn validate_input_placeholders_in_yaml_map(
    map: &BTreeMap<String, yaml_serde::Value>,
    inputs: &BTreeMap<String, FlowInputSpec>,
) -> Result<()> {
    for value in map.values() {
        validate_input_placeholders_in_yaml_value(value, inputs)?;
    }
    Ok(())
}

pub(crate) fn validate_supported_binding_placeholders_in_yaml_map(
    map: &BTreeMap<String, yaml_serde::Value>,
    inputs: &BTreeMap<String, FlowInputSpec>,
    known_vars: &BTreeSet<String>,
    surface: &str,
) -> Result<()> {
    for value in map.values() {
        validate_supported_binding_placeholders_in_yaml_value(value, inputs, known_vars, surface)?;
    }
    Ok(())
}

pub(crate) fn validate_input_placeholders_in_string(
    value: &str,
    inputs: &BTreeMap<String, FlowInputSpec>,
) -> Result<()> {
    for key in extract_input_placeholders(value) {
        if !inputs.contains_key(&key) {
            bail!("unknown workflow input placeholder '{}'", key);
        }
    }
    Ok(())
}

pub(crate) fn validate_supported_binding_placeholders_in_string(
    value: &str,
    inputs: &BTreeMap<String, FlowInputSpec>,
    known_vars: &BTreeSet<String>,
    surface: &str,
) -> Result<()> {
    if surface.contains("prompt_file")
        && let Some(reference) = extract_step_placeholders(value).into_iter().next()
    {
        bail!(
            "unsupported step placeholder '{{{{steps.{}}}}}' in {}; this surface only supports '{{{{inputs.*}}}}' and '{{{{vars.*}}}}' bindings. Move cross-step data through workflow inputs, workflow vars, or workflow-owned files",
            reference,
            surface
        );
    }
    for key in extract_vars_placeholders(value) {
        if !known_vars.contains(&key) {
            bail!(
                "unknown workflow var placeholder '{{{{vars.{}}}}}' in {}; var is not defined by any prior step",
                key,
                surface
            );
        }
    }
    validate_input_placeholders_in_string(value, inputs)
}

fn validate_input_placeholders_in_yaml_value(
    value: &yaml_serde::Value,
    inputs: &BTreeMap<String, FlowInputSpec>,
) -> Result<()> {
    match value {
        yaml_serde::Value::String(string) => {
            for key in extract_input_placeholders(string) {
                if !inputs.contains_key(&key) {
                    bail!("unknown workflow input placeholder '{}'", key);
                }
            }
        }
        yaml_serde::Value::Sequence(values) => {
            for value in values {
                validate_input_placeholders_in_yaml_value(value, inputs)?;
            }
        }
        yaml_serde::Value::Mapping(values) => {
            for value in values.values() {
                validate_input_placeholders_in_yaml_value(value, inputs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_supported_binding_placeholders_in_yaml_value(
    value: &yaml_serde::Value,
    inputs: &BTreeMap<String, FlowInputSpec>,
    known_vars: &BTreeSet<String>,
    surface: &str,
) -> Result<()> {
    match value {
        yaml_serde::Value::String(string) => {
            validate_supported_binding_placeholders_in_string(string, inputs, known_vars, surface)?;
        }
        yaml_serde::Value::Sequence(values) => {
            for value in values {
                validate_supported_binding_placeholders_in_yaml_value(
                    value, inputs, known_vars, surface,
                )?;
            }
        }
        yaml_serde::Value::Mapping(values) => {
            for value in values.values() {
                validate_supported_binding_placeholders_in_yaml_value(
                    value, inputs, known_vars, surface,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn ensure_supported_input_type(value_type: &str) -> Result<()> {
    match value_type.trim() {
        "string" | "number" | "boolean" | "object" | "array" => Ok(()),
        other => bail!("unsupported input type '{}'", other),
    }
}

fn ensure_yaml_type_matches(value_type: &str, value: &yaml_serde::Value) -> Result<()> {
    match (value_type.trim(), value) {
        ("string", yaml_serde::Value::String(_)) => Ok(()),
        ("number", yaml_serde::Value::Number(_)) => Ok(()),
        ("boolean", yaml_serde::Value::Bool(_)) => Ok(()),
        ("object", yaml_serde::Value::Mapping(_)) => Ok(()),
        ("array", yaml_serde::Value::Sequence(_)) => Ok(()),
        (expected, _) => bail!("expected {}", expected),
    }
}

pub(crate) fn extract_input_placeholders(value: &str) -> Vec<String> {
    extract_placeholders(value, "{{inputs.")
}

pub(crate) fn extract_vars_placeholders(value: &str) -> Vec<String> {
    extract_placeholders(value, "{{vars.")
}

fn extract_step_placeholders(value: &str) -> Vec<String> {
    extract_placeholders(value, "{{steps.")
}

/// Validates that `set` RHS YAML map values only reference known inputs and
/// self-or-prior step outputs. `{{vars.*}}` is rejected in this surface.
///
/// `step_idx` is the 0-based index of the step that owns the `set` block.
/// `step_index` maps step IDs to their 0-based positions in the flow.
pub(crate) fn validate_set_rhs_placeholders_in_yaml_map(
    map: &BTreeMap<String, yaml_serde::Value>,
    inputs: &BTreeMap<String, FlowInputSpec>,
    step_idx: usize,
    step_index: &BTreeMap<String, usize>,
) -> Result<()> {
    for (key, value) in map {
        validate_set_rhs_placeholders_in_yaml_value(value, inputs, step_idx, step_index)
            .with_context(|| format!("invalid set.{} value", key))?;
    }
    Ok(())
}

fn validate_set_rhs_placeholders_in_yaml_value(
    value: &yaml_serde::Value,
    inputs: &BTreeMap<String, FlowInputSpec>,
    step_idx: usize,
    step_index: &BTreeMap<String, usize>,
) -> Result<()> {
    match value {
        yaml_serde::Value::String(s) => {
            validate_set_rhs_placeholders_in_string(s, inputs, step_idx, step_index)?;
        }
        yaml_serde::Value::Sequence(values) => {
            for v in values {
                validate_set_rhs_placeholders_in_yaml_value(v, inputs, step_idx, step_index)?;
            }
        }
        yaml_serde::Value::Mapping(values) => {
            for v in values.values() {
                validate_set_rhs_placeholders_in_yaml_value(v, inputs, step_idx, step_index)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_set_rhs_placeholders_in_string(
    value: &str,
    inputs: &BTreeMap<String, FlowInputSpec>,
    step_idx: usize,
    step_index: &BTreeMap<String, usize>,
) -> Result<()> {
    // {{vars.*}} is not valid on the RHS of set — vars are write-only within the same set block.
    if let Some(var_key) = extract_vars_placeholders(value).into_iter().next() {
        bail!(
            "'{{{{vars.{}}}}}' is not allowed in set RHS values; use '{{{{inputs.*}}}}' or '{{{{steps.<id>.out.*}}}}'",
            var_key
        );
    }
    // Validate {{inputs.*}} references.
    validate_input_placeholders_in_string(value, inputs)?;
    // Validate {{steps.<id>.*}} references: only self or prior steps allowed.
    for placeholder in extract_step_placeholders(value) {
        // placeholder is the portion after "{{steps." up to "}}", e.g. "foo.out.x"
        let selector = format!("steps.{placeholder}");
        let step_id = match placeholder.strip_suffix(".out") {
            Some(id) => id,
            None => {
                transist_protocol::split_output_selector_for_ids(
                    &selector,
                    step_index.keys().map(String::as_str),
                )
                .map_err(anyhow::Error::msg)?
                .0
            }
        };
        match step_index.get(step_id) {
            None => {
                bail!(
                    "'{{{{steps.{}}}}}' references unknown step '{}'",
                    placeholder,
                    step_id
                );
            }
            Some(&ref_idx) if ref_idx > step_idx => {
                bail!(
                    "'{{{{steps.{}}}}}' is a forward reference to step '{}'; set RHS can only reference the current step or earlier steps",
                    placeholder,
                    step_id
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validates that every bare `vars.<key>` reference in a guard / loop
/// expression refers to a key defined by some prior step's `set` block.
///
/// Expression syntax uses bare dot-paths (e.g. `vars.flag == true`), not
/// `{{vars.*}}` braces. This scanner walks the string, identifies tokens of
/// the form `vars.<segment>`, and rejects unknown keys. The boundary check
/// requires the preceding char to be a non-identifier char (or start of
/// string) so identifiers like `myvars.x` are not falsely flagged.
pub(crate) fn validate_vars_references_in_expression(
    expr: &str,
    known_vars: &BTreeSet<String>,
    surface: &str,
) -> Result<()> {
    let bytes = expr.as_bytes();
    let mut i = 0usize;
    while i + 5 <= bytes.len() {
        if &bytes[i..i + 5] == b"vars." {
            // Boundary: previous char must not be part of an identifier.
            let preceded_by_ident = i > 0 && {
                let c = bytes[i - 1];
                c.is_ascii_alphanumeric() || c == b'_' || c == b'.'
            };
            if !preceded_by_ident {
                // Extract the key segment after "vars."
                let mut j = i + 5;
                while j < bytes.len() {
                    let c = bytes[j];
                    if c.is_ascii_alphanumeric() || c == b'_' {
                        j += 1;
                    } else {
                        break;
                    }
                }
                if j > i + 5 {
                    let key = &expr[i + 5..j];
                    if !known_vars.contains(key) {
                        bail!(
                            "unknown workflow var reference 'vars.{}' in {}; var is not defined by any prior step",
                            key,
                            surface
                        );
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    Ok(())
}

fn extract_placeholders(value: &str, prefix: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find(prefix) {
        let after = &rest[start + prefix.len()..];
        let Some(end) = after.find("}}") else {
            break;
        };
        refs.push(after[..end].trim().to_string());
        rest = &after[end + 2..];
    }
    refs
}
