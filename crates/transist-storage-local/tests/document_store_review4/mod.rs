// SPDX-License-Identifier: MPL-2.0
//! Independent-review regressions: real process death, missing history and manifest secrets.
use super::*;
use sha2::{Digest, Sha256};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn cleanup_active(store: &LocalRunStore, ordinal: u32) {
    let mut record = store.load_run("run").unwrap().unwrap();
    record.steps.remove("produce");
    let mut cleanup = transist_core::PersistedCleanupRecord::new(
        transist_protocol::CleanupTrigger::Failed,
        &["produce".into()],
        transist_core::CleanupPhaseState::Running,
    );
    let state = cleanup.steps.get_mut("produce").unwrap();
    state.status = KernelStepStatus::Running;
    state.executions = ordinal;
    record.cleanup = Some(cleanup);
    store.update_run_state("run", &record).unwrap();
}

fn project(store: &LocalRunStore, ordinal: u32, cleanup: bool, status: KernelStepStatus) {
    let mut record = store.load_run("run").unwrap().unwrap();
    let state = if cleanup {
        record
            .cleanup
            .as_mut()
            .unwrap()
            .steps
            .get_mut("produce")
            .unwrap()
    } else {
        record.steps.get_mut("produce").unwrap()
    };
    state.executions = ordinal;
    state.status = status;
    state.last_result = Some(result(
        "run",
        ordinal,
        if status == KernelStepStatus::Succeeded {
            "success"
        } else {
            "failed"
        },
    ));
    state.finished_at_ms = Some(2);
    store.update_run_state("run", &record).unwrap();
}

#[test]
fn publication_crash_child() {
    let Some(home) = std::env::var_os("TRANSIST_DOC_CRASH_HOME") else {
        return;
    };
    let target = std::env::var_os("TRANSIST_DOC_CRASH_TARGET").unwrap();
    let ready = std::env::var_os("TRANSIST_DOC_CRASH_READY").unwrap();
    pause_after_durable_publication(Path::new(&target), Path::new(&ready));
    let store = LocalRunStore::new(LocalStorageConfig::new(&home));
    if std::env::var("TRANSIST_DOC_CRASH_OPERATION").unwrap() == "capture" {
        store
            .prepare_document_revisions(
                "run",
                "produce",
                1,
                &outputs(Path::new(&home)),
                &Redactor::default(),
            )
            .unwrap();
    } else {
        commit(&store, &completion(&store, "run", 1, "success")).unwrap();
    }
    panic!("publication pause was not reached");
}

#[test]
#[cfg(unix)]
fn review4_real_kill_after_durable_link_recovers_blob_candidate_main_and_cleanup() {
    use std::os::unix::fs::MetadataExt;
    for case in ["blob", "candidate", "main", "cleanup"] {
        let tmp = tempfile::tempdir().unwrap();
        let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        birth(&store, tmp.path(), "run", None);
        if case == "cleanup" {
            cleanup_active(&store, 1);
        } else {
            active(&store, "run", 1);
        }
        let root = tmp.path().join("runs/run_run");
        let target = match case {
            "blob" => root
                .join("documents/blobs")
                .join(format!("{:x}", Sha256::digest(b"a bytes"))),
            "candidate" => root.join("documents/candidates/run/produce/execution_001.json"),
            _ => root.join("step_executions/produce/execution_001.json"),
        };
        let operation = if matches!(case, "main" | "cleanup") {
            store
                .prepare_document_revisions(
                    "run",
                    "produce",
                    1,
                    &outputs(tmp.path()),
                    &Redactor::default(),
                )
                .unwrap();
            "commit"
        } else {
            "capture"
        };
        let ready = tmp.path().join("publication-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "review4::publication_crash_child", "--nocapture"])
            .env("TRANSIST_DOC_CRASH_HOME", tmp.path())
            .env("TRANSIST_DOC_CRASH_TARGET", &target)
            .env("TRANSIST_DOC_CRASH_READY", &ready)
            .env("TRANSIST_DOC_CRASH_OPERATION", operation)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("child exited before publication pause: {case}: {status}");
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("publication pause timed out: {case}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::metadata(&target).unwrap().nlink(),
            2,
            "the crash must retain a genuine temp alias: {case}"
        );
        child.kill().unwrap();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9));
        let restarted = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
        if operation == "capture" {
            assert_eq!(restarted.document_heads("run").unwrap().len(), 1);
            if case == "candidate" {
                assert_eq!(
                    restarted
                        .prepared_document_revisions("run", "produce", 1)
                        .unwrap()
                        .len(),
                    2
                );
            }
            restarted
                .prepare_document_revisions(
                    "run",
                    "produce",
                    1,
                    &outputs(tmp.path()),
                    &Redactor::default(),
                )
                .unwrap();
            commit(&restarted, &completion(&restarted, "run", 1, "success")).unwrap();
        }
        restarted.reconcile_document_heads("run").unwrap();
        let heads = restarted.document_heads("run").unwrap();
        assert_eq!(heads.len(), 3, "{case}");
        assert_eq!(
            restarted.read_document_blob("run", &heads["a"]).unwrap(),
            b"a bytes"
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
        assert_eq!(
            fs::metadata(target).unwrap().nlink(),
            2,
            "read/recovery must not delete aliases"
        );
        let target = match case {
            "blob" => root
                .join("documents/blobs")
                .join(format!("{:x}", Sha256::digest(b"a bytes"))),
            "candidate" => root.join("documents/candidates/run/produce/execution_001.json"),
            _ => root.join("step_executions/produce/execution_001.json"),
        };
        if matches!(case, "main" | "cleanup") {
            let mut record: serde_json::Value =
                serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
            record["step_id"] = "wrong-step".into();
            fs::write(target, serde_json::to_vec(&record).unwrap()).unwrap();
        } else {
            fs::write(target, b"corrupt").unwrap();
        }
        assert!(
            restarted.document_heads("run").is_err(),
            "runtime aliases must not bypass corruption checks: {case}"
        );
    }
}

#[test]
fn review4_missing_consumed_tail_and_whole_execution_directory_fail_closed() {
    for cleanup in [false, true] {
        for failed_tail in [false, true] {
            for whole_directory in [false, true] {
                let tmp = tempfile::tempdir().unwrap();
                let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
                birth(&store, tmp.path(), "run", None);
                for ordinal in [1, 2] {
                    if cleanup {
                        cleanup_active(&store, ordinal);
                    } else {
                        active(&store, "run", ordinal);
                    }
                    store
                        .prepare_document_revisions(
                            "run",
                            "produce",
                            ordinal,
                            &outputs(tmp.path()),
                            &Redactor::default(),
                        )
                        .unwrap();
                    let status = if ordinal == 2 && failed_tail {
                        "failed"
                    } else {
                        "success"
                    };
                    commit(&store, &completion(&store, "run", ordinal, status)).unwrap();
                    project(
                        &store,
                        ordinal,
                        cleanup,
                        if status == "success" {
                            KernelStepStatus::Succeeded
                        } else {
                            KernelStepStatus::Failed
                        },
                    );
                }
                let directory = tmp.path().join("runs/run_run/step_executions/produce");
                if whole_directory {
                    fs::remove_dir_all(directory).unwrap();
                } else {
                    fs::remove_file(directory.join("execution_002.json")).unwrap();
                }
                assert!(
                    store.document_heads("run").is_err(),
                    "cleanup={cleanup} failed_tail={failed_tail} whole={whole_directory}"
                );
                assert!(store.reconcile_document_heads("run").is_err());
            }
        }
    }
}

#[test]
fn review4_missing_prior_history_is_not_hidden_by_new_inflight_or_reset_projection() {
    for status in [
        KernelStepStatus::Running,
        KernelStepStatus::Pending,
        KernelStepStatus::Failed,
    ] {
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
        let mut record = store.load_run("run").unwrap().unwrap();
        let step = record.steps.get_mut("produce").unwrap();
        step.status = status;
        step.last_result = None;
        step.executions = if status == KernelStepStatus::Running {
            2
        } else {
            1
        };
        store.update_run_state("run", &record).unwrap();
        assert!(
            store.document_heads("run").is_ok(),
            "valid retained history/reset or in-flight latest must be readable"
        );
        fs::remove_file(
            tmp.path()
                .join("runs/run_run/step_executions/produce/execution_001.json"),
        )
        .unwrap();
        assert!(store.document_heads("run").is_err(), "{status:?}");
    }
}

#[test]
fn review4_canonical_manifest_secret_is_rejected_before_any_batch_blob_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalRunStore::new(LocalStorageConfig::new(tmp.path()));
    birth(&store, tmp.path(), "run", None);
    active(&store, "run", 1);
    let tree = tmp.path().join("tree");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("empty"), []).unwrap();
    let mut batch = outputs(tmp.path());
    batch.push(DocumentOutput {
        document: "tree".into(),
        path: tree,
    });
    let secret = format!("{:x}", Sha256::digest([]));
    assert!(
        store
            .prepare_document_revisions(
                "run",
                "produce",
                1,
                &batch,
                &Redactor::from_secrets([secret.as_str()])
            )
            .is_err()
    );
    assert_eq!(store.document_heads("run").unwrap().len(), 1);
    assert!(
        !tmp.path()
            .join("runs/run_run/documents/candidates")
            .exists()
    );
    assert_eq!(
        fs::read_dir(tmp.path().join("runs/run_run/documents/blobs"))
            .unwrap()
            .count(),
        1,
        "no first-output or manifest/member blob residue"
    );
}
