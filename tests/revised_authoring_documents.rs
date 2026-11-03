// SPDX-License-Identifier: MPL-2.0
//! Contract B through actual compiler, APP, sandbox and immutable local records.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Once},
};
use transist::{KernelValue, RuntimeOperatingMode, TransistContext, init_home_dir_with_mode};

struct Secrets;
impl transist_core::SecretProviderPort for Secrets {
    fn get_secret(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok((name == "DOCUMENT_TEST_SECRET")
            .then(|| "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()))
    }
    fn has_secret(&self, name: &str) -> anyhow::Result<bool> {
        Ok(name == "DOCUMENT_TEST_SECRET")
    }
}
struct Overlay;
impl transist::RuntimeServiceOverlay for Overlay {
    fn apply(
        &self,
        input: transist::RuntimeServiceOverlayInput,
    ) -> anyhow::Result<transist::RuntimeServiceOverlayOutput> {
        Ok(transist::RuntimeServiceOverlayOutput {
            policy: input.policy,
            secrets: Arc::new(Secrets),
        })
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
    context: TransistContext,
}

const DOCUMENTS: &str = "documents:\n  file: {kind: file, max_bytes: 4096, initial: 'initial'}\n  other: {kind: file, max_bytes: 4096}\n  tree: {kind: tree, max_bytes: 4096, max_files: 3}\n";

impl Fixture {
    fn new() -> Self {
        Self::with_literal_names("home", "workspace")
    }
    fn with_literal_names(home_name: &str, workspace_name: &str) -> Self {
        static INIT: Once = Once::new();
        INIT.call_once(|| transist::set_runtime_service_overlay(Arc::new(Overlay)).unwrap());
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let workspace = root.join(workspace_name);
        let home = root.join(home_name);
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("source.txt"), "outside bytes").unwrap();
        init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).unwrap();
        fs::write(home.join("config.yaml"), "version: v1\nruntime: {mode: standalone}\npolicy:\n  profile: permissive\n  remote_secret_delivery: {process: true, http: false, mcp: false}\n").unwrap();
        let context = TransistContext::with_home(&workspace, &home);
        let plugin = root.join("plugin");
        fs::create_dir(&plugin).unwrap();
        for name in [
            "plugin.yaml",
            "plugin.py",
            "input.json",
            "strict.json",
            "typed.json",
            "output.json",
        ] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(".agents/testing/e2e_suites/managed-documents/plugin")
                    .join(name),
                plugin.join(name),
            )
            .unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(plugin.join("plugin.py"), fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        context
            .install_plugin_file(&plugin.join("plugin.yaml"))
            .unwrap();
        Self {
            _temp: temp,
            root,
            workspace,
            home,
            context,
        }
    }
    fn flow(&self, extra: &str, steps: &str) -> PathBuf {
        let path = self.root.join("workflow.yaml");
        fs::write(&path, format!("schema: transist.flow.v1\nid: documents\nversion: 0.1.0\nplugins: {{fixture.documents: 0.1.0}}\nworkspace: {{path: '{}', access: read_only}}\n{DOCUMENTS}{extra}steps:\n{steps}", self.workspace.display())).unwrap();
        path
    }
    fn compile(&self, extra: &str, steps: &str) -> transist::Recipe {
        let recipe = self
            .context
            .compile_flow_file(&self.flow(extra, steps))
            .unwrap();
        yaml_serde::from_str(&fs::read_to_string(recipe).unwrap()).unwrap()
    }
    fn prepare(&self, id: &str) {
        let report = self
            .context
            .prepare_workflow("documents@0.1.0", Some(id), BTreeMap::new())
            .unwrap();
        assert_eq!(report.status, "planned");
    }
    fn run(&self, id: &str, expected: &str) {
        self.prepare(id);
        let report = self.context.run_started_run(id).unwrap();
        assert_eq!(
            report.status,
            expected,
            "{}",
            serde_json::to_string_pretty(&self.state(id)).unwrap()
        );
    }
    fn run_root(&self, id: &str) -> PathBuf {
        self.context.run_root(id).unwrap()
    }
    fn state(&self, id: &str) -> Value {
        read_json(&self.run_root(id).join("state.json"))
    }
    fn execution(&self, id: &str, step: &str) -> Value {
        read_json(
            &self
                .run_root(id)
                .join(format!("step_executions/{step}/execution_001.json")),
        )
    }
    fn heads(&self, id: &str) -> Value {
        read_json(&self.run_root(id).join("documents/heads.json"))
    }
    fn bytes(&self, id: &str, revision: &Value) -> Vec<u8> {
        fs::read(
            self.run_root(id)
                .join("documents/blobs")
                .join(revision["blob"]["sha256"].as_str().unwrap()),
        )
        .unwrap()
    }
}
fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn step(id: &str, data: &str) -> String {
    format!("  - id: {id}\n    uses: process:fixture.documents/call\n    with: {data}\n")
}

#[test]
fn runtime_metadata_capture_uses_actual_root_and_child_ids_without_private_paths_or_secrets() {
    let f = Fixture::new();
    fs::write(f.root.join("identity.yaml"), "schema: transist.module.v1\nid: identity\nversion: 0.1.0\ninputs:\n  expected: {type: string, required: true}\nsteps:\n  - id: capture_child\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: file, access: read}, op: read, content: '{{inputs.expected}}'}, {path: {$document: other, access: write}, op: runtime_metadata}]}\n").unwrap();
    f.compile(
        "modules: {identity: {path: identity.yaml}}\ncleanup:\n  - id: capture_cleanup\n    when: [success]\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: file, access: read}, op: read, content: '{{steps.capture.out.value}}'}]}\n",
        &(step("capture", "{secret_refs: [DOCUMENT_TEST_SECRET], ops: [{path: {$document: file, access: write}, op: runtime_metadata}]}")
            + "  - id: child\n    module:\n      ref: identity\n      inputs: {expected: '{{steps.capture.out.value}}'}\n"),
    );
    for id in ["metadata-first", "metadata-second"] {
        f.run(id, "success");
        let heads = f.heads(id);
        let root: Value = serde_json::from_slice(&f.bytes(id, &heads["file"])).unwrap();
        assert_eq!(
            root,
            json!({
                "runtime": {"version": env!("CARGO_PKG_VERSION")},
                "workflow": {"id": "documents"}, "run": {"id": id},
                "plugins": [{"name": "fixture.documents", "version": "0.1.0"}]
            })
        );
        let child_id = heads["other"]["run_id"].as_str().unwrap();
        assert_ne!(child_id, id);
        let child: Value = serde_json::from_slice(&f.bytes(id, &heads["other"])).unwrap();
        assert_eq!(child["run"], json!({"id": child_id}));
        let child_state = f.state(child_id);
        assert_eq!(child["workflow"]["id"], child_state["workflow_id"]);
        let secret =
            transist_core::SecretProviderPort::get_secret(&Secrets, "DOCUMENT_TEST_SECRET")
                .unwrap()
                .unwrap();
        let redactor = transist_core::Redactor::from_secrets([secret.as_str()]);
        for path in collect_files(&f.run_root(id)) {
            if let Ok(text) = fs::read_to_string(&path) {
                assert!(
                    !text.contains("transist-runtime-temp-"),
                    "{}",
                    path.display()
                );
                assert!(
                    !text.contains(".transist-staging/invoke-"),
                    "{}",
                    path.display()
                );
                assert!(!redactor.contains_secret(&text), "{}", path.display());
            }
        }
    }
}

#[test]
fn file_update_multiple_outputs_read_only_workspace_and_logical_metadata() {
    let f = Fixture::new();
    let steps = step(
        "produce",
        "{ops: [{path: {$document: other, access: write}, op: write, content: exact}, {path: {$document: tree, access: write}, op: tree, content: member}]}",
    ) + &step(
        "update",
        "{ops: [{path: {$document: file, access: update}, op: update, content: changed}, {path: {$document: tree, access: update}, op: tree_update, content: replaced}]}",
    ) + &step(
        "consume",
        &format!(
            "{{ops: [{{path: {{$document: file, access: read}}, op: read, content: initialchanged}}, {{path: {{$document: other, access: read}}, op: read, content: exact}}, {{path: {{$document: other, access: read}}, op: read, content: exact}}, {{path: {{$document: tree, access: read}}, op: tree_read, content: replaced}}], home_probe: '{}', workspace_read_only: true, large: true, structured: {{nested: [true, null]}}}}",
            f.home.join("config.yaml").display()
        ),
    );
    let recipe = f.compile("", &steps);
    assert_eq!(
        serde_json::to_value(&recipe.steps[0].with[transist_protocol::DOCUMENT_BINDINGS_KEY])
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    f.run("run", "success");
    let old = &f.execution("run", "produce")["document_revisions"][1];
    let old_manifest = f.bytes("run", old);
    let manifest: Value = serde_json::from_slice(&old_manifest).unwrap();
    assert_eq!(
        fs::read(
            f.run_root("run").join("documents/blobs").join(
                manifest["files"]["sub/member.txt"]["sha256"]
                    .as_str()
                    .unwrap()
            )
        )
        .unwrap(),
        b"member"
    );
    let initial =
        &read_json(&f.run_root("run").join("documents/initial.json"))["document_revisions"][0];
    assert_eq!(f.bytes("run", initial), b"initial");
    assert_eq!(f.bytes("run", &f.heads("run")["file"]), b"initialchanged");
    let state = f.state("run");
    assert_eq!(
        state["steps"]["consume"]["last_request"]["input"]["ops"][0]["path"],
        json!({"$document":"file","access":"read"})
    );
    assert_eq!(
        state["steps"]["consume"]["last_result"]["output"]["paths"][0]["exact"],
        json!({"$document":"file","access":"read"})
    );
    for path in collect_files(&f.run_root("run")) {
        if let Ok(text) = fs::read_to_string(&path) {
            assert!(
                !text.contains("transist-runtime-temp-"),
                "{}",
                path.display()
            );
            assert!(
                !text.contains(".transist-staging/invoke-"),
                "{}",
                path.display()
            );
        }
    }
    assert!(!f.workspace.join("forbidden").exists());
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(collect_files(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[test]
fn all_outputs_validate_before_any_batch_blob_is_persisted() {
    for (name, op) in [
        ("other", "missing"),
        ("other", "symlink"),
        ("other", "oversized"),
        ("tree", "missing"),
        ("tree", "tree_symlink"),
        ("tree", "too_many"),
    ] {
        let f = Fixture::new();
        f.compile("", &step("produce", &format!("{{ops: [{{path: {{$document: file, access: write}}, op: write, content: unpublished-batch}}, {{path: {{$document: {name}, access: write}}, op: {op}}}]}}")));
        f.run("run", "failed");
        assert_eq!(f.bytes("run", &f.heads("run")["file"]), b"initial");
        assert_eq!(
            fs::read_dir(f.run_root("run").join("documents/blobs"))
                .unwrap()
                .count(),
            1
        );
        assert!(
            f.execution("run", "produce")
                .get("document_revisions")
                .is_none()
        );
        let result = f.state("run")["steps"]["produce"]["last_result"].clone();
        assert_eq!(result["status"], "failed");
        assert!(
            result["stderr"]
                .as_str()
                .unwrap()
                .contains(if op == "missing" {
                    "No such file"
                } else if op == "oversized" {
                    "bounds"
                } else if op == "too_many" {
                    "max_files"
                } else {
                    "symlink"
                })
                || result["stderr"]
                    .as_str()
                    .unwrap()
                    .contains("Too many levels"),
            "{op}: {result}"
        );
    }
}

#[test]
fn unpublished_read_and_update_fail_without_transport_or_publication() {
    for access in ["read", "update"] {
        let f = Fixture::new();
        f.compile(
            "",
            &step(
                "consume",
                &format!("{{ops: [{{path: {{$document: other, access: {access}}}, op: read}}]}}"),
            ),
        );
        f.run("run", "failed");
        assert!(
            f.state("run")["steps"]["consume"]["last_result"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unpublished"),
            "{}",
            serde_json::to_string_pretty(&f.state("run")).unwrap()
        );
        assert!(f.heads("run").get("other").is_none());
    }
}

#[test]
fn failed_canceled_and_schema_invalid_results_never_publish_written_outputs() {
    for extra in [
        "status: failed",
        "status: cancelled",
        "invalid_result: true",
    ] {
        let f = Fixture::new();
        f.compile("", &step("produce", &format!("{{ops: [{{path: {{$document: file, access: update}}, op: update, content: changed}}], {extra}}}")));
        f.prepare("run");
        let _ = f.context.run_started_run("run").unwrap();
        assert_eq!(f.bytes("run", &f.heads("run")["file"]), b"initial");
        assert!(
            f.execution("run", "produce")
                .get("document_revisions")
                .is_none()
        );
    }
}

#[test]
fn materialized_input_schema_is_checked_before_first_probe_main_and_cleanup() {
    for cleanup in [false, true] {
        let f = Fixture::new();
        let bad = "    uses: process:fixture.documents/strict\n    with: {path: {$document: file, access: read}}\n";
        let path = if cleanup {
            f.flow(
                &format!("cleanup:\n  - id: invalid\n    when: [success]\n{bad}"),
                "  - id: finish\n    control: control.noop\n",
            )
        } else {
            f.flow("", &format!("  - id: invalid\n{bad}"))
        };
        let source = fs::read_to_string(&path)
            .unwrap()
            .replace("access: read_only", "access: full");
        fs::write(&path, source).unwrap();
        f.context.compile_flow_file(&path).unwrap();
        f.prepare("run");
        let _ = f.context.run_started_run("run").unwrap();
        let state = f.state("run");
        let invalid = if cleanup {
            &state["cleanup"]["steps"]["invalid"]
        } else {
            &state["steps"]["invalid"]
        };
        assert_eq!(invalid["status"], "failed", "{state}");
        assert!(
            invalid["last_result"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("schema")
        );
        assert!(
            invalid["last_result"]["output"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        assert!(
            !f.workspace.join("startups").exists(),
            "schema failure must precede the first APP probe"
        );
    }
}

#[test]
fn recursive_bindings_reject_conflicting_writes_authority_http_mcp_and_unknown_review_gates() {
    let reviewed = Fixture::new();
    reviewed.compile(
        "",
        &("  - id: review\n    control: control.gate.manual\n    with: {review_documents: [file]}\n"
            .to_string()
            + &step(
                "consumer",
                "{path: {$document: file, access: read, reviewed_by: review}}",
            )),
    );
    for data in [
        "{ops: [{path: {$document: file, access: write}}, {path: {$document: file, access: update}}]}",
        "{path: {$document: absent, access: read}}",
        "{path: {$document: file}}",
        "{path: {$document: file, access: read, reviewed_by: review}}",
        "{nested: {command: {$document: file, access: read}}}",
        "{repo_path: {$document: file, access: read}}",
        "{prompt_file: {$document: file, access: read}}",
        "{secret_refs: [{$document: file, access: read}]}",
    ] {
        let f = Fixture::new();
        assert!(
            f.context
                .compile_flow_file(&f.flow("", &step("invalid", data)))
                .is_err(),
            "{data}"
        );
    }
    for adapter in ["http", "mcp"] {
        let flow: transist_compiler::Flow = yaml_serde::from_str(&format!("schema: transist.flow.v1\nid: invalid\nversion: 0.1.0\nplugins: {{fixture.documents: 0.1.0}}\n{DOCUMENTS}steps:\n  - id: invalid\n    uses: {adapter}:fixture.documents/call\n    with: {{path: {{$document: file, access: read}}}}\n")).unwrap();
        let error = transist_compiler::compile_flow(&flow).unwrap_err();
        assert!(format!("{error:#}").contains("HTTP/MCP"), "{error:#}");
    }
}

#[test]
fn statically_known_non_string_document_path_is_rejected() {
    let f = Fixture::new();
    let path = f.flow("", "  - id: invalid\n    uses: process:fixture.documents/typed\n    with: {path: {$document: file, access: read}}\n");
    let error = f.context.compile_flow_file(&path).unwrap_err();
    assert!(
        format!("{error:#}").contains("statically incompatible"),
        "{error:#}"
    );
}

#[test]
fn input_and_step_output_objects_cannot_smuggle_document_authority() {
    for field in ["command", "repo_path", "source", "prompt_file", "ordinary"] {
        let f = Fixture::new();
        let flow = f.flow(
            "inputs:\n  payload: {type: object, required: true}\n",
            &step("invalid", &format!("{{{field}: '{{{{inputs.payload}}}}'}}")),
        );
        f.context.compile_flow_file(&flow).unwrap();
        let inputs = BTreeMap::from([(
            "payload".into(),
            KernelValue::from_json(json!({"$document":"file","access":"read"})),
        )]);
        assert!(
            f.context
                .prepare_workflow("documents@0.1.0", Some("blocked"), inputs)
                .is_err()
        );
        assert!(f.context.runs_list().unwrap().is_empty());
        // Return a binding-shaped object as ordinary output, then attempt to promote it.
        let f = Fixture::new();
        f.compile(
            "",
            &(step("producer", "{smuggle: true}")
                + &step(
                    "invalid",
                    &format!("{{{field}: '{{{{steps.producer.out.value}}}}'}}"),
                )),
        );
        f.run("smuggled", "failed");
    }
}

#[test]
fn nested_modules_cleanup_resume_and_replay_share_only_the_current_root_namespace() {
    let f = Fixture::new();
    fs::write(f.root.join("leaf.yaml"), "schema: transist.module.v1\nid: leaf\nversion: 0.1.0\nsteps:\n  - id: update\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: file, access: update}, op: update, content: leaf}]}\n").unwrap();
    fs::write(f.root.join("middle.yaml"), "schema: transist.module.v1\nid: middle\nversion: 0.1.0\nmodules: {leaf: {path: leaf.yaml}}\nsteps:\n  - id: leaf\n    module: leaf\n").unwrap();
    f.compile("modules: {middle: {path: middle.yaml}}\ncleanup:\n  - id: clean\n    when: [success]\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: other, access: write}, op: write, content: cleanup}]}\n", &("  - id: middle\n    module: middle\n".to_string() + &step("tail", "{ops: [{path: {$document: file, access: read}, op: read, content: initialleaf}], fail_first: true}")));
    f.run("source", "failed");
    assert_eq!(
        f.bytes("source", &f.heads("source")["file"]),
        b"initialleaf"
    );
    assert_eq!(f.context.runs_resume("source").unwrap().status, "success");
    assert_eq!(f.bytes("source", &f.heads("source")["other"]), b"cleanup");
    assert_eq!(
        f.bytes("source", &f.heads("source")["file"]),
        b"initialleaf"
    );
    let replay = f.context.runs_replay("source", Some("replay")).unwrap();
    assert_eq!(replay.status, "failed");
    assert_eq!(f.context.runs_resume("replay").unwrap().status, "success");
    assert_eq!(
        f.bytes("replay", &f.heads("replay")["file"]),
        b"initialleaf"
    );
    assert_ne!(
        f.heads("source")["file"]["revision_id"],
        f.heads("replay")["file"]["revision_id"]
    );
    f.run("independent", "failed");
    assert_eq!(
        f.bytes("independent", &f.heads("independent")["file"]),
        b"initialleaf"
    );
}

#[test]
fn raw_encoded_and_manifest_only_secrets_reject_the_whole_batch_without_byte_redaction() {
    for operation in [
        "op: secret",
        "op: secret, encoded: true",
        "op: manifest_secret",
    ] {
        let f = Fixture::new();
        let name = if operation.contains("manifest") {
            "tree"
        } else {
            "other"
        };
        f.compile("", &step("produce", &format!("{{secret_refs: [DOCUMENT_TEST_SECRET], ops: [{{path: {{$document: file, access: write}}, op: write, content: unpublished}}, {{path: {{$document: {name}, access: write}}, {operation}}}]}}")));
        f.run("run", "failed");
        assert_eq!(
            fs::read_dir(f.run_root("run").join("documents/blobs"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(f.bytes("run", &f.heads("run")["file"]), b"initial");
        assert!(
            f.state("run")["steps"]["produce"]["last_result"]["stderr"]
                .as_str()
                .unwrap()
                .contains("secret"),
            "{}",
            f.state("run")
        );
    }
}

#[test]
fn document_deferral_keeps_static_schema_policy_secret_and_prerequisite_blockers() {
    for blocker in ["schema", "policy", "secret", "prerequisite"] {
        let f = Fixture::new();
        let extra = if blocker == "prerequisite" {
            "prerequisites:\n  paths:\n    - {path: absent-required-file, kind: file, required: true}\n"
        } else {
            ""
        };
        let data = if blocker == "schema" {
            "{ops: [{path: {$document: file, access: read}}]}"
        } else if blocker == "secret" {
            "{ops: [{path: {$document: file, access: read}, op: read}], secret_refs: [DOCUMENT_MISSING_SECRET]}"
        } else {
            "{ops: [{path: {$document: file, access: read}, op: read}]}"
        };
        f.compile(extra, &step("blocked", data));
        if blocker == "policy" {
            fs::write(
                f.home.join("config.yaml"),
                "version: v1\nruntime: {mode: standalone}\npolicy: {profile: allowlist}\n",
            )
            .unwrap();
        }
        let report = f
            .context
            .workflow_validation_report("documents@0.1.0", BTreeMap::new(), false, false)
            .unwrap();
        assert!(!report.is_validation_passed(), "{blocker}: {report:#?}");
        assert!(
            f.context
                .prepare_workflow("documents@0.1.0", Some("blocked"), BTreeMap::new())
                .is_err()
        );
        assert!(f.context.runs_list().unwrap().is_empty());
        assert!(!f.workspace.join("startups").exists());
    }
}

#[test]
fn repeated_reads_with_one_writer_keep_snapshot_bytes_and_typed_dataflow() {
    let f = Fixture::new();
    let mut operations =
        vec!["{path: {$document: file, access: update}, op: update, content: changed}".to_string()];
    operations.extend((0..12).map(|_| {
        "{path: {$document: file, access: read}, op: read, content: initial}".to_string()
    }));
    f.compile(
        "",
        &(step(
            "producer",
            "{return_value: {array: [true, null], count: 3}}",
        ) + &step(
            "consumer",
            &format!(
                "{{ops: [{}], structured: '{{{{steps.producer.out.value}}}}'}}",
                operations.join(",")
            ),
        )),
    );
    f.run("run", "success");
    assert_eq!(f.bytes("run", &f.heads("run")["file"]), b"initialchanged");
    let output = &f.state("run")["steps"]["consumer"]["last_result"]["output"];
    assert_eq!(output["structured"], json!({"array":[true,null],"count":3}));
    assert_eq!(output["paths"].as_array().unwrap().len(), 13);
    for path in output["paths"].as_array().unwrap().iter().skip(1) {
        assert_eq!(path["exact"], json!({"$document":"file","access":"read"}));
        assert_eq!(path["nested"][0], "prefix=$document:file/child suffix");
    }
}

#[test]
fn cleanup_failed_canceled_invalid_and_missing_outputs_do_not_publish() {
    for extra in [
        "status: failed",
        "status: cancelled",
        "invalid_result: true",
        "missing: true",
    ] {
        let f = Fixture::new();
        let op = if extra == "missing: true" {
            "missing"
        } else {
            "write"
        };
        f.compile(&format!("cleanup:\n  - id: cleanup_output\n    when: [success]\n    uses: process:fixture.documents/call\n    with: {{ops: [{{path: {{$document: file, access: update}}, op: update, content: changed}}, {{path: {{$document: other, access: write}}, op: {op}}}], {extra}}}\n"), "  - id: finish\n    control: control.noop\n");
        f.prepare("run");
        f.context.run_started_run("run").unwrap();
        assert_eq!(f.bytes("run", &f.heads("run")["file"]), b"initial");
        assert!(f.heads("run").get("other").is_none());
        assert_ne!(
            f.state("run")["cleanup"]["steps"]["cleanup_output"]["status"],
            "succeeded"
        );
        assert!(
            f.execution("run", "cleanup_output")
                .get("document_revisions")
                .is_none()
        );
        assert_eq!(
            fs::read_dir(f.run_root("run").join("documents/blobs"))
                .unwrap()
                .count(),
            1
        );
    }
}

#[test]
fn review_step4_tmpdir_workspace_alias_native_process() {
    if let Some(root) = std::env::var_os("TRANSIST_STEP4_REVIEW_ALIAS_ROOT") {
        let root = PathBuf::from(root);
        let context = TransistContext::with_home(root.join("workspace"), root.join("home"));
        let report = context.run_started_run("alias-review").unwrap();
        assert_eq!(
            report.status,
            "success",
            "{}",
            read_json(&context.run_root("alias-review").unwrap().join("state.json"))
        );
        return;
    }
    let f = Fixture::new();
    let path = f.flow("", &step("read", "{ops: [{path: {$document: file, access: read}, op: read, content: initial}], workspace_alias_probe: true}"));
    let flow = fs::read_to_string(&path)
        .unwrap()
        .replace("access: read_only", "access: full");
    fs::write(&path, flow).unwrap();
    f.context.compile_flow_file(&path).unwrap();
    f.prepare("alias-review");
    let temporary = f.workspace.join("tmp");
    fs::create_dir(&temporary).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "review_step4_tmpdir_workspace_alias_native_process",
            "--nocapture",
        ])
        .env("TMPDIR", &temporary)
        .env("TRANSIST_STEP4_REVIEW_ALIAS_ROOT", &f.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        f.bytes("alias-review", &f.heads("alias-review")["file"]),
        b"initial"
    );
    assert!(!fs::read_dir(temporary).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("transist-runtime-temp-")
    }));
}

#[test]
fn review_step4_reserved_document_environment_is_rejected_at_install() {
    let f = Fixture::new();
    let manifest = f.root.join("plugin/plugin.yaml");
    let original = fs::read_to_string(&manifest).unwrap();
    for (index, value) in [
        "runtime://temp/documents/read/binding-00000000000000000000/injected",
        "runtime://temp/documents/read/binding-00000000000000000123/injected",
        "runtime://temp/documents/write/binding-00000000000000000000",
        "runtime://temp/Documents/WRITE/anything",
        "runtime://temp/cache/../documents/read",
        "runtime://temp/%64ocuments/read",
        "runtime://temp/documents",
    ]
    .iter()
    .enumerate()
    {
        let home = f.root.join(format!("install-rejection-{index}"));
        init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).unwrap();
        let changed = original.replace(
            "  command: plugin.py",
            &format!("  command: plugin.py\n  env: {{CACHE_DIR: '{value}'}}"),
        );
        fs::write(&manifest, changed).unwrap();
        let context = TransistContext::with_home(&f.workspace, &home);
        assert!(
            context.install_plugin_file(&manifest).is_err(),
            "accepted reserved/aliased environment: {value}"
        );
        assert!(!home.join("plugins/fixture.documents/0.1.0").exists());
        assert!(context.runs_list().unwrap().is_empty());
    }
    assert_eq!(
        fs::read_to_string(f.home.join("plugins/fixture.documents/0.1.0/plugin.yaml")).unwrap(),
        original
    );
}

#[test]
fn review_step4_captured_source_survives_source_removal() {
    let f = Fixture::new();
    f.context
        .install_plugin_file(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml"),
        )
        .unwrap();
    let helper = f.root.join("helpers/run.sh");
    fs::create_dir(helper.parent().unwrap()).unwrap();
    fs::write(&helper, "#!/bin/sh\nprintf captured-source-bytes\n").unwrap();
    let path = f.flow("", "  - id: captured\n    uses: process:shell/exec\n    with: {command: 'source://helpers/run.sh'}\n");
    fs::write(
        &path,
        fs::read_to_string(&path).unwrap().replace(
            "plugins: {fixture.documents: 0.1.0}",
            "plugins: {shell: 0.1.0}",
        ),
    )
    .unwrap();
    f.context.compile_flow_file(&path).unwrap();
    f.prepare("source-review");
    fs::remove_file(helper).unwrap();
    assert_eq!(
        f.context.run_started_run("source-review").unwrap().status,
        "success",
        "{}",
        f.state("source-review")
    );
    assert!(
        f.state("source-review")["steps"]["captured"]["last_result"]["output"]["stdout"]
            .as_str()
            .unwrap()
            .contains("captured-source-bytes")
    );
    assert_eq!(f.state("source-review")["workflow_asset_snapshot"], true);
}

#[test]
fn review_step4_temp_parent_workspace_blocks_preparation_without_plugin_startup() {
    let f = Fixture::new();
    let path = f.flow(
        "",
        &step(
            "read",
            "{ops: [{path: {$document: file, access: read}, op: read, content: initial}]}",
        ),
    );
    let source = fs::read_to_string(&path).unwrap().replace(
        &f.workspace.display().to_string(),
        &transist_core::runtime_temp_parent().display().to_string(),
    );
    fs::write(&path, source).unwrap();
    f.context.compile_flow_file(&path).unwrap();
    let report = f
        .context
        .workflow_validation_report("documents@0.1.0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(!report.is_validation_passed(), "{report:#?}");
    assert!(
        f.context
            .prepare_workflow("documents@0.1.0", Some("blocked"), BTreeMap::new())
            .is_err()
    );
    assert!(f.context.runs_list().unwrap().is_empty());
    assert!(!f.workspace.join("startups").exists());
}

#[test]
fn review_step4_literal_home_and_workspace_paths_accept_native_names() {
    for (home, workspace) in [
        ("home[review]", "workspace"),
        ("home", "workspace[review]"),
        ("home[review]?", "workspace[review]?"),
    ] {
        let f = Fixture::with_literal_names(home, workspace);
        f.compile("", &(step("write", "{ops: [{path: {$document: other, access: write}, op: write, content: exact}]}")
            + &step("read", "{ops: [{path: {$document: file, access: read}, op: read, content: initial}, {path: {$document: other, access: read}, op: read, content: exact}]}")));
        f.run("literal-paths", "success");
        assert_eq!(
            f.bytes("literal-paths", &f.heads("literal-paths")["other"]),
            b"exact"
        );
        assert_eq!(
            f.bytes("literal-paths", &f.heads("literal-paths")["file"]),
            b"initial"
        );
    }
}
