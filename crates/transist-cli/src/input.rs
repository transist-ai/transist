// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use transist::KernelValue;

pub(crate) fn merge_workflow_inputs(
    inputs_file: Option<&std::path::Path>,
    cli_inputs: &[String],
) -> Result<BTreeMap<String, KernelValue>> {
    let mut merged = BTreeMap::new();
    if let Some(path) = inputs_file {
        merged.extend(load_inputs_file(path)?);
    }
    merged.extend(parse_cli_inputs(cli_inputs)?);
    Ok(merged)
}

pub(crate) fn load_inputs_file(path: &std::path::Path) -> Result<BTreeMap<String, KernelValue>> {
    let raw = fs::read_to_string(path)?;
    let yaml_value: yaml_serde::Value = yaml_serde::from_str(&raw)?;
    let json = serde_json::to_value(yaml_value)?;
    let object = json
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("inputs file must contain a top-level object"))?;
    Ok(object
        .iter()
        .map(|(key, value)| (key.clone(), KernelValue::from_json(value.clone())))
        .collect())
}

pub(crate) fn parse_cli_inputs(values: &[String]) -> Result<BTreeMap<String, KernelValue>> {
    let mut inputs = BTreeMap::new();
    for raw in values {
        let (key, raw_value) = raw
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --input '{}': expected KEY=VALUE", raw))?;
        if key.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "invalid --input '{}': key cannot be empty",
                raw
            ));
        }
        let parsed = match serde_json::from_str::<serde_json::Value>(raw_value) {
            Ok(value) => KernelValue::from_json(value),
            Err(_) => KernelValue::String(raw_value.to_string()),
        };
        inputs.insert(key.trim().to_string(), parsed);
    }
    Ok(inputs)
}
