// SPDX-License-Identifier: MPL-2.0
use super::{inputs, prompts, skills};
use crate::WorkflowDefinitionSnapshot;
use crate::flow::{Flow, FlowCleanupWhen};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;
use transist_protocol::{CleanupTrigger, RecipeCleanupStep, RecipeStep, StepExecutionDescriptor};

pub(crate) fn compile_cleanup_steps_with_serializer(
    flow: &Flow,
    home_dir: Option<&Path>,
    serializer: inputs::YamlJsonSerializer,
    snapshot: Option<&WorkflowDefinitionSnapshot>,
) -> Result<Vec<RecipeCleanupStep>> {
    let mut cleanup = Vec::with_capacity(flow.cleanup.len());
    for (step_idx, step) in flow.cleanup.iter().enumerate() {
        let step_like = step.as_flow_step();
        let prompt_binding = match home_dir {
            Some(home_dir) => prompts::compile_prompt_binding(home_dir, &step_like, step_idx)?,
            None => None,
        };
        let skills_binding = match home_dir {
            Some(home_dir) => {
                skills::compile_skills_binding(home_dir, flow, &step_like, step_idx, snapshot)?
            }
            None => None,
        };
        let (resolved_plugin_version, plugin_sha256) = super::resolve_step_plugin_pin(
            home_dir,
            &step.plugin,
            step.plugin_version.as_deref(),
            &step.id,
        )?;
        cleanup.push(RecipeCleanupStep {
            when: step.when.iter().copied().map(map_cleanup_when).collect(),
            step: RecipeStep {
                id: step.id.clone(),
                execution: StepExecutionDescriptor::Invoke {
                    adapter: crate::validate::flow_step_adapter(&step_like)?,
                    plugin_id: Some(step.plugin.clone()),
                    plugin: step.plugin.clone(),
                    action: step.action.clone(),
                    execution_kind: step.execution_kind.clone(),
                    plugin_version: resolved_plugin_version,
                    plugin_sha256,
                },
                with: inputs::yaml_map_to_kernel_with_serializer(
                    &step.with,
                    &flow.id,
                    &step.id,
                    flow.source_path.as_deref(),
                    "cleanup",
                    serializer,
                )?,
                vars: BTreeMap::new(),
                prompt_binding,
                skills_binding,
                filesystem_policy: step.filesystem_policy.clone(),
                network_policy: step.network_policy.clone(),
                effective_filesystem_policy: Default::default(),
                effective_filesystem_policy_source: None,
                effective_network_policy: Default::default(),
                effective_network_policy_source: None,
            },
        });
        let compiled = &mut cleanup.last_mut().expect("compiled cleanup").step;
        super::compile_document_bindings(compiled, &flow.documents)?;
        let deferred =
            transist_protocol::step_deferred_bindings(compiled).map_err(anyhow::Error::msg)?;
        if !deferred.is_empty() {
            compiled.with.insert(
                transist_protocol::DEFERRED_BINDINGS_KEY.into(),
                transist_protocol::KernelValue::from_json(serde_json::to_value(deferred)?),
            );
        }
    }
    Ok(cleanup)
}

fn map_cleanup_when(value: FlowCleanupWhen) -> CleanupTrigger {
    match value {
        FlowCleanupWhen::Success => CleanupTrigger::Success,
        FlowCleanupWhen::Canceled => CleanupTrigger::Canceled,
        FlowCleanupWhen::Failed => CleanupTrigger::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Flow, FlowCleanupStep};

    fn failing_serializer(_: &yaml_serde::Value) -> serde_json::Result<serde_json::Value> {
        serde_json::from_str("[")
    }

    #[test]
    fn cleanup_with_serialization_failure_is_contextual() {
        let mut with = BTreeMap::new();
        with.insert(
            "cleanup_input".to_string(),
            yaml_serde::Value::String("value".to_string()),
        );
        let flow = Flow {
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
            cleanup: vec![FlowCleanupStep {
                id: "cleanup-1".to_string(),
                when: vec![FlowCleanupWhen::Failed],
                adapter: "process".to_string(),
                plugin: "plugin".to_string(),
                action: "cleanup".to_string(),
                execution_kind: "external".to_string(),
                plugin_version: None,
                prompt: None,
                prompt_file: None,
                skills: None,
                with,
                filesystem_policy: None,
                network_policy: None,
            }],
            source_path: Some(Path::new("/flows/workflow.yaml").to_path_buf()),
        };

        let error = compile_cleanup_steps_with_serializer(&flow, None, failing_serializer, None)
            .expect_err("injected serializer failure");

        let message = error.to_string();
        assert!(
            message.contains("cleanup input 'cleanup_input'"),
            "{message}"
        );
        assert!(message.contains("workflow 'workflow-1'"), "{message}");
        assert!(message.contains("step 'cleanup-1'"), "{message}");
        assert!(message.contains("/flows/workflow.yaml"), "{message}");
    }
}
