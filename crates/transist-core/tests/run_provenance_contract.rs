// SPDX-License-Identifier: MPL-2.0
use serde_json::Value;
use transist_core::{
    RUN_PROVENANCE_SCHEMA_V1, RunExternalTransferSummary, RunProvenanceDenied,
    RunProvenancePolicyDecision, RunProvenanceRecord,
};

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("failed reading fixture: {}", path.display()));
    serde_json::from_str(&raw).expect("fixture must be valid JSON")
}

#[test]
fn allowed_fixture_deserializes_and_matches_schema() {
    let json = fixture("run_provenance_allowed.json");
    let record: RunProvenanceRecord = serde_json::from_value(json.clone()).expect("deserialize");
    assert_eq!(record.schema, RUN_PROVENANCE_SCHEMA_V1);
    assert_eq!(record.run_id, "run-allowed");
    assert_eq!(record.workflow_id, "demo");
    assert_eq!(record.workflow_version, "v1");
    assert_eq!(record.workflow_digest.as_deref(), Some("abc123"));
    assert!(record.workflow_signature_verified);
    assert_eq!(record.workflow_signer_key_id.as_deref(), Some("key-1"));
    assert_eq!(record.workflow_trust_decision, "verified");
    assert_eq!(record.plugins.len(), 1);
    assert_eq!(record.plugins[0].id, "shell");
    assert_eq!(record.plugins[0].trust_decision, "verified");
    assert!(record.trusted_execution_mode);
    assert!(record.policy_decision.allowed);
    assert_eq!(record.policy_decision.mode, "run_start");
    assert_eq!(record.providers.len(), 1);
    assert_eq!(record.providers[0].provider.as_deref(), Some("openai"));
    assert_eq!(record.external_transfer_summary.count, 0);
    assert_eq!(record.approval_summary.len(), 1);
    assert_eq!(record.approval_summary[0].gate_id, "review");
    assert_eq!(record.decision, "allowed");
    assert!(record.denied.is_none());
    assert_eq!(record.created_at_ms, 1000);
    assert_eq!(record.finalized_at_ms, Some(2000));
    assert!(record.organization_id.is_none());
    assert!(record.policy_revision.is_none());
}

#[test]
fn denied_fixture_deserializes_and_matches_schema() {
    let json = fixture("run_provenance_denied.json");
    let record: RunProvenanceRecord = serde_json::from_value(json.clone()).expect("deserialize");
    assert_eq!(record.schema, RUN_PROVENANCE_SCHEMA_V1);
    assert_eq!(record.decision, "denied");
    let denied = record.denied.expect("denied block");
    assert_eq!(denied.reason, "digest_mismatch");
    assert_eq!(denied.offending_kind, "workflow");
    assert_eq!(denied.offending_id, "demo");
    assert_eq!(
        denied.failed_verification_type.as_deref(),
        Some("workflow_signature")
    );
    assert_eq!(denied.at_ms, 500);
    assert!(record.finalized_at_ms.is_none());
}

#[test]
fn serde_round_trip_preserves_forward_reserve_nulls() {
    let record = RunProvenanceRecord {
        schema: RUN_PROVENANCE_SCHEMA_V1.to_string(),
        run_id: "rt-run".to_string(),
        workflow_id: "wf".to_string(),
        workflow_version: "v1".to_string(),
        workflow_digest: None,
        workflow_signature_verified: false,
        workflow_signer_key_id: None,
        workflow_trust_decision: "not_verified".to_string(),
        plugins: Vec::new(),
        trusted_execution_mode: false,
        local_only_mode: false,
        runtime_mode: "unknown".to_string(),
        policy_profile: "unknown".to_string(),
        capability_enforcement: "unknown".to_string(),
        policy_decision: RunProvenancePolicyDecision {
            allowed: true,
            reason_code: None,
            message: None,
            mode: "run_start".to_string(),
        },
        providers: Vec::new(),
        external_transfer_summary: RunExternalTransferSummary {
            count: 0,
            external_transfers: Vec::new(),
        },
        approval_summary: Vec::new(),
        decision: "allowed".to_string(),
        denied: None,
        organization_id: None,
        policy_revision: None,
        created_at_ms: 1,
        finalized_at_ms: None,
    };
    let json = serde_json::to_string(&record).expect("serialize");
    let parsed: RunProvenanceRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, record);
    let value: Value = serde_json::from_str(&json).expect("parse as value");
    assert!(
        value.get("organization_id").is_none(),
        "organization_id must be omitted when None"
    );
    assert!(
        value.get("policy_revision").is_none(),
        "policy_revision must be omitted when None"
    );
}

#[test]
fn allowed_fixture_round_trips_through_serde() {
    let json = fixture("run_provenance_allowed.json");
    let record: RunProvenanceRecord =
        serde_json::from_value(json.clone()).expect("deserialize from fixture");
    let reserialized = serde_json::to_value(&record).expect("serialize back");
    assert_eq!(
        reserialized["schema"], json["schema"],
        "schema must not drift"
    );
    assert_eq!(
        reserialized["decision"], json["decision"],
        "decision must not drift"
    );
    let fixture_keys: Vec<String> = json.as_object().expect("object").keys().cloned().collect();
    let reserialized_keys: Vec<String> = reserialized
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    let missing: Vec<&String> = fixture_keys
        .iter()
        .filter(|k| !reserialized_keys.contains(k))
        .collect();
    assert!(missing.is_empty(), "keys lost in round-trip: {missing:?}");
}

#[test]
fn denied_fixture_includes_policy_offending_kind() {
    let mut denied = RunProvenanceDenied {
        reason: "policy_denied".to_string(),
        offending_kind: "policy".to_string(),
        offending_id: "demo".to_string(),
        failed_verification_type: Some("policy_denied".to_string()),
        at_ms: 42,
    };
    let json = serde_json::to_string(&denied).expect("serialize");
    let parsed: RunProvenanceDenied = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.offending_kind, "policy");
    denied.offending_kind = "filesystem_policy".to_string();
    let json2 = serde_json::to_string(&denied).expect("serialize");
    let parsed2: RunProvenanceDenied = serde_json::from_str(&json2).expect("deserialize");
    assert_eq!(parsed2.offending_kind, "filesystem_policy");
}
