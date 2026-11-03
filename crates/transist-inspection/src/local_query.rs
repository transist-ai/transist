// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::path::{Component, Path};
use transist_core::LeasePort;
use transist_core::{ManagementMode, RuntimeOwnerIdentity};
use transist_core::{RunProvenanceRecord, TrustDecisionRecord};
use transist_protocol::{EXTERNAL_TRANSFER_SCHEMA_V1, ExternalTransferRecord};
use transist_storage_local::inspection::InspectionCache;
use transist_storage_local::run_paths::run_provenance_path;

/// Local filesystem-backed implementation of [`InspectionQueryPort`].
#[derive(Debug)]
pub struct LocalInspectionQuery {
    repo_root: PathBuf,
    home_dir: PathBuf,
    store: LocalInspectionStore,
    summaries: LocalRunSummaryStore,
}

impl LocalInspectionQuery {
    /// Creates a local inspection query service for one repository and Transist home.
    pub fn new(repo_root: impl Into<PathBuf>, home_dir: impl Into<PathBuf>) -> Self {
        Self::with_cache(
            repo_root,
            home_dir,
            std::sync::Arc::new(InspectionCache::with_default_capacity(10)),
        )
    }

    /// Creates a local inspection query service with an explicit shared cache.
    pub fn with_cache(
        repo_root: impl Into<PathBuf>,
        home_dir: impl Into<PathBuf>,
        cache: std::sync::Arc<InspectionCache>,
    ) -> Self {
        let home_dir = home_dir.into();
        Self {
            repo_root: repo_root.into(),
            home_dir: home_dir.clone(),
            store: LocalInspectionStore::with_cache(
                home_dir.clone(),
                std::sync::Arc::clone(&cache),
            ),
            summaries: LocalRunSummaryStore::with_cache(home_dir, cache),
        }
    }

    fn load_existing_run_record(&self, run_id: &str) -> Result<Option<PersistedRunRecord>> {
        if !self.store.run_exists(run_id) {
            return Ok(None);
        }
        let mut record = self.store.load_run_record(run_id)?;
        transist_storage_local::run_paths::rebase_persisted_run_record(
            &mut record,
            &self.home_dir,
            &self.repo_root,
        )?;
        Ok(Some(record))
    }

    fn get_run_with_projected(
        &self,
        run_id: &str,
        stored_summaries: &[PersistedRunSummaryRecord],
        projected: &[InspectionRunSummary],
        projected_by_id: &BTreeMap<String, InspectionRunSummary>,
    ) -> Result<Option<InspectionRunDetail>> {
        let record = match self.load_existing_run_record(run_id)? {
            Some(record) => record,
            None => return Ok(None),
        };
        let steps = build_steps(
            &self.store,
            &record,
            run_id,
            stored_summaries,
            projected_by_id,
        )?;
        let gates = build_gates(&self.store, &record, run_id)?;
        let artifacts = self.store.list_blobs(run_id, BlobFamily::Artifact)?.len();
        let evidence = self.store.list_blobs(run_id, BlobFamily::Evidence)?.len();
        let events = self.store.list_events(run_id)?;
        let recipe = self.store.load_run_recipe(run_id)?;
        let summary = match projected
            .iter()
            .find(|summary| summary.run_id == run_id)
            .cloned()
        {
            Some(summary) => summary,
            None => build_run_summary(&self.store, &record)?,
        };
        let contained_runs =
            list_contained_runs_from_summaries(projected, &summary.root_run_id, &summary.run_id);
        let actionable_run_id = resolve_actionable_run_id(&self.store, projected, &summary.run_id)?;
        let active_lease = transist_storage_local::LocalLeaseStore::new(
            &self.home_dir,
            local_runtime_owner_identity()?,
        )
        .inspect_root_run_tree_lease(&summary.root_run_id)?;
        let provenance = read_run_provenance(&self.home_dir, run_id)?;
        let trust = build_trust_summary(&record, provenance.as_ref());
        let locality = build_locality_view(&self.store, run_id, provenance.as_ref())?;
        let evidence_export = build_evidence_export_view(run_id, provenance.as_ref());
        Ok(Some(InspectionRunDetail {
            current_step: summary.current_step_id.as_ref().and_then(|step_id| {
                steps
                    .iter()
                    .find(|step| step.step_id == *step_id)
                    .map(|step| RunCurrentStep {
                        step_id: step.step_id.clone(),
                        status: step.status.clone(),
                    })
            }),
            actionable_run_id,
            terminal_outcome: is_terminal_run_status(&summary.status)
                .then(|| terminal_outcome(&record))
                .flatten(),
            recovery_state: record
                .recovery_state
                .map(|state| state.as_str().to_string()),
            workflow_inputs: record.workflow_inputs.clone(),
            global_default_filesystem_policy: recipe
                .as_ref()
                .and_then(infer_global_default_filesystem_policy),
            workflow_filesystem_policy: recipe.as_ref().and_then(infer_workflow_filesystem_policy),
            preflight: record.preflight.as_ref().map(preflight_summary_from_record),
            cleanup: record.cleanup.as_ref().map(cleanup_summary_from_record),
            active_lease,
            blocked_gates: gates
                .iter()
                .filter(|gate| gate.status == "blocked")
                .cloned()
                .collect(),
            step_counts: count_step_statuses(&record.steps.values().cloned().collect::<Vec<_>>()),
            lineage: record.lineage.clone(),
            contained_runs,
            summary,
            steps,
            artifact_count: artifacts,
            evidence_count: evidence,
            event_count: events.len(),
            latest_event_id: events.last().map(|event| event.sequence.to_string()),
            trusted_execution_mode: record.trusted_execution_mode,
            runtime_mode: record.runtime_mode.clone(),
            policy_profile: record.policy_profile.clone(),
            capability_enforcement: record.capability_enforcement.clone(),
            trust,
            locality,
            evidence_export,
        }))
    }
}

fn read_run_provenance(home_dir: &Path, run_id: &str) -> Result<Option<RunProvenanceRecord>> {
    let path = run_provenance_path(home_dir, run_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let record = serde_json::from_str::<RunProvenanceRecord>(&raw)?;
    Ok(Some(record))
}

fn build_trust_summary(
    record: &PersistedRunRecord,
    provenance: Option<&RunProvenanceRecord>,
) -> InspectionTrustSummary {
    let workflow = record
        .trust_decision
        .as_ref()
        .map(|decision| trust_subject_from_decision(&decision.workflow));
    let plugins = record
        .trust_decision
        .as_ref()
        .map(|decision| {
            decision
                .plugins
                .iter()
                .map(trust_subject_from_decision)
                .collect()
        })
        .unwrap_or_default();
    let decision = record
        .trust_decision
        .as_ref()
        .map(|decision| InspectionTrustDecision {
            outcome: trust_outcome_from_record(decision),
            reason: decision.reason_code.clone(),
            failed_verification_type: decision.failed_verification_type.clone(),
        });
    let policy_decision = provenance.map(|provenance| InspectionPolicyDecision {
        allowed: provenance.policy_decision.allowed,
        mode: provenance.policy_decision.mode.clone(),
        reason_code: provenance.policy_decision.reason_code.clone(),
        message: provenance.policy_decision.message.clone(),
        runtime_mode: provenance.runtime_mode.clone(),
        policy_profile: provenance.policy_profile.clone(),
        capability_enforcement: provenance.capability_enforcement.clone(),
    });

    InspectionTrustSummary {
        trusted_execution_mode: record.trusted_execution_mode,
        workflow,
        plugins,
        decision,
        policy_decision,
    }
}

fn trust_subject_from_decision(
    subject: &transist_core::TrustSubjectDecision,
) -> InspectionTrustSubject {
    InspectionTrustSubject {
        id: subject.id.clone(),
        version: subject.version.clone(),
        digest: subject.digest.clone(),
        signature_status: subject.trust_decision.clone(),
        signer_key_id: subject.signer_key_id.clone(),
    }
}

fn trust_outcome_from_record(decision: &TrustDecisionRecord) -> String {
    if decision.decision == "denied" {
        "denied".to_string()
    } else if decision.decision == "bypassed"
        || decision.workflow.trust_decision == "bypassed"
        || decision
            .plugins
            .iter()
            .any(|plugin| plugin.trust_decision == "bypassed")
    {
        "bypassed".to_string()
    } else {
        "verified".to_string()
    }
}

fn build_locality_view(
    store: &LocalInspectionStore,
    run_id: &str,
    provenance: Option<&RunProvenanceRecord>,
) -> Result<InspectionLocalityView> {
    let steps = provenance
        .map(|provenance| {
            provenance
                .providers
                .iter()
                .map(|provider| InspectionLocalityStep {
                    step_id: provider.step_id.clone(),
                    provider: provider.provider.clone(),
                    model: provider.model.clone(),
                    provider_kind: provider.provider_kind.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    let mut external_transfers = Vec::new();
    for blob in store.list_blobs(run_id, BlobFamily::Evidence)? {
        if !blob
            .relative_path
            .starts_with("transist_external_transfer_")
            || !blob.relative_path.ends_with(".json")
        {
            continue;
        }
        let bytes = store.read_blob_content(
            run_id,
            BlobFamily::Evidence,
            &blob.step_id,
            blob.execution,
            &blob.relative_path,
        )?;
        let Ok(transfer) = serde_json::from_slice::<ExternalTransferRecord>(&bytes) else {
            continue;
        };
        if transfer.schema != EXTERNAL_TRANSFER_SCHEMA_V1 {
            continue;
        }
        external_transfers.push(InspectionExternalTransfer {
            step_id: transfer.step_id,
            execution: transfer.execution,
            timestamp_ms: transfer.timestamp_ms,
            destination: transfer.destination,
            adapter_kind: transfer.adapter_kind.to_string(),
            provider: transfer.provider,
            provider_kind: transfer.provider_kind.as_str().to_string(),
            bytes_sent: transfer.bytes_sent,
            content_hash: transfer.content_hash,
            policy_mode: transfer.policy_decision.mode,
        });
    }
    external_transfers.sort_by(|left, right| {
        left.timestamp_ms
            .cmp(&right.timestamp_ms)
            .then(left.step_id.cmp(&right.step_id))
            .then(left.execution.cmp(&right.execution))
    });

    Ok(InspectionLocalityView {
        steps,
        external_transfers,
    })
}

fn build_evidence_export_view(
    run_id: &str,
    provenance: Option<&RunProvenanceRecord>,
) -> InspectionEvidenceExportView {
    let generated_at_ms = provenance.and_then(|provenance| {
        provenance
            .finalized_at_ms
            .or(Some(provenance.created_at_ms))
    });
    InspectionEvidenceExportView {
        location: generated_at_ms.map(|_| format!("transist runs evidence export {run_id}")),
        command: format!("transist runs evidence export {run_id}"),
        format: "markdown".to_string(),
        generated_at_ms,
    }
}

fn infer_global_default_filesystem_policy(
    recipe: &Recipe,
) -> Option<InspectionFilesystemPolicySummary> {
    recipe
        .steps
        .iter()
        .find(|step| {
            matches!(
                step.effective_filesystem_policy_source,
                Some(transist_protocol::FilesystemPolicySource::Builtin)
                    | Some(transist_protocol::FilesystemPolicySource::Global)
            )
        })
        .map(|step| InspectionFilesystemPolicySummary {
            source: step
                .effective_filesystem_policy_source
                .map(|source| match source {
                    transist_protocol::FilesystemPolicySource::Builtin => "builtin".to_string(),
                    transist_protocol::FilesystemPolicySource::Global => "global".to_string(),
                    transist_protocol::FilesystemPolicySource::Workflow => "workflow".to_string(),
                    transist_protocol::FilesystemPolicySource::Step => "step".to_string(),
                }),
            workspace_root: None,
            workspace_access: step
                .effective_filesystem_policy
                .workspace_access
                .clone()
                .unwrap_or_default(),
            read_allow_paths: step.effective_filesystem_policy.read_allow_paths.clone(),
            write_allow_paths: step.effective_filesystem_policy.write_allow_paths.clone(),
        })
}

fn infer_workflow_filesystem_policy(recipe: &Recipe) -> Option<InspectionFilesystemPolicySummary> {
    recipe
        .filesystem_policy
        .as_ref()
        .map(|policy| InspectionFilesystemPolicySummary {
            source: Some("workflow".to_string()),
            workspace_root: None,
            workspace_access: policy.workspace_access.clone().unwrap_or_default(),
            read_allow_paths: policy.read_allow_paths.clone(),
            write_allow_paths: policy.write_allow_paths.clone(),
        })
}

impl InspectionQueryPort for LocalInspectionQuery {
    fn list_all_runs(
        &self,
        scope: RunListScope,
        status: Option<&str>,
    ) -> Result<Vec<InspectionRunSummary>> {
        let stored = self.summaries.list_run_summaries()?;
        let mut summaries = projected_run_summaries(&stored)
            .into_iter()
            .filter(|summary| matches!(scope, RunListScope::All) || summary.parent_run_id.is_none())
            .collect::<Vec<_>>();
        if let Some(status) = status {
            summaries.retain(|summary| summary.status == status);
        }
        summaries.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then(right.created_at_ms.cmp(&left.created_at_ms))
                .then(left.run_id.cmp(&right.run_id))
        });
        Ok(summaries)
    }

    fn list_runs(&self, query: &RunListQuery) -> Result<Page<InspectionRunSummary>> {
        let mut summaries = self.list_all_runs(query.scope, query.status.as_deref())?;
        summaries.retain(|summary| matches_run_filters(summary, query));

        let total = summaries.len();
        let offset = query.pagination.offset;
        let limit = normalize_limit(query.pagination.limit, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT);
        let data = summaries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        Ok(Page {
            data,
            total,
            limit,
            offset,
        })
    }

    fn get_run(&self, run_id: &str) -> Result<Option<InspectionRunDetail>> {
        let stored_summaries = self.summaries.list_run_summaries()?;
        let projected = projected_run_summaries(&stored_summaries);
        let projected_by_id = projected
            .iter()
            .cloned()
            .map(|summary| (summary.run_id.clone(), summary))
            .collect::<BTreeMap<_, _>>();
        self.get_run_with_projected(run_id, &stored_summaries, &projected, &projected_by_id)
    }

    fn get_runs(&self, run_ids: &[String]) -> Result<BTreeMap<String, InspectionRunDetail>> {
        let stored_summaries = self.summaries.list_run_summaries()?;
        let projected = projected_run_summaries(&stored_summaries);
        let projected_by_id = projected
            .iter()
            .cloned()
            .map(|summary| (summary.run_id.clone(), summary))
            .collect::<BTreeMap<_, _>>();
        let mut out = BTreeMap::new();
        for run_id in run_ids {
            if let Ok(Some(detail)) =
                self.get_run_with_projected(run_id, &stored_summaries, &projected, &projected_by_id)
            {
                out.insert(run_id.clone(), detail);
            }
        }
        Ok(out)
    }

    fn list_events(
        &self,
        run_id: &str,
        query: &EventListQuery,
    ) -> Result<Option<Page<InspectionEvent>>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }

        let limit = normalize_limit(query.limit, DEFAULT_EVENT_LIMIT, MAX_EVENT_LIMIT);
        let mut events = self.store.list_events(run_id)?;
        if let Some(before) = query.before {
            events.retain(|event| event.sequence < before);
        }
        if let Some(after) = query.after {
            events.retain(|event| event.sequence > after);
        }
        let total = events.len();
        if matches!(query.order, SortOrder::Desc) {
            events.reverse();
        }
        let data = events
            .into_iter()
            .take(limit)
            .map(|event| InspectionEvent {
                event_id: event.sequence.to_string(),
                timestamp_unix_ms: event.event.timestamp_unix_ms,
                event_type: event.event.kind,
                step_id: event.event.step_id,
                payload: event.event.payload,
            })
            .collect();
        Ok(Some(Page {
            data,
            total,
            limit,
            offset: 0,
        }))
    }

    fn list_steps(&self, run_id: &str) -> Result<Option<Vec<InspectionStep>>> {
        let record = match self.load_existing_run_record(run_id)? {
            Some(record) => record,
            None => return Ok(None),
        };
        let stored_summaries = self.summaries.list_run_summaries()?;
        let projected = projected_run_summaries(&stored_summaries);
        let projected_by_id = projected
            .into_iter()
            .map(|summary| (summary.run_id.clone(), summary))
            .collect::<BTreeMap<_, _>>();
        Ok(Some(build_steps(
            &self.store,
            &record,
            run_id,
            &stored_summaries,
            &projected_by_id,
        )?))
    }

    fn get_step(&self, run_id: &str, step_id: &str) -> Result<Option<InspectionStep>> {
        let record = match self.load_existing_run_record(run_id)? {
            Some(record) => record,
            None => return Ok(None),
        };
        let stored_summaries = self.summaries.list_run_summaries()?;
        let projected = projected_run_summaries(&stored_summaries);
        let projected_by_id = projected
            .into_iter()
            .map(|summary| (summary.run_id.clone(), summary))
            .collect::<BTreeMap<_, _>>();
        Ok(build_steps(
            &self.store,
            &record,
            run_id,
            &stored_summaries,
            &projected_by_id,
        )?
        .into_iter()
        .find(|step| step.step_id == step_id))
    }

    fn list_step_executions(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<Vec<InspectionStepExecution>>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }
        let records = self.store.list_step_executions(run_id, step_id)?;
        let executions = records
            .into_iter()
            .map(|record| InspectionStepExecution {
                execution: record.execution,
                status: record.status,
                started_at_ms: record.started_at_ms,
                finished_at_ms: record.finished_at_ms,
                request: record.request,
                result: record.result,
                state_mutations: record.state_mutations,
            })
            .collect();
        Ok(Some(executions))
    }

    fn get_workflow_file(
        &self,
        run_id: &str,
        step_id: &str,
        locator_kind: InspectionWorkflowFileLocatorKind,
        relative_path: &str,
    ) -> Result<Option<InspectionWorkflowFileRef>> {
        Ok(self.get_step(run_id, step_id)?.and_then(|step| {
            step.output_summary.workflow_files.into_iter().find(|file| {
                file.locator_kind == locator_kind && file.relative_path == relative_path
            })
        }))
    }

    fn read_workflow_file_content(
        &self,
        run_id: &str,
        step_id: &str,
        locator_kind: InspectionWorkflowFileLocatorKind,
        relative_path: &str,
        workspace_root: Option<&std::path::Path>,
    ) -> Result<Option<InspectionWorkflowFileContent>> {
        let Some(file) = self.get_workflow_file(run_id, step_id, locator_kind, relative_path)?
        else {
            return Ok(None);
        };
        let bytes = match locator_kind {
            InspectionWorkflowFileLocatorKind::Workspace => {
                let root = workspace_root
                    .ok_or_else(|| anyhow::anyhow!("workspace root is unavailable"))?;
                crate::secure_file::read_relative_to_root(root, &file.relative_path)?
            }
            InspectionWorkflowFileLocatorKind::RepositoryState => {
                let workspace_root = workspace_root
                    .ok_or_else(|| anyhow::anyhow!("workspace root is unavailable"))?;
                let latest_request = self
                    .store
                    .load_latest_step_execution(run_id, step_id)?
                    .and_then(|execution| execution.request);
                let request = match latest_request {
                    Some(request) => request,
                    None => self
                        .store
                        .load_run_record(run_id)?
                        .steps
                        .get(step_id)
                        .and_then(|step| step.last_request.clone())
                        .ok_or_else(|| anyhow::anyhow!("workflow file request is unavailable"))?,
                };
                let repo_path = request
                    .input
                    .get("repo_path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("repository root is unavailable"))?;
                let relative = repository_state_relative_path(repo_path, &file.relative_path)?;
                crate::secure_file::read_relative_to_root(workspace_root, &relative)?
            }
            InspectionWorkflowFileLocatorKind::Run => {
                let root_run_id =
                    transist_storage_local::run_paths::resolve_root_run_id(&self.home_dir, run_id)?;
                let run_relative_path =
                    transist_storage_local::run_paths::run_dir_name(&root_run_id);
                crate::secure_file::read_relative(
                    &self.home_dir,
                    &run_relative_path,
                    &file.relative_path,
                )?
            }
        };
        Ok(Some(InspectionWorkflowFileContent {
            path: file.relative_path,
            name: file.name,
            bytes,
        }))
    }

    fn list_gates(&self, run_id: &str) -> Result<Option<Vec<InspectionGate>>> {
        let record = match self.load_existing_run_record(run_id)? {
            Some(record) => record,
            None => return Ok(None),
        };
        Ok(Some(build_gates(&self.store, &record, run_id)?))
    }

    fn get_document(&self, run_id: &str, revision_id: &str) -> Result<Option<InspectionDocument>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }
        crate::documents::get_document(&self.home_dir, run_id, revision_id)
    }

    fn read_document_content(
        &self,
        run_id: &str,
        revision_id: &str,
        member_id: Option<&str>,
    ) -> Result<Option<InspectionBlobContent>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }
        crate::documents::read_document(&self.home_dir, run_id, revision_id, member_id)
    }

    fn list_failures(&self, run_id: &str) -> Result<Option<Vec<FailureRecord>>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }
        let mut failures = self.store.list_failures(run_id)?;
        failures.sort_by(|left, right| {
            right
                .last_seen_at_unix_ms
                .cmp(&left.last_seen_at_unix_ms)
                .then(right.execution_attempt.cmp(&left.execution_attempt))
                .then(left.step_id.cmp(&right.step_id))
                .then(left.failure_id.cmp(&right.failure_id))
        });
        Ok(Some(failures))
    }

    fn get_failure(&self, run_id: &str, failure_id: &str) -> Result<Option<FailureRecord>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }
        self.store.get_failure(run_id, failure_id)
    }

    fn list_blobs(
        &self,
        run_id: &str,
        family: BlobFamily,
        scope: BlobListScope,
    ) -> Result<Option<Vec<InspectionBlob>>> {
        let record = match self.load_existing_run_record(run_id)? {
            Some(record) => record,
            None => return Ok(None),
        };

        let owner_run_ids = match scope {
            BlobListScope::SelfOnly => vec![run_id.to_string()],
            BlobListScope::Subtree => {
                let stored_summaries = self.summaries.list_run_summaries()?;
                let projected = projected_run_summaries(&stored_summaries);
                let summary = projected
                    .iter()
                    .find(|summary| summary.run_id == run_id)
                    .cloned()
                    .unwrap_or(build_run_summary(&self.store, &record)?);
                let mut run_ids = vec![run_id.to_string()];
                run_ids.extend(
                    list_contained_runs_from_summaries(&projected, &summary.root_run_id, run_id)
                        .into_iter()
                        .map(|summary| summary.run_id),
                );
                run_ids
            }
        };

        let mut blobs = Vec::new();
        for owner_run_id in owner_run_ids {
            blobs.extend(
                self.store
                    .list_blobs(&owner_run_id, family)?
                    .into_iter()
                    .map(|blob| InspectionBlob {
                        run_id: owner_run_id.clone(),
                        family: blob.family,
                        step_id: blob.step_id,
                        execution: blob.execution,
                        name: Path::new(&blob.relative_path)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or(&blob.relative_path)
                            .to_string(),
                        relative_path: blob.relative_path,
                        bytes: blob.bytes,
                        created_at_ms: blob.created_at_ms,
                        descriptor: blob.descriptor.map(|descriptor| {
                            super::models::InspectionBlobDescriptor {
                                source_path: descriptor.source_path,
                                sha256: descriptor.sha256,
                                bytes: descriptor.bytes,
                                invoke_id: descriptor.invoke_id,
                                status: descriptor.status,
                            }
                        }),
                    }),
            );
        }
        blobs.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then(left.run_id.cmp(&right.run_id))
                .then(left.step_id.cmp(&right.step_id))
                .then(left.execution.cmp(&right.execution))
                .then(left.relative_path.cmp(&right.relative_path))
        });
        Ok(Some(blobs))
    }

    fn get_blob(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlob>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }
        let blobs = match self.list_blobs(run_id, family, BlobListScope::SelfOnly)? {
            Some(blobs) => blobs,
            None => return Ok(None),
        };
        Ok(blobs.into_iter().find(|blob| {
            blob.step_id == step_id
                && blob.execution == execution
                && blob.relative_path == relative_path
        }))
    }

    fn read_blob_content(
        &self,
        run_id: &str,
        family: BlobFamily,
        step_id: &str,
        execution: u32,
        relative_path: &str,
    ) -> Result<Option<InspectionBlobContent>> {
        if self.load_existing_run_record(run_id)?.is_none() {
            return Ok(None);
        }
        let blob = match self.get_blob(run_id, family, step_id, execution, relative_path)? {
            Some(blob) => blob,
            None => return Ok(None),
        };
        let bytes =
            self.store
                .read_blob_content(run_id, family, step_id, execution, relative_path)?;
        Ok(Some(InspectionBlobContent {
            relative_path: blob.relative_path,
            name: blob.name,
            bytes,
        }))
    }

    fn list_workflows(&self, query: &PaginationQuery) -> Result<Page<InspectionWorkflowSummary>> {
        let workflows = discover_workflows(&self.store)?;
        let total = workflows.len();
        let offset = query.offset;
        let limit = normalize_limit(query.limit, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT);
        Ok(Page {
            data: workflows
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(workflow_summary_from_discovery)
                .collect(),
            total,
            limit,
            offset,
        })
    }

    fn get_workflow(&self, workflow_ref: &str) -> Result<Option<InspectionWorkflowDetail>> {
        let workflows = discover_workflows(&self.store)?;
        Ok(workflows
            .into_iter()
            .find(|workflow| workflow.workflow_ref() == workflow_ref)
            .map(|workflow| workflow_detail_from_discovery(&self.home_dir, workflow)))
    }

    fn read_workflow_source(
        &self,
        workflow_ref: &str,
    ) -> Result<Option<InspectionWorkflowContent>> {
        let workflows = discover_workflows(&self.store)?;
        let Some(found) = workflows
            .into_iter()
            .find(|workflow| workflow.workflow_ref() == workflow_ref)
        else {
            return Ok(None);
        };
        let relative = match found.source_path {
            Some(p) if p.is_absolute() => p
                .strip_prefix(&self.repo_root)
                .map_err(|_| {
                    anyhow::anyhow!("workflow source path is outside the repository root")
                })?
                .to_path_buf(),
            Some(p) => p,
            None => match find_workflow_source_in_repo(&self.repo_root, &found.workflow_id) {
                Some(p) => p
                    .strip_prefix(&self.repo_root)
                    .expect("repository scan returns descendants")
                    .to_path_buf(),
                None => return Ok(None),
            },
        };
        Ok(Some(InspectionWorkflowContent {
            path: relative.display().to_string(),
            bytes: crate::secure_file::read_relative_to_root(
                &self.repo_root,
                &relative.to_string_lossy(),
            )?,
        }))
    }

    fn read_workflow_compiled_recipe(
        &self,
        workflow_ref: &str,
    ) -> Result<Option<InspectionWorkflowContent>> {
        let Some((workflow_id, workflow_version)) = workflow_ref_parts(workflow_ref) else {
            return Ok(None);
        };
        let path = compiled_recipe_path(&self.home_dir, &workflow_id, &workflow_version);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(read_workflow_content(&path)?))
    }
}

fn local_runtime_owner_identity() -> Result<RuntimeOwnerIdentity> {
    let pid = std::process::id();
    let owner_id = format!("inspection-{pid}");
    Ok(RuntimeOwnerIdentity {
        management_mode: ManagementMode::Standalone,
        owner_id: owner_id.clone(),
        lease_id: owner_id,
        pid,
        hostname: std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
        command: "transist-inspection".to_string(),
        transist_version: env!("CARGO_PKG_VERSION").to_string(),
        external: None,
        runtime_id: None,
        organization_id: None,
    })
}

fn preflight_summary_from_record(
    preflight: &transist_core::PersistedPreflightRecord,
) -> InspectionPreflightSummary {
    InspectionPreflightSummary {
        status: preflight.status.as_str().to_string(),
        checked_at_ms: preflight.checked_at_ms,
        blocker_count: preflight.blocker_count,
        warning_count: preflight.warning_count,
        failed_checks: preflight
            .checks
            .iter()
            .filter(|check| check.status == transist_core::PreflightStatus::Failed)
            .map(|check| InspectionPreflightFailedCheck {
                kind: check.kind.clone(),
                message: check.message.clone(),
                step_ids: check.step_ids.clone(),
                plugin_id: check.plugin_id.clone(),
                action: check.action.clone(),
            })
            .collect(),
    }
}

fn cleanup_summary_from_record(
    cleanup: &transist_core::PersistedCleanupRecord,
) -> InspectionCleanupSummary {
    InspectionCleanupSummary {
        trigger: match cleanup.trigger {
            transist_protocol::CleanupTrigger::Success => "success",
            transist_protocol::CleanupTrigger::Canceled => "canceled",
            transist_protocol::CleanupTrigger::Failed => "failed",
        }
        .to_string(),
        state: cleanup.state.as_str().to_string(),
        started_at_ms: cleanup.started_at_ms,
        finished_at_ms: cleanup.finished_at_ms,
        preflight: cleanup
            .preflight
            .as_ref()
            .map(preflight_summary_from_record),
        steps: cleanup
            .steps
            .values()
            .map(|step| InspectionCleanupStep {
                step_id: step.step_id.clone(),
                status: crate::steps::step_status_name(step.status).to_string(),
                execution: (step.executions > 0).then_some(step.executions),
                started_at_ms: step.started_at_ms,
                finished_at_ms: step.finished_at_ms,
                error_message: cleanup_step_error_message(step),
            })
            .collect(),
    }
}

fn cleanup_step_error_message(step: &transist_core::KernelStepState) -> Option<String> {
    let result = step.last_result.as_ref()?;
    if let Some(error) = result.error.as_ref() {
        let message = error.message.trim();
        if !message.is_empty() {
            return Some(message.to_string());
        }
    }
    let stderr = result.stderr.trim();
    if result.status != "success" && !stderr.is_empty() {
        return Some(stderr.to_string());
    }
    None
}

/// Locates the authored workflow source file for `workflow_id` under `repo_root`.
///
/// The compilation recipe does not record the original source path, so when
/// discovery yields none we fall back to a bounded recursive scan for the
/// conventional `<workflow_id>.flow.yaml` / `<workflow_id>.flow.yml` file. The
/// shallowest match wins, so a top-level workflow shadows a same-named
/// subflow file. Returns `None` when no matching authored file exists.
fn find_workflow_source_in_repo(repo_root: &Path, workflow_id: &str) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    let mut stack = vec![repo_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if skip_source_scan_dir(&path) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let base = name
                .strip_suffix(".flow.yaml")
                .or_else(|| name.strip_suffix(".flow.yml"));
            if base != Some(workflow_id) {
                continue;
            }
            let depth = path.components().count();
            let best_depth = best
                .as_ref()
                .map(|b| b.components().count())
                .unwrap_or(usize::MAX);
            if depth < best_depth {
                best = Some(path);
            }
        }
    }
    best
}

fn skip_source_scan_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some(name)
            if name == "target"
                || name == ".git"
                || name.starts_with('.')
    )
}

fn repository_state_relative_path(repo_path: &str, file_path: &str) -> Result<String> {
    let repo_path = Path::new(repo_path.trim());
    if repo_path.as_os_str().is_empty() || repo_path.is_absolute() {
        anyhow::bail!("repository path must be a non-empty workspace-relative path");
    }
    let mut relative = PathBuf::new();
    for component in repo_path.components() {
        let Component::Normal(name) = component else {
            anyhow::bail!("repository path contains a non-normal component");
        };
        relative.push(name);
    }
    relative.push(".transist");
    for component in Path::new(file_path).components() {
        let Component::Normal(name) = component else {
            anyhow::bail!("repository state file path contains a non-normal component");
        };
        relative.push(name);
    }
    Ok(relative.to_string_lossy().to_string())
}
