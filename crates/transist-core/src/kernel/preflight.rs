// SPDX-License-Identifier: MPL-2.0
//! Preflight orchestration and preflight-check helpers.

use super::*;
use crate::domain::taxonomy::RUN_ID_NESTING_SEPARATOR;
use transist_prerequisites::{
    EvaluationContext, PrerequisiteScope, PrerequisiteStatus as HostPrerequisiteStatus,
    evaluate_prerequisites,
};

const MAX_SUBFLOW_PREFLIGHT_DEPTH: usize = 8;

struct PreparedChildPreflight {
    recipe: Recipe,
    record: PersistedRunRecord,
    snapshot: KernelRunSnapshot,
    deferred_inputs: BTreeSet<String>,
}

enum ChildPreflightContext {
    Prepared(Box<PreparedChildPreflight>),
    Blocked(PreflightCheck),
}

struct ParentSubflowPreflightContext<'a> {
    paths: RuntimePaths<'a>,
    parent_record: &'a PersistedRunRecord,
    parent_snapshot: &'a KernelRunSnapshot,
    parent_step: &'a transist_protocol::RecipeStep,
    parent_step_state: &'a KernelStepState,
    child_recipe_template: &'a Recipe,
    parent_deferred_inputs: &'a BTreeSet<String>,
}

impl RuntimeKernel {
    /// Runs prepare-time preflight for a persisted run, persists the result,
    /// and marks the run failed when blockers are found.
    pub fn prepare_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<PreflightReport> {
        let lease = self.acquire_mutation_lease(home_dir, run_id)?;
        let result = (|| {
            let record = load_run_record(self.services.run_store.as_ref(), run_id)?
                .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
            let recipe = self.load_persisted_recipe(run_id, &record)?;
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
            self.prepare_loaded_run(&workspace, home_dir, &recipe, record)
        })();
        self.finish_mutation_operation(result, lease, "prepare-time preflight")
            .map(|outcome| {
                let mut report = outcome.value;
                report.lease_release_warning = outcome.lease_release_warning;
                report
            })
    }

    pub(crate) fn prepare_loaded_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        record: PersistedRunRecord,
    ) -> Result<PreflightReport> {
        record.validate_contract_versions()?;
        let run_id = record.run_id.clone();
        let effective_record =
            self.effective_record_for_preflight(home_dir, repo_root, &run_id, recipe, &record)?;
        self.publish_event("run_preflight_started", &run_id, None, KernelValue::Null)?;
        self.services
            .telemetry
            .record(&TelemetryEvent::PreflightStarted {
                run_id: run_id.clone(),
            });
        let preflight = self.build_preflight_record(
            RuntimePaths {
                repo_root,
                home_dir,
            },
            recipe,
            &effective_record,
            !matches!(record.status, KernelRunStatus::Failed),
        )?;
        let mut stored_record = record.clone();
        stored_record.preflight = Some(preflight.clone());
        if preflight.status == PreflightStatus::Failed
            && run_transition_allowed(stored_record.status, KernelRunStatus::Failed)
        {
            stored_record.status = KernelRunStatus::Failed;
        }
        stored_record.updated_at_ms = self.services.clock.now_unix_ms();
        persist_run_record(self.services.run_store.as_ref(), &stored_record)?;
        self.publish_preflight_outcome(&run_id, &preflight)?;
        self.services
            .telemetry
            .record(&TelemetryEvent::PreflightFinished {
                run_id: run_id.clone(),
                status: preflight.status.as_str().to_string(),
            });
        if preflight.status == PreflightStatus::Failed {
            self.publish_event(
                "run_stopped",
                &run_id,
                None,
                serde_json::json!({ "status": stored_record.status.as_str() }).into(),
            )?;
        }
        Ok(preflight.to_report(run_id))
    }

    /// Runs the generic preflight routine for a persisted run without executing steps.
    ///
    /// Unlike `prepare_run`, this does not transition the run status, but it does
    /// persist the latest preflight snapshot back onto the run record so inspection
    /// surfaces can show the operator the exact checks that were evaluated.
    pub fn preflight_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<PreflightReport> {
        let lease = self.acquire_mutation_lease(home_dir, run_id)?;
        let result = (|| {
            let record = load_run_record(self.services.run_store.as_ref(), run_id)?
                .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
            let recipe = self.load_persisted_recipe(run_id, &record)?;
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
            let asset_lock_checks = self.collect_asset_lock_checks(&record, &recipe, home_dir)?;
            let effective_record = self
                .effective_record_for_preflight(home_dir, &workspace, run_id, &recipe, &record)?;
            let mut preflight = self.build_preflight_record(
                RuntimePaths {
                    repo_root: &workspace,
                    home_dir,
                },
                &recipe,
                &effective_record,
                !matches!(record.status, KernelRunStatus::Failed),
            )?;
            let mut checks = asset_lock_checks
                .into_iter()
                .filter_map(|outcome| {
                    let message = outcome.error?;
                    let kind = match outcome.lock.kind {
                        crate::types::AssetLockKind::Plugin => "asset_lock.plugin",
                        crate::types::AssetLockKind::PromptFile => "asset_lock.prompt_file",
                    };
                    Some(PreflightCheck {
                        kind: kind.to_string(),
                        status: PreflightStatus::Failed,
                        step_ids: vec![outcome.lock.step_id],
                        adapter_kind: None,
                        plugin_id: (outcome.lock.kind == crate::types::AssetLockKind::Plugin)
                            .then_some(outcome.lock.ref_id),
                        action: None,
                        message,
                    })
                })
                .collect::<Vec<_>>();
            checks.extend(preflight.checks);
            preflight =
                PersistedPreflightRecord::from_checks(self.services.clock.now_unix_ms(), checks);

            let mut stored_record = record.clone();
            stored_record.preflight = Some(preflight.clone());
            stored_record.updated_at_ms = self.services.clock.now_unix_ms();
            persist_run_record(self.services.run_store.as_ref(), &stored_record)?;

            Ok(preflight.to_report(run_id))
        })();
        self.finish_mutation_operation(result, lease, "manual preflight")
            .map(|outcome| {
                let mut report = outcome.value;
                report.lease_release_warning = outcome.lease_release_warning;
                report
            })
    }

    pub(crate) fn effective_record_for_preflight(
        &self,
        home_dir: &Path,
        repo_root: &Path,
        run_id: &str,
        recipe: &Recipe,
        record: &PersistedRunRecord,
    ) -> Result<PersistedRunRecord> {
        let mut effective = record.clone();
        if effective.status == KernelRunStatus::Failed {
            let graph = build_execution_graph(recipe)?;
            let mut snapshot = effective.snapshot();
            self.ensure_snapshot_steps(
                &mut snapshot,
                &recipe
                    .steps
                    .iter()
                    .map(|step| step.id.clone())
                    .collect::<Vec<_>>(),
            );
            self.reset_failed_run(&graph, &mut snapshot);
            if run_transition_allowed(snapshot.status, KernelRunStatus::Running) {
                snapshot.status = KernelRunStatus::Running;
            }
            effective.status = snapshot.status;
            effective.steps = snapshot.steps;
            return Ok(effective);
        }

        if effective.status == KernelRunStatus::Blocked {
            for step in effective.steps.values_mut() {
                if step.status == KernelStepStatus::Blocked
                    && step_transition_allowed(
                        KernelStepStatus::Blocked,
                        KernelStepStatus::Runnable,
                    )
                {
                    step.status = KernelStepStatus::Runnable;
                }
            }
        }
        self.preview_request_changes_reentry(home_dir, repo_root, run_id, recipe, &mut effective)?;
        Ok(effective)
    }

    pub(crate) fn build_preflight_record(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        record: &PersistedRunRecord,
        apply_recovery: bool,
    ) -> Result<PersistedPreflightRecord> {
        let step_ids: Vec<String> = recipe.steps.iter().map(|step| step.id.clone()).collect();
        let graph = build_execution_graph(recipe)?;
        let mut snapshot = record.snapshot();
        self.ensure_snapshot_steps(&mut snapshot, &step_ids);
        if apply_recovery {
            recover_snapshot(&mut snapshot, self.services.clock.now_unix_ms());
        }
        self.ensure_entry_runnable(&graph, &mut snapshot);
        let checks = self.collect_preflight_checks(paths, recipe, record, &snapshot)?;
        Ok(PersistedPreflightRecord::from_checks(
            self.services.clock.now_unix_ms(),
            checks,
        ))
    }

    pub(crate) fn collect_preflight_checks(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        record: &PersistedRunRecord,
        snapshot: &KernelRunSnapshot,
    ) -> Result<Vec<PreflightCheck>> {
        let mut workflow_stack = vec![workflow_ref(recipe)];
        let policy_workflow_ref = self.policy_workflow_ref(record)?;
        self.collect_preflight_checks_inner(
            paths,
            recipe,
            record,
            snapshot,
            &policy_workflow_ref,
            &mut workflow_stack,
            &BTreeSet::new(),
        )
    }

    #[allow(clippy::too_many_arguments)] // Nested policy and symbolic input scopes are independent.
    fn collect_preflight_checks_inner(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        record: &PersistedRunRecord,
        snapshot: &KernelRunSnapshot,
        policy_workflow_ref: &str,
        workflow_stack: &mut Vec<String>,
        deferred_inputs: &BTreeSet<String>,
    ) -> Result<Vec<PreflightCheck>> {
        let measurement_scope = crate::PreflightMeasurementScope::KernelRecipe {
            depth: workflow_stack.len().saturating_sub(1),
        };
        let _total_measurement =
            crate::phase_span(measurement_scope, crate::PreflightMeasurementPhase::Total);
        if matches!(
            measurement_scope,
            crate::PreflightMeasurementScope::KernelRecipe { depth: 0 }
        ) {
            crate::record_preflight_phase_duration(
                measurement_scope,
                crate::PreflightMeasurementPhase::RecipeLoad,
                std::time::Duration::ZERO,
            );
            crate::record_preflight_phase_duration(
                measurement_scope,
                crate::PreflightMeasurementPhase::Binding,
                std::time::Duration::ZERO,
            );
        }
        let mut checks = workflow_prerequisite_checks(paths, recipe);
        checks.extend(filesystem_custom_mode_checks(
            paths,
            recipe,
            self.services.home_settings.as_ref(),
        ));
        let trust_measurement =
            crate::phase_span(measurement_scope, crate::PreflightMeasurementPhase::Trust);
        if record.trusted_execution_mode {
            checks.push(match record.trust_decision.as_ref() {
                Some(decision) if !decision.denied() => preflight_check(
                    "trusted_execution",
                    PreflightStatus::Passed,
                    Vec::new(),
                    None,
                    None,
                    None,
                    format!(
                        "trusted execution decision '{}' recorded for this run",
                        decision.decision
                    ),
                ),
                Some(decision) => preflight_check(
                    "trusted_execution",
                    PreflightStatus::Failed,
                    Vec::new(),
                    None,
                    None,
                    None,
                    format!(
                        "trusted execution denied (code={}, verification={})",
                        decision
                            .reason_code
                            .as_deref()
                            .unwrap_or("trusted_execution_denied"),
                        decision
                            .failed_verification_type
                            .as_deref()
                            .unwrap_or("unknown")
                    ),
                ),
                None => preflight_check(
                    "trusted_execution",
                    PreflightStatus::Failed,
                    Vec::new(),
                    None,
                    None,
                    None,
                    "trusted execution is enabled but no trust decision was recorded",
                ),
            });
        }
        drop(trust_measurement);
        if has_blocking_workflow_prerequisites(&checks) {
            return Ok(checks);
        }
        let graph = build_execution_graph(recipe)?;
        let reachable = crate::execution_graph::reachable_steps_in_scope(
            &graph,
            Some(snapshot),
            Some(&record.workflow_inputs),
            deferred_inputs,
        );
        let policy_measurement =
            crate::phase_span(measurement_scope, crate::PreflightMeasurementPhase::Policy);
        let mut invocations = Vec::<PreflightInvocation>::new();
        let mut local_only_blocked_steps = Vec::new();
        for step in &recipe.steps {
            let Some(step_state) = snapshot.steps.get(&step.id) else {
                continue;
            };
            if step_state.status.is_terminal() {
                continue;
            }
            if !reachable.contains(&step.id) {
                continue;
            }

            match &step.execution {
                StepExecutionDescriptor::Invoke {
                    adapter,
                    plugin,
                    action,
                    ..
                } => {
                    let prepared = match persisted_capabilities_for_step(recipe, &step.id, None)
                        .and_then(|persisted| {
                            make_execution_request_with_capabilities(
                                ExecutionRequestContext {
                                    repo_root: paths.repo_root,
                                    home_dir: paths.home_dir,
                                    recipe_source_dir: recipe.source_dir.as_deref().map(Path::new),
                                    run_id: &snapshot.run_id,
                                    workflow_id: &snapshot.workflow_id,
                                    workflow_inputs: &record.workflow_inputs,
                                    workflow_vars: &record.workflow_vars,
                                    execution: step_state.executions,
                                    invoke_id: &format!(
                                        "preflight_{}",
                                        self.services.ids.generate_step_id()
                                    ),
                                    local_only_mode: record.local_only_mode,
                                },
                                step,
                                recipe.workspace.is_some(),
                                &persisted,
                                record,
                                self.services.run_store.as_ref(),
                                self.services.plugin_metadata.as_ref(),
                                self.services.prompt_files.as_ref(),
                                Some(snapshot),
                                true,
                                deferred_inputs,
                            )
                        }) {
                        Ok(Some(prepared)) => {
                            checks.push(preflight_check(
                                "request_preparation",
                                PreflightStatus::Passed,
                                vec![step.id.clone()],
                                Some(adapter.to_string()),
                                Some(plugin.clone()),
                                Some(action.clone()),
                                "execution request prepared",
                            ));
                            prepared
                        }
                        Ok(None) => continue,
                        Err(error) => {
                            let kind = if error
                                .downcast_ref::<crate::filesystem_policy::FilesystemDenial>()
                                .is_some()
                            {
                                "filesystem_workspace_boundary"
                            } else {
                                "request_preparation"
                            };
                            checks.push(preflight_check(
                                kind,
                                PreflightStatus::Failed,
                                vec![step.id.clone()],
                                Some(adapter.to_string()),
                                Some(plugin.clone()),
                                Some(action.clone()),
                                error.to_string(),
                            ));
                            continue;
                        }
                    };

                    let decision_payload = match super::build_step_policy_descriptor(
                        policy_workflow_ref,
                        &prepared.request,
                        &prepared.secret_refs,
                    ) {
                        Ok(payload) => payload,
                        Err(error) => {
                            checks.push(preflight_check_for_request(
                                &prepared.request,
                                "step_policy",
                                PreflightStatus::Failed,
                                error.to_string(),
                            ));
                            continue;
                        }
                    };
                    let decision = match self.services.policy.validate_step_execution(
                        &snapshot.run_id,
                        &step.id,
                        &decision_payload,
                    ) {
                        Ok(decision) => decision,
                        Err(error) => {
                            checks.push(preflight_check_for_request(
                                &prepared.request,
                                "step_policy",
                                PreflightStatus::Failed,
                                error.to_string(),
                            ));
                            continue;
                        }
                    };
                    if !decision.allowed {
                        let provider_kind = prepared
                            .request
                            .metadata
                            .get("provider_kind")
                            .and_then(serde_json::Value::as_str)
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(transist_protocol::ProviderKind::External);
                        if record.local_only_mode
                            && provider_kind.is_external()
                            && decision.reason_code.as_deref() == Some("local_only_denied_external")
                        {
                            local_only_blocked_steps.push(step.id.clone());
                        }
                        checks.push(preflight_check_for_request(
                            &prepared.request,
                            "step_policy",
                            PreflightStatus::Failed,
                            format!(
                                "step execution denied by policy (code={}, message={})",
                                decision.reason_code.as_deref().unwrap_or("policy_denied"),
                                decision.message.as_deref().unwrap_or("denied"),
                            ),
                        ));
                        continue;
                    }

                    let available_secret_refs =
                        match self.check_secret_refs_available(&prepared.secret_refs) {
                            Ok(available_secret_refs) => {
                                if !prepared.secret_refs.is_empty() {
                                    checks.push(preflight_check_for_request(
                                        &prepared.request,
                                        "secret_resolution",
                                        PreflightStatus::Passed,
                                        format!(
                                            "resolved {} secret reference(s)",
                                            prepared.secret_refs.len()
                                        ),
                                    ));
                                }
                                available_secret_refs
                            }
                            Err(SecretResolveError::ManagedPolicyDenied { secret_name, error }) => {
                                // Managed auth-context policy denied this secret ref;
                                // surface as a step_policy failure (same category as other
                                // managed denials) rather than a generic secret_resolution
                                // failure so preflight results are consistent.
                                checks.push(preflight_check_for_request(
                                    &prepared.request,
                                    "step_policy",
                                    PreflightStatus::Failed,
                                    format!(
                                        "secret ref '{}' denied by managed auth context ({}): {}",
                                        secret_name, MANAGED_SECRET_DENIAL_CODE, error.message,
                                    ),
                                ));
                                continue;
                            }
                            Err(SecretResolveError::Other(error)) => {
                                checks.push(preflight_check_for_request(
                                    &prepared.request,
                                    "secret_resolution",
                                    PreflightStatus::Failed,
                                    error.message,
                                ));
                                continue;
                            }
                        };
                    if !available_secret_refs.is_empty()
                        && !decision
                            .remote_secret_policy
                            .allows_adapter_kind(prepared.request.adapter_kind.as_str())
                    {
                        checks.push(preflight_check_for_request(
                            &prepared.request,
                            "remote_secret_policy",
                            PreflightStatus::Failed,
                            format!(
                                "remote secret delivery is denied by policy for adapter '{}'",
                                prepared.request.adapter_kind
                            ),
                        ));
                        continue;
                    }

                    let deferred_paths = prepared.deferred_paths;
                    let mut request = prepared.request;
                    if let KernelValue::Object(payload) = &decision.payload {
                        if let Some(policy) = request.policy.as_object_mut() {
                            for key in [
                                "policy_id",
                                "policy_profile",
                                "requested_capabilities",
                                "granted_capabilities",
                                "denial_reason",
                            ] {
                                if let Some(value) = payload.get(key) {
                                    policy.insert(
                                        key.to_string(),
                                        serde_json::Value::from(value.clone()),
                                    );
                                }
                            }
                        }
                    }
                    let invocation = PreflightInvocation {
                        request,
                        available_secret_refs,
                        remote_secret_policy: decision.remote_secret_policy,
                    };
                    if deferred_paths.is_empty() {
                        invocations.push(invocation);
                    } else {
                        checks.extend(
                            self.services
                                .preflight
                                .validate_deferred_input(&invocation, &deferred_paths)?,
                        );
                        checks.push(preflight_check_for_request(&invocation.request, "deferred_to_dispatch", PreflightStatus::DeferredToDispatch, "complete input validation and live readiness deferred until consumer dispatch"));
                    }
                }
                StepExecutionDescriptor::Subflow {
                    recipe: child_recipe,
                } => {
                    let mut child_checks = self.collect_subflow_preflight_checks(
                        ParentSubflowPreflightContext {
                            paths,
                            parent_record: record,
                            parent_snapshot: snapshot,
                            parent_step: step,
                            parent_step_state: step_state,
                            child_recipe_template: child_recipe,
                            parent_deferred_inputs: deferred_inputs,
                        },
                        policy_workflow_ref,
                        workflow_stack,
                    )?;
                    checks.append(&mut child_checks);
                }
                StepExecutionDescriptor::Control { primitive }
                    if primitive == crate::ControlStepKind::GateManual.as_wire_str() =>
                {
                    let gate_id = validated_gate_id_for_step(step)?;
                    let operation = if load_gate_decision_record(
                        self.services.run_store.as_ref(),
                        paths.home_dir,
                        &snapshot.run_id,
                        &gate_id,
                    )?
                    .is_some()
                    {
                        "decide"
                    } else {
                        "request"
                    };
                    let capability =
                        match persisted_capabilities_for_step(recipe, &step.id, Some(operation)) {
                            Ok(capability) => capability,
                            Err(error) => {
                                checks.push(preflight_check(
                                    "approval_authority",
                                    PreflightStatus::Failed,
                                    vec![step.id.clone()],
                                    None,
                                    None,
                                    None,
                                    error.to_string(),
                                ));
                                continue;
                            }
                        };
                    let descriptor = match build_gate_policy_descriptor_for_workflow(
                        policy_workflow_ref,
                        &capability,
                    ) {
                        Ok(descriptor) => descriptor,
                        Err(error) => {
                            checks.push(preflight_check(
                                "approval_policy",
                                PreflightStatus::Failed,
                                vec![step.id.clone()],
                                None,
                                None,
                                None,
                                error.to_string(),
                            ));
                            continue;
                        }
                    };
                    match self.services.policy.validate_step_execution(
                        &snapshot.run_id,
                        &step.id,
                        &descriptor,
                    ) {
                        Ok(decision) if decision.allowed => {}
                        Ok(decision) => checks.push(preflight_check(
                            "approval_policy",
                            PreflightStatus::Failed,
                            vec![step.id.clone()],
                            None,
                            None,
                            None,
                            decision
                                .message
                                .unwrap_or_else(|| "approval capability denied".to_string()),
                        )),
                        Err(error) => checks.push(preflight_check(
                            "approval_policy",
                            PreflightStatus::Failed,
                            vec![step.id.clone()],
                            None,
                            None,
                            None,
                            error.to_string(),
                        )),
                    }
                }
                StepExecutionDescriptor::Control { .. } => {}
            }
        }

        if record.local_only_mode {
            let has_blockers = !local_only_blocked_steps.is_empty();
            checks.push(preflight_check(
                "local_only",
                if has_blockers {
                    PreflightStatus::Failed
                } else {
                    PreflightStatus::Passed
                },
                local_only_blocked_steps,
                None,
                None,
                None,
                if has_blockers {
                    "local-only mode is active; reachable external-provider steps will be denied before dispatch"
                } else {
                    "local-only mode is active; no reachable external-provider steps were found"
                },
            ));
        }

        drop(policy_measurement);

        if checks
            .iter()
            .any(|check| check.status == PreflightStatus::Failed)
        {
            return Ok(checks);
        }

        let adapter_measurement =
            crate::phase_span(measurement_scope, crate::PreflightMeasurementPhase::Adapter);
        let mut preflight_staging = Vec::new();
        for invocation in &mut invocations {
            if crate::dispatch::request_uses_workflow_asset_snapshot(&invocation.request) {
                let staging = self
                    .services
                    .run_store
                    .create_invocation_staging(paths.home_dir, &invocation.request)?;
                let assets = crate::dispatch::workflow_source_assets_for_request(
                    self.services.run_store.as_ref(),
                    &invocation.request.run_id,
                    &invocation.request,
                )?;
                staging.materialize_source_assets(&assets)?;
                staging.apply_to_request(&mut invocation.request);
                preflight_staging.push(staging);
            }
        }
        let mut adapter_checks =
            crate::with_preflight_measurement_scope(measurement_scope, || {
                self.services.preflight.preflight_steps(&invocations)
            })?;
        drop(adapter_measurement);
        checks.append(&mut adapter_checks);
        Ok(checks)
    }

    fn collect_subflow_preflight_checks(
        &self,
        context: ParentSubflowPreflightContext<'_>,
        policy_workflow_ref: &str,
        workflow_stack: &mut Vec<String>,
    ) -> Result<Vec<PreflightCheck>> {
        let ParentSubflowPreflightContext {
            paths,
            parent_record,
            parent_snapshot,
            parent_step,
            parent_step_state,
            child_recipe_template,
            parent_deferred_inputs,
        } = context;
        let child_scope = crate::PreflightMeasurementScope::KernelRecipe {
            depth: workflow_stack.len(),
        };
        let bound_child_inputs = match crate::measure_preflight_phase(
            child_scope,
            crate::PreflightMeasurementPhase::Binding,
            || {
                let authored = subflows::subflow_provided_inputs(&parent_step.with);
                match crate::resolve_action_value(
                    &KernelValue::Object(authored),
                    &parent_record.workflow_inputs,
                    &parent_record.workflow_vars,
                    Some(parent_snapshot),
                    false,
                ) {
                    Ok(KernelValue::Object(inputs)) if parent_deferred_inputs.is_empty() => {
                        crate::bind_recipe_inputs(child_recipe_template, inputs)
                            .map(|values| PartialChildInputs {
                                values,
                                deferred_names: BTreeSet::new(),
                            })
                            .map_err(anyhow::Error::new)
                    }
                    _ => bind_deferred_subflow_inputs_in_scope(
                        child_recipe_template,
                        &parent_step.with,
                        &parent_record.workflow_inputs,
                        &parent_record.workflow_vars,
                        parent_deferred_inputs,
                    ),
                }
            },
        ) {
            Ok(inputs) => inputs,
            Err(error) => {
                return Ok(vec![preflight_check(
                    "subflow_input_binding",
                    PreflightStatus::Failed,
                    vec![format!("{}/", parent_step.id)],
                    None,
                    None,
                    None,
                    error.to_string(),
                )]);
            }
        };

        let child_workflow_ref = workflow_ref(child_recipe_template);
        let child_depth = workflow_stack.len();
        if workflow_stack.contains(&child_workflow_ref) || child_depth > MAX_SUBFLOW_PREFLIGHT_DEPTH
        {
            let message = if workflow_stack.contains(&child_workflow_ref) {
                format!(
                    "child preflight skipped: workflow '{}' would recurse cyclically",
                    child_workflow_ref
                )
            } else {
                format!(
                    "child preflight skipped: subflow nesting depth {} exceeds limit {}",
                    child_depth, MAX_SUBFLOW_PREFLIGHT_DEPTH
                )
            };
            return Ok(vec![preflight_check(
                if workflow_stack.contains(&child_workflow_ref) {
                    "subflow_preflight.cycle_detected"
                } else {
                    "subflow_preflight.depth_exceeded"
                },
                PreflightStatus::Warning,
                vec![format!("{}/", parent_step.id)],
                None,
                None,
                None,
                message,
            )]);
        }

        let child_run_id =
            preflight_child_run_id(&parent_snapshot.run_id, parent_step, parent_step_state);
        let child_context = crate::measure_preflight_phase(
            child_scope,
            crate::PreflightMeasurementPhase::RecipeLoad,
            || {
                self.subflow_preflight_context(
                    paths,
                    parent_record,
                    parent_step,
                    child_recipe_template,
                    &child_run_id,
                    bound_child_inputs,
                )
            },
        )?;
        let PreparedChildPreflight {
            recipe: child_recipe,
            record: child_record,
            snapshot: child_snapshot,
            deferred_inputs: child_deferred,
        } = match child_context {
            ChildPreflightContext::Prepared(context) => *context,
            ChildPreflightContext::Blocked(check) => return Ok(vec![check]),
        };

        workflow_stack.push(child_workflow_ref);
        let child_checks_result = self.collect_preflight_checks_inner(
            paths,
            &child_recipe,
            &child_record,
            &child_snapshot,
            policy_workflow_ref,
            workflow_stack,
            &child_deferred,
        );
        workflow_stack.pop();

        let mut child_checks = child_checks_result?;
        prefix_preflight_checks(&mut child_checks, &parent_step.id);
        Ok(child_checks)
    }

    fn subflow_preflight_context(
        &self,
        paths: RuntimePaths<'_>,
        parent_record: &PersistedRunRecord,
        parent_step: &transist_protocol::RecipeStep,
        child_recipe_template: &Recipe,
        child_run_id: &str,
        bound_child_inputs: PartialChildInputs,
    ) -> Result<ChildPreflightContext> {
        let PartialChildInputs {
            values: bound_child_inputs,
            deferred_names,
        } = bound_child_inputs;
        if let Some(child_record) = load_run_record(self.services.run_store.as_ref(), child_run_id)?
        {
            if child_record
                .workflow_inputs
                .keys()
                .ne(bound_child_inputs.keys())
                || bound_child_inputs.iter().any(|(name, value)| {
                    !deferred_names.contains(name)
                        && child_record.workflow_inputs.get(name) != Some(value)
                })
            {
                return Ok(ChildPreflightContext::Blocked(preflight_check(
                    "subflow_input_binding",
                    PreflightStatus::Failed,
                    vec![parent_step.id.clone()],
                    None,
                    None,
                    None,
                    format!(
                        "persisted child inputs for run '{}' do not match inputs rebound from parent step '{}'; preflight is denied",
                        child_run_id, parent_step.id
                    ),
                )));
            }
            let mut child_recipe = self.load_persisted_recipe(child_run_id, &child_record)?;
            resolve_recipe_filesystem_policies(
                paths.home_dir,
                self.services.home_settings.as_ref(),
                &mut child_recipe,
            )?;
            resolve_recipe_network_policies(
                paths.home_dir,
                self.services.home_settings.as_ref(),
                &mut child_recipe,
            )?;
            let child_workspace =
                persisted_workspace_or_legacy(paths.home_dir, paths.repo_root, &child_record)?;
            validate_persisted_authority(
                paths.home_dir,
                &child_workspace,
                &child_recipe,
                &child_record.workflow_inputs,
                child_record.local_only_mode,
                child_record.capability_authority_version == 1 && !child_record.test_mode,
                self.services.home_settings.as_ref(),
                self.services.plugin_metadata.as_ref(),
            )?;
            let mut effective_child = self.effective_record_for_preflight(
                paths.home_dir,
                paths.repo_root,
                child_run_id,
                &child_recipe,
                &child_record,
            )?;
            // This is an ephemeral preview. Deferred data must not reuse concrete
            // values from an older activation to validate requests or prune guards.
            effective_child.workflow_inputs = bound_child_inputs;
            self.preview_request_changes_reentry(
                paths.home_dir,
                paths.repo_root,
                child_run_id,
                &child_recipe,
                &mut effective_child,
            )?;
            let child_step_ids: Vec<String> = child_recipe
                .steps
                .iter()
                .map(|step| step.id.clone())
                .collect();
            let child_graph = build_execution_graph(&child_recipe)?;
            let mut child_snapshot = effective_child.snapshot();
            self.ensure_snapshot_steps(&mut child_snapshot, &child_step_ids);
            self.ensure_entry_runnable(&child_graph, &mut child_snapshot);
            effective_child.status = child_snapshot.status;
            effective_child.steps = child_snapshot.steps.clone();
            return Ok(ChildPreflightContext::Prepared(Box::new(
                PreparedChildPreflight {
                    recipe: child_recipe,
                    record: effective_child,
                    snapshot: child_snapshot,
                    deferred_inputs: deferred_names,
                },
            )));
        }

        let mut child_recipe = child_recipe_template.clone();
        resolve_recipe_filesystem_policies(
            paths.home_dir,
            self.services.home_settings.as_ref(),
            &mut child_recipe,
        )?;
        resolve_recipe_network_policies(
            paths.home_dir,
            self.services.home_settings.as_ref(),
            &mut child_recipe,
        )?;
        super::inputs::populate_recipe_capabilities_in_scope(
            paths.home_dir,
            paths.repo_root,
            &mut child_recipe,
            &bound_child_inputs,
            parent_record.local_only_mode,
            self.services.home_settings.as_ref(),
            self.services.plugin_metadata.as_ref(),
            &deferred_names,
        )?;
        let child_step_ids: Vec<String> = child_recipe
            .steps
            .iter()
            .map(|step| step.id.clone())
            .collect();
        let child_graph = build_execution_graph(&child_recipe)?;
        let mut child_snapshot = KernelRunSnapshot::new(
            child_run_id,
            &child_recipe.workflow.id,
            &child_recipe.workflow.version,
            &child_step_ids,
        );
        self.ensure_snapshot_steps(&mut child_snapshot, &child_step_ids);
        self.ensure_entry_runnable(&child_graph, &mut child_snapshot);

        let root_run_id = parent_record
            .lineage
            .as_ref()
            .map(|lineage| lineage.root_run_id.trim())
            .filter(|root_run_id| !root_run_id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| parent_record.run_id.clone());

        let mut child_record = PersistedRunRecord::from_snapshot(
            child_snapshot.clone(),
            None,
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id,
                parent_run_id: Some(parent_record.run_id.clone()),
                parent_step_id: Some(parent_step.id.clone()),
            }),
            bound_child_inputs,
            0,
            parent_record.created_at_ms,
            parent_record.updated_at_ms,
        );
        child_record.test_mode =
            crate::kernel::test_mode::is_test_workflow(&child_recipe.workflow.version);
        child_record.workspace = child_recipe.workspace.clone();
        child_record.local_only_mode = parent_record.local_only_mode;
        child_record.capability_authority_version = parent_record.capability_authority_version;
        Ok(ChildPreflightContext::Prepared(Box::new(
            PreparedChildPreflight {
                recipe: child_recipe,
                record: child_record,
                snapshot: child_snapshot,
                deferred_inputs: deferred_names,
            },
        )))
    }

    pub(crate) fn publish_preflight_outcome(
        &self,
        run_id: &str,
        preflight: &PersistedPreflightRecord,
    ) -> Result<()> {
        self.publish_event(
            if preflight.status == PreflightStatus::Failed {
                "run_preflight_failed"
            } else {
                "run_preflight_completed"
            },
            run_id,
            None,
            serde_json::json!({
                "status": preflight.status.as_str(),
                "blocker_count": preflight.blocker_count,
                "warning_count": preflight.warning_count,
            })
            .into(),
        )
    }
}

fn preflight_child_run_id(
    parent_run_id: &str,
    parent_step: &transist_protocol::RecipeStep,
    parent_step_state: &KernelStepState,
) -> String {
    parent_step_state
        .last_result
        .as_ref()
        .and_then(|result| result.output.get("child_run_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{parent_run_id}{RUN_ID_NESTING_SEPARATOR}{}",
                parent_step.id
            )
        })
}

fn prefix_preflight_checks(checks: &mut [PreflightCheck], prefix: &str) {
    for check in checks {
        check.step_ids = if check.step_ids.is_empty() {
            vec![format!("{prefix}/")]
        } else {
            check
                .step_ids
                .iter()
                .map(|step_id| prefix_preflight_step_id(prefix, step_id))
                .collect()
        };
    }
}

fn prefix_preflight_step_id(prefix: &str, step_id: &str) -> String {
    if step_id.is_empty() {
        format!("{prefix}/")
    } else {
        format!("{prefix}/{step_id}")
    }
}

fn workflow_ref(recipe: &Recipe) -> String {
    format!("{}@{}", recipe.workflow.id, recipe.workflow.version)
}

pub(crate) fn preflight_check(
    kind: impl Into<String>,
    status: PreflightStatus,
    step_ids: Vec<String>,
    adapter_kind: Option<String>,
    plugin_id: Option<String>,
    action: Option<String>,
    message: impl Into<String>,
) -> PreflightCheck {
    PreflightCheck {
        kind: kind.into(),
        status,
        step_ids,
        adapter_kind,
        plugin_id,
        action,
        message: message.into(),
    }
}

pub(crate) fn preflight_check_for_request(
    request: &ExecutionRequest,
    kind: impl Into<String>,
    status: PreflightStatus,
    message: impl Into<String>,
) -> PreflightCheck {
    preflight_check(
        kind,
        status,
        vec![request.step.id.clone()],
        Some(request.adapter_kind.to_string()),
        preflight_plugin_id(request),
        Some(request.action.clone()),
        message,
    )
}

pub(crate) fn preflight_plugin_id(request: &ExecutionRequest) -> Option<String> {
    request
        .metadata
        .get("plugin_id")
        .and_then(serde_json::Value::as_str)
        .map(std::string::ToString::to_string)
}

fn has_blocking_workflow_prerequisites(checks: &[PreflightCheck]) -> bool {
    checks.iter().any(|check| {
        check.kind == "workflow_prerequisite" && check.status == PreflightStatus::Failed
    })
}

fn workflow_prerequisite_checks(paths: RuntimePaths<'_>, recipe: &Recipe) -> Vec<PreflightCheck> {
    if recipe.prerequisites.is_empty() {
        return Vec::new();
    }
    let report = match evaluate_prerequisites(
        &recipe.prerequisites,
        &EvaluationContext::new(PrerequisiteScope::Workflow, paths.repo_root),
    ) {
        Ok(report) => report,
        Err(error) => {
            return vec![preflight_check(
                "workflow_prerequisite",
                PreflightStatus::Failed,
                Vec::new(),
                None,
                None,
                None,
                error.to_string(),
            )];
        }
    };
    report
        .checks
        .into_iter()
        .map(|check| {
            preflight_check(
                "workflow_prerequisite",
                match check.status {
                    HostPrerequisiteStatus::Passed => PreflightStatus::Passed,
                    HostPrerequisiteStatus::Warning => PreflightStatus::Warning,
                    HostPrerequisiteStatus::Failed => PreflightStatus::Failed,
                },
                Vec::new(),
                None,
                None,
                None,
                format!(
                    "[{}:{}] {}",
                    check.scope.as_str(),
                    check.kind,
                    check.message
                ),
            )
        })
        .collect()
}

/// Reason code emitted when a persisted recipe carries the unsupported
/// `workspace_access.mode: custom` before adapter dispatch.
const FILESYSTEM_CUSTOM_MODE_UNSUPPORTED: &str = "filesystem_custom_mode_unsupported";

/// Scans the persisted recipe for any declaration of the unsupported
/// `workspace_access.mode: custom` — at the workflow level, in every
/// authored step/cleanup policy, and in every effective policy. Persisted
/// recipes that bypassed the compiler (e.g. compiled before this
/// enforcement landed) must still be rejected before dispatch with a
/// uniform reason code on both platforms.
pub(crate) fn filesystem_custom_mode_checks(
    paths: RuntimePaths<'_>,
    recipe: &Recipe,
    settings_port: &dyn crate::ports::HomeSettingsPort,
) -> Vec<PreflightCheck> {
    let workflow = recipe.filesystem_policy.as_ref();
    let mut checks = Vec::new();

    // Determine whether any step lacks an effective policy source — if so,
    // global settings must be consulted for resolution. When every step is
    // already resolved (frozen recipe), current global settings are not
    // authoritative and must not block a run whose persisted policy is valid.
    let needs_global = recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|c| &c.step))
        .any(|step| step.effective_filesystem_policy_source.is_none());
    let global = if needs_global {
        settings_port
            .load_settings(paths.home_dir)
            .ok()
            .and_then(|settings| settings.filesystem_policy)
    } else {
        None
    };

    // Workflow-level policy.
    if let Some(wf_policy) = workflow {
        if matches!(
            wf_policy.workspace_access.as_ref().map(|wa| wa.mode),
            Some(transist_protocol::WorkspaceAccessMode::Custom)
        ) {
            checks.push(preflight_check(
                FILESYSTEM_CUSTOM_MODE_UNSUPPORTED,
                PreflightStatus::Failed,
                Vec::new(),
                None,
                None,
                None,
                "workflow filesystem_policy.workspace_access.mode 'custom' is not supported"
                    .to_string(),
            ));
        }
    }

    // Global home settings policy — only checked when resolution is needed
    // (some step lacks an effective source). A frozen recipe should not be
    // blocked by current mutable global settings.
    if needs_global {
        if let Some(ref gp) = global {
            if matches!(
                gp.workspace_access.as_ref().map(|wa| wa.mode),
                Some(transist_protocol::WorkspaceAccessMode::Custom)
            ) {
                checks.push(preflight_check(
                    FILESYSTEM_CUSTOM_MODE_UNSUPPORTED,
                    PreflightStatus::Failed,
                    Vec::new(),
                    None,
                    None,
                    None,
                    "global settings.yaml filesystem_policy.workspace_access.mode 'custom' is not supported"
                        .to_string(),
                ));
            }
        }
    }

    let mut emit = |step: &transist_protocol::RecipeStep| {
        // Check authored step policy.
        if let Some(authored) = step.filesystem_policy.as_ref() {
            if matches!(
                authored.workspace_access.as_ref().map(|wa| wa.mode),
                Some(transist_protocol::WorkspaceAccessMode::Custom)
            ) {
                checks.push(preflight_check(
                    FILESYSTEM_CUSTOM_MODE_UNSUPPORTED,
                    PreflightStatus::Failed,
                    vec![step.id.clone()],
                    None,
                    None,
                    None,
                    format!(
                        "step '{}' authored filesystem_policy.workspace_access.mode 'custom' is not supported",
                        step.id
                    ),
                ));
                return;
            }
        }
        // Check effective policy (may differ from authored when an older
        // recipe was compiled before the custom rejection landed).
        let mode = if step.effective_filesystem_policy_source.is_some() {
            step.effective_filesystem_policy
                .workspace_access
                .as_ref()
                .map_or(transist_protocol::WorkspaceAccessMode::Full, |wa| wa.mode)
        } else {
            let (effective, _) = transist_protocol::FilesystemPolicy::resolve(
                global.as_ref(),
                workflow,
                step.filesystem_policy.as_ref(),
            );
            effective
                .workspace_access
                .as_ref()
                .map_or(transist_protocol::WorkspaceAccessMode::Full, |wa| wa.mode)
        };
        if mode == transist_protocol::WorkspaceAccessMode::Custom {
            let (adapter_kind, plugin_id, action) = match &step.execution {
                transist_protocol::StepExecutionDescriptor::Invoke {
                    adapter,
                    plugin,
                    action,
                    ..
                } => (
                    Some(adapter.to_string()),
                    Some(plugin.clone()),
                    Some(action.clone()),
                ),
                _ => (None, None, None),
            };
            checks.push(preflight_check(
                FILESYSTEM_CUSTOM_MODE_UNSUPPORTED,
                PreflightStatus::Failed,
                vec![step.id.clone()],
                adapter_kind,
                plugin_id,
                action,
                format!(
                    "step '{}' effective workspace_access.mode 'custom' is not supported",
                    step.id
                ),
            ));
        }
    };

    for step in &recipe.steps {
        emit(step);
    }
    for cleanup in &recipe.cleanup {
        emit(&cleanup.step);
    }
    checks
}

#[cfg(test)]
mod grouped_evidence_tests {
    use super::*;

    #[test]
    fn nested_preflight_prefix_preserves_every_grouped_step_association() {
        let mut checks = vec![preflight_check(
            "adapter_handshake",
            PreflightStatus::Passed,
            vec!["step-b".to_string(), "step-a".to_string()],
            Some("process".to_string()),
            Some("plugin".to_string()),
            Some("run".to_string()),
            "passed",
        )];

        prefix_preflight_checks(&mut checks, "parent/child");

        assert_eq!(
            checks[0].step_ids,
            ["parent/child/step-b", "parent/child/step-a"]
        );
    }
}
