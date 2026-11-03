// SPDX-License-Identifier: MPL-2.0
use crate::flow::{Flow, FlowInputSpec};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use transist_protocol::{KernelValue, RecipeInputSpec};

pub(crate) type YamlJsonSerializer =
    fn(&yaml_serde::Value) -> serde_json::Result<serde_json::Value>;

pub(crate) fn yaml_map_to_kernel_with_serializer(
    map: &BTreeMap<String, yaml_serde::Value>,
    workflow_id: &str,
    step_id: &str,
    source_path: Option<&Path>,
    field: &str,
    serializer: YamlJsonSerializer,
) -> Result<BTreeMap<String, KernelValue>> {
    map.iter()
        .map(|(key, value)| {
            serialize_yaml_value(value, serializer)
                .with_context(|| {
                    format!(
                        "failed serializing {field} input '{key}' for workflow '{workflow_id}' step '{step_id}' source '{}'",
                        source_identity(source_path)
                    )
                })
                .map(|json| (key.clone(), KernelValue::from_json(json)))
        })
        .collect()
}

pub(crate) fn compile_recipe_inputs_with_serializer(
    flow: &Flow,
    serializer: YamlJsonSerializer,
) -> Result<BTreeMap<String, RecipeInputSpec>> {
    flow.inputs
        .iter()
        .map(|(key, spec)| {
            compile_recipe_input(flow, key, spec, serializer).map(|input| (key.clone(), input))
        })
        .collect()
}

pub(crate) fn compile_subflow_inputs_with_serializer(
    flow: &Flow,
    step_id: &str,
    inputs: &BTreeMap<String, yaml_serde::Value>,
    serializer: YamlJsonSerializer,
) -> Result<BTreeMap<String, KernelValue>> {
    yaml_map_to_kernel_with_serializer(
        inputs,
        &flow.id,
        step_id,
        flow.source_path.as_deref(),
        "subflow",
        serializer,
    )
}

fn compile_recipe_input(
    flow: &Flow,
    key: &str,
    spec: &FlowInputSpec,
    serializer: YamlJsonSerializer,
) -> Result<RecipeInputSpec> {
    let default = spec
        .default
        .as_ref()
        .map(|value| {
            serialize_yaml_value(value, serializer)
                .with_context(|| {
                    format!(
                        "failed serializing workflow default input '{key}' for workflow '{}' step '<workflow-default>' source '{}'",
                        flow.id,
                        source_identity(flow.source_path.as_deref())
                    )
                })
                .map(KernelValue::from_json)
        })
        .transpose()?;
    Ok(RecipeInputSpec {
        value_type: spec.value_type.clone(),
        required: spec.required,
        default,
        description: spec.description.clone(),
    })
}

pub(crate) fn serialize_yaml_value(
    value: &yaml_serde::Value,
    serializer: YamlJsonSerializer,
) -> Result<serde_json::Value> {
    serializer(value).map_err(Into::into)
}

pub(crate) fn serialize_yaml_value_default(
    value: &yaml_serde::Value,
) -> serde_json::Result<serde_json::Value> {
    serde_json::to_value(value)
}

fn source_identity(source_path: Option<&Path>) -> String {
    source_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<inline>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::Flow;

    fn failing_serializer(_: &yaml_serde::Value) -> serde_json::Result<serde_json::Value> {
        serde_json::from_str("[")
    }

    fn flow() -> Flow {
        Flow {
            schema: "transist.flow.v1".to_string(),
            id: "workflow-1".to_string(),
            version: "1".to_string(),
            plugins: BTreeMap::new(),
            prerequisites: Default::default(),
            inputs: BTreeMap::new(),
            context: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            documents: Default::default(),
            profiles: BTreeMap::new(),
            workspace: None,
            filesystem_policy: None,
            network_policy: None,
            steps: Vec::new(),
            cleanup: Vec::new(),
            source_path: Some(Path::new("/flows/workflow.yaml").to_path_buf()),
        }
    }

    #[test]
    fn step_with_serialization_failure_is_contextual() {
        let mut values = BTreeMap::new();
        values.insert(
            "payload".to_string(),
            yaml_serde::Value::String("value".to_string()),
        );

        let error = yaml_map_to_kernel_with_serializer(
            &values,
            "workflow-1",
            "step-1",
            Some(Path::new("/flows/workflow.yaml")),
            "step with",
            failing_serializer,
        )
        .expect_err("injected serializer failure");

        let message = error.to_string();
        assert!(message.contains("step with input 'payload'"), "{message}");
        assert!(message.contains("workflow 'workflow-1'"), "{message}");
        assert!(message.contains("step 'step-1'"), "{message}");
        assert!(message.contains("/flows/workflow.yaml"), "{message}");
    }

    #[test]
    fn subflow_input_serialization_failure_is_contextual() {
        let mut values = BTreeMap::new();
        values.insert(
            "child_input".to_string(),
            yaml_serde::Value::String("value".to_string()),
        );

        let flow = flow();
        let error = compile_subflow_inputs_with_serializer(
            &flow,
            "subflow-step",
            &values,
            failing_serializer,
        )
        .expect_err("injected serializer failure");

        let message = error.to_string();
        assert!(message.contains("subflow input 'child_input'"), "{message}");
        assert!(message.contains("workflow 'workflow-1'"), "{message}");
        assert!(message.contains("step 'subflow-step'"), "{message}");
        assert!(message.contains("/flows/workflow.yaml"), "{message}");
    }

    #[test]
    fn workflow_default_serialization_failure_is_contextual() {
        let mut flow = flow();
        flow.inputs.insert(
            "config".to_string(),
            FlowInputSpec {
                value_type: "string".to_string(),
                required: false,
                default: Some(yaml_serde::Value::String("value".to_string())),
                description: None,
            },
        );

        let error = compile_recipe_inputs_with_serializer(&flow, failing_serializer)
            .expect_err("injected serializer failure");

        let message = error.to_string();
        assert!(
            message.contains("workflow default input 'config'"),
            "{message}"
        );
        assert!(message.contains("workflow 'workflow-1'"), "{message}");
        assert!(message.contains("step '<workflow-default>'"), "{message}");
        assert!(message.contains("/flows/workflow.yaml"), "{message}");
    }
}
