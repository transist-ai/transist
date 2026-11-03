// SPDX-License-Identifier: MPL-2.0
//! Helpers for persisted run provenance written beside `state.json`.

use super::*;
use crate::types::{
    ExternalTransferRef, RUN_PROVENANCE_SCHEMA_V1, RunApprovalProvenance,
    RunExternalTransferSummary, RunProvenanceDenied, RunProvenancePolicyDecision,
    RunProvenanceRecord, RunProvenanceSubject, RunProviderProvenance,
};

/// Serializes the initial `transist.run_provenance.v1` record for a run
/// at run birth. Used so the kernel can pass the bytes to the staged run
/// creation port.
pub(crate) fn serialize_birth_provenance_record(
    home_dir: &Path,
    record: &PersistedRunRecord,
    policy_decision: RunProvenancePolicyDecision,
    created_at_ms: u64,
) -> Result<Vec<u8>> {
    let payload =
        build_run_provenance_record(None, home_dir, record, policy_decision, created_at_ms, None)?;
    serde_json::to_vec_pretty(&payload).context("failed serializing run provenance")
}

pub(crate) fn finalize_run_provenance_record(
    run_store: &dyn RunStorePort,
    home_dir: &Path,
    record: &PersistedRunRecord,
    finalized_at_ms: u64,
) -> Result<()> {
    let Some(existing) = read_and_validate_provenance(run_store, home_dir, &record.run_id)? else {
        return Ok(());
    };
    let payload = build_run_provenance_record(
        Some(run_store),
        home_dir,
        record,
        existing.policy_decision,
        existing.created_at_ms,
        Some(finalized_at_ms),
    )?;
    write_run_provenance_record(run_store, home_dir, &record.run_id, &payload)
}

/// Records a step-dispatch policy or filesystem-policy denial in the run's
/// provenance record before the kernel bails. If no provenance record exists
/// yet (e.g. the run was created without one), this is a no-op.
pub(crate) fn record_step_dispatch_denied_provenance(
    run_store: &dyn RunStorePort,
    home_dir: &Path,
    record: &PersistedRunRecord,
    step_id: &str,
    reason_code: &str,
    failed_verification_type: &str,
    now_ms: u64,
) -> Result<()> {
    let Some(existing) = read_and_validate_provenance(run_store, home_dir, &record.run_id)? else {
        return Ok(());
    };
    let plugin_id = record
        .steps
        .get(step_id)
        .and_then(|step| step.last_request.as_ref())
        .and_then(|request| request.metadata.get("plugin_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let denied = RunProvenanceDenied {
        reason: reason_code.to_string(),
        offending_kind: "policy".to_string(),
        offending_id: if !plugin_id.is_empty() {
            plugin_id.to_string()
        } else {
            record.workflow_id.clone()
        },
        failed_verification_type: Some(failed_verification_type.to_string()),
        at_ms: now_ms,
    };

    let mut payload = build_run_provenance_record(
        Some(run_store),
        home_dir,
        record,
        existing.policy_decision,
        existing.created_at_ms,
        None,
    )?;
    payload.decision = "denied".to_string();
    payload.denied = Some(denied);
    write_run_provenance_record(run_store, home_dir, &record.run_id, &payload)
}

fn read_and_validate_provenance(
    run_store: &dyn RunStorePort,
    home_dir: &Path,
    run_id: &str,
) -> Result<Option<RunProvenanceRecord>> {
    let path = run_store.run_provenance_path(home_dir, run_id)?;
    let Some(raw) = run_store.read_run_provenance(home_dir, run_id)? else {
        return Ok(None);
    };
    let record: RunProvenanceRecord = serde_json::from_slice(&raw)
        .with_context(|| format!("invalid run provenance json: {}", path.display()))?;
    if record.schema != RUN_PROVENANCE_SCHEMA_V1 {
        bail!(
            "run provenance '{}' uses unsupported schema '{}'; expected '{}'",
            path.display(),
            record.schema,
            RUN_PROVENANCE_SCHEMA_V1
        );
    }
    Ok(Some(record))
}

fn write_run_provenance_record(
    run_store: &dyn RunStorePort,
    home_dir: &Path,
    run_id: &str,
    payload: &RunProvenanceRecord,
) -> Result<()> {
    let body = serde_json::to_vec_pretty(payload).context("failed serializing run provenance")?;
    run_store.write_run_provenance(home_dir, run_id, &body)
}

#[cfg(test)]
fn read_and_validate_provenance_from_path(path: &Path) -> Result<RunProvenanceRecord> {
    let raw = std::fs::read(path)
        .with_context(|| format!("failed reading run provenance: {}", path.display()))?;
    let record: RunProvenanceRecord = serde_json::from_slice(&raw)
        .with_context(|| format!("invalid run provenance json: {}", path.display()))?;
    if record.schema != RUN_PROVENANCE_SCHEMA_V1 {
        bail!(
            "run provenance '{}' uses unsupported schema '{}'; expected '{}'",
            path.display(),
            record.schema,
            RUN_PROVENANCE_SCHEMA_V1
        );
    }
    Ok(record)
}

fn build_run_provenance_record(
    run_store: Option<&dyn RunStorePort>,
    home_dir: &Path,
    record: &PersistedRunRecord,
    policy_decision: RunProvenancePolicyDecision,
    created_at_ms: u64,
    finalized_at_ms: Option<u64>,
) -> Result<RunProvenanceRecord> {
    let trust_decision = record.trust_decision.as_ref();
    let workflow = trust_decision.map(|decision| &decision.workflow);
    let plugins = trust_decision
        .map(|decision| {
            decision
                .plugins
                .iter()
                .map(|plugin| RunProvenanceSubject {
                    id: plugin.id.clone(),
                    version: plugin.version.clone(),
                    digest: plugin.digest.clone(),
                    signature_verified: plugin.signature_verified,
                    signer_key_id: plugin.signer_key_id.clone(),
                    trust_decision: plugin.trust_decision.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let providers = collect_provider_provenance(home_dir, record);
    let approvals = collect_approval_provenance(run_store, home_dir, record);
    let denied = trust_decision
        .filter(|decision| decision.denied())
        .map(build_denied_provenance);

    Ok(RunProvenanceRecord {
        schema: crate::types::RUN_PROVENANCE_SCHEMA_V1.to_string(),
        run_id: record.run_id.clone(),
        workflow_id: record.workflow_id.clone(),
        workflow_version: record.workflow_version.clone(),
        workflow_digest: workflow.map(|workflow| workflow.digest.clone()),
        workflow_signature_verified: workflow.is_some_and(|workflow| workflow.signature_verified),
        workflow_signer_key_id: workflow.and_then(|workflow| workflow.signer_key_id.clone()),
        workflow_trust_decision: workflow
            .map(|workflow| workflow.trust_decision.clone())
            .unwrap_or_else(|| "not_verified".to_string()),
        plugins,
        trusted_execution_mode: record.trusted_execution_mode,
        local_only_mode: record.local_only_mode,
        runtime_mode: record.runtime_mode.clone(),
        policy_profile: record.policy_profile.clone(),
        capability_enforcement: record.capability_enforcement.clone(),
        policy_decision,
        providers,
        external_transfer_summary: collect_external_transfer_summary(run_store, home_dir, record)?,
        approval_summary: approvals,
        decision: if denied.is_some() {
            "denied".to_string()
        } else {
            "allowed".to_string()
        },
        denied,
        organization_id: None,
        policy_revision: None,
        created_at_ms,
        finalized_at_ms,
    })
}

fn collect_provider_provenance(
    home_dir: &Path,
    record: &PersistedRunRecord,
) -> Vec<RunProviderProvenance> {
    let mut providers = Vec::new();
    for (step_id, step) in &record.steps {
        let Some(result) = step.last_result.as_ref() else {
            continue;
        };
        let Some(usage) = result.usage.as_ref() else {
            continue;
        };
        if usage.provider.is_none() && usage.model.is_none() {
            continue;
        }
        let provider_kind = step.last_request.as_ref().map(|request| {
            provider_kind_for_request(home_dir, request)
                .as_str()
                .to_string()
        });
        providers.push(RunProviderProvenance {
            step_id: step_id.clone(),
            provider: usage.provider.clone(),
            model: usage.model.clone(),
            provider_kind,
        });
    }
    providers
}

/// Reads the run-local external-transfer evidence ledger from files actually
/// written beneath `artifacts/**/evidence/transist_external_transfer_*.json`.
/// Default-mode runs record zero because transfer recording is only active in
/// trusted-execution or local-only modes.
fn collect_external_transfer_summary(
    run_store: Option<&dyn RunStorePort>,
    home_dir: &Path,
    record: &PersistedRunRecord,
) -> Result<RunExternalTransferSummary> {
    let external_transfers = collect_external_transfer_refs(run_store, home_dir, &record.run_id)?;
    Ok(RunExternalTransferSummary {
        count: external_transfers.len() as u64,
        external_transfers,
    })
}

fn collect_external_transfer_refs(
    run_store: Option<&dyn RunStorePort>,
    home_dir: &Path,
    run_id: &str,
) -> Result<Vec<ExternalTransferRef>> {
    let Some(run_store) = run_store else {
        return Ok(Vec::new());
    };
    let mut transfers = run_store
        .load_external_transfer_evidence(home_dir, run_id)?
        .into_iter()
        .map(|evidence| {
            let record = evidence.record;
            ExternalTransferRef {
                step_id: record.step_id,
                evidence_path: evidence.evidence_path.to_string_lossy().replace('\\', "/"),
                destination: record.destination,
                adapter_kind: record.adapter_kind.as_str().to_string(),
                provider: record.provider,
                provider_kind: record.provider_kind.as_str().to_string(),
                content_hash: record.content_hash,
                timestamp_ms: record.timestamp_ms,
            }
        })
        .collect::<Vec<_>>();
    transfers.sort_by(|a, b| {
        a.timestamp_ms
            .cmp(&b.timestamp_ms)
            .then_with(|| a.step_id.cmp(&b.step_id))
            .then_with(|| a.evidence_path.cmp(&b.evidence_path))
    });
    Ok(transfers)
}

fn provider_kind_for_request(
    _home_dir: &Path,
    request: &ExecutionRequest,
) -> transist_protocol::ProviderKind {
    request
        .metadata
        .get("provider_kind")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<transist_protocol::ProviderKind>().ok())
        .unwrap_or(transist_protocol::ProviderKind::External)
}

fn collect_approval_provenance(
    run_store: Option<&dyn RunStorePort>,
    home_dir: &Path,
    record: &PersistedRunRecord,
) -> Vec<RunApprovalProvenance> {
    let mut approvals = Vec::new();
    for step in record.steps.values() {
        let Some(result) = step.last_result.as_ref() else {
            continue;
        };
        let Some(gate_id) = result
            .output
            .get("gate_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(decision) = result
            .output
            .get("decision")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let reason = result
            .output
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let actor = result
            .output
            .get("actor")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let decided_at_ms = result
            .output
            .get("decided_at_ms")
            .and_then(serde_json::Value::as_u64)
            .or(step.finished_at_ms)
            .or(step.started_at_ms);
        let persisted = run_store.and_then(|store| {
            load_gate_decision_record(store, home_dir, &record.run_id, gate_id)
                .ok()
                .flatten()
        });
        approvals.push(RunApprovalProvenance {
            gate_id: gate_id.to_string(),
            decision: decision.to_string(),
            actor: persisted
                .as_ref()
                .and_then(|decision| decision.actor.clone())
                .or(actor),
            reason: persisted
                .as_ref()
                .and_then(|decision| decision.reason.clone())
                .or(reason),
            decided_at_ms: persisted
                .as_ref()
                .and_then(|decision| decision.decided_at_ms)
                .or(decided_at_ms),
        });
    }
    approvals
}

fn build_denied_provenance(decision: &crate::types::TrustDecisionRecord) -> RunProvenanceDenied {
    if decision.workflow.trust_decision == "denied" {
        return RunProvenanceDenied {
            reason: decision
                .reason_code
                .clone()
                .unwrap_or_else(|| "trusted_execution_denied".to_string()),
            offending_kind: "workflow".to_string(),
            offending_id: decision.workflow.id.clone(),
            failed_verification_type: decision.failed_verification_type.clone(),
            at_ms: decision.decided_at_ms,
        };
    }

    let offending_plugin = decision
        .plugins
        .iter()
        .find(|plugin| plugin.trust_decision == "denied");
    let offending_id = offending_plugin
        .map(|plugin| plugin.id.clone())
        .unwrap_or_else(|| decision.workflow.id.clone());
    RunProvenanceDenied {
        reason: decision
            .reason_code
            .clone()
            .unwrap_or_else(|| "trusted_execution_denied".to_string()),
        offending_kind: "plugin".to_string(),
        offending_id,
        failed_verification_type: decision.failed_verification_type.clone(),
        at_ms: decision.decided_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{KernelRunSnapshot, PersistedRunRecord, RecipeSnapshotRef};

    fn minimal_record() -> PersistedRunRecord {
        let snapshot = KernelRunSnapshot::new("run-test", "demo", "v1", &["step".to_string()]);
        PersistedRunRecord::from_snapshot(
            snapshot,
            Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: "abc".to_string(),
            }),
            None,
            BTreeMap::new(),
            0,
            1,
            1,
        )
    }

    #[test]
    fn denied_provenance_record_round_trips_with_policy_offending_kind() {
        let record = minimal_record();
        let policy = RunProvenancePolicyDecision {
            allowed: false,
            reason_code: Some("policy_denied".to_string()),
            message: None,
            mode: "step_dispatch".to_string(),
        };
        let provenance = build_run_provenance_record(
            None,
            std::path::Path::new("/tmp"),
            &record,
            policy,
            42,
            None,
        )
        .expect("build provenance");
        assert_eq!(provenance.schema, RUN_PROVENANCE_SCHEMA_V1);
        assert_eq!(provenance.decision, "allowed");
        assert!(provenance.denied.is_none());

        let mut denied = provenance;
        denied.decision = "denied".to_string();
        denied.denied = Some(RunProvenanceDenied {
            reason: "policy_denied".to_string(),
            offending_kind: "policy".to_string(),
            offending_id: "demo".to_string(),
            failed_verification_type: Some("policy_denied".to_string()),
            at_ms: 99,
        });
        let json = serde_json::to_string(&denied).expect("serialize");
        let parsed: RunProvenanceRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, denied);
        assert_eq!(parsed.schema, RUN_PROVENANCE_SCHEMA_V1);
        assert_eq!(parsed.organization_id, None);
        assert_eq!(parsed.policy_revision, None);
        assert_eq!(parsed.denied.as_ref().unwrap().offending_kind, "policy");
    }

    #[test]
    fn read_and_validate_provenance_rejects_unknown_schema() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("provenance.json");
        let payload = serde_json::json!({
            "schema": "transist.run_provenance.v99",
            "run_id": "run-test",
            "workflow_id": "demo",
            "workflow_version": "v1",
            "workflow_signature_verified": false,
            "workflow_trust_decision": "not_verified",
            "trusted_execution_mode": false,
            "policy_decision": {
                "allowed": true,
                "mode": "run_start"
            },
            "external_transfer_summary": { "count": 0 },
            "decision": "allowed",
            "created_at_ms": 1u64
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&payload).unwrap())
            .expect("write provenance");
        let error = read_and_validate_provenance_from_path(&path);
        assert!(error.is_err());
        let msg = error.unwrap_err().to_string();
        assert!(msg.contains("unsupported schema"), "actual error: {msg}");
    }
}
