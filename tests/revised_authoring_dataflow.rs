// SPDX-License-Identifier: MPL-2.0
//! Contract A through real compilation, preparation, APP probes and dispatch.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use transist::{
    KernelValue, RuntimeOperatingMode, TransistContext, ValidationStatus, init_home_dir_with_mode,
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
    context: TransistContext,
}

impl Fixture {
    fn new(producer_schema: Value) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let workspace = root.join("workspace");
        let home = root.join("home");
        fs::create_dir(&workspace).unwrap();
        init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).unwrap();
        let context = TransistContext::with_home(&workspace, &home);
        let fixture = Self {
            _temp: temp,
            root,
            workspace,
            home,
            context,
        };
        fixture.plugin("data.producer", producer_schema, json!({"type":"object"}));
        fixture.plugin(
            "data.consumer",
            json!({}),
            json!({"type":"object","additionalProperties":false,"properties": {
                "message":{"type":"string","minLength":3}, "prompt":{"type":"string"},
                "typed":{}, "flag":{"type":"boolean"}, "number":{"type":"number"},
                "nothing":{"type":"null"}, "array":{"type":"array"},
                "secret_refs":{"type":"array","items":{"type":"string"}}
            }}),
        );
        fixture
    }

    fn plugin(&self, id: &str, output_schema: Value, input_schema: Value) {
        let dir = self.root.join(id);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("input.json"), input_schema.to_string()).unwrap();
        fs::write(dir.join("output.json"), output_schema.to_string()).unwrap();
        let script = r#"#!/usr/bin/env python3
import json, pathlib, sys
ID = '__ID__'
log = pathlib.Path.cwd() / 'effects.jsonl'
def emit(value):
    print(json.dumps(value), flush=True)
def effect(value):
    with log.open('a') as stream:
        stream.write(json.dumps(value) + '\n')
effect({'plugin': ID, 'phase': 'startup'})
handshake = json.loads(sys.stdin.readline())
emit({'type':'app.handshake.response.v1','protocol_version':'app.v1',
      'plugin':{'id':ID,'version':'0.1.0'},'adapter_kind':'process',
      'capabilities':{'actions':['call'],'supports_cancel':False,'supports_health':False,
                      'supports_artifact_hints':False,'supports_evidence_hints':False}})
line = sys.stdin.readline()
if not line:
    sys.exit(0)
request = json.loads(line)
payload = request.get('input', {})
effect({'plugin': ID, 'phase': 'invoke', 'input': payload})
if ID == 'data.producer':
    output = payload.get('output', {})
    if payload.get('opaque_template'):
        output = {'value': 'literal {{inputs.marker}}'}
    counter = pathlib.Path.cwd() / 'producer-count'
    count = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(count))
    if payload.get('increment'):
        output = {'value': 'value-' + str(count)}
    if payload.get('omit_after_first') and count > 1:
        output = {}
else:
    output = {'seen': payload}
emit({'type':'app.invoke.result.v1','protocol_version':'app.v1',
      'invoke_id':request['invoke_id'],'status':'success','output':output,
      'artifact_hints':[],'evidence_hints':[],'metrics':{},'usage':None,'error':None})
"#
        .replace("__ID__", id);
        fs::write(dir.join("plugin.py"), script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join("plugin.py"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(dir.join("plugin.yaml"), format!("schema: transist.plugin.v1\nid: {id}\nversion: 0.1.0\nadapter: process\ndefault_provider_kind: local\nentrypoint:\n  type: process\n  command: plugin.py\npackage:\n  files: [plugin.py, input.json, output.json]\nactions:\n  - name: call\n    input_schema: input.json\n    output_schema: output.json\n    secret_inputs:\n      - name: DATAFLOW_MISSING_SECRET\n        required: false\n        binding: {{type: env, name: TOKEN}}\n    bindings:\n      transist_prompt_v1:\n        inline_field: prompt\n        file_field: prompt_file\ncapabilities:\n  supports_cancel: false\n  supports_health: false\n  supports_artifact_hints: false\n  supports_evidence_hints: false\n")).unwrap();
        self.context
            .install_plugin_file(&dir.join("plugin.yaml"))
            .unwrap();
    }

    fn write_flow(&self, extra: &str, steps: &str) -> PathBuf {
        let path = self.root.join("workflow.yaml");
        fs::write(&path, format!("schema: transist.flow.v1\nid: dataflow\nversion: 0.1.0\nplugins:\n  data.producer: 0.1.0\n  data.consumer: 0.1.0\nworkspace:\n  path: {}\n  access: full\n{extra}steps:\n{steps}", self.workspace.display())).unwrap();
        path
    }

    fn compile(&self, extra: &str, steps: &str) -> transist::Recipe {
        let path = self.write_flow(extra, steps);
        let recipe = self.context.compile_flow_file(&path).unwrap();
        yaml_serde::from_str(&fs::read_to_string(recipe).unwrap()).unwrap()
    }

    fn prepare(&self) {
        let report = self
            .context
            .prepare_workflow("dataflow@0.1.0", Some("data-run"), BTreeMap::new())
            .unwrap();
        assert_eq!(report.status, "planned", "{report:#?}");
        assert!(
            self.state()["preflight"]["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["status"] == "deferred_to_dispatch")
        );
    }

    fn state(&self) -> Value {
        serde_json::from_str(
            &fs::read_to_string(
                self.context
                    .run_root("data-run")
                    .unwrap()
                    .join("state.json"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn effects(&self, id: &str) -> Vec<Value> {
        fs::read_to_string(self.workspace.join("effects.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|v| v["plugin"] == id)
            .collect()
    }
}

#[test]
fn review_deferred_nested_guard_keeps_secret_and_prerequisite_blockers_before_birth() {
    for nested in [false, true] {
        for secret in [false, true] {
            let f = Fixture::new(json!({}));
            let prerequisites = if secret {
                ""
            } else {
                "prerequisites:\n  paths:\n    - path: absent-required-file\n      kind: file\n      required: true\n"
            };
            let secret_refs = if secret {
                ", secret_refs: [DATAFLOW_MISSING_SECRET]"
            } else {
                ""
            };
            fs::write(f.root.join("blocked.yaml"), format!("schema: transist.module.v1\nid: blocked\nversion: 0.1.0\n{prerequisites}steps:\n  - id: consume\n    uses: process:data.consumer/call\n    with: {{message: hello{secret_refs}}}\n")).unwrap();
            fs::write(f.root.join("guarded.yaml"), "schema: transist.module.v1\nid: guarded\nversion: 0.1.0\ninputs:\n  flag: {type: boolean, required: true}\nmodules:\n  blocked: {path: blocked.yaml}\nsteps:\n  - id: guard\n    control: control.noop\n    if:\n      guard: 'inputs.flag == true'\n      then: [{ref: blocked}]\n  - id: blocked\n    module: blocked\n  - id: finish\n    control: control.finish\n").unwrap();
            if nested {
                fs::write(f.root.join("middle.yaml"), "schema: transist.module.v1\nid: middle\nversion: 0.1.0\ninputs:\n  alias: {type: boolean, required: true}\nmodules:\n  guarded: {path: guarded.yaml}\nsteps:\n  - id: guarded\n    module:\n      ref: guarded\n      inputs: {flag: '{{inputs.alias}}'}\n").unwrap();
            }
            let module = if nested { "middle" } else { "guarded" };
            let input = if nested { "alias" } else { "flag" };
            let recipe = f.compile(&format!("modules:\n  child: {{path: {module}.yaml}}\n"), &format!("  - id: produce\n    uses: process:data.producer/call\n    with: {{output: {{flag: true}}}}\n  - id: child\n    module:\n      ref: child\n      inputs: {{{input}: '{{{{steps.produce.out.flag}}}}'}}\n"));
            let report = f
                .context
                .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
                .unwrap();
            assert!(
                !report.is_validation_passed(),
                "guarded blocker disappeared: {report:#?}"
            );
            assert!(
                report
                    .checks
                    .iter()
                    .any(|check| check.status == ValidationStatus::Failed
                        && if secret {
                            check.code == "secret_resolution"
                        } else {
                            check.code.starts_with("workflow_prerequisite")
                        }),
                "{report:#?}"
            );
            assert!(
                f.context
                    .prepare_workflow("dataflow@0.1.0", Some("denied"), BTreeMap::new())
                    .is_err()
            );
            assert!(
                f.context
                    .start_recipe(&recipe, Some("kernel-denied"), BTreeMap::new())
                    .is_err(),
                "kernel preflight must also inspect symbolic guards before birth"
            );
            assert!(f.context.runs_list().unwrap().is_empty());
            assert!(f.effects("data.producer").is_empty());
            assert!(f.effects("data.consumer").is_empty());
        }
    }
}

#[test]
fn review_reused_middle_and_leaf_defer_comparison_until_current_outputs_exist() {
    for changed in [false, true] {
        let f = Fixture::new(json!({}));
        fs::write(f.root.join("leaf.yaml"), "schema: transist.module.v1\nid: leaf\nversion: 0.1.0\ninputs:\n  payload: {type: string, required: true}\nsteps:\n  - id: consume\n    uses: process:data.consumer/call\n    with: {message: '{{inputs.payload}}'}\n").unwrap();
        let generator = if changed {
            "{increment: true}"
        } else {
            "{output: {value: hello}}"
        };
        fs::write(f.root.join("middle.yaml"), format!("schema: transist.module.v1\nid: middle\nversion: 0.1.0\nmodules:\n  leaf: {{path: leaf.yaml}}\nsteps:\n  - id: produce\n    uses: process:data.producer/call\n    with: {generator}\n  - id: leaf\n    module:\n      ref: leaf\n      inputs: {{payload: '{{{{steps.produce.out.value}}}}'}}\n")).unwrap();
        f.compile("modules:\n  middle: {path: middle.yaml}\n", "  - id: repeat\n    control: control.noop\n    loop:\n      body: [{ref: middle}]\n      until: 'false'\n      max_iters: 3\n  - id: middle\n    module: middle\n");
        f.prepare();
        let report = f
            .context
            .run_started_run("data-run")
            .expect("deferred preflight must not return a bare comparison error");
        assert_eq!(report.status, if changed { "failed" } else { "success" });
        let invokes = f
            .effects("data.consumer")
            .iter()
            .filter(|effect| effect["phase"] == "invoke")
            .count();
        assert_eq!(invokes, if changed { 1 } else { 2 });
        if changed {
            let state = f.state();
            assert_eq!(state["steps"]["middle"]["status"], "failed");
            assert!(
                state["steps"]["middle"]["last_result"]["output"]["failure"]["error_message"]
                    .as_str()
                    .unwrap()
                    .contains("reused child inputs changed")
            );
            let producer_effects = f.effects("data.producer");
            let consumer_effects = f.effects("data.consumer");
            for _ in 0..2 {
                let resumed = f.context.runs_resume("data-run").expect(
                    "known immutable mismatch must settle as a failed report, not a bare error",
                );
                assert_eq!(resumed.status, "failed");
                let state = f.state();
                assert_eq!(state["status"], "failed");
                assert_eq!(state["recovery_state"], "recoverable");
                assert_eq!(state["preflight"]["status"], "failed");
                assert!(
                    state["preflight"]["checks"].as_array().unwrap().iter().any(
                        |check| check["status"] == "failed"
                            && check["kind"] == "subflow_input_binding"
                            && check["step_ids"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|step| step == "middle/leaf")
                    )
                );
                assert_eq!(f.effects("data.producer"), producer_effects);
                assert_eq!(f.effects("data.consumer"), consumer_effects);
            }
        }
    }
}

#[test]
fn review_numeric_union_selectors_preserve_array_and_object_possibilities() {
    for (schema, value) in [
        (
            json!({"type":["array","object"], "properties":{"0":{"type":"boolean"}}, "items":{"type":"string"}}),
            json!(["hello"]),
        ),
        (
            json!({"type":"array", "properties":{"0":{"type":"boolean"}}, "prefixItems":[{"type":"string"}], "items":false}),
            json!(["hello"]),
        ),
        (
            json!({"anyOf":[{"type":"array","items":{"type":"string"}},{"type":"object","properties":{"0":{"type":"boolean"}}}]}),
            json!(["hello"]),
        ),
    ] {
        let f = Fixture::new(json!({"type":"object", "properties":{"value":schema}}));
        let output = serde_json::to_string(&json!({"value":value})).unwrap();
        f.compile("", &format!("  - id: produce\n    uses: process:data.producer/call\n    with: {{output: {output}}}\n  - id: consume\n    uses: process:data.consumer/call\n    with: {{message: '{{{{steps.produce.out.value.0}}}}'}}\n"));
        f.prepare();
        assert_eq!(
            f.context.run_started_run("data-run").unwrap().status,
            "success"
        );
        assert_eq!(
            f.state()["steps"]["consume"]["last_result"]["output"]["seen"]["message"],
            "hello"
        );
    }
}

#[test]
fn review_partial_composites_cannot_hide_known_missing_fields_before_birth() {
    for keyword in ["anyOf", "oneOf", "allOf"] {
        let f = Fixture::new(json!({}));
        let mut schema = json!({"type":"object", "properties":{"message":{"type":"string"}}});
        schema[keyword] = json!([{"required":["fixed_a"]},{"required":["fixed_b"]}]);
        f.plugin("data.composite", json!({}), schema);
        let path = f.write_flow("", &format!("{PRODUCE}  - id: consume\n    uses: process:data.composite/call\n    with: {{message: '{{{{steps.produce.out.value}}}}'}}\n"));
        let raw = fs::read_to_string(&path)
            .unwrap()
            .replace("  data.consumer: 0.1.0", "  data.composite: 0.1.0");
        fs::write(&path, raw).unwrap();
        f.context.compile_flow_file(&path).unwrap();
        let report = f
            .context
            .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
            .unwrap();
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.code == "input_schema"
                    && check.status == ValidationStatus::Failed),
            "{keyword}: {report:#?}"
        );
        assert!(
            f.context
                .prepare_workflow("dataflow@0.1.0", Some("denied"), BTreeMap::new())
                .is_err()
        );
        assert!(f.effects("data.producer").is_empty());
        assert!(f.effects("data.composite").is_empty());
    }
}

#[test]
fn review2_unevaluated_known_extra_field_blocks_birth_without_any_plugin_startup() {
    let f = Fixture::new(json!({}));
    f.plugin(
        "data.closed",
        json!({}),
        json!({"type":"object","properties":{"message":{}},"unevaluatedProperties":false}),
    );
    let path = f.write_flow("", &format!("{PRODUCE}  - id: consume\n    uses: process:data.closed/call\n    with: {{message: '{{{{steps.produce.out.value}}}}', extra: 1}}\n"));
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("  data.consumer: 0.1.0", "  data.closed: 0.1.0"),
    )
    .unwrap();
    let compiled = f.context.compile_flow_file(&path).unwrap();
    let recipe: transist::Recipe =
        yaml_serde::from_str(&fs::read_to_string(compiled).unwrap()).unwrap();
    let report = f
        .context
        .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
        .unwrap();
    let report_schema: Value = serde_json::from_str(include_str!(
        "../schemas/runtime/workflow-reports.v1.schema.json"
    ))
    .unwrap();
    let report_validator = jsonschema::JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(&report_schema)
        .unwrap();
    assert!(report_validator.is_valid(&serde_json::to_value(&report).unwrap()));
    assert!(!report.is_validation_passed(), "{report:#?}");
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "input_schema" && check.status == ValidationStatus::Failed)
    );
    assert!(
        f.context
            .prepare_workflow("dataflow@0.1.0", Some("denied"), BTreeMap::new())
            .is_err()
    );
    assert!(
        f.context
            .start_recipe(&recipe, Some("kernel-denied"), BTreeMap::new())
            .is_err()
    );
    assert!(f.context.runs_list().unwrap().is_empty());
    assert!(f.effects("data.producer").is_empty());
    assert!(f.effects("data.closed").is_empty());
}

#[test]
fn review2_numeric_const_and_enum_allow_valid_current_materialization() {
    for keyword in ["const", "enum"] {
        let f = Fixture::new(json!({}));
        let literal = json!({"n":1.0,"message":"hello"});
        let mut schema = json!({"type":"object"});
        schema[keyword] = if keyword == "const" {
            literal
        } else {
            json!([literal])
        };
        f.plugin("data.numeric", json!({}), schema);
        let path = f.write_flow("", &format!("{PRODUCE}  - id: consume\n    uses: process:data.numeric/call\n    with: {{n: 1, message: '{{{{steps.produce.out.value}}}}'}}\n"));
        fs::write(
            &path,
            fs::read_to_string(&path)
                .unwrap()
                .replace("  data.consumer: 0.1.0", "  data.numeric: 0.1.0"),
        )
        .unwrap();
        f.context.compile_flow_file(&path).unwrap();
        f.prepare();
        assert_eq!(
            f.context.run_started_run("data-run").unwrap().status,
            "success"
        );
        let seen = &f.state()["steps"]["consume"]["last_result"]["output"]["seen"];
        assert_eq!(seen["n"], 1);
        assert_eq!(seen["message"], "hello");
    }
}

const PRODUCE: &str =
    "  - id: produce\n    uses: process:data.producer/call\n    with: {output: {value: hello}}\n";
const CONSUME: &str = "  - id: consume\n    uses: process:data.consumer/call\n    with: {message: '{{steps.produce.out.value}}'}\n";

#[test]
fn producer_consumer_validates_prepares_and_dispatches_typed_data() {
    let f = Fixture::new(json!({}));
    let recipe = f.compile("", &format!("{PRODUCE}{CONSUME}"));
    assert_eq!(
        serde_json::to_value(&recipe.steps[1].with).unwrap()["__transist_deferred_bindings"][0]["selector"],
        "steps.produce.out.value"
    );
    let (report, summary) = f
        .context
        .workflow_validation_report_with_summary("dataflow@0.1.0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(report.is_validation_passed(), "{report:#?}");
    assert!(summary.is_validation_passed());
    let schema: Value = serde_json::from_str(include_str!(
        "../schemas/runtime/workflow-reports.v1.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(&schema)
        .unwrap();
    assert!(validator.is_valid(&serde_json::to_value(&report).unwrap()));
    assert!(validator.is_valid(&serde_json::to_value(&summary).unwrap()));
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.status == ValidationStatus::DeferredToDispatch)
    );
    assert!(f.effects("data.producer").is_empty());
    f.prepare();
    assert!(
        f.effects("data.consumer").is_empty(),
        "consumer must not probe before data exists"
    );
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "success"
    );
    assert_eq!(
        f.state()["steps"]["consume"]["last_result"]["output"]["seen"]["message"],
        "hello"
    );
}

#[test]
fn materialized_schema_failure_prevents_consumer_startup_and_first_effect() {
    let f = Fixture::new(json!({"type":"object","properties":{"value":{"type":"string"}}}));
    f.compile("", &format!("{}{CONSUME}", PRODUCE.replace("hello", "hi")));
    f.prepare();
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "failed",
        "state={} effects={:?}",
        f.state(),
        f.effects("data.producer")
    );
    assert!(f.effects("data.consumer").is_empty());
    assert_eq!(
        f.state()["steps"]["consume"]["last_result"]["error"]["code"],
        "validation_error"
    );
}

#[test]
fn structured_arrays_objects_null_and_dotted_ids_preserve_json_types() {
    let f = Fixture::new(json!({}));
    f.compile("", r#"  - id: produce.data
    uses: process:data.producer/call
    with:
      output: {flag: true, number: 7, nothing: null, nested: {items: [false, 3, null, {x: [1, 2]}]}}
  - id: consume
    uses: process:data.consumer/call
    prompt: 'Flag {{steps.produce.data.out.flag}}'
    with:
      flag: '{{steps.produce.data.out.flag}}'
      number: '{{steps.produce.data.out.number}}'
      nothing: '{{steps.produce.data.out.nothing}}'
      array: ['{{steps.produce.data.out.flag}}', '{{steps.produce.data.out.number}}', '{{steps.produce.data.out.nothing}}']
      typed: {nested: '{{steps.produce.data.out.nested}}', indexed: '{{steps.produce.data.out.nested.items.3.x}}'}
"#);
    f.prepare();
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "success"
    );
    let state = f.state();
    let seen = &state["steps"]["consume"]["last_result"]["output"]["seen"];
    assert_eq!(seen["array"], json!([true, 7, null]));
    assert_eq!(seen["typed"]["indexed"], json!([1, 2]));
    assert_eq!(
        seen["typed"]["nested"]["items"],
        json!([false,3,null,{"x":[1,2]}])
    );
    assert_eq!(seen["prompt"], "Flag true");
}

#[test]
fn dotted_ids_with_out_segments_and_unicode_fields_are_resolved_without_guessing() {
    let f = Fixture::new(json!({}));
    f.compile("", "  - id: produce.out.data\n    uses: process:data.producer/call\n    with: {output: {flag: true, résumé: hello}}\n    set: {copied: '{{steps.produce.out.data.out.flag}}'}\n  - id: consume\n    uses: process:data.consumer/call\n    with: {flag: '{{steps.produce.out.data.out.flag}}', message: '{{steps.produce.out.data.out.résumé}}', typed: '{{vars.copied}}'}\n");
    f.prepare();
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "success"
    );
    let seen = &f.state()["steps"]["consume"]["last_result"]["output"]["seen"];
    assert_eq!(seen["flag"], true);
    assert_eq!(seen["typed"], true);
    assert_eq!(seen["message"], "hello");
    let f = Fixture::new(json!({}));
    let path = f.write_flow("", &format!("{PRODUCE}  - id: produce.out.data\n    uses: process:data.producer/call\n    with: {{output: {{flag: true}}}}\n  - id: consume\n    uses: process:data.consumer/call\n    with: {{flag: '{{{{steps.produce.out.data.out.flag}}}}'}}\n"));
    assert!(
        f.context
            .compile_flow_file(&path)
            .unwrap_err()
            .to_string()
            .contains("ambiguous output selector")
    );
}

#[test]
fn cleanup_deferred_validation_prevents_the_consumers_first_effect() {
    let f = Fixture::new(json!({}));
    f.compile("cleanup:\n  - id: consume_cleanup\n    when: [success]\n    uses: process:data.consumer/call\n    with: {message: '{{steps.produce.out.value}}'}\n", &PRODUCE.replace("hello", "hi"));
    let report = f
        .context
        .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.status == ValidationStatus::DeferredToDispatch
                && check.step_path.as_deref() == Some("consume_cleanup"))
    );
    assert_eq!(
        f.context
            .prepare_workflow("dataflow@0.1.0", Some("data-run"), BTreeMap::new())
            .unwrap()
            .status,
        "planned"
    );
    f.context.run_started_run("data-run").unwrap();
    assert_eq!(f.state()["cleanup"]["state"], "failed");
    assert_eq!(
        f.state()["cleanup"]["steps"]["consume_cleanup"]["last_result"]["error"]["code"],
        "validation_error"
    );
    assert!(f.effects("data.consumer").is_empty());
}

#[test]
fn nested_modules_bind_deferred_inputs_and_export_only_named_values() {
    let f = Fixture::new(json!({}));
    fs::write(f.root.join("leaf.yaml"), "schema: transist.module.v1\nid: leaf\nversion: 0.1.0\ninputs:\n  payload: {type: object, required: true}\noutputs:\n  result: '{{steps.echo.out.value}}'\nsteps:\n  - id: echo\n    uses: process:data.producer/call\n    with: {output: '{{inputs.payload}}'}\n").unwrap();
    fs::write(f.root.join("middle.yaml"), "schema: transist.module.v1\nid: middle\nversion: 0.1.0\ninputs:\n  payload: {type: object, required: true}\nmodules:\n  leaf: {path: leaf.yaml}\noutputs:\n  exported: '{{steps.inner.out.result}}'\nsteps:\n  - id: inner\n    module:\n      ref: leaf\n      inputs: {payload: '{{inputs.payload}}'}\n").unwrap();
    f.compile(
        "modules:\n  middle: {path: middle.yaml}\n",
        r#"  - id: produce
    uses: process:data.producer/call
    with: {output: {payload: {value: [true, null, {number: 4}]}}}
  - id: module.call
    module:
      ref: middle
      inputs: {payload: '{{steps.produce.out.payload}}'}
  - id: consume
    uses: process:data.consumer/call
    with: {typed: '{{steps.module.call.out.exported}}'}
"#,
    );
    f.prepare();
    let persisted = f.context.persisted_recipe("data-run").unwrap();
    assert!(
        !persisted.capabilities["module.call/inner/echo"].is_empty(),
        "all nested authority must be persisted at root birth"
    );
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "success"
    );
    let state = f.state();
    assert_eq!(
        state["steps"]["module.call"]["last_result"]["output"]["exported"],
        json!([true,null,{"number":4}])
    );
    assert_eq!(
        state["steps"]["consume"]["last_result"]["output"]["seen"]["typed"],
        json!([true,null,{"number":4}])
    );
}

#[test]
fn missing_field_and_skipped_producer_fail_durably_without_consumer_effects() {
    for skipped in [false, true] {
        let f = Fixture::new(json!({}));
        let steps = if skipped {
            format!(
                "  - id: branch\n    control: control.noop\n    if:\n      guard: 'false'\n      then: [{{ref: produce}}]\n{PRODUCE}{CONSUME}"
            )
        } else {
            format!(
                "{}{CONSUME}",
                PRODUCE.replace("value: hello", "other: hello")
            )
        };
        f.compile("", &steps);
        f.prepare();
        assert_eq!(
            f.context.run_started_run("data-run").unwrap().status,
            "failed"
        );
        assert!(f.effects("data.consumer").is_empty());
        assert_eq!(f.state()["steps"]["consume"]["status"], "failed");
    }
}

#[test]
fn loop_missing_current_output_never_uses_previous_iteration() {
    let f = Fixture::new(json!({}));
    f.compile("", &format!("  - id: repeat\n    control: control.noop\n    loop:\n      body: [{{ref: produce}}, {{ref: consume}}]\n      until: 'false'\n      max_iters: 3\n{}{CONSUME}", PRODUCE.replace("with: {output: {value: hello}}", "with: {output: {value: hello}, omit_after_first: true}")));
    f.prepare();
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "failed",
        "state={} effects={:?}",
        f.state(),
        f.effects("data.producer")
    );
    assert_eq!(
        f.effects("data.consumer")
            .iter()
            .filter(|e| e["phase"] == "invoke")
            .count(),
        1
    );
    assert_eq!(f.state()["steps"]["consume"]["executions"], 2);
}

#[test]
fn missing_grants_and_secrets_still_block_deferred_and_nested_consumers() {
    for secret in [false, true] {
        let f = Fixture::new(json!({}));
        f.compile(
            "",
            &format!(
                "{PRODUCE}{}",
                if secret {
                    CONSUME.replace(
                        "with: {message:",
                        "with: {secret_refs: [DATAFLOW_MISSING_SECRET], message:",
                    )
                } else {
                    CONSUME.into()
                }
            ),
        );
        if !secret {
            fs::write(
                f.home.join("config.yaml"),
                "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n",
            )
            .unwrap();
        }
        let report = f
            .context
            .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
            .unwrap();
        assert!(!report.is_validation_passed());
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.status == ValidationStatus::Failed
                    && c.step_path.as_deref() == Some("consume")),
            "{report:#?}"
        );
        assert!(
            f.context
                .prepare_workflow("dataflow@0.1.0", Some("blocked"), BTreeMap::new())
                .is_err()
        );
        assert!(f.effects("data.consumer").is_empty());
    }
}

#[test]
fn unknown_malformed_structured_interpolation_and_dynamic_authority_are_rejected() {
    let f = Fixture::new(json!({"type":"object","properties":{"value":{"type":"object"}}}));
    for binding in [
        "{{steps.missing.out.value}}",
        "{{steps.produce.output.value}}",
        "{{steps.produce.out.}}",
        "{{steps.produce.out.value",
        "prefix {{steps.produce.out.value}}",
    ] {
        let path = f.write_flow(
            "",
            &format!(
                "{PRODUCE}{}",
                CONSUME.replace("{{steps.produce.out.value}}", binding)
            ),
        );
        assert!(
            f.context.compile_flow_file(&path).is_err(),
            "accepted {binding}"
        );
    }
    for step in [
        "    with: {secret_refs: ['{{steps.produce.out.value}}']}\n",
        "    prompt_file: '{{steps.produce.out.value}}'\n",
        "    filesystem_policy: {read_allow_paths: ['{{steps.produce.out.value}}']}\n",
        "    with: {typed: 'source://{{steps.produce.out.value}}'}\n",
    ] {
        let path = f.write_flow(
            "",
            &format!("{PRODUCE}  - id: consume\n    uses: process:data.consumer/call\n{step}"),
        );
        assert!(
            f.context.compile_flow_file(&path).is_err(),
            "accepted {step}"
        );
    }
}

#[test]
fn statically_proven_schema_incompatibility_is_rejected_but_unions_are_conservative() {
    let f = Fixture::new(json!({"type":"object","properties":{"value":{"type":"boolean"}}}));
    let path = f.write_flow("", &format!("{PRODUCE}{CONSUME}"));
    let error = f.context.compile_flow_file(&path).unwrap_err();
    assert!(
        error.to_string().contains("statically incompatible"),
        "{error:#}"
    );
    let f = Fixture::new(
        json!({"type":"object","properties":{"value":{"anyOf":[{"type":"string"},{"type":"null"}]}}}),
    );
    f.compile("", &format!("{PRODUCE}{CONSUME}"));
}

#[test]
fn whole_inline_prompt_rejects_proven_structured_output_and_unknown_shape_fails_before_probe() {
    for known in [false, true] {
        let schema = if known {
            json!({"type":"object","properties":{"value":{"type":"object"}}})
        } else {
            json!({})
        };
        let f = Fixture::new(schema);
        let steps = format!(
            "{}  - id: consume\n    uses: process:data.consumer/call\n    prompt: '{{{{steps.produce.out.value}}}}'\n",
            PRODUCE.replace("value: hello", "value: {flag: true}")
        );
        if known {
            let path = f.write_flow("", &steps);
            assert!(
                f.context
                    .compile_flow_file(&path)
                    .unwrap_err()
                    .to_string()
                    .contains("structured output")
            );
        } else {
            f.compile("", &steps);
            f.prepare();
            assert_eq!(
                f.context.run_started_run("data-run").unwrap().status,
                "failed"
            );
            assert!(f.effects("data.consumer").is_empty());
        }
    }
}

#[test]
fn module_exports_validate_names_scope_and_conservative_input_alias_types() {
    for outputs in [
        "  child_run_id: 1\n",
        "  result: '{{steps.absent.out.value}}'\n",
        "  result: '{{inputs.absent}}'\n",
    ] {
        let f = Fixture::new(json!({}));
        fs::write(f.root.join("child.yaml"), format!("schema: transist.module.v1\nid: child\nversion: 0.1.0\noutputs:\n{outputs}steps:\n{PRODUCE}")).unwrap();
        let path = f.write_flow("modules:\n  child: {path: child.yaml}\n", "  - id: child\n    module: child\n  - id: consume\n    uses: process:data.consumer/call\n    with: {message: hello}\n");
        assert!(f.context.compile_flow_file(&path).is_err());
    }
    let f = Fixture::new(json!({}));
    fs::write(f.root.join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: 0.1.0\ninputs:\n  payload: {type: object, required: true}\noutputs:\n  result: '{{inputs.payload}}'\nsteps:\n  - id: finish\n    control: control.finish\n").unwrap();
    f.compile("modules:\n  child: {path: child.yaml}\n", &format!("{PRODUCE}  - id: child\n    module:\n      ref: child\n      inputs: {{payload: {{message: hello}}}}\n  - id: consume\n    uses: process:data.consumer/call\n    with: {{message: '{{{{steps.child.out.result.message}}}}'}}\n"));
    f.prepare();
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "success"
    );
}

#[test]
fn outputs_cannot_become_nested_module_authority_through_input_aliases() {
    let f = Fixture::new(json!({}));
    fs::write(f.root.join("leaf.yaml"), "schema: transist.module.v1\nid: leaf\nversion: 0.1.0\ninputs:\n  alias: {type: string, required: true}\nsteps:\n  - id: consume\n    uses: process:data.consumer/call\n    with: {message: hello, secret_refs: ['{{inputs.alias}}']}\n").unwrap();
    fs::write(f.root.join("middle.yaml"), "schema: transist.module.v1\nid: middle\nversion: 0.1.0\ninputs:\n  payload: {type: string, required: true}\nmodules:\n  leaf: {path: leaf.yaml}\nsteps:\n  - id: inner\n    module:\n      ref: leaf\n      inputs: {alias: '{{inputs.payload}}'}\n").unwrap();
    f.compile("modules:\n  middle: {path: middle.yaml}\n", &format!("{PRODUCE}  - id: nested\n    module:\n      ref: middle\n      inputs: {{payload: '{{{{steps.produce.out.value}}}}'}}\n"));
    let report = f
        .context
        .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(!report.is_validation_passed(), "{report:#?}");
    assert!(
        f.context
            .prepare_workflow("dataflow@0.1.0", Some("denied"), BTreeMap::new())
            .is_err()
    );
    assert!(f.context.runs_list().unwrap().is_empty());
}

#[test]
fn set_vars_remain_available_and_external_template_text_is_opaque() {
    let f = Fixture::new(json!({}));
    f.compile(
        "inputs:\n  marker: {type: string, default: rewritten}\n",
        r#"  - id: produce
    uses: process:data.producer/call
    with: {opaque_template: true}
    set: {copied: '{{steps.produce.out.value}}'}
  - id: consume
    uses: process:data.consumer/call
    with: {message: '{{steps.produce.out.value}}', typed: '{{vars.copied}}'}
"#,
    );
    f.prepare();
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "success"
    );
    assert_eq!(
        f.state()["steps"]["consume"]["last_result"]["output"]["seen"]["message"],
        "literal {{inputs.marker}}"
    );
    assert_eq!(
        f.state()["steps"]["consume"]["last_result"]["output"]["seen"]["typed"],
        "literal {{inputs.marker}}"
    );
}

#[test]
fn nullable_vars_keep_existing_semantics_while_direct_skipped_outputs_remain_unavailable() {
    let f = Fixture::new(json!({}));
    f.compile("", &format!("  - id: branch\n    control: control.noop\n    if:\n      guard: 'false'\n      then: [{{ref: produce}}]\n{PRODUCE}    set: {{optional: '{{{{steps.produce.out.value}}}}'}}\n  - id: consume\n    uses: process:data.consumer/call\n    with: {{typed: '{{{{vars.optional}}}}'}}\n"));
    f.prepare();
    assert_eq!(
        f.context.run_started_run("data-run").unwrap().status,
        "success"
    );
    assert_eq!(
        f.state()["steps"]["consume"]["last_result"]["output"]["seen"]["typed"],
        Value::Null
    );
}

#[test]
fn local_schema_refs_and_unions_infer_only_proven_incompatibilities() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("root.json"), r#"{"type":"object","properties":{"flag":{"$ref":"value.json#/$defs/flag"},"maybe":{"$ref":"value.json#/$defs/maybe"},"items":{"type":"array","items":{"$ref":"value.json#/$defs/flag"}}}}"#).unwrap();
    fs::write(temp.path().join("value.json"), r#"{"$defs":{"flag":{"type":"boolean"},"maybe":{"oneOf":[{"type":"string"},{"type":"null"}]}}}"#).unwrap();
    assert_eq!(
        transist_plugins::schema_types_at_path(temp.path(), "root.json", &["flag".into()]).unwrap(),
        ["boolean".into()].into()
    );
    assert_eq!(
        transist_plugins::schema_types_at_path(temp.path(), "root.json", &["maybe".into()])
            .unwrap(),
        ["string".into(), "null".into()].into()
    );
    assert_eq!(
        transist_plugins::schema_types_at_path(
            temp.path(),
            "root.json",
            &["items".into(), "0".into()]
        )
        .unwrap(),
        ["boolean".into()].into()
    );
}

#[test]
fn failed_inflight_and_stale_results_are_never_available() {
    let f = Fixture::new(json!({}));
    f.compile("", &format!("{PRODUCE}{CONSUME}"));
    f.prepare();
    f.context.run_started_run("data-run").unwrap();
    let record: transist_core::PersistedRunRecord = serde_json::from_value(f.state()).unwrap();
    for status in [
        transist_core::KernelStepStatus::Failed,
        transist_core::KernelStepStatus::Skipped,
        transist_core::KernelStepStatus::Running,
        transist_core::KernelStepStatus::Pending,
    ] {
        let mut snapshot = record.snapshot();
        snapshot.steps.get_mut("produce").unwrap().status = status;
        assert!(
            transist_core::resolve_action_value(
                &KernelValue::String("{{steps.produce.out.value}}".into()),
                &BTreeMap::new(),
                &BTreeMap::new(),
                Some(&snapshot),
                false
            )
            .is_err()
        );
    }
    let mut snapshot = record.snapshot();
    snapshot.steps.get_mut("produce").unwrap().executions += 1;
    assert!(
        transist_core::resolve_action_value(
            &KernelValue::String("{{steps.produce.out.value}}".into()),
            &BTreeMap::new(),
            &BTreeMap::new(),
            Some(&snapshot),
            false
        )
        .is_err()
    );
}

#[test]
fn literal_input_and_output_template_text_does_not_create_a_deferred_dependency() {
    let f = Fixture::new(json!({}));
    f.compile("inputs:\n  text: {type: string, required: true}\n", &format!("{PRODUCE}  - id: consume\n    uses: process:data.consumer/call\n    with: {{message: '{{{{inputs.text}}}}'}}\n"));
    let inputs = BTreeMap::from([(
        "text".into(),
        KernelValue::String("literal {{steps.absent.out.value}}".into()),
    )]);
    let report = f
        .context
        .workflow_validation_report("dataflow@0.1.0", inputs.clone(), false, false)
        .unwrap();
    assert!(report.is_validation_passed());
    assert!(
        report
            .checks
            .iter()
            .all(|c| c.status != ValidationStatus::DeferredToDispatch)
    );
    assert_eq!(
        f.context
            .prepare_workflow("dataflow@0.1.0", Some("literal"), inputs)
            .unwrap()
            .status,
        "planned"
    );
    assert_eq!(
        f.context.run_started_run("literal").unwrap().status,
        "success"
    );
    let effect = f
        .effects("data.consumer")
        .into_iter()
        .find(|e| e["phase"] == "invoke")
        .unwrap();
    assert_eq!(
        effect["input"]["message"],
        "literal {{steps.absent.out.value}}"
    );
}

#[test]
fn reused_child_rejects_changed_inputs_and_fresh_children_receive_current_values() {
    for fresh in [false, true] {
        let f = Fixture::new(json!({}));
        fs::write(f.root.join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: 0.1.0\ninputs:\n  payload: {type: string, required: true}\noutputs:\n  value: '{{steps.echo.out.seen.message}}'\nsteps:\n  - id: echo\n    uses: process:data.consumer/call\n    with: {message: '{{inputs.payload}}'}\n").unwrap();
        let mode = if fresh { "fresh" } else { "reuse" };
        f.compile("modules:\n  child: {path: child.yaml}\n", &format!("  - id: repeat\n    control: control.noop\n    loop:\n      body: [{{ref: produce}}, {{ref: child}}]\n      until: 'false'\n      max_iters: 3\n  - id: produce\n    uses: process:data.producer/call\n    with: {{increment: true}}\n  - id: child\n    module:\n      ref: child\n      rerun_mode: {mode}\n      inputs: {{payload: '{{{{steps.produce.out.value}}}}'}}\n"));
        f.prepare();
        let report = f.context.run_started_run("data-run").unwrap();
        assert_eq!(
            report.status,
            if fresh { "success" } else { "failed" },
            "{}",
            f.state()
        );
        assert_eq!(
            f.effects("data.consumer")
                .iter()
                .filter(|e| e["phase"] == "invoke")
                .count(),
            if fresh { 2 } else { 1 }
        );
        if fresh {
            assert_eq!(
                f.state()["steps"]["child"]["last_result"]["output"]["value"],
                "value-2"
            );
        }
    }
}

#[test]
fn known_static_errors_and_nested_secret_requirements_do_not_disappear_when_deferred() {
    let f = Fixture::new(json!({}));
    f.compile(
        "",
        &format!(
            "{PRODUCE}{}",
            CONSUME.replace("with: {message:", "with: {flag: wrong, message:")
        ),
    );
    let report = f
        .context
        .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.code == "input_schema" && c.status == ValidationStatus::Failed)
    );
    let f = Fixture::new(json!({}));
    fs::write(f.root.join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: 0.1.0\ninputs:\n  payload: {type: string, required: true}\nsteps:\n  - id: echo\n    uses: process:data.consumer/call\n    with: {message: '{{inputs.payload}}', secret_refs: [DATAFLOW_MISSING_SECRET]}\n").unwrap();
    f.compile("modules:\n  child: {path: child.yaml}\n", &format!("{PRODUCE}  - id: child\n    module:\n      ref: child\n      inputs: {{payload: '{{{{steps.produce.out.value}}}}'}}\n"));
    let report = f
        .context
        .workflow_validation_report("dataflow@0.1.0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(
        report.checks.iter().any(|c| c.code == "secret_resolution"
            && c.status == ValidationStatus::Failed
            && c.step_path.as_deref() == Some("child/echo")),
        "{report:#?}"
    );
    assert!(
        f.context
            .prepare_workflow("dataflow@0.1.0", Some("denied"), BTreeMap::new())
            .is_err()
    );
    assert!(f.effects("data.consumer").is_empty());
}
