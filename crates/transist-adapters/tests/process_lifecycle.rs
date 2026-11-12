// SPDX-License-Identifier: MPL-2.0
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "../../../tests/support/process_lifecycle.rs"]
mod fixture;

#[path = "support/timing.rs"]
mod timing;
use timing::{assert_timing, wall_ms};

use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use transist_adapters::{DEFAULT_TERMINATION_GRACE_MS, ProcessAdapter};
use transist_core::ports::{
    ActiveRecordPort, ActiveRecordWrite, ExecutionCancelRequest, ExecutionInvocation,
    ExecutionPort, RemoteSecretPolicy,
};
use transist_protocol::{
    AdapterKind, CanonicalErrorCode, ExecutionRequest, InvokePaths, InvokeStep,
};
use transist_storage_local::LocalActiveRecordStore;

/// Installs an isolated fixture and builds a typed invocation selecting `mode`.
fn invocation(root: &Path, mode: &str) -> ExecutionInvocation {
    let plugin = root.join("plugin");
    fixture::write_plugin(&plugin);
    transist_plugins::install_plugin_file(root, &plugin.join("plugin.yaml")).expect("install");
    let staging = root.join(".transist/runs/run_lifecycle");
    fs::create_dir_all(staging.join("artifacts")).expect("artifacts");
    fs::create_dir_all(staging.join("evidence")).expect("evidence");
    ExecutionInvocation {
        request: ExecutionRequest {
            message_type: "tep.invoke.request.v1".into(),
            protocol_version: "tep.v1".into(),
            invoke_id: "lifecycle-invoke".into(),
            run_id: "lifecycle-run".into(),
            workflow_id: "lifecycle".into(),
            step: InvokeStep {
                id: "invoke".into(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "invoke".into(),
            input: json!({"mode": mode}),
            paths: InvokePaths {
                run_root: staging.display().to_string(),
                artifacts_dir: staging.join("artifacts").display().to_string(),
                evidence_dir: staging.join("evidence").display().to_string(),
                workspace_dir: root.display().to_string(),
            },
            requested_capabilities: vec![],
            // These cases measure the post-terminal 250-ms boundary, not
            // interpreter/package startup. The timeout-specific case overrides it.
            timeout_ms: Some(30_000),
            max_timeout_ms: None,
            policy: json!({"workspace_declared": true}),
            metadata: json!({"plugin_id":"fixture.lifecycle"}),
        },
        secret_values: BTreeMap::new(),
        remote_secret_policy: RemoteSecretPolicy::default(),
    }
}

/// Creates a process adapter using the real local active-record implementation.
fn adapter() -> ProcessAdapter {
    ProcessAdapter::new(Arc::new(LocalActiveRecordStore))
}

/// Delegates persistence while introducing cancellation at the final observation.
#[derive(Default)]
struct DelayedFinalCancelStore {
    observation: Mutex<Option<(u64, u64, u64)>>,
}

impl ActiveRecordPort for DelayedFinalCancelStore {
    fn write_record(&self, run_root: &Path, record: &ActiveRecordWrite) -> anyhow::Result<()> {
        LocalActiveRecordStore.write_record(run_root, record)
    }

    fn remove_record(&self, run_root: &Path, invoke_id: &str) -> anyhow::Result<()> {
        LocalActiveRecordStore.remove_record(run_root, invoke_id)
    }

    fn list_records(&self, run_root: &Path) -> anyhow::Result<Vec<ActiveRecordWrite>> {
        LocalActiveRecordStore.list_records(run_root)
    }

    fn write_cancel_sentinel(&self, run_root: &Path, invoke_id: &str) -> anyhow::Result<()> {
        LocalActiveRecordStore.write_cancel_sentinel(run_root, invoke_id)
    }

    fn remove_cancel_sentinel(&self, run_root: &Path, invoke_id: &str) -> anyhow::Result<()> {
        LocalActiveRecordStore.remove_cancel_sentinel(run_root, invoke_id)
    }

    fn cancel_sentinel_exists(&self, _run_root: &Path, _invoke_id: &str) -> bool {
        let before_delay = wall_ms();
        let delay = Instant::now();
        thread::sleep(Duration::from_millis(50));
        let observed_at = wall_ms();
        let delay_ms = delay.elapsed().as_millis() as u64;
        let previous = self.observation.lock().expect("observation lock").replace((
            before_delay,
            observed_at,
            delay_ms,
        ));
        assert!(
            previous.is_none(),
            "expected only the final cancellation check"
        );
        true
    }
}

#[test]
fn late_cancellation_timing_includes_final_observation() {
    let mut stale_finishes = Vec::new();
    for mode in ["clean", "leader"] {
        let repo = tempfile::tempdir().expect("tempdir");
        let request = invocation(repo.path(), mode);
        let store = Arc::new(DelayedFinalCancelStore::default());
        let adapter = ProcessAdapter::new(store.clone());
        let before = wall_ms();
        let result = adapter.execute_step(&request).expect("execute");
        let after = wall_ms();
        let (before_delay, observed_at, delay_ms) = store
            .observation
            .lock()
            .expect("observation lock")
            .expect("final cancellation observed");
        assert_timing(&result.timing, before, after, true);
        let start = result.timing.started_at.parse::<u64>().expect("start");
        let finish = result.timing.finished_at.parse::<u64>().expect("finish");
        assert!(
            (before..=before_delay).contains(&start),
            "original start must survive: {mode}"
        );
        if finish < observed_at {
            stale_finishes.push((mode, finish, observed_at));
        }
        assert!(delay_ms > 0);
        assert!(
            result.timing.duration_ms >= delay_ms,
            "duration must include the observed delay: {mode}"
        );
        assert_eq!(result.status, "cancelled");
        assert_eq!(result.exit_code, None);
        assert_eq!(
            result.error.as_ref().expect("error").code,
            CanonicalErrorCode::Cancelled
        );
        assert_eq!(result.output, json!({}));
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert!(result.artifacts.is_empty());
        if mode == "clean" {
            assert!(result.evidence.is_empty());
        } else {
            assert_eq!(result.evidence.len(), 1);
            assert_eq!(evidence(&result)["cleanup"], "confirmed");
        }
        let staging = Path::new(&request.request.paths.run_root);
        assert!(
            LocalActiveRecordStore
                .list_records(staging)
                .expect("records")
                .is_empty()
        );
        assert!(
            !LocalActiveRecordStore.cancel_sentinel_exists(staging, &request.request.invoke_id)
        );
        assert_exited(repo.path());
    }
    assert!(
        stale_finishes.is_empty(),
        "finish precedes final cancellation observation: {stale_finishes:?}"
    );
}

/// Reads the execution's separate lifecycle diagnostic and checks its checksum.
fn evidence(result: &transist_protocol::ExecutionResult) -> Value {
    let file = result
        .evidence
        .iter()
        .find(|file| {
            file.path
                .ends_with("transist_process_lifecycle_evidence.json")
        })
        .expect("lifecycle evidence");
    let bytes = fs::read(&file.path).expect("evidence bytes");
    assert_eq!(file.sha256, transist_trust_digest::sha256_hex(&bytes));
    serde_json::from_slice(&bytes).expect("json")
}

/// Confirms every recorded numeric fixture PID is absent or an inert zombie.
fn assert_exited(root: &Path) {
    for name in ["lifecycle-child.pid", "lifecycle-leader.pid"] {
        let Ok(pid) = fs::read_to_string(root.join(name)) else {
            continue;
        };
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .expect("probe");
        let state = String::from_utf8_lossy(&output.stdout);
        assert!(
            state.trim().is_empty() || state.trim().starts_with('Z'),
            "survivor {pid}: {state}"
        );
    }
}

#[test]
fn clean_terminal_result_remains_successful() {
    let repo = tempfile::tempdir().expect("tempdir");
    let request = invocation(repo.path(), "clean");
    let result = adapter().execute_step(&request).expect("execute");
    assert_eq!(result.status, "success");
    assert_eq!(result.output["stdout"], "terminal-output");
    assert!(
        !result
            .evidence
            .iter()
            .any(|file| file.path.contains("process_lifecycle"))
    );
    assert_exited(repo.path());
}

#[test]
fn terminal_result_with_live_boundary_fails_closed_and_forces_cleanup() {
    for mode in ["leader", "descendant", "failed_descendant", "failed_empty"] {
        let repo = tempfile::tempdir().expect("tempdir");
        let request = invocation(repo.path(), mode);
        let before = wall_ms();
        let result = adapter().execute_step(&request).expect("execute");
        let after = wall_ms();
        assert_timing(&result.timing, before, after, true);
        assert_eq!(result.status, "failed", "{mode}");
        assert_eq!(result.exit_code, Some(2));
        assert_eq!(
            result.error.as_ref().unwrap().retryability,
            transist_protocol::Retryability::None
        );
        assert_eq!(
            result.error.as_ref().expect("error").code,
            CanonicalErrorCode::ProtocolError
        );
        assert_eq!(
            result.error.as_ref().unwrap().message,
            "process boundary did not exit within 250 ms after terminal APP result"
        );
        assert_eq!(result.output, json!({}));
        assert!(result.stdout.is_empty());
        let retained = evidence(&result);
        assert_eq!(retained["schema"], "transist.process_lifecycle.v1");
        assert_eq!(retained["reason"], "process_exit_grace_exceeded");
        assert_eq!(retained["exit_grace_ms"], DEFAULT_TERMINATION_GRACE_MS);
        assert_eq!(retained["cleanup"], "confirmed");
        assert_eq!(
            retained["snapshot"]["leader_alive"],
            mode == "leader" || mode == "failed_empty"
        );
        assert!(
            !retained["snapshot"]["live_pids"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        if mode == "failed_empty" {
            assert!(retained.get("original_output").is_none());
            assert_eq!(
                retained["original_terminal_error"]["code"],
                "tool_execution_error"
            );
            assert!(result.artifacts.is_empty());
            assert_eq!(result.evidence.len(), 1);
        } else {
            assert_eq!(retained["original_output"]["stdout"], "terminal-output");
            assert_eq!(result.artifacts.len(), 1);
            assert_eq!(result.evidence.len(), 2);
        }
        assert_exited(repo.path());
    }
}

#[test]
fn invalid_identity_and_output_are_never_retained() {
    for mode in [
        "invalid_identity",
        "invalid_type",
        "invalid_version",
        "invalid_output",
    ] {
        let repo = tempfile::tempdir().expect("tempdir");
        let request = invocation(repo.path(), mode);
        let result = adapter().execute_step(&request).expect("execute");
        assert_eq!(result.output, json!({}));
        assert_eq!(
            result.error.as_ref().unwrap().code,
            if mode == "invalid_output" {
                CanonicalErrorCode::ValidationError
            } else {
                CanonicalErrorCode::ProtocolError
            }
        );
        assert!(result.artifacts.is_empty());
        assert_eq!(result.evidence.len(), 1);
        let retained = evidence(&result);
        assert!(retained.get("original_output").is_none());
        assert_eq!(
            retained["terminal_validation_error"]["code"],
            if mode == "invalid_output" {
                "validation_error"
            } else {
                "protocol_error"
            }
        );
        if mode != "invalid_output" {
            assert!(retained.get("original_terminal_status").is_none());
            assert!(retained.get("original_terminal_error").is_none());
        }
        assert_exited(repo.path());
    }
}

#[test]
fn terminal_output_error_and_hints_redact_raw_and_base64_secrets() {
    let repo = tempfile::tempdir().expect("tempdir");
    let mut request = invocation(repo.path(), "failed_descendant");
    request
        .secret_values
        .insert("LIFECYCLE_SECRET".into(), "topsecret".into());
    let result = adapter().execute_step(&request).expect("execute");
    let retained = evidence(&result);
    assert!(
        retained["original_output"]["stdout"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    for file in result.artifacts.iter().chain(&result.evidence) {
        let text = fs::read_to_string(&file.path).expect("bytes");
        for variant in ["topsecret", "dG9wc2VjcmV0"] {
            assert!(!text.contains(variant));
        }
    }
    assert_exited(repo.path());
}

#[test]
fn cancellation_after_terminal_frame_wins() {
    for mode in ["leader", "invalid_output"] {
        let repo = tempfile::tempdir().expect("tempdir");
        let request = invocation(repo.path(), mode);
        let adapter = adapter();
        let executor = adapter.clone();
        let before = wall_ms();
        let handle = thread::spawn(move || executor.execute_step(&request).expect("execute"));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !repo.path().join("lifecycle-terminal.ready").exists() {
            assert!(Instant::now() < deadline, "terminal frame never sent");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            adapter
                .cancel_step(&ExecutionCancelRequest {
                    invoke_id: "lifecycle-invoke".into()
                })
                .expect("cancel")
                .accepted
        );
        let result = handle.join().expect("join");
        let after = wall_ms();
        assert_timing(&result.timing, before, after, true);
        assert_eq!(result.status, "cancelled");
        assert_eq!(result.error.unwrap().code, CanonicalErrorCode::Cancelled);
        assert_eq!(result.output, json!({}));
        assert_exited(repo.path());
    }
}

#[test]
fn pre_terminal_timeout_keeps_existing_classification() {
    let repo = tempfile::tempdir().expect("tempdir");
    let mut request = invocation(repo.path(), "pre_terminal");
    request.request.timeout_ms = Some(500);
    let before = wall_ms();
    let result = adapter().execute_step(&request).expect("execute");
    let after = wall_ms();
    assert_timing(&result.timing, before, after, true);
    assert_eq!(result.error.unwrap().code, CanonicalErrorCode::TimeoutError);
    assert_eq!(result.exit_code, Some(124));
    assert!(result.evidence.is_empty());
    assert_exited(repo.path());
}

#[test]
fn external_cancellation_after_terminal_frame_wins() {
    use transist_core::ports::ActiveRecordPort;
    let repo = tempfile::tempdir().expect("tempdir");
    let request = invocation(repo.path(), "leader");
    let staging = std::path::PathBuf::from(&request.request.paths.run_root);
    let before = wall_ms();
    let handle = thread::spawn(move || adapter().execute_step(&request).expect("execute"));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !repo.path().join("lifecycle-terminal.ready").exists() {
        assert!(Instant::now() < deadline, "terminal frame never sent");
        thread::sleep(Duration::from_millis(5));
    }
    LocalActiveRecordStore
        .write_cancel_sentinel(&staging, "lifecycle-invoke")
        .expect("sentinel");
    let result = handle.join().expect("join");
    let after = wall_ms();
    assert_timing(&result.timing, before, after, true);
    assert_eq!(result.status, "cancelled");
    assert_eq!(result.error.unwrap().code, CanonicalErrorCode::Cancelled);
    assert_eq!(result.output, json!({}));
    assert_exited(repo.path());
}

#[test]
fn clean_invalid_terminal_results_preserve_validation_and_provider_errors() {
    for mode in [
        "invalid_output_clean",
        "invalid_identity_clean",
        "invalid_type_clean",
        "invalid_version_clean",
        "failed_empty_clean",
    ] {
        let repo = tempfile::tempdir().expect("tempdir");
        let request = invocation(repo.path(), mode);
        let result = adapter().execute_step(&request).expect("execute");
        assert_eq!(result.status, "failed");
        assert_eq!(result.output, json!({}));
        if mode == "failed_empty_clean" {
            assert_eq!(result.artifacts.len(), 1);
            assert_eq!(result.evidence.len(), 1);
        } else {
            assert!(result.artifacts.is_empty());
            assert!(result.evidence.is_empty());
        }
        let error = result.error.unwrap();
        assert_eq!(
            error.code,
            match mode {
                "invalid_output_clean" => CanonicalErrorCode::ValidationError,
                "failed_empty_clean" => CanonicalErrorCode::ToolExecutionError,
                _ => CanonicalErrorCode::ProtocolError,
            }
        );
        if mode == "failed_empty_clean" {
            assert_eq!(error.message, "terminal-output");
        }
        assert_exited(repo.path());
    }
}

#[test]
fn clean_provider_failure_preserves_redacted_error_and_existing_hints() {
    let repo = tempfile::tempdir().expect("tempdir");
    let mut request = invocation(repo.path(), "failed_empty_clean");
    request
        .secret_values
        .insert("LIFECYCLE_SECRET".into(), "topsecret".into());
    let result = adapter().execute_step(&request).expect("execute");
    assert_eq!(result.status, "failed");
    assert_eq!(result.output, json!({}));
    assert!(result.stdout.is_empty());
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(result.evidence.len(), 1);
    for file in result.artifacts.iter().chain(&result.evidence) {
        let text = fs::read_to_string(&file.path).expect("hint contents");
        for forbidden in ["topsecret", "dG9wc2VjcmV0"] {
            assert!(!text.contains(forbidden));
        }
    }
    let error = result.error.unwrap();
    assert_eq!(error.code, CanonicalErrorCode::ToolExecutionError);
    assert!(error.message.contains("[REDACTED]"));
    for forbidden in ["topsecret", "dG9wc2VjcmV0"] {
        assert!(!error.message.contains(forbidden));
    }
    assert_exited(repo.path());
}
