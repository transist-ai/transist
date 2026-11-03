// SPDX-License-Identifier: MPL-2.0
use crate::application::runs::{RunService, run_started_run_in_home};
use crate::application::workflows::{compile_flow_file_in_home, prepare_workflow_in_home};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use transist_core::ports::{LeasePort, ManagementMode, RuntimeOwnerIdentity};
use transist_storage_local::run_paths::resolve_run_root;
    use transist_core::{
    KernelRunStatus, KernelSkipReason, KernelStepStatus, PersistedRunRecord, RunLineage,
    RunLineageMode,
};
use transist_protocol::GateDecisionRecord;
use transist_storage_local::{LocalLeaseStore, LocalRunSummaryStore};

#[test]
fn inspection_only_reports_reached_manual_gates_as_blocked() {
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::init_repo(tmp.path()).expect("init");
    let home_dir = tmp.path().join(".transist");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: approvals_demo
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
    .expect("flow");

    compile_flow_file_in_home(tmp.path(), &home_dir, &flow).expect("compile");
    prepare_workflow_in_home(
        tmp.path(),
        &home_dir,
        "approvals_demo@v1",
        Some("approval-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let inspection = crate::TransistInspectionContext::with_home(tmp.path(), &home_dir);
    let created = inspection
        .get_run("approval-run")
        .expect("inspect created")
        .expect("created run");
    assert!(created.blocked_gates.is_empty());
    assert!(!created.summary.has_blocked_gates);

    run_started_run_in_home(tmp.path(), &home_dir, "approval-run").expect("run");
    let blocked = inspection
        .get_run("approval-run")
        .expect("inspect blocked")
        .expect("blocked run");
    assert_eq!(blocked.summary.status, "blocked");
    assert_eq!(blocked.blocked_gates.len(), 1);
    assert_eq!(blocked.blocked_gates[0].gate_id, "review");
    assert!(blocked.summary.has_blocked_gates);
}

#[test]
fn inspection_renders_input_placeholders_in_gate_request_summary() {
    use transist_protocol::KernelValue;

    let tmp = tempfile::tempdir().expect("tempdir");
    crate::init_repo(tmp.path()).expect("init");
    let home_dir = tmp.path().join(".transist");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: guidance_template_demo
version: v1
inputs:
  required_pr_approvals:
    type: number
    required: true
steps:
  - id: review
    control: control.gate.manual
    with:
      gate_id: review
      operator_guidance:
        conditions:
          items:
            - "Approving exits the loop only when the PR has at least {{inputs.required_pr_approvals}} approvals."
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");

    compile_flow_file_in_home(tmp.path(), &home_dir, &flow).expect("compile");
    let mut inputs = BTreeMap::new();
    inputs.insert(
        "required_pr_approvals".to_string(),
        KernelValue::Number(serde_json::Number::from(2u64)),
    );
    prepare_workflow_in_home(
        tmp.path(),
        &home_dir,
        "guidance_template_demo@v1",
        Some("guidance-template-run"),
        inputs,
    )
    .expect("prepare");
    run_started_run_in_home(tmp.path(), &home_dir, "guidance-template-run").expect("run");

    let inspection = crate::TransistInspectionContext::with_home(tmp.path(), &home_dir);
    let blocked = inspection
        .get_run("guidance-template-run")
        .expect("inspect")
        .expect("run");
    assert_eq!(blocked.blocked_gates.len(), 1);
    let summary = &blocked.blocked_gates[0].request_summary;

    let guidance = match summary.get("operator_guidance") {
        Some(KernelValue::Object(map)) => map,
        other => panic!("expected operator_guidance object, got {:?}", other),
    };
    let conditions = match guidance.get("conditions") {
        Some(KernelValue::Object(map)) => map,
        other => panic!("expected conditions object, got {:?}", other),
    };
    let items = match conditions.get("items") {
        Some(KernelValue::Array(items)) => items,
        other => panic!("expected items array, got {:?}", other),
    };
    let first = match items.first() {
        Some(KernelValue::String(text)) => text.as_str(),
        other => panic!("expected items[0] string, got {:?}", other),
    };
    assert!(
        !first.contains("{{inputs."),
        "placeholder should be rendered, got: {first}"
    );
    assert!(
        first.contains("at least 2 approvals"),
        "rendered text missing expected value: {first}"
    );
}

#[test]
fn inspection_includes_active_lease_when_present() {
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::init_repo(tmp.path()).expect("init");
    let home_dir = tmp.path().join(".transist");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: lease_demo
version: v1
steps:
  - id: start
    control: control.noop
    with: {}
"#,
    )
    .expect("flow");

    compile_flow_file_in_home(tmp.path(), &home_dir, &flow).expect("compile");
    prepare_workflow_in_home(
        tmp.path(),
        &home_dir,
        "lease_demo@v1",
        Some("lease-run"),
        BTreeMap::new(),
    )
    .expect("prepare");

    let leases = LocalLeaseStore::new(
        &home_dir,
        RuntimeOwnerIdentity {
            management_mode: ManagementMode::Standalone,
            owner_id: "owner-a".to_string(),
            lease_id: "lease-a".to_string(),
            pid: std::process::id(),
            hostname: "host-a".to_string(),
            command: "transist runs start lease-run".to_string(),
            transist_version: env!("CARGO_PKG_VERSION").to_string(),
            external: None,
            runtime_id: None,
            organization_id: None,
        },
    );
    let handle = leases
        .acquire_root_run_tree_lease("lease-run")
        .expect("acquire lease");

    let inspection = crate::TransistInspectionContext::with_home(tmp.path(), &home_dir);
    let detail = inspection
        .get_run("lease-run")
        .expect("inspect run")
        .expect("run detail");
    let active_lease = detail.active_lease.expect("active lease");
    assert_eq!(active_lease.root_run_id, "lease-run");
    assert_eq!(active_lease.owner_id, "owner-a");
    assert_eq!(active_lease.pid, std::process::id());

    leases
        .release_root_run_tree_lease(&handle)
        .expect("release lease");
}

#[test]
fn inspection_prefers_current_blocked_gate_over_historical_request_changes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::init_repo(tmp.path()).expect("init");
    let home_dir = tmp.path().join(".transist");

    let flow = tmp.path().join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: approvals_demo
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
    .expect("flow");

    compile_flow_file_in_home(tmp.path(), &home_dir, &flow).expect("compile");
    prepare_workflow_in_home(
        tmp.path(),
        &home_dir,
        "approvals_demo@v1",
        Some("approval-run"),
        BTreeMap::new(),
    )
    .expect("prepare");
    run_started_run_in_home(tmp.path(), &home_dir, "approval-run").expect("run");

    let history_dir = home_dir.join("runs/run_approval-run/gates/history");
    fs::create_dir_all(&history_dir).expect("history dir");
    let record = GateDecisionRecord {
        review_request: None,
        schema: "transist.gate.decision.v1".to_string(),
        run_id: "approval-run".to_string(),
        gate_id: "review".to_string(),
        step_id: "review".to_string(),
        decision: "request_changes".to_string(),
        actor: Some("reviewer@example.com".to_string()),
        actor_source: Some("flag".to_string()),
        reason: Some("stale historical feedback".to_string()),
        decided_at_ms: Some(1),
    };
    fs::write(
        history_dir.join("000001_request_changes.json"),
        serde_json::to_string_pretty(&record).expect("serialize gate history"),
    )
    .expect("write gate history");

    let inspection = crate::TransistInspectionContext::with_home(tmp.path(), &home_dir);
    let blocked = inspection
        .get_run("approval-run")
        .expect("inspect blocked")
        .expect("blocked run");
    assert_eq!(blocked.summary.status, "blocked");
    assert_eq!(blocked.blocked_gates.len(), 1);
    assert_eq!(blocked.blocked_gates[0].gate_id, "review");
    assert_eq!(blocked.blocked_gates[0].status, "blocked");
    assert_eq!(
        blocked.blocked_gates[0]
            .decision
            .as_ref()
            .expect("historical decision")
            .state,
        "request_changes"
    );
    assert!(blocked.summary.has_blocked_gates);
}

#[test]
fn inspection_rolls_up_blocked_gates_for_parent_and_root_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path().join(".transist");
    let root_run_id = prepare_nested_review_loop_reblocked_run(tmp.path(), &home_dir);
    let parent_run_id = format!("{root_run_id}__pr_review_loop");
    let child_run_id = format!("{parent_run_id}__review_cycle");

    let inspection = crate::TransistInspectionContext::with_home(tmp.path(), &home_dir);
    let runs = inspection
        .list_runs(&crate::RunListQuery::default())
        .expect("list runs");
    let root_summary = runs
        .data
        .iter()
        .find(|run| run.run_id == root_run_id)
        .expect("root run summary");
    assert!(root_summary.has_blocked_gates);

    let root = inspection
        .get_run(&root_run_id)
        .expect("inspect root")
        .expect("root run");
    assert!(root.summary.has_blocked_gates);

    let parent = inspection
        .get_run(&parent_run_id)
        .expect("inspect parent")
        .expect("parent run");
    assert!(parent.summary.has_blocked_gates);

    let child = inspection
        .get_run(&child_run_id)
        .expect("inspect child")
        .expect("child run");
    assert!(child.summary.has_blocked_gates);

    let parent_summary_path = transist_storage_local::run_paths::resolve_run_root(&home_dir, &parent_run_id)
        .expect("parent run root")
        .join("summary.json");
    fs::remove_file(&parent_summary_path).expect("remove parent summary");

    let parent_without_summary = inspection
        .get_run(&parent_run_id)
        .expect("inspect parent without summary")
        .expect("parent run without summary");
    assert!(parent_without_summary.summary.has_blocked_gates);
}

#[test]
fn inspection_ignores_stale_created_children_for_terminal_parent_status() {
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::init_repo(tmp.path()).expect("init");
    let home_dir = tmp.path().join(".transist");

    let grandchild_dir = tmp.path().join("grandchild");
    fs::create_dir_all(&grandchild_dir).expect("grandchild dir");
    fs::write(
        grandchild_dir.join("workflow.yaml"),
        r#"
schema: transist.flow.v1
id: stale_nested_grandchild_demo
version: v1
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("grandchild flow");

    let child_dir = tmp.path().join("child");
    fs::create_dir_all(&child_dir).expect("child dir");
    fs::write(
        child_dir.join("workflow.yaml"),
        r#"
schema: transist.flow.v1
id: stale_nested_child_demo
version: v1
steps:
  - id: review_changes_flow
    subflow:
      path: ../grandchild/workflow.yaml
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    let root_flow = tmp.path().join("workflow.yaml");
    fs::write(
        &root_flow,
        r#"
schema: transist.flow.v1
id: stale_nested_root_demo
version: v1
steps:
  - id: child
    subflow:
      path: ./child/workflow.yaml
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("root flow");

    compile_flow_file_in_home(tmp.path(), &home_dir, &grandchild_dir.join("workflow.yaml"))
        .expect("compile grandchild");
    compile_flow_file_in_home(tmp.path(), &home_dir, &child_dir.join("workflow.yaml"))
        .expect("compile child");
    compile_flow_file_in_home(tmp.path(), &home_dir, &root_flow).expect("compile root");

    prepare_workflow_in_home(
        tmp.path(),
        &home_dir,
        "stale_nested_root_demo@v1",
        Some("root-run"),
        BTreeMap::new(),
    )
    .expect("prepare root");
    prepare_workflow_in_home(
        tmp.path(),
        &home_dir,
        "stale_nested_child_demo@v1",
        Some("root-run__child"),
        BTreeMap::new(),
    )
    .expect("prepare child");
    prepare_workflow_in_home(
        tmp.path(),
        &home_dir,
        "stale_nested_grandchild_demo@v1",
        Some("root-run__child__review_changes_flow"),
        BTreeMap::new(),
    )
    .expect("prepare grandchild");

    update_run_record(&home_dir, "root-run", |record| {
        record.status = KernelRunStatus::Failed;
        let child = record.steps.get_mut("child").expect("root child step");
        child.status = KernelStepStatus::Failed;
        child.executions = 1;
        child.started_at_ms = Some(10);
        child.finished_at_ms = Some(20);
        let finish = record.steps.get_mut("finish").expect("root finish step");
        finish.status = KernelStepStatus::Skipped;
        finish.skip_reason = Some(KernelSkipReason::DependencyFailed);
    });
    update_run_record(&home_dir, "root-run__child", |record| {
        record.status = KernelRunStatus::Failed;
        record.lineage = Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: "root-run".to_string(),
            parent_run_id: Some("root-run".to_string()),
            parent_step_id: Some("child".to_string()),
        });
        let review_changes = record
            .steps
            .get_mut("review_changes_flow")
            .expect("child review_changes_flow step");
        review_changes.status = KernelStepStatus::Skipped;
        review_changes.skip_reason = Some(KernelSkipReason::DependencyFailed);
        let finish = record.steps.get_mut("finish").expect("child finish step");
        finish.status = KernelStepStatus::Skipped;
        finish.skip_reason = Some(KernelSkipReason::DependencyFailed);
    });
    update_run_record(
        &home_dir,
        "root-run__child__review_changes_flow",
        |record| {
            record.status = KernelRunStatus::Created;
            record.lineage = Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root-run".to_string(),
                parent_run_id: Some("root-run__child".to_string()),
                parent_step_id: Some("review_changes_flow".to_string()),
            });
        },
    );

    let inspection = crate::TransistInspectionContext::with_home(tmp.path(), &home_dir);
    let root = inspection
        .get_run("root-run")
        .expect("inspect root")
        .expect("root run");
    assert_eq!(root.summary.status, "failed");

    let child = inspection
        .get_run("root-run__child")
        .expect("inspect child")
        .expect("child run");
    assert_eq!(child.summary.status, "failed");

    let runs = inspection
        .list_runs(&crate::RunListQuery::default())
        .expect("list runs");
    let root_summary = runs
        .data
        .iter()
        .find(|run| run.run_id == "root-run")
        .expect("root summary");
    assert_eq!(root_summary.status, "failed");
}

fn prepare_nested_review_loop_reblocked_run(
    repo_root: &std::path::Path,
    home_dir: &std::path::Path,
) -> String {
    crate::init_repo(repo_root).expect("init");
    let root_run_id = "review-loop-root-run".to_string();

    let changes_dir = repo_root.join("changes");
    fs::create_dir_all(&changes_dir).expect("changes dir");
    fs::write(
        changes_dir.join("workflow.yaml"),
        r#"
schema: transist.flow.v1
id: review_changes_demo
version: v1
steps:
  - id: review_plan_revision
    control: control.gate.manual
    with:
      gate_id: review-plan-revision
  - id: finish
    control: control.finish
"#,
    )
    .expect("changes flow");

    let cycle_dir = repo_root.join("cycle");
    fs::create_dir_all(&cycle_dir).expect("cycle dir");
    fs::write(
        cycle_dir.join("workflow.yaml"),
        r#"
schema: transist.flow.v1
id: review_cycle_demo
version: v1
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
    subflow:
      path: ../changes/workflow.yaml
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("cycle flow");

    let loop_dir = repo_root.join("loop");
    fs::create_dir_all(&loop_dir).expect("loop dir");
    fs::write(
        loop_dir.join("workflow.yaml"),
        r#"
schema: transist.flow.v1
id: pr_review_loop_demo
version: v1
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
    subflow:
      path: ../cycle/workflow.yaml
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("loop flow");

    let flow = repo_root.join("workflow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: parent_review_loop_root_demo
version: v1
steps:
  - id: pr_review_loop
    subflow:
      path: ./loop/workflow.yaml
      inputs: {}
  - id: finish
    control: control.finish
"#,
    )
    .expect("root flow");

    compile_flow_file_in_home(repo_root, home_dir, &flow).expect("compile");
    prepare_workflow_in_home(
        repo_root,
        home_dir,
        "parent_review_loop_root_demo@v1",
        Some(&root_run_id),
        BTreeMap::new(),
    )
    .expect("prepare");

    let blocked = run_started_run_in_home(repo_root, home_dir, &root_run_id).expect("run");
    assert_eq!(blocked.status, "blocked");

    let runs = RunService::new(repo_root.to_path_buf(), home_dir.to_path_buf());

    let changed = runs
        .request_changes_gate(
            &root_run_id,
            "review-cycle-decision",
            Some("run one review cycle"),
        )
        .expect("request changes");
    assert_eq!(changed.status, "blocked");

    let approved = runs
        .approve_gate(&root_run_id, "review-plan-revision")
        .expect("approve nested review gate");
    assert_eq!(approved.status, "blocked");

    root_run_id
}

fn update_run_record(home_dir: &Path, run_id: &str, update: impl FnOnce(&mut PersistedRunRecord)) {
    let run_root = resolve_run_root(home_dir, run_id).expect("run root");
    let state_path = run_root.join("state.json");
    let raw = fs::read_to_string(&state_path).expect("read state");
    let mut record: PersistedRunRecord = serde_json::from_str(&raw).expect("parse persisted run");
    update(&mut record);
    record.refresh_executions();
    fs::write(
        &state_path,
        serde_json::to_string_pretty(&record).expect("serialize persisted run"),
    )
    .expect("write state");
    LocalRunSummaryStore::new(home_dir)
        .refresh_run(run_id)
        .expect("refresh summary");
}

#[test]
fn list_workflows_marks_referenced_subflows_as_is_subflow() {
    use crate::PaginationQuery;

    let tmp = tempfile::tempdir().expect("tempdir");
    crate::init_repo(tmp.path()).expect("init");
    let home_dir = tmp.path().join(".transist");

    // Root workflow that references the subflow.
    let root_flow = tmp.path().join("root.flow.yaml");
    let subflows_dir = tmp.path().join("subflows");
    fs::create_dir_all(&subflows_dir).expect("subflows dir");
    let child_flow = subflows_dir.join("child.flow.yaml");

    fs::write(
        &child_flow,
        r#"
schema: transist.flow.v1
id: child_workflow
version: v1
steps:
  - id: finish
    control: control.finish
"#,
    )
    .expect("child flow");

    fs::write(
        &root_flow,
        r#"
schema: transist.flow.v1
id: root_workflow
version: v1
steps:
  - id: call_child
    subflow:
      path: subflows/child.flow.yaml
  - id: finish
    control: control.finish
"#,
    )
    .expect("root flow");

    compile_flow_file_in_home(tmp.path(), &home_dir, &child_flow).expect("compile child");
    compile_flow_file_in_home(tmp.path(), &home_dir, &root_flow).expect("compile root");

    let inspection = crate::TransistInspectionContext::with_home(tmp.path(), &home_dir);
    let page = inspection
        .list_workflows(&PaginationQuery::default())
        .expect("list workflows");

    let root = page
        .data
        .iter()
        .find(|w| w.workflow_id == "root_workflow")
        .expect("root workflow in list");
    let child = page
        .data
        .iter()
        .find(|w| w.workflow_id == "child_workflow")
        .expect("child workflow in list");

    assert!(
        !root.is_subflow,
        "root workflow must not be marked as subflow"
    );
    assert!(child.is_subflow, "child workflow must be marked as subflow");
}
