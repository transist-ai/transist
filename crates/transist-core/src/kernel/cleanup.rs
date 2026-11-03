// SPDX-License-Identifier: MPL-2.0
//! Cleanup-phase execution and failed-run cleanup entry points.

use super::*;

/// Cleanup has its own outcome; it never replaces the primary run outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupExecutionOutcome {
    Succeeded,
    Skipped,
    Failed,
    Interrupted,
}

impl RuntimeKernel {
    /// Executes terminal cleanup for a root run.
    pub fn cleanup_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<KernelRunSummary> {
        let root_run_id = self
            .services
            .run_store
            .resolve_root_run_id(home_dir, run_id)?;
        let lease_guard =
            MutationLeaseGuard::new(self, self.acquire_mutation_lease(home_dir, &root_run_id)?);
        let preparation =
            (|| -> Result<(PersistedRunRecord, Recipe, CleanupTrigger, std::path::PathBuf)> {
            // This marker belongs to the operation that preceded explicit
            // cleanup. Capture its identity under the root lease, before any
            // preparation that could overlap a newly requested cancellation.
            let prior_cancel = self.services.control.inspect_cancel_request(&root_run_id)?;
            let record = load_and_rebase_run_record(
                self.services.run_store.as_ref(),
                home_dir,
                repo_root,
                &root_run_id,
            )?
            .ok_or_else(|| anyhow::anyhow!("run '{}' not found", root_run_id))?;
            if record
                .lineage
                .as_ref()
                .and_then(|lineage| lineage.parent_run_id.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
            {
                bail!(
                    "run '{}' is not a root run; cleanup is root-owned",
                    root_run_id
                );
            }
            if !record.status.is_terminal() {
                bail!(
                    "run '{}' cannot be cleaned up because status is '{}'",
                    root_run_id,
                    record.status.as_str()
                );
            }
            let trigger = cleanup_trigger_for_status(record.status).ok_or_else(|| {
                anyhow::anyhow!(
                    "run '{}' cannot be cleaned up from status '{}'",
                    root_run_id,
                    record.status.as_str()
                )
            })?;
            let recipe = self.load_persisted_recipe(&root_run_id, &record)?;
            let workspace = persisted_workspace_or_legacy(home_dir, repo_root, &record)?;
            validate_persisted_authority(
                home_dir,
                &workspace,
                &recipe,
                &record.workflow_inputs,
                record.local_only_mode,
                record.capability_authority_version == 1 && !record.test_mode,
                self.services.home_settings.as_ref(),
                self.services.plugin_metadata.as_ref(),
            )?;
            if let Some(prior_cancel) = prior_cancel
                && self.services.control.inspect_cancel_request(&root_run_id)?
                    .is_some_and(|current| current.request_id == prior_cancel.request_id)
            {
                // The root lease makes this the sole marker-removing owner.
                // request_cancel creates or returns an existing marker; it
                // cannot replace this identity before we remove it. Once
                // removed, a new request gets a new identity and is honored by
                // both the watcher and the interruptible cleanup step loop.
                self.services.control.clear_cancel_request(&root_run_id)?;
            }
            Ok((record, recipe, trigger, workspace))
        })();
        let (mut record, recipe, trigger, workspace) = match preparation {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(lease_guard.release_after_error(error, "run cleanup preparation"));
            }
        };
        let lease = lease_guard.into_inner()?;
        let operation = (|| {
            if trigger == CleanupTrigger::Canceled {
                self.settle_canceled_descendants(home_dir, &root_run_id)?;
            }
            if record.status == KernelRunStatus::Failed {
                // This durable boundary prevents primary work from resuming
                // while explicit failed-run cleanup is underway.
                record.recovery_state = Some(KernelRunRecoveryState::CleanupRequired);
                persist_run_record(self.services.run_store.as_ref(), &record)?;
            }
            let control_watcher = self.start_control_watcher(home_dir, &lease);
            let outcome = self.execute_cleanup_phase(
                RuntimePaths {
                    repo_root: &workspace,
                    home_dir,
                },
                &recipe,
                &mut record,
                trigger,
                Some(&lease),
                true,
            );
            drop(control_watcher);
            let outcome = outcome?;
            self.complete_failed_cleanup_recovery(&mut record, outcome)?;
            match outcome {
                CleanupExecutionOutcome::Succeeded | CleanupExecutionOutcome::Skipped => {}
                CleanupExecutionOutcome::Failed | CleanupExecutionOutcome::Interrupted => bail!(
                    "cleanup for run '{}' {}; retry `transist runs cleanup {}`",
                    root_run_id,
                    if outcome == CleanupExecutionOutcome::Interrupted {
                        "was interrupted"
                    } else {
                        "failed"
                    },
                    root_run_id
                ),
            }
            Ok(run_summary_from_snapshot(&record.snapshot()))
        })();
        self.finish_mutation_operation(operation, lease, "run cleanup")
            .map(|outcome| {
                let mut summary = outcome.value;
                summary.lease_release_warning = outcome.lease_release_warning;
                summary
            })
    }

    pub(crate) fn initialize_failed_cleanup_state(
        &self,
        recipe: &Recipe,
        record: &mut PersistedRunRecord,
    ) -> Result<()> {
        if record.status != KernelRunStatus::Failed {
            return Ok(());
        }
        if record
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.parent_run_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        {
            return Ok(());
        }
        record.recovery_state = Some(KernelRunRecoveryState::Recoverable);
        if recipe.cleanup.is_empty() {
            return Ok(());
        }

        let matched = matching_cleanup_steps(recipe, CleanupTrigger::Failed);
        if !matched.is_empty() {
            record.recovery_state = Some(KernelRunRecoveryState::CleanupRequired);
        }
        let matched_step_ids = matched
            .iter()
            .map(|step| step.id.clone())
            .collect::<Vec<_>>();
        let state = if matched_step_ids.is_empty() {
            CleanupPhaseState::Skipped
        } else {
            CleanupPhaseState::Pending
        };
        if record.cleanup.is_none() {
            let mut cleanup =
                PersistedCleanupRecord::new(CleanupTrigger::Failed, &matched_step_ids, state);
            if state == CleanupPhaseState::Skipped {
                let now = self.services.clock.now_unix_ms();
                cleanup.started_at_ms = Some(now);
                cleanup.finished_at_ms = Some(now);
            }
            record.cleanup = Some(cleanup);
        }
        Ok(())
    }

    /// Crash-consistent recovery for cleanup execution records.
    ///
    /// Mirrors [`RuntimeKernel::recover_execution_records`] for the cleanup
    /// phase, which lives inside the durable run record rather than a separate
    /// step-state projection. Runs before cleanup `planned_steps` are computed
    /// so a committed cleanup result is reconciled to its terminal state and
    /// never re-dispatched, an existing `interrupted` placeholder makes the
    /// replacement begin at `N+1`, and a consumed-but-incomplete ordinal
    /// without a record gets an immutable `interrupted` marker (guarded against
    /// the transient ordinal-zero case).
    fn recover_cleanup_execution_records(
        &self,
        run_id: &str,
        cleanup_steps: &[transist_protocol::RecipeStep],
        record: &mut PersistedRunRecord,
    ) -> Result<()> {
        if let Some(documents) = self.services.run_store.document_store() {
            documents.reconcile_document_heads(run_id)?;
        }
        let Some(cleanup) = record.cleanup.as_mut() else {
            return Ok(());
        };
        let now_ms = self.services.clock.now_unix_ms();
        let step_ids: Vec<String> = cleanup.steps.keys().cloned().collect();
        for step_id in step_ids {
            let Some(step_state) = cleanup.steps.get_mut(&step_id) else {
                continue;
            };
            let in_flight = matches!(
                step_state.status,
                KernelStepStatus::Dispatching | KernelStepStatus::Running
            );
            let ordinal = step_state.executions;
            // Transient ordinal-zero guard: a step showing as in-flight with a
            // zero execution counter never consumed an execution, so no
            // spurious `execution_000` record may be created.
            if ordinal == 0 {
                continue;
            }

            let existing = self
                .services
                .run_store
                .load_step_execution(run_id, &step_id, ordinal)?;
            if let Some(record) = existing.as_ref() {
                validate_record_identity(record, run_id, &step_id, ordinal)?;
            }

            match existing {
                Some(record) if record_is_completed(&record) => {
                    // A terminal projection can already be durable while its
                    // append-only failure/event writes are still missing.
                    // Only transient state is reprojected: a new activation's
                    // pending state must not be replaced by an older success.
                    if in_flight {
                        apply_terminal_state_from_record(step_state, &record, now_ms);
                    }
                    self.backfill_completion_event(&record, true)?;
                    // Backfill the classified failure record when the cleanup
                    // run was torn after the immutable record but before
                    // `store_failure_record`.
                    if let Some(step) = cleanup_steps.iter().find(|step| step.id == step_id) {
                        self.backfill_failure_record(&step.execution, &record)?;
                    }
                }
                Some(_) => {
                    // An `interrupted` placeholder already exists; the
                    // replacement will begin at N+1.
                }
                None if in_flight => {
                    self.persist_interrupted_execution_record(run_id, step_state, ordinal)?;
                }
                None => {}
            }
        }
        Ok(())
    }

    pub(crate) fn maybe_execute_automatic_cleanup(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        record: &mut PersistedRunRecord,
        lease: &LeaseHandle,
        trigger: CleanupTrigger,
    ) -> Result<()> {
        if record
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.parent_run_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        {
            return Ok(());
        }
        if trigger == CleanupTrigger::Canceled {
            self.settle_canceled_descendants(paths.home_dir, &lease.root_run_id)?;
        }
        // Failed runs with no matching declaration remain eligible for primary
        // retry. Matching cleanup irrevocably selects cleanup recovery before
        // any cleanup preflight, including a preflight that fails or errors.
        if trigger == CleanupTrigger::Failed && !matching_cleanup_steps(recipe, trigger).is_empty()
        {
            record.recovery_state = Some(KernelRunRecoveryState::CleanupRequired);
            persist_run_record(self.services.run_store.as_ref(), record)?;
        }
        let outcome =
            self.execute_cleanup_phase(paths, recipe, record, trigger, Some(lease), false)?;
        self.complete_failed_cleanup_recovery(record, outcome)
    }

    /// Starts cleanup eligibility for a deliberately resumed primary activation.
    /// Immutable history and lifetime ordinals survive; cleanup-only recovery
    /// never calls this reset, so committed cleanup work is not repeated there.
    pub(crate) fn reset_cleanup_for_primary_activation(
        &self,
        recipe: &Recipe,
        record: &mut PersistedRunRecord,
    ) -> Result<()> {
        let cleanup_steps = recipe
            .cleanup
            .iter()
            .map(|cleanup| cleanup.step.clone())
            .collect::<Vec<_>>();
        self.recover_cleanup_execution_records(&record.run_id.clone(), &cleanup_steps, record)?;
        if let Some(cleanup) = record.cleanup.as_mut() {
            for step in cleanup.steps.values_mut() {
                reset_step_for_rerun(step);
            }
            cleanup.state = CleanupPhaseState::Pending;
            cleanup.started_at_ms = None;
            cleanup.finished_at_ms = None;
            cleanup.preflight = None;
        }
        Ok(())
    }

    fn complete_failed_cleanup_recovery(
        &self,
        record: &mut PersistedRunRecord,
        outcome: CleanupExecutionOutcome,
    ) -> Result<()> {
        if record.status == KernelRunStatus::Failed
            && record.recovery_state == Some(KernelRunRecoveryState::CleanupRequired)
            && matches!(
                outcome,
                CleanupExecutionOutcome::Succeeded | CleanupExecutionOutcome::Skipped
            )
        {
            record.recovery_state = Some(KernelRunRecoveryState::CleanupCompleted);
            persist_run_record(self.services.run_store.as_ref(), record)?;
        }
        Ok(())
    }

    fn execute_cleanup_phase(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        record: &mut PersistedRunRecord,
        trigger: CleanupTrigger,
        lease: Option<&LeaseHandle>,
        explicit: bool,
    ) -> Result<CleanupExecutionOutcome> {
        let matched_steps = matching_cleanup_steps(recipe, trigger);
        let matched_step_ids = matched_steps
            .iter()
            .map(|step| step.id.clone())
            .collect::<Vec<_>>();

        // Reconcile before changing triggers or resetting any step state. A
        // resumed canceled run still owes a record for every prior ordinal,
        // including cleanup steps not selected by its next terminal trigger.
        if record.cleanup.is_some() || !matched_steps.is_empty() {
            let cleanup_steps = recipe
                .cleanup
                .iter()
                .map(|cleanup| cleanup.step.clone())
                .collect::<Vec<_>>();
            self.recover_cleanup_execution_records(&record.run_id.clone(), &cleanup_steps, record)?;
        }

        if recipe.cleanup.is_empty() && !explicit {
            return Ok(CleanupExecutionOutcome::Skipped);
        }

        if recipe.cleanup.is_empty() || matched_steps.is_empty() {
            if explicit || !recipe.cleanup.is_empty() {
                let now = self.services.clock.now_unix_ms();
                let cleanup = record.cleanup.get_or_insert_with(|| {
                    PersistedCleanupRecord::new(trigger, &[], CleanupPhaseState::Skipped)
                });
                cleanup.trigger = trigger;
                cleanup.state = CleanupPhaseState::Skipped;
                cleanup.started_at_ms = Some(now);
                cleanup.finished_at_ms = Some(now);
                cleanup.preflight = None;
                persist_run_record(self.services.run_store.as_ref(), record)?;
            }
            return Ok(CleanupExecutionOutcome::Skipped);
        }

        let now = self.services.clock.now_unix_ms();
        {
            let cleanup_record = record.cleanup.get_or_insert_with(|| {
                PersistedCleanupRecord::new(trigger, &matched_step_ids, CleanupPhaseState::Pending)
            });
            if cleanup_record.trigger != trigger {
                // A resumed canceled run can reach another terminal trigger.
                // Keep lifetime cleanup ordinals and prior records intact.
                cleanup_record.trigger = trigger;
                cleanup_record.state = CleanupPhaseState::Pending;
                cleanup_record.started_at_ms = None;
                cleanup_record.finished_at_ms = None;
                cleanup_record.preflight = None;
                for step in cleanup_record.steps.values_mut() {
                    if matches!(
                        step.status,
                        KernelStepStatus::Dispatching | KernelStepStatus::Running
                    ) {
                        reset_step_for_rerun(step);
                    }
                }
                for step_id in &matched_step_ids {
                    if let Some(step) = cleanup_record.steps.get_mut(step_id) {
                        reset_step_for_rerun(step);
                    }
                }
            }
            for step_id in &matched_step_ids {
                cleanup_record
                    .steps
                    .entry(step_id.clone())
                    .or_insert_with(|| KernelStepState::pending(step_id));
            }
        }

        let cleanup_record = record.cleanup.as_mut().ok_or_else(|| {
            anyhow::anyhow!(
                "run '{}' lost its cleanup record before planning",
                record.run_id
            )
        })?;
        let planned_steps = matched_steps
            .iter()
            .filter(|step| {
                cleanup_record
                    .steps
                    .get(&step.id)
                    .map(|state| state.status != KernelStepStatus::Succeeded)
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();

        if planned_steps.is_empty() {
            cleanup_record.state = CleanupPhaseState::Succeeded;
            cleanup_record.started_at_ms.get_or_insert(now);
            cleanup_record.finished_at_ms = Some(now);
            persist_run_record(self.services.run_store.as_ref(), record)?;
            return Ok(CleanupExecutionOutcome::Succeeded);
        }

        cleanup_record.state = CleanupPhaseState::Running;
        cleanup_record.started_at_ms.get_or_insert(now);
        cleanup_record.finished_at_ms = None;
        persist_run_record(self.services.run_store.as_ref(), record)?;
        self.publish_event(
            "cleanup_started",
            &record.run_id,
            None,
            serde_json::json!({
                "trigger": cleanup_trigger_name(trigger),
                "step_count": planned_steps.len(),
            })
            .into(),
        )?;
        self.publish_event(
            "cleanup_preflight_started",
            &record.run_id,
            None,
            serde_json::json!({ "trigger": cleanup_trigger_name(trigger) }).into(),
        )?;

        let preflight_recipe = Recipe {
            schema: recipe.schema.clone(),
            workflow: recipe.workflow.clone(),
            source_dir: recipe.source_dir.clone(),
            workspace: recipe.workspace.clone(),
            repositories: recipe.repositories.clone(),
            prerequisites: recipe.prerequisites.clone(),
            filesystem_policy: recipe.filesystem_policy.clone(),
            network_policy: recipe.network_policy.clone(),
            inputs: recipe.inputs.clone(),
            steps: planned_steps.clone(),
            cleanup: Vec::new(),
            graph: None,
            capabilities: planned_steps
                .iter()
                .filter_map(|step| {
                    recipe
                        .capabilities
                        .get(&step.id)
                        .cloned()
                        .map(|requests| (step.id.clone(), requests))
                })
                .collect(),
            capabilities_persisted: true,
            documents: recipe.documents.clone(),
        };
        let preflight = self.build_preflight_record(paths, &preflight_recipe, record, false)?;
        record
            .cleanup
            .as_mut()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "run '{}' lost its cleanup record before preflight persistence",
                    record.run_id
                )
            })?
            .preflight = Some(preflight.clone());
        persist_run_record(self.services.run_store.as_ref(), record)?;
        self.publish_event(
            if preflight.status == PreflightStatus::Failed {
                "cleanup_preflight_failed"
            } else {
                "cleanup_preflight_completed"
            },
            &record.run_id,
            None,
            serde_json::json!({
                "trigger": cleanup_trigger_name(trigger),
                "status": preflight.status.as_str(),
                "blocker_count": preflight.blocker_count,
                "warning_count": preflight.warning_count,
            })
            .into(),
        )?;
        if preflight.status == PreflightStatus::Failed {
            let cleanup_record = record.cleanup.as_mut().ok_or_else(|| {
                anyhow::anyhow!(
                    "run '{}' lost its cleanup record after preflight",
                    record.run_id
                )
            })?;
            cleanup_record.state = CleanupPhaseState::Failed;
            cleanup_record.finished_at_ms = Some(self.services.clock.now_unix_ms());
            persist_run_record(self.services.run_store.as_ref(), record)?;
            self.publish_event(
                "cleanup_finished",
                &record.run_id,
                None,
                serde_json::json!({
                    "trigger": cleanup_trigger_name(trigger),
                    "state": CleanupPhaseState::Failed.as_str(),
                })
                .into(),
            )?;
            return Ok(CleanupExecutionOutcome::Failed);
        }

        let mut interrupted = false;
        for step in &planned_steps {
            // Cancellation has already selected this terminal phase. Neither
            // the original marker nor repeated requests may cancel its work.
            if trigger != CleanupTrigger::Canceled
                && let Some(lease) = lease
                && self.cancel_request_pending(&lease.root_run_id)?
            {
                interrupted = true;
                break;
            }

            {
                let cleanup_record = record.cleanup.as_mut().ok_or_else(|| {
                    anyhow::anyhow!(
                        "run '{}' lost its cleanup record before step dispatch",
                        record.run_id
                    )
                })?;
                let step_state = cleanup_record
                    .steps
                    .entry(step.id.clone())
                    .or_insert_with(|| KernelStepState::pending(&step.id));
                if step_state.status != KernelStepStatus::Pending
                    && step_state.status != KernelStepStatus::Runnable
                {
                    reset_step_for_rerun(step_state);
                }
                step_state.status = KernelStepStatus::Dispatching;
                step_state.skip_reason = None;
                step_state.retry_wait_until_ms = None;
                step_state.executions = step_state.executions.saturating_add(1);
                if self.services.run_store.step_execution_record_exists(
                    &record.run_id,
                    &step.id,
                    step_state.executions,
                )? {
                    bail!(
                        "execution record already exists for run '{}' cleanup step '{}' execution {}; cannot dispatch over an immutable prior execution",
                        record.run_id,
                        step.id,
                        step_state.executions
                    );
                }
                step_state.started_at_ms = Some(self.services.clock.now_unix_ms());
                step_state.finished_at_ms = None;
                if step_transition_allowed(step_state.status, KernelStepStatus::Running) {
                    step_state.status = KernelStepStatus::Running;
                }
            }
            persist_run_record(self.services.run_store.as_ref(), record)?;
            let execution = record
                .cleanup
                .as_ref()
                .and_then(|cleanup| cleanup.steps.get(&step.id))
                .map(|state| state.executions)
                .unwrap_or(1);
            self.publish_event(
                "cleanup_step_started",
                &record.run_id,
                Some(&step.id),
                serde_json::json!({ "execution": execution }).into(),
            )?;

            let mut result = {
                match persisted_capabilities_for_step(recipe, &step.id, None).and_then(
                    |persisted| {
                        make_execution_request_with_capabilities(
                            ExecutionRequestContext {
                                repo_root: paths.repo_root,
                                home_dir: paths.home_dir,
                                recipe_source_dir: recipe.source_dir.as_deref().map(Path::new),
                                run_id: &record.run_id,
                                workflow_id: &record.workflow_id,
                                workflow_inputs: &record.workflow_inputs,
                                workflow_vars: &record.workflow_vars,
                                execution,
                                invoke_id: &self.services.ids.generate_step_id(),
                                local_only_mode: record.local_only_mode,
                            },
                            step,
                            recipe.workspace.is_some(),
                            &persisted,
                            record,
                            self.services.run_store.as_ref(),
                            self.services.plugin_metadata.as_ref(),
                            self.services.prompt_files.as_ref(),
                            Some(&record.snapshot()),
                            false,
                            &BTreeSet::new(),
                        )
                    },
                ) {
                    Ok(Some(prepared)) => {
                        let policy_workflow_ref = self.policy_workflow_ref(record)?;
                        let decision_payload = super::build_step_policy_descriptor(
                            &policy_workflow_ref,
                            &prepared.request,
                            &prepared.secret_refs,
                        )?;
                        let decision = self.services.policy.validate_step_execution(
                            &record.run_id,
                            &step.id,
                            &decision_payload,
                        )?;
                        let mut request = prepared.request.clone();
                        let execution_kind = request
                            .policy
                            .get("execution_kind")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "cleanup execution policy is missing execution_kind"
                                )
                            })?
                            .to_string();
                        let adapter_kind = request.adapter_kind;
                        let runtime_auth_payload = match &decision.payload {
                            KernelValue::Object(map) => map.get("runtime_auth_context").cloned(),
                            _ => None,
                        };
                        let filesystem_policy = request.policy.get("filesystem").cloned();
                        let network_policy = request.policy.get("network").cloned();
                        let workspace_declared = request
                            .policy
                            .get("workspace_declared")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let mut policy = serde_json::json!({
                            "execution_kind": execution_kind,
                            "adapter": adapter_kind,
                            "workspace_declared": workspace_declared,
                            "remote_secret_delivery": decision.remote_secret_policy.as_kernel_value(),
                        });
                        if let Some(runtime_auth_payload) = runtime_auth_payload {
                            policy["runtime_auth_context"] =
                                serde_json::Value::from(runtime_auth_payload);
                        }
                        if let Some(fs) = filesystem_policy {
                            if !fs.is_null() {
                                policy["filesystem"] = fs;
                            }
                        }
                        if let Some(network) = network_policy {
                            if !network.is_null() {
                                policy["network"] = network;
                            }
                        }
                        if let KernelValue::Object(payload) = &decision.payload {
                            for key in [
                                "policy_id",
                                "policy_profile",
                                "requested_capabilities",
                                "granted_capabilities",
                                "denial_reason",
                            ] {
                                if let Some(value) = payload.get(key) {
                                    policy[key] = serde_json::Value::from(value.clone());
                                }
                            }
                        }
                        request.policy = policy;
                        {
                            let cleanup_record = record.cleanup.as_mut().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "run '{}' lost its cleanup record before request persistence",
                                    record.run_id
                                )
                            })?;
                            if let Some(step_state) = cleanup_record.steps.get_mut(&step.id) {
                                step_state.last_request = Some(request.clone());
                            }
                        }
                        persist_run_record(self.services.run_store.as_ref(), record)?;
                        let deferred_validation = if decision.allowed
                            && request.metadata.get("document_bindings").is_none()
                            && !transist_protocol::step_deferred_bindings(step)
                                .map_err(anyhow::Error::msg)?
                                .is_empty()
                        {
                            self.validate_deferred_dispatch(
                                paths,
                                &request,
                                &prepared.secret_refs,
                                decision.remote_secret_policy,
                            )
                        } else {
                            Ok(())
                        };
                        if !decision.allowed {
                            let _ = self.publish_event(
                                "policy.denied",
                                &record.run_id,
                                Some(&step.id),
                                serde_json::json!({
                                    "reason_code": decision.reason_code.as_deref().unwrap_or("policy_denied"),
                                    "workflow_ref": record.workflow_id.clone(),
                                    "adapter_kind": request.adapter_kind.to_string(),
                                    "plugin_id": required_policy_string(&request.metadata, "plugin_id")?,
                                    "secret_ref": serde_json::Value::Null,
                                }).into(),
                            );
                            let _ = record_step_dispatch_denied_provenance(
                                self.services.run_store.as_ref(),
                                paths.home_dir,
                                record,
                                &step.id,
                                decision.reason_code.as_deref().unwrap_or("policy_denied"),
                                "policy_denied",
                                self.services.clock.now_unix_ms(),
                            );
                            if decision.reason_code.as_deref() == Some("capability_not_granted") {
                                policy_denied_result_with_code(
                                    &request,
                                    decision.message,
                                    CanonicalErrorCode::CapabilityNotGranted,
                                )
                            } else {
                                policy_denied_result(&request, decision.message)
                            }
                        } else if let Err(error) = deferred_validation {
                            let mut result = execution_failed_result(&request, error.to_string());
                            result.error = Some(transist_protocol::ProtocolError::new(
                                CanonicalErrorCode::ValidationError,
                                error.to_string(),
                            ));
                            result
                        } else {
                            match self.resolve_secret_refs(&prepared.secret_refs) {
                                Ok(secret_values) => {
                                    if !secret_values.is_empty()
                                        && !decision.remote_secret_policy.allows_adapter_kind(
                                            prepared.request.adapter_kind.as_str(),
                                        )
                                    {
                                        let reason_code = "remote_secret_policy";
                                        let message = format!(
                                            "remote secret delivery is denied by policy for adapter '{}'",
                                            prepared.request.adapter_kind
                                        );
                                        let _ = self.publish_event(
                                            "policy.denied",
                                            &record.run_id,
                                            Some(&step.id),
                                            serde_json::json!({
                                                "reason_code": reason_code,
                                                "workflow_ref": record.workflow_id.clone(),
                                                "adapter_kind": request.adapter_kind.to_string(),
                                                "plugin_id": required_policy_string(&request.metadata, "plugin_id")?,
                                                "secret_ref": serde_json::Value::Null,
                                            }).into(),
                                        );
                                        let _ = record_step_dispatch_denied_provenance(
                                            self.services.run_store.as_ref(),
                                            paths.home_dir,
                                            record,
                                            &step.id,
                                            reason_code,
                                            "policy_denied",
                                            self.services.clock.now_unix_ms(),
                                        );
                                        policy_denied_result(&request, Some(message))
                                    } else {
                                        let staging_result = self
                                            .services
                                            .run_store
                                            .create_invocation_staging(paths.home_dir, &request);
                                        match staging_result {
                                            Err(error) => {
                                                execution_failed_result(&request, error.to_string())
                                            }
                                            Ok(staging) => {
                                                let mut adapter_request = request.clone();
                                                let mut redaction_values = secret_values
                                                    .values()
                                                    .cloned()
                                                    .collect::<Vec<_>>();
                                                redaction_values
                                                    .extend(staging.transient_redaction_values());
                                                let redactor = Redactor::from_secrets(
                                                    redaction_values.iter().map(String::as_str),
                                                );
                                                let source_assets = if crate::dispatch::request_uses_workflow_asset_snapshot(&request)
                                                {
                                                    crate::dispatch::workflow_source_assets_for_request(
                                                        self.services.run_store.as_ref(),
                                                        &request.run_id,
                                                        &request,
                                                    )
                                                } else {
                                                    Ok(std::collections::BTreeMap::new())
                                                };
                                                match source_assets.and_then(|assets| {
                                                    staging.materialize_source_assets(&assets)?;
                                                    self.stage_request_documents(
                                                        staging.as_ref(),
                                                        &mut adapter_request,
                                                        paths.home_dir,
                                                    )
                                                }) {
                                                    Err(error) => execution_failed_result(
                                                        &request,
                                                        redactor.redact_str(&error.to_string()),
                                                    ),
                                                    Ok(()) => {
                                                        staging
                                                            .apply_to_request(&mut adapter_request);
                                                        let invocation = ExecutionInvocation {
                                                            request: adapter_request.clone(),
                                                            secret_values,
                                                            remote_secret_policy: decision
                                                                .remote_secret_policy,
                                                        };
                                                        match self.validate_staged_documents_before_dispatch(paths, &adapter_request, &prepared.secret_refs, decision.remote_secret_policy).and_then(|()| self.services.execution.execute_step(&invocation))
                                                        {
                                                            Ok(mut result) => {
                                                                if let Err(denial) = self
                                                                    .guard_result_files_against_policy(
                                                                        &adapter_request,
                                                                        &result,
                                                                        paths.repo_root,
                                                                    )
                                                                {
                                                                    let _ = self
                                                                        .publish_filesystem_policy_denial(
                                                                            &record.run_id,
                                                                            &denial,
                                                                        );
                                                                    result = policy_denied_result(
                                                                        &request,
                                                                        Some(denial.to_string()),
                                                                    );
                                                                } else if let Err(error) =
                                                                    normalize_result_file_refs(
                                                                        &mut result,
                                                                        record.trusted_execution_mode,
                                                                        |path| {
                                                                            self.services
                                                                                .run_store
                                                                                .read_result_file(path)
                                                                        },
                                                                    )
                                                                {
                                                                    result = failed_step_result(
                                                                        &request.run_id,
                                                                        &step.id,
                                                                        execution,
                                                                        error.to_string(),
                                                                    );
                                                                } else if let Err(error) =
                                                                    staging.promote(&mut result, &redactor).and_then(|()| self.capture_staged_documents(&adapter_request, &result, staging.as_ref(), &redactor))
                                                                {
                                                                    if result.status == "success" {
                                                                        result = failed_step_result(
                                                                            &request.run_id,
                                                                            &step.id,
                                                                            execution,
                                                                            error.to_string(),
                                                                        );
                                                                    } else {
                                                                        result.artifacts.clear();
                                                                        result.evidence.clear();
                                                                    }
                                                                }
                                                                if let Err(error) = staging.normalize_result(&mut result) {
                                                                    result = execution_failed_result(&request, error.to_string());
                                                                }
                                                                redactor.redact_execution_result(
                                                                    &mut result,
                                                                );
                                                                result
                                                            }
                                                            Err(error) => {
                                                                let mut result =
                                                                    execution_failed_result(
                                                                        &request,
                                                                        error.to_string(),
                                                                    );
                                                                redactor.redact_execution_result(
                                                                    &mut result,
                                                                );
                                                                result
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(SecretResolveError::ManagedPolicyDenied {
                                    secret_name,
                                    error,
                                }) => {
                                    let _ = self.publish_event(
                                        "policy.denied",
                                        &record.run_id,
                                        Some(&step.id),
                                        serde_json::json!({
                                            "reason_code": MANAGED_SECRET_DENIAL_CODE,
                                            "workflow_ref": record.workflow_id.clone(),
                                            "adapter_kind": request.adapter_kind.to_string(),
                                            "plugin_id": required_policy_string(&request.metadata, "plugin_id")?,
                                            "secret_ref": secret_name,
                                        }).into(),
                                    );
                                    let _ = record_step_dispatch_denied_provenance(
                                        self.services.run_store.as_ref(),
                                        paths.home_dir,
                                        record,
                                        &step.id,
                                        MANAGED_SECRET_DENIAL_CODE,
                                        "policy_denied",
                                        self.services.clock.now_unix_ms(),
                                    );
                                    secret_resolution_failed_result(&prepared.request, error)
                                }
                                Err(SecretResolveError::Other(error)) => {
                                    secret_resolution_failed_result(&prepared.request, error)
                                }
                            }
                        }
                    }
                    Ok(None) => bail!(
                        "cleanup invoke step '{}' did not produce an execution request",
                        step.id
                    ),
                    Err(error) => {
                        if let Some(denial) =
                            error.downcast_ref::<crate::filesystem_policy::FilesystemDenial>()
                        {
                            let _ = self.publish_filesystem_policy_denial(&record.run_id, denial);
                            let _ = record_step_dispatch_denied_provenance(
                                self.services.run_store.as_ref(),
                                paths.home_dir,
                                record,
                                &step.id,
                                "filesystem_policy_denied",
                                "filesystem_policy",
                                self.services.clock.now_unix_ms(),
                            );
                            filesystem_policy_denied_result(
                                &record.run_id,
                                &step.id,
                                execution,
                                denial,
                            )
                        } else {
                            failed_step_result(
                                &record.run_id,
                                &step.id,
                                execution,
                                error.to_string(),
                            )
                        }
                    }
                }
            };

            let request_for_evidence = record
                .cleanup
                .as_ref()
                .and_then(|cleanup| cleanup.steps.get(&step.id))
                .and_then(|state| state.last_request.as_ref());
            if let Err(error) =
                normalize_result_file_refs(&mut result, record.trusted_execution_mode, |path| {
                    self.services.run_store.read_result_file(path)
                })
            {
                if record.trusted_execution_mode {
                    let request = request_for_evidence.ok_or_else(|| {
                        anyhow::anyhow!(
                            "cleanup step '{}' has no persisted request for result verification",
                            step.id
                        )
                    })?;
                    let _ = self.publish_event(
                        "trust.denied",
                        &record.run_id,
                        Some(&step.id),
                        serde_json::json!({
                            "reason_code": "result_file_sha256_mismatch",
                            "failed_verification_type": "result_file_sha256",
                            "workflow_ref": record.workflow_id.clone(),
                            "plugin_id": required_policy_string(&request.metadata, "plugin_id")?,
                        })
                        .into(),
                    );
                    let _ = record_step_dispatch_denied_provenance(
                        self.services.run_store.as_ref(),
                        paths.home_dir,
                        record,
                        &step.id,
                        "result_file_sha256_mismatch",
                        "result_file_sha256",
                        self.services.clock.now_unix_ms(),
                    );
                }
                result = failed_step_result(&record.run_id, &step.id, execution, error.to_string());
            }
            let artifact_mutation_count = result.artifacts.len();
            let evidence_mutation_count = result.evidence.len();
            let _ = self.publish_sandbox_failure_event(&record.run_id, &step.id, &result);
            self.append_runtime_invoke_evidence(request_for_evidence, record, &mut result)?;
            let runtime_evidence = result.evidence.clone();
            if let Some(request) = request_for_evidence {
                if let Err(denial) =
                    self.guard_result_files_against_policy(request, &result, paths.repo_root)
                {
                    let _ = self.publish_filesystem_policy_denial(&record.run_id, &denial);
                    let _ = record_step_dispatch_denied_provenance(
                        self.services.run_store.as_ref(),
                        paths.home_dir,
                        record,
                        &step.id,
                        "filesystem_policy_denied",
                        "filesystem_policy",
                        self.services.clock.now_unix_ms(),
                    );
                    let mut denied_result = policy_denied_result(request, Some(denial.to_string()));
                    denied_result.evidence = runtime_evidence;
                    result = denied_result;
                }
            }
            let state_mutations = self.persist_result_files(
                &result,
                artifact_mutation_count,
                evidence_mutation_count,
            )?;

            let mut step_failed = false;
            let mut step_cancelled = false;
            let step_execution_record = {
                let cleanup_record = record.cleanup.as_mut().ok_or_else(|| {
                    anyhow::anyhow!(
                        "run '{}' lost its cleanup record before result persistence",
                        record.run_id
                    )
                })?;
                let step_state = cleanup_record.steps.get_mut(&step.id).ok_or_else(|| {
                    anyhow::anyhow!(
                        "run '{}' cleanup step '{}' is missing from its cleanup record",
                        record.run_id,
                        step.id
                    )
                })?;
                step_state.last_result = Some(result.clone());
                step_state.finished_at_ms = Some(self.services.clock.now_unix_ms());
                step_state.skip_reason = None;
                if result.status == "success" {
                    step_state.status = KernelStepStatus::Succeeded;
                    step_state.retry_wait_until_ms = None;
                } else if result.status == "cancelled" {
                    step_state.status = KernelStepStatus::Canceled;
                    step_state.retry_wait_until_ms = None;
                    step_cancelled = true;
                } else if let Some(wait_until) = self.next_retry_wait(step, step_state) {
                    step_state.status = KernelStepStatus::RetryWait;
                    step_state.retry_wait_until_ms = Some(wait_until);
                    step_failed = true;
                } else {
                    step_state.status = KernelStepStatus::Failed;
                    step_state.retry_wait_until_ms = None;
                    step_failed = true;
                }
                StepExecutionRecord {
                    document_revisions: Vec::new(),
                    run_id: record.run_id.clone(),
                    step_id: step.id.clone(),
                    execution: step_state.executions,
                    status: result.status.clone(),
                    started_at_ms: step_state.started_at_ms,
                    finished_at_ms: step_state.finished_at_ms,
                    retry_wait_until_ms: step_state.retry_wait_until_ms,
                    request: step_state.last_request.clone(),
                    result: step_state.last_result.clone(),
                    metrics_validation_errors: step_state.last_result.as_ref().and_then(|value| {
                        (value.metrics_validation_errors > 0)
                            .then_some(value.metrics_validation_errors)
                    }),
                    state_mutations,
                }
            };
            // The immutable execution record is the completion commit point for
            // cleanup too: it is persisted before the cleanup run-record
            // projection, so after a crash between the two a re-entry
            // reconciles the cleanup step's terminal state from this record
            // rather than re-running a committed step.
            persist_step_execution(self.services.run_store.as_ref(), &step_execution_record)?;
            persist_run_record(self.services.run_store.as_ref(), record)?;
            if let Some(failure_record) =
                self.failure_record_for_step(&step.execution, &step_execution_record, None)
            {
                self.services
                    .run_store
                    .store_failure_record(&failure_record)?;
            }
            self.publish_event(
                if step_cancelled {
                    "cleanup_step_cancelled"
                } else if step_failed {
                    "cleanup_step_failed"
                } else {
                    "cleanup_step_finished"
                },
                &record.run_id,
                Some(&step.id),
                serde_json::json!({
                    "execution": step_execution_record.execution,
                    "status": result.status,
                    "exit_code": result.exit_code,
                })
                .into(),
            )?;
            if step_cancelled {
                interrupted = true;
                break;
            }
        }

        let cleanup_state = {
            let cleanup_record = record.cleanup.as_mut().ok_or_else(|| {
                anyhow::anyhow!(
                    "run '{}' lost its cleanup record before completion",
                    record.run_id
                )
            })?;
            cleanup_record.state = if !interrupted
                && matched_step_ids.iter().all(|id| {
                    cleanup_record
                        .steps
                        .get(id)
                        .is_some_and(|step| step.status == KernelStepStatus::Succeeded)
                }) {
                CleanupPhaseState::Succeeded
            } else {
                CleanupPhaseState::Failed
            };
            cleanup_record.finished_at_ms = Some(self.services.clock.now_unix_ms());
            cleanup_record.state
        };
        persist_run_record(self.services.run_store.as_ref(), record)?;
        self.publish_event(
            "cleanup_finished",
            &record.run_id,
            None,
            serde_json::json!({
                "trigger": cleanup_trigger_name(trigger),
                "state": cleanup_state.as_str(),
                "interrupted": interrupted,
            })
            .into(),
        )?;
        Ok(if interrupted {
            CleanupExecutionOutcome::Interrupted
        } else if cleanup_state == CleanupPhaseState::Succeeded {
            CleanupExecutionOutcome::Succeeded
        } else {
            CleanupExecutionOutcome::Failed
        })
    }
}

fn cleanup_trigger_for_status(status: KernelRunStatus) -> Option<CleanupTrigger> {
    match status {
        KernelRunStatus::Success => Some(CleanupTrigger::Success),
        KernelRunStatus::Canceled => Some(CleanupTrigger::Canceled),
        KernelRunStatus::Failed => Some(CleanupTrigger::Failed),
        _ => None,
    }
}

fn cleanup_trigger_name(trigger: CleanupTrigger) -> &'static str {
    match trigger {
        CleanupTrigger::Success => "success",
        CleanupTrigger::Canceled => "canceled",
        CleanupTrigger::Failed => "failed",
    }
}

fn matching_cleanup_steps(
    recipe: &Recipe,
    trigger: CleanupTrigger,
) -> Vec<transist_protocol::RecipeStep> {
    recipe
        .cleanup
        .iter()
        .filter(|cleanup| cleanup.when.contains(&trigger))
        .map(|cleanup| cleanup.step.clone())
        .collect()
}

fn required_policy_string(values: &serde_json::Value, key: &str) -> Result<String> {
    values
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("policy data is missing non-empty string '{key}'"))
}
