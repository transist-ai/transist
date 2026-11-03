// SPDX-License-Identifier: MPL-2.0
//! End-to-end lifecycle coverage for workflow, run, plugin, and gate flows
//! exercised through the public `TransistContext` surface.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use transist::PluginUninstallError;
use transist::{
    ActiveRunCancellation, GateDecisionInput, RuntimeOperatingMode, TransistContext,
    WorkflowInputBindingError, WorkflowInputBindingErrorKind, repo_local_home_dir,
};
use transist_core::ports::RunStorePort;
use transist_core::{KernelRunStatus, KernelStepStatus, PersistedRunRecord};
use transist_protocol::KernelValue;

fn resolve_run_root(home_dir: &Path, run_id: &str) -> anyhow::Result<std::path::PathBuf> {
    TransistContext::with_home(home_dir, home_dir).run_root(run_id)
}

fn init_repo(repo_root: &Path) -> anyhow::Result<()> {
    transist::init_home_dir_with_mode(
        &repo_local_home_dir(repo_root),
        RuntimeOperatingMode::Standalone,
    )
}

fn wait_for_step_status(ctx: &TransistContext, run_id: &str, step_id: &str, status: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(run) = ctx.runs_show(run_id) {
            let run = Value::from(run);
            if run["steps"][step_id]["status"].as_str() == Some(status) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for step '{}' in run '{}' to reach status '{}'",
            step_id,
            run_id,
            status
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_accepted_cancellation(ctx: &TransistContext, run_id: &str) -> ActiveRunCancellation {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(cancellation)) = ctx.cancel_active_run_execution(run_id)
            && cancellation.accepted
        {
            return cancellation;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for run '{}' to expose an accepted active cancellation",
            run_id
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

fn write_process_plugin_fixture(dir: &Path, manifest: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas dir");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input schema");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output schema");
    fs::write(
        dir.join("plugin.sh"),
        r#"#!/bin/sh
read -r handshake
printf '%s\n' '{"type":"app.handshake.response.v1","protocol_version":"app.v1","plugin":{"id":"gated.shell","version":"1.0.0"},"adapter_kind":"process","capabilities":{"actions":["exec"],"supports_cancel":false,"supports_health":true,"supports_artifact_hints":false,"supports_evidence_hints":false}}'
read -r request
exit 0
"#,
    )
    .expect("script");
    set_executable(&dir.join("plugin.sh"));
    fs::write(
        dir.join("plugin.yaml"),
        manifest.replace(
            "actions:\n",
            "package:\n  files: [plugin.sh, schemas/input.json, schemas/output.json]\nactions:\n",
        ),
    )
    .expect("manifest");
}

fn install_shell_plugin(ctx: &TransistContext) {
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install shell plugin");
}

fn manual_gate_input(reason: &str) -> GateDecisionInput {
    GateDecisionInput {
        actor: Some("reviewer@example.com".to_string()),
        actor_source: Some("flag".to_string()),
        reason: Some(reason.to_string()),
        decided_at_ms: None,
    }
}

fn approve_gate(ctx: &TransistContext, run_id: &str, gate_id: &str) -> transist::RunReport {
    ctx.approve_gate_with_input(run_id, gate_id, manual_gate_input("approved"))
        .expect("approve")
}

fn approve_current_gate(ctx: &TransistContext, run_id: &str) -> transist::RunReport {
    ctx.approve_current_gate_with_input(run_id, manual_gate_input("approved"))
        .expect("approve")
}

fn request_changes_gate(
    ctx: &TransistContext,
    run_id: &str,
    gate_id: &str,
    reason: &str,
) -> transist::RunReport {
    ctx.request_changes_gate_with_input(run_id, gate_id, manual_gate_input(reason))
        .expect("request changes")
}

fn request_changes_current_gate(
    ctx: &TransistContext,
    run_id: &str,
    reason: &str,
) -> transist::RunReport {
    ctx.request_changes_current_gate_with_input(run_id, manual_gate_input(reason))
        .expect("request changes")
}

#[test]
fn prepare_run_and_query_use_cases_follow_the_started_run_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: shell_demo
version: v1
inputs:
  jira_ticket:
    type: string
    required: true
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");

    ctx.validate_flow_file(&flow).expect("validate");
    let compiled = ctx.compile_flow_file(&flow).expect("compile");
    assert!(compiled.exists());

    let started = ctx
        .prepare_workflow(
            "shell_demo@v1",
            Some("flow-run"),
            BTreeMap::from([(
                "jira_ticket".to_string(),
                KernelValue::String("JIRA-123".to_string()),
            )]),
        )
        .expect("prepare");
    assert_eq!(started.run_id, "flow-run");
    assert_eq!(started.status, "planned");

    let report = ctx.run_started_run("flow-run").expect("run started");
    assert_eq!(report.status, "success");

    let runs = ctx.runs_list().expect("list runs");
    assert_eq!(runs, vec!["flow-run".to_string()]);

    let run = ctx.runs_show("flow-run").expect("show run");
    let run_json = Value::from(run);
    assert_eq!(run_json["run_id"], "flow-run");
    assert_eq!(run_json["workflow_inputs"]["jira_ticket"], "JIRA-123");
    assert!(
        repo_local_home_dir(tmp.path())
            .join("runs/run_flow-run/context/run_inputs.json")
            .exists()
    );
}

#[test]
fn prepare_workflow_accepts_declared_prompt_inputs_for_prompt_aware_steps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/codex.cli");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("prompt.workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: prompt_demo
version: v1
plugins:
  codex.cli: 0.1.0
inputs:
  prompt:
    type: string
    required: true
steps:
  - id: ask
    uses: process:codex.cli/exec_task
    prompt: "{{inputs.prompt}}"
"#,
    )
    .expect("flow");

    ctx.validate_flow_file(&flow).expect("validate");
    ctx.compile_flow_file(&flow).expect("compile");
    let started = ctx
        .prepare_workflow(
            "prompt_demo@v1",
            Some("prompt-run"),
            BTreeMap::from([(
                "prompt".to_string(),
                KernelValue::String("Say hello".to_string()),
            )]),
        )
        .expect("prepare");
    assert_eq!(started.run_id, "prompt-run");
    assert_eq!(started.status, "planned");

    let run = ctx.runs_show("prompt-run").expect("show run");
    let run_json = Value::from(run);
    assert_eq!(run_json["workflow_inputs"]["prompt"], "Say hello");
}

#[test]
fn prepare_workflow_reports_structured_input_binding_errors_before_run_creation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: binding_demo
version: v1
inputs:
  repo_path:
    type: string
    required: true
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let error = ctx
        .prepare_workflow(
            "binding_demo@v1",
            Some("binding-demo"),
            BTreeMap::from([(
                "worktree_root".to_string(),
                KernelValue::String("/tmp/worktrees".to_string()),
            )]),
        )
        .expect_err("prepare should fail");

    let binding_error = error
        .downcast_ref::<WorkflowInputBindingError>()
        .expect("workflow input binding error");
    assert_eq!(binding_error.workflow_id, "binding_demo");
    assert_eq!(binding_error.workflow_version, "v1");
    assert_eq!(
        binding_error.kind,
        WorkflowInputBindingErrorKind::UndeclaredInput {
            input_name: "worktree_root".to_string(),
        }
    );
    assert_eq!(binding_error.declared_inputs, vec!["repo_path".to_string()]);
    assert!(
        binding_error
            .recipe_path
            .ends_with("registry/recipes/binding_demo-v1.yaml")
    );
}

#[test]
fn prepare_workflow_rejects_deterministic_workflow_prerequisites_before_run_birth() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: preflight_blocker_demo
version: v1
plugins:
  shell: 0.1.0
inputs:
  repo_path:
    type: string
    required: true
prerequisites:
  paths:
    - path: .missing-workflow-prereq
      kind: file
      required: true
steps:
  - id: invoke
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "echo blocked"]
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let error = ctx
        .prepare_workflow(
            "preflight_blocker_demo@v1",
            Some("preflight-blocker-run"),
            BTreeMap::from([(
                "repo_path".to_string(),
                KernelValue::String("/workspace/demo".to_string()),
            )]),
        )
        .expect_err("deterministic prerequisite must block preparation");
    assert!(error.to_string().contains("pre-dispatch blockers"));
    assert!(
        error.to_string().contains("--input 'repo_path=<value>'"),
        "the diagnostic must not suggest an unbound check for a run prepared with inputs: {error:#}"
    );
    assert!(ctx.runs_list().expect("runs list").is_empty());
    assert!(resolve_run_root(&repo_local_home_dir(tmp.path()), "preflight-blocker-run").is_err());
}

#[test]
fn prepare_workflow_reports_workflow_prerequisite_blockers_without_a_run() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);

    let flow = tmp.path().join("workflow-prereq.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: workflow_prereq_demo
version: v1
plugins:
  shell: 0.1.0
prerequisites:
  paths:
    - path: .workflow-ready
      kind: file
      required: true
steps:
  - id: invoke
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "echo should-not-run"]
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let validation = ctx
        .validate_bound_workflow("workflow_prereq_demo@v1", BTreeMap::new(), false, false)
        .expect("bound validation");
    assert_eq!(validation.status.as_str(), "failed");
    assert!(validation.checks.iter().any(|check| {
        check.code == "workflow_prerequisite.path" && check.status.as_str() == "failed"
    }));
    let error = ctx
        .prepare_workflow(
            "workflow_prereq_demo@v1",
            Some("workflow-prereq-run"),
            BTreeMap::new(),
        )
        .expect_err("deterministic prerequisite must block preparation");
    assert!(error.to_string().contains("pre-dispatch blockers"));
    assert!(ctx.runs_list().expect("runs list").is_empty());
}

#[test]
fn prepare_workflow_rejects_legacy_executable_prerequisites_before_dispatch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);
    let sentinel = tmp.path().join("legacy-prerequisite-ran");

    let flow = tmp.path().join("legacy-prereq.yaml");
    fs::write(
        &flow,
        format!(
            r#"
schema: transist.flow.v1
id: legacy_prereq_demo
version: v1
plugins:
  shell: 0.1.0
prerequisites:
  commands:
    - argv: ["sh", "-c", "touch {sentinel}"]
      required: true
steps:
  - id: invoke
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "touch should-not-run"]
"#,
            sentinel = sentinel.display()
        ),
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let validation = ctx
        .validate_bound_workflow("legacy_prereq_demo@v1", BTreeMap::new(), false, false)
        .expect("bound validation");
    assert!(validation.checks.iter().any(|check| {
        check.code.starts_with("workflow_prerequisite")
            && check.status.as_str() == "failed"
            && check
                .message
                .contains("legacy_executable_prerequisite_unsupported")
    }));
    let error = ctx
        .prepare_workflow(
            "legacy_prereq_demo@v1",
            Some("legacy-prereq-run"),
            BTreeMap::new(),
        )
        .expect_err("legacy prerequisite must block preparation");
    assert!(error.to_string().contains("pre-dispatch blockers"));
    assert!(!sentinel.exists());
    assert!(ctx.runs_list().expect("runs list").is_empty());
}

#[test]
fn prepare_workflow_reports_action_prerequisite_blockers_before_adapter_handshake() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let plugin_dir = tmp.path().join("gated.shell");
    write_process_plugin_fixture(
        &plugin_dir,
        r#"
schema: transist.plugin.v1
id: gated.shell
version: 1.0.0
adapter: process
entrypoint:
  type: process
  command: plugin.sh
actions:
  - name: exec
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    prerequisites:
      paths:
        - path: .missing-action-tool
          kind: file
          required: true
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    );
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("action-prereq.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: action_prereq_demo
version: v1
plugins:
  gated.shell: 1.0.0
steps:
  - id: invoke
    uses: process:gated.shell/exec
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let validation = ctx
        .validate_bound_workflow("action_prereq_demo@v1", BTreeMap::new(), false, false)
        .expect("bound validation");
    assert!(
        validation.checks.iter().any(|check| {
            check.code == "action_prerequisite" && check.status.as_str() == "failed"
        })
    );
    let error = ctx
        .prepare_workflow(
            "action_prereq_demo@v1",
            Some("action-prereq-run"),
            BTreeMap::new(),
        )
        .expect_err("action prerequisite must block preparation");
    assert!(error.to_string().contains("pre-dispatch blockers"));
    assert!(ctx.runs_list().expect("runs list").is_empty());
}

#[test]
fn prepare_workflow_surfaces_nested_subflow_preflight_blockers_at_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let plugin_dir = tmp.path().join("gated.shell");
    write_process_plugin_fixture(
        &plugin_dir,
        r#"
schema: transist.plugin.v1
id: gated.shell
version: 1.0.0
adapter: process
entrypoint:
  type: process
  command: plugin.sh
actions:
  - name: exec
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    );
    fs::write(
        plugin_dir.join("schemas/input.json"),
        r#"{
  "type": "object",
  "properties": {
    "model": { "type": "string" }
  },
  "required": ["model"]
}"#,
    )
    .expect("input schema");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install plugin");

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_nested_preflight_demo
version: v1
plugins:
  gated.shell: 1.0.0
inputs:
  model:
    type: string
steps:
  - id: generate_plan
    uses: process:gated.shell/exec
    with:
      model: "{{inputs.model}}"
"#,
    )
    .expect("child flow");

    let parent = tmp.path().join("parent.yaml");
    fs::write(
        &parent,
        r#"
schema: transist.flow.v1
id: parent_nested_preflight_demo
version: v1
plugins:
  gated.shell: 1.0.0
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: plan_loop
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&parent).expect("compile");
    let validation = ctx
        .validate_bound_workflow(
            "parent_nested_preflight_demo@v1",
            BTreeMap::new(),
            false,
            false,
        )
        .expect("bound validation");
    assert!(validation.checks.iter().any(|check| {
        check.code == "input_schema"
            && check.status.as_str() == "failed"
            && check.step_path.as_deref() == Some("plan_loop/generate_plan")
            && check.message.contains("instance path '/model'")
    }));
    let error = ctx
        .prepare_workflow(
            "parent_nested_preflight_demo@v1",
            Some("nested-preflight-root"),
            BTreeMap::new(),
        )
        .expect_err("nested deterministic blocker must reject preparation");
    assert!(error.to_string().contains("pre-dispatch blockers"));
    assert!(ctx.runs_list().expect("runs list").is_empty());
    assert!(ctx.runs_show("nested-preflight-root").is_err());
    assert!(ctx.runs_show("nested-preflight-root__plan_loop").is_err());
}

#[test]
fn launch_workflow_executes_prepared_run_when_preflight_passes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: launch_demo
version: v1
steps:
  - id: noop
    control: control.noop
    with: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let report = ctx
        .launch_workflow("launch_demo@v1", Some("launch-run"), BTreeMap::new())
        .expect("launch");
    assert_eq!(report.run_id, "launch-run");
    assert_eq!(report.status, "success");

    let run = Value::from(ctx.runs_show("launch-run").expect("show run"));
    assert_eq!(run["status"], "success");
    assert_eq!(run["preflight"]["status"], "passed");
}

#[test]
fn gate_decision_use_cases_resume_blocked_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: approvals_demo
version: v1
steps:
  - id: approval
    control: control.gate.manual
    with:
      gate_id: review
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("approvals_demo@v1", Some("approval-run"), BTreeMap::new())
        .expect("prepare");

    let blocked = ctx.run_started_run("approval-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let approved = approve_gate(&ctx, "approval-run", "review");
    assert_eq!(approved.status, "success");
}

#[test]
fn gate_decision_denies_when_persisted_decide_capability_is_removed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: approvals_tamper_demo
version: v1
steps:
  - id: approval
    control: control.gate.manual
    with:
      gate_id: review
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "approvals_tamper_demo@v1",
        Some("approval-tamper-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx.run_started_run("approval-tamper-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    // Remove the persisted 'decide' approval capability for the gate step
    // from the run-scoped recipe.yaml, simulating a corrupted/removed
    // persisted authority. The decision must be denied through the
    // application API rather than falling back to a freshly regenerated
    // capability request.
    let run_root =
        resolve_run_root(ctx.home_dir(), "approval-tamper-run").expect("resolve run root");
    let recipe_path = run_root.join("recipe.yaml");
    let recipe_yaml = fs::read_to_string(&recipe_path).expect("read recipe.yaml");
    let mut recipe_value: yaml_serde::Value =
        yaml_serde::from_str(&recipe_yaml).expect("parse recipe.yaml");
    recipe_value["capabilities"]["approval"] = yaml_serde::Value::Sequence(Vec::new());
    fs::write(
        &recipe_path,
        yaml_serde::to_string(&recipe_value).expect("serialize recipe.yaml"),
    )
    .expect("write tampered recipe.yaml");

    let err = ctx
        .approve_gate_with_input(
            "approval-tamper-run",
            "review",
            manual_gate_input("approved"),
        )
        .expect_err("decision must be denied when persisted decide authority is missing");
    let message = err.to_string();
    let full_chain = err
        .chain()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        full_chain.contains("persisted authority") || message.contains("approval decision denied"),
        "expected a persisted-authority denial, got: {full_chain}"
    );
}

#[test]
fn gate_decision_denies_when_persisted_authority_is_request_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: approvals_request_only_demo
version: v1
steps:
  - id: approval
    control: control.gate.manual
    with:
      gate_id: review
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "approvals_request_only_demo@v1",
        Some("approval-request-only-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx
        .run_started_run("approval-request-only-run")
        .expect("run");
    assert_eq!(blocked.status, "blocked");

    // Replace the persisted approval capability with a request-only entry
    // (no "decide" operation). A decide attempt must still be denied even
    // though a persisted approval capability exists for this step.
    let run_root =
        resolve_run_root(ctx.home_dir(), "approval-request-only-run").expect("resolve run root");
    let recipe_path = run_root.join("recipe.yaml");
    let recipe_yaml = fs::read_to_string(&recipe_path).expect("read recipe.yaml");
    let mut recipe_value: yaml_serde::Value =
        yaml_serde::from_str(&recipe_yaml).expect("parse recipe.yaml");
    recipe_value["capabilities"]["approval"] = yaml_serde::from_str(
        r#"
- capability: approval
  resource: approvals_request_only_demo/review
  operations: ["request"]
"#,
    )
    .expect("request-only capability fixture");
    fs::write(
        &recipe_path,
        yaml_serde::to_string(&recipe_value).expect("serialize recipe.yaml"),
    )
    .expect("write tampered recipe.yaml");

    let err = ctx
        .approve_gate_with_input(
            "approval-request-only-run",
            "review",
            manual_gate_input("approved"),
        )
        .expect_err("decision must be denied when persisted authority is request-only");
    let full_chain = err
        .chain()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        full_chain.contains("persisted authority"),
        "expected a persisted-authority denial, got: {full_chain}"
    );
}

#[test]
fn gate_decisions_on_parent_runs_route_into_blocked_subflows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_demo
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("parent_demo@v1", Some("parent-run"), BTreeMap::new())
        .expect("prepare");

    let blocked = ctx.run_started_run("parent-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let approved = approve_gate(&ctx, "parent-run", "review");
    assert_eq!(approved.run_id, "parent-run");
    assert_eq!(approved.status, "success");
}

#[test]
fn inferred_gate_decisions_on_parent_runs_route_into_blocked_subflows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_demo
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("parent_demo@v1", Some("parent-run"), BTreeMap::new())
        .expect("prepare");

    let blocked = ctx.run_started_run("parent-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let approved = approve_current_gate(&ctx, "parent-run");
    assert_eq!(approved.run_id, "parent-run");
    assert_eq!(approved.status, "success");
}

#[test]
fn gate_decisions_on_child_runs_reenter_the_containing_root_run() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_demo
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("parent_demo@v1", Some("parent-run"), BTreeMap::new())
        .expect("prepare");

    let blocked = ctx.run_started_run("parent-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let approved = approve_gate(&ctx, "parent-run__child", "review");
    assert_eq!(approved.run_id, "parent-run");
    assert_eq!(approved.status, "success");
}

#[test]
fn gate_decisions_on_child_runs_do_not_reenter_newly_blocked_descendants_twice() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_demo
version: v1
steps:
  - id: review_one
    control: control.gate.manual
    with:
      gate_id: review-one
  - id: review_two
    control: control.gate.manual
    with:
      gate_id: review-two
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("parent_demo@v1", Some("parent-run"), BTreeMap::new())
        .expect("prepare");

    let blocked = ctx.run_started_run("parent-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let approved = approve_gate(&ctx, "parent-run__child", "review-one");
    assert_eq!(approved.run_id, "parent-run");
    assert_eq!(approved.status, "blocked");

    let child = Value::from(ctx.runs_show("parent-run__child").expect("show child"));
    assert_eq!(child["status"], "blocked");
    assert_eq!(child["steps"]["review_one"]["status"], "succeeded");
    assert_eq!(child["steps"]["review_one"]["executions"], 2);
    assert_eq!(child["steps"]["review_two"]["status"], "blocked");
    assert_eq!(child["steps"]["review_two"]["executions"], 1);
}

#[test]
fn rerunning_parent_recovers_stale_child_subflow_before_gate_approval() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_demo
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("parent_demo@v1", Some("parent-run"), BTreeMap::new())
        .expect("prepare");

    let blocked = ctx.run_started_run("parent-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let home_dir = repo_local_home_dir(tmp.path());
    let child_state_path = resolve_run_root(&home_dir, "parent-run__child")
        .expect("child run root")
        .join("state.json");
    let mut child_record: PersistedRunRecord =
        serde_json::from_str(&fs::read_to_string(&child_state_path).expect("child state"))
            .expect("child record");
    child_record.status = KernelRunStatus::Running;
    if let Some(step) = child_record.steps.get_mut("review") {
        step.status = KernelStepStatus::Pending;
        step.started_at_ms = None;
        step.finished_at_ms = None;
        step.last_request = None;
        step.last_result = None;
    }
    child_record.refresh_executions();
    fs::write(
        &child_state_path,
        serde_json::to_vec_pretty(&child_record).expect("serialize child record"),
    )
    .expect("write child state");

    let rerun = ctx.run_started_run("parent-run").expect("rerun");
    assert_eq!(rerun.status, "blocked");

    let approved = approve_gate(&ctx, "parent-run", "review");
    assert_eq!(approved.run_id, "parent-run");
    assert_eq!(approved.status, "success");
}

#[test]
fn resuming_root_run_recovers_failed_child_subflow_after_fix() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_resume_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: verify_marker
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "test -f .resume-ok"]
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_resume_demo
version: v1
plugins:
  shell: 0.1.0
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("parent_resume_demo@v1", Some("parent-run"), BTreeMap::new())
        .expect("prepare");

    let report = ctx.run_started_run("parent-run").expect("run started");
    assert_eq!(report.status, "failed");

    fs::write(tmp.path().join(".resume-ok"), "").expect("marker");

    let resumed = ctx.runs_resume("parent-run").expect("resume");
    assert_eq!(resumed.run_id, "parent-run");
    assert_eq!(resumed.status, "success");

    let child = ctx.runs_show("parent-run__child").expect("show child");
    let child_json = Value::from(child);
    assert_eq!(child_json["resume_count"], 1);
}

#[test]
fn subflow_child_defaults_bind_before_run_creation_and_invalid_mappings_fail_parent_step() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_defaults_demo
version: v1
inputs:
  required_ticket:
    type: string
    required: true
  target_branch:
    type: string
    default: main
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let parent = tmp.path().join("parent.yaml");
    fs::write(
        &parent,
        r#"
schema: transist.flow.v1
id: parent_defaults_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
inputs:
  ticket:
    type: string
    required: true
steps:
  - id: child
    module:
      ref: child
      inputs:
        required_ticket: "{{inputs.ticket}}"
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&parent)
        .expect("compile success parent");
    ctx.prepare_workflow(
        "parent_defaults_demo@v1",
        Some("parent-defaults-run"),
        BTreeMap::from([(
            "ticket".to_string(),
            KernelValue::String("ISSUE-289".to_string()),
        )]),
    )
    .expect("prepare");

    let report = ctx.run_started_run("parent-defaults-run").expect("run");
    assert_eq!(report.status, "success");

    let parent_run = Value::from(ctx.runs_show("parent-defaults-run").expect("show parent"));
    assert_eq!(
        parent_run["steps"]["child"]["last_result"]["output"]["bound_inputs"]["required_ticket"],
        "ISSUE-289"
    );
    assert_eq!(
        parent_run["steps"]["child"]["last_result"]["output"]["bound_inputs"]["target_branch"],
        "main"
    );

    let child_run_inputs_path = resolve_run_root(
        &repo_local_home_dir(tmp.path()),
        "parent-defaults-run__child",
    )
    .expect("child run root")
    .join("context/run_inputs.json");
    let child_run_inputs: Value = serde_json::from_str(
        &fs::read_to_string(&child_run_inputs_path).expect("read child run inputs"),
    )
    .expect("parse child run inputs");
    assert_eq!(child_run_inputs["values"]["required_ticket"], "ISSUE-289");
    assert_eq!(child_run_inputs["values"]["target_branch"], "main");
}

#[test]
fn run_started_run_rejects_failed_runs_and_points_to_resume() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: failed_reentry_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: verify_marker
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "test -f .resume-ok"]
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "failed_reentry_demo@v1",
        Some("failed-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let failed = ctx.run_started_run("failed-run").expect("run");
    assert_eq!(failed.status, "failed");

    let error = ctx
        .run_started_run("failed-run")
        .expect_err("reject failed run");
    assert!(
        error
            .to_string()
            .contains("use 'transist runs resume failed-run' instead"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn approve_on_loop_gate_reblocks_when_safe_exit_check_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: approved_loop_retry_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: review_loop
    control: control.noop
    loop:
      body:
        - ref: review
        - ref: check_exit
      until: 'steps.review.out.decision == "approved" && steps.check_exit.result.status == "success"'
      max_iters: 4
      max_duration: 1h
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: check_exit
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "exit 1"]
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "approved_loop_retry_demo@v1",
        Some("approved-loop-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx.run_started_run("approved-loop-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let approved = approve_gate(&ctx, "approved-loop-run", "review");
    assert_eq!(approved.status, "blocked");

    let root = Value::from(ctx.runs_show("approved-loop-run").expect("show root"));
    assert_eq!(root["status"], "blocked");
    assert_eq!(root["steps"]["review"]["status"], "blocked");
    assert_eq!(root["steps"]["review"]["executions"], 3);
    assert_eq!(
        root["steps"]["review"]["last_result"]["output"]["decision"],
        Value::Null
    );
}

#[test]
fn approve_on_nested_loop_gate_reblocks_root_when_safe_exit_check_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: approved_loop_retry_child
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
  - id: check_exit
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "exit 1"]
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: approved_loop_retry_parent
version: v1
plugins:
  shell: 0.1.0
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: review_loop
    control: control.noop
    loop:
      body:
        - ref: review_cycle
      until: 'steps.review_cycle.out.child_steps.review.out.decision == "approved" && steps.review_cycle.out.child_steps.check_exit.result.status == "success"'
      max_iters: 4
      max_duration: 1h
  - id: review_cycle
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "approved_loop_retry_parent@v1",
        Some("approved-nested-loop-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx
        .run_started_run("approved-nested-loop-run")
        .expect("run");
    assert_eq!(blocked.status, "blocked");

    let approved = approve_gate(&ctx, "approved-nested-loop-run", "review");
    assert_eq!(approved.status, "blocked");

    let root = Value::from(
        ctx.runs_show("approved-nested-loop-run")
            .expect("show root"),
    );
    assert_eq!(root["status"], "blocked");
    assert_eq!(root["steps"]["review_cycle"]["status"], "blocked");

    let child = Value::from(
        ctx.runs_show("approved-nested-loop-run__review_cycle")
            .expect("show child"),
    );
    assert_eq!(child["status"], "blocked");
    assert_eq!(child["steps"]["review"]["status"], "blocked");
    assert_eq!(child["steps"]["review"]["executions"], 3);
    assert_eq!(
        child["steps"]["review"]["last_result"]["output"]["decision"],
        Value::Null
    );
}

#[test]
fn cancel_active_run_execution_cancels_running_process_step() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: cancel_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: wait_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "sleep 5"]
"#,
    )
    .expect("flow");

    ctx.validate_flow_file(&flow).expect("validate");
    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("cancel_demo@v1", Some("cancel-run"), BTreeMap::new())
        .expect("prepare");

    let ctx_clone = ctx.clone();
    let handle = thread::spawn(move || ctx_clone.run_started_run("cancel-run"));
    wait_for_step_status(&ctx, "cancel-run", "wait_step", "running");

    let cancellation = wait_for_accepted_cancellation(&ctx, "cancel-run");
    assert_eq!(cancellation.run_id, "cancel-run");
    assert_eq!(cancellation.step_id, "wait_step");
    assert!(cancellation.accepted);

    let report = handle
        .join()
        .expect("run thread join")
        .expect("run report after cancel");
    assert_eq!(report.status, "canceled");

    let run = Value::from(ctx.runs_show("cancel-run").expect("show canceled run"));
    assert_eq!(run["status"], "canceled");
    assert_eq!(run["steps"]["wait_step"]["status"], "canceled");
}

#[test]
fn cancel_active_run_execution_reaches_running_child_subflow_step() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_cancel_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: wait_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "sleep 5"]
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_cancel_demo
version: v1
plugins:
  shell: 0.1.0
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: child
    module:
      ref: child
      inputs: {}
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "parent_cancel_demo@v1",
        Some("parent-cancel-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let ctx_clone = ctx.clone();
    let handle = thread::spawn(move || ctx_clone.run_started_run("parent-cancel-run"));
    wait_for_step_status(&ctx, "parent-cancel-run__child", "wait_step", "running");

    let cancellation = wait_for_accepted_cancellation(&ctx, "parent-cancel-run");
    assert_eq!(cancellation.run_id, "parent-cancel-run__child");
    assert_eq!(cancellation.step_id, "wait_step");
    assert!(cancellation.accepted);

    let report = handle
        .join()
        .expect("run thread join")
        .expect("run report after child cancel");
    assert_eq!(report.status, "canceled");

    let parent = Value::from(ctx.runs_show("parent-cancel-run").expect("show parent run"));
    assert_eq!(parent["status"], "canceled");
    assert_eq!(parent["steps"]["child"]["status"], "canceled");

    let child = Value::from(
        ctx.runs_show("parent-cancel-run__child")
            .expect("show child run"),
    );
    assert_eq!(child["status"], "canceled");
    assert_eq!(child["steps"]["wait_step"]["status"], "canceled");
}

#[test]
fn resume_after_cancel_reruns_the_canceled_step_and_keeps_prior_step_results() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let sentinel = tmp.path().join("first_step.flag");
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        format!(
            r#"
schema: transist.flow.v1
id: cancel_resume_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: first_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "echo done > {sentinel}"]
  - id: wait_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "sleep 8"]
"#,
            sentinel = sentinel.display()
        ),
    )
    .expect("flow");

    ctx.validate_flow_file(&flow).expect("validate");
    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "cancel_resume_demo@v1",
        Some("cancel-resume-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let ctx_clone = ctx.clone();
    let handle = thread::spawn(move || ctx_clone.run_started_run("cancel-resume-run"));
    wait_for_step_status(&ctx, "cancel-resume-run", "wait_step", "running");

    let cancellation = wait_for_accepted_cancellation(&ctx, "cancel-resume-run");
    assert!(cancellation.accepted);
    let first_report = handle
        .join()
        .expect("run thread join")
        .expect("run report after cancel");
    assert_eq!(first_report.status, "canceled");

    let canceled = Value::from(ctx.runs_show("cancel-resume-run").expect("show canceled"));
    assert_eq!(canceled["status"], "canceled");
    assert_eq!(canceled["steps"]["first_step"]["status"], "succeeded");
    assert_eq!(canceled["steps"]["wait_step"]["status"], "canceled");
    let first_finished_at_ms = canceled["steps"]["first_step"]["finished_at_ms"].clone();
    assert!(first_finished_at_ms.as_u64().is_some());

    // Remove the sentinel so we can prove first_step was NOT re-executed.
    fs::remove_file(&sentinel).expect("remove sentinel");

    // Replace the sleep with an instant success so the resume path completes
    // quickly. We rewrite the on-disk workflow file; the kernel uses the
    // persisted recipe snapshot for the canceled run, but the resumed step
    // will be dispatched with the persisted ExecutionRequest from the first
    // attempt, so timing is what matters here, not the workflow source.
    // (We do not modify the persisted recipe; the resumed sleep will simply
    // run to completion since it sleeps for 8s.)

    let resumed = ctx
        .runs_resume("cancel-resume-run")
        .expect("resume canceled");
    assert_eq!(resumed.status, "success");

    let after = Value::from(
        ctx.runs_show("cancel-resume-run")
            .expect("show resumed run"),
    );
    assert_eq!(after["status"], "success");
    // first_step preserved untouched.
    assert_eq!(after["steps"]["first_step"]["status"], "succeeded");
    assert_eq!(
        after["steps"]["first_step"]["finished_at_ms"], first_finished_at_ms,
        "first_step result must be untouched across resume"
    );
    assert_eq!(after["steps"]["first_step"]["executions"], 1);
    // The sentinel must NOT be recreated -> first_step did not re-execute.
    assert!(!sentinel.exists(), "first_step must not be re-executed");

    // wait_step rerun: a new attempt is appended.
    assert_eq!(after["steps"]["wait_step"]["status"], "succeeded");
    assert_eq!(after["steps"]["wait_step"]["executions"], 2);
    assert!(after["resume_count"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn request_changes_on_loop_gate_reenters_same_loop_instead_of_advancing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_loop_demo
version: v1
steps:
  - id: plan_loop
    control: control.noop
    loop:
      body:
        - ref: review_plan
      until: 'steps.review_plan.out.decision == "approved"'
      max_iters: 4
      max_duration: 1h
  - id: review_plan
    control: control.gate.manual
    with:
      gate_id: plan-review
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_loop_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: plan_loop
    module:
      ref: child
      inputs: {}
  - id: implementation_loop
    control: control.noop
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "parent_loop_demo@v1",
        Some("parent-loop-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx.run_started_run("parent-loop-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let changed = request_changes_gate(
        &ctx,
        "parent-loop-run",
        "plan-review",
        "answer the questions",
    );
    assert_eq!(changed.status, "blocked");

    let parent = Value::from(ctx.runs_show("parent-loop-run").expect("show parent"));
    assert_eq!(parent["status"], "blocked");
    assert_eq!(parent["steps"]["plan_loop"]["status"], "blocked");
    assert_eq!(parent["steps"]["implementation_loop"]["status"], "pending");

    let child = Value::from(
        ctx.runs_show("parent-loop-run__plan_loop")
            .expect("show child"),
    );
    assert_eq!(child["status"], "blocked");
    assert_eq!(child["steps"]["review_plan"]["status"], "blocked");
    assert_eq!(child["steps"]["review_plan"]["executions"], 2);
    assert_eq!(
        child["steps"]["review_plan"]["last_result"]["output"]["decision"],
        Value::Null
    );
}

#[test]
fn request_changes_on_multi_step_loop_replays_the_llm_step_before_reblocking() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_loop_demo
version: v1
steps:
  - id: plan_loop
    control: control.noop
    loop:
      body:
        - ref: generate_plan
        - ref: review_plan
      until: 'steps.review_plan.out.decision == "approved"'
      max_iters: 4
      max_duration: 1h
  - id: generate_plan
    control: control.noop
  - id: review_plan
    control: control.gate.manual
    with:
      gate_id: plan-review
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_loop_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: plan_loop
    module:
      ref: child
      inputs: {}
  - id: implementation_loop
    control: control.noop
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "parent_loop_demo@v1",
        Some("parent-multistep-loop-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx
        .run_started_run("parent-multistep-loop-run")
        .expect("run");
    assert_eq!(blocked.status, "blocked");

    let changed = request_changes_gate(
        &ctx,
        "parent-multistep-loop-run",
        "plan-review",
        "answer the questions",
    );
    assert_eq!(changed.status, "blocked");

    let child = Value::from(
        ctx.runs_show("parent-multistep-loop-run__plan_loop")
            .expect("show child"),
    );
    assert_eq!(child["status"], "blocked");
    assert_eq!(child["steps"]["review_plan"]["status"], "blocked");
    assert_eq!(child["steps"]["generate_plan"]["executions"], 2);
    assert_eq!(child["steps"]["review_plan"]["executions"], 2);
    assert_eq!(
        child["steps"]["review_plan"]["last_result"]["output"]["decision"],
        Value::Null
    );
}

#[test]
fn request_changes_can_rerun_a_completed_child_subflow_inside_a_parent_loop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_review_cycle
version: v1
steps:
  - id: review_cycle_decision
    control: control.gate.manual
    with:
      gate_id: review-cycle-decision
  - id: maybe_refresh_review_state
    control: control.noop
    if:
      guard: 'steps.review_cycle_decision.out.decision == "request_changes"'
      then:
        - ref: inspect_pr_state
  - id: inspect_pr_state
    control: control.noop
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_review_loop_demo
version: v1
modules:
  child:
    path: ./child/workflow.yaml
steps:
  - id: review_loop
    control: control.noop
    loop:
      body:
        - ref: review_cycle
      until: 'steps.review_cycle.out.child_steps.review_cycle_decision.out.decision == "approved"'
      max_iters: 4
      max_duration: 1h
  - id: review_cycle
    module:
      ref: child
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "parent_review_loop_demo@v1",
        Some("parent-review-loop-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx.run_started_run("parent-review-loop-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let changed = request_changes_gate(
        &ctx,
        "parent-review-loop-run",
        "review-cycle-decision",
        "fetch the new review state",
    );
    assert_eq!(changed.status, "blocked");

    let child = Value::from(
        ctx.runs_show("parent-review-loop-run__review_cycle")
            .expect("show child"),
    );
    assert_eq!(child["status"], "blocked");
    assert_eq!(child["steps"]["review_cycle_decision"]["status"], "blocked");
    // The child subflow is rerun under the same run id, so the gate execution
    // counter is append-only: initial block (1), request_changes consumption
    // (2), and the loop-driven rerun that re-blocks waiting for the next
    // decision (3). Prior execution records remain inspectable.
    assert_eq!(child["steps"]["review_cycle_decision"]["executions"], 3);
}

#[test]
fn request_changes_current_gate_reruns_completed_nested_subflow_inside_review_cycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    let changes_dir = child_dir.join("changes");
    fs::create_dir_all(&changes_dir).expect("changes dir");
    fs::write(
        changes_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: review_changes_demo
version: v1
steps:
  - id: review_plan
    control: control.gate.manual
    with:
      gate_id: review-plan
  - id: finish
    control: control.finish
"#,
    )
    .expect("changes flow");

    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_review_cycle
version: v1
modules:
  review_changes:
    path: changes/workflow.yaml
steps:
  - id: review_cycle_decision
    control: control.gate.manual
    with:
      gate_id: review-cycle-decision
  - id: maybe_review_changes
    control: control.noop
    if:
      guard: 'steps.review_cycle_decision.out.decision == "request_changes"'
      then:
        - ref: review_changes_flow
  - id: review_changes_flow
    module:
      ref: review_changes
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_review_loop_demo_nested
version: v1
modules:
  review_cycle:
    path: ./child/workflow.yaml
steps:
  - id: review_loop
    control: control.noop
    loop:
      body:
        - ref: review_cycle
      until: 'steps.review_cycle.out.child_steps.review_cycle_decision.out.decision == "approved"'
      max_iters: 4
      max_duration: 1h
  - id: review_cycle
    module:
      ref: review_cycle
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "parent_review_loop_demo_nested@v1",
        Some("parent-review-loop-nested-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx
        .run_started_run("parent-review-loop-nested-run")
        .expect("run");
    assert_eq!(blocked.status, "blocked");

    let changed = request_changes_current_gate(
        &ctx,
        "parent-review-loop-nested-run",
        "run one review cycle",
    );
    assert_eq!(changed.status, "blocked");

    let nested = Value::from(
        ctx.runs_show("parent-review-loop-nested-run__review_cycle__review_changes_flow")
            .expect("show nested"),
    );
    assert_eq!(nested["status"], "blocked");
    assert_eq!(nested["steps"]["review_plan"]["status"], "blocked");
    assert_eq!(nested["steps"]["review_plan"]["executions"], 1);

    let approved = approve_gate(&ctx, "parent-review-loop-nested-run", "review-plan");
    assert_eq!(approved.status, "blocked");

    let top = Value::from(
        ctx.runs_show("parent-review-loop-nested-run__review_cycle")
            .expect("show child"),
    );
    assert_eq!(top["status"], "blocked");
    assert_eq!(top["steps"]["review_cycle_decision"]["status"], "blocked");

    let changed_again = request_changes_gate(
        &ctx,
        "parent-review-loop-nested-run",
        "review-cycle-decision",
        "rerun nested review changes",
    );
    assert_eq!(changed_again.status, "blocked");

    let nested_again = Value::from(
        ctx.runs_show("parent-review-loop-nested-run__review_cycle__review_changes_flow")
            .expect("show nested again"),
    );
    assert_eq!(nested_again["status"], "blocked");
    assert_eq!(nested_again["steps"]["review_plan"]["status"], "blocked");
    // The nested review-changes subflow is rerun under the same run id each
    // time the parent review cycle re-enters it, so the gate execution
    // counter is append-only: initial block (1), approved consumption (2),
    // and the request_changes-driven rerun that re-blocks waiting for the
    // next decision (3). Prior execution records remain inspectable.
    assert_eq!(nested_again["steps"]["review_plan"]["executions"], 3);
}

#[test]
fn request_changes_reason_can_trigger_review_changes_without_review_comments() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let changes_dir = tmp.path().join("changes");
    fs::create_dir_all(&changes_dir).expect("changes dir");
    fs::write(
        changes_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: review_changes_demo_reason
version: v1
steps:
  - id: review_plan
    control: control.gate.manual
    with:
      gate_id: review-plan
  - id: finish
    control: control.finish
"#,
    )
    .expect("changes flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
            &flow,
            r#"
schema: transist.flow.v1
id: review_cycle_reason_demo
version: v1
modules:
  review_changes:
    path: ./changes/workflow.yaml
steps:
  - id: review_cycle_decision
    control: control.gate.manual
    with:
      gate_id: review-cycle-decision
  - id: maybe_review_changes
    control: control.noop
    if:
      guard: 'steps.review_cycle_decision.out.decision == "request_changes" && steps.review_cycle_decision.out.reason != ""'
      then:
        - ref: review_changes_flow
  - id: review_changes_flow
    module:
      ref: review_changes
      inputs: {}
  - id: finish
    control: control.finish
"#,
        )
        .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "review_cycle_reason_demo@v1",
        Some("review-cycle-reason-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx.run_started_run("review-cycle-reason-run").expect("run");
    assert_eq!(blocked.status, "blocked");

    let changed = request_changes_gate(
        &ctx,
        "review-cycle-reason-run",
        "review-cycle-decision",
        "manual review feedback",
    );
    assert_eq!(changed.status, "blocked");

    let root = Value::from(ctx.runs_show("review-cycle-reason-run").expect("show"));
    assert_eq!(root["status"], "blocked");
    assert_eq!(
        root["steps"]["review_cycle_decision"]["last_result"]["output"]["reason"],
        "manual review feedback"
    );
    assert_eq!(root["steps"]["review_changes_flow"]["status"], "blocked");

    let nested = Value::from(
        ctx.runs_show("review-cycle-reason-run__review_changes_flow")
            .expect("show nested"),
    );
    assert_eq!(nested["status"], "blocked");
    assert_eq!(nested["steps"]["review_plan"]["status"], "blocked");
}

#[test]
fn request_changes_with_fresh_review_cycle_rerun_creates_a_new_cycle_child_run() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let child_dir = tmp.path().join("child");
    let changes_dir = child_dir.join("changes");
    fs::create_dir_all(&changes_dir).expect("changes dir");
    fs::write(
        changes_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: review_changes_demo
version: v1
steps:
  - id: review_plan
    control: control.gate.manual
    with:
      gate_id: review-plan
  - id: finish
    control: control.finish
"#,
    )
    .expect("changes flow");

    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.module.v1
id: child_review_cycle_fresh
version: v1
modules:
  review_changes:
    path: changes/workflow.yaml
steps:
  - id: review_cycle_decision
    control: control.gate.manual
    with:
      gate_id: review-cycle-decision
  - id: maybe_review_changes
    control: control.noop
    if:
      guard: 'steps.review_cycle_decision.out.decision == "request_changes"'
      then:
        - ref: review_changes_flow
  - id: review_changes_flow
    module:
      ref: review_changes
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_review_loop_demo_fresh
version: v1
modules:
  review_cycle:
    path: ./child/workflow.yaml
steps:
  - id: review_loop
    control: control.noop
    loop:
      body:
        - ref: review_cycle
      until: 'steps.review_cycle.out.child_steps.review_cycle_decision.out.decision == "approved"'
      max_iters: 4
      max_duration: 1h
  - id: review_cycle
    module:
      ref: review_cycle
      rerun_mode: fresh
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("parent flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "parent_review_loop_demo_fresh@v1",
        Some("parent-review-loop-fresh-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = ctx
        .run_started_run("parent-review-loop-fresh-run")
        .expect("run");
    assert_eq!(blocked.status, "blocked");

    let changed = request_changes_gate(
        &ctx,
        "parent-review-loop-fresh-run",
        "review-cycle-decision",
        "run first review cycle",
    );
    assert_eq!(changed.status, "blocked");

    let first_nested = Value::from(
        ctx.runs_show("parent-review-loop-fresh-run__review_cycle__review_changes_flow")
            .expect("show first nested"),
    );
    assert_eq!(first_nested["status"], "blocked");
    assert_eq!(first_nested["steps"]["review_plan"]["status"], "blocked");

    let approved = approve_gate(&ctx, "parent-review-loop-fresh-run", "review-plan");
    assert_eq!(approved.status, "blocked");

    let changed_again = request_changes_gate(
        &ctx,
        "parent-review-loop-fresh-run",
        "review-cycle-decision",
        "run second review cycle",
    );
    assert_eq!(changed_again.status, "blocked");

    let first_cycle = Value::from(
        ctx.runs_show("parent-review-loop-fresh-run__review_cycle")
            .expect("show first cycle"),
    );
    assert_eq!(first_cycle["status"], "success");

    let root = Value::from(
        ctx.runs_show("parent-review-loop-fresh-run")
            .expect("show root"),
    );
    let second_cycle_run_id =
        root["steps"]["review_cycle"]["last_result"]["output"]["child_run_id"]
            .as_str()
            .expect("second cycle child run id");
    assert_ne!(
        second_cycle_run_id,
        "parent-review-loop-fresh-run__review_cycle"
    );

    let second_cycle = Value::from(
        ctx.runs_show(second_cycle_run_id)
            .expect("show second cycle"),
    );
    assert_eq!(second_cycle["status"], "blocked");

    let second_nested_run_id =
        second_cycle["steps"]["review_changes_flow"]["last_result"]["output"]["child_run_id"]
            .as_str()
            .expect("second nested child run id");
    let second_nested = Value::from(
        ctx.runs_show(second_nested_run_id)
            .expect("show second nested"),
    );
    assert_eq!(second_nested["status"], "blocked");
    assert_eq!(second_nested["steps"]["review_plan"]["status"], "blocked");
    assert_eq!(second_nested["steps"]["review_plan"]["executions"], 1);
}

#[test]
fn recipe_and_plugin_use_cases_cover_management_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");

    let installed = ctx
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");
    let plugins = ctx.list_plugins().expect("list plugins");
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].id, "shell");

    let manifests = ctx.inspect_plugin("shell").expect("inspect plugin");
    assert_eq!(manifests.len(), 1);
    assert_eq!(manifests[0].id, "shell");

    ctx.remove_plugin("shell", &installed.installed.version)
        .expect("remove plugin");
    let plugins = ctx.list_plugins().expect("list plugins after remove");
    assert!(plugins.is_empty());
}

#[test]
fn uninstall_plugin_is_blocked_while_prepared_run_references_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    let installed = ctx
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: plugin_guard_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("write flow");
    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "plugin_guard_demo@v1",
        Some("plugin-guard-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let err = ctx
        .remove_plugin("shell", &installed.installed.version)
        .expect_err("plugin removal should be blocked");
    assert!(
        err.to_string()
            .contains("protected runs still reference it")
    );

    let err = ctx
        .uninstall_plugin("shell")
        .expect_err("plugins uninstall should be blocked");
    assert!(
        err.to_string()
            .contains("protected runs still reference it")
    );
}

#[test]
fn uninstall_blocked_by_subflow_step_in_planned_parent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    let installed = ctx
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let child = tmp.path().join("child.yaml");
    fs::write(
        &child,
        r#"
schema: transist.module.v1
id: child_plugin_guard_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: child_invoke
    uses: process:shell/echo
    with:
      args: ["hello from child"]
"#,
    )
    .expect("write child flow");

    let parent = tmp.path().join("parent.yaml");
    fs::write(
        &parent,
        r#"
schema: transist.flow.v1
id: parent_plugin_guard_demo
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
    .expect("write parent flow");

    ctx.compile_flow_file(&parent).expect("compile");
    ctx.prepare_workflow(
        "parent_plugin_guard_demo@v1",
        Some("plugin-guard-subflow-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let err = ctx
        .remove_plugin("shell", &installed.installed.version)
        .expect_err("plugin removal should be blocked by subflow");
    assert!(
        err.to_string()
            .contains("protected runs still reference it")
    );
}

#[test]
fn plugin_uninstall_inuse_error_round_trips_through_anyhow_downcast() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    let installed = ctx
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: plugin_typed_guard_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("write flow");
    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "plugin_typed_guard_demo@v1",
        Some("plugin-typed-guard-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let err = ctx
        .remove_plugin("shell", &installed.installed.version)
        .expect_err("plugin removal should be blocked");
    let typed = err
        .downcast_ref::<PluginUninstallError>()
        .expect("typed plugins uninstall error");
    assert!(matches!(
        typed,
        PluginUninstallError::InUse {
            plugin_id,
            plugin_version,
            run_ids,
        } if plugin_id == "shell"
            && plugin_version.as_deref() == Some(installed.installed.version.as_str())
            && run_ids == &vec!["plugin-typed-guard-run".to_string()]
    ));
}

#[test]
fn uninstall_plugin_succeeds_after_referencing_run_is_successful() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    let installed = ctx
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: plugin_guard_success_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("write flow");
    ctx.compile_flow_file(&flow).expect("compile");
    ctx.launch_workflow(
        "plugin_guard_success_demo@v1",
        Some("plugin-success-run"),
        BTreeMap::new(),
    )
    .expect("run success");

    ctx.remove_plugin("shell", &installed.installed.version)
        .expect("remove plugin after success");
    assert!(ctx.list_plugins().expect("list plugins").is_empty());
}

#[test]
fn inspect_workflow_returns_compiled_recipe_and_summary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install plugin");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: inspect_demo
version: v1
plugins:
  shell: 0.1.0
inputs:
  ticket:
    type: string
    required: true
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("flow");

    let compiled = ctx.compile_flow_file(&flow).expect("compile");
    let inspection = ctx
        .inspect_workflow("inspect_demo@v1")
        .expect("inspect workflow");

    assert_eq!(inspection.id, "inspect_demo");
    assert_eq!(inspection.version, "v1");
    assert_eq!(inspection.path, compiled);
    assert_eq!(inspection.recipe.workflow.id, "inspect_demo");
    assert_eq!(inspection.recipe.workflow.version, "v1");
    assert_eq!(inspection.summary.workflow_id, "inspect_demo");
    assert_eq!(inspection.summary.workflow_version, "v1");
    assert!(inspection.summary.source_ref.is_none());
    assert!(inspection.summary.compiled_recipe_available);
    assert!(inspection.summary.runnable_in_principle);
    assert_eq!(inspection.summary.step_count, 1);
    assert_eq!(inspection.summary.root_step_count, 1);
    assert_eq!(inspection.summary.inputs.len(), 1);
    assert_eq!(inspection.summary.inputs[0].name, "ticket");
    assert_eq!(inspection.summary.inputs[0].value_type, "string");
    assert!(inspection.summary.inputs[0].required);
}

#[test]
fn uninstall_workflow_handles_clean_and_blocked_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install plugin");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: uninstall_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("flow");

    let compiled = ctx.compile_flow_file(&flow).expect("compile clean");
    let removed = ctx
        .uninstall_workflow("uninstall_demo@v1", false)
        .expect("clean uninstall");
    assert_eq!(removed, "uninstall_demo@v1");
    assert!(!compiled.exists());

    let compiled = ctx.compile_flow_file(&flow).expect("compile blocked");
    ctx.prepare_workflow("uninstall_demo@v1", Some("uninstall-demo"), BTreeMap::new())
        .expect("prepare run");

    let err = ctx
        .uninstall_workflow("uninstall_demo@v1", false)
        .expect_err("blocked uninstall should fail");
    let message = err.to_string();
    assert!(message.contains("cannot uninstall compiled workflow 'uninstall_demo@v1'"));
    assert!(message.contains("uninstall-demo"));
    assert!(compiled.exists());

    let err = ctx
        .uninstall_workflow("uninstall_demo@v1", true)
        .expect_err("compatibility force flag must not bypass the guard");
    assert!(
        err.to_string()
            .contains("protected runs still reference it")
    );
    assert!(compiled.exists());
}

#[test]
fn failed_run_blocks_uninstall_until_cleanup_completes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    let installed = ctx
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install plugin");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: failed_guard_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: fail_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "exit 1"]
"#,
    )
    .expect("flow");

    let compiled = ctx.compile_flow_file(&flow).expect("compile");
    let report = ctx
        .launch_workflow(
            "failed_guard_demo@v1",
            Some("failed-guard-demo"),
            BTreeMap::new(),
        )
        .expect("launch failed workflow");
    assert_eq!(report.status, "failed");

    let workflow_err = ctx
        .uninstall_workflow("failed_guard_demo@v1", false)
        .expect_err("recoverable failed run should block workflows uninstall");
    assert!(
        workflow_err
            .to_string()
            .contains("protected runs still reference it")
    );

    let plugin_err = ctx
        .remove_plugin("shell", &installed.installed.version)
        .expect_err("recoverable failed run should block plugins uninstall");
    assert!(
        plugin_err
            .to_string()
            .contains("protected runs still reference it")
    );

    ctx.runs_cleanup("failed-guard-demo")
        .expect("cleanup failed run");

    let removed = ctx
        .uninstall_workflow("failed_guard_demo@v1", false)
        .expect("workflows uninstall after cleanup");
    assert_eq!(removed, "failed_guard_demo@v1");
    assert!(!compiled.exists());

    ctx.remove_plugin("shell", &installed.installed.version)
        .expect("plugins uninstall after cleanup");
}

#[test]
fn resume_and_replay_use_cases_operate_on_persisted_recipe_snapshots() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    ctx.install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: resume_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: verify_marker
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "test -f .resume-ok"]
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow("resume_demo@v1", Some("resume-demo"), BTreeMap::new())
        .expect("prepare");
    let report = ctx.run_started_run("resume-demo").expect("run started");
    assert_eq!(report.status, "failed");

    fs::write(tmp.path().join(".resume-ok"), "").expect("marker");

    let resumed = ctx.runs_resume("resume-demo").expect("resume");
    assert_eq!(resumed.run_id, "resume-demo");
    assert_eq!(resumed.status, "success");

    let replayed = ctx
        .runs_replay("resume-demo", Some("resume-demo-replay"))
        .expect("replay");
    assert_eq!(replayed.run_id, "resume-demo-replay");
    assert_eq!(replayed.status, "success");

    let source = ctx.runs_show("resume-demo").expect("show source");
    let source_json = Value::from(source);
    assert_eq!(source_json["resume_count"], 1);

    let replay = ctx.runs_show("resume-demo-replay").expect("show replay");
    let replay_json = Value::from(replay);
    assert_eq!(replay_json["lineage"]["source_run_id"], "resume-demo");
}

#[test]
fn replay_inherits_asset_locks_and_fails_when_pinned_plugin_drifts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: replay_lock_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let launched = ctx
        .launch_workflow(
            "replay_lock_demo@v1",
            Some("replay-lock-demo"),
            BTreeMap::new(),
        )
        .expect("launch");
    assert_eq!(launched.status, "success");

    let installed = transist_plugins::list_plugins_in_home(&repo_local_home_dir(tmp.path()))
        .expect("list plugins");
    let shell = installed
        .into_iter()
        .find(|plugin| plugin.id == "shell")
        .expect("installed shell plugin");
    fs::write(
        shell.dir.join("plugin.py"),
        "#!/usr/bin/env python3\nprint('drifted')\n",
    )
    .expect("mutate plugin");

    let err = ctx
        .runs_replay("replay-lock-demo", Some("replay-lock-demo-r2"))
        .expect_err("replay should fail when plugin bytes drift");
    assert!(err.to_string().contains("has changed"));
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn v0_workflow_allows_registry_overwrite_and_run_recipe_patching() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: test_workflow_demo
version: v0
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["original"]
"#,
    )
    .expect("flow");

    let compiled = ctx.compile_flow_file(&flow).expect("compile first version");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: test_workflow_demo
version: v0
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["from_registry"]
"#,
    )
    .expect("rewrite flow");

    ctx.compile_flow_file(&flow)
        .expect("compile overwritten v0 version");
    let compiled_recipe = fs::read_to_string(&compiled).expect("read compiled recipe");
    assert!(compiled_recipe.contains("from_registry"));

    let prepared = ctx
        .prepare_workflow(
            "test_workflow_demo@v0",
            Some("test-v0-run"),
            BTreeMap::new(),
        )
        .expect("prepare");
    assert_eq!(prepared.status, "planned");

    let run_state = Value::from(ctx.runs_show("test-v0-run").expect("show prepared run"));
    assert_eq!(run_state["test_mode"], Value::Bool(true));

    let run_root =
        resolve_run_root(&repo_local_home_dir(tmp.path()), "test-v0-run").expect("run root");
    let recipe_path = run_root.join("recipe.yaml");
    let mut persisted_recipe: transist::Recipe =
        yaml_serde::from_str(&fs::read_to_string(&recipe_path).expect("read recipe snapshot"))
            .expect("parse recipe snapshot");
    persisted_recipe.steps[0].with.insert(
        "args".to_string(),
        KernelValue::Array(vec![KernelValue::String("from_run_snapshot".to_string())]),
    );
    fs::write(
        &recipe_path,
        yaml_serde::to_string(&persisted_recipe).expect("serialize recipe snapshot"),
    )
    .expect("write patched recipe snapshot");

    let report = ctx
        .run_started_run("test-v0-run")
        .expect("run patched v0 workflow");
    assert_eq!(report.status, "success");

    let run = Value::from(ctx.runs_show("test-v0-run").expect("show completed run"));
    assert_eq!(
        run["steps"]["say_hello"]["last_result"]["stdout"],
        "from_run_snapshot"
    );
}

#[test]
fn release_workflow_rejects_run_recipe_patching() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: release_workflow_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["release"]
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "release_workflow_demo@v1",
        Some("test-v1-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let run_root =
        resolve_run_root(&repo_local_home_dir(tmp.path()), "test-v1-run").expect("run root");
    let recipe_path = run_root.join("recipe.yaml");
    let mut persisted_recipe: transist::Recipe =
        yaml_serde::from_str(&fs::read_to_string(&recipe_path).expect("read recipe snapshot"))
            .expect("parse recipe snapshot");
    persisted_recipe.steps[0].with.insert(
        "args".to_string(),
        KernelValue::Array(vec![KernelValue::String("patched".to_string())]),
    );
    fs::write(
        &recipe_path,
        yaml_serde::to_string(&persisted_recipe).expect("serialize recipe snapshot"),
    )
    .expect("write patched recipe snapshot");

    let err = ctx
        .run_started_run("test-v1-run")
        .expect_err("release workflow should reject recipe snapshot drift");
    assert!(err.to_string().contains("persisted recipe snapshot"));
    assert!(err.to_string().contains("has changed"));
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn runs_sync_recipe_refreshes_v0_run_snapshot_from_registry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: sync_recipe_demo
version: v0
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["first"]
"#,
    )
    .expect("flow");

    let compiled = ctx.compile_flow_file(&flow).expect("compile first version");
    ctx.prepare_workflow(
        "sync_recipe_demo@v0",
        Some("sync-recipe-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: sync_recipe_demo
version: v0
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["second"]
"#,
    )
    .expect("rewrite flow");
    ctx.compile_flow_file(&flow)
        .expect("compile second version");

    let report = ctx
        .runs_sync_recipe("sync-recipe-run")
        .expect("sync recipe");
    assert_eq!(report.run_id, "sync-recipe-run");
    assert!(
        report
            .source_registry_path
            .ends_with("registry/recipes/sync_recipe_demo-v0.yaml")
    );
    assert!(
        report
            .run_recipe_path
            .ends_with("runs/run_sync-recipe-run/recipe.yaml")
    );

    let compiled_recipe = fs::read_to_string(&compiled).expect("read compiled recipe");
    let run_root =
        resolve_run_root(&repo_local_home_dir(tmp.path()), "sync-recipe-run").expect("run root");
    let run_recipe =
        fs::read_to_string(run_root.join("recipe.yaml")).expect("read synced run recipe");
    assert_eq!(run_recipe, compiled_recipe);

    let shown = Value::from(ctx.runs_show("sync-recipe-run").expect("runs show"));
    let synced_sha = shown["recipe_snapshot"]["sha256"]
        .as_str()
        .expect("snapshot sha");
    assert!(!synced_sha.is_empty());
}

#[test]
fn runs_sync_recipe_rejects_non_test_workflows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: sync_recipe_release_demo
version: v1
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    ctx.prepare_workflow(
        "sync_recipe_release_demo@v1",
        Some("sync-recipe-release-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let err = ctx
        .runs_sync_recipe("sync-recipe-release-run")
        .expect_err("release workflow should reject sync-recipe");
    assert!(err.to_string().contains("not a test workflow"));
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn v0_run_replay_ignores_pinned_plugin_drift() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    install_shell_plugin(&ctx);

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: replay_lock_test_demo
version: v0
plugins:
  shell: 0.1.0
steps:
  - id: say_hello
    uses: process:shell/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("flow");

    ctx.compile_flow_file(&flow).expect("compile");
    let launched = ctx
        .launch_workflow(
            "replay_lock_test_demo@v0",
            Some("replay-lock-test-demo"),
            BTreeMap::new(),
        )
        .expect("launch");
    assert_eq!(launched.status, "success");

    let installed = transist_plugins::list_plugins_in_home(&repo_local_home_dir(tmp.path()))
        .expect("list plugins");
    let shell = installed
        .into_iter()
        .find(|plugin| plugin.id == "shell")
        .expect("installed shell plugin");
    let plugin_path = shell.dir.join("plugin.py");
    let mut plugin_source = fs::read_to_string(&plugin_path).expect("read installed plugin");
    plugin_source.push_str("\n# Deliberate package drift for v0 replay coverage.\n");
    fs::write(plugin_path, plugin_source).expect("mutate plugin without breaking APP behavior");

    let replayed = ctx
        .runs_replay("replay-lock-test-demo", Some("replay-lock-test-demo-r2"))
        .expect("v0 replay should ignore plugin drift");
    assert_eq!(replayed.run_id, "replay-lock-test-demo-r2");
    assert_eq!(replayed.status, "success");
}

#[test]
fn legacy_run_remains_inspectable_via_runs_show() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let step_ids = vec!["a".to_string()];
    let snapshot = transist_core::KernelRunSnapshot::new("legacy-run", "wf", "v1", &step_ids);
    let mut record =
        PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 1);
    record.asset_lock_version = 0;
    let store = transist_storage_local::LocalRunStore::new(
        transist_storage_local::LocalStorageConfig::new(repo_local_home_dir(tmp.path())),
    );
    store.create_run(&record).expect("create run");

    let shown = ctx.runs_show("legacy-run").expect("runs show");
    let shown = Value::from(shown);
    assert_eq!(shown["run_id"], "legacy-run");
    assert_eq!(shown["workflow_id"], "wf");
    assert!(shown.get("asset_lock_version").is_none());
}

#[test]
fn deterministic_workflow_blockers_leave_prepare_and_launch_run_storage_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());
    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: deterministic_pre_dispatch_demo
version: v1
prerequisites:
  paths:
    - path: .missing-pre-dispatch-prerequisite
      kind: file
      required: true
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");
    ctx.compile_flow_file(&flow).expect("compile");

    let home_dir = repo_local_home_dir(tmp.path());
    let runs_before = ctx.runs_list().expect("runs list");
    let storage_before = run_storage_entries(&home_dir);

    let prepare_error = ctx
        .prepare_workflow(
            "deterministic_pre_dispatch_demo@v1",
            Some("rejected-prepare"),
            BTreeMap::new(),
        )
        .expect_err("deterministic prepare blocker");
    assert!(prepare_error.to_string().contains("pre-dispatch blockers"));

    let launch_error = ctx
        .launch_workflow(
            "deterministic_pre_dispatch_demo@v1",
            Some("rejected-launch"),
            BTreeMap::new(),
        )
        .expect_err("deterministic launch blocker");
    assert!(launch_error.to_string().contains("pre-dispatch blockers"));

    assert_eq!(ctx.runs_list().expect("runs list"), runs_before);
    assert_eq!(run_storage_entries(&home_dir), storage_before);
    assert!(
        resolve_run_root(&home_dir, "rejected-prepare").is_err(),
        "prepare blocker must not publish a run locator"
    );
    assert!(
        resolve_run_root(&home_dir, "rejected-launch").is_err(),
        "launch blocker must not publish a run locator"
    );
}

fn run_storage_entries(home_dir: &Path) -> Vec<String> {
    let mut entries = fs::read_dir(home_dir.join("runs"))
        .expect("runs storage")
        .map(|entry| {
            entry
                .expect("runs storage entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

/// Regression: a fresh root launch must not inherit a stale durable
/// cancel-request marker left behind by a prior incarnation. Before the
/// fix, the kernel's pre-loop cancel check honored the leftover marker
/// and produced a run with every step canceled and zero executions, with
/// no observable progress to the user.
#[test]
fn fresh_launch_clears_stale_cancel_request_marker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: stale_cancel_demo
version: v1
steps:
  - id: noop
    control: control.noop
    with: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");
    ctx.compile_flow_file(&flow).expect("compile");

    // Plant a stale cancel-request that mimics what a prior, since-finished
    // run with the same root id would have left behind.
    let home_dir = repo_local_home_dir(tmp.path());
    let control_dir = home_dir.join("control").join("runs").join("stale-run");
    fs::create_dir_all(&control_dir).expect("control dir");
    let cancel_request = control_dir.join("cancel-request.json");
    fs::write(
        &cancel_request,
        r#"{
  "schema_version": "transist.control_request.v1",
  "request_id": "stale-request-id",
  "request_type": "cancel",
  "root_run_id": "stale-run",
  "requested_at_ms": 1
}"#,
    )
    .expect("plant cancel-request");
    assert!(cancel_request.exists(), "precondition: marker exists");

    let report = ctx
        .launch_workflow("stale_cancel_demo@v1", Some("stale-run"), BTreeMap::new())
        .expect("launch");
    assert_eq!(report.status, "success", "stale marker must not cancel run");

    let run = Value::from(ctx.runs_show("stale-run").expect("show"));
    assert_eq!(run["status"], "success");
    // Every step should have actually executed at least once.
    for step_id in ["noop", "finish"] {
        let executions = run["steps"][step_id]["executions"]
            .as_u64()
            .expect("executions count");
        assert!(
            executions >= 1,
            "step '{}' should have executed (got {})",
            step_id,
            executions
        );
    }
    assert!(
        !cancel_request.exists(),
        "stale cancel-request file should have been cleared on launch"
    );
}

/// Regression: a successful root run must clear its own cancel-request
/// marker on terminal completion. Before the fix, the marker leaked
/// across runs sharing the same root id and re-canceled the next launch.
#[test]
fn terminal_completion_clears_cancel_request_marker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: cleanup_marker_demo
version: v1
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");
    ctx.compile_flow_file(&flow).expect("compile");

    let report = ctx
        .launch_workflow(
            "cleanup_marker_demo@v1",
            Some("cleanup-marker-run"),
            BTreeMap::new(),
        )
        .expect("launch");
    assert_eq!(report.status, "success");

    let home_dir = repo_local_home_dir(tmp.path());
    let cancel_request = home_dir
        .join("control")
        .join("runs")
        .join("cleanup-marker-run")
        .join("cancel-request.json");
    assert!(
        !cancel_request.exists(),
        "successful run must not leave a cancel-request behind"
    );
}

#[test]
fn post_rename_crash_recovers_on_next_real_start_recipe_attempt() {
    // Real kernel entrypoint, real clock, real local storage: after a crash
    // injected right after the run-birth rename (state.json on disk, no
    // published locator), the next real `launch_workflow` attempt for the
    // same run id must recover the complete, discoverable run instead of
    // bailing or regenerating.
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: birth_recovery_demo
version: v1
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");
    ctx.compile_flow_file(&flow).expect("compile");

    transist_storage_local::fault_injection::set(
        transist_storage_local::fault_injection::FAULT_RUN_BIRTH_AFTER_RENAME,
    );
    let first_attempt = ctx.launch_workflow(
        "birth_recovery_demo@v1",
        Some("birth-recovery-run"),
        BTreeMap::new(),
    );
    transist_storage_local::fault_injection::clear();
    assert!(
        first_attempt.is_err(),
        "first attempt must fail at the injected post-rename fault"
    );

    let home_dir = repo_local_home_dir(tmp.path());
    let run_root = resolve_run_root(&home_dir, "birth-recovery-run");
    assert!(
        run_root.is_err(),
        "no locator should be published after the injected crash"
    );

    // The next real attempt (real clock, no fault armed) must recover.
    let recovered = ctx
        .launch_workflow(
            "birth_recovery_demo@v1",
            Some("birth-recovery-run"),
            BTreeMap::new(),
        )
        .expect("recovery attempt must succeed");
    assert_eq!(recovered.run_id, "birth-recovery-run");
    assert_eq!(recovered.status, "success");

    let run = Value::from(ctx.runs_show("birth-recovery-run").expect("show run"));
    assert_eq!(run["status"], "success");
}

#[test]
fn post_locator_link_crash_recovers_on_next_real_start_recipe_attempt() {
    // Real kernel entrypoint, real clock, real local storage: after a
    // locator-publication crash (the run-birth root is fully committed —
    // state.json, recipe.yaml, run_inputs.json, provenance.json all present
    // — but the run's locator was never durably published, exactly the
    // "ambiguous post-link" outcome `write_new_file_if_identical` and
    // `create_run_staged`'s reconciliation branch are built to recover
    // from), the next real `launch_workflow` attempt for the same run id
    // must recover the complete, discoverable run rather than reporting
    // "already exists" or failing outright.
    let tmp = tempfile::tempdir().expect("tempdir");
    init_repo(tmp.path()).expect("init");
    let ctx = TransistContext::repo_local(tmp.path());

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: locator_recovery_demo
version: v1
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");
    ctx.compile_flow_file(&flow).expect("compile");

    let repo_root = tmp.path();
    let home_dir = repo_local_home_dir(repo_root);
    let recipe_path = home_dir
        .join("registry/recipes")
        .join("locator_recovery_demo-v1.yaml");
    let recipe_yaml = fs::read_to_string(&recipe_path).expect("read compiled recipe");
    let recipe: transist_protocol::Recipe =
        yaml_serde::from_str(&recipe_yaml).expect("parse compiled recipe");

    let first = ctx
        .start_recipe(&recipe, Some("locator-recovery-run"), BTreeMap::new())
        .expect("first start_recipe must succeed");
    assert_eq!(first.run_id, "locator-recovery-run");

    // Simulate the locator-publication crash: the birth root is complete and
    // durable, but its locator index entry never made it to disk (the exact
    // observable state after a crash between the birth rename and the
    // kernel's post-birth `write_run_locator` call, or an ambiguous
    // publication that the caller must retry). The run has not executed any
    // step yet, exactly like the original crashed attempt would not have.
    let locator_path =
        transist_storage_local::run_paths::run_locator_path(&home_dir, "locator-recovery-run");
    assert!(locator_path.is_file(), "locator must exist after birth");
    std::fs::remove_file(&locator_path).expect("remove locator to simulate crash");
    assert!(
        resolve_run_root(&home_dir, "locator-recovery-run").is_err(),
        "run must be undiscoverable via the locator after removal"
    );

    // The next real attempt (real clock, no fault armed) must recover: it
    // reconciles against the existing complete birth root and republishes
    // the locator instead of bailing or regenerating.
    let recovered = ctx
        .start_recipe(&recipe, Some("locator-recovery-run"), BTreeMap::new())
        .expect("recovery attempt must succeed");
    assert_eq!(recovered.run_id, "locator-recovery-run");
    assert!(
        locator_path.is_file(),
        "locator must be republished by the recovery attempt"
    );

    // The run remains fully executable afterward.
    let executed = ctx
        .run_started_run("locator-recovery-run")
        .expect("run must still be executable after reconciliation");
    assert_eq!(executed.status, "success");
}
