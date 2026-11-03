// SPDX-License-Identifier: MPL-2.0
//! Deterministic terminal-cleanup, cancellation, and commit-recovery regressions.

use super::*;
use transist_core::{CleanupPhaseState, KernelRunRecoveryState};
use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

struct DurableCleanupEvents {
    events: Arc<RecordingEvents>,
    store: Arc<MemoryRunStore>,
}

impl EventPort for DurableCleanupEvents {
    fn publish(&self, event: &RuntimeEvent) -> Result<()> {
        self.store.inject_event(event.clone());
        self.events.publish(event)
    }
}

fn cleanup_kernel(
    home_dir: &Path,
    execution: Arc<dyn ExecutionPort>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    control: Arc<dyn ControlRequestPort>,
    preflight: Arc<dyn PreflightPort>,
) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts: Arc::new(RunOwnedArtifacts {
            home_dir: home_dir.to_path_buf(),
        }),
        run_store: run_store.clone(),
        lease: Arc::new(NoopLease::default()),
        control,
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events: Arc::new(DurableCleanupEvents {
            events,
            store: run_store,
        }),
        policy: Arc::new(AllowPolicy),
        preflight,
        secrets: Arc::new(StaticSecrets),
        plugin_verifier: Arc::new(AllowPluginVerification),
        plugin_metadata: Arc::new(TestPluginMetadata),
        prompt_files: Arc::new(TestPromptFiles),
        home_settings: Arc::new(DefaultHomeSettings),
        telemetry: Arc::new(NoopTelemetry::default()),
    }))
}

fn cleanup_recipe(triggers: Vec<CleanupTrigger>) -> Recipe {
    let mut recipe = recipe("automatic_cleanup", vec![invoke_step("main")], None);
    recipe.cleanup = vec![RecipeCleanupStep {
        when: triggers,
        step: invoke_step("teardown"),
    }];
    recipe
}

#[test]
fn failed_automatic_cleanup_completes_and_blocks_primary_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        events.clone(),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
    );
    execution.plan_result("run", "main", 1, failed_result("run", "main", 1, "primary"));
    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home,
            &cleanup_recipe(vec![CleanupTrigger::Failed]),
            Some("run"),
        )
        .unwrap();
    assert_eq!(summary.status, KernelRunStatus::Failed);
    let record = store.load_run("run").unwrap().unwrap();
    assert_eq!(
        record.recovery_state,
        Some(KernelRunRecoveryState::CleanupCompleted)
    );
    assert_eq!(record.cleanup.unwrap().state, CleanupPhaseState::Succeeded);
    assert_eq!(store.step_executions().len(), 2);
    assert!(kernel.resume_run(tmp.path(), &home, "run").is_err());
    assert_eq!(execution.invocations().len(), 2);
    assert_eq!(
        events
            .snapshot()
            .iter()
            .filter(|e| e.kind == "cleanup_started")
            .count(),
        1
    );
    assert!(!events.snapshot().iter().any(|e| e.kind == "step_cancelled"));
}

struct RecoveryCheckingPreflight {
    store: Arc<MemoryRunStore>,
    fail_main: AtomicBool,
    fail_cleanup: AtomicBool,
}

impl PreflightPort for RecoveryCheckingPreflight {
    fn preflight_steps(&self, invocations: &[PreflightInvocation]) -> Result<Vec<PreflightCheck>> {
        let mut checks = Vec::new();
        for invocation in invocations {
            let step_id = invocation.request.step.id.as_str();
            if step_id == "teardown" {
                let record = self.store.load_run("run")?.unwrap();
                assert_eq!(record.status, KernelRunStatus::Failed);
                assert_eq!(
                    record.recovery_state,
                    Some(KernelRunRecoveryState::CleanupRequired)
                );
                if self.fail_cleanup.load(Ordering::SeqCst) {
                    checks.push(test_preflight_check(
                        step_id,
                        "adapter_health",
                        PreflightStatus::Failed,
                        "cleanup unavailable",
                    ));
                }
            } else if self.fail_main.load(Ordering::SeqCst) {
                checks.push(test_preflight_check(
                    step_id,
                    "adapter_health",
                    PreflightStatus::Failed,
                    "primary unavailable",
                ));
            }
        }
        Ok(checks)
    }
}

#[test]
fn failed_automatic_cleanup_selects_recovery_before_preflight_and_can_be_retried() {
    for main_preflight_failure in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let execution = Arc::new(RecordingExecution::default());
        let store = Arc::new(MemoryRunStore::default());
        let events = Arc::new(RecordingEvents::default());
        let preflight = Arc::new(RecoveryCheckingPreflight {
            store: store.clone(),
            fail_main: AtomicBool::new(false),
            fail_cleanup: AtomicBool::new(true),
        });
        let kernel = cleanup_kernel(
            &home,
            execution.clone(),
            store.clone(),
            events.clone(),
            Arc::new(NoopControlRequests),
            preflight.clone(),
        );
        kernel
            .start_recipe(
                tmp.path(),
                &home,
                &cleanup_recipe(vec![CleanupTrigger::Failed]),
                Some("run"),
                BTreeMap::new(),
            )
            .unwrap();
        preflight
            .fail_main
            .store(main_preflight_failure, Ordering::SeqCst);
        execution.plan_result("run", "main", 1, failed_result("run", "main", 1, "primary"));
        let summary = kernel.run_started_run(tmp.path(), &home, "run").unwrap();
        assert_eq!(summary.status, KernelRunStatus::Failed);
        let record = store.load_run("run").unwrap().unwrap();
        assert_eq!(
            record.recovery_state,
            Some(KernelRunRecoveryState::CleanupRequired)
        );
        assert_eq!(record.cleanup.unwrap().state, CleanupPhaseState::Failed);
        assert!(
            execution
                .invocations()
                .iter()
                .all(|i| i.request.step.id != "teardown")
        );
        assert!(kernel.resume_run(tmp.path(), &home, "run").is_err());
        assert_eq!(
            events
                .snapshot()
                .iter()
                .filter(|e| e.kind == "cleanup_started")
                .count(),
            1
        );
        preflight.fail_cleanup.store(false, Ordering::SeqCst);
        kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
        assert_eq!(
            store.load_run("run").unwrap().unwrap().recovery_state,
            Some(KernelRunRecoveryState::CleanupCompleted)
        );
        assert_eq!(
            execution
                .invocations()
                .iter()
                .filter(|i| i.request.step.id == "teardown")
                .count(),
            1
        );
    }
}

#[test]
fn failed_automatic_cleanup_does_not_recurse_and_retry_keeps_succeeded_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        events.clone(),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
    );
    let mut recipe = cleanup_recipe(vec![CleanupTrigger::Failed]);
    recipe.cleanup.push(RecipeCleanupStep {
        when: vec![CleanupTrigger::Failed],
        step: invoke_step("teardown2"),
    });
    execution.plan_result("run", "main", 1, failed_result("run", "main", 1, "primary"));
    execution.plan_result(
        "run",
        "teardown2",
        1,
        failed_result("run", "teardown2", 1, "cleanup"),
    );
    assert_eq!(
        kernel
            .execute_recipe(tmp.path(), &home, &recipe, Some("run"))
            .unwrap()
            .status,
        KernelRunStatus::Failed
    );
    assert_eq!(
        store.load_run("run").unwrap().unwrap().recovery_state,
        Some(KernelRunRecoveryState::CleanupRequired)
    );
    assert_eq!(execution.invocations().len(), 3);
    assert_eq!(
        events
            .snapshot()
            .iter()
            .filter(|e| e.kind == "cleanup_started")
            .count(),
        1
    );
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    let calls = execution.invocations();
    assert_eq!(
        calls
            .iter()
            .filter(|i| i.request.step.id == "teardown")
            .count(),
        1
    );
    assert_eq!(calls.last().unwrap().request.step.execution, 2);
    assert_eq!(store.step_executions().len(), 4);
    assert_eq!(
        store.load_run("run").unwrap().unwrap().recovery_state,
        Some(KernelRunRecoveryState::CleanupCompleted)
    );
}

#[test]
fn no_matching_failed_cleanup_keeps_primary_retry_available() {
    for triggers in [Vec::new(), vec![CleanupTrigger::Success]] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let execution = Arc::new(RecordingExecution::default());
        let store = Arc::new(MemoryRunStore::default());
        let events = Arc::new(RecordingEvents::default());
        let kernel = cleanup_kernel(
            &home,
            execution.clone(),
            store.clone(),
            events.clone(),
            Arc::new(NoopControlRequests),
            Arc::new(RecordingPreflight::default()),
        );
        let mut recipe = cleanup_recipe(triggers.clone());
        if triggers.is_empty() {
            recipe.cleanup.clear();
        }
        execution.plan_result("run", "main", 1, failed_result("run", "main", 1, "primary"));
        assert_eq!(
            kernel
                .execute_recipe(tmp.path(), &home, &recipe, Some("run"))
                .unwrap()
                .status,
            KernelRunStatus::Failed
        );
        let record = store.load_run("run").unwrap().unwrap();
        assert_eq!(
            record.recovery_state,
            Some(KernelRunRecoveryState::Recoverable)
        );
        if !triggers.is_empty() {
            assert_eq!(record.cleanup.unwrap().state, CleanupPhaseState::Skipped);
        }
        assert_eq!(execution.invocations().len(), 1);
        assert!(
            !events
                .snapshot()
                .iter()
                .any(|e| e.kind == "cleanup_started")
        );
        assert_eq!(
            kernel.resume_run(tmp.path(), &home, "run").unwrap().status,
            KernelRunStatus::Success
        );
    }
}

/// Holds cleanup across watcher polls, so cancellation assertions depend on
/// completed polling cycles rather than arbitrary sleeps or scheduler speed.
struct CancelDuringCleanupExecution {
    inner: RecordingExecution,
    control: Arc<PendingCancelControlRequests>,
    polls: Mutex<std::sync::mpsc::Receiver<()>>,
    cancel_calls: AtomicUsize,
    expect_pending_on_entry: bool,
}

impl ExecutionPort for CancelDuringCleanupExecution {
    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        if invocation.request.step.id == "teardown" {
            if self.expect_pending_on_entry {
                assert!(
                    self.control.is_pending(),
                    "original request remains until finalization"
                );
            }
            self.control.request_cancel(&invocation.request.run_id)?;
            self.control.request_cancel(&invocation.request.run_id)?;
            let polls = self.polls.lock().unwrap();
            // The second poll proves the first resolver/cancel cycle finished.
            for _ in 0..2 {
                polls
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("watcher inspected the run tree");
            }
            if self.cancel_calls.load(Ordering::SeqCst) > 0 {
                let mut result = success_result(invocation);
                result.status = "cancelled".to_string();
                return Ok(result);
            }
        }
        self.inner.execute_step(invocation)
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.cancel_step(request)
    }
}

#[test]
fn canceled_automatic_cleanup_ignores_original_and_repeated_cancel_in_both_finalizers() {
    for cancel_before_dispatch in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let store = Arc::new(MemoryRunStore::default());
        let control = Arc::new(PendingCancelControlRequests::default());
        let (tx, rx) = std::sync::mpsc::channel();
        *store.tree_inspections.lock().unwrap() = Some(tx);
        let execution = Arc::new(CancelDuringCleanupExecution {
            inner: RecordingExecution::default(),
            control: control.clone(),
            polls: Mutex::new(rx),
            cancel_calls: AtomicUsize::new(0),
            expect_pending_on_entry: cancel_before_dispatch,
        });
        let events = Arc::new(RecordingEvents::default());
        let kernel = cleanup_kernel(
            &home,
            execution.clone(),
            store.clone(),
            events.clone(),
            control.clone(),
            Arc::new(RecordingPreflight::default()),
        );
        kernel
            .start_recipe(
                tmp.path(),
                &home,
                &cleanup_recipe(vec![CleanupTrigger::Canceled]),
                Some("run"),
                BTreeMap::new(),
            )
            .unwrap();
        if cancel_before_dispatch {
            control.request_cancel("run").unwrap();
        } else {
            let mut result = success_result_for("run", "main", 1);
            result.status = "cancelled".to_string();
            execution.inner.plan_result("run", "main", 1, result);
        }
        assert_eq!(
            kernel
                .run_started_run(tmp.path(), &home, "run")
                .unwrap()
                .status,
            KernelRunStatus::Canceled
        );
        let record = store.load_run("run").unwrap().unwrap();
        assert_eq!(record.cleanup.unwrap().state, CleanupPhaseState::Succeeded);
        assert_eq!(execution.cancel_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            execution
                .inner
                .invocations()
                .iter()
                .filter(|i| i.request.step.id == "teardown")
                .count(),
            1
        );
        assert_eq!(
            events
                .snapshot()
                .iter()
                .filter(|e| e.kind == "cleanup_started")
                .count(),
            1
        );
        assert!(!control.is_pending());
        kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
        assert_eq!(
            execution
                .inner
                .invocations()
                .iter()
                .filter(|i| i.request.step.id == "teardown")
                .count(),
            1
        );
    }
}

#[test]
fn cancel_during_other_automatic_cleanup_preserves_primary_and_settles_interruption() {
    for (status, trigger) in [
        (KernelRunStatus::Failed, CleanupTrigger::Failed),
        (KernelRunStatus::Success, CleanupTrigger::Success),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let store = Arc::new(MemoryRunStore::default());
        let control = Arc::new(PendingCancelControlRequests::default());
        let (tx, rx) = std::sync::mpsc::channel();
        *store.tree_inspections.lock().unwrap() = Some(tx);
        let execution = Arc::new(CancelDuringCleanupExecution {
            inner: RecordingExecution::default(),
            control: control.clone(),
            polls: Mutex::new(rx),
            cancel_calls: AtomicUsize::new(0),
            expect_pending_on_entry: false,
        });
        let kernel = cleanup_kernel(
            &home,
            execution.clone(),
            store.clone(),
            Arc::new(RecordingEvents::default()),
            control.clone(),
            Arc::new(RecordingPreflight::default()),
        );
        if status == KernelRunStatus::Failed {
            execution.inner.plan_result(
                "run",
                "main",
                1,
                failed_result("run", "main", 1, "primary"),
            );
        }
        assert_eq!(
            kernel
                .execute_recipe(
                    tmp.path(),
                    &home,
                    &cleanup_recipe(vec![trigger]),
                    Some("run")
                )
                .unwrap()
                .status,
            status
        );
        let record = store.load_run("run").unwrap().unwrap();
        assert_eq!(record.status, status);
        assert_eq!(record.cleanup.unwrap().state, CleanupPhaseState::Failed);
        assert_eq!(execution.cancel_calls.load(Ordering::SeqCst), 1);
        assert!(!control.is_pending());
        if status == KernelRunStatus::Failed {
            assert_eq!(
                record.recovery_state,
                Some(KernelRunRecoveryState::CleanupRequired)
            );
        }
    }
}

struct CrashAfterCleanupResult {
    inner: RecordingExecution,
    store: Arc<MemoryRunStore>,
}

impl ExecutionPort for CrashAfterCleanupResult {
    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        let result = self.inner.execute_step(invocation)?;
        if invocation.request.step.id == "teardown" {
            // The next projection follows the immutable cleanup commit point.
            *self.store.fail_updates.lock().unwrap() = true;
        }
        Ok(result)
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        self.inner.cancel_step(request)
    }
}

#[test]
fn failed_automatic_cleanup_recovers_commit_without_repeating_effects() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let store = Arc::new(MemoryRunStore::default());
    let execution = Arc::new(CrashAfterCleanupResult {
        inner: RecordingExecution::default(),
        store: store.clone(),
    });
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
    );
    execution
        .inner
        .plan_result("run", "main", 1, failed_result("run", "main", 1, "primary"));
    let mut recipe = cleanup_recipe(vec![CleanupTrigger::Failed]);
    recipe.cleanup.push(RecipeCleanupStep {
        when: vec![CleanupTrigger::Failed],
        step: invoke_step("teardown2"),
    });
    let error = kernel
        .execute_recipe(tmp.path(), &home, &recipe, Some("run"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected run-state update failure")
    );
    let record = store.load_run("run").unwrap().unwrap();
    assert_eq!(record.status, KernelRunStatus::Failed);
    assert_eq!(
        record.recovery_state,
        Some(KernelRunRecoveryState::CleanupRequired)
    );
    assert_eq!(store.step_executions().len(), 2);
    assert!(kernel.resume_run(tmp.path(), &home, "run").is_err());
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    let calls = execution.inner.invocations();
    assert_eq!(
        calls
            .iter()
            .filter(|i| i.request.step.id == "teardown")
            .count(),
        1
    );
    assert_eq!(
        calls.iter().filter(|i| i.request.step.id == "main").count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|i| i.request.step.id == "teardown2")
            .count(),
        1
    );
    assert_eq!(store.step_executions().len(), 3);
    assert_eq!(
        store.load_run("run").unwrap().unwrap().recovery_state,
        Some(KernelRunRecoveryState::CleanupCompleted)
    );
}

struct CancelAfterPrimaryFailure {
    inner: RecordingExecution,
    control: Arc<PendingCancelControlRequests>,
}

impl ExecutionPort for CancelAfterPrimaryFailure {
    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        if invocation.request.step.id == "main" {
            self.control.request_cancel(&invocation.request.run_id)?;
            return Ok(failed_result(
                &invocation.request.run_id,
                "main",
                invocation.request.step.execution,
                "primary",
            ));
        }
        self.inner.execute_step(invocation)
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        self.inner.cancel_step(request)
    }
}

#[test]
fn primary_failure_committed_before_cancel_never_selects_canceled_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let store = Arc::new(MemoryRunStore::default());
    let control = Arc::new(PendingCancelControlRequests::default());
    let execution = Arc::new(CancelAfterPrimaryFailure {
        inner: RecordingExecution::default(),
        control: control.clone(),
    });
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        control.clone(),
        Arc::new(RecordingPreflight::default()),
    );
    let mut recipe = cleanup_recipe(vec![CleanupTrigger::Failed]);
    recipe.cleanup.push(RecipeCleanupStep {
        when: vec![CleanupTrigger::Canceled],
        step: invoke_step("canceled_teardown"),
    });
    assert_eq!(
        kernel
            .execute_recipe(tmp.path(), &home, &recipe, Some("run"))
            .unwrap()
            .status,
        KernelRunStatus::Failed
    );
    let record = store.load_run("run").unwrap().unwrap();
    let cleanup = record.cleanup.unwrap();
    assert_eq!(cleanup.trigger, CleanupTrigger::Failed);
    assert_eq!(cleanup.state, CleanupPhaseState::Failed);
    assert_eq!(
        record.recovery_state,
        Some(KernelRunRecoveryState::CleanupRequired)
    );
    assert!(execution.inner.invocations().is_empty());
    assert!(!control.is_pending());
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    assert_eq!(execution.inner.invocations().len(), 1);
    assert_eq!(execution.inner.invocations()[0].request.step.id, "teardown");
}

#[test]
fn cleanup_trigger_change_preserves_lifetime_execution_records() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
    );
    let mut canceled = success_result_for("run", "main", 1);
    canceled.status = "cancelled".to_string();
    execution.plan_result("run", "main", 1, canceled);
    let recipe = cleanup_recipe(vec![CleanupTrigger::Canceled, CleanupTrigger::Success]);
    assert_eq!(
        kernel
            .execute_recipe(tmp.path(), &home, &recipe, Some("run"))
            .unwrap()
            .status,
        KernelRunStatus::Canceled
    );
    let first_records = store.step_executions();
    assert_eq!(
        kernel.resume_run(tmp.path(), &home, "run").unwrap().status,
        KernelRunStatus::Success
    );
    let records = store.step_executions();
    assert_eq!(&records[..first_records.len()], first_records.as_slice());
    let cleanup_calls: Vec<_> = execution
        .invocations()
        .into_iter()
        .filter(|i| i.request.step.id == "teardown")
        .collect();
    assert_eq!(
        cleanup_calls
            .iter()
            .map(|i| i.request.step.execution)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

struct ChildCancellationExecution {
    inner: RecordingExecution,
    store: Arc<MemoryRunStore>,
}

impl ExecutionPort for ChildCancellationExecution {
    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        assert_ne!(
            invocation.request.step.id, "child_teardown",
            "cleanup is root-owned"
        );
        if invocation.request.step.id == "teardown" {
            let child = self.store.load_run("run__child")?.unwrap();
            assert_eq!(
                child.status,
                KernelRunStatus::Canceled,
                "child cancellation precedes root cleanup"
            );
            assert!(child.cleanup.is_none());
        }
        self.inner.execute_step(invocation)
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        self.inner.cancel_step(request)
    }
}

#[test]
fn root_automatic_cleanup_follows_child_cancellation_without_child_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let store = Arc::new(MemoryRunStore::default());
    let execution = Arc::new(ChildCancellationExecution {
        inner: RecordingExecution::default(),
        store: store.clone(),
    });
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
    );
    let mut canceled = success_result_for("run__child", "nested", 1);
    canceled.status = "cancelled".to_string();
    execution
        .inner
        .plan_result("run__child", "nested", 1, canceled);
    let mut child = recipe("child", vec![invoke_step("nested")], None);
    child.cleanup.push(RecipeCleanupStep {
        when: vec![CleanupTrigger::Canceled],
        step: invoke_step("child_teardown"),
    });
    let mut recipe = cleanup_recipe(vec![CleanupTrigger::Canceled]);
    recipe.steps = vec![subflow_step("child", child)];
    assert_eq!(
        kernel
            .execute_recipe(tmp.path(), &home, &recipe, Some("run"))
            .unwrap()
            .status,
        KernelRunStatus::Canceled
    );
    assert_eq!(
        execution
            .inner
            .invocations()
            .iter()
            .filter(|i| i.request.step.id == "teardown")
            .count(),
        1
    );
}

#[test]
fn unmatched_cleanup_never_dispatches_for_success_or_cancellation() {
    for canceled in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let execution = Arc::new(RecordingExecution::default());
        let store = Arc::new(MemoryRunStore::default());
        let events = Arc::new(RecordingEvents::default());
        let kernel = cleanup_kernel(
            &home,
            execution.clone(),
            store.clone(),
            events.clone(),
            Arc::new(NoopControlRequests),
            Arc::new(RecordingPreflight::default()),
        );
        if canceled {
            let mut result = success_result_for("run", "main", 1);
            result.status = "cancelled".to_string();
            execution.plan_result("run", "main", 1, result);
        }
        kernel
            .execute_recipe(
                tmp.path(),
                &home,
                &cleanup_recipe(vec![CleanupTrigger::Failed]),
                Some("run"),
            )
            .unwrap();
        assert_eq!(
            store
                .load_run("run")
                .unwrap()
                .unwrap()
                .cleanup
                .unwrap()
                .state,
            CleanupPhaseState::Skipped
        );
        assert_eq!(execution.invocations().len(), 1);
        assert!(
            !events
                .snapshot()
                .iter()
                .any(|e| e.kind == "cleanup_started")
        );
    }
}

fn seed_descendant(
    kernel: &RuntimeKernel,
    store: &MemoryRunStore,
    repo: &Path,
    home: &Path,
    run_id: &str,
    parent_run_id: &str,
    recipe: &Recipe,
) -> PersistedRunRecord {
    kernel
        .start_recipe(repo, home, recipe, Some(run_id), BTreeMap::new())
        .unwrap();
    let mut record = store.load_run(run_id).unwrap().unwrap();
    let lineage = record.lineage.as_mut().unwrap();
    lineage.root_run_id = "run".to_string();
    lineage.parent_run_id = Some(parent_run_id.to_string());
    lineage.parent_step_id = Some("child".to_string());
    store.update_run_state(run_id, &record).unwrap();
    record
}

#[test]
fn pending_root_cancel_settles_existing_descendants_deepest_first_before_cleanup() {
    for child_in_flight in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let execution = Arc::new(RecordingExecution::default());
        let store = Arc::new(MemoryRunStore::default());
        let events = Arc::new(RecordingEvents::default());
        let control = Arc::new(PendingCancelControlRequests::default());
        let kernel = cleanup_kernel(
            &home,
            execution.clone(),
            store.clone(),
            events.clone(),
            control.clone(),
            Arc::new(RecordingPreflight::default()),
        );
        let mut child_recipe = recipe("child", vec![invoke_step("nested")], None);
        child_recipe.cleanup.push(RecipeCleanupStep {
            when: vec![CleanupTrigger::Canceled],
            step: invoke_step("child_teardown"),
        });
        let mut root_recipe = cleanup_recipe(vec![CleanupTrigger::Canceled]);
        root_recipe.steps = vec![subflow_step("child", child_recipe.clone())];
        kernel
            .start_recipe(
                tmp.path(),
                &home,
                &root_recipe,
                Some("run"),
                BTreeMap::new(),
            )
            .unwrap();
        let mut child = seed_descendant(
            &kernel,
            &store,
            tmp.path(),
            &home,
            "run__child",
            "run",
            &child_recipe,
        );
        if child_in_flight {
            child.status = KernelRunStatus::Running;
            let step = child.steps.get_mut("nested").unwrap();
            step.status = transist_core::KernelStepStatus::Running;
            step.executions = 1;
            store.update_run_state("run__child", &child).unwrap();
            // Root recovery after a crash must settle the same orphan subtree.
            let mut root = store.load_run("run").unwrap().unwrap();
            root.status = KernelRunStatus::Running;
            store.update_run_state("run", &root).unwrap();
        }
        seed_descendant(
            &kernel,
            &store,
            tmp.path(),
            &home,
            "run__grandchild",
            "run__child",
            &child_recipe,
        );
        let mut terminals = Vec::new();
        for (id, status) in [
            ("run__old_success", KernelRunStatus::Success),
            ("run__old_failed", KernelRunStatus::Failed),
            ("run__old_canceled", KernelRunStatus::Canceled),
        ] {
            let mut record =
                seed_descendant(&kernel, &store, tmp.path(), &home, id, "run", &child_recipe);
            record.status = status;
            store.update_run_state(id, &record).unwrap();
            terminals.push(record);
        }
        kernel
            .start_recipe(
                tmp.path(),
                &home,
                &child_recipe,
                Some("unrelated"),
                BTreeMap::new(),
            )
            .unwrap();
        control.request_cancel("run").unwrap();
        assert_eq!(
            kernel
                .run_started_run(tmp.path(), &home, "run")
                .unwrap()
                .status,
            KernelRunStatus::Canceled
        );
        for id in ["run__child", "run__grandchild"] {
            let record = store.load_run(id).unwrap().unwrap();
            assert_eq!(record.status, KernelRunStatus::Canceled);
            assert!(record.cleanup.is_none(), "child cleanup never runs");
            assert_eq!(
                record.steps["nested"].status,
                transist_core::KernelStepStatus::Canceled
            );
        }
        for before in terminals {
            assert_eq!(
                serde_json::to_value(store.load_run(&before.run_id).unwrap().unwrap()).unwrap(),
                serde_json::to_value(before).unwrap()
            );
        }
        assert_eq!(
            store.load_run("unrelated").unwrap().unwrap().status,
            KernelRunStatus::Created
        );
        let events = events.snapshot();
        let stopped = |id: &str| {
            events
                .iter()
                .position(|e| e.kind == "run_stopped" && e.run_id == id)
                .unwrap()
        };
        let cleanup = events
            .iter()
            .position(|e| e.kind == "cleanup_started")
            .unwrap();
        assert!(stopped("run__grandchild") < stopped("run__child"));
        assert!(stopped("run__child") < cleanup);
        assert_eq!(execution.invocations().len(), 1);
        assert_eq!(execution.invocations()[0].request.step.id, "teardown");
        if child_in_flight {
            let interrupted = store
                .load_step_execution("run__child", "nested", 1)
                .unwrap()
                .unwrap();
            assert_eq!(interrupted.status, "interrupted");
        }
        assert!(!control.is_pending());
    }
}

#[test]
fn reused_canceled_child_resume_honors_new_root_cancel_without_clearing_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let control = Arc::new(PendingCancelControlRequests::default());
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        control.clone(),
        Arc::new(RecordingPreflight::default()),
    );
    let child_recipe = recipe("child", vec![invoke_step("nested")], None);
    let root_recipe = recipe(
        "parent",
        vec![subflow_step("child", child_recipe.clone())],
        None,
    );
    kernel
        .start_recipe(
            tmp.path(),
            &home,
            &root_recipe,
            Some("run"),
            BTreeMap::new(),
        )
        .unwrap();
    let mut child = seed_descendant(
        &kernel,
        &store,
        tmp.path(),
        &home,
        "run__child",
        "run",
        &child_recipe,
    );
    child.status = KernelRunStatus::Canceled;
    child.steps.get_mut("nested").unwrap().status = transist_core::KernelStepStatus::Canceled;
    store.update_run_state("run__child", &child).unwrap();
    let mut root = store.load_run("run").unwrap().unwrap();
    root.status = KernelRunStatus::Running;
    store.update_run_state("run", &root).unwrap();
    control.request_cancel("run").unwrap();
    assert_eq!(
        kernel
            .resume_run(tmp.path(), &home, "run__child")
            .unwrap()
            .status,
        KernelRunStatus::Canceled
    );
    assert!(
        control.is_pending(),
        "a child cannot consume root cancellation"
    );
    assert!(execution.invocations().is_empty());
    assert_eq!(
        store.load_run("run").unwrap().unwrap().status,
        KernelRunStatus::Running
    );
    assert_eq!(
        kernel
            .run_started_run(tmp.path(), &home, "run")
            .unwrap()
            .status,
        KernelRunStatus::Canceled
    );
    assert!(!control.is_pending());
}

#[test]
fn terminal_cleanup_projection_backfills_missing_history_once_before_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let preflight = Arc::new(RecoveryCheckingPreflight {
        store: store.clone(),
        fail_main: AtomicBool::new(false),
        fail_cleanup: AtomicBool::new(true),
    });
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        events.clone(),
        Arc::new(NoopControlRequests),
        preflight.clone(),
    );
    kernel
        .start_recipe(
            tmp.path(),
            &home,
            &cleanup_recipe(vec![CleanupTrigger::Failed]),
            Some("run"),
            BTreeMap::new(),
        )
        .unwrap();
    let result = failed_result("run", "teardown", 1, "committed before append crash");
    let committed = transist_core::StepExecutionRecord {
        run_id: "run".to_string(),
        step_id: "teardown".to_string(),
        execution: 1,
        status: result.status.clone(),
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        retry_wait_until_ms: None,
        request: None,
        result: Some(result.clone()),
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: Vec::new(),
    };
    store
        .store_step_execution(&RunStoreStepExecution {
            run_id: "run".to_string(),
            step_id: "teardown".to_string(),
            execution: 1,
            state: serde_json::to_value(&committed).unwrap().into(),
        })
        .unwrap();
    let mut record = store.load_run("run").unwrap().unwrap();
    record.status = KernelRunStatus::Failed;
    record.recovery_state = Some(KernelRunRecoveryState::CleanupRequired);
    let mut cleanup = transist_core::PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown".to_string()],
        CleanupPhaseState::Running,
    );
    let step = cleanup.steps.get_mut("teardown").unwrap();
    step.status = transist_core::KernelStepStatus::Failed;
    step.executions = 1;
    step.last_result = Some(result);
    step.started_at_ms = Some(1);
    step.finished_at_ms = Some(2);
    record.cleanup = Some(cleanup);
    store.update_run_state("run", &record).unwrap();
    for _ in 0..2 {
        assert!(kernel.cleanup_run(tmp.path(), &home, "run").is_err());
        assert_eq!(
            store
                .failures()
                .iter()
                .filter(|f| f.step_id == "teardown" && f.execution_attempt == 1)
                .count(),
            1
        );
        assert_eq!(
            events
                .snapshot()
                .iter()
                .filter(
                    |e| e.kind == "cleanup_step_failed" && e.step_id.as_deref() == Some("teardown")
                )
                .count(),
            1
        );
        assert!(execution.invocations().is_empty());
        assert_eq!(
            store
                .load_run("run")
                .unwrap()
                .unwrap()
                .cleanup
                .unwrap()
                .steps["teardown"]
                .status,
            transist_core::KernelStepStatus::Failed
        );
    }
    preflight.fail_cleanup.store(false, Ordering::SeqCst);
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    assert_eq!(execution.invocations()[0].request.step.execution, 2);
    assert_eq!(
        serde_json::to_value(
            store
                .load_step_execution("run", "teardown", 1)
                .unwrap()
                .unwrap()
                .result
        )
        .unwrap(),
        serde_json::to_value(committed.result).unwrap()
    );
}

#[test]
fn second_canceled_primary_activation_runs_cleanup_at_next_lifetime_ordinal() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let execution = Arc::new(RecordingExecution::default());
    let store = Arc::new(MemoryRunStore::default());
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
    );
    for ordinal in [1, 2] {
        let mut result = success_result_for("run", "main", ordinal);
        result.status = "cancelled".to_string();
        execution.plan_result("run", "main", ordinal, result);
    }
    let recipe = cleanup_recipe(vec![CleanupTrigger::Canceled]);
    assert_eq!(
        kernel
            .execute_recipe(tmp.path(), &home, &recipe, Some("run"))
            .unwrap()
            .status,
        KernelRunStatus::Canceled
    );
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    let first_records = store.step_executions();
    assert_eq!(execution.invocations().len(), 2);
    assert_eq!(
        kernel.resume_run(tmp.path(), &home, "run").unwrap().status,
        KernelRunStatus::Canceled
    );
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    let ordinals: Vec<_> = execution
        .invocations()
        .iter()
        .filter(|i| i.request.step.id == "teardown")
        .map(|i| i.request.step.execution)
        .collect();
    assert_eq!(ordinals, vec![1, 2]);
    let records = store.step_executions();
    assert_eq!(&records[..first_records.len()], first_records.as_slice());
    assert_eq!(records.len(), 4);
}

#[derive(Default)]
struct GenerationalCleanupControl {
    pending: Mutex<Option<ControlRequestRecord>>,
    generations: AtomicUsize,
    inspections: AtomicUsize,
    replace_on_inspection: AtomicUsize,
    clears: AtomicUsize,
}

impl GenerationalCleanupControl {
    fn new_request(&self, root_run_id: &str) -> ControlRequestRecord {
        ControlRequestRecord {
            schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
            request_id: format!(
                "cancel-{}",
                self.generations.fetch_add(1, Ordering::SeqCst) + 1
            ),
            request_type: ControlRequestType::Cancel,
            root_run_id: root_run_id.to_string(),
            requested_at_ms: 1,
            requester: None,
        }
    }

    fn pending_id(&self) -> Option<String> {
        self.pending
            .lock()
            .unwrap()
            .as_ref()
            .map(|request| request.request_id.clone())
    }
}

impl ControlRequestPort for GenerationalCleanupControl {
    fn request_cancel(&self, root_run_id: &str) -> Result<ControlRequestRecord> {
        let mut pending = self.pending.lock().unwrap();
        Ok(pending
            .get_or_insert_with(|| self.new_request(root_run_id))
            .clone())
    }

    fn inspect_cancel_request(&self, root_run_id: &str) -> Result<Option<ControlRequestRecord>> {
        let inspection = self.inspections.fetch_add(1, Ordering::SeqCst) + 1;
        let mut pending = self.pending.lock().unwrap();
        if inspection == self.replace_on_inspection.load(Ordering::SeqCst) {
            // Simulate observing a different request after entry preparation.
            // An identity mismatch must never authorize clearing that request.
            *pending = Some(self.new_request(root_run_id));
        }
        Ok(pending.clone())
    }

    fn clear_cancel_request(&self, _root_run_id: &str) -> Result<()> {
        if self.pending.lock().unwrap().take().is_some() {
            self.clears.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

fn seed_failed_cleanup_recovery(
    kernel: &RuntimeKernel,
    store: &MemoryRunStore,
    repo: &Path,
    home: &Path,
) {
    kernel
        .start_recipe(
            repo,
            home,
            &cleanup_recipe(vec![CleanupTrigger::Failed]),
            Some("run"),
            BTreeMap::new(),
        )
        .unwrap();
    let mut record = store.load_run("run").unwrap().unwrap();
    record.status = KernelRunStatus::Failed;
    record.recovery_state = Some(KernelRunRecoveryState::CleanupRequired);
    record.cleanup = Some(transist_core::PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown".to_string()],
        CleanupPhaseState::Pending,
    ));
    store.update_run_state("run", &record).unwrap();
}

#[test]
fn explicit_failed_cleanup_retires_prior_operation_cancel_after_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let store = Arc::new(MemoryRunStore::default());
    let control = Arc::new(GenerationalCleanupControl::default());
    let execution = Arc::new(RecordingExecution::default());
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        control.clone(),
        Arc::new(RecordingPreflight::default()),
    );
    seed_failed_cleanup_recovery(&kernel, &store, tmp.path(), &home);
    control.request_cancel("run").unwrap();
    let summary = kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    assert_eq!(summary.status, KernelRunStatus::Failed);
    assert_eq!(control.clears.load(Ordering::SeqCst), 1);
    assert!(control.pending_id().is_none());
    assert_eq!(execution.invocations().len(), 1);
    assert_eq!(
        store.load_run("run").unwrap().unwrap().recovery_state,
        Some(KernelRunRecoveryState::CleanupCompleted)
    );
}

struct NewCancelDuringExplicitCleanup {
    inner: RecordingExecution,
    control: Arc<GenerationalCleanupControl>,
    polls: Mutex<std::sync::mpsc::Receiver<()>>,
    inject_once: AtomicBool,
    old_request_id: String,
    cancel_calls: AtomicUsize,
}

impl ExecutionPort for NewCancelDuringExplicitCleanup {
    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        let mut result = self.inner.execute_step(invocation)?;
        if self.inject_once.swap(false, Ordering::SeqCst) {
            assert!(
                self.control.pending_id().is_none(),
                "prior request retired before cleanup dispatch"
            );
            let request = self.control.request_cancel(&invocation.request.run_id)?;
            assert_ne!(request.request_id, self.old_request_id);
            let polls = self.polls.lock().unwrap();
            for _ in 0..2 {
                polls
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("watcher inspected selected tree");
            }
            assert_eq!(self.cancel_calls.load(Ordering::SeqCst), 1);
            result.status = "cancelled".to_string();
        }
        Ok(result)
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.cancel_step(request)
    }
}

#[test]
fn explicit_failed_cleanup_honors_new_cancel_generation_after_retiring_old_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let store = Arc::new(MemoryRunStore::default());
    let control = Arc::new(GenerationalCleanupControl::default());
    let (tx, rx) = std::sync::mpsc::channel();
    *store.tree_inspections.lock().unwrap() = Some(tx);
    let execution = Arc::new(NewCancelDuringExplicitCleanup {
        inner: RecordingExecution::default(),
        control: control.clone(),
        polls: Mutex::new(rx),
        inject_once: AtomicBool::new(true),
        old_request_id: "cancel-1".to_string(),
        cancel_calls: AtomicUsize::new(0),
    });
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        control.clone(),
        Arc::new(RecordingPreflight::default()),
    );
    seed_failed_cleanup_recovery(&kernel, &store, tmp.path(), &home);
    let old = control.request_cancel("run").unwrap();
    // An unrelated corrupt run must not prevent watcher targeting either.
    store
        .runs
        .lock()
        .unwrap()
        .insert("unrelated".to_string(), serde_json::json!("corrupt state"));
    store
        .physical_tree_memberships
        .lock()
        .unwrap()
        .insert("unrelated".to_string(), "unrelated".to_string());
    let error = kernel.cleanup_run(tmp.path(), &home, "run").unwrap_err();
    assert!(error.to_string().contains("was interrupted"));
    let newer_id = control.pending_id().unwrap();
    assert_ne!(newer_id, old.request_id);
    assert_eq!(
        control.clears.load(Ordering::SeqCst),
        1,
        "new request must remain intact"
    );
    let record = store.load_run("run").unwrap().unwrap();
    assert_eq!(record.status, KernelRunStatus::Failed);
    assert_eq!(
        record.recovery_state,
        Some(KernelRunRecoveryState::CleanupRequired)
    );
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    assert_eq!(
        execution
            .inner
            .invocations()
            .iter()
            .map(|i| i.request.step.execution)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(control.clears.load(Ordering::SeqCst), 2);
    assert!(control.pending_id().is_none());
}

#[test]
fn explicit_cleanup_never_clears_a_changed_entry_request_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let store = Arc::new(MemoryRunStore::default());
    let control = Arc::new(GenerationalCleanupControl::default());
    let execution = Arc::new(RecordingExecution::default());
    let kernel = cleanup_kernel(
        &home,
        execution.clone(),
        store.clone(),
        Arc::new(RecordingEvents::default()),
        control.clone(),
        Arc::new(RecordingPreflight::default()),
    );
    seed_failed_cleanup_recovery(&kernel, &store, tmp.path(), &home);
    let old = control.request_cancel("run").unwrap();
    control.replace_on_inspection.store(2, Ordering::SeqCst);
    assert!(
        kernel
            .cleanup_run(tmp.path(), &home, "run")
            .unwrap_err()
            .to_string()
            .contains("was interrupted")
    );
    assert_ne!(control.pending_id().unwrap(), old.request_id);
    assert_eq!(control.clears.load(Ordering::SeqCst), 0);
    assert!(execution.invocations().is_empty());
}

#[test]
fn selected_root_cancellation_ignores_unrelated_corrupt_state_but_rejects_related_corruption() {
    for corruption in ["unrelated", "related_state", "related_lineage"] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let store = Arc::new(MemoryRunStore::default());
        let control = Arc::new(PendingCancelControlRequests::default());
        let execution = Arc::new(RecordingExecution::default());
        let kernel = cleanup_kernel(
            &home,
            execution.clone(),
            store.clone(),
            Arc::new(RecordingEvents::default()),
            control.clone(),
            Arc::new(RecordingPreflight::default()),
        );
        kernel
            .start_recipe(
                tmp.path(),
                &home,
                &cleanup_recipe(vec![CleanupTrigger::Canceled]),
                Some("run"),
                BTreeMap::new(),
            )
            .unwrap();
        let mut child = seed_descendant(
            &kernel,
            &store,
            tmp.path(),
            &home,
            "run__child",
            "run",
            &recipe("child", vec![invoke_step("nested")], None),
        );
        store
            .physical_tree_memberships
            .lock()
            .unwrap()
            .insert("run__child".to_string(), "run".to_string());
        match corruption {
            "unrelated" => {
                store
                    .runs
                    .lock()
                    .unwrap()
                    .insert("unrelated".to_string(), serde_json::json!("corrupt state"));
                store
                    .physical_tree_memberships
                    .lock()
                    .unwrap()
                    .insert("unrelated".to_string(), "unrelated".to_string());
            }
            "related_state" => {
                store
                    .runs
                    .lock()
                    .unwrap()
                    .insert("run__child".to_string(), serde_json::json!("corrupt state"));
            }
            "related_lineage" => {
                child.lineage.as_mut().unwrap().root_run_id = "unrelated".to_string();
                store.update_run_state("run__child", &child).unwrap();
            }
            _ => unreachable!(),
        }
        control.request_cancel("run").unwrap();
        let result = kernel.run_started_run(tmp.path(), &home, "run");
        if corruption == "unrelated" {
            assert_eq!(result.unwrap().status, KernelRunStatus::Canceled);
            assert_eq!(
                store.load_run("run__child").unwrap().unwrap().status,
                KernelRunStatus::Canceled
            );
            assert_eq!(execution.invocations().len(), 1);
        } else {
            assert!(
                result.is_err(),
                "corruption inside selected tree must fail closed"
            );
            assert!(
                execution.invocations().is_empty(),
                "root cleanup cannot precede validation of descendants"
            );
            assert!(control.is_pending());
        }
    }
}
