// SPDX-License-Identifier: MPL-2.0
//! Kernel-level document commit and re-entry tests using the existing port harness.
use super::*;
use transist_core::ports::{DocumentOutput, DocumentStorePort};
use transist_protocol::{DocumentBlob, DocumentDeclaration, DocumentKind, DocumentRevision};

impl DocumentStorePort for MemoryRunStore {
    fn prepare_document_revisions(
        &self,
        _run: &str,
        _step: &str,
        _execution: u32,
        _outputs: &[DocumentOutput],
        _redactor: &transist_core::Redactor,
    ) -> Result<Vec<DocumentRevision>> {
        bail!("test harness captures candidates at its fake execution boundary")
    }
    fn prepared_document_revisions(
        &self,
        run: &str,
        step: &str,
        execution: u32,
    ) -> Result<Vec<DocumentRevision>> {
        Ok(self
            .document_candidates
            .lock()
            .unwrap()
            .get(&(run.into(), step.into(), execution))
            .cloned()
            .unwrap_or_default())
    }
    fn document_heads(&self, run: &str) -> Result<BTreeMap<String, DocumentRevision>> {
        let mut heads = BTreeMap::new();
        for value in self.step_executions() {
            let record: transist_core::StepExecutionRecord = serde_json::from_value(value)?;
            if record.run_id == run {
                assert!(record.status == "success" || record.document_revisions.is_empty());
                for revision in record.document_revisions {
                    heads.insert(revision.document.clone(), revision);
                }
            }
        }
        Ok(heads)
    }
    fn read_document_blob(&self, _run: &str, _revision: &DocumentRevision) -> Result<Vec<u8>> {
        bail!("unused")
    }
    fn read_document_member(
        &self,
        _run: &str,
        _revision: &DocumentRevision,
        _member: &str,
    ) -> Result<Vec<u8>> {
        bail!("unused")
    }
    fn reconcile_document_heads(&self, run: &str) -> Result<()> {
        self.document_heads(run)?;
        self.trace("document_heads_reconciled");
        Ok(())
    }
}

struct Producer {
    store: Arc<MemoryRunStore>,
    calls: AtomicUsize,
    status: Mutex<String>,
}

impl ExecutionPort for Producer {
    fn capabilities(&self) -> AdapterCapabilities {
        RecordingExecution::default().capabilities()
    }
    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        Ok(ExecutionCancelResult {
            invoke_id: request.invoke_id.clone(),
            accepted: true,
        })
    }
    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let request = &invocation.request;
        let revisions = ["a", "b"]
            .into_iter()
            .map(|name| DocumentRevision {
                schema: DocumentRevision::SCHEMA.into(),
                revision_id: format!("{name}-{}-{}", request.run_id, request.step.execution),
                root_run_id: request.run_id.clone(),
                document: name.into(),
                run_id: request.run_id.clone(),
                step_id: request.step.id.clone(),
                execution: request.step.execution,
                commit_sequence: request.step.execution.into(),
                kind: DocumentKind::File,
                blob: DocumentBlob {
                    sha256: "a".repeat(64),
                    size_bytes: 3,
                },
                size_bytes: 3,
                file_count: 1,
            })
            .collect();
        self.store.document_candidates.lock().unwrap().insert(
            (
                request.run_id.clone(),
                request.step.id.clone(),
                request.step.execution,
            ),
            revisions,
        );
        let mut result =
            success_result_for(&request.run_id, &request.step.id, request.step.execution);
        result.status = self.status.lock().unwrap().clone();
        Ok(result)
    }
}

struct ScopedArtifacts(PathBuf);

impl ArtifactPort for ScopedArtifacts {
    fn write_artifact(&self, request: &ArtifactWriteRequest) -> Result<PathBuf> {
        let path = self
            .0
            .join("runs")
            .join(format!("run_{}", request.run_id))
            .join("artifacts")
            .join(&request.step_id)
            .join(format!("execution_{:03}", request.execution))
            .join(&request.logical_path);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_vec(&request.payload)?)?;
        Ok(path)
    }
    fn read_artifact(&self, _request: &ArtifactReadRequest) -> Result<KernelValue> {
        bail!("unused")
    }
}

fn setup(
    home: &Path,
) -> (
    RuntimeKernel,
    Arc<MemoryRunStore>,
    Arc<Producer>,
    OperationTrace,
) {
    let trace = OperationTrace::default();
    let store = Arc::new(MemoryRunStore {
        documents_enabled: true,
        operation_trace: Some(trace.clone()),
        ..Default::default()
    });
    let execution = Arc::new(Producer {
        store: store.clone(),
        calls: AtomicUsize::new(0),
        status: Mutex::new("success".into()),
    });
    let kernel = RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution: execution.clone(),
        artifacts: Arc::new(ScopedArtifacts(home.to_path_buf())),
        run_store: store.clone(),
        lease: Arc::new(NoopLease::default()),
        control: Arc::new(NoopControlRequests),
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events: Arc::new(RecordingEvents::default()),
        policy: Arc::new(AllowPolicy),
        preflight: Arc::new(RecordingPreflight::default()),
        secrets: Arc::new(StaticSecrets),
        plugin_verifier: Arc::new(AllowPluginVerification),
        plugin_metadata: Arc::new(TestPluginMetadata),
        prompt_files: Arc::new(TestPromptFiles),
        home_settings: Arc::new(DefaultHomeSettings),
        telemetry: Arc::new(NoopTelemetry::default()),
    }));
    (kernel, store, execution, trace)
}

fn document_recipe() -> Recipe {
    let mut recipe = recipe("document-wf", vec![invoke_step("produce")], None);
    recipe.workflow.version = "0.1.0".into();
    recipe.documents = ["a", "b"]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                DocumentDeclaration {
                    kind: DocumentKind::File,
                    max_bytes: 32,
                    max_files: None,
                    initial: None,
                },
            )
        })
        .collect();
    recipe
}

#[test]
fn document_recovery_main_commit_never_redispatches_and_replay_reruns_in_new_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let (kernel, store, execution, trace) = setup(&home);
    transist_core::set(transist_core::FAULT_STEP_EXECUTION_RECORD);
    assert!(
        kernel
            .execute_recipe(tmp.path(), &home, &document_recipe(), Some("source"))
            .is_err()
    );
    transist_core::clear();
    assert_eq!(store.document_heads("source").unwrap().len(), 2);
    assert_eq!(
        store.run_json("source")["steps"]["produce"]["status"],
        "running"
    );
    kernel.run_started_run(tmp.path(), &home, "source").unwrap();
    assert_eq!(execution.calls.load(Ordering::SeqCst), 1);
    assert!(
        trace
            .snapshot()
            .iter()
            .any(|entry| entry == "document_heads_reconciled")
    );
    assert!(store.document_heads("replay").unwrap().is_empty());
    kernel
        .replay_run(tmp.path(), &home, "source", Some("replay"))
        .unwrap();
    assert_eq!(execution.calls.load(Ordering::SeqCst), 2);
    assert!(
        store
            .document_heads("replay")
            .unwrap()
            .values()
            .all(|revision| revision.run_id == "replay"
                && revision.root_run_id == "replay"
                && revision.execution == 1)
    );
}

#[test]
fn document_recovery_uncommitted_candidates_get_interrupted_ordinal_and_retry_n_plus_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let (kernel, store, execution, _) = setup(&home);
    transist_core::set(transist_core::FAULT_STEP_RESULT_PROMOTION);
    assert!(
        kernel
            .execute_recipe(tmp.path(), &home, &document_recipe(), Some("run"))
            .is_err()
    );
    transist_core::clear();
    assert!(store.document_heads("run").unwrap().is_empty());
    kernel.run_started_run(tmp.path(), &home, "run").unwrap();
    assert_eq!(execution.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store
            .load_step_execution("run", "produce", 1)
            .unwrap()
            .unwrap()
            .status,
        "interrupted"
    );
    assert!(
        store
            .load_step_execution("run", "produce", 1)
            .unwrap()
            .unwrap()
            .document_revisions
            .is_empty()
    );
    assert!(
        store
            .document_heads("run")
            .unwrap()
            .values()
            .all(|revision| revision.execution == 2)
    );
}

#[test]
fn document_recovery_failed_and_canceled_results_do_not_publish_candidates() {
    for status in ["failed", "cancelled"] {
        let tmp = tempfile::tempdir().unwrap();
        let home = test_home_dir(&tmp);
        let (kernel, store, execution, _) = setup(&home);
        *execution.status.lock().unwrap() = status.into();
        kernel
            .execute_recipe(tmp.path(), &home, &document_recipe(), Some("run"))
            .unwrap();
        assert!(store.document_heads("run").unwrap().is_empty());
        assert!(
            store
                .load_step_execution("run", "produce", 1)
                .unwrap()
                .unwrap()
                .document_revisions
                .is_empty()
        );
    }
}

#[test]
fn document_recovery_cleanup_uses_the_shared_commit_and_reconciliation_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let (kernel, store, execution, trace) = setup(&home);
    let mut recipe = document_recipe();
    recipe.cleanup.push(transist_protocol::RecipeCleanupStep {
        when: vec![transist_protocol::CleanupTrigger::Failed],
        step: invoke_step("teardown"),
    });
    kernel
        .start_recipe(tmp.path(), &home, &recipe, Some("run"), BTreeMap::new())
        .unwrap();
    let mut record = store.load_run("run").unwrap().unwrap();
    record.status = KernelRunStatus::Failed;
    store.update_run_state("run", &record).unwrap();
    kernel
        .cleanup_run(tmp.path(), &home, "run")
        .unwrap_or_else(|error| {
            panic!(
                "{error:#}: {}",
                serde_json::to_string_pretty(
                    &store.run_json("run")["cleanup"]["steps"]["teardown"]["last_result"]
                )
                .unwrap()
            )
        });
    let committed = store
        .load_step_execution("run", "teardown", 1)
        .unwrap()
        .unwrap();
    assert_eq!(committed.document_revisions.len(), 2);
    let mut record = store.load_run("run").unwrap().unwrap();
    let cleanup = record.cleanup.as_mut().unwrap();
    cleanup.state = transist_core::CleanupPhaseState::Running;
    let step = cleanup.steps.get_mut("teardown").unwrap();
    step.status = transist_core::KernelStepStatus::Running;
    step.last_result = None;
    store.update_run_state("run", &record).unwrap();
    trace.clear();
    kernel.cleanup_run(tmp.path(), &home, "run").unwrap();
    assert_eq!(execution.calls.load(Ordering::SeqCst), 1);
    assert!(
        trace
            .snapshot()
            .iter()
            .any(|entry| entry == "document_heads_reconciled")
    );
    assert_eq!(
        store
            .load_step_execution("run", "teardown", 1)
            .unwrap()
            .unwrap()
            .document_revisions,
        committed.document_revisions
    );
}

#[test]
fn document_recovery_rejects_reserved_objects_before_hand_built_request_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let home = test_home_dir(&tmp);
    let (kernel, _, execution, _) = setup(&home);
    let mut recipe = document_recipe();
    recipe.steps[0].with.insert(
        "output".into(),
        KernelValue::from_json(serde_json::json!({"nested":[{"$document":"a", "access":"write"}]})),
    );
    assert!(
        kernel
            .start_recipe(tmp.path(), &home, &recipe, Some("run"), BTreeMap::new())
            .is_err()
    );
    assert_eq!(execution.calls.load(Ordering::SeqCst), 0);
}
