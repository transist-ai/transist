// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use transist_core::FailureClass;
use transist_core::ports::{
    ArtifactPort, ArtifactReadRequest, ArtifactWriteRequest, ClockPort, ControlRequestPort,
    ControlRequestRecord, ControlRequestType, EventPort, ExecutionCancelRequest,
    ExecutionCancelResult, ExecutionInvocation, ExecutionPort, HomeSettingsPort, IdGeneratorPort,
    InvocationStagingPort, LeaseBreakOptions, LeaseHandle, LeasePort, ManagementMode,
    PluginActionMetadata, PluginMetadataPort, PluginVerificationPort, PolicyDecision, PolicyPort,
    PreflightInvocation, PreflightPort, PromptFilePort, PromptFileReference, RemoteSecretPolicy,
    RunBirthArtifacts, RunStorePort, RunStoreStepExecution, RunStoreStepState,
    RuntimeOwnerIdentity, SecretProviderPort, TelemetryPort, TrustDecisionPort,
};
use transist_core::{
    KernelRunSnapshot, KernelRunStatus, PersistedPreflightRecord, PersistedRunRecord,
    PreflightCheck, PreflightStatus, RecipeInputBindingErrorKind, RecipeSnapshotRef, RunLineage,
    RunLineageMode, RuntimeKernel, RuntimeServiceParts, RuntimeServices, TRUST_DECISION_SCHEMA_V1,
    TelemetryEvent, TrustDecisionRecord, TrustSubjectDecision, bind_recipe_inputs,
};
use transist_protocol::{
    AdapterCapabilities, AdapterKind, ExecutionRequest, ExecutionResult, FileRef, HomeSettings,
    KernelValue, Recipe, RecipeGraph, RecipeGraphEdge, RecipeInputSpec, RecipeLoop, RecipeStep,
    RuntimeEvent, StepExecutionDescriptor, TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
    WorkflowRef,
};

struct AllowPluginVerification;

struct TestPluginMetadata;

impl PluginMetadataPort for TestPluginMetadata {
    fn resolve_action(
        &self,
        _plugin_id: &str,
        _plugin_version: &str,
        _action: &str,
    ) -> Result<Option<PluginActionMetadata>> {
        Ok(Some(PluginActionMetadata {
            provider_kind: transist_protocol::ProviderKind::External,
            secret_inputs: Vec::new(),
        }))
    }

    fn resolve_network_origin(
        &self,
        _plugin_id: &str,
        _plugin_version: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }
}

struct LocalNetworkPluginMetadata;

impl PluginMetadataPort for LocalNetworkPluginMetadata {
    fn resolve_action(
        &self,
        _plugin_id: &str,
        _plugin_version: &str,
        _action: &str,
    ) -> Result<Option<PluginActionMetadata>> {
        Ok(Some(PluginActionMetadata {
            provider_kind: transist_protocol::ProviderKind::Local,
            secret_inputs: Vec::new(),
        }))
    }

    fn resolve_network_origin(
        &self,
        _plugin_id: &str,
        _plugin_version: &str,
    ) -> Result<Option<String>> {
        Ok(Some("https://plugin.example.test".to_string()))
    }
}

struct TestPromptFiles;

impl PromptFilePort for TestPromptFiles {
    fn read_source_asset(
        &self,
        raw_path: &str,
        recipe_source_dir: Option<&Path>,
        _step_id: &str,
    ) -> Result<Vec<u8>> {
        Ok(std::fs::read(
            recipe_source_dir
                .context("missing source directory")?
                .join(raw_path),
        )?)
    }

    fn resolve_prompt_file(
        &self,
        raw_path: &str,
        recipe_source_dir: Option<&Path>,
        _step_id: &str,
    ) -> Result<PromptFileReference> {
        let path = Path::new(raw_path);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            recipe_source_dir
                .context("missing prompt source directory")?
                .join(path)
        };
        let content = std::fs::read_to_string(&resolved)
            .map_err(|error| anyhow::anyhow!("prompt file does not exist: {error}"))?;
        Ok(PromptFileReference {
            sha256: format!("{:x}", sha2::Sha256::digest(content.as_bytes())),
            path: resolved,
            content,
        })
    }
}

struct DispatchChangingPromptFiles {
    calls: AtomicUsize,
    change_on_call: AtomicUsize,
}

impl DispatchChangingPromptFiles {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            change_on_call: AtomicUsize::new(usize::MAX),
        }
    }
}

impl PromptFilePort for DispatchChangingPromptFiles {
    fn resolve_prompt_file(
        &self,
        raw_path: &str,
        recipe_source_dir: Option<&Path>,
        _step_id: &str,
    ) -> Result<PromptFileReference> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let changed = call >= self.change_on_call.load(Ordering::SeqCst);
        let content = if changed {
            "changed prompt"
        } else {
            "verified prompt"
        };
        let path = Path::new(raw_path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            recipe_source_dir
                .context("missing prompt source directory")?
                .join(path)
        };
        Ok(PromptFileReference {
            path,
            content: content.to_string(),
            sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
        })
    }
}

struct TestInvocationStaging {
    operation_trace: Option<OperationTrace>,
    source_assets: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

const TEST_TRANSIENT_RUNTIME_PATH: &str = "/private/tmp/transist-runtime-temp-test";

impl InvocationStagingPort for TestInvocationStaging {
    fn apply_to_request(&self, _request: &mut ExecutionRequest) {}

    fn materialize_source_assets(&self, assets: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        self.source_assets
            .lock()
            .expect("lock")
            .extend(assets.clone());
        Ok(())
    }

    fn promote(
        &self,
        result: &mut ExecutionResult,
        _redactor: &transist_core::Redactor,
    ) -> Result<()> {
        if let Some(trace) = &self.operation_trace {
            trace.record("result_promoted");
        }
        for file in result.artifacts.iter().chain(result.evidence.iter()) {
            if !Path::new(&file.path).is_file() {
                bail!("test staged output is not a regular file: {}", file.path);
            }
        }
        Ok(())
    }

    fn transient_redaction_values(&self) -> Vec<String> {
        vec![TEST_TRANSIENT_RUNTIME_PATH.to_string()]
    }
}

impl PluginVerificationPort for AllowPluginVerification {
    fn verify_plugin_digest(&self, home_dir: &Path, lock: &transist_core::AssetLock) -> Result<()> {
        let version = lock
            .version
            .as_deref()
            .context("plugin lock has no version")?;
        let plugin_dir = home_dir.join("plugins").join(&lock.ref_id).join(version);
        let actual = plugin_fixture_digest(&plugin_dir);
        if actual != lock.sha256 {
            anyhow::bail!(
                "plugin '{}' version '{}' pinned by step '{}' has changed; expected digest '{}' but found '{}'",
                lock.ref_id,
                version,
                lock.step_id,
                lock.sha256,
                actual
            );
        }
        Ok(())
    }
}

struct DefaultHomeSettings;

impl HomeSettingsPort for DefaultHomeSettings {
    fn load_settings(&self, home_dir: &Path) -> Result<HomeSettings> {
        let path = home_dir.join("settings.yaml");
        if !path.exists() {
            return Ok(HomeSettings::default());
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(yaml_serde::from_str(&raw)?)
    }
}

#[derive(Default)]
struct RecordingExecution {
    invocations: Mutex<Vec<ExecutionInvocation>>,
    responses: Mutex<BTreeMap<(String, String, u32), ExecutionResult>>,
    errors: Mutex<BTreeMap<(String, String, u32), String>>,
}

#[derive(Default)]
struct RecordingPreflight {
    invocations: Mutex<Vec<PreflightInvocation>>,
    responses: Mutex<Vec<Vec<PreflightCheck>>>,
}

#[derive(Default)]
struct NoopTelemetry {
    operation_trace: Option<OperationTrace>,
}

impl TelemetryPort for NoopTelemetry {
    fn record(&self, event: &TelemetryEvent) {
        if matches!(event, TelemetryEvent::StepFinished { .. })
            && let Some(trace) = &self.operation_trace
        {
            trace.record("telemetry:step_finished");
        }
    }
}

impl RecordingPreflight {
    fn plan_response(&self, checks: Vec<PreflightCheck>) {
        self.responses.lock().expect("lock").push(checks);
    }

    fn invocations(&self) -> Vec<PreflightInvocation> {
        self.invocations.lock().expect("lock").clone()
    }
}

impl PreflightPort for RecordingPreflight {
    // RecordingExecution's fixture echo actions accept any object input. Model
    // that static contract explicitly without recording or running a live probe.
    fn validate_deferred_input(
        &self,
        invocation: &PreflightInvocation,
        _deferred_paths: &[String],
    ) -> Result<Vec<PreflightCheck>> {
        assert!(invocation.request.input.is_object());
        Ok(Vec::new())
    }

    fn preflight_steps(&self, invocations: &[PreflightInvocation]) -> Result<Vec<PreflightCheck>> {
        self.invocations
            .lock()
            .expect("lock")
            .extend(invocations.iter().cloned());
        let mut responses = self.responses.lock().expect("lock");
        if responses.is_empty() {
            return Ok(Vec::new());
        }
        Ok(responses.remove(0))
    }
}

impl RecordingExecution {
    fn invocations(&self) -> Vec<ExecutionInvocation> {
        self.invocations.lock().expect("lock").clone()
    }

    fn clear_invocations(&self) {
        self.invocations.lock().expect("lock").clear();
    }

    fn plan_result(&self, run_id: &str, step_id: &str, execution: u32, result: ExecutionResult) {
        self.responses
            .lock()
            .expect("lock")
            .insert((run_id.to_string(), step_id.to_string(), execution), result);
    }

    fn plan_error(&self, run_id: &str, step_id: &str, execution: u32, message: &str) {
        self.errors.lock().expect("lock").insert(
            (run_id.to_string(), step_id.to_string(), execution),
            message.to_string(),
        );
    }
}

impl ExecutionPort for RecordingExecution {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            actions: vec!["process".to_string()],
            supports_cancel: Some(true),
            supports_health: Some(false),
        }
    }

    fn execute_step(&self, invocation: &ExecutionInvocation) -> Result<ExecutionResult> {
        self.invocations
            .lock()
            .expect("lock")
            .push(invocation.clone());
        let key = (
            invocation.request.run_id.clone(),
            invocation.request.step.id.clone(),
            invocation.request.step.execution,
        );
        if let Some(message) = self.errors.lock().expect("lock").get(&key).cloned() {
            return Err(anyhow::anyhow!(message));
        }
        let mut result = self
            .responses
            .lock()
            .expect("lock")
            .get(&key)
            .cloned()
            .unwrap_or_else(|| success_result(invocation));
        materialize_staged_file_refs(
            &invocation.request.paths.artifacts_dir,
            &mut result.artifacts,
        );
        materialize_staged_file_refs(&invocation.request.paths.evidence_dir, &mut result.evidence);
        Ok(result)
    }

    fn cancel_step(&self, request: &ExecutionCancelRequest) -> Result<ExecutionCancelResult> {
        Ok(ExecutionCancelResult {
            invoke_id: request.invoke_id.clone(),
            accepted: true,
        })
    }
}

#[derive(Default)]
struct MemoryArtifacts;

impl ArtifactPort for MemoryArtifacts {
    fn write_artifact(&self, _request: &ArtifactWriteRequest) -> Result<PathBuf> {
        Ok(PathBuf::from("/tmp/unused"))
    }

    fn read_artifact(&self, _request: &ArtifactReadRequest) -> Result<KernelValue> {
        Ok(KernelValue::Null)
    }
}

/// Filesystem-backed artifact port whose writes land inside the run-owned
/// `artifacts/<step>/execution_<NNN>` tree under the test home directory.
///
/// `MemoryArtifacts` returns an out-of-workspace path (`/tmp/unused`) that the
/// cleanup-phase filesystem-policy guard rejects for any successful cleanup
/// step (the guard re-checks evidence AFTER runtime invoke evidence is
/// appended). This port writes payloads to a run-owned path so the guard's
/// `file_ref_is_run_owned` short-circuit applies, letting a cleanup step
/// genuinely succeed through the execution port.
struct RunOwnedArtifacts {
    home_dir: PathBuf,
}

impl ArtifactPort for RunOwnedArtifacts {
    fn write_artifact(&self, request: &ArtifactWriteRequest) -> Result<PathBuf> {
        let run_root = self
            .home_dir
            .join("runs")
            .join(format!("run_{}", request.run_id));
        let execution_dir = run_root
            .join("artifacts")
            .join(&request.step_id)
            .join(format!("execution_{:03}", request.execution));
        let path = execution_dir.join(&request.logical_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create artifact dir {}", parent.display()))?;
        }
        let json: serde_json::Value = request.payload.clone().into();
        let bytes = serde_json::to_vec_pretty(&json).context("serialize artifact payload")?;
        std::fs::write(&path, &bytes)
            .with_context(|| format!("write artifact {}", path.display()))?;
        Ok(path)
    }

    fn read_artifact(&self, _request: &ArtifactReadRequest) -> Result<KernelValue> {
        Ok(KernelValue::Null)
    }
}

#[derive(Default)]
struct MemoryRunStore {
    documents_enabled: bool,
    document_candidates:
        Mutex<BTreeMap<(String, String, u32), Vec<transist_protocol::DocumentRevision>>>,
    runs: Mutex<BTreeMap<String, serde_json::Value>>,
    recipes: Mutex<BTreeMap<String, String>>,
    executions: Mutex<Vec<serde_json::Value>>,
    failures: Mutex<Vec<transist_core::FailureRecord>>,
    events: Mutex<Vec<RuntimeEvent>>,
    fail_updates: Mutex<bool>,
    operation_trace: Option<OperationTrace>,
    supports_asset_snapshots: bool,
    asset_manifests: Mutex<BTreeMap<String, Vec<u8>>>,
    asset_blobs: Mutex<BTreeMap<String, BTreeMap<String, Vec<u8>>>>,
    staged_source_assets: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    tree_inspections: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    physical_tree_memberships: Mutex<BTreeMap<String, String>>,
}

#[derive(Clone, Default)]
struct OperationTrace(Arc<Mutex<Vec<String>>>);

impl OperationTrace {
    fn record(&self, operation: &str) {
        self.0.lock().expect("lock").push(operation.to_string());
    }

    fn snapshot(&self) -> Vec<String> {
        self.0.lock().expect("lock").clone()
    }

    fn clear(&self) {
        self.0.lock().expect("lock").clear();
    }
}

impl MemoryRunStore {
    fn with_operation_trace(mut self, trace: OperationTrace) -> Self {
        self.operation_trace = Some(trace);
        self
    }

    fn trace(&self, operation: &str) {
        if let Some(trace) = &self.operation_trace {
            trace.record(operation);
        }
    }

    fn run_json(&self, run_id: &str) -> serde_json::Value {
        self.runs
            .lock()
            .expect("lock")
            .get(run_id)
            .cloned()
            .expect("run json")
    }

    fn has_run(&self, run_id: &str) -> bool {
        self.runs.lock().expect("lock").contains_key(run_id)
    }

    fn inject_event(&self, event: RuntimeEvent) {
        self.events.lock().expect("lock").push(event);
    }

    fn inject_record(&self, record: PersistedRunRecord) {
        self.runs.lock().expect("lock").insert(
            record.run_id.clone(),
            serde_json::to_value(record).expect("json"),
        );
    }

    fn inject_recipe_snapshot(&self, run_id: &str, recipe: &Recipe) {
        self.recipes.lock().expect("lock").insert(
            run_id.to_string(),
            yaml_serde::to_string(recipe).expect("recipe yaml"),
        );
    }

    fn step_executions(&self) -> Vec<serde_json::Value> {
        self.executions.lock().expect("lock").clone()
    }

    fn failures(&self) -> Vec<transist_core::FailureRecord> {
        self.failures.lock().expect("lock").clone()
    }

    #[cfg(feature = "dev-version-bypass")]
    fn fail_next_update(&self) {
        *self.fail_updates.lock().expect("lock") = true;
    }
}

fn seed_run_locator(home_dir: &std::path::Path, run_id: &str) {
    std::fs::create_dir_all(home_dir.join("runs").join(format!("run_{run_id}")))
        .expect("create run root");
}

fn test_home_dir(tmp: &tempfile::TempDir) -> PathBuf {
    let name = tmp
        .path()
        .file_name()
        .expect("temporary repository name")
        .to_string_lossy();
    let home = tmp
        .path()
        .parent()
        .expect("temporary repository parent")
        .join(format!(".transist-test-{name}"));
    std::fs::create_dir_all(&home).expect("create test home");
    home
}

impl RunStorePort for MemoryRunStore {
    fn document_store(&self) -> Option<&dyn transist_core::ports::DocumentStorePort> {
        self.documents_enabled.then_some(self)
    }
    fn create_run(&self, record: &PersistedRunRecord) -> Result<()> {
        self.runs.lock().expect("lock").insert(
            record.run_id.clone(),
            serde_json::to_value(record).expect("record json"),
        );
        Ok(())
    }

    fn create_run_staged(&self, artifacts: &RunBirthArtifacts) -> Result<()> {
        self.trace("run_staged");
        let record = artifacts.record;
        self.runs.lock().expect("lock").insert(
            record.run_id.clone(),
            serde_json::to_value(record).expect("record json"),
        );
        self.recipes
            .lock()
            .expect("lock")
            .insert(record.run_id.clone(), artifacts.recipe_yaml.to_string());
        if let Some(manifest) = artifacts.workflow_asset_manifest_json {
            self.asset_manifests
                .lock()
                .expect("lock")
                .insert(record.run_id.clone(), manifest.to_vec());
        }
        self.asset_blobs.lock().expect("lock").insert(
            record.run_id.clone(),
            artifacts
                .workflow_asset_blobs
                .iter()
                .map(|blob| (blob.sha256.clone(), blob.bytes.clone()))
                .collect(),
        );
        Ok(())
    }

    fn supports_workflow_asset_snapshots(&self) -> bool {
        self.supports_asset_snapshots
    }

    fn read_workflow_asset_manifest(&self, run_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .asset_manifests
            .lock()
            .expect("lock")
            .get(run_id)
            .cloned())
    }

    fn read_workflow_asset_blob(&self, run_id: &str, sha256: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .asset_blobs
            .lock()
            .expect("lock")
            .get(run_id)
            .and_then(|blobs| blobs.get(sha256))
            .cloned())
    }

    fn run_exists(&self, run_id: &str) -> Result<bool> {
        Ok(self.runs.lock().expect("lock").contains_key(run_id))
    }

    fn load_run(&self, run_id: &str) -> Result<Option<transist_core::PersistedRunRecord>> {
        let Some(value) = self.runs.lock().expect("lock").get(run_id).cloned() else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_value(value)?))
    }

    fn update_run_state(&self, run_id: &str, record: &PersistedRunRecord) -> Result<()> {
        if *self.fail_updates.lock().expect("lock") {
            *self.fail_updates.lock().expect("lock") = false;
            bail!("injected run-state update failure");
        }
        self.runs
            .lock()
            .expect("lock")
            .insert(run_id.to_string(), serde_json::to_value(record)?);
        Ok(())
    }

    fn sync_recipe_snapshot(
        &self,
        run_id: &str,
        record: &PersistedRunRecord,
        recipe_yaml: &str,
    ) -> Result<()> {
        self.update_run_state(run_id, record)?;
        self.write_recipe_snapshot(run_id, recipe_yaml)
    }

    fn store_step_state(&self, step_state: &RunStoreStepState) -> Result<()> {
        let step_json = serde_json::Value::from(step_state.state.clone());
        if step_json["last_request"].is_object() {
            self.trace("request_persisted");
        }
        if matches!(
            step_json["status"].as_str(),
            Some("succeeded" | "failed" | "canceled" | "blocked")
        ) {
            self.trace("terminal_projection");
        }
        let mut guard = self.runs.lock().expect("lock");
        let run = guard
            .entry(step_state.run_id.clone())
            .or_insert_with(|| serde_json::json!({}));
        let run_object = run.as_object_mut().expect("object");
        let steps = run_object
            .entry("steps")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .expect("steps object");
        steps.insert(step_state.step_id.clone(), step_json.clone());
        let executions = run_object
            .entry("executions")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .expect("executions object");
        executions.insert(
            step_state.step_id.clone(),
            serde_json::Value::from(step_json["executions"].as_u64().unwrap_or_default()),
        );
        Ok(())
    }

    fn store_step_execution(&self, execution: &RunStoreStepExecution) -> Result<()> {
        self.trace("immutable_execution_record");
        self.executions
            .lock()
            .expect("lock")
            .push(serde_json::Value::from(execution.state.clone()));
        Ok(())
    }

    fn step_execution_record_exists(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<bool> {
        Ok(self.executions.lock().expect("lock").iter().any(|e| {
            e["run_id"] == run_id && e["step_id"] == step_id && e["execution"] == execution
        }))
    }

    fn load_step_execution(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<Option<transist_core::StepExecutionRecord>> {
        let executions = self.executions.lock().expect("lock");
        let value = executions.iter().find(|e| {
            e["run_id"] == run_id && e["step_id"] == step_id && e["execution"] == execution
        });
        let Some(value) = value else {
            return Ok(None);
        };
        let record =
            serde_json::from_value(value.clone()).context("invalid in-memory execution record")?;
        Ok(Some(record))
    }

    fn load_run_events(&self, run_id: &str) -> Result<Vec<RuntimeEvent>> {
        Ok(self
            .events
            .lock()
            .expect("lock")
            .iter()
            .filter(|event| event.run_id == run_id)
            .cloned()
            .collect())
    }

    fn store_failure_record(&self, failure: &transist_core::FailureRecord) -> Result<()> {
        self.trace("failure_record");
        self.failures.lock().expect("lock").push(failure.clone());
        Ok(())
    }

    fn load_run_failures(&self, run_id: &str) -> Result<Vec<transist_core::FailureRecord>> {
        Ok(self
            .failures
            .lock()
            .expect("lock")
            .iter()
            .filter(|failure| failure.run_id == run_id)
            .cloned()
            .collect())
    }

    fn write_recipe_snapshot(&self, run_id: &str, recipe_yaml: &str) -> Result<()> {
        self.recipes
            .lock()
            .expect("lock")
            .insert(run_id.to_string(), recipe_yaml.to_string());
        Ok(())
    }

    fn load_recipe_snapshot(&self, run_id: &str) -> Result<String> {
        Ok(self
            .recipes
            .lock()
            .expect("lock")
            .get(run_id)
            .cloned()
            .expect("recipe yaml"))
    }

    fn load_external_transfer_evidence(
        &self,
        _home_dir: &Path,
        _run_id: &str,
    ) -> Result<Vec<transist_core::ports::ExternalTransferEvidence>> {
        Ok(Vec::new())
    }

    fn resolve_run_root(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf> {
        Ok(home_dir.join("runs").join(format!("run_{run_id}")))
    }

    fn resolve_run_root_or_default(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf> {
        self.resolve_run_root(home_dir, run_id)
    }

    fn resolve_invocation_paths(
        &self,
        home_dir: &Path,
        workspace_dir: &Path,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<transist_protocol::InvokePaths> {
        let run_root = self.resolve_run_root_or_default(home_dir, run_id)?;
        let execution_dir = run_root
            .join("artifacts")
            .join(step_id)
            .join(format!("execution_{execution:03}"));
        Ok(transist_protocol::InvokePaths {
            run_root: run_root.display().to_string(),
            artifacts_dir: execution_dir.join("artifacts").display().to_string(),
            evidence_dir: execution_dir.join("evidence").display().to_string(),
            workspace_dir: workspace_dir.display().to_string(),
        })
    }

    fn read_result_file(&self, path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path).map_err(Into::into)
    }

    fn result_file_is_within(&self, path: &Path, root: &Path) -> bool {
        let Ok(path) = std::fs::canonicalize(path) else {
            return false;
        };
        let Ok(root) = std::fs::canonicalize(root) else {
            return false;
        };
        path.strip_prefix(root).is_ok()
    }

    fn resolve_root_run_id(&self, _home_dir: &Path, run_id: &str) -> Result<String> {
        Ok(self
            .load_run(run_id)?
            .and_then(|record| record.lineage)
            .map(|lineage| lineage.root_run_id)
            .filter(|root| !root.is_empty())
            .unwrap_or_else(|| run_id.to_string()))
    }

    fn run_provenance_path(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf> {
        Ok(self
            .resolve_run_root_or_default(home_dir, run_id)?
            .join("provenance.json"))
    }

    fn gates_dir(&self, home_dir: &Path, run_id: &str) -> Result<PathBuf> {
        Ok(self
            .resolve_run_root_or_default(home_dir, run_id)?
            .join("gates"))
    }

    fn has_pending_gate_decision(&self, _home_dir: &Path, _run_id: &str) -> bool {
        false
    }

    fn gate_decision_path(&self, home_dir: &Path, run_id: &str, gate_id: &str) -> Result<PathBuf> {
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, gate_id)?;
        Ok(self
            .gates_dir(home_dir, run_id)?
            .join(format!("{gate_id}.json")))
    }

    fn load_gate_decision(
        &self,
        _home_dir: &Path,
        _run_id: &str,
        _gate_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn load_gate_decision_history(
        &self,
        _home_dir: &Path,
        _run_id: &str,
        _gate_id: &str,
    ) -> Result<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }

    fn consume_gate_decision(&self, _home_dir: &Path, _run_id: &str, _gate_id: &str) -> Result<()> {
        Ok(())
    }

    fn store_gate_decision(
        &self,
        _home_dir: &Path,
        _record: &transist_protocol::GateDecisionRecord,
    ) -> Result<()> {
        Ok(())
    }

    fn read_run_provenance(&self, _home_dir: &Path, _run_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn write_run_provenance(&self, _home_dir: &Path, _run_id: &str, _payload: &[u8]) -> Result<()> {
        Ok(())
    }

    fn list_run_roots(&self, home_dir: &Path) -> Result<Vec<PathBuf>> {
        let roots = self
            .runs
            .lock()
            .expect("lock")
            .keys()
            .map(|run_id| home_dir.join("runs").join(format!("run_{run_id}")))
            .collect();
        if let Some(sender) = self.tree_inspections.lock().expect("lock").as_ref() {
            let _ = sender.send(());
        }
        Ok(roots)
    }

    fn list_run_tree_roots(&self, home_dir: &Path, root_run_id: &str) -> Result<Vec<PathBuf>> {
        let runs = self.runs.lock().expect("lock");
        let memberships = self.physical_tree_memberships.lock().expect("lock");
        let roots = runs
            .iter()
            .filter(|(id, record)| {
                id.as_str() == root_run_id
                    || memberships
                        .get(id.as_str())
                        .map(String::as_str)
                        .or_else(|| record["lineage"]["root_run_id"].as_str())
                        == Some(root_run_id)
            })
            .map(|(id, _)| home_dir.join("runs").join(format!("run_{id}")))
            .collect();
        if let Some(sender) = self.tree_inspections.lock().expect("lock").as_ref() {
            let _ = sender.send(());
        }
        Ok(roots)
    }

    fn run_id_from_run_root(&self, run_root: &Path) -> Option<String> {
        run_root
            .file_name()
            .and_then(|value| value.to_str())
            .and_then(|value| value.strip_prefix("run_"))
            .map(str::to_string)
    }

    fn rebase_persisted_run_record(
        &self,
        record: &mut PersistedRunRecord,
        home_dir: &Path,
        repo_root: &Path,
    ) -> Result<()> {
        for (step_id, step) in record.steps.iter_mut() {
            let Some(request) = step.last_request.as_mut() else {
                continue;
            };
            let run_root = self.resolve_run_root_or_default(home_dir, &request.run_id)?;
            let execution_dir = run_root
                .join("artifacts")
                .join(step_id)
                .join(format!("execution_{:03}", request.step.execution));
            request.paths.run_root = run_root.display().to_string();
            request.paths.artifacts_dir = execution_dir.join("artifacts").display().to_string();
            request.paths.evidence_dir = execution_dir.join("evidence").display().to_string();
            request.paths.workspace_dir = repo_root.display().to_string();
        }
        Ok(())
    }

    fn publish_run_locator(&self, _home_dir: &Path, _record: &PersistedRunRecord) -> Result<()> {
        self.trace("locator_published");
        Ok(())
    }

    fn create_invocation_staging(
        &self,
        _home_dir: &Path,
        _request: &ExecutionRequest,
    ) -> Result<Box<dyn InvocationStagingPort>> {
        Ok(Box::new(TestInvocationStaging {
            operation_trace: self.operation_trace.clone(),
            source_assets: self.staged_source_assets.clone(),
        }))
    }
}

#[derive(Default)]
struct NoopLease {
    release_error: Option<String>,
    operation_trace: Option<OperationTrace>,
}

impl NoopLease {
    fn failing_release(message: &str) -> Self {
        Self {
            release_error: Some(message.to_string()),
            operation_trace: None,
        }
    }

    fn with_operation_trace(trace: OperationTrace) -> Self {
        Self {
            release_error: None,
            operation_trace: Some(trace),
        }
    }
}

impl LeasePort for NoopLease {
    fn owner_identity(&self) -> RuntimeOwnerIdentity {
        RuntimeOwnerIdentity {
            management_mode: ManagementMode::Standalone,
            owner_id: "test-owner".to_string(),
            lease_id: "test-lease".to_string(),
            pid: 1,
            hostname: "test-host".to_string(),
            command: "transist test".to_string(),
            transist_version: "test".to_string(),
            external: None,
            runtime_id: None,
            organization_id: None,
        }
    }

    fn acquire_root_run_tree_lease(&self, root_run_id: &str) -> Result<LeaseHandle> {
        if let Some(trace) = &self.operation_trace {
            trace.record(&format!("lease_acquired:{root_run_id}"));
        }
        Ok(LeaseHandle {
            root_run_id: root_run_id.to_string(),
            lease_id: "test-lease".to_string(),
            owner: self.owner_identity(),
        })
    }

    fn refresh_root_run_tree_lease(
        &self,
        handle: &LeaseHandle,
    ) -> Result<transist_core::RunTreeLeaseRecord> {
        Ok(transist_core::RunTreeLeaseRecord {
            schema_version: transist_core::RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
            root_run_id: handle.root_run_id.clone(),
            lease_id: handle.lease_id.clone(),
            owner_id: handle.owner.owner_id.clone(),
            management_mode: handle.owner.management_mode,
            pid: handle.owner.pid,
            hostname: handle.owner.hostname.clone(),
            command: handle.owner.command.clone(),
            transist_version: handle.owner.transist_version.clone(),
            acquired_at_ms: 1,
            last_heartbeat_ms: 1,
            external: None,
        })
    }

    fn inspect_root_run_tree_lease(
        &self,
        _root_run_id: &str,
    ) -> Result<Option<transist_core::RunTreeLeaseRecord>> {
        Ok(None)
    }

    fn break_root_run_tree_lease(
        &self,
        _root_run_id: &str,
        _options: LeaseBreakOptions,
    ) -> Result<Option<transist_core::RunTreeLeaseRecord>> {
        Ok(None)
    }

    fn release_root_run_tree_lease(&self, handle: &LeaseHandle) -> Result<()> {
        if let Some(trace) = &self.operation_trace {
            trace.record(&format!("lease_released:{}", handle.root_run_id));
        }
        match &self.release_error {
            Some(message) => bail!("{message}"),
            None => Ok(()),
        }
    }
}

struct ChildBirthConflictLease {
    plugin_acquisitions: AtomicUsize,
    child_conflicted: Arc<AtomicBool>,
}

impl LeasePort for ChildBirthConflictLease {
    fn owner_identity(&self) -> RuntimeOwnerIdentity {
        NoopLease::default().owner_identity()
    }

    fn acquire_root_run_tree_lease(&self, root_run_id: &str) -> Result<LeaseHandle> {
        if root_run_id == "plugin-lifecycle-reservation"
            && self.plugin_acquisitions.fetch_add(1, Ordering::SeqCst) == 1
        {
            self.child_conflicted.store(true, Ordering::SeqCst);
            return Err(transist_core::LeaseConflict {
                root_run_id: root_run_id.to_string(),
                live_lease: transist_core::RunTreeLeaseRecord {
                    schema_version: transist_core::RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
                    root_run_id: root_run_id.to_string(),
                    lease_id: "live-lease".to_string(),
                    owner_id: "live-owner".to_string(),
                    management_mode: ManagementMode::Standalone,
                    pid: 42,
                    hostname: "live-host".to_string(),
                    command: "helper process".to_string(),
                    transist_version: "test".to_string(),
                    acquired_at_ms: 1,
                    last_heartbeat_ms: 1,
                    external: None,
                },
            }
            .into());
        }
        Ok(LeaseHandle {
            root_run_id: root_run_id.to_string(),
            lease_id: "test-lease".to_string(),
            owner: self.owner_identity(),
        })
    }

    fn refresh_root_run_tree_lease(
        &self,
        handle: &LeaseHandle,
    ) -> Result<transist_core::RunTreeLeaseRecord> {
        Ok(transist_core::RunTreeLeaseRecord {
            schema_version: transist_core::RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
            root_run_id: handle.root_run_id.clone(),
            lease_id: handle.lease_id.clone(),
            owner_id: handle.owner.owner_id.clone(),
            management_mode: handle.owner.management_mode,
            pid: handle.owner.pid,
            hostname: handle.owner.hostname.clone(),
            command: handle.owner.command.clone(),
            transist_version: handle.owner.transist_version.clone(),
            acquired_at_ms: 1,
            last_heartbeat_ms: 1,
            external: None,
        })
    }

    fn inspect_root_run_tree_lease(
        &self,
        _root_run_id: &str,
    ) -> Result<Option<transist_core::RunTreeLeaseRecord>> {
        Ok(None)
    }

    fn break_root_run_tree_lease(
        &self,
        _root_run_id: &str,
        _options: LeaseBreakOptions,
    ) -> Result<Option<transist_core::RunTreeLeaseRecord>> {
        panic!("run birth must not break a plugin lifecycle reservation")
    }

    fn release_root_run_tree_lease(&self, _handle: &LeaseHandle) -> Result<()> {
        Ok(())
    }
}

struct CancelAfterChildConflict {
    child_conflicted: Arc<AtomicBool>,
}

impl ControlRequestPort for CancelAfterChildConflict {
    fn request_cancel(&self, root_run_id: &str) -> Result<ControlRequestRecord> {
        Ok(ControlRequestRecord {
            schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
            request_id: "request-1".to_string(),
            request_type: ControlRequestType::Cancel,
            root_run_id: root_run_id.to_string(),
            requested_at_ms: 1,
            requester: None,
        })
    }

    fn inspect_cancel_request(&self, root_run_id: &str) -> Result<Option<ControlRequestRecord>> {
        if !self.child_conflicted.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(ControlRequestRecord {
            schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
            request_id: "request-1".to_string(),
            request_type: ControlRequestType::Cancel,
            root_run_id: root_run_id.to_string(),
            requested_at_ms: 1,
            requester: None,
        }))
    }

    fn clear_cancel_request(&self, _root_run_id: &str) -> Result<()> {
        Ok(())
    }
}

#[test]
fn source_and_replay_root_leases_are_independent_while_held_concurrently() {
    let leases = NoopLease::default();
    let source = leases
        .acquire_root_run_tree_lease("source-run")
        .expect("source lease");
    let replay = leases
        .acquire_root_run_tree_lease("replay-run")
        .expect("replay lease");

    assert_ne!(source.root_run_id, replay.root_run_id);
    assert_eq!(source.root_run_id, "source-run");
    assert_eq!(replay.root_run_id, "replay-run");

    leases
        .release_root_run_tree_lease(&source)
        .expect("release source");
    leases
        .release_root_run_tree_lease(&replay)
        .expect("release replay");
}

struct FixedTrustDecision {
    decision: TrustDecisionRecord,
}

impl TrustDecisionPort for FixedTrustDecision {
    fn resolve_run_decision(
        &self,
        _repo_root: &Path,
        _home_dir: &Path,
        _record: &PersistedRunRecord,
        _recipe: &Recipe,
    ) -> Result<TrustDecisionRecord> {
        Ok(self.decision.clone())
    }
}

#[derive(Default)]
struct NoopControlRequests;

impl ControlRequestPort for NoopControlRequests {
    fn request_cancel(&self, root_run_id: &str) -> Result<ControlRequestRecord> {
        Ok(ControlRequestRecord {
            schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
            request_id: "request-1".to_string(),
            request_type: ControlRequestType::Cancel,
            root_run_id: root_run_id.to_string(),
            requested_at_ms: 1,
            requester: None,
        })
    }

    fn inspect_cancel_request(&self, _root_run_id: &str) -> Result<Option<ControlRequestRecord>> {
        Ok(None)
    }

    fn clear_cancel_request(&self, _root_run_id: &str) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct PendingCancelControlRequests {
    pending: Mutex<bool>,
}

impl PendingCancelControlRequests {
    fn is_pending(&self) -> bool {
        *self.pending.lock().expect("lock")
    }
}

impl ControlRequestPort for PendingCancelControlRequests {
    fn request_cancel(&self, root_run_id: &str) -> Result<ControlRequestRecord> {
        *self.pending.lock().expect("lock") = true;
        Ok(ControlRequestRecord {
            schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
            request_id: "request-1".to_string(),
            request_type: ControlRequestType::Cancel,
            root_run_id: root_run_id.to_string(),
            requested_at_ms: 1,
            requester: None,
        })
    }

    fn inspect_cancel_request(&self, root_run_id: &str) -> Result<Option<ControlRequestRecord>> {
        if !*self.pending.lock().expect("lock") {
            return Ok(None);
        }
        Ok(Some(ControlRequestRecord {
            schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
            request_id: "request-1".to_string(),
            request_type: ControlRequestType::Cancel,
            root_run_id: root_run_id.to_string(),
            requested_at_ms: 1,
            requester: None,
        }))
    }

    fn clear_cancel_request(&self, _root_run_id: &str) -> Result<()> {
        *self.pending.lock().expect("lock") = false;
        Ok(())
    }
}

#[derive(Default)]
struct FixedClock;

impl ClockPort for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        1
    }
}

#[derive(Default)]
struct FixedIds;

impl IdGeneratorPort for FixedIds {
    fn generate_run_id(&self) -> String {
        "generated-run".to_string()
    }

    fn generate_step_id(&self) -> String {
        "generated-step".to_string()
    }

    fn generate_artifact_id(&self) -> String {
        "generated-artifact".to_string()
    }
}

#[derive(Default)]
struct RecordingEvents {
    events: Mutex<Vec<RuntimeEvent>>,
    operation_trace: Option<OperationTrace>,
    durable_mirror: Option<Arc<MemoryRunStore>>,
}

impl RecordingEvents {
    fn with_operation_trace(mut self, trace: OperationTrace) -> Self {
        self.operation_trace = Some(trace);
        self
    }

    fn with_durable_mirror(mut self, run_store: Arc<MemoryRunStore>) -> Self {
        self.durable_mirror = Some(run_store);
        self
    }

    fn snapshot(&self) -> Vec<RuntimeEvent> {
        self.events.lock().expect("lock").clone()
    }
}

impl EventPort for RecordingEvents {
    fn publish(&self, event: &RuntimeEvent) -> Result<()> {
        if let Some(trace) = &self.operation_trace {
            trace.record(&format!("event:{}", event.kind));
        }
        self.events.lock().expect("lock").push(event.clone());
        if let Some(run_store) = &self.durable_mirror {
            run_store.inject_event(event.clone());
        }
        Ok(())
    }
}

#[derive(Default)]
struct AllowPolicy;

impl PolicyPort for AllowPolicy {
    fn validate_run_start(
        &self,
        _repo_root: &std::path::Path,
        _workflow_ref: &str,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }

    fn validate_step_execution(
        &self,
        _run_id: &str,
        _step_id: &str,
        _descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }
}

struct StaticSecrets;

impl SecretProviderPort for StaticSecrets {
    fn get_secret(&self, name: &str) -> Result<Option<String>> {
        Ok(match name {
            "API_TOKEN" => Some("topsecret".to_string()),
            _ => None,
        })
    }

    fn has_secret(&self, name: &str) -> Result<bool> {
        Ok(name == "API_TOKEN")
    }
}

/// Policy that denies every `validate_run_start` call and allows all step executions.
struct DenyRunStartPolicy;

impl PolicyPort for DenyRunStartPolicy {
    fn validate_run_start(
        &self,
        _repo_root: &std::path::Path,
        _workflow_ref: &str,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: false,
            reason_code: Some("TEST_RUN_START_DENIED".to_string()),
            message: Some("run start denied by test policy".to_string()),
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }

    fn validate_step_execution(
        &self,
        _run_id: &str,
        _step_id: &str,
        _descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }
}

/// Policy that allows `validate_step_execution` on the **first** call for each
/// step (the preflight check) and denies on the **second** call (runtime
/// dispatch).  This lets a run reach the dispatch loop so that the kernel can
/// mark the step as `failed` and emit a `policy.denied` event, instead of
/// terminating at preflight with the step still `runnable`.
struct DenyStepAtDispatchPolicy {
    preflighted: Mutex<std::collections::HashSet<String>>,
    calls: Mutex<Vec<PolicyCall>>,
}

#[derive(Clone)]
struct PolicyCall {
    phase: &'static str,
    step_id: String,
    descriptor: KernelValue,
}

impl Default for DenyStepAtDispatchPolicy {
    fn default() -> Self {
        Self {
            preflighted: Mutex::new(std::collections::HashSet::new()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl DenyStepAtDispatchPolicy {
    fn calls(&self) -> Vec<PolicyCall> {
        self.calls.lock().expect("lock").clone()
    }
}

impl PolicyPort for DenyStepAtDispatchPolicy {
    fn validate_run_start(
        &self,
        _repo_root: &std::path::Path,
        _workflow_ref: &str,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }

    fn validate_step_execution(
        &self,
        _run_id: &str,
        step_id: &str,
        descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        let mut seen = self.preflighted.lock().expect("lock");
        let preflight = seen.insert(step_id.to_string());
        self.calls.lock().expect("lock").push(PolicyCall {
            phase: if preflight { "preflight" } else { "dispatch" },
            step_id: step_id.to_string(),
            descriptor: descriptor.clone(),
        });
        if preflight {
            // First call for this step = preflight check → allow so the run
            // can reach the dispatch loop.
            Ok(PolicyDecision {
                allowed: true,
                reason_code: None,
                message: None,
                payload: KernelValue::Null,
                remote_secret_policy: RemoteSecretPolicy::default(),
            })
        } else {
            // Second call for this step = runtime dispatch → deny.
            Ok(PolicyDecision {
                allowed: false,
                reason_code: Some("TEST_STEP_DENIED".to_string()),
                message: Some("step denied by test policy at dispatch".to_string()),
                payload: KernelValue::Null,
                remote_secret_policy: RemoteSecretPolicy::default(),
            })
        }
    }
}

/// Policy that records every descriptor passed to `validate_step_execution`
/// and allows all calls through.
#[derive(Default)]
struct RecordingPolicy {
    descriptors: Mutex<Vec<KernelValue>>,
    run_start_refs: Mutex<Vec<String>>,
}

impl RecordingPolicy {
    fn captured_descriptors(&self) -> Vec<KernelValue> {
        self.descriptors.lock().expect("lock").clone()
    }

    fn captured_run_start_refs(&self) -> Vec<String> {
        self.run_start_refs.lock().expect("lock").clone()
    }
}

impl PolicyPort for RecordingPolicy {
    fn validate_run_start(
        &self,
        _repo_root: &std::path::Path,
        workflow_ref: &str,
    ) -> Result<PolicyDecision> {
        self.run_start_refs
            .lock()
            .expect("lock")
            .push(workflow_ref.to_string());
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }

    fn validate_step_execution(
        &self,
        _run_id: &str,
        _step_id: &str,
        descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        self.descriptors
            .lock()
            .expect("lock")
            .push(descriptor.clone());
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }
}

struct FixedRemoteSecretPolicy {
    remote_secret_policy: RemoteSecretPolicy,
}

impl PolicyPort for FixedRemoteSecretPolicy {
    fn validate_run_start(
        &self,
        _repo_root: &std::path::Path,
        _workflow_ref: &str,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: self.remote_secret_policy,
        })
    }

    fn validate_step_execution(
        &self,
        _run_id: &str,
        _step_id: &str,
        _descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: self.remote_secret_policy,
        })
    }
}

#[derive(Default)]
struct DenyProcessSecretDeliveryAtDispatchPolicy {
    preflighted: Mutex<std::collections::HashSet<String>>,
}

impl PolicyPort for DenyProcessSecretDeliveryAtDispatchPolicy {
    fn validate_run_start(
        &self,
        _repo_root: &std::path::Path,
        _workflow_ref: &str,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy::default(),
        })
    }

    fn validate_step_execution(
        &self,
        _run_id: &str,
        step_id: &str,
        _descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        let mut seen = self.preflighted.lock().expect("lock");
        let process_allowed = seen.insert(step_id.to_string());
        Ok(PolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            payload: KernelValue::Null,
            remote_secret_policy: RemoteSecretPolicy {
                process: process_allowed,
                ..RemoteSecretPolicy::default()
            },
        })
    }
}

fn test_kernel(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
) -> RuntimeKernel {
    test_kernel_with_lease(execution, run_store, events, Arc::new(NoopLease::default()))
}

fn test_kernel_with_phase_trace(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    trace: OperationTrace,
) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts: Arc::new(MemoryArtifacts),
        run_store,
        lease: Arc::new(NoopLease::default()),
        control: Arc::new(NoopControlRequests),
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events,
        policy: Arc::new(AllowPolicy),
        preflight: Arc::new(RecordingPreflight::default()),
        secrets: Arc::new(StaticSecrets),
        plugin_verifier: Arc::new(AllowPluginVerification),
        plugin_metadata: Arc::new(TestPluginMetadata),
        prompt_files: Arc::new(TestPromptFiles),
        home_settings: Arc::new(DefaultHomeSettings),
        telemetry: Arc::new(NoopTelemetry {
            operation_trace: Some(trace),
        }),
    }))
}

fn test_kernel_with_prompt_files(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    prompt_files: Arc<dyn PromptFilePort>,
) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts: Arc::new(MemoryArtifacts),
        run_store,
        lease: Arc::new(NoopLease::default()),
        control: Arc::new(NoopControlRequests),
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events,
        policy: Arc::new(AllowPolicy),
        preflight: Arc::new(RecordingPreflight::default()),
        secrets: Arc::new(StaticSecrets),
        plugin_verifier: Arc::new(AllowPluginVerification),
        plugin_metadata: Arc::new(TestPluginMetadata),
        prompt_files,
        home_settings: Arc::new(DefaultHomeSettings),
        telemetry: Arc::new(NoopTelemetry::default()),
    }))
}

/// Like `test_kernel` but accepts a custom `ArtifactPort` implementation.
fn test_kernel_with_artifacts(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    artifacts: Arc<dyn ArtifactPort>,
) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts,
        run_store,
        lease: Arc::new(NoopLease::default()),
        control: Arc::new(NoopControlRequests),
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events,
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

fn test_kernel_with_lease(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    lease: Arc<dyn LeasePort>,
) -> RuntimeKernel {
    test_kernel_with_preflight_and_lease(
        execution,
        Arc::new(RecordingPreflight::default()),
        run_store,
        events,
        lease,
    )
}

fn test_kernel_with_lease_and_control(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    lease: Arc<dyn LeasePort>,
    control: Arc<dyn ControlRequestPort>,
) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts: Arc::new(MemoryArtifacts),
        run_store,
        lease,
        control,
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events,
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

fn test_kernel_with_preflight(
    execution: Arc<RecordingExecution>,
    preflight: Arc<RecordingPreflight>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
) -> RuntimeKernel {
    test_kernel_with_preflight_and_lease(
        execution,
        preflight,
        run_store,
        events,
        Arc::new(NoopLease::default()),
    )
}

fn test_kernel_with_preflight_and_lease(
    execution: Arc<RecordingExecution>,
    preflight: Arc<RecordingPreflight>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    lease: Arc<dyn LeasePort>,
) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts: Arc::new(MemoryArtifacts),
        run_store,
        lease,
        control: Arc::new(NoopControlRequests),
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events,
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

/// Like `test_kernel` but accepts an arbitrary `PolicyPort` implementation.
fn test_kernel_with_policy(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    policy: Arc<dyn PolicyPort>,
) -> RuntimeKernel {
    test_kernel_with_policy_and_control(
        execution,
        run_store,
        events,
        policy,
        Arc::new(NoopControlRequests),
    )
}

fn test_kernel_with_policy_and_control(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    policy: Arc<dyn PolicyPort>,
    control: Arc<dyn ControlRequestPort>,
) -> RuntimeKernel {
    test_kernel_with_policy_control_preflight(
        execution,
        run_store,
        events,
        policy,
        control,
        Arc::new(RecordingPreflight::default()),
    )
}

fn test_kernel_with_policy_control_preflight(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    policy: Arc<dyn PolicyPort>,
    control: Arc<dyn ControlRequestPort>,
    preflight: Arc<RecordingPreflight>,
) -> RuntimeKernel {
    test_kernel_with_policy_control_preflight_and_plugin_metadata(
        execution,
        run_store,
        events,
        policy,
        control,
        preflight,
        Arc::new(TestPluginMetadata),
    )
}

fn test_kernel_with_policy_control_preflight_and_plugin_metadata(
    execution: Arc<RecordingExecution>,
    run_store: Arc<MemoryRunStore>,
    events: Arc<RecordingEvents>,
    policy: Arc<dyn PolicyPort>,
    control: Arc<dyn ControlRequestPort>,
    preflight: Arc<RecordingPreflight>,
    plugin_metadata: Arc<dyn PluginMetadataPort>,
) -> RuntimeKernel {
    RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution,
        artifacts: Arc::new(MemoryArtifacts),
        run_store,
        lease: Arc::new(NoopLease::default()),
        control,
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events,
        policy,
        preflight,
        secrets: Arc::new(StaticSecrets),
        plugin_verifier: Arc::new(AllowPluginVerification),
        plugin_metadata,
        prompt_files: Arc::new(TestPromptFiles),
        home_settings: Arc::new(DefaultHomeSettings),
        telemetry: Arc::new(NoopTelemetry::default()),
    }))
}

fn invoke_external_step(
    id: &str,
    adapter: AdapterKind,
    plugin: &str,
    action: &str,
    with: BTreeMap<String, KernelValue>,
) -> RecipeStep {
    RecipeStep {
        id: id.to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter,
            plugin_id: Some(plugin.to_string()),
            plugin: plugin.to_string(),
            action: action.to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            plugin_sha256: None,
        },
        with,
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Default::default(),
        network_policy: Default::default(),
        effective_filesystem_policy: Default::default(),
        effective_filesystem_policy_source: None,
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    }
}

fn invoke_step(id: &str) -> RecipeStep {
    invoke_external_step(id, AdapterKind::Process, "shell", "echo", BTreeMap::new())
}

fn invoke_step_with(id: &str, with: BTreeMap<String, KernelValue>) -> RecipeStep {
    invoke_external_step(id, AdapterKind::Process, "shell", "echo", with)
}

fn control_step(id: &str, primitive: &str, with: BTreeMap<String, KernelValue>) -> RecipeStep {
    RecipeStep {
        id: id.to_string(),
        execution: StepExecutionDescriptor::Control {
            primitive: primitive.to_string(),
        },
        with,
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Default::default(),
        network_policy: Default::default(),
        effective_filesystem_policy: Default::default(),
        effective_filesystem_policy_source: None,
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    }
}

fn subflow_step(id: &str, recipe: Recipe) -> RecipeStep {
    RecipeStep {
        id: id.to_string(),
        execution: StepExecutionDescriptor::Subflow {
            recipe: Box::new(recipe),
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Default::default(),
        network_policy: Default::default(),
        effective_filesystem_policy: Default::default(),
        effective_filesystem_policy_source: None,
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    }
}

fn subflow_step_with_with(
    id: &str,
    recipe: Recipe,
    with: BTreeMap<String, KernelValue>,
) -> RecipeStep {
    RecipeStep {
        id: id.to_string(),
        execution: StepExecutionDescriptor::Subflow {
            recipe: Box::new(recipe),
        },
        with,
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Default::default(),
        network_policy: Default::default(),
        effective_filesystem_policy: Default::default(),
        effective_filesystem_policy_source: None,
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    }
}

fn recipe_with_inputs(
    workflow_id: &str,
    inputs: BTreeMap<String, RecipeInputSpec>,
    steps: Vec<RecipeStep>,
    graph: Option<RecipeGraph>,
) -> Recipe {
    let mut recipe = recipe(workflow_id, steps, graph);
    recipe.inputs = inputs;
    recipe
}

fn looping_child_recipe(workflow_id: &str) -> Recipe {
    let loop_meta = BTreeMap::from([(
        "__transist_loop".to_string(),
        serde_json::json!({
            "body_steps": ["nested"],
            "max_iters": 2
        })
        .into(),
    )]);
    recipe(
        workflow_id,
        vec![
            control_step("loop", "control.noop", loop_meta),
            invoke_step("nested"),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "loop".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "nested".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions < 2".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "finish".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions >= 2".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "nested".to_string(),
                    to: "loop".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: Some(RecipeLoop {
                        while_expr: None,
                        until_expr: None,
                        max_iters: None,
                    }),
                },
            ],
            max_iterations: None,
        }),
    )
}

fn recipe(workflow_id: &str, steps: Vec<RecipeStep>, graph: Option<RecipeGraph>) -> Recipe {
    Recipe {
        schema: "transist.recipe.v1".to_string(),
        workflow: WorkflowRef {
            id: workflow_id.to_string(),
            version: "v1".to_string(),
        },
        source_dir: None,
        repositories: Vec::new(),
        prerequisites: Default::default(),
        filesystem_policy: None,
        network_policy: None,
        inputs: BTreeMap::new(),
        workspace: None,
        steps,
        cleanup: Vec::new(),
        graph,
        capabilities: Default::default(),
        capabilities_persisted: false,
        documents: Default::default(),
    }
}

fn recipe_snapshot_sha(recipe: &Recipe) -> String {
    let yaml = yaml_serde::to_string(recipe).expect("recipe yaml");
    let mut hasher = Sha256::new();
    hasher.update(yaml.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn allowed_trust_decision(
    run_id: &str,
    workflow_id: &str,
    workflow_version: &str,
) -> TrustDecisionRecord {
    TrustDecisionRecord {
        schema: TRUST_DECISION_SCHEMA_V1.to_string(),
        run_id: run_id.to_string(),
        trusted_execution_mode: true,
        decision: "allowed".to_string(),
        workflow: TrustSubjectDecision {
            id: workflow_id.to_string(),
            version: workflow_version.to_string(),
            digest: "workflow-digest".to_string(),
            signature_verified: true,
            signer_key_id: Some("test-key".to_string()),
            reason_code: None,
            trust_decision: "verified".to_string(),
        },
        plugins: Vec::new(),
        reason_code: None,
        failed_verification_type: None,
        organization_id: None,
        policy_revision: None,
        decided_at_ms: 1,
    }
}

#[test]
fn successful_execute_and_start_ignore_lease_release_failure_after_durable_commit() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_lease(
        execution,
        run_store.clone(),
        events,
        Arc::new(NoopLease::failing_release("release failed")),
    );
    let recipe = recipe("lease_warning_wf", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let executed = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("lease-execute-run"))
        .expect("successful execute must not fail on lease release");
    assert_eq!(executed.status, KernelRunStatus::Success);
    assert_eq!(
        executed
            .lease_release_warning
            .as_ref()
            .expect("release warning")
            .message,
        "run execution lease release failed: release failed"
    );
    assert_eq!(run_store.run_json("lease-execute-run")["status"], "success");

    let started = kernel
        .start_recipe_with_outcome(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("lease-start-run"),
            BTreeMap::new(),
        )
        .expect("successful start must not fail on lease release");
    assert_eq!(started.value, "lease-start-run");
    assert_eq!(
        started
            .lease_release_warning
            .expect("release warning")
            .message,
        "run creation lease release failed: release failed"
    );
    assert_eq!(run_store.run_json("lease-start-run")["status"], "created");
}

#[test]
fn gate_decision_use_case_propagates_lease_release_warning() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_lease(
        execution,
        run_store,
        events,
        Arc::new(NoopLease::failing_release("release failed")),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = transist_protocol::GateDecisionRecord {
        review_request: None,
        schema: "transist.gate_decision.v1".to_string(),
        run_id: "gate-warning-run".to_string(),
        gate_id: "review".to_string(),
        step_id: "gate".to_string(),
        decision: "approved".to_string(),
        actor: Some("alice".to_string()),
        actor_source: None,
        reason: Some("approved".to_string()),
        decided_at_ms: Some(1),
    };

    let outcome = kernel
        .record_gate_decision(tmp.path(), "gate-warning-run", &record)
        .expect("gate decision remains successful after release failure");

    assert_eq!(
        outcome
            .lease_release_warning
            .expect("release warning")
            .message,
        "gate decision persistence lease release failed: release failed"
    );
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn recipe_sync_preserves_durable_state_when_lease_release_fails() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_lease(
        execution,
        run_store.clone(),
        events,
        Arc::new(NoopLease::failing_release("release failed")),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let mut original = recipe("sync_test_wf", vec![invoke_step("invoke")], None);
    original.workflow.version = "v0".to_string();
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &original,
            Some("recipe-sync-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let mut synced = original.clone();
    synced
        .steps
        .push(control_step("after_sync", "control.noop", BTreeMap::new()));
    synced.steps[0].with.insert(
        "message".to_string(),
        KernelValue::String("synced".to_string()),
    );
    let recipe_yaml = yaml_serde::to_string(&synced).expect("recipe yaml");
    let source_registry_path = tmp.path().join("registry/sync-test.yaml");
    let outcome = kernel
        .sync_recipe_snapshot(&home_dir, &run_id, &source_registry_path, &recipe_yaml)
        .expect("recipe sync");

    assert_eq!(outcome.value.run_id, run_id);
    assert_eq!(
        outcome
            .lease_release_warning
            .expect("release warning")
            .message,
        "recipe snapshot synchronization lease release failed: release failed"
    );
    let persisted_recipe: Recipe = yaml_serde::from_str(
        &run_store
            .load_recipe_snapshot(&run_id)
            .expect("recipe snapshot"),
    )
    .expect("persisted recipe yaml");
    assert_eq!(
        persisted_recipe.steps[0].with["message"],
        KernelValue::String("synced".to_string())
    );
    assert_eq!(
        run_store.run_json(&run_id)["recipe_snapshot"]["sha256"],
        recipe_snapshot_sha(&synced)
    );
    let persisted = run_store.run_json(&run_id);
    assert!(persisted["steps"]["after_sync"].is_object());
    assert!(persisted["preflight"].is_null());
    assert!(persisted["trust_decision"].is_null());
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn recipe_sync_leaves_recipe_bytes_unchanged_when_state_update_fails() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let mut original = recipe("sync_rollback_wf", vec![invoke_step("invoke")], None);
    original.workflow.version = "v0".to_string();
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &original,
            Some("recipe-sync-rollback-run"),
            BTreeMap::new(),
        )
        .expect("start");
    let original_yaml = run_store
        .load_recipe_snapshot(&run_id)
        .expect("original recipe snapshot");
    let original_digest = run_store.run_json(&run_id)["recipe_snapshot"]["sha256"].clone();

    let mut replacement = original.clone();
    replacement.steps[0].with.insert(
        "message".to_string(),
        KernelValue::String("replacement".to_string()),
    );
    let replacement_yaml = yaml_serde::to_string(&replacement).expect("replacement yaml");
    run_store.fail_next_update();

    let error = kernel
        .sync_recipe_snapshot(
            &home_dir,
            &run_id,
            &tmp.path().join("registry/sync-rollback.yaml"),
            &replacement_yaml,
        )
        .expect_err("state update failure must fail sync");
    assert!(
        error
            .to_string()
            .contains("injected run-state update failure")
    );
    assert_eq!(
        run_store
            .load_recipe_snapshot(&run_id)
            .expect("unchanged recipe snapshot"),
        original_yaml
    );
    assert_eq!(
        run_store.run_json(&run_id)["recipe_snapshot"]["sha256"],
        original_digest
    );
}

#[test]
fn trust_refresh_preserves_durable_state_when_lease_release_fails() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_lease(
        execution,
        run_store.clone(),
        events,
        Arc::new(NoopLease::failing_release("release failed")),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let recipe = recipe("trust_refresh_wf", vec![invoke_step("invoke")], None);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("trust-refresh-run"),
            BTreeMap::new(),
        )
        .expect("start");
    let original_snapshot_sha = run_store.run_json(&run_id)["recipe_snapshot"]["sha256"].clone();
    let decision = allowed_trust_decision(&run_id, "trust_refresh_wf", "v1");

    let outcome = kernel
        .refresh_trusted_execution_for_run(
            tmp.path(),
            &home_dir,
            &run_id,
            true,
            &FixedTrustDecision {
                decision: decision.clone(),
            },
        )
        .expect("trust refresh");

    assert_eq!(
        outcome
            .lease_release_warning
            .expect("release warning")
            .message,
        "trusted-execution refresh lease release failed: release failed"
    );
    let persisted = run_store.run_json(&run_id);
    assert_eq!(persisted["trusted_execution_mode"], true);
    assert_eq!(
        persisted["trust_decision"],
        serde_json::to_value(decision).expect("decision json")
    );
    assert_eq!(
        persisted["recipe_snapshot"]["sha256"],
        original_snapshot_sha
    );
}

#[test]
fn shared_recipe_input_binding_helper_applies_defaults() {
    let recipe = recipe_with_inputs(
        "child-defaults",
        BTreeMap::from([
            (
                "required_ticket".to_string(),
                RecipeInputSpec {
                    value_type: "string".to_string(),
                    required: true,
                    default: None,
                    description: None,
                },
            ),
            (
                "target_branch".to_string(),
                RecipeInputSpec {
                    value_type: "string".to_string(),
                    required: false,
                    default: Some(KernelValue::String("main".to_string())),
                    description: None,
                },
            ),
        ]),
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        Some(RecipeGraph {
            entry: "finish".to_string(),
            edges: Vec::new(),
            max_iterations: None,
        }),
    );

    let bound = bind_recipe_inputs(
        &recipe,
        BTreeMap::from([(
            "required_ticket".to_string(),
            KernelValue::String("ISSUE-289".to_string()),
        )]),
    )
    .expect("bind inputs");

    assert_eq!(
        bound,
        BTreeMap::from([
            (
                "required_ticket".to_string(),
                KernelValue::String("ISSUE-289".to_string()),
            ),
            (
                "target_branch".to_string(),
                KernelValue::String("main".to_string()),
            ),
        ])
    );
}

#[test]
fn subflow_input_binding_failure_is_caught_by_root_preflight_before_child_run_creation() {
    // A statically-resolvable, undeclared child input is caught by recursive
    // root preflight before dispatch: the run fails at preflight with a
    // parent-scoped `subflow_input_binding` blocker, the parent step never
    // executes, and no child run is created. The execution-time binding path
    // (exercised when preflight cannot evaluate the child inputs yet) is
    // covered separately below.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe_with_inputs(
        "child-inputs",
        BTreeMap::from([(
            "required_ticket".to_string(),
            RecipeInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: None,
            },
        )]),
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        Some(RecipeGraph {
            entry: "finish".to_string(),
            edges: Vec::new(),
            max_iterations: None,
        }),
    );
    let parent_recipe = recipe(
        "parent-inputs",
        vec![
            subflow_step_with_with(
                "child",
                child_recipe,
                BTreeMap::from([(
                    "unexpected".to_string(),
                    KernelValue::String("boom".to_string()),
                )]),
            ),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "child".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "child".to_string(),
                to: "finish".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let error = kernel
        .execute_recipe(tmp.path(), &home_dir, &parent_recipe, Some("parent-run"))
        .expect_err("invalid child binding must reject run birth");
    assert!(
        error
            .to_string()
            .contains("workflow input 'unexpected' is not declared by 'child-inputs@v1'")
    );
    assert!(!run_store.has_run("parent-run"));

    let child_run_exists = run_store
        .runs
        .lock()
        .expect("lock")
        .contains_key("parent-run__child");
    assert!(
        !child_run_exists,
        "child run must not be created on binding failure"
    );

    let binding_error = bind_recipe_inputs(
        &recipe_with_inputs(
            "child-inputs",
            BTreeMap::from([(
                "required_ticket".to_string(),
                RecipeInputSpec {
                    value_type: "string".to_string(),
                    required: true,
                    default: None,
                    description: None,
                },
            )]),
            Vec::new(),
            None,
        ),
        BTreeMap::from([(
            "unexpected".to_string(),
            KernelValue::String("boom".to_string()),
        )]),
    )
    .expect_err("bind should fail");
    assert_eq!(
        binding_error.kind,
        RecipeInputBindingErrorKind::UndeclaredInput {
            input_name: "unexpected".to_string(),
        }
    );
}

#[test]
fn subflow_reservation_wait_cancellation_creates_no_child_or_start_failure() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let child_conflicted = Arc::new(AtomicBool::new(false));
    let kernel = test_kernel_with_lease_and_control(
        execution,
        run_store.clone(),
        events,
        Arc::new(ChildBirthConflictLease {
            plugin_acquisitions: AtomicUsize::new(0),
            child_conflicted: child_conflicted.clone(),
        }),
        Arc::new(CancelAfterChildConflict { child_conflicted }),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let child_recipe = recipe(
        "child",
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        None,
    );
    let parent_recipe = recipe("parent", vec![subflow_step("child", child_recipe)], None);

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &parent_recipe, Some("parent-run"))
        .expect("parent cancellation is a terminal run outcome");
    let parent = run_store.run_json("parent-run");

    assert_eq!(summary.status, KernelRunStatus::Canceled);
    assert!(!run_store.has_run("parent-run__child"));
    assert_eq!(
        parent["steps"]["child"]["last_result"]["status"],
        "cancelled"
    );
    assert!(
        parent["steps"]["child"]["last_result"]["output"]
            .get("failure")
            .is_none()
    );
}

#[test]
fn plugin_lifecycle_reservation_spans_staged_birth_and_locator_publication() {
    let execution = Arc::new(RecordingExecution::default());
    let trace = OperationTrace::default();
    let run_store = Arc::new(MemoryRunStore::default().with_operation_trace(trace.clone()));
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_lease(
        execution,
        run_store,
        events,
        Arc::new(NoopLease::with_operation_trace(trace.clone())),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let recipe = recipe(
        "reservation-order",
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        None,
    );

    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("reservation-order-run"),
            BTreeMap::new(),
        )
        .expect("start recipe");

    assert_eq!(
        trace.snapshot(),
        vec![
            "lease_acquired:reservation-order-run",
            "lease_acquired:plugin-lifecycle-reservation",
            "run_staged",
            "locator_published",
            "lease_released:plugin-lifecycle-reservation",
            "lease_released:reservation-order-run",
        ]
    );
}

#[test]
fn child_authority_preparation_failure_prevents_parent_and_child_birth() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut child_step = invoke_step("invoke");
    child_step.with.insert(
        "secret_refs".to_string(),
        KernelValue::Array(vec![KernelValue::String(
            "{{inputs.secret_name}}".to_string(),
        )]),
    );
    let child_recipe = recipe_with_inputs(
        "child-authority",
        BTreeMap::from([(
            "secret_name".to_string(),
            RecipeInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: None,
            },
        )]),
        vec![child_step],
        None,
    );
    let parent_recipe = recipe(
        "parent-authority",
        vec![subflow_step_with_with(
            "child",
            child_recipe,
            BTreeMap::from([(
                "secret_name".to_string(),
                KernelValue::String("{{vars.secret_name}}".to_string()),
            )]),
        )],
        None,
    );

    let error = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("parent-authority-run"),
        )
        .expect_err("child authority preparation must fail before birth");
    assert!(
        error
            .to_string()
            .contains("child authority preparation failed")
    );
    assert!(run_store.runs.lock().expect("runs lock").is_empty());
}

fn write_home_filesystem_policy(home_dir: &std::path::Path, mode: &str) {
    std::fs::create_dir_all(home_dir).expect("create home dir");
    std::fs::write(
        home_dir.join("settings.yaml"),
        format!("filesystem_policy:\n  workspace_access:\n    mode: {mode}\n"),
    )
    .expect("write settings");
}

fn install_shell_plugin_fixture(home_dir: &std::path::Path) -> (PathBuf, String) {
    let plugin_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("plugins")
        .join("shell")
        .join("plugin.yaml");
    let plugin_source = plugin_manifest.parent().expect("plugin source");
    let plugin_version = "1.0.0".to_string();
    let plugin_dir = home_dir.join("plugins").join("shell").join(&plugin_version);
    copy_plugin_fixture(plugin_source, &plugin_dir);
    (plugin_dir, plugin_version)
}

fn copy_plugin_fixture(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).expect("create plugin target");
    for entry in std::fs::read_dir(source).expect("read plugin fixture") {
        let entry = entry.expect("plugin entry");
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path).expect("plugin metadata");
        if metadata.is_dir() {
            copy_plugin_fixture(&source_path, &target_path);
        } else if metadata.is_file() {
            std::fs::copy(source_path, target_path).expect("copy plugin file");
        }
    }
}

fn plugin_fixture_digest(plugin_dir: &Path) -> String {
    let manifest: serde_json::Value = yaml_serde::from_str(
        &std::fs::read_to_string(plugin_dir.join("plugin.yaml")).expect("plugin manifest"),
    )
    .expect("parse plugin manifest");
    let mut relative_paths = vec!["plugin.yaml".to_string()];
    relative_paths.extend(
        manifest["package"]["files"]
            .as_array()
            .expect("package files")
            .iter()
            .map(|path| path.as_str().expect("package file path").to_string()),
    );
    let mut files = Vec::new();
    for relative in relative_paths {
        let path = plugin_dir.join(&relative);
        let metadata = std::fs::symlink_metadata(&path).expect("plugin metadata");
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(unix)]
        let mode = metadata.mode();
        #[cfg(not(unix))]
        let mode = 0;
        files.push((relative, mode, std::fs::read(path).expect("plugin bytes")));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    transist_trust_digest::digest_file_entries(&files)
}

fn workspace_policy(
    mode: transist_protocol::WorkspaceAccessMode,
) -> transist_protocol::FilesystemPolicy {
    transist_protocol::FilesystemPolicy {
        workspace_access: Some(transist_protocol::WorkspaceAccessPolicy { mode }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    }
}

fn success_result(invocation: &ExecutionInvocation) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: invocation.request.invoke_id.clone(),
        run_id: invocation.request.run_id.clone(),
        step_id: invocation.request.step.id.clone(),
        execution: invocation.request.step.execution,
        status: "success".to_string(),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timing: transist_protocol::InvokeTiming {
            started_at: "1".to_string(),
            finished_at: "1".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({"ok": true}),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    }
}

fn materialize_staged_file_refs(base_dir: &str, refs: &mut [FileRef]) {
    for file_ref in refs {
        let Some(relative) = file_ref.path.strip_prefix("staged://") else {
            continue;
        };
        let path = std::path::Path::new(base_dir).join(relative);
        std::fs::create_dir_all(path.parent().expect("staged file parent")).expect("staged dir");
        std::fs::write(&path, b"staged output").expect("staged file");
        file_ref.path = path.display().to_string();
        file_ref.sha256 = "untrusted".to_string();
        file_ref.bytes = 13;
    }
}

#[test]
fn prepare_persists_global_filesystem_policy_after_settings_change() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);

    let recipe = recipe("global_policy_wf", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    write_home_filesystem_policy(&home_dir, "read_only");

    let (run_id, preflight) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("prepared-global-policy-run"),
            BTreeMap::new(),
        )
        .expect("prepare");
    assert_eq!(preflight.status, PreflightStatus::Passed);

    let persisted_recipe_yaml = run_store
        .load_recipe_snapshot(&run_id)
        .expect("load persisted recipe");
    let persisted_recipe: Recipe =
        yaml_serde::from_str(&persisted_recipe_yaml).expect("recipe yaml");
    assert_eq!(
        persisted_recipe.steps[0]
            .effective_filesystem_policy_source
            .expect("policy source"),
        transist_protocol::FilesystemPolicySource::Global
    );
    assert_eq!(
        persisted_recipe.steps[0]
            .effective_filesystem_policy
            .workspace_access
            .as_ref()
            .unwrap()
            .mode,
        transist_protocol::WorkspaceAccessMode::ReadOnly
    );

    write_home_filesystem_policy(&home_dir, "full");

    let _summary = kernel
        .run_prepared_run(tmp.path(), &home_dir, &run_id)
        .expect("run prepared");
    assert!(!execution.invocations().is_empty());

    let invocation = execution.invocations().pop().expect("one invocation");
    assert_eq!(
        invocation.request.policy["filesystem"]["source"],
        serde_json::json!("global")
    );
    assert_eq!(
        invocation.request.policy["filesystem"]["workspace_access"]["mode"],
        serde_json::json!("read_only")
    );

    let execution_record = run_store
        .step_executions()
        .pop()
        .expect("step execution record");
    assert_eq!(
        execution_record["request"]["policy"]["filesystem"]["source"],
        serde_json::json!("global")
    );
    assert_eq!(
        execution_record["request"]["policy"]["filesystem"]["workspace_access"]["mode"],
        serde_json::json!("read_only")
    );
}

#[test]
fn run_prepared_run_rejects_prompt_file_drift_even_with_persisted_preflight() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let prompt_path = tmp.path().join("prompt.txt");
    std::fs::write(&prompt_path, "hello prompt").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: prompt_path.display().to_string(),
        },
    });
    let mut recipe = recipe("prompt_lock_wf", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());

    let (run_id, preflight) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("prepared-prompt-lock-run"),
            BTreeMap::new(),
        )
        .expect("prepare");
    assert_eq!(preflight.status, PreflightStatus::Passed);

    let record_json = run_store.run_json(&run_id);
    assert!(
        record_json["asset_locks"]
            .as_array()
            .is_some_and(|locks| !locks.is_empty()),
        "expected prompt-file asset lock in persisted record"
    );

    std::fs::write(&prompt_path, "changed prompt").expect("mutate prompt");

    let err = kernel
        .run_prepared_run(tmp.path(), &home_dir, &run_id)
        .expect_err("prepared run should reject prompt drift");
    assert!(
        err.to_string().contains("prompt_file") && err.to_string().contains("has changed"),
        "expected prompt drift error, got: {err}"
    );
}

#[test]
fn normal_dispatch_rejects_changed_prompt_bytes_after_reentry_verification() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let prompt_files = Arc::new(DispatchChangingPromptFiles::new());
    let kernel =
        test_kernel_with_prompt_files(execution.clone(), run_store, events, prompt_files.clone());
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    std::fs::write(tmp.path().join("prompt.md"), "placeholder").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompt.md".to_string(),
        },
    });
    let mut recipe = recipe("dispatch_prompt_lock", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());
    let (run_id, _) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("dispatch-prompt-lock-run"),
            BTreeMap::new(),
        )
        .expect("prepare");

    // Re-entry verifies once before the request builder reads the prompt again.
    prompt_files.change_on_call.store(
        prompt_files.calls.load(Ordering::SeqCst) + 2,
        Ordering::SeqCst,
    );
    let summary = kernel
        .run_prepared_run(tmp.path(), &home_dir, &run_id)
        .expect("run records the dispatch failure");
    assert_eq!(summary.status, KernelRunStatus::Failed);
    assert!(execution.invocations().is_empty());
}

#[test]
fn replay_dispatch_rejects_changed_prompt_bytes() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let prompt_files = Arc::new(DispatchChangingPromptFiles::new());
    let kernel = test_kernel_with_prompt_files(
        execution.clone(),
        run_store,
        Arc::new(RecordingEvents::default()),
        prompt_files.clone(),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    std::fs::write(tmp.path().join("prompt.md"), "placeholder").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompt.md".to_string(),
        },
    });
    let mut recipe = recipe("replay_dispatch_prompt_lock", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());
    kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("prompt-lock-source-run"),
        )
        .expect("execute source run");
    execution.clear_invocations();

    // Replay verifies the copied lock twice before its dispatch request reads the prompt.
    prompt_files.change_on_call.store(
        prompt_files.calls.load(Ordering::SeqCst) + 3,
        Ordering::SeqCst,
    );
    let replay = kernel
        .replay_run(
            tmp.path(),
            &home_dir,
            "prompt-lock-source-run",
            Some("prompt-lock-replay-run"),
        )
        .expect("replay records the dispatch failure");
    assert_eq!(replay.status, KernelRunStatus::Failed);
    assert!(execution.invocations().is_empty());
}

#[test]
fn replayed_nested_subflows_preserve_source_child_prompt_locks() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let caller = tempfile::tempdir().expect("caller");
    let workspace = tempfile::tempdir().expect("workspace");
    let home_holder = tempfile::tempdir().expect("home holder");
    let home_dir = test_home_dir(&home_holder);
    std::fs::create_dir_all(&home_dir).expect("home");
    let prompt_path = workspace.path().join("nested-prompt.txt");
    std::fs::write(&prompt_path, "nested prompt").expect("write prompt");
    let mut leaf_step = invoke_step("leaf");
    leaf_step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: prompt_path.display().to_string(),
        },
    });
    let mut leaf = recipe("nested_replay_leaf", vec![leaf_step], None);
    leaf.source_dir = Some(workspace.path().display().to_string());
    let middle = recipe(
        "nested_replay_middle",
        vec![subflow_step("inner", leaf)],
        None,
    );
    let mut parent = recipe(
        "nested_replay_parent",
        vec![subflow_step("outer", middle)],
        None,
    );
    parent.workspace = Some(transist_protocol::WorkspaceBinding {
        path: workspace.path().display().to_string(),
        access: transist_protocol::WorkspaceAccessMode::Full,
    });

    kernel
        .execute_recipe(
            caller.path(),
            &home_dir,
            &parent,
            Some("nested-replay-source"),
        )
        .expect("source");
    execution.clear_invocations();
    let unrelated_cwd = tempfile::tempdir().expect("unrelated cwd");
    kernel
        .replay_run(
            unrelated_cwd.path(),
            &home_dir,
            "nested-replay-source",
            Some("nested-replay-target"),
        )
        .expect("replay");

    let source = run_store.run_json("nested-replay-source__outer__inner");
    let replay = run_store.run_json("nested-replay-target__outer__inner");
    assert_eq!(replay["asset_locks"], source["asset_locks"]);
    assert_eq!(
        replay["lineage"]["source_run_id"],
        "nested-replay-source__outer__inner"
    );
    let expected_workspace = std::fs::canonicalize(workspace.path())
        .unwrap()
        .display()
        .to_string();
    assert!(!execution.invocations().is_empty());
    assert!(
        execution
            .invocations()
            .iter()
            .all(|invocation| { invocation.request.paths.workspace_dir == expected_workspace })
    );
}

fn asset_snapshot_recipe(source_dir: &Path) -> Recipe {
    std::fs::write(source_dir.join("prompt.md"), "captured prompt").expect("write prompt");
    std::fs::write(source_dir.join("helper.sh"), "captured helper").expect("write helper");
    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompt.md".to_string(),
        },
    });
    step.with.insert(
        "script_file".to_string(),
        KernelValue::String("source://helper.sh".to_string()),
    );
    let mut cleanup_step = step.clone();
    cleanup_step.id = "cleanup".to_string();
    let mut recipe = recipe("asset_snapshot", vec![step], None);
    recipe.source_dir = Some(source_dir.display().to_string());
    recipe.cleanup.push(transist_protocol::RecipeCleanupStep {
        step: cleanup_step,
        when: vec![transist_protocol::CleanupTrigger::Success],
    });
    recipe
}

#[test]
fn asset_snapshot_current_v1_marker_preserves_captured_dispatch_preflight_cleanup_and_replay() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore {
        supports_asset_snapshots: true,
        ..Default::default()
    });
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let recipe = asset_snapshot_recipe(tmp.path());
    let (run_id, _) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("snapshot-current"),
            BTreeMap::new(),
        )
        .expect("prepare");
    let json = run_store.run_json(&run_id);
    assert_eq!(json["asset_lock_version"], 1);
    assert_eq!(json["workflow_asset_snapshot"], true);
    let record: PersistedRunRecord = serde_json::from_value(json).expect("record");
    assert!(record.uses_workflow_asset_snapshot());
    std::fs::remove_file(tmp.path().join("prompt.md")).expect("remove prompt");
    std::fs::remove_file(tmp.path().join("helper.sh")).expect("remove helper");
    assert_eq!(
        kernel
            .preflight_run(tmp.path(), &home_dir, &run_id)
            .expect("captured preflight")
            .status,
        PreflightStatus::Passed
    );
    assert_eq!(
        kernel
            .run_prepared_run(tmp.path(), &home_dir, &run_id)
            .expect("captured run")
            .status,
        KernelRunStatus::Success
    );
    assert_eq!(
        kernel
            .replay_run(
                tmp.path(),
                &home_dir,
                &run_id,
                Some("snapshot-current-replay")
            )
            .expect("captured replay")
            .status,
        KernelRunStatus::Success
    );
    let replay = run_store.run_json("snapshot-current-replay");
    assert_eq!(replay["asset_lock_version"], 1);
    assert_eq!(replay["workflow_asset_snapshot"], true);
    assert_eq!(
        replay["asset_locks"],
        run_store.run_json(&run_id)["asset_locks"]
    );
    let invocations = execution.invocations();
    assert_eq!(
        invocations.len(),
        4,
        "main and cleanup in source and replay"
    );
    for invocation in invocations {
        assert_eq!(invocation.request.input["prompt"], "captured prompt");
        assert_eq!(invocation.request.metadata["asset_lock_version"], 1);
        assert_eq!(invocation.request.metadata["workflow_asset_snapshot"], true);
        assert!(
            invocation
                .request
                .metadata
                .get("recipe_source_dir")
                .is_none()
        );
    }
    assert_eq!(
        run_store.staged_source_assets.lock().expect("lock")["helper.sh"],
        b"captured helper"
    );
}

#[test]
fn asset_snapshot_historical_v1_without_marker_keeps_authored_source_semantics() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore {
        supports_asset_snapshots: true,
        ..Default::default()
    });
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let recipe = asset_snapshot_recipe(tmp.path());
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("snapshot-historical-v1"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut json = run_store.run_json(&run_id);
    json.as_object_mut()
        .expect("object")
        .remove("workflow_asset_snapshot");
    let record: PersistedRunRecord = serde_json::from_value(json).expect("old record");
    assert!(!record.workflow_asset_snapshot);
    assert!(!record.uses_workflow_asset_snapshot());
    assert!(
        serde_json::to_value(&record)
            .expect("json")
            .get("workflow_asset_snapshot")
            .is_none()
    );
    run_store.inject_record(record);
    // Snapshot files still exist, but they must not select semantics.
    std::fs::write(tmp.path().join("helper.sh"), "live helper").expect("change helper");
    assert_eq!(
        kernel
            .run_started_run(tmp.path(), &home_dir, &run_id)
            .expect("legacy source dispatch")
            .status,
        KernelRunStatus::Success
    );
    for invocation in execution.invocations() {
        assert!(
            invocation
                .request
                .metadata
                .get("workflow_asset_snapshot")
                .is_none()
        );
        assert_eq!(
            invocation.request.metadata["recipe_source_dir"],
            tmp.path().display().to_string()
        );
        assert_eq!(invocation.request.input["prompt"], "captured prompt");
    }
    assert!(
        run_store
            .staged_source_assets
            .lock()
            .expect("lock")
            .is_empty()
    );
    std::fs::write(tmp.path().join("prompt.md"), "changed prompt").expect("change prompt");
    let error = kernel
        .replay_run(
            tmp.path(),
            &home_dir,
            &run_id,
            Some("snapshot-historical-v1-replay"),
        )
        .expect_err("old v1 still verifies authored prompts");
    assert!(error.to_string().contains("has changed"), "{error}");
}

#[test]
fn unsupported_contract_versions_reject_all_reentry_without_mutation() {
    for field in ["asset_lock_version", "capability_authority_version"] {
        for version in [2, 3, u32::MAX] {
            for marker in [false, true] {
                for test_mode in [false, true] {
                    assert_unsupported_contract_reentry(field, version, marker, test_mode);
                }
            }
        }
    }
}

fn assert_unsupported_contract_reentry(field: &str, version: u32, marker: bool, test_mode: bool) {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore {
        supports_asset_snapshots: true,
        ..Default::default()
    });
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let recipe = asset_snapshot_recipe(tmp.path());
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("unsupported-contract"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut json = run_store.run_json(&run_id);
    json[field] = serde_json::json!(version);
    json["workflow_asset_snapshot"] = serde_json::json!(marker);
    json["test_mode"] = serde_json::json!(test_mode);
    let record: PersistedRunRecord = serde_json::from_value(json).expect("record");
    run_store.inject_record(record);
    let before = run_store.runs.lock().expect("lock").clone();
    let recipes = run_store.recipes.lock().expect("lock").clone();
    let manifests = run_store.asset_manifests.lock().expect("lock").clone();
    let blobs = run_store.asset_blobs.lock().expect("lock").clone();
    let events = run_store.events.lock().expect("lock").len();
    for error in [
        kernel
            .prepare_run(tmp.path(), &home_dir, &run_id)
            .expect_err("prepare"),
        kernel
            .preflight_run(tmp.path(), &home_dir, &run_id)
            .expect_err("preflight"),
        kernel
            .run_started_run(tmp.path(), &home_dir, &run_id)
            .expect_err("run"),
        kernel
            .resume_run(tmp.path(), &home_dir, &run_id)
            .expect_err("resume"),
        kernel
            .cleanup_run(tmp.path(), &home_dir, &run_id)
            .expect_err("cleanup"),
        kernel
            .replay_run(tmp.path(), &home_dir, &run_id, Some("unsupported-replay"))
            .expect_err("replay"),
    ] {
        let contract = if field == "asset_lock_version" {
            "asset lock"
        } else {
            "capability authority"
        };
        assert!(
            error
                .to_string()
                .contains(&format!("unsupported {contract} version {version}")),
            "{error:#}"
        );
        assert_eq!(*run_store.runs.lock().expect("lock"), before);
        assert_eq!(*run_store.recipes.lock().expect("lock"), recipes);
        assert_eq!(*run_store.asset_manifests.lock().expect("lock"), manifests);
        assert_eq!(*run_store.asset_blobs.lock().expect("lock"), blobs);
        assert_eq!(run_store.events.lock().expect("lock").len(), events);
        assert!(run_store.executions.lock().expect("lock").is_empty());
        assert!(run_store.failures.lock().expect("lock").is_empty());
        assert!(execution.invocations().is_empty());
    }
}

#[test]
fn asset_snapshot_missing_or_corrupt_captures_reject_reentry_preflight_and_replay() {
    {
        let version = 1;
        for damage in ["manifest", "blob", "digest"] {
            let execution = Arc::new(RecordingExecution::default());
            let run_store = Arc::new(MemoryRunStore {
                supports_asset_snapshots: true,
                ..Default::default()
            });
            let kernel = test_kernel(
                execution.clone(),
                run_store.clone(),
                Arc::new(RecordingEvents::default()),
            );
            let tmp = tempfile::tempdir().expect("tempdir");
            let home_dir = test_home_dir(&tmp);
            let recipe = asset_snapshot_recipe(tmp.path());
            let run_id = kernel
                .start_recipe(
                    tmp.path(),
                    &home_dir,
                    &recipe,
                    Some("snapshot-damaged"),
                    BTreeMap::new(),
                )
                .expect("start");
            match damage {
                "manifest" => {
                    run_store
                        .asset_manifests
                        .lock()
                        .expect("lock")
                        .remove(&run_id);
                }
                "blob" => {
                    run_store.asset_blobs.lock().expect("lock").remove(&run_id);
                }
                "digest" => {
                    for bytes in run_store
                        .asset_blobs
                        .lock()
                        .expect("lock")
                        .get_mut(&run_id)
                        .expect("blobs")
                        .values_mut()
                    {
                        *bytes = b"corrupt bytes".to_vec();
                    }
                }
                _ => unreachable!(),
            }
            // Authored files remain usable: falling back would incorrectly succeed.
            for error in [
                kernel
                    .run_started_run(tmp.path(), &home_dir, &run_id)
                    .expect_err("damaged snapshot reentry"),
                kernel
                    .preflight_run(tmp.path(), &home_dir, &run_id)
                    .expect_err("damaged snapshot preflight"),
                kernel
                    .replay_run(
                        tmp.path(),
                        &home_dir,
                        &run_id,
                        Some("snapshot-damaged-replay"),
                    )
                    .expect_err("damaged snapshot replay"),
            ] {
                let message = error.to_string();
                assert!(
                    message.contains("missing") || message.contains("digest"),
                    "v{version} {damage}: {error}"
                );
            }
            assert!(execution.invocations().is_empty());
            assert!(!run_store.has_run("snapshot-damaged-replay"));
        }
    }
}

#[test]
fn asset_snapshot_trusted_parent_inherits_and_replays_child_captures_without_authored_assets() {
    {
        let parent_version = 1;
        let execution = Arc::new(RecordingExecution::default());
        let run_store = Arc::new(MemoryRunStore {
            supports_asset_snapshots: true,
            ..Default::default()
        });
        let kernel = test_kernel(
            execution.clone(),
            run_store.clone(),
            Arc::new(RecordingEvents::default()),
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = test_home_dir(&tmp);
        let mut parent = asset_snapshot_recipe(tmp.path());
        parent.workflow.id = "trusted_asset_snapshot_parent".to_string();
        let mut child = parent.clone();
        child.workflow.id = "trusted_asset_snapshot_child".to_string();
        child.steps[0].prompt_binding = None;
        child.cleanup.clear();
        parent.steps.push(subflow_step("child", child));
        let run_id = kernel
            .start_recipe_with_trust(
                tmp.path(),
                &home_dir,
                &parent,
                Some("trusted-snapshot-parent"),
                BTreeMap::new(),
                true,
                Some(allowed_trust_decision(
                    "trusted-snapshot-parent",
                    &parent.workflow.id,
                    &parent.workflow.version,
                )),
                false,
            )
            .expect("start trusted parent");
        assert!(!run_store.has_run("trusted-snapshot-parent__child"));

        // Remove the helper before the first child birth: only trusted-parent
        // inheritance can supply it, not a live-source capture by the child.
        std::fs::remove_file(tmp.path().join("helper.sh")).expect("remove helper");
        assert_eq!(
            kernel
                .run_started_run(tmp.path(), &home_dir, &run_id)
                .expect("trusted child inherits parent captures")
                .status,
            KernelRunStatus::Success,
            "parent asset-lock version {parent_version}"
        );
        let child_id = "trusted-snapshot-parent__child";
        let child_manifest = run_store
            .read_workflow_asset_manifest(child_id)
            .expect("read child manifest")
            .expect("child manifest");
        let child_blobs = run_store.asset_blobs.lock().expect("lock")[child_id].clone();
        assert_eq!(
            child_blobs.values().collect::<Vec<_>>(),
            vec![&b"captured helper".to_vec()]
        );

        // Remove the remaining prompt before replay, not after it. Replay must
        // copy both parent and child captures while every authored asset is gone.
        std::fs::remove_file(tmp.path().join("prompt.md")).expect("remove prompt");
        execution.clear_invocations();
        run_store.staged_source_assets.lock().expect("lock").clear();
        assert_eq!(
            kernel
                .replay_run(
                    tmp.path(),
                    &home_dir,
                    &run_id,
                    Some("trusted-snapshot-parent-replay"),
                )
                .expect("trusted replay without authored assets")
                .status,
            KernelRunStatus::Success,
            "parent asset-lock version {parent_version}"
        );
        let replay_child_id = "trusted-snapshot-parent-replay__child";
        assert_eq!(
            run_store
                .read_workflow_asset_manifest(replay_child_id)
                .expect("read replay child manifest"),
            Some(child_manifest)
        );
        assert_eq!(
            run_store.asset_blobs.lock().expect("lock")[replay_child_id],
            child_blobs
        );
        for current_id in [child_id, "trusted-snapshot-parent-replay", replay_child_id] {
            let record = run_store.run_json(current_id);
            assert_eq!(record["trusted_execution_mode"], true, "{current_id}");
            assert_eq!(
                record["trust_decision"]["decision"], "allowed",
                "{current_id}"
            );
            assert_eq!(record["asset_lock_version"], 1, "{current_id}");
            assert_eq!(record["workflow_asset_snapshot"], true, "{current_id}");
        }
        let replay_child = run_store.run_json(replay_child_id);
        assert_eq!(replay_child["lineage"]["source_run_id"], child_id);
        let invocations = execution.invocations();
        assert!(invocations.iter().any(|invocation| {
            invocation.request.run_id == replay_child_id
                && invocation.request.metadata["asset_lock_version"] == 1
                && invocation.request.metadata["workflow_asset_snapshot"] == true
                && invocation
                    .request
                    .metadata
                    .get("recipe_source_dir")
                    .is_none()
        }));
        assert!(invocations.iter().any(|invocation| {
            invocation.request.run_id == "trusted-snapshot-parent-replay"
                && invocation.request.input["prompt"] == "captured prompt"
        }));
        assert_eq!(
            run_store.staged_source_assets.lock().expect("lock")["helper.sh"],
            b"captured helper"
        );
    }
}

#[test]
fn asset_snapshot_current_v1_marker_propagates_to_subflow_replay() {
    let run_store = Arc::new(MemoryRunStore {
        supports_asset_snapshots: true,
        ..Default::default()
    });
    let kernel = test_kernel(
        Arc::new(RecordingExecution::default()),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let child = asset_snapshot_recipe(tmp.path());
    let mut parent = child.clone();
    parent.workflow.id = "asset_snapshot_parent".to_string();
    parent.steps.push(subflow_step("child", child));
    assert_eq!(
        kernel
            .execute_recipe(tmp.path(), &home_dir, &parent, Some("snapshot-parent"))
            .expect("execute parent")
            .status,
        KernelRunStatus::Success
    );
    assert_eq!(
        kernel
            .replay_run(
                tmp.path(),
                &home_dir,
                "snapshot-parent",
                Some("snapshot-parent-replay"),
            )
            .expect("replay parent")
            .status,
        KernelRunStatus::Success
    );
    for run_id in [
        "snapshot-parent",
        "snapshot-parent__child",
        "snapshot-parent-replay",
        "snapshot-parent-replay__child",
    ] {
        let record = run_store.run_json(run_id);
        assert_eq!(record["asset_lock_version"], 1, "{run_id}");
        assert_eq!(record["workflow_asset_snapshot"], true, "{run_id}");
    }
    let source = run_store.run_json("snapshot-parent__child");
    let replay = run_store.run_json("snapshot-parent-replay__child");
    assert_eq!(replay["asset_locks"], source["asset_locks"]);
    assert_eq!(replay["lineage"]["source_run_id"], "snapshot-parent__child");
    std::fs::remove_file(tmp.path().join("prompt.md")).expect("remove prompt");
    std::fs::remove_file(tmp.path().join("helper.sh")).expect("remove helper");
    assert_eq!(
        kernel
            .preflight_run(tmp.path(), &home_dir, "snapshot-parent-replay__child")
            .expect("persisted child preflight")
            .status,
        PreflightStatus::Passed
    );
}

#[test]
fn asset_snapshot_marker_cannot_override_legacy_zero_or_unknown_three_rejection() {
    for version in [0, 3] {
        let run_store = Arc::new(MemoryRunStore::default());
        let kernel = test_kernel(
            Arc::new(RecordingExecution::default()),
            run_store.clone(),
            Arc::new(RecordingEvents::default()),
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = test_home_dir(&tmp);
        let recipe = recipe(
            "snapshot_invalid_version",
            vec![invoke_step("invoke")],
            None,
        );
        let run_id = kernel
            .start_recipe(
                tmp.path(),
                &home_dir,
                &recipe,
                Some("snapshot-invalid-version"),
                BTreeMap::new(),
            )
            .expect("start");
        let mut record = run_store.load_run(&run_id).expect("load").expect("record");
        record.asset_lock_version = version;
        record.workflow_asset_snapshot = true;
        run_store.inject_record(record);
        let expected = if version == 0 {
            "before run asset locking support"
        } else {
            "unsupported asset lock version 3"
        };
        for error in [
            kernel
                .run_started_run(tmp.path(), &home_dir, &run_id)
                .expect_err("invalid version reentry"),
            kernel
                .replay_run(tmp.path(), &home_dir, &run_id, Some("invalid-replay"))
                .expect_err("invalid version replay"),
        ] {
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}

#[test]
fn reentry_and_replay_reject_unknown_asset_lock_versions() {
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        Arc::new(RecordingExecution::default()),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let recipe = recipe(
        "unknown_asset_lock_version",
        vec![invoke_step("invoke")],
        None,
    );
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("unknown-asset-lock-version"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut record = run_store.load_run(&run_id).expect("load").expect("record");
    record.asset_lock_version = 3;
    run_store
        .update_run_state(&run_id, &record)
        .expect("persist");

    let reentry = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect_err("unknown asset lock version must reject reentry");
    assert!(
        reentry
            .to_string()
            .contains("unsupported asset lock version 3")
    );
    let replay = kernel
        .replay_run(
            tmp.path(),
            &home_dir,
            &run_id,
            Some("unknown-asset-lock-version-r2"),
        )
        .expect_err("unknown asset lock version must reject replay");
    assert!(
        replay
            .to_string()
            .contains("unsupported asset lock version 3")
    );
}

#[test]
fn replay_preserves_source_asset_locks_when_prompt_changes_before_rebuild() {
    // Regression for Finding 1: `replay_run` verifies the source run's asset
    // locks, releases the source lease, then calls `start_recipe_internal`,
    // which rebuilds asset locks from the *current* prompt-file bytes. A
    // prompt mutated in that window would become the replay's accepted
    // baseline. The replay must instead preserve the source run's persisted
    // locks so the changed bytes are rejected at dispatch time.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let prompt_files = Arc::new(DispatchChangingPromptFiles::new());
    let kernel = test_kernel_with_prompt_files(
        execution.clone(),
        run_store,
        Arc::new(RecordingEvents::default()),
        prompt_files.clone(),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    std::fs::write(tmp.path().join("prompt.md"), "placeholder").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompt.md".to_string(),
        },
    });
    let mut recipe = recipe("replay_preserve_source_locks", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());
    kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("preserve-locks-source-run"),
        )
        .expect("execute source run");
    execution.clear_invocations();

    // The source run made two `resolve_prompt_file` calls: one in
    // `build_asset_locks` at run birth, one in dispatch. The replay's next
    // call verifies the source's persisted locks (unchanged bytes must
    // match). Setting `change_on_call` to current_calls + 2 makes the bytes
    // flip *after* source verification (call N+1) but *during* the replay's
    // lock rebuild (call N+2). Without the fix the rebuilt locks would
    // capture the changed bytes and dispatch would silently accept them.
    prompt_files.change_on_call.store(
        prompt_files.calls.load(Ordering::SeqCst) + 2,
        Ordering::SeqCst,
    );
    let replay = kernel
        .replay_run(
            tmp.path(),
            &home_dir,
            "preserve-locks-source-run",
            Some("preserve-locks-replay-run"),
        )
        .expect("replay records the dispatch failure");
    assert_eq!(
        replay.status,
        KernelRunStatus::Failed,
        "replay must reject prompt bytes changed after source verification"
    );
    assert!(
        execution.invocations().is_empty(),
        "changed prompt bytes must not reach the adapter"
    );
}

#[test]
fn cleanup_dispatch_rejects_changed_prompt_bytes() {
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let prompt_files = Arc::new(DispatchChangingPromptFiles::new());
    let kernel = test_kernel_with_prompt_files(
        execution.clone(),
        run_store,
        Arc::new(RecordingEvents::default()),
        prompt_files.clone(),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    std::fs::write(tmp.path().join("prompt.md"), "placeholder").expect("write prompt");

    let mut cleanup_step = invoke_step("cleanup");
    cleanup_step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompt.md".to_string(),
        },
    });
    let mut recipe = recipe(
        "cleanup_dispatch_prompt_lock",
        vec![invoke_step("main")],
        None,
    );
    recipe.source_dir = Some(tmp.path().display().to_string());
    recipe.cleanup = vec![RecipeCleanupStep {
        when: vec![CleanupTrigger::Success],
        step: cleanup_step,
    }];
    let (run_id, _) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cleanup-dispatch-prompt-lock-run"),
            BTreeMap::new(),
        )
        .expect("prepare");

    // Re-entry verifies the cleanup lock once before cleanup dispatch reads it.
    prompt_files.change_on_call.store(
        prompt_files.calls.load(Ordering::SeqCst) + 2,
        Ordering::SeqCst,
    );
    let summary = kernel
        .run_prepared_run(tmp.path(), &home_dir, &run_id)
        .expect("run records the cleanup dispatch failure");
    assert_eq!(summary.status, KernelRunStatus::Success);
    assert!(
        execution
            .invocations()
            .iter()
            .all(|invocation| invocation.request.step.id != "cleanup")
    );
}

#[test]
fn resume_dispatch_rejects_changed_prompt_bytes() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let prompt_files = Arc::new(DispatchChangingPromptFiles::new());
    let kernel = test_kernel_with_prompt_files(
        execution.clone(),
        run_store,
        Arc::new(RecordingEvents::default()),
        prompt_files.clone(),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    std::fs::write(tmp.path().join("prompt.md"), "placeholder").expect("write prompt");
    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompt.md".to_string(),
        },
    });
    let mut recipe = recipe("resume_dispatch_prompt_lock", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());
    execution.plan_result(
        "resume-dispatch-prompt-lock-run",
        "invoke",
        1,
        failed_result(
            "resume-dispatch-prompt-lock-run",
            "invoke",
            1,
            "planned failure",
        ),
    );
    let initial = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("resume-dispatch-prompt-lock-run"),
        )
        .expect("initial run");
    assert_eq!(initial.status, KernelRunStatus::Failed);
    execution.clear_invocations();

    prompt_files.change_on_call.store(
        prompt_files.calls.load(Ordering::SeqCst) + 2,
        Ordering::SeqCst,
    );
    let summary = kernel
        .resume_run(tmp.path(), &home_dir, "resume-dispatch-prompt-lock-run")
        .expect("resume records the dispatch failure");
    assert_eq!(summary.status, KernelRunStatus::Failed);
    assert!(execution.invocations().is_empty());
}

#[test]
fn delete_locked_prompt_file_fails_re_entry_run_level() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let prompt_path = tmp.path().join("prompt.txt");
    std::fs::write(&prompt_path, "hello prompt").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: prompt_path.display().to_string(),
        },
    });
    let mut recipe = recipe("prompt_delete_lock_wf", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());

    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("deleted-prompt-lock-run"),
            BTreeMap::new(),
        )
        .expect("start");

    std::fs::remove_file(&prompt_path).expect("delete prompt");

    let err = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect_err("run should reject deleted prompt file");
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn preflight_run_reports_plugin_digest_drift_as_failed_check() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let (plugin_dir, plugin_version) = install_shell_plugin_fixture(&home_dir);
    let plugin_sha256 = plugin_fixture_digest(&plugin_dir);

    let step = RecipeStep {
        id: "invoke".to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: None,
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: Some(plugin_version.clone()),
            plugin_sha256: Some(plugin_sha256),
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Default::default(),
        network_policy: Default::default(),
        effective_filesystem_policy: Default::default(),
        effective_filesystem_policy_source: None,
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    };
    let recipe = recipe("plugin_lock_wf", vec![step], None);

    let (run_id, preflight) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("preflight-plugin-lock-run"),
            BTreeMap::new(),
        )
        .expect("prepare");
    assert_eq!(preflight.status, PreflightStatus::Passed);

    std::fs::write(
        plugin_dir.join("plugin.py"),
        "#!/usr/bin/env python3\nprint('drifted')\n",
    )
    .expect("mutate plugin");

    let report = kernel
        .preflight_run(tmp.path(), &home_dir, &run_id)
        .expect("preflight should report plugin drift");
    assert_eq!(report.status, PreflightStatus::Failed);
    assert!(
        report.checks.iter().any(|check| {
            check.kind == "asset_lock.plugin"
                && check.status == PreflightStatus::Failed
                && check
                    .message
                    .contains(&format!("plugin 'shell' version '{}'", plugin_version))
                && check.message.contains("has changed")
        }),
        "expected plugin drift check, got: {:?}",
        report.checks
    );

    let run = run_store.run_json(&run_id);
    assert!(run_has_preflight_check(&run, "asset_lock.plugin", "failed"));
}

#[test]
fn prepare_rejects_custom_filesystem_mode_before_run_birth() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let _ = install_shell_plugin_fixture(&home_dir);

    // A persisted recipe that bypassed compiler validation and carries the
    // unsupported custom mode in its authored step-level policy.
    let custom_policy = transist_protocol::FilesystemPolicy {
        workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
            mode: transist_protocol::WorkspaceAccessMode::Custom,
        }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    };
    let step = RecipeStep {
        id: "invoke".to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: None,
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            plugin_sha256: None,
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Some(custom_policy),
        network_policy: Default::default(),
        effective_filesystem_policy: Default::default(),
        effective_filesystem_policy_source: None,
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    };
    let recipe = recipe("custom-mode-wf", vec![step], None);

    let error = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("preflight-custom-mode-run"),
            BTreeMap::new(),
        )
        .expect_err("custom mode must reject preparation");
    assert!(
        error
            .to_string()
            .contains("filesystem_custom_mode_unsupported")
    );
    assert!(!run_store.has_run("preflight-custom-mode-run"));
}

#[test]
fn run_prepared_run_rejects_stale_preflight_with_custom_filesystem_mode() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let _ = install_shell_plugin_fixture(&home_dir);

    // Build a custom-mode recipe and inject it as the persisted snapshot,
    // along with a stale passing preflight that predates the custom-mode
    // rejection.
    let custom_policy = transist_protocol::FilesystemPolicy {
        workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
            mode: transist_protocol::WorkspaceAccessMode::Custom,
        }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    };
    let custom_step = RecipeStep {
        id: "invoke".to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: None,
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            plugin_sha256: None,
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Some(custom_policy.clone()),
        network_policy: Default::default(),
        effective_filesystem_policy: custom_policy,
        effective_filesystem_policy_source: Some(transist_protocol::FilesystemPolicySource::Step),
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    };
    let custom_recipe = recipe("stale-custom-wf", vec![custom_step], None);
    let custom_yaml = yaml_serde::to_string(&custom_recipe).expect("recipe yaml");
    run_store
        .write_recipe_snapshot("stale-custom-run", &custom_yaml)
        .expect("write recipe snapshot");

    let snapshot = KernelRunSnapshot::new(
        "stale-custom-run",
        "stale-custom-wf",
        "v1",
        &["invoke".to_string()],
    );
    let mut record = PersistedRunRecord::from_snapshot(
        snapshot,
        Some(RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: recipe_snapshot_sha(&custom_recipe),
        }),
        None,
        BTreeMap::new(),
        0,
        1,
        1,
    );
    record.asset_lock_version = 1;
    record.preflight = Some(PersistedPreflightRecord::from_checks(1, vec![]));
    seed_run_locator(&home_dir, "stale-custom-run");
    run_store.inject_record(record);

    // Launch the prepared run. The stale preflight override must not bypass
    // the custom-mode re-check.
    let summary = kernel
        .run_prepared_run(tmp.path(), &home_dir, "stale-custom-run")
        .expect("run returns summary even on preflight failure");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let run = run_store.run_json("stale-custom-run");
    assert!(
        run_has_preflight_check(&run, "filesystem_custom_mode_unsupported", "failed"),
        "expected custom-mode re-check to fire despite stale preflight override"
    );
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn preflight_run_ignores_plugin_digest_drift_for_v0_workflow() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let (plugin_dir, plugin_version) = install_shell_plugin_fixture(&home_dir);
    let plugin_sha256 = plugin_fixture_digest(&plugin_dir);

    let step = RecipeStep {
        id: "invoke".to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: None,
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: Some(plugin_version),
            plugin_sha256: Some(plugin_sha256),
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Default::default(),
        network_policy: Default::default(),
        effective_filesystem_policy: Default::default(),
        effective_filesystem_policy_source: None,
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    };
    let mut recipe = recipe("plugin_lock_test_wf", vec![step], None);
    recipe.workflow.version = "v0".to_string();

    let (run_id, preflight) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("preflight-plugin-lock-test-run"),
            BTreeMap::new(),
        )
        .expect("prepare");
    assert_eq!(preflight.status, PreflightStatus::Passed);

    std::fs::write(
        plugin_dir.join("plugin.py"),
        "#!/usr/bin/env python3\nprint('drifted')\n",
    )
    .expect("mutate plugin");

    let report = kernel
        .preflight_run(tmp.path(), &home_dir, &run_id)
        .expect("preflight should ignore plugin drift for v0");
    assert_eq!(report.status, PreflightStatus::Passed);
    assert!(
        !report
            .checks
            .iter()
            .any(|check| check.kind == "asset_lock.plugin"),
        "did not expect plugin drift check, got: {:?}",
        report.checks
    );

    let run = run_store.run_json(&run_id);
    assert!(!run_has_preflight_check(
        &run,
        "asset_lock.plugin",
        "failed"
    ));
}

#[test]
fn preflight_run_reports_prompt_file_drift_as_failed_check() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let prompt_path = tmp.path().join("prompt.txt");
    std::fs::write(&prompt_path, "hello prompt").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: prompt_path.display().to_string(),
        },
    });
    let mut recipe = recipe("prompt_preflight_lock_wf", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());

    let (run_id, preflight) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("preflight-prompt-lock-run"),
            BTreeMap::new(),
        )
        .expect("prepare");
    assert_eq!(preflight.status, PreflightStatus::Passed);

    std::fs::write(&prompt_path, "changed prompt").expect("mutate prompt");

    let report = kernel
        .preflight_run(tmp.path(), &home_dir, &run_id)
        .expect("preflight should report prompt drift");
    assert_eq!(report.status, PreflightStatus::Failed);
    assert!(
        report.checks.iter().any(|check| {
            check.kind == "asset_lock.prompt_file"
                && check.status == PreflightStatus::Failed
                && check.message.contains("prompt_file")
                && check.message.contains("has changed")
        }),
        "expected prompt drift check, got: {:?}",
        report.checks
    );

    let run = run_store.run_json(&run_id);
    assert!(run_has_preflight_check(
        &run,
        "asset_lock.prompt_file",
        "failed"
    ));
}

#[test]
fn prompt_file_lock_is_portable_across_recipe_source_dir_renames() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let source_a = tmp.path().join("recipe-a");
    let source_b = tmp.path().join("recipe-b");
    let prompts_dir = source_a.join("prompts");
    std::fs::create_dir_all(&prompts_dir).expect("create prompts dir");
    std::fs::write(prompts_dir.join("intake.txt"), "hello prompt").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompts/intake.txt".to_string(),
        },
    });
    let mut recipe = recipe("portable_prompt_lock_wf", vec![step], None);
    recipe.source_dir = Some(source_a.display().to_string());

    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("portable-prompt-lock-run"),
            BTreeMap::new(),
        )
        .expect("start");

    std::fs::rename(&source_a, &source_b).expect("rename source dir");
    recipe.source_dir = Some(source_b.display().to_string());
    let persisted_raw = run_store
        .recipes
        .lock()
        .expect("recipes lock")
        .get(&run_id)
        .cloned()
        .expect("persisted recipe");
    let mut persisted_recipe: Recipe =
        yaml_serde::from_str(&persisted_raw).expect("persisted recipe yaml");
    persisted_recipe.source_dir = Some(source_b.display().to_string());
    let mut record: PersistedRunRecord =
        serde_json::from_value(run_store.run_json(&run_id)).expect("record json");
    record.recipe_snapshot = Some(RecipeSnapshotRef {
        path: "recipe.yaml".to_string(),
        sha256: recipe_snapshot_sha(&persisted_recipe),
    });
    run_store.inject_record(record);
    run_store.inject_recipe_snapshot(&run_id, &persisted_recipe);

    let report = kernel
        .preflight_run(tmp.path(), &home_dir, &run_id)
        .expect("preflight should succeed after source dir rename");
    assert_eq!(report.status, PreflightStatus::Passed);
}

#[test]
fn prompt_file_lock_resolves_input_placeholder_to_absolute_path() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let prompt_path = tmp.path().join("intake.txt");
    std::fs::write(&prompt_path, "hello prompt").expect("write prompt");

    let mut step = invoke_step("invoke");
    step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "{{inputs.path}}.txt".to_string(),
        },
    });
    let mut recipe = recipe("templated_prompt_lock_wf", vec![step], None);
    recipe.source_dir = Some(tmp.path().display().to_string());

    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("templated-prompt-lock-run"),
            BTreeMap::from([(
                "path".to_string(),
                KernelValue::String("intake".to_string()),
            )]),
        )
        .expect("start");

    let run = run_store.run_json(&run_id);
    assert_eq!(run["asset_locks"][0]["ref_id"], "{{inputs.path}}.txt");

    let report = kernel
        .preflight_run(tmp.path(), &home_dir, &run_id)
        .expect("preflight should resolve templated prompt path");
    assert_eq!(report.status, PreflightStatus::Passed);
}

#[test]
fn replay_copies_persisted_global_filesystem_policy_after_settings_change() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);

    let recipe = recipe("replay_global_policy_wf", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    write_home_filesystem_policy(&home_dir, "read_only");

    let _first = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("source-global-policy-run"),
        )
        .expect("execute source run");
    assert!(!execution.invocations().is_empty());
    execution.clear_invocations();

    write_home_filesystem_policy(&home_dir, "full");

    let _replay = kernel
        .replay_run(
            tmp.path(),
            &home_dir,
            "source-global-policy-run",
            Some("replayed-global-policy-run"),
        )
        .expect("replay run");
    assert!(!execution.invocations().is_empty());

    let invocation = execution.invocations().pop().expect("replay invocation");
    assert_eq!(
        invocation.request.policy["filesystem"]["source"],
        serde_json::json!("global")
    );
    assert_eq!(
        invocation.request.policy["filesystem"]["workspace_access"]["mode"],
        serde_json::json!("read_only")
    );

    let replay_recipe_yaml = run_store
        .load_recipe_snapshot("replayed-global-policy-run")
        .expect("load replay recipe snapshot");
    let replay_recipe: Recipe =
        yaml_serde::from_str(&replay_recipe_yaml).expect("replay recipe yaml");
    assert_eq!(
        replay_recipe.steps[0]
            .effective_filesystem_policy_source
            .expect("replay policy source"),
        transist_protocol::FilesystemPolicySource::Global
    );
    assert_eq!(
        replay_recipe.steps[0]
            .effective_filesystem_policy
            .workspace_access
            .as_ref()
            .unwrap()
            .mode,
        transist_protocol::WorkspaceAccessMode::ReadOnly
    );
}

#[test]
fn start_recipe_uses_builtin_full_when_no_policy_layers_exist() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let recipe = recipe("builtin_fs_wf", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("builtin-fs-run"),
            BTreeMap::new(),
        )
        .expect("start recipe");

    let persisted_recipe_yaml = run_store
        .load_recipe_snapshot("builtin-fs-run")
        .expect("load recipe snapshot");
    let persisted_recipe: Recipe =
        yaml_serde::from_str(&persisted_recipe_yaml).expect("recipe yaml");
    assert_eq!(
        persisted_recipe.steps[0]
            .effective_filesystem_policy
            .workspace_access,
        None
    );
    assert_eq!(
        persisted_recipe.steps[0].effective_filesystem_policy_source,
        Some(transist_protocol::FilesystemPolicySource::Builtin)
    );
}

#[test]
fn start_recipe_workflow_policy_overrides_global_policy() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let mut recipe = recipe("workflow_fs_wf", vec![invoke_step("invoke")], None);
    recipe.filesystem_policy = Some(workspace_policy(
        transist_protocol::WorkspaceAccessMode::ReadOnly,
    ));
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    write_home_filesystem_policy(&home_dir, "full");

    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("workflow-fs-run"),
            BTreeMap::new(),
        )
        .expect("start recipe");

    let persisted_recipe_yaml = run_store
        .load_recipe_snapshot("workflow-fs-run")
        .expect("load recipe snapshot");
    let persisted_recipe: Recipe =
        yaml_serde::from_str(&persisted_recipe_yaml).expect("recipe yaml");
    assert_eq!(
        persisted_recipe.steps[0].effective_filesystem_policy_source,
        Some(transist_protocol::FilesystemPolicySource::Workflow)
    );
    assert_eq!(
        persisted_recipe.steps[0]
            .effective_filesystem_policy
            .workspace_access
            .as_ref()
            .unwrap()
            .mode,
        transist_protocol::WorkspaceAccessMode::ReadOnly
    );
}

#[test]
fn start_recipe_step_policy_overrides_workflow_policy() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let mut step = invoke_step("invoke");
    step.filesystem_policy = Some(workspace_policy(
        transist_protocol::WorkspaceAccessMode::ReadOnly,
    ));
    let mut recipe = recipe("step_fs_wf", vec![step], None);
    recipe.filesystem_policy = Some(workspace_policy(
        transist_protocol::WorkspaceAccessMode::Full,
    ));
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("step-fs-run"),
            BTreeMap::new(),
        )
        .expect("start recipe");

    let persisted_recipe_yaml = run_store
        .load_recipe_snapshot("step-fs-run")
        .expect("load recipe snapshot");
    let persisted_recipe: Recipe =
        yaml_serde::from_str(&persisted_recipe_yaml).expect("recipe yaml");
    assert_eq!(
        persisted_recipe.steps[0].effective_filesystem_policy_source,
        Some(transist_protocol::FilesystemPolicySource::Step)
    );
    assert_eq!(
        persisted_recipe.steps[0]
            .effective_filesystem_policy
            .workspace_access
            .as_ref()
            .unwrap()
            .mode,
        transist_protocol::WorkspaceAccessMode::ReadOnly
    );
}

fn failed_result(run_id: &str, step_id: &str, execution: u32, message: &str) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("{run_id}-{step_id}-{execution}"),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: message.to_string(),
        timing: transist_protocol::InvokeTiming {
            started_at: "1".to_string(),
            finished_at: "1".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({}),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(transist_protocol::ProtocolError {
            code: transist_protocol::CanonicalErrorCode::ToolExecutionError,
            class: transist_protocol::CanonicalErrorCode::ToolExecutionError.class(),
            message: message.to_string(),
            retryability: transist_protocol::CanonicalErrorCode::ToolExecutionError.retryability(),
        }),
    }
}

/// Builds a minimal success `ExecutionResult` identified by run_id / step_id /
/// execution number. Callers use struct update syntax to override fields such
/// as `output`.
fn success_result_for(run_id: &str, step_id: &str, execution: u32) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("{run_id}-{step_id}-{execution}"),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution,
        status: "success".to_string(),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timing: transist_protocol::InvokeTiming {
            started_at: "1".to_string(),
            finished_at: "1".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({"ok": true}),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    }
}

fn failed_result_for(run_id: &str, step_id: &str, execution: u32) -> ExecutionResult {
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("{run_id}-{step_id}-{execution}"),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: "boom".to_string(),
        timing: transist_protocol::InvokeTiming {
            started_at: "1".to_string(),
            finished_at: "2".to_string(),
            duration_ms: 1,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({"ok": false}),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
    }
}

fn legacy_record(run_id: &str, step_ids: &[String]) -> PersistedRunRecord {
    let snapshot = KernelRunSnapshot::new(run_id, "wf", "v1", step_ids);
    let mut record =
        PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 1);
    record.recipe_snapshot = None;
    record.asset_lock_version = 0;
    record
}

fn test_preflight_check(
    step_id: &str,
    kind: &str,
    status: PreflightStatus,
    message: &str,
) -> PreflightCheck {
    PreflightCheck {
        kind: kind.to_string(),
        status,
        step_ids: vec![step_id.to_string()],
        adapter_kind: Some("process".to_string()),
        plugin_id: Some("shell".to_string()),
        action: Some("echo".to_string()),
        message: message.to_string(),
    }
}

fn run_has_preflight_check(run: &serde_json::Value, kind: &str, status: &str) -> bool {
    run["preflight"]["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|check| check["kind"] == kind && check["status"] == status)
}

#[test]
fn adapter_execution_errors_become_failed_steps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_store = Arc::new(MemoryRunStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe("wf", vec![invoke_step("invoke")], None);

    kernel
        .start_recipe(
            tmp.path(),
            &test_home_dir(&tmp),
            &recipe,
            Some("spawn-error"),
            BTreeMap::new(),
        )
        .expect("start");
    execution.plan_error("spawn-error", "invoke", 1, "failed spawning process plugin");

    let summary = kernel
        .run_started_run(tmp.path(), &test_home_dir(&tmp), "spawn-error")
        .expect("run");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let run = run_store.run_json("spawn-error");
    assert_eq!(run["status"], "failed");
    assert_eq!(run["steps"]["invoke"]["status"], "failed");
    assert_eq!(
        run["steps"]["invoke"]["last_result"]["error"]["code"],
        "tool_execution_error"
    );
}

#[test]
fn failed_step_rejects_missing_artifact_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_store = Arc::new(MemoryRunStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe("wf", vec![invoke_step("invoke")], None);

    kernel
        .start_recipe(
            tmp.path(),
            &test_home_dir(&tmp),
            &recipe,
            Some("artifact-failure"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut result = failed_result("artifact-failure", "invoke", 1, "tool failed");
    result.artifacts.push(FileRef {
        path: "tool-output.txt".to_string(),
        sha256: "artifact-sha".to_string(),
        bytes: 12,
    });
    execution.plan_result("artifact-failure", "invoke", 1, result);

    let summary = kernel
        .run_started_run(tmp.path(), &test_home_dir(&tmp), "artifact-failure")
        .expect("run");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let executions = run_store.step_executions();
    let record: transist_core::StepExecutionRecord =
        serde_json::from_value(executions[0].clone()).expect("execution record");
    assert!(record.state_mutations.is_empty());

    let failures = run_store.failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].run_id, "artifact-failure");
    assert_eq!(failures[0].step_id, "invoke");
    assert_eq!(failures[0].failure_class, FailureClass::ToolError);
    assert_eq!(failures[0].error_code, "tool_execution_error");
    assert!(failures[0].retryable);
    assert!(failures[0].safe_to_retry);
    assert!(!failures[0].state_mutation_detected);
}

#[test]
fn failed_step_with_staged_artifact_records_state_mutation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_store = Arc::new(MemoryRunStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe("wf", vec![invoke_step("invoke")], None);

    kernel
        .start_recipe(
            tmp.path(),
            &test_home_dir(&tmp),
            &recipe,
            Some("staged-artifact-failure"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut result = failed_result("staged-artifact-failure", "invoke", 1, "tool failed");
    result.artifacts.push(FileRef {
        path: "staged://tool-output.txt".to_string(),
        sha256: "artifact-sha".to_string(),
        bytes: 0,
    });
    execution.plan_result("staged-artifact-failure", "invoke", 1, result);

    let summary = kernel
        .run_started_run(tmp.path(), &test_home_dir(&tmp), "staged-artifact-failure")
        .expect("run");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let executions = run_store.step_executions();
    let record: transist_core::StepExecutionRecord =
        serde_json::from_value(executions[0].clone()).expect("execution record");
    assert_eq!(record.state_mutations.len(), 1);
    assert!(matches!(
        &record.state_mutations[0],
        transist_core::StateMutation::ArtifactWritten { .. }
    ));

    let failures = run_store.failures();
    assert_eq!(failures.len(), 1);
    assert!(!failures[0].safe_to_retry);
    assert!(failures[0].state_mutation_detected);
}

#[test]
fn failed_step_without_adapter_outputs_records_no_state_mutations() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_store = Arc::new(MemoryRunStore::default());
    let execution = Arc::new(RecordingExecution::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe("wf", vec![invoke_step("invoke")], None);

    kernel
        .start_recipe(
            tmp.path(),
            &test_home_dir(&tmp),
            &recipe,
            Some("no-mutation-failure"),
            BTreeMap::new(),
        )
        .expect("start");
    execution.plan_result(
        "no-mutation-failure",
        "invoke",
        1,
        failed_result("no-mutation-failure", "invoke", 1, "tool failed"),
    );

    let summary = kernel
        .run_started_run(tmp.path(), &test_home_dir(&tmp), "no-mutation-failure")
        .expect("run");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let executions = run_store.step_executions();
    let record: transist_core::StepExecutionRecord =
        serde_json::from_value(executions[0].clone()).expect("execution record");
    assert!(record.state_mutations.is_empty());

    let failures = run_store.failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].failure_class, FailureClass::ToolError);
    assert!(failures[0].retryable);
    assert!(failures[0].safe_to_retry);
    assert!(!failures[0].state_mutation_detected);
}

#[test]
fn kernel_builds_new_execution_identity_and_keeps_secrets_out_of_persisted_request() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);

    let recipe = recipe(
        "demo",
        vec![invoke_step_with(
            "invoke",
            BTreeMap::from([
                (
                    "args".to_string(),
                    KernelValue::Array(vec![KernelValue::String("hello".to_string())]),
                ),
                (
                    "secret_refs".to_string(),
                    KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
                ),
            ]),
        )],
        None,
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("run_1"))
        .expect("execute");
    assert_eq!(summary.status.as_str(), "success");

    let invocations = execution.invocations();
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].request.adapter_kind, AdapterKind::Process);
    assert_eq!(invocations[0].request.metadata["plugin_id"], "shell");
    assert_eq!(
        invocations[0]
            .secret_values
            .get("API_TOKEN")
            .map(String::as_str),
        Some("topsecret")
    );

    let persisted = run_store.run_json("run_1");
    let persisted_text = persisted.to_string();
    assert!(!persisted_text.contains("topsecret"));
    assert_eq!(
        persisted["steps"]["invoke"]["last_request"]["metadata"]["plugin_id"],
        "shell"
    );
    assert_eq!(persisted["recipe_snapshot"]["path"], "recipe.yaml");
}

#[test]
fn resume_failed_run_reuses_succeeded_steps_and_retries_only_failed_step() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let recipe = recipe(
        "resume_demo",
        vec![invoke_step("a"), invoke_step("b")],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result(
        "resume-run",
        "b",
        1,
        failed_result("resume-run", "b", 1, "boom"),
    );

    let first = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("resume-run"))
        .expect("first run");
    assert_eq!(first.status, KernelRunStatus::Failed);

    execution.plan_result(
        "resume-run",
        "b",
        2,
        success_result(&execution.invocations()[1]),
    );
    let resumed = kernel
        .resume_run(tmp.path(), &home_dir, "resume-run")
        .expect("resume");
    assert_eq!(resumed.status, KernelRunStatus::Success);

    let invocations = execution.invocations();
    let step_ids: Vec<String> = invocations
        .iter()
        .map(|invocation| invocation.request.step.id.clone())
        .collect();
    assert_eq!(step_ids, vec!["a", "b", "b"]);

    let persisted = run_store.run_json("resume-run");
    assert_eq!(persisted["status"], "success");
    assert_eq!(persisted["resume_count"], 1);
    assert_eq!(persisted["steps"]["a"]["executions"], 1);
    assert_eq!(persisted["steps"]["b"]["executions"], 2);

    let executions = run_store.step_executions();
    assert_eq!(executions.len(), 3);
    let resumed_event = events
        .snapshot()
        .into_iter()
        .find(|event| event.kind == "run_resumed")
        .expect("run_resumed event");
    assert_eq!(
        serde_json::Value::from(resumed_event.payload)["resume_count"],
        1
    );
}

#[test]
fn replay_failed_run_creates_new_run_and_reexecutes_all_steps_from_start() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let lease_trace = OperationTrace::default();
    let kernel = test_kernel_with_lease(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(NoopLease::with_operation_trace(lease_trace.clone())),
    );
    let recipe = recipe(
        "replay_demo",
        vec![invoke_step("a"), invoke_step("b")],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result(
        "source-run",
        "b",
        1,
        failed_result("source-run", "b", 1, "boom"),
    );
    let first = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("source-run"))
        .expect("first run");
    assert_eq!(first.status, KernelRunStatus::Failed);
    lease_trace.clear();

    let replayed = kernel
        .replay_run(tmp.path(), &home_dir, "source-run", Some("replay-run"))
        .expect("replay");
    assert_eq!(replayed.run_id, "replay-run");
    assert_eq!(replayed.status, KernelRunStatus::Success);

    let invocations = execution.invocations();
    let run_step_executions: Vec<(String, String, u32)> = invocations
        .iter()
        .map(|invocation| {
            (
                invocation.request.run_id.clone(),
                invocation.request.step.id.clone(),
                invocation.request.step.execution,
            )
        })
        .collect();
    assert_eq!(
        run_step_executions,
        vec![
            ("source-run".to_string(), "a".to_string(), 1),
            ("source-run".to_string(), "b".to_string(), 1),
            ("replay-run".to_string(), "a".to_string(), 1),
            ("replay-run".to_string(), "b".to_string(), 1),
        ]
    );

    let source = run_store.run_json("source-run");
    let replay = run_store.run_json("replay-run");
    assert_eq!(source["status"], "failed");
    assert_eq!(replay["status"], "success");
    assert_eq!(replay["lineage"]["mode"], "replay");
    assert_eq!(replay["lineage"]["source_run_id"], "source-run");
    assert_eq!(replay["lineage"]["root_run_id"], "replay-run");
    assert_eq!(
        lease_trace.snapshot(),
        vec![
            "lease_acquired:source-run",
            "lease_released:source-run",
            "lease_acquired:replay-run",
            "lease_acquired:plugin-lifecycle-reservation",
            "lease_released:plugin-lifecycle-reservation",
            "lease_released:replay-run",
        ]
    );
    let started = events
        .snapshot()
        .into_iter()
        .find(|event| event.kind == "run_started" && event.run_id == "replay-run")
        .expect("replay run_started");
    assert_eq!(serde_json::Value::from(started.payload)["mode"], "replay");
}

#[test]
fn guard_blocked_skipped_step_remains_skipped_across_resume() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe(
        "guard_demo",
        vec![invoke_step("a"), invoke_step("b"), invoke_step("c")],
        Some(RecipeGraph {
            entry: "a".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "a".to_string(),
                    to: "b".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("false".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "a".to_string(),
                    to: "c".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("true".to_string()),
                    r#loop: None,
                },
            ],
            max_iterations: None,
        }),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result(
        "guard-run",
        "c",
        1,
        failed_result("guard-run", "c", 1, "boom"),
    );
    let first = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("guard-run"))
        .expect("first run");
    assert_eq!(first.status, KernelRunStatus::Failed);

    let resumed = kernel
        .resume_run(tmp.path(), &home_dir, "guard-run")
        .expect("resume");
    assert_eq!(resumed.status, KernelRunStatus::Success);

    let persisted = run_store.run_json("guard-run");
    assert_eq!(persisted["steps"]["b"]["status"], "skipped");
    assert_eq!(persisted["steps"]["b"]["skip_reason"], "guard_blocked");
    assert_eq!(persisted["steps"]["c"]["executions"], 2);
    assert!(
        execution
            .invocations()
            .iter()
            .all(|invocation| invocation.request.step.id != "b")
    );
}

#[test]
fn dependency_failed_skipped_descendant_resets_and_executes_after_resume() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe(
        "dep_demo",
        vec![
            invoke_step("a"),
            invoke_step("b"),
            invoke_step("c"),
            invoke_step("d"),
        ],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result("dep-run", "b", 1, failed_result("dep-run", "b", 1, "boom"));
    let first = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("dep-run"))
        .expect("first run");
    assert_eq!(first.status, KernelRunStatus::Failed);
    let first_persisted = run_store.run_json("dep-run");
    assert_eq!(
        first_persisted["steps"]["c"]["skip_reason"],
        "dependency_failed"
    );
    assert_eq!(
        first_persisted["steps"]["d"]["skip_reason"],
        "dependency_failed"
    );

    let resumed = kernel
        .resume_run(tmp.path(), &home_dir, "dep-run")
        .expect("resume");
    assert_eq!(resumed.status, KernelRunStatus::Success);

    let invocations = execution.invocations();
    let step_ids: Vec<String> = invocations
        .iter()
        .map(|invocation| invocation.request.step.id.clone())
        .collect();
    assert_eq!(step_ids, vec!["a", "b", "b", "c", "d"]);

    let persisted = run_store.run_json("dep-run");
    assert_eq!(persisted["steps"]["a"]["executions"], 1);
    assert_eq!(persisted["steps"]["b"]["executions"], 2);
    assert_eq!(persisted["steps"]["c"]["executions"], 1);
    assert_eq!(persisted["steps"]["d"]["executions"], 1);
}

#[test]
fn manual_resume_is_allowed_after_automatic_retry_budget_is_exhausted() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe(
        "retry_demo",
        vec![invoke_step_with(
            "a",
            BTreeMap::from([
                (
                    "retry_max".to_string(),
                    KernelValue::Number(serde_json::Number::from(1_u64)),
                ),
                (
                    "retry_wait_ms".to_string(),
                    KernelValue::Number(serde_json::Number::from(0_u64)),
                ),
            ]),
        )],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result(
        "retry-run",
        "a",
        1,
        failed_result("retry-run", "a", 1, "boom"),
    );
    execution.plan_result(
        "retry-run",
        "a",
        2,
        failed_result("retry-run", "a", 2, "boom"),
    );
    let first = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("retry-run"))
        .expect("first run");
    assert_eq!(first.status, KernelRunStatus::Failed);
    assert_eq!(
        run_store.run_json("retry-run")["steps"]["a"]["executions"],
        2
    );

    let resumed = kernel
        .resume_run(tmp.path(), &home_dir, "retry-run")
        .expect("resume");
    assert_eq!(resumed.status, KernelRunStatus::Success);
    assert_eq!(
        run_store.run_json("retry-run")["steps"]["a"]["executions"],
        3
    );
}

#[test]
fn fresh_run_with_existing_run_ref_fails_explicitly() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store, events);
    let recipe = recipe("duplicate_demo", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("duplicate-run"))
        .expect("first run");
    let err = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("duplicate-run"))
        .expect_err("duplicate run id must fail");
    assert!(err.to_string().contains("runs retry"));
    assert!(err.to_string().contains("runs replay"));
}

#[test]
fn legacy_run_without_recipe_snapshot_fails_for_resume_and_replay() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let mut record = legacy_record("legacy-run", &["a".to_string()]);
    record.status = KernelRunStatus::Failed;
    run_store.inject_record(record);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let resume_err = kernel
        .resume_run(tmp.path(), &home_dir, "legacy-run")
        .expect_err("legacy resume must fail");
    assert!(
        resume_err
            .to_string()
            .contains("before run asset locking support")
    );

    let replay_err = kernel
        .replay_run(tmp.path(), &home_dir, "legacy-run", Some("legacy-replay"))
        .expect_err("legacy replay must fail");
    assert!(
        replay_err
            .to_string()
            .contains("before run asset locking support")
    );
}

#[test]
fn legacy_v1_asset_lock_run_is_rejected_with_migration_message() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let mut record = legacy_record("legacy-started-run", &["a".to_string()]);
    record.recipe_snapshot = Some(RecipeSnapshotRef {
        path: "recipe.yaml".to_string(),
        sha256: "deadbeef".to_string(),
    });
    run_store.inject_record(record);
    run_store.inject_recipe_snapshot(
        "legacy-started-run",
        &recipe("wf", vec![invoke_step("a")], None),
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let err = kernel
        .run_started_run(tmp.path(), &home_dir, "legacy-started-run")
        .expect_err("legacy started run must fail");
    assert!(err.to_string().contains("before run asset locking support"));
}

#[test]
fn run_started_run_rejects_recipe_snapshot_drift_for_persisted_v1_workflow() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let original = recipe(
        "snapshot_lock_wf",
        vec![invoke_step_with(
            "a",
            BTreeMap::from([(
                "message".to_string(),
                KernelValue::String("original".to_string()),
            )]),
        )],
        None,
    );
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &original,
            Some("snapshot-drift-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let mut patched = original.clone();
    patched.steps[0].with.insert(
        "message".to_string(),
        KernelValue::String("patched".to_string()),
    );
    // The mutable snapshot claims v0, but the persisted run record remains v1.
    patched.workflow.version = "v0".to_string();
    run_store.inject_recipe_snapshot(&run_id, &patched);

    let err = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect_err("non-test workflow should reject patched recipe snapshot");
    assert!(
        err.to_string().contains("persisted recipe snapshot")
            && err.to_string().contains("has changed"),
        "expected snapshot drift error, got: {err}"
    );
    assert!(execution.invocations().is_empty());
}

#[test]
fn current_run_missing_persisted_capability_map_fails_before_dispatch() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let recipe = recipe("missing-authority", vec![invoke_step("invoke")], None);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("missing-authority-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let raw = run_store
        .recipes
        .lock()
        .expect("recipes lock")
        .get(&run_id)
        .cloned()
        .expect("persisted recipe");
    let mut patched: Recipe = yaml_serde::from_str(&raw).expect("recipe");
    assert!(patched.capabilities_persisted);
    patched.capabilities.clear();
    let patched_yaml = yaml_serde::to_string(&patched).expect("patched recipe yaml");
    run_store
        .recipes
        .lock()
        .expect("recipes lock")
        .insert(run_id.clone(), patched_yaml.clone());
    let mut record: PersistedRunRecord =
        serde_json::from_value(run_store.run_json(&run_id)).expect("record");
    record.recipe_snapshot.as_mut().expect("snapshot").sha256 =
        format!("{:x}", Sha256::digest(patched_yaml.as_bytes()));
    run_store.inject_record(record);

    let error = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect_err("missing current authority must deny");
    assert!(error.to_string().contains("persisted authority"));
    assert!(execution.invocations().is_empty());
}

#[test]
fn current_run_reentry_rejects_workspace_authority_drift() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store, events);
    let original_repo = tempfile::tempdir().expect("original repo");
    let new_repo = tempfile::tempdir().expect("new repo");
    let home_dir = test_home_dir(&original_repo);
    let recipe = recipe("workspace-authority", vec![invoke_step("invoke")], None);
    let run_id = kernel
        .start_recipe(
            original_repo.path(),
            &home_dir,
            &recipe,
            Some("workspace-authority-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let error = kernel
        .run_started_run(new_repo.path(), &home_dir, &run_id)
        .expect_err("workspace authority drift must require replay");
    assert!(error.to_string().contains("replay"));
    assert!(execution.invocations().is_empty());
}

#[test]
fn workspace_binding_reentry_rejects_state_recipe_access_mismatch() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let home_dir = test_home_dir(&tmp);
    std::fs::create_dir_all(&home_dir).expect("home");
    let mut recipe = recipe("workspace-state-recipe", vec![invoke_step("invoke")], None);
    recipe.workspace = Some(transist_protocol::WorkspaceBinding {
        path: workspace.path().display().to_string(),
        access: transist_protocol::WorkspaceAccessMode::Full,
    });
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("workspace-state-recipe-run"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut record = run_store.load_run(&run_id).unwrap().unwrap();
    record.workspace.as_mut().unwrap().access = transist_protocol::WorkspaceAccessMode::ReadOnly;
    run_store.inject_record(record);

    let error = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect_err("workspace access mismatch must fail closed");

    assert!(error.to_string().contains("recipe workspace"));
    assert!(execution.invocations().is_empty());
}

#[test]
fn workspace_binding_persisted_workspace_requires_directory_outside_home() {
    for overlap_home in [false, true] {
        let execution = Arc::new(RecordingExecution::default());
        let run_store = Arc::new(MemoryRunStore::default());
        let kernel = test_kernel(
            execution.clone(),
            run_store.clone(),
            Arc::new(RecordingEvents::default()),
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tempfile::tempdir().expect("workspace");
        let home_dir = test_home_dir(&tmp);
        std::fs::create_dir_all(&home_dir).expect("home");
        let mut recipe = recipe("workspace-validation", vec![invoke_step("invoke")], None);
        recipe.workspace = Some(transist_protocol::WorkspaceBinding {
            path: workspace.path().display().to_string(),
            access: transist_protocol::WorkspaceAccessMode::Full,
        });
        let run_id = kernel
            .start_recipe(
                tmp.path(),
                &home_dir,
                &recipe,
                Some(if overlap_home {
                    "workspace-overlap-run"
                } else {
                    "workspace-file-run"
                }),
                BTreeMap::new(),
            )
            .expect("start");
        let invalid = if overlap_home {
            std::fs::canonicalize(&home_dir).expect("canonical home")
        } else {
            let file = tmp.path().join("workspace-file");
            std::fs::write(&file, "not a directory").expect("file");
            std::fs::canonicalize(file).expect("canonical file")
        };
        let mut record = run_store.load_run(&run_id).unwrap().unwrap();
        record.workspace.as_mut().unwrap().path = invalid.display().to_string();
        run_store.inject_record(record);

        let error = kernel
            .run_started_run(tmp.path(), &home_dir, &run_id)
            .expect_err("invalid persisted workspace must fail closed");

        assert!(error.to_string().contains(if overlap_home {
            "overlaps Transist home"
        } else {
            "is not a directory"
        }));
        assert!(execution.invocations().is_empty());
    }
}

#[test]
fn workspace_binding_subflow_persists_inherited_workspace_for_direct_recovery() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let home_dir = test_home_dir(&tmp);
    std::fs::create_dir_all(&home_dir).expect("home");
    let child = recipe("workspace-child", vec![invoke_step("child-invoke")], None);
    let mut child_step = subflow_step("child", child);
    child_step.filesystem_policy = Some(transist_protocol::FilesystemPolicy {
        workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
            mode: transist_protocol::WorkspaceAccessMode::ReadOnly,
        }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    });
    let mut parent = recipe("workspace-parent", vec![child_step], None);
    parent.workspace = Some(transist_protocol::WorkspaceBinding {
        path: workspace.path().display().to_string(),
        access: transist_protocol::WorkspaceAccessMode::Full,
    });

    kernel
        .execute_recipe_with_inputs(
            tmp.path(),
            &home_dir,
            &parent,
            Some("workspace-parent-run"),
            BTreeMap::new(),
        )
        .expect("execute parent");

    let expected = std::fs::canonicalize(workspace.path()).expect("canonical workspace");
    let child_record = run_store
        .load_run("workspace-parent-run__child")
        .unwrap()
        .expect("child record");
    assert_eq!(
        child_record.workspace,
        parent.workspace.map(|mut binding| {
            binding.path = expected.display().to_string();
            binding.access = transist_protocol::WorkspaceAccessMode::ReadOnly;
            binding
        })
    );
    let child_recipe: Recipe = yaml_serde::from_str(
        &run_store
            .load_recipe_snapshot("workspace-parent-run__child")
            .expect("child recipe"),
    )
    .expect("parse child recipe");
    assert_eq!(child_recipe.workspace, child_record.workspace);
    let workspace_capability = child_recipe.capabilities["child-invoke"]
        .iter()
        .find(|request| {
            request.capability == transist_protocol::CapabilityName::Filesystem
                && request.resource.starts_with("workspace://")
        })
        .expect("child workspace capability");
    assert_eq!(workspace_capability.operations, ["list", "read"]);
    assert_eq!(
        execution
            .invocations()
            .last()
            .unwrap()
            .request
            .paths
            .workspace_dir,
        expected.display().to_string()
    );
    assert_eq!(
        execution.invocations().last().unwrap().request.policy["workspace_declared"],
        true
    );

    execution.clear_invocations();
    let error = kernel
        .run_started_run(tmp.path(), &home_dir, "workspace-parent-run__child")
        .expect_err("completed child cannot execute again");
    assert!(error.to_string().contains("status is 'success'"));
    assert!(execution.invocations().is_empty());
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn workspace_binding_recipe_sync_rejects_workspace_mismatch() {
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        Arc::new(RecordingExecution::default()),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let other = tempfile::tempdir().expect("other workspace");
    let home_dir = test_home_dir(&tmp);
    std::fs::create_dir_all(&home_dir).expect("home");
    let mut original = recipe("workspace-sync", vec![invoke_step("invoke")], None);
    original.workflow.version = "v0".to_string();
    original.workspace = Some(transist_protocol::WorkspaceBinding {
        path: workspace.path().display().to_string(),
        access: transist_protocol::WorkspaceAccessMode::Full,
    });
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &original,
            Some("workspace-sync-run"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut replacement = original;
    replacement.workspace.as_mut().unwrap().path = other.path().display().to_string();
    let replacement_yaml = yaml_serde::to_string(&replacement).expect("replacement yaml");

    let error = kernel
        .sync_recipe_snapshot(
            &home_dir,
            &run_id,
            &tmp.path().join("registry/workspace-sync.yaml"),
            &replacement_yaml,
        )
        .expect_err("sync must reject workspace mismatch");

    assert!(error.to_string().contains("workspace does not match"));
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn run_started_run_allows_recipe_snapshot_drift_for_v0_workflow() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut original = recipe(
        "snapshot_test_wf",
        vec![invoke_step_with(
            "a",
            BTreeMap::from([(
                "message".to_string(),
                KernelValue::String("original".to_string()),
            )]),
        )],
        None,
    );
    original.workflow.version = "v0".to_string();
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &original,
            Some("snapshot-test-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let mut patched = original.clone();
    patched.steps[0].with.insert(
        "message".to_string(),
        KernelValue::String("patched".to_string()),
    );
    run_store.inject_recipe_snapshot(&run_id, &patched);

    kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("v0 workflow should allow patched recipe snapshot");

    let invocation = execution.invocations().pop().expect("one invocation");
    assert_eq!(
        invocation.request.input["message"],
        serde_json::json!("patched")
    );
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn start_recipe_persists_test_mode_for_v0_workflow() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let mut recipe = recipe("test_mode_wf", vec![invoke_step("a")], None);
    recipe.workflow.version = "v0".to_string();

    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("test-mode-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let record = run_store.run_json(&run_id);
    assert_eq!(record["test_mode"], serde_json::json!(true));
}

#[test]
fn start_recipe_defaults_test_mode_to_false_for_release_workflow() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe("release_mode_wf", vec![invoke_step("a")], None),
            Some("release-mode-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let record = run_store.run_json(&run_id);
    assert_eq!(record["test_mode"], serde_json::json!(false));
}

#[test]
fn automatic_preflight_failure_rejects_run_birth_without_dispatching_steps() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    preflight.plan_response(vec![test_preflight_check(
        "invoke",
        "adapter_handshake",
        PreflightStatus::Failed,
        "process plugin handshake failed",
    )]);
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_preflight(
        execution.clone(),
        preflight.clone(),
        run_store.clone(),
        events.clone(),
    );
    let recipe = recipe("preflight_demo", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let error = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("preflight-fail"))
        .expect_err("failed preflight must reject run birth");
    assert!(error.to_string().contains("adapter_handshake"));
    assert!(execution.invocations().is_empty());
    assert!(run_store.step_executions().is_empty());
    assert_eq!(preflight.invocations().len(), 1);
    assert!(!run_store.has_run("preflight-fail"));
    assert!(events.snapshot().is_empty());
}

#[test]
fn recursive_subflow_preflight_surfaces_nested_blocker_with_prefixed_step_id() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    preflight.plan_response(vec![test_preflight_check(
        "generate_plan",
        "input_schema",
        PreflightStatus::Failed,
        "input schema validation failed at instance path '/model' and schema path '/properties/model/type'",
    )]);
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_preflight(
        execution.clone(),
        preflight.clone(),
        run_store.clone(),
        events,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe_with_inputs(
        "child_preflight_nested",
        BTreeMap::from([(
            "model".to_string(),
            RecipeInputSpec {
                value_type: "string".to_string(),
                required: false,
                default: None,
                description: None,
            },
        )]),
        vec![invoke_external_step(
            "generate_plan",
            AdapterKind::Process,
            "openspec",
            "generate_plan",
            BTreeMap::from([(
                "model".to_string(),
                KernelValue::String("{{inputs.model}}".to_string()),
            )]),
        )],
        None,
    );
    let parent_recipe = recipe(
        "parent_preflight_nested",
        vec![subflow_step("plan_loop", child_recipe)],
        None,
    );

    let error = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("nested-preflight"),
            BTreeMap::new(),
        )
        .expect_err("nested blocker must reject run birth");
    assert!(error.to_string().contains("input_schema"));
    assert!(execution.invocations().is_empty());
    assert_eq!(preflight.invocations().len(), 1);
    assert!(!run_store.has_run("nested-preflight"));
}

#[test]
fn recursive_subflow_preflight_passes_when_child_preflight_passes() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    preflight.plan_response(vec![test_preflight_check(
        "generate_plan",
        "request_preparation",
        PreflightStatus::Passed,
        "execution request prepared",
    )]);
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_preflight(execution, preflight, run_store, events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe_with_inputs(
        "child_preflight_ok",
        BTreeMap::from([(
            "model".to_string(),
            RecipeInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: None,
            },
        )]),
        vec![invoke_external_step(
            "generate_plan",
            AdapterKind::Process,
            "openspec",
            "generate_plan",
            BTreeMap::from([(
                "model".to_string(),
                KernelValue::String("{{inputs.model}}".to_string()),
            )]),
        )],
        None,
    );
    let parent_recipe = recipe(
        "parent_preflight_ok",
        vec![subflow_step_with_with(
            "plan_loop",
            child_recipe,
            BTreeMap::from([(
                "model".to_string(),
                KernelValue::String("gpt-5.4".to_string()),
            )]),
        )],
        None,
    );

    let (_run_id, report) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("nested-preflight-pass"),
            BTreeMap::new(),
        )
        .expect("prepare");

    assert_eq!(report.status, PreflightStatus::Passed);
    assert_eq!(report.blocker_count, 0);
    assert_eq!(report.warning_count, 0);
    assert_eq!(report.checks.len(), 2);
    assert_eq!(report.checks[0].kind, "request_preparation");
    assert_eq!(report.checks[0].status, PreflightStatus::Passed);
    assert_eq!(
        report.checks[0].step_ids,
        vec!["plan_loop/generate_plan".to_string()]
    );
    assert_eq!(report.checks[1].kind, "request_preparation");
    assert_eq!(report.checks[1].status, PreflightStatus::Passed);
    assert_eq!(
        report.checks[1].step_ids,
        vec!["plan_loop/generate_plan".to_string()]
    );
}

#[test]
fn recursive_subflow_preflight_uses_root_workflow_policy_scope() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let policy = Arc::new(RecordingPolicy::default());
    let kernel = test_kernel_with_policy_control_preflight(
        execution,
        run_store,
        events,
        policy.clone(),
        Arc::new(NoopControlRequests),
        preflight.clone(),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tempfile::tempdir().expect("workspace");
    let home_dir = test_home_dir(&tmp);
    let child_recipe = recipe(
        "child_policy_scope",
        vec![invoke_step("nested_invoke")],
        None,
    );
    let mut parent_recipe = recipe(
        "root_policy_scope",
        vec![subflow_step("child", child_recipe)],
        None,
    );
    parent_recipe.workspace = Some(transist_protocol::WorkspaceBinding {
        path: workspace.path().display().to_string(),
        access: transist_protocol::WorkspaceAccessMode::Full,
    });

    kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("root-policy-scope-run"),
            BTreeMap::new(),
        )
        .expect("prepare");

    let descriptors = policy.captured_descriptors();
    assert!(!descriptors.is_empty());
    assert!(descriptors.iter().all(|descriptor| {
        let KernelValue::Object(fields) = descriptor else {
            return false;
        };
        fields.get("workflow_ref") == Some(&KernelValue::String("root_policy_scope@v1".to_string()))
    }));
    let expected_workspace = std::fs::canonicalize(workspace.path())
        .unwrap()
        .display()
        .to_string();
    assert!(
        preflight
            .invocations()
            .iter()
            .all(|invocation| invocation.request.paths.workspace_dir == expected_workspace)
    );

    kernel
        .run_started_run(tmp.path(), &home_dir, "root-policy-scope-run")
        .expect("execute prepared root and child");
    assert!(policy.captured_descriptors().iter().all(|descriptor| {
        let KernelValue::Object(fields) = descriptor else {
            return false;
        };
        fields.get("workflow_ref") == Some(&KernelValue::String("root_policy_scope@v1".to_string()))
    }));
    assert!(
        policy
            .captured_run_start_refs()
            .iter()
            .all(|workflow_ref| workflow_ref == "root_policy_scope@v1")
    );
}

#[test]
fn recursive_subflow_preflight_preserves_local_only_mode_for_policy_descriptor() {
    let local_execution = Arc::new(RecordingExecution::default());
    let local_run_store = Arc::new(MemoryRunStore::default());
    let local_events = Arc::new(RecordingEvents::default());
    let local_policy = Arc::new(RecordingPolicy::default());
    let local_kernel = test_kernel_with_policy_control_preflight_and_plugin_metadata(
        local_execution,
        local_run_store.clone(),
        local_events,
        local_policy.clone(),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
        Arc::new(LocalNetworkPluginMetadata),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let mut nested_invoke = invoke_step("nested_invoke");
    let StepExecutionDescriptor::Invoke { plugin_version, .. } = &mut nested_invoke.execution
    else {
        panic!("nested step must be an invocation");
    };
    *plugin_version = Some("1.0.0".to_string());
    let child_recipe = recipe("child_local_only_policy", vec![nested_invoke], None);
    let parent_recipe = recipe(
        "root_local_only_policy",
        vec![subflow_step("child", child_recipe.clone())],
        None,
    );

    let (_run_id, report) = local_kernel
        .prepare_recipe_with_trust(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("local-only-subflow-preflight"),
            BTreeMap::new(),
            false,
            None,
            true,
        )
        .expect("local-only root preparation succeeds");
    assert_eq!(report.status, PreflightStatus::Passed);
    assert!(
        !local_run_store.has_run("local-only-subflow-preflight__child"),
        "recursive preflight must not create a durable child run"
    );

    let local_descriptor = local_policy
        .captured_descriptors()
        .into_iter()
        .find(|descriptor| {
            serde_json::Value::from(descriptor.clone())
                .get("action")
                .and_then(serde_json::Value::as_str)
                == Some("echo")
        })
        .expect("nested invocation policy descriptor");
    let local_descriptor = serde_json::Value::from(local_descriptor);
    assert_eq!(
        local_descriptor.pointer("/workflow_ref"),
        Some(&serde_json::Value::String(
            "root_local_only_policy@v1".to_string()
        ))
    );
    assert_eq!(
        local_descriptor.pointer("/network_policy/mode"),
        Some(&serde_json::Value::String("loopback".to_string()))
    );
    assert!(
        local_descriptor["requested_capabilities"]
            .as_array()
            .expect("requested capabilities array")
            .iter()
            .all(|capability| capability["capability"] != "network"),
        "local-only nested invocation must not require network destinations"
    );

    let normal_execution = Arc::new(RecordingExecution::default());
    let normal_run_store = Arc::new(MemoryRunStore::default());
    let normal_events = Arc::new(RecordingEvents::default());
    let normal_policy = Arc::new(RecordingPolicy::default());
    let normal_kernel = test_kernel_with_policy_control_preflight_and_plugin_metadata(
        normal_execution,
        normal_run_store.clone(),
        normal_events,
        normal_policy.clone(),
        Arc::new(NoopControlRequests),
        Arc::new(RecordingPreflight::default()),
        Arc::new(LocalNetworkPluginMetadata),
    );
    let (_run_id, report) = normal_kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("normal-subflow-preflight"),
            BTreeMap::new(),
        )
        .expect("normal root preparation succeeds");
    assert_eq!(report.status, PreflightStatus::Passed);
    assert!(
        !normal_run_store.has_run("normal-subflow-preflight__child"),
        "normal recursive preflight must not create a durable child run"
    );

    let normal_descriptor = normal_policy
        .captured_descriptors()
        .into_iter()
        .find(|descriptor| {
            serde_json::Value::from(descriptor.clone())
                .get("action")
                .and_then(serde_json::Value::as_str)
                == Some("echo")
        })
        .map(serde_json::Value::from)
        .expect("normal nested invocation policy descriptor");
    assert_eq!(
        normal_descriptor.pointer("/network_policy/mode"),
        Some(&serde_json::Value::String("any".to_string()))
    );
    assert!(
        normal_descriptor["requested_capabilities"]
            .as_array()
            .expect("requested capabilities array")
            .iter()
            .any(|capability| capability["capability"] == "network"),
        "normal nested invocation retains network destination authority"
    );
}

#[test]
fn recursive_subflow_preflight_rejects_persisted_child_input_rebinding_drift() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        execution,
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let child_recipe = recipe_with_inputs(
        "child_input_authority",
        BTreeMap::from([(
            "repo_path".to_string(),
            RecipeInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: None,
            },
        )]),
        vec![invoke_step("nested_invoke")],
        None,
    );
    let parent_recipe = recipe(
        "root_input_authority",
        vec![subflow_step_with_with(
            "child",
            child_recipe.clone(),
            BTreeMap::from([(
                "repo_path".to_string(),
                KernelValue::String("/persisted/workspace".to_string()),
            )]),
        )],
        None,
    );
    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("root-input-authority-run"),
            BTreeMap::new(),
        )
        .expect("start parent");
    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &child_recipe,
            Some("root-input-authority-run__child"),
            BTreeMap::from([(
                "repo_path".to_string(),
                KernelValue::String("/different/workspace".to_string()),
            )]),
        )
        .expect("start child");
    let mut child = run_store
        .load_run("root-input-authority-run__child")
        .unwrap()
        .unwrap();
    child.lineage = Some(RunLineage {
        mode: RunLineageMode::Fresh,
        source_run_id: None,
        root_run_id: "root-input-authority-run".to_string(),
        parent_run_id: Some("root-input-authority-run".to_string()),
        parent_step_id: Some("child".to_string()),
    });
    run_store.inject_record(child);

    let report = kernel
        .preflight_run(tmp.path(), &home_dir, "root-input-authority-run")
        .expect("input drift is a durable preflight finding");

    assert_eq!(report.status, PreflightStatus::Failed);
    assert_eq!(report.blocker_count, 1);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.kind == "subflow_input_binding"
                && check.status == PreflightStatus::Failed
                && check.step_ids == ["child"]
                && check.message.contains("do not match inputs rebound"))
    );
    let persisted = run_store.run_json("root-input-authority-run");
    assert_eq!(persisted["status"], "created");
    assert_eq!(persisted["preflight"]["status"], "failed");
}

#[test]
fn recursive_subflow_preflight_allows_prompt_file_under_external_read_grant() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    preflight.plan_response(vec![test_preflight_check(
        "generate_plan",
        "request_preparation",
        PreflightStatus::Passed,
        "execution request prepared",
    )]);
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_preflight(execution, preflight, run_store, events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let child_source = tmp.path().join("child-source");
    let home_dir = test_home_dir(&tmp);
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(child_source.join("prompts")).expect("prompt directory");
    std::fs::write(child_source.join("prompts/task.md"), "nested prompt").expect("prompt file");

    let mut child_step = invoke_external_step(
        "generate_plan",
        AdapterKind::Process,
        "openspec",
        "generate_plan",
        BTreeMap::new(),
    );
    child_step.prompt_binding = Some(transist_protocol::RecipePromptBinding {
        inline_field: "prompt".to_string(),
        file_field: Some("prompt_file".to_string()),
        source: transist_protocol::RecipePromptSource::File {
            path: "prompts/task.md".to_string(),
        },
    });
    child_step.filesystem_policy = Some(transist_protocol::FilesystemPolicy {
        workspace_access: None,
        read_allow_paths: vec![child_source.display().to_string()],
        write_allow_paths: Vec::new(),
    });
    let mut child_recipe = recipe("child_external_prompt", vec![child_step], None);
    child_recipe.source_dir = Some(child_source.display().to_string());
    let parent_recipe = recipe(
        "parent_external_prompt",
        vec![subflow_step("plan_loop", child_recipe)],
        None,
    );

    let (_run_id, report) = kernel
        .prepare_recipe(
            &workspace,
            &home_dir,
            &parent_recipe,
            Some("nested-external-prompt"),
            BTreeMap::new(),
        )
        .expect("prepare");

    assert_eq!(report.status, PreflightStatus::Passed);
    assert_eq!(report.blocker_count, 0);
}

#[test]
fn recursive_subflow_preflight_surfaces_binding_failures_on_parent_step() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel =
        test_kernel_with_preflight(execution.clone(), preflight.clone(), run_store, events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe_with_inputs(
        "child_binding_failure",
        BTreeMap::from([(
            "model".to_string(),
            RecipeInputSpec {
                value_type: "string".to_string(),
                required: true,
                default: None,
                description: None,
            },
        )]),
        vec![invoke_external_step(
            "generate_plan",
            AdapterKind::Process,
            "openspec",
            "generate_plan",
            BTreeMap::from([(
                "model".to_string(),
                KernelValue::String("{{inputs.model}}".to_string()),
            )]),
        )],
        None,
    );
    let parent_recipe = recipe(
        "parent_binding_failure",
        vec![subflow_step("plan_loop", child_recipe)],
        None,
    );

    let error = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("nested-binding-failure"),
            BTreeMap::new(),
        )
        .expect_err("binding failure must reject run birth");
    assert!(error.to_string().contains("subflow_input_binding"));
    assert!(
        error
            .to_string()
            .contains("missing required workflow input 'model'")
    );
    assert!(execution.invocations().is_empty());
    assert!(preflight.invocations().is_empty());
}

#[test]
fn recursive_subflow_preflight_warns_on_cycle() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel =
        test_kernel_with_preflight(execution.clone(), preflight.clone(), run_store, events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let parent_recipe = recipe(
        "cycle_demo",
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        None,
    );
    let cyclic_child = recipe(
        "cycle_demo",
        vec![subflow_step("loop_again", parent_recipe.clone())],
        None,
    );
    let parent_with_cycle = recipe(
        "cycle_demo",
        vec![subflow_step("plan_loop", cyclic_child)],
        None,
    );

    let (_run_id, report) = kernel
        .prepare_recipe(
            tmp.path(),
            &home_dir,
            &parent_with_cycle,
            Some("nested-cycle"),
            BTreeMap::new(),
        )
        .expect("prepare");

    assert_eq!(report.status, PreflightStatus::Warning);
    assert!(execution.invocations().is_empty());
    assert!(preflight.invocations().is_empty());
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].kind, "subflow_preflight.cycle_detected");
    assert_eq!(report.checks[0].status, PreflightStatus::Warning);
    assert_eq!(report.checks[0].step_ids, vec!["plan_loop/".to_string()]);
    assert!(
        report.checks[0]
            .message
            .contains("would recurse cyclically")
    );
}

#[test]
fn manual_preflight_persists_latest_report_without_mutating_run_status() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    preflight.plan_response(Vec::new());
    preflight.plan_response(vec![test_preflight_check(
        "invoke",
        "adapter_health",
        PreflightStatus::Warning,
        "health endpoint is not available",
    )]);
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_preflight(
        execution.clone(),
        preflight.clone(),
        run_store.clone(),
        events.clone(),
    );
    let recipe = recipe("preflight_manual_demo", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("manual-preflight"),
            BTreeMap::new(),
        )
        .expect("start");

    let report = kernel
        .preflight_run(tmp.path(), &home_dir, &run_id)
        .expect("preflight");
    assert_eq!(report.status, PreflightStatus::Warning);
    assert_eq!(report.warning_count, 1);
    assert!(execution.invocations().is_empty());
    assert_eq!(preflight.invocations().len(), 2);

    let run = run_store.run_json(&run_id);
    assert_eq!(run["status"], "created");
    assert_eq!(run["preflight"]["status"], "warning");
    assert!(
        events
            .snapshot()
            .iter()
            .all(|event| event.kind != "run_preflight_completed")
    );
}

#[test]
fn rejected_birth_can_be_retried_after_environment_is_fixed() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    preflight.plan_response(vec![test_preflight_check(
        "invoke",
        "adapter_handshake",
        PreflightStatus::Failed,
        "process plugin handshake failed",
    )]);
    preflight.plan_response(Vec::new());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_preflight(
        execution.clone(),
        preflight.clone(),
        run_store.clone(),
        events,
    );
    let recipe = recipe("preflight_resume_demo", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let first_error = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("resume-preflight"))
        .expect_err("first readiness check must reject birth");
    assert!(first_error.to_string().contains("adapter_handshake"));
    assert!(execution.invocations().is_empty());
    assert!(!run_store.has_run("resume-preflight"));

    let retried = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("resume-preflight"))
        .expect("retry launch");
    assert_eq!(retried.status, KernelRunStatus::Success);
    assert_eq!(execution.invocations().len(), 1);
    assert_eq!(preflight.invocations().len(), 2);

    let run = run_store.run_json("resume-preflight");
    assert_eq!(run["status"], "success");
    assert_eq!(run["resume_count"], 0);
    assert_eq!(run["preflight"]["status"], "passed");
}

#[test]
fn missing_secret_reference_fails_during_preflight_before_dispatch() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe(
        "missing_secret_demo",
        vec![invoke_step_with(
            "invoke",
            BTreeMap::from([
                (
                    "args".to_string(),
                    KernelValue::Array(vec![KernelValue::String("hello".to_string())]),
                ),
                (
                    "secret_refs".to_string(),
                    KernelValue::Array(vec![KernelValue::String("MISSING_TOKEN".to_string())]),
                ),
            ]),
        )],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let error = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("missing-secret-run"))
        .expect_err("missing secret must reject run birth");
    assert!(error.to_string().contains("secret_resolution"));
    assert!(execution.invocations().is_empty());
    assert!(!run_store.has_run("missing-secret-run"));
}

#[test]
fn http_secret_delivery_is_blocked_by_preflight_policy_before_dispatch() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_preflight(
        execution.clone(),
        preflight.clone(),
        run_store.clone(),
        events,
    );
    let recipe = recipe(
        "http_secret_demo",
        vec![invoke_external_step(
            "invoke",
            AdapterKind::Http,
            "http.echo",
            "echo",
            BTreeMap::from([
                (
                    "message".to_string(),
                    KernelValue::String("hello".to_string()),
                ),
                (
                    "secret_refs".to_string(),
                    KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
                ),
            ]),
        )],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let error = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("http-secret-run"))
        .expect_err("denied HTTP secret delivery must reject run birth");
    assert!(error.to_string().contains("remote_secret_policy"));
    assert!(execution.invocations().is_empty());
    assert!(preflight.invocations().is_empty());
    assert!(!run_store.has_run("http-secret-run"));
}

#[test]
fn process_secret_delivery_is_blocked_by_preflight_policy_before_dispatch() {
    let execution = Arc::new(RecordingExecution::default());
    let preflight = Arc::new(RecordingPreflight::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = RuntimeKernel::new(RuntimeServices::new(RuntimeServiceParts {
        execution: execution.clone(),
        artifacts: Arc::new(MemoryArtifacts),
        run_store: run_store.clone(),
        lease: Arc::new(NoopLease::default()),
        control: Arc::new(NoopControlRequests),
        clock: Arc::new(FixedClock),
        ids: Arc::new(FixedIds),
        events,
        policy: Arc::new(FixedRemoteSecretPolicy {
            remote_secret_policy: RemoteSecretPolicy {
                process: false,
                ..RemoteSecretPolicy::default()
            },
        }),
        preflight: preflight.clone(),
        secrets: Arc::new(StaticSecrets),
        plugin_verifier: Arc::new(AllowPluginVerification),
        plugin_metadata: Arc::new(TestPluginMetadata),
        prompt_files: Arc::new(TestPromptFiles),
        home_settings: Arc::new(DefaultHomeSettings),
        telemetry: Arc::new(NoopTelemetry::default()),
    }));
    let recipe = recipe(
        "process_secret_demo",
        vec![invoke_step_with(
            "invoke",
            BTreeMap::from([(
                "secret_refs".to_string(),
                KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
            )]),
        )],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let error = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("process-secret-run"))
        .expect_err("denied process secret delivery must reject run birth");
    assert!(error.to_string().contains("remote_secret_policy"));
    assert!(execution.invocations().is_empty());
    assert!(preflight.invocations().is_empty());
    assert!(!run_store.has_run("process-secret-run"));
}

#[test]
fn successful_subflow_is_rerun_when_loop_resets_parent_step() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe("child_loop_body", vec![invoke_step("nested")], None);
    let loop_meta = BTreeMap::from([(
        "__transist_loop".to_string(),
        serde_json::json!({
            "body_steps": ["review_child"],
            "max_iters": 3
        })
        .into(),
    )]);
    let parent_recipe = recipe(
        "parent_loop",
        vec![
            control_step("loop", "control.noop", loop_meta),
            subflow_step("review_child", child_recipe.clone()),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "loop".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "review_child".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions < 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "finish".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions >= 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "review_child".to_string(),
                    to: "loop".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: Some(RecipeLoop {
                        while_expr: None,
                        until_expr: None,
                        max_iters: None,
                    }),
                },
            ],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &parent_recipe, Some("loop-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let invocations = execution
        .invocations()
        .into_iter()
        .map(|invocation| {
            (
                invocation.request.run_id,
                invocation.request.step.id,
                invocation.request.step.execution,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        invocations,
        vec![
            (
                "loop-run__review_child".to_string(),
                "nested".to_string(),
                1
            ),
            (
                "loop-run__review_child".to_string(),
                "nested".to_string(),
                2
            ),
        ]
    );

    let child_run = run_store.run_json("loop-run__review_child");
    assert_eq!(child_run["status"], "success");
    assert_eq!(child_run["steps"]["nested"]["executions"], 2);
}

#[test]
fn successful_looping_subflow_is_rerun_from_scratch_when_parent_loop_resets_step() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = looping_child_recipe("child_loop_body_nested");
    let loop_meta = BTreeMap::from([(
        "__transist_loop".to_string(),
        serde_json::json!({
            "body_steps": ["review_child"],
            "max_iters": 3
        })
        .into(),
    )]);
    let parent_recipe = recipe(
        "parent_loop_nested",
        vec![
            control_step("loop", "control.noop", loop_meta),
            subflow_step("review_child", child_recipe),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "loop".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "review_child".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions < 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "finish".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions >= 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "review_child".to_string(),
                    to: "loop".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: Some(RecipeLoop {
                        while_expr: None,
                        until_expr: None,
                        max_iters: None,
                    }),
                },
            ],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("loop-nested-run"),
        )
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let invocations = execution
        .invocations()
        .into_iter()
        .map(|invocation| {
            (
                invocation.request.run_id,
                invocation.request.step.id,
                invocation.request.step.execution,
            )
        })
        .collect::<Vec<_>>();
    // The child subflow uses default `rerun_mode: reuse`, so it re-enters the
    // same child run id. The child's internal loop restarts from scratch on
    // each parent re-dispatch because the activation baseline makes loop
    // `max_iters` activation-local. The lifetime `nested` execution counter is
    // append-only, so the second activation dispatches at execution 2.
    assert_eq!(
        invocations,
        vec![
            (
                "loop-nested-run__review_child".to_string(),
                "nested".to_string(),
                1
            ),
            (
                "loop-nested-run__review_child".to_string(),
                "nested".to_string(),
                2
            ),
        ]
    );

    let child_run = run_store.run_json("loop-nested-run__review_child");
    assert_eq!(child_run["status"], "success");
    assert_eq!(child_run["steps"]["loop"]["executions"], 4);
    assert_eq!(child_run["steps"]["nested"]["executions"], 2);
}

#[test]
fn rerunning_started_run_resets_successful_nested_subflows_from_scratch() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let review_changes_recipe = recipe(
        "review_changes",
        vec![
            invoke_step("nested"),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "nested".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "nested".to_string(),
                to: "finish".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );
    let review_cycle_recipe = recipe(
        "review_cycle",
        vec![
            subflow_step("review_changes_flow", review_changes_recipe),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "review_changes_flow".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "review_changes_flow".to_string(),
                to: "finish".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let loop_meta = BTreeMap::from([(
        "__transist_loop".to_string(),
        serde_json::json!({
            "body_steps": ["review_cycle"],
            "max_iters": 3
        })
        .into(),
    )]);
    let parent_recipe = recipe(
        "parent_review_loop",
        vec![
            control_step("loop", "control.noop", loop_meta),
            subflow_step("review_cycle", review_cycle_recipe),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "loop".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "review_cycle".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions < 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "finish".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions >= 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "review_cycle".to_string(),
                    to: "loop".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: Some(RecipeLoop {
                        while_expr: None,
                        until_expr: None,
                        max_iters: None,
                    }),
                },
            ],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("parent-review-loop-run"),
        )
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let invocations = execution
        .invocations()
        .into_iter()
        .map(|invocation| {
            (
                invocation.request.run_id,
                invocation.request.step.id,
                invocation.request.step.execution,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        invocations,
        vec![
            (
                "parent-review-loop-run__review_cycle__review_changes_flow".to_string(),
                "nested".to_string(),
                1,
            ),
            (
                "parent-review-loop-run__review_cycle__review_changes_flow".to_string(),
                "nested".to_string(),
                2,
            ),
        ]
    );

    let child_run = run_store.run_json("parent-review-loop-run__review_cycle__review_changes_flow");
    assert_eq!(child_run["status"], "success");
    assert_eq!(child_run["steps"]["nested"]["executions"], 2);
}

#[test]
fn subflow_rerun_mode_fresh_creates_a_new_child_run_per_loop_iteration() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let review_changes_recipe = recipe(
        "review_changes",
        vec![
            invoke_step("nested"),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "nested".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "nested".to_string(),
                to: "finish".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );
    let review_cycle_recipe = recipe(
        "review_cycle",
        vec![
            subflow_step("review_changes_flow", review_changes_recipe),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "review_changes_flow".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "review_changes_flow".to_string(),
                to: "finish".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let loop_meta = BTreeMap::from([(
        "__transist_loop".to_string(),
        serde_json::json!({
            "body_steps": ["review_cycle"],
            "max_iters": 3
        })
        .into(),
    )]);
    let subflow_meta = BTreeMap::from([(
        "__transist_subflow".to_string(),
        serde_json::json!({
            "rerun_mode": "fresh"
        })
        .into(),
    )]);
    let parent_recipe = recipe(
        "parent_review_loop_fresh_subflows",
        vec![
            control_step("loop", "control.noop", loop_meta),
            subflow_step_with_with("review_cycle", review_cycle_recipe, subflow_meta),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "loop".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "review_cycle".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions < 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "loop".to_string(),
                    to: "finish".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("executions >= 3".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "review_cycle".to_string(),
                    to: "loop".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: Some(RecipeLoop {
                        while_expr: None,
                        until_expr: None,
                        max_iters: None,
                    }),
                },
            ],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("parent-review-loop-fresh-run"),
        )
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let invocations = execution
        .invocations()
        .into_iter()
        .map(|invocation| {
            (
                invocation.request.run_id,
                invocation.request.step.id,
                invocation.request.step.execution,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        invocations,
        vec![
            (
                "parent-review-loop-fresh-run__review_cycle__review_changes_flow".to_string(),
                "nested".to_string(),
                1,
            ),
            (
                "parent-review-loop-fresh-run__review_cycle__r002__review_changes_flow".to_string(),
                "nested".to_string(),
                1,
            ),
        ]
    );

    assert_eq!(
        run_store.run_json("parent-review-loop-fresh-run__review_cycle")["status"],
        "success"
    );
    assert_eq!(
        run_store.run_json("parent-review-loop-fresh-run__review_cycle__r002")["status"],
        "success"
    );
}

#[test]
fn join_step_runs_after_taken_branch_when_other_branch_is_guard_blocked() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store, events);
    let recipe = recipe(
        "branch_join_demo",
        vec![
            control_step("route", "control.noop", BTreeMap::new()),
            invoke_step("then_step"),
            invoke_step("else_step"),
            invoke_step("finish_step"),
        ],
        Some(RecipeGraph {
            entry: "route".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "route".to_string(),
                    to: "then_step".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("true".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "route".to_string(),
                    to: "else_step".to_string(),
                    event: "on_success".to_string(),
                    guard: Some("false".to_string()),
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "then_step".to_string(),
                    to: "finish_step".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "else_step".to_string(),
                    to: "finish_step".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: None,
                },
            ],
            max_iterations: None,
        }),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("branch-join-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let invocations: Vec<(String, u32)> = execution
        .invocations()
        .into_iter()
        .map(|invocation| {
            (
                invocation.request.step.id,
                invocation.request.step.execution,
            )
        })
        .collect();
    assert_eq!(
        invocations,
        vec![("then_step".to_string(), 1), ("finish_step".to_string(), 1),]
    );
}

#[test]
fn resume_recovers_failed_looping_run_that_has_no_failed_steps() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let recipe = looping_child_recipe("stuck_loop_child");
    run_store.inject_recipe_snapshot("stuck-loop-run", &recipe);

    let mut snapshot = KernelRunSnapshot::new(
        "stuck-loop-run",
        "stuck_loop_child",
        "v1",
        &[
            "loop".to_string(),
            "nested".to_string(),
            "finish".to_string(),
        ],
    );
    snapshot.status = KernelRunStatus::Failed;
    if let Some(loop_step) = snapshot.steps.get_mut("loop") {
        loop_step.status = transist_core::KernelStepStatus::Runnable;
        loop_step.executions = 2;
    }
    if let Some(nested_step) = snapshot.steps.get_mut("nested") {
        nested_step.status = transist_core::KernelStepStatus::Pending;
        nested_step.executions = 1;
    }
    if let Some(finish_step) = snapshot.steps.get_mut("finish") {
        finish_step.status = transist_core::KernelStepStatus::Pending;
        finish_step.executions = 1;
    }
    let mut record = PersistedRunRecord::from_snapshot(
        snapshot,
        Some(RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: recipe_snapshot_sha(&recipe),
        }),
        None,
        BTreeMap::new(),
        0,
        1,
        1,
    );
    record.asset_lock_version = 1;
    seed_run_locator(&home_dir, "stuck-loop-run");
    run_store.inject_record(record);

    let summary = kernel
        .resume_run(tmp.path(), &home_dir, "stuck-loop-run")
        .expect("resume");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let invocations = execution
        .invocations()
        .into_iter()
        .map(|invocation| {
            (
                invocation.request.run_id,
                invocation.request.step.id,
                invocation.request.step.execution,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        invocations,
        vec![("stuck-loop-run".to_string(), "nested".to_string(), 2)]
    );
}

#[test]
fn recovering_parent_step_reuses_successful_child_subflow() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe("child_recovery", vec![invoke_step("nested")], None);
    let child_run_id = "parent-run__child";
    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &child_recipe,
            Some(child_run_id),
            BTreeMap::new(),
        )
        .expect("start child");
    let child_summary = kernel
        .run_started_run(tmp.path(), &home_dir, child_run_id)
        .expect("run child");
    assert_eq!(child_summary.status, KernelRunStatus::Success);
    execution.clear_invocations();

    let parent_recipe = recipe(
        "parent_recovery",
        vec![
            subflow_step("child", child_recipe),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "child".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "child".to_string(),
                to: "finish".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );
    run_store.inject_recipe_snapshot("parent-run", &parent_recipe);

    let mut snapshot = KernelRunSnapshot::new(
        "parent-run",
        "parent_recovery",
        "v1",
        &["child".to_string(), "finish".to_string()],
    );
    snapshot.status = KernelRunStatus::Running;
    if let Some(child_step) = snapshot.steps.get_mut("child") {
        child_step.status = transist_core::KernelStepStatus::Runnable;
        child_step.executions = 1;
        child_step.started_at_ms = Some(1);
    }
    let mut parent_record = PersistedRunRecord::from_snapshot(
        snapshot,
        Some(RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: recipe_snapshot_sha(&parent_recipe),
        }),
        None,
        BTreeMap::new(),
        0,
        1,
        1,
    );
    parent_record.asset_lock_version = 1;
    seed_run_locator(&home_dir, "parent-run");
    run_store.inject_record(parent_record);

    let summary = kernel
        .run_started_run(tmp.path(), &home_dir, "parent-run")
        .expect("recover parent");
    assert_eq!(summary.status, KernelRunStatus::Success);
    assert!(execution.invocations().is_empty());

    let child_run = run_store.run_json(child_run_id);
    assert_eq!(child_run["steps"]["nested"]["executions"], 1);
}

#[test]
fn subflow_step_output_exposes_child_step_outputs() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe("child_visibility", vec![invoke_step("nested")], None);
    let parent_recipe = recipe(
        "parent_visibility",
        vec![
            subflow_step("child", child_recipe),
            control_step("finish", "control.finish", BTreeMap::new()),
        ],
        Some(RecipeGraph {
            entry: "child".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "child".to_string(),
                to: "finish".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("parent-visibility"),
        )
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let run = run_store.run_json("parent-visibility");
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["child_steps"]["nested"]["status"],
        "succeeded"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["child_steps"]["nested"]["executions"],
        1
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["child_steps"]["nested"]["out"]["ok"],
        true
    );
}

#[test]
fn failed_subflow_step_output_persists_compact_child_failure() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut nested_failure = failed_result(
        "parent-failure__child",
        "nested",
        1,
        "stderr first\nstderr second",
    );
    nested_failure.stderr = "stderr first\nstderr second".to_string();
    if let Some(error) = nested_failure.error.as_mut() {
        error.message = "error first\nerror second".to_string();
    }
    execution.plan_result("parent-failure__child", "nested", 1, nested_failure);

    let child_recipe = recipe("child_failure", vec![invoke_step("nested")], None);
    let parent_recipe = recipe(
        "parent_failure",
        vec![subflow_step("child", child_recipe)],
        None,
    );

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("parent-failure"),
        )
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let run = run_store.run_json("parent-failure");
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["child_run_id"],
        "parent-failure__child"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["failed_step_id"],
        "nested"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["error_code"],
        "tool_execution_error"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["error_message"],
        "error first"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["stderr"],
        "stderr first"
    );
}

#[test]
fn failed_nested_subflow_step_output_propagates_deepest_child_failure() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut leaf_failure = failed_result(
        "root-failure__child__grand",
        "leaf",
        1,
        "deep stderr\nextra",
    );
    if let Some(error) = leaf_failure.error.as_mut() {
        error.message = "deep error\nextra".to_string();
    }
    execution.plan_result("root-failure__child__grand", "leaf", 1, leaf_failure);

    let grand_recipe = recipe("grand_failure", vec![invoke_step("leaf")], None);
    let child_recipe = recipe(
        "child_failure",
        vec![subflow_step("grand", grand_recipe)],
        None,
    );
    let root_recipe = recipe(
        "root_failure",
        vec![subflow_step("child", child_recipe)],
        None,
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &root_recipe, Some("root-failure"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let run = run_store.run_json("root-failure");
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["child_run_id"],
        "root-failure__child__grand"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["failed_step_id"],
        "leaf"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["error_message"],
        "deep error"
    );
    assert_eq!(
        run["steps"]["child"]["last_result"]["output"]["failure"]["stderr"],
        "deep stderr"
    );
}

#[test]
fn successful_subflow_step_output_omits_failure_metadata() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let child_recipe = recipe("child_success", vec![invoke_step("nested")], None);
    let parent_recipe = recipe(
        "parent_success",
        vec![subflow_step("child", child_recipe)],
        None,
    );

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &parent_recipe,
            Some("parent-success"),
        )
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let run = run_store.run_json("parent-success");
    let output = run["steps"]["child"]["last_result"]["output"]
        .as_object()
        .expect("subflow output object");
    assert!(!output.contains_key("failure"));
}

// ── workflow vars (set block) ─────────────────────────────────────────────────

/// A step with `vars: {"count": 42}` should write `workflow_vars.count = 42`
/// into the persisted run record after it succeeds.
#[test]
fn set_block_writes_literal_value_to_workflow_vars() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut step = invoke_step("collect");
    step.vars = BTreeMap::from([("count".to_string(), KernelValue::Number(42.into()))]);

    let r = recipe("vars-literal", vec![step], None);
    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-literal-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let run = run_store.run_json("vars-literal-run");
    assert_eq!(run["workflow_vars"]["count"], serde_json::json!(42));
}

/// A failed step must NOT write into `workflow_vars`.
#[test]
fn set_block_does_not_write_on_step_failure() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut step = invoke_step("collect");
    step.vars = BTreeMap::from([("count".to_string(), KernelValue::Number(1.into()))]);

    execution.plan_result(
        "vars-fail-run",
        "collect",
        1,
        failed_result("vars-fail-run", "collect", 1, "oops"),
    );

    let r = recipe("vars-fail", vec![step], None);
    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-fail-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Failed);

    let run = run_store.run_json("vars-fail-run");
    // workflow_vars must be absent or empty — never written on failure.
    let vars = &run["workflow_vars"];
    assert!(
        vars.is_null() || vars == &serde_json::json!({}),
        "expected empty workflow_vars, got: {}",
        vars
    );
}

/// A step's `vars` that reference a prior step's output via
/// `{{steps.<id>.out.<key>}}` should resolve correctly at runtime.
#[test]
fn set_block_resolves_prior_step_output_placeholder() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    // step_a returns output {"value": "hello"}.
    // step_b sets var "captured" = "{{steps.step_a.out.value}}"
    execution.plan_result(
        "vars-step-out-run",
        "step_a",
        1,
        ExecutionResult {
            output: serde_json::json!({"value": "hello"}),
            ..success_result_for("vars-step-out-run", "step_a", 1)
        },
    );

    let mut step_b = invoke_step("step_b");
    step_b.vars = BTreeMap::from([(
        "captured".to_string(),
        KernelValue::String("{{steps.step_a.out.value}}".to_string()),
    )]);

    let r = recipe(
        "vars-step-out",
        vec![invoke_step("step_a"), step_b],
        Some(RecipeGraph {
            entry: "step_a".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "step_a".to_string(),
                to: "step_b".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-step-out-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let run = run_store.run_json("vars-step-out-run");
    assert_eq!(run["workflow_vars"]["captured"], serde_json::json!("hello"));
}

/// `set` should preserve booleans, objects, and arrays — not just strings.
/// Type fidelity matters because downstream `{{vars.k}}` exact-match
/// placeholders carry the value verbatim.
#[test]
fn set_block_preserves_bool_object_and_array_types() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut step = invoke_step("collect");
    step.vars = BTreeMap::from([
        ("flag".to_string(), KernelValue::Bool(true)),
        (
            "tags".to_string(),
            KernelValue::Array(vec![
                KernelValue::String("alpha".to_string()),
                KernelValue::String("beta".to_string()),
            ]),
        ),
        (
            "meta".to_string(),
            KernelValue::Object(BTreeMap::from([
                ("k".to_string(), KernelValue::String("v".to_string())),
                ("n".to_string(), KernelValue::Number(7.into())),
            ])),
        ),
    ]);

    let r = recipe("vars-types", vec![step], None);
    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-types-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let run = run_store.run_json("vars-types-run");
    assert_eq!(run["workflow_vars"]["flag"], serde_json::json!(true));
    assert_eq!(
        run["workflow_vars"]["tags"],
        serde_json::json!(["alpha", "beta"])
    );
    assert_eq!(
        run["workflow_vars"]["meta"],
        serde_json::json!({"k": "v", "n": 7})
    );
}

/// When two consecutive steps both write the same var key, the second
/// write must overwrite the first ("last write wins" semantics).
#[test]
fn set_block_last_write_wins_across_linear_steps() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut first = invoke_step("first");
    first.vars = BTreeMap::from([(
        "shared".to_string(),
        KernelValue::String("from_first".to_string()),
    )]);

    let mut second = invoke_step("second");
    second.vars = BTreeMap::from([(
        "shared".to_string(),
        KernelValue::String("from_second".to_string()),
    )]);

    let r = recipe(
        "vars-lww",
        vec![first, second],
        Some(RecipeGraph {
            entry: "first".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "first".to_string(),
                to: "second".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-lww-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let run = run_store.run_json("vars-lww-run");
    assert_eq!(
        run["workflow_vars"]["shared"],
        serde_json::json!("from_second")
    );
}

/// A subsequent step's `with` referencing `{{vars.k}}` (exact-match)
/// should receive the typed `KernelValue` from `workflow_vars`.
#[test]
fn vars_placeholder_in_with_resolves_with_type_preservation() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut producer = invoke_step("producer");
    producer.vars = BTreeMap::from([
        ("count".to_string(), KernelValue::Number(42.into())),
        ("ready".to_string(), KernelValue::Bool(true)),
    ]);

    let consumer = invoke_step_with(
        "consumer",
        BTreeMap::from([
            (
                "n".to_string(),
                KernelValue::String("{{vars.count}}".to_string()),
            ),
            (
                "r".to_string(),
                KernelValue::String("{{vars.ready}}".to_string()),
            ),
        ]),
    );

    let r = recipe(
        "vars-with",
        vec![producer, consumer],
        Some(RecipeGraph {
            entry: "producer".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "producer".to_string(),
                to: "consumer".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-with-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    // Inspect the consumer's invocation input: numbers/bools must be
    // preserved as JSON number/bool, not stringified.
    let invocations = execution.invocations();
    let consumer_call = invocations
        .iter()
        .find(|inv| inv.request.step.id == "consumer")
        .expect("consumer invocation");
    assert_eq!(consumer_call.request.input["n"], serde_json::json!(42));
    assert_eq!(consumer_call.request.input["r"], serde_json::json!(true));
}

/// A numeric workflow input plumbed through an exact
/// `{{inputs.x}}` placeholder *inside an array* (e.g. `args:`) must arrive
/// at the plugin as a JSON string, not a number. Plugin input schemas like
/// `args: string[]` reject numbers at preflight; preserving the source
/// `KernelValue::Number` here used to surface as a confusing
/// path-only schema validation error mid-loop.
///
/// Top-level type preservation for scalar `with:` values is covered by
/// `vars_placeholder_in_with_resolves_with_type_preservation`; the two
/// together pin down the array-vs-top-level coercion contract.
#[test]
fn array_placeholder_resolves_numeric_input_as_string() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut producer = invoke_step("producer");
    producer.vars = BTreeMap::from([("min_count".to_string(), KernelValue::Number(7.into()))]);

    // Consumer mimics a shell.exec invocation: `args` is an array whose
    // single element is the exact placeholder. This is the failing shape
    // that previously failed schema validation.
    let consumer = invoke_step_with(
        "consumer",
        BTreeMap::from([(
            "args".to_string(),
            KernelValue::Array(vec![KernelValue::String("{{vars.min_count}}".to_string())]),
        )]),
    );

    let r = recipe(
        "args-stringify",
        vec![producer, consumer],
        Some(RecipeGraph {
            entry: "producer".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "producer".to_string(),
                to: "consumer".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("args-stringify-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let invocations = execution.invocations();
    let consumer_call = invocations
        .iter()
        .find(|inv| inv.request.step.id == "consumer")
        .expect("consumer invocation");
    assert_eq!(
        consumer_call.request.input["args"],
        serde_json::json!(["7"]),
        "array element from exact placeholder must be stringified for plugin schemas requiring string[]"
    );
}

/// A cleanup step's `with` should be able to read `{{vars.*}}` written
/// by main steps. This verifies the cleanup dispatch path threads
/// `workflow_vars` into the resolution context.
#[test]
fn cleanup_step_resolves_vars_placeholder() {
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let mut producer = invoke_step("producer");
    producer.vars = BTreeMap::from([(
        "label".to_string(),
        KernelValue::String("teardown-me".to_string()),
    )]);

    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Success, CleanupTrigger::Failed],
        step: invoke_external_step(
            "teardown",
            AdapterKind::Process,
            "shell",
            "echo",
            BTreeMap::from([(
                "tag".to_string(),
                KernelValue::String("{{vars.label}}".to_string()),
            )]),
        ),
    };

    let mut r = recipe("vars-cleanup", vec![producer], None);
    r.cleanup = vec![cleanup_step];

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-cleanup-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let cleanup_call = execution
        .invocations()
        .into_iter()
        .find(|inv| inv.request.step.id == "teardown")
        .expect("cleanup invocation");
    assert_eq!(
        cleanup_call.request.input["tag"],
        serde_json::json!("teardown-me")
    );
}

/// Cleanup execution uses the same transient-path redaction boundary as main
/// execution, so adapter output cannot persist the resolved runtime temp root.
#[test]
fn cleanup_step_redacts_transient_runtime_paths() {
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let kernel = test_kernel_with_artifacts(
        execution.clone(),
        run_store.clone(),
        events,
        Arc::new(RunOwnedArtifacts {
            home_dir: home_dir.clone(),
        }),
    );
    let run_id = "cleanup-redaction-run";

    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Success],
        step: invoke_step("teardown"),
    };
    let mut recipe = recipe(
        "cleanup-redaction",
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        None,
    );
    recipe.cleanup = vec![cleanup_step];
    let mut cleanup_result = success_result_for(run_id, "teardown", 1);
    cleanup_result.stdout = format!("temporary root: {TEST_TRANSIENT_RUNTIME_PATH}");
    cleanup_result.output = serde_json::json!({
        "temporary_root": TEST_TRANSIENT_RUNTIME_PATH,
    });
    execution.plan_result(run_id, "teardown", 1, cleanup_result);

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some(run_id))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let persisted = serde_json::to_string(&run_store.run_json(run_id)).expect("serialize run");
    assert!(
        !persisted.contains(TEST_TRANSIENT_RUNTIME_PATH),
        "cleanup result persisted the runtime temporary path: {persisted}"
    );
    assert!(persisted.contains("[REDACTED]"), "{persisted}");
}

/// `workflow_vars` written before an interrupt must be preserved when the
/// kernel resumes the same run from the persisted record. We simulate
/// this by injecting a pre-populated `PersistedRunRecord` and executing.
#[test]
fn workflow_vars_persist_through_resume() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    // First run: producer writes a var, consumer reads it.
    let mut producer = invoke_step("producer");
    producer.vars = BTreeMap::from([(
        "phase".to_string(),
        KernelValue::String("initialized".to_string()),
    )]);

    let consumer = invoke_step_with(
        "consumer",
        BTreeMap::from([(
            "p".to_string(),
            KernelValue::String("{{vars.phase}}".to_string()),
        )]),
    );

    let r = recipe(
        "vars-resume",
        vec![producer, consumer],
        Some(RecipeGraph {
            entry: "producer".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "producer".to_string(),
                to: "consumer".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: None,
            }],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-resume-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    // The persisted record must hold the var, and the consumer's invocation
    // must have resolved {{vars.phase}} correctly — proving vars round-trip
    // through the persistence layer that resume relies on.
    let run = run_store.run_json("vars-resume-run");
    assert_eq!(
        run["workflow_vars"]["phase"],
        serde_json::json!("initialized")
    );

    let consumer_call = execution
        .invocations()
        .into_iter()
        .find(|inv| inv.request.step.id == "consumer")
        .expect("consumer invocation");
    assert_eq!(
        consumer_call.request.input["p"],
        serde_json::json!("initialized")
    );
}

// ── policy port tests ─────────────────────────────────────────────────────────

#[test]
fn pending_cancel_after_birth_is_honored_before_execution_preflight() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let preflight = Arc::new(RecordingPreflight::default());
    let control = Arc::new(PendingCancelControlRequests::default());
    let kernel = test_kernel_with_policy_control_preflight(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(AllowPolicy),
        control.clone(),
        preflight.clone(),
    );
    let recipe = recipe(
        "cancel_before_dispatch_wf",
        vec![invoke_step("invoke")],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cancel-before-dispatch-run"),
            BTreeMap::new(),
        )
        .expect("start");
    control
        .request_cancel("cancel-before-dispatch-run")
        .expect("request cancel");

    let summary = kernel
        .run_started_run(tmp.path(), &home_dir, "cancel-before-dispatch-run")
        .expect("cancel before dispatch");
    assert_eq!(summary.status, KernelRunStatus::Canceled);
    assert_eq!(preflight.invocations().len(), 1);
    assert!(execution.invocations().is_empty());
    assert!(run_store.step_executions().is_empty());

    let run = run_store.run_json("cancel-before-dispatch-run");
    assert_eq!(run["status"], "canceled");
    assert_eq!(run["steps"]["invoke"]["status"], "canceled");
    assert_eq!(run["steps"]["invoke"]["executions"], 0);

    let events = events.snapshot();
    assert!(
        events
            .iter()
            .all(|event| !event.kind.starts_with("run_preflight"))
    );
    assert!(!events.iter().any(|event| event.kind == "step_started"));
    let stopped_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "run_stopped")
        .collect();
    assert_eq!(stopped_events.len(), 1);
    let stopped = stopped_events[0];
    assert_eq!(
        serde_json::Value::from(stopped.payload.clone())["status"],
        "canceled"
    );
    assert!(!control.is_pending());
}

/// A `PolicyPort` that denies `validate_run_start` must cause the kernel to
/// return an `Err` before any steps are dispatched.
#[test]
fn run_start_denial_returns_error() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(DenyRunStartPolicy),
    );
    let recipe = recipe("deny_start_wf", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let err = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("deny-start-run"))
        .expect_err("run start should be denied by policy");
    let msg = err.to_string();
    assert!(
        msg.contains("denied") || msg.contains("policy"),
        "expected policy denial message, got: {msg}"
    );
    assert!(
        execution.invocations().is_empty(),
        "no steps must execute when run start is denied"
    );
}

/// A `PolicyPort` that denies `validate_step_execution` must mark the step as
/// failed with `policy_denied` and emit a `policy.denied` runtime event.
/// The adapter must never be invoked for the denied step.
#[test]
fn step_denial_persists_request_before_event_and_result_after_event() {
    let execution = Arc::new(RecordingExecution::default());
    let trace = OperationTrace::default();
    let run_store = Arc::new(MemoryRunStore::default().with_operation_trace(trace.clone()));
    let events = Arc::new(
        RecordingEvents::default()
            .with_operation_trace(trace.clone())
            .with_durable_mirror(run_store.clone()),
    );
    let policy = Arc::new(DenyStepAtDispatchPolicy::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        policy.clone(),
    );
    let recipe = recipe("deny_step_wf", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("deny-step-run"))
        .expect("execute must not error — policy denial is a run-level outcome");
    assert_eq!(summary.status, KernelRunStatus::Failed);
    assert!(
        execution.invocations().is_empty(),
        "adapter must not be called for a policy-denied step"
    );

    let run = run_store.run_json("deny-step-run");
    assert_eq!(run["steps"]["invoke"]["status"], "failed");
    assert_eq!(
        run["steps"]["invoke"]["last_result"]["error"]["code"], "policy_denied",
        "policy denial must produce a policy_denied error code"
    );
    assert!(
        events.snapshot().iter().any(|e| e.kind == "policy.denied"),
        "policy.denied event must be emitted for denied step"
    );

    let policy_event = events
        .snapshot()
        .into_iter()
        .find(|event| event.kind == "policy.denied")
        .expect("policy.denied event");
    assert_eq!(policy_event.step_id.as_deref(), Some("invoke"));
    let policy_payload = serde_json::Value::from(policy_event.payload);
    assert_eq!(
        policy_payload,
        serde_json::json!({
            "reason_code": "TEST_STEP_DENIED",
            "workflow_ref": "deny_step_wf",
            "adapter_kind": "process",
            "plugin_id": "shell",
            "secret_ref": null,
        })
    );

    let execution_records = run_store.step_executions();
    assert_eq!(execution_records.len(), 1);
    assert_eq!(execution_records[0]["status"], "failed");
    assert_eq!(
        execution_records[0]["result"]["error"]["code"],
        "policy_denied"
    );

    let calls = policy.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].phase, "preflight");
    assert_eq!(calls[1].phase, "dispatch");
    assert_eq!(calls[0].step_id, "invoke");
    assert_eq!(calls[1].step_id, "invoke");
    assert_eq!(calls[0].descriptor, calls[1].descriptor);
    let descriptor = match &calls[1].descriptor {
        KernelValue::Object(values) => values,
        other => panic!("policy descriptor must be an object, got {other:?}"),
    };
    assert_eq!(
        descriptor["workflow_ref"],
        KernelValue::String("deny_step_wf@v1".to_string())
    );
    assert_eq!(
        descriptor["adapter_kind"],
        KernelValue::String("process".to_string())
    );

    let operations = trace.snapshot();
    let index_of = |operation: &str| {
        operations
            .iter()
            .position(|observed| observed == operation)
            .unwrap_or_else(|| panic!("missing operation trace entry: {operation}"))
    };
    assert!(
        index_of("request_persisted") < index_of("event:policy.denied")
            && index_of("event:policy.denied") < index_of("immutable_execution_record")
            && index_of("immutable_execution_record") < index_of("terminal_projection")
            && index_of("terminal_projection") < index_of("failure_record")
            && index_of("failure_record") < index_of("event:step_failed")
    );
    let event_kinds: Vec<_> = events
        .snapshot()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    let event_index = |kind: &str| {
        event_kinds
            .iter()
            .position(|observed| observed == kind)
            .unwrap_or_else(|| panic!("missing event: {kind}"))
    };
    assert!(
        event_index("step_started") < event_index("policy.denied")
            && event_index("policy.denied") < event_index("step_failed")
            && event_index("step_failed") < event_index("run_stopped")
    );
}

/// When a cleanup step is denied by policy the cleanup phase is marked `failed`
/// but the primary run status (set before cleanup runs) is preserved as
/// `success`. A `policy.denied` event is emitted for the denied cleanup step.
#[test]
fn cleanup_step_denial_marks_cleanup_phase_failed() {
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    // DenyStepAtDispatchPolicy allows the first validate_step_execution call
    // per step (cleanup preflight) and denies the second (cleanup dispatch).
    // The main recipe uses only a control step so the policy is never consulted
    // during main-phase preflight.
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(DenyStepAtDispatchPolicy::default()),
    );

    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Success],
        step: invoke_step("teardown"),
    };
    let mut r = recipe(
        "cleanup_deny_wf",
        // control.finish is not an invoke step — validate_step_execution is not called.
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        None,
    );
    r.cleanup = vec![cleanup_step];

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("cleanup-deny-run"))
        .expect("execute");
    // Primary run status must not be downgraded by a cleanup failure.
    assert_eq!(summary.status, KernelRunStatus::Success);

    let run = run_store.run_json("cleanup-deny-run");
    assert_eq!(
        run["cleanup"]["state"], "failed",
        "cleanup phase must be marked failed when a cleanup step is denied"
    );
    assert_eq!(
        run["cleanup"]["steps"]["teardown"]["status"], "failed",
        "denied cleanup step must be persisted as failed"
    );
    assert!(
        events.snapshot().iter().any(|e| e.kind == "policy.denied"),
        "policy.denied event must be emitted for denied cleanup step"
    );
}

#[test]
fn process_secret_delivery_denial_marks_step_failed_before_adapter_call() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(DenyProcessSecretDeliveryAtDispatchPolicy::default()),
    );
    let recipe = recipe(
        "process_secret_dispatch_demo",
        vec![invoke_step_with(
            "invoke",
            BTreeMap::from([(
                "secret_refs".to_string(),
                KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
            )]),
        )],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("process-secret-dispatch-run"),
        )
        .expect("execute must not error");
    assert_eq!(summary.status, KernelRunStatus::Failed);
    assert!(execution.invocations().is_empty());

    let run = run_store.run_json("process-secret-dispatch-run");
    assert_eq!(run["steps"]["invoke"]["status"], "failed");
    assert_eq!(
        run["steps"]["invoke"]["last_result"]["error"]["code"],
        "policy_denied"
    );
    assert!(
        events.snapshot().iter().any(|event| {
            event.kind == "policy.denied"
                && matches!(
                    &event.payload,
                    KernelValue::Object(fields)
                        if fields.get("reason_code")
                            == Some(&KernelValue::String("remote_secret_policy".to_string()))
                )
        }),
        "policy.denied event with remote_secret_policy reason must be emitted"
    );
}

#[test]
fn cleanup_process_secret_delivery_denial_marks_cleanup_failed() {
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(DenyProcessSecretDeliveryAtDispatchPolicy::default()),
    );

    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Success],
        step: invoke_step_with(
            "teardown",
            BTreeMap::from([(
                "secret_refs".to_string(),
                KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
            )]),
        ),
    };
    let mut recipe = recipe(
        "cleanup_process_secret_demo",
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        None,
    );
    recipe.cleanup = vec![cleanup_step];

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let summary = kernel
        .execute_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cleanup-process-secret-run"),
        )
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);
    assert!(execution.invocations().is_empty());

    let run = run_store.run_json("cleanup-process-secret-run");
    assert_eq!(run["cleanup"]["state"], "failed");
    assert_eq!(run["cleanup"]["steps"]["teardown"]["status"], "failed");
    assert!(
        events.snapshot().iter().any(|event| {
            event.kind == "policy.denied"
                && matches!(
                    &event.payload,
                    KernelValue::Object(fields)
                        if fields.get("reason_code")
                            == Some(&KernelValue::String("remote_secret_policy".to_string()))
                )
        }),
        "policy.denied event with remote_secret_policy reason must be emitted"
    );
}

/// After policy rejects run birth, relaxing policy allows a fresh launch with
/// the same requested run id.
#[test]
fn rejected_birth_can_launch_after_policy_relaxation() {
    // ConditionalStepPolicy denies step execution while the shared flag is true.
    struct ConditionalStepPolicy(Arc<Mutex<bool>>);

    impl PolicyPort for ConditionalStepPolicy {
        fn validate_run_start(
            &self,
            _repo_root: &std::path::Path,
            _workflow_ref: &str,
        ) -> Result<PolicyDecision> {
            Ok(PolicyDecision {
                allowed: true,
                reason_code: None,
                message: None,
                payload: KernelValue::Null,
                remote_secret_policy: RemoteSecretPolicy::default(),
            })
        }

        fn validate_step_execution(
            &self,
            _run_id: &str,
            _step_id: &str,
            _descriptor: &KernelValue,
        ) -> Result<PolicyDecision> {
            let deny = *self.0.lock().expect("lock");
            Ok(PolicyDecision {
                allowed: !deny,
                reason_code: if deny {
                    Some("TEST_CONDITIONAL_DENY".to_string())
                } else {
                    None
                },
                message: if deny {
                    Some("step conditionally denied".to_string())
                } else {
                    None
                },
                payload: KernelValue::Null,
                remote_secret_policy: RemoteSecretPolicy::default(),
            })
        }
    }

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let flag = Arc::new(Mutex::new(true)); // deny initially
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(ConditionalStepPolicy(flag.clone())),
    );

    let recipe = recipe("conditional_wf", vec![invoke_step("invoke")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    // First launch: step is denied, so no run is born and nothing dispatches.
    let error = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("conditional-run"))
        .expect_err("policy denial must reject birth");
    assert!(error.to_string().contains("TEST_CONDITIONAL_DENY"));
    assert!(
        execution.invocations().is_empty(),
        "adapter must not be invoked for denied step"
    );
    assert!(!run_store.has_run("conditional-run"));

    // Relax the policy; a fresh launch using the same id now succeeds.
    *flag.lock().expect("lock") = false;
    let launched = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("conditional-run"))
        .expect("launch after policy relaxation");
    assert_eq!(launched.status, KernelRunStatus::Success);
    assert_eq!(
        execution.invocations().len(),
        1,
        "step must execute once after policy is relaxed"
    );
}

/// `validate_step_execution` must receive the full local-policy descriptor for
/// invoke steps, including workflow, plugin, provider, and secret-ref metadata.
#[test]
fn step_descriptor_exposes_adapter_kind_and_plugin_id() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let policy = Arc::new(RecordingPolicy::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        policy.clone(),
    );
    let recipe = recipe(
        "descriptor_wf",
        vec![invoke_external_step(
            "invoke",
            AdapterKind::Process,
            "shell",
            "echo",
            BTreeMap::from([(
                "secret_refs".to_string(),
                KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
            )]),
        )],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("descriptor-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let descriptors = policy.captured_descriptors();
    // validate_step_execution is called twice for a single invoke step:
    // once during preflight (to detect policy issues before dispatch) and
    // once at runtime dispatch.  Both calls receive the same descriptor shape.
    assert_eq!(
        descriptors.len(),
        2,
        "validate_step_execution must be called once at preflight and once at dispatch"
    );
    // Both the preflight and the dispatch call must expose the full descriptor
    // used by local allowlist policy evaluation.
    for descriptor in &descriptors {
        let KernelValue::Object(values) = descriptor else {
            panic!("descriptor must be an object");
        };
        assert_eq!(
            values.get("workflow_ref"),
            Some(&KernelValue::String("descriptor_wf@v1".to_string()))
        );
        assert_eq!(
            values.get("adapter_kind"),
            Some(&KernelValue::String("process".to_string()))
        );
        assert!(values.contains_key("requested_capabilities"));
    }
}

/// Loop `until: vars.x == "done"` must terminate the loop once the body
/// writes `vars.x = "done"`. This exercises the Step 4 runtime resolver
/// path for `vars.*` in expression contexts.
#[test]
fn loop_until_terminates_on_var_value() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    // Body writes vars.flag = "done" on success.
    let mut body = invoke_step("body");
    body.vars = BTreeMap::from([("flag".to_string(), KernelValue::String("done".to_string()))]);

    // Loop wrapper terminates when vars.flag == "done".
    let loop_meta = BTreeMap::from([(
        "__transist_loop".to_string(),
        serde_json::json!({
            "body_steps": ["body"],
            "max_iters": 5,
            "until": "vars.flag == \"done\""
        })
        .into(),
    )]);
    let loop_wrapper = control_step("loop_step", "control.noop", loop_meta);
    let finish = control_step("finish", "control.finish", BTreeMap::new());

    let r = recipe(
        "vars-loop",
        vec![loop_wrapper, body, finish],
        Some(RecipeGraph {
            entry: "loop_step".to_string(),
            edges: vec![
                RecipeGraphEdge {
                    from: "loop_step".to_string(),
                    to: "body".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: None,
                },
                RecipeGraphEdge {
                    from: "body".to_string(),
                    to: "loop_step".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: Some(RecipeLoop {
                        while_expr: None,
                        until_expr: Some("vars.flag == \"done\"".to_string()),
                        max_iters: Some(5),
                    }),
                },
                RecipeGraphEdge {
                    from: "loop_step".to_string(),
                    to: "finish".to_string(),
                    event: "on_success".to_string(),
                    guard: None,
                    r#loop: None,
                },
            ],
            max_iterations: None,
        }),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &r, Some("vars-loop-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    // The body should have executed exactly once because the first iteration
    // already wrote `flag == "done"`. We assert the body's executions count.
    let run = run_store.run_json("vars-loop-run");
    assert_eq!(
        run["steps"]["body"]["executions"],
        serde_json::json!(1),
        "loop body should run once and terminate via vars.flag == 'done'"
    );
}

// ── filesystem policy runtime tests ─────────────────────────────────────────

#[test]
fn cleanup_emits_sandbox_failed_event() {
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store,
        events.clone(),
        Arc::new(AllowPolicy),
    );

    let mut recipe = recipe(
        "cleanup-sandbox-fail-wf",
        vec![control_step("finish", "control.finish", BTreeMap::new())],
        None,
    );
    recipe.cleanup = vec![RecipeCleanupStep {
        when: vec![CleanupTrigger::Success],
        step: invoke_step("cleanup_step"),
    }];

    let mut result = success_result_for("cleanup-sandbox-run", "cleanup_step", 1);
    result.status = "failed".to_string();
    result.exit_code = Some(2);
    result.output = serde_json::json!({
        "__sandbox_failed__": {
            "step_id": "cleanup_step",
            "adapter": "process",
            "backend": "sandbox-exec",
            "reason": "sandbox profile generation failed",
            "mode": "strict",
            "ignore_paths": [".env"]
        }
    });
    result.error = Some(transist_protocol::ProtocolError::new(
        transist_protocol::CanonicalErrorCode::PolicyDenied,
        "strict filesystem sandboxing failed: sandbox profile generation failed",
    ));
    execution.plan_result("cleanup-sandbox-run", "cleanup_step", 1, result);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("cleanup-sandbox-run"))
        .expect("execute");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let event = events
        .snapshot()
        .into_iter()
        .find(|event| event.kind == "filesystem_policy.sandbox_failed")
        .expect("filesystem_policy.sandbox_failed event");
    let payload = serde_json::Value::from(event.payload);
    assert_eq!(payload["step_id"], "cleanup_step");
    assert_eq!(payload["backend"], "sandbox-exec");
    assert_eq!(payload["ignore_paths"][0], ".env");
}

/// Strict mode with a matching ignore_paths pattern denies an artifact in the
/// ignored path. The step is marked failed with policy_denied error code.
#[test]
fn strict_mode_denies_artifact_in_ignored_path() {
    let execution = Arc::new(RecordingExecution::default());
    let trace = OperationTrace::default();
    let run_store = Arc::new(MemoryRunStore::default().with_operation_trace(trace.clone()));
    let events = Arc::new(RecordingEvents::default().with_operation_trace(trace.clone()));
    let kernel = test_kernel_with_phase_trace(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        trace.clone(),
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let home_dir = test_home_dir(&tmp);
    let secret_file = workspace.join("secrets/token.txt");
    std::fs::create_dir_all(secret_file.parent().expect("parent")).expect("create dirs");
    std::fs::write(&secret_file, b"secret").expect("write file");

    let step_id = "invoke";
    let read_only_policy = transist_protocol::FilesystemPolicy {
        workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
            mode: transist_protocol::WorkspaceAccessMode::ReadOnly,
        }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    };
    let step = RecipeStep {
        id: step_id.to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: None,
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            plugin_sha256: None,
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Some(read_only_policy.clone()),
        network_policy: Default::default(),
        effective_filesystem_policy: read_only_policy,
        effective_filesystem_policy_source: Some(transist_protocol::FilesystemPolicySource::Step),
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    };
    let r = recipe("strict-artifact-wf", vec![step], None);

    // Plan adapter result with an artifact at the ignored path.
    let result = ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke-1".to_string(),
        run_id: "strict-artifact-run".to_string(),
        step_id: step_id.to_string(),
        execution: 1,
        status: "success".to_string(),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timing: transist_protocol::InvokeTiming {
            started_at: "1".to_string(),
            finished_at: "1".to_string(),
            duration_ms: 0,
        },
        metrics: BTreeMap::new(),
        output: serde_json::json!({"ok": true}),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
        artifacts: vec![FileRef {
            path: secret_file.display().to_string(),
            sha256: String::new(),
            bytes: 0,
        }],
        evidence: Vec::new(),
    };
    execution.plan_result("strict-artifact-run", step_id, 1, result);

    let summary = kernel
        .execute_recipe(&workspace, &home_dir, &r, Some("strict-artifact-run"))
        .expect("execute");

    assert_eq!(summary.status, KernelRunStatus::Failed);
    let run = run_store.run_json("strict-artifact-run");
    assert_eq!(run["steps"][step_id]["status"], "failed");
    assert_eq!(
        run["steps"][step_id]["last_result"]["error"]["code"], "policy_denied",
        "denied artifact must produce policy_denied error code"
    );
    let operations = trace.snapshot();
    assert!(
        !operations
            .iter()
            .any(|operation| operation == "result_promoted")
    );
    let index_of = |operation: &str| {
        operations
            .iter()
            .position(|observed| observed == operation)
            .unwrap_or_else(|| panic!("missing operation trace entry: {operation}"))
    };
    assert!(
        index_of("immutable_execution_record") < index_of("terminal_projection")
            && index_of("terminal_projection") < index_of("event:filesystem_policy.denied")
    );
    let event = events
        .snapshot()
        .into_iter()
        .find(|event| event.kind == "filesystem_policy.denied")
        .expect("filesystem_policy.denied event");
    let payload = serde_json::Value::from(event.payload);
    assert_eq!(payload["step_id"], step_id);
    assert_eq!(payload["surface"], "artifact");
    assert_eq!(payload["source"], "step");
    assert_eq!(payload["mode"], "read_only");
    assert_eq!(payload["operation"], "write");
    assert_eq!(payload["matched_pattern"], "<read_only>");
    assert_eq!(payload["path"], "secrets/token.txt");
}

/// Strict mode with a matching ignore_paths pattern denies evidence in the
/// ignored path.
#[test]
fn strict_mode_denies_evidence_in_ignored_path() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(AllowPolicy),
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let home_dir = test_home_dir(&tmp);
    let secret_file = workspace.join("secrets/evidence.log");
    std::fs::create_dir_all(secret_file.parent().expect("parent")).expect("create dirs");
    std::fs::write(&secret_file, b"secret evidence").expect("write file");

    let step_id = "invoke";
    let read_only_policy = transist_protocol::FilesystemPolicy {
        workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
            mode: transist_protocol::WorkspaceAccessMode::ReadOnly,
        }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    };
    let step = RecipeStep {
        id: step_id.to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: None,
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            plugin_sha256: None,
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Some(read_only_policy.clone()),
        network_policy: Default::default(),
        effective_filesystem_policy: read_only_policy,
        effective_filesystem_policy_source: Some(transist_protocol::FilesystemPolicySource::Step),
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    };
    let r = recipe("strict-evidence-wf", vec![step], None);

    let result = ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke-1".to_string(),
        run_id: "strict-evidence-run".to_string(),
        step_id: step_id.to_string(),
        execution: 1,
        status: "success".to_string(),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timing: transist_protocol::InvokeTiming {
            started_at: "1".to_string(),
            finished_at: "1".to_string(),
            duration_ms: 0,
        },
        metrics: BTreeMap::new(),
        output: serde_json::json!({"ok": true}),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
        artifacts: Vec::new(),
        evidence: vec![FileRef {
            path: secret_file.display().to_string(),
            sha256: String::new(),
            bytes: 0,
        }],
    };
    execution.plan_result("strict-evidence-run", step_id, 1, result);

    let summary = kernel
        .execute_recipe(&workspace, &home_dir, &r, Some("strict-evidence-run"))
        .expect("execute");

    assert_eq!(summary.status, KernelRunStatus::Failed);
    let run = run_store.run_json("strict-evidence-run");
    assert_eq!(run["steps"][step_id]["status"], "failed");
    assert_eq!(
        run["steps"][step_id]["last_result"]["error"]["code"], "policy_denied",
        "denied evidence must produce policy_denied error code"
    );
    let event = events
        .snapshot()
        .into_iter()
        .find(|event| event.kind == "filesystem_policy.denied")
        .expect("filesystem_policy.denied event");
    let payload = serde_json::Value::from(event.payload);
    assert_eq!(payload["step_id"], step_id);
    assert_eq!(payload["surface"], "evidence");
    assert_eq!(payload["source"], "step");
    assert_eq!(payload["mode"], "read_only");
    assert_eq!(payload["operation"], "write");
    assert_eq!(payload["matched_pattern"], "<read_only>");
    assert_eq!(payload["path"], "secrets/evidence.log");
}

#[test]
fn read_only_policy_denies_artifact_write_in_workspace() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel_with_policy(
        execution.clone(),
        run_store.clone(),
        events.clone(),
        Arc::new(AllowPolicy),
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let home_dir = test_home_dir(&tmp);
    let report_file = workspace.join("reports/output.txt");
    std::fs::create_dir_all(report_file.parent().expect("parent")).expect("create dirs");
    std::fs::write(&report_file, b"report").expect("write file");

    let step_id = "invoke";
    let read_only_policy = transist_protocol::FilesystemPolicy {
        workspace_access: Some(transist_protocol::WorkspaceAccessPolicy {
            mode: transist_protocol::WorkspaceAccessMode::ReadOnly,
        }),
        read_allow_paths: Vec::new(),
        write_allow_paths: Vec::new(),
    };
    let step = RecipeStep {
        id: step_id.to_string(),
        execution: StepExecutionDescriptor::Invoke {
            adapter: AdapterKind::Process,
            plugin_id: None,
            plugin: "shell".to_string(),
            action: "echo".to_string(),
            execution_kind: "external".to_string(),
            plugin_version: None,
            plugin_sha256: None,
        },
        with: BTreeMap::new(),
        prompt_binding: None,
        skills_binding: None,
        vars: BTreeMap::new(),
        filesystem_policy: Some(read_only_policy.clone()),
        network_policy: Default::default(),
        effective_filesystem_policy: read_only_policy,
        effective_filesystem_policy_source: Some(transist_protocol::FilesystemPolicySource::Step),
        effective_network_policy: Default::default(),
        effective_network_policy_source: None,
    };
    let r = recipe("read-only-artifact-wf", vec![step], None);

    let result = ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: "invoke-1".to_string(),
        run_id: "read-only-artifact-run".to_string(),
        step_id: step_id.to_string(),
        execution: 1,
        status: "success".to_string(),
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timing: transist_protocol::InvokeTiming {
            started_at: "1".to_string(),
            finished_at: "1".to_string(),
            duration_ms: 0,
        },
        metrics: BTreeMap::new(),
        output: serde_json::json!({"ok": true}),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
        artifacts: vec![FileRef {
            path: report_file.display().to_string(),
            sha256: String::new(),
            bytes: 0,
        }],
        evidence: Vec::new(),
    };
    execution.plan_result("read-only-artifact-run", step_id, 1, result);

    let summary = kernel
        .execute_recipe(&workspace, &home_dir, &r, Some("read-only-artifact-run"))
        .expect("execute");

    assert_eq!(summary.status, KernelRunStatus::Failed);
    let event = events
        .snapshot()
        .into_iter()
        .find(|event| event.kind == "filesystem_policy.denied")
        .expect("filesystem_policy.denied event");
    let payload = serde_json::Value::from(event.payload);
    assert_eq!(payload["surface"], "artifact");
    assert_eq!(payload["source"], "step");
    assert_eq!(payload["mode"], "read_only");
    assert_eq!(payload["operation"], "write");
    assert_eq!(payload["matched_pattern"], "<read_only>");
    assert_eq!(payload["path"], "reports/output.txt");
}

#[test]
fn start_recipe_rejects_traversal_run_ref() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store, events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let r = recipe("run_ref_containment_wf", vec![invoke_step("invoke")], None);

    let err = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &r,
            Some("../escape"),
            BTreeMap::new(),
        )
        .expect_err("traversal run ref must be rejected");
    assert!(
        err.to_string().contains("run reference"),
        "unexpected error: {err}"
    );
}

#[test]
fn start_recipe_accepts_slug_run_ref() {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution, run_store, events);

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let r = recipe("run_ref_containment_wf", vec![invoke_step("invoke")], None);

    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &r,
            Some("valid-run-ref.1"),
            BTreeMap::new(),
        )
        .expect("start");
    assert_eq!(run_id, "valid-run-ref.1");
}

struct ExecutionFaultGuard;

impl ExecutionFaultGuard {
    fn arm(point: usize) -> Self {
        transist_core::clear();
        transist_core::set(point);
        Self
    }
}

impl Drop for ExecutionFaultGuard {
    fn drop(&mut self) {
        transist_core::clear();
    }
}

fn assert_transient_execution_fault_recovery(
    point: usize,
    initial_invocations: usize,
    expected_started_events: usize,
) {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default().with_durable_mirror(run_store.clone()));
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let recipe = recipe("transient-fault", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = "transient-fault-run";

    let error = {
        let _fault = ExecutionFaultGuard::arm(point);
        kernel
            .execute_recipe(tmp.path(), &home_dir, &recipe, Some(run_id))
            .expect_err("transient execution fault must abort the first attempt")
    };
    assert!(error.to_string().contains("injected fault"));
    assert_eq!(execution.invocations().len(), initial_invocations);
    assert!(
        run_store
            .step_executions()
            .iter()
            .all(|record| record["execution"] != 1)
    );
    let in_flight = run_store.run_json(run_id);
    assert_eq!(in_flight["steps"]["a"]["executions"], 1);
    assert!(matches!(
        in_flight["steps"]["a"]["status"].as_str(),
        Some("dispatching" | "running")
    ));
    assert_eq!(
        events
            .snapshot()
            .iter()
            .filter(|event| event.kind == "step_started")
            .count(),
        expected_started_events
    );

    execution.plan_result(run_id, "a", 2, success_result_for(run_id, "a", 2));
    let recovered = kernel
        .run_started_run(tmp.path(), &home_dir, run_id)
        .expect("recover transient execution fault");
    assert_eq!(recovered.status, KernelRunStatus::Success);
    assert_eq!(execution.invocations().len(), initial_invocations + 1);

    let execution_records = run_store.step_executions();
    assert_eq!(
        execution_records
            .iter()
            .filter(|record| {
                record["step_id"] == "a"
                    && record["execution"] == 1
                    && record["status"] == "interrupted"
            })
            .count(),
        1
    );
    assert_eq!(
        execution_records
            .iter()
            .filter(|record| {
                record["step_id"] == "a"
                    && record["execution"] == 2
                    && record["status"] == "success"
            })
            .count(),
        1
    );
    assert!(
        execution
            .invocations()
            .iter()
            .any(|invocation| invocation.request.step.execution == 2)
    );
}

#[test]
fn dispatch_intent_fault_recovers_at_next_ordinal() {
    use transist_core::FAULT_STEP_DISPATCH_INTENT;

    assert_transient_execution_fault_recovery(FAULT_STEP_DISPATCH_INTENT, 0, 0);
}

#[test]
fn step_start_event_fault_recovers_at_next_ordinal() {
    use transist_core::FAULT_STEP_START_EVENT;

    assert_transient_execution_fault_recovery(FAULT_STEP_START_EVENT, 0, 1);
}

#[test]
fn result_promotion_fault_recovers_at_next_ordinal() {
    use transist_core::FAULT_STEP_RESULT_PROMOTION;

    assert_transient_execution_fault_recovery(FAULT_STEP_RESULT_PROMOTION, 1, 1);
}

#[test]
fn loaded_invoke_result_phases_commit_before_terminal_projection_and_event() {
    let execution = Arc::new(RecordingExecution::default());
    let trace = OperationTrace::default();
    let run_store = Arc::new(MemoryRunStore::default().with_operation_trace(trace.clone()));
    let events = Arc::new(RecordingEvents::default().with_operation_trace(trace.clone()));
    let kernel = test_kernel_with_phase_trace(execution.clone(), run_store, events, trace.clone());
    let recipe = recipe("phase-order", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    execution.plan_result(
        "phase-order-run",
        "a",
        1,
        success_result_for("phase-order-run", "a", 1),
    );

    let summary = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("phase-order-run"))
        .expect("execute phase-order run");
    assert_eq!(summary.status, KernelRunStatus::Success);

    let operations = trace.snapshot();
    let index_of = |operation: &str| {
        operations
            .iter()
            .position(|observed| observed == operation)
            .unwrap_or_else(|| panic!("missing operation trace entry: {operation}"))
    };
    assert!(
        index_of("result_promoted") < index_of("immutable_execution_record")
            && index_of("immutable_execution_record") < index_of("terminal_projection")
            && index_of("terminal_projection") < index_of("event:step_finished")
            && index_of("event:step_finished") < index_of("telemetry:step_finished")
    );
}

fn assert_post_terminal_execution_fault(
    point: usize,
    expected_failure_record: bool,
    expected_step_failed_event: bool,
) {
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default().with_durable_mirror(run_store.clone()));
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let recipe = recipe("post-terminal-fault", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = "post-terminal-fault-run";
    execution.plan_result(run_id, "a", 1, failed_result(run_id, "a", 1, "boom"));

    let error = {
        let _fault = ExecutionFaultGuard::arm(point);
        kernel
            .execute_recipe(tmp.path(), &home_dir, &recipe, Some(run_id))
            .expect_err("post-terminal execution fault must abort the attempt")
    };
    assert!(error.to_string().contains("injected fault"));

    let persisted = run_store.run_json(run_id);
    assert_eq!(persisted["steps"]["a"]["status"], "failed");
    let execution_records = run_store.step_executions();
    assert_eq!(execution_records.len(), 1);
    assert_eq!(execution_records[0]["execution"], 1);
    assert_eq!(execution_records[0]["status"], "failed");
    assert_eq!(
        run_store.failures().len(),
        if expected_failure_record { 1 } else { 0 }
    );
    let step_failed_events: Vec<_> = events
        .snapshot()
        .into_iter()
        .filter(|event| event.kind == "step_failed")
        .collect();
    assert_eq!(
        step_failed_events.len(),
        if expected_step_failed_event { 1 } else { 0 }
    );
    assert_eq!(
        run_store
            .load_run_events(run_id)
            .expect("durable events")
            .iter()
            .filter(|event| event.kind == "step_failed")
            .count(),
        if expected_step_failed_event { 1 } else { 0 }
    );

    let recovered = kernel
        .run_started_run(tmp.path(), &home_dir, run_id)
        .expect("re-enter after terminal fault");
    assert_eq!(recovered.status, KernelRunStatus::Failed);
    assert_eq!(execution.invocations().len(), 1);
    assert_eq!(
        run_store
            .failures()
            .into_iter()
            .filter(|failure| failure.run_id == run_id && failure.step_id == "a")
            .count(),
        1,
        "re-entry must backfill exactly one failure record"
    );
    assert_eq!(
        run_store
            .load_run_events(run_id)
            .expect("durable events")
            .iter()
            .filter(|event| event.kind == "step_failed")
            .count(),
        1,
        "re-entry must backfill exactly one completion event"
    );
}

#[test]
fn state_projection_fault_recovers_failure_effects_on_reentry() {
    use transist_core::FAULT_STEP_STATE_PROJECTION;

    assert_post_terminal_execution_fault(FAULT_STEP_STATE_PROJECTION, false, false);
}

#[test]
fn failure_append_fault_recovers_completion_event_on_reentry() {
    use transist_core::FAULT_STEP_FAILURE_APPEND;

    assert_post_terminal_execution_fault(FAULT_STEP_FAILURE_APPEND, true, false);
}

#[test]
fn completion_event_fault_preserves_failed_event_and_durable_state() {
    use transist_core::FAULT_STEP_COMPLETION_EVENT;

    assert_post_terminal_execution_fault(FAULT_STEP_COMPLETION_EVENT, true, true);
}

#[test]
fn recovery_reconciles_committed_execution_record_without_redispatch() {
    // Simulate a crash after the immutable execution record was committed but
    // before the terminal step-state projection was persisted (the
    // FAULT_STEP_EXECUTION_RECORD window). On re-entry the kernel must
    // reconcile the step's terminal state from the immutable record and never
    // re-dispatch the committed result.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let recipe = recipe(
        "reconcile-record",
        vec![invoke_step("a"), invoke_step("b")],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result(
        "reconcile-run",
        "a",
        1,
        success_result_for("reconcile-run", "a", 1),
    );
    execution.plan_result(
        "reconcile-run",
        "b",
        1,
        success_result_for("reconcile-run", "b", 1),
    );

    let first = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("reconcile-run"))
        .expect("first run");
    assert_eq!(first.status, KernelRunStatus::Success);
    assert_eq!(execution.invocations().len(), 2);

    // Craft the crash-after-record state: step "b" shows as Running with a
    // consumed ordinal (executions=1), no projected result, while the
    // immutable execution record for ordinal 1 already exists in the store.
    let mut record = run_store
        .load_run("reconcile-run")
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Running;
    if let Some(step) = record.steps.get_mut("b") {
        step.status = transist_core::KernelStepStatus::Running;
        step.executions = 1;
        step.last_result = None;
        step.finished_at_ms = None;
    }
    run_store
        .update_run_state("reconcile-run", &record)
        .expect("plant crash state");

    let reconciled = kernel
        .run_started_run(tmp.path(), &home_dir, "reconcile-run")
        .expect("reconcile on re-entry");
    assert_eq!(reconciled.status, KernelRunStatus::Success);

    // The committed result for step "b" at ordinal 1 must never be dispatched
    // again.
    assert_eq!(
        execution.invocations().len(),
        2,
        "a committed result must never be dispatched again"
    );

    let persisted = run_store.run_json("reconcile-run");
    assert_eq!(persisted["steps"]["b"]["executions"], 1);
    assert_eq!(persisted["steps"]["b"]["status"], "succeeded");
    let execution_records = run_store.step_executions();
    assert_eq!(
        execution_records
            .iter()
            .filter(|e| e["step_id"] == "b" && e["execution"] == 1)
            .count(),
        1,
        "every consumed execution ordinal has exactly one immutable record"
    );
}

#[test]
fn recovery_creates_interrupted_record_for_inflight_ordinal_before_replacement() {
    // Simulate a crash before any completion record exists for a consumed
    // ordinal (the FAULT_STEP_DISPATCH_INTENT / FAULT_STEP_START_EVENT window).
    // Recovery must write an immutable `interrupted` record for ordinal N and
    // the replacement dispatch must begin only at N+1.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let recipe = recipe("interrupted-record", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("interrupt-run"),
            BTreeMap::new(),
        )
        .expect("start");

    // Plant a crash-after-dispatch-intent state: step "a" was dispatched at
    // ordinal 1 (executions=1, started) but no execution record exists.
    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Running;
    if let Some(step) = record.steps.get_mut("a") {
        step.status = transist_core::KernelStepStatus::Dispatching;
        step.executions = 1;
        step.started_at_ms = Some(7);
        step.last_result = None;
    }
    run_store.update_run_state(&run_id, &record).expect("plant");

    // The replacement dispatch must occur at N+1 (ordinal 2).
    execution.plan_result(&run_id, "a", 2, success_result_for(&run_id, "a", 2));

    let result = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("recover interrupted run");
    assert_eq!(result.status, KernelRunStatus::Success);

    let execution_records = run_store.step_executions();
    let interrupted = execution_records
        .iter()
        .find(|e| e["step_id"] == "a" && e["execution"] == 1 && e["status"] == "interrupted")
        .expect("an immutable interrupted record must exist for ordinal 1");
    assert_eq!(interrupted["result"], serde_json::Value::Null);
    let replacement = execution_records
        .iter()
        .find(|e| e["step_id"] == "a" && e["execution"] == 2)
        .expect("replacement dispatch must be recorded at N+1");
    assert_eq!(replacement["status"], "success");

    let persisted = run_store.run_json(&run_id);
    assert_eq!(persisted["steps"]["a"]["executions"], 2);
    assert_eq!(persisted["steps"]["a"]["status"], "succeeded");
}

#[test]
fn execution_record_fault_is_durably_committed_and_reconciled_on_reentry() {
    use transist_core::FAULT_STEP_EXECUTION_RECORD;

    // A crash injected exactly at the immutable-execution-record commit point:
    // the record IS persisted (it is the completion commit point) and the
    // injected error then aborts the run before the state projection. A
    // re-entry reconciles the step's terminal state from the committed record
    // and never re-dispatches.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default().with_durable_mirror(run_store.clone()));
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let mut step = invoke_step("a");
    step.vars = BTreeMap::from([(
        "recovered".to_string(),
        KernelValue::String("yes".to_string()),
    )]);
    let recipe = recipe("record-fault", vec![step], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result("fault-run", "a", 1, success_result_for("fault-run", "a", 1));

    let err = {
        let _fault = ExecutionFaultGuard::arm(FAULT_STEP_EXECUTION_RECORD);
        kernel
            .execute_recipe(tmp.path(), &home_dir, &recipe, Some("fault-run"))
            .expect_err("injected fault at the execution-record commit must abort the run")
    };
    assert!(err.to_string().contains("injected fault"));

    // The immutable execution record for ordinal 1 was committed.
    let execution_records = run_store.step_executions();
    assert_eq!(
        execution_records
            .iter()
            .filter(|e| e["step_id"] == "a" && e["execution"] == 1)
            .count(),
        1,
        "the immutable execution record must survive the crash"
    );
    assert_eq!(execution_records[0]["status"], "success");
    assert!(
        !events
            .snapshot()
            .iter()
            .any(|event| event.kind == "step_finished")
    );

    // A re-entry reconciles the step from that record and never re-dispatches.
    let recovered = kernel
        .run_started_run(tmp.path(), &home_dir, "fault-run")
        .expect("recover committed execution");
    assert_eq!(recovered.status, KernelRunStatus::Success);
    assert_eq!(
        execution.invocations().len(),
        1,
        "a committed result must never be dispatched again"
    );
    let persisted = run_store.run_json("fault-run");
    assert_eq!(persisted["steps"]["a"]["executions"], 1);
    assert_eq!(persisted["steps"]["a"]["status"], "succeeded");
    assert_eq!(persisted["workflow_vars"]["recovered"], "yes");
    let completion_events: Vec<_> = events
        .snapshot()
        .into_iter()
        .filter(|event| event.kind == "step_finished")
        .collect();
    assert_eq!(completion_events.len(), 1);
    assert_eq!(
        serde_json::Value::from(completion_events[0].payload.clone())["execution"],
        1
    );
}

#[test]
fn recovery_reactivates_loop_target_after_execution_record_fault() {
    use transist_core::FAULT_STEP_EXECUTION_RECORD;

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default().with_durable_mirror(run_store.clone()));
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let a = invoke_step_with(
        "a",
        BTreeMap::from([(
            "__transist_loop".to_string(),
            serde_json::json!({ "body_steps": ["b"], "max_iters": 2 }).into(),
        )]),
    );
    let recipe = recipe(
        "loop-target-recovery",
        vec![a, invoke_step("b")],
        Some(RecipeGraph {
            entry: "a".to_string(),
            edges: vec![RecipeGraphEdge {
                from: "a".to_string(),
                to: "b".to_string(),
                event: "on_success".to_string(),
                guard: None,
                r#loop: Some(RecipeLoop {
                    while_expr: None,
                    until_expr: None,
                    max_iters: None,
                }),
            }],
            max_iterations: None,
        }),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("loop-target-recovery-run"),
            BTreeMap::new(),
        )
        .expect("start");

    // The active loop must reset its body and reactivate this otherwise
    // terminal target during recovery; without either reconstructed effect it
    // would never dispatch.
    let mut planted = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("run present");
    planted.steps.get_mut("b").expect("step b").status = transist_core::KernelStepStatus::Succeeded;
    run_store
        .update_run_state(&run_id, &planted)
        .expect("plant target");

    execution.plan_result(&run_id, "a", 1, success_result_for(&run_id, "a", 1));
    execution.plan_result(&run_id, "b", 1, success_result_for(&run_id, "b", 1));
    {
        let _fault = ExecutionFaultGuard::arm(FAULT_STEP_EXECUTION_RECORD);
        kernel
            .run_started_run(tmp.path(), &home_dir, &run_id)
            .expect_err("fault after committing a");
    }

    let recovered = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("recover loop target");
    assert_eq!(recovered.status, KernelRunStatus::Success);
    assert!(
        execution
            .invocations()
            .iter()
            .any(|invocation| invocation.request.step.id == "b"
                && invocation.request.step.execution == 1)
    );
}

#[test]
fn recovery_reconciles_committed_retryable_failure_as_retry_wait_from_record() {
    // A committed retryable failure whose retry deadline is still in the future
    // must be reconciled as `RetryWait` from the immutable record (which now
    // persists the deadline), not degraded to a plain `Failed`, and must never
    // be re-dispatched.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let recipe = recipe(
        "retry_recover_wf",
        vec![invoke_step_with(
            "a",
            BTreeMap::from([
                (
                    "retry_max".to_string(),
                    KernelValue::Number(serde_json::Number::from(1_u64)),
                ),
                (
                    "retry_wait_ms".to_string(),
                    KernelValue::Number(serde_json::Number::from(100_u64)),
                ),
            ]),
        )],
        None,
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    execution.plan_result(
        "retry-run",
        "a",
        1,
        failed_result("retry-run", "a", 1, "boom"),
    );

    let first = kernel
        .execute_recipe(tmp.path(), &home_dir, &recipe, Some("retry-run"))
        .expect("first run");
    // FixedClock is at 1ms, so the retry deadline (1 + 100) is in the future and
    // the step parks in RetryWait without being re-dispatched in the same loop.
    assert_eq!(first.status, KernelRunStatus::Waiting);
    assert_eq!(
        run_store.run_json("retry-run")["steps"]["a"]["executions"],
        1
    );
    assert_eq!(execution.invocations().len(), 1);

    // The committed immutable record carries the retry deadline.
    let committed = run_store
        .load_step_execution("retry-run", "a", 1)
        .expect("load record")
        .expect("record present");
    assert_eq!(committed.retry_wait_until_ms, Some(101));

    // Plant the crash-after-record window: step shows as in-flight (executions=1)
    // with the terminal projection lost.
    let mut record = run_store
        .load_run("retry-run")
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Running;
    if let Some(step) = record.steps.get_mut("a") {
        step.status = transist_core::KernelStepStatus::Running;
        step.executions = 1;
        step.last_result = None;
        step.retry_wait_until_ms = None;
        step.finished_at_ms = None;
    }
    run_store
        .update_run_state("retry-run", &record)
        .expect("plant");

    let recovered = kernel
        .run_started_run(tmp.path(), &home_dir, "retry-run")
        .expect("recover committed retryable failure");
    assert_eq!(recovered.status, KernelRunStatus::Waiting);
    assert_eq!(
        execution.invocations().len(),
        1,
        "a committed retryable failure must never be re-dispatched"
    );
    let persisted = run_store.run_json("retry-run");
    assert_eq!(persisted["steps"]["a"]["status"], "retry_wait");
    assert_eq!(persisted["steps"]["a"]["retry_wait_until_ms"], 101);
}

#[test]
fn recovery_never_creates_execution_000_for_transient_ordinal_zero() {
    // A step showing as in-flight with a zero execution counter never consumed
    // an execution, so recovery must not fabricate a spurious `execution_000`
    // record; the first real dispatch happens at ordinal 1.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let recipe = recipe("ordinal-zero-wf", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("zero-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Running;
    if let Some(step) = record.steps.get_mut("a") {
        step.status = transist_core::KernelStepStatus::Running;
        step.executions = 0;
        step.last_result = None;
        step.finished_at_ms = None;
    }
    run_store.update_run_state(&run_id, &record).expect("plant");

    execution.plan_result(&run_id, "a", 1, success_result_for(&run_id, "a", 1));
    let result = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("recover and dispatch at first real ordinal");
    assert_eq!(result.status, KernelRunStatus::Success);

    let execution_records = run_store.step_executions();
    assert!(
        !execution_records
            .iter()
            .any(|e| e["step_id"] == "a" && e["execution"] == 0),
        "no spurious execution_000 record may be created"
    );
    assert!(
        execution_records
            .iter()
            .any(|e| e["step_id"] == "a" && e["execution"] == 1 && e["status"] == "success"),
        "the first real dispatch must be recorded at ordinal 1"
    );
}

#[test]
fn recovery_backfills_the_missing_completion_event_but_not_an_existing_one() {
    use transist_protocol::RuntimeEvent;

    // Case A: an immutable completion record exists but the completion event
    // was never published (the FAULT_STEP_COMPLETION_EVENT window). Recovery
    // must publish exactly one backfilled event.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let recipe = recipe("event-backfill-wf", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("backfill-run"),
            BTreeMap::new(),
        )
        .expect("start");

    let record_for = |run_id: &str, step_id: &str| transist_core::StepExecutionRecord {
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution: 1,
        status: "success".to_string(),
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        retry_wait_until_ms: None,
        request: None,
        result: Some(success_result_for(run_id, step_id, 1)),
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: Vec::new(),
    };
    run_store
        .store_step_execution(&RunStoreStepExecution {
            run_id: run_id.clone(),
            step_id: "a".to_string(),
            execution: 1,
            state: serde_json::to_value(record_for(&run_id, "a"))
                .expect("json")
                .into(),
        })
        .expect("store record");

    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Running;
    if let Some(step) = record.steps.get_mut("a") {
        step.status = transist_core::KernelStepStatus::Running;
        step.executions = 1;
        step.last_result = None;
        step.finished_at_ms = None;
    }
    run_store.update_run_state(&run_id, &record).expect("plant");

    let recovered = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("recover and backfill");
    assert_eq!(recovered.status, KernelRunStatus::Success);
    let backfilled: Vec<_> = events
        .snapshot()
        .into_iter()
        .filter(|e| e.kind == "step_finished" && e.step_id.as_deref() == Some("a"))
        .collect();
    assert_eq!(
        backfilled.len(),
        1,
        "the missing completion event must be backfilled exactly once"
    );

    // Case B: the completion event is already durably published; recovery must
    // not backfill a duplicate.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("dedup-run"),
            BTreeMap::new(),
        )
        .expect("start");
    run_store
        .store_step_execution(&RunStoreStepExecution {
            run_id: run_id.clone(),
            step_id: "a".to_string(),
            execution: 1,
            state: serde_json::to_value(record_for(&run_id, "a"))
                .expect("json")
                .into(),
        })
        .expect("store record");
    // The run store already holds the completion event, so recovery must dedupe.
    run_store.inject_event(RuntimeEvent {
        kind: "step_finished".to_string(),
        run_id: run_id.clone(),
        step_id: Some("a".to_string()),
        timestamp_unix_ms: 2,
        payload: serde_json::json!({
            "execution": 1,
            "status": "success",
            "exit_code": 0,
        })
        .into(),
    });
    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Running;
    if let Some(step) = record.steps.get_mut("a") {
        step.status = transist_core::KernelStepStatus::Running;
        step.executions = 1;
        step.last_result = None;
        step.finished_at_ms = None;
    }
    run_store.update_run_state(&run_id, &record).expect("plant");

    kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("recover with dedup");
    assert!(
        !events
            .snapshot()
            .iter()
            .any(|e| e.kind == "step_finished" && e.step_id.as_deref() == Some("a")),
        "an already-published completion event must not be backfilled again"
    );
}

#[test]
fn recovery_backfills_the_missing_failure_record_but_not_an_existing_one() {
    // An immutable *failed* execution record exists but the run was torn before
    // `store_failure_record` (the FAULT_STEP_STATE_PROJECTION window). Recovery
    // must backfill exactly one classified failure record, and must not append
    // a second one when it already exists.
    let failed_record_for = |run_id: &str, step_id: &str| transist_core::StepExecutionRecord {
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        execution: 1,
        status: "failed".to_string(),
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        retry_wait_until_ms: None,
        request: None,
        result: Some(failed_result_for(run_id, step_id, 1)),
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: Vec::new(),
    };
    let plant_inflight = |run_store: &MemoryRunStore, run_id: &str| {
        let mut record = run_store
            .load_run(run_id)
            .expect("load run")
            .expect("record present");
        record.status = KernelRunStatus::Running;
        if let Some(step) = record.steps.get_mut("a") {
            step.status = transist_core::KernelStepStatus::Running;
            step.executions = 1;
            step.last_result = None;
            step.finished_at_ms = None;
        }
        run_store.update_run_state(run_id, &record).expect("plant");
    };

    let recipe = recipe("failure-backfill-wf", vec![invoke_step("a")], None);
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);

    // Case A: no failure record yet -> backfill exactly one.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("fail-backfill-run"),
            BTreeMap::new(),
        )
        .expect("start");
    run_store
        .store_step_execution(&RunStoreStepExecution {
            run_id: run_id.clone(),
            step_id: "a".to_string(),
            execution: 1,
            state: serde_json::to_value(failed_record_for(&run_id, "a"))
                .expect("json")
                .into(),
        })
        .expect("store record");
    plant_inflight(&run_store, &run_id);

    kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("recover and backfill");
    let failures: Vec<_> = run_store
        .failures()
        .into_iter()
        .filter(|f| f.run_id == run_id && f.step_id == "a")
        .collect();
    assert_eq!(
        failures.len(),
        1,
        "the missing failure record must be backfilled exactly once"
    );
    assert_eq!(failures[0].execution_attempt, 1);

    // Case B: a failure record already exists for this ordinal; recovery must
    // not append a duplicate.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events.clone());
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("fail-dedup-run"),
            BTreeMap::new(),
        )
        .expect("start");
    run_store
        .store_step_execution(&RunStoreStepExecution {
            run_id: run_id.clone(),
            step_id: "a".to_string(),
            execution: 1,
            state: serde_json::to_value(failed_record_for(&run_id, "a"))
                .expect("json")
                .into(),
        })
        .expect("store record");
    run_store
        .store_failure_record(&transist_core::FailureRecord {
            failure_id: "pre-existing".to_string(),
            run_id: run_id.clone(),
            step_id: "a".to_string(),
            execution_attempt: 1,
            last_stable_step_id: None,
            failure_class: transist_core::FailureClass::ToolError,
            error_code: "tool_execution_error".to_string(),
            message_redacted: "boom".to_string(),
            adapter_kind: "process".to_string(),
            adapter_name: None,
            tool_name: None,
            retryable: true,
            safe_to_retry: true,
            requires_manual_attention: false,
            state_mutation_detected: false,
            first_seen_at_unix_ms: 2,
            last_seen_at_unix_ms: 2,
            occurrence_count: 1,
        })
        .expect("seed failure");
    plant_inflight(&run_store, &run_id);

    kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect("recover with failure dedup");
    let failures: Vec<_> = run_store
        .failures()
        .into_iter()
        .filter(|f| f.run_id == run_id && f.step_id == "a")
        .collect();
    assert_eq!(
        failures.len(),
        1,
        "an already-recorded failure must not be backfilled again"
    );
    assert_eq!(failures[0].failure_id, "pre-existing");
}

#[test]
fn cleanup_reconciliation_never_reruns_a_committed_cleanup_result() {
    use transist_core::PersistedCleanupRecord;
    use transist_core::StepExecutionRecord;
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    // A cleanup step whose immutable execution record is already committed must
    // be reconciled from that record on cleanup re-entry and never re-run.
    // State is constructed directly (rather than via a live cleanup) so the
    // assertion is deterministic and independent of adapter/filesystem policy.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);

    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Failed],
        step: invoke_step("teardown"),
    };
    let mut recipe = recipe("cleanup_crash_wf", vec![invoke_step("main")], None);
    recipe.cleanup = vec![cleanup_step];
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cleanup-crash-run"),
            BTreeMap::new(),
        )
        .expect("start");

    // Build a terminal failed run whose cleanup step is in-flight at ordinal 1
    // while an immutable SUCCESS record for that ordinal is already committed.
    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Failed;
    let mut teardown = transist_core::KernelStepState::pending("teardown");
    teardown.status = transist_core::KernelStepStatus::Running;
    teardown.executions = 1;
    teardown.started_at_ms = Some(1);
    let mut cleanup = PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown".to_string()],
        transist_core::CleanupPhaseState::Running,
    );
    cleanup.steps.insert("teardown".to_string(), teardown);
    record.cleanup = Some(cleanup);
    run_store.update_run_state(&run_id, &record).expect("plant");

    let committed = StepExecutionRecord {
        run_id: run_id.clone(),
        step_id: "teardown".to_string(),
        execution: 1,
        status: "success".to_string(),
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        retry_wait_until_ms: None,
        request: None,
        result: Some(success_result_for(&run_id, "teardown", 1)),
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: Vec::new(),
    };
    run_store
        .store_step_execution(&RunStoreStepExecution {
            run_id: run_id.clone(),
            step_id: "teardown".to_string(),
            execution: 1,
            state: serde_json::to_value(&committed).expect("json").into(),
        })
        .expect("store record");

    let invocations_before = execution.invocations().len();
    kernel
        .cleanup_run(tmp.path(), &home_dir, &run_id)
        .expect("cleanup recovery");
    assert_eq!(
        execution.invocations().len(),
        invocations_before,
        "a committed cleanup result must never be re-run"
    );
    assert_eq!(
        run_store.run_json(&run_id)["cleanup"]["steps"]["teardown"]["status"],
        "succeeded"
    );
    assert_eq!(
        run_store
            .step_executions()
            .iter()
            .filter(|e| e["step_id"] == "teardown" && e["execution"] == 1)
            .count(),
        1,
        "every consumed cleanup ordinal has exactly one immutable record"
    );
}

#[test]
fn cleanup_recovery_creates_interrupted_record_for_inflight_ordinal() {
    use transist_core::PersistedCleanupRecord;
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    // A cleanup step that was in flight with no completion record gets an
    // immutable `interrupted` marker for the committed ordinal, and the
    // replacement dispatch begins only at N+1.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);

    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Failed],
        step: invoke_step("teardown"),
    };
    let mut recipe = recipe("cleanup_crash_wf2", vec![invoke_step("main")], None);
    recipe.cleanup = vec![cleanup_step];
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cleanup-crash-run2"),
            BTreeMap::new(),
        )
        .expect("start");

    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Failed;
    let mut teardown = transist_core::KernelStepState::pending("teardown");
    teardown.status = transist_core::KernelStepStatus::Running;
    teardown.executions = 1;
    teardown.started_at_ms = Some(1);
    let mut cleanup = PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown".to_string()],
        transist_core::CleanupPhaseState::Running,
    );
    cleanup.steps.insert("teardown".to_string(), teardown);
    record.cleanup = Some(cleanup);
    run_store.update_run_state(&run_id, &record).expect("plant");

    // The replacement dispatch happens at N+1 (ordinal 2).
    let error = kernel
        .cleanup_run(tmp.path(), &home_dir, &run_id)
        .expect_err("failed cleanup must report an error");
    assert!(error.to_string().contains("retry `transist runs cleanup"));

    let execution_records = run_store.step_executions();
    assert!(
        execution_records.iter().any(|e| e["step_id"] == "teardown"
            && e["execution"] == 1
            && e["status"] == "interrupted"),
        "an immutable interrupted record must exist for the in-flight ordinal 1"
    );
    assert!(
        execution_records
            .iter()
            .any(|e| e["step_id"] == "teardown" && e["execution"] == 2),
        "the cleanup replacement dispatch must be recorded at N+1 (ordinal 2)"
    );
}

#[test]
fn failed_cleanup_requires_retry_and_rejects_resume() {
    use transist_core::{KernelRunRecoveryState, PersistedCleanupRecord};
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let mut recipe = recipe("cleanup_retry_wf", vec![invoke_step("main")], None);
    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Failed],
        step: invoke_step("teardown"),
    };
    recipe.cleanup = vec![cleanup_step];
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cleanup-retry-run"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Failed;
    record.cleanup = Some(PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown".to_string()],
        transist_core::CleanupPhaseState::Pending,
    ));
    run_store.update_run_state(&run_id, &record).expect("plant");

    execution.plan_result(
        &run_id,
        "teardown",
        1,
        failed_result(&run_id, "teardown", 1, "boom"),
    );
    let error = kernel
        .cleanup_run(tmp.path(), &home_dir, &run_id)
        .expect_err("failed cleanup must return an error");
    assert!(
        error
            .to_string()
            .contains("retry `transist runs cleanup cleanup-retry-run`")
    );
    assert_eq!(
        run_store.run_json(&run_id)["recovery_state"],
        "cleanup_required"
    );
    assert!(kernel.resume_run(tmp.path(), &home_dir, &run_id).is_err());

    let retry_error = kernel
        .cleanup_run(tmp.path(), &home_dir, &run_id)
        .expect_err("failed cleanup retry must report an error");
    assert!(
        retry_error
            .to_string()
            .contains("retry `transist runs cleanup cleanup-retry-run`")
    );
    assert_eq!(execution.invocations().len(), 2);
    assert_eq!(
        run_store.run_json(&run_id)["recovery_state"],
        KernelRunRecoveryState::CleanupRequired.as_str()
    );

    let prepared_error = kernel
        .run_prepared_run(tmp.path(), &home_dir, &run_id)
        .expect_err("prepared reentry must reject cleanup-required run");
    assert!(
        prepared_error
            .to_string()
            .contains("explicit cleanup has begun")
    );
}

#[test]
fn workspace_binding_explicit_cleanup_uses_persisted_workspace() {
    use transist_core::PersistedCleanupRecord;
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    let caller = tempfile::tempdir().expect("caller");
    let workspace = tempfile::tempdir().expect("workspace");
    let home_holder = tempfile::tempdir().expect("home holder");
    let home_dir = test_home_dir(&home_holder);
    std::fs::create_dir_all(&home_dir).expect("home");
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel_with_artifacts(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
        Arc::new(RunOwnedArtifacts {
            home_dir: home_dir.clone(),
        }),
    );
    let mut recipe = recipe("workspace-cleanup", vec![invoke_step("main")], None);
    recipe.workspace = Some(transist_protocol::WorkspaceBinding {
        path: workspace.path().display().to_string(),
        access: transist_protocol::WorkspaceAccessMode::Full,
    });
    recipe.cleanup = vec![RecipeCleanupStep {
        when: vec![CleanupTrigger::Failed],
        step: invoke_step("teardown"),
    }];
    let run_id = kernel
        .start_recipe(
            caller.path(),
            &home_dir,
            &recipe,
            Some("workspace-cleanup-run"),
            BTreeMap::new(),
        )
        .expect("start");
    let mut record = run_store.load_run(&run_id).unwrap().unwrap();
    record.status = KernelRunStatus::Failed;
    record.cleanup = Some(PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown".to_string()],
        transist_core::CleanupPhaseState::Pending,
    ));
    run_store.update_run_state(&run_id, &record).expect("plant");
    let unrelated_cwd = tempfile::tempdir().expect("unrelated cwd");

    kernel
        .cleanup_run(unrelated_cwd.path(), &home_dir, &run_id)
        .expect("cleanup");

    assert_eq!(
        execution
            .invocations()
            .last()
            .unwrap()
            .request
            .paths
            .workspace_dir,
        std::fs::canonicalize(workspace.path())
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn cleanup_retry_recovers_failed_step_and_preserves_succeeded_step() {
    use transist_core::{KernelRunRecoveryState, PersistedCleanupRecord};
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    // A failed run with two cleanup steps. The first cleanup pass succeeds
    // teardown1 and fails teardown2, leaving recovery_state == cleanup_required.
    // Retrying cleanup must rerun ONLY teardown2 (now succeeding), preserve the
    // already-succeeded teardown1, and transition recovery_state to
    // cleanup_completed. resume_run must remain rejected throughout.
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    // A run-owned artifact port is required so successful cleanup steps can
    // persist runtime invoke evidence inside the run-owned evidence tree;
    // `MemoryArtifacts` returns an out-of-workspace path that the cleanup
    // filesystem-policy guard denies, turning every success into a denial.
    let kernel = test_kernel_with_artifacts(
        execution.clone(),
        run_store.clone(),
        events,
        Arc::new(RunOwnedArtifacts {
            home_dir: home_dir.clone(),
        }),
    );
    let mut recipe = recipe("cleanup_retry_recover_wf", vec![invoke_step("main")], None);
    recipe.cleanup = vec![
        RecipeCleanupStep {
            when: vec![CleanupTrigger::Failed],
            step: invoke_step("teardown1"),
        },
        RecipeCleanupStep {
            when: vec![CleanupTrigger::Failed],
            step: invoke_step("teardown2"),
        },
    ];
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cleanup-retry-recover-run"),
            BTreeMap::new(),
        )
        .expect("start");

    // Plant a terminal failed run with a pending cleanup phase covering both
    // teardown steps.
    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Failed;
    record.cleanup = Some(PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown1".to_string(), "teardown2".to_string()],
        transist_core::CleanupPhaseState::Pending,
    ));
    run_store.update_run_state(&run_id, &record).expect("plant");

    // First cleanup pass: teardown1 succeeds (ordinal 1), teardown2 fails
    // (ordinal 1). The runtime must record recovery_state == cleanup_required
    // and reject resume.
    execution.plan_result(
        &run_id,
        "teardown1",
        1,
        success_result_for(&run_id, "teardown1", 1),
    );
    execution.plan_result(
        &run_id,
        "teardown2",
        1,
        failed_result(&run_id, "teardown2", 1, "boom"),
    );
    let cleanup_error = kernel
        .cleanup_run(tmp.path(), &home_dir, &run_id)
        .expect_err("failed cleanup must report an error");
    assert!(
        cleanup_error
            .to_string()
            .contains("retry `transist runs cleanup cleanup-retry-recover-run`"),
        "unexpected error: {cleanup_error}"
    );
    assert_eq!(
        run_store.run_json(&run_id)["recovery_state"],
        KernelRunRecoveryState::CleanupRequired.as_str()
    );
    assert!(
        kernel.resume_run(tmp.path(), &home_dir, &run_id).is_err(),
        "resume must be rejected while cleanup is required"
    );
    assert_eq!(
        run_store.run_json(&run_id)["cleanup"]["steps"]["teardown1"]["status"],
        "succeeded"
    );
    assert_eq!(
        run_store.run_json(&run_id)["cleanup"]["steps"]["teardown2"]["status"],
        "failed"
    );

    // Retry cleanup: teardown1 must NOT be re-executed (already succeeded);
    // teardown2 must rerun at ordinal 2 and succeed this time.
    execution.plan_result(
        &run_id,
        "teardown2",
        2,
        success_result_for(&run_id, "teardown2", 2),
    );
    kernel
        .cleanup_run(tmp.path(), &home_dir, &run_id)
        .expect("cleanup retry recovers failed step");

    // The whole cleanup phase is now complete and resume stays rejected.
    assert_eq!(
        run_store.run_json(&run_id)["recovery_state"],
        KernelRunRecoveryState::CleanupCompleted.as_str()
    );
    let prepared_error = kernel
        .run_prepared_run(tmp.path(), &home_dir, &run_id)
        .expect_err("prepared reentry must reject cleanup-completed run");
    assert!(
        prepared_error
            .to_string()
            .contains("explicit cleanup has begun")
    );
    assert_eq!(
        run_store.run_json(&run_id)["cleanup"]["steps"]["teardown1"]["status"],
        "succeeded"
    );
    assert_eq!(
        run_store.run_json(&run_id)["cleanup"]["steps"]["teardown2"]["status"],
        "succeeded"
    );
    assert!(
        kernel.resume_run(tmp.path(), &home_dir, &run_id).is_err(),
        "resume must remain rejected after cleanup has begun"
    );

    // teardown1 was executed exactly once (on the first pass) and never
    // re-dispatched on retry; teardown2 was dispatched twice (failed then
    // recovered).
    let invocations = execution.invocations();
    let teardown1_count = invocations
        .iter()
        .filter(|inv| inv.request.step.id == "teardown1")
        .count();
    let teardown2_count = invocations
        .iter()
        .filter(|inv| inv.request.step.id == "teardown2")
        .count();
    assert_eq!(
        teardown1_count, 1,
        "teardown1 must not be re-executed on retry"
    );
    assert_eq!(
        teardown2_count, 2,
        "teardown2 must be retried after failure"
    );
}

#[test]
fn run_started_run_rejects_cleanup_required_and_cleanup_completed() {
    use transist_core::{KernelRunRecoveryState, PersistedCleanupRecord};
    use transist_protocol::{CleanupTrigger, RecipeCleanupStep};

    // A failed run whose explicit cleanup has begun (recovery_state ==
    // cleanup_required) must not be admitted through `run_started_run`, which
    // would otherwise transition it back to Running. The same rejection must
    // apply once cleanup has completed (recovery_state == cleanup_completed).
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let events = Arc::new(RecordingEvents::default());
    let kernel = test_kernel(execution.clone(), run_store.clone(), events);
    let mut recipe = recipe("cleanup_reentry_wf", vec![invoke_step("main")], None);
    let cleanup_step = RecipeCleanupStep {
        when: vec![CleanupTrigger::Failed],
        step: invoke_step("teardown"),
    };
    recipe.cleanup = vec![cleanup_step];
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = test_home_dir(&tmp);
    let run_id = kernel
        .start_recipe(
            tmp.path(),
            &home_dir,
            &recipe,
            Some("cleanup-reentry-run"),
            BTreeMap::new(),
        )
        .expect("start");

    // Plant a terminal failed run with a pending cleanup phase.
    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.status = KernelRunStatus::Failed;
    record.cleanup = Some(PersistedCleanupRecord::new(
        CleanupTrigger::Failed,
        &["teardown".to_string()],
        transist_core::CleanupPhaseState::Pending,
    ));
    run_store.update_run_state(&run_id, &record).expect("plant");

    // Drive cleanup to a failing teardown so the runtime records
    // recovery_state == cleanup_required.
    execution.plan_result(
        &run_id,
        "teardown",
        1,
        failed_result(&run_id, "teardown", 1, "boom"),
    );
    let cleanup_error = kernel
        .cleanup_run(tmp.path(), &home_dir, &run_id)
        .expect_err("failed cleanup must report an error");
    assert!(
        cleanup_error
            .to_string()
            .contains("retry `transist runs cleanup cleanup-reentry-run`")
    );
    assert_eq!(
        run_store.run_json(&run_id)["recovery_state"],
        KernelRunRecoveryState::CleanupRequired.as_str()
    );

    // `run_started_run` must reject a run whose cleanup has begun.
    let reentry_error = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect_err("run_started_run must reject a cleanup-begun run");
    assert!(
        reentry_error
            .to_string()
            .contains("cannot be executed because explicit cleanup has begun"),
        "unexpected error: {reentry_error}"
    );
    // The run must remain failed, not transitioned to Running.
    assert_eq!(
        run_store.run_json(&run_id)["status"],
        KernelRunStatus::Failed.as_str()
    );

    // Now simulate a completed cleanup by planting recovery_state ==
    // cleanup_completed directly. `run_started_run` must reject this state too.
    let mut record = run_store
        .load_run(&run_id)
        .expect("load run")
        .expect("record present");
    record.recovery_state = Some(KernelRunRecoveryState::CleanupCompleted);
    run_store.update_run_state(&run_id, &record).expect("plant");

    let reentry_error = kernel
        .run_started_run(tmp.path(), &home_dir, &run_id)
        .expect_err("run_started_run must reject a cleanup-completed run");
    assert!(
        reentry_error
            .to_string()
            .contains("cannot be executed because explicit cleanup has begun"),
        "unexpected error: {reentry_error}"
    );
    assert_eq!(
        run_store.run_json(&run_id)["recovery_state"],
        KernelRunRecoveryState::CleanupCompleted.as_str()
    );
}
#[path = "automatic_cleanup/mod.rs"]
mod automatic_cleanup;
#[path = "document_recovery/mod.rs"]
mod document_recovery;
