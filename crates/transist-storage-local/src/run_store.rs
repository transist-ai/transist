// SPDX-License-Identifier: MPL-2.0
use crate::atomic_io::{
    AnchoredDir, append_jsonl_durable, create_dir_all_durable, lock_directory, read_jsonl_records,
    read_regular_file_no_follow, remove_file_anchored, rename_dir_anchored, sync_directory,
    write_new_file, write_new_file_if_identical, write_replace_file,
};
use crate::fault_injection::{
    FAULT_RUN_BIRTH_AFTER_INPUTS, FAULT_RUN_BIRTH_AFTER_PROVENANCE, FAULT_RUN_BIRTH_AFTER_RECIPE,
    FAULT_RUN_BIRTH_AFTER_RENAME, FAULT_RUN_BIRTH_AFTER_STATE, FAULT_RUN_BIRTH_BEFORE_RENAME,
    check_fault,
};
use crate::invocation_staging;
use crate::run_paths;
use crate::util::{
    FAILURES_JSONL, STATE_JSON, STEP_EXECUTIONS_DIR, load_record_from_path, now_unix_ms,
    write_json, write_json_create_new, write_string_atomic,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;
use transist_core::FailureRecord;
use transist_core::PersistedRunRecord;
use transist_core::ports::{
    ExternalTransferEvidence, InvocationStagingPort, RunBirthArtifacts, RunStorePort,
    RunStoreStepExecution, RunStoreStepState,
};
use transist_protocol::GateDecisionRecord;

const RECIPE_SYNC_INTENT_FILE: &str = ".recipe-sync.intent.json";
const RECIPE_SYNC_INTENT_SCHEMA: &str = "transist.recipe-sync-intent.v1";
const WORKFLOW_ASSETS_DIR: &str = "workflow_assets";
const WORKFLOW_ASSET_BLOBS_DIR: &str = "workflow_assets/blobs";
const WORKFLOW_ASSET_MANIFEST: &str = "workflow_assets/manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecipeSyncIntent {
    schema: String,
    old_state_id: String,
    new_state_id: String,
    old_state_json: String,
    new_state_json: String,
    old_recipe_yaml: String,
    new_recipe_yaml: String,
}

/// Root configuration for local Transist storage services.
#[derive(Debug, Clone)]
pub struct LocalStorageConfig {
    /// Root directory that contains the local Transist home layout.
    pub root: PathBuf,
}

impl LocalStorageConfig {
    /// Creates local storage config with the provided root directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// Filesystem-backed implementation of the durable run store port.
#[derive(Debug)]
pub struct LocalRunStore {
    pub(crate) config: LocalStorageConfig,
}

impl LocalRunStore {
    /// Creates a local run store.
    pub fn new(config: LocalStorageConfig) -> Self {
        Self { config }
    }

    fn run_root(&self, run_id: &str) -> Result<PathBuf> {
        run_paths::resolve_run_root_or_default(&self.config.root, run_id)
    }

    fn run_root_for_record(
        &self,
        record: &PersistedRunRecord,
    ) -> Result<(PathBuf, run_paths::RunLocator)> {
        let locator = run_paths::plan_run_locator(
            &self.config.root,
            &record.run_id,
            record.lineage.as_ref(),
        )?;
        Ok((
            run_paths::runs_root(&self.config.root).join(locator.relative_path_buf()?),
            locator,
        ))
    }

    fn state_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self.run_root(run_id)?.join(STATE_JSON))
    }

    fn step_path(&self, run_id: &str, step_id: &str) -> Result<PathBuf> {
        Ok(self
            .run_root(run_id)?
            .join("steps")
            .join(format!("{step_id}.json")))
    }

    pub(crate) fn step_execution_path(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<PathBuf> {
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::RunRef, run_id)?;
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::StepId, step_id)?;
        if execution == 0 {
            bail!("execution ordinal must be positive");
        }
        Ok(self
            .run_root(run_id)?
            .join(STEP_EXECUTIONS_DIR)
            .join(step_id)
            .join(format!("execution_{execution:03}.json")))
    }

    fn failures_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self.run_root(run_id)?.join(FAILURES_JSONL))
    }

    fn recipe_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self.run_root(run_id)?.join("recipe.yaml"))
    }

    fn recipe_sync_intent_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self.run_root(run_id)?.join(RECIPE_SYNC_INTENT_FILE))
    }

    pub(crate) fn recover_recipe_sync(&self, run_id: &str) -> Result<()> {
        let root = run_paths::resolve_run_root_or_default(&self.config.root, run_id)?;
        let _guard = lock_directory(&root)?;
        self.recover_recipe_sync_locked(&root)
    }

    fn recover_recipe_sync_locked(&self, root: &std::path::Path) -> Result<()> {
        let intent_path = root.join(RECIPE_SYNC_INTENT_FILE);
        match std::fs::symlink_metadata(&intent_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed inspecting recipe sync intent: {}",
                        intent_path.display()
                    )
                });
            }
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                bail!(
                    "recipe sync intent is not a regular file: {}",
                    intent_path.display()
                );
            }
            Ok(_) => {}
        }

        let raw = read_regular_file_no_follow(&intent_path)?;
        let intent: RecipeSyncIntent = serde_json::from_slice(&raw)
            .with_context(|| format!("invalid recipe sync intent: {}", intent_path.display()))?;
        if intent.schema != RECIPE_SYNC_INTENT_SCHEMA {
            bail!(
                "unsupported recipe sync intent schema '{}' at {}",
                intent.schema,
                intent_path.display()
            );
        }
        validate_recipe_sync_pair(
            &intent.old_state_id,
            &intent.old_state_json,
            &intent.old_recipe_yaml,
        )?;
        validate_recipe_sync_pair(
            &intent.new_state_id,
            &intent.new_state_json,
            &intent.new_recipe_yaml,
        )?;
        let state_path = root.join(STATE_JSON);
        let current_state = read_regular_file_no_follow(&state_path)?;
        let current_state_id = bytes_digest(&current_state);
        // Both embedded records have passed the shared core revision validator.
        // Require the current bytes to match one of them before any repair write
        // or intent deletion, including when the other pair would be selected.
        let (state_json, recipe_yaml) = if current_state_id == intent.new_state_id {
            (&intent.new_state_json, &intent.new_recipe_yaml)
        } else if current_state_id == intent.old_state_id {
            (&intent.old_state_json, &intent.old_recipe_yaml)
        } else {
            bail!(
                "recipe sync intent state does not match old or new state: {}",
                state_path.display()
            );
        };

        write_replace_file(&root.join("recipe.yaml"), recipe_yaml.as_bytes())?;
        write_replace_file(&state_path, state_json.as_bytes())?;
        sync_directory(root)?;
        remove_file_anchored(&intent_path)?;
        check_fault(crate::fault_injection::FAULT_AFTER_REMOVE_FILE)?;
        sync_directory(root)?;
        Ok(())
    }

    fn prepare_recipe_state(&self, run_id: &str, input: &PersistedRunRecord) -> Result<Vec<u8>> {
        let existing = load_record_from_path(&self.state_path(run_id)?)?;
        let mut record = input.clone();
        for (step_id, new_step) in &record.steps {
            validate_activation_baseline(run_id, step_id, new_step, existing.steps.get(step_id))?;
            if let Some(existing_step) = existing.steps.get(step_id)
                && new_step.executions < existing_step.executions
            {
                bail!(
                    "execution counter cannot decrease for run '{}' step '{}': {} < {}",
                    run_id,
                    step_id,
                    new_step.executions,
                    existing_step.executions
                );
            }
        }
        record.refresh_executions();
        if record.created_at_ms == 0 {
            record.created_at_ms = now_unix_ms();
        }
        record.updated_at_ms = now_unix_ms();
        Ok(serde_json::to_vec_pretty(&record)?)
    }

    fn validate_home_dir(&self, home_dir: &std::path::Path) -> Result<()> {
        if home_dir != self.config.root {
            bail!(
                "run store home does not match its configured root: {} != {}",
                home_dir.display(),
                self.config.root.display()
            );
        }
        Ok(())
    }

    /// Validates an on-disk run locator against the expected serialized bytes.
    ///
    /// The locator lives in its own directory (the run index), separate from
    /// the run root, so it is anchored on its own parent with
    /// [`AnchoredDir`] — the parent is opened as a real non-symlink directory
    /// and the leaf is resolved with `O_NOFOLLOW`, so a locator whose parent
    /// or final file is swapped for a symlink is rejected rather than
    /// read through.
    fn validate_stored_locator(
        &self,
        locator_path: &std::path::Path,
        expected: &[u8],
        run_id: &str,
    ) -> Result<()> {
        let parent = locator_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("run locator is missing a parent directory"))?;
        let anchored = AnchoredDir::open(parent).with_context(|| {
            format!(
                "failed anchoring locator parent for run '{}': {}",
                run_id,
                parent.display()
            )
        })?;
        let name = locator_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("run locator is missing a file name"))?;
        let path = std::path::Path::new(name);
        let actual = anchored
            .read_regular(path)
            .with_context(|| "incomplete run birth: missing run locator".to_string())?;
        if actual != expected {
            bail!("conflicting run birth artifact: {}", locator_path.display());
        }
        Ok(())
    }
}

fn gate_history_file_gate_id(file_name: &str) -> Option<&str> {
    let stem = file_name.strip_suffix(".json")?;
    let (prefix, decision) = stem.rsplit_once('-')?;
    if !matches!(decision, "approved" | "rejected" | "request_changes") {
        return None;
    }
    let (gate_id, timestamp) = prefix.rsplit_once('-')?;
    if gate_id.is_empty() || timestamp.parse::<u64>().is_err() {
        return None;
    }
    Some(gate_id)
}

fn gate_history_file_matches(file_name: &str, gate_id: &str) -> bool {
    gate_history_file_gate_id(file_name) == Some(gate_id)
}

fn malformed_history_file_belongs_to_gate(
    path: &std::path::Path,
    file_name: &str,
    gate_id: &str,
) -> Result<bool> {
    let raw = read_regular_file_no_follow(path)
        .with_context(|| format!("failed reading gate decision history: {}", path.display()))?;
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw)
        && let Some(record_gate_id) = value.get("gate_id").and_then(serde_json::Value::as_str)
    {
        return Ok(record_gate_id == gate_id);
    }

    let suffix = file_name
        .strip_prefix(&format!("{gate_id}-"))
        .and_then(|value| value.strip_suffix(".json"))
        .unwrap_or_default();
    Ok(!suffix.contains('-'))
}

impl RunStorePort for LocalRunStore {
    fn document_store(&self) -> Option<&dyn transist_core::ports::DocumentStorePort> {
        Some(self)
    }
    fn create_run(&self, record: &PersistedRunRecord) -> Result<()> {
        let (root, locator) = self.run_root_for_record(record)?;
        create_dir_all_durable(&root)
            .with_context(|| format!("failed creating run root: {}", root.display()))?;
        let state_path = root.join(STATE_JSON);
        let locator_path = run_paths::run_locator_path(&self.config.root, &record.run_id);
        if state_path.exists() {
            if !locator_path.exists()
                && std::fs::read_to_string(&state_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<PersistedRunRecord>(&raw).ok())
                    .is_some_and(|existing| existing.run_id == record.run_id)
            {
                run_paths::write_run_locator(&self.config.root, &locator)?;
                return Ok(());
            }
            bail!("run '{}' already exists", record.run_id);
        }
        let mut record = record.clone();
        record.refresh_executions();
        write_json_create_new(&state_path, &record)?;
        // `create_run` is the legacy complete-create port. The kernel's
        // staged birth path publishes its locator separately.
        run_paths::write_run_locator(&self.config.root, &locator)?;
        Ok(())
    }

    fn create_run_staged(&self, artifacts: &RunBirthArtifacts) -> Result<()> {
        let record = artifacts.record;
        validate_workflow_asset_bundle(artifacts)?;
        let (final_root, locator) = self.run_root_for_record(record)?;
        let locator_path = run_paths::run_locator_path(&self.config.root, &record.run_id);
        match std::fs::symlink_metadata(&final_root) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("run root is a symlink: {}", final_root.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed inspecting run root: {}", final_root.display())
                });
            }
        }
        if final_root.join(STATE_JSON).exists() {
            // Hold ONE anchored descriptor on the run root across all the birth
            // reconciliation reads below. Every file (state.json, recipe.yaml,
            // context/run_inputs.json, provenance.json) resolves relative to
            // that single held descriptor, so a same-user racer that swaps the
            // run root or any intermediate component for a symlink between
            // individual reads cannot make different reads observe different
            // (attacker-supplied) objects, nor redirect a later read.
            let anchored_root = AnchoredDir::open(&final_root)
                .with_context(|| format!("failed anchoring run root: {}", final_root.display()))?;
            let mut expected_record = record.clone();
            expected_record.refresh_executions();
            let expected_state = serde_json::to_vec_pretty(&expected_record)
                .with_context(|| format!("failed serializing state for run '{}'", record.run_id))?;
            // A retried birth attempt reaches this branch with a real clock,
            // so `created_at_ms`/`updated_at_ms` legitimately differ from the
            // value persisted by the original (successful but not fully
            // published) attempt. Reconciliation is structural, not
            // timestamp-exact: every field except those two must match.
            validate_birth_json_structurally(
                &anchored_root,
                "state.json",
                &expected_state,
                "state.json",
                &["created_at_ms", "updated_at_ms"],
            )?;
            validate_birth_file(
                &anchored_root,
                "recipe.yaml",
                artifacts.recipe_yaml.as_bytes(),
                "recipe.yaml",
            )?;
            validate_birth_file(
                &anchored_root,
                "context/run_inputs.json",
                artifacts.run_inputs_json,
                "context/run_inputs.json",
            )?;
            validate_birth_json_structurally(
                &anchored_root,
                "provenance.json",
                artifacts.provenance_json,
                "provenance.json",
                &["created_at_ms", "updated_at_ms", "finalized_at_ms"],
            )?;
            validate_stored_workflow_assets(&anchored_root, artifacts)?;
            crate::document_store::validate_initial_documents(
                &final_root,
                &record.run_id,
                artifacts.recipe_yaml,
                locator.parent_run_id.is_some(),
            )?;
            if locator_path.exists() {
                let expected_locator = serde_json::to_vec_pretty(&locator).with_context(|| {
                    format!("failed serializing locator for run '{}'", record.run_id)
                })?;
                self.validate_stored_locator(&locator_path, &expected_locator, &record.run_id)?;
            }
            sync_birth_parent_directories(&final_root, &self.config.root)?;
            return Ok(());
        }
        if locator_path.exists() {
            bail!(
                "run '{}' has a locator without a complete birth root",
                record.run_id
            );
        }
        // If the final root exists without state.json (e.g. a partially
        // populated directory from a crashed previous attempt), move it
        // aside to a quarantine location so the staging rename succeeds on
        // platforms where rename(2) fails with ENOTEMPTY (macOS). The
        // quarantined directory is preserved for operator inspection and is
        // never deleted — this prevents data loss from concurrent run births,
        // runner logs, or crash-recovery evidence.
        if final_root.exists() {
            let staging_parent = self.config.root.join(crate::util::RUN_BIRTH_STAGING_DIR);
            create_dir_all_durable(&staging_parent).with_context(|| {
                format!(
                    "failed creating run-birth staging parent for quarantine: {}",
                    staging_parent.display()
                )
            })?;
            let quarantine_root = staging_parent.join(format!(
                "quarantine-{}-{}-{}",
                record.run_id,
                std::process::id(),
                now_unix_ms()
            ));
            rename_dir_anchored(
                final_root.parent().expect("final root has a parent"),
                final_root.file_name().expect("final root has a file name"),
                &staging_parent,
                quarantine_root
                    .file_name()
                    .expect("quarantine root has a file name"),
            )
            .with_context(|| {
                format!(
                    "failed quarantining pre-existing run root '{}' to '{}'",
                    final_root.display(),
                    quarantine_root.display()
                )
            })?;
            if let Some(source_parent) = final_root.parent() {
                sync_directory(source_parent)?;
            }
            sync_directory(&staging_parent)?;
        }
        // Sweep orphaned staging directories from previous crashed run
        // births before creating a new one.
        crate::util::cleanup_stale_run_birth_staging(&self.config.root)?;
        let staging_parent = self.config.root.join(crate::util::RUN_BIRTH_STAGING_DIR);
        create_dir_all_durable(&staging_parent).with_context(|| {
            format!(
                "failed creating run-birth staging parent: {}",
                staging_parent.display()
            )
        })?;
        let staging_root = staging_parent.join(format!(
            "run-birth-{}-{}",
            record.run_id,
            std::process::id()
        ));
        if staging_root.exists() {
            let _ = std::fs::remove_dir_all(&staging_root);
        }
        create_dir_all_durable(&staging_root).with_context(|| {
            format!(
                "failed creating run-birth staging dir: {}",
                staging_root.display()
            )
        })?;

        let mut staged_record = record.clone();
        staged_record.refresh_executions();
        let state_bytes = serde_json::to_vec_pretty(&staged_record)
            .with_context(|| format!("failed serializing state for run '{}'", record.run_id))?;
        write_new_file(&staging_root.join(STATE_JSON), &state_bytes)
            .with_context(|| "failed writing staged state.json")?;
        check_fault(FAULT_RUN_BIRTH_AFTER_STATE)?;

        write_new_file(
            &staging_root.join("recipe.yaml"),
            artifacts.recipe_yaml.as_bytes(),
        )
        .with_context(|| "failed writing staged recipe.yaml")?;
        check_fault(FAULT_RUN_BIRTH_AFTER_RECIPE)?;

        let inputs_parent = staging_root.join("context");
        create_dir_all_durable(&inputs_parent)
            .with_context(|| "failed creating staged context dir")?;
        write_new_file(
            &staging_root.join("context").join("run_inputs.json"),
            artifacts.run_inputs_json,
        )
        .with_context(|| "failed writing staged run_inputs.json")?;
        check_fault(FAULT_RUN_BIRTH_AFTER_INPUTS)?;

        write_new_file(
            &staging_root.join("provenance.json"),
            artifacts.provenance_json,
        )
        .with_context(|| "failed writing staged provenance.json")?;
        check_fault(FAULT_RUN_BIRTH_AFTER_PROVENANCE)?;

        write_staged_workflow_assets(&staging_root, artifacts)?;
        crate::document_store::initialize_documents(
            &staging_root,
            &record.run_id,
            artifacts.recipe_yaml,
            locator.parent_run_id.is_some(),
        )?;

        if let Some(parent) = final_root.parent() {
            create_dir_all_durable(parent).with_context(|| {
                format!(
                    "failed creating final run root parent: {}",
                    parent.display()
                )
            })?;
        }

        check_fault(FAULT_RUN_BIRTH_BEFORE_RENAME)?;
        // Publish the staged run root through anchored parent descriptors so a
        // same-user racer that swaps either direct parent for a symlink cannot
        // redirect the directory rename.
        rename_dir_anchored(
            staging_root.parent().expect("staging root has a parent"),
            staging_root
                .file_name()
                .expect("staging root has a file name"),
            final_root.parent().expect("final root has a parent"),
            final_root.file_name().expect("final root has a file name"),
        )
        .with_context(|| {
            format!(
                "failed renaming staged run root '{}' to '{}'",
                staging_root.display(),
                final_root.display()
            )
        })?;
        check_fault(FAULT_RUN_BIRTH_AFTER_RENAME)?;
        sync_birth_parent_directories(&final_root, &self.config.root)?;
        Ok(())
    }

    fn read_workflow_asset_blob(&self, run_id: &str, sha256: &str) -> Result<Option<Vec<u8>>> {
        if !is_sha256_hex(sha256) {
            bail!("workflow asset blob has invalid sha256 '{sha256}'");
        }
        let path = self
            .run_root(run_id)?
            .join(WORKFLOW_ASSET_BLOBS_DIR)
            .join(sha256);
        match read_regular_file_no_follow(&path) {
            Ok(bytes) => {
                if Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    != sha256
                {
                    bail!("workflow asset blob '{sha256}' does not match its bytes");
                }
                Ok(Some(bytes))
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|source| source.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error)
                .with_context(|| format!("failed reading workflow asset blob: {}", path.display())),
        }
    }
    fn read_workflow_asset_manifest(&self, run_id: &str) -> Result<Option<Vec<u8>>> {
        let path = self.run_root(run_id)?.join(WORKFLOW_ASSET_MANIFEST);
        match read_regular_file_no_follow(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|source| source.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error).with_context(|| {
                format!("failed reading workflow asset manifest: {}", path.display())
            }),
        }
    }
    fn supports_workflow_asset_snapshots(&self) -> bool {
        true
    }
    fn run_exists(&self, run_id: &str) -> Result<bool> {
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::RunRef, run_id)?;
        let locator_path = run_paths::run_locator_path(&self.config.root, run_id);
        match std::fs::symlink_metadata(&locator_path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                bail!(
                    "run locator is not a regular file: {}",
                    locator_path.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let legacy_root =
                    run_paths::runs_root(&self.config.root).join(run_paths::run_dir_name(run_id));
                let legacy_state = legacy_root.join(STATE_JSON);
                return match std::fs::symlink_metadata(&legacy_state) {
                    Ok(metadata) if metadata.is_file() => {
                        // A complete current birth without a locator is absent
                        // until the caller reconciles and republishes it. Older
                        // layouts lacked at least one of these birth artifacts.
                        let current_birth_complete = [
                            legacy_root.join("recipe.yaml"),
                            legacy_root.join("context/run_inputs.json"),
                            legacy_root.join("provenance.json"),
                        ]
                        .iter()
                        .all(|path| path.is_file());
                        Ok(!current_birth_complete)
                    }
                    Ok(_) => bail!(
                        "legacy run state is not a regular file: {}",
                        legacy_state.display()
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                    Err(error) => Err(error).with_context(|| {
                        format!(
                            "failed inspecting legacy run state: {}",
                            legacy_state.display()
                        )
                    }),
                };
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed inspecting run locator: {}", locator_path.display())
                });
            }
        };
        self.recover_recipe_sync(run_id)?;
        let path = self.state_path(run_id)?;
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(anyhow::anyhow!(
                "run state path is not a regular file: {}",
                path.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("failed inspecting run state: {}", path.display())),
        }
    }

    fn load_run(&self, run_id: &str) -> Result<Option<PersistedRunRecord>> {
        // Absence is anchored on the run locator: a run is discoverable only
        // once its locator is published, so a missing locator means the run
        // does not exist (`Ok(None)`). Corruption and I/O errors on a
        // locatable run propagate as errors and are never conflated with
        // absence.
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::RunRef, run_id)?;
        let locator_path = run_paths::run_locator_path(&self.config.root, run_id);
        match std::fs::symlink_metadata(&locator_path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                bail!(
                    "run locator is not a regular file: {}",
                    locator_path.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed inspecting run locator: {}", locator_path.display())
                });
            }
        }
        self.recover_recipe_sync(run_id)?;
        let path = self.state_path(run_id)?;
        // Load through the strict loader so schema, lineage-defaulting, and
        // activation-baseline invariants are validated the same way on every
        // read surface. Corruption and I/O errors propagate as errors; only
        // a genuinely absent run yields `Ok(None)`.
        load_record_from_path(&path).map(Some)
    }

    fn update_run_state(&self, run_id: &str, record: &PersistedRunRecord) -> Result<()> {
        self.recover_recipe_sync(run_id)?;
        let path = self.state_path(run_id)?;
        if let Some(parent) = path.parent() {
            create_dir_all_durable(parent).with_context(|| {
                format!("failed creating run state parent: {}", parent.display())
            })?;
        }
        let mut record = record.clone();
        // Enforce monotonic execution counters: no step's lifetime execution
        // count may decrease. This guards against stale or legacy state writes
        // that would regress an append-only counter below its prior maximum.
        let existing = if path.exists() {
            Some(load_record_from_path(&path)?)
        } else {
            None
        };
        for (step_id, new_step) in &record.steps {
            validate_activation_baseline(
                run_id,
                step_id,
                new_step,
                existing
                    .as_ref()
                    .and_then(|record| record.steps.get(step_id)),
            )?;
            if let Some(existing_step) = existing
                .as_ref()
                .and_then(|record| record.steps.get(step_id))
                && new_step.executions < existing_step.executions
            {
                bail!(
                    "execution counter cannot decrease for run '{}' step '{}': {} < {}",
                    run_id,
                    step_id,
                    new_step.executions,
                    existing_step.executions
                );
            }
        }
        record.refresh_executions();
        if record.created_at_ms == 0 {
            record.created_at_ms = now_unix_ms();
        }
        record.updated_at_ms = now_unix_ms();
        write_json(&path, &record)
    }

    fn store_step_state(&self, step_state: &RunStoreStepState) -> Result<()> {
        self.recover_recipe_sync(&step_state.run_id)?;
        let run_path = self.run_root(&step_state.run_id)?;
        create_dir_all_durable(&run_path)
            .with_context(|| format!("failed creating run root: {}", run_path.display()))?;
        let step_path = self.step_path(&step_state.run_id, &step_state.step_id)?;
        if let Some(parent) = step_path.parent() {
            create_dir_all_durable(parent)
                .with_context(|| format!("failed creating step dir: {}", parent.display()))?;
        }
        let step_json = serde_json::Value::from(step_state.state.clone());
        let new_step: transist_core::KernelStepState = serde_json::from_value(step_json.clone())
            .with_context(|| {
                format!(
                    "invalid step state for '{}:{}'",
                    step_state.run_id, step_state.step_id
                )
            })?;

        let mut record = load_record_from_path(&self.state_path(&step_state.run_id)?)?;
        // Enforce the same monotonic execution-counter guard as
        // `update_run_state`: a stale per-step write must not lower this
        // step's lifetime execution counter below its previously persisted
        // value.
        if let Some(existing_step) = record.steps.get(&step_state.step_id)
            && new_step.executions < existing_step.executions
        {
            bail!(
                "execution counter cannot decrease for run '{}' step '{}': {} < {}",
                step_state.run_id,
                step_state.step_id,
                new_step.executions,
                existing_step.executions
            );
        }
        validate_activation_baseline(
            &step_state.run_id,
            &step_state.step_id,
            &new_step,
            record.steps.get(&step_state.step_id),
        )?;

        write_json(&step_path, &step_json)?;

        let mut next_steps = record.steps.clone();
        next_steps.insert(step_state.step_id.clone(), new_step);
        record.steps = next_steps;
        record.refresh_executions();
        record.updated_at_ms = now_unix_ms();
        write_json(&self.state_path(&step_state.run_id)?, &record)
    }

    fn store_step_execution(&self, execution: &RunStoreStepExecution) -> Result<()> {
        self.recover_recipe_sync(&execution.run_id)?;
        let execution_path =
            self.step_execution_path(&execution.run_id, &execution.step_id, execution.execution)?;
        if execution_path.exists() {
            bail!(
                "execution record already exists for run '{}' step '{}' execution {}",
                execution.run_id,
                execution.step_id,
                execution.execution
            );
        }
        if let Some(parent) = execution_path.parent() {
            create_dir_all_durable(parent).with_context(|| {
                format!("failed creating step executions dir: {}", parent.display())
            })?;
        }
        let json = serde_json::Value::from(execution.state.clone());
        let record: transist_core::StepExecutionRecord = serde_json::from_value(json.clone())?;
        if record.run_id != execution.run_id
            || record.step_id != execution.step_id
            || record.execution != execution.execution
        {
            bail!("execution record identity mismatch");
        }
        self.validate_document_execution(&record)?;
        if !record.document_revisions.is_empty() {
            check_fault(crate::fault_injection::FAULT_DOCUMENT_BEFORE_COMMIT)?;
        }
        write_json_create_new(&execution_path, &json)?;
        if !record.document_revisions.is_empty() {
            check_fault(crate::fault_injection::FAULT_DOCUMENT_AFTER_COMMIT)?;
            transist_core::ports::DocumentStorePort::reconcile_document_heads(
                self,
                &execution.run_id,
            )?;
        }
        Ok(())
    }

    fn step_execution_record_exists(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<bool> {
        self.recover_recipe_sync(run_id)?;
        Ok(self
            .step_execution_path(run_id, step_id, execution)?
            .exists())
    }

    fn load_step_execution(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<Option<transist_core::StepExecutionRecord>> {
        self.recover_recipe_sync(run_id)?;
        let path = self.step_execution_path(run_id, step_id, execution)?;
        if !path.exists() {
            return Ok(None);
        }
        let raw = String::from_utf8(read_regular_file_no_follow(&path)?)
            .with_context(|| format!("failed reading execution record: {}", path.display()))?;
        let record = serde_json::from_str::<transist_core::StepExecutionRecord>(&raw)
            .with_context(|| format!("invalid execution record json: {}", path.display()))?;
        if record.run_id != run_id || record.step_id != step_id || record.execution != execution {
            bail!("execution record identity mismatch");
        }
        self.validate_loaded_document_execution(&record)?;
        Ok(Some(record))
    }

    fn load_run_events(&self, run_id: &str) -> Result<Vec<transist_protocol::RuntimeEvent>> {
        self.recover_recipe_sync(run_id)?;
        let path = self.run_root(run_id)?.join(crate::util::EVENTS_JSONL);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw_records = read_jsonl_records(&path)
            .with_context(|| format!("failed reading event stream: {}", path.display()))?;
        let mut events = Vec::new();
        for raw in raw_records {
            let event = serde_json::from_slice::<transist_protocol::RuntimeEvent>(&raw)
                .with_context(|| format!("invalid event record json: {}", path.display()))?;
            events.push(event);
        }
        Ok(events)
    }

    fn store_failure_record(&self, failure: &FailureRecord) -> Result<()> {
        self.recover_recipe_sync(&failure.run_id)?;
        let path = self.failures_path(&failure.run_id)?;
        let events_path = self
            .run_root(&failure.run_id)?
            .join(crate::util::EVENTS_JSONL);
        crate::util::repair_stream_and_note(&path, &events_path, &failure.run_id, "failures")
            .with_context(|| format!("failed repairing failure stream: {}", path.display()))?;
        let mut line = serde_json::to_vec(failure)?;
        line.push(b'\n');
        append_jsonl_durable(&path, &line)
            .with_context(|| format!("failed appending failure: {}", path.display()))?;
        Ok(())
    }

    fn load_run_failures(&self, run_id: &str) -> Result<Vec<FailureRecord>> {
        self.recover_recipe_sync(run_id)?;
        let path = self.failures_path(run_id)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw_records = read_jsonl_records(&path)
            .with_context(|| format!("failed reading failure stream: {}", path.display()))?;
        let mut failures = Vec::new();
        for raw in raw_records {
            let failure = serde_json::from_slice::<FailureRecord>(&raw)
                .with_context(|| format!("invalid failure record json: {}", path.display()))?;
            failures.push(failure);
        }
        Ok(failures)
    }

    fn write_recipe_snapshot(&self, run_id: &str, recipe_yaml: &str) -> Result<()> {
        self.recover_recipe_sync(run_id)?;
        let path = self.recipe_path(run_id)?;
        if let Some(parent) = path.parent() {
            create_dir_all_durable(parent)
                .with_context(|| format!("failed creating recipe dir: {}", parent.display()))?;
        }
        write_string_atomic(&path, recipe_yaml)
            .with_context(|| format!("failed writing recipe snapshot: {}", path.display()))
    }

    fn load_recipe_snapshot(&self, run_id: &str) -> Result<String> {
        self.recover_recipe_sync(run_id)?;
        let path = self.recipe_path(run_id)?;
        std::fs::read_to_string(&path)
            .with_context(|| format!("failed reading recipe snapshot: {}", path.display()))
    }

    fn sync_recipe_snapshot(
        &self,
        run_id: &str,
        record: &PersistedRunRecord,
        recipe_yaml: &str,
    ) -> Result<()> {
        let root = self.run_root(run_id)?;
        let _guard = lock_directory(&root)?;
        self.recover_recipe_sync_locked(&root)?;
        create_dir_all_durable(&root)
            .with_context(|| format!("failed creating recipe sync root: {}", root.display()))?;
        let state_path = root.join(STATE_JSON);
        let recipe_path = root.join("recipe.yaml");
        let old_state = read_regular_file_no_follow(&state_path)?;
        let old_recipe = String::from_utf8(read_regular_file_no_follow(&recipe_path)?)
            .context("existing recipe snapshot is not valid UTF-8")?;
        crate::document_store::validate_recipe_documents_unchanged(&old_recipe, recipe_yaml)?;
        let new_state = String::from_utf8(self.prepare_recipe_state(run_id, record)?)
            .context("serialized run state is not UTF-8")?;
        let intent = RecipeSyncIntent {
            schema: RECIPE_SYNC_INTENT_SCHEMA.to_string(),
            old_state_id: bytes_digest(&old_state),
            new_state_id: bytes_digest(new_state.as_bytes()),
            old_state_json: String::from_utf8(old_state)
                .context("existing run state is not valid UTF-8")?,
            new_state_json: new_state,
            old_recipe_yaml: old_recipe,
            new_recipe_yaml: recipe_yaml.to_string(),
        };
        let intent_path = self.recipe_sync_intent_path(run_id)?;
        write_json(&intent_path, &intent)?;
        sync_directory(&root)?;
        write_replace_file(&recipe_path, intent.new_recipe_yaml.as_bytes())?;
        write_replace_file(&state_path, intent.new_state_json.as_bytes())?;
        sync_directory(&root)?;
        remove_file_anchored(&intent_path)?;
        check_fault(crate::fault_injection::FAULT_AFTER_REMOVE_FILE)?;
        sync_directory(&root)?;
        Ok(())
    }

    fn load_external_transfer_evidence(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<Vec<ExternalTransferEvidence>> {
        self.validate_home_dir(home_dir)?;
        let run_root = self.run_root(run_id)?;
        let artifacts_root = run_root.join("artifacts");
        match std::fs::symlink_metadata(&artifacts_root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed inspecting artifacts root: {}",
                        artifacts_root.display()
                    )
                });
            }
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "artifacts root is not a real directory: {}",
                    artifacts_root.display()
                );
            }
            Ok(_) => {}
        }
        let anchored_artifacts = AnchoredDir::open(&artifacts_root)?;
        let mut evidence = Vec::new();
        collect_external_transfer_evidence(
            &anchored_artifacts,
            &artifacts_root,
            std::path::Path::new(""),
            run_id,
            &mut evidence,
        )?;
        Ok(evidence)
    }

    fn resolve_run_root(&self, home_dir: &std::path::Path, run_id: &str) -> Result<PathBuf> {
        self.validate_home_dir(home_dir)?;
        self.run_root(run_id)
    }

    fn resolve_run_root_or_default(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<PathBuf> {
        self.validate_home_dir(home_dir)?;
        run_paths::resolve_run_root_or_default(&self.config.root, run_id)
    }

    fn resolve_invocation_paths(
        &self,
        home_dir: &std::path::Path,
        workspace_dir: &std::path::Path,
        run_id: &str,
        step_id: &str,
        execution: u32,
    ) -> Result<transist_protocol::InvokePaths> {
        self.validate_home_dir(home_dir)?;
        let run_root = run_paths::resolve_run_root_or_default(&self.config.root, run_id)?;
        let execution_dir = run_paths::execution_dir_path(&run_root, step_id, execution);
        Ok(transist_protocol::InvokePaths {
            run_root: run_root.display().to_string(),
            artifacts_dir: execution_dir.join("artifacts").display().to_string(),
            evidence_dir: execution_dir.join("evidence").display().to_string(),
            workspace_dir: workspace_dir.display().to_string(),
        })
    }

    fn read_result_file(&self, path: &std::path::Path) -> Result<Vec<u8>> {
        read_regular_file_no_follow(path)
    }

    fn result_file_is_within(&self, path: &std::path::Path, root: &std::path::Path) -> bool {
        let Ok(path) = std::fs::canonicalize(path) else {
            return false;
        };
        let Ok(root) = std::fs::canonicalize(root) else {
            return false;
        };
        path.strip_prefix(root).is_ok()
    }

    fn resolve_root_run_id(&self, home_dir: &std::path::Path, run_id: &str) -> Result<String> {
        self.validate_home_dir(home_dir)?;
        run_paths::resolve_root_run_id(&self.config.root, run_id)
    }

    fn run_provenance_path(&self, home_dir: &std::path::Path, run_id: &str) -> Result<PathBuf> {
        self.validate_home_dir(home_dir)?;
        run_paths::run_provenance_path(&self.config.root, run_id)
    }

    fn gates_dir(&self, home_dir: &std::path::Path, run_id: &str) -> Result<PathBuf> {
        self.validate_home_dir(home_dir)?;
        run_paths::gates_dir(&self.config.root, run_id)
    }

    fn has_pending_gate_decision(&self, home_dir: &std::path::Path, run_id: &str) -> bool {
        if self.validate_home_dir(home_dir).is_err() {
            return false;
        }
        let Ok(gates_dir) = self.gates_dir(home_dir, run_id) else {
            return false;
        };
        let Ok(entries) = std::fs::read_dir(gates_dir) else {
            return false;
        };
        entries.filter_map(|entry| entry.ok()).any(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("json")
        })
    }

    fn gate_decision_path(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<PathBuf> {
        self.validate_home_dir(home_dir)?;
        run_paths::gate_decision_path(&self.config.root, run_id, gate_id)
    }

    fn load_gate_decision(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.validate_home_dir(home_dir)?;
        let path = self.gate_decision_path(home_dir, run_id, gate_id)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => read_regular_file_no_follow(&path)
                .map(Some)
                .with_context(|| format!("failed reading gate decision: {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed inspecting gate decision: {}", path.display())),
        }
    }

    fn load_gate_decision_history(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<Vec<Vec<u8>>> {
        self.validate_home_dir(home_dir)?;
        transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, gate_id)
            .context("invalid gate id")?;
        let history_dir = run_paths::gate_decision_history_dir(&self.config.root, run_id)?;
        match std::fs::symlink_metadata(&history_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "gate decision history directory must not be a symlink: {}",
                    history_dir.display()
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!(
                    "gate decision history path is not a directory: {}",
                    history_dir.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed inspecting gate decision history directory: {}",
                        history_dir.display()
                    )
                });
            }
        }

        let entries = std::fs::read_dir(&history_dir)
            .with_context(|| {
                format!(
                    "failed reading gate decision history directory: {}",
                    history_dir.display()
                )
            })?
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if gate_history_file_matches(file_name, gate_id) {
                paths.push(path);
                continue;
            }
            if file_name.starts_with(&format!("{gate_id}-"))
                && file_name.ends_with(".json")
                && gate_history_file_gate_id(file_name).is_none()
                && malformed_history_file_belongs_to_gate(&path, file_name, gate_id)?
            {
                bail!(
                    "malformed gate decision history filename for gate '{}': {}",
                    gate_id,
                    path.display()
                );
            }
        }
        paths.sort();

        paths
            .into_iter()
            .map(|path| {
                read_regular_file_no_follow(&path).with_context(|| {
                    format!("failed reading gate decision history: {}", path.display())
                })
            })
            .collect()
    }

    fn consume_gate_decision(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        gate_id: &str,
    ) -> Result<()> {
        self.validate_home_dir(home_dir)?;
        let path = self.gate_decision_path(home_dir, run_id, gate_id)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed inspecting gate decision: {}", path.display())
                });
            }
        }
        remove_file_anchored(&path)
            .with_context(|| format!("failed clearing gate decision: {}", path.display()))?;
        check_fault(crate::fault_injection::FAULT_AFTER_REMOVE_FILE)
            .with_context(|| "fault injected after gate decision removal".to_string())?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)
                .with_context(|| format!("failed syncing gate directory: {}", parent.display()))?;
            if let Some(run_root) = parent.parent() {
                sync_directory(run_root).with_context(|| {
                    format!("failed syncing run directory: {}", run_root.display())
                })?;
            }
        }
        Ok(())
    }

    fn store_gate_decision(
        &self,
        home_dir: &std::path::Path,
        record: &GateDecisionRecord,
    ) -> Result<()> {
        self.validate_home_dir(home_dir)?;
        if !matches!(
            record.decision.as_str(),
            "approved" | "rejected" | "request_changes"
        ) {
            bail!("unknown gate decision '{}'", record.decision);
        }
        if record.schema != "transist.gate_decision.v1" {
            bail!("unsupported gate decision schema '{}'", record.schema);
        }
        for (kind, value) in [
            (transist_protocol::IdentifierKind::RunRef, &record.run_id),
            (transist_protocol::IdentifierKind::GateId, &record.gate_id),
            (transist_protocol::IdentifierKind::StepId, &record.step_id),
        ] {
            transist_protocol::validate_identifier(kind, value)?;
        }
        let active_path = self.gate_decision_path(home_dir, &record.run_id, &record.gate_id)?;
        let history_path = run_paths::gate_decision_history_path(
            &self.config.root,
            &record.run_id,
            &record.gate_id,
            &record.decision,
            record
                .decided_at_ms
                .ok_or_else(|| anyhow::anyhow!("gate decision is missing its timestamp"))?,
        )?;
        for path in [&active_path, &history_path] {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("gate decision path has no parent"))?;
            create_dir_all_durable(parent).with_context(|| {
                format!(
                    "failed creating gate decision directory: {}",
                    parent.display()
                )
            })?;
        }
        let history_write = if record.review_request.is_some() {
            write_new_file_if_identical(&history_path, &serde_json::to_vec_pretty(record)?)
        } else {
            write_json_create_new(&history_path, record)
        };
        history_write.with_context(|| {
            format!(
                "failed writing gate decision history: {}",
                history_path.display()
            )
        })?;
        if let Err(error) = write_json(&active_path, record)
            .with_context(|| format!("failed writing gate decision: {}", active_path.display()))
        {
            // Atomic replacement may have published the active decision before
            // a later directory-sync failure. Reconcile that ambiguous outcome
            // instead of deleting its immutable audit record.
            if let Ok(raw) = read_regular_file_no_follow(&active_path)
                && serde_json::from_slice::<GateDecisionRecord>(&raw)
                    .is_ok_and(|active| active == *record)
            {
                return Ok(());
            }
            return Err(error).context(format!(
                "gate decision was not made active; audit history remains at {}",
                history_path.display()
            ));
        }
        Ok(())
    }

    fn read_run_provenance(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.validate_home_dir(home_dir)?;
        let path = self.run_provenance_path(home_dir, run_id)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => read_regular_file_no_follow(&path)
                .map(Some)
                .with_context(|| format!("failed reading run provenance: {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed inspecting run provenance: {}", path.display())),
        }
    }

    fn write_run_provenance(
        &self,
        home_dir: &std::path::Path,
        run_id: &str,
        payload: &[u8],
    ) -> Result<()> {
        self.validate_home_dir(home_dir)?;
        let path = self.run_provenance_path(home_dir, run_id)?;
        write_replace_file(&path, payload)
            .with_context(|| format!("failed writing run provenance: {}", path.display()))
    }

    fn list_run_roots(&self, home_dir: &std::path::Path) -> Result<Vec<PathBuf>> {
        self.validate_home_dir(home_dir)?;
        run_paths::list_run_roots(&self.config.root)
    }

    fn list_run_tree_roots(
        &self,
        home_dir: &std::path::Path,
        root_run_id: &str,
    ) -> Result<Vec<PathBuf>> {
        self.validate_home_dir(home_dir)?;
        run_paths::list_run_tree_roots(&self.config.root, root_run_id)
    }

    fn run_id_from_run_root(&self, run_root: &std::path::Path) -> Option<String> {
        run_paths::run_id_from_run_root(run_root)
    }

    fn rebase_persisted_run_record(
        &self,
        record: &mut PersistedRunRecord,
        home_dir: &std::path::Path,
        repo_root: &std::path::Path,
    ) -> Result<()> {
        self.validate_home_dir(home_dir)?;
        run_paths::rebase_persisted_run_record(record, &self.config.root, repo_root)
    }

    fn publish_run_locator(
        &self,
        home_dir: &std::path::Path,
        record: &PersistedRunRecord,
    ) -> Result<()> {
        self.validate_home_dir(home_dir)?;
        let (_, locator) = self.run_root_for_record(record)?;
        run_paths::write_run_locator(&self.config.root, &locator)
    }

    fn cleanup_stale_staging(&self, home_dir: &std::path::Path) -> Result<()> {
        self.validate_home_dir(home_dir)?;
        invocation_staging::cleanup_stale_staging(&self.config.root)
    }

    fn create_invocation_staging(
        &self,
        home_dir: &std::path::Path,
        request: &transist_protocol::ExecutionRequest,
    ) -> Result<Box<dyn InvocationStagingPort>> {
        self.validate_home_dir(home_dir)?;
        Ok(Box::new(invocation_staging::InvocationStaging::new(
            &self.config.root,
            request,
        )?))
    }
}

fn collect_external_transfer_evidence(
    anchored_artifacts: &AnchoredDir,
    dir: &std::path::Path,
    relative_dir: &std::path::Path,
    run_id: &str,
    evidence: &mut Vec<ExternalTransferEvidence>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("failed reading artifacts dir: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let relative_path = relative_dir.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("failed reading evidence metadata: {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "external transfer evidence tree contains a symlink: {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            collect_external_transfer_evidence(
                anchored_artifacts,
                &path,
                &relative_path,
                run_id,
                evidence,
            )?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("transist_external_transfer_") || !name.ends_with(".json") {
            continue;
        }
        let raw = String::from_utf8(anchored_artifacts.read_regular(&relative_path)?)
            .with_context(|| {
                format!(
                    "failed reading external transfer evidence: {}",
                    path.display()
                )
            })?;
        let record: transist_protocol::ExternalTransferRecord = serde_json::from_str(&raw)
            .with_context(|| format!("invalid external transfer evidence: {}", path.display()))?;
        if record.schema != transist_protocol::EXTERNAL_TRANSFER_SCHEMA_V1 {
            bail!(
                "unsupported external transfer evidence schema: {}",
                record.schema
            );
        }
        if record.run_id != run_id {
            bail!(
                "external transfer evidence run id '{}' does not match '{}'",
                record.run_id,
                run_id
            );
        }
        evidence.push(ExternalTransferEvidence {
            record,
            evidence_path: std::path::Path::new("artifacts").join(relative_path),
        });
    }
    Ok(())
}

fn bytes_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn validate_recipe_sync_pair(state_id: &str, state_json: &str, recipe_yaml: &str) -> Result<()> {
    if bytes_digest(state_json.as_bytes()) != state_id {
        bail!("recipe sync intent state digest does not match its embedded state bytes");
    }
    let state: PersistedRunRecord = serde_json::from_str(state_json)
        .context("recipe sync intent contains invalid embedded state JSON")?;
    state.validate_contract_versions()?;
    let expected_recipe = state
        .recipe_snapshot
        .as_ref()
        .map(|snapshot| snapshot.sha256.as_str())
        .ok_or_else(|| anyhow::anyhow!("recipe sync intent state lacks recipe snapshot digest"))?;
    if bytes_digest(recipe_yaml.as_bytes()) != expected_recipe {
        bail!("recipe sync intent recipe bytes do not match embedded state digest");
    }
    Ok(())
}

fn validate_activation_baseline(
    run_id: &str,
    step_id: &str,
    new_step: &transist_core::KernelStepState,
    existing_step: Option<&transist_core::KernelStepState>,
) -> Result<()> {
    // The activation baseline is the lifetime execution count captured when
    // the current rerun activation began. It is append-only like `executions`:
    // it may never exceed the lifetime count, and it may never regress below
    // the previously persisted value (the only writer is the from-scratch
    // reset, which sets it to the current lifetime count).
    if new_step.activation_executions_baseline > new_step.executions {
        bail!(
            "activation execution baseline cannot exceed execution count for run '{}' step '{}': {} > {}",
            run_id,
            step_id,
            new_step.activation_executions_baseline,
            new_step.executions
        );
    }
    if let Some(existing_step) = existing_step
        && new_step.activation_executions_baseline < existing_step.activation_executions_baseline
    {
        bail!(
            "activation execution baseline cannot decrease for run '{}' step '{}': {} < {}",
            run_id,
            step_id,
            new_step.activation_executions_baseline,
            existing_step.activation_executions_baseline
        );
    }
    Ok(())
}

fn validate_birth_file(
    anchored_root: &AnchoredDir,
    relative: &str,
    expected: &[u8],
    label: &str,
) -> Result<()> {
    // Read through the single held root descriptor (`openat` with `O_NOFOLLOW`
    // per component) so a same-user racer that replaces the run root or any
    // intermediate component with a symlink between reads cannot make
    // reconciliation compare bytes from a different object.
    let actual = anchored_root
        .read_regular(std::path::Path::new(relative))
        .with_context(|| format!("incomplete run birth: missing {label}"))?;
    if actual != expected {
        bail!("conflicting run birth artifact: {}", relative);
    }
    Ok(())
}

fn validate_workflow_asset_bundle(artifacts: &RunBirthArtifacts) -> Result<()> {
    match (
        artifacts.workflow_asset_manifest_json,
        artifacts.workflow_asset_blobs.is_empty(),
    ) {
        (None, true) => return Ok(()),
        (Some(_), false) => {}
        (None, false) => bail!("workflow asset blobs require a manifest"),
        (Some(_), true) => bail!("workflow asset manifest requires at least one blob"),
    }

    serde_json::from_slice::<serde_json::Value>(
        artifacts
            .workflow_asset_manifest_json
            .expect("manifest presence checked"),
    )
    .context("workflow asset manifest must be valid JSON")?;
    let mut seen = BTreeSet::new();
    for blob in artifacts.workflow_asset_blobs {
        if !is_sha256_hex(&blob.sha256) {
            bail!("workflow asset blob has invalid sha256 '{}", blob.sha256);
        }
        if !seen.insert(&blob.sha256) {
            bail!(
                "workflow asset bundle contains duplicate blob '{}",
                blob.sha256
            );
        }
        let actual: String = Sha256::digest(&blob.bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if actual != blob.sha256 {
            bail!(
                "workflow asset blob '{}' does not match its bytes",
                blob.sha256
            );
        }
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn write_staged_workflow_assets(
    staging_root: &std::path::Path,
    artifacts: &RunBirthArtifacts,
) -> Result<()> {
    let Some(manifest) = artifacts.workflow_asset_manifest_json else {
        return Ok(());
    };
    let assets_dir = staging_root.join(WORKFLOW_ASSETS_DIR);
    create_dir_all_durable(&assets_dir).context("failed creating staged workflow assets dir")?;
    write_new_file(&staging_root.join(WORKFLOW_ASSET_MANIFEST), manifest)
        .context("failed writing staged workflow asset manifest")?;
    let blobs_dir = staging_root.join(WORKFLOW_ASSET_BLOBS_DIR);
    create_dir_all_durable(&blobs_dir)
        .context("failed creating staged workflow asset blobs dir")?;
    for blob in artifacts.workflow_asset_blobs {
        write_new_file(&blobs_dir.join(&blob.sha256), &blob.bytes).with_context(|| {
            format!(
                "failed writing staged workflow asset blob '{}'",
                blob.sha256
            )
        })?;
    }
    Ok(())
}

fn validate_stored_workflow_assets(
    anchored_root: &AnchoredDir,
    artifacts: &RunBirthArtifacts,
) -> Result<()> {
    let Some(manifest) = artifacts.workflow_asset_manifest_json else {
        return Ok(());
    };
    validate_birth_file(
        anchored_root,
        WORKFLOW_ASSET_MANIFEST,
        manifest,
        "workflow asset manifest",
    )?;
    for blob in artifacts.workflow_asset_blobs {
        let relative = format!("{WORKFLOW_ASSET_BLOBS_DIR}/{}", blob.sha256);
        validate_birth_file(anchored_root, &relative, &blob.bytes, "workflow asset blob")?;
    }
    Ok(())
}

/// Validates a JSON birth artifact structurally rather than byte-for-byte.
///
/// A retried birth attempt after a crash (post-rename, pre-locator-publish)
/// reaches this comparison with a fresh, real clock: fields that are
/// legitimately recomputed on every attempt (e.g. `created_at_ms`) may
/// differ from the value recorded by the original attempt without the birth
/// being a genuine conflict. `volatile_top_level_keys` names the top-level
/// object keys to strip from both documents before comparing; every other
/// field — including nested structures like `steps` — must match exactly.
/// A parse failure, a non-object document, or any remaining field mismatch
/// is treated as a conflicting birth artifact.
fn validate_birth_json_structurally(
    anchored_root: &AnchoredDir,
    relative: &str,
    expected: &[u8],
    label: &str,
    volatile_top_level_keys: &[&str],
) -> Result<()> {
    // Anchored read through the single held root descriptor: see
    // `validate_birth_file` for the race rationale.
    let actual_bytes = anchored_root
        .read_regular(std::path::Path::new(relative))
        .with_context(|| format!("incomplete run birth: missing {label}"))?;
    let mut actual_value: serde_json::Value = serde_json::from_slice(&actual_bytes)
        .with_context(|| format!("incomplete run birth: {label} does not parse as JSON"))?;
    let mut expected_value: serde_json::Value = serde_json::from_slice(expected)
        .with_context(|| format!("failed parsing expected {label} for structural comparison"))?;
    for value in [&mut actual_value, &mut expected_value] {
        if let serde_json::Value::Object(map) = value {
            for key in volatile_top_level_keys {
                map.remove(*key);
            }
            // Fresh birth preflight is rerun before an unpublished birth can
            // be reconciled, so only its evaluation timestamp may differ.
            if relative == STATE_JSON
                && let Some(serde_json::Value::Object(preflight)) = map.get_mut("preflight")
            {
                preflight.remove("checked_at_ms");
            }
        }
    }
    if actual_value != expected_value {
        bail!("conflicting run birth artifact: {}", relative);
    }
    Ok(())
}

fn sync_birth_parent_directories(
    final_root: &std::path::Path,
    home_dir: &std::path::Path,
) -> Result<()> {
    let source_parent = home_dir.join(crate::util::RUN_BIRTH_STAGING_DIR);
    sync_directory(&source_parent).with_context(|| "failed syncing run-birth source parent")?;
    let destination_parent = final_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("final run root is missing a parent directory"))?;
    sync_directory(destination_parent).with_context(|| "failed syncing final run root parent")?;
    Ok(())
}
