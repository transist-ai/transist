// SPDX-License-Identifier: MPL-2.0
//! Compare explanation with the real execution binders at the root test boundary.
//! The compiler has no production or dev dependency on the runtime kernel.
use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use transist_compiler::explanation::{WorkflowExplanation, explain_workflow};
use transist_core::ports::{
    PluginActionMetadata, PluginMetadataPort, PromptFilePort, PromptFileReference,
};
use transist_core::{
    ExecutionRequestContext, KernelRunSnapshot, KernelStepStatus, bind_deferred_subflow_inputs,
    make_static_execution_request, resolve_action_value,
};
use transist_protocol::{
    InvokePaths, KernelValue, RecipePromptBinding, RecipePromptSource, RecipeStep,
    StepExecutionDescriptor,
};

fn known_inputs() -> BTreeMap<String, KernelValue> {
    serde_json::from_value(json!({"n": 2, "flag": true, "nil": null, "text": "hello", "payload": {"items": [1, true, null]}, "list": [1, true, null], "dotted.value": 17})).unwrap()
}

fn input_specs(inputs: &BTreeMap<String, KernelValue>) -> Value {
    inputs
        .iter()
        .map(|(name, value)| {
            let value = value.clone().to_json();
            let kind = match &value {
                Value::Number(_) => "number",
                Value::Bool(_) => "boolean",
                Value::Object(_) => "object",
                Value::Array(_) => "array",
                _ => "string",
            };
            (name.clone(), json!({"type": kind, "default": value}))
        })
        .collect()
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new(bindings: Value, prompt: Option<&str>, inputs: &BTreeMap<String, KernelValue>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let path = root.join("workflow.yaml");
        let mut call = json!({"id": "call", "uses": "process:fixture/call", "with": bindings});
        if let Some(prompt) = prompt {
            call["prompt"] = json!(prompt);
        }
        let flow = json!({"schema": "transist.flow.v1", "id": "explanation.parity", "version": "0.1.0", "plugins": {"fixture": "0.1.0"}, "inputs": input_specs(inputs),
            "steps": [{"id": "seed", "uses": "process:fixture/call", "set": {"copied": "{{inputs.n}}"}}, call]});
        fs::write(&path, flow.to_string()).unwrap();
        Self {
            _temp: temp,
            root,
            path,
        }
    }
    fn explain(&self) -> Result<WorkflowExplanation> {
        explain_workflow(&self.path, None, &BTreeMap::new())
    }
    fn call(&self) -> RecipeStep {
        transist_compiler::compile_workflow_snapshot(
            &transist_compiler::capture_workflow_definition(&self.path).unwrap(),
        )
        .unwrap()
        .steps
        .remove(1)
    }
}

#[test]
fn birth_known_bindings_match_execution_including_array_coercion_and_dotted_keys() {
    let inputs = known_inputs();
    for bindings in [
        json!({"number": "{{inputs.n}}", "boolean": "{{inputs.flag}}", "null": "{{inputs.nil}}", "object": "{{inputs.payload}}", "array": "{{inputs.list}}", "dotted": "{{inputs.dotted.value}}"}),
        json!({"args": ["{{inputs.n}}", "{{inputs.flag}}", "{{inputs.nil}}", "{{inputs.payload}}", {"nested": "{{inputs.n}}"}, ["{{inputs.flag}}"]]}),
        json!({"number": "{{ inputs.n }}", "boolean": "{{ inputs.flag }}", "outer_space": "  {{inputs.n}}  ", "interpolation": "n={{inputs.n}} flag={{ inputs.flag }}"}),
        json!({"deferred": "{{ steps.seed.out.number }}", "vars": ["{{vars.copied}}", "{{ vars.copied }}"]}),
    ] {
        let f = Fixture::new(bindings.clone(), None, &inputs);
        let expected = resolve_action_value(
            &KernelValue::from_json(bindings),
            &inputs,
            &BTreeMap::new(),
            None,
            true,
        )
        .unwrap()
        .to_json();
        assert_eq!(
            f.explain().unwrap().steps["root/steps/call"].bound_bindings,
            expected
        );
    }
    let f = Fixture::new(
        json!({"args": ["{{inputs.n}}", "{{inputs.flag}}", {"n": "{{inputs.n}}"}]}),
        None,
        &inputs,
    );
    assert_eq!(
        f.explain().unwrap().steps["root/steps/call"].bound_bindings,
        json!({"args": ["2", "true", {"n": "2"}]})
    );
}

#[test]
fn invalid_structured_whitespace_and_null_interpolation_match_execution_errors() {
    let inputs = known_inputs();
    for raw in [
        "{{ inputs.payload }}",
        "{{inputs.payload }}",
        "{{ inputs.list }}",
        "prefix {{inputs.payload}}",
        "{{ inputs.nil }}",
        "prefix {{inputs.nil}}",
    ] {
        let bindings = json!({"value": raw});
        let f = Fixture::new(bindings.clone(), None, &inputs);
        let expected = resolve_action_value(
            &KernelValue::from_json(bindings),
            &inputs,
            &BTreeMap::new(),
            None,
            true,
        )
        .unwrap_err();
        let error = f.explain().unwrap_err();
        assert_eq!(
            error.root_cause().to_string(),
            expected.root_cause().to_string(),
            "{raw}"
        );
    }
}

#[test]
fn nested_module_bindings_match_the_runtime_child_input_binder() {
    let inputs = known_inputs();
    let f = Fixture::new(json!({}), None, &inputs);
    let mut flow: Value = serde_json::from_slice(&fs::read(&f.path).unwrap()).unwrap();
    flow["modules"] = json!({"middle": {"path": "middle.yaml"}});
    flow["steps"] = json!([{"id": "middle", "module": {"ref": "middle", "inputs": {"request": {"payload": "{{inputs.payload}}", "args": ["{{inputs.n}}", "{{inputs.flag}}"], "dotted": "{{inputs.dotted.value}}", "null": "{{inputs.nil}}"}}}}]);
    fs::write(&f.path, flow.to_string()).unwrap();
    fs::write(f.root.join("middle.yaml"), json!({"schema": "transist.module.v1", "id": "middle", "version": "0.1.0", "inputs": {"request": {"type": "object", "required": true}},
        "modules": {"inner": {"path": "inner.yaml"}}, "steps": [{"id": "inner", "module": {"ref": "inner", "inputs": {"request": "{{inputs.request}}"}}}]}).to_string()).unwrap();
    fs::write(f.root.join("inner.yaml"), json!({"schema": "transist.module.v1", "id": "inner", "version": "0.1.0", "inputs": {"request": {"type": "object", "required": true}},
        "steps": [{"id": "call", "uses": "process:fixture/call", "with": {"request": "{{inputs.request}}", "args": ["{{inputs.n}}", "{{inputs.flag}}"]}}]}).to_string()).unwrap();
    let explanation = f.explain().unwrap();
    let recipe = transist_compiler::compile_workflow_snapshot(
        &transist_compiler::capture_workflow_definition(&f.path).unwrap(),
    )
    .unwrap();
    let mut call = &recipe.steps[0];
    let mut bound_inputs = inputs.clone();
    let mut scope = "root".to_string();
    while let StepExecutionDescriptor::Subflow { recipe } = &call.execution {
        bound_inputs =
            bind_deferred_subflow_inputs(recipe, &call.with, &bound_inputs, &BTreeMap::new())
                .unwrap();
        scope = format!("{scope}/steps/{}/module", call.id);
        for (name, value) in &bound_inputs {
            assert_eq!(
                explanation.scopes[&scope].inputs[name].value,
                Some(value.clone().to_json())
            );
        }
        call = &recipe.steps[0];
    }
    let expected = resolve_action_value(
        &KernelValue::Object(call.with.clone()),
        &bound_inputs,
        &BTreeMap::new(),
        None,
        true,
    )
    .unwrap()
    .to_json();
    assert_eq!(
        explanation.steps[&format!("{scope}/steps/call")].bound_bindings,
        expected
    );

    flow["steps"][0]["module"]["inputs"]["request"] = json!("{{ inputs.payload }}");
    fs::write(&f.path, flow.to_string()).unwrap();
    let recipe = transist_compiler::compile_workflow_snapshot(
        &transist_compiler::capture_workflow_definition(&f.path).unwrap(),
    )
    .unwrap();
    let StepExecutionDescriptor::Subflow { recipe: child } = &recipe.steps[0].execution else {
        panic!("module");
    };
    let error =
        bind_deferred_subflow_inputs(child, &recipe.steps[0].with, &inputs, &BTreeMap::new())
            .unwrap_err();
    assert_eq!(
        f.explain().unwrap_err().root_cause().to_string(),
        error.root_cause().to_string()
    );
}

struct Metadata;
impl PluginMetadataPort for Metadata {
    fn resolve_action(
        &self,
        _id: &str,
        _version: &str,
        _action: &str,
    ) -> Result<Option<PluginActionMetadata>> {
        Ok(Some(PluginActionMetadata {
            provider_kind: transist_protocol::ProviderKind::Local,
            secret_inputs: Vec::new(),
        }))
    }
    fn resolve_network_origin(&self, _id: &str, _version: &str) -> Result<Option<String>> {
        Ok(None)
    }
}
struct NoPromptFiles;
impl PromptFilePort for NoPromptFiles {
    fn resolve_prompt_file(
        &self,
        _raw: &str,
        _source: Option<&Path>,
        _step: &str,
    ) -> Result<PromptFileReference> {
        panic!("inline prompt must not read assets");
    }
}

fn runtime_prompt(
    f: &Fixture,
    prompt: &str,
    inputs: &BTreeMap<String, KernelValue>,
) -> Result<String> {
    let mut step = f.call();
    step.prompt_binding = Some(RecipePromptBinding {
        inline_field: "prompt".into(),
        file_field: None,
        source: RecipePromptSource::Inline {
            value: prompt.into(),
        },
    });
    let workspace = f.root.join("workspace");
    let home = f.root.join("home");
    let context = ExecutionRequestContext {
        repo_root: &workspace,
        home_dir: &home,
        recipe_source_dir: None,
        run_id: "parity",
        workflow_id: "explanation.parity",
        workflow_inputs: inputs,
        workflow_vars: &BTreeMap::new(),
        execution: 1,
        invoke_id: "parity-call",
        local_only_mode: false,
    };
    let paths = InvokePaths {
        run_root: "inert-staging".into(),
        artifacts_dir: "inert-artifacts".into(),
        evidence_dir: "inert-evidence".into(),
        workspace_dir: workspace.display().to_string(),
    };
    // The real prompt-injection path builds a request only. These ports cannot
    // probe an adapter, resolve a secret or read a prompt file.
    let prepared =
        make_static_execution_request(context, &step, false, paths, &Metadata, &NoPromptFiles)?
            .unwrap();
    Ok(prepared.request.input["prompt"].as_str().unwrap().into())
}

#[test]
fn inline_prompt_digests_and_failures_match_actual_runtime_injection() {
    let inputs = known_inputs();
    for raw in [
        "{{inputs.n}}",
        "{{inputs.flag}}",
        "{{inputs.text}}",
        "{{ inputs.n }}",
        "{{ inputs.flag }}",
        "  {{inputs.n}}  ",
        "n={{inputs.n}} flag={{ inputs.flag }}",
        "{{inputs.nil}}",
        "{{ inputs.nil }}",
        "nil={{inputs.nil}}",
        "{{inputs.payload}}",
        "{{ inputs.payload }}",
        "{{inputs.list}}",
        "null",
        "{{steps.seed.out.number}}",
        "{{ steps.seed.out.number }}",
    ] {
        let f = Fixture::new(json!({}), Some(raw), &inputs);
        let expected = runtime_prompt(&f, raw, &inputs);
        let explanation = f.explain();
        match expected {
            Ok(prompt) => {
                let view = serde_json::to_value(
                    explanation.unwrap().steps["root/steps/call"]
                        .prompt
                        .as_ref()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    view["bound_sha256"],
                    format!("{:x}", Sha256::digest(prompt.as_bytes())),
                    "{raw}"
                );
                assert_eq!(
                    view["binding_state"],
                    if raw.contains("steps.") {
                        "symbolic"
                    } else {
                        "birth_known"
                    }
                );
            }
            Err(error) => assert_eq!(
                explanation.unwrap_err().root_cause().to_string(),
                error.root_cause().to_string(),
                "{raw}"
            ),
        }
        assert!(!f.root.join("home").exists());
        assert!(!f.root.join("workspace").exists());
    }
    for text in ["", "  ", "{{steps.not_a_dependency.out.value}}"] {
        let mut inputs = inputs.clone();
        inputs.insert("text".into(), KernelValue::String(text.into()));
        let raw = "{{inputs.text}}";
        let f = Fixture::new(json!({}), Some(raw), &inputs);
        match runtime_prompt(&f, raw, &inputs) {
            Ok(prompt) => {
                let view = serde_json::to_value(
                    f.explain().unwrap().steps["root/steps/call"]
                        .prompt
                        .as_ref()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    view["bound_sha256"],
                    format!("{:x}", Sha256::digest(prompt.as_bytes()))
                );
                assert_eq!(
                    view["binding_state"], "birth_known",
                    "external text is opaque"
                );
            }
            Err(error) => assert_eq!(
                f.explain().unwrap_err().root_cause().to_string(),
                error.root_cause().to_string()
            ),
        }
    }
}

#[test]
fn variable_values_stay_symbolic_and_direct_step_output_types_are_not_array_coerced() {
    let inputs = known_inputs();
    let f = Fixture::new(
        json!({"args": ["{{vars.copied}}", "{{steps.seed.out.number}}", "{{steps.seed.out.flag}}"]}),
        None,
        &inputs,
    );
    let explanation = f.explain().unwrap();
    assert_eq!(
        explanation.steps["root/steps/call"].bound_bindings,
        json!({"args": ["{{vars.copied}}", "{{steps.seed.out.number}}", "{{steps.seed.out.flag}}"]})
    );
    let mut snapshot =
        KernelRunSnapshot::new("parity", "explanation.parity", "0.1.0", &["seed".into()]);
    let state = snapshot.steps.get_mut("seed").unwrap();
    state.status = KernelStepStatus::Succeeded;
    state.executions = 1;
    state.last_result = Some(serde_json::from_value(json!({"type": "tep.invoke.result", "protocol_version": "v1", "invoke_id": "seed", "run_id": "parity", "step_id": "seed", "execution": 1, "status": "success", "timing": {"started_at": "", "finished_at": "", "duration_ms": 0}, "output": {"number": 2, "flag": true}})).unwrap());
    let vars = BTreeMap::from([("copied".into(), KernelValue::from_json(json!(2)))]);
    let resolved = resolve_action_value(
        &KernelValue::from_json(explanation.steps["root/steps/call"].bindings.clone()),
        &inputs,
        &vars,
        Some(&snapshot),
        false,
    )
    .unwrap()
    .to_json();
    assert_eq!(resolved, json!({"args": ["2", 2, true]}));
}
