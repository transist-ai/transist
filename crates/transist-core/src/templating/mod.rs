// SPDX-License-Identifier: MPL-2.0
use crate::types::KernelRunSnapshot;
use std::collections::BTreeMap;
use transist_protocol::KernelValue;
pub(crate) mod dataflow;

pub(crate) fn resolve_set_kernel_value(
    value: &KernelValue,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    snapshot: &KernelRunSnapshot,
) -> KernelValue {
    match value {
        KernelValue::String(raw) => resolve_set_string(raw, workflow_inputs, snapshot),
        KernelValue::Array(items) => KernelValue::Array(
            items
                .iter()
                .map(|v| resolve_set_kernel_value(v, workflow_inputs, snapshot))
                .collect(),
        ),
        KernelValue::Object(entries) => KernelValue::Object(
            entries
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        resolve_set_kernel_value(v, workflow_inputs, snapshot),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn resolve_set_string(
    raw: &str,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    snapshot: &KernelRunSnapshot,
) -> KernelValue {
    if let Some(key) = exact_set_inputs_placeholder(raw) {
        return workflow_inputs
            .get(&key)
            .cloned()
            .unwrap_or(KernelValue::Null);
    }
    if let Some((step_id, out_path)) = exact_set_steps_placeholder(raw, snapshot) {
        return resolve_step_output(&step_id, &out_path, snapshot);
    }
    if !raw.contains("{{inputs.") && !raw.contains("{{steps.") {
        return KernelValue::String(raw.to_string());
    }

    let mut rendered = String::new();
    let mut rest = raw;
    loop {
        let inputs_pos = rest.find("{{inputs.");
        let steps_pos = rest.find("{{steps.");

        if inputs_pos.is_none() && steps_pos.is_none() {
            break;
        }

        let use_inputs = match (inputs_pos, steps_pos) {
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(i), Some(s)) => i <= s,
            (None, None) => break,
        };

        if use_inputs {
            let i = inputs_pos.unwrap_or_default();
            rendered.push_str(&rest[..i]);
            let after = &rest[i + "{{inputs.".len()..];
            let Some(end_idx) = after.find("}}") else {
                rendered.push_str(rest);
                return KernelValue::String(rendered);
            };
            let key = after[..end_idx].trim();
            let scalar = workflow_inputs
                .get(key)
                .map(kernel_value_to_string)
                .unwrap_or_default();
            rendered.push_str(&scalar);
            rest = &after[end_idx + 2..];
        } else {
            let s = steps_pos.unwrap_or_default();
            rendered.push_str(&rest[..s]);
            let after = &rest[s + "{{steps.".len()..];
            let Some(end_idx) = after.find("}}") else {
                rendered.push_str(rest);
                return KernelValue::String(rendered);
            };
            let path = after[..end_idx].trim();
            if let Some((step_id, out_path)) = split_step_out_path(path, snapshot) {
                let val = resolve_step_output(&step_id, &out_path, snapshot);
                rendered.push_str(&kernel_value_to_string(&val));
            }
            rest = &after[end_idx + 2..];
        }
    }
    rendered.push_str(rest);
    KernelValue::String(rendered)
}

pub(crate) fn exact_set_inputs_placeholder(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("{{inputs.")?.strip_suffix("}}")?;
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    let key = inner.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

fn exact_set_steps_placeholder(
    raw: &str,
    snapshot: &KernelRunSnapshot,
) -> Option<(String, String)> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("{{steps.")?.strip_suffix("}}")?;
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    split_step_out_path(inner.trim(), snapshot)
}

fn split_step_out_path(path: &str, snapshot: &KernelRunSnapshot) -> Option<(String, String)> {
    if let Some(id) = path.strip_suffix(".out")
        && snapshot.steps.contains_key(id)
    {
        return Some((id.into(), "out".into()));
    }
    let selector = format!("steps.{path}");
    let (id, field) = transist_protocol::split_output_selector_for_ids(
        &selector,
        snapshot.steps.keys().map(String::as_str),
    )
    .ok()?;
    Some((id.into(), format!("out.{field}")))
}

pub(crate) fn resolve_step_output(
    step_id: &str,
    out_path: &str,
    snapshot: &KernelRunSnapshot,
) -> KernelValue {
    let Some(step_state) = snapshot.steps.get(step_id) else {
        return KernelValue::Null;
    };
    let Some(result) = step_state.last_result.as_ref() else {
        return KernelValue::Null;
    };
    if out_path == "out" {
        return KernelValue::from(result.output.clone());
    }
    let key_path = &out_path["out.".len()..];
    navigate_json(&result.output, key_path)
        .map(KernelValue::from)
        .unwrap_or(KernelValue::Null)
}

pub(crate) fn navigate_json(value: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current.clone())
}

pub(crate) fn kernel_value_to_string(value: &KernelValue) -> String {
    match value {
        KernelValue::Null => String::new(),
        KernelValue::Bool(b) => b.to_string(),
        KernelValue::Number(n) => n.to_string(),
        KernelValue::String(s) => s.clone(),
        KernelValue::Array(_) | KernelValue::Object(_) => {
            serde_json::to_string(&serde_json::Value::from(value.clone()))
                .expect("KernelValue JSON serialization must be infallible")
        }
    }
}
