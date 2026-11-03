// SPDX-License-Identifier: MPL-2.0
//! Replay and resume preparation helpers.

use super::*;
use crate::ControlStepKind;
use crate::domain::taxonomy::RUN_ID_NESTING_SEPARATOR;
use std::collections::BTreeSet;
use std::path::Path;

impl RuntimeKernel {
    fn reset_run_from_scratch(
        &self,
        record: &PersistedRunRecord,
        recipe: &Recipe,
    ) -> Result<PersistedRunRecord> {
        let mut snapshot = record.snapshot();
        self.ensure_snapshot_steps(
            &mut snapshot,
            &recipe
                .steps
                .iter()
                .map(|step| step.id.clone())
                .collect::<Vec<_>>(),
        );
        for step in snapshot.steps.values_mut() {
            restart_step_from_scratch(step);
        }
        snapshot.status = KernelRunStatus::Created;

        let mut restarted_record = record.clone();
        restarted_record.status = snapshot.status;
        restarted_record.steps = snapshot.steps.clone();
        restarted_record.recovery_state = None;
        restarted_record.cleanup = None;
        restarted_record.preflight = None;
        restarted_record.updated_at_ms = self.services.clock.now_unix_ms();
        Ok(restarted_record)
    }

    fn plan_run_tree_reset(
        &self,
        run_id: &str,
        record: &PersistedRunRecord,
        recipe: &Recipe,
        resets: &mut Vec<PersistedRunRecord>,
        decisions: &mut Vec<(String, String)>,
    ) -> Result<()> {
        for step in &recipe.steps {
            if let StepExecutionDescriptor::Subflow { .. } = &step.execution {
                let child_run_id = format!("{run_id}{RUN_ID_NESTING_SEPARATOR}{}", step.id);
                if let Some(child_record) =
                    load_run_record(self.services.run_store.as_ref(), &child_run_id)?
                {
                    let child_recipe = self.load_persisted_recipe(&child_run_id, &child_record)?;
                    self.plan_run_tree_reset(
                        &child_run_id,
                        &child_record,
                        &child_recipe,
                        resets,
                        decisions,
                    )?;
                }
            } else if matches!(
                &step.execution,
                StepExecutionDescriptor::Control { primitive }
                    if primitive == ControlStepKind::GateManual.as_wire_str()
            ) {
                decisions.push((run_id.to_string(), validated_gate_id_for_step(step)?));
            }
        }
        resets.push(self.reset_run_from_scratch(record, recipe)?);
        Ok(())
    }

    pub(crate) fn load_persisted_recipe(
        &self,
        run_id: &str,
        record: &PersistedRunRecord,
    ) -> Result<Recipe> {
        record.validate_contract_versions()?;
        if record.recipe_snapshot.is_none() || record.asset_lock_version == 0 {
            bail!(
                "run '{}' was created before run asset locking support; recompile the workflow and start a fresh run",
                run_id
            );
        }
        if record.uses_workflow_asset_snapshot() {
            self.validate_workflow_asset_snapshot(run_id)?;
        }
        let recipe_yaml = self
            .services
            .run_store
            .load_recipe_snapshot(run_id)
            .with_context(|| {
                format!(
                    "run '{}' cannot be resumed or replayed because its persisted run-scoped recipe snapshot is missing",
                    run_id
                )
            })?;
        let parsed: Recipe = yaml_serde::from_str(&recipe_yaml).with_context(|| {
            format!(
                "run '{}' cannot be resumed or replayed because its persisted recipe snapshot is invalid",
                run_id
            )
        })?;
        if parsed.workspace != record.workspace {
            bail!(
                "run '{}' persisted recipe workspace does not match run state workspace",
                run_id
            );
        }
        let Some(expected_snapshot) = record.recipe_snapshot.as_ref() else {
            bail!(
                "run '{}' cannot be resumed or replayed because its recipe snapshot metadata is missing",
                run_id
            );
        };
        let current_sha = sha256_hex(recipe_yaml.as_bytes());
        if current_sha != expected_snapshot.sha256
            && crate::kernel::test_mode::is_test_workflow(&record.workflow_version)
        {
            tracing::warn!(
                run_id = %run_id,
                workflow_id = %record.workflow_id,
                version = %record.workflow_version,
                path = %expected_snapshot.path,
                "reloading patched run-scoped recipe snapshot for test workflow"
            );
            return Ok(parsed);
        }
        if current_sha != expected_snapshot.sha256 {
            bail!(
                "run '{}' cannot be resumed or replayed because its persisted recipe snapshot '{}' has changed; expected digest '{}' but found '{}'",
                run_id,
                expected_snapshot.path,
                expected_snapshot.sha256,
                current_sha
            );
        }
        Ok(parsed)
    }

    fn validate_workflow_asset_snapshot(&self, run_id: &str) -> Result<()> {
        let manifest = self
            .services
            .run_store
            .read_workflow_asset_manifest(run_id)?
            .ok_or_else(|| {
                anyhow::anyhow!("run '{run_id}' is missing its workflow asset manifest")
            })?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest)
            .with_context(|| format!("run '{run_id}' has an invalid workflow asset manifest"))?;
        if manifest.get("schema").and_then(serde_json::Value::as_str)
            != Some("transist.workflow_asset_snapshot.v1")
        {
            bail!("run '{run_id}' has an unsupported workflow asset manifest schema");
        }
        let entries = manifest
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                anyhow::anyhow!("run '{run_id}' workflow asset manifest has no entries")
            })?;
        let mut paths = BTreeSet::new();
        for entry in entries {
            let path = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("workflow asset manifest entry has no path"))?;
            let relative = path
                .strip_prefix("prompts/")
                .or_else(|| path.strip_prefix("source/"))
                .ok_or_else(|| anyhow::anyhow!("workflow asset path is not namespaced: {path}"))?;
            let relative_path = Path::new(relative);
            if relative_path.as_os_str().is_empty()
                || relative_path.is_absolute()
                || relative_path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
                || !paths.insert(path.to_string())
            {
                bail!("workflow asset path is invalid or duplicated: {path}");
            }
            let sha256 = entry
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("workflow asset '{path}' has no sha256"))?;
            let bytes = self
                .services
                .run_store
                .read_workflow_asset_blob(run_id, sha256)?
                .ok_or_else(|| {
                    anyhow::anyhow!("run '{run_id}' is missing asset blob '{sha256}'")
                })?;
            if crate::kernel::sha256_hex(&bytes) != sha256 {
                bail!("workflow asset blob '{sha256}' does not match its manifest digest");
            }
        }
        Ok(())
    }

    /// Loads a validated snapshot bundle for copying into a replayed run.
    pub(crate) fn load_workflow_asset_bundle(
        &self,
        run_id: &str,
        record: &PersistedRunRecord,
    ) -> Result<Option<(Vec<u8>, Vec<crate::ports::WorkflowAssetBlob>)>> {
        if !record.uses_workflow_asset_snapshot() {
            return Ok(None);
        }
        self.validate_workflow_asset_snapshot(run_id)?;
        let manifest = self
            .services
            .run_store
            .read_workflow_asset_manifest(run_id)?
            .expect("validated workflow asset manifest");
        let manifest_value: serde_json::Value = serde_json::from_slice(&manifest)?;
        let entries = manifest_value
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .expect("validated workflow asset entries");
        let mut blobs = BTreeSet::new();
        let mut assets = Vec::new();
        for entry in entries {
            let sha256 = entry
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .expect("validated workflow asset digest");
            if !blobs.insert(sha256.to_string()) {
                continue;
            }
            let bytes = self
                .services
                .run_store
                .read_workflow_asset_blob(run_id, sha256)?
                .expect("validated workflow asset blob");
            assets.push(crate::ports::WorkflowAssetBlob {
                sha256: sha256.to_string(),
                bytes,
            });
        }
        Ok(Some((manifest, assets)))
    }

    pub(crate) fn reset_failed_run(
        &self,
        graph: &ExecutionGraph,
        snapshot: &mut KernelRunSnapshot,
    ) -> Vec<String> {
        let recoverable_steps: Vec<String> = snapshot
            .steps
            .iter()
            .filter_map(|(step_id, step)| {
                matches!(
                    step.status,
                    KernelStepStatus::Failed | KernelStepStatus::Canceled
                )
                .then_some(step_id.clone())
            })
            .collect();
        if recoverable_steps.is_empty() {
            let mut restarted_steps = snapshot
                .steps
                .iter()
                .filter_map(|(step_id, step)| {
                    (!step.status.is_terminal()).then_some(step_id.clone())
                })
                .collect::<Vec<_>>();
            restarted_steps.sort();
            for step_id in &restarted_steps {
                if let Some(step) = snapshot.steps.get_mut(step_id) {
                    restart_step_from_scratch(step);
                }
            }
            return restarted_steps;
        }
        let reset_skipped = dependency_failed_descendants(graph, snapshot, &recoverable_steps);
        let mut reset_steps = Vec::new();

        for step_id in recoverable_steps {
            if let Some(step) = snapshot.steps.get_mut(&step_id) {
                step.status = KernelStepStatus::Runnable;
                step.retry_wait_until_ms = None;
                step.skip_reason = None;
                reset_steps.push(step_id);
            }
        }
        for step_id in reset_skipped {
            if let Some(step) = snapshot.steps.get_mut(&step_id) {
                step.status = KernelStepStatus::Pending;
                step.retry_wait_until_ms = None;
                step.skip_reason = None;
                reset_steps.push(step_id);
            }
        }
        reset_steps.sort();
        reset_steps
    }

    pub(crate) fn rerun_started_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<KernelRunSummary> {
        let lease_guard =
            MutationLeaseGuard::new(self, self.acquire_mutation_lease(home_dir, run_id)?);
        let preparation = (|| -> Result<(PersistedRunRecord, Recipe)> {
            let record = load_and_rebase_run_record(
                self.services.run_store.as_ref(),
                home_dir,
                repo_root,
                run_id,
            )?
            .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
            if matches!(
                record.recovery_state,
                Some(
                    KernelRunRecoveryState::CleanupRequired
                        | KernelRunRecoveryState::CleanupCompleted
                )
            ) {
                bail!(
                    "run '{}' cannot be rerun because explicit cleanup has begun for this run; retry `transist runs cleanup {}`",
                    run_id,
                    run_id
                );
            }
            let recipe = self.load_persisted_recipe(run_id, &record)?;
            validate_persisted_authority(
                home_dir,
                repo_root,
                &recipe,
                &record.workflow_inputs,
                record.local_only_mode,
                record.capability_authority_version == 1 && !record.test_mode,
                self.services.home_settings.as_ref(),
                self.services.plugin_metadata.as_ref(),
            )?;
            // Verify run-level asset locks so a changed plugin or prompt file is
            // caught before rerun dispatches any work.
            self.verify_asset_locks(&record, &recipe, home_dir)?;
            let mut resets = Vec::new();
            let mut decisions = Vec::new();
            self.plan_run_tree_reset(run_id, &record, &recipe, &mut resets, &mut decisions)?;
            let restarted_record = resets.pop().ok_or_else(|| {
                anyhow::anyhow!("rerun reset plan did not include root run '{}'", run_id)
            })?;
            // The root reset is the durable intent for the whole tree. Persist it
            // first so any interruption leaves a re-enterable root rather than a
            // terminal root with only some descendants reset.
            persist_run_record(self.services.run_store.as_ref(), &restarted_record)?;
            for reset in resets {
                persist_run_record(self.services.run_store.as_ref(), &reset)?;
            }
            for (decision_run_id, gate_id) in decisions {
                consume_gate_decision_record(
                    self.services.run_store.as_ref(),
                    home_dir,
                    &decision_run_id,
                    &gate_id,
                )?;
            }

            Ok((restarted_record, recipe))
        })();
        let (restarted_record, recipe) = match preparation {
            Ok(prepared) => prepared,
            Err(error) => return Err(lease_guard.release_after_error(error, "rerun preparation")),
        };
        self.execute_loaded_run(
            RuntimePaths {
                repo_root,
                home_dir,
            },
            &recipe,
            restarted_record,
            lease_guard.into_inner()?,
            ExecuteLoadedRunOptions {
                run_started_payload: None,
                apply_recovery: false,
                preflight_override: None,
            },
        )
    }
}

pub(crate) fn reset_step_for_rerun(step: &mut KernelStepState) {
    step.status = KernelStepStatus::Pending;
    step.retry_wait_until_ms = None;
    step.skip_reason = None;
    step.last_request = None;
    step.last_result = None;
    step.started_at_ms = None;
    step.finished_at_ms = None;
}

/// Resets transient runtime fields of a step before a rerun while preserving
/// its lifetime execution counter and establishing a new activation baseline.
///
/// The lifetime `executions` counter is append-only: it is never reset to
/// zero, so the next dispatch starts at `N+1` and prior execution records,
/// artifacts, and evidence remain inspectable. The activation baseline is set
/// to the current lifetime count so that loop budgets (`max_iters`), scheduler
/// cyclic-entry bootstrap, and author-facing `executions` expressions operate
/// on activation-local counts starting from zero. See the kernel contract for
/// the rationale.
pub(crate) fn restart_step_from_scratch(step: &mut KernelStepState) {
    reset_step_for_rerun(step);
    step.activation_executions_baseline = step.executions;
}

pub(crate) fn dependency_failed_descendants(
    graph: &ExecutionGraph,
    snapshot: &KernelRunSnapshot,
    failed_steps: &[String],
) -> Vec<String> {
    let mut queue: VecDeque<String> = failed_steps.iter().cloned().collect();
    let mut visited: BTreeSet<String> = failed_steps.iter().cloned().collect();
    let mut descendants = BTreeSet::new();

    while let Some(step_id) = queue.pop_front() {
        for edge in outgoing_edges(graph, &step_id) {
            if edge.to == step_id {
                continue;
            }
            if !visited.insert(edge.to.clone()) {
                continue;
            }
            queue.push_back(edge.to.clone());
            if let Some(step) = snapshot.steps.get(&edge.to) {
                if step.status == KernelStepStatus::Skipped
                    && step.skip_reason == Some(KernelSkipReason::DependencyFailed)
                {
                    descendants.insert(edge.to.clone());
                }
            }
        }
    }

    descendants.into_iter().collect()
}

pub(crate) fn run_summary_from_snapshot(snapshot: &KernelRunSnapshot) -> KernelRunSummary {
    KernelRunSummary {
        run_id: snapshot.run_id.clone(),
        status: snapshot.status,
        steps: snapshot
            .steps
            .values()
            .map(|step| KernelStepSummary {
                step_id: step.step_id.clone(),
                status: step.status,
                executions: step.executions,
                exit_code: step
                    .last_result
                    .as_ref()
                    .and_then(|result| result.exit_code),
            })
            .collect(),
        lease_release_warning: None,
    }
}
