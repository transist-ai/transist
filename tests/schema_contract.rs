// SPDX-License-Identifier: MPL-2.0

use jsonschema::{Draft, Retrieve, Uri, Validator};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;
fn load_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read schema")).expect("parse schema")
}

fn compile_schema(path: &Path) -> Validator {
    compile_repo_schema(path)
}

/// Resolves cross-file `$ref` between committed repo schemas by mapping the
/// canonical `https://transist.dev/schemas/...` `$id` space back to files on
/// disk. This keeps the alias schemas (pure `$ref` to their canonical
/// versioned files) and shared component schemas (e.g. `error.schema.json`)
/// compiling offline without duplicating definitions.
struct RepoSchemaRetriever {
    repo_root: PathBuf,
}

impl Retrieve for RepoSchemaRetriever {
    /// Maps canonical repository schema URIs to local files without network I/O.
    fn retrieve(&self, uri: &Uri<&str>) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let url = Url::parse(uri.as_str())?;
        if url.scheme() != "https" || url.host_str() != Some("transist.dev") {
            return Err(anyhow::anyhow!("unsupported schema URL '{url}'").into());
        }
        let relative = url.path().trim_start_matches('/');
        let file = self.repo_root.join(relative);
        let raw = fs::read_to_string(&file).map_err(|err| {
            anyhow::anyhow!(
                "failed reading referenced schema '{}': {err}",
                file.display()
            )
        })?;
        let document: Value = serde_json::from_str(&raw).map_err(|err| {
            anyhow::anyhow!(
                "invalid JSON in referenced schema '{}': {err}",
                file.display()
            )
        })?;
        Ok(document)
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn compile_repo_schema(path: &Path) -> Validator {
    let document = load_json(path);
    let mut options = jsonschema::options();
    options
        .with_draft(Draft::Draft202012)
        .with_retriever(RepoSchemaRetriever {
            repo_root: repo_root(),
        });
    options.build(&document).unwrap_or_else(|err| {
        panic!(
            "failed compiling schema '{}' at schema path '{}'",
            path.display(),
            err.schema_path
        )
    })
}

fn schema_dir() -> PathBuf {
    repo_root().join("schemas")
}

/// Recursively collects every `*.json` file under `dir` into `out`.
fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
}

/// Validates `instance` against `schema`, panicking with a readable error list
/// on failure. `Validator::validate` returns an iterator that does not
/// implement `Debug`, so the failures must be collected and formatted manually.
fn assert_valid(schema: &Validator, instance: &Value, label: &str) {
    if let Some(errors) = schema.validate(instance).err() {
        let messages: Vec<String> = errors
            .map(|err| format!("at '{}': {}", err.instance_path, err))
            .collect();
        panic!("{label}:\n{}", messages.join("\n"));
    }
}

fn install_shell_plugin(home_dir: &Path) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml");
    transist_plugins::install_plugin_file_in_home(home_dir, &manifest).expect("install shell");
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, contents).expect("write file");
}

fn admin_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("admin_tooling")
}

#[test]
fn workflow_explanation_schema_matches_compiled_and_compared_public_views() {
    use std::collections::BTreeMap;
    let path =
        repo_root().join("crates/transist-compiler/tests/fixtures/explanation/workflow.yaml");
    let mut view = transist::explain_workflow_file(None, &path, &BTreeMap::new(), None).unwrap();
    view.comparison =
        Some(transist_compiler::explanation::compare_explanations(&view, &view).unwrap());
    let schema =
        compile_repo_schema(&schema_dir().join("runtime/workflow-explanation.v1.schema.json"));
    let mut value = serde_json::to_value(view).unwrap();
    assert_valid(&schema, &value, "compiled explanation");
    value["steps"]["root/steps/review"]["gate"]["decision_state"] = "approved".into();
    assert!(
        !schema.is_valid(&value),
        "static explanations cannot fabricate approval"
    );
    let golden = load_json(
        &repo_root()
            .join("crates/transist-compiler/tests/fixtures/explanation/explanation.golden.json"),
    );
    assert_valid(&schema, &golden, "explanation golden");
}

#[test]
fn workflow_explanation_resolved_action_uses_authoritative_plugin_schema_constraints() {
    use serde_json::json;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let package = home.join("plugins/shell/0.1.0");
    let source = repo_root().join("plugins/shell");
    let manifest: transist_plugins::PluginManifest =
        yaml_serde::from_str(&fs::read_to_string(source.join("plugin.yaml")).unwrap()).unwrap();
    // Setup a real pinned package without installation, configuration or probes.
    for name in
        std::iter::once("plugin.yaml").chain(manifest.package.files.iter().map(String::as_str))
    {
        let to = package.join(name);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(source.join(name), to).unwrap();
    }
    let path = temp.path().join("workflow.yaml");
    write_file(
        &path,
        "schema: transist.flow.v1\nid: explanation.schema\nversion: 0.1.0\nplugins: {shell: 0.1.0}\nsteps:\n  - id: call\n    uses: process:shell/exec_with_secret_env\n    with: {secret_refs: [MY_API_TOKEN]}\n",
    );
    let explanation =
        transist::explain_workflow_file(Some(&home), &path, &Default::default(), None).unwrap();
    let value = serde_json::to_value(explanation).unwrap();
    assert_eq!(
        value["steps"]["root/steps/call"]["contract_status"],
        "resolved"
    );
    let schema =
        compile_repo_schema(&schema_dir().join("runtime/workflow-explanation.v1.schema.json"));
    assert_valid(&schema, &value, "actual resolved action contract");
    let canonical = load_json(&schema_dir().join("runtime/workflow-explanation.v1.schema.json"));
    assert_eq!(
        canonical["$defs"]["step"]["properties"]["contract"]["properties"]["action"]["$ref"],
        "../plugin/plugin-manifest.schema.json#/$defs/action"
    );
    for (field, invalid) in [
        ("name", json!(123)),
        ("name", json!("")),
        ("input_schema", Value::Null),
        ("input_schema", json!("")),
        ("output_schema", json!([])),
        ("secret_inputs", json!(false)),
        ("bindings", json!("invalid")),
        (
            "secret_inputs",
            json!([{"name": "TOKEN", "required": "yes", "binding": {"type": "env", "name": "TOKEN"}}]),
        ),
        (
            "bindings",
            json!({"transist_prompt_v1": {"inline_field": 123}}),
        ),
        (
            "bindings",
            json!({"transist_gate_decision_v1": {"decision_field": null}}),
        ),
        ("provider_kind", json!("inferred")),
        ("unexpected", json!(true)),
    ] {
        let mut invalid_view = value.clone();
        invalid_view["steps"]["root/steps/call"]["contract"]["action"][field] = invalid;
        assert!(
            !schema.is_valid(&invalid_view),
            "accepted invalid action field {field}"
        );
    }
    assert!(!home.join("installation.json").exists());
}

#[test]
fn revised_authoring_module_exports_and_deferred_report_schemas_are_explicit() {
    let schema = compile_repo_schema(&schema_dir().join("workflow.schema.json"));
    let module = serde_json::json!({"schema":"transist.module.v1", "id":"module", "version":"0.1.0", "outputs":{"value":"{{steps.producer.out.value}}"}, "steps":[{"id":"producer","control":"control.finish"}]});
    assert_valid(&schema, &module, "module exports");
    let mut invalid = module.clone();
    invalid["schema"] = "transist.flow.v1".into();
    assert!(!schema.is_valid(&invalid), "root exports are unsupported");
    invalid = module;
    invalid["outputs"] = serde_json::json!({"child_run_id":"shadow"});
    assert!(
        !schema.is_valid(&invalid),
        "runtime metadata cannot be shadowed"
    );
    let reports =
        compile_repo_schema(&schema_dir().join("runtime/workflow-reports.v1.schema.json"));
    let report = serde_json::json!({"schema":"transist.workflow_readiness.v1", "workflow":"demo@0.1.0", "status":"ready", "evaluation_mode":"live_readiness", "validation_status":"passed", "run_created":false, "checks":[{"code":"deferred_to_dispatch", "status":"deferred_to_dispatch", "step_path":"consumer", "message":"materialization awaits producer"}]});
    assert_valid(&reports, &report, "nonblocking explicit deferral");
    let mut invalid = report;
    invalid["checks"][0]["status"] = "silently_passed".into();
    assert!(!reports.is_valid(&invalid));
}

#[test]
fn document_storage_schemas_validate_real_initial_and_execution_publication_records() {
    use transist_core::ports::{
        DocumentOutput, DocumentStorePort, RunBirthArtifacts, RunStorePort, RunStoreStepExecution,
    };
    use transist_storage_local::{LocalRunStore, LocalStorageConfig};
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    let mut record = transist_core::PersistedRunRecord::from_snapshot(
        transist_core::KernelRunSnapshot::new("run", "demo", "0.1.0", &["produce".into()]),
        None,
        None,
        Default::default(),
        0,
        1,
        1,
    );
    let yaml = "documents:\n  file: {kind: file, max_bytes: 32, initial: seed}\n";
    store
        .create_run_staged(&RunBirthArtifacts {
            record: &record,
            recipe_yaml: yaml,
            run_inputs_json: b"{}",
            provenance_json: b"{}",
            workflow_asset_manifest_json: None,
            workflow_asset_blobs: &[],
        })
        .unwrap();
    store.publish_run_locator(tmp.path(), &record).unwrap();
    let schema_path = schema_dir().join("runtime/managed-documents.v1.schema.json");
    let validate_definition = |definition: &str, value: &Value| {
        let mut document = load_json(&schema_path);
        document["$ref"] = format!("#/$defs/{definition}").into();
        let schema = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .build(&document)
            .unwrap();
        assert_valid(&schema, value, definition);
    };
    let initial = load_json(&tmp.path().join("runs/run_run/documents/initial.json"));
    validate_definition("initialization", &initial);
    validate_definition(
        "heads",
        &serde_json::to_value(store.document_heads("run").unwrap()).unwrap(),
    );
    let step = record.steps.get_mut("produce").unwrap();
    step.status = transist_core::KernelStepStatus::Running;
    step.executions = 1;
    store.update_run_state("run", &record).unwrap();
    let output = tmp.path().join("output");
    fs::write(&output, b"exact content").unwrap();
    let revisions = store
        .prepare_document_revisions(
            "run",
            "produce",
            1,
            &[DocumentOutput {
                document: "file".into(),
                path: output,
            }],
            &transist_core::Redactor::default(),
        )
        .unwrap();
    validate_definition("candidates", &serde_json::to_value(&revisions).unwrap());
    let result: transist_protocol::ExecutionResult = serde_json::from_value(serde_json::json!({
        "type": "tep.invoke.result.v1", "protocol_version": "tep.v1", "invoke_id": "invoke", "run_id": "run", "step_id": "produce", "execution": 1,
        "status": "success", "timing": {"started_at": "2026-10-14T00:00:00Z", "finished_at": "2026-10-14T00:00:01Z", "duration_ms": 1000}, "output": {}
    })).unwrap();
    let completion = transist_core::StepExecutionRecord {
        run_id: "run".into(),
        step_id: "produce".into(),
        execution: 1,
        status: "success".into(),
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        retry_wait_until_ms: None,
        request: None,
        result: Some(result),
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: revisions,
    };
    let value = serde_json::to_value(&completion).unwrap();
    let schema = compile_repo_schema(&schema_dir().join("runtime/step-execution.v1.schema.json"));
    assert_valid(&schema, &value, "execution publication");
    let mut invalid = value.clone();
    invalid["status"] = "cancelled".into();
    assert!(!schema.is_valid(&invalid));
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "run".into(),
            step_id: "produce".into(),
            execution: 1,
            state: value.into(),
        })
        .unwrap();
    assert_valid(
        &schema,
        &serde_json::to_value(
            store
                .load_step_execution("run", "produce", 1)
                .unwrap()
                .unwrap(),
        )
        .unwrap(),
        "loaded immutable execution",
    );
}

#[test]
fn document_storage_workflow_schema_enforces_root_declarations_and_positive_bounds() {
    let schema = compile_repo_schema(&schema_dir().join("workflow.schema.json"));
    let valid = serde_json::json!({"schema":"transist.flow.v1", "id":"demo", "version":"0.1.0", "documents":{"file":{"kind":"file", "max_bytes":32, "initial":"seed"}, "tree":{"kind":"tree", "max_bytes":32, "max_files":3}}, "steps":[{"id":"finish", "control":"control.noop"}]});
    assert_valid(&schema, &valid, "root document declarations");
    let mut invalid = valid.clone();
    invalid["documents"]["tree"]["max_files"] = 0.into();
    assert!(!schema.is_valid(&invalid));
    invalid = valid.clone();
    invalid["documents"]["file"]["max_bytes"] = 0.into();
    assert!(!schema.is_valid(&invalid));
    invalid = valid;
    invalid["schema"] = "transist.module.v1".into();
    assert!(!schema.is_valid(&invalid));
}

#[test]
fn revised_authoring_run_state_retains_nonblocking_deferred_preflight_checks() {
    let snapshot = transist_core::KernelRunSnapshot::new(
        "deferred-run",
        "demo",
        "0.1.0",
        &["consumer".into()],
    );
    let mut record = transist_core::PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        None,
        Default::default(),
        0,
        1,
        1,
    );
    record.preflight = Some(transist_core::PersistedPreflightRecord::from_checks(
        1,
        vec![transist_core::PreflightCheck {
            kind: "deferred_to_dispatch".into(),
            status: transist_core::PreflightStatus::DeferredToDispatch,
            step_ids: vec!["consumer".into()],
            adapter_kind: Some("process".into()),
            plugin_id: Some("demo".into()),
            action: Some("consume".into()),
            message: "full request validation deferred".into(),
        }],
    ));
    assert_eq!(record.preflight.as_ref().unwrap().blocker_count, 0);
    assert_eq!(
        record.preflight.as_ref().unwrap().status,
        transist_core::PreflightStatus::Passed
    );
    assert_valid(
        &compile_repo_schema(&schema_dir().join("runtime/run-state.v1.schema.json")),
        &serde_json::to_value(record).unwrap(),
        "deferred persisted preflight",
    );
}

#[test]
fn workspace_binding_schema_accepts_documented_runtime_supported_shape() {
    let schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/workflow.schema.json"),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_shell_plugin(&home_dir);

    let child_path = tmp.path().join("modules/cleanup.yaml");
    write_file(
        &child_path,
        r#"
schema: transist.module.v1
id: child_cleanup
version: v1
inputs:
  user_name:
    type: string
steps:
  - id: child_finish
    control: control.finish
"#,
    );
    let workflow_path = tmp.path().join("workflow.yaml");
    let workflow_yaml = r#"
schema: transist.flow.v1
id: schema_contract_demo
version: v1
plugins:
  shell: 0.1.0
modules:
  cleanup:
    path: modules/cleanup.yaml
prerequisites:
  env:
    - name: HOME
      required: true
inputs:
  user_name:
    type: string
    required: true
    default: "guest"
filesystem_policy:
  workspace_access:
    mode: read_only
network:
  mode: allowlist
  allow_hosts: [api.example.com]
  allow_ports: [443]
  tls_required: true
  max_redirects: 4
steps:
  - id: loop_control
    control: control.noop
    loop:
      body:
        - ref: invoke_step
      until: vars.done == "yes"
      max_iters: 2
  - id: invoke_step
    uses: process:shell/echo
    filesystem_policy:
      workspace_access:
        mode: read_only
      read_allow_paths: ["/etc/gitconfig"]
    network:
      mode: loopback
    with:
      args: ["hello"]
    set:
      done: "yes"
  - id: nested
    module:
      ref: cleanup
      inputs:
        user_name: "{{inputs.user_name}}"
      rerun_mode: reuse
cleanup:
  - id: cleanup_step
    when: [failed]
    uses: process:shell/exec
    filesystem_policy:
      workspace_access:
        mode: full
    network:
      mode: none
    with:
      command: sh
      args: ["-c", "printf cleanup"]
"#;
    write_file(&workflow_path, workflow_yaml);

    let workflow_json: serde_json::Value =
        yaml_serde::from_str(workflow_yaml).expect("yaml to json");
    let result = schema.validate(&workflow_json);
    assert!(
        result.is_ok(),
        "schema errors: {:?}",
        result
            .err()
            .map(|errs| errs.map(|e| e.to_string()).collect::<Vec<_>>())
    );

    let flow = transist_compiler::load_flow(&workflow_path).expect("load flow");
    transist_compiler::validate_flow_in_home(&home_dir, &flow).expect("rust validate");
}

#[test]
fn workflow_schema_accepts_governed_declarations_and_rejects_version_ranges() {
    let schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/workflow.schema.json"),
    );
    let mut workflow = serde_json::json!({
        "schema": "transist.flow.v1",
        "id": "governed_contract",
        "version": "v2",
        "plugins": {"shell": "0.1.0"},
        "inputs": {"repo_path": {"type": "string", "required": true}},
        "context": {"ticket_root": "run://example/{{inputs.repo_path}}"},
        "artifacts": {"result": {"scope": "repository", "path": "result.json"}},
        "profiles": {
            "echo": {"uses": "process:shell/echo", "with": {"args": ["hello"]}}
        },
        "steps": [{"id": "echo", "uses": "process:shell/echo", "profile": "echo"}]
    });
    assert_valid(&schema, &workflow, "governed declarations");

    workflow["plugins"]["shell"] = serde_json::json!("^1.4");
    assert!(schema.validate(&workflow).is_err());
}

#[test]
fn workflow_schema_accepts_gate_decision_only_on_invoke_steps() {
    let schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/workflow.schema.json"),
    );
    let mut workflow = serde_json::json!({
        "schema": "transist.flow.v1",
        "id": "gate_binding_contract",
        "version": "v1",
        "profiles": {
            "agent": {"uses": "process:agent/execute"}
        },
        "steps": [
            {
                "id": "implement",
                "uses": "process:agent/execute",
                "gate_decision": {"step": "review"}
            },
            {
                "id": "review",
                "control": "control.gate.manual"
            }
        ]
    });

    assert_valid(&schema, &workflow, "invoke-step gate decision binding");

    workflow["profiles"]["agent"]["gate_decision"] = serde_json::json!({"step": "review"});
    assert!(
        schema.validate(&workflow).is_err(),
        "profiles must reject the invoke-only gate_decision field"
    );
}

#[test]
fn workflow_schema_rejects_stale_invalid_shapes() {
    let schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/workflow.schema.json"),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_shell_plugin(&home_dir);

    let invalid_yaml = r#"
schema: transist.flow.v1
id: invalid_workflow
version: v1
inputs:
  user_name:
    type: string
    default: 42
steps:
  - id: bad_control
    control: control.finish
    loop:
      body:
        - ref: bad_control
      until: vars.done == "yes"
      max_iters: 2
"#;
    let invalid_json: serde_json::Value = yaml_serde::from_str(invalid_yaml).expect("yaml to json");
    assert!(
        schema.validate(&invalid_json).is_err(),
        "schema should reject stale shape"
    );

    let workflow_path = tmp.path().join("invalid.yaml");
    write_file(&workflow_path, invalid_yaml);
    let flow = transist_compiler::load_flow_unvalidated(&workflow_path).expect("load flow");
    assert!(transist_compiler::validate_flow_in_home(&home_dir, &flow).is_err());
}

#[test]
fn workspace_binding_validator_rejects_custom_filesystem_mode_with_dedicated_reason_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    install_shell_plugin(&home_dir);

    let custom_yaml = r#"
schema: transist.flow.v1
id: custom_mode_workflow
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: invoke_step
    uses: process:shell/echo
    filesystem_policy:
      workspace_access:
        mode: custom
    with:
      args: ["hello"]
"#;
    let workflow_path = tmp.path().join("custom.yaml");
    write_file(&workflow_path, custom_yaml);
    let flow = transist_compiler::load_flow_unvalidated(&workflow_path).expect("load flow");
    let err = transist_compiler::validate_flow_in_home(&home_dir, &flow)
        .expect_err("custom mode must be rejected");
    assert!(
        err.to_string()
            .contains("filesystem_custom_mode_unsupported"),
        "expected dedicated reason code, got: {err}"
    );

    // The published JSON Schema must also reject custom mode.
    let schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/workflow.schema.json"),
    );
    let custom_json: serde_json::Value = yaml_serde::from_str(custom_yaml).expect("yaml to json");
    let schema_err = schema
        .validate(&custom_json)
        .expect_err("schema should reject custom");
    let schema_errors: Vec<String> = schema_err.map(|e| e.to_string()).collect();
    assert!(
        schema_errors
            .iter()
            .any(|e| e.contains("custom") || e.contains("enum")),
        "expected schema to reject custom mode, got: {schema_errors:?}"
    );
}

#[test]
fn workspace_binding_validator_rejects_custom_filesystem_mode_on_control_step() {
    let flow_yaml = r#"
schema: transist.flow.v1
id: custom_control_step
version: v1
steps:
  - id: control_step
    control: control.noop
    filesystem_policy:
      workspace_access:
        mode: custom
"#;
    let tmp = tempfile::tempdir().expect("tempdir");
    let workflow_path = tmp.path().join("custom_control.yaml");
    write_file(&workflow_path, flow_yaml);
    let flow = transist_compiler::load_flow_unvalidated(&workflow_path).expect("load flow");
    let err = transist_compiler::validate_flow(&flow)
        .expect_err("custom on control step must be rejected");
    assert!(
        err.to_string()
            .contains("filesystem_custom_mode_unsupported"),
        "expected dedicated reason code on control step, got: {err}"
    );
}

#[test]
fn runtime_temp_plugin_manifest_schema_accepts_documented_runtime_supported_shape() {
    let schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("schemas/plugin/plugin-manifest.schema.json"),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    write_file(&tmp.path().join("schemas/input.json"), "{}");
    write_file(&tmp.path().join("schemas/output.json"), "{}");
    write_file(&tmp.path().join("plugin.sh"), "#!/bin/sh\nexit 0\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(tmp.path().join("plugin.sh"))
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(tmp.path().join("plugin.sh"), perms).expect("chmod");
    }

    let manifest_yaml = r#"
schema: transist.plugin.v1
id: mcp.prompt
version: 1.0.0
adapter: mcp
entrypoint:
  type: mcp
  command: plugin.sh
  env:
    CACHE_DIR: runtime://temp/provider/cache
    TMPDIR: runtime://temp/provider/tmp
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
prerequisites:
  binaries:
    - name: sh
      required: true
default_provider_kind: local
actions:
  - name: ask
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    provider_kind: external
    prerequisites:
      env:
        - name: HOME
          required: true
    secret_inputs:
      - name: API_TOKEN
        required: true
        binding:
          type: env
          name: API_TOKEN
    bindings:
      tool_name: ask_tool
      transist_prompt_v1:
        inline_field: prompt
        file_field: prompt_file
      transist_skills_v1:
        bundle_field: skills_bundle
        rendered_field: skills_rendered
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#;
    let manifest_json: serde_json::Value =
        yaml_serde::from_str(manifest_yaml).expect("yaml to json");
    let result = schema.validate(&manifest_json);
    assert!(
        result.is_ok(),
        "schema errors: {:?}",
        result
            .err()
            .map(|errs| errs.map(|e| e.to_string()).collect::<Vec<_>>())
    );

    let manifest_path = tmp.path().join("plugin.yaml");
    write_file(&manifest_path, manifest_yaml);
    transist_plugins::load_manifest_file(&manifest_path).expect("rust validate");
}

#[test]
fn plugin_manifest_schema_rejects_stale_invalid_shapes() {
    let schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("schemas/plugin/plugin-manifest.schema.json"),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    write_file(&tmp.path().join("schemas/input.json"), "{}");
    write_file(&tmp.path().join("schemas/output.json"), "{}");
    write_file(&tmp.path().join("plugin.sh"), "#!/bin/sh\nexit 0\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(tmp.path().join("plugin.sh"))
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(tmp.path().join("plugin.sh"), perms).expect("chmod");
    }

    let invalid_manifest_yaml = r#"
schema: transist.plugin.v1
id: broken.plugin
version: 1.0.0
adapter: http
entrypoint:
  type: process
  command: plugin.sh
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: ask
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#;
    let invalid_manifest_json: serde_json::Value =
        yaml_serde::from_str(invalid_manifest_yaml).expect("yaml to json");
    assert!(
        schema.validate(&invalid_manifest_json).is_err(),
        "schema should reject stale manifest shape"
    );

    let manifest_path = tmp.path().join("plugin.yaml");
    write_file(&manifest_path, invalid_manifest_yaml);
    assert!(transist_plugins::load_manifest_file(&manifest_path).is_err());
}

#[test]
fn trust_root_and_signature_schemas_accept_committed_runtime_contract_examples() {
    let trust_root_schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/trust/trust-root.v1.schema.json"),
    );
    let signature_schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/trust/signature.v1.schema.json"),
    );
    let fixture_root = admin_fixture_root();

    let trust_root: transist_core::TrustRootRecord =
        serde_json::from_value(load_json(&fixture_root.join("test-org.pub")))
            .expect("deserialize trust root");
    let trust_root = serde_json::to_value(trust_root).expect("serialize trust root");
    assert!(trust_root_schema.validate(&trust_root).is_ok());

    let workflow_sig: transist_core::SignatureEnvelope =
        serde_json::from_value(load_json(&fixture_root.join("demo.workflow.yaml.sig")))
            .expect("deserialize workflow signature");
    let workflow_sig = serde_json::to_value(workflow_sig).expect("serialize workflow signature");
    assert!(signature_schema.validate(&workflow_sig).is_ok());

    let plugin_sig: transist_core::SignatureEnvelope =
        serde_json::from_value(load_json(&fixture_root.join("demo-plugin.sig")))
            .expect("deserialize plugin signature");
    let plugin_sig = serde_json::to_value(plugin_sig).expect("serialize plugin signature");
    assert!(signature_schema.validate(&plugin_sig).is_ok());
}

#[test]
fn trust_root_and_signature_schemas_reject_stale_invalid_shapes() {
    let trust_root_schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/trust/trust-root.v1.schema.json"),
    );
    let signature_schema = compile_schema(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/trust/signature.v1.schema.json"),
    );

    let invalid_trust_root = serde_json::json!({
        "schema": "transist.trust_root.v1",
        "name": "",
        "key_id": "not-hex",
        "public_key": ""
    });
    assert!(trust_root_schema.validate(&invalid_trust_root).is_err());

    let invalid_signature = serde_json::json!({
        "schema": "transist.signature.v1",
        "sig_alg": "ed25519",
        "digest_alg": "sha256",
        "subject": {
            "kind": "run",
            "id": "demo.workflow",
            "version": "v1",
            "digest": "short"
        },
        "key_id": "not-hex",
        "signed_at_ms": -1,
        "signature": ""
    });
    assert!(signature_schema.validate(&invalid_signature).is_err());
}

#[test]
fn tep_invoke_schemas_accept_canonical_capability_requests() {
    let root = schema_dir().join("tep");
    let v1 = compile_repo_schema(&root.join("invoke.v1.schema.json"));
    let alias = compile_repo_schema(&root.join("invoke.schema.json"));
    let request = serde_json::json!({
        "type": "tep.invoke.request.v1",
        "protocol_version": "tep.v1",
        "invoke_id": "invoke-1",
        "run_id": "run-1",
        "workflow_id": "demo",
        "step": {"id": "invoke", "execution": 1},
        "adapter_kind": "process",
        "action": "exec",
        "input": {},
        "paths": {
            "run_root": "/tmp/run",
            "artifacts_dir": "/tmp/run/artifacts",
            "evidence_dir": "/tmp/run/evidence",
            "workspace_dir": "/tmp/workspace"
        },
        "requested_capabilities": [{
            "capability": "adapter",
            "resource": "process",
            "operations": ["invoke"]
        }],
        "timeout_ms": null,
        "max_timeout_ms": null,
        "policy": {},
        "metadata": {}
    });
    assert!(v1.validate(&request).is_ok());
    assert!(alias.validate(&request).is_ok());
}

#[test]
fn tep_and_app_schemas_validate_representative_serialized_protocol_payloads() {
    use transist_protocol as proto;

    let app = schema_dir().join("app");
    let tep = schema_dir().join("tep");

    let handshake_req = proto::AppHandshakeRequest {
        message_type: proto::APP_HANDSHAKE_REQUEST_TYPE_V1.to_string(),
        protocol_version: proto::APP_PROTOCOL_VERSION_V1.to_string(),
    };
    assert_valid(
        &compile_repo_schema(&app.join("handshake.request.v1.schema.json")),
        &serde_json::to_value(&handshake_req).expect("serialize"),
        "app handshake request validates",
    );

    let handshake_resp = proto::AppHandshakeResponse {
        message_type: proto::APP_HANDSHAKE_RESPONSE_TYPE_V1.to_string(),
        protocol_version: proto::APP_PROTOCOL_VERSION_V1.to_string(),
        plugin: proto::AppPluginIdentity {
            id: "shell".to_string(),
            version: "0.1.0".to_string(),
        },
        adapter_kind: proto::AdapterKind::Process,
        capabilities: proto::AppCapabilities {
            actions: vec!["exec".to_string()],
            supports_cancel: true,
            supports_health: false,
            supports_artifact_hints: false,
            supports_evidence_hints: false,
        },
    };
    assert_valid(
        &compile_repo_schema(&app.join("handshake.response.v1.schema.json")),
        &serde_json::to_value(&handshake_resp).expect("serialize"),
        "app handshake response validates",
    );

    let invoke_req = proto::AppInvokeRequest {
        message_type: proto::APP_INVOKE_REQUEST_TYPE_V1.to_string(),
        protocol_version: proto::APP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
        run_id: "run_1".to_string(),
        workflow_id: "workflow_1".to_string(),
        step: proto::AppInvokeStep {
            id: "step_1".to_string(),
            execution: 1,
        },
        plugin: proto::AppPluginIdentity {
            id: "shell".to_string(),
            version: "0.1.0".to_string(),
        },
        action: "exec".to_string(),
        input: serde_json::json!({"command": "echo hi"}),
        paths: proto::InvokePaths {
            run_root: "/tmp/run".to_string(),
            artifacts_dir: "/tmp/run/artifacts".to_string(),
            evidence_dir: "/tmp/run/evidence".to_string(),
            workspace_dir: "/tmp/workspace".to_string(),
        },
        requested_capabilities: vec![
            proto::CapabilityRequest::new(
                proto::CapabilityName::Action,
                "shell@0.1.0/exec",
                ["invoke"],
            )
            .expect("capability"),
        ],
        policy: serde_json::json!({}),
        timeout_ms: Some(1_000),
        max_timeout_ms: Some(2_000),
        metadata: serde_json::json!({}),
    };
    assert_valid(
        &compile_repo_schema(&app.join("invoke.request.v1.schema.json")),
        &serde_json::to_value(&invoke_req).expect("serialize"),
        "app invoke request validates",
    );

    let invoke_result = proto::AppInvokeResult {
        message_type: proto::APP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: proto::APP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
        status: "success".to_string(),
        output: serde_json::json!({"ok": true}),
        artifact_hints: vec![],
        evidence_hints: vec![],
        metrics: [("latency_ms".to_string(), 12.0)].into_iter().collect(),
        usage: Some(proto::InvokeUsage {
            provider: Some("shell".to_string()),
            model: None,
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
        }),
        error: None,
        metrics_validation_errors: 0,
    };
    assert_valid(
        &compile_repo_schema(&app.join("invoke.result.v1.schema.json")),
        &serde_json::to_value(&invoke_result).expect("serialize"),
        "app invoke result validates",
    );

    let progress = proto::AppInvokeProgress {
        message_type: proto::APP_INVOKE_PROGRESS_TYPE_V1.to_string(),
        protocol_version: proto::APP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
        stream: "stdout".to_string(),
        delta_lines: 1,
        line_preview: Some("planning".to_string()),
    };
    assert_valid(
        &compile_repo_schema(&app.join("invoke.progress.v1.schema.json")),
        &serde_json::to_value(&progress).expect("serialize"),
        "app progress validates",
    );

    let cancel_req = proto::AppCancelRequest {
        message_type: proto::APP_CANCEL_REQUEST_TYPE_V1.to_string(),
        protocol_version: proto::APP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
    };
    assert_valid(
        &compile_repo_schema(&app.join("cancel.request.v1.schema.json")),
        &serde_json::to_value(&cancel_req).expect("serialize"),
        "app cancel request validates",
    );
    let cancel_res = proto::AppCancelResult {
        message_type: proto::APP_CANCEL_RESULT_TYPE_V1.to_string(),
        protocol_version: proto::APP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
        accepted: true,
    };
    assert_valid(
        &compile_repo_schema(&app.join("cancel.result.v1.schema.json")),
        &serde_json::to_value(&cancel_res).expect("serialize"),
        "app cancel result validates",
    );

    let tep_handshake_req = proto::HandshakeRequest {
        message_type: proto::TEP_HANDSHAKE_REQUEST_TYPE_V1.to_string(),
        protocol_version: proto::TEP_PROTOCOL_VERSION_V1.to_string(),
    };
    assert_valid(
        &compile_repo_schema(&tep.join("handshake.request.v1.schema.json")),
        &serde_json::to_value(&tep_handshake_req).expect("serialize"),
        "tep handshake request validates",
    );

    let tep_handshake_resp = proto::HandshakeResponse {
        message_type: proto::TEP_HANDSHAKE_RESPONSE_TYPE_V1.to_string(),
        protocol_version: proto::TEP_PROTOCOL_VERSION_V1.to_string(),
        adapter: proto::AdapterIdentity {
            kind: proto::AdapterKind::Process,
            version: "1.0.0".to_string(),
        },
        capabilities: proto::AdapterCapabilities {
            actions: vec!["exec".to_string()],
            supports_cancel: Some(true),
            supports_health: None,
        },
    };
    assert_valid(
        &compile_repo_schema(&tep.join("handshake.response.v1.schema.json")),
        &serde_json::to_value(&tep_handshake_resp).expect("serialize"),
        "tep handshake response validates",
    );

    let tep_invoke = proto::InvokeRequest {
        message_type: proto::TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
        protocol_version: proto::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
        run_id: "run_1".to_string(),
        workflow_id: "workflow_1".to_string(),
        step: proto::InvokeStep {
            id: "step_1".to_string(),
            execution: 1,
        },
        adapter_kind: proto::AdapterKind::Process,
        action: "exec".to_string(),
        input: serde_json::json!({}),
        paths: proto::InvokePaths {
            run_root: "/tmp/run".to_string(),
            artifacts_dir: "/tmp/run/artifacts".to_string(),
            evidence_dir: "/tmp/run/evidence".to_string(),
            workspace_dir: "/tmp/workspace".to_string(),
        },
        requested_capabilities: vec![
            proto::CapabilityRequest::new(proto::CapabilityName::Adapter, "process", ["invoke"])
                .expect("capability"),
        ],
        timeout_ms: None,
        max_timeout_ms: None,
        policy: serde_json::json!({}),
        metadata: serde_json::json!({}),
    };
    let tep_invoke_value = serde_json::to_value(&tep_invoke).expect("serialize");
    assert_valid(
        &compile_repo_schema(&tep.join("invoke.v1.schema.json")),
        &tep_invoke_value,
        "tep invoke request validates against canonical schema",
    );
    assert_valid(
        &compile_repo_schema(&tep.join("invoke.schema.json")),
        &tep_invoke_value,
        "tep invoke request validates against alias schema",
    );

    let tep_result = proto::InvokeResult {
        message_type: proto::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: proto::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke_1".to_string(),
        run_id: "run_1".to_string(),
        step_id: "step_1".to_string(),
        execution: 1,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: "boom".to_string(),
        timing: proto::InvokeTiming {
            started_at: "2024-01-01T00:00:00Z".to_string(),
            finished_at: "2024-01-01T00:00:01Z".to_string(),
            duration_ms: 1000,
        },
        artifacts: vec![],
        evidence: vec![],
        metrics: [].into_iter().collect(),
        output: serde_json::Value::Null,
        metrics_validation_errors: 0,
        usage: None,
        error: Some(proto::ProtocolError::new(
            proto::CanonicalErrorCode::ToolExecutionError,
            "command failed",
        )),
    };
    let tep_result_value = serde_json::to_value(&tep_result).expect("serialize");
    assert_valid(
        &compile_repo_schema(&tep.join("result.v1.schema.json")),
        &tep_result_value,
        "tep invoke result validates against canonical schema",
    );
    assert_valid(
        &compile_repo_schema(&tep.join("result.schema.json")),
        &tep_result_value,
        "tep invoke result validates against alias schema",
    );
}

#[test]
fn tep_error_schema_rejects_unknown_canonical_codes() {
    let schema = compile_repo_schema(&schema_dir().join("tep/error.schema.json"));
    assert!(
        schema
            .validate(&serde_json::json!({
                "code": "unknown_code",
                "class": "transport",
                "message": "x",
                "retryability": "safe"
            }))
            .is_err()
    );
    assert!(
        schema
            .validate(&serde_json::json!({
                "code": "capability_not_granted",
                "class": "policy",
                "message": "denied",
                "retryability": "operator"
            }))
            .is_ok()
    );
}

#[test]
fn run_state_schema_accepts_serialized_records_for_every_variant_and_optional_shape() {
    use transist_core::{AssetLock, AssetLockKind};
    use transist_core::{
        CleanupPhaseState, KernelRunRecoveryState, KernelRunSnapshot, KernelRunStatus,
        KernelSkipReason, KernelStepState, KernelStepStatus, PreflightCheck, PreflightStatus,
        RecipeSnapshotRef, RunLineage, RunLineageMode, TrustDecisionRecord, TrustSubjectDecision,
    };
    use transist_protocol::{
        CanonicalErrorCode, CleanupTrigger, ErrorClass, InvokePaths, InvokeResult, InvokeTiming,
        InvokeUsage, KernelValue, ProtocolError, Retryability,
    };

    let schema = compile_repo_schema(&schema_dir().join("runtime/run-state.v1.schema.json"));

    fn record_with(
        run_status: KernelRunStatus,
        step_status: KernelStepStatus,
        step_executions: u32,
        baseline: u32,
    ) -> transist_core::PersistedRunRecord {
        let mut snapshot = KernelRunSnapshot::new("run-1", "demo", "v1", &["step".to_string()]);
        snapshot.status = run_status;
        let mut step = KernelStepState::pending("step");
        step.status = step_status;
        step.executions = step_executions;
        step.activation_executions_baseline = baseline;
        snapshot.steps.insert("step".to_string(), step);
        transist_core::PersistedRunRecord::from_snapshot(
            snapshot,
            Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
                    .to_string(),
            }),
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "run-1".to_string(),
                parent_run_id: None,
                parent_step_id: None,
            }),
            [(
                "marker".to_string(),
                KernelValue::String("alpha".to_string()),
            )]
            .into_iter()
            .collect(),
            0,
            1,
            2,
        )
    }

    // Every run-status enum variant must serialize into a schema-valid record.
    for run_status in [
        KernelRunStatus::Created,
        KernelRunStatus::Planned,
        KernelRunStatus::Running,
        KernelRunStatus::Waiting,
        KernelRunStatus::Blocked,
        KernelRunStatus::Success,
        KernelRunStatus::Failed,
        KernelRunStatus::Canceled,
    ] {
        let record = record_with(run_status, KernelStepStatus::Pending, 0, 0);
        let value = serde_json::to_value(record).expect("serialize run-state");
        assert_valid(
            &schema,
            &value,
            &format!("run-status variant {run_status:?}"),
        );
    }

    // Every step-status enum variant serializes into a schema-valid record.
    for step_status in [
        KernelStepStatus::Pending,
        KernelStepStatus::Runnable,
        KernelStepStatus::Dispatching,
        KernelStepStatus::Running,
        KernelStepStatus::Blocked,
        KernelStepStatus::Succeeded,
        KernelStepStatus::Failed,
        KernelStepStatus::RetryWait,
        KernelStepStatus::Skipped,
        KernelStepStatus::Canceled,
    ] {
        // Lifetime counters obey `baseline <= executions` (enforced on load).
        let record = record_with(KernelRunStatus::Running, step_status, 3, 2);
        let value = serde_json::to_value(record).expect("serialize run-state");
        assert_valid(
            &schema,
            &value,
            &format!("step-status variant {step_status:?}"),
        );
    }

    // Every skip-reason variant serializes into a schema-valid record.
    for skip_reason in [
        KernelSkipReason::DependencyFailed,
        KernelSkipReason::GuardBlocked,
    ] {
        let mut record = record_with(KernelRunStatus::Running, KernelStepStatus::Skipped, 1, 0);
        record.steps.get_mut("step").expect("step").skip_reason = Some(skip_reason);
        let value = serde_json::to_value(&record).expect("serialize run-state");
        assert_valid(
            &schema,
            &value,
            &format!("skip-reason variant {skip_reason:?}"),
        );
    }

    // Replayed lineage and a populated recovery/cleanup/preflight/trust/asset
    // shape: every optional field-present form validates.
    let mut record = record_with(KernelRunStatus::Running, KernelStepStatus::Succeeded, 2, 2);
    record.lineage = Some(RunLineage {
        mode: RunLineageMode::Replay,
        source_run_id: Some("source-run".to_string()),
        root_run_id: "root-run".to_string(),
        parent_run_id: Some("parent-run".to_string()),
        parent_step_id: Some("parent-step".to_string()),
    });
    record
        .workflow_vars
        .insert("done".to_string(), KernelValue::Bool(true));
    record.recovery_state = Some(KernelRunRecoveryState::CleanupCompleted);
    record.cleanup = Some(transist_core::PersistedCleanupRecord::new(
        CleanupTrigger::Success,
        &["cleanup-a".to_string()],
        CleanupPhaseState::Succeeded,
    ));
    record.preflight = Some(transist_core::PersistedPreflightRecord::from_checks(
        10,
        vec![PreflightCheck {
            kind: "secret_resolution".to_string(),
            status: PreflightStatus::Failed,
            step_ids: vec!["step".to_string()],
            adapter_kind: Some("process".to_string()),
            plugin_id: Some("shell".to_string()),
            action: Some("exec".to_string()),
            message: "ok".to_string(),
        }],
    ));
    record.trust_decision = Some(TrustDecisionRecord {
        schema: transist_core::TRUST_DECISION_SCHEMA_V1.to_string(),
        run_id: "run-1".to_string(),
        trusted_execution_mode: true,
        decision: "allowed".to_string(),
        workflow: TrustSubjectDecision {
            id: "demo".to_string(),
            version: "v1".to_string(),
            digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string(),
            signature_verified: true,
            signer_key_id: Some("abcd".to_string()),
            reason_code: None,
            trust_decision: "verified".to_string(),
        },
        plugins: vec![],
        reason_code: None,
        failed_verification_type: None,
        organization_id: None,
        policy_revision: None,
        decided_at_ms: 5,
    });
    record.asset_locks = vec![AssetLock {
        step_id: "step".to_string(),
        kind: AssetLockKind::Plugin,
        ref_id: "shell".to_string(),
        version: Some("1.0.0".to_string()),
        sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string(),
    }];
    record.asset_locks.push(AssetLock {
        step_id: "cleanup-a".to_string(),
        kind: AssetLockKind::PromptFile,
        ref_id: "run://prompt.txt".to_string(),
        version: None,
        sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string(),
    });
    record.asset_lock_version = 1;
    record.workflow_asset_snapshot = true;
    record.local_only_mode = true;
    record.test_mode = false;
    record.capability_authority_version = 1;
    record
        .trust_decision
        .as_mut()
        .expect("trust decision")
        .decision = "bypassed".to_string();
    record
        .trust_decision
        .as_mut()
        .expect("trust decision")
        .workflow
        .trust_decision = "bypassed".to_string();
    record
        .trust_decision
        .as_mut()
        .expect("trust decision")
        .plugins
        .push(TrustSubjectDecision {
            id: "shell".to_string(),
            version: "1.0.0".to_string(),
            digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string(),
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("signature_missing".to_string()),
            trust_decision: "denied".to_string(),
        });
    let step = record.steps.get_mut("step").expect("step");
    step.last_request = Some(transist_protocol::InvokeRequest {
        message_type: transist_protocol::TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
        protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke-1".to_string(),
        run_id: "run-1".to_string(),
        workflow_id: "demo".to_string(),
        step: transist_protocol::InvokeStep {
            id: "step".to_string(),
            execution: 2,
        },
        adapter_kind: transist_protocol::AdapterKind::Process,
        action: "exec".to_string(),
        input: serde_json::json!({"command": "echo hi"}),
        paths: InvokePaths {
            run_root: "/tmp/run".to_string(),
            artifacts_dir: "/tmp/run/artifacts".to_string(),
            evidence_dir: "/tmp/run/evidence".to_string(),
            workspace_dir: "/tmp/workspace".to_string(),
        },
        requested_capabilities: vec![],
        timeout_ms: Some(1_000),
        max_timeout_ms: Some(2_000),
        policy: serde_json::json!({"mode": "allow"}),
        metadata: serde_json::json!({"source": "schema-test"}),
    });
    step.last_result = Some(InvokeResult {
        message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke-1".to_string(),
        run_id: "run-1".to_string(),
        step_id: "step".to_string(),
        execution: 2,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: "out".to_string(),
        stderr: "err".to_string(),
        timing: InvokeTiming {
            started_at: "2024-01-01T00:00:00Z".to_string(),
            finished_at: "2024-01-01T00:00:01Z".to_string(),
            duration_ms: 1_000,
        },
        artifacts: vec![transist_protocol::FileRef {
            path: "artifacts/result.json".to_string(),
            sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string(),
            bytes: 12,
        }],
        evidence: vec![],
        metrics: [("latency_ms".to_string(), 12.0)].into_iter().collect(),
        output: serde_json::json!({"ok": false}),
        metrics_validation_errors: 0,
        usage: Some(InvokeUsage {
            provider: Some("shell".to_string()),
            model: Some("local".to_string()),
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
        }),
        error: Some(ProtocolError {
            code: CanonicalErrorCode::ToolExecutionError,
            class: ErrorClass::Tool,
            message: "boom".to_string(),
            retryability: Retryability::Safe,
        }),
    });
    let value = serde_json::to_value(&record).expect("serialize run-state");
    assert_valid(
        &schema,
        &value,
        "run-state with every optional field populated",
    );
}

#[test]
fn run_state_schema_rejects_unknown_statuses_and_invalid_envelopes() {
    let schema = compile_repo_schema(&schema_dir().join("runtime/run-state.v1.schema.json"));

    // Unknown run-status, step-status, and enum values fail closed.
    let base = serde_json::json!({
        "schema": "transist.run_state.v1",
        "run_id": "run-1",
        "workflow_id": "demo",
        "workflow_version": "v1",
        "status": "running",
        "steps": {"step": {"step_id": "step", "status": "succeeded", "executions": 1}},
        "workflow_inputs": {},
        "resume_count": 0
    });
    let mut unknown_run_status = base.clone();
    for field in ["asset_lock_version", "capability_authority_version"] {
        for version in [0, 1, 2, 3, u32::MAX] {
            let mut value = base.clone();
            value[field] = serde_json::json!(version);
            assert_eq!(schema.is_valid(&value), version <= 1, "{field}: {version}");
        }
    }
    for invalid_marker in [serde_json::json!(1), serde_json::json!("true"), Value::Null] {
        let mut invalid_snapshot = base.clone();
        invalid_snapshot["workflow_asset_snapshot"] = invalid_marker;
        assert!(schema.validate(&invalid_snapshot).is_err());
    }
    unknown_run_status["status"] = serde_json::json!("exploded");
    assert!(schema.validate(&unknown_run_status).is_err());

    let unknown_step_status = serde_json::json!({
        "schema": "transist.run_state.v1",
        "run_id": "run-1",
        "workflow_id": "demo",
        "workflow_version": "v1",
        "status": "running",
        "steps": {"step": {"step_id": "step", "status": "exploded", "executions": 1}},
        "workflow_inputs": {},
        "resume_count": 0
    });
    assert!(schema.validate(&unknown_step_status).is_err());

    // Unknown `schema` value fails closed.
    let mut wrong_schema = serde_json::json!({
        "schema": "transist.run_state.v2",
        "run_id": "run-1",
        "workflow_id": "demo",
        "workflow_version": "v1",
        "status": "created",
        "steps": {"step": {"step_id": "step", "status": "pending", "executions": 0}},
        "workflow_inputs": {},
        "resume_count": 0
    });
    // A record missing the required top-level fields also fails.
    assert!(schema.validate(&wrong_schema).is_err());
    wrong_schema["schema"] = serde_json::json!("transist.run_state.v1");
    wrong_schema
        .as_object_mut()
        .expect("object")
        .remove("resume_count");
    assert!(schema.validate(&wrong_schema).is_err());
}

#[test]
fn trust_envelopes_cover_every_subject_kind_and_reject_unknown_shapes() {
    let signature_schema =
        compile_repo_schema(&schema_dir().join("trust/signature.v1.schema.json"));
    let trust_root_schema =
        compile_repo_schema(&schema_dir().join("trust/trust-root.v1.schema.json"));

    // Every `transist.signature.v1` subject kind serializes into a valid envelope.
    for kind in ["workflow", "plugin"] {
        let envelope = serde_json::json!({
            "schema": "transist.signature.v1",
            "sig_alg": "ed25519",
            "digest_alg": "sha256",
            "subject": {
                "kind": kind,
                "id": "demo",
                "version": "v1",
                "digest": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
            },
            "key_id": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "signed_at_ms": 1,
            "signature": "c2lnbmF0dXJlLWJ5dGVz"
        });
        assert_valid(
            &signature_schema,
            &envelope,
            &format!("signature subject kind {kind:?}"),
        );
    }

    // An unknown subject kind and an unknown trust-root `schema` fail closed.
    let mut bad_kind = serde_json::json!({
        "schema": "transist.signature.v1",
        "sig_alg": "ed25519",
        "digest_alg": "sha256",
        "subject": {
            "kind": "run",
            "id": "demo",
            "version": "v1",
            "digest": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        },
        "key_id": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "signed_at_ms": 1,
        "signature": "c2lnbmF0dXJlLWJ5dGVz"
    });
    assert!(signature_schema.validate(&bad_kind).is_err());
    // A trust root with a non-64-hex key id fails closed.
    let bad_root = serde_json::json!({
        "schema": "transist.trust_root.v1",
        "name": "acme",
        "key_id": "not-hex",
        "public_key": "QUFBQQ=="
    });
    assert!(trust_root_schema.validate(&bad_root).is_err());
    let _ = &mut bad_kind;
}

#[test]
fn managed_document_binding_schema_enforces_recursive_process_only_action_data() {
    let schema = compile_repo_schema(&schema_dir().join("workflow.schema.json"));
    let mut workflow = serde_json::json!({"schema":"transist.flow.v1","id":"documents","version":"0.1.0","plugins":{"fixture.documents":"0.1.0"},"documents":{"file":{"kind":"file","max_bytes":32}},"steps":[{"id":"invoke","uses":"process:fixture.documents/call","with":{"nested":[{"path":{"$document":"file","access":"read"}}]}}]});
    assert_valid(&schema, &workflow, "managed documents");
    let mut reviewed = workflow.clone();
    reviewed["steps"][0]["with"]["nested"][0]["path"]["reviewed_by"] = "review".into();
    reviewed["steps"].as_array_mut().unwrap().insert(0, serde_json::json!({"id":"review","control":"control.gate.manual","with":{"review_documents":["file"]}}));
    assert_valid(&schema, &reviewed, "manual gate document review");
    for names in [
        serde_json::json!("file"),
        serde_json::json!(["file", "file"]),
        serde_json::json!(["{{inputs.name}}"]),
        serde_json::json!([1]),
    ] {
        let mut bad = reviewed.clone();
        bad["steps"][0]["with"]["review_documents"] = names;
        assert!(schema.validate(&bad).is_err());
    }
    for access in ["write", "update"] {
        workflow["steps"][0]["with"]["nested"][0]["path"]["access"] = access.into();
        assert_valid(&schema, &workflow, "document access");
    }
    for data in [
        serde_json::json!({"path":{"$document":"file","access":"write","reviewed_by":"gate"}}),
        serde_json::json!({"nested":[{"command":{"$document":"file","access":"read"}}]}),
        serde_json::json!({"path":{"$document":"file","access":"invalid"}}),
        serde_json::json!({"repo_path":{"$document":"file","access":"read"}}),
    ] {
        let mut bad = workflow.clone();
        bad["steps"][0]["with"] = data;
        assert!(schema.validate(&bad).is_err());
    }
    for adapter in ["http", "mcp"] {
        let mut bad = workflow.clone();
        bad["steps"][0]["uses"] = format!("{adapter}:fixture.documents/call").into();
        assert!(schema.validate(&bad).is_err());
        bad["steps"][0]["with"] = serde_json::json!({"ordinary":{"data":[true, null]}});
        assert_valid(&schema, &bad, "ordinary HTTP/MCP data");
    }
    let mut control = workflow.clone();
    control["steps"][0].as_object_mut().unwrap().remove("uses");
    control["steps"][0]["control"] = "control.gate.manual".into();
    assert!(schema.validate(&control).is_err());
    let metadata = transist_protocol::DocumentBindingLocation {
        path: "/nested/0/path".into(),
        binding: transist_protocol::DocumentBinding {
            reviewed_by: None,
            document: "file".into(),
            access: transist_protocol::DocumentAccess::Read,
        },
    };
    let mut location_schema =
        load_json(&schema_dir().join("runtime/managed-documents.v1.schema.json"));
    location_schema["$ref"] = "#/$defs/bindingLocation".into();
    let location_schema = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&location_schema)
        .unwrap();
    let mut metadata = serde_json::to_value(metadata).unwrap();
    assert_valid(&location_schema, &metadata, "compiled document location");
    metadata["path"] = "".into();
    assert_valid(&location_schema, &metadata, "whole action-data location");
    metadata["binding"]["reviewed_by"] = "review".into();
    assert_valid(&location_schema, &metadata, "reviewed read binding");
}

#[test]
fn document_review_request_and_decision_schema_require_exact_revision_metadata() {
    let schema = compile_repo_schema(&schema_dir().join("runtime/gate-review.v1.schema.json"));
    let revision = transist_protocol::DocumentRevision {
        schema: transist_protocol::DocumentRevision::SCHEMA.into(),
        revision_id: "a".repeat(64),
        root_run_id: "root".into(),
        document: "notes".into(),
        run_id: "root".into(),
        step_id: "producer".into(),
        execution: 1,
        commit_sequence: 1,
        kind: transist_protocol::DocumentKind::File,
        blob: transist_protocol::DocumentBlob {
            sha256: "b".repeat(64),
            size_bytes: 7,
        },
        size_bytes: 7,
        file_count: 1,
    };
    let decision = transist_protocol::GateDecisionRecord {
        schema: "transist.gate_decision.v1".into(),
        run_id: "child".into(),
        gate_id: "review".into(),
        step_id: "gate".into(),
        decision: "approved".into(),
        actor: Some("fixture".into()),
        actor_source: None,
        reason: Some("test".into()),
        decided_at_ms: Some(1),
        review_request: Some(transist_protocol::GateReviewRequest {
            schema: "transist.gate_review_request.v1".into(),
            run_id: "child".into(),
            gate_id: "review".into(),
            step_id: "gate".into(),
            execution: 2,
            review_documents: vec![revision],
        }),
    };
    let value = serde_json::to_value(decision).unwrap();
    assert_valid(&schema, &value, "document review decision");
    for field in [
        "review_documents",
        "execution",
        "run_id",
        "step_id",
        "gate_id",
    ] {
        let mut bad = value.clone();
        bad["review_request"].as_object_mut().unwrap().remove(field);
        assert!(schema.validate(&bad).is_err(), "{field}");
    }
    for field in ["revision_id", "blob", "root_run_id", "size_bytes"] {
        let mut bad = value.clone();
        bad["review_request"]["review_documents"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(schema.validate(&bad).is_err(), "{field}");
    }
}

#[test]
fn review_step4_document_temp_namespace_is_reserved_in_both_manifest_schemas() {
    let schema = compile_repo_schema(&schema_dir().join("plugin/plugin-manifest.schema.json"));
    let source = fs::read_to_string(
        repo_root().join(".agents/testing/e2e_suites/managed-documents/plugin/plugin.yaml"),
    )
    .unwrap();
    let original: Value = yaml_serde::from_str(&source).unwrap();
    for adapter in ["process", "mcp"] {
        let mut manifest = original.clone();
        manifest["adapter"] = adapter.into();
        manifest["entrypoint"]["type"] = adapter.into();
        for value in [
            "runtime://temp",
            "runtime://temp/provider/cache",
            "runtime://temp/documents-other",
            "/literal/cache",
        ] {
            manifest["entrypoint"]["env"] = serde_json::json!({"CACHE_DIR":value});
            assert_valid(&schema, &manifest, "valid provider namespace");
        }
        for value in [
            "runtime://temp/documents",
            "runtime://temp/Documents/read/anything",
            "runtime://temp/documents/write/binding-00000000000000000009/injected",
            "runtime://temp/cache/../documents",
            "runtime://temp/%64ocuments/read",
        ] {
            manifest["entrypoint"]["env"] = serde_json::json!({"CACHE_DIR":value});
            assert!(!schema.is_valid(&manifest), "{adapter}: accepted {value}");
        }
    }
}

#[test]
fn every_committed_json_schema_compiles() {
    // Deterministic exhaustive gate: every committed JSON Schema document (a
    // `*.json` file under `schemas/` carrying a top-level `$schema` key) must
    // semantically compile with `jsonschema`. Adding a semantically invalid
    // schema fails `cargo test`, not just the structural `$schema` check in
    // `check_schema_api_contracts.sh`.
    let mut files = Vec::new();
    collect_json_files(&schema_dir(), &mut files);

    let mut schema_files: Vec<&PathBuf> = files
        .iter()
        .filter(|path| {
            load_json(path)
                .as_object()
                .map(|obj| obj.contains_key("$schema"))
                .unwrap_or(false)
        })
        .collect();
    schema_files.sort();

    assert!(
        !schema_files.is_empty(),
        "expected at least one JSON Schema document under schemas/"
    );

    for path in schema_files {
        compile_repo_schema(path);
    }
}
