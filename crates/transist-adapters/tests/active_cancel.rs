// SPDX-License-Identifier: MPL-2.0
//! Integration tests for cross-process cancellation via on-disk active records.
//!
//! These tests exercise [`transist_adapters::cancel_active_invocations_on_disk`]
//! end-to-end on Unix: they spawn a child process in its own process group,
//! write the active-invocation record by hand, then ask the cancel helper to
//! signal it. The cancel helper has no in-process knowledge of the spawned
//! child — it discovers the pid/pgid only by reading the JSON file, exactly
//! as an external supervisor would when cancelling a run owned by a separate
//! Transist process.

// SAFETY: Test: uses libc fork/getpgid for cross-process cancellation tests.
#![allow(unsafe_code)]
#![cfg(unix)]

#[path = "../../../tests/support/process_lifecycle.rs"]
mod fixture;
#[path = "support/timing.rs"]
mod timing;

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use transist_adapters::{
    CancelOutcome, DEFAULT_TERMINATION_GRACE_MS, cancel_active_invocations_on_disk,
};
use transist_storage_local::LocalActiveRecordStore;
use transist_storage_local::run_paths::{
    ACTIVE_INVOCATION_RECORD_SCHEMA_V1, ActiveInvocationRecord, active_invocations_dir,
    write_active_invocation_record,
};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn spawn_in_own_process_group(args: &[&str]) -> std::process::Child {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    // SAFETY: `pre_exec` runs in the forked child before exec; `setpgid(0,0)`
    // is async-signal-safe and assigns a new process group.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().expect("spawn test process")
}

fn wait_until_dead(child: &mut std::process::Child, max: Duration) -> bool {
    let deadline = Instant::now() + max;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return false,
        }
    }
    matches!(child.try_wait(), Ok(Some(_)))
}

fn wait_until_exists(path: &Path, max: Duration) -> bool {
    let deadline = Instant::now() + max;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    path.exists()
}

fn write_record(run_root: &Path, child: &std::process::Child, adapter_kind: &str) {
    write_record_for_step(run_root, child, adapter_kind, "step-a");
}

fn write_record_for_step(
    run_root: &Path,
    child: &std::process::Child,
    adapter_kind: &str,
    step_id: &str,
) {
    let pid = child.id();
    // SAFETY: `getpgid` reads the process group id of the child; the pid is
    // a live child spawned by this test process.
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    assert!(pgid >= 0, "getpgid returned {pgid}");
    let record = ActiveInvocationRecord {
        schema: ACTIVE_INVOCATION_RECORD_SCHEMA_V1.to_string(),
        invoke_id: format!("test-invoke-{}", pid),
        run_id: "test-run".to_string(),
        step_id: step_id.to_string(),
        adapter_kind: adapter_kind.to_string(),
        pid,
        pgid: Some(pgid as u32),
        executor_pid: std::process::id(),
        started_at_ms: now_ms(),
        process_birth_token: Some(
            transist_adapters::os_process::process_birth_token(pid)
                .expect("read process birth token"),
        ),
    };
    write_active_invocation_record(run_root, &record).expect("write record");
}

fn record_path(run_root: &Path, pid: u32) -> std::path::PathBuf {
    active_invocations_dir(run_root).join(format!("test-invoke-{pid}.json"))
}

#[test]
fn cancels_signal_obedient_child() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_test");
    std::fs::create_dir_all(&run_root).expect("run root");

    // sleep 30 obeys SIGTERM and exits promptly.
    let mut child = spawn_in_own_process_group(&["sleep", "30"]);
    let pid = child.id();
    write_record(&run_root, &child, "process");

    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "step-a",
        Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
    )
    .expect("cancel");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].outcome, CancelOutcome::Terminated);
    assert!(
        wait_until_dead(&mut child, Duration::from_secs(2)),
        "child {pid} should be dead after SIGTERM"
    );

    // The record file must have been removed.
    let dir = active_invocations_dir(&run_root);
    let remaining: Vec<_> = std::fs::read_dir(&dir)
        .map(|iter| {
            iter.filter_map(Result::ok)
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| x == "json")
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        remaining.is_empty(),
        "record should be removed after cancel"
    );

    // Reap to avoid leaving a zombie.
    let _ = child.wait();
}

#[test]
fn escalates_to_sigkill_when_sigterm_is_ignored() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_test");
    let ready_path = tmp.path().join("term-trap-ready");
    std::fs::create_dir_all(&run_root).expect("run root");

    // Keep the group leader itself alive after SIGTERM so the assertion does
    // not depend on how bash resumes a foreground `sleep` after that child is
    // terminated. The ready file prevents cancellation from racing trap setup.
    // The loop is short-lived: cancellation below must SIGKILL it.
    let ready_path = ready_path.to_str().expect("UTF-8 ready path");
    let mut child = spawn_in_own_process_group(&[
        "bash",
        "-c",
        "trap '' TERM; : > \"$1\"; while true; do :; done",
        "bash",
        ready_path,
    ]);
    assert!(
        wait_until_exists(Path::new(ready_path), Duration::from_secs(1)),
        "child should install its SIGTERM trap before cancellation"
    );
    let pid = child.id();
    write_record(&run_root, &child, "process");

    // Use a short grace so the test stays quick; ample SIGKILL timeout below.
    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "step-a",
        Duration::from_millis(100),
    )
    .expect("cancel");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].outcome, CancelOutcome::Killed);

    assert!(
        wait_until_dead(&mut child, Duration::from_secs(3)),
        "child {pid} should be SIGKILL'd after grace"
    );
    let _ = child.wait();
}

#[test]
fn reports_stale_when_pid_already_gone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_test");
    std::fs::create_dir_all(&run_root).expect("run root");

    // Keep the child alive long enough to capture its process group before
    // reaping it, then verify cancellation treats the recorded pid as stale.
    let mut child = spawn_in_own_process_group(&["sleep", "1"]);
    let _pid = child.id();
    write_record(&run_root, &child, "process");
    // Reap the child so its pid is fully released back to the kernel.
    let _ = child.wait();

    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "step-a",
        Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
    )
    .expect("cancel");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].outcome, CancelOutcome::Absent);
}

#[test]
fn refuses_pid_with_mismatched_birth_identity_and_retains_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_test");
    std::fs::create_dir_all(&run_root).expect("run root");

    let mut child = spawn_in_own_process_group(&["sleep", "30"]);
    let pid = child.id();
    write_record(&run_root, &child, "process");
    let path = record_path(&run_root, pid);
    let mut record: ActiveInvocationRecord =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read active record"))
            .expect("parse active record");
    record.process_birth_token = Some("linux:pid-reused".to_string());
    write_active_invocation_record(&run_root, &record).expect("rewrite record");

    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "step-a",
        Duration::from_millis(50),
    )
    .expect("cancel");
    assert!(matches!(
        outcomes[0].outcome,
        CancelOutcome::IdentityMismatch { .. }
            | CancelOutcome::SignalFailure { .. }
            | CancelOutcome::PermissionDenied { .. }
    ));
    assert!(
        path.exists(),
        "unconfirmed cancellation must retain the record"
    );
    assert!(
        child.try_wait().expect("probe child").is_none(),
        "child must not be signalled"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn refuses_legacy_record_without_birth_identity_and_retains_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_test");
    std::fs::create_dir_all(&run_root).expect("run root");

    let mut child = spawn_in_own_process_group(&["sleep", "30"]);
    let pid = child.id();
    write_record(&run_root, &child, "process");
    let path = record_path(&run_root, pid);
    let mut record: ActiveInvocationRecord =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read active record"))
            .expect("parse active record");
    record.process_birth_token = None;
    write_active_invocation_record(&run_root, &record).expect("rewrite legacy record");

    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "step-a",
        Duration::from_millis(50),
    )
    .expect("cancel");
    assert!(matches!(
        outcomes[0].outcome,
        CancelOutcome::IdentityMismatch { .. }
            | CancelOutcome::SignalFailure { .. }
            | CancelOutcome::PermissionDenied { .. }
    ));
    assert!(path.exists(), "legacy record must remain for recovery");
    assert!(
        child.try_wait().expect("probe child").is_none(),
        "child must not be signalled"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn empty_run_root_returns_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_test");
    std::fs::create_dir_all(&run_root).expect("run root");

    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "step-a",
        Duration::from_millis(50),
    )
    .expect("cancel");
    assert!(outcomes.is_empty());
}

#[test]
fn cancellation_is_scoped_to_the_requested_step() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let run_root = tmp.path().join("runs/run_test");
    std::fs::create_dir_all(&run_root).expect("run root");

    let mut step_a = spawn_in_own_process_group(&["sleep", "30"]);
    let mut step_b = spawn_in_own_process_group(&["sleep", "30"]);
    let step_b_pid = step_b.id();
    write_record_for_step(&run_root, &step_a, "process", "step-a");
    write_record_for_step(&run_root, &step_b, "process", "step-b");

    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "step-a",
        Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
    )
    .expect("cancel step a");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].record.step_id, "step-a");
    assert!(wait_until_dead(&mut step_a, Duration::from_secs(2)));
    assert!(
        step_b.try_wait().expect("probe step b").is_none(),
        "step B must not be signalled"
    );
    assert!(
        active_invocations_dir(&run_root)
            .join(format!("test-invoke-{step_b_pid}.json"))
            .exists(),
        "step B active record must remain"
    );

    let _ = step_a.wait();
    let _ = step_b.kill();
    let _ = step_b.wait();
}

#[test]
fn adapter_result_timing_survives_on_disk_cancellation() {
    use std::{collections::BTreeMap, sync::Arc};
    use transist_adapters::ProcessAdapter;
    use transist_core::ports::{ExecutionInvocation, ExecutionPort, RemoteSecretPolicy};

    let tmp = tempfile::tempdir().expect("tempdir");
    let plugin = tmp.path().join("plugin");
    fixture::write_plugin(&plugin);
    transist_plugins::install_plugin_file(tmp.path(), &plugin.join("plugin.yaml"))
        .expect("install");
    let run_root = tmp.path().join(".transist/runs/run_cancel_timing");
    std::fs::create_dir_all(run_root.join("artifacts")).expect("artifacts");
    std::fs::create_dir_all(run_root.join("evidence")).expect("evidence");
    let invocation = ExecutionInvocation {
        request: serde_json::from_value(serde_json::json!({
            "type": "tep.invoke.request.v1", "protocol_version": "tep.v1",
            "invoke_id": "cancel-timing", "run_id": "cancel-timing-run", "workflow_id": "cancel-timing",
            "step": {"id": "invoke", "execution": 1}, "adapter_kind": "process",
            "action": "invoke", "input": {"mode": "pre_terminal"}, "requested_capabilities": [],
            "timeout_ms": 30000,
            "paths": {
                "run_root": run_root, "workspace_dir": tmp.path(),
                "artifacts_dir": run_root.join("artifacts"), "evidence_dir": run_root.join("evidence")
            },
            "policy": {"workspace_declared": true}, "metadata": {"plugin_id": "fixture.lifecycle"}
        }))
        .expect("request"),
        secret_values: BTreeMap::new(),
        remote_secret_policy: RemoteSecretPolicy::default(),
    };
    let before = timing::wall_ms();
    let handle = std::thread::spawn(move || {
        ProcessAdapter::new(Arc::new(LocalActiveRecordStore))
            .execute_step(&invocation)
            .expect("execute")
    });
    assert!(wait_until_exists(
        &tmp.path().join("lifecycle-leader.pid"),
        Duration::from_secs(30)
    ));
    let outcomes = cancel_active_invocations_on_disk(
        &LocalActiveRecordStore,
        &run_root,
        "invoke",
        Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
    )
    .expect("cancel");
    let result = handle.join().expect("join");
    let after = timing::wall_ms();
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0].outcome,
        CancelOutcome::Terminated | CancelOutcome::Killed
    ));
    assert_eq!(result.status, "cancelled");
    assert_eq!(
        result.error.as_ref().expect("error").code,
        transist_protocol::CanonicalErrorCode::Cancelled
    );
    timing::assert_timing(&result.timing, before, after, true);
    assert!(
        !active_invocations_dir(&run_root)
            .join("cancel-timing.json")
            .exists()
    );
    assert!(result.artifacts.is_empty());
    assert!(result.evidence.is_empty());
}
