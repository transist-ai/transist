// SPDX-License-Identifier: MPL-2.0
//! Real process/document/gate proof at early approval and the bounded-loop cap.
use super::*;

fn start(f: &Fixture, cap: u32, fail_publication_once: bool) {
    f.ctx
        .install_plugin_file(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell/plugin.yaml"),
        )
        .unwrap();
    fs::write(f.root.join("require_approval.py"), "import sys\nif sys.argv[1:] != ['approved']:\n    print('approval_required: implementation was not approved', file=sys.stderr)\n    sys.exit(23)\n").unwrap();
    fs::write(
        f.root.join("iteration.yaml"),
        r#"
schema: transist.module.v1
id: iteration
version: 0.1.0
outputs:
  decision: '{{steps.review.out.decision}}'
steps:
- id: produce
  uses: process:fixture.documents/call
  with: {ops: [{path: {$document: file, access: write}, op: write, content: reviewed}]}
- id: review
  control: control.gate.manual
  with: {review_documents: [file]}
- id: route
  control: control.noop
  if:
    guard: 'steps.review.out.decision == "request_changes"'
    then: [{ref: changes}]
    else: [{ref: approved_copy}]
- id: changes
  control: control.noop
- id: approved_copy
  uses: process:fixture.documents/call
  with:
    ops:
    - {path: {$document: file, access: read, reviewed_by: review}, op: read, content: reviewed}
    - {path: {$document: missing, access: write}, op: write, content: reviewed}
- id: finish
  control: control.finish
"#,
    )
    .unwrap();
    fs::write(
        f.root.join("implementation.yaml"),
        format!(
            r#"
schema: transist.module.v1
id: implementation
version: 0.1.0
modules:
  iteration: {{path: iteration.yaml}}
steps:
- id: repeat
  control: control.noop
  loop:
    until: 'steps.iteration.out.decision == "approved"'
    max_iters: {cap}
    body: [{{ref: iteration}}]
- id: iteration
  module: {{ref: iteration, rerun_mode: fresh}}
- id: require_approval
  uses: process:shell/exec
  with:
    command: python3
    args: [source://require_approval.py, '{{{{steps.iteration.out.decision}}}}']
- id: finish
  control: control.finish
"#
        ),
    )
    .unwrap();
    let path = f.flow("modules:\n  implementation: {path: implementation.yaml}\n", &format!("  - id: implementation\n    module: implementation\n  - id: publish\n    uses: process:fixture.documents/call\n    with:\n      fail_first: {fail_publication_once}\n      ops: [{{path: {{$document: missing, access: read}}, op: read, content: reviewed}}]\n"));
    let source = fs::read_to_string(&path).unwrap().replace(
        "plugins: {fixture.documents: 0.1.0}",
        "plugins: {fixture.documents: 0.1.0, shell: 0.1.0}",
    );
    fs::write(&path, source).unwrap();
    f.ctx.compile_flow_file(&path).unwrap();
    f.ctx
        .prepare_workflow("reviews@0.1.0", Some("run"), BTreeMap::new())
        .unwrap();
    assert_eq!(f.resume(), "blocked");
}

fn implementation(f: &Fixture) -> Value {
    read(
        &f.ctx
            .run_root("run__implementation")
            .unwrap()
            .join("state.json"),
    )
}

fn active_iteration(f: &Fixture) -> String {
    implementation(f)["steps"]["iteration"]["last_result"]["output"]["child_run_id"]
        .as_str()
        .unwrap()
        .into()
}

fn request(f: &Fixture, child: &str) -> Value {
    read(&f.ctx.run_root(child).unwrap().join("state.json"))["steps"]["review"]["last_result"]["output"]["review_request"].clone()
}

fn decide(f: &Fixture, child: &str, decision: GateDecision) {
    f.ctx
        .record_gate_decision_with_input(child, None, decision, input())
        .unwrap();
}

#[test]
fn loop_cap_routing_request_changes_at_cap_fails_approval_guard_before_parent_publication() {
    let f = Fixture::new();
    start(&f, 3, false);
    let mut children = Vec::new();
    let mut reviewed = Vec::new();
    for iteration in 1..=2 {
        let child = active_iteration(&f);
        reviewed.push(request(&f, &child)["review_documents"][0].clone());
        decide(&f, &child, GateDecision::RequestChanges);
        children.push(child);
        assert_eq!(
            f.resume(),
            if iteration == 2 { "failed" } else { "blocked" }
        );
    }
    assert_ne!(children[0], children[1]);
    assert_ne!(reviewed[0]["revision_id"], reviewed[1]["revision_id"]);
    assert_eq!(reviewed[0]["blob"]["sha256"], reviewed[1]["blob"]["sha256"]);
    let state = implementation(&f);
    assert_eq!(state["status"], "failed");
    assert_eq!(state["steps"]["repeat"]["executions"], 3);
    let guard = &state["steps"]["require_approval"];
    assert_eq!(guard["executions"], 1);
    assert_eq!(guard["status"], "failed");
    assert!(
        guard["last_result"]["stderr"]
            .as_str()
            .unwrap()
            .contains("approval_required"),
        "{guard}"
    );
    assert_eq!(f.state()["steps"]["publish"]["executions"], 0);
    assert!(
        read(&f.run_root().join("documents/heads.json"))
            .get("missing")
            .is_none()
    );
    for child in children {
        let state = read(&f.ctx.run_root(&child).unwrap().join("state.json"));
        assert_eq!(state["steps"]["produce"]["executions"], 1);
        assert_eq!(state["steps"]["changes"]["executions"], 1);
        assert_eq!(state["steps"]["approved_copy"]["executions"], 0);
    }
}

#[test]
fn loop_cap_routing_early_and_last_body_approval_preserve_exact_document_review() {
    for approve_at in [1, 3] {
        let f = Fixture::new();
        start(&f, 4, false);
        let mut previous = None;
        let mut last_child = String::new();
        let mut last_request = Value::Null;
        for iteration in 1..=approve_at {
            let child = active_iteration(&f);
            let current = request(&f, &child);
            let revision = current["review_documents"][0]["revision_id"].clone();
            if let Some(previous) = previous {
                assert_ne!(previous, revision);
            }
            previous = Some(revision);
            decide(
                &f,
                &child,
                if iteration == approve_at {
                    GateDecision::Approved
                } else {
                    GateDecision::RequestChanges
                },
            );
            assert_eq!(
                f.resume(),
                if iteration == approve_at {
                    "success"
                } else {
                    "blocked"
                }
            );
            last_child = child;
            last_request = current;
        }
        let state = implementation(&f);
        assert_eq!(state["steps"]["repeat"]["executions"], approve_at + 1);
        assert_eq!(state["steps"]["require_approval"]["executions"], 1);
        assert_eq!(state["steps"]["require_approval"]["status"], "succeeded");
        assert_eq!(f.state()["steps"]["publish"]["executions"], 1);
        assert_eq!(
            f.state()["steps"]["publish"]["last_result"]["output"]["seen"],
            json!(["reviewed"])
        );
        let child_root = f.ctx.run_root(&last_child).unwrap();
        let child = read(&child_root.join("state.json"));
        assert_eq!(child["steps"]["approved_copy"]["executions"], 1);
        let decision = &child["steps"]["review"]["last_result"]["output"]["review_decision"];
        assert_eq!(decision["review_request"], last_request);
        let heads = read(&f.run_root().join("documents/heads.json"));
        assert_eq!(
            heads["file"]["revision_id"],
            last_request["review_documents"][0]["revision_id"]
        );
        assert_eq!(heads["missing"]["run_id"], last_child);
        assert_eq!(heads["missing"]["step_id"], "approved_copy");
    }
}

#[test]
fn loop_cap_routing_resume_after_parent_failure_does_not_repeat_guard_or_approved_copy() {
    let f = Fixture::new();
    start(&f, 3, true);
    let child = active_iteration(&f);
    decide(&f, &child, GateDecision::Approved);
    assert_eq!(f.resume(), "failed");
    let implementation_before = implementation(&f);
    let child_root = f.ctx.run_root(&child).unwrap();
    let copy_before =
        fs::read(child_root.join("step_executions/approved_copy/execution_001.json")).unwrap();
    let heads_before = fs::read(f.run_root().join("documents/heads.json")).unwrap();
    assert_eq!(f.ctx.runs_resume("run").unwrap().status, "success");
    assert_eq!(
        implementation(&f)["steps"]["require_approval"]["executions"],
        implementation_before["steps"]["require_approval"]["executions"]
    );
    assert_eq!(
        fs::read(child_root.join("step_executions/approved_copy/execution_001.json")).unwrap(),
        copy_before
    );
    assert_eq!(
        fs::read(f.run_root().join("documents/heads.json")).unwrap(),
        heads_before
    );
    assert_eq!(f.state()["steps"]["publish"]["executions"], 2);
}
