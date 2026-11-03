// SPDX-License-Identifier: MPL-2.0
//! Round-trip test: write persisted run state and provenance through the same
//! serde types the kernel uses, then load them via `LocalInspectionQuery` and
//! verify the inspection surfaces match. Guards against silent schema drift
//! between what the kernel writes and what the inspection layer reads.

use std::collections::BTreeMap;
use std::fs;

use transist_core::RunStorePort;
use transist_core::{
    ExternalTransferRef, KernelRunSnapshot, KernelRunStatus, KernelStepState, KernelStepStatus,
    PersistedRunRecord, RunExternalTransferSummary, RunLineage, RunLineageMode,
    RunProvenancePolicyDecision, RunProvenanceRecord, RunProviderProvenance, TrustDecisionRecord,
    TrustSubjectDecision,
};
use transist_inspection::{InspectionQueryPort, LocalInspectionQuery};
use transist_protocol::{
    EXTERNAL_TRANSFER_SCHEMA_V1, ExternalTransferPolicyDecision, ExternalTransferRecord,
    ProviderKind,
};
use transist_storage_local::run_paths::run_provenance_path;
use transist_storage_local::{LocalRunStore, LocalRunSummaryStore, LocalStorageConfig};

const TS_CREATED: u64 = 1_735_689_600_000;
const TS_UPDATED: u64 = 1_735_689_604_000;

fn write_run_record(home_dir: &std::path::Path, record: &PersistedRunRecord) {
    let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
    store.create_run(record).expect("create run");
    let summaries = LocalRunSummaryStore::new(home_dir);
    summaries
        .refresh_run(&record.run_id)
        .expect("refresh summary");
}

fn write_provenance(home_dir: &std::path::Path, provenance: &RunProvenanceRecord) {
    let path = run_provenance_path(home_dir, &provenance.run_id).expect("provenance path");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("provenance dir");
    }
    let body = serde_json::to_vec_pretty(provenance).expect("serialize provenance");
    fs::write(path, body).expect("write provenance");
}

fn write_external_transfer_evidence(
    home_dir: &std::path::Path,
    run_id: &str,
    transfer: &ExternalTransferRecord,
) {
    let run_root = transist_storage_local::run_paths::resolve_run_root(home_dir, run_id)
        .expect("resolve run root");
    let evidence_dir = run_root
        .join("artifacts")
        .join(&transfer.step_id)
        .join(format!("execution_{:03}", transfer.execution))
        .join("evidence");
    fs::create_dir_all(&evidence_dir).expect("evidence dir");

    let file_name = format!("transist_external_transfer_{}.json", transfer.execution);
    let evidence_path = evidence_dir.join(&file_name);
    let body = serde_json::to_vec_pretty(transfer).expect("serialize external transfer");
    fs::write(&evidence_path, &body).expect("write external transfer evidence");

    let descriptor_path = evidence_dir.join(format!("000_{file_name}.json"));
    fs::write(
        &descriptor_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "kind": "evidence",
            "source_path": evidence_path.display().to_string(),
            "sha256": "external-transfer-sha",
            "bytes": body.len(),
            "status": "success",
            "invoke_id": null,
            "execution": transfer.execution
        }))
        .expect("descriptor json"),
    )
    .expect("write descriptor");
}

fn sample_record(
    run_id: &str,
    workflow_id: &str,
    workflow_version: &str,
    trust_decision: Option<TrustDecisionRecord>,
    trusted_execution: bool,
) -> PersistedRunRecord {
    let mut snapshot =
        KernelRunSnapshot::new(run_id, workflow_id, workflow_version, &["step".to_string()]);
    snapshot.status = KernelRunStatus::Success;
    snapshot.steps.insert(
        "step".to_string(),
        KernelStepState {
            step_id: "step".to_string(),
            status: KernelStepStatus::Succeeded,
            executions: 1,
            retry_wait_until_ms: None,
            skip_reason: None,
            last_request: None,
            last_result: None,
            started_at_ms: Some(TS_CREATED),
            finished_at_ms: Some(TS_UPDATED),
            activation_executions_baseline: 0,
        },
    );
    let mut record = PersistedRunRecord::from_snapshot(
        snapshot,
        None,
        Some(RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: run_id.to_string(),
            parent_run_id: None,
            parent_step_id: None,
        }),
        BTreeMap::new(),
        0,
        TS_CREATED,
        TS_UPDATED,
    );
    record.trusted_execution_mode = trusted_execution;
    record.trust_decision = trust_decision;
    record
}

fn verified_trust_decision(run_id: &str) -> TrustDecisionRecord {
    TrustDecisionRecord {
        schema: "transist.trust_decision.v1".to_string(),
        run_id: run_id.to_string(),
        trusted_execution_mode: true,
        decision: "allowed".to_string(),
        workflow: TrustSubjectDecision {
            id: "demo_wf".to_string(),
            version: "v1".to_string(),
            digest: "verified-digest".to_string(),
            signature_verified: true,
            signer_key_id: Some("key_verified".to_string()),
            reason_code: None,
            trust_decision: "verified".to_string(),
        },
        plugins: vec![TrustSubjectDecision {
            id: "demo_plugin".to_string(),
            version: "1.0.0".to_string(),
            digest: "plugin-verified-digest".to_string(),
            signature_verified: true,
            signer_key_id: Some("pkey_verified".to_string()),
            reason_code: None,
            trust_decision: "verified".to_string(),
        }],
        reason_code: None,
        failed_verification_type: None,
        organization_id: None,
        policy_revision: None,
        decided_at_ms: TS_CREATED,
    }
}

fn denied_trust_decision(run_id: &str) -> TrustDecisionRecord {
    TrustDecisionRecord {
        schema: "transist.trust_decision.v1".to_string(),
        run_id: run_id.to_string(),
        trusted_execution_mode: true,
        decision: "denied".to_string(),
        workflow: TrustSubjectDecision {
            id: "demo_wf".to_string(),
            version: "v1".to_string(),
            digest: "tampered-digest".to_string(),
            signature_verified: false,
            signer_key_id: Some("key_denied".to_string()),
            reason_code: Some("workflow_digest_mismatch".to_string()),
            trust_decision: "denied".to_string(),
        },
        plugins: Vec::new(),
        reason_code: Some("workflow_digest_mismatch".to_string()),
        failed_verification_type: Some("workflow_signature".to_string()),
        organization_id: None,
        policy_revision: None,
        decided_at_ms: TS_CREATED,
    }
}

fn bypassed_trust_decision(run_id: &str) -> TrustDecisionRecord {
    TrustDecisionRecord {
        schema: "transist.trust_decision.v1".to_string(),
        run_id: run_id.to_string(),
        trusted_execution_mode: true,
        decision: "bypassed".to_string(),
        workflow: TrustSubjectDecision {
            id: "demo_wf".to_string(),
            version: "v0".to_string(),
            digest: "v0-digest".to_string(),
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("test_version_bypass".to_string()),
            trust_decision: "bypassed".to_string(),
        },
        plugins: vec![TrustSubjectDecision {
            id: "demo_plugin".to_string(),
            version: "0.1.0".to_string(),
            digest: "v0-plugin-digest".to_string(),
            signature_verified: false,
            signer_key_id: None,
            reason_code: Some("test_version_bypass".to_string()),
            trust_decision: "bypassed".to_string(),
        }],
        reason_code: Some("test_version_bypass".to_string()),
        failed_verification_type: None,
        organization_id: None,
        policy_revision: None,
        decided_at_ms: TS_CREATED,
    }
}

fn sample_provenance(
    run_id: &str,
    workflow_trust_decision: &str,
    trusted_execution: bool,
    external_transfer_count: u64,
) -> RunProvenanceRecord {
    RunProvenanceRecord {
        schema: transist_core::RUN_PROVENANCE_SCHEMA_V1.to_string(),
        run_id: run_id.to_string(),
        workflow_id: "demo_wf".to_string(),
        workflow_version: "v1".to_string(),
        workflow_digest: Some("verified-digest".to_string()),
        workflow_signature_verified: workflow_trust_decision == "verified",
        workflow_signer_key_id: Some("key_verified".to_string()),
        workflow_trust_decision: workflow_trust_decision.to_string(),
        plugins: Vec::new(),
        trusted_execution_mode: trusted_execution,
        local_only_mode: false,
        runtime_mode: "governed".to_string(),
        policy_profile: "allowlist".to_string(),
        capability_enforcement: "enforced".to_string(),
        policy_decision: RunProvenancePolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            mode: "run_start".to_string(),
        },
        providers: vec![RunProviderProvenance {
            step_id: "step".to_string(),
            provider: Some("openai".to_string()),
            model: Some("gpt-test".to_string()),
            provider_kind: Some("external".to_string()),
        }],
        external_transfer_summary: RunExternalTransferSummary {
            count: external_transfer_count,
            external_transfers: if external_transfer_count == 0 {
                Vec::new()
            } else {
                vec![ExternalTransferRef {
                    step_id: "step".to_string(),
                    evidence_path:
                        "artifacts/step/execution_001/evidence/transist_external_transfer_1.json"
                            .to_string(),
                    destination: Some("api.openai.com".to_string()),
                    adapter_kind: "process".to_string(),
                    provider: Some("openai".to_string()),
                    provider_kind: "external".to_string(),
                    content_hash: Some("payload-hash".to_string()),
                    timestamp_ms: TS_UPDATED,
                }]
            },
        },
        approval_summary: Vec::new(),
        decision: "allowed".to_string(),
        denied: None,
        organization_id: None,
        policy_revision: None,
        created_at_ms: TS_CREATED,
        finalized_at_ms: Some(TS_UPDATED),
    }
}

fn sample_external_transfer(run_id: &str) -> ExternalTransferRecord {
    ExternalTransferRecord {
        schema: EXTERNAL_TRANSFER_SCHEMA_V1.to_string(),
        run_id: run_id.to_string(),
        step_id: "step".to_string(),
        execution: 1,
        timestamp_ms: TS_UPDATED,
        destination: Some("api.openai.com".to_string()),
        adapter_kind: transist_protocol::AdapterKind::Process,
        provider: Some("openai".to_string()),
        provider_kind: ProviderKind::External,
        bytes_sent: None,
        content_hash: Some("payload-hash".to_string()),
        redaction_applied: false,
        policy_decision: ExternalTransferPolicyDecision {
            allowed: true,
            mode: "trusted_execution".to_string(),
        },
    }
}

#[test]
fn verified_trust_round_trips_through_inspection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path();
    fs::create_dir_all(home_dir.join("runs")).expect("runs dir");

    let run_id = "round-trip-verified";
    let record = sample_record(
        run_id,
        "demo_wf",
        "v1",
        Some(verified_trust_decision(run_id)),
        true,
    );
    write_run_record(home_dir, &record);
    write_provenance(home_dir, &sample_provenance(run_id, "verified", true, 1));
    write_external_transfer_evidence(home_dir, run_id, &sample_external_transfer(run_id));

    let query = LocalInspectionQuery::new(home_dir, home_dir);
    let detail = query.get_run(run_id).expect("get_run").expect("run exists");

    assert!(detail.trusted_execution_mode);
    assert_eq!(
        detail
            .trust
            .workflow
            .as_ref()
            .expect("workflow")
            .signature_status,
        "verified"
    );
    assert_eq!(
        detail
            .trust
            .workflow
            .as_ref()
            .expect("workflow")
            .signer_key_id
            .as_deref(),
        Some("key_verified")
    );
    assert_eq!(detail.trust.plugins.len(), 1);
    assert_eq!(detail.trust.plugins[0].signature_status, "verified");
    assert_eq!(
        detail.trust.decision.as_ref().expect("decision").outcome,
        "verified"
    );
    assert!(detail.trust.policy_decision.is_some());

    assert_eq!(detail.locality.steps.len(), 1);
    assert_eq!(detail.locality.steps[0].provider.as_deref(), Some("openai"));
    assert_eq!(
        detail.locality.steps[0].provider_kind.as_deref(),
        Some("external")
    );
    assert_eq!(detail.locality.external_transfers.len(), 1);
    assert_eq!(
        detail.locality.external_transfers[0].destination.as_deref(),
        Some("api.openai.com")
    );
    assert_eq!(
        detail.locality.external_transfers[0].policy_mode,
        "trusted_execution"
    );

    assert!(detail.evidence_export.location.is_some());
    assert!(!detail.evidence_export.command.is_empty());
    assert_eq!(detail.evidence_export.format, "markdown");
}

#[test]
fn denied_trust_round_trips_through_inspection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path();
    fs::create_dir_all(home_dir.join("runs")).expect("runs dir");

    let run_id = "round-trip-denied";
    let record = sample_record(
        run_id,
        "demo_wf",
        "v1",
        Some(denied_trust_decision(run_id)),
        true,
    );
    write_run_record(home_dir, &record);
    write_provenance(home_dir, &sample_provenance(run_id, "denied", true, 0));

    let query = LocalInspectionQuery::new(home_dir, home_dir);
    let detail = query.get_run(run_id).expect("get_run").expect("run exists");

    assert_eq!(
        detail.trust.decision.as_ref().expect("decision").outcome,
        "denied"
    );
    assert_eq!(
        detail
            .trust
            .decision
            .as_ref()
            .expect("decision")
            .reason
            .as_deref(),
        Some("workflow_digest_mismatch")
    );
    assert_eq!(
        detail
            .trust
            .decision
            .as_ref()
            .expect("decision")
            .failed_verification_type
            .as_deref(),
        Some("workflow_signature")
    );
    assert_eq!(
        detail
            .trust
            .workflow
            .as_ref()
            .expect("workflow")
            .signature_status,
        "denied"
    );
    assert_eq!(detail.locality.external_transfers.len(), 0);
}

#[test]
fn bypassed_trust_round_trips_through_inspection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path();
    fs::create_dir_all(home_dir.join("runs")).expect("runs dir");

    let run_id = "round-trip-bypassed";
    let record = sample_record(
        run_id,
        "demo_wf",
        "v0",
        Some(bypassed_trust_decision(run_id)),
        true,
    );
    write_run_record(home_dir, &record);
    write_provenance(home_dir, &sample_provenance(run_id, "bypassed", true, 0));

    let query = LocalInspectionQuery::new(home_dir, home_dir);
    let detail = query.get_run(run_id).expect("get_run").expect("run exists");

    assert_eq!(
        detail.trust.decision.as_ref().expect("decision").outcome,
        "bypassed"
    );
    assert_eq!(
        detail
            .trust
            .workflow
            .as_ref()
            .expect("workflow")
            .signature_status,
        "bypassed"
    );
    assert_eq!(detail.trust.plugins[0].signature_status, "bypassed");
}

#[test]
fn run_without_provenance_still_loads_cleanly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home_dir = tmp.path();
    fs::create_dir_all(home_dir.join("runs")).expect("runs dir");

    let run_id = "round-trip-no-provenance";
    let record = sample_record(run_id, "demo_wf", "v1", None, false);
    write_run_record(home_dir, &record);

    let query = LocalInspectionQuery::new(home_dir, home_dir);
    let detail = query.get_run(run_id).expect("get_run").expect("run exists");

    assert!(!detail.trusted_execution_mode);
    assert!(detail.trust.workflow.is_none());
    assert!(detail.trust.decision.is_none());
    assert!(detail.trust.policy_decision.is_none());
    assert!(detail.locality.steps.is_empty());
    assert!(detail.locality.external_transfers.is_empty());
    assert!(detail.evidence_export.location.is_none());
    assert!(!detail.evidence_export.command.is_empty());
    assert_eq!(detail.evidence_export.format, "markdown");
}
