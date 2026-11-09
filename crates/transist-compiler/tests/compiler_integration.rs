// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use transist_compiler::graph::{WorkflowEdgeKind, flow_graph, recipe_graph};
use transist_compiler::templating::input_refs::{
    collect_input_refs_from, render_input_placeholders_lenient,
};
use transist_compiler::{
    FLOW_SCHEMA_V1, Flow, FlowCleanupStep, FlowCleanupWhen, FlowIfBlock, FlowInputSpec,
    FlowLoopBlock, FlowRef, FlowStep, RECIPE_SCHEMA_V1, capture_workflow_definition, compile_flow,
    compile_flow_in_home, compile_workflow_snapshot_in_home, load_flow, load_flow_unvalidated,
    validate_flow, validate_flow_in_home, write_recipe_in_home,
};
use transist_protocol::{
    AdapterKind, FilesystemPolicy, NetworkAccessMode, NetworkPolicy, PathKind, PathPrerequisite,
    PrerequisiteSet, Recipe, RecipeGraph, RecipeGraphEdge, RecipeLoop, RecipePromptSource,
    RecipeStep, StepExecutionDescriptor, WorkflowRef, WorkspaceAccessMode, WorkspaceAccessPolicy,
};

fn install_plugin_fixture(home_dir: &Path, relative: &str) {
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    transist_plugins::install_plugin_file_in_home(home_dir, &plugin_dir.join("plugin.yaml"))
        .expect("install plugin");
}

fn plugin_fixture_version(relative: &str) -> String {
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    let manifest: yaml_serde::Value = yaml_serde::from_str(
        &std::fs::read_to_string(plugin_dir.join("plugin.yaml")).expect("read plugin manifest"),
    )
    .expect("parse plugin manifest");
    manifest["version"]
        .as_str()
        .expect("plugin version string")
        .to_string()
}

fn install_shell_fixture_with_version(home_dir: &Path, version: &str) {
    let fixture_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("plugins/shell");
    let fixture_dst = home_dir.join("fixtures").join(format!("shell-{version}"));
    std::fs::create_dir_all(&fixture_dst).expect("fixture dst dir");
    std::fs::create_dir_all(fixture_dst.join("schemas")).expect("fixture schema dir");
    for entry in std::fs::read_dir(&fixture_src).expect("read shell fixture") {
        let entry = entry.expect("fixture entry");
        let src_path = entry.path();
        let dst_path = fixture_dst.join(entry.file_name());
        if entry.file_name() == "schemas" {
            std::fs::create_dir_all(&dst_path).expect("create nested dir");
            for nested in std::fs::read_dir(&src_path).expect("read nested dir") {
                let nested = nested.expect("nested entry");
                std::fs::copy(nested.path(), dst_path.join(nested.file_name()))
                    .expect("copy nested file");
            }
        } else if src_path.is_file() {
            std::fs::copy(&src_path, &dst_path).expect("copy fixture file");
        }
    }
    let manifest_path = fixture_dst.join("plugin.yaml");
    let manifest = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let source_version = plugin_fixture_version("plugins/shell");
    let manifest = manifest.replacen(
        &format!("version: {source_version}"),
        &format!("version: {version}"),
        1,
    );
    std::fs::write(&manifest_path, manifest).expect("write manifest");
    transist_plugins::install_plugin_file_in_home(home_dir, &manifest_path)
        .expect("install plugin fixture version");
}

fn flow_step(id: &str, plugin: &str, action: &str) -> FlowStep {
    FlowStep {
        id: id.to_string(),
        adapter: Some("process".to_string()),
        plugin: Some(plugin.to_string()),
        action: Some(action.to_string()),
        execution_kind: Some("external".to_string()),
        profile: None,
        control: None,
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
    }
}

fn write_skill_fixture(base: &Path, name: &str) -> PathBuf {
    let skill_dir = base.join(name);
    std::fs::create_dir_all(skill_dir.join("references")).expect("skill refs dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: Example skill\n---\n\nSee [Reference](references/ref.md).\n"
        ),
    )
    .expect("skill file");
    std::fs::write(
        skill_dir.join("references/ref.md"),
        format!("# {name} reference\n\nUse this carefully.\n"),
    )
    .expect("skill reference");
    skill_dir
}

fn write_broken_skill_fixture(base: &Path, name: &str) -> PathBuf {
    let skill_dir = base.join(name);
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: broken\ndescription: Broken skill\n---\n\nSee [Missing](references/missing.md).\n",
    )
    .expect("skill file");
    skill_dir
}

#[test]
fn flow_graph_builder_derives_if_and_loop_edges() {
    let mut with = BTreeMap::new();
    with.insert(
        "message".to_string(),
        yaml_serde::Value::String("hello".to_string()),
    );
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "graph-demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![
            FlowStep {
                id: "gate".to_string(),
                if_block: Some(FlowIfBlock {
                    guard: "inputs.ok".to_string(),
                    then_branch: vec![FlowRef {
                        ref_step: "then_step".to_string(),
                    }],
                    else_branch: vec![FlowRef {
                        ref_step: "else_step".to_string(),
                    }],
                }),
                with: with.clone(),
                ..flow_step("gate", "shell", "echo")
            },
            FlowStep {
                ..flow_step("then_step", "shell", "echo")
            },
            FlowStep {
                ..flow_step("else_step", "shell", "echo")
            },
            FlowStep {
                id: "loop_step".to_string(),
                loop_block: Some(FlowLoopBlock {
                    while_expr: Some("inputs.keep_going".to_string()),
                    until: None,
                    max_iters: 3,
                    max_duration: None,
                    body: vec![FlowRef {
                        ref_step: "loop_body".to_string(),
                    }],
                }),
                ..flow_step("loop_step", "shell", "echo")
            },
            FlowStep {
                ..flow_step("loop_body", "shell", "echo")
            },
            FlowStep {
                ..flow_step("done", "shell", "echo")
            },
        ],
        cleanup: Vec::new(),
        source_path: None,
    };

    let edges = flow_graph::edges_from(&flow);
    assert!(edges.iter().any(|edge| edge.kind == WorkflowEdgeKind::Then));
    assert!(edges.iter().any(|edge| edge.kind == WorkflowEdgeKind::Else));
    assert!(edges.iter().any(|edge| edge.kind == WorkflowEdgeKind::Loop));
    assert!(edges.iter().any(|edge| edge.kind == WorkflowEdgeKind::Exit));
    assert!(
        edges
            .iter()
            .any(|edge| edge.kind == WorkflowEdgeKind::Repeat)
    );
}

#[test]
fn recipe_graph_builder_classifies_edges() {
    let recipe = Recipe {
        schema: RECIPE_SCHEMA_V1.to_string(),
        workflow: WorkflowRef {
            id: "recipe-graph-demo".to_string(),
            version: "v1".to_string(),
        },
        source_dir: None,
        repositories: Vec::new(),
        prerequisites: PrerequisiteSet::default(),
        filesystem_policy: None,
        network_policy: None,
        inputs: BTreeMap::new(),
        workspace: None,
        steps: vec![
            RecipeStep {
                id: "loop_step".to_string(),
                execution: StepExecutionDescriptor::Control {
                    primitive: "control.noop".to_string(),
                },
                with: BTreeMap::from([(
                    "__transist_loop".to_string(),
                    transist_protocol::KernelValue::Object(BTreeMap::from([
                        (
                            "while".to_string(),
                            transist_protocol::KernelValue::String("inputs.keep_going".to_string()),
                        ),
                        (
                            "max_iters".to_string(),
                            transist_protocol::KernelValue::Number(serde_json::Number::from(2u64)),
                        ),
                    ])),
                )]),
                prompt_binding: None,
                skills_binding: None,
                vars: BTreeMap::new(),
                filesystem_policy: None,
                network_policy: None,
                effective_filesystem_policy: Default::default(),
                effective_filesystem_policy_source: None,
                effective_network_policy: Default::default(),
                effective_network_policy_source: None,
            },
            RecipeStep {
                id: "loop_body".to_string(),
                execution: StepExecutionDescriptor::Control {
                    primitive: "control.noop".to_string(),
                },
                with: BTreeMap::new(),
                prompt_binding: None,
                skills_binding: None,
                vars: BTreeMap::new(),
                filesystem_policy: None,
                network_policy: None,
                effective_filesystem_policy: Default::default(),
                effective_filesystem_policy_source: None,
                effective_network_policy: Default::default(),
                effective_network_policy_source: None,
            },
            RecipeStep {
                id: "done".to_string(),
                execution: StepExecutionDescriptor::Control {
                    primitive: "control.finish".to_string(),
                },
                with: BTreeMap::new(),
                prompt_binding: None,
                skills_binding: None,
                vars: BTreeMap::new(),
                filesystem_policy: None,
                network_policy: None,
                effective_filesystem_policy: Default::default(),
                effective_filesystem_policy_source: None,
                effective_network_policy: Default::default(),
                effective_network_policy_source: None,
            },
        ],
        cleanup: Vec::new(),
        graph: Some(RecipeGraph {
            entry: "loop_step".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "loop_step".to_string(),
                    to: "loop_body".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("inputs.keep_going".to_string()),
                    r#loop: None,
                    loop_route: None,
                },
                RecipeGraphEdge {
                    from: "loop_step".to_string(),
                    to: "done".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("NOT (inputs.keep_going)".to_string()),
                    r#loop: None,
                    loop_route: None,
                },
                RecipeGraphEdge {
                    from: "loop_body".to_string(),
                    to: "loop_step".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: Some(RecipeLoop {
                        while_expr: None,
                        until_expr: None,
                        max_iters: None,
                    }),
                    loop_route: None,
                },
            ],
            max_iterations: Some(10_000),
            loop_routing_version: None,
        }),
        capabilities: Default::default(),
        capabilities_persisted: false,
        documents: Default::default(),
    };

    let edges = recipe_graph::edges_from(&recipe);
    assert!(edges.iter().any(|edge| edge.kind == WorkflowEdgeKind::Loop));
    assert!(edges.iter().any(|edge| edge.kind == WorkflowEdgeKind::Exit));
    assert!(
        edges
            .iter()
            .any(|edge| edge.kind == WorkflowEdgeKind::Repeat)
    );
}

#[test]
fn templating_helpers_collect_and_render_input_refs() {
    let refs = collect_input_refs_from("use {{inputs.alpha}} then inputs.beta now");
    assert!(refs.iter().any(|item| item.key == "alpha"));
    assert!(refs.iter().any(|item| item.key == "beta"));

    let mut inputs = BTreeMap::new();
    inputs.insert(
        "alpha".to_string(),
        transist_protocol::KernelValue::String("A".to_string()),
    );
    let rendered =
        render_input_placeholders_lenient("x={{inputs.alpha}} y={{inputs.missing}}", &inputs);
    assert_eq!(rendered, "x=A y={{inputs.missing}}");
}

#[test]
fn compiler_accepts_flat_invoke_shape() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("recipe");
    assert!(matches!(
        &recipe.steps[0].execution,
        StepExecutionDescriptor::Invoke {
            plugin_id: Some(plugin_id),
            adapter,
            plugin,
            action,
            execution_kind,
            ..
        } if *adapter == AdapterKind::Process
            && plugin_id == "shell"
            && plugin == "shell"
            && action == "echo"
            && execution_kind == "external"
    ));
}

#[test]
fn compiler_rejects_nested_execution_syntax() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("old.flow.yaml");
    std::fs::write(
        &path,
        r#"
schema: transist.flow.v1
id: demo
version: v1
steps:
  - id: step_1
    execution:
      kind: invoke
      adapter: process
      plugin: shell
      action: echo
      execution_kind: external
"#,
    )
    .expect("write");
    load_flow(&path).expect_err("nested execution must fail");
}

#[test]
fn compiler_accepts_typed_workflow_inputs() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::from([(
            "jira_ticket".to_string(),
            FlowInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: Some("Issue key".to_string()),
            },
        )]),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            ..flow_step("step_1", "shell", "exec")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("recipe");
    assert!(recipe.inputs.contains_key("jira_ticket"));
}

#[test]
fn compiler_expands_declarations_and_profile_defaults() {
    let flow: Flow = yaml_serde::from_str(
        r#"
schema: transist.flow.v1
id: declared-bindings
version: v1
plugins:
  shell: 0.1.0
inputs:
  branch:
    type: string
    required: true
context:
  greeting: hello
  target: "{{inputs.branch}}"
artifacts:
  report:
    scope: repository
    path: "reports/{{inputs.branch}}.md"
profiles:
  echo:
    uses: process:shell/echo
    with:
      greeting: "{{context.greeting}}"
steps:
  - id: announce
    uses: process:shell/echo
    profile: echo
    with:
      target: "{{context.target}}"
      report: "{{artifacts.report}}"
"#,
    )
    .expect("parse flow");

    let recipe = compile_flow(&flow).expect("compile declarations");
    assert_eq!(
        recipe.steps[0].with.get("greeting"),
        Some(&transist_protocol::KernelValue::String("hello".to_string()))
    );
    assert_eq!(
        recipe.steps[0].with.get("target"),
        Some(&transist_protocol::KernelValue::String(
            "{{inputs.branch}}".to_string()
        ))
    );
    assert_eq!(
        recipe.steps[0].with.get("report"),
        Some(&transist_protocol::KernelValue::String(
            "reports/{{inputs.branch}}.md".to_string()
        ))
    );
}

#[test]
fn compiler_rejects_mutable_context_declarations() {
    let flow: Flow = yaml_serde::from_str(
        r#"
schema: transist.flow.v1
id: invalid-declarations
version: v1
plugins:
  shell: 0.1.0
context:
  mutable: "{{vars.value}}"
artifacts:
  report:
    scope: run
    path: "{{steps.write.out.path}}"
profiles:
  echo:
    uses: process:shell/echo
    with:
      command: echo profile
steps:
  - id: announce
    uses: process:shell/exec
    profile: echo
    with:
      command: echo step
"#,
    )
    .expect("parse flow");

    let error = validate_flow(&flow).expect_err("reject invalid declarations");
    let rendered = error.to_string();
    assert!(
        rendered.contains("context.mutable only permits literals"),
        "{rendered}"
    );
}

#[test]
fn compiler_rejects_profile_that_changes_step_uses() {
    let flow: Flow = yaml_serde::from_str(
        r#"
schema: transist.flow.v1
id: invalid-profile
version: v1
plugins:
  shell: 0.1.0
profiles:
  echo:
    uses: process:shell/echo
    with:
      command: echo profile
steps:
  - id: announce
    uses: process:shell/exec
    profile: echo
    with:
      command: echo step
"#,
    )
    .expect("parse flow");

    let error = validate_flow(&flow).expect_err("profile cannot change uses");
    assert!(
        error
            .to_string()
            .contains("profiles cannot change a step's uses"),
        "{error}"
    );
}

#[test]
fn compiler_rejects_profile_step_binding_conflicts() {
    let flow: Flow = yaml_serde::from_str(
        r#"
schema: transist.flow.v1
id: profile-conflict
version: v1
plugins:
  shell: 0.1.0
profiles:
  echo:
    uses: process:shell/echo
    with:
      command: echo profile
steps:
  - id: announce
    uses: process:shell/echo
    profile: echo
    with:
      command: echo step
"#,
    )
    .expect("parse flow");

    let error = validate_flow(&flow).expect_err("profile binding conflict");
    assert!(
        error
            .to_string()
            .contains("conflicts with profile 'echo' on with.command"),
        "{error}"
    );
}

#[test]
fn compiler_rejects_mutable_artifact_paths() {
    let flow: Flow = yaml_serde::from_str(
        r#"
schema: transist.flow.v1
id: invalid-artifact
version: v1
artifacts:
  report:
    scope: run
    path: "{{steps.write.out.path}}"
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("parse flow");

    let error = validate_flow(&flow).expect_err("mutable artifact path");
    assert!(
        error
            .to_string()
            .contains("artifacts.report.path only permits literals"),
        "{error}"
    );
}

#[test]
fn compiler_preserves_workflow_prerequisites_in_recipe() {
    let prerequisites = PrerequisiteSet {
        paths: vec![PathPrerequisite {
            path: ".workflow-ready".to_string(),
            kind: PathKind::File,
            required: true,
        }],
        ..PrerequisiteSet::default()
    };
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: prerequisites.clone(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            id: "finish".to_string(),
            adapter: None,
            plugin: None,
            action: None,
            execution_kind: None,
            profile: None,
            control: Some("control.finish".to_string()),
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
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("recipe");
    assert_eq!(recipe.prerequisites, prerequisites);
}

#[test]
fn compiler_requires_home_context_for_first_class_prompt_fields() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            prompt: Some("Say hello".to_string()),
            ..flow_step("step_1", "codex.cli", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(PathBuf::from("/tmp/workflow.yaml")),
    };

    let err = validate_flow(&flow).expect_err("prompt fields require home-aware validation");
    assert!(err.to_string().contains("uses first-class prompt fields"));
}

#[test]
fn compiler_compiles_prompt_aware_steps_in_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            prompt: Some("Say hello".to_string()),
            ..flow_step("step_1", "codex.cli", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile");
    let expected_source_dir = tmp.path().display().to_string();
    assert_eq!(
        recipe.source_dir.as_deref(),
        Some(expected_source_dir.as_str())
    );
    let prompt_binding = recipe.steps[0]
        .prompt_binding
        .as_ref()
        .expect("prompt binding");
    assert_eq!(prompt_binding.inline_field, "prompt");
    assert!(matches!(
        prompt_binding.source,
        RecipePromptSource::Inline { ref value } if value == "Say hello"
    ));
}

#[test]
fn compiler_accepts_prompt_file_for_prompt_aware_steps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/goose.cli");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            prompt_file: Some("prompts/task.md".to_string()),
            ..flow_step("step_1", "goose.cli", "run_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    validate_flow_in_home(&home_dir, &flow).expect("validate");
    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile");
    let prompt_binding = recipe.steps[0]
        .prompt_binding
        .as_ref()
        .expect("prompt binding");
    assert_eq!(prompt_binding.file_field.as_deref(), Some("prompt_file"));
}

#[test]
fn compiler_rejects_multiple_prompt_sources() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");

    let mut step = flow_step("step_1", "codex.cli", "exec_task");
    step.prompt = Some("Say hello".to_string());
    step.with.insert(
        "prompt".to_string(),
        yaml_serde::Value::String("Say something else".to_string()),
    );
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![step],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("mixed prompt sources");
    assert!(err.to_string().contains("multiple prompt sources"));
}

#[test]
fn compiler_rejects_prompt_file_for_http_actions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let plugin_dir = tmp.path().join("jira.prompt");
    std::fs::create_dir_all(plugin_dir.join("schemas")).expect("schemas dir");
    std::fs::write(
        plugin_dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: jira.prompt
version: 1.0.0
adapter: http
entrypoint:
  type: http
  base_url: https://example.com
package:
  files: [schemas/input.json, schemas/output.json]
actions:
  - name: ask
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings:
      transist_prompt_v1:
        inline_field: prompt
        file_field: prompt_file
capabilities:
  supports_cancel: false
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    )
    .expect("plugin manifest");
    std::fs::write(plugin_dir.join("schemas/input.json"), "{}").expect("input schema");
    std::fs::write(plugin_dir.join("schemas/output.json"), "{}").expect("output schema");
    transist_plugins::install_plugin_file_in_home(&home_dir, &plugin_dir.join("plugin.yaml"))
        .expect("install plugin");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            adapter: Some("http".to_string()),
            prompt_file: Some("prompts/http.md".to_string()),
            ..flow_step("step_1", "jira.prompt", "ask")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("http prompt_file");
    assert!(
        err.to_string()
            .contains("cannot use prompt_file with adapter 'http'")
    );
}

#[test]
fn compiler_rejects_prompt_aware_steps_without_binding() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            prompt: Some("Say hello".to_string()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("missing binding");
    assert!(err.to_string().contains("is not prompt-aware"));
}

#[test]
fn compiler_rejects_secret_refs_not_declared_by_action() {
    for opted_in in [false, true] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        install_plugin_fixture(&home_dir, "plugins/shell");
        if opted_in {
            enable_installed_action_input_projection(&home_dir);
        }

        let mut step = flow_step("step_1", "shell", "echo");
        step.with.insert(
            "secret_refs".to_string(),
            yaml_serde::Value::Sequence(vec![yaml_serde::Value::String("API_TOKEN".to_string())]),
        );
        let flow = Flow {
            schema: FLOW_SCHEMA_V1.to_string(),
            id: "demo".to_string(),
            version: "v1".to_string(),
            plugins: BTreeMap::new(),
            prerequisites: Default::default(),
            inputs: BTreeMap::new(),
            workspace: None,
            context: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            documents: Default::default(),
            profiles: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            steps: vec![step],
            cleanup: Vec::new(),
            source_path: Some(tmp.path().join("workflow.yaml")),
        };

        let err = validate_flow_in_home(&home_dir, &flow).expect_err("undeclared secret ref");
        assert!(
            err.to_string()
                .contains("undeclared secret ref 'API_TOKEN'")
        );
    }
}

#[test]
fn compiler_rejects_missing_required_secret_refs() {
    for opted_in in [false, true] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path().join(".transist");
        install_plugin_fixture(&home_dir, "plugins/shell");
        if opted_in {
            enable_installed_action_input_projection(&home_dir);
        }

        let flow = Flow {
            schema: FLOW_SCHEMA_V1.to_string(),
            id: "demo".to_string(),
            version: "v1".to_string(),
            plugins: BTreeMap::new(),
            prerequisites: Default::default(),
            inputs: BTreeMap::new(),
            workspace: None,
            context: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            documents: Default::default(),
            profiles: BTreeMap::new(),
            filesystem_policy: None,
            network_policy: None,
            steps: vec![flow_step("step_1", "shell", "exec_with_secret_env")],
            cleanup: Vec::new(),
            source_path: Some(tmp.path().join("workflow.yaml")),
        };

        let err = compile_flow_in_home(&home_dir, &flow).expect_err("missing required secret ref");
        assert!(
            err.to_string()
                .contains("missing required secret ref 'MY_API_TOKEN'")
        );
    }
}

fn enable_installed_action_input_projection(home_dir: &Path) {
    let path = home_dir.join("plugins/shell/0.1.0/plugin.yaml");
    let mut manifest: transist_plugins::PluginManifest =
        yaml_serde::from_str(&std::fs::read_to_string(&path).expect("manifest"))
            .expect("parse manifest");
    for action in &mut manifest.actions {
        action.bindings.insert(
            "input_projection".into(),
            serde_json::json!("action_data_v1"),
        );
    }
    std::fs::write(
        path,
        yaml_serde::to_string(&manifest).expect("serialize manifest"),
    )
    .expect("manifest");
}

#[test]
fn compiler_defers_input_bound_secret_ref_validation_to_run_birth() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let mut step = flow_step("step_1", "shell", "exec_with_secret_env");
    step.with.insert(
        "secret_refs".to_string(),
        yaml_serde::Value::Sequence(vec![yaml_serde::Value::String(
            "{{ inputs.secret_name }}".to_string(),
        )]),
    );
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "dynamic-secret".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::from([(
            "secret_name".to_string(),
            FlowInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: None,
            },
        )]),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![step],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    compile_flow_in_home(&home_dir, &flow).expect("dynamic secret is checked at run birth");
}

#[test]
fn compiler_accepts_step_placeholders_in_invoke_with() {
    let mut step = flow_step("step_1", "shell", "exec");
    step.with.insert(
        "command".to_string(),
        yaml_serde::Value::String("echo {{steps.resolve_plan_dir.out.stdout}}".to_string()),
    );
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![flow_step("resolve_plan_dir", "shell", "exec"), step],
        cleanup: Vec::new(),
        source_path: Some(PathBuf::from("/tmp/workflow.yaml")),
    };

    validate_flow(&flow).expect("direct output binding is valid");
}

#[test]
fn compiler_accepts_step_placeholders_in_prompt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![
            flow_step("review_plan", "codex.cli", "exec_task"),
            FlowStep {
                prompt: Some("Summarize {{steps.review_plan.out.reason}}".to_string()),
                ..flow_step("step_1", "codex.cli", "exec_task")
            },
        ],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    validate_flow_in_home(&home_dir, &flow).expect("inline output prompt is valid");
}

#[test]
fn compiler_rejects_step_placeholders_in_prompt_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/goose.cli");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            prompt_file: Some("{{steps.review_plan.out.prompt_path}}".to_string()),
            ..flow_step("step_1", "goose.cli", "run_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("step placeholder in prompt_file");
    let rendered = format!("{err:#}");
    assert!(rendered.contains("prompt_file"));
    assert!(rendered.contains("authority"));
}

#[test]
fn compiler_accepts_step_placeholders_in_subflow_inputs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let child_path = tmp.path().join("child.yaml");
    std::fs::write(
        &child_path,
        r#"
schema: transist.module.v1
id: child
version: v1
inputs:
  review_feedback:
    type: string
    required: false
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![
            flow_step("review_cycle_decision", "shell", "exec"),
            FlowStep {
                id: "child".to_string(),
                adapter: None,
                plugin: None,
                action: None,
                execution_kind: None,
                profile: None,
                control: None,
                subflow: Some(transist_compiler::FlowSubflowCall {
                    path: "./child.yaml".to_string(),
                    inputs: BTreeMap::from([(
                        "review_feedback".to_string(),
                        yaml_serde::Value::String(
                            "{{steps.review_cycle_decision.out.reason}}".to_string(),
                        ),
                    )]),
                    rerun_mode: None,
                }),
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
            },
        ],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("parent.yaml")),
    };

    validate_flow(&flow).expect("module inputs support direct output bindings");
}

#[test]
fn load_flow_unvalidated_allows_prompt_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(
        &path,
        r#"
schema: transist.flow.v1
id: demo
version: v1
plugins:
  codex.cli: 0.1.0
steps:
  - id: step_1
    uses: process:codex.cli/exec_task
    prompt: Say hello
"#,
    )
    .expect("write");

    let flow = load_flow_unvalidated(&path).expect("load");
    assert_eq!(flow.steps[0].prompt.as_deref(), Some("Say hello"));
}

#[test]
fn compiler_requires_home_context_for_first_class_skills_fields() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            skills: Some(vec!["./skills/example".to_string()]),
            ..flow_step("step_1", "codex.cli", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(PathBuf::from("/tmp/workflow.yaml")),
    };

    let err = validate_flow(&flow).expect_err("skills require home-aware validation");
    assert!(err.to_string().contains("uses first-class skills fields"));
}

#[test]
fn compiler_compiles_skill_aware_steps_in_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");
    let skill_dir = write_skill_fixture(tmp.path(), "authoring");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            prompt: Some("Say hello".to_string()),
            skills: Some(vec![skill_dir.display().to_string()]),
            ..flow_step("step_1", "codex.cli", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile");
    let skills_binding = recipe.steps[0]
        .skills_binding
        .as_ref()
        .expect("skills binding");
    assert_eq!(skills_binding.bundle_field, "skills_bundle");
    assert_eq!(skills_binding.rendered_field, "skills_rendered");
    assert_eq!(skills_binding.bundle.roots.len(), 1);
    assert_eq!(skills_binding.bundle.roots[0].skill_name, "authoring");
    assert_eq!(skills_binding.bundle.roots[0].documents.len(), 2);
    assert!(skills_binding.rendered.contains("# Workflow Skills"));
    assert!(skills_binding.rendered.contains("Use this carefully."));
    assert!(recipe.steps[0].prompt_binding.is_some());
}

#[test]
fn snapshot_compilation_reads_skill_documents_only_from_captured_bytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");
    let skill_dir = write_skill_fixture(tmp.path(), "authoring");
    let workflow = tmp.path().join("workflow.yaml");
    std::fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: snapshot-skills\nversion: v1\nplugins:\n  codex.cli: 0.1.0\nsteps:\n  - id: run\n    uses: process:codex.cli/exec_task\n    skills:\n      - authoring\ncleanup:\n  - id: cleanup\n    when: [failed]\n    uses: process:codex.cli/exec_task\n    skills:\n      - authoring\n",
    )
    .expect("workflow");
    let snapshot = capture_workflow_definition(&workflow).expect("snapshot");

    std::fs::write(skill_dir.join("SKILL.md"), "invalid live skill").expect("replace skill");
    std::fs::write(
        skill_dir.join("references/ref.md"),
        "live bytes must not compile",
    )
    .expect("replace reference");

    let recipe = compile_workflow_snapshot_in_home(&home_dir, &snapshot).expect("compile snapshot");
    let binding = recipe.steps[0]
        .skills_binding
        .as_ref()
        .expect("skills binding");
    assert_eq!(binding.bundle.roots[0].skill_name, "authoring");
    assert!(binding.rendered.contains("Use this carefully."));
    assert!(!binding.rendered.contains("live bytes must not compile"));
    let cleanup_binding = recipe.cleanup[0]
        .step
        .skills_binding
        .as_ref()
        .expect("cleanup skills binding");
    assert!(cleanup_binding.rendered.contains("Use this carefully."));
}

#[test]
fn compiler_rejects_empty_skills_array() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("workflow.yaml");
    std::fs::write(
        &path,
        r#"
schema: transist.flow.v1
id: demo
version: v1
plugins:
  codex.cli: 0.1.0
steps:
  - id: step_1
    uses: process:codex.cli/exec_task
    skills: []
"#,
    )
    .expect("write");

    let flow = load_flow_unvalidated(&path).expect("load");
    let err = validate_flow(&flow).expect_err("empty skills");
    assert!(err.to_string().contains("skills cannot be empty"));
}

#[test]
fn compiler_rejects_skills_on_control_steps() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            id: "gate".to_string(),
            adapter: None,
            plugin: None,
            action: None,
            execution_kind: None,
            profile: None,
            control: Some("control.gate.manual".to_string()),
            subflow: None,
            prompt: None,
            prompt_file: None,
            skills: Some(vec!["./skills/example".to_string()]),
            gate_decision: None,
            plugin_version: None,
            with: BTreeMap::new(),
            if_block: None,
            loop_block: None,
            set: None,
            filesystem_policy: None,
            network_policy: None,
        }],
        cleanup: Vec::new(),
        source_path: Some(PathBuf::from("/tmp/workflow.yaml")),
    };

    let err = validate_flow(&flow).expect_err("skills on control");
    assert!(
        err.to_string()
            .contains("cannot define skills on a control step")
    );
}

#[test]
fn compiler_rejects_skills_on_subflow_steps() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            id: "child".to_string(),
            adapter: None,
            plugin: None,
            action: None,
            execution_kind: None,
            profile: None,
            control: None,
            subflow: Some(transist_compiler::FlowSubflowCall {
                path: "./child.yaml".to_string(),
                inputs: BTreeMap::new(),
                rerun_mode: None,
            }),
            prompt: None,
            prompt_file: None,
            skills: Some(vec!["./skills/example".to_string()]),
            gate_decision: None,
            plugin_version: None,
            with: BTreeMap::new(),
            if_block: None,
            loop_block: None,
            set: None,
            filesystem_policy: None,
            network_policy: None,
        }],
        cleanup: Vec::new(),
        source_path: Some(PathBuf::from("/tmp/workflow.yaml")),
    };

    let err = validate_flow(&flow).expect_err("skills on subflow");
    assert!(
        err.to_string()
            .contains("cannot define skills on a subflow step")
    );
}

#[test]
fn compiler_rejects_colliding_skills_fields_in_with() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");
    let skill_dir = write_skill_fixture(tmp.path(), "review");

    let mut step = flow_step("step_1", "codex.cli", "exec_task");
    step.skills = Some(vec![skill_dir.display().to_string()]);
    step.with.insert(
        "skills_bundle".to_string(),
        yaml_serde::Value::String("collision".to_string()),
    );
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![step],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("skills collision");
    assert!(
        err.to_string()
            .contains("collide with injected skills fields")
    );
}

#[test]
fn compiler_rejects_missing_skill_reference() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            skills: Some(vec!["./missing-skill".to_string()]),
            ..flow_step("step_1", "codex.cli", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("missing skill");
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn compiler_rejects_empty_skill_collection_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");
    let empty_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&empty_dir).expect("empty skills dir");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            skills: Some(vec!["./skills".to_string()]),
            ..flow_step("step_1", "codex.cli", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("empty collection");
    assert!(err.to_string().contains("resolves to no skills"));
}

#[test]
fn compiler_rejects_missing_transitive_markdown_reference() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");
    let skill_dir = write_broken_skill_fixture(tmp.path(), "broken");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            skills: Some(vec![skill_dir.display().to_string()]),
            ..flow_step("step_1", "codex.cli", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("broken markdown ref");
    assert!(err.to_string().contains("references missing markdown file"));
}

#[test]
fn compiler_rejects_skills_for_missing_plugin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let skill_dir = write_skill_fixture(tmp.path(), "authoring");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            skills: Some(vec![skill_dir.display().to_string()]),
            ..flow_step("step_1", "missing.plugin", "exec_task")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("missing plugin");
    assert!(
        err.to_string()
            .contains("references plugin 'missing.plugin' which is not installed")
    );
}

#[test]
fn compiler_reports_all_missing_plugins_in_one_validation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![
            flow_step("first", "missing.plugin_a", "run"),
            flow_step("second", "missing.plugin_b", "run"),
        ],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let error = validate_flow_in_home(&home_dir, &flow).expect_err("missing plugins");
    let rendered = error.to_string();
    assert!(rendered.contains("2 workflow validation error(s)"));
    assert!(rendered.contains("references plugin 'missing.plugin_a' which is not installed"));
    assert!(rendered.contains("references plugin 'missing.plugin_b' which is not installed"));
    assert!(
        rendered
            .find("missing.plugin_a")
            .expect("first plugin diagnostic")
            < rendered
                .find("missing.plugin_b")
                .expect("second plugin diagnostic")
    );

    let error = compile_flow_in_home(&home_dir, &flow).expect_err("compile must not proceed");
    let rendered = error.to_string();
    assert!(rendered.contains("references plugin 'missing.plugin_a' which is not installed"));
    assert!(rendered.contains("references plugin 'missing.plugin_b' which is not installed"));
}

#[test]
fn compiler_rejects_skills_for_unknown_action() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "tests/fixtures/plugins/codex.cli");
    let skill_dir = write_skill_fixture(tmp.path(), "authoring");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            skills: Some(vec![skill_dir.display().to_string()]),
            ..flow_step("step_1", "codex.cli", "unknown_action")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("unknown action");
    assert!(
        err.to_string()
            .contains("references unknown action 'codex.cli.unknown_action'")
    );
}

#[test]
fn compiler_rejects_skills_for_non_skill_aware_actions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");
    let skill_dir = write_skill_fixture(tmp.path(), "authoring");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            skills: Some(vec![skill_dir.display().to_string()]),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("missing skills binding");
    assert!(err.to_string().contains("is not skills-aware"));
}

// ── set block validation ─────────────────────────────────────────────────────

#[test]
fn compiler_compiles_set_block_into_recipe_vars() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::from([(
            "greeting".to_string(),
            FlowInputSpec {
                description: None,
                default: None,
                required: false,
                value_type: "string".to_string(),
            },
        )]),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            set: Some(BTreeMap::from([
                (
                    "msg".to_string(),
                    yaml_serde::Value::String("hello".to_string()),
                ),
                (
                    "forwarded".to_string(),
                    yaml_serde::Value::String("{{inputs.greeting}}".to_string()),
                ),
            ])),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile");
    let step = &recipe.steps[0];
    assert_eq!(step.vars.len(), 2);
    assert!(step.vars.contains_key("msg"));
    assert!(step.vars.contains_key("forwarded"));
}

#[test]
fn compiler_rejects_set_block_on_control_step() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            id: "gate".to_string(),
            adapter: None,
            plugin: None,
            action: None,
            execution_kind: None,
            profile: None,
            control: Some("control.finish".to_string()),
            subflow: None,
            prompt: None,
            prompt_file: None,
            skills: None,
            gate_decision: None,
            plugin_version: None,
            with: BTreeMap::new(),
            if_block: None,
            loop_block: None,
            set: Some(BTreeMap::from([(
                "x".to_string(),
                yaml_serde::Value::String("1".to_string()),
            )])),
            filesystem_policy: None,
            network_policy: None,
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let err = validate_flow(&flow).expect_err("set on control step");
    assert!(
        err.to_string().contains("set"),
        "expected error mentioning 'set', got: {}",
        err
    );
}

#[test]
fn compiler_rejects_vars_placeholder_in_set_rhs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            set: Some(BTreeMap::from([(
                "x".to_string(),
                yaml_serde::Value::String("{{vars.previous}}".to_string()),
            )])),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("vars in set rhs");
    assert!(
        err.to_string().contains("set RHS"),
        "expected error about vars in set rhs, got: {}",
        err
    );
}

#[test]
fn compiler_rejects_set_block_on_subflow_step() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            id: "call".to_string(),
            adapter: None,
            plugin: None,
            action: None,
            execution_kind: None,
            profile: None,
            control: None,
            subflow: Some(transist_compiler::FlowSubflowCall {
                path: "child.yaml".to_string(),
                inputs: BTreeMap::new(),
                rerun_mode: None,
            }),
            prompt: None,
            prompt_file: None,
            skills: None,
            gate_decision: None,
            plugin_version: None,
            with: BTreeMap::new(),
            if_block: None,
            loop_block: None,
            set: Some(BTreeMap::from([(
                "x".to_string(),
                yaml_serde::Value::String("1".to_string()),
            )])),
            filesystem_policy: None,
            network_policy: None,
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let err = validate_flow(&flow).expect_err("set on subflow step");
    assert!(
        err.to_string().contains("set") && err.to_string().contains("subflow"),
        "expected error mentioning 'set' and 'subflow', got: {}",
        err
    );
}

#[test]
fn compiler_allows_set_key_shadowing_input_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    // Input named "name" exists; set block writes a var also named "name".
    // The two namespaces are independent and this should compile cleanly.
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::from([(
            "name".to_string(),
            FlowInputSpec {
                description: None,
                default: None,
                required: false,
                value_type: "string".to_string(),
            },
        )]),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            set: Some(BTreeMap::from([(
                "name".to_string(),
                yaml_serde::Value::String("shadow".to_string()),
            )])),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    compile_flow_in_home(&home_dir, &flow).expect("set key may shadow input key");
}

#[test]
fn compiler_rejects_undefined_vars_reference_in_if_guard() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let if_wrapper = FlowStep {
        id: "gate".to_string(),
        adapter: None,
        plugin: None,
        action: None,
        execution_kind: None,
        profile: None,
        control: Some("control.noop".to_string()),
        subflow: None,
        prompt: None,
        prompt_file: None,
        skills: None,
        gate_decision: None,
        plugin_version: None,
        with: BTreeMap::new(),
        if_block: Some(FlowIfBlock {
            guard: "vars.never_defined == \"x\"".to_string(),
            then_branch: vec![FlowRef {
                ref_step: "tail".to_string(),
            }],
            else_branch: Vec::new(),
        }),
        loop_block: None,
        set: None,
        filesystem_policy: None,
        network_policy: None,
    };

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![if_wrapper, flow_step("tail", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("undefined vars in guard");
    let msg = err.to_string();
    assert!(
        msg.contains("never_defined") && msg.contains("guard"),
        "expected error about undefined var in guard, got: {}",
        msg
    );
}

#[test]
fn compiler_rejects_undefined_vars_reference_in_loop_until() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let loop_wrapper = FlowStep {
        id: "loop_step".to_string(),
        adapter: None,
        plugin: None,
        action: None,
        execution_kind: None,
        profile: None,
        control: Some("control.noop".to_string()),
        subflow: None,
        prompt: None,
        prompt_file: None,
        skills: None,
        gate_decision: None,
        plugin_version: None,
        with: BTreeMap::new(),
        if_block: None,
        loop_block: Some(FlowLoopBlock {
            body: vec![FlowRef {
                ref_step: "body".to_string(),
            }],
            while_expr: None,
            until: Some("vars.bogus == \"done\"".to_string()),
            max_iters: 3,
            max_duration: None,
        }),
        set: None,
        filesystem_policy: None,
        network_policy: None,
    };

    // Body step does NOT define `vars.bogus` — should fail.
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![loop_wrapper, flow_step("body", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = validate_flow_in_home(&home_dir, &flow).expect_err("undefined vars in until");
    let msg = err.to_string();
    assert!(
        msg.contains("bogus") && msg.contains("until"),
        "expected error about undefined var in loop.until, got: {}",
        msg
    );
}

#[test]
fn compiler_accepts_loop_until_referencing_var_set_in_body() {
    // The loop wrapper precedes the body lexically, but loop exit
    // expressions are evaluated after each iteration, so vars written by
    // body steps must be accepted by the validator.
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let loop_wrapper = FlowStep {
        id: "loop_step".to_string(),
        adapter: None,
        plugin: None,
        action: None,
        execution_kind: None,
        profile: None,
        control: Some("control.noop".to_string()),
        subflow: None,
        prompt: None,
        prompt_file: None,
        skills: None,
        gate_decision: None,
        plugin_version: None,
        with: BTreeMap::new(),
        if_block: None,
        loop_block: Some(FlowLoopBlock {
            body: vec![FlowRef {
                ref_step: "body".to_string(),
            }],
            while_expr: None,
            until: Some("vars.flag == \"done\"".to_string()),
            max_iters: 3,
            max_duration: None,
        }),
        set: None,
        filesystem_policy: None,
        network_policy: None,
    };

    let body = FlowStep {
        set: Some(BTreeMap::from([(
            "flag".to_string(),
            yaml_serde::Value::String("done".to_string()),
        )])),
        ..flow_step("body", "shell", "echo")
    };

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![loop_wrapper, body],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow)
        .expect("loop until referencing body-set var should compile");
    let graph = recipe.graph.as_ref().unwrap();
    assert_eq!(graph.loop_routing_version, Some(1));
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.loop_route.is_some())
            .count(),
        1
    );
    transist_protocol::validate_recipe_loop_routing(&recipe).unwrap();
}

// ── plugin_version ───────────────────────────────────────────────────────────

#[test]
fn compiler_resolves_exact_plugin_version_in_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let shell_version = plugin_fixture_version("plugins/shell");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            plugin_version: Some(shell_version.clone()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile with exact version");
    assert!(matches!(
        &recipe.steps[0].execution,
        StepExecutionDescriptor::Invoke { plugin_version: Some(v), .. } if v == &shell_version
    ));
}

#[test]
fn compiler_lowers_declared_uses_to_a_pinned_recipe_invocation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let shell_version = plugin_fixture_version("plugins/shell");
    install_plugin_fixture(&home_dir, "plugins/shell");
    let workflow_path = tmp.path().join("workflow.yaml");
    std::fs::write(
        &workflow_path,
        format!(
            "schema: transist.flow.v1\nid: uses-demo\nversion: v1\nplugins:\n  shell: {shell_version}\nsteps:\n  - id: echo\n    uses: process:shell/echo\n    with:\n      message: hello\n"
        ),
    )
    .expect("write workflow");

    let flow = load_flow(&workflow_path).expect("load uses workflow");
    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile uses workflow");
    assert!(matches!(
        &recipe.steps[0].execution,
        StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin,
            action,
            plugin_version: Some(version),
            ..
        } if plugin == "shell" && action == "echo" && version == &shell_version
    ));
}

#[test]
fn compiler_rejects_invalid_or_undeclared_uses_dependencies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cases = [
        (
            "missing-declaration",
            "steps:\n  - id: echo\n    uses: process:shell/echo\n",
            "declares no plugins",
        ),
        (
            "non-exact-version",
            "plugins:\n  shell: latest\nsteps:\n  - id: echo\n    uses: process:shell/echo\n",
            "exact semantic version",
        ),
        (
            "unused-declaration",
            "plugins:\n  shell: 0.1.0\n  unused: 1.0.0\nsteps:\n  - id: echo\n    uses: process:shell/echo\n",
            "unused",
        ),
    ];

    for (name, body, expected) in cases {
        let path = tmp.path().join(format!("{name}.yaml"));
        std::fs::write(
            &path,
            format!("schema: transist.flow.v1\nid: {name}\nversion: v1\n{body}"),
        )
        .expect("write invalid workflow");
        let error = load_flow(&path).expect_err("workflow must be rejected");
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[test]
fn compiler_rejects_mixed_or_adapter_mismatched_uses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mixed_path = tmp.path().join("mixed.yaml");
    std::fs::write(
        &mixed_path,
        "schema: transist.flow.v1\nid: mixed\nversion: v1\nplugins:\n  shell: 0.1.0\nsteps:\n  - id: echo\n    uses: process:shell/echo\n    plugin: shell\n",
    )
    .expect("write mixed workflow");
    let mixed_error = load_flow_unvalidated(&mixed_path).expect_err("mixed syntax must fail");
    assert!(
        format!("{mixed_error:#}").contains("uses"),
        "{mixed_error:#}"
    );

    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");
    let mismatch_path = tmp.path().join("mismatch.yaml");
    std::fs::write(
        &mismatch_path,
        "schema: transist.flow.v1\nid: mismatch\nversion: v1\nplugins:\n  shell: 0.1.0\nsteps:\n  - id: echo\n    uses: http:shell/echo\n",
    )
    .expect("write mismatch workflow");
    let mismatch = load_flow(&mismatch_path).expect("parse mismatch workflow");
    assert!(
        compile_flow_in_home(&home_dir, &mismatch)
            .expect_err("adapter mismatch must fail")
            .to_string()
            .contains("declares adapter")
    );
}

#[test]
fn compiler_resolves_latest_plugin_version_in_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            plugin_version: Some("latest".to_string()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile with latest version");
    // "latest" resolves to the installed version string for the shell fixture.
    assert!(matches!(
        &recipe.steps[0].execution,
        StepExecutionDescriptor::Invoke {
            plugin_version: Some(_),
            ..
        }
    ));
}

#[test]
fn compiler_passes_through_exact_version_without_home() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            plugin_version: Some("0.1.0".to_string()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("compile without home");
    assert!(matches!(
        &recipe.steps[0].execution,
        StepExecutionDescriptor::Invoke { plugin_version: Some(v), .. } if v == "0.1.0"
    ));
}

#[test]
fn compiler_omits_plugin_version_when_latest_and_no_home() {
    // "latest" cannot be resolved without a home context; the field must be
    // absent from the recipe so the adapter falls back to its default resolver.
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            plugin_version: Some("latest".to_string()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("compile without home");
    assert!(matches!(
        &recipe.steps[0].execution,
        StepExecutionDescriptor::Invoke {
            plugin_version: None,
            ..
        }
    ));
}

#[test]
fn omitted_plugin_version_with_zero_installed_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![flow_step("step_1", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = compile_flow_in_home(&home_dir, &flow).expect_err("compile should fail");
    assert!(
        err.to_string()
            .contains("references plugin 'shell' which is not installed"),
        "unexpected error: {err}"
    );
}

#[test]
fn omitted_plugin_version_with_multiple_installed_fails_at_compile_time() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");
    install_shell_fixture_with_version(&home_dir, "1.5.0");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![flow_step("step_1", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let err = compile_flow_in_home(&home_dir, &flow).expect_err("compile should fail");
    assert!(
        err.to_string().contains("multiple installed versions"),
        "unexpected error: {err}"
    );
}

#[test]
fn exact_plugin_version_resolves_with_multiple_installed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");
    install_shell_fixture_with_version(&home_dir, "1.5.0");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            plugin_version: Some("1.5.0".to_string()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile with exact 1.5.0");
    assert!(
        matches!(
            &recipe.steps[0].execution,
            StepExecutionDescriptor::Invoke {
                plugin_version: Some(v),
                ..
            } if v == "1.5.0"
        ),
        "exact selector should resolve to 1.5.0 when multiple versions installed"
    );
}

#[test]
fn latest_plugin_version_resolves_to_highest_with_multiple_installed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");
    install_shell_fixture_with_version(&home_dir, "1.5.0");

    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            plugin_version: Some("latest".to_string()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: Some(tmp.path().join("workflow.yaml")),
    };

    let recipe = compile_flow_in_home(&home_dir, &flow).expect("compile with latest");
    assert!(
        matches!(
            &recipe.steps[0].execution,
            StepExecutionDescriptor::Invoke {
                plugin_version: Some(v),
                ..
            } if v == "1.5.0"
        ),
        "latest selector should resolve to highest version (1.5.0) when multiple installed"
    );
}

#[test]
fn subflow_yaml_drift_under_same_root_version_is_rejected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_plugin_fixture(&home_dir, "plugins/shell");

    let child = tmp.path().join("child.yaml");
    std::fs::write(
        &child,
        r#"
schema: transist.module.v1
id: child_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: child_step
    uses: process:shell/echo
    with:
      args: ["first"]
"#,
    )
    .expect("write child v1");

    let parent = tmp.path().join("parent.yaml");
    std::fs::write(
        &parent,
        r#"
schema: transist.flow.v1
id: parent_demo
version: v1
plugins:
  shell: 0.1.0
modules:
  child:
    path: child.yaml
steps:
  - id: child
    module: child
"#,
    )
    .expect("write parent");

    let first = load_flow(&parent).expect("load parent v1");
    let first_recipe = compile_flow_in_home(&home_dir, &first).expect("compile parent v1");
    write_recipe_in_home(&home_dir, &first_recipe).expect("write first recipe");

    std::fs::write(
        &child,
        r#"
schema: transist.module.v1
id: child_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: child_step
    uses: process:shell/echo
    with:
      args: ["second"]
"#,
    )
    .expect("write child v2");

    let second = load_flow(&parent).expect("load parent v2");
    let second_recipe = compile_flow_in_home(&home_dir, &second).expect("compile parent v2");
    let err = write_recipe_in_home(&home_dir, &second_recipe).expect_err("drift must be rejected");
    assert!(
        err.to_string()
            .contains("already compiled with different content"),
        "unexpected error: {err}"
    );
}

#[test]
fn compiler_rejects_plugin_version_on_control_step() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            id: "gate".to_string(),
            adapter: None,
            plugin: None,
            action: None,
            execution_kind: None,
            profile: None,
            control: Some("control.finish".to_string()),
            subflow: None,
            prompt: None,
            prompt_file: None,
            skills: None,
            gate_decision: None,
            plugin_version: Some("1.0.0".to_string()),
            with: BTreeMap::new(),
            if_block: None,
            loop_block: None,
            set: None,
            filesystem_policy: None,
            network_policy: None,
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let err = validate_flow(&flow).expect_err("plugin_version on control step");
    assert!(
        err.to_string().contains("plugin_version"),
        "expected error about plugin_version on control step, got: {}",
        err
    );
}

#[test]
fn compiler_rejects_empty_plugin_version() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            plugin_version: Some("".to_string()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let err = validate_flow(&flow).expect_err("empty plugin_version");
    assert!(
        err.to_string().contains("plugin_version"),
        "expected error about empty plugin_version, got: {}",
        err
    );
}

#[test]
fn compiler_rejects_traversal_flow_id() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "../escape".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![flow_step("step_1", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: None,
    };

    let err = validate_flow(&flow).expect_err("traversal flow id must be rejected");
    assert!(
        err.to_string().contains("flow id"),
        "unexpected error: {err}"
    );
}

#[test]
fn compiler_rejects_absolute_flow_version() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "/etc/passwd".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![flow_step("step_1", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: None,
    };

    let err = validate_flow(&flow).expect_err("absolute flow version must be rejected");
    assert!(
        err.to_string().contains("flow version"),
        "unexpected error: {err}"
    );
}

#[test]
fn compiler_rejects_step_id_with_path_separator() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![flow_step("a/b", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: None,
    };

    let err = validate_flow(&flow).expect_err("step id with separator must be rejected");
    assert!(
        err.to_string().contains("flow.steps"),
        "unexpected error: {err}"
    );
}

#[test]
fn compiler_accepts_valid_slug_ids() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "demo-workflow.v2".to_string(),
        version: "1.2.3".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![flow_step("step_1.a-b", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: None,
    };

    validate_flow(&flow).expect("valid slug ids must be accepted");
}

// ── filesystem_policy compiler tests ────────────────────────────────────────

fn custom_policy() -> FilesystemPolicy {
    FilesystemPolicy {
        workspace_access: Some(WorkspaceAccessPolicy {
            mode: WorkspaceAccessMode::Custom,
        }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    }
}

fn external_grant_policy(reads: &[&str], writes: &[&str]) -> FilesystemPolicy {
    FilesystemPolicy {
        workspace_access: Some(WorkspaceAccessPolicy {
            mode: WorkspaceAccessMode::ReadOnly,
        }),
        read_allow_paths: reads.iter().map(|value| value.to_string()).collect(),
        write_allow_paths: writes.iter().map(|value| value.to_string()).collect(),
    }
}

fn custom_network_policy(
    mode: NetworkAccessMode,
    allow_hosts: &[&str],
    allow_ports: &[u16],
) -> NetworkPolicy {
    NetworkPolicy {
        mode,
        allow_hosts: allow_hosts.iter().map(|value| value.to_string()).collect(),
        allow_ports: allow_ports.to_vec(),
        tls_required: true,
        max_redirects: 5,
    }
}

#[test]
fn compiler_round_trips_regular_invoke_filesystem_policy() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "fs-policy".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(external_grant_policy(
                &["/etc/gitconfig", "~/.gitconfig"],
                &["~/.cache/transist"],
            )),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("compile");
    let step = &recipe.steps[0];
    let policy = step.filesystem_policy.as_ref().expect("step policy");
    assert_eq!(
        policy.workspace_access.as_ref().unwrap().mode,
        WorkspaceAccessMode::ReadOnly
    );
    assert_eq!(
        policy.read_allow_paths,
        vec!["/etc/gitconfig", "~/.gitconfig"]
    );
    assert_eq!(policy.write_allow_paths, vec!["~/.cache/transist"]);
}

#[test]
fn compiler_rejects_custom_filesystem_policy() {
    let err = validate_flow(&Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "custom-policy".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(custom_policy()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    })
    .expect_err("custom policy rejected");
    assert!(
        err.to_string()
            .contains("filesystem_custom_mode_unsupported")
    );
}

#[test]
fn compiler_round_trips_cleanup_step_filesystem_policy() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "fs-policy-cleanup".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: vec![FlowCleanupStep {
            id: "cleanup_1".to_string(),
            when: vec![FlowCleanupWhen::Failed],
            adapter: "process".to_string(),
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            prompt: None,
            prompt_file: None,
            skills: None,
            with: BTreeMap::new(),
            filesystem_policy: Some(FilesystemPolicy {
                workspace_access: Some(WorkspaceAccessPolicy {
                    mode: WorkspaceAccessMode::ReadOnly,
                }),
                read_allow_paths: vec!["/tmp/transist".to_string()],
                write_allow_paths: Vec::new(),
            }),
            network_policy: None,
        }],
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("compile");
    assert_eq!(recipe.cleanup.len(), 1);
    let step = &recipe.cleanup[0].step;
    let policy = step.filesystem_policy.as_ref().expect("cleanup policy");
    assert_eq!(
        policy.workspace_access.as_ref().unwrap().mode,
        WorkspaceAccessMode::ReadOnly
    );
    assert_eq!(policy.read_allow_paths, vec!["/tmp/transist"]);
}

#[test]
fn compiler_round_trips_regular_invoke_network_policy() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "network-policy".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            network_policy: Some(custom_network_policy(
                NetworkAccessMode::Allowlist,
                &["api.example.com", "auth.example.com"],
                &[443, 8443],
            )),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("compile");
    let policy = recipe.steps[0]
        .network_policy
        .as_ref()
        .expect("network policy");
    assert_eq!(policy.mode, NetworkAccessMode::Allowlist);
    assert_eq!(
        policy.allow_hosts,
        vec!["api.example.com", "auth.example.com"]
    );
    assert_eq!(policy.allow_ports, vec![443, 8443]);
    assert!(policy.tls_required);
    assert_eq!(policy.max_redirects, 5);
}

#[test]
fn compiler_does_not_inject_with_ignore_paths() {
    let mut with = BTreeMap::new();
    with.insert(
        "message".to_string(),
        yaml_serde::Value::String("plugin-owned".to_string()),
    );
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "preserve-ignore".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(external_grant_policy(
                &["/etc/gitconfig"],
                &["~/.cache/transist"],
            )),
            with,
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    };

    let recipe = compile_flow(&flow).expect("compile");
    assert!(!recipe.steps[0].with.contains_key("ignore_paths"));
    assert_eq!(
        serde_json::to_value(recipe.steps[0].with.get("message").unwrap()).unwrap(),
        serde_json::json!("plugin-owned"),
    );
}

#[test]
fn compiler_rejects_empty_filesystem_policy_pattern() {
    let err = validate_flow(&Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "empty-pattern".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(FilesystemPolicy {
                workspace_access: Some(WorkspaceAccessPolicy {
                    mode: WorkspaceAccessMode::ReadOnly,
                }),
                read_allow_paths: vec!["".to_string()],
                write_allow_paths: Vec::new(),
            }),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    })
    .expect_err("empty external path");
    assert!(err.to_string().contains("non-empty"));
}

#[test]
fn workspace_binding_compiler_accepts_optional_string_read_allow_path_input() {
    let flow = Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "optional-read-path".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::from([(
            "config_path".to_string(),
            FlowInputSpec {
                value_type: "string".to_string(),
                required: false,
                default: None,
                description: None,
            },
        )]),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: Some(FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec!["{{inputs.config_path}}".to_string()],
            write_allow_paths: Vec::new(),
        }),
        network_policy: None,
        steps: vec![flow_step("step_1", "shell", "echo")],
        cleanup: Vec::new(),
        source_path: None,
    };

    validate_flow(&flow).expect("optional immutable string path is valid");
}

#[test]
fn workspace_binding_compiler_rejects_interpolated_or_mutable_read_allow_paths() {
    for path in ["/tmp/{{inputs.config_path}}", "{{vars.config_path}}"] {
        let flow = Flow {
            schema: FLOW_SCHEMA_V1.to_string(),
            id: "invalid-read-path".to_string(),
            version: "v1".to_string(),
            plugins: BTreeMap::new(),
            prerequisites: Default::default(),
            inputs: BTreeMap::from([(
                "config_path".to_string(),
                FlowInputSpec {
                    value_type: "string".to_string(),
                    required: false,
                    default: None,
                    description: None,
                },
            )]),
            workspace: None,
            context: BTreeMap::new(),
            outputs: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            documents: Default::default(),
            profiles: BTreeMap::new(),
            filesystem_policy: Some(FilesystemPolicy {
                workspace_access: None,
                read_allow_paths: vec![path.to_string()],
                write_allow_paths: Vec::new(),
            }),
            network_policy: None,
            steps: vec![flow_step("step_1", "shell", "echo")],
            cleanup: Vec::new(),
            source_path: None,
        };

        validate_flow(&flow).expect_err("non-exact immutable input reference must fail");
    }
}

#[test]
fn compiler_rejects_filesystem_policy_paths_with_surrounding_whitespace() {
    let err = validate_flow(&Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "whitespace-pattern".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(external_grant_policy(&[" /tmp/transist"], &[])),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    })
    .expect_err("surrounding whitespace must be rejected");
    assert!(err.to_string().contains("surrounding whitespace"));
}

#[test]
fn compiler_rejects_absolute_filesystem_policy_pattern() {
    let err = validate_flow(&Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "absolute-pattern".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(external_grant_policy(&["relative/path"], &[])),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    })
    .expect_err("relative external path");
    assert!(err.to_string().contains("absolute"));
}

#[test]
fn compiler_rejects_parent_traversal_filesystem_policy_pattern() {
    let err = validate_flow(&Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "parent-traversal".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(external_grant_policy(&["/etc/../etc/passwd"], &[])),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    })
    .expect_err("parent traversal pattern");
    assert!(err.to_string().contains("parent traversal") || err.to_string().contains(".."));
}

#[test]
fn compiler_rejects_nul_filesystem_policy_pattern() {
    let err = validate_flow(&Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "nul-pattern".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(external_grant_policy(&["/etc/bad\0file"], &[])),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    })
    .expect_err("NUL pattern");
    assert!(err.to_string().contains("NUL"));
}

#[test]
fn compiler_rejects_custom_policy_without_allow_paths() {
    let err = validate_flow(&Flow {
        schema: FLOW_SCHEMA_V1.to_string(),
        id: "empty-custom-policy".to_string(),
        version: "v1".to_string(),
        plugins: BTreeMap::new(),
        prerequisites: Default::default(),
        inputs: BTreeMap::new(),
        workspace: None,
        context: BTreeMap::new(),
        outputs: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        documents: Default::default(),
        profiles: BTreeMap::new(),
        filesystem_policy: None,
        network_policy: None,
        steps: vec![FlowStep {
            filesystem_policy: Some(custom_policy()),
            ..flow_step("step_1", "shell", "echo")
        }],
        cleanup: Vec::new(),
        source_path: None,
    })
    .expect_err("custom policy rejected");
    assert!(
        err.to_string()
            .contains("filesystem_custom_mode_unsupported")
    );
}
