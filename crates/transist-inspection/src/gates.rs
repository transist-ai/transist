// SPDX-License-Identifier: MPL-2.0
//! Gate-focused inspection helpers.

use super::*;
use transist_compiler::templating::input_refs::render_input_placeholders_lenient;
use transist_core::ports::RunStorePort;
use transist_storage_local::{LocalRunStore, LocalStorageConfig};

/// Render `{{inputs.<key>}}` placeholders inside a `with` value tree against
/// the persisted run's `workflow_inputs`, so inspection consumers (CLI
/// fullscreen, server `/api/v1/...`) see the same concrete strings the
/// runtime would have used at execution time. This is intentionally lenient:
/// missing or non-scalar inputs leave the placeholder untouched instead of
/// failing the inspection request, since `request_summary` is a display
/// surface and must never crash the run-detail view.
fn render_with_for_display(
    value: KernelValue,
    workflow_inputs: &BTreeMap<String, KernelValue>,
) -> KernelValue {
    match value {
        KernelValue::String(raw) => {
            KernelValue::String(render_input_placeholders_lenient(&raw, workflow_inputs))
        }
        KernelValue::Array(items) => KernelValue::Array(
            items
                .into_iter()
                .map(|item| render_with_for_display(item, workflow_inputs))
                .collect(),
        ),
        KernelValue::Object(entries) => KernelValue::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key, render_with_for_display(value, workflow_inputs)))
                .collect(),
        ),
        other => other,
    }
}

pub(crate) fn build_gates(
    store: &LocalInspectionStore,
    record: &PersistedRunRecord,
    run_id: &str,
) -> Result<Vec<InspectionGate>> {
    let recipe = match store.load_run_recipe(run_id)? {
        Some(recipe) => recipe,
        None => return Ok(Vec::new()),
    };
    let all_decisions = store.list_gate_decisions(run_id)?;
    // Consumed history must still agree with each immutable completion, including
    // when a history file has been removed entirely from the enumeration above.
    for step in &recipe.steps {
        if transist_protocol::gate_review_documents(step)
            .map_err(anyhow::Error::msg)?
            .is_empty()
        {
            continue;
        }
        for execution in store.list_step_executions(run_id, &step.id)? {
            if let Some(value) = execution
                .result
                .as_ref()
                .and_then(|r| r.output.get("review_decision"))
            {
                let decision: GateDecisionRecord = serde_json::from_value(value.clone())?;
                validate_review_decision(store, &recipe, run_id, &decision)?;
            }
        }
    }
    let mut history_by_gate: BTreeMap<String, Vec<InspectionGateDecision>> = BTreeMap::new();
    for record in &all_decisions {
        validate_review_decision(store, &recipe, run_id, record)?;
        history_by_gate
            .entry(record.gate_id.clone())
            .or_default()
            .push(InspectionGateDecision {
                review_request: record.review_request.clone(),
                state: normalize_gate_decision(&record.decision).to_string(),
                actor: record.actor.clone(),
                reason: record.reason.clone(),
                decided_at_ms: record.decided_at_ms,
            });
    }
    for entries in history_by_gate.values_mut() {
        entries.sort_by_key(|entry| entry.decided_at_ms.unwrap_or(0));
    }
    let mut latest_by_gate: BTreeMap<String, GateDecisionRecord> = BTreeMap::new();
    for record in all_decisions {
        let ts = record.decided_at_ms.unwrap_or(0);
        match latest_by_gate.get(&record.gate_id) {
            Some(existing) if existing.decided_at_ms.unwrap_or(0) >= ts => {}
            _ => {
                latest_by_gate.insert(record.gate_id.clone(), record);
            }
        }
    }
    let decisions = latest_by_gate;

    let mut gates = Vec::new();
    for step in &recipe.steps {
        if !is_manual_gate_step(step) {
            continue;
        }
        let gate_id = gate_id_for_step(step);
        let step_state = record.steps.get(&step.id);
        let decision = decisions.get(&gate_id);
        let document_gate = !transist_protocol::gate_review_documents(step)
            .map_err(anyhow::Error::msg)?
            .is_empty();
        let status = step_state
            .filter(|state| state.status == KernelStepStatus::Blocked)
            .map(|_| "blocked".to_string())
            .or_else(|| {
                step_state.and_then(|state| {
                    let decision = state
                        .last_result
                        .as_ref()
                        .and_then(|result| result.output.get("decision"))
                        .and_then(serde_json::Value::as_str);
                    decision.map(|value| normalize_gate_decision(value).to_string())
                })
            })
            .or_else(|| {
                decision.map(|decision| normalize_gate_decision(&decision.decision).to_string())
            })
            .unwrap_or_else(|| "pending".to_string());
        let mut request_summary: BTreeMap<_, _> = step
            .with
            .iter()
            // Authored guidance is untrusted. These keys describe runtime-owned
            // commits only; never let authored values create review links/counts.
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "review_request" | "review_document_count" | "review_decision"
                )
            })
            .map(|(key, value)| {
                (
                    key.clone(),
                    render_with_for_display(value.clone(), &record.workflow_inputs),
                )
            })
            .collect();
        let mut current_review = None;
        if let Some(state) = step_state.filter(|_| document_gate) {
            let executions = store.list_step_executions(run_id, &step.id)?;
            if state.last_result.is_some()
                && let Some(committed) = executions.iter().find(|e| e.execution == state.executions)
                && let Some(result) = &committed.result
                && let Some(value) = result.output.get("review_request")
            {
                let request: transist_protocol::GateReviewRequest =
                    serde_json::from_value(value.clone())?;
                let original = executions
                    .iter()
                    .find(|e| e.execution == request.execution)
                    .context("missing immutable gate review request")?;
                if request.run_id != run_id
                    || request.step_id != step.id
                    || request.gate_id != gate_id
                    || request.schema != "transist.gate_review_request.v1"
                    || original.status != "blocked"
                    || request.execution <= state.activation_executions_baseline
                    || serde_json::to_value(&state.last_result)?
                        != serde_json::to_value(&committed.result)?
                    || original
                        .result
                        .as_ref()
                        .and_then(|r| r.output.get("review_request"))
                        != Some(value)
                    || request
                        .review_documents
                        .iter()
                        .map(|r| r.document.clone())
                        .collect::<Vec<_>>()
                        != transist_protocol::gate_review_documents(step)
                            .map_err(anyhow::Error::msg)?
                {
                    anyhow::bail!("invalid committed gate review request");
                }
                request_summary.insert(
                    "review_request".into(),
                    KernelValue::from_json(value.clone()),
                );
                request_summary.insert(
                    "review_document_count".into(),
                    KernelValue::from_json(request.review_documents.len().into()),
                );
                current_review = Some(request);
            }
        }
        let decision = decision.filter(|decision| {
            !document_gate
                || current_review
                    .as_ref()
                    .is_some_and(|request| decision.review_request.as_ref() == Some(request))
        });
        let status = if document_gate && current_review.is_none() {
            match step_state.map(|s| s.status) {
                Some(KernelStepStatus::Failed) => "failed".into(),
                Some(KernelStepStatus::Canceled) => "canceled".into(),
                _ => "pending".into(),
            }
        } else {
            status
        };
        gates.push(InspectionGate {
            gate_id: gate_id.clone(),
            status,
            step_id: step.id.clone(),
            created_at_ms: step_state.and_then(|state| state.started_at_ms),
            decision: decision.map(|decision| InspectionGateDecision {
                review_request: decision.review_request.clone(),
                state: normalize_gate_decision(&decision.decision).to_string(),
                actor: decision.actor.clone(),
                reason: decision.reason.clone(),
                decided_at_ms: decision.decided_at_ms,
            }),
            decision_history: history_by_gate.get(&gate_id).cloned().unwrap_or_default(),
            request_summary,
        });
    }
    Ok(gates)
}

fn validate_review_decision(
    store: &LocalInspectionStore,
    recipe: &Recipe,
    run_id: &str,
    decision: &GateDecisionRecord,
) -> Result<()> {
    let step = recipe.steps.iter().find(|step| step.id == decision.step_id);
    let document_gate = step
        .map(transist_protocol::gate_review_documents)
        .transpose()
        .map_err(anyhow::Error::msg)?
        .is_some_and(|names| !names.is_empty());
    if decision.review_request.is_none() && !document_gate {
        return Ok(());
    }
    let step = step.context("review decision gate step missing")?;
    let request = decision
        .review_request
        .as_ref()
        .context("document decision lost its review request")?;
    let executions = store.list_step_executions(run_id, &step.id)?;
    let original = executions
        .iter()
        .find(|e| e.execution == request.execution)
        .context("review decision request execution missing")?;
    if !document_gate
        || decision.schema != "transist.gate_decision.v1"
        || decision.run_id != run_id
        || !matches!(
            decision.decision.as_str(),
            "approved" | "rejected" | "request_changes"
        )
        || request.schema != "transist.gate_review_request.v1"
        || request.run_id != run_id
        || request.step_id != step.id
        || request.gate_id != decision.gate_id
        || decision.gate_id != gate_id_for_step(step)
        || original.status != "blocked"
        || original
            .result
            .as_ref()
            .and_then(|r| r.output.get("review_request"))
            != Some(&serde_json::to_value(request)?)
        || request
            .review_documents
            .iter()
            .map(|r| r.document.clone())
            .collect::<Vec<_>>()
            != transist_protocol::gate_review_documents(step).map_err(anyhow::Error::msg)?
    {
        anyhow::bail!("invalid document review decision identity or immutable request");
    }
    let run_store = LocalRunStore::new(LocalStorageConfig::new(store.home_dir()));
    let history = run_store
        .load_gate_decision_history(store.home_dir(), run_id, &decision.gate_id)?
        .into_iter()
        .map(|raw| serde_json::from_slice::<GateDecisionRecord>(&raw).map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;
    let matching: Vec<_> = history
        .iter()
        .filter(|record| record.review_request.as_ref() == Some(request))
        .collect();
    if matching.len() != 1 || matching[0] != decision {
        anyhow::bail!("document review decision differs from its exact immutable history");
    }
    let request_value = serde_json::to_value(request)?;
    for execution in &executions {
        let Some(result) = &execution.result else {
            continue;
        };
        if result.output.get("review_request") != Some(&request_value) {
            continue;
        }
        if result.output.get("decision").is_some() || result.output.get("review_decision").is_some()
        {
            let committed: GateDecisionRecord = serde_json::from_value(
                result
                    .output
                    .get("review_decision")
                    .context("decided gate completion lost its full review decision")?
                    .clone(),
            )?;
            if committed != *decision
                || result
                    .output
                    .get("decision")
                    .and_then(serde_json::Value::as_str)
                    != Some(decision.decision.as_str())
            {
                anyhow::bail!(
                    "document review history differs from its immutable completion decision"
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn is_manual_gate_step(step: &RecipeStep) -> bool {
    matches!(
        &step.execution,
        transist_protocol::StepExecutionDescriptor::Control { primitive }
            if primitive == "control.gate.manual"
    )
}

pub(crate) fn gate_id_for_step(step: &RecipeStep) -> String {
    // Inspection reads recipes that already passed run-birth gate-id
    // validation, so the effective id is the authored value verbatim (never
    // trimmed) or the validated step id when absent. Fall back to step id only
    // for display robustness on legacy records.
    match step.with.get("gate_id") {
        Some(KernelValue::String(value)) if !value.trim().is_empty() => value.clone(),
        _ => step.id.clone(),
    }
}

pub(crate) fn normalize_gate_decision(raw: &str) -> &str {
    match raw {
        "approved" => "approved",
        "rejected" => "rejected",
        "request_changes" => "request_changes",
        _ => "blocked",
    }
}
