// SPDX-License-Identifier: MPL-2.0
//! Shared test fixtures for `transist-cli` integration tests.
//!
//! All tests here drive the actual `transist` binary via `assert_cmd`, mirroring
//! how an operator (or another CLI process) interacts with a run. The kernel
//! itself is exercised in detail by the workspace-level tests under
//! `tests/`; this crate's suite focuses on the **process boundary**: detached
//! runner spawn, on-disk control channel, lease semantics, and runner.log.

// SAFETY: Test: uses libc::kill for test runner liveness checks.
#![allow(unsafe_code)]
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::Value;
use tempfile::TempDir;

/// Hostname value the test forces into the CLI subprocess via `HOSTNAME=`.
/// We use the same value when synthesizing lease records so the reaper's
/// "same-host" check passes deterministically.
pub const TEST_HOSTNAME: &str = "transist-cli-tests";

/// A scratch repo with an initialized Transist home, ready to receive flows.
pub struct TestRepo {
    _tmp: TempDir,
    pub repo_root: PathBuf,
    pub home_dir: PathBuf,
}

impl TestRepo {
    pub fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let repo_root = tmp.path().to_path_buf();
        transist::init_home_dir_with_mode(
            &transist::repo_local_home_dir(&repo_root),
            transist::RuntimeOperatingMode::Standalone,
        )
        .expect("init repo");
        let home_dir = transist::repo_local_home_dir(&repo_root);
        Self {
            _tmp: tmp,
            repo_root,
            home_dir,
        }
    }

    /// Build a `transist` binary command preconfigured with `--home <home>` and
    /// the deterministic `HOSTNAME` the synthetic-lease tests rely on.
    pub fn cli(&self) -> StdCommand {
        let mut cmd = StdCommand::cargo_bin("transist").expect("transist binary");
        cmd.arg("--home").arg(&self.home_dir);
        cmd.env("HOSTNAME", TEST_HOSTNAME);
        cmd.env_remove("TRANSIST_REAPER_DISABLE");
        cmd.env_remove("TRANSIST_REAPER_STALE_AFTER_MS");
        cmd
    }

    /// Write a flow file into the repo root.
    pub fn write_flow(&self, file_name: &str, contents: &str) -> PathBuf {
        let path = self.repo_root.join(file_name);
        fs::write(&path, contents).expect("write flow");
        path
    }

    /// Compile a flow via the in-process API. Used for setup; the actual
    /// behavior under test is the spawned `transist` binary.
    pub fn compile(&self, flow_path: &Path) {
        let ctx = transist::TransistContext::repo_local(&self.repo_root);
        ctx.compile_flow_file(flow_path).expect("compile flow");
    }

    pub fn run_root(&self, run_id: &str) -> PathBuf {
        transist::TransistContext::with_home(&self.repo_root, &self.home_dir)
            .run_root(run_id)
            .expect("resolve run root")
    }

    pub fn runner_log(&self, run_id: &str) -> PathBuf {
        self.run_root(run_id).join("runner.log")
    }

    pub fn lease_path(&self, run_id: &str) -> PathBuf {
        self.home_dir
            .join("locks")
            .join("runs")
            .join(format!("{run_id}.lock"))
    }

    pub fn read_state(&self, run_id: &str) -> Value {
        let raw =
            fs::read_to_string(self.run_root(run_id).join("state.json")).expect("read state.json");
        serde_json::from_str(&raw).expect("parse state.json")
    }
}

/// A trivially-blocked flow: a single manual gate. Running it always settles
/// the run as `blocked` until an `approve`/`reject`/`request-changes` decision
/// is recorded.
pub const MANUAL_GATE_FLOW: &str = r#"
schema: transist.flow.v1
id: manual_gate_demo
version: v1
steps:
  - id: gate
    control: control.gate.manual
    with:
      gate_id: review
"#;

/// Block the calling thread until `predicate` returns true or the deadline
/// elapses. Polls every 50ms.
pub fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait for `state.json[status]` to match one of the expected values.
pub fn wait_for_status(
    repo: &TestRepo,
    run_id: &str,
    expected: &[&str],
    timeout: Duration,
) -> Option<String> {
    let mut last = String::new();
    let ok = wait_for(timeout, || {
        let state_path = repo.run_root(run_id).join("state.json");
        let Ok(raw) = fs::read_to_string(&state_path) else {
            return false;
        };
        let Ok(val): Result<Value, _> = serde_json::from_str(&raw) else {
            return false;
        };
        let status = val
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        last = status.clone();
        expected.iter().any(|exp| *exp == status)
    });
    ok.then_some(last)
}

/// Returns true if the given pid is currently alive (Unix only).
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` checks liveness without sending a signal.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}

/// Synthesize a `RunTreeLeaseRecord` JSON file at the lease path. Caller picks
/// the pid + heartbeat age. Hostname is fixed to [`TEST_HOSTNAME`] so the
/// reaper's same-host check passes.
pub fn write_synthetic_lease(repo: &TestRepo, run_id: &str, owner_pid: u32, heartbeat_age_ms: u64) {
    let lease_path = repo.lease_path(run_id);
    if let Some(parent) = lease_path.parent() {
        fs::create_dir_all(parent).expect("lease parent dir");
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    let heartbeat_ms = now_ms.saturating_sub(heartbeat_age_ms);
    let payload = serde_json::json!({
        "schema_version": "transist.run_tree_lease.v1",
        "root_run_id": run_id,
        "lease_id": "test-lease",
        "owner_id": "test-owner",
        "management_mode": "standalone",
        "pid": owner_pid,
        "hostname": TEST_HOSTNAME,
        "command": "synthetic",
        "transist_version": "test",
        "acquired_at_ms": heartbeat_ms,
        "last_heartbeat_ms": heartbeat_ms,
    });
    fs::write(&lease_path, serde_json::to_vec_pretty(&payload).unwrap())
        .expect("write synthetic lease");
}

/// Returns a pid that has very recently exited and is unlikely to be reused
/// before the immediate test step that uses it. Returns `None` if pid_alive
/// reports the slot has already been reused.
pub fn freshly_dead_pid() -> Option<u32> {
    let mut child = StdCommand::new("/usr/bin/true")
        .spawn()
        .expect("spawn /usr/bin/true");
    let pid = child.id();
    let _ = child.wait();
    std::thread::sleep(Duration::from_millis(50));
    if pid_alive(pid) { None } else { Some(pid) }
}
