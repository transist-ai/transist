// SPDX-License-Identifier: MPL-2.0
//! Serializable inspection snapshot envelope for local inspection and outbound export flows.
//!
//! This module composes the local inspection and metrics read models into a single
//! `transist.inspection.snapshot.v1` payload. The payload is read-only, local-first,
//! and intended for best-effort outbound sync to an external read model.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::{
    BlobFamily, BlobListScope, EventListQuery, InspectionBlob, InspectionGate,
    InspectionGateDecision, InspectionQueryPort, InspectionRunDetail, InspectionRunDetailSummary,
    InspectionRunSummary, InspectionStep, InspectionStepOutputSummary, MAX_EVENT_LIMIT, SortOrder,
    SyncPolicyConfig, contains_forbidden_key_recursive, forbidden_keys,
};
use transist_core::FailureRecord;
use transist_metrics::{MetricsQueryPort, StepExecutionMetricsSummary, StepMetricsSummary};
use transist_protocol::KernelValue;

/// Stable snapshot kind identifier for central inspection envelopes.
pub const INSPECTION_SNAPSHOT_KIND: &str = "transist.inspection.snapshot";

/// Stable schema version for the snapshot envelope.
pub const INSPECTION_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Validation error raised when a snapshot envelope still carries a forbidden key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeContainsForbiddenKey {
    /// Dot-formatted path to the offending key.
    pub path: String,
    /// The offending key itself.
    pub key: String,
}

impl fmt::Display for EnvelopeContainsForbiddenKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "inspection snapshot metadata contract violation at '{}' for key '{}'",
            self.path, self.key
        )
    }
}

impl std::error::Error for EnvelopeContainsForbiddenKey {}

/// Default maximum JSON payload size for one serialized snapshot envelope.
pub const DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES: usize = 5 * 1024 * 1024;

const EVENT_SUMMARY_ALLOWLIST_JSON: &str =
    include_str!("../../../schemas/inspection/event_summary_allowlist.json");
const SNAPSHOT_SCHEMA_JSON: &str =
    include_str!("../../../schemas/inspection/snapshot.v1.schema.json");

/// Builder input for one inspection snapshot envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotRequest {
    /// Run identifier used to resolve the root run tree.
    pub run_id: String,
    /// Central-issued durable source identifier.
    pub source_id: String,
    /// Operator or admin supplied workspace label.
    pub workspace_id: String,
    /// Optional operator or machine label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_label: Option<String>,
    /// Local production time in unix-millisecond UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_at_unix_ms: Option<u64>,
    /// Monotonic version assigned to the current root-run tree snapshot.
    pub tree_version: u64,
    /// Per-run lower exclusive event cursors.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub event_after_by_run: BTreeMap<String, u64>,
    /// Optional maximum allowed serialized payload size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_payload_bytes: Option<usize>,
    /// Optional sync policy descriptor used to opt into richer payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_policy: Option<SyncPolicyConfig>,
    /// Reserved additive extension fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, KernelValue>,
    /// Reserved additive provider fact fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_facts: BTreeMap<String, KernelValue>,
    /// Reserved additive policy fact fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policy_facts: BTreeMap<String, KernelValue>,
    /// Reserved forward-only central runtime enrollment identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// Reserved forward-only central organization identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Reserved forward-only policy revision identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_revision: Option<String>,
    /// Whether trusted-execution mode was enabled for the run tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_execution_mode: Option<bool>,
    /// Workflow digest captured for trust verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_digest: Option<String>,
    /// Workflow signature status: `verified`, `not_verified`, `denied`, or `bypassed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_signature_status: Option<String>,
    /// Signer key id present in the workflow signature when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_signer_key_id: Option<String>,
    /// Per-plugin trust provenance (digests + signature statuses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_trust: Option<Vec<InspectionSnapshotPluginTrust>>,
    /// Run-tree trust decision: `verified`, `denied`, or `bypassed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_decision: Option<String>,
    /// External-transfer summary for the run tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_transfer_summary: Option<InspectionSnapshotExternalTransferSummary>,
    /// Approver identities recorded for manual gates in the run tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approver_identity: Option<Vec<InspectionSnapshotApproverIdentity>>,
    /// Root-run locality view for provider/model and external-transfer display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<InspectionSnapshotLocalityView>,
    /// Root-run evidence export location metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_export: Option<InspectionSnapshotEvidenceExportView>,
}

impl Default for InspectionSnapshotRequest {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            source_id: String::new(),
            workspace_id: String::new(),
            operator_label: None,
            produced_at_unix_ms: None,
            tree_version: 0,
            event_after_by_run: BTreeMap::new(),
            max_payload_bytes: Some(DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES),
            sync_policy: None,
            extensions: BTreeMap::new(),
            provider_facts: BTreeMap::new(),
            policy_facts: BTreeMap::new(),
            runtime_id: None,
            organization_id: None,
            policy_revision: None,
            trusted_execution_mode: None,
            workflow_digest: None,
            workflow_signature_status: None,
            workflow_signer_key_id: None,
            plugin_trust: None,
            trust_decision: None,
            external_transfer_summary: None,
            approver_identity: None,
            locality: None,
            evidence_export: None,
        }
    }
}

/// Serialized `transist.inspection.snapshot.v1` envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotEnvelope {
    /// Stable snapshot kind identifier.
    pub kind: String,
    /// Snapshot schema version.
    pub schema_version: u32,
    /// Central-issued durable source identifier.
    pub source_id: String,
    /// Operator or admin supplied workspace label.
    pub workspace_id: String,
    /// Optional operator or machine label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_label: Option<String>,
    /// Local production time in unix-millisecond UTC.
    pub produced_at_unix_ms: u64,
    /// Monotonic version assigned to the current root-run tree snapshot.
    pub tree_version: u64,
    /// Root run identifier for the enclosed tree.
    pub root_run_id: String,
    /// Hash of the ordered per-run fingerprints in the tree.
    pub tree_fingerprint: String,
    /// Active sync policy descriptor used to produce this envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_policy: Option<SyncPolicyConfig>,
    /// Root run plus descendants included in this snapshot.
    pub runs: Vec<InspectionSnapshotRun>,
    /// Reserved additive extension fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, KernelValue>,
    /// Reserved additive provider fact fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_facts: BTreeMap<String, KernelValue>,
    /// Reserved additive policy fact fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policy_facts: BTreeMap<String, KernelValue>,
    /// Reserved forward-only central runtime enrollment identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// Reserved forward-only central organization identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Reserved forward-only policy revision identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_revision: Option<String>,
    /// Whether trusted-execution mode was enabled for the run tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_execution_mode: Option<bool>,
    /// Workflow digest captured for trust verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_digest: Option<String>,
    /// Workflow signature status: `verified`, `not_verified`, `denied`, or `bypassed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_signature_status: Option<String>,
    /// Signer key id present in the workflow signature when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_signer_key_id: Option<String>,
    /// Per-plugin trust provenance (digests + signature statuses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_trust: Option<Vec<InspectionSnapshotPluginTrust>>,
    /// Run-tree trust decision: `verified`, `denied`, or `bypassed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_decision: Option<String>,
    /// External-transfer summary for the run tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_transfer_summary: Option<InspectionSnapshotExternalTransferSummary>,
    /// Approver identities recorded for manual gates in the run tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approver_identity: Option<Vec<InspectionSnapshotApproverIdentity>>,
    /// Root-run locality view for provider/model and external-transfer display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<InspectionSnapshotLocalityView>,
    /// Root-run evidence export location metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_export: Option<InspectionSnapshotEvidenceExportView>,
}

impl InspectionSnapshotEnvelope {
    /// Validates the structural contract for one snapshot envelope.
    pub fn validate(&self) -> Result<()> {
        if self.kind != INSPECTION_SNAPSHOT_KIND {
            bail!(
                "snapshot kind '{}' does not match '{}'",
                self.kind,
                INSPECTION_SNAPSHOT_KIND
            );
        }
        if self.schema_version != INSPECTION_SNAPSHOT_SCHEMA_VERSION {
            bail!(
                "snapshot schema_version '{}' does not match '{}'",
                self.schema_version,
                INSPECTION_SNAPSHOT_SCHEMA_VERSION
            );
        }
        if self.source_id.trim().is_empty() {
            bail!("snapshot source_id must not be empty");
        }
        if self.workspace_id.trim().is_empty() {
            bail!("snapshot workspace_id must not be empty");
        }
        if self.root_run_id.trim().is_empty() {
            bail!("snapshot root_run_id must not be empty");
        }
        if self.runs.is_empty() {
            bail!("snapshot must contain at least one run block");
        }

        let mut seen = BTreeSet::new();
        let mut root_found = false;
        for (run_index, run) in self.runs.iter().enumerate() {
            if !seen.insert(run.run_id.clone()) {
                bail!("snapshot run '{}' is duplicated", run.run_id);
            }
            if run.run_id == self.root_run_id {
                root_found = true;
            }
            if run.run_summary.run_id != run.run_id {
                bail!(
                    "snapshot run '{}' does not match summary run '{}'",
                    run.run_id,
                    run.run_summary.run_id
                );
            }
            if run.run_detail_summary.summary.run_id != run.run_id {
                bail!(
                    "snapshot run '{}' does not match detail run '{}'",
                    run.run_id,
                    run.run_detail_summary.summary.run_id
                );
            }
            if run.run_fingerprint.trim().is_empty() {
                bail!("snapshot run '{}' is missing run_fingerprint", run.run_id);
            }

            let mut last_seq = 0;
            for (event_index, event) in run.events.iter().enumerate() {
                if event.event_seq <= last_seq {
                    bail!(
                        "snapshot run '{}' events are not strictly increasing",
                        run.run_id
                    );
                }
                last_seq = event.event_seq;

                validate_kernel_value(
                    KernelValue::Object(BTreeMap::from([(
                        "summary".to_string(),
                        event.summary.clone(),
                    )])),
                    &[
                        "runs".to_string(),
                        format!("[{run_index}]"),
                        "events".to_string(),
                        format!("[{event_index}]"),
                    ],
                )?;
            }
            if run.last_event_seq < last_seq {
                bail!(
                    "snapshot run '{}' last_event_seq '{}' is behind its events",
                    run.run_id,
                    run.last_event_seq
                );
            }

            for (failure_index, failure) in run.failures.iter().enumerate() {
                if failure.run_id != run.run_id {
                    bail!(
                        "snapshot run '{}' contains failure '{}' for run '{}'",
                        run.run_id,
                        failure.failure_id,
                        failure.run_id
                    );
                }
                validate_failure_record(
                    failure,
                    &[
                        "runs".to_string(),
                        format!("[{run_index}]"),
                        "failures".to_string(),
                        format!("[{failure_index}]"),
                    ],
                )?;
            }
        }

        if !root_found {
            bail!(
                "snapshot root run '{}' is not present in runs[]",
                self.root_run_id
            );
        }
        if self.tree_fingerprint.trim().is_empty() {
            bail!("snapshot tree_fingerprint must not be empty");
        }
        Ok(())
    }

    /// Returns the serialized JSON payload size in bytes.
    pub fn estimated_size_bytes(&self) -> Result<usize> {
        Ok(serde_json::to_vec(self)
            .context("failed serializing inspection snapshot envelope")?
            .len())
    }
}

/// Splits an inspection snapshot into payload-sized pages while preserving run context.
pub fn paginate_snapshot(
    snapshot: InspectionSnapshotEnvelope,
    max_payload_bytes: usize,
) -> Result<Vec<InspectionSnapshotEnvelope>> {
    if snapshot.estimated_size_bytes()? <= max_payload_bytes {
        return Ok(vec![snapshot]);
    }

    let mut template = snapshot.clone();
    for run in &mut template.runs {
        run.events.clear();
    }
    let template_size = template.estimated_size_bytes()?;
    if template_size > max_payload_bytes {
        bail!(
            "inspection snapshot for root run '{}' is {} bytes before any event delta; reduce metadata or raise the payload cap",
            template.root_run_id,
            template_size
        );
    }

    let run_events = snapshot
        .runs
        .iter()
        .map(|run| run.events.clone())
        .collect::<Vec<_>>();
    let mut cursors = vec![0usize; run_events.len()];
    let mut pages = Vec::new();

    while cursors
        .iter()
        .enumerate()
        .any(|(index, cursor)| *cursor < run_events[index].len())
    {
        let mut page = template.clone();
        let mut added_any = false;
        for (run_index, events) in run_events.iter().enumerate() {
            while cursors[run_index] < events.len() {
                page.runs[run_index]
                    .events
                    .push(events[cursors[run_index]].clone());
                let size = page.estimated_size_bytes()?;
                if size > max_payload_bytes {
                    page.runs[run_index].events.pop();
                    break;
                }
                cursors[run_index] += 1;
                added_any = true;
            }
        }

        if !added_any {
            bail!(
                "inspection snapshot for root run '{}' contains an event delta page that cannot fit within {} bytes",
                snapshot.root_run_id,
                max_payload_bytes
            );
        }
        pages.push(page);
    }

    if pages.is_empty() {
        pages.push(template);
    }
    Ok(pages)
}

/// Validates an inspection snapshot against the Rust invariants and bundled JSON schema.
pub fn validate_snapshot_contract(snapshot: &InspectionSnapshotEnvelope) -> Result<()> {
    snapshot.validate()?;
    let schema: serde_json::Value = serde_json::from_str(SNAPSHOT_SCHEMA_JSON)
        .context("invalid bundled inspection snapshot JSON schema")?;
    let instance = serde_json::to_value(snapshot)
        .context("failed serializing inspection snapshot for schema validation")?;
    let validator = jsonschema::Validator::new(&schema)
        .map_err(|error| anyhow!("failed compiling inspection snapshot JSON schema: {error}"))?;
    if let Err(errors) = validator.validate(&instance) {
        let messages = errors
            .take(5)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        bail!(
            "inspection snapshot schema validation failed: {}",
            messages.join("; ")
        );
    }
    Ok(())
}

/// One run block inside an inspection snapshot tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotRun {
    /// Stable run identifier.
    pub run_id: String,
    /// Compact run summary for list-style inspection.
    pub run_summary: InspectionRunSummary,
    /// Metadata-only run detail assembled from persisted local state.
    #[serde(alias = "run_detail")]
    pub run_detail_summary: InspectionRunDetailSummary,
    /// Selected step metrics rows keyed by step id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_metrics: Vec<InspectionSnapshotStepMetrics>,
    /// Manual gates recorded for the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<InspectionSnapshotGate>,
    /// Event delta since the supplied per-run cursor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<InspectionSnapshotEvent>,
    /// Artifact metadata only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<InspectionSnapshotBlobMetadata>,
    /// Evidence metadata only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<InspectionSnapshotBlobMetadata>,
    /// Classified failure records for this run.
    #[serde(default)]
    pub failures: Vec<FailureRecord>,
    /// Per-step usage facts for central-side aggregation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_facts: Vec<InspectionSnapshotUsageFact>,
    /// Hash of the run-local fingerprint input.
    pub run_fingerprint: String,
    /// Highest known event sequence for the run.
    pub last_event_seq: u64,
}

/// Snapshot-safe manual gate record with keys-only request metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotGate {
    /// Stable gate id.
    pub gate_id: String,
    /// Gate status string.
    pub status: String,
    /// Step id that owns the gate.
    pub step_id: String,
    /// Gate creation timestamp in Unix milliseconds when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<u64>,
    /// Recorded decision when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<InspectionGateDecision>,
    /// Full chronological history of gate decisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_history: Vec<InspectionGateDecision>,
    /// Request payload keys only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_keys: Vec<String>,
    /// Full request payload when extended sync explicitly allows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_summary: Option<BTreeMap<String, KernelValue>>,
}

/// Snapshot-safe plugin trust provenance carried for central fleet visibility.
///
/// Represents plugin digests and signature statuses in a stable, centrally
/// syncable shape. All fields mirror the persisted run provenance record and
/// are additive/nullable so older inspectors tolerate their absence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotPluginTrust {
    /// Stable plugin id.
    pub id: String,
    /// Stable plugin version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Recomputed digest captured for the plugin.
    pub digest: String,
    /// Whether detached-signature verification succeeded.
    pub signature_verified: bool,
    /// Signer key id when one was present in the detached signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key_id: Option<String>,
    /// Plugin-level trust outcome: `verified`, `denied`, `bypassed`, or `not_verified`.
    pub trust_decision: String,
}

/// Snapshot-safe external-transfer summary carried for central fleet visibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotExternalTransferSummary {
    /// Number of recorded external transfers.
    pub count: u64,
    /// Recorded transfer ledger entries for this run.
    #[serde(default)]
    pub external_transfers: Vec<InspectionSnapshotExternalTransferLedgerEntry>,
}

/// Snapshot-safe reference to one recorded external transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotExternalTransferLedgerEntry {
    /// Step id that triggered the external call.
    pub step_id: String,
    /// Run-root-relative path to the recorded evidence file.
    pub evidence_path: String,
    /// Best-effort destination identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Adapter kind used for the call.
    pub adapter_kind: String,
    /// Provider identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Declared provider locality marker.
    pub provider_kind: String,
    /// SHA-256 hash of the payload sent to the external destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Timestamp in Unix milliseconds when the transfer was recorded.
    pub timestamp_ms: u64,
}

/// Snapshot-safe approver identity carried for central fleet visibility.
///
/// Mirrors a manual-gate approval so central inspection can display approver
/// identity, reason, and timestamp later without coupling to cloud execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotApproverIdentity {
    /// Stable gate id.
    pub gate_id: String,
    /// Recorded decision such as `approved` or `rejected`.
    pub decision: String,
    /// Reserved future actor identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Optional human rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional decision timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at_ms: Option<u64>,
}

/// Snapshot-safe provider locality row carried for central inspection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotLocalityStep {
    /// Step id that reported provider/model usage.
    pub step_id: String,
    /// Provider identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Declared provider locality marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
}

/// Snapshot-safe external-transfer evidence row carried for central inspection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotExternalTransfer {
    /// Step id that triggered the external call.
    pub step_id: String,
    /// Step execution number.
    pub execution: u32,
    /// Timestamp in Unix milliseconds when the transfer was recorded.
    pub timestamp_ms: u64,
    /// Best-effort destination identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Adapter kind used for the call.
    pub adapter_kind: String,
    /// Provider identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Declared provider locality marker.
    pub provider_kind: String,
    /// Bytes sent when measurable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// SHA-256 hash of the sent payload when captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Execution mode that governed the transfer evidence.
    pub policy_mode: String,
}

/// Snapshot-safe locality view carried for central inspection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotLocalityView {
    /// Provider/model rows keyed by step execution provenance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<InspectionSnapshotLocalityStep>,
    /// External-transfer evidence records. Local-only runs keep this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_transfers: Vec<InspectionSnapshotExternalTransfer>,
}

/// Snapshot-safe evidence export location metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionSnapshotEvidenceExportView {
    /// Where an operator can retrieve or generate the report, when provenance exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Stable command hint for generating the export.
    pub command: String,
    /// Export format currently supported by the runtime.
    pub format: String,
    /// Timestamp associated with the persisted provenance used for the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at_ms: Option<u64>,
}

/// One compact event summary inside a snapshot run block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotEvent {
    /// Monotonic event sequence within the run.
    pub event_seq: u64,
    /// Event timestamp in unix-millisecond UTC.
    pub timestamp_unix_ms: u64,
    /// Stable event kind label.
    pub event_type: String,
    /// Step associated with the event when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Compact structured event summary payload.
    pub summary: KernelValue,
}

/// Artifact or evidence metadata retained in a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotBlobMetadata {
    /// Run that owns the blob.
    pub run_id: String,
    /// Blob family label such as `artifact` or `evidence`.
    pub family: String,
    /// Step id that produced the blob.
    pub step_id: String,
    /// Execution number that produced the blob.
    pub execution: u32,
    /// Relative path within the family directory.
    pub relative_path: String,
    /// File name component of the blob path.
    pub name: String,
    /// Blob size in bytes.
    pub bytes: u64,
    /// Blob creation timestamp in unix-millisecond UTC when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<u64>,
    /// Optional descriptor metadata recorded alongside the blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<InspectionSnapshotBlobDescriptor>,
}

/// Structured descriptor metadata attached to a snapshot blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotBlobDescriptor {
    /// Producer-side source path recorded for the blob.
    pub source_path: String,
    /// Optional SHA-256 checksum copied from the descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Optional byte count copied from the descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Optional invoke id copied from the descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoke_id: Option<String>,
    /// Optional producer-reported status copied from the descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Selected step metrics included in a snapshot run block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotStepMetrics {
    /// Stable step identifier.
    pub step_id: String,
    /// Total execution count recorded for the step subtree.
    pub execution_count: u32,
    /// Execution counts grouped by status label.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub status_counts: BTreeMap<String, u32>,
    /// Latest concrete execution summary for the step itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_execution: Option<StepExecutionMetricsSummary>,
    /// Summed active execution duration in milliseconds across the step subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_execution_duration_ms: Option<u64>,
    /// Summed blocked wait duration in milliseconds across the step subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_wait_duration_ms: Option<u64>,
    /// Whether the latest step execution produced any stderr output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub had_stderr: Option<bool>,
    /// Whether the latest step execution recorded an error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub had_error: Option<bool>,
}

/// One per-step usage fact used for central-side aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionSnapshotUsageFact {
    /// Run that owns the step.
    pub run_id: String,
    /// Step that reported usage.
    pub step_id: String,
    /// Plugin id recorded on the invoke request metadata.
    pub plugin_id: String,
    /// Action id recorded on the invoke request.
    pub action: String,
    /// Provider identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Input token count when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output token count when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Total token count when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Number of usage reports folded into this row.
    pub usage_reports: u64,
    /// Best-effort observation time for the usage row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_unix_ms: Option<u64>,
}

/// Builder that composes local inspection and metrics views into one snapshot envelope.
pub struct InspectionSnapshotBuilder<'a> {
    inspection: &'a dyn InspectionQueryPort,
    metrics: &'a dyn MetricsQueryPort,
}

impl<'a> InspectionSnapshotBuilder<'a> {
    /// Creates a snapshot builder from read-only inspection and metrics query ports.
    pub fn new(inspection: &'a dyn InspectionQueryPort, metrics: &'a dyn MetricsQueryPort) -> Self {
        Self {
            inspection,
            metrics,
        }
    }

    /// Builds one root-run-tree snapshot envelope.
    ///
    /// Returns `Ok(None)` when the requested run does not exist.
    pub fn build(
        &self,
        request: &InspectionSnapshotRequest,
    ) -> Result<Option<InspectionSnapshotEnvelope>> {
        validate_request(request)?;

        let requested = match self.inspection.get_run(&request.run_id)? {
            Some(run) => run,
            None => return Ok(None),
        };
        let root_run_id = requested.summary.root_run_id.clone();
        let root_detail = if requested.summary.run_id == root_run_id {
            requested
        } else {
            self.inspection
                .get_run(&root_run_id)?
                .with_context(|| format!("root run '{}' was not found", root_run_id))?
        };

        let mut ordered_run_ids = vec![root_run_id.clone()];
        ordered_run_ids.extend(
            root_detail
                .contained_runs
                .iter()
                .map(|run| run.run_id.clone()),
        );

        let run_details = self.inspection.get_runs(&ordered_run_ids)?;
        let produced_at_unix_ms = request.produced_at_unix_ms.unwrap_or_else(now_unix_ms);
        let sync_policy = request.sync_policy.clone().unwrap_or_default();
        let mut runs = Vec::with_capacity(ordered_run_ids.len());
        for run_id in &ordered_run_ids {
            let detail = run_details
                .get(run_id)
                .cloned()
                .ok_or_else(|| anyhow!("snapshot run '{}' could not be loaded", run_id))?;
            let after_seq = request.event_after_by_run.get(run_id).copied();
            let events = list_all_events_after(self.inspection, run_id, after_seq, &sync_policy)?;
            let artifacts = self
                .inspection
                .list_blobs(run_id, BlobFamily::Artifact, BlobListScope::SelfOnly)?
                .unwrap_or_default()
                .into_iter()
                .map(blob_metadata_from_blob)
                .collect::<Vec<_>>();
            let evidence = self
                .inspection
                .list_blobs(run_id, BlobFamily::Evidence, BlobListScope::SelfOnly)?
                .unwrap_or_default()
                .into_iter()
                .map(blob_metadata_from_blob)
                .collect::<Vec<_>>();
            let gates = self
                .inspection
                .list_gates(run_id)?
                .unwrap_or_default()
                .into_iter()
                .map(|gate| snapshot_gate_from_gate(gate, &sync_policy))
                .collect();
            let failures = self.inspection.list_failures(run_id)?.unwrap_or_default();
            let (step_metrics, usage_facts) =
                self.snapshot_step_metrics_and_usage(run_id, &detail.steps)?;
            let last_event_seq =
                parse_optional_event_seq(detail.latest_event_id.as_deref())?.unwrap_or(0);
            let run_fingerprint = run_fingerprint(&detail, last_event_seq)?;

            validate_sync_policy_payloads(run_id, &detail, &sync_policy)?;

            runs.push(InspectionSnapshotRun {
                run_id: run_id.clone(),
                run_summary: detail.summary.clone(),
                run_detail_summary: InspectionRunDetailSummary::from_detail(&detail, &sync_policy),
                step_metrics,
                gates,
                events,
                artifacts,
                evidence,
                failures,
                usage_facts,
                run_fingerprint,
                last_event_seq,
            });
        }

        let envelope = InspectionSnapshotEnvelope {
            kind: INSPECTION_SNAPSHOT_KIND.to_string(),
            schema_version: INSPECTION_SNAPSHOT_SCHEMA_VERSION,
            source_id: request.source_id.clone(),
            workspace_id: request.workspace_id.clone(),
            operator_label: request.operator_label.clone(),
            produced_at_unix_ms,
            tree_version: request.tree_version,
            root_run_id,
            tree_fingerprint: tree_fingerprint(&runs)?,
            sync_policy: Some(sync_policy),
            runs,
            extensions: request.extensions.clone(),
            provider_facts: request.provider_facts.clone(),
            policy_facts: request.policy_facts.clone(),
            runtime_id: request.runtime_id.clone(),
            organization_id: request.organization_id.clone(),
            policy_revision: request.policy_revision.clone(),
            trusted_execution_mode: request.trusted_execution_mode,
            workflow_digest: request.workflow_digest.clone(),
            workflow_signature_status: request.workflow_signature_status.clone(),
            workflow_signer_key_id: request.workflow_signer_key_id.clone(),
            plugin_trust: request.plugin_trust.clone(),
            trust_decision: request.trust_decision.clone(),
            external_transfer_summary: request.external_transfer_summary.clone(),
            approver_identity: request.approver_identity.clone(),
            locality: request.locality.clone(),
            evidence_export: request.evidence_export.clone(),
        };
        envelope.validate()?;
        let estimated_size = envelope.estimated_size_bytes()?;
        let max_payload_bytes = request
            .max_payload_bytes
            .unwrap_or(DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES);
        if estimated_size > max_payload_bytes {
            bail!(
                "inspection snapshot for root run '{}' is {} bytes, exceeding max_payload_bytes {}",
                envelope.root_run_id,
                estimated_size,
                max_payload_bytes
            );
        }
        Ok(Some(envelope))
    }

    fn snapshot_step_metrics_and_usage(
        &self,
        run_id: &str,
        steps: &[InspectionStep],
    ) -> Result<(
        Vec<InspectionSnapshotStepMetrics>,
        Vec<InspectionSnapshotUsageFact>,
    )> {
        let mut step_metrics = Vec::new();
        let mut usage_facts = Vec::new();
        for step in steps {
            let metrics = self.metrics.get_step_metrics(run_id, &step.step_id)?;
            let Some(metrics) = metrics else {
                continue;
            };
            step_metrics.push(snapshot_step_metrics_from_summary(
                &step.step_id,
                &metrics,
                &step.output_summary,
            ));
            usage_facts.extend(usage_facts_from_step(run_id, step, &metrics));
        }
        Ok((step_metrics, usage_facts))
    }
}

#[derive(Serialize)]
struct RunFingerprintInput<'a> {
    run_status: &'a str,
    last_step_id: Option<&'a str>,
    last_step_status: Option<&'a str>,
    step_count: usize,
    last_event_seq: u64,
    last_modified_unix_ms: u64,
}

#[derive(Serialize)]
struct TreeFingerprintInput<'a> {
    run_id: &'a str,
    run_fingerprint: &'a str,
}

fn validate_request(request: &InspectionSnapshotRequest) -> Result<()> {
    if request.run_id.trim().is_empty() {
        bail!("snapshot request run_id must not be empty");
    }
    if request.source_id.trim().is_empty() {
        bail!("snapshot request source_id must not be empty");
    }
    if request.workspace_id.trim().is_empty() {
        bail!("snapshot request workspace_id must not be empty");
    }
    Ok(())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn list_all_events_after(
    inspection: &dyn InspectionQueryPort,
    run_id: &str,
    after: Option<u64>,
    policy: &SyncPolicyConfig,
) -> Result<Vec<InspectionSnapshotEvent>> {
    let mut next_after = after;
    let mut events = Vec::new();
    loop {
        let page = inspection
            .list_events(
                run_id,
                &EventListQuery {
                    limit: MAX_EVENT_LIMIT,
                    before: None,
                    after: next_after,
                    order: SortOrder::Asc,
                },
            )?
            .ok_or_else(|| anyhow!("run '{}' disappeared while listing events", run_id))?;
        if page.data.is_empty() {
            break;
        }

        let mut last_seq = next_after.unwrap_or(0);
        for event in page.data {
            let event_seq = parse_event_seq(&event.event_id)?;
            if event_seq <= last_seq {
                bail!(
                    "run '{}' returned non-increasing event sequence '{}' while paging",
                    run_id,
                    event_seq
                );
            }
            last_seq = event_seq;
            let summary = if policy.include_event_payloads {
                validate_kernel_value(
                    event.payload.clone(),
                    &[
                        "runs".to_string(),
                        run_id.to_string(),
                        "events".to_string(),
                        format!("[{event_seq}]"),
                        "summary".to_string(),
                    ],
                )?;
                event.payload
            } else {
                allowlisted_event_summary(&event.event_type, event.payload)
            };
            events.push(InspectionSnapshotEvent {
                event_seq,
                timestamp_unix_ms: event.timestamp_unix_ms,
                event_type: event.event_type.clone(),
                step_id: event.step_id,
                summary,
            });
        }
        if events.len() % MAX_EVENT_LIMIT != 0 {
            break;
        }
        next_after = Some(last_seq);
    }
    Ok(events)
}

fn parse_event_seq(raw: &str) -> Result<u64> {
    raw.parse::<u64>()
        .with_context(|| format!("invalid event id '{}' for inspection snapshot", raw))
}

fn parse_optional_event_seq(raw: Option<&str>) -> Result<Option<u64>> {
    raw.map(parse_event_seq).transpose()
}

fn blob_metadata_from_blob(blob: InspectionBlob) -> InspectionSnapshotBlobMetadata {
    InspectionSnapshotBlobMetadata {
        run_id: blob.run_id,
        family: match blob.family {
            BlobFamily::Artifact => "artifact",
            BlobFamily::Evidence => "evidence",
        }
        .to_string(),
        step_id: blob.step_id,
        execution: blob.execution,
        relative_path: blob.relative_path,
        name: blob.name,
        bytes: blob.bytes,
        created_at_ms: blob.created_at_ms,
        descriptor: blob
            .descriptor
            .map(|descriptor| InspectionSnapshotBlobDescriptor {
                source_path: descriptor.source_path,
                sha256: descriptor.sha256,
                bytes: descriptor.bytes,
                invoke_id: descriptor.invoke_id,
                status: descriptor.status,
            }),
    }
}

fn snapshot_gate_from_gate(
    mut gate: InspectionGate,
    policy: &SyncPolicyConfig,
) -> InspectionSnapshotGate {
    // Exact review refs are a local review/API surface. Snapshot request detail
    // remains controlled by include_gate_request_summary, not decision metadata.
    if let Some(decision) = &mut gate.decision {
        decision.review_request = None;
    }
    for decision in &mut gate.decision_history {
        decision.review_request = None;
    }
    let request_summary = gate.request_summary;
    let mut request_keys = request_summary.keys().cloned().collect::<Vec<_>>();
    request_keys.sort();

    let request_summary = if policy.include_gate_request_summary {
        Some(request_summary)
    } else {
        None
    };

    InspectionSnapshotGate {
        gate_id: gate.gate_id,
        status: gate.status,
        step_id: gate.step_id,
        created_at_ms: gate.created_at_ms,
        decision: gate.decision,
        decision_history: gate.decision_history,
        request_keys,
        request_summary,
    }
}

fn validate_sync_policy_payloads(
    run_id: &str,
    detail: &InspectionRunDetail,
    policy: &SyncPolicyConfig,
) -> Result<()> {
    if policy.include_workflow_input_values || should_include_run_detail_full(policy) {
        validate_kernel_value(
            KernelValue::Object(detail.workflow_inputs.clone()),
            &[
                "runs".to_string(),
                format!("run:{run_id}"),
                "run_detail_summary".to_string(),
                "workflow_input_values".to_string(),
            ],
        )?;
    }

    if should_include_run_detail_full(policy) || policy.include_gate_request_summary {
        for (gate_index, gate) in detail.blocked_gates.iter().enumerate() {
            validate_kernel_value(
                KernelValue::Object(gate.request_summary.clone()),
                &[
                    "runs".to_string(),
                    format!("run:{run_id}"),
                    "run_detail_summary".to_string(),
                    "run_detail_full".to_string(),
                    "blocked_gates".to_string(),
                    format!("[{gate_index}]"),
                ],
            )?;
        }
    }

    Ok(())
}

fn should_include_run_detail_full(policy: &SyncPolicyConfig) -> bool {
    policy.include_run_detail_full
        || policy.include_stderr_tail
        || policy.include_gate_request_summary
}

fn allowlisted_event_summary(event_type: &str, summary: KernelValue) -> KernelValue {
    let allowlist = event_summary_allowlist();
    let Some(allowed_keys) = allowlist.get(event_type) else {
        return KernelValue::Object(BTreeMap::from([(
            "unknown_event_type".to_string(),
            KernelValue::Bool(true),
        )]));
    };

    let KernelValue::Object(entries) = summary else {
        return KernelValue::Object(BTreeMap::new());
    };

    KernelValue::Object(
        entries
            .into_iter()
            .filter(|(key, _)| allowed_keys.contains(key))
            .collect(),
    )
}

fn event_summary_allowlist() -> &'static BTreeMap<String, BTreeSet<String>> {
    static EVENT_SUMMARY_ALLOWLIST: OnceLock<BTreeMap<String, BTreeSet<String>>> = OnceLock::new();

    EVENT_SUMMARY_ALLOWLIST.get_or_init(|| {
        let parsed: BTreeMap<String, Vec<String>> =
            serde_json::from_str(EVENT_SUMMARY_ALLOWLIST_JSON)
                .expect("event_summary_allowlist.json must be valid");

        parsed
            .into_iter()
            .map(|(event_type, keys)| {
                let set = keys.iter().cloned().collect::<BTreeSet<_>>();
                if set.len() != keys.len() {
                    warn!(
                        event_type,
                        "duplicate keys found in event summary allowlist"
                    );
                }
                (event_type, set)
            })
            .collect()
    })
}

fn validate_kernel_value(value: KernelValue, prefix: &[String]) -> Result<()> {
    let Some(path_segments) = contains_forbidden_key_recursive(&value) else {
        return Ok(());
    };

    let mut combined = prefix.to_vec();
    combined.extend(path_segments);
    let key = combined
        .iter()
        .rev()
        .find(|segment| !segment.starts_with('['))
        .cloned()
        .unwrap_or_default();

    Err(EnvelopeContainsForbiddenKey {
        path: format_path(&combined),
        key,
    }
    .into())
}

fn validate_failure_record(failure: &FailureRecord, prefix: &[String]) -> Result<()> {
    if failure.failure_id.trim().is_empty() {
        bail!("snapshot failure record must not have an empty failure_id");
    }
    if failure.step_id.trim().is_empty() {
        bail!(
            "snapshot failure '{}' must not have an empty step_id",
            failure.failure_id
        );
    }
    if let Some(key) = forbidden_key_token_in_message(&failure.message_redacted) {
        let mut combined = prefix.to_vec();
        combined.push("message_redacted".to_string());
        return Err(EnvelopeContainsForbiddenKey {
            path: format_path(&combined),
            key: key.to_string(),
        }
        .into());
    }
    Ok(())
}

fn forbidden_key_token_in_message(message: &str) -> Option<&'static str> {
    message
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .filter(|token| !token.is_empty())
        .find_map(|token| {
            forbidden_keys()
                .iter()
                .copied()
                .find(|forbidden| token.eq_ignore_ascii_case(forbidden))
        })
}

fn format_path(segments: &[String]) -> String {
    let mut path = String::new();
    for segment in segments {
        if segment.starts_with('[') || path.is_empty() {
            path.push_str(segment);
        } else {
            path.push('.');
            path.push_str(segment);
        }
    }
    path
}

fn snapshot_step_metrics_from_summary(
    step_id: &str,
    metrics: &StepMetricsSummary,
    output_summary: &InspectionStepOutputSummary,
) -> InspectionSnapshotStepMetrics {
    InspectionSnapshotStepMetrics {
        step_id: step_id.to_string(),
        execution_count: metrics.execution_count,
        status_counts: metrics.status_counts.clone(),
        latest_execution: metrics.latest_execution.clone(),
        active_execution_duration_ms: metrics.active_execution_duration_ms,
        blocked_wait_duration_ms: metrics.blocked_wait_duration_ms,
        had_stderr: output_summary.stderr_tail.as_ref().map(|s| !s.is_empty()),
        had_error: output_summary.error_message.as_ref().map(|s| !s.is_empty()),
    }
}

fn usage_facts_from_step(
    run_id: &str,
    step: &InspectionStep,
    metrics: &StepMetricsSummary,
) -> Vec<InspectionSnapshotUsageFact> {
    let Some(plugin_id) = step
        .input_summary
        .plugin_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };
    let Some(action) = step
        .input_summary
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };

    let observed_at_unix_ms = metrics
        .latest_execution
        .as_ref()
        .and_then(usage_observed_at)
        .or(step.finished_at_ms)
        .or(step.started_at_ms);

    metrics
        .usage_breakdown
        .iter()
        .map(|entry| InspectionSnapshotUsageFact {
            run_id: run_id.to_string(),
            step_id: step.step_id.clone(),
            plugin_id: plugin_id.to_string(),
            action: action.to_string(),
            provider: entry.provider.clone(),
            model: entry.model.clone(),
            input_tokens: entry.input_tokens,
            output_tokens: entry.output_tokens,
            total_tokens: entry.total_tokens,
            usage_reports: entry.usage_reports,
            observed_at_unix_ms,
        })
        .collect()
}

fn usage_observed_at(execution: &StepExecutionMetricsSummary) -> Option<u64> {
    execution.finished_at_ms.or(execution.started_at_ms)
}

fn run_fingerprint(detail: &InspectionRunDetail, last_event_seq: u64) -> Result<String> {
    let (last_step_id, last_step_status) = last_fingerprint_step(detail)
        .map(|step| (Some(step.step_id.as_str()), Some(step.status.as_str())))
        .unwrap_or((None, None));
    fingerprint_hex(&RunFingerprintInput {
        run_status: &detail.summary.status,
        last_step_id,
        last_step_status,
        step_count: detail.steps.len(),
        last_event_seq,
        last_modified_unix_ms: detail.summary.updated_at_ms,
    })
}

fn tree_fingerprint(runs: &[InspectionSnapshotRun]) -> Result<String> {
    let ordered = runs
        .iter()
        .map(|run| TreeFingerprintInput {
            run_id: run.run_id.as_str(),
            run_fingerprint: run.run_fingerprint.as_str(),
        })
        .collect::<Vec<_>>();
    fingerprint_hex(&ordered)
}

fn fingerprint_hex(value: &impl Serialize) -> Result<String> {
    let bytes =
        serde_json::to_vec(value).context("failed serializing snapshot fingerprint input")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn last_fingerprint_step(detail: &InspectionRunDetail) -> Option<&InspectionStep> {
    detail.steps.iter().max_by(|left, right| {
        step_fingerprint_sort_key(left)
            .cmp(&step_fingerprint_sort_key(right))
            .then(left.step_id.cmp(&right.step_id))
    })
}

fn step_fingerprint_sort_key(step: &InspectionStep) -> (u64, u32) {
    (
        step.finished_at_ms.or(step.started_at_ms).unwrap_or(0),
        step.execution.unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalInspectionQuery;
    use std::collections::BTreeMap;
    use transist_core::ports::{EventPort, RunStorePort, RunStoreStepExecution};
    use transist_core::{FailureClass, FailureRecord};
    use transist_core::{
        KernelRunSnapshot, KernelRunStatus, KernelStepState, KernelStepStatus, PersistedRunRecord,
        RecipeSnapshotRef, RunLineage, RunLineageMode, StepExecutionRecord,
    };
    use transist_metrics::LocalMetricsQuery;
    use transist_protocol::{
        AdapterKind, ExecutionRequest, ExecutionResult, InvokePaths, InvokeStep, RuntimeEvent,
        TEP_INVOKE_REQUEST_TYPE_V1, TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
    };
    use transist_storage_local::{
        LocalEventSink, LocalRunStore, LocalRunSummaryStore, LocalStorageConfig,
    };

    #[test]
    fn inspection_snapshot_envelope_resolves_root_tree_and_usage_facts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path();
        let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
        let summaries = LocalRunSummaryStore::new(home_dir);
        let events = LocalEventSink::new(LocalStorageConfig::new(home_dir));

        let child_run_id = "root__child";
        let child_request =
            sample_request(child_run_id, "plugin_step", "complete", Some("ai.planner"));
        let child_result = sample_usage_result(
            child_run_id,
            "plugin_step",
            Some(serde_json::json!({
                "provider": "openai",
                "model": "gpt-4.1",
                "input_tokens": 11,
                "output_tokens": 19,
                "total_tokens": 30
            })),
            None,
        );
        let child = sample_record(
            child_run_id,
            "child_wf",
            vec![(
                "plugin_step",
                KernelStepState {
                    step_id: "plugin_step".to_string(),
                    status: KernelStepStatus::Succeeded,
                    executions: 1,
                    retry_wait_until_ms: None,
                    skip_reason: None,
                    last_request: Some(child_request.clone()),
                    last_result: Some(child_result.clone()),
                    started_at_ms: Some(200),
                    finished_at_ms: Some(350),
                    activation_executions_baseline: 0,
                },
            )],
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root".to_string(),
                parent_run_id: Some("root".to_string()),
                parent_step_id: Some("subflow".to_string()),
            }),
            200,
            350,
            KernelRunStatus::Success,
        );

        let root_result = sample_usage_result(
            "root",
            "subflow",
            None,
            Some(serde_json::json!({ "child_run_id": child_run_id, "status": "success" })),
        );
        let root = sample_record(
            "root",
            "root_wf",
            vec![(
                "subflow",
                KernelStepState {
                    step_id: "subflow".to_string(),
                    status: KernelStepStatus::Succeeded,
                    executions: 1,
                    retry_wait_until_ms: None,
                    skip_reason: None,
                    last_request: None,
                    last_result: Some(root_result.clone()),
                    started_at_ms: Some(100),
                    finished_at_ms: Some(360),
                    activation_executions_baseline: 0,
                },
            )],
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "root".to_string(),
                parent_run_id: None,
                parent_step_id: None,
            }),
            100,
            360,
            KernelRunStatus::Success,
        );

        store.create_run(&root).expect("create root");
        store.create_run(&child).expect("create child");
        persist_execution(
            &store,
            step_execution_record("root", "subflow", None, Some(root_result)),
        );
        persist_execution(
            &store,
            step_execution_record(
                child_run_id,
                "plugin_step",
                Some(child_request),
                Some(child_result),
            ),
        );

        summaries.refresh_run("root").expect("refresh root summary");
        summaries
            .refresh_run(child_run_id)
            .expect("refresh child summary");

        persist_event(
            &events,
            &summaries,
            runtime_event("root", None, "run_created", 1000),
        );
        persist_event(
            &events,
            &summaries,
            runtime_event("root", Some("subflow"), "step_finished", 1100),
        );
        persist_event(
            &events,
            &summaries,
            runtime_event(child_run_id, Some("plugin_step"), "step_finished", 1200),
        );

        let inspection = LocalInspectionQuery::new(home_dir, home_dir);
        let metrics = LocalMetricsQuery::new(home_dir);
        let builder = InspectionSnapshotBuilder::new(&inspection, &metrics);
        let request = InspectionSnapshotRequest {
            run_id: child_run_id.to_string(),
            source_id: "src_example".to_string(),
            workspace_id: "example/workflows".to_string(),
            operator_label: Some("laptop-a".to_string()),
            produced_at_unix_ms: Some(5_000),
            tree_version: 7,
            event_after_by_run: BTreeMap::new(),
            max_payload_bytes: Some(DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES),
            sync_policy: None,
            extensions: BTreeMap::new(),
            provider_facts: BTreeMap::new(),
            policy_facts: BTreeMap::new(),
            runtime_id: None,
            organization_id: None,
            policy_revision: None,
            trusted_execution_mode: None,
            workflow_digest: None,
            workflow_signature_status: None,
            workflow_signer_key_id: None,
            plugin_trust: None,
            trust_decision: None,
            external_transfer_summary: None,
            approver_identity: None,
            locality: None,
            evidence_export: None,
        };

        let snapshot = builder
            .build(&request)
            .expect("build snapshot")
            .expect("snapshot");

        assert_eq!(snapshot.kind, INSPECTION_SNAPSHOT_KIND);
        assert_eq!(snapshot.schema_version, INSPECTION_SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(snapshot.root_run_id, "root");
        assert_eq!(snapshot.runs.len(), 2);
        assert!(!snapshot.tree_fingerprint.is_empty());
        snapshot.validate().expect("validate snapshot");

        let root_run = snapshot
            .runs
            .iter()
            .find(|run| run.run_id == "root")
            .expect("root run block");
        assert_eq!(root_run.events.len(), 2);
        assert_eq!(root_run.last_event_seq, 2);
        assert!(root_run.failures.is_empty());
        let root_run_json = serde_json::to_value(root_run).expect("root run json");
        assert!(root_run_json.get("failures").is_some());

        let child_run = snapshot
            .runs
            .iter()
            .find(|run| run.run_id == child_run_id)
            .expect("child run block");
        assert_eq!(child_run.events.len(), 1);
        assert_eq!(child_run.last_event_seq, 1);
        assert_eq!(child_run.usage_facts.len(), 1);
        assert_eq!(child_run.usage_facts[0].plugin_id, "ai.planner");
        assert_eq!(child_run.usage_facts[0].action, "complete");
        assert_eq!(child_run.usage_facts[0].provider.as_deref(), Some("openai"));
        assert_eq!(child_run.usage_facts[0].model.as_deref(), Some("gpt-4.1"));
        assert_eq!(child_run.usage_facts[0].total_tokens, Some(30));
        assert!(child_run.failures.is_empty());

        assert!(root_run.usage_facts.is_empty());
    }

    #[test]
    fn inspection_snapshot_envelope_respects_event_cursors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path();
        let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
        let summaries = LocalRunSummaryStore::new(home_dir);
        let events = LocalEventSink::new(LocalStorageConfig::new(home_dir));

        let record = sample_record(
            "single",
            "wf",
            vec![(
                "step",
                KernelStepState {
                    step_id: "step".to_string(),
                    status: KernelStepStatus::Succeeded,
                    executions: 1,
                    retry_wait_until_ms: None,
                    skip_reason: None,
                    last_request: None,
                    last_result: Some(sample_usage_result(
                        "single",
                        "step",
                        None,
                        Some(serde_json::json!({})),
                    )),
                    started_at_ms: Some(10),
                    finished_at_ms: Some(20),
                    activation_executions_baseline: 0,
                },
            )],
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "single".to_string(),
                parent_run_id: None,
                parent_step_id: None,
            }),
            10,
            20,
            KernelRunStatus::Success,
        );

        store.create_run(&record).expect("create run");
        persist_execution(
            &store,
            step_execution_record(
                "single",
                "step",
                None,
                Some(sample_usage_result(
                    "single",
                    "step",
                    None,
                    Some(serde_json::json!({})),
                )),
            ),
        );
        summaries.refresh_run("single").expect("refresh summary");
        persist_event(
            &events,
            &summaries,
            runtime_event("single", None, "run_created", 100),
        );
        persist_event(
            &events,
            &summaries,
            runtime_event("single", Some("step"), "step_started", 200),
        );
        persist_event(
            &events,
            &summaries,
            runtime_event("single", Some("step"), "step_finished", 300),
        );

        let inspection = LocalInspectionQuery::new(home_dir, home_dir);
        let metrics = LocalMetricsQuery::new(home_dir);
        let builder = InspectionSnapshotBuilder::new(&inspection, &metrics);

        let request = InspectionSnapshotRequest {
            run_id: "single".to_string(),
            source_id: "src_example".to_string(),
            workspace_id: "example/workflows".to_string(),
            operator_label: None,
            produced_at_unix_ms: Some(999),
            tree_version: 2,
            event_after_by_run: BTreeMap::from([("single".to_string(), 1)]),
            max_payload_bytes: Some(DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES),
            sync_policy: None,
            extensions: BTreeMap::new(),
            provider_facts: BTreeMap::new(),
            policy_facts: BTreeMap::new(),
            runtime_id: None,
            organization_id: None,
            policy_revision: None,
            trusted_execution_mode: None,
            workflow_digest: None,
            workflow_signature_status: None,
            workflow_signer_key_id: None,
            plugin_trust: None,
            trust_decision: None,
            external_transfer_summary: None,
            approver_identity: None,
            locality: None,
            evidence_export: None,
        };

        let snapshot = builder
            .build(&request)
            .expect("build snapshot")
            .expect("snapshot");
        let run = snapshot.runs.first().expect("single run");
        assert_eq!(run.events.len(), 2);
        assert_eq!(run.events[0].event_seq, 2);
        assert_eq!(run.events[1].event_seq, 3);
        assert_eq!(run.last_event_seq, 3);
        assert!(snapshot.estimated_size_bytes().expect("size") > 0);
        snapshot.validate().expect("validate snapshot");
    }

    #[test]
    fn inspection_snapshot_envelope_includes_failure_records() {
        let snapshot = snapshot_with_failure_message("exit code 1");
        snapshot.validate().expect("validate snapshot");

        let run = snapshot.runs.first().expect("failed run block");
        assert_eq!(run.run_id, "failed-run");
        assert_eq!(run.failures.len(), 1);
        let failure = &run.failures[0];
        assert_eq!(failure.failure_id, "failure_failed-run_compile_1");
        assert_eq!(failure.step_id, "compile");
        assert_eq!(failure.failure_class, FailureClass::ToolError);
        assert_eq!(failure.error_code, "tool_execution_error");
        assert!(failure.retryable);
        assert!(!failure.safe_to_retry);
    }

    #[test]
    fn inspection_snapshot_envelope_serializes_fleet_syncable_fields_round_trip() {
        let snapshot = snapshot_with_failure_message("exit code 1");
        let json_without = serde_json::to_string(&snapshot).expect("serialize without fields");
        assert!(
            !json_without.contains("trusted_execution_mode"),
            "absent fleet fields must be omitted: {json_without}"
        );

        let mut snapshot_with = snapshot.clone();
        snapshot_with.trusted_execution_mode = Some(true);
        snapshot_with.workflow_digest = Some("deadbeef".to_string());
        snapshot_with.workflow_signature_status = Some("verified".to_string());
        snapshot_with.workflow_signer_key_id = Some("key-1".to_string());
        snapshot_with.trust_decision = Some("allowed".to_string());
        snapshot_with.runtime_id = Some("rt-1".to_string());
        snapshot_with.organization_id = Some("org-example".to_string());
        snapshot_with.policy_revision = Some("rev-1".to_string());
        snapshot_with.plugin_trust = Some(vec![InspectionSnapshotPluginTrust {
            id: "plugin.demo".to_string(),
            version: Some("v1".to_string()),
            digest: "digest-1".to_string(),
            signature_verified: true,
            signer_key_id: Some("key-1".to_string()),
            trust_decision: "verified".to_string(),
        }]);
        snapshot_with.external_transfer_summary = Some(InspectionSnapshotExternalTransferSummary {
            count: 0,
            external_transfers: Vec::new(),
        });
        snapshot_with.approver_identity = Some(vec![InspectionSnapshotApproverIdentity {
            gate_id: "plan-review".to_string(),
            decision: "approved".to_string(),
            actor: Some("alice".to_string()),
            reason: Some("looks good".to_string()),
            decided_at_ms: Some(330),
        }]);

        let json_with = serde_json::to_string(&snapshot_with).expect("serialize with fields");
        assert!(json_with.contains("\"trusted_execution_mode\":true"));
        assert!(json_with.contains("\"workflow_digest\":\"deadbeef\""));
        assert!(json_with.contains("\"workflow_signature_status\":\"verified\""));
        assert!(json_with.contains("\"runtime_id\":\"rt-1\""));
        assert!(json_with.contains("\"organization_id\":\"org-example\""));
        assert!(json_with.contains("\"policy_revision\":\"rev-1\""));
        assert!(json_with.contains("\"plugin_trust\""));
        assert!(json_with.contains("\"external_transfer_summary\""));
        assert!(json_with.contains("\"approver_identity\""));

        let reparsed: InspectionSnapshotEnvelope =
            serde_json::from_str(&json_with).expect("deserialize round trip");
        assert_eq!(reparsed.trusted_execution_mode, Some(true));
        assert_eq!(reparsed.workflow_digest.as_deref(), Some("deadbeef"));
        assert_eq!(reparsed.trust_decision.as_deref(), Some("allowed"));
        assert_eq!(reparsed.organization_id.as_deref(), Some("org-example"));
        assert_eq!(reparsed.runtime_id.as_deref(), Some("rt-1"));
        assert_eq!(reparsed.policy_revision.as_deref(), Some("rev-1"));
        let plugin = reparsed
            .plugin_trust
            .as_ref()
            .and_then(|plugins| plugins.first())
            .expect("plugin trust");
        assert_eq!(plugin.id, "plugin.demo");
        assert_eq!(plugin.digest, "digest-1");
        assert!(plugin.signature_verified);
        let approver = reparsed
            .approver_identity
            .as_ref()
            .and_then(|approvers| approvers.first())
            .expect("approver identity");
        assert_eq!(approver.gate_id, "plan-review");
        assert_eq!(approver.actor.as_deref(), Some("alice"));
        let transfer = reparsed
            .external_transfer_summary
            .as_ref()
            .expect("external transfer summary");
        assert_eq!(transfer.count, 0);
    }

    #[test]
    fn inspection_snapshot_validation_rejects_forbidden_failure_message_tokens() {
        let mut snapshot = snapshot_with_failure_message("exit code 1");
        snapshot.runs[0].failures[0].message_redacted = "adapter returned stderr".to_string();

        let error = snapshot.validate().expect_err("forbidden token rejected");
        let message = error.to_string();
        assert!(message.contains("runs[0].failures[0].message_redacted"));
        assert!(message.contains("stderr"));
    }

    #[test]
    fn snapshot_gate_uses_request_keys_only() {
        let review_decision = InspectionGateDecision {
            state: "approved".into(),
            actor: None,
            reason: None,
            decided_at_ms: Some(1),
            review_request: Some(transist_protocol::GateReviewRequest {
                schema: "transist.gate_review_request.v1".into(),
                run_id: "private-review-run".into(),
                gate_id: "review".into(),
                step_id: "review".into(),
                execution: 1,
                review_documents: Vec::new(),
            }),
        };
        let gate = InspectionGate {
            gate_id: "review".to_string(),
            status: "blocked".to_string(),
            step_id: "review".to_string(),
            created_at_ms: Some(1),
            decision: Some(review_decision.clone()),
            decision_history: vec![review_decision],
            request_summary: BTreeMap::from([
                (
                    "operator_guidance".to_string(),
                    KernelValue::String("secret".to_string()),
                ),
                (
                    "ticket".to_string(),
                    KernelValue::String("TICKET-123".to_string()),
                ),
            ]),
        };

        let snapshot_gate = snapshot_gate_from_gate(gate, &SyncPolicyConfig::default());
        assert_eq!(
            snapshot_gate.request_keys,
            vec!["operator_guidance".to_string(), "ticket".to_string()]
        );
        let gate_json = serde_json::to_string(&snapshot_gate).expect("serialize gate");
        assert!(!gate_json.contains("secret"));
        assert!(!gate_json.contains("TICKET-123"));
        assert!(!gate_json.contains("review_request"));
        assert!(!gate_json.contains("private-review-run"));
    }

    #[test]
    fn allowlisted_event_summary_keeps_only_safe_fields() {
        let filtered = allowlisted_event_summary(
            "step_finished",
            KernelValue::Object(BTreeMap::from([
                (
                    "stdout".to_string(),
                    KernelValue::String("secret".to_string()),
                ),
                ("status".to_string(), KernelValue::String("ok".to_string())),
            ])),
        );

        assert_eq!(
            filtered,
            KernelValue::Object(BTreeMap::from([(
                "status".to_string(),
                KernelValue::String("ok".to_string()),
            )]))
        );
    }

    #[test]
    fn allowlisted_event_summary_flags_unknown_event_types() {
        let filtered = allowlisted_event_summary(
            "custom.event",
            KernelValue::Object(BTreeMap::from([(
                "status".to_string(),
                KernelValue::String("ok".to_string()),
            )])),
        );

        assert_eq!(
            filtered,
            KernelValue::Object(BTreeMap::from([(
                "unknown_event_type".to_string(),
                KernelValue::Bool(true),
            )]))
        );
    }

    /// Anchors the published example fixture to the Rust contract constants.
    ///
    /// The fixture at `examples/inspection-snapshot/v1.example.json` is the
    /// reference payload tooling and external producers validate against the
    /// JSON Schema at `schemas/inspection/snapshot.v1.schema.json`. We do not
    /// strictly deserialize it into `InspectionSnapshotEnvelope` here because
    /// the embedded `run_summary` and `run_detail_summary` blocks are intentionally
    /// minimal in the fixture and would over-constrain external producers.
    /// The check below guarantees the fixture's top-level envelope identity,
    /// schema version, and structural shape stay in lockstep with the
    /// runtime constants.
    #[test]
    fn inspection_snapshot_envelope_example_fixture_matches_contract_constants() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_path = manifest_dir.join("../../examples/inspection-snapshot/v1.example.json");
        let raw = std::fs::read_to_string(&fixture_path)
            .expect("read inspection snapshot example fixture");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("parse inspection snapshot example fixture");
        let validator = inspection_snapshot_schema_validator();
        if let Err(errors) = validator.validate(&value) {
            let messages = errors
                .take(5)
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            panic!(
                "inspection snapshot example fixture failed schema validation: {}",
                messages.join("; ")
            );
        }

        assert_eq!(
            value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("fixture kind"),
            INSPECTION_SNAPSHOT_KIND
        );
        assert_eq!(
            value
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                .expect("fixture schema_version"),
            INSPECTION_SNAPSHOT_SCHEMA_VERSION as u64
        );

        let runs = value
            .get("runs")
            .and_then(serde_json::Value::as_array)
            .expect("fixture runs");
        assert!(
            runs.len() >= 2,
            "example fixture must include a root run plus at least one child run"
        );
        let root_run_id = value
            .get("root_run_id")
            .and_then(serde_json::Value::as_str)
            .expect("fixture root_run_id");
        assert!(
            runs.iter().any(|run| run
                .get("run_id")
                .and_then(serde_json::Value::as_str)
                .map(|id| id == root_run_id)
                .unwrap_or(false)),
            "example fixture root run must appear in runs[]"
        );

        for run in runs {
            for required in [
                "run_id",
                "run_summary",
                "run_detail_summary",
                "failures",
                "run_fingerprint",
                "last_event_seq",
            ] {
                assert!(
                    run.get(required).is_some(),
                    "example fixture run is missing '{required}'"
                );
            }
        }
    }

    #[test]
    fn inspection_snapshot_gate_schema_accepts_v1_gate_fields() {
        let validator = inspection_snapshot_schema_validator();
        let value = serde_json::json!({
            "kind": INSPECTION_SNAPSHOT_KIND,
            "schema_version": INSPECTION_SNAPSHOT_SCHEMA_VERSION,
            "source_id": "src_example",
            "workspace_id": "example/workflows",
            "produced_at_unix_ms": 5_000,
            "tree_version": 7,
            "root_run_id": "root",
            "tree_fingerprint": "0000000000000000000000000000000000000000000000000000000000000000",
            "runs": [{
                "run_id": "root",
                "run_summary": {
                    "run_id": "root",
                    "workflow_id": "root_wf",
                    "workflow_version": "v1",
                    "status": "blocked",
                    "created_at_ms": 100,
                    "updated_at_ms": 360,
                    "current_step_id": "review",
                    "has_blocked_gates": true,
                    "has_artifacts": false,
                    "has_events": false,
                    "artifact_count": 0,
                    "evidence_count": 0,
                    "root_run_id": "root",
                    "parent_run_id": null
                },
                "run_detail_summary": {
                    "summary": {
                        "run_id": "root"
                    },
                    "input_keys": [],
                    "input_types": {},
                    "input_presence": {},
                    "input_sources": {},
                    "blocked_gate_ids": ["plan-review"],
                    "step_count": 1
                },
                "gates": [{
                    "gate_id": "plan-review",
                    "status": "blocked",
                    "step_id": "review",
                    "created_at_ms": 320,
                    "decision": {
                        "state": "request_changes",
                        "reason": "Need a tighter plan.",
                        "decided_at_ms": 330
                    },
                    "decision_history": [{
                        "state": "request_changes",
                        "reason": "Need a tighter plan.",
                        "decided_at_ms": 330
                    }],
                    "request_keys": ["prompt", "ticket"],
                    "request_summary": {
                        "prompt_excerpt": "Refine the migration plan.",
                        "ticket": "TICKET-123"
                    }
                }],
                "failures": [],
                "run_fingerprint": "1111111111111111111111111111111111111111111111111111111111111111",
                "last_event_seq": 0
            }]
        });

        if let Err(errors) = validator.validate(&value) {
            let messages = errors.map(|error| error.to_string()).collect::<Vec<_>>();
            panic!(
                "inspection snapshot gate schema rejected valid v1 gate fields: {}",
                messages.join("; ")
            );
        }
    }

    #[test]
    fn inspection_snapshot_gate_schema_rejects_unknown_gate_keys() {
        let validator = inspection_snapshot_schema_validator();
        let mut value = serde_json::json!({
            "kind": INSPECTION_SNAPSHOT_KIND,
            "schema_version": INSPECTION_SNAPSHOT_SCHEMA_VERSION,
            "source_id": "src_example",
            "workspace_id": "example/workflows",
            "produced_at_unix_ms": 5_000,
            "tree_version": 7,
            "root_run_id": "root",
            "tree_fingerprint": "0000000000000000000000000000000000000000000000000000000000000000",
            "runs": [{
                "run_id": "root",
                "run_summary": {
                    "run_id": "root",
                    "workflow_id": "root_wf",
                    "workflow_version": "v1",
                    "status": "blocked",
                    "created_at_ms": 100,
                    "updated_at_ms": 360,
                    "current_step_id": "review",
                    "has_blocked_gates": true,
                    "has_artifacts": false,
                    "has_events": false,
                    "artifact_count": 0,
                    "evidence_count": 0,
                    "root_run_id": "root",
                    "parent_run_id": null
                },
                "run_detail_summary": {
                    "summary": {
                        "run_id": "root"
                    },
                    "input_keys": [],
                    "input_types": {},
                    "input_presence": {},
                    "input_sources": {},
                    "blocked_gate_ids": ["plan-review"],
                    "step_count": 1
                },
                "gates": [{
                    "gate_id": "plan-review",
                    "status": "blocked",
                    "step_id": "review",
                    "request_keys": ["prompt"]
                }],
                "failures": [],
                "run_fingerprint": "1111111111111111111111111111111111111111111111111111111111111111",
                "last_event_seq": 0
            }]
        });
        value["runs"][0]["gates"][0]["unexpected"] = serde_json::json!(true);

        let errors = validator
            .validate(&value)
            .expect_err("unknown gate key should fail schema validation")
            .collect::<Vec<_>>();
        assert!(
            errors
                .iter()
                .any(|error| error.instance_path.to_string() == "/runs/0/gates/0"),
            "expected gate validation error at /runs/0/gates/0, got {:?}",
            errors
                .iter()
                .map(|error| error.instance_path.to_string())
                .collect::<Vec<_>>()
        );
    }

    fn inspection_snapshot_schema_validator() -> jsonschema::Validator {
        let schema: serde_json::Value = serde_json::from_str(SNAPSHOT_SCHEMA_JSON)
            .expect("parse inspection snapshot v1 schema");
        jsonschema::Validator::new(&schema).expect("compile inspection snapshot v1 schema")
    }

    fn sample_record(
        run_id: &str,
        workflow_id: &str,
        steps: Vec<(&str, KernelStepState)>,
        lineage: Option<RunLineage>,
        created_at_ms: u64,
        updated_at_ms: u64,
        status: KernelRunStatus,
    ) -> PersistedRunRecord {
        let step_ids = steps
            .iter()
            .map(|(step_id, _)| (*step_id).to_string())
            .collect::<Vec<_>>();
        let mut snapshot = KernelRunSnapshot::new(run_id, workflow_id, "v1", &step_ids);
        snapshot.status = status;
        for (step_id, step) in steps {
            snapshot.steps.insert(step_id.to_string(), step);
        }
        PersistedRunRecord::from_snapshot(
            snapshot,
            Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: "abc".to_string(),
            }),
            lineage,
            BTreeMap::new(),
            0,
            created_at_ms,
            updated_at_ms,
        )
    }

    fn snapshot_with_failure_message(message: &str) -> InspectionSnapshotEnvelope {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home_dir = tmp.path();
        let store = LocalRunStore::new(LocalStorageConfig::new(home_dir));
        let summaries = LocalRunSummaryStore::new(home_dir);

        let record = sample_record(
            "failed-run",
            "wf",
            vec![(
                "compile",
                KernelStepState {
                    step_id: "compile".to_string(),
                    status: KernelStepStatus::Failed,
                    executions: 1,
                    retry_wait_until_ms: None,
                    skip_reason: None,
                    last_request: None,
                    last_result: None,
                    started_at_ms: Some(10),
                    finished_at_ms: Some(20),
                    activation_executions_baseline: 0,
                },
            )],
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "failed-run".to_string(),
                parent_run_id: None,
                parent_step_id: None,
            }),
            10,
            20,
            KernelRunStatus::Failed,
        );

        store.create_run(&record).expect("create run");
        store
            .store_failure_record(&sample_failure_record("failed-run", "compile", message))
            .expect("store failure");
        summaries
            .refresh_run("failed-run")
            .expect("refresh summary");

        let inspection = LocalInspectionQuery::new(home_dir, home_dir);
        let metrics = LocalMetricsQuery::new(home_dir);
        let builder = InspectionSnapshotBuilder::new(&inspection, &metrics);
        builder
            .build(&InspectionSnapshotRequest {
                run_id: "failed-run".to_string(),
                source_id: "src_example".to_string(),
                workspace_id: "example/workflows".to_string(),
                operator_label: None,
                produced_at_unix_ms: Some(1_000),
                tree_version: 1,
                event_after_by_run: BTreeMap::new(),
                max_payload_bytes: Some(DEFAULT_INSPECTION_SNAPSHOT_MAX_BYTES),
                sync_policy: None,
                extensions: BTreeMap::new(),
                provider_facts: BTreeMap::new(),
                policy_facts: BTreeMap::new(),
                runtime_id: None,
                organization_id: None,
                policy_revision: None,
                trusted_execution_mode: None,
                workflow_digest: None,
                workflow_signature_status: None,
                workflow_signer_key_id: None,
                plugin_trust: None,
                trust_decision: None,
                external_transfer_summary: None,
                approver_identity: None,
                locality: None,
                evidence_export: None,
            })
            .expect("build snapshot")
            .expect("snapshot")
    }

    fn sample_failure_record(run_id: &str, step_id: &str, message: &str) -> FailureRecord {
        FailureRecord {
            failure_id: format!("failure_{run_id}_{step_id}_1"),
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
            execution_attempt: 1,
            last_stable_step_id: None,
            failure_class: FailureClass::ToolError,
            error_code: "tool_execution_error".to_string(),
            message_redacted: message.to_string(),
            adapter_kind: "process".to_string(),
            adapter_name: Some("shell".to_string()),
            tool_name: Some("exec".to_string()),
            retryable: true,
            safe_to_retry: false,
            requires_manual_attention: false,
            state_mutation_detected: true,
            first_seen_at_unix_ms: 20,
            last_seen_at_unix_ms: 20,
            occurrence_count: 1,
        }
    }

    fn sample_request(
        run_id: &str,
        step_id: &str,
        action: &str,
        plugin_id: Option<&str>,
    ) -> ExecutionRequest {
        ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: format!("invoke_{run_id}_{step_id}"),
            run_id: run_id.to_string(),
            workflow_id: "wf".to_string(),
            step: InvokeStep {
                id: step_id.to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: action.to_string(),
            input: serde_json::json!({"prompt": "hello"}),
            paths: InvokePaths {
                run_root: format!("/tmp/runs/{run_id}"),
                artifacts_dir: format!("/tmp/runs/{run_id}/artifacts"),
                evidence_dir: format!("/tmp/runs/{run_id}/evidence"),
                workspace_dir: "/tmp/workspace".to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: None,
            max_timeout_ms: None,
            policy: serde_json::Value::Null,
            metadata: plugin_id
                .map(|plugin_id| serde_json::json!({"plugin_id": plugin_id}))
                .unwrap_or(serde_json::Value::Null),
        }
    }

    fn sample_usage_result(
        run_id: &str,
        step_id: &str,
        usage: Option<serde_json::Value>,
        output: Option<serde_json::Value>,
    ) -> ExecutionResult {
        serde_json::from_value(serde_json::json!({
            "type": TEP_INVOKE_RESULT_TYPE_V1,
            "protocol_version": TEP_PROTOCOL_VERSION_V1,
            "invoke_id": format!("invoke_{run_id}_{step_id}"),
            "run_id": run_id,
            "step_id": step_id,
            "execution": 1,
            "status": "success",
            "stdout": "",
            "stderr": "",
            "timing": {
                "started_at": "0",
                "finished_at": "1",
                "duration_ms": 25
            },
            "artifacts": [],
            "evidence": [],
            "metrics": {"latency_ms": 25.0},
            "output": output.unwrap_or_else(|| serde_json::json!({})),
            "usage": usage,
            "error": null
        }))
        .expect("result")
    }

    fn step_execution_record(
        run_id: &str,
        step_id: &str,
        request: Option<ExecutionRequest>,
        result: Option<ExecutionResult>,
    ) -> StepExecutionRecord {
        StepExecutionRecord {
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
            execution: 1,
            status: "success".to_string(),
            started_at_ms: Some(100),
            finished_at_ms: Some(350),
            retry_wait_until_ms: None,
            request,
            result,
            metrics_validation_errors: None,
            state_mutations: Vec::new(),
            document_revisions: Vec::new(),
        }
    }

    fn persist_execution(store: &LocalRunStore, record: StepExecutionRecord) {
        store
            .store_step_execution(&RunStoreStepExecution {
                run_id: record.run_id.clone(),
                step_id: record.step_id.clone(),
                execution: record.execution,
                state: serde_json::to_value(record).expect("execution json").into(),
            })
            .expect("persist execution");
    }

    fn runtime_event(
        run_id: &str,
        step_id: Option<&str>,
        kind: &str,
        timestamp_unix_ms: u64,
    ) -> RuntimeEvent {
        RuntimeEvent {
            kind: kind.to_string(),
            run_id: run_id.to_string(),
            step_id: step_id.map(str::to_string),
            timestamp_unix_ms,
            payload: KernelValue::Object(BTreeMap::from([(
                "kind".to_string(),
                KernelValue::String(kind.to_string()),
            )])),
        }
    }

    fn persist_event(sink: &LocalEventSink, summaries: &LocalRunSummaryStore, event: RuntimeEvent) {
        sink.publish(&event).expect("publish event");
        summaries.note_event(&event).expect("note event");
    }
}
