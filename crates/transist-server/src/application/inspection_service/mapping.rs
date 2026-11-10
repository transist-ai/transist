// SPDX-License-Identifier: MPL-2.0
use super::content::media_type_for_path;
use super::resource_ids::{encode_artifact_id, encode_evidence_id, encode_workflow_file_id};
use super::time::format_timestamp;
use crate::domain::api_models::{
    ArtifactLinks, ArtifactMetadata, ArtifactOutputRef, BlobDescriptorSummary, ChildFailureSummary,
    CleanupStepSummary, CleanupSummary, ContainedRunSummary, CurrentStepSummary,
    EvidenceExportView, EvidenceLinks, EvidenceMetadata, EvidenceOutputRef, ExternalTransferView,
    FilesystemPolicySummary, GateDecisionSummary, GateSummary, LocalityStepView, LocalityView,
    PolicyDecisionView, PreflightFailedCheck, PreflightSummary, ResourceLinks, RunBlobSummary,
    RunDetail, RunEventSummary, RunSummary, StepDetail, StepExecutionSummary, StepInputSummary,
    StepOutputSummary, StepSummary, TrustDecisionView, TrustSubjectView, TrustSummaryView,
    WorkflowDetail, WorkflowFileLinks, WorkflowFileMetadata, WorkflowFileOutputRef,
    WorkflowGraphEdge, WorkflowInput, WorkflowLinks, WorkflowPromptReference, WorkflowStep,
    WorkflowSummary, WorkflowValidation,
};
use std::path::{Path, PathBuf};
use transist::{
    InspectionBlob, InspectionBlobDescriptor, InspectionChildFailure, InspectionGateDecision,
    InspectionPreflightFailedCheck, InspectionRunDetail, InspectionRunSummary, InspectionStep,
    InspectionStepBlobRef, InspectionStepInputSummary, InspectionWorkflowDetail,
    InspectionWorkflowFileRef, InspectionWorkflowSummary,
};

const PUBLIC_PATH_REDACTED: &str = "[path redacted]";

pub(crate) fn map_run_summary(run: InspectionRunSummary) -> RunSummary {
    run.into()
}

impl From<InspectionRunSummary> for RunSummary {
    fn from(run: InspectionRunSummary) -> Self {
        let InspectionRunSummary {
            run_id,
            workflow_id,
            workflow_version,
            status,
            created_at_ms,
            updated_at_ms,
            started_at_ms,
            finished_at_ms,
            current_step_id,
            has_blocked_gates,
            has_artifacts,
            has_events,
            artifact_count,
            evidence_count,
            root_run_id,
            parent_run_id,
        } = run;
        let self_url = format!("/api/v1/runs/{run_id}");
        Self {
            run_id,
            workflow_id,
            workflow_version,
            status,
            created_at: format_timestamp(Some(created_at_ms)).unwrap_or_default(),
            updated_at: format_timestamp(Some(updated_at_ms)).unwrap_or_default(),
            started_at: format_timestamp(started_at_ms),
            finished_at: format_timestamp(finished_at_ms),
            current_step_id,
            has_blocked_gates,
            has_artifacts,
            has_events,
            artifact_count,
            evidence_count,
            root_run_id,
            parent_run_id,
            metrics: None,
            links: ResourceLinks {
                self_url: self_url.clone(),
                events: Some(format!("{self_url}/events")),
                steps: Some(format!("{self_url}/steps")),
                gates: Some(format!("{self_url}/gates")),
                artifacts: Some(format!("{self_url}/artifacts")),
                evidence: Some(format!("{self_url}/evidence")),
            },
        }
    }
}

pub(crate) fn map_run_detail(
    run: InspectionRunDetail,
    workspace_root: Option<&Path>,
) -> Result<RunDetail, super::ApiProblem> {
    let self_url = format!("/api/v1/runs/{}", run.summary.run_id);
    Ok(RunDetail {
        run_id: run.summary.run_id.clone(),
        workflow_id: run.summary.workflow_id,
        workflow_version: run.summary.workflow_version,
        status: run.summary.status,
        created_at: format_timestamp(Some(run.summary.created_at_ms)).unwrap_or_default(),
        updated_at: format_timestamp(Some(run.summary.updated_at_ms)).unwrap_or_default(),
        started_at: format_timestamp(run.summary.started_at_ms),
        finished_at: format_timestamp(run.summary.finished_at_ms),
        workflow_input_summary: serde_json::Value::from(transist::KernelValue::Object(
            run.workflow_inputs,
        )),
        global_default_filesystem_policy: run
            .global_default_filesystem_policy
            .map(|policy| map_filesystem_policy(policy, workspace_root)),
        workflow_filesystem_policy: run
            .workflow_filesystem_policy
            .map(|policy| map_filesystem_policy(policy, workspace_root)),
        current_step: run.current_step.map(|step| CurrentStepSummary {
            step_id: step.step_id,
            status: step.status,
        }),
        actionable_run_id: run.actionable_run_id,
        terminal_outcome: run.terminal_outcome,
        recovery_state: run.recovery_state,
        preflight: run.preflight.map(map_preflight_summary),
        cleanup: run.cleanup.map(|cleanup| CleanupSummary {
            trigger: cleanup.trigger,
            state: cleanup.state,
            started_at: format_timestamp(cleanup.started_at_ms),
            finished_at: format_timestamp(cleanup.finished_at_ms),
            preflight: cleanup.preflight.map(map_preflight_summary),
            steps: cleanup
                .steps
                .into_iter()
                .map(|step| CleanupStepSummary {
                    step_id: step.step_id,
                    status: step.status,
                    execution: step.execution,
                    started_at: format_timestamp(step.started_at_ms),
                    finished_at: format_timestamp(step.finished_at_ms),
                    error_message: step.error_message,
                })
                .collect(),
        }),
        active_lease: run
            .active_lease
            .and_then(|lease| serde_json::to_value(lease).ok()),
        blocked_gates: run
            .blocked_gates
            .into_iter()
            .map(|gate| map_gate_summary(&run.summary.run_id, gate))
            .collect::<Result<_, _>>()?,
        artifact_summary: RunBlobSummary {
            count: run.artifact_count,
        },
        evidence_summary: RunBlobSummary {
            count: run.evidence_count,
        },
        event_summary: RunEventSummary {
            count: run.event_count,
            latest_event_id: run.latest_event_id,
        },
        root_run_id: run.summary.root_run_id.clone(),
        parent_run_id: run.summary.parent_run_id.clone(),
        contained_runs: run
            .contained_runs
            .into_iter()
            .map(map_contained_run_summary)
            .collect(),
        lineage: run
            .lineage
            .and_then(|lineage| serde_json::to_value(lineage).ok()),
        step_counts: serde_json::to_value(run.step_counts)
            .unwrap_or_else(|_| serde_json::json!({})),
        metrics: None,
        trusted_execution_mode: run.trusted_execution_mode,
        runtime_mode: run.runtime_mode,
        policy_profile: run.policy_profile,
        capability_enforcement: run.capability_enforcement,
        trust: map_trust_summary(run.trust),
        locality: map_locality_view(run.locality),
        evidence_export: EvidenceExportView {
            location: run.evidence_export.location,
            command: run.evidence_export.command,
            format: run.evidence_export.format,
            generated_at: format_timestamp(run.evidence_export.generated_at_ms),
        },
        links: ResourceLinks {
            self_url: self_url.clone(),
            events: Some(format!("{self_url}/events")),
            steps: Some(format!("{self_url}/steps")),
            gates: Some(format!("{self_url}/gates")),
            artifacts: Some(format!("{self_url}/artifacts")),
            evidence: Some(format!("{self_url}/evidence")),
        },
    })
}

pub(crate) fn map_gate_summary(
    run_id: &str,
    gate: transist::InspectionGate,
) -> Result<GateSummary, super::ApiProblem> {
    let review_documents = gate.review_documents().map_err(|e| {
        super::ApiProblem::log_and_storage_query_failure("gate_review_documents", e)
    })?;
    Ok(GateSummary {
        review_documents,
        run_id: run_id.into(),
        gate_id: gate.gate_id,
        status: gate.status,
        step_id: gate.step_id,
        created_at: format_timestamp(gate.created_at_ms),
        decision: gate.decision.map(Into::into),
        decision_history: gate
            .decision_history
            .into_iter()
            .map(map_gate_decision)
            .collect(),
        request_summary: serde_json::to_value(gate.request_summary).unwrap_or_default(),
    })
}

pub(crate) fn map_trust_summary(summary: transist::InspectionTrustSummary) -> TrustSummaryView {
    TrustSummaryView {
        trusted_execution_mode: summary.trusted_execution_mode,
        workflow: summary.workflow.map(map_trust_subject),
        plugins: summary.plugins.into_iter().map(map_trust_subject).collect(),
        decision: summary.decision.map(|decision| TrustDecisionView {
            outcome: decision.outcome,
            reason: decision.reason,
            failed_verification_type: decision.failed_verification_type,
        }),
        policy_decision: summary.policy_decision.map(|policy| PolicyDecisionView {
            allowed: policy.allowed,
            mode: policy.mode,
            reason_code: policy.reason_code,
            message: policy.message,
            runtime_mode: policy.runtime_mode,
            policy_profile: policy.policy_profile,
            capability_enforcement: policy.capability_enforcement,
        }),
    }
}

fn map_trust_subject(subject: transist::InspectionTrustSubject) -> TrustSubjectView {
    TrustSubjectView {
        id: subject.id,
        version: subject.version,
        digest: subject.digest,
        signature_status: subject.signature_status,
        signer_key_id: subject.signer_key_id,
    }
}

pub(crate) fn map_locality_view(locality: transist::InspectionLocalityView) -> LocalityView {
    LocalityView {
        steps: locality
            .steps
            .into_iter()
            .map(|step| LocalityStepView {
                step_id: step.step_id,
                provider: step.provider,
                model: step.model,
                provider_kind: step.provider_kind,
            })
            .collect(),
        external_transfers: locality
            .external_transfers
            .into_iter()
            .map(|transfer| ExternalTransferView {
                step_id: transfer.step_id,
                execution: transfer.execution,
                timestamp: format_timestamp(Some(transfer.timestamp_ms)).unwrap_or_default(),
                destination: transfer.destination,
                adapter_kind: transfer.adapter_kind,
                provider: transfer.provider,
                provider_kind: transfer.provider_kind,
                bytes_sent: transfer.bytes_sent,
                content_hash: transfer.content_hash,
                policy_mode: transfer.policy_mode,
            })
            .collect(),
    }
}

fn map_contained_run_summary(run: InspectionRunSummary) -> ContainedRunSummary {
    ContainedRunSummary {
        run_id: run.run_id,
        workflow_id: run.workflow_id,
        workflow_version: run.workflow_version,
        status: run.status,
        created_at: format_timestamp(Some(run.created_at_ms)).unwrap_or_default(),
        updated_at: format_timestamp(Some(run.updated_at_ms)).unwrap_or_default(),
        current_step_id: run.current_step_id,
        artifact_count: run.artifact_count,
        evidence_count: run.evidence_count,
        parent_run_id: run.parent_run_id,
    }
}

pub(crate) fn map_step_summary(
    run_id: &str,
    step: InspectionStep,
    workspace_root: Option<&Path>,
) -> StepSummary {
    let policy_workspace_root = step
        .input_summary
        .filesystem_policy
        .as_ref()
        .and_then(|policy| policy.workspace_root.clone())
        .map(PathBuf::from);
    let workspace_root = workspace_root.or(policy_workspace_root.as_deref());
    StepSummary {
        run_id: run_id.to_string(),
        step_id: step.step_id,
        step_kind: step.step_kind,
        status: step.status,
        execution: step.execution,
        started_at: format_timestamp(step.started_at_ms),
        finished_at: format_timestamp(step.finished_at_ms),
        input_summary: map_step_input_summary(step.input_summary, workspace_root),
        output_summary: StepOutputSummary {
            result_present: step.output_summary.result_present,
            invoke_id: step.output_summary.invoke_id,
            status: step.output_summary.status,
            exit_code: step.output_summary.exit_code,
            error_code: step.output_summary.error_code,
            error_message: step.output_summary.error_message,
            stderr_tail: step.output_summary.stderr_tail,
            child_run_id: step.output_summary.child_run_id,
            child_failure: step.output_summary.child_failure.map(Into::into),
            output_object_keys: step.output_summary.output_object_keys,
            artifact_count: step.output_summary.artifact_count,
            evidence_count: step.output_summary.evidence_count,
            artifact_refs: step
                .output_summary
                .artifact_refs
                .into_iter()
                .map(map_artifact_output_ref)
                .collect(),
            evidence_refs: step
                .output_summary
                .evidence_refs
                .into_iter()
                .map(map_evidence_output_ref)
                .collect(),
            workflow_files: step
                .output_summary
                .workflow_files
                .into_iter()
                .map(|reference| map_workflow_file_output_ref(reference, workspace_root))
                .collect(),
        },
        is_current_active: step.is_current_active,
        executions: step
            .executions
            .into_iter()
            .map(|exec| StepExecutionSummary {
                execution: exec.execution,
                status: exec.status,
                started_at: format_timestamp(exec.started_at_ms),
                finished_at: format_timestamp(exec.finished_at_ms),
            })
            .collect(),
    }
}

pub(crate) fn map_step_detail(
    summary: StepSummary,
    output: crate::domain::api_models::StepOutput,
    output_executions: Vec<u32>,
) -> StepDetail {
    StepDetail {
        run_id: summary.run_id,
        step_id: summary.step_id,
        step_kind: summary.step_kind,
        status: summary.status,
        execution: summary.execution,
        started_at: summary.started_at,
        finished_at: summary.finished_at,
        input_summary: summary.input_summary,
        output_summary: summary.output_summary,
        is_current_active: summary.is_current_active,
        executions: summary.executions,
        output,
        output_executions,
        metrics: None,
    }
}

fn map_artifact_output_ref(reference: InspectionStepBlobRef) -> ArtifactOutputRef {
    ArtifactOutputRef {
        artifact_id: encode_artifact_id(
            &reference.step_id,
            reference.execution,
            &reference.relative_path,
        ),
        name: reference.name,
        media_type: media_type_for_path(&reference.relative_path),
        size: reference.bytes,
    }
}

fn map_evidence_output_ref(reference: InspectionStepBlobRef) -> EvidenceOutputRef {
    EvidenceOutputRef {
        evidence_id: encode_evidence_id(
            &reference.step_id,
            reference.execution,
            &reference.relative_path,
        ),
        name: reference.name,
        media_type: media_type_for_path(&reference.relative_path),
        size: reference.bytes,
    }
}

fn map_workflow_file_output_ref(
    reference: InspectionWorkflowFileRef,
    workspace_root: Option<&Path>,
) -> WorkflowFileOutputRef {
    let InspectionWorkflowFileRef {
        step_id,
        field_name,
        locator_kind,
        relative_path,
        local_path: _,
        name,
        bytes,
    } = reference;
    let workflow_file_id = Some(encode_workflow_file_id(
        locator_kind,
        &step_id,
        &field_name,
        &relative_path,
    ));
    WorkflowFileOutputRef {
        workflow_file_id,
        field_name,
        locator_kind: workflow_file_locator_kind_name(locator_kind),
        name,
        relative_path: public_relative_path(&relative_path, workspace_root),
        media_type: media_type_for_path(&relative_path),
        size: bytes,
    }
}

fn map_preflight_summary(preflight: transist::InspectionPreflightSummary) -> PreflightSummary {
    PreflightSummary {
        status: preflight.status,
        checked_at: format_timestamp(Some(preflight.checked_at_ms)).unwrap_or_default(),
        blocker_count: preflight.blocker_count,
        warning_count: preflight.warning_count,
        failed_checks: preflight
            .failed_checks
            .into_iter()
            .map(Into::into)
            .collect(),
    }
}

pub(crate) fn map_artifact_metadata(
    _run_id: &str,
    artifact_id: &str,
    artifact: InspectionBlob,
) -> ArtifactMetadata {
    let owner_run_id = artifact.run_id.clone();
    let self_url = format!("/api/v1/runs/{owner_run_id}/artifacts/{artifact_id}");
    ArtifactMetadata {
        run_id: owner_run_id,
        artifact_id: artifact_id.to_string(),
        name: artifact.name.clone(),
        media_type: media_type_for_path(&artifact.relative_path),
        size: artifact.bytes,
        producing_step_id: artifact.step_id,
        execution: artifact.execution,
        created_at: format_timestamp(artifact.created_at_ms),
        descriptor: artifact.descriptor.map(map_blob_descriptor_summary),
        links: ArtifactLinks {
            self_url: self_url.clone(),
            content: format!("{self_url}/content"),
        },
    }
}

pub(crate) fn map_evidence_metadata(
    _run_id: &str,
    evidence_id: &str,
    evidence: InspectionBlob,
) -> EvidenceMetadata {
    let owner_run_id = evidence.run_id.clone();
    let self_url = format!("/api/v1/runs/{owner_run_id}/evidence/{evidence_id}");
    EvidenceMetadata {
        run_id: owner_run_id,
        evidence_id: evidence_id.to_string(),
        name: evidence.name.clone(),
        media_type: media_type_for_path(&evidence.relative_path),
        size: evidence.bytes,
        producing_step_id: evidence.step_id,
        execution: evidence.execution,
        created_at: format_timestamp(evidence.created_at_ms),
        descriptor: evidence.descriptor.map(map_blob_descriptor_summary),
        links: EvidenceLinks {
            self_url: self_url.clone(),
            content: format!("{self_url}/content"),
        },
    }
}

pub(crate) fn map_workflow_file_metadata(
    run_id: &str,
    workflow_file_id: &str,
    file: InspectionWorkflowFileRef,
    workspace_root: Option<&Path>,
    content_available: bool,
) -> WorkflowFileMetadata {
    let self_url = format!("/api/v1/runs/{run_id}/workflow-files/{workflow_file_id}");
    WorkflowFileMetadata {
        run_id: run_id.to_string(),
        workflow_file_id: workflow_file_id.to_string(),
        field_name: file.field_name,
        locator_kind: workflow_file_locator_kind_name(file.locator_kind),
        name: file.name,
        relative_path: public_relative_path(&file.relative_path, workspace_root),
        media_type: media_type_for_path(&file.relative_path),
        size: file.bytes,
        producing_step_id: file.step_id,
        links: WorkflowFileLinks {
            self_url: self_url.clone(),
            content: content_available.then(|| format!("{self_url}/content")),
        },
    }
}

fn workflow_file_locator_kind_name(kind: transist::InspectionWorkflowFileLocatorKind) -> String {
    match kind {
        transist::InspectionWorkflowFileLocatorKind::Workspace => "workspace".to_string(),
        transist::InspectionWorkflowFileLocatorKind::RepositoryState => {
            "repository_state".to_string()
        }
        transist::InspectionWorkflowFileLocatorKind::Run => "run".to_string(),
    }
}

fn map_blob_descriptor_summary(descriptor: InspectionBlobDescriptor) -> BlobDescriptorSummary {
    BlobDescriptorSummary {
        source_path_basename: portable_basename(&descriptor.source_path),
        sha256: descriptor.sha256,
        bytes: descriptor.bytes,
        invoke_id: descriptor.invoke_id,
        status: descriptor.status,
    }
}

pub(crate) fn map_workflow_summary(
    workflow: InspectionWorkflowSummary,
    workspace_root: Option<&Path>,
) -> WorkflowSummary {
    let workflow_ref = format!("{}@{}", workflow.workflow_id, workflow.workflow_version);
    WorkflowSummary {
        workflow_ref: workflow_ref.clone(),
        workflow_id: workflow.workflow_id,
        workflow_version: workflow.workflow_version,
        source_ref: workflow
            .source_ref
            .map(|path| public_relative_path(&path, workspace_root)),
        title: workflow.title,
        input_schema_summary: workflow
            .inputs
            .into_iter()
            .map(|input| WorkflowInput {
                name: input.name,
                value_type: input.value_type,
                required: input.required,
                has_default: input.has_default,
                description: input.description,
            })
            .collect(),
        compiled_recipe_available: workflow.compiled_recipe_available,
        runnable_in_principle: workflow.runnable_in_principle,
        step_count: workflow.step_count,
        root_step_count: workflow.root_step_count,
        is_subflow: workflow.is_subflow,
        links: WorkflowLinks {
            self_url: format!("/api/v1/workflows/{workflow_ref}"),
        },
    }
}

pub(crate) fn map_workflow_detail(
    workflow: InspectionWorkflowDetail,
    workspace_root: Option<&Path>,
) -> WorkflowDetail {
    let workflow_ref = format!(
        "{}@{}",
        workflow.summary.workflow_id, workflow.summary.workflow_version
    );
    WorkflowDetail {
        workflow_ref,
        workflow_id: workflow.summary.workflow_id,
        workflow_version: workflow.summary.workflow_version,
        source_ref: workflow
            .summary
            .source_ref
            .map(|path| public_relative_path(&path, workspace_root)),
        title: workflow.summary.title,
        input_declarations: workflow
            .summary
            .inputs
            .into_iter()
            .map(|input| WorkflowInput {
                name: input.name,
                value_type: input.value_type,
                required: input.required,
                has_default: input.has_default,
                description: input.description,
            })
            .collect(),
        step_graph_summary: workflow
            .steps
            .into_iter()
            .map(|step| WorkflowStep {
                step_id: step.step_id,
                parent_step_id: step.parent_step_id,
                child_step_ids: step.child_step_ids,
                kind: step.kind,
                adapter: step.adapter,
                plugin: step.plugin,
                action: step.action,
                execution_kind: step.execution_kind,
                control: step.control,
                subflow_path: step
                    .subflow_path
                    .map(|path| public_relative_path(&path, workspace_root)),
                with_keys: step.with_keys,
                subflow_input_keys: step.subflow_input_keys,
                workflow_input_refs: step.workflow_input_refs,
                if_guard: step.if_guard,
                if_then_refs: step.if_then_refs,
                if_else_refs: step.if_else_refs,
                loop_while: step.loop_while,
                loop_until: step.loop_until,
                loop_body_refs: step.loop_body_refs,
                loop_max_iters: step.loop_max_iters,
                loop_max_duration: step.loop_max_duration,
                prompt_source: step.prompt_source,
                prompt_detail: step.prompt_detail,
                skills: step.skills,
                graph_edges: step
                    .graph_edges
                    .into_iter()
                    .map(|edge| WorkflowGraphEdge {
                        kind: edge.kind,
                        label: edge.label,
                        target_step_id: edge.target_step_id,
                        guard: edge.guard,
                        loop_while: edge.loop_while,
                        loop_until: edge.loop_until,
                        loop_max_iters: edge.loop_max_iters,
                        loop_routing_version: edge.loop_routing_version,
                    })
                    .collect(),
            })
            .collect(),
        prompt_references: workflow
            .prompt_references
            .into_iter()
            .map(|prompt| WorkflowPromptReference {
                step_id: prompt.step_id,
                source: prompt.source,
                path: prompt
                    .path
                    .map(|path| public_relative_path(&path, workspace_root)),
                preview: prompt.preview,
            })
            .collect(),
        validation: WorkflowValidation {
            status: workflow.validation.status,
            errors: workflow.validation.errors,
        },
        compiled_recipe_available: workflow.summary.compiled_recipe_available,
        runnable_in_principle: workflow.summary.runnable_in_principle,
        is_subflow: workflow.summary.is_subflow,
    }
}

pub(crate) fn map_gate_decision(decision: InspectionGateDecision) -> GateDecisionSummary {
    decision.into()
}

fn map_filesystem_policy(
    policy: transist::InspectionFilesystemPolicySummary,
    workspace_root: Option<&Path>,
) -> FilesystemPolicySummary {
    let persisted_workspace_root = policy.workspace_root.as_deref().map(Path::new);
    let policy_workspace_root = workspace_root.or(persisted_workspace_root);
    FilesystemPolicySummary {
        source: policy.source,
        workspace_root: policy_workspace_root.map(|_| ".".to_string()),
        workspace_access: policy.workspace_access,
        read_allow_paths: policy
            .read_allow_paths
            .into_iter()
            .map(|path| public_relative_path(&path, policy_workspace_root))
            .collect(),
        write_allow_paths: policy
            .write_allow_paths
            .into_iter()
            .map(|path| public_relative_path(&path, policy_workspace_root))
            .collect(),
    }
}

fn public_relative_path(path: &str, workspace_root: Option<&Path>) -> String {
    let Some(normalized) = normalize_portable_path(path) else {
        return PUBLIC_PATH_REDACTED.to_string();
    };

    if let Some(root) = workspace_root {
        if let (Ok(path), Ok(root)) = (Path::new(path).canonicalize(), root.canonicalize()) {
            if let Ok(relative) = path.strip_prefix(root) {
                return relative.display().to_string();
            }
        }

        if let Some(relative) = normalized_relative_path(&normalized, &root.to_string_lossy()) {
            return relative;
        }
    }

    if is_safe_relative_path(&normalized) {
        return normalized;
    }

    PUBLIC_PATH_REDACTED.to_string()
}

fn normalize_portable_path(path: &str) -> Option<String> {
    let mut replaced = path.replace('\\', "/");
    while let Some(stripped) = replaced.strip_prefix("./") {
        replaced = stripped.to_string();
    }
    let absolute = replaced.starts_with('/');
    let mut components = Vec::new();
    for component in replaced.split('/') {
        match component {
            "" | "." => {}
            ".." if components.last().is_some_and(|last| *last != "..") => {
                components.pop();
            }
            ".." if absolute => return None,
            component => components.push(component),
        }
    }
    let joined = components.join("/");
    Some(if absolute {
        format!("/{joined}")
    } else {
        joined
    })
}

fn normalized_relative_path(path: &str, root: &str) -> Option<String> {
    let normalized_root = normalize_portable_path(root)?;
    if path == normalized_root {
        return Some(".".to_string());
    }
    path.strip_prefix(&format!("{normalized_root}/"))
        .filter(|relative| is_safe_relative_path(relative))
        .map(ToString::to_string)
}

fn is_safe_relative_path(path: &str) -> bool {
    !(path.is_empty()
        || path.starts_with('/')
        || path.starts_with("~/")
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && path.as_bytes()[2] == b'/')
        || path.split('/').any(|component| component == ".."))
}

fn portable_basename(path: &str) -> String {
    path.replace('\\', "/")
        .rsplit('/')
        .find(|component| !component.is_empty())
        .unwrap_or("resource")
        .to_string()
}

impl From<InspectionGateDecision> for GateDecisionSummary {
    fn from(decision: InspectionGateDecision) -> Self {
        let InspectionGateDecision {
            review_request,
            state,
            actor,
            reason,
            decided_at_ms,
        } = decision;
        Self {
            review_request,
            state,
            actor,
            reason,
            decided_at: format_timestamp(decided_at_ms),
        }
    }
}

impl From<InspectionChildFailure> for ChildFailureSummary {
    fn from(failure: InspectionChildFailure) -> Self {
        let InspectionChildFailure {
            child_run_id,
            failed_step_id,
            error_code,
            error_message,
            stderr,
            preflight_blocker_count,
            preflight_warning_count,
            preflight_first_blocker,
        } = failure;
        Self {
            child_run_id,
            failed_step_id,
            error_code,
            error_message,
            stderr,
            preflight_blocker_count,
            preflight_warning_count,
            preflight_first_blocker,
        }
    }
}

impl From<InspectionPreflightFailedCheck> for PreflightFailedCheck {
    fn from(check: InspectionPreflightFailedCheck) -> Self {
        let InspectionPreflightFailedCheck {
            kind,
            message,
            step_ids,
            plugin_id,
            action,
        } = check;
        Self {
            kind,
            message,
            step_ids,
            plugin_id,
            action,
        }
    }
}

fn map_step_input_summary(
    summary: InspectionStepInputSummary,
    workspace_root: Option<&Path>,
) -> StepInputSummary {
    let InspectionStepInputSummary {
        request_present,
        invoke_id,
        adapter_kind,
        plugin_id,
        action,
        input_object_keys,
        metadata_object_keys,
        filesystem_policy,
    } = summary;
    StepInputSummary {
        request_present,
        invoke_id,
        adapter_kind,
        plugin_id,
        action,
        input_object_keys,
        metadata_object_keys,
        filesystem_policy: filesystem_policy
            .map(|policy| map_filesystem_policy(policy, workspace_root)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_paths_preserve_nested_workspace_paths_and_hide_host_forms() {
        let workspace = Path::new("/workspace");
        assert_eq!(
            public_relative_path("/workspace/reports/nested/output.md", Some(workspace)),
            "reports/nested/output.md"
        );
        assert_eq!(
            public_relative_path("~/secrets/token", None),
            PUBLIC_PATH_REDACTED
        );
        assert_eq!(
            public_relative_path("C:\\Users\\alice\\secrets\\token", None),
            PUBLIC_PATH_REDACTED
        );
        assert_eq!(
            public_relative_path("\\\\server\\share\\secrets\\token", None),
            PUBLIC_PATH_REDACTED
        );
        assert_eq!(
            public_relative_path("../secrets/token", None),
            PUBLIC_PATH_REDACTED
        );
        assert_eq!(
            public_relative_path("./C:\\Users\\alice\\secrets\\token", None),
            PUBLIC_PATH_REDACTED
        );
        assert_eq!(
            public_relative_path("./~/.ssh/id_rsa", None),
            PUBLIC_PATH_REDACTED
        );
        assert_eq!(
            public_relative_path("./\\\\server\\share\\secret.txt", None),
            PUBLIC_PATH_REDACTED
        );
        assert_eq!(
            public_relative_path("/workspace/../secret", Some(workspace)),
            PUBLIC_PATH_REDACTED
        );
    }

    #[test]
    fn workflow_file_metadata_keeps_nested_relative_path() {
        let file = InspectionWorkflowFileRef {
            step_id: "collect".to_string(),
            field_name: "output_file".to_string(),
            locator_kind: transist::InspectionWorkflowFileLocatorKind::Workspace,
            relative_path: "reports/nested/output.md".to_string(),
            local_path: Some(
                Path::new("/private/workspace/reports/nested/output.md").to_path_buf(),
            ),
            name: "output.md".to_string(),
            bytes: 12,
        };
        let metadata = map_workflow_file_metadata(
            "run",
            "wf_opaque",
            file.clone(),
            Some(Path::new("/workspace")),
            true,
        );
        assert_eq!(metadata.relative_path, "reports/nested/output.md");
        assert_eq!(metadata.workflow_file_id, "wf_opaque");
        let json = serde_json::to_string(&metadata).expect("metadata JSON");
        assert!(!json.contains("/private/workspace"));
        assert!(!json.contains("local_path"));
        let output = map_workflow_file_output_ref(file, Some(Path::new("/workspace")));
        let json = serde_json::to_string(&output).expect("output JSON");
        assert!(!json.contains("/private/workspace"));
        assert!(!json.contains("local_path"));
    }

    #[test]
    fn blob_descriptor_basename_is_independent_of_host_os() {
        assert_eq!(
            portable_basename(r"C:\Users\alice\output.txt"),
            "output.txt"
        );
        assert_eq!(
            portable_basename(r"\\server\share\output.txt"),
            "output.txt"
        );
    }
}
