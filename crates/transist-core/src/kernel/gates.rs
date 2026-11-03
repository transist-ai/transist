// SPDX-License-Identifier: MPL-2.0
//! Manual-gate and loop re-entry helpers.

use super::*;
use crate::ControlStepKind;
use crate::domain::taxonomy::LOOP_META_KEY;

impl RuntimeKernel {
    /// Persists a validated manual-gate decision while holding the root-tree
    /// mutation lease.
    pub fn record_gate_decision(
        &self,
        home_dir: &Path,
        root_run_id: &str,
        record: &transist_protocol::GateDecisionRecord,
    ) -> Result<KernelOperationOutcome<()>> {
        validate_gate_decision_record(record)?;
        let lease = self.acquire_mutation_lease(home_dir, root_run_id)?;
        let operation = (|| {
            let mut record = record.clone();
            self.bind_gate_review_decision(home_dir, root_run_id, &mut record)?;
            self.services
                .run_store
                .store_gate_decision(home_dir, &record)
        })();
        self.finish_mutation_operation(operation, lease, "gate decision persistence")
    }

    pub(crate) fn evaluate_step_loop_state(
        &self,
        step: &transist_protocol::RecipeStep,
        _graph: &ExecutionGraph,
        snapshot: &KernelRunSnapshot,
        workflow_inputs: &BTreeMap<String, KernelValue>,
        workflow_vars: &BTreeMap<String, KernelValue>,
        step_state: &KernelStepState,
    ) -> GateState {
        if let Some(loop_cfg) = recipe_loop_from_step(step) {
            return evaluate_loop_state(
                &loop_cfg,
                step_state,
                Some(snapshot),
                Some(workflow_inputs),
                Some(workflow_vars),
            );
        }
        GateState::LoopCompleted
    }

    pub(crate) fn prepare_request_changes_reentry(
        &self,
        home_dir: &Path,
        repo_root: &Path,
        run_id: &str,
        recipe: &Recipe,
        record: &mut PersistedRunRecord,
    ) -> Result<()> {
        validate_persisted_authority(
            home_dir,
            repo_root,
            recipe,
            &record.workflow_inputs,
            record.local_only_mode,
            record.capability_authority_version == 1 && !record.test_mode,
            self.services.home_settings.as_ref(),
            self.services.plugin_metadata.as_ref(),
        )?;
        self.apply_request_changes_reentry(home_dir, run_id, recipe, record, true)
    }

    pub(crate) fn preview_request_changes_reentry(
        &self,
        home_dir: &Path,
        repo_root: &Path,
        run_id: &str,
        recipe: &Recipe,
        record: &mut PersistedRunRecord,
    ) -> Result<()> {
        validate_persisted_authority(
            home_dir,
            repo_root,
            recipe,
            &record.workflow_inputs,
            record.local_only_mode,
            record.capability_authority_version == 1 && !record.test_mode,
            self.services.home_settings.as_ref(),
            self.services.plugin_metadata.as_ref(),
        )?;
        self.apply_request_changes_reentry(home_dir, run_id, recipe, record, false)
    }

    fn apply_request_changes_reentry(
        &self,
        home_dir: &Path,
        run_id: &str,
        recipe: &Recipe,
        record: &mut PersistedRunRecord,
        clear_decisions: bool,
    ) -> Result<()> {
        let mut blocked_gate_steps = Vec::new();
        for step in recipe.steps.iter().filter(|step| {
            matches!(
                &step.execution,
                StepExecutionDescriptor::Control { primitive }
                    if primitive == ControlStepKind::GateManual.as_wire_str()
            )
        }) {
            let Some(step_state) = record.steps.get(&step.id) else {
                continue;
            };
            if step_state.status != KernelStepStatus::Runnable {
                continue;
            }
            let gate_id = validated_gate_id_for_step(step)?;
            let decision = load_gate_decision_record(
                self.services.run_store.as_ref(),
                home_dir,
                run_id,
                &gate_id,
            )?
            .filter(|decision| decision.decision == "request_changes");
            if let Some(decision) = decision {
                if !transist_protocol::gate_review_documents(step)
                    .map_err(anyhow::Error::msg)?
                    .is_empty()
                {
                    // Re-entry may reset and dispatch a loop body before it reaches
                    // the gate again. Validate the waiting identity before that reset.
                    let previous = step_state
                        .last_result
                        .as_ref()
                        .filter(|r| r.status == "blocked")
                        .context("request-changes has no current waiting review execution")?;
                    if previous.execution != step_state.executions {
                        bail!("request-changes waiting execution mismatch");
                    }
                    let request = self
                        .gate_review_request(
                            run_id,
                            step,
                            step_state
                                .executions
                                .checked_add(1)
                                .context("gate execution exhausted")?,
                            Some(previous),
                        )?
                        .context("missing request-changes review request")?;
                    self.validate_review_decision_history(home_dir, &decision, &request)?;
                }
                blocked_gate_steps.push((step.id.clone(), gate_id));
            }
        }

        for (blocked_gate_step_id, gate_id) in blocked_gate_steps {
            let mut reset_any_loop = false;
            for loop_step in recipe.steps.iter().filter(|step| {
                loop_body_steps(step)
                    .iter()
                    .any(|body_step_id| body_step_id == &blocked_gate_step_id)
            }) {
                self.reset_loop_body_steps_states(loop_step, &mut record.steps);
                reset_any_loop = true;
            }
            if reset_any_loop {
                // `request_changes` is the signal to rerun the loop body once.
                // Clear the active gate decision before the rerun reaches the
                // same manual gate again so the next gate execution blocks for a
                // fresh operator decision instead of auto-consuming stale
                // feedback and triggering another loop iteration.
                if clear_decisions {
                    consume_gate_decision_record(
                        self.services.run_store.as_ref(),
                        home_dir,
                        run_id,
                        &gate_id,
                    )?;
                }
            }
        }

        Ok(())
    }

    pub(crate) fn prepare_approved_gate_reentry(
        &self,
        home_dir: &Path,
        run_id: &str,
        recipe: &Recipe,
        graph: &ExecutionGraph,
        record: &PersistedRunRecord,
        snapshot: &mut KernelRunSnapshot,
    ) -> Result<Option<Vec<String>>> {
        let Some((gate_step_id, reset_steps)) =
            self.approved_gate_reentry_plan(run_id, recipe, graph, record, snapshot)?
        else {
            return Ok(None);
        };

        let gate_step = recipe
            .steps
            .iter()
            .find(|step| step.id == gate_step_id)
            .ok_or_else(|| anyhow::anyhow!("approved gate step '{}' is missing", gate_step_id))?;
        let gate_id = validated_gate_id_for_step(gate_step)?;
        consume_gate_decision_record(self.services.run_store.as_ref(), home_dir, run_id, &gate_id)?;

        for step_id in &reset_steps {
            let Some(step) = snapshot.steps.get_mut(step_id) else {
                continue;
            };
            reset_step_for_rerun(step);
        }

        Ok(Some(reset_steps))
    }

    pub(crate) fn reset_loop_body_steps(
        &self,
        step: &transist_protocol::RecipeStep,
        snapshot: &mut KernelRunSnapshot,
    ) -> Vec<String> {
        self.reset_loop_body_steps_states(step, &mut snapshot.steps)
    }

    pub(crate) fn reset_loop_body_steps_states(
        &self,
        step: &transist_protocol::RecipeStep,
        step_states: &mut BTreeMap<String, KernelStepState>,
    ) -> Vec<String> {
        let body_steps = loop_body_steps(step);
        let mut changed = Vec::new();
        for body_step_id in body_steps {
            let Some(body_step) = step_states.get_mut(&body_step_id) else {
                continue;
            };
            reset_step_for_rerun(body_step);
            changed.push(body_step_id);
        }
        changed
    }

    pub(crate) fn activate_loop_targets(
        &self,
        graph: &ExecutionGraph,
        step_id: &str,
        snapshot: &mut KernelRunSnapshot,
    ) -> Vec<String> {
        let mut changed = Vec::new();
        for edge in outgoing_edges(graph, step_id) {
            if edge.r#loop.is_none() {
                continue;
            }
            let Some(target_step) = snapshot.steps.get_mut(&edge.to) else {
                continue;
            };
            reset_step_for_rerun(target_step);
            changed.push(edge.to.clone());
        }
        changed
    }

    fn approved_gate_reentry_plan(
        &self,
        run_id: &str,
        recipe: &Recipe,
        graph: &ExecutionGraph,
        record: &PersistedRunRecord,
        snapshot: &KernelRunSnapshot,
    ) -> Result<Option<(String, Vec<String>)>> {
        let child_loop_context = self.run_is_loop_body_subflow(run_id, record)?;
        let mut candidates = recipe
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    &step.execution,
                    StepExecutionDescriptor::Control { primitive }
                        if primitive == ControlStepKind::GateManual.as_wire_str()
                )
            })
            .filter_map(|step| {
                let step_state = snapshot.steps.get(&step.id)?;
                if step_state.status != KernelStepStatus::Succeeded {
                    return None;
                }
                let decision = step_state
                    .last_result
                    .as_ref()?
                    .output
                    .get("decision")
                    .and_then(serde_json::Value::as_str)?
                    .trim();
                if decision != "approved" {
                    return None;
                }
                Some((
                    step.id.clone(),
                    step_state
                        .finished_at_ms
                        .or(step_state.started_at_ms)
                        .unwrap_or_default(),
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|rhs| std::cmp::Reverse(rhs.1));

        for (gate_step_id, _) in candidates {
            if !child_loop_context && !step_is_in_loop_body(recipe, &gate_step_id) {
                continue;
            }
            let reset_steps = non_loop_descendants_including_self(graph, &gate_step_id);
            let has_safe_failed_descendant = reset_steps.iter().any(|step_id| {
                step_id != &gate_step_id
                    && snapshot
                        .steps
                        .get(step_id)
                        .map(step_is_safe_condition_failure)
                        .unwrap_or(false)
            });
            if has_safe_failed_descendant {
                return Ok(Some((gate_step_id, reset_steps)));
            }
        }

        Ok(None)
    }

    fn run_is_loop_body_subflow(&self, run_id: &str, record: &PersistedRunRecord) -> Result<bool> {
        let Some(lineage) = record.lineage.as_ref() else {
            return Ok(false);
        };
        let Some(parent_run_id) = lineage
            .parent_run_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(false);
        };
        let Some(parent_step_id) = lineage
            .parent_step_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(false);
        };
        let Some(parent_record) = load_run_record(self.services.run_store.as_ref(), parent_run_id)?
        else {
            bail!(
                "run '{}' references missing parent run '{}'",
                run_id,
                parent_run_id
            );
        };
        let parent_recipe = self.load_persisted_recipe(parent_run_id, &parent_record)?;
        Ok(step_is_in_loop_body(&parent_recipe, parent_step_id))
    }
}

fn step_is_in_loop_body(recipe: &Recipe, step_id: &str) -> bool {
    recipe.steps.iter().any(|step| {
        loop_body_steps(step)
            .iter()
            .any(|body_step_id| body_step_id == step_id)
    })
}

fn non_loop_descendants_including_self(graph: &ExecutionGraph, root_step_id: &str) -> Vec<String> {
    let mut queue = VecDeque::from([root_step_id.to_string()]);
    let mut visited = BTreeSet::new();
    let mut descendants = Vec::new();

    while let Some(step_id) = queue.pop_front() {
        if !visited.insert(step_id.clone()) {
            continue;
        }
        descendants.push(step_id.clone());
        for edge in outgoing_edges(graph, &step_id) {
            if edge.r#loop.is_some() {
                continue;
            }
            queue.push_back(edge.to.clone());
        }
    }

    descendants
}

fn step_is_safe_condition_failure(step: &KernelStepState) -> bool {
    if step.status != KernelStepStatus::Failed {
        return false;
    }
    let Some(result) = step.last_result.as_ref() else {
        return false;
    };
    let is_safe = result
        .error
        .as_ref()
        .map(|error| error.retryability == transist_protocol::Retryability::Safe)
        .unwrap_or(false);
    let is_negative_ok = result.output.get("ok") == Some(&serde_json::Value::Bool(false));
    is_safe && is_negative_ok
}

/// Returns the effective gate id for a gate step, strictly.
///
/// When `with.gate_id` is authored it must be a non-empty, non-whitespace
/// String that satisfies the gate-id identifier rule; the authored value is
/// used verbatim (never trimmed), so a padded or whitespace-only value is
/// rejected rather than silently normalized. A non-string `gate_id` value
/// (typed-invalid) is also rejected. Only an absent `gate_id` falls back to the
/// validated step id.
pub(crate) fn gate_id_for_step(step: &transist_protocol::RecipeStep) -> anyhow::Result<String> {
    match step.with.get("gate_id") {
        None => Ok(step.id.clone()),
        Some(KernelValue::String(value)) => {
            if value.trim().is_empty() {
                bail!(
                    "gate step '{}' has an empty or whitespace-only gate_id",
                    step.id
                );
            }
            transist_protocol::validate_identifier(
                transist_protocol::IdentifierKind::GateId,
                value,
            )
            .with_context(|| format!("gate step '{}' has invalid gate id", step.id))?;
            Ok(value.clone())
        }
        Some(_) => bail!(
            "gate step '{}' has a non-string gate_id (must be a string)",
            step.id
        ),
    }
}

/// Returns the effective gate id for a gate step, validated with
/// [`transist_protocol::IdentifierKind::GateId`].
///
/// This is the single accepted derivation for both authored and persisted gate
/// ids: the value comes from `with.gate_id` or falls back to the validated step
/// id, and must satisfy the identifier rule before any decision path, storage
/// key, or policy descriptor is built from it.
pub(crate) fn validated_gate_id_for_step(
    step: &transist_protocol::RecipeStep,
) -> anyhow::Result<String> {
    let gate_id = gate_id_for_step(step)?;
    // The step-id fallback is also validated (authored values were validated
    // inline above).
    transist_protocol::validate_identifier(transist_protocol::IdentifierKind::GateId, &gate_id)
        .with_context(|| format!("gate step '{}' has invalid gate id", step.id))?;
    Ok(gate_id)
}

/// Validates the effective gate id of every manual gate step in a recipe,
/// failing closed before run birth.
pub(crate) fn validate_recipe_gate_ids(recipe: &Recipe) -> anyhow::Result<()> {
    transist_protocol::validate_document_reviews(recipe).map_err(anyhow::Error::msg)?;
    for step in recipe.steps.iter().filter(|step| {
        matches!(
            &step.execution,
            StepExecutionDescriptor::Control { primitive }
                if primitive == crate::ControlStepKind::GateManual.as_wire_str()
        )
    }) {
        validated_gate_id_for_step(step)?;
    }
    Ok(())
}

pub(crate) fn recipe_loop_from_step(
    step: &transist_protocol::RecipeStep,
) -> Option<transist_protocol::RecipeLoop> {
    let KernelValue::Object(loop_meta) = step.with.get(LOOP_META_KEY)? else {
        return None;
    };
    Some(transist_protocol::RecipeLoop {
        while_expr: loop_meta.get("while").and_then(|value| match value {
            KernelValue::String(value) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            _ => None,
        }),
        until_expr: loop_meta.get("until").and_then(|value| match value {
            KernelValue::String(value) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            _ => None,
        }),
        max_iters: loop_meta.get("max_iters").and_then(|value| match value {
            KernelValue::Number(value) => value.as_u64().map(|raw| raw as u32),
            _ => None,
        }),
    })
}

pub(crate) fn loop_body_steps(step: &transist_protocol::RecipeStep) -> Vec<String> {
    let Some(KernelValue::Object(loop_meta)) = step.with.get(LOOP_META_KEY) else {
        return Vec::new();
    };
    let Some(KernelValue::Array(entries)) = loop_meta.get("body_steps") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| match entry {
            KernelValue::String(value) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            _ => None,
        })
        .collect()
}

pub(crate) fn load_gate_decision_record(
    run_store: &dyn RunStorePort,
    home_dir: &Path,
    run_id: &str,
    gate_id: &str,
) -> Result<Option<transist_protocol::GateDecisionRecord>> {
    let path = run_store.gate_decision_path(home_dir, run_id, gate_id)?;
    let Some(raw) = run_store.load_gate_decision(home_dir, run_id, gate_id)? else {
        return Ok(None);
    };
    validate_gate_decision_bytes(&raw, &path, run_id, gate_id)
}

/// Loads the newest valid immutable decision for the compiled gate binding.
///
/// Active gate records are deliberately excluded because loop re-entry consumes
/// them before the next invoke. History is authoritative and every candidate
/// for the exact compiled gate must validate before a plugin can receive it.
pub(crate) fn load_latest_gate_decision_history_record(
    run_store: &dyn RunStorePort,
    home_dir: &Path,
    run_id: &str,
    gate_step_id: &str,
    gate_id: &str,
) -> Result<Option<transist_protocol::GateDecisionRecord>> {
    let records = run_store.load_gate_decision_history(home_dir, run_id, gate_id)?;
    select_latest_gate_decision_history_record(records, run_id, gate_step_id, gate_id)
}

pub(crate) fn select_latest_gate_decision_history_record(
    records: Vec<Vec<u8>>,
    run_id: &str,
    gate_step_id: &str,
    gate_id: &str,
) -> Result<Option<transist_protocol::GateDecisionRecord>> {
    let mut latest = None::<transist_protocol::GateDecisionRecord>;
    let mut latest_count = 0usize;
    for (index, raw) in records.iter().enumerate() {
        let record = serde_json::from_slice::<transist_protocol::GateDecisionRecord>(raw)
            .with_context(|| format!("invalid gate decision history record at index {index}"))?;
        validate_gate_decision_record(&record)
            .with_context(|| format!("gate decision history record at index {index} is invalid"))?;
        if record.run_id != run_id {
            bail!(
                "gate decision history record at index {index} is for run '{}' not '{}'",
                record.run_id,
                run_id
            );
        }
        if record.step_id != gate_step_id {
            bail!(
                "gate decision history record at index {index} is for gate step '{}' not '{}'",
                record.step_id,
                gate_step_id
            );
        }
        if record.gate_id != gate_id {
            bail!(
                "gate decision history record at index {index} is for gate '{}' not '{}'",
                record.gate_id,
                gate_id
            );
        }
        let timestamp = record.decided_at_ms.ok_or_else(|| {
            anyhow::anyhow!(
                "gate decision history record at index {index} is missing decided_at_ms"
            )
        })?;
        if let Some(existing) = latest.as_ref() {
            let existing_timestamp = existing
                .decided_at_ms
                .expect("accepted history records always have a timestamp");
            if timestamp == existing_timestamp {
                latest_count += 1;
                continue;
            }
            if timestamp < existing_timestamp {
                continue;
            }
        }
        latest = Some(record);
        latest_count = 1;
    }
    if latest_count > 1 {
        let timestamp = latest
            .as_ref()
            .and_then(|record| record.decided_at_ms)
            .expect("ambiguous latest history records always have a timestamp");
        bail!(
            "gate decision history has ambiguous latest records for gate '{}' at decided_at_ms {}",
            gate_id,
            timestamp
        );
    }
    Ok(latest)
}

fn validate_gate_decision_bytes(
    raw: &[u8],
    path: &Path,
    run_id: &str,
    gate_id: &str,
) -> Result<Option<transist_protocol::GateDecisionRecord>> {
    let record = serde_json::from_slice::<transist_protocol::GateDecisionRecord>(raw)
        .with_context(|| format!("invalid gate decision json: {}", path.display()))?;
    validate_gate_decision_record(&record)
        .with_context(|| format!("gate decision '{}' is invalid", path.display()))?;
    // Verify the persisted decision identity matches the exact request. A
    // mismatched record must fail closed: it is never treated as the decision
    // for a different run or gate.
    if record.schema != "transist.gate_decision.v1" {
        bail!(
            "gate decision '{}' has unsupported schema '{}'",
            path.display(),
            record.schema
        );
    }
    if record.run_id != run_id {
        bail!(
            "gate decision '{}' is for run '{}' not the requested run '{}'",
            path.display(),
            record.run_id,
            run_id
        );
    }
    if record.gate_id != gate_id {
        bail!(
            "gate decision '{}' is for gate '{}' not the requested gate '{}'",
            path.display(),
            record.gate_id,
            gate_id
        );
    }
    if let Err(error) = transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::GateId,
        &record.gate_id,
    ) {
        bail!(
            "gate decision '{}' has invalid gate id: {error}",
            path.display()
        );
    }
    if let Err(error) = transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::StepId,
        &record.step_id,
    ) {
        bail!(
            "gate decision '{}' has invalid step id: {error}",
            path.display()
        );
    }
    // The decision enum is validated on load so an unknown or tampered decision
    // value fails closed instead of being silently treated as a pass.
    if !matches!(
        record.decision.as_str(),
        "approved" | "rejected" | "request_changes"
    ) {
        bail!(
            "gate decision '{}' has unknown decision '{}'",
            path.display(),
            record.decision
        );
    }
    Ok(Some(record))
}

pub(crate) fn consume_gate_decision_record(
    run_store: &dyn RunStorePort,
    home_dir: &Path,
    run_id: &str,
    gate_id: &str,
) -> Result<()> {
    run_store.consume_gate_decision(home_dir, run_id, gate_id)
}

pub(super) fn validate_gate_decision_record(
    record: &transist_protocol::GateDecisionRecord,
) -> Result<()> {
    if record.schema != "transist.gate_decision.v1" {
        bail!("unsupported gate decision schema '{}'", record.schema);
    }
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::RunRef,
        &record.run_id,
    )
    .context("invalid gate decision run id")?;
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::GateId,
        &record.gate_id,
    )
    .context("invalid gate decision gate id")?;
    transist_protocol::validate_identifier(
        transist_protocol::IdentifierKind::StepId,
        &record.step_id,
    )
    .context("invalid gate decision step id")?;
    if !matches!(
        record.decision.as_str(),
        "approved" | "rejected" | "request_changes"
    ) {
        bail!("unknown gate decision '{}'", record.decision);
    }
    if let Some(request) = &record.review_request {
        if request.schema != "transist.gate_review_request.v1"
            || request.run_id != record.run_id
            || request.step_id != record.step_id
            || request.gate_id != record.gate_id
            || request.execution == 0
            || request.review_documents.is_empty()
        {
            bail!("gate review decision request identity mismatch");
        }
    }
    Ok(())
}

#[cfg(test)]
fn load_gate_decision_record_from_path(
    path: &Path,
    run_id: &str,
    gate_id: &str,
) -> Result<Option<transist_protocol::GateDecisionRecord>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(path)
        .with_context(|| format!("failed reading gate decision: {}", path.display()))?;
    validate_gate_decision_bytes(&raw, path, run_id, gate_id)
}

#[cfg(test)]
fn clear_gate_decision_record_from_path(path: &Path, _run_id: &str, _gate_id: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(path)
        .with_context(|| format!("failed clearing gate decision: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use transist_protocol::KernelValue;

    fn gate_step(with: BTreeMap<String, KernelValue>) -> transist_protocol::RecipeStep {
        transist_protocol::RecipeStep {
            id: "gate".to_string(),
            execution: StepExecutionDescriptor::Control {
                primitive: crate::ControlStepKind::GateManual.as_wire_str().to_string(),
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

    fn decision_path(home: &Path, run_id: &str, gate_id: &str) -> PathBuf {
        home.join("runs")
            .join(format!("run_{run_id}"))
            .join("gates")
            .join(format!("{gate_id}.json"))
    }

    #[test]
    fn valid_gate_ids_are_accepted() {
        for value in ["review", "review-a", "review_a", "review.a", "gate1.v2"] {
            let step = gate_step(BTreeMap::from([(
                "gate_id".to_string(),
                KernelValue::String(value.to_string()),
            )]));
            assert_eq!(
                validated_gate_id_for_step(&step).expect("valid gate id"),
                value
            );
        }
    }

    #[test]
    fn invalid_gate_ids_are_rejected() {
        for value in [
            "review/a",
            ".hidden",
            "-flag",
            "..",
            "has space",
            "café",
            "a\tb",
            " review", // leading whitespace is never trimmed
            "review ", // trailing whitespace is never trimmed
            "   ",     // whitespace-only
            "",        // empty
        ] {
            let step = gate_step(BTreeMap::from([(
                "gate_id".to_string(),
                KernelValue::String(value.to_string()),
            )]));
            assert!(
                validated_gate_id_for_step(&step).is_err(),
                "gate id '{value}' must be rejected"
            );
        }
    }

    #[test]
    fn typed_invalid_gate_ids_are_rejected() {
        for value in [
            KernelValue::Number(serde_json::Number::from(42)),
            KernelValue::Bool(true),
            KernelValue::Array(Vec::new()),
            KernelValue::Object(BTreeMap::new()),
        ] {
            let step = gate_step(BTreeMap::from([("gate_id".to_string(), value)]));
            assert!(
                validated_gate_id_for_step(&step).is_err(),
                "non-string gate_id must be rejected"
            );
        }
    }

    #[test]
    fn absent_gate_id_falls_back_to_step_id() {
        let step = gate_step(BTreeMap::new());
        assert_eq!(
            validated_gate_id_for_step(&step).expect("step-id fallback is valid"),
            "gate"
        );
    }

    #[test]
    fn distinct_ids_produce_distinct_decision_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        for id in ["review-a", "review_a", "review.a", "review"] {
            let path = decision_path(home, "run-1", id);
            assert!(
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy() == format!("{id}.json")),
                "decision must be stored under the exact validated id '{id}', got {path:?}"
            );
        }
    }

    #[test]
    fn loaded_decision_must_match_requested_identity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::create_dir_all(home.join("runs/run_run-1")).expect("create run root");
        let record = |run_id: &str, gate_id: &str| transist_protocol::GateDecisionRecord {
            review_request: None,
            schema: "transist.gate_decision.v1".to_string(),
            run_id: run_id.to_string(),
            gate_id: gate_id.to_string(),
            step_id: "gate".to_string(),
            decision: "approved".to_string(),
            actor: Some("alice".to_string()),
            actor_source: Some("flag".to_string()),
            reason: Some("ok".to_string()),
            decided_at_ms: Some(1),
        };

        // A matching record loads successfully.
        let path = decision_path(home, "run-1", "review");
        std::fs::create_dir_all(path.parent().expect("gate dir")).expect("create dir");
        std::fs::write(
            &path,
            serde_json::to_string(&record("run-1", "review")).expect("serialize"),
        )
        .expect("write decision");
        assert!(
            load_gate_decision_record_from_path(&path, "run-1", "review")
                .expect("load matching")
                .is_some()
        );

        // A record for the wrong run at the requested path fails closed.
        let path = decision_path(home, "run-1", "other");
        std::fs::write(
            &path,
            serde_json::to_string(&record("run-2", "other")).expect("serialize"),
        )
        .expect("write mismatched-run decision");
        assert!(
            load_gate_decision_record_from_path(&path, "run-1", "other").is_err(),
            "a decision for the wrong run must fail closed"
        );

        // A record for the wrong gate at the requested path fails closed.
        let path = decision_path(home, "run-1", "gatex");
        std::fs::write(
            &path,
            serde_json::to_string(&record("run-1", "gatey")).expect("serialize"),
        )
        .expect("write mismatched-gate decision");
        assert!(
            load_gate_decision_record_from_path(&path, "run-1", "gatex").is_err(),
            "a decision for the wrong gate must fail closed"
        );
    }

    #[test]
    fn clear_syncs_gate_directory_after_deletion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::create_dir_all(home.join("runs/run_run-1")).expect("create run root");
        let path = decision_path(home, "run-1", "review");
        std::fs::create_dir_all(path.parent().expect("gate dir")).expect("create dir");
        std::fs::write(&path, b"{}").expect("write decision");

        clear_gate_decision_record_from_path(&path, "run-1", "review").expect("clear decision");
        assert!(!path.exists(), "consumed decision must be gone");

        // Clearing a missing decision is a no-op.
        clear_gate_decision_record_from_path(&path, "run-1", "review").expect("clear missing");
    }

    #[test]
    fn loaded_decision_fails_closed_on_bad_schema_or_decision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::create_dir_all(home.join("runs/run_run-1")).expect("create run root");
        let base = || transist_protocol::GateDecisionRecord {
            review_request: None,
            schema: "transist.gate_decision.v1".to_string(),
            run_id: "run-1".to_string(),
            gate_id: "review".to_string(),
            step_id: "gate".to_string(),
            decision: "approved".to_string(),
            actor: Some("alice".to_string()),
            actor_source: Some("flag".to_string()),
            reason: Some("ok".to_string()),
            decided_at_ms: Some(1),
        };

        // Unknown decision value fails closed.
        let mut bad_decision = base();
        bad_decision.decision = "maybe".to_string();
        let path = decision_path(home, "run-1", "review");
        std::fs::create_dir_all(path.parent().expect("gate dir")).expect("create dir");
        std::fs::write(
            &path,
            serde_json::to_string(&bad_decision).expect("serialize"),
        )
        .expect("write decision");
        assert!(
            load_gate_decision_record_from_path(&path, "run-1", "review").is_err(),
            "an unknown decision must fail closed on load"
        );

        // Unsupported schema fails closed.
        let mut bad_schema = base();
        bad_schema.decision = "approved".to_string();
        bad_schema.schema = "transist.gate_decision.v99".to_string();
        std::fs::write(
            &path,
            serde_json::to_string(&bad_schema).expect("serialize"),
        )
        .expect("write decision");
        assert!(
            load_gate_decision_record_from_path(&path, "run-1", "review").is_err(),
            "an unsupported schema must fail closed on load"
        );

        // Invalid step id fails closed.
        let mut bad_step = base();
        bad_step.decision = "approved".to_string();
        bad_step.schema = "transist.gate_decision.v1".to_string();
        bad_step.step_id = "../escape".to_string();
        std::fs::write(&path, serde_json::to_string(&bad_step).expect("serialize"))
            .expect("write decision");
        assert!(
            load_gate_decision_record_from_path(&path, "run-1", "review").is_err(),
            "an invalid step id must fail closed on load"
        );
    }

    #[test]
    fn gate_decision_history_selection_returns_the_newest_exact_record() {
        let record = |decision: &str, decided_at_ms: u64| transist_protocol::GateDecisionRecord {
            review_request: None,
            schema: "transist.gate_decision.v1".to_string(),
            run_id: "run-1".to_string(),
            gate_id: "review".to_string(),
            step_id: "gate".to_string(),
            decision: decision.to_string(),
            actor: None,
            actor_source: None,
            reason: Some("needs a revision".to_string()),
            decided_at_ms: Some(decided_at_ms),
        };
        let newest = select_latest_gate_decision_history_record(
            vec![
                serde_json::to_vec(&record("approved", 10)).expect("serialize"),
                serde_json::to_vec(&record("request_changes", 20)).expect("serialize"),
            ],
            "run-1",
            "gate",
            "review",
        )
        .expect("history resolves")
        .expect("latest record");

        assert_eq!(newest.decision, "request_changes");
        assert_eq!(newest.reason.as_deref(), Some("needs a revision"));
    }

    #[test]
    fn gate_decision_history_ignores_ties_older_than_the_unique_latest_record() {
        let record = |decision: &str, decided_at_ms: u64| transist_protocol::GateDecisionRecord {
            review_request: None,
            schema: "transist.gate_decision.v1".to_string(),
            run_id: "run-1".to_string(),
            gate_id: "review".to_string(),
            step_id: "gate".to_string(),
            decision: decision.to_string(),
            actor: None,
            actor_source: None,
            reason: None,
            decided_at_ms: Some(decided_at_ms),
        };
        let newest = select_latest_gate_decision_history_record(
            vec![
                serde_json::to_vec(&record("request_changes", 10)).expect("serialize"),
                serde_json::to_vec(&record("rejected", 10)).expect("serialize"),
                serde_json::to_vec(&record("approved", 20)).expect("serialize"),
            ],
            "run-1",
            "gate",
            "review",
        )
        .expect("older ties do not make the latest record ambiguous")
        .expect("latest record");

        assert_eq!(newest.decision, "approved");
        assert_eq!(newest.decided_at_ms, Some(20));
    }

    #[test]
    fn gate_decision_history_selection_rejects_timestamp_ties_and_identity_mismatches() {
        let record = |step_id: &str, decided_at_ms: u64| transist_protocol::GateDecisionRecord {
            review_request: None,
            schema: "transist.gate_decision.v1".to_string(),
            run_id: "run-1".to_string(),
            gate_id: "review".to_string(),
            step_id: step_id.to_string(),
            decision: "request_changes".to_string(),
            actor: None,
            actor_source: None,
            reason: None,
            decided_at_ms: Some(decided_at_ms),
        };
        assert!(
            select_latest_gate_decision_history_record(
                vec![
                    serde_json::to_vec(&record("gate", 20)).expect("serialize"),
                    serde_json::to_vec(&record("gate", 20)).expect("serialize"),
                ],
                "run-1",
                "gate",
                "review",
            )
            .is_err()
        );
        assert!(
            select_latest_gate_decision_history_record(
                vec![serde_json::to_vec(&record("other", 20)).expect("serialize")],
                "run-1",
                "gate",
                "review",
            )
            .is_err()
        );
        assert!(
            select_latest_gate_decision_history_record(
                vec![b"not valid json".to_vec()],
                "run-1",
                "gate",
                "review",
            )
            .is_err()
        );
        assert!(
            select_latest_gate_decision_history_record(
                vec![br#"{"schema":"transist.gate_decision.v1","run_id":"run-1","gate_id":"review","step_id":"gate","decision":"approved","decided_at_ms":20,"unexpected":true}"#.to_vec()],
                "run-1",
                "gate",
                "review",
            )
            .is_err(),
            "unknown persisted fields must fail closed"
        );
        assert_eq!(
            select_latest_gate_decision_history_record(Vec::new(), "run-1", "gate", "review")
                .expect("empty history is optional"),
            None
        );
    }
    #[test]
    fn recipe_with_invalid_gate_id_fails_before_run_birth() {
        let recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: transist_protocol::WorkflowRef {
                id: "demo".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps: vec![gate_step(BTreeMap::from([(
                "gate_id".to_string(),
                KernelValue::String("../bad".to_string()),
            )]))],
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        assert!(
            validate_recipe_gate_ids(&recipe).is_err(),
            "a recipe with an invalid gate id must fail before run birth"
        );
    }
}
