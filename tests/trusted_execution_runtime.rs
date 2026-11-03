// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use transist::{
    GateDecisionInput, TransistContext, import_trust_root_file, signature_sidecar_path,
    workflow_definition_sha256,
};
use transist_admin::{
    export_public_key, generate_key, sign_plugin as admin_sign_plugin,
    sign_workflow as admin_sign_workflow,
};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("admin_tooling")
}

fn tempdir() -> Result<tempfile::TempDir> {
    tempfile::tempdir_in(fs::canonicalize("/tmp")?).map_err(Into::into)
}

fn prepare_trust_material(root: &Path) -> Result<PathBuf> {
    let trust_dir = root.join(".test-trust");
    fs::create_dir_all(&trust_dir)?;
    let key_path = trust_dir.join("runtime-test.key");
    let public_key_path = trust_dir.join("runtime-test.pub");
    generate_key("runtime-test", &key_path)?;
    export_public_key(&key_path, &public_key_path)?;
    Ok(public_key_path)
}

fn import_test_trust_root(home_dir: &Path, repo_root: &Path) -> Result<()> {
    let public_key_path = prepare_trust_material(repo_root)?;
    import_trust_root_file(home_dir, &public_key_path)?;
    Ok(())
}

fn sign_workflow(path: &Path, workflow_id: &str, workflow_version: &str) -> Result<()> {
    let _ = (workflow_id, workflow_version);
    let key_path = path
        .parent()
        .expect("workflow parent")
        .join(".test-trust/runtime-test.key");
    admin_sign_workflow(path, &key_path, &signature_sidecar_path(path))?;
    Ok(())
}

fn write_demo_workflow(root: &Path) -> Result<PathBuf> {
    let workflow = root.join("workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: trusted.demo
version: v1
plugins:
  demo.plugin: 1.0.0
steps:
  - id: run_demo
    uses: process:demo.plugin/run
"#,
    )?;
    sign_workflow(&workflow, "trusted.demo", "v1")?;
    Ok(workflow)
}

fn write_trusted_gate_workflow(root: &Path) -> Result<PathBuf> {
    let workflow = root.join("trusted-gate-workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: trusted.source
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
"#,
    )?;
    sign_workflow(&workflow, "trusted.source", "v1")?;
    Ok(workflow)
}

fn prepare_governed_nested_gate(root: &Path) -> Result<(PathBuf, TransistContext)> {
    let home_dir = transist::repo_local_home_dir(root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Governed)?;
    import_test_trust_root(&home_dir, root)?;
    fs::write(
        home_dir.join("config.yaml"),
        r#"version: v1
runtime:
  mode: governed
policy:
  profile: allowlist
  grant_sets:
    nested_gate:
      capabilities:
        - capability: approval
          resource: governed_root-module-child/review
          operations: [request, decide]
    deep_nested_gate:
      capabilities:
        - capability: approval
          resource: governed_deep_root-module-child-module-grandchild/review
          operations: [request, decide]
    permissive_nested_gate:
      capabilities:
        - capability: approval
          resource: governed_permissive_root-module-child/review
          operations: [request, decide]
    extra_authority:
      capabilities:
        - capability: approval
          resource: governed_permissive_root-module-child/emergency_override
          operations: [request, decide]
  workflow_grants:
    - workflow: governed_root@v1
      use: [nested_gate]
    - workflow: governed_deep_root@v1
      use: [deep_nested_gate]
    - workflow: governed_permissive_root@v1
      use: [permissive_nested_gate, extra_authority]
"#,
    )?;

    let child_dir = root.join("child");
    fs::create_dir_all(&child_dir)?;
    let child = child_dir.join("workflow.yaml");
    fs::write(
        &child,
        r#"schema: transist.module.v1
id: governed_child
version: v1
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
"#,
    )?;
    let workflow = root.join("workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: governed_root
version: v1
modules:
  child:
    path: child/workflow.yaml
steps:
  - id: child
    module: child
"#,
    )?;
    sign_workflow(&workflow, "governed_root", "v1")?;

    let context = TransistContext::with_home(root, &home_dir);
    context.compile_flow_file_with_trust(&workflow, true)?;

    let permissive_workflow = root.join("permissive-workflow.yaml");
    fs::write(
        &permissive_workflow,
        r#"schema: transist.flow.v1
id: governed_permissive_root
version: v1
modules:
  child:
    path: child/workflow.yaml
steps:
  - id: child
    module: child
"#,
    )?;
    sign_workflow(&permissive_workflow, "governed_permissive_root", "v1")?;
    context.compile_flow_file_with_trust(&permissive_workflow, true)?;

    let middle = root.join("middle-workflow.yaml");
    fs::write(
        &middle,
        r#"schema: transist.module.v1
id: governed_middle
version: v1
modules:
  grandchild:
    path: child/workflow.yaml
steps:
  - id: grandchild
    module: grandchild
"#,
    )?;
    let deep_workflow = root.join("deep-workflow.yaml");
    fs::write(
        &deep_workflow,
        r#"schema: transist.flow.v1
id: governed_deep_root
version: v1
modules:
  child:
    path: middle-workflow.yaml
steps:
  - id: child
    module: child
"#,
    )?;
    sign_workflow(&deep_workflow, "governed_deep_root", "v1")?;
    context.compile_flow_file_with_trust(&deep_workflow, true)?;
    Ok((home_dir, context))
}

fn gate_decision_input(reason: &str) -> GateDecisionInput {
    GateDecisionInput {
        actor: Some("reviewer@example.com".to_string()),
        actor_source: Some("test".to_string()),
        reason: Some(reason.to_string()),
        decided_at_ms: Some(42),
    }
}

fn write_process_plugin_script(path: &Path, version: &str) -> Result<()> {
    let script = r#"#!/bin/sh
IFS= read -r _handshake
printf '%s\n' '{"type":"app.handshake.response.v1","protocol_version":"app.v1","plugin":{"id":"demo.plugin","version":"PLUGIN_VERSION"},"adapter_kind":"process","capabilities":{"actions":["run"],"supports_cancel":false,"supports_health":true,"supports_artifact_hints":false,"supports_evidence_hints":false}}'
IFS= read -r request || exit 0
invoke_id=$(printf '%s\n' "$request" | sed -n 's/.*"invoke_id":"\([^"]*\)".*/\1/p')
printf '%s\n' "{\"type\":\"app.invoke.result.v1\",\"protocol_version\":\"app.v1\",\"invoke_id\":\"$invoke_id\",\"status\":\"success\",\"output\":{},\"artifact_hints\":[],\"evidence_hints\":[],\"metrics\":{},\"usage\":null,\"error\":null}"
"#
    .replace("PLUGIN_VERSION", version);
    fs::write(path, script)?;
    Ok(())
}

fn copy_demo_plugin(root: &Path) -> Result<PathBuf> {
    let plugin_dir = root.join("demo-plugin");
    fs::create_dir_all(plugin_dir.join("schemas"))?;
    for relative in [
        "plugin.yaml",
        "run.sh",
        "schemas/input.json",
        "schemas/output.json",
    ] {
        let src = fixture_root().join("demo-plugin").join(relative);
        let dst = plugin_dir.join(relative);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    let manifest_path = plugin_dir.join("plugin.yaml");
    fs::write(
        &manifest_path,
        fs::read_to_string(&manifest_path)?.replacen("version: v1", "version: 1.0.0", 1),
    )?;
    write_process_plugin_script(&plugin_dir.join("run.sh"), "1.0.0")?;
    Ok(plugin_dir)
}

fn copy_signed_demo_plugin(root: &Path) -> Result<PathBuf> {
    let plugin_dir = copy_demo_plugin(root)?;
    let key_path = root.join(".test-trust/runtime-test.key");
    admin_sign_plugin(&plugin_dir, &key_path, &signature_sidecar_path(&plugin_dir))?;
    Ok(plugin_dir)
}

fn copy_signed_prompt_demo_plugin(root: &Path) -> Result<PathBuf> {
    let plugin_dir = copy_demo_plugin(root)?;
    fs::write(
        plugin_dir.join("run.sh"),
        r#"#!/bin/sh
IFS= read -r _
printf '%s\n' '{"type":"app.handshake.response.v1","protocol_version":"app.v1","plugin":{"id":"demo.plugin","version":"1.0.0"},"adapter_kind":"process","capabilities":{"actions":["run"],"supports_cancel":false,"supports_health":true,"supports_artifact_hints":false,"supports_evidence_hints":false}}'
"#,
    )?;
    fs::write(
        plugin_dir.join("plugin.yaml"),
        r#"schema: transist.plugin.v1
id: demo.plugin
version: 1.0.0
adapter: process
entrypoint:
  type: process
  command: run.sh
package:
  files: [run.sh, schemas/input.json, schemas/output.json]
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    bindings:
      transist_prompt_v1:
        inline_field: prompt
        file_field: prompt_file
      transist_skills_v1:
        bundle_field: skills_bundle
        rendered_field: skills_rendered
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    )?;
    let key_path = root.join(".test-trust/runtime-test.key");
    admin_sign_plugin(&plugin_dir, &key_path, &signature_sidecar_path(&plugin_dir))?;
    Ok(plugin_dir)
}

fn copy_unsigned_demo_plugin(root: &Path) -> Result<PathBuf> {
    copy_demo_plugin(root)
}

#[test]
fn workflow_compile_trust_status_tracks_metadata() -> Result<()> {
    let tmp = tempdir()?;
    let home = tmp.path().join("home");
    let workflow = tmp.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: status.demo\nversion: v1\nplugins:\n  demo.plugin: 1.0.0\nsteps:\n  - id: step\n    uses: process:demo.plugin/run\n",
    )?;
    transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)?;
    let context = TransistContext::with_home(tmp.path(), &home);

    let status = context.workflow_compile_trust_status(&workflow)?;
    assert_eq!(status.workflow_id, "status.demo");
    assert_eq!(status.workflow_version, "v1");
    assert!(!status.trusted_metadata_exists);

    let metadata_dir = home.join("registry/trust");
    fs::create_dir_all(&metadata_dir)?;
    fs::write(metadata_dir.join("status.demo-v1.json"), b"{}").expect("metadata");
    assert!(
        context
            .workflow_compile_trust_status(&workflow)?
            .trusted_metadata_exists
    );
    Ok(())
}

#[test]
fn trusted_compile_rejects_invalid_identity_before_writing_metadata() -> Result<()> {
    let tmp = tempdir()?;
    let home = tmp.path().join("home");
    let workflow = tmp.path().join("invalid-identity.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: invalid/identity\nversion: v1\nsteps: []\n",
    )?;
    transist::init_home_dir_with_mode(&home, transist::RuntimeOperatingMode::Standalone)?;
    let context = TransistContext::with_home(tmp.path(), &home);

    let error = context
        .compile_flow_file_with_trust(&workflow, true)
        .expect_err("invalid workflow identity must be rejected before trust-path creation");

    assert!(error.to_string().contains("invalid workflow id"));
    assert!(!tmp.path().join("identity-v1.json").exists());
    assert!(!home.join("registry/trust").exists());
    Ok(())
}

#[test]
fn failed_recompiles_preserve_existing_trust_metadata() -> Result<()> {
    let temp = tempdir()?;
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, repo_root)?;

    let plugin_dir = copy_signed_demo_plugin(repo_root)?;
    let workflow_path = write_demo_workflow(repo_root)?;
    let context = TransistContext::with_home(repo_root, &home_dir);
    context.install_plugin_dir_with_trust(&plugin_dir, true)?;
    context.compile_flow_file_with_trust(&workflow_path, true)?;

    let metadata_path = home_dir.join("registry/trust/trusted.demo-v1.json");
    let original_metadata = fs::read(&metadata_path)?;
    fs::write(
        &workflow_path,
        r#"schema: transist.flow.v1
id: trusted.demo
version: v1
plugins:
  demo.plugin: 1.0.0
# same version with changed recipe bytes
steps:
  - id: run_demo
    uses: process:demo.plugin/run
  - id: second_step
    uses: process:demo.plugin/run
"#,
    )?;
    sign_workflow(&workflow_path, "trusted.demo", "v1")?;

    let trusted_error = context
        .compile_flow_file_with_trust(&workflow_path, true)
        .expect_err("changed immutable recipe must reject trusted recompilation");
    assert!(
        trusted_error
            .to_string()
            .contains("already compiled with different content")
    );
    assert_eq!(fs::read(&metadata_path)?, original_metadata);

    let standard_error = context
        .compile_flow_file_with_trust(&workflow_path, false)
        .expect_err("changed immutable recipe must reject standard recompilation");
    assert!(
        standard_error
            .to_string()
            .contains("already compiled with different content")
    );
    assert_eq!(fs::read(&metadata_path)?, original_metadata);
    Ok(())
}

#[test]
fn successful_standard_compile_removes_trust_metadata_after_recipe_commit() -> Result<()> {
    let temp = tempdir()?;
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, repo_root)?;

    let plugin_dir = copy_signed_demo_plugin(repo_root)?;
    let workflow_path = write_demo_workflow(repo_root)?;
    let context = TransistContext::with_home(repo_root, &home_dir);
    context.install_plugin_dir_with_trust(&plugin_dir, true)?;
    context.compile_flow_file_with_trust(&workflow_path, true)?;

    let metadata_path = home_dir.join("registry/trust/trusted.demo-v1.json");
    assert!(metadata_path.exists());
    context.compile_flow_file_with_trust(&workflow_path, false)?;
    assert!(!metadata_path.exists());
    Ok(())
}

#[cfg(feature = "dev-version-bypass")]
fn write_v0_workflow(root: &Path) -> Result<PathBuf> {
    let workflow = root.join("workflow-v0.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: bypass.demo
version: v0
plugins:
  demo.plugin: v0
steps:
  - id: run_demo
    uses: process:demo.plugin/run
"#,
    )?;
    Ok(workflow)
}

#[cfg(feature = "dev-version-bypass")]
fn write_unsigned_test_plugin(root: &Path) -> Result<PathBuf> {
    let plugin_dir = root.join("demo-plugin-v0");
    fs::create_dir_all(plugin_dir.join("schemas"))?;
    fs::write(
        plugin_dir.join("plugin.yaml"),
        r#"schema: transist.plugin.v1
id: demo.plugin
version: v0
adapter: process
entrypoint:
  type: process
  command: run.sh
package:
  files: [run.sh, schemas/input.json, schemas/output.json]
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    )?;
    write_process_plugin_script(&plugin_dir.join("run.sh"), "v0")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(plugin_dir.join("run.sh"))?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(plugin_dir.join("run.sh"), permissions)?;
    }
    fs::write(plugin_dir.join("schemas/input.json"), "{}")?;
    fs::write(plugin_dir.join("schemas/output.json"), "{}")?;
    Ok(plugin_dir)
}

fn assert_trusted_execution_blocker(
    context: &TransistContext,
    workflow_ref: &str,
    reason_code: &str,
) -> Result<()> {
    let validation = context.validate_bound_workflow(workflow_ref, BTreeMap::new(), true, false)?;
    let check = validation
        .checks
        .iter()
        .find(|check| check.code == "trusted_execution")
        .expect("trusted execution validation check");
    assert_eq!(check.status.as_str(), "failed");
    assert!(
        check.message.contains(&format!("code={reason_code}")),
        "unexpected trusted execution check: {}",
        check.message
    );
    Ok(())
}

fn read_provenance(home_dir: &Path, run_id: &str) -> serde_json::Value {
    let path = transist::TransistContext::with_home(home_dir, home_dir)
        .run_root(run_id)
        .expect("run root")
        .join("provenance.json");
    serde_json::from_str(&fs::read_to_string(path).expect("read provenance"))
        .expect("parse provenance")
}

fn read_workflow_install_index(home_dir: &Path) -> serde_json::Value {
    let path = home_dir
        .join("registry")
        .join("workflow-install-index.json");
    serde_json::from_str(&fs::read_to_string(path).expect("read workflows install index"))
        .expect("parse workflows install index")
}

#[test]
fn trusted_prepare_records_allowed_decision() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    context
        .compile_flow_file_with_trust(&workflow_path, true)
        .expect("trusted workflow installation");

    let report = context
        .prepare_workflow_with_trust(
            "trusted.demo@v1",
            Some("run-allow"),
            Default::default(),
            true,
            false,
        )
        .expect("prepare trusted run");
    assert_eq!(report.run_id, "run-allow");

    let state = context.runs_show("run-allow").expect("load run state");
    let state_json = serde_json::to_value(state).expect("state json");
    assert_eq!(
        state_json["trust_decision"]["decision"],
        serde_json::Value::String("allowed".to_string())
    );

    let provenance = read_provenance(&home_dir, "run-allow");
    assert_eq!(provenance["workflow_trust_decision"], "verified");
    assert_eq!(provenance["plugins"][0]["trust_decision"], "verified");
    assert_eq!(provenance["decision"], "allowed");
    assert!(provenance["finalized_at_ms"].is_null());
}

#[test]
fn workspace_binding_preflight_treats_root_and_subflow_prompts_as_workflow_assets() -> Result<()> {
    let temp = tempdir()?;
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    let source_a = repo_root.join("workflow-source-a");
    let workspace_b = repo_root.join("workspace-b");
    fs::create_dir_all(source_a.join("prompts"))?;
    fs::create_dir_all(source_a.join("subflows/prompts"))?;
    fs::create_dir_all(source_a.join("subflows/scripts"))?;
    fs::create_dir_all(source_a.join("subflows/skills/demo"))?;
    fs::create_dir_all(&workspace_b)?;
    fs::write(source_a.join("prompts/root.md"), "root prompt")?;
    fs::write(source_a.join("subflows/prompts/child.md"), "child prompt")?;
    fs::write(source_a.join("subflows/scripts/helper.sh"), "exit 0\n")?;
    fs::write(
        source_a.join("subflows/skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Test skill\n---\n\n# Demo\n",
    )?;
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, repo_root)?;

    let plugin_dir = copy_signed_prompt_demo_plugin(repo_root)?;
    fs::write(
        source_a.join("subflows/child.yaml"),
        r#"schema: transist.module.v1
id: workspace_binding_child
version: v1
steps:
  - id: child_prompt
    uses: process:demo.plugin/run
    prompt_file: prompts/child.md
    skills:
      - skills/demo
    with:
      helper: source://scripts/helper.sh
"#,
    )?;
    let workflow = source_a.join("workflow.yaml");
    fs::write(
        &workflow,
        format!(
            r#"schema: transist.flow.v1
id: workspace_binding_assets
version: v1
plugins:
  demo.plugin: 1.0.0
modules:
  child:
    path: subflows/child.yaml
workspace:
  path: {}
  access: full
steps:
  - id: root_prompt
    uses: process:demo.plugin/run
    prompt_file: prompts/root.md
  - id: child
    module: child
"#,
            workspace_b.display()
        ),
    )?;
    admin_sign_workflow(
        &workflow,
        &repo_root.join(".test-trust/runtime-test.key"),
        &signature_sidecar_path(&workflow),
    )?;

    let context = TransistContext::with_home(repo_root, &home_dir);
    context.install_plugin_dir_with_trust(&plugin_dir, true)?;
    context.compile_flow_file_with_trust(&workflow, true)?;

    let prepared = context.prepare_workflow_with_trust(
        "workspace_binding_assets@v1",
        Some("workspace-binding-assets-run"),
        BTreeMap::new(),
        true,
        false,
    )?;
    assert_eq!(
        prepared.preflight.status.as_str(),
        "passed",
        "unexpected preflight checks: {:?}",
        prepared.preflight.checks
    );
    assert!(
        !prepared.preflight.checks.iter().any(|check| {
            check.kind == "filesystem_workspace_boundary"
                && check.status.as_str() == "failed"
                && check.message.contains("prompt_file")
        }),
        "workflow source assets must not require filesystem capability: {:?}",
        prepared.preflight.checks
    );

    let unrelated_root = repo_root.join("unrelated-caller");
    fs::create_dir_all(&unrelated_root)?;
    let unrelated_context = TransistContext::with_home(&unrelated_root, &home_dir);
    let repeated = unrelated_context.runs_preflight("workspace-binding-assets-run")?;
    assert_eq!(
        repeated.status.as_str(),
        "passed",
        "persisted preflight must not depend on the caller root: {:?}",
        repeated.checks
    );
    Ok(())
}

#[test]
fn governed_nested_gate_requests_and_decisions_use_root_workflow_grant() -> Result<()> {
    let temp = tempdir()?;
    let (_home_dir, context) = prepare_governed_nested_gate(temp.path())?;

    for run_id in ["nested-approve", "nested-reject", "nested-request-changes"] {
        let blocked = context.launch_workflow_with_trust(
            "governed_root@v1",
            Some(run_id),
            BTreeMap::new(),
            true,
            false,
        )?;
        assert_eq!(blocked.status, "blocked");
    }

    let approved = context.approve_gate_with_input(
        "nested-approve",
        "review",
        gate_decision_input("approved"),
    )?;
    assert_eq!(approved.status, "success");

    let rejected = context.reject_gate_with_input(
        "nested-reject",
        "review",
        gate_decision_input("rejected"),
    )?;
    assert_eq!(rejected.status, "failed");

    let changed = context.request_changes_gate_with_input(
        "nested-request-changes",
        "review",
        gate_decision_input("changes requested"),
    )?;
    assert_eq!(changed.run_id, "nested-request-changes");
    let child = serde_json::Value::from(context.runs_show("nested-request-changes__child")?);
    assert_eq!(
        child["steps"]["review"]["last_result"]["output"]["decision"],
        "request_changes"
    );

    let deep = context.launch_workflow_with_trust(
        "governed_deep_root@v1",
        Some("deep-nested-approve"),
        BTreeMap::new(),
        true,
        false,
    )?;
    assert_eq!(deep.status, "blocked");
    let deep_approved = context.approve_gate_with_input(
        "deep-nested-approve",
        "review",
        gate_decision_input("approved through validated deep lineage"),
    )?;
    assert_eq!(deep_approved.status, "success");
    Ok(())
}

#[test]
fn governed_nested_gate_decision_fails_closed_on_invalid_root_lineage() -> Result<()> {
    let temp = tempdir()?;
    let (_home_dir, context) = prepare_governed_nested_gate(temp.path())?;
    let run_id = "nested-invalid-lineage";
    let blocked = context.launch_workflow_with_trust(
        "governed_root@v1",
        Some(run_id),
        BTreeMap::new(),
        true,
        false,
    )?;
    assert_eq!(blocked.status, "blocked");

    let permissive = context.launch_workflow_with_trust(
        "governed_permissive_root@v1",
        Some("unrelated-permissive-root"),
        BTreeMap::new(),
        true,
        false,
    )?;
    assert_eq!(permissive.status, "blocked");

    let child_run_id = format!("{run_id}__child");
    let child_state_path = context.run_root(&child_run_id)?.join("state.json");
    let original_child_state = fs::read_to_string(&child_state_path)?;
    let mut child_state: serde_json::Value = serde_json::from_str(&original_child_state)?;
    child_state["lineage"]["root_run_id"] = serde_json::json!("unrelated-permissive-root");
    fs::write(&child_state_path, serde_json::to_vec_pretty(&child_state)?)?;

    let substituted_root = context
        .approve_gate_with_input(
            run_id,
            "review",
            gate_decision_input("must not be recorded"),
        )
        .expect_err("an unrelated valid root must not supply policy authority");
    assert!(
        substituted_root
            .to_string()
            .contains("inconsistent policy lineage")
    );

    fs::write(&child_state_path, &original_child_state)?;
    let mut child_state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&child_state_path)?)?;
    child_state["lineage"]["parent_run_id"] = serde_json::json!("unrelated-permissive-root");
    fs::write(&child_state_path, serde_json::to_vec_pretty(&child_state)?)?;

    let parent_mismatch = context
        .approve_gate_with_input(
            run_id,
            "review",
            gate_decision_input("must still not be recorded"),
        )
        .expect_err("an unrelated parent must deny the decision");
    assert!(
        parent_mismatch.to_string().contains("not child workflow"),
        "unexpected parent mismatch error: {parent_mismatch:#}"
    );

    fs::write(&child_state_path, &original_child_state)?;
    let mut child_state: serde_json::Value = serde_json::from_str(&original_child_state)?;
    child_state["lineage"]["parent_run_id"] = serde_json::json!(child_run_id);
    fs::write(&child_state_path, serde_json::to_vec_pretty(&child_state)?)?;

    let cycle = context
        .approve_gate_with_input(
            run_id,
            "review",
            gate_decision_input("must still not be recorded"),
        )
        .expect_err("a lineage cycle must deny the decision");
    assert!(
        cycle
            .to_string()
            .contains("cycle in its persisted policy lineage")
    );

    fs::write(&child_state_path, &original_child_state)?;
    let mut child_state: serde_json::Value = serde_json::from_str(&original_child_state)?;
    child_state["lineage"]["parent_step_id"] = serde_json::Value::Null;
    fs::write(&child_state_path, serde_json::to_vec_pretty(&child_state)?)?;

    let malformed = context
        .approve_gate_with_input(
            run_id,
            "review",
            gate_decision_input("must still not be recorded"),
        )
        .expect_err("an incomplete parent link must deny the decision");
    assert!(
        malformed
            .to_string()
            .contains("must name both parent_run_id and parent_step_id")
    );

    fs::write(&child_state_path, &original_child_state)?;
    let mut child_state: serde_json::Value = serde_json::from_str(&original_child_state)?;
    child_state["lineage"]["parent_run_id"] = serde_json::json!("missing-parent");
    fs::write(&child_state_path, serde_json::to_vec_pretty(&child_state)?)?;

    let missing_parent = context
        .approve_gate_with_input(
            run_id,
            "review",
            gate_decision_input("must still not be recorded"),
        )
        .expect_err("a missing parent must deny the decision");
    assert!(
        missing_parent
            .to_string()
            .contains("required for policy evaluation")
    );
    Ok(())
}

#[test]
fn trusted_source_path_new_metadata_is_absolute() -> Result<()> {
    let temp = tempdir()?;
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, repo_root)?;

    let plugin_dir = copy_signed_demo_plugin(repo_root)?;
    let workflow_path = write_demo_workflow(repo_root)?;
    let context = TransistContext::with_home(repo_root, &home_dir);
    context.install_plugin_dir_with_trust(&plugin_dir, true)?;
    context.compile_flow_file_with_trust(&workflow_path, true)?;

    let metadata: serde_json::Value = serde_json::from_slice(&fs::read(
        home_dir.join("registry/trust/trusted.demo-v1.json"),
    )?)?;
    assert_eq!(
        metadata["workflow_source_path"],
        serde_json::Value::String(workflow_path.canonicalize()?.display().to_string())
    );
    Ok(())
}

#[test]
fn trusted_source_path_relative_metadata_uses_recipe_source_dir() -> Result<()> {
    let temp = tempdir()?;
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, repo_root)?;

    let plugin_dir = copy_signed_demo_plugin(repo_root)?;
    let workflow_path = write_demo_workflow(repo_root)?;
    let context = TransistContext::with_home(repo_root, &home_dir);
    context.install_plugin_dir_with_trust(&plugin_dir, true)?;
    context.compile_flow_file_with_trust(&workflow_path, true)?;

    let metadata_path = home_dir.join("registry/trust/trusted.demo-v1.json");
    let mut metadata: serde_json::Value = serde_json::from_slice(&fs::read(&metadata_path)?)?;
    metadata["workflow_source_path"] =
        serde_json::Value::String("legacy/workflow.yaml".to_string());
    fs::write(&metadata_path, serde_json::to_vec(&metadata)?)?;

    context.prepare_workflow_with_trust(
        "trusted.demo@v1",
        Some("legacy-source-dir"),
        Default::default(),
        true,
        false,
    )?;
    Ok(())
}

#[test]
fn trusted_source_path_preflight_is_independent_of_context_repo_root() -> Result<()> {
    let temp = tempdir()?;
    let workflow_root = temp.path().join("workflow-root");
    let unrelated_root = temp.path().join("unrelated-root");
    fs::create_dir_all(&workflow_root)?;
    fs::create_dir_all(&unrelated_root)?;
    let home_dir = temp.path().join("home");
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, &workflow_root)?;

    let workflow_path = write_trusted_gate_workflow(&workflow_root)?;
    let source_context = TransistContext::with_home(&workflow_root, &home_dir);
    source_context.compile_flow_file_with_trust(&workflow_path, true)?;
    source_context.prepare_workflow_with_trust(
        "trusted.source@v1",
        Some("cwd-independent"),
        Default::default(),
        true,
        false,
    )?;

    let unrelated_context = TransistContext::with_home(&unrelated_root, &home_dir);
    unrelated_context.runs_preflight("cwd-independent")?;
    let state = serde_json::to_value(unrelated_context.runs_show("cwd-independent")?)?;
    assert_eq!(
        state["trust_decision"]["workflow"]["trust_decision"],
        "verified"
    );
    Ok(())
}

#[test]
fn trusted_source_path_missing_original_fails_closed() -> Result<()> {
    let temp = tempdir()?;
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, repo_root)?;

    let plugin_dir = copy_signed_demo_plugin(repo_root)?;
    let workflow_path = write_demo_workflow(repo_root)?;
    let context = TransistContext::with_home(repo_root, &home_dir);
    context.install_plugin_dir_with_trust(&plugin_dir, true)?;
    context.compile_flow_file_with_trust(&workflow_path, true)?;
    fs::remove_file(&workflow_path)?;
    fs::remove_file(signature_sidecar_path(&workflow_path))?;

    let error = context
        .prepare_workflow_with_trust(
            "trusted.demo@v1",
            Some("missing-source"),
            Default::default(),
            true,
            false,
        )
        .expect_err("missing trusted workflow source must be denied");
    assert!(
        error
            .to_string()
            .contains("trusted execution requires workflow source")
    );
    Ok(())
}

#[test]
fn trusted_workflow_install_records_durable_install_entry() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");

    let report = context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect("trusted workflows install");
    assert_eq!(report.workflow_id, "trusted.demo");
    assert_eq!(report.workflow_version, "v1");
    assert!(report.recipe_path.exists());
    assert_eq!(
        report.workflow_definition_sha256,
        workflow_definition_sha256(&workflow_path).expect("workflow digest")
    );

    let index = read_workflow_install_index(&home_dir);
    assert_eq!(index["schema"], "transist.workflow_install_index.v1");
    assert_eq!(
        index["entries"]["trusted.demo@v1"]["workflow_definition_sha256"],
        report.workflow_definition_sha256
    );
    assert_eq!(
        index["entries"]["trusted.demo@v1"]["trusted_execution"],
        serde_json::Value::Bool(true)
    );
}

#[test]
fn trusted_installs_accept_explicit_signature_sources() -> Result<()> {
    let temp = tempdir()?;
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)?;
    import_test_trust_root(&home_dir, repo_root)?;
    let key_path = repo_root.join(".test-trust/runtime-test.key");
    let signatures = repo_root.join("signatures");
    fs::create_dir_all(&signatures)?;

    let plugin_dir = copy_demo_plugin(repo_root)?;
    let plugin_signature = signatures.join("demo-plugin.signature");
    admin_sign_plugin(&plugin_dir, &key_path, &plugin_signature)?;
    assert!(!signature_sidecar_path(&plugin_dir).exists());

    let context = TransistContext::with_home(repo_root, &home_dir);
    context.install_plugin_dir_with_trust_and_signature(
        &plugin_dir,
        true,
        Some(&plugin_signature),
    )?;
    assert!(
        home_dir.join("plugins/demo.plugin/1.0.0.sig").is_file(),
        "trusted install persists the verified custom plugin signature"
    );

    let workflow_path = write_demo_workflow(repo_root)?;
    fs::remove_file(signature_sidecar_path(&workflow_path))?;
    let workflow_signature = signatures.join("workflow.signature");
    admin_sign_workflow(&workflow_path, &key_path, &workflow_signature)?;

    context.install_flow_file_with_trust_and_signature(
        &workflow_path,
        true,
        Some(&workflow_signature),
    )?;
    context.prepare_workflow_with_trust(
        "trusted.demo@v1",
        Some("custom-signature-source"),
        BTreeMap::new(),
        true,
        false,
    )?;

    fs::remove_file(&workflow_signature)?;
    let error = context
        .prepare_workflow_with_trust(
            "trusted.demo@v1",
            Some("missing-custom-signature-source"),
            BTreeMap::new(),
            true,
            false,
        )
        .expect_err("trusted re-verification must use the recorded custom signature source");
    assert!(
        error
            .to_string()
            .contains("failed reading workflow signature")
    );
    Ok(())
}

#[test]
fn workflow_install_rolls_back_recipe_when_fault_follows_recipe_publication() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");
    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");

    transist_storage_local::fault_injection::set(
        transist_storage_local::fault_injection::FAULT_WORKFLOW_INSTALL_AFTER_RECIPE,
    );
    let error = context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect_err("fault after recipe publication");
    transist_storage_local::fault_injection::clear();

    assert!(error.to_string().contains("injected fault"));
    assert!(
        !home_dir
            .join("registry/recipes/trusted.demo-v1.yaml")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/trust/trusted.demo-v1.json")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn workflow_install_rolls_back_recipe_and_trust_when_fault_follows_trust_publication() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");
    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");

    transist_storage_local::fault_injection::set(
        transist_storage_local::fault_injection::FAULT_WORKFLOW_INSTALL_AFTER_TRUST,
    );
    let error = context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect_err("fault after trust publication");
    transist_storage_local::fault_injection::clear();

    assert!(error.to_string().contains("injected fault"));
    assert!(
        !home_dir
            .join("registry/recipes/trusted.demo-v1.yaml")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/trust/trusted.demo-v1.json")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn idempotent_install_fault_preserves_preexisting_artifacts() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");
    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect("first install");

    transist_storage_local::fault_injection::set(
        transist_storage_local::fault_injection::FAULT_WORKFLOW_INSTALL_AFTER_RECIPE,
    );
    context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect_err("fault during idempotent install");
    transist_storage_local::fault_injection::clear();

    assert!(
        home_dir
            .join("registry/recipes/trusted.demo-v1.yaml")
            .is_file()
    );
    assert!(
        home_dir
            .join("registry/trust/trusted.demo-v1.json")
            .is_file()
    );
    assert!(read_workflow_install_index(&home_dir)["entries"]["trusted.demo@v1"].is_object());
}

#[test]
fn promotion_ownership_rollback_removes_new_empty_install_index() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");
    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    let package = transist::capture_workflow_package(&workflow_path).expect("capture package");
    let lock = transist::lock_workflow_installation(&home_dir).expect("install lock");
    let mut ownership = transist::WorkflowInstallOwnershipJournal::new();

    context
        .install_workflow_package_snapshot_with_trust_and_ownership(
            &package,
            true,
            &lock,
            &mut ownership,
        )
        .expect("install");
    ownership.rollback(&home_dir).expect("rollback");

    assert!(
        !home_dir
            .join("registry/recipes/trusted.demo-v1.yaml")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/trust/trusted.demo-v1.json")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn promotion_ownership_rollback_preserves_preexisting_index_and_unrelated_entry() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");
    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    let index_path = home_dir.join("registry/workflow-install-index.json");
    fs::write(
        &index_path,
        r#"{"schema":"transist.workflow_install_index.v1","entries":{"unrelated@v1":{"workflow_id":"unrelated","workflow_version":"v1","workflow_definition_sha256":"other-digest","recipe_path":"other-recipe","workflow_source_path":"other-source","trusted_execution":false,"installed_at_ms":1}}}"#,
    )
    .expect("preexisting index");
    let package = transist::capture_workflow_package(&workflow_path).expect("capture package");
    let lock = transist::lock_workflow_installation(&home_dir).expect("install lock");
    let mut ownership = transist::WorkflowInstallOwnershipJournal::new();

    context
        .install_workflow_package_snapshot_with_trust_and_ownership(
            &package,
            true,
            &lock,
            &mut ownership,
        )
        .expect("install");
    ownership.rollback(&home_dir).expect("rollback");

    let index = read_workflow_install_index(&home_dir);
    assert!(index_path.is_file());
    assert!(index["entries"]["trusted.demo@v1"].is_null());
    assert_eq!(
        index["entries"]["unrelated@v1"]["workflow_definition_sha256"],
        "other-digest"
    );
}

#[cfg(unix)]
#[test]
fn promotion_ownership_rollback_surfaces_artifact_removal_errors() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");
    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    let package = transist::capture_workflow_package(&workflow_path).expect("capture package");
    let lock = transist::lock_workflow_installation(&home_dir).expect("install lock");
    let mut ownership = transist::WorkflowInstallOwnershipJournal::new();
    context
        .install_workflow_package_snapshot_with_trust_and_ownership(
            &package,
            true,
            &lock,
            &mut ownership,
        )
        .expect("install");
    let recipe_path = home_dir.join("registry/recipes/trusted.demo-v1.yaml");
    fs::remove_file(&recipe_path).expect("replace recipe");
    symlink(&workflow_path, &recipe_path).expect("symlink recipe");

    let error = ownership
        .rollback(&home_dir)
        .expect_err("symlink must make rollback report an error");

    assert!(
        error
            .to_string()
            .contains("workflow install rollback failed")
    );
    assert!(error.to_string().contains("trusted.demo-v1.yaml"));
    assert!(recipe_path.is_symlink());
    assert!(
        !home_dir
            .join("registry/trust/trusted.demo-v1.json")
            .exists()
    );
    assert!(
        !home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn workflow_install_rejects_comment_only_drift_under_same_version() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect("first trusted workflows install");

    fs::write(
        &workflow_path,
        r#"schema: transist.flow.v1
id: trusted.demo
version: v1
plugins:
  demo.plugin: 1.0.0
# comment-only drift
steps:
  - id: run_demo
    uses: process:demo.plugin/run
"#,
    )
    .expect("rewrite workflow");
    sign_workflow(&workflow_path, "trusted.demo", "v1").expect("re-sign workflow");

    let error = context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect_err("comment-only digest drift should be rejected");
    assert!(
        error
            .to_string()
            .contains("already installed with different content"),
        "unexpected install error: {error}"
    );
}

#[test]
fn workflow_uninstall_removes_install_index_entry() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    context
        .install_flow_file_with_trust(&workflow_path, true)
        .expect("trusted workflows install");

    context
        .uninstall_workflow("trusted.demo@v1", false)
        .expect("uninstall workflow");

    let index = read_workflow_install_index(&home_dir);
    assert!(index["entries"]["trusted.demo@v1"].is_null());
}

#[test]
fn trusted_prepare_denies_plugin_without_installed_signature() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir(&plugin_dir)
        .expect("plain plugins install");
    context
        .compile_flow_file_with_trust(&workflow_path, true)
        .expect("trusted workflow installation");

    let error = context
        .prepare_workflow_with_trust(
            "trusted.demo@v1",
            Some("run-deny"),
            Default::default(),
            true,
            false,
        )
        .expect_err("prepare should deny unsigned installed plugin");
    assert!(
        error
            .to_string()
            .contains("deterministic pre-dispatch blockers"),
        "unexpected preparation error: {error}"
    );

    assert_trusted_execution_blocker(&context, "trusted.demo@v1", "unsigned")
        .expect("unsigned plugin must be a trust blocker");
    assert!(
        !context
            .runs_list()
            .expect("list runs")
            .contains(&"run-deny".to_string()),
        "denied trusted execution must not create a run"
    );
}

#[test]
fn plain_mode_preserves_unsigned_workflow_and_plugin_behavior() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");

    let plugin_dir = copy_unsigned_demo_plugin(repo_root).expect("copy unsigned plugin");
    let workflow_path = repo_root.join("workflow.yaml");
    fs::write(
        &workflow_path,
        r#"schema: transist.flow.v1
id: plain.demo
version: v1
plugins:
  demo.plugin: 1.0.0
steps:
  - id: run_demo
    uses: process:demo.plugin/run
"#,
    )
    .expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir(&plugin_dir)
        .expect("plain install");
    context
        .compile_flow_file(&workflow_path)
        .expect("plain compile should allow unsigned workflow");
    context
        .prepare_workflow("plain.demo@v1", Some("plain-run"), Default::default())
        .expect("plain prepare should allow unsigned inputs");
}

#[test]
fn trusted_compile_denies_unsigned_workflow() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir(&plugin_dir)
        .expect("plain install");

    let workflow_path = repo_root.join("unsigned.workflow.yaml");
    fs::write(
        &workflow_path,
        r#"schema: transist.flow.v1
id: unsigned.demo
version: v1
plugins:
  demo.plugin: 1.0.0
steps:
  - id: run_demo
    uses: process:demo.plugin/run
"#,
    )
    .expect("write workflow");

    let error = context
        .compile_flow_file_with_trust(&workflow_path, true)
        .expect_err("trusted compile should deny unsigned workflow");
    assert!(
        error
            .to_string()
            .contains("trusted workflows install denied"),
        "actual error: {error}"
    );
}

#[test]
fn trusted_prepare_denies_tampered_workflow_before_dispatch() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    context
        .compile_flow_file_with_trust(&workflow_path, true)
        .expect("trusted workflow installation");
    fs::write(
        &workflow_path,
        r#"schema: transist.flow.v1
id: trusted.demo
version: v1
plugins:
  demo.plugin: 1.0.0
steps:
  - id: run_demo
    uses: process:demo.plugin/run
  - id: extra
    uses: process:demo.plugin/run
"#,
    )
    .expect("tamper workflow");

    let error = context
        .prepare_workflow_with_trust(
            "trusted.demo@v1",
            Some("run-tampered-wf"),
            Default::default(),
            true,
            false,
        )
        .expect_err("tampered workflow should deny");
    assert!(
        error
            .to_string()
            .contains("deterministic pre-dispatch blockers"),
        "unexpected preparation error: {error}"
    );

    assert_trusted_execution_blocker(&context, "trusted.demo@v1", "digest_mismatch")
        .expect("tampered workflow must be a trust blocker");
    assert!(
        !context
            .runs_list()
            .expect("list runs")
            .contains(&"run-tampered-wf".to_string()),
        "denied trusted execution must not create a run"
    );
}

#[test]
fn trusted_prepare_denies_tampered_plugin_before_dispatch() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    context
        .compile_flow_file_with_trust(&workflow_path, true)
        .expect("trusted workflow installation");
    fs::write(
        home_dir.join("plugins/demo.plugin/1.0.0/run.sh"),
        "#!/bin/sh\nprintf 'tampered plugin\\n'\n",
    )
    .expect("tamper installed plugin");

    let error = context
        .prepare_workflow_with_trust(
            "trusted.demo@v1",
            Some("run-tampered-plugin"),
            Default::default(),
            true,
            false,
        )
        .expect_err("tampered plugin should deny");
    assert!(
        error.to_string().contains(
            "plugin_pin_mismatch: package 'demo.plugin@1.0.0' differs from compiled digest"
        ),
        "unexpected preparation error: {error}"
    );

    let validation_error = context
        .validate_bound_workflow("trusted.demo@v1", BTreeMap::new(), true, false)
        .expect_err("tampered plugin must fail immutable validation");
    assert!(
        validation_error.to_string().contains(
            "plugin_pin_mismatch: package 'demo.plugin@1.0.0' differs from compiled digest"
        ),
        "unexpected validation error: {validation_error}"
    );
    assert!(
        !context
            .runs_list()
            .expect("list runs")
            .contains(&"run-tampered-plugin".to_string()),
        "denied trusted execution must not create a run"
    );
}

#[test]
fn persisted_trusted_mode_rechecks_without_repeating_flag() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");
    import_test_trust_root(&home_dir, repo_root).expect("import key");

    let plugin_dir = copy_signed_demo_plugin(repo_root).expect("copy plugin");
    let workflow_path = write_demo_workflow(repo_root).expect("write workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted plugins install");
    context
        .compile_flow_file_with_trust(&workflow_path, true)
        .expect("trusted workflow installation");
    context
        .prepare_workflow_with_trust(
            "trusted.demo@v1",
            Some("persisted-run"),
            Default::default(),
            true,
            false,
        )
        .expect("prepare trusted run");
    fs::write(
        home_dir.join("plugins/demo.plugin/1.0.0/run.sh"),
        "#!/bin/sh\nprintf 'tampered plugin\\n'\n",
    )
    .expect("tamper installed plugin");

    let error = context
        .runs_preflight("persisted-run")
        .expect_err("persisted trusted mode should recheck without flag");
    assert!(
        error.to_string().contains("trusted execution denied"),
        "actual error: {error}"
    );

    let events = context.runs_tail("persisted-run", 20).expect("tail events");
    assert!(events.iter().any(|event| event.kind == "trust.denied"));
}

#[test]
fn legacy_run_state_without_trust_fields_still_parses() {
    let record = transist_core::PersistedRunRecord::from_snapshot(
        transist_core::KernelRunSnapshot::new("legacy-run", "demo", "v1", &["step".to_string()]),
        Some(transist_core::RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: "abc".to_string(),
        }),
        None,
        Default::default(),
        0,
        1,
        1,
    );
    let mut json = serde_json::to_value(record).expect("record json");
    json.as_object_mut()
        .expect("json object")
        .remove("trusted_execution_mode");
    json.as_object_mut()
        .expect("json object")
        .remove("trust_decision");

    let parsed: transist_core::PersistedRunRecord =
        serde_json::from_value(json).expect("legacy record should parse");
    assert!(!parsed.trusted_execution_mode);
    assert!(parsed.trust_decision.is_none());
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn trusted_execution_bypasses_unsigned_v0_workflow_and_test_plugin() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path();
    let home_dir = transist::repo_local_home_dir(repo_root);
    transist::init_home_dir_with_mode(&home_dir, transist::RuntimeOperatingMode::Standalone)
        .expect("init home");

    let plugin_dir = write_unsigned_test_plugin(repo_root).expect("write test plugin");
    let workflow_path = write_v0_workflow(repo_root).expect("write v0 workflow");
    let context = TransistContext::with_home(repo_root, &home_dir);
    context
        .install_plugin_dir_with_trust(&plugin_dir, true)
        .expect("trusted install should bypass test plugin");
    context
        .compile_flow_file(&workflow_path)
        .expect("plain compile should allow unsigned v0 workflow");

    let report = context
        .prepare_workflow_with_trust(
            "bypass.demo@v0",
            Some("bypass-run"),
            Default::default(),
            true,
            false,
        )
        .expect("trusted prepare should bypass unsigned v0 workflow");
    assert_eq!(report.run_id, "bypass-run");

    let state_json =
        serde_json::to_value(context.runs_show("bypass-run").expect("state")).expect("state json");
    assert_eq!(state_json["trust_decision"]["decision"], "bypassed");
    assert_eq!(
        state_json["trust_decision"]["reason_code"],
        "test_version_bypass"
    );
    assert_eq!(
        state_json["trust_decision"]["workflow"]["trust_decision"],
        "bypassed"
    );
    assert_eq!(
        state_json["trust_decision"]["plugins"][0]["trust_decision"],
        "bypassed"
    );
}
