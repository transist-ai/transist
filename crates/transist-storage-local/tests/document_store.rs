// SPDX-License-Identifier: MPL-2.0
//! Focused publication/recovery and hostile filesystem tests for managed documents.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use transist_core::ports::{
    DocumentOutput, DocumentStorePort, RunBirthArtifacts, RunStorePort, RunStoreStepExecution,
};
use transist_core::{
    KernelRunSnapshot, KernelStepStatus, PersistedRunRecord, Redactor, StepExecutionRecord,
};
use transist_protocol::{DocumentTreeManifest, ExecutionResult};
use transist_storage_local::{LocalRunStore, LocalStorageConfig, fault_injection::*};

#[path = "document_store_review4/mod.rs"]
mod review4;

const DECLARATIONS: &str = "documents:\n  a: {kind: file, max_bytes: 32}\n  b: {kind: file, max_bytes: 32}\n  seed: {kind: file, max_bytes: 32, initial: 'seed bytes'}\n  tree: {kind: tree, max_bytes: 32, max_files: 3}\n";

fn record(run: &str) -> PersistedRunRecord {
    PersistedRunRecord::from_snapshot(
        KernelRunSnapshot::new(run, "demo", "0.1.0", &["produce".into()]),
        None,
        None,
        BTreeMap::new(),
        0,
        1,
        1,
    )
}

fn birth(store: &LocalRunStore, home: &Path, run: &str, child: Option<&str>) {
    let mut record = record(run);
    if let Some(parent) = child {
        record.lineage = Some(transist_core::RunLineage {
            mode: transist_core::RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: parent.into(),
            parent_run_id: Some(parent.into()),
            parent_step_id: Some("module".into()),
        });
    }
    let yaml = format!("schema: transist.recipe.v1\n{DECLARATIONS}");
    store
        .create_run_staged(&RunBirthArtifacts {
            record: &record,
            recipe_yaml: &yaml,
            run_inputs_json: b"{}",
            provenance_json: b"{}",
            workflow_asset_manifest_json: None,
            workflow_asset_blobs: &[],
        })
        .unwrap();
    store.publish_run_locator(home, &record).unwrap();
}

fn active(store: &LocalRunStore, run: &str, ordinal: u32) {
    let mut record = store.load_run(run).unwrap().unwrap();
    let step = record.steps.get_mut("produce").unwrap();
    step.executions = ordinal;
    step.status = KernelStepStatus::Running;
    store.update_run_state(run, &record).unwrap();
}

fn result(run: &str, ordinal: u32, status: &str) -> ExecutionResult {
    ExecutionResult {
        protocol_version: "tep.v1".into(),
        message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.into(),
        invoke_id: format!("{run}-{ordinal}"),
        run_id: run.into(),
        step_id: "produce".into(),
        execution: ordinal,
        status: status.into(),
        exit_code: Some(0),
        output: serde_json::json!({}),
        stdout: String::new(),
        stderr: String::new(),
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        metrics_validation_errors: 0,
        usage: None,
        error: None,
        timing: transist_protocol::InvokeTiming {
            started_at: "2026-10-14T00:00:00Z".into(),
            finished_at: "2026-10-14T00:00:01Z".into(),
            duration_ms: 1000,
        },
    }
}

fn completion(store: &LocalRunStore, run: &str, ordinal: u32, status: &str) -> StepExecutionRecord {
    StepExecutionRecord {
        run_id: run.into(),
        step_id: "produce".into(),
        execution: ordinal,
        status: status.into(),
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        retry_wait_until_ms: None,
        request: None,
        result: Some(result(run, ordinal, status)),
        metrics_validation_errors: None,
        state_mutations: Vec::new(),
        document_revisions: if status == "success" {
            store
                .prepared_document_revisions(run, "produce", ordinal)
                .unwrap()
        } else {
            Vec::new()
        },
    }
}

fn commit(store: &LocalRunStore, completion: &StepExecutionRecord) -> anyhow::Result<()> {
    store.store_step_execution(&RunStoreStepExecution {
        run_id: completion.run_id.clone(),
        step_id: completion.step_id.clone(),
        execution: completion.execution,
        state: serde_json::to_value(completion)?.into(),
    })
}

fn outputs(root: &Path) -> Vec<DocumentOutput> {
    ["a", "b"]
        .into_iter()
        .map(|name| {
            let path = root.join(name);
            fs::write(&path, format!("{name} bytes")).unwrap();
            DocumentOutput {
                document: name.into(),
                path,
            }
        })
        .collect()
}

#[test]
fn document_store_crash_windows_expose_all_committed_outputs_or_none() {
    for point in [
        FAULT_DOCUMENT_BEFORE_BLOBS,
        FAULT_DOCUMENT_AFTER_BLOBS,
        FAULT_DOCUMENT_BEFORE_COMMIT,
        FAULT_DOCUMENT_AFTER_COMMIT,
        FAULT_DOCUMENT_BEFORE_HEADS,
        FAULT_DOCUMENT_AFTER_HEADS,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        birth(&store, tmp.path(), "run", None);
        active(&store, "run", 1);
        let outputs = outputs(tmp.path());
        set(point);
        let capture =
            store.prepare_document_revisions("run", "produce", 1, &outputs, &Redactor::default());
        let operation = match capture {
            Ok(_) => commit(&store, &completion(&store, "run", 1, "success")),
            Err(error) => Err(error),
        };
        clear();
        assert!(operation.is_err(), "fault {point} was not exercised");
        let restarted = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        let heads = restarted.document_heads("run").unwrap();
        let committed = point >= FAULT_DOCUMENT_AFTER_COMMIT;
        assert_eq!(heads.contains_key("a"), committed, "fault {point}");
        assert_eq!(heads.contains_key("b"), committed, "fault {point}");
        assert_eq!(heads.len(), if committed { 3 } else { 1 });
        restarted.reconcile_document_heads("run").unwrap();
        let cached: BTreeMap<String, transist_protocol::DocumentRevision> = serde_json::from_slice(
            &fs::read(tmp.path().join("runs/run_run/documents/heads.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cached, heads);
        if committed {
            assert_eq!(
                restarted.read_document_blob("run", &heads["a"]).unwrap(),
                b"a bytes"
            );
            assert!(commit(&restarted, &completion(&restarted, "run", 1, "success")).is_err());
        }
    }
}

#[test]
fn document_store_initialization_is_atomic_with_root_birth() {
    for point in [FAULT_RUN_BIRTH_BEFORE_RENAME, FAULT_RUN_BIRTH_AFTER_RENAME] {
        let tmp = tempfile::tempdir().unwrap();
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        let record = record("birth");
        let yaml = format!("schema: transist.recipe.v1\n{DECLARATIONS}");
        set(point);
        assert!(
            store
                .create_run_staged(&RunBirthArtifacts {
                    record: &record,
                    recipe_yaml: &yaml,
                    run_inputs_json: b"{}",
                    provenance_json: b"{}",
                    workflow_asset_manifest_json: None,
                    workflow_asset_blobs: &[],
                })
                .is_err()
        );
        clear();
        assert!(store.load_run("birth").unwrap().is_none());
        birth(&store, tmp.path(), "birth", None);
        let seed = store
            .document_heads("birth")
            .unwrap()
            .remove("seed")
            .unwrap();
        assert_eq!(seed.execution, 0);
        assert_eq!(seed.commit_sequence, 0);
        assert_eq!(
            store.read_document_blob("birth", &seed).unwrap(),
            b"seed bytes"
        );
    }
}

#[test]
fn document_store_partial_blob_persistence_is_unpublished_and_reconcilable() {
    for occurrence in [1, 2] {
        let tmp = tempfile::tempdir().unwrap();
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        birth(&store, tmp.path(), "run", None);
        active(&store, "run", 1);
        let outputs = outputs(tmp.path());
        set_occurrence(FAULT_DOCUMENT_AFTER_BLOB, occurrence);
        assert!(
            store
                .prepare_document_revisions("run", "produce", 1, &outputs, &Redactor::default())
                .is_err()
        );
        clear();
        assert_eq!(store.document_heads("run").unwrap().len(), 1);
        store
            .prepare_document_revisions("run", "produce", 1, &outputs, &Redactor::default())
            .unwrap();
        commit(&store, &completion(&store, "run", 1, "success")).unwrap();
        assert_eq!(store.document_heads("run").unwrap().len(), 3);
    }
}

#[test]
fn document_store_ambiguous_execution_link_and_sync_publish_complete_batches() {
    for point in [FAULT_AFTER_LINK, FAULT_AFTER_DIRECTORY_SYNC] {
        let tmp = tempfile::tempdir().unwrap();
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        birth(&store, tmp.path(), "run", None);
        active(&store, "run", 1);
        store
            .prepare_document_revisions(
                "run",
                "produce",
                1,
                &outputs(tmp.path()),
                &Redactor::default(),
            )
            .unwrap();
        let record = completion(&store, "run", 1, "success");
        fs::create_dir_all(tmp.path().join("runs/run_run/step_executions/produce")).unwrap();
        // Two validated dependency-directory resyncs precede record publication.
        set_occurrence(
            point,
            if point == FAULT_AFTER_DIRECTORY_SYNC {
                3
            } else {
                1
            },
        );
        assert!(commit(&store, &record).is_err());
        clear();
        assert_eq!(store.document_heads("run").unwrap().len(), 3);
        assert!(commit(&store, &record).is_err());
        store.reconcile_document_heads("run").unwrap();
    }
}

#[test]
fn document_store_cleanup_publication_and_recovery_use_the_same_ordinal_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    let mut record = store.load_run("run").unwrap().unwrap();
    let mut state = record.steps.remove("produce").unwrap();
    state.status = KernelStepStatus::Running;
    state.executions = 1;
    let mut cleanup = transist_core::PersistedCleanupRecord::new(
        transist_protocol::CleanupTrigger::Failed,
        &["produce".into()],
        transist_core::CleanupPhaseState::Running,
    );
    cleanup.steps.insert("produce".into(), state);
    record.cleanup = Some(cleanup);
    store.update_run_state("run", &record).unwrap();
    store
        .prepare_document_revisions(
            "run",
            "produce",
            1,
            &outputs(tmp.path()),
            &Redactor::default(),
        )
        .unwrap();
    set(FAULT_DOCUMENT_AFTER_COMMIT);
    assert!(commit(&store, &completion(&store, "run", 1, "success")).is_err());
    clear();
    let restarted = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    restarted.reconcile_document_heads("run").unwrap();
    assert_eq!(restarted.document_heads("run").unwrap().len(), 3);
    assert!(
        restarted
            .prepare_document_revisions(
                "run",
                "produce",
                1,
                &outputs(tmp.path()),
                &Redactor::default()
            )
            .is_err()
    );
    assert_eq!(
        restarted
            .load_step_execution("run", "produce", 1)
            .unwrap()
            .unwrap()
            .document_revisions
            .len(),
        2
    );
}

#[test]
fn document_store_failed_canceled_interrupted_retry_and_two_ticket_runs_are_isolated() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    for run in ["ticket-42-a", "ticket-42-b", "replay-42"] {
        birth(&store, tmp.path(), run, None);
    }
    let outputs = outputs(tmp.path());
    for (ordinal, status) in [(1, "failed"), (2, "cancelled"), (3, "interrupted")] {
        active(&store, "ticket-42-a", ordinal);
        store
            .prepare_document_revisions(
                "ticket-42-a",
                "produce",
                ordinal,
                &outputs,
                &Redactor::default(),
            )
            .unwrap();
        let mut invalid = completion(&store, "ticket-42-a", ordinal, status);
        invalid.document_revisions = store
            .prepared_document_revisions("ticket-42-a", "produce", ordinal)
            .unwrap();
        assert!(commit(&store, &invalid).is_err());
        invalid.document_revisions.clear();
        commit(&store, &invalid).unwrap();
        assert_eq!(store.document_heads("ticket-42-a").unwrap().len(), 1);
    }
    active(&store, "ticket-42-a", 4);
    store
        .prepare_document_revisions("ticket-42-a", "produce", 4, &outputs, &Redactor::default())
        .unwrap();
    commit(&store, &completion(&store, "ticket-42-a", 4, "success")).unwrap();
    let heads = store.document_heads("ticket-42-a").unwrap();
    assert_eq!(heads["a"].execution, 4);
    assert_eq!(heads["a"].commit_sequence, 1);
    for other in ["ticket-42-b", "replay-42"] {
        assert_eq!(store.document_heads(other).unwrap().len(), 1);
        assert!(store.read_document_blob(other, &heads["a"]).is_err());
    }
    assert!(
        store
            .prepare_document_revisions("ticket-42-a", "produce", 4, &outputs, &Redactor::default())
            .is_err()
    );
    assert!(
        store
            .prepare_document_revisions("ticket-42-a", "produce", 3, &outputs, &Redactor::default())
            .is_err()
    );
    assert!(
        store
            .prepare_document_revisions(
                "ticket-42-a",
                "../escape",
                5,
                &outputs,
                &Redactor::default()
            )
            .is_err()
    );
}

#[test]
fn document_store_contained_runs_share_blobs_heads_and_publication_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "root", None);
    birth(&store, tmp.path(), "child", Some("root"));
    let outputs = outputs(tmp.path());
    for (run, sequence) in [("root", 1), ("child", 2)] {
        active(&store, run, 1);
        store
            .prepare_document_revisions(run, "produce", 1, &outputs, &Redactor::default())
            .unwrap();
        commit(&store, &completion(&store, run, 1, "success")).unwrap();
        assert_eq!(
            store.document_heads("root").unwrap()["a"].commit_sequence,
            sequence
        );
    }
    assert_eq!(
        store.document_heads("child").unwrap(),
        store.document_heads("root").unwrap()
    );
    assert_eq!(
        fs::read_dir(tmp.path().join("runs/run_root/documents/blobs"))
            .unwrap()
            .count(),
        3
    );
    assert!(
        !tmp.path()
            .join("runs/run_root/contained_runs/run_child/documents")
            .exists()
    );
}

#[test]
fn document_store_tree_manifest_is_bounded_and_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    active(&store, "run", 1);
    let tree = tmp.path().join("tree");
    fs::create_dir_all(tree.join("nested/empty")).unwrap();
    fs::write(tree.join("nested/file"), [0, 255, 42]).unwrap();
    let output = DocumentOutput {
        document: "tree".into(),
        path: tree.clone(),
    };
    let candidates = store
        .prepare_document_revisions("run", "produce", 1, &[output.clone()], &Redactor::default())
        .unwrap();
    assert!(store.read_document_blob("run", &candidates[0]).is_err());
    commit(&store, &completion(&store, "run", 1, "success")).unwrap();
    let revision = store.document_heads("run").unwrap().remove("tree").unwrap();
    let manifest: DocumentTreeManifest =
        serde_json::from_slice(&store.read_document_blob("run", &revision).unwrap()).unwrap();
    assert_eq!(manifest.directories, ["nested", "nested/empty"]);
    assert_eq!(revision.file_count, 1);
    assert_eq!(revision.size_bytes, 3);
    assert_eq!(
        store
            .read_document_member("run", &revision, "nested/file")
            .unwrap(),
        [0, 255, 42]
    );
    assert!(
        store
            .read_document_member("run", &revision, "../file")
            .is_err()
    );
    active(&store, "run", 2);
    fs::write(tree.join("large"), [0; 33]).unwrap();
    assert!(
        store
            .prepare_document_revisions(
                "run",
                "produce",
                2,
                &[output.clone()],
                &Redactor::default()
            )
            .is_err()
    );
    fs::remove_file(tree.join("large")).unwrap();
    for name in ["a", "b", "c"] {
        fs::write(tree.join(name), []).unwrap();
    }
    assert!(
        store
            .prepare_document_revisions("run", "produce", 2, &[output], &Redactor::default())
            .is_err()
    );
    assert_eq!(store.document_heads("run").unwrap()["tree"], revision);
}

#[cfg(unix)]
#[test]
fn document_store_rejects_symlinks_hardlinks_special_files_and_linked_roots() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    active(&store, "run", 1);
    let tree = tmp.path().join("tree");
    fs::create_dir(&tree).unwrap();
    let outside = tmp.path().join("outside");
    fs::write(&outside, b"outside").unwrap();
    let output = DocumentOutput {
        document: "tree".into(),
        path: tree.clone(),
    };
    symlink(&outside, tree.join("link")).unwrap();
    assert!(
        store
            .prepare_document_revisions(
                "run",
                "produce",
                1,
                &[output.clone()],
                &Redactor::default()
            )
            .is_err()
    );
    fs::remove_file(tree.join("link")).unwrap();
    fs::hard_link(&outside, tree.join("hard")).unwrap();
    assert!(
        store
            .prepare_document_revisions(
                "run",
                "produce",
                1,
                &[output.clone()],
                &Redactor::default()
            )
            .is_err()
    );
    fs::remove_file(tree.join("hard")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(tree.join("socket")).unwrap();
    assert!(
        store
            .prepare_document_revisions("run", "produce", 1, &[output], &Redactor::default())
            .is_err()
    );
    let linked_tree = tmp.path().join("linked");
    symlink(&tree, &linked_tree).unwrap();
    assert!(
        store
            .prepare_document_revisions(
                "run",
                "produce",
                1,
                &[DocumentOutput {
                    document: "tree".into(),
                    path: linked_tree
                }],
                &Redactor::default()
            )
            .is_err()
    );
    let root = tmp.path().join("runs/run_run");
    let moved = tmp.path().join("moved");
    fs::rename(&root, &moved).unwrap();
    symlink(&moved, &root).unwrap();
    assert!(store.document_heads("run").is_err());
}

#[test]
fn document_store_rejects_known_raw_and_encoded_secrets_without_modifying_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    active(&store, "run", 1);
    let redactor = Redactor::from_secrets(["secret!?", "\u{ffff}"]);
    for secret in [
        "secret!?",
        "c2VjcmV0IT8=",
        "c2VjcmV0IT8",
        "\u{ffff}",
        "77+/",
        "77-_",
    ] {
        let path = tmp.path().join("secret-file");
        fs::write(&path, secret).unwrap();
        assert!(
            store
                .prepare_document_revisions(
                    "run",
                    "produce",
                    1,
                    &[DocumentOutput {
                        document: "a".into(),
                        path: path.clone()
                    }],
                    &redactor
                )
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), secret.as_bytes());
    }
    assert_eq!(store.document_heads("run").unwrap().len(), 1);
    assert_eq!(
        fs::read_dir(tmp.path().join("runs/run_run/documents/blobs"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn document_store_validates_immutable_revision_identity_size_digest_and_blobs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    active(&store, "run", 1);
    store
        .prepare_document_revisions(
            "run",
            "produce",
            1,
            &outputs(tmp.path()),
            &Redactor::default(),
        )
        .unwrap();
    let valid = completion(&store, "run", 1, "success");
    let mut omitted = valid.clone();
    omitted.document_revisions.clear();
    assert!(
        commit(&store, &omitted).is_err(),
        "a successful record must not drop prepared outputs"
    );
    for field in [
        "run_id",
        "step_id",
        "execution",
        "size_bytes",
        "revision_id",
    ] {
        let mut bad = serde_json::to_value(&valid).unwrap();
        bad["document_revisions"][0][field] = if matches!(field, "execution" | "size_bytes") {
            999.into()
        } else {
            "tampered".into()
        };
        assert!(
            commit(&store, &serde_json::from_value(bad).unwrap()).is_err(),
            "{field}"
        );
    }
    commit(&store, &valid).unwrap();
    let heads = store.document_heads("run").unwrap();
    fs::write(
        tmp.path()
            .join("runs/run_run/documents/blobs")
            .join(&heads["a"].blob.sha256),
        b"corrupt",
    )
    .unwrap();
    assert!(store.document_heads("run").is_err());
    assert!(store.load_step_execution("run", "produce", 1).is_err());
}

#[test]
fn document_store_heads_are_derived_and_declaration_drift_and_dropped_arrays_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    active(&store, "run", 1);
    store
        .prepare_document_revisions(
            "run",
            "produce",
            1,
            &outputs(tmp.path()),
            &Redactor::default(),
        )
        .unwrap();
    commit(&store, &completion(&store, "run", 1, "success")).unwrap();
    let root = tmp.path().join("runs/run_run");
    fs::write(
        root.join("documents/heads.json"),
        b"not an authoritative record",
    )
    .unwrap();
    assert_eq!(store.document_heads("run").unwrap().len(), 3);
    store.reconcile_document_heads("run").unwrap();
    let old_yaml = fs::read_to_string(root.join("recipe.yaml")).unwrap();
    let new_yaml = old_yaml.replace("max_bytes: 32", "max_bytes: 64");
    assert!(
        store
            .sync_recipe_snapshot("run", &store.load_run("run").unwrap().unwrap(), &new_yaml)
            .is_err()
    );
    fs::write(root.join("recipe.yaml"), &new_yaml).unwrap();
    assert!(store.document_heads("run").is_err());
    fs::write(root.join("recipe.yaml"), "schema: transist.recipe.v1\n").unwrap();
    assert!(store.reconcile_document_heads("run").is_err());
    fs::write(root.join("recipe.yaml"), old_yaml).unwrap();
    let execution = root.join("step_executions/produce/execution_001.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&execution).unwrap()).unwrap();
    record["document_revisions"] = serde_json::json!([]);
    fs::write(execution, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(store.document_heads("run").is_err());
    assert!(store.load_step_execution("run", "produce", 1).is_err());
}

#[test]
fn document_store_rejects_digest_valid_but_traversing_tree_manifests() {
    use sha2::{Digest, Sha256};
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    active(&store, "run", 1);
    let tree = tmp.path().join("tree");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("file"), b"bytes").unwrap();
    let mut revisions = store
        .prepare_document_revisions(
            "run",
            "produce",
            1,
            &[DocumentOutput {
                document: "tree".into(),
                path: tree,
            }],
            &Redactor::default(),
        )
        .unwrap();
    let blobs = tmp.path().join("runs/run_run/documents/blobs");
    let mut manifest: DocumentTreeManifest =
        serde_json::from_slice(&fs::read(blobs.join(&revisions[0].blob.sha256)).unwrap()).unwrap();
    let file = manifest.files.remove("file").unwrap();
    manifest.files.insert("../escape".into(), file);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    revisions[0].blob.sha256 = format!("{:x}", Sha256::digest(&bytes));
    revisions[0].blob.size_bytes = bytes.len() as u64;
    fs::write(blobs.join(&revisions[0].blob.sha256), bytes).unwrap();
    revisions[0].revision_id.clear();
    revisions[0].revision_id = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&revisions[0]).unwrap())
    );
    fs::write(
        tmp.path()
            .join("runs/run_run/documents/candidates/run/produce/execution_001.json"),
        serde_json::to_vec(&revisions).unwrap(),
    )
    .unwrap();
    assert!(
        store
            .prepared_document_revisions("run", "produce", 1)
            .is_err()
    );
    assert_eq!(store.document_heads("run").unwrap().len(), 1);
}
