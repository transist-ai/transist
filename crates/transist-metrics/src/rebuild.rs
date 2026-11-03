// SPDX-License-Identifier: MPL-2.0
//! Metrics rebuild and per-run aggregation helpers.

use super::*;
use crate::fingerprints::run_root;
use crate::index::{
    BlobExecutionCounts, RunContribution, RunFingerprint, SampleKey, StepDescriptor,
    UsageAccumulator, UsageKey,
};
use crate::prometheus::{
    add_sample, bounded_labels, histogram_samples, step_labels, usage_breakdown_entries,
    usage_labels,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn build_run_contribution(
    home_dir: &Path,
    run_id: &str,
    fingerprint: Option<RunFingerprint>,
) -> Result<Option<RunContribution>> {
    let store = LocalInspectionStore::new(home_dir);
    let record = match store.load_run_record(run_id) {
        Ok(record) => record,
        Err(error) => {
            if run_root(home_dir, run_id)
                .map(|path| !path.exists())
                .unwrap_or(true)
            {
                return Ok(None);
            }
            return Err(error);
        }
    };

    let recipe = store.load_run_recipe(run_id)?;
    let descriptors = step_descriptors(recipe.as_ref());
    let blob_counts = blob_counts_by_execution(home_dir, run_id)?;
    let step_ids = collect_step_ids(home_dir, &record, &descriptors)?;
    let gate_decisions = store
        .list_gate_decisions(run_id)?
        .into_iter()
        .map(|decision| (decision.step_id.clone(), decision))
        .collect::<BTreeMap<_, _>>();

    let mut step_summaries = BTreeMap::new();
    let mut run_usage = UsageAccumulator::default();
    let mut usage_breakdown = BTreeMap::<UsageKey, UsageAccumulator>::new();
    let mut samples = BTreeMap::<SampleKey, f64>::new();

    let workflow_labels = bounded_labels([
        ("workflow_id", record.workflow_id.clone()),
        ("workflow_version", record.workflow_version.clone()),
    ]);

    add_sample(
        &mut samples,
        "transist_runs_created_total",
        workflow_labels.clone(),
        1.0,
    );
    if record.lineage.as_ref().map(|lineage| lineage.mode)
        == Some(transist_core::RunLineageMode::Replay)
    {
        add_sample(
            &mut samples,
            "transist_runs_replayed_total",
            workflow_labels.clone(),
            1.0,
        );
    }
    if record.resume_count > 0 {
        add_sample(
            &mut samples,
            "transist_runs_resumed_total",
            workflow_labels.clone(),
            record.resume_count as f64,
        );
    }

    let current_active_step_count = record
        .steps
        .values()
        .filter(|step| {
            matches!(
                step.status,
                KernelStepStatus::Dispatching | KernelStepStatus::Running
            )
        })
        .count();
    let blocked_gate_count = record
        .steps
        .iter()
        .filter(|(step_id, step)| {
            step.status == KernelStepStatus::Blocked
                && descriptors
                    .get(*step_id)
                    .map(|descriptor| descriptor.is_manual_gate)
                    .unwrap_or(false)
        })
        .count();
    if is_active_run(record.status) {
        add_sample(
            &mut samples,
            "transist_runs_active",
            workflow_labels.clone(),
            1.0,
        );
    }
    if blocked_gate_count > 0 {
        add_sample(
            &mut samples,
            "transist_runs_blocked_on_gates",
            workflow_labels.clone(),
            1.0,
        );
    }

    let mut total_executions = 0_u32;
    let mut succeeded_executions = 0_u32;
    let mut failed_executions = 0_u32;
    let mut cancelled_executions = 0_u32;
    let mut total_artifacts = 0_usize;
    let mut total_evidence = 0_usize;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let mut run_active_execution_duration_ms = 0_u64;
    let mut run_blocked_wait_duration_ms = 0_u64;

    for step_id in step_ids {
        let descriptor = descriptors
            .get(&step_id)
            .cloned()
            .unwrap_or_else(|| StepDescriptor {
                step_kind: infer_step_kind_from_state(&record, &step_id),
                adapter_kind: None,
                action: None,
                is_manual_gate: false,
            });
        let step_executions = store.list_step_executions(run_id, &step_id)?;
        let step_labels = step_labels(
            &workflow_labels,
            &descriptor.step_kind,
            descriptor.adapter_kind.as_deref(),
            descriptor.action.as_deref(),
        );
        let started_executions = record
            .steps
            .get(&step_id)
            .map(|step| step.executions)
            .or_else(|| u32::try_from(step_executions.len()).ok())
            .unwrap_or_default();
        total_executions = total_executions.saturating_add(started_executions);
        if started_executions > 0 {
            add_sample(
                &mut samples,
                "transist_step_executions_started_total",
                step_labels.clone(),
                started_executions as f64,
            );
        }
        let mut status_counts = BTreeMap::new();
        let mut executions = Vec::new();
        let mut step_usage = UsageAccumulator::default();
        let mut step_breakdown = BTreeMap::<UsageKey, UsageAccumulator>::new();
        let mut retries = 0_u32;
        let mut step_active_execution_duration_ms = 0_u64;
        let mut step_blocked_wait_duration_ms = 0_u64;
        let mut child_run_id: Option<String> = None;
        let step_blob_totals = blob_counts
            .iter()
            .filter(|((blob_step_id, _), _)| blob_step_id == &step_id)
            .fold(BlobExecutionCounts::default(), |mut acc, (_, counts)| {
                acc.artifacts = acc.artifacts.saturating_add(counts.artifacts);
                acc.evidence = acc.evidence.saturating_add(counts.evidence);
                acc
            });
        total_artifacts = total_artifacts.saturating_add(step_blob_totals.artifacts);
        total_evidence = total_evidence.saturating_add(step_blob_totals.evidence);
        let step_artifacts = step_blob_totals.artifacts;
        let step_evidence = step_blob_totals.evidence;

        for execution in step_executions {
            let result = execution.result.clone();
            let validation_errors = execution.metrics_validation_errors;
            let provider = result
                .as_ref()
                .and_then(|result| result.usage.as_ref())
                .and_then(|usage| usage.provider.clone());
            let model = result
                .as_ref()
                .and_then(|result| result.usage.as_ref())
                .and_then(|usage| usage.model.clone());
            let counts = blob_counts
                .get(&(execution.step_id.clone(), execution.execution))
                .cloned()
                .unwrap_or_default();

            let execution_summary = execution_summary(&record, &descriptor, execution, counts);
            *status_counts
                .entry(execution_summary.status.clone())
                .or_insert(0_u32) += 1;
            if execution_summary.status == "succeeded" {
                succeeded_executions = succeeded_executions.saturating_add(1);
            } else if execution_summary.status == "failed" {
                failed_executions = failed_executions.saturating_add(1);
            } else if execution_summary.status == "cancelled" {
                cancelled_executions = cancelled_executions.saturating_add(1);
            }
            if let Some(validation_errors) = validation_errors {
                add_sample(
                    &mut samples,
                    "transist_metrics_validation_errors_total",
                    Vec::new(),
                    validation_errors as f64,
                );
            }

            if let Some(result) = result.as_ref() {
                run_usage.add(result);
                step_usage.add(result);
                if result.usage.is_some() {
                    let key = UsageKey {
                        provider: provider.clone(),
                        model: model.clone(),
                    };
                    usage_breakdown.entry(key.clone()).or_default().add(result);
                    step_breakdown.entry(key).or_default().add(result);
                }
                if child_run_id.is_none() {
                    if let Some(value) = result
                        .output
                        .get("child_run_id")
                        .and_then(|value| value.as_str())
                    {
                        child_run_id = Some(value.to_string());
                    }
                }
            }

            add_sample(
                &mut samples,
                "transist_step_executions_finished_total",
                step_labels.clone(),
                1.0,
            );
            if matches!(
                execution_summary.status.as_str(),
                "succeeded" | "failed" | "cancelled"
            ) {
                let mut finished_labels = step_labels.clone();
                finished_labels.push(("status".to_string(), execution_summary.status.clone()));
                add_sample(
                    &mut samples,
                    "transist_step_executions_finished_by_status_total",
                    finished_labels,
                    1.0,
                );
            }
            if let Some(duration_ms) = execution_summary.duration_ms {
                step_active_execution_duration_ms =
                    step_active_execution_duration_ms.saturating_add(duration_ms);
                run_active_execution_duration_ms =
                    run_active_execution_duration_ms.saturating_add(duration_ms);
                let mut duration_labels = step_labels.clone();
                duration_labels.push(("status".to_string(), execution_summary.status.clone()));
                histogram_samples(
                    &mut samples,
                    "transist_step_execution_duration_ms",
                    duration_labels,
                    duration_ms,
                    STEP_DURATION_BUCKETS_MS,
                );
            }
            if result
                .as_ref()
                .and_then(|result| result.usage.as_ref())
                .is_some()
            {
                let usage_labels = usage_labels(
                    &workflow_labels,
                    &descriptor.step_kind,
                    descriptor.adapter_kind.as_deref(),
                    descriptor.action.as_deref(),
                    provider.as_deref(),
                    model.as_deref(),
                );
                if let Some(input_tokens) = execution_summary.input_tokens {
                    add_sample(
                        &mut samples,
                        "transist_usage_input_tokens_total",
                        usage_labels.clone(),
                        input_tokens as f64,
                    );
                }
                if let Some(output_tokens) = execution_summary.output_tokens {
                    add_sample(
                        &mut samples,
                        "transist_usage_output_tokens_total",
                        usage_labels.clone(),
                        output_tokens as f64,
                    );
                }
                if let Some(total_tokens) = execution_summary.total_tokens {
                    add_sample(
                        &mut samples,
                        "transist_usage_total_tokens_total",
                        usage_labels.clone(),
                        total_tokens as f64,
                    );
                }
                add_sample(
                    &mut samples,
                    "transist_usage_reports_total",
                    usage_labels,
                    1.0,
                );
            }
            if descriptor.is_manual_gate && execution_summary.status == "blocked" {
                add_sample(
                    &mut samples,
                    "transist_gate_waits_total",
                    step_labels.clone(),
                    1.0,
                );
                if let Some(decision) = gate_decisions.get(&step_id) {
                    let decision_value = normalize_gate_decision(&decision.decision);
                    if decision_value == "approved" {
                        add_sample(
                            &mut samples,
                            "transist_gate_approvals_total",
                            step_labels.clone(),
                            1.0,
                        );
                    } else if decision_value == "rejected" {
                        add_sample(
                            &mut samples,
                            "transist_gate_rejections_total",
                            step_labels.clone(),
                            1.0,
                        );
                    }
                    if let (Some(start_ms), Some(decided_at_ms)) = (
                        execution_summary
                            .finished_at_ms
                            .or(execution_summary.started_at_ms),
                        decision.decided_at_ms,
                    ) {
                        if decided_at_ms >= start_ms {
                            let wait_duration_ms = decided_at_ms - start_ms;
                            step_blocked_wait_duration_ms =
                                step_blocked_wait_duration_ms.saturating_add(wait_duration_ms);
                            run_blocked_wait_duration_ms =
                                run_blocked_wait_duration_ms.saturating_add(wait_duration_ms);
                            histogram_samples(
                                &mut samples,
                                "transist_gate_wait_duration_ms",
                                step_labels.clone(),
                                wait_duration_ms,
                                GATE_WAIT_BUCKETS_MS,
                            );
                        }
                    }
                } else if let Some(start_ms) = execution_summary
                    .finished_at_ms
                    .or(execution_summary.started_at_ms)
                {
                    if now_ms >= start_ms {
                        let wait_duration_ms = now_ms - start_ms;
                        step_blocked_wait_duration_ms =
                            step_blocked_wait_duration_ms.saturating_add(wait_duration_ms);
                        run_blocked_wait_duration_ms =
                            run_blocked_wait_duration_ms.saturating_add(wait_duration_ms);
                    }
                }
            }
            executions.push(execution_summary);
        }

        if started_executions > 1 {
            retries = retries.saturating_add(started_executions - 1);
            add_sample(
                &mut samples,
                "transist_step_retries_total",
                step_labels.clone(),
                retries as f64,
            );
        }

        let latest_execution = executions.last().cloned();
        let step_breakdown_entries = usage_breakdown_entries(step_breakdown);
        step_summaries.insert(
            step_id.clone(),
            StepMetricsSummary {
                run_id: run_id.to_string(),
                step_id,
                step_kind: descriptor.step_kind,
                adapter_kind: descriptor.adapter_kind,
                action: descriptor.action,
                execution_count: started_executions,
                status_counts,
                latest_execution,
                executions,
                artifacts_produced: step_artifacts,
                evidence_files_produced: step_evidence,
                input_tokens: step_usage.input_tokens,
                output_tokens: step_usage.output_tokens,
                total_tokens: step_usage.total_tokens,
                usage_breakdown: step_breakdown_entries,
                child_run_id,
                active_execution_duration_ms: (step_active_execution_duration_ms > 0)
                    .then_some(step_active_execution_duration_ms),
                blocked_wait_duration_ms: (step_blocked_wait_duration_ms > 0)
                    .then_some(step_blocked_wait_duration_ms),
            },
        );
    }

    let started_at_ms = record
        .steps
        .values()
        .filter_map(|step| step.started_at_ms)
        .min();
    let finished_at_ms = record
        .steps
        .values()
        .filter_map(|step| step.finished_at_ms)
        .max();
    let terminal_status = terminal_status(record.status);
    let total_duration_ms = match (started_at_ms, finished_at_ms) {
        (Some(started_at_ms), Some(finished_at_ms)) if finished_at_ms >= started_at_ms => {
            Some(finished_at_ms - started_at_ms)
        }
        (Some(started_at_ms), None)
            if is_active_run(record.status) || record.status == KernelRunStatus::Blocked =>
        {
            now_ms.checked_sub(started_at_ms)
        }
        _ => None,
    };
    if started_at_ms.is_some() || record.status != KernelRunStatus::Created {
        add_sample(
            &mut samples,
            "transist_runs_started_total",
            workflow_labels.clone(),
            1.0,
        );
    }
    if let Some(status) = terminal_status.clone() {
        add_sample(
            &mut samples,
            "transist_runs_finished_total",
            workflow_labels.clone(),
            1.0,
        );
        let mut finished_labels = workflow_labels.clone();
        finished_labels.push(("status".to_string(), status.clone()));
        add_sample(
            &mut samples,
            "transist_runs_finished_by_status_total",
            finished_labels,
            1.0,
        );
        if let Some(duration_ms) = total_duration_ms {
            histogram_samples(
                &mut samples,
                "transist_run_duration_ms",
                workflow_labels.clone(),
                duration_ms,
                RUN_DURATION_BUCKETS_MS,
            );
        }
    }

    Ok(Some(RunContribution {
        fingerprint,
        parent_run_id: record
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.parent_run_id.clone()),
        parent_step_id: record
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.parent_step_id.clone()),
        run_list_summary: RunListMetricsSummary {
            run_id: run_id.to_string(),
            total_duration_ms,
            wall_clock_duration_ms: total_duration_ms,
            active_execution_duration_ms: (run_active_execution_duration_ms > 0)
                .then_some(run_active_execution_duration_ms),
            blocked_wait_duration_ms: (run_blocked_wait_duration_ms > 0)
                .then_some(run_blocked_wait_duration_ms),
            total_step_executions: total_executions,
            total_artifacts_produced: total_artifacts,
            total_evidence_files_produced: total_evidence,
            total_tokens: run_usage.total_tokens,
        },
        run_summary: RunMetricsSummary {
            run_id: run_id.to_string(),
            workflow_id: record.workflow_id.clone(),
            workflow_version: record.workflow_version.clone(),
            created_at_ms: record.created_at_ms,
            started_at_ms,
            finished_at_ms,
            terminal_status,
            total_duration_ms,
            wall_clock_duration_ms: total_duration_ms,
            active_execution_duration_ms: (run_active_execution_duration_ms > 0)
                .then_some(run_active_execution_duration_ms),
            blocked_wait_duration_ms: (run_blocked_wait_duration_ms > 0)
                .then_some(run_blocked_wait_duration_ms),
            current_active_step_count,
            blocked_gate_count,
            total_step_executions: total_executions,
            succeeded_step_executions: succeeded_executions,
            failed_step_executions: failed_executions,
            cancelled_step_executions: cancelled_executions,
            total_artifacts_produced: total_artifacts,
            total_evidence_files_produced: total_evidence,
            input_tokens: run_usage.input_tokens,
            output_tokens: run_usage.output_tokens,
            total_tokens: run_usage.total_tokens,
            usage_breakdown: usage_breakdown_entries(usage_breakdown),
        },
        step_summaries,
        samples,
    }))
}

fn execution_summary(
    record: &PersistedRunRecord,
    descriptor: &StepDescriptor,
    execution: StepExecutionRecord,
    counts: BlobExecutionCounts,
) -> StepExecutionMetricsSummary {
    let step_id = execution.step_id.clone();
    let result = execution.result.as_ref();
    let usage = result.and_then(|result| result.usage.as_ref());
    StepExecutionMetricsSummary {
        run_id: execution.run_id,
        step_id: step_id.clone(),
        execution: execution.execution,
        step_kind: descriptor.step_kind.clone(),
        adapter_kind: descriptor
            .adapter_kind
            .clone()
            .or_else(|| {
                execution
                    .request
                    .as_ref()
                    .map(|request| request.adapter_kind.to_string())
            })
            .or_else(|| {
                record.steps.get(&step_id).and_then(|step| {
                    step.last_request
                        .as_ref()
                        .map(|request| request.adapter_kind.to_string())
                })
            }),
        action: descriptor
            .action
            .clone()
            .or_else(|| {
                execution
                    .request
                    .as_ref()
                    .map(|request| request.action.clone())
            })
            .or_else(|| {
                record.steps.get(&step_id).and_then(|step| {
                    step.last_request
                        .as_ref()
                        .map(|request| request.action.clone())
                })
            }),
        status: normalize_execution_status(&execution.status),
        started_at_ms: execution.started_at_ms,
        finished_at_ms: execution.finished_at_ms,
        duration_ms: result.map(|result| result.timing.duration_ms).or_else(|| {
            match (execution.started_at_ms, execution.finished_at_ms) {
                (Some(started_at_ms), Some(finished_at_ms)) if finished_at_ms >= started_at_ms => {
                    Some(finished_at_ms - started_at_ms)
                }
                _ => None,
            }
        }),
        provider: usage.and_then(|usage| usage.provider.clone()),
        model: usage.and_then(|usage| usage.model.clone()),
        input_tokens: usage.and_then(|usage| usage.input_tokens),
        output_tokens: usage.and_then(|usage| usage.output_tokens),
        total_tokens: usage.and_then(|usage| usage.total_tokens),
        adapter_metrics: result
            .map(|result| result.metrics.clone())
            .unwrap_or_default(),
        artifact_count: counts.artifacts,
        evidence_count: counts.evidence,
    }
}

fn blob_counts_by_execution(
    home_dir: &Path,
    run_id: &str,
) -> Result<BTreeMap<(String, u32), BlobExecutionCounts>> {
    let store = LocalInspectionStore::new(home_dir);
    let mut out = BTreeMap::<(String, u32), BlobExecutionCounts>::new();
    for blob in store.list_blobs(run_id, BlobFamily::Artifact)? {
        let counts = out.entry((blob.step_id, blob.execution)).or_default();
        counts.artifacts = counts.artifacts.saturating_add(1);
    }
    for blob in store.list_blobs(run_id, BlobFamily::Evidence)? {
        let counts = out.entry((blob.step_id, blob.execution)).or_default();
        counts.evidence = counts.evidence.saturating_add(1);
    }
    Ok(out)
}

fn collect_step_ids(
    home_dir: &Path,
    record: &PersistedRunRecord,
    descriptors: &BTreeMap<String, StepDescriptor>,
) -> Result<Vec<String>> {
    let mut step_ids = record.steps.keys().cloned().collect::<BTreeSet<_>>();
    step_ids.extend(descriptors.keys().cloned());
    let executions_root = run_root(home_dir, &record.run_id)?.join("step_executions");
    if executions_root.exists() {
        for entry in fs::read_dir(&executions_root).with_context(|| {
            format!(
                "failed reading step executions dir: {}",
                executions_root.display()
            )
        })? {
            let entry = entry?;
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    step_ids.insert(name.to_string());
                }
            }
        }
    }
    Ok(step_ids.into_iter().collect())
}

fn step_descriptors(recipe: Option<&Recipe>) -> BTreeMap<String, StepDescriptor> {
    recipe
        .map(|recipe| {
            recipe
                .steps
                .iter()
                .map(|step| (step.id.clone(), step_descriptor(step)))
                .collect()
        })
        .unwrap_or_default()
}

fn step_descriptor(step: &RecipeStep) -> StepDescriptor {
    match &step.execution {
        transist_protocol::StepExecutionDescriptor::Invoke {
            adapter, action, ..
        } => StepDescriptor {
            step_kind: "invoke".to_string(),
            adapter_kind: Some(adapter.to_string()),
            action: Some(action.clone()),
            is_manual_gate: false,
        },
        transist_protocol::StepExecutionDescriptor::Control { primitive } => StepDescriptor {
            step_kind: "control".to_string(),
            adapter_kind: None,
            action: Some(primitive.clone()),
            is_manual_gate: primitive == "control.gate.manual",
        },
        transist_protocol::StepExecutionDescriptor::Subflow { .. } => StepDescriptor {
            step_kind: "subflow".to_string(),
            adapter_kind: None,
            action: None,
            is_manual_gate: false,
        },
    }
}

fn infer_step_kind_from_state(record: &PersistedRunRecord, step_id: &str) -> String {
    record
        .steps
        .get(step_id)
        .and_then(|step| step.last_request.as_ref().map(infer_step_kind_from_request))
        .unwrap_or_else(|| "unknown".to_string())
}

fn infer_step_kind_from_request(request: &ExecutionRequest) -> String {
    request
        .metadata
        .get("step_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("invoke")
        .to_string()
}

fn normalize_execution_status(raw: &str) -> String {
    match raw {
        "success" | "succeeded" => "succeeded".to_string(),
        "canceled" | "cancelled" => "cancelled".to_string(),
        other => other.to_string(),
    }
}

fn normalize_gate_decision(raw: &str) -> &str {
    match raw {
        "approved" => "approved",
        "rejected" => "rejected",
        _ => raw,
    }
}

fn terminal_status(status: KernelRunStatus) -> Option<String> {
    match status {
        KernelRunStatus::Success => Some("succeeded".to_string()),
        KernelRunStatus::Failed => Some("failed".to_string()),
        KernelRunStatus::Canceled => Some("cancelled".to_string()),
        _ => None,
    }
}

fn is_active_run(status: KernelRunStatus) -> bool {
    matches!(
        status,
        KernelRunStatus::Running | KernelRunStatus::Waiting | KernelRunStatus::Blocked
    )
}
