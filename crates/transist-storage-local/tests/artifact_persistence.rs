// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::path::Path;
use transist_core::ports::{ArtifactPort, ArtifactReadRequest, ArtifactWriteRequest, RunStorePort};
use transist_core::{
    KernelRunSnapshot, PersistedRunRecord, RecipeSnapshotRef, RunLineage, RunLineageMode,
};
use transist_protocol::KernelValue;
use transist_storage_local::run_paths::resolve_run_root;
use transist_storage_local::{LocalArtifactStore, LocalRunStore, LocalStorageConfig};

const FAILURE_ARTIFACT_RELATIVE_PATH: &str =
    "runs/run_run_artifact/artifacts/step_failure/execution_002/evidence/error.json";
const FAILURE_ARTIFACT_BYTES: &[u8] = b"{\n  \"status\": \"failed\",\n  \"stderr\": \"boom\"\n}";

fn seed_run(tmp: &tempfile::TempDir, run_id: &str) {
    let cfg = LocalStorageConfig::new(tmp.path());
    let store = LocalRunStore::new(cfg);
    let record = PersistedRunRecord::from_snapshot(
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
    );
    store.create_run(&record).expect("create run");
}

#[test]
fn artifact_write_and_read_for_success_case() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));
    let request = ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_success".to_string(),
        execution: 1,
        logical_path: "artifacts/result.json".to_string(),
        payload: serde_json::json!({"status": "success", "value": 42}).into(),
    };

    let path = store.write_artifact(&request).expect("write");
    assert!(path.exists());

    let loaded = store
        .read_artifact(&ArtifactReadRequest {
            run_id: "run_artifact".to_string(),
            step_id: "step_success".to_string(),
            execution: 1,
            logical_path: "artifacts/result.json".to_string(),
        })
        .expect("read");
    assert_eq!(
        serde_json::Value::from(loaded),
        serde_json::json!({"status": "success", "value": 42})
    );
}

#[test]
fn artifact_write_and_read_for_failure_case() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));
    let request = ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_failure".to_string(),
        execution: 2,
        logical_path: "evidence/error.json".to_string(),
        payload: KernelValue::from(serde_json::json!({
            "status": "failed",
            "stderr": "boom"
        })),
    };

    let path = store.write_artifact(&request).expect("write");
    assert!(path.exists());
    assert_eq!(
        path.strip_prefix(tmp.path()).expect("artifact under home"),
        Path::new(FAILURE_ARTIFACT_RELATIVE_PATH)
    );
    assert_eq!(
        std::fs::read(&path).expect("read artifact bytes"),
        FAILURE_ARTIFACT_BYTES
    );

    let loaded = store
        .read_artifact(&ArtifactReadRequest {
            run_id: "run_artifact".to_string(),
            step_id: "step_failure".to_string(),
            execution: 2,
            logical_path: "evidence/error.json".to_string(),
        })
        .expect("read");
    assert_eq!(
        serde_json::Value::from(loaded),
        serde_json::json!({"status": "failed", "stderr": "boom"})
    );
}

#[test]
fn write_artifact_rejects_traversal_and_absolute_logical_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));

    for logical_path in ["..", "../escape.txt", "../../etc/passwd", "a/../../b.txt"] {
        let request = ArtifactWriteRequest {
            run_id: "run_artifact".to_string(),
            step_id: "step_ok".to_string(),
            execution: 1,
            logical_path: logical_path.to_string(),
            payload: KernelValue::from(serde_json::json!({"v": 1})),
        };
        assert!(
            store.write_artifact(&request).is_err(),
            "expected '{}' to be rejected",
            logical_path
        );
    }

    let request = ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_ok".to_string(),
        execution: 1,
        logical_path: "/etc/passwd".to_string(),
        payload: KernelValue::from(serde_json::json!({"v": 1})),
    };
    assert!(
        store.write_artifact(&request).is_err(),
        "absolute artifact path must be rejected"
    );
}

#[test]
fn read_artifact_rejects_traversal_and_absolute_logical_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));

    for logical_path in [
        "",
        ".",
        "..",
        "../escape.txt",
        "../../etc/passwd",
        "a/../../b.txt",
        "/etc/passwd",
    ] {
        let request = ArtifactReadRequest {
            run_id: "run_artifact".to_string(),
            step_id: "step_ok".to_string(),
            execution: 1,
            logical_path: logical_path.to_string(),
        };
        assert!(
            store.read_artifact(&request).is_err(),
            "expected '{}' to be rejected",
            logical_path
        );
    }

    let bad_step = ArtifactReadRequest {
        run_id: "run_artifact".to_string(),
        step_id: "../escape".to_string(),
        execution: 1,
        logical_path: "artifacts/result.json".to_string(),
    };
    assert!(
        store.read_artifact(&bad_step).is_err(),
        "traversal step id must be rejected"
    );
}

#[test]
fn write_artifact_rejects_empty_logical_path_and_invalid_step_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));

    let empty = ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_ok".to_string(),
        execution: 1,
        logical_path: "".to_string(),
        payload: KernelValue::from(serde_json::json!({"v": 1})),
    };
    assert!(
        store.write_artifact(&empty).is_err(),
        "empty logical path must be rejected"
    );

    let bad_step = ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "../escape".to_string(),
        execution: 1,
        logical_path: "artifacts/out.json".to_string(),
        payload: KernelValue::from(serde_json::json!({"v": 1})),
    };
    assert!(
        store.write_artifact(&bad_step).is_err(),
        "traversal step id must be rejected"
    );
}

#[test]
fn artifact_write_and_read_for_valid_nested_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));
    let request = ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_nested".to_string(),
        execution: 1,
        logical_path: "nested/deep/result.json".to_string(),
        payload: KernelValue::from(serde_json::json!({"status": "ok"})),
    };

    let path = store.write_artifact(&request).expect("write nested");
    assert!(path.exists());

    let loaded = store
        .read_artifact(&ArtifactReadRequest {
            run_id: "run_artifact".to_string(),
            step_id: "step_nested".to_string(),
            execution: 1,
            logical_path: "nested/deep/result.json".to_string(),
        })
        .expect("read nested");
    assert_eq!(
        serde_json::Value::from(loaded),
        serde_json::json!({"status": "ok"})
    );
}

#[cfg(unix)]
#[test]
fn read_artifact_rejects_symlinked_artifact_file() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));
    let request = ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_symlink".to_string(),
        execution: 1,
        logical_path: "artifacts/result.json".to_string(),
        payload: KernelValue::from(serde_json::json!({"status": "ok"})),
    };
    let path = store.write_artifact(&request).expect("write");

    // Replace the artifact file with a symlink pointing at attacker bytes. The
    // anchored no-follow read must reject the symlink rather than follow it.
    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, b"attacker-bytes").expect("write outside");
    std::fs::remove_file(&path).expect("remove artifact");
    symlink(&outside, &path).expect("symlink artifact");

    let read = store.read_artifact(&ArtifactReadRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_symlink".to_string(),
        execution: 1,
        logical_path: "artifacts/result.json".to_string(),
    });
    assert!(read.is_err(), "symlinked artifact must be rejected");
    let error = read.expect_err("read symlinked artifact");
    assert!(
        format!("{error:#}").contains("symlink"),
        "unexpected error: {error:#}"
    );
}

#[cfg(unix)]
#[test]
fn write_artifact_rejects_symlinked_artifact_ancestor() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().expect("tempdir");
    seed_run(&tmp, "run_artifact");
    let run_root = resolve_run_root(tmp.path(), "run_artifact").expect("run root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    symlink(&outside, run_root.join("artifacts")).expect("symlink artifacts root");
    let store = LocalArtifactStore::new(LocalStorageConfig::new(tmp.path()));

    let result = store.write_artifact(&ArtifactWriteRequest {
        run_id: "run_artifact".to_string(),
        step_id: "step_symlink".to_string(),
        execution: 1,
        logical_path: "artifacts/result.json".to_string(),
        payload: KernelValue::from(serde_json::json!({"status": "blocked"})),
    });
    assert!(result.is_err(), "symlinked ancestor must reject the write");
    assert!(
        !outside
            .join("step_symlink/execution_001/artifacts/result.json")
            .exists()
    );
}
