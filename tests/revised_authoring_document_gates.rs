// SPDX-License-Identifier: MPL-2.0
//! Contract C: immutable review content, current lexical decisions and pre-probe enforcement.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use transist::{
    GateDecision, GateDecisionInput, RuntimeOperatingMode, TransistContext, init_home_dir_with_mode,
};

const GATE: &str = "  - id: review\n    control: control.gate.manual\n    with: {gate_id: operator-review, review_documents: [file]}\n";
const CONSUME: &str = "  - id: consume\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: file, access: read, reviewed_by: review}, op: read, content: initial}]}\n";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    ctx: TransistContext,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).unwrap();
        init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).unwrap();
        let ctx = TransistContext::with_home(&workspace, &home);
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
        ctx.install_plugin_file(&plugin.join("plugin.yaml"))
            .unwrap();
        Self {
            _temp: temp,
            root,
            home,
            workspace,
            ctx,
        }
    }
    fn flow(&self, extra: &str, steps: &str) -> PathBuf {
        let path = self.root.join("workflow.yaml");
        fs::write(&path, format!("schema: transist.flow.v1\nid: reviews\nversion: 0.1.0\nplugins: {{fixture.documents: 0.1.0}}\nworkspace: {{path: '{}', access: full}}\ndocuments:\n  file: {{kind: file, max_bytes: 4096, initial: initial}}\n  missing: {{kind: file, max_bytes: 4096}}\n  tree: {{kind: tree, max_bytes: 4096, max_files: 3}}\n{extra}steps:\n{steps}", self.workspace.display())).unwrap();
        path
    }
    fn start(&self, extra: &str, steps: &str) {
        self.ctx
            .compile_flow_file(&self.flow(extra, steps))
            .unwrap();
        self.ctx
            .prepare_workflow("reviews@0.1.0", Some("run"), BTreeMap::new())
            .unwrap();
        assert_eq!(self.ctx.run_started_run("run").unwrap().status, "blocked");
    }
    fn run_root(&self) -> PathBuf {
        self.ctx.run_root("run").unwrap()
    }
    fn state(&self) -> Value {
        read(&self.run_root().join("state.json"))
    }
    fn request(&self) -> Value {
        self.state()["steps"]["review"]["last_result"]["output"]["review_request"].clone()
    }
    fn decide(&self, decision: GateDecision) {
        self.ctx
            .record_gate_decision_with_input("run", None, decision, input())
            .unwrap();
    }
    fn history(&self) -> PathBuf {
        fs::read_dir(self.run_root().join("gates/history"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }
    fn resume(&self) -> String {
        self.ctx.run_started_run("run").unwrap().status
    }
}
fn input() -> GateDecisionInput {
    GateDecisionInput {
        actor: Some("fixture-reviewer".into()),
        reason: Some("offline test decision".into()),
        ..Default::default()
    }
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

#[test]
fn approval_persists_exact_request_and_allows_private_consumption() {
    let f = Fixture::new();
    f.start("", &(GATE.to_string() + CONSUME));
    let request = f.request();
    assert_eq!(request["execution"], 1);
    assert_eq!(request["review_documents"][0]["document"], "file");
    assert_eq!(request["review_documents"][0]["size_bytes"], 7);
    assert!(!f.workspace.join("startups").exists());
    f.decide(GateDecision::Approved);
    assert_eq!(read(&f.history())["review_request"], request);
    let query = transist::TransistInspectionContext::with_home(&f.workspace, &f.home);
    let gates = query.list_gates("run").unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&gates[0].decision.as_ref().unwrap().review_request).unwrap(),
        request
    );
    assert_eq!(
        serde_json::to_value(&gates[0].decision_history[0].review_request).unwrap(),
        request
    );
    assert_eq!(f.resume(), "success");
    assert_eq!(
        f.state()["steps"]["consume"]["last_result"]["output"]["seen"],
        json!(["initial"])
    );
    assert!(!f.run_root().join("gates/operator-review.json").exists());
    assert_eq!(read(&f.history())["review_request"], request);
}

#[test]
fn rejection_never_starts_the_consumer() {
    let f = Fixture::new();
    f.start("", &(GATE.to_string() + CONSUME));
    f.decide(GateDecision::Rejected);
    assert_eq!(f.resume(), "failed");
    assert!(!f.workspace.join("startups").exists());
    assert_eq!(f.state()["steps"]["consume"]["executions"], 0);
}

#[test]
fn decision_retry_reconciles_history_without_replacing_or_duplicating_the_review() {
    let f = Fixture::new();
    f.start("", &(GATE.to_string() + CONSUME));
    f.decide(GateDecision::Approved);
    let history = f.history();
    let original = fs::read(&history).unwrap();
    // Simulate history committed before an interrupted active-record replacement.
    fs::remove_file(f.run_root().join("gates/operator-review.json")).unwrap();
    f.decide(GateDecision::Approved);
    assert_eq!(fs::read(&history).unwrap(), original);
    assert_eq!(
        fs::read_dir(f.run_root().join("gates/history"))
            .unwrap()
            .count(),
        1
    );
    assert!(
        f.ctx
            .record_gate_decision_with_input("run", None, GateDecision::Rejected, input())
            .is_err()
    );
    assert_eq!(f.resume(), "success");
}

#[test]
fn changed_revision_even_identical_bytes_requires_new_approval_before_first_probe() {
    for bytes in ["initial", "different"] {
        let f = Fixture::new();
        let update = format!(
            "  - id: replace\n    uses: process:fixture.documents/call\n    with: {{ops: [{{path: {{$document: file, access: write}}, op: write, content: {bytes}}}]}}\n"
        );
        f.start("", &(GATE.to_string() + &update + CONSUME));
        let reviewed = f.request()["review_documents"][0].clone();
        f.decide(GateDecision::Approved);
        assert_eq!(f.resume(), "failed");
        let state = f.state();
        assert!(
            state["steps"]["consume"]["last_result"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not covered"),
            "{state}"
        );
        let head = &read(&f.run_root().join("documents/heads.json"))["file"];
        assert_ne!(head["revision_id"], reviewed["revision_id"]);
        assert_eq!(
            head["blob"]["sha256"] == reviewed["blob"]["sha256"],
            bytes == "initial"
        );
        // Exactly replace's deferred probe and action ran. Consumer never probes.
        assert_eq!(
            fs::read_to_string(f.workspace.join("startups"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }
}

#[test]
fn restart_waiting_preserves_the_original_immutable_request() {
    let f = Fixture::new();
    f.start("", &(GATE.to_string() + CONSUME));
    let request = f.request();
    // Latest is a non-authoritative projection; neither restart nor review reads it.
    write(
        &f.run_root().join("documents/heads.json"),
        &json!({"file": {"revision_id": "untrusted-new-head"}}),
    );
    let restarted = TransistContext::with_home(&f.workspace, &f.home);
    assert_eq!(restarted.run_started_run("run").unwrap().status, "blocked");
    assert_eq!(f.request(), request);
    assert_eq!(f.state()["steps"]["review"]["executions"], 2);
    f.decide(GateDecision::Approved);
    assert_eq!(f.resume(), "success");
    assert_eq!(read(&f.history())["review_request"], request);
}

#[test]
fn waiting_request_does_not_follow_a_new_committed_head_and_old_content_stays_inspectable() {
    use transist::InspectionQueryPort;
    use transist_core::ports::{
        DocumentOutput, DocumentStorePort, RunStorePort, RunStoreStepExecution,
    };
    let f = Fixture::new();
    let writer = "  - id: replace\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: file, access: write}, op: write, content: later}]}\n";
    f.start("", &(GATE.to_string() + writer + CONSUME));
    let original = f.request();
    // Controlled successful publication while the root runner is stopped at its gate.
    let store = transist_storage_local::LocalRunStore::new(
        transist_storage_local::LocalStorageConfig::new(&f.home),
    );
    let mut run = store.load_run("run").unwrap().unwrap();
    let step = run.steps.get_mut("replace").unwrap();
    step.executions = 1;
    step.status = transist_core::KernelStepStatus::Running;
    store.update_run_state("run", &run).unwrap();
    let path = f.root.join("publication");
    fs::write(&path, "later").unwrap();
    let revisions = store
        .prepare_document_revisions(
            "run",
            "replace",
            1,
            &[DocumentOutput {
                document: "file".into(),
                path,
            }],
            &transist_core::Redactor::default(),
        )
        .unwrap();
    let mut result = f.state()["steps"]["review"]["last_result"].clone();
    result["step_id"] = "replace".into();
    result["status"] = "success".into();
    result["output"] = json!({});
    let completion = json!({"run_id":"run","step_id":"replace","execution":1,"status":"success","started_at_ms":1,"finished_at_ms":2,"result":result,"document_revisions":revisions});
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "run".into(),
            step_id: "replace".into(),
            execution: 1,
            state: transist::KernelValue::from_json(completion),
        })
        .unwrap();
    let step = run.steps.get_mut("replace").unwrap();
    step.status = transist_core::KernelStepStatus::Succeeded;
    step.last_result = Some(serde_json::from_value(result).unwrap());
    store.update_run_state("run", &run).unwrap();
    store.reconcile_document_heads("run").unwrap();
    assert_eq!(f.resume(), "blocked");
    assert_eq!(f.request(), original);
    let query = transist::TransistInspectionContext::with_home(&f.workspace, &f.home);
    let old_id = original["review_documents"][0]["revision_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        query
            .read_document_content("run", old_id, None)
            .unwrap()
            .unwrap()
            .bytes,
        b"initial"
    );
    f.decide(GateDecision::Approved);
    assert_eq!(f.resume(), "failed");
    assert!(!f.workspace.join("startups").exists());
}

#[test]
fn unpublished_review_document_is_a_durable_request_failure() {
    let f = Fixture::new();
    f.ctx
        .compile_flow_file(
            &f.flow(
                "",
                &(GATE.replace("[file]", "[missing]")
                    + CONSUME
                        .replace("$document: file", "$document: missing")
                        .as_str()),
            ),
        )
        .unwrap();
    f.ctx
        .prepare_workflow("reviews@0.1.0", Some("run"), BTreeMap::new())
        .unwrap();
    assert_eq!(f.resume(), "failed");
    assert!(
        f.state()["steps"]["review"]["last_result"]["stderr"]
            .as_str()
            .unwrap()
            .contains("unpublished")
    );
    assert!(
        f.run_root()
            .join("step_executions/review/execution_001.json")
            .exists()
    );
    assert!(!f.workspace.join("startups").exists());
}

#[test]
fn active_and_history_tampering_fails_closed_for_run_step_gate_execution_and_revision() {
    for history in [false, true] {
        for field in [
            "run_id",
            "step_id",
            "gate_id",
            "execution",
            "revision_id",
            "digest",
            "missing",
        ] {
            let f = Fixture::new();
            f.start("", &(GATE.to_string() + CONSUME));
            f.decide(GateDecision::Approved);
            let path = if history {
                f.history()
            } else {
                f.run_root().join("gates/operator-review.json")
            };
            let mut decision = read(&path);
            match field {
                "execution" => decision["review_request"]["execution"] = 99.into(),
                "revision_id" => {
                    decision["review_request"]["review_documents"][0]["revision_id"] =
                        "0".repeat(64).into()
                }
                "digest" => {
                    decision["review_request"]["review_documents"][0]["blob"]["sha256"] =
                        "0".repeat(64).into()
                }
                "missing" => {
                    decision.as_object_mut().unwrap().remove("review_request");
                }
                _ => decision[field] = "other".into(),
            }
            write(&path, &decision);
            let outcome = f.ctx.run_started_run("run");
            assert!(
                outcome.is_err() || outcome.unwrap().status != "success",
                "{history} {field}"
            );
            assert!(!f.workspace.join("startups").exists(), "{history} {field}");
        }
    }
}

#[test]
fn tampered_request_projection_cannot_change_runtime_owned_review_content() {
    let f = Fixture::new();
    f.start("", &(GATE.to_string() + CONSUME));
    let path = f.run_root().join("state.json");
    let mut state = read(&path);
    state["steps"]["review"]["last_result"]["output"]["review_request"]["review_documents"][0]["revision_id"] =
        "f".repeat(64).into();
    write(&path, &state);
    assert!(
        f.ctx
            .record_gate_decision_with_input("run", None, GateDecision::Approved, input())
            .is_err()
    );
    assert!(!f.workspace.join("startups").exists());
    let outcome = f.ctx.run_started_run("run");
    assert!(outcome.is_err() || outcome.unwrap().status == "failed");
    assert!(!f.workspace.join("startups").exists());
}

#[test]
fn consumed_review_history_is_still_checked_before_the_consumer_first_probe() {
    for field in [
        "run_id",
        "step_id",
        "gate_id",
        "execution",
        "decision",
        "missing",
    ] {
        let f = Fixture::new();
        f.start(
            "",
            &(GATE.to_string() + "  - id: pause\n    control: control.gate.manual\n" + CONSUME),
        );
        f.decide(GateDecision::Approved);
        let history = f.history();
        assert_eq!(f.resume(), "blocked");
        let mut record = read(&history);
        match field {
            "execution" => record["review_request"]["execution"] = 99.into(),
            "decision" => record["decision"] = "rejected".into(),
            "missing" => {
                record.as_object_mut().unwrap().remove("review_request");
            }
            _ => record[field] = "wrong".into(),
        }
        write(&history, &record);
        f.decide(GateDecision::Approved);
        assert_eq!(f.resume(), "failed", "{field}");
        assert!(!f.workspace.join("startups").exists(), "{field}");
    }
}

#[test]
fn loop_reentry_cannot_replay_a_consumed_approval_for_unchanged_content() {
    let f = Fixture::new();
    let loop_step = "  - id: repeat\n    control: control.noop\n    loop:\n      body: [{ref: review}, {ref: consume}]\n      until: 'false'\n      max_iters: 3\n";
    f.start("", &(loop_step.to_string() + GATE + CONSUME));
    f.decide(GateDecision::Approved);
    let old = read(&f.history());
    assert_eq!(f.resume(), "blocked");
    assert_ne!(f.request()["execution"], old["review_request"]["execution"]);
    let gates = transist::TransistInspectionContext::with_home(&f.workspace, &f.home)
        .list_gates("run")
        .unwrap()
        .unwrap();
    assert!(
        gates[0].decision.is_none(),
        "an earlier approval is history, not the new request's decision"
    );
    assert_eq!(gates[0].decision_history.len(), 1);
    let count = fs::read_to_string(f.workspace.join("startups"))
        .unwrap()
        .lines()
        .count();
    write(&f.run_root().join("gates/operator-review.json"), &old);
    let outcome = f.ctx.run_started_run("run");
    assert!(outcome.is_err() || outcome.unwrap().status == "failed");
    assert_eq!(
        fs::read_to_string(f.workspace.join("startups"))
            .unwrap()
            .lines()
            .count(),
        count
    );
}

#[test]
fn request_changes_loop_requires_current_review_before_resetting_the_body() {
    let f = Fixture::new();
    let loop_step = "  - id: repeat\n    control: control.noop\n    loop:\n      body: [{ref: review}, {ref: consume}]\n      until: 'false'\n      max_iters: 3\n";
    f.start("", &(loop_step.to_string() + GATE + CONSUME));
    f.decide(GateDecision::RequestChanges);
    let old = read(&f.history());
    assert_eq!(f.resume(), "blocked");
    assert_ne!(f.request()["execution"], old["review_request"]["execution"]);
    assert!(!f.workspace.join("startups").exists());
    write(&f.run_root().join("gates/operator-review.json"), &old);
    let before = f.state()["steps"]["review"]["executions"].clone();
    assert!(f.ctx.run_started_run("run").is_err());
    assert_eq!(f.state()["steps"]["review"]["executions"], before);
    assert!(!f.workspace.join("startups").exists());
}

#[test]
fn review_names_and_gate_bindings_are_literal_recipe_local_and_checked_early() {
    for steps in [
        GATE.replace("[file]", "[unknown]") + CONSUME,
        GATE.replace("[file]", "[file, file]") + CONSUME,
        GATE.replace("[file]", "file") + CONSUME,
        GATE.replace("[file]", "['{{inputs.name}}']") + CONSUME,
        GATE.to_string() + &CONSUME.replace("reviewed_by: review", "reviewed_by: operator-review"),
        GATE.to_string() + &CONSUME.replace("access: read", "access: update"),
        GATE.to_string() + &CONSUME.replace("reviewed_by: review", "reviewed_by: null"),
        GATE.replace("control.gate.manual", "control.noop") + CONSUME,
    ] {
        let f = Fixture::new();
        assert!(
            f.ctx.compile_flow_file(&f.flow("", &steps)).is_err(),
            "{steps}"
        );
        assert!(!f.workspace.join("startups").exists());
    }
}

#[test]
fn nested_gate_shares_root_documents_but_gate_names_do_not_cross_recipe_scope() {
    let f = Fixture::new();
    fs::write(
        f.root.join("child.yaml"),
        format!("schema: transist.module.v1\nid: child\nversion: 0.1.0\nsteps:\n{GATE}{CONSUME}"),
    )
    .unwrap();
    f.start(
        "modules: {child: {path: child.yaml}}\n",
        "  - id: produce\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: file, access: write}, op: write, content: initial}]}\n  - id: child\n    module: child\n",
    );
    let child_id = f.state()["steps"]["child"]["last_result"]["output"]["child_run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let child = read(&f.ctx.run_root(&child_id).unwrap().join("state.json"));
    let request = &child["steps"]["review"]["last_result"]["output"]["review_request"];
    assert_eq!(request["run_id"], child_id);
    assert_eq!(request["review_documents"][0]["run_id"], "run");
    assert_eq!(request["review_documents"][0]["step_id"], "produce");
    use transist::InspectionQueryPort;
    let query = transist::TransistInspectionContext::with_home(&f.workspace, &f.home);
    let revision = request["review_documents"][0]["revision_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        query
            .read_document_content(&child_id, revision, None)
            .unwrap()
            .unwrap()
            .bytes,
        b"initial"
    );
    f.decide(GateDecision::Approved);
    assert_eq!(f.resume(), "success");
    let f = Fixture::new();
    fs::write(
        f.root.join("child.yaml"),
        format!("schema: transist.module.v1\nid: child\nversion: 0.1.0\nsteps:\n{CONSUME}"),
    )
    .unwrap();
    assert!(
        f.ctx
            .compile_flow_file(&f.flow(
                "modules: {child: {path: child.yaml}}\n",
                &(GATE.to_string() + "  - id: child\n    module: child\n")
            ))
            .is_err()
    );
}

#[test]
fn review3_reviewed_tree_binding_requires_the_exact_approved_revision() {
    let produce = "  - id: produce\n    uses: process:fixture.documents/call\n    with: {ops: [{path: {$document: tree, access: write}, op: tree, content: member}]}\n";
    let gate = GATE.replace("[file]", "[tree]");
    let consume = CONSUME
        .replace("$document: file", "$document: tree")
        .replace("op: read", "op: tree_read")
        .replace("content: initial", "content: member");
    for outcome in ["approved", "rejected", "changed"] {
        let f = Fixture::new();
        let replace = if outcome == "changed" {
            produce
                .replace("id: produce", "id: replace")
                .replace("content: member", "content: different")
        } else {
            String::new()
        };
        f.start("", &(produce.to_string() + &gate + &replace + &consume));
        let request = f.request();
        assert_eq!(request["review_documents"][0]["kind"], "tree");
        assert_eq!(request["review_documents"][0]["file_count"], 1);
        f.decide(if outcome == "rejected" {
            GateDecision::Rejected
        } else {
            GateDecision::Approved
        });
        assert_eq!(
            f.resume(),
            if outcome == "approved" {
                "success"
            } else {
                "failed"
            }
        );
        let state = f.state();
        if outcome == "approved" {
            assert_eq!(
                state["steps"]["consume"]["last_result"]["output"]["seen"],
                json!(["member"])
            );
        } else if outcome == "changed" {
            assert!(
                state["steps"]["consume"]["last_result"]["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("not covered")
            );
        } else {
            assert_eq!(state["steps"]["consume"]["executions"], 0);
        }
        let startups = fs::read_to_string(f.workspace.join("startups"))
            .unwrap()
            .lines()
            .count();
        assert_eq!(startups, if outcome == "rejected" { 2 } else { 4 });
    }
}

#[test]
fn review3_cleanup_reviewed_file_and_tree_bindings_enforce_approval_before_probes() {
    for name in ["file", "tree"] {
        for changed in [false, true] {
            let f = Fixture::new();
            let (write_op, read_op, content) = if name == "tree" {
                ("tree", "tree_read", "member")
            } else {
                ("write", "read", "initial")
            };
            let writer = format!(
                "  - id: produce\n    uses: process:fixture.documents/call\n    with: {{ops: [{{path: {{$document: {name}, access: write}}, op: {write_op}, content: {content}}}]}}\n"
            );
            let produce = if name == "tree" {
                writer.clone()
            } else {
                String::new()
            };
            let replace = if changed {
                writer
                    .replace("id: produce", "id: replace")
                    .replace(&format!("content: {content}"), "content: changed")
            } else {
                String::new()
            };
            let cleanup = format!(
                "cleanup:\n  - id: consume_cleanup\n    when: [success]\n    uses: process:fixture.documents/call\n    with: {{ops: [{{path: {{$document: {name}, access: read, reviewed_by: review}}, op: {read_op}, content: {content}}}], home_probe: '{}'}}\n",
                f.home.join("config.yaml").display()
            );
            f.start(
                &cleanup,
                &(produce + &GATE.replace("[file]", &format!("[{name}]")) + &replace),
            );
            f.decide(GateDecision::Approved);
            f.resume();
            let state = f.state();
            let consumer = &state["cleanup"]["steps"]["consume_cleanup"];
            assert_eq!(
                consumer["status"],
                if changed { "failed" } else { "succeeded" },
                "{state}"
            );
            if changed {
                assert!(
                    consumer["last_result"]["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("not covered")
                );
            } else {
                assert_eq!(consumer["last_result"]["output"]["seen"], json!([content]));
                assert_eq!(
                    consumer["last_request"]["input"]["ops"][0]["path"],
                    json!({"$document":name, "access":"read", "reviewed_by":"review"})
                );
            }
            let startups = fs::read_to_string(f.workspace.join("startups"))
                .unwrap()
                .lines()
                .count();
            assert_eq!(
                startups,
                if name == "tree" { 4 } else { 2 },
                "{name}, changed={changed}"
            );
        }
    }
}
