// SPDX-License-Identifier: MPL-2.0
//! Marked-loop recovery uses real kernel phases and immutable completion records.
use super::*;
use transist_core::{FAULT_STEP_EXECUTION_RECORD, KernelStepStatus};
use transist_protocol::{RecipeLoopRoute, RecipeLoopRouteRole};

fn bounded_recipe(cap: u32) -> Recipe {
    let mut recipe: Recipe = serde_json::from_str(include_str!("legacy.recipe.json")).unwrap();
    recipe.steps[0].with.insert(
        "__transist_loop".into(),
        serde_json::json!({
            "body_steps":["body"], "max_iters":cap, "until":"false", "max_duration_ms":60000
        })
        .into(),
    );
    recipe.steps[2] = invoke_step("mandatory_successor");
    let graph = recipe.graph.as_mut().unwrap();
    graph.loop_routing_version = Some(1);
    for (edge, role) in graph
        .edges
        .iter_mut()
        .take(2)
        .zip([RecipeLoopRouteRole::Enter, RecipeLoopRouteRole::Exit])
    {
        edge.loop_route = Some(RecipeLoopRoute {
            owner_step_id: "loop".into(),
            role,
        });
    }
    recipe
}

fn kernel(execution: Arc<dyn ExecutionPort>, store: Arc<MemoryRunStore>) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts: Arc::new(MemoryArtifacts),
        run_store: store.clone(),
        lease: Arc::new(NoopLease::default()),
        control: Arc::new(NoopControlRequests),
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events: Arc::new(RecordingEvents::default().with_durable_mirror(store)),
        policy: Arc::new(AllowPolicy),
        preflight: Arc::new(RecordingPreflight::default()),
        secrets: Arc::new(StaticSecrets),
        plugin_verifier: Arc::new(AllowPluginVerification),
        plugin_metadata: Arc::new(TestPluginMetadata),
        prompt_files: Arc::new(TestPromptFiles),
        home_settings: Arc::new(DefaultHomeSettings),
        telemetry: Arc::new(NoopTelemetry::default()),
    }))
}

struct FaultOnStep {
    inner: Arc<RecordingExecution>,
    step: &'static str,
    execution: u32,
    armed: AtomicBool,
}
impl ExecutionPort for FaultOnStep {
    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }
    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        let result = self.inner.execute_step(invocation)?;
        if invocation.request.step.id == self.step
            && invocation.request.step.execution == self.execution
            && self.armed.swap(false, Ordering::SeqCst)
        {
            transist_core::set(FAULT_STEP_EXECUTION_RECORD);
        }
        Ok(result)
    }
    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        self.inner.cancel_step(request)
    }
}

#[test]
fn loop_cap_routing_recovery_approval_guard_failure_fails_module_before_parent_publication() {
    for cap in [1, 2] {
        let execution = Arc::new(RecordingExecution::default());
        let store = Arc::new(MemoryRunStore::default());
        let kernel = kernel(execution.clone(), store.clone());
        let child = bounded_recipe(cap);
        let parent = recipe(
            "parent",
            vec![
                subflow_step("implementation", child),
                invoke_step("publish"),
            ],
            None,
        );
        execution.plan_result(
            "parent__implementation",
            "mandatory_successor",
            1,
            failed_result(
                "parent__implementation",
                "mandatory_successor",
                1,
                "approval_required: implementation was not approved",
            ),
        );
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let result = kernel
            .execute_recipe(tmp.path(), &home, &parent, Some("parent"))
            .unwrap();
        assert_eq!(result.status, KernelRunStatus::Failed);
        let child = store.run_json("parent__implementation");
        assert_eq!(child["steps"]["loop"]["executions"], cap);
        assert_eq!(child["steps"]["body"]["executions"], cap - 1);
        assert_eq!(child["steps"]["mandatory_successor"]["executions"], 1);
        assert_eq!(child["steps"]["mandatory_successor"]["status"], "failed");
        assert!(
            child["steps"]["mandatory_successor"]["last_result"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("approval_required")
        );
        assert_eq!(
            store.run_json("parent")["steps"]["publish"]["executions"],
            0
        );
        assert_eq!(execution.invocations().len(), 1);
    }
}

#[test]
fn loop_cap_routing_recovery_resume_does_not_repeat_committed_successor() {
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let kernel = kernel(execution.clone(), store.clone());
    let mut recipe = bounded_recipe(2);
    recipe.steps[3] = invoke_step("finish");
    execution.plan_result(
        "resume",
        "finish",
        1,
        failed_result("resume", "finish", 1, "transient tail failure"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    assert_eq!(
        kernel
            .execute_recipe(tmp.path(), &home, &recipe, Some("resume"))
            .unwrap()
            .status,
        KernelRunStatus::Failed
    );
    let pinned = store.load_recipe_snapshot("resume").unwrap();
    assert_eq!(
        kernel
            .resume_run(tmp.path(), &home, "resume")
            .unwrap()
            .status,
        KernelRunStatus::Success
    );
    assert_eq!(store.load_recipe_snapshot("resume").unwrap(), pinned);
    let calls: Vec<_> = execution
        .invocations()
        .iter()
        .map(|i| (i.request.step.id.clone(), i.request.step.execution))
        .collect();
    assert_eq!(
        calls,
        vec![
            ("mandatory_successor".into(), 1),
            ("finish".into(), 1),
            ("finish".into(), 2)
        ]
    );
    assert_eq!(store.run_json("resume")["steps"]["loop"]["executions"], 2);
    assert_eq!(
        store
            .step_executions()
            .iter()
            .filter(|e| e["step_id"] == "mandatory_successor")
            .count(),
        1
    );
}

#[test]
fn loop_cap_routing_recovery_reconciles_committed_successor_without_redispatch() {
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let fault = Arc::new(FaultOnStep {
        inner: execution.clone(),
        step: "mandatory_successor",
        execution: 1,
        armed: AtomicBool::new(true),
    });
    let kernel = kernel(fault, store.clone());
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    {
        let _clear = ExecutionFaultGuard;
        assert!(
            kernel
                .execute_recipe(tmp.path(), &home, &bounded_recipe(2), Some("commit"))
                .unwrap_err()
                .to_string()
                .contains("injected fault")
        );
    }
    assert_eq!(
        store.run_json("commit")["steps"]["mandatory_successor"]["status"],
        "running"
    );
    assert_eq!(
        store
            .step_executions()
            .iter()
            .filter(|e| e["step_id"] == "mandatory_successor")
            .count(),
        1
    );
    let pinned = store.load_recipe_snapshot("commit").unwrap();
    assert_eq!(
        kernel
            .run_started_run(tmp.path(), &home, "commit")
            .unwrap()
            .status,
        KernelRunStatus::Success
    );
    assert_eq!(execution.invocations().len(), 1);
    assert_eq!(
        store.run_json("commit")["steps"]["mandatory_successor"]["executions"],
        1
    );
    assert_eq!(store.run_json("commit")["steps"]["finish"]["executions"], 1);
    assert_eq!(store.load_recipe_snapshot("commit").unwrap(), pinned);
}

fn nested_recipe(fresh: bool) -> Recipe {
    let mut child = bounded_recipe(2);
    child.steps[1] = invoke_step("body");
    let mut parent = bounded_recipe(3);
    parent.steps[1] = subflow_step("body", child);
    if fresh {
        parent.steps[1].with.insert(
            "__transist_subflow".into(),
            serde_json::json!({"rerun_mode":"fresh"}).into(),
        );
    }
    parent
}

#[test]
fn loop_cap_routing_recovery_reused_and_fresh_children_keep_activation_budgets() {
    for fresh in [false, true] {
        let execution = Arc::new(RecordingExecution::default());
        let store = Arc::new(MemoryRunStore::default());
        let kernel = kernel(execution.clone(), store.clone());
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        assert_eq!(
            kernel
                .execute_recipe(tmp.path(), &home, &nested_recipe(fresh), Some("nested"))
                .unwrap()
                .status,
            KernelRunStatus::Success
        );
        let root = store.run_json("nested");
        assert_eq!(root["steps"]["loop"]["executions"], 3);
        assert_eq!(root["steps"]["body"]["executions"], 2);
        assert_eq!(root["steps"]["mandatory_successor"]["executions"], 1);
        let child = store.run_json("nested__body");
        assert_eq!(
            child["steps"]["loop"]["executions"],
            if fresh { 2 } else { 4 }
        );
        assert_eq!(
            child["steps"]["loop"]["activation_executions_baseline"]
                .as_u64()
                .unwrap_or(0),
            if fresh { 0 } else { 2 }
        );
        assert_eq!(
            child["steps"]["body"]["executions"],
            if fresh { 1 } else { 2 }
        );
        assert_eq!(
            child["steps"]["mandatory_successor"]["executions"],
            if fresh { 1 } else { 2 }
        );
        if fresh {
            let second = store.run_json("nested__body__r002");
            assert_eq!(second["steps"]["loop"]["executions"], 2);
            assert_eq!(second["steps"]["body"]["executions"], 1);
            assert_eq!(second["steps"]["mandatory_successor"]["executions"], 1);
        }
        assert_eq!(execution.invocations().len(), 5);
    }
}

#[test]
fn loop_cap_routing_recovery_crash_restores_nonzero_activation_budget_and_repeat_target() {
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let fault = Arc::new(FaultOnStep {
        inner: execution.clone(),
        step: "body",
        execution: 2,
        armed: AtomicBool::new(true),
    });
    let kernel = kernel(fault, store.clone());
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    {
        let _clear = ExecutionFaultGuard;
        assert!(
            kernel
                .execute_recipe(tmp.path(), &home, &nested_recipe(false), Some("activation"))
                .unwrap_err()
                .to_string()
                .contains("injected fault")
        );
    }
    let before = store.load_run("activation__body").unwrap().unwrap();
    assert_eq!(before.steps["loop"].activation_executions_baseline, 2);
    assert_eq!(before.steps["loop"].executions, 3);
    assert_eq!(before.steps["body"].status, KernelStepStatus::Running);
    assert_eq!(
        kernel
            .run_started_run(tmp.path(), &home, "activation")
            .unwrap()
            .status,
        KernelRunStatus::Success
    );
    let child = store.load_run("activation__body").unwrap().unwrap();
    assert_eq!(child.steps["loop"].executions, 4);
    assert_eq!(child.steps["loop"].activation_executions_baseline, 2);
    assert_eq!(child.steps["loop"].activation_executions(), 2);
    assert_eq!(child.steps["body"].executions, 2);
    assert_eq!(child.steps["mandatory_successor"].executions, 2);
    assert_eq!(
        store.run_json("activation")["steps"]["loop"]["executions"],
        3
    );
    assert_eq!(
        store.run_json("activation")["steps"]["mandatory_successor"]["executions"],
        1
    );
    assert_eq!(
        store
            .step_executions()
            .iter()
            .filter(|e| e["run_id"] == "activation__body"
                && e["step_id"] == "body"
                && e["execution"] == 2)
            .count(),
        1
    );
    assert_eq!(execution.invocations().len(), 5);
}

#[test]
fn loop_cap_routing_recovery_replay_preserves_marked_and_frozen_legacy_snapshots() {
    for marked in [false, true] {
        let execution = Arc::new(RecordingExecution::default());
        let store = Arc::new(MemoryRunStore::default());
        let kernel = kernel(execution, store.clone());
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let mut recipe = if marked {
            bounded_recipe(2)
        } else {
            serde_json::from_str(include_str!("legacy.recipe.json")).unwrap()
        };
        kernel
            .start_recipe(tmp.path(), &home, &recipe, Some("source"), BTreeMap::new())
            .unwrap();
        let pinned = store.load_recipe_snapshot("source").unwrap();
        // Changing the caller's current definition cannot replace the prepared
        // snapshot. Recovery/replay must not invoke the current compiler.
        recipe = bounded_recipe(1);
        assert_eq!(recipe.graph.as_ref().unwrap().loop_routing_version, Some(1));
        assert_eq!(
            kernel
                .run_started_run(tmp.path(), &home, "source")
                .unwrap()
                .status,
            KernelRunStatus::Success
        );
        assert_eq!(store.load_recipe_snapshot("source").unwrap(), pinned);
        assert_eq!(
            kernel
                .replay_run(tmp.path(), &home, "source", Some("replay"))
                .unwrap()
                .status,
            KernelRunStatus::Success
        );
        assert_eq!(store.load_recipe_snapshot("source").unwrap(), pinned);
        assert_eq!(store.load_recipe_snapshot("replay").unwrap(), pinned);
        for id in ["source", "replay"] {
            let state = store.run_json(id);
            assert_eq!(state["steps"]["loop"]["executions"], 2);
            assert_eq!(state["steps"]["body"]["executions"], 1);
            assert_eq!(
                state["steps"]["mandatory_successor"]["executions"],
                if marked { 1 } else { 0 }
            );
            let recipe: Recipe =
                yaml_serde::from_str(&store.load_recipe_snapshot(id).unwrap()).unwrap();
            assert_eq!(
                recipe.graph.unwrap().loop_routing_version,
                marked.then_some(1)
            );
        }
    }
}
