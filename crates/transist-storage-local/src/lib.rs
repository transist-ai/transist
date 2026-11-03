// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Local filesystem-backed persistence and inspection helpers for Transist.
//!
//! This crate owns local artifact storage, durable run persistence, lease and
//! event helpers, secret lookup, and inspection-oriented indexing over the
//! on-disk Transist home. It may depend on `transist-core` and
//! `transist-protocol`, but higher-level crates must not depend on its storage
//! layout semantics.
//!
//! The documented store/query types exported from the crate root form the intended
//! stable surface. The `run_paths` module is the explicit local-layout surface
//! used by local inspection and operator tooling.

/// Local storage crate version.
pub const TRANSIST_STORAGE_LOCAL_VERSION: &str = env!("CARGO_PKG_VERSION");

mod active_record_store;
mod artifact_store;
mod atomic_io;
mod clock;
mod control_request_store;
mod defaults;
mod document_store;
mod event_sink;
pub mod fault_injection;
mod ids;
pub mod inspection;
mod installation;
pub mod invocation_staging;
mod lease_store;
mod query;
pub mod run_paths;
mod run_store;
mod run_summary;
mod secrets;
mod trust_store;
mod util;

pub use active_record_store::LocalActiveRecordStore;
pub use artifact_store::LocalArtifactStore;
pub use atomic_io::{
    AnchoredDir, DirectoryLock, create_dir_all_durable, lock_directory,
    read_regular_file_no_follow, remove_file_anchored, sync_directory,
};
pub use clock::LocalClock;
pub use control_request_store::LocalControlRequestStore;
pub use defaults::{DEFAULT_HOME_CONFIG_YAML, DEFAULT_REPO_CONFIG_YAML};
pub use event_sink::LocalEventSink;
pub use ids::LocalIdGenerator;
pub use inspection::{
    BlobDescriptor, BlobFamily, INSPECTION_CACHE_DISABLE_ENV, IndexedBlob, IndexedRuntimeEvent,
    InspectionCache, InspectionCacheKey, InspectionCacheStats, LocalInspectionStore,
};
pub use installation::{Installation, installation_hostname_hash};
pub use lease_store::{
    DEFAULT_HEARTBEAT_INTERVAL_MS, DEFAULT_STALE_AFTER_MS, LocalLeaseStore, REAPER_DISABLE_ENV,
    REAPER_STALE_AFTER_MS_ENV, ReaperConfig, reaper_config_from_env, shared_lease_store,
};
pub use query::LocalRunQueryService;
pub use run_paths::*;
pub use run_store::{LocalRunStore, LocalStorageConfig};
pub use run_summary::{LocalRunSummaryStore, PersistedRunSummaryRecord};
pub use secrets::EnvSecretProvider;
pub use trust_store::{LocalTrustStore, trust_store_dir};

/// Atomically rewrites a JSON file in place using a temp file and same-directory rename.
pub fn write_json_atomic(
    path: &std::path::Path,
    value: &impl serde::Serialize,
) -> anyhow::Result<()> {
    util::write_json(path, value)
}

/// Atomically creates a brand-new JSON file and fails if the target already exists.
pub fn write_json_atomic_create_new(
    path: &std::path::Path,
    value: &impl serde::Serialize,
) -> anyhow::Result<()> {
    util::write_json_create_new(path, value)
}

/// Atomically rewrites a UTF-8 text file in place using a temp file and same-directory rename.
pub fn write_string_atomic(path: &std::path::Path, value: &str) -> anyhow::Result<()> {
    util::write_string_atomic(path, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use transist_core::ports::{
        ArtifactPort, ArtifactReadRequest, ArtifactWriteRequest, ControlRequestPort, EventPort,
        ExternalOwnerMetadata, LeasePort, ManagementMode, RunStorePort, RuntimeOwnerIdentity,
    };
    use transist_core::{
        KernelRunSnapshot, KernelRunStatus, KernelStepState, KernelStepStatus, PersistedRunRecord,
        RecipeSnapshotRef,
    };
    use transist_core::{RunLineage, RunLineageMode};
    use transist_protocol::{
        AdapterKind, KernelValue, Recipe, RecipeStep, RuntimeEvent, StepExecutionDescriptor,
        WorkflowRef,
    };

    fn sample_record(run_id: &str) -> PersistedRunRecord {
        PersistedRunRecord::from_snapshot(
            KernelRunSnapshot::new(run_id, "wf", "v1", &["step".to_string()]),
            Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: "abc".to_string(),
            }),
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: run_id.to_string(),
                parent_run_id: None,
                parent_step_id: None,
            }),
            BTreeMap::new(),
            0,
            1,
            1,
        )
    }

    #[test]
    fn run_store_persists_state_and_query_reads_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = LocalStorageConfig::new(tmp.path());
        let store = LocalRunStore::new(cfg.clone());
        let mut record = sample_record("1");
        record.status = KernelRunStatus::Success;
        store.create_run(&record).expect("create");
        let query = LocalRunQueryService::new(tmp.path());
        let listed = query.list_runs().expect("list");
        assert_eq!(listed, vec!["1".to_string()]);
        let show = query.show_run("1").expect("show");
        assert!(matches!(show, KernelValue::Object(_)));
    }

    #[test]
    fn run_store_gate_and_provenance_ports_preserve_bytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        store.create_run(&sample_record("1")).expect("create");

        let gate_path = store
            .gate_decision_path(tmp.path(), "1", "review")
            .expect("gate path");
        std::fs::create_dir_all(gate_path.parent().expect("gate parent")).expect("gate dir");
        let gate_bytes = br#"{"decision":"approved"}"#;
        std::fs::write(&gate_path, gate_bytes).expect("write gate");
        assert_eq!(
            store
                .load_gate_decision(tmp.path(), "1", "review")
                .expect("load gate"),
            Some(gate_bytes.to_vec())
        );
        store
            .consume_gate_decision(tmp.path(), "1", "review")
            .expect("consume gate");
        assert!(!gate_path.exists());

        let provenance_bytes = b"provenance bytes preserved exactly";
        store
            .write_run_provenance(tmp.path(), "1", provenance_bytes)
            .expect("write provenance");
        assert_eq!(
            store
                .read_run_provenance(tmp.path(), "1")
                .expect("read provenance"),
            Some(provenance_bytes.to_vec())
        );
    }

    #[test]
    fn run_store_invocation_paths_and_result_file_bytes_are_exact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));

        let paths = store
            .resolve_invocation_paths(tmp.path(), std::path::Path::new("/repo"), "1", "step", 7)
            .expect("invocation paths");
        assert_eq!(
            paths.run_root,
            tmp.path().join("runs/run_1").display().to_string()
        );
        assert_eq!(
            paths.artifacts_dir,
            tmp.path()
                .join("runs/run_1/artifacts/step/execution_007/artifacts")
                .display()
                .to_string()
        );
        assert_eq!(
            paths.evidence_dir,
            tmp.path()
                .join("runs/run_1/artifacts/step/execution_007/evidence")
                .display()
                .to_string()
        );
        assert_eq!(paths.workspace_dir, "/repo");

        let artifacts_dir = std::path::Path::new(&paths.artifacts_dir);
        fs::create_dir_all(artifacts_dir).expect("artifacts dir");
        let result_path = artifacts_dir.join("result.bin");
        let result_bytes = [0, 0xff, b'\n'];
        fs::write(&result_path, result_bytes).expect("result file");
        assert_eq!(
            store.read_result_file(&result_path).expect("read result"),
            result_bytes
        );
        assert!(store.result_file_is_within(&result_path, artifacts_dir));
        let outside_path = tmp.path().join("outside.bin");
        fs::write(&outside_path, b"outside").expect("outside file");
        assert!(!store.result_file_is_within(&outside_path, artifacts_dir));
    }

    #[test]
    fn gate_decision_history_survives_active_record_consumption() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        store.create_run(&sample_record("1")).expect("create");
        let record = transist_protocol::GateDecisionRecord {
            review_request: None,
            schema: "transist.gate_decision.v1".to_string(),
            run_id: "1".to_string(),
            gate_id: "review".to_string(),
            step_id: "gate".to_string(),
            decision: "approved".to_string(),
            actor: Some("alice".to_string()),
            actor_source: None,
            reason: Some("looks good".to_string()),
            decided_at_ms: Some(42),
        };

        store
            .store_gate_decision(tmp.path(), &record)
            .expect("store gate decision");

        assert!(
            store
                .gate_decision_path(tmp.path(), "1", "review")
                .expect("active path")
                .is_file()
        );
        assert_eq!(
            std::fs::read_dir(tmp.path().join("runs/run_1/gates/history"))
                .expect("history dir")
                .count(),
            1
        );
        store
            .consume_gate_decision(tmp.path(), "1", "review")
            .expect("consume active decision");
        let history = store
            .load_gate_decision_history(tmp.path(), "1", "review")
            .expect("load durable history");
        assert_eq!(history.len(), 1);
        assert_eq!(
            serde_json::from_slice::<transist_protocol::GateDecisionRecord>(&history[0])
                .expect("history record"),
            record
        );
    }

    #[test]
    fn gate_decision_history_uses_exact_gate_id_prefix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        store.create_run(&sample_record("1")).expect("create");
        for (gate_id, decided_at_ms) in [("review", 42), ("review-security", 43)] {
            store
                .store_gate_decision(
                    tmp.path(),
                    &transist_protocol::GateDecisionRecord {
                        review_request: None,
                        schema: "transist.gate_decision.v1".to_string(),
                        run_id: "1".to_string(),
                        gate_id: gate_id.to_string(),
                        step_id: gate_id.to_string(),
                        decision: "approved".to_string(),
                        actor: None,
                        actor_source: None,
                        reason: None,
                        decided_at_ms: Some(decided_at_ms),
                    },
                )
                .expect("store gate decision");
        }
        fs::write(
            tmp.path()
                .join("runs/run_1/gates/history/review-security-corrupt.json"),
            br#"{"gate_id":"review-security"}"#,
        )
        .expect("malformed other-gate history entry");

        let history = store
            .load_gate_decision_history(tmp.path(), "1", "review")
            .expect("load exact history");
        assert_eq!(history.len(), 1);
        let record = serde_json::from_slice::<transist_protocol::GateDecisionRecord>(&history[0])
            .expect("history record");
        assert_eq!(record.gate_id, "review");
        assert!(
            store
                .load_gate_decision_history(tmp.path(), "1", "review-security")
                .is_err(),
            "the malformed entry must fail closed for its own gate"
        );
    }

    #[test]
    fn gate_decision_history_rejects_malformed_matching_filename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        store.create_run(&sample_record("1")).expect("create");
        let history_dir = tmp.path().join("runs/run_1/gates/history");
        fs::create_dir_all(&history_dir).expect("history dir");
        fs::write(history_dir.join("review-corrupt.json"), b"{}").expect("malformed history entry");

        let error = store
            .load_gate_decision_history(tmp.path(), "1", "review")
            .expect_err("malformed matching filename must fail closed");
        assert!(
            error
                .to_string()
                .contains("malformed gate decision history filename")
        );
    }

    #[test]
    fn atomic_write_syncs_parent_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("nested").join("state.json");
        write_json_atomic(&target, &serde_json::json!({"ok": true})).expect("write json");
        assert!(target.exists());
    }

    #[test]
    fn artifact_store_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = LocalStorageConfig::new(tmp.path());
        let store = LocalRunStore::new(cfg.clone());
        let artifacts = LocalArtifactStore::new(cfg);
        store
            .create_run(&sample_record("run_1"))
            .expect("create run");
        artifacts
            .write_artifact(&ArtifactWriteRequest {
                run_id: "run_1".to_string(),
                step_id: "s1".to_string(),
                execution: 2,
                logical_path: "artifacts/out.json".to_string(),
                payload: KernelValue::String("ok".to_string()),
            })
            .expect("write");
        let value = artifacts
            .read_artifact(&ArtifactReadRequest {
                run_id: "run_1".to_string(),
                step_id: "s1".to_string(),
                execution: 2,
                logical_path: "artifacts/out.json".to_string(),
            })
            .expect("read");
        assert_eq!(value, KernelValue::String("ok".to_string()));
    }

    #[test]
    fn lease_rejects_other_owner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let owner = RuntimeOwnerIdentity {
            management_mode: ManagementMode::Standalone,
            owner_id: "owner-a".to_string(),
            lease_id: "lease-a".to_string(),
            pid: 1,
            hostname: "host-a".to_string(),
            command: "transist runs start".to_string(),
            transist_version: "test".to_string(),
            external: None,
            runtime_id: None,
            organization_id: None,
        };
        let leases = LocalLeaseStore::new(tmp.path(), owner);
        let _ = leases.acquire_root_run_tree_lease("run_1").expect("lease");
        let competing = LocalLeaseStore::new(
            tmp.path(),
            RuntimeOwnerIdentity {
                management_mode: ManagementMode::Standalone,
                owner_id: "owner-b".to_string(),
                lease_id: "lease-b".to_string(),
                pid: 2,
                hostname: "host-b".to_string(),
                command: "transist runs start".to_string(),
                transist_version: "test".to_string(),
                external: None,
                runtime_id: None,
                organization_id: None,
            },
        );
        let err = competing
            .acquire_root_run_tree_lease("run_1")
            .expect_err("must fail");
        assert!(err.to_string().contains("already leased"));
    }

    #[test]
    fn runtime_owner_identity_reserves_fleet_identifiers() {
        let owner = RuntimeOwnerIdentity {
            management_mode: ManagementMode::Standalone,
            owner_id: "owner-a".to_string(),
            lease_id: "lease-a".to_string(),
            pid: 1,
            hostname: "host-a".to_string(),
            command: "transist runs start".to_string(),
            transist_version: "test".to_string(),
            external: None,
            runtime_id: Some("rt-issued-1".to_string()),
            organization_id: Some("org-example".to_string()),
        };
        let json = serde_json::to_string(&owner).expect("serialize owner");
        assert!(
            json.contains("\"runtime_id\":\"rt-issued-1\""),
            "runtime_id must serialize: {json}"
        );
        assert!(
            json.contains("\"organization_id\":\"org-example\""),
            "organization_id must serialize: {json}"
        );

        let unreserved = RuntimeOwnerIdentity {
            runtime_id: None,
            organization_id: None,
            ..owner
        };
        let unreserved_json = serde_json::to_string(&unreserved).expect("serialize unreserved");
        assert!(
            !unreserved_json.contains("runtime_id"),
            "runtime_id must be omitted when None: {unreserved_json}"
        );
        assert!(
            !unreserved_json.contains("organization_id"),
            "organization_id must be omitted when None: {unreserved_json}"
        );

        let parsed: RuntimeOwnerIdentity =
            serde_json::from_str(&unreserved_json).expect("deserialize unreserved");
        assert!(parsed.runtime_id.is_none());
        assert!(parsed.organization_id.is_none());
    }

    #[test]
    fn lease_inspect_reports_external_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let owner = RuntimeOwnerIdentity {
            management_mode: ManagementMode::Managed,
            owner_id: "managed-owner".to_string(),
            lease_id: "managed-lease".to_string(),
            pid: std::process::id(),
            hostname: "host-a".to_string(),
            command: "transist workflows run managed-run".to_string(),
            transist_version: "test".to_string(),
            external: Some(ExternalOwnerMetadata {
                host_id: "host-a".to_string(),
                runner_id: Some("runner-1".to_string()),
                external_run_id: Some("run-ext-1".to_string()),
            }),
            runtime_id: None,
            organization_id: None,
        };
        let leases = LocalLeaseStore::new(tmp.path(), owner.clone());
        let handle = leases
            .acquire_root_run_tree_lease("managed-root")
            .expect("lease");

        let record = leases
            .inspect_root_run_tree_lease("managed-root")
            .expect("inspect")
            .expect("record");
        assert_eq!(record.owner_id, owner.owner_id);
        assert_eq!(record.management_mode, ManagementMode::Managed);
        let external = record.external.expect("external metadata");
        assert_eq!(external.host_id, "host-a");
        assert_eq!(external.runner_id.as_deref(), Some("runner-1"));
        assert_eq!(external.external_run_id.as_deref(), Some("run-ext-1"));

        leases
            .release_root_run_tree_lease(&handle)
            .expect("release lease");
        assert!(
            leases
                .inspect_root_run_tree_lease("managed-root")
                .expect("inspect after release")
                .is_none()
        );
    }

    #[test]
    fn control_request_store_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let control = LocalControlRequestStore::new(
            tmp.path(),
            Some(RuntimeOwnerIdentity {
                management_mode: ManagementMode::Standalone,
                owner_id: "owner-a".to_string(),
                lease_id: "lease-a".to_string(),
                pid: std::process::id(),
                hostname: "host-a".to_string(),
                command: "transist runs start demo".to_string(),
                transist_version: "test".to_string(),
                external: None,
                runtime_id: None,
                organization_id: None,
            }),
        );

        let first = control.request_cancel("root-run").expect("first request");
        let second = control.request_cancel("root-run").expect("second request");
        assert_eq!(first.request_id, second.request_id);
        let inspected = control
            .inspect_cancel_request("root-run")
            .expect("inspect")
            .expect("record");
        assert_eq!(inspected.request_id, first.request_id);
        let requester = inspected.requester.expect("requester metadata");
        assert_eq!(requester.owner_id, "owner-a");
    }

    #[test]
    fn control_request_clear_removes_marker_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let control = LocalControlRequestStore::new(
            tmp.path(),
            Some(RuntimeOwnerIdentity {
                management_mode: ManagementMode::Standalone,
                owner_id: "owner-clear".to_string(),
                lease_id: "lease-clear".to_string(),
                pid: std::process::id(),
                hostname: "host-clear".to_string(),
                command: "transist runs start demo".to_string(),
                transist_version: "test".to_string(),
                external: None,
                runtime_id: None,
                organization_id: None,
            }),
        );

        // Clearing when no marker is present is a no-op (e.g. fresh launch
        // path on a clean control directory).
        control
            .clear_cancel_request("clean-root")
            .expect("clear with no marker");

        // After requesting + clearing, inspect must observe the marker as
        // gone so the kernel's pre-loop cancel check returns false.
        control.request_cancel("dirty-root").expect("plant marker");
        assert!(
            control
                .inspect_cancel_request("dirty-root")
                .expect("inspect")
                .is_some()
        );
        control
            .clear_cancel_request("dirty-root")
            .expect("clear after request");
        assert!(
            control
                .inspect_cancel_request("dirty-root")
                .expect("inspect after clear")
                .is_none()
        );

        // Calling clear again must not error (idempotent).
        control
            .clear_cancel_request("dirty-root")
            .expect("clear is idempotent");
    }

    #[test]
    fn event_sink_and_tail_query_work() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = LocalStorageConfig::new(tmp.path());
        let store = LocalRunStore::new(cfg.clone());
        let events = LocalEventSink::new(cfg);
        store
            .create_run(&sample_record("run_1"))
            .expect("create run");
        events
            .publish(&RuntimeEvent {
                kind: "run_started".to_string(),
                run_id: "run_1".to_string(),
                step_id: None,
                timestamp_unix_ms: 1,
                payload: KernelValue::Null,
            })
            .expect("publish");
        let query = LocalRunQueryService::new(tmp.path());
        let tailed = query.tail_events("run_1", 10).expect("tail");
        assert_eq!(tailed.len(), 1);
        assert_eq!(tailed[0].kind, "run_started");
    }

    #[test]
    fn run_summary_refresh_writes_summary_sidecar_with_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = LocalStorageConfig::new(tmp.path());
        let store = LocalRunStore::new(cfg);
        let summaries = LocalRunSummaryStore::new(tmp.path());
        let mut snapshot = KernelRunSnapshot::new(
            "summary-run",
            "wf",
            "v1",
            &["collect".to_string(), "review".to_string()],
        );
        snapshot.status = KernelRunStatus::Blocked;
        snapshot.steps.insert(
            "collect".to_string(),
            KernelStepState {
                step_id: "collect".to_string(),
                status: KernelStepStatus::Succeeded,
                executions: 1,
                retry_wait_until_ms: None,
                skip_reason: None,
                last_request: None,
                last_result: None,
                started_at_ms: Some(10),
                finished_at_ms: Some(20),
                activation_executions_baseline: 0,
            },
        );
        snapshot.steps.insert(
            "review".to_string(),
            KernelStepState {
                step_id: "review".to_string(),
                status: KernelStepStatus::Blocked,
                executions: 1,
                retry_wait_until_ms: None,
                skip_reason: None,
                last_request: None,
                last_result: None,
                started_at_ms: Some(21),
                finished_at_ms: None,
                activation_executions_baseline: 0,
            },
        );
        let record = PersistedRunRecord::from_snapshot(
            snapshot,
            Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: "recipe-sha".to_string(),
            }),
            Some(RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: "summary-run".to_string(),
                parent_run_id: None,
                parent_step_id: None,
            }),
            BTreeMap::new(),
            0,
            1,
            22,
        );
        store.create_run(&record).expect("create run");
        let recipe = Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: "wf".to_string(),
                version: "v1".to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps: vec![
                RecipeStep {
                    id: "collect".to_string(),
                    execution: StepExecutionDescriptor::Invoke {
                        adapter: AdapterKind::Process,
                        plugin_id: Some("local".to_string()),
                        plugin: "local".to_string(),
                        action: "exec".to_string(),
                        execution_kind: "external".to_string(),
                        plugin_version: None,
                        plugin_sha256: None,
                    },
                    with: BTreeMap::new(),
                    prompt_binding: None,
                    skills_binding: None,
                    vars: BTreeMap::new(),
                    filesystem_policy: Default::default(),
                    network_policy: Default::default(),
                    effective_filesystem_policy: Default::default(),
                    effective_filesystem_policy_source: None,
                    effective_network_policy: Default::default(),
                    effective_network_policy_source: None,
                },
                RecipeStep {
                    id: "review".to_string(),
                    execution: StepExecutionDescriptor::Control {
                        primitive: "control.gate.manual".to_string(),
                    },
                    with: BTreeMap::new(),
                    prompt_binding: None,
                    skills_binding: None,
                    vars: BTreeMap::new(),
                    filesystem_policy: Default::default(),
                    network_policy: Default::default(),
                    effective_filesystem_policy: Default::default(),
                    effective_filesystem_policy_source: None,
                    effective_network_policy: Default::default(),
                    effective_network_policy_source: None,
                },
            ],
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        };
        store
            .write_recipe_snapshot(
                "summary-run",
                &yaml_serde::to_string(&recipe).expect("recipe yaml"),
            )
            .expect("recipe snapshot");
        let run_root = run_paths::resolve_run_root(tmp.path(), "summary-run").expect("run root");
        fs::create_dir_all(run_root.join("artifacts/collect/execution_001/artifacts"))
            .expect("artifact dir");
        fs::create_dir_all(run_root.join("artifacts/collect/execution_001/evidence"))
            .expect("evidence dir");
        fs::write(
            run_root.join("artifacts/collect/execution_001/artifacts/000_output.txt"),
            b"artifact",
        )
        .expect("artifact bytes");
        fs::write(
            run_root.join("artifacts/collect/execution_001/evidence/000_trace.log"),
            b"trace",
        )
        .expect("evidence bytes");

        let summary = summaries
            .refresh_run("summary-run")
            .expect("refresh summary");
        assert_eq!(summary.run_id, "summary-run");
        assert_eq!(summary.artifact_count, 1);
        assert_eq!(summary.evidence_count, 1);
        assert!(summary.has_artifacts);
        assert!(summary.has_blocked_gates);
        assert_eq!(summary.manual_gate_step_ids, vec!["review".to_string()]);
        assert!(run_root.join("summary.json").exists());
    }

    #[test]
    fn run_summary_note_event_updates_event_counters() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = LocalStorageConfig::new(tmp.path());
        let store = LocalRunStore::new(cfg);
        let summaries = LocalRunSummaryStore::new(tmp.path());
        let record = sample_record("summary-events");
        store.create_run(&record).expect("create run");
        summaries
            .refresh_run("summary-events")
            .expect("refresh summary");

        summaries
            .note_event(&RuntimeEvent {
                kind: "run_started".to_string(),
                run_id: "summary-events".to_string(),
                step_id: None,
                timestamp_unix_ms: 25,
                payload: KernelValue::Null,
            })
            .expect("note event");
        let summary = summaries
            .load_run_summary("summary-events")
            .expect("load summary")
            .expect("summary exists");
        assert_eq!(summary.event_count, 1);
        assert!(summary.has_events);
        assert_eq!(summary.updated_at_ms, 25);
    }

    #[test]
    fn reaper_only_removes_dead_local_orphans() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hostname = "host-reap".to_string();
        // PID 1 is reserved for init on Unix and is reliably alive; use it as
        // the "live owner" sentinel.
        let live_pid: u32 = 1;
        // Spawn and reap a short-lived child to obtain a PID that is
        // guaranteed not to exist in the kernel's process table by the time
        // the reaper runs.
        let mut probe = std::process::Command::new("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn probe");
        let dead_pid: u32 = probe.id();
        probe.wait().expect("reap probe");

        let owner = RuntimeOwnerIdentity {
            management_mode: ManagementMode::Standalone,
            owner_id: "reaper-self".to_string(),
            lease_id: "reaper-lease".to_string(),
            pid: std::process::id(),
            hostname: hostname.clone(),
            command: "transist test".to_string(),
            transist_version: "test".to_string(),
            external: None,
            runtime_id: None,
            organization_id: None,
        };
        let leases = LocalLeaseStore::new(tmp.path(), owner);

        // Acquire one lease this store currently owns; it must never be reaped.
        let _own_handle = leases
            .acquire_root_run_tree_lease("run_self_owned")
            .expect("acquire own lease");

        let lock_dir = tmp.path().join("locks").join("runs");
        fs::create_dir_all(&lock_dir).expect("locks dir");
        let now_ms = crate::util::now_unix_ms();
        let stale_ts = now_ms.saturating_sub(120_000);
        let fresh_ts = now_ms;

        let foreign = |run_id: &str, pid: u32, host: &str, heartbeat_ms: u64| {
            let record = transist_core::ports::RunTreeLeaseRecord {
                schema_version: transist_core::ports::RunTreeLeaseRecord::SCHEMA_VERSION
                    .to_string(),
                root_run_id: run_id.to_string(),
                lease_id: format!("foreign-{run_id}"),
                owner_id: format!("foreign-owner-{run_id}"),
                management_mode: ManagementMode::Standalone,
                pid,
                hostname: host.to_string(),
                command: "ghost".to_string(),
                transist_version: "test".to_string(),
                acquired_at_ms: heartbeat_ms,
                last_heartbeat_ms: heartbeat_ms,
                external: None,
            };
            let path = lock_dir.join(format!("{run_id}.lock"));
            crate::util::write_json(&path, &record).expect("write lease");
            path
        };

        let path_dead_stale = foreign("run_dead_stale", dead_pid, &hostname, stale_ts);
        let path_dead_fresh = foreign("run_dead_fresh", dead_pid, &hostname, fresh_ts);
        let path_live_stale = foreign("run_live_stale", live_pid, &hostname, stale_ts);
        let path_other_host = foreign("run_other_host", dead_pid, "other-host", stale_ts);

        let reaped = leases
            .release_orphan_leases(ReaperConfig::default())
            .expect("reap");
        let reaped_ids: Vec<_> = reaped.iter().map(|r| r.root_run_id.clone()).collect();

        assert_eq!(reaped_ids, vec!["run_dead_stale".to_string()]);
        assert!(!path_dead_stale.exists(), "dead+stale must be removed");
        assert!(
            path_dead_fresh.exists(),
            "dead+fresh must be kept (within heartbeat window)"
        );
        assert!(path_live_stale.exists(), "live PID must never be reaped");
        assert!(
            path_other_host.exists(),
            "foreign-host leases must never be reaped"
        );
        assert!(
            tmp.path().join("locks/runs/run_self_owned.lock").exists(),
            "self-owned lease must never be reaped"
        );

        // Disabled reaper is a no-op even when stale entries are present.
        let dead_stale_again = foreign("run_dead_stale_again", dead_pid, &hostname, stale_ts);
        let reaped = leases
            .release_orphan_leases(ReaperConfig {
                disabled: true,
                ..Default::default()
            })
            .expect("disabled reap");
        assert!(reaped.is_empty());
        assert!(dead_stale_again.exists());
    }
}
