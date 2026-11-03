// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::sync::Arc;
use transist_core::FailureRecord;
use transist_core::PersistedRunRecord;
use transist_core::ports::{
    EventObserver, EventPort, ExternalTransferEvidence, RunBirthArtifacts, RunStorePort,
    RunStoreStepExecution, RunStoreStepState, RunWriteObserver,
};
use transist_protocol::RuntimeEvent;
pub struct ObservedRunStore {
    inner: Arc<dyn RunStorePort>,
    observers: Vec<Arc<dyn RunWriteObserver>>,
}
impl ObservedRunStore {
    /// Wraps a run store and notifies observers after durable writes.
    pub fn new(inner: Arc<dyn RunStorePort>, observers: Vec<Arc<dyn RunWriteObserver>>) -> Self {
        Self { inner, observers }
    }
}
impl RunStorePort for ObservedRunStore {
    fn document_store(&self) -> Option<&dyn transist_core::ports::DocumentStorePort> {
        self.inner.document_store()
    }
    fn create_run(&self, record: &PersistedRunRecord) -> Result<()> {
        self.inner.create_run(record)?;
        for observer in &self.observers {
            observer.on_run_write(&record.run_id);
        }
        Ok(())
    }

    fn create_run_staged(&self, artifacts: &RunBirthArtifacts) -> Result<()> {
        self.inner.create_run_staged(artifacts)?;
        for observer in &self.observers {
            observer.on_run_write(&artifacts.record.run_id);
        }
        Ok(())
    }

    fn read_workflow_asset_blob(&self, run_id: &str, sha256: &str) -> Result<Option<Vec<u8>>> {
        self.inner.read_workflow_asset_blob(run_id, sha256)
    }

    fn read_workflow_asset_manifest(&self, run_id: &str) -> Result<Option<Vec<u8>>> {
        self.inner.read_workflow_asset_manifest(run_id)
    }

    fn supports_workflow_asset_snapshots(&self) -> bool {
        self.inner.supports_workflow_asset_snapshots()
    }

    fn run_exists(&self, run_id: &str) -> Result<bool> {
        self.inner.run_exists(run_id)
    }
    fn load_run(&self, run_id: &str) -> Result<Option<transist_core::PersistedRunRecord>> {
        self.inner.load_run(run_id)
    }
    fn update_run_state(&self, run_id: &str, record: &PersistedRunRecord) -> Result<()> {
        self.inner.update_run_state(run_id, record)?;
        for observer in &self.observers {
            observer.on_run_write(run_id);
        }
        Ok(())
    }

    fn sync_recipe_snapshot(
        &self,
        run_id: &str,
        record: &PersistedRunRecord,
        recipe_yaml: &str,
    ) -> Result<()> {
        self.inner
            .sync_recipe_snapshot(run_id, record, recipe_yaml)?;
        for observer in &self.observers {
            observer.on_run_write(run_id);
        }
        Ok(())
    }

    fn store_step_state(&self, step_state: &RunStoreStepState) -> Result<()> {
        self.inner.store_step_state(step_state)?;
        for observer in &self.observers {
            observer.on_run_write(&step_state.run_id);
        }
        Ok(())
    }
    fn store_step_execution(&self, execution: &RunStoreStepExecution) -> Result<()> {
        self.inner.store_step_execution(execution)?;
        for observer in &self.observers {
            observer.on_run_write(&execution.run_id);
        }
        Ok(())
    }
    fn step_execution_record_exists(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<bool> {
        self.inner
            .step_execution_record_exists(run_id, step_id, execution)
    }
    fn load_step_execution(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<Option<transist_core::StepExecutionRecord>> {
        self.inner.load_step_execution(run_id, step_id, execution)
    }
    fn load_run_events(&self, run_id: &str) -> Result<Vec<RuntimeEvent>> {
        self.inner.load_run_events(run_id)
    }
    fn store_failure_record(&self, failure: &FailureRecord) -> Result<()> {
        self.inner.store_failure_record(failure)?;
        for observer in &self.observers {
            observer.on_run_write(&failure.run_id);
        }
        Ok(())
    }
    fn load_run_failures(&self, run_id: &str) -> Result<Vec<FailureRecord>> {
        self.inner.load_run_failures(run_id)
    }
    fn write_recipe_snapshot(&self, run_id: &str, recipe_yaml: &str) -> Result<()> {
        self.inner.write_recipe_snapshot(run_id, recipe_yaml)?;
        for observer in &self.observers {
            observer.on_run_write(run_id);
        }
        Ok(())
    }

    fn load_recipe_snapshot(&self, run_id: &str) -> Result<String> {
        self.inner.load_recipe_snapshot(run_id)
    }

    fn load_external_transfer_evidence(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<Vec<ExternalTransferEvidence>> {
        self.inner.load_external_transfer_evidence(home_dir, run_id)
    }

    fn resolve_run_root(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<std::path::PathBuf> {
        self.inner.resolve_run_root(home_dir, run_id)
    }

    fn resolve_run_root_or_default(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<std::path::PathBuf> {
        self.inner.resolve_run_root_or_default(home_dir, run_id)
    }

    fn resolve_invocation_paths(
        &self,
        home_dir: &std::path::Path,
        workspace_dir: &std::path::Path,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<transist_protocol::InvokePaths> {
        self.inner
            .resolve_invocation_paths(home_dir, workspace_dir, run_id, step_id, execution)
    }

    fn read_result_file(&self, path: &std::path::Path) -> Result<Vec<u8>> {
        self.inner.read_result_file(path)
    }

    fn result_file_is_within(&self, path: &std::path::Path, root: &std::path::Path) -> bool {
        self.inner.result_file_is_within(path, root)
    }

    fn resolve_root_run_id(&self, home_dir: &std::path::Path, run_id: &str) -> Result<String> {
        self.inner.resolve_root_run_id(home_dir, run_id)
    }

    fn run_provenance_path(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<std::path::PathBuf> {
        self.inner.run_provenance_path(home_dir, run_id)
    }

    fn gates_dir(&self, home_dir: &std::path::Path, run_id: &str) -> Result<std::path::PathBuf> {
        self.inner.gates_dir(home_dir, run_id)
    }

    fn has_pending_gate_decision(&self, home_dir: &std::path::Path, run_id: &str) -> bool {
        self.inner.has_pending_gate_decision(home_dir, run_id)
    }

    fn gate_decision_path(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<std::path::PathBuf> {
        self.inner.gate_decision_path(home_dir, run_id, gate_id)
    }

    fn load_gate_decision(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.inner.load_gate_decision(home_dir, run_id, gate_id)
    }

    fn load_gate_decision_history(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<Vec<Vec<u8>>> {
        self.inner
            .load_gate_decision_history(home_dir, run_id, gate_id)
    }

    fn consume_gate_decision(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<()> {
        self.inner.consume_gate_decision(home_dir, run_id, gate_id)
    }

    fn store_gate_decision(
        &self,
        home_dir: &std::path::Path,
        record: &transist_protocol::GateDecisionRecord,
    ) -> Result<()> {
        self.inner.store_gate_decision(home_dir, record)?;
        for observer in &self.observers {
            observer.on_run_write(&record.run_id);
        }
        Ok(())
    }

    fn read_run_provenance(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.inner.read_run_provenance(home_dir, run_id)
    }

    fn write_run_provenance(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        payload: &[u8],
    ) -> Result<()> {
        self.inner.write_run_provenance(home_dir, run_id, payload)
    }

    fn list_run_roots(&self, home_dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
        self.inner.list_run_roots(home_dir)
    }

    fn run_id_from_run_root(&self, run_root: &std::path::Path) -> Option<String> {
        self.inner.run_id_from_run_root(run_root)
    }

    fn rebase_persisted_run_record(
        &self,
        record: &mut PersistedRunRecord,
        home_dir: &std::path::Path,
        repo_root: &std::path::Path,
    ) -> Result<()> {
        self.inner
            .rebase_persisted_run_record(record, home_dir, repo_root)
    }

    fn publish_run_locator(
        &self,
        home_dir: &std::path::Path,
        record: &PersistedRunRecord,
    ) -> Result<()> {
        self.inner.publish_run_locator(home_dir, record)
    }

    fn cleanup_stale_staging(&self, home_dir: &std::path::Path) -> Result<()> {
        self.inner.cleanup_stale_staging(home_dir)
    }

    fn create_invocation_staging(
        &self,
        home_dir: &std::path::Path,
        request: &transist_protocol::ExecutionRequest,
    ) -> Result<Box<dyn transist_core::ports::InvocationStagingPort>> {
        self.inner.create_invocation_staging(home_dir, request)
    }
}
pub struct ObservedEventSink {
    inner: Arc<dyn EventPort>,
    observers: Vec<Arc<dyn EventObserver>>,
}
impl ObservedEventSink {
    /// Wraps an event sink and notifies observers after publish.
    pub fn new(inner: Arc<dyn EventPort>, observers: Vec<Arc<dyn EventObserver>>) -> Self {
        Self { inner, observers }
    }
}
impl EventPort for ObservedEventSink {
    fn publish(&self, event: &RuntimeEvent) -> Result<()> {
        self.inner.publish(event)?;
        for observer in &self.observers {
            observer.on_event(event);
        }
        Ok(())
    }
}
