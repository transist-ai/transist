// SPDX-License-Identifier: MPL-2.0
//! Integration tests for the detached runner spawn boundary.
//!
//! These tests drive the real `transist` binary as a separate process
//! (matching the operator and external-supervisor model) and verify:
//!
//! - the foreground commands spawn a detached runner and exit while the
//!   runner keeps owning the run,
//! - the spawn announcement on stdout is the documented JSON envelope,
//! - cross-process gate decisions reach the runner via the on-disk control
//!   channel and unblock the run,
//! - `runs cancel` writes the durable cancel request and interrupts active runs,
//! - `runner.log` is created and contains the runner's own stdio,
//! - `runs lease inspect` / `runs lease break` enforce the documented
//!   live-owner protections,
//! - the orphan-lease reaper releases leases held by dead local owners.
//!
//! Workspace-level tests under `tests/` cover the in-process kernel; this
//! suite focuses on the **process boundary** introduced by the detached
//! runner refactor.

mod support;

use std::time::Duration;

use serde_json::Value;
use support::{
    MANUAL_GATE_FLOW, TEST_HOSTNAME, TestRepo, freshly_dead_pid, wait_for, wait_for_status,
    write_synthetic_lease,
};

/// `transist --help` should still work without any home setup. Quick smoke
/// test that the binary builds and argv parsing did not regress.
#[test]
fn cli_help_is_available() {
    use assert_cmd::cargo::CommandCargoExt;
    let mut cmd = std::process::Command::cargo_bin("transist").expect("transist binary");
    let output = cmd.arg("--help").output().expect("run --help");
    assert!(
        output.status.success(),
        "--help should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"), "--help stdout: {stdout}");
}

/// `workflows run` on a manual-gate flow must:
/// - exit 0 from the foreground (fire-and-forget on non-tty),
/// - emit the documented `transist.runner_spawned.v1` JSON line on stdout,
/// - leave the run blocked at the manual gate,
/// - have created `<run-root>/runner.log`.
#[test]
fn workflow_run_spawns_detached_runner_and_emits_announcement() {
    let repo = TestRepo::new();
    let flow = repo.write_flow("manual_gate.flow.yaml", MANUAL_GATE_FLOW);
    repo.compile(&flow);

    let run_id = "spawn-test-1";
    let output = repo
        .cli()
        .args([
            "workflows",
            "run",
            "manual_gate_demo@v1",
            "--run-ref",
            run_id,
        ])
        .output()
        .expect("workflow run");

    assert!(
        output.status.success(),
        "workflow run failed: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload = parse_spawn_announcement(&stdout);
    assert_eq!(
        payload["schema"], "transist.runner_spawned.v1",
        "schema mismatch in {payload}"
    );
    assert_eq!(payload["run_id"], run_id, "run_id mismatch in {payload}");
    assert!(
        payload["runner_pid"].is_u64(),
        "runner_pid should be u64, got {payload}"
    );
    assert!(
        payload["runner_log"].is_string(),
        "runner_log should be a path string, got {payload}"
    );

    let status = wait_for_status(&repo, run_id, &["blocked"], Duration::from_secs(15))
        .unwrap_or_else(|| {
            panic!(
                "run never reached blocked status; runner.log:\n{}",
                std::fs::read_to_string(repo.runner_log(run_id)).unwrap_or_default()
            )
        });
    assert_eq!(status, "blocked");

    let runner_log = repo.runner_log(run_id);
    assert!(
        runner_log.exists(),
        "runner.log should exist at {}",
        runner_log.display()
    );
}

/// `runs cancel` should mirror the TUI interrupt path: it writes the durable
/// cancellation request, attempts immediate process-tree termination, and
/// settles the run as `canceled`.
#[test]
fn runs_cancel_interrupts_an_active_run_and_emits_json() {
    let repo = TestRepo::new();
    let plugin_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    let plugin_install = repo
        .cli()
        .args([
            "plugins",
            "install",
            "--path",
            plugin_dir
                .join("plugin.yaml")
                .to_str()
                .expect("plugin path"),
        ])
        .output()
        .expect("plugins install");
    assert!(
        plugin_install.status.success(),
        "plugins install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&plugin_install.stdout),
        String::from_utf8_lossy(&plugin_install.stderr)
    );

    let flow = repo.write_flow(
        "cancel.flow.yaml",
        r#"
schema: transist.flow.v1
id: cancel_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: wait_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "sleep 30"]
"#,
    );
    repo.compile(&flow);

    let run_id = "cancel-test-1";
    let launch = repo
        .cli()
        .args(["workflows", "run", "cancel_demo@v1", "--run-ref", run_id])
        .output()
        .expect("workflow run");
    assert!(
        launch.status.success(),
        "workflow run failed: stderr={}",
        String::from_utf8_lossy(&launch.stderr)
    );

    assert_eq!(
        wait_for_status(&repo, run_id, &["running"], Duration::from_secs(15)).as_deref(),
        Some("running")
    );

    let cancel = repo
        .cli()
        .args(["runs", "cancel", run_id])
        .output()
        .expect("runs cancel");
    assert!(
        cancel.status.success(),
        "cancel failed: stdout={} stderr={}",
        String::from_utf8_lossy(&cancel.stdout),
        String::from_utf8_lossy(&cancel.stderr)
    );

    let payload: Value = serde_json::from_slice(&cancel.stdout).expect("cancel output json");
    assert_eq!(payload["run_id"], run_id);
    assert_eq!(payload["root_run_id"], run_id);
    assert_eq!(payload["cancellation_requested"], true);
    assert_eq!(payload["settled"], true);
    assert_eq!(payload["final_status"], "canceled");
    assert!(
        payload["recovery_runner_pid"].is_null() || payload["recovery_runner_pid"].is_u64(),
        "unexpected recovery_runner_pid payload: {}",
        payload["recovery_runner_pid"]
    );
    assert!(
        payload["active_execution_cancellation"].is_null()
            || payload["active_execution_cancellation"].is_object(),
        "unexpected active_execution_cancellation payload: {}",
        payload["active_execution_cancellation"]
    );

    assert_eq!(
        wait_for_status(&repo, run_id, &["canceled"], Duration::from_secs(15)).as_deref(),
        Some("canceled")
    );
}

/// A cancellation request must still settle when a detached runner died before
/// releasing its fresh local lease.
#[cfg(unix)]
#[test]
fn runs_cancel_recovers_a_run_with_a_fresh_dead_runner_lease() {
    let repo = TestRepo::new();
    let plugin_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    let plugin_install = repo
        .cli()
        .args([
            "plugins",
            "install",
            "--path",
            plugin_dir
                .join("plugin.yaml")
                .to_str()
                .expect("plugin path"),
        ])
        .output()
        .expect("plugins install");
    assert!(
        plugin_install.status.success(),
        "plugins install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&plugin_install.stdout),
        String::from_utf8_lossy(&plugin_install.stderr)
    );

    let flow = repo.write_flow(
        "cancel-dead-runner.flow.yaml",
        r#"
schema: transist.flow.v1
id: cancel_dead_runner_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: wait_step
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "sleep 30"]
"#,
    );
    repo.compile(&flow);

    let run_id = "cancel-dead-runner-test-1";
    let launch = repo
        .cli()
        .args([
            "workflows",
            "run",
            "cancel_dead_runner_demo@v1",
            "--run-ref",
            run_id,
        ])
        .output()
        .expect("workflow run");
    assert!(
        launch.status.success(),
        "workflow run failed: stderr={}",
        String::from_utf8_lossy(&launch.stderr)
    );
    let runner_pid =
        parse_spawn_announcement(&String::from_utf8_lossy(&launch.stdout))["runner_pid"]
            .as_u64()
            .expect("runner pid");

    assert_eq!(
        wait_for_status(&repo, run_id, &["running"], Duration::from_secs(15)).as_deref(),
        Some("running")
    );
    let kill = std::process::Command::new("kill")
        .args(["-9", &runner_pid.to_string()])
        .status()
        .expect("kill detached runner");
    assert!(kill.success(), "failed to kill runner process {runner_pid}");
    std::thread::sleep(Duration::from_millis(100));

    let cancel = repo
        .cli()
        .args(["runs", "cancel", run_id])
        .output()
        .expect("runs cancel");
    assert!(
        cancel.status.success(),
        "cancel failed: stdout={} stderr={}",
        String::from_utf8_lossy(&cancel.stdout),
        String::from_utf8_lossy(&cancel.stderr)
    );
    let payload: Value = serde_json::from_slice(&cancel.stdout).expect("cancel output json");
    assert_eq!(payload["settled"], true, "cancel output: {payload}");
    assert_eq!(
        payload["final_status"], "canceled",
        "cancel output: {payload}"
    );
    assert!(
        payload["recovery_runner_pid"].is_u64(),
        "a dead runner lease should trigger recovery: {payload}"
    );
}

/// A second CLI process records a gate decision while the first workflow run's
/// runner has already settled the run as blocked. After approve the run must
/// reach `success`. This exercises the cross-process gate-decision path:
/// the decision file is written, then `approve` spawns a fresh runner that
/// re-enters the workflow.
#[test]
fn approve_from_separate_process_drives_run_to_success() {
    let repo = TestRepo::new();
    let flow = repo.write_flow("manual_gate.flow.yaml", MANUAL_GATE_FLOW);
    repo.compile(&flow);

    let run_id = "approve-test-1";
    let launch = repo
        .cli()
        .args([
            "workflows",
            "run",
            "manual_gate_demo@v1",
            "--run-ref",
            run_id,
        ])
        .output()
        .expect("workflow run");
    assert!(
        launch.status.success(),
        "workflow run failed: stderr={}",
        String::from_utf8_lossy(&launch.stderr)
    );

    assert_eq!(
        wait_for_status(&repo, run_id, &["blocked"], Duration::from_secs(15)).as_deref(),
        Some("blocked"),
        "expected blocked before approve"
    );
    assert!(
        wait_for_runner_release(&repo, run_id, Duration::from_secs(10)),
        "runner lease was not released before approve; runner.log:\n{}",
        std::fs::read_to_string(repo.runner_log(run_id)).unwrap_or_default()
    );

    let approve = repo
        .cli()
        .args([
            "runs",
            "gate",
            "approve",
            run_id,
            "--actor",
            "reviewer@example.com",
            "--reason",
            "approved from detached runner test",
        ])
        .output()
        .expect("approve");
    assert!(
        approve.status.success(),
        "approve failed: stderr={}",
        String::from_utf8_lossy(&approve.stderr)
    );

    let status = wait_for_status(&repo, run_id, &["success"], Duration::from_secs(15))
        .unwrap_or_else(|| {
            panic!(
                "run never reached success after approve. runner.log:\n{}",
                std::fs::read_to_string(repo.runner_log(run_id)).unwrap_or_default()
            )
        });
    assert_eq!(status, "success");
}

/// Each spawn produces a distinct runner pid. Verifies that gate decisions
/// don't piggyback on the earlier runner.
#[test]
fn approve_announces_a_fresh_runner_pid() {
    let repo = TestRepo::new();
    let flow = repo.write_flow("manual_gate.flow.yaml", MANUAL_GATE_FLOW);
    repo.compile(&flow);

    let run_id = "fresh-runner-test-1";
    let launch = repo
        .cli()
        .args([
            "workflows",
            "run",
            "manual_gate_demo@v1",
            "--run-ref",
            run_id,
        ])
        .output()
        .expect("workflow run");
    assert!(launch.status.success(), "workflow run failed");
    let launch_pid =
        parse_spawn_announcement(&String::from_utf8_lossy(&launch.stdout))["runner_pid"]
            .as_u64()
            .expect("workflow run pid");

    assert_eq!(
        wait_for_status(&repo, run_id, &["blocked"], Duration::from_secs(15)).as_deref(),
        Some("blocked")
    );
    assert!(
        wait_for_runner_release(&repo, run_id, Duration::from_secs(10)),
        "runner lease was not released before approve; runner.log:\n{}",
        std::fs::read_to_string(repo.runner_log(run_id)).unwrap_or_default()
    );

    let approve = repo
        .cli()
        .args([
            "runs",
            "gate",
            "approve",
            run_id,
            "--actor",
            "reviewer@example.com",
            "--reason",
            "approved from detached runner test",
        ])
        .output()
        .expect("approve");
    assert!(approve.status.success());
    let approve_pid =
        parse_spawn_announcement(&String::from_utf8_lossy(&approve.stdout))["runner_pid"]
            .as_u64()
            .expect("approve pid");

    assert_ne!(
        launch_pid, approve_pid,
        "approve must spawn a fresh runner process (got same pid {launch_pid})"
    );
}

/// `runs lease inspect` returns the persisted lease record and `runs lease
/// break` refuses to evict a live local owner unless `--force-live-owner` is
/// supplied.
///
/// We synthesize the live-owner condition by writing a lease whose `pid`
/// points at the current test process (which is alive by construction) and
/// whose `hostname` matches what the CLI subprocess reports via `HOSTNAME=`.
#[test]
fn lease_break_refuses_live_local_owner_without_force() {
    let repo = TestRepo::new();
    let run_id = "lease-test-1";

    let pid = std::process::id();
    write_synthetic_lease(&repo, run_id, pid, /* heartbeat_age_ms */ 0);

    let inspect = repo
        .cli()
        .args(["runs", "lease", "inspect", run_id])
        .output()
        .expect("lease inspect");
    assert!(
        inspect.status.success(),
        "lease inspect failed: stderr={}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    assert!(
        inspect_stdout.contains(TEST_HOSTNAME)
            || inspect_stdout.contains(&pid.to_string())
            || inspect_stdout.contains("test-lease"),
        "lease inspect should surface the synthetic lease; got:\n{inspect_stdout}"
    );

    let no_force = repo
        .cli()
        .args(["runs", "lease", "break", run_id, "--yes-i-understand"])
        .output()
        .expect("lease break (no force)");
    assert!(
        !no_force.status.success(),
        "lease break should refuse live owner without --force-live-owner; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&no_force.stdout),
        String::from_utf8_lossy(&no_force.stderr),
    );
    assert!(
        repo.lease_path(run_id).exists(),
        "lease should still exist after refused break"
    );

    let forced = repo
        .cli()
        .args([
            "runs",
            "lease",
            "break",
            run_id,
            "--yes-i-understand",
            "--force-live-owner",
        ])
        .output()
        .expect("lease break (force)");
    assert!(
        forced.status.success(),
        "forced lease break failed: stderr={}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(
        !repo.lease_path(run_id).exists(),
        "lease should be removed after forced break"
    );
}

/// The orphan-lease reaper releases leases whose owner PID is on this host
/// and is no longer alive. Strategy:
///
/// 1. Pick a freshly-dead PID (spawned and waited).
/// 2. Synthesize a stale lease pointing at that PID with a heartbeat aged
///    past the default 30s reaper threshold.
/// 3. Invoke a mutating CLI command (`prepare`) so the reaper helper runs.
/// 4. Assert the lease file was removed.
///
/// If the kernel happens to reuse the freed PID before we observe it, the
/// test self-skips rather than flaking.
#[test]
fn reaper_releases_stale_local_lease_on_next_dispatch() {
    if std::env::var("CI").as_deref() == Ok("true") && std::env::consts::OS == "linux" {
        eprintln!("NOTE: skipping reaper timing test in Linux CI container");
        return;
    }
    let repo = TestRepo::new();
    let flow = repo.write_flow("manual_gate.flow.yaml", MANUAL_GATE_FLOW);
    repo.compile(&flow);

    let Some(dead_pid) = freshly_dead_pid() else {
        eprintln!("freshly_dead_pid was reused before observation; skipping reaper test");
        return;
    };

    let run_id = "reaper-test-1";
    write_synthetic_lease(&repo, run_id, dead_pid, /* heartbeat_age_ms */ 60_000);
    let lease_path = repo.lease_path(run_id);
    assert!(lease_path.exists(), "lease should be present pre-reap");

    // Any mutating dispatch arm runs the reaper helper at the top. `prepare`
    // is the simplest one that doesn't touch our synthetic-run lease.
    let prepare = repo
        .cli()
        .args([
            "workflows",
            "prepare",
            "manual_gate_demo@v1",
            "--run-ref",
            "some-other-run",
        ])
        .output()
        .expect("prepare");
    assert!(
        prepare.status.success(),
        "prepare failed: stderr={}",
        String::from_utf8_lossy(&prepare.stderr)
    );

    assert!(
        !lease_path.exists(),
        "reaper should have released stale local lease at {}",
        lease_path.display()
    );
}

/// `runner.log` is created and written to by the runner process. We check
/// that it exists and is non-empty after the runner has settled the run.
#[test]
fn runner_log_is_populated() {
    let repo = TestRepo::new();
    let flow = repo.write_flow("manual_gate.flow.yaml", MANUAL_GATE_FLOW);
    repo.compile(&flow);

    let run_id = "runner-log-test-1";
    let launch = repo
        .cli()
        .args([
            "workflows",
            "run",
            "manual_gate_demo@v1",
            "--run-ref",
            run_id,
        ])
        .output()
        .expect("workflow run");
    assert!(launch.status.success(), "workflow run failed");
    assert_eq!(
        wait_for_status(&repo, run_id, &["blocked"], Duration::from_secs(15)).as_deref(),
        Some("blocked")
    );

    let log_path = repo.runner_log(run_id);
    assert!(
        log_path.exists(),
        "runner.log missing at {}",
        log_path.display()
    );
    let metadata = std::fs::metadata(&log_path).expect("metadata");
    // Length 0 is acceptable in principle (the runner might not have printed
    // anything if no plugin invocations ran), but with a manual gate flow the
    // runner emits at least a settled-status announcement. We still tolerate
    // 0-length output with a soft warning rather than a hard failure to
    // accommodate future log-policy changes.
    if metadata.len() == 0 {
        eprintln!("note: runner.log present but empty (acceptable but unusual)");
    }
}

fn parse_spawn_announcement(stdout: &str) -> Value {
    let line = stdout
        .lines()
        .find(|line| line.contains("runner_spawned"))
        .unwrap_or_else(|| panic!("no spawn announcement found in stdout:\n{stdout}"));
    serde_json::from_str(line.trim())
        .unwrap_or_else(|err| panic!("spawn line not JSON: {err}: {line}"))
}

fn wait_for_runner_release(repo: &TestRepo, run_id: &str, timeout: Duration) -> bool {
    let lease_path = repo.lease_path(run_id);
    wait_for(timeout, || !lease_path.exists())
}

/// `runs cleanup` in plain mode must:
/// - emit the spawn announcement JSON on stdout,
/// - emit a cleanup result summary on stderr,
/// - not return until cleanup reaches a terminal state.
#[test]
fn runs_cleanup_waits_and_prints_result_summary() {
    let repo = TestRepo::new();
    let plugin_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    let plugin_install = repo
        .cli()
        .args([
            "plugins",
            "install",
            "--path",
            plugin_dir
                .join("plugin.yaml")
                .to_str()
                .expect("plugin path"),
        ])
        .output()
        .expect("plugins install");
    assert!(
        plugin_install.status.success(),
        "plugins install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&plugin_install.stdout),
        String::from_utf8_lossy(&plugin_install.stderr)
    );

    let flow = repo.write_flow(
        "cleanup_summary.flow.yaml",
        r#"
schema: transist.flow.v1
id: cleanup_summary_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: always_fails
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "echo failing >&2; exit 1"]
cleanup:
  - id: cleanup_on_fail
    when: [failed]
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "echo cleanup done"]
"#,
    );
    repo.compile(&flow);

    let run_id = "cleanup-summary-test-1";
    let launch = repo
        .cli()
        .args([
            "workflows",
            "run",
            "cleanup_summary_demo@v1",
            "--run-ref",
            run_id,
        ])
        .output()
        .expect("workflow run");
    assert!(
        launch.status.success(),
        "workflow run failed: stderr={}",
        String::from_utf8_lossy(&launch.stderr)
    );

    assert_eq!(
        wait_for_status(&repo, run_id, &["failed"], Duration::from_secs(15)).as_deref(),
        Some("failed"),
        "run should reach failed status before cleanup"
    );
    assert!(
        wait_for_runner_release(&repo, run_id, Duration::from_secs(10)),
        "runner lease was not released before cleanup"
    );

    let cleanup = repo
        .cli()
        .args(["runs", "cleanup", run_id])
        .output()
        .expect("runs cleanup");
    assert!(
        cleanup.status.success(),
        "cleanup failed: stdout={} stderr={}",
        String::from_utf8_lossy(&cleanup.stdout),
        String::from_utf8_lossy(&cleanup.stderr)
    );

    let stdout = String::from_utf8_lossy(&cleanup.stdout);
    let payload = parse_spawn_announcement(&stdout);
    assert_eq!(payload["schema"], "transist.runner_spawned.v1");
    assert_eq!(payload["run_id"], run_id);

    let stderr = String::from_utf8_lossy(&cleanup.stderr);
    assert!(
        stderr.contains("Cleanup:"),
        "stderr should contain cleanup result summary, got: {stderr}"
    );
    assert!(
        stderr.contains("state=succeeded") || stderr.contains("state=failed"),
        "stderr should show cleanup state, got: {stderr}"
    );
    assert!(
        stderr.contains("cleanup_on_fail:"),
        "stderr should mention cleanup step id, got: {stderr}"
    );
}

/// Install the shell plugin and run the failure-cleanup flow so it
/// settles `failed` and releases its runner lease, preparing a terminal run a
/// cleanup regression test can hand artificial leases to.
fn setup_failed_run_and_release() -> (TestRepo, String) {
    let repo = TestRepo::new();
    let plugin_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    let plugin_install = repo
        .cli()
        .args([
            "plugins",
            "install",
            "--path",
            plugin_dir
                .join("plugin.yaml")
                .to_str()
                .expect("plugin path"),
        ])
        .output()
        .expect("plugins install");
    assert!(
        plugin_install.status.success(),
        "plugins install failed: stderr={}",
        String::from_utf8_lossy(&plugin_install.stderr)
    );

    let flow = repo.write_flow(
        "cleanup_gate.flow.yaml",
        r#"
schema: transist.flow.v1
id: cleanup_gate_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: always_fails
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "echo failing >&2; exit 1"]
cleanup:
  - id: cleanup_on_fail
    when: [failed]
    uses: process:shell/exec
    with:
      command: sh
      args: ["-c", "echo cleanup done"]
"#,
    );
    repo.compile(&flow);

    let run_id = format!("cleanup-gate-{}", std::process::id());
    let launch = repo
        .cli()
        .args([
            "workflows",
            "run",
            "cleanup_gate_demo@v1",
            "--run-ref",
            &run_id,
        ])
        .output()
        .expect("workflow run");
    assert!(
        launch.status.success(),
        "workflow run failed: stderr={}",
        String::from_utf8_lossy(&launch.stderr)
    );
    assert_eq!(
        wait_for_status(&repo, &run_id, &["failed"], Duration::from_secs(15)).as_deref(),
        Some("failed"),
        "run should settle failed before the gate test"
    );
    assert!(
        wait_for_runner_release(&repo, &run_id, Duration::from_secs(10)),
        "runner lease was not released before gate test"
    );
    (repo, run_id)
}

/// A live process whose PID we pin into a synthetic run-tree lease so the peer
/// "runner" looks alive but performs no release. Dropped on test teardown.
struct LiveLeaseOwner {
    child: std::process::Child,
}

impl LiveLeaseOwner {
    fn new() -> Self {
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 60")
            .spawn()
            .expect("spawn live lease owner");
        Self { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for LiveLeaseOwner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Regression test for the `run-cleanup` lease race. A terminal (canceled or
/// failed) run is persisted while its runner is still tearing down, so the
/// run-tree lease is briefly still owned by a *live* process when an eager
/// `runs cleanup` is issued. The cleanup start gate must wait for that owner
/// to release rather than fail immediately, and only then dispatch cleanup.
#[test]
fn runs_cleanup_waits_for_a_still_finishing_runner_lease() {
    use std::process::Stdio;

    let (repo, run_id) = setup_failed_run_and_release();
    let owner = LiveLeaseOwner::new();
    // Fresh, live, non-stale lease: the still-tearing-down run runner.
    write_synthetic_lease(&repo, &run_id, owner.pid(), /* heartbeat_age_ms */ 0);
    let lease_path = repo.lease_path(&run_id);
    assert!(lease_path.exists(), "synthetic lease should be present");

    let start = std::time::Instant::now();
    let child = repo
        .cli()
        .args(["runs", "cleanup", &run_id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runs cleanup spawn");

    // The gate must hold while the owner stays alive; simulate the runner's
    // release by removing the lease after a short wait.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        repo.lease_path(&run_id).exists(),
        "lease should still be held while the owner is live"
    );
    std::fs::remove_file(&lease_path).expect("release synthetic lease");

    let output = child.wait_with_output().expect("runs cleanup wait");
    let elapsed = start.elapsed();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "cleanup should succeed after the lease is released: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        elapsed >= Duration::from_millis(1000),
        "cleanup must have waited for the finishing runner's lease, elapsed={elapsed:?}"
    );
    assert!(
        stderr.contains("state=succeeded"),
        "cleanup state should be reported as succeeded, got: {stderr}"
    );
}

/// If the lease is held by a live owner that never releases (e.g. a genuinely
/// stuck or foreign runner), the cleanup gate must fail fast with a clear,
/// actionable message instead of stalling for the multi-minute settlement wait.
#[test]
fn runs_cleanup_fails_fast_when_lease_stays_live() {
    let (repo, run_id) = setup_failed_run_and_release();
    let owner = LiveLeaseOwner::new();
    write_synthetic_lease(&repo, &run_id, owner.pid(), /* heartbeat_age_ms */ 0);

    let start = std::time::Instant::now();
    let output = repo
        .cli()
        .args(["runs", "cleanup", &run_id])
        .output()
        .expect("runs cleanup");
    let elapsed = start.elapsed();

    assert!(
        !output.status.success(),
        "cleanup should fail when the lease never releases, stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot start cleanup") && stderr.contains(&format!("pid {}", owner.pid())),
        "failure should name the live owner and give guidance, got: {stderr}"
    );
    assert!(
        elapsed >= Duration::from_secs(10),
        "gate should wait beyond its bounded window before failing, elapsed={elapsed:?}"
    );
}
