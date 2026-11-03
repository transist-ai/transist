// SPDX-License-Identifier: MPL-2.0
//! Real-CLI cancellation settlement and cleanup-recovery regressions.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use support::{TestRepo, wait_for, wait_for_status};

const RUN_ID: &str = "cancel-cleanup-test";
const PRIMARY_SCRIPT: &str = r#"
: > primary.started
n=0
until [ -f primary.release ]; do
    n=$((n + 1))
    [ "$n" -lt 600 ] || exit 97
    sleep 0.05
done
: > primary.finished
"#;
const CLEANUP_SCRIPT: &str = r#"
: > cleanup.started
n=0
until [ -f cleanup.release ]; do
    n=$((n + 1))
    [ "$n" -lt 600 ] || exit 98
    sleep 0.05
done
: > cleanup.finished
printf 'cleanup committed\n'
"#;

/// Release shell gates even when an assertion fails, so detached work can exit.
struct ReleaseMarkers(PathBuf);

impl Drop for ReleaseMarkers {
    fn drop(&mut self) {
        for marker in ["primary.release", "cleanup.release"] {
            let _ = std::fs::write(self.0.join(marker), b"");
        }
    }
}

fn setup_flow(cleanup_script: &str) -> TestRepo {
    let repo = TestRepo::new();
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    let install = repo
        .cli()
        .args(["plugins", "install", "--path"])
        .arg(plugin_dir.join("plugin.yaml"))
        .output()
        .expect("install shell plugin");
    assert_success(&install);

    let flow = format!(
        r#"
schema: transist.flow.v1
id: cancel_cleanup_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: primary
    uses: process:shell/exec
    with:
      command: sh
      args: {}
cleanup:
  - id: cleanup_on_cancel
    when: [canceled]
    uses: process:shell/exec
    with:
      command: sh
      args: {}
"#,
        json!(["-c", PRIMARY_SCRIPT]),
        json!(["-c", cleanup_script])
    );
    let path = repo.write_flow("cancel-cleanup.flow.yaml", &flow);
    repo.compile(&path);
    repo
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed: status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cancel(repo: &TestRepo) -> Output {
    repo.cli()
        .current_dir(&repo.repo_root)
        .args(["runs", "cancel", RUN_ID])
        .output()
        .expect("runs cancel")
}

fn cancel_report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "cancel report is not JSON: {error}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn wait_for_caller(mut child: Child) -> Output {
    let exited = wait_for(Duration::from_secs(20), || {
        child
            .try_wait()
            .expect("poll cancellation caller")
            .is_some()
    });
    if !exited {
        let _ = child.kill();
    }
    let output = child
        .wait_with_output()
        .expect("collect cancellation report");
    assert!(
        exited,
        "cancellation caller did not exit: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Prepare without a runner, then model a crash after primary cancellation was
/// persisted but before cleanup was initialized. No runtime owns this fixture.
fn prepare_canceled_without_cleanup(repo: &TestRepo) {
    let prepare = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args([
            "workflows",
            "prepare",
            "cancel_cleanup_demo@v1",
            "--run-ref",
            RUN_ID,
        ])
        .output()
        .expect("prepare cancellation crash fixture");
    assert_success(&prepare);
    assert!(!repo.lease_path(RUN_ID).exists());
    let mut state = repo.read_state(RUN_ID);
    assert!(state["cleanup"].is_null(), "unexpected cleanup: {state}");
    state["status"] = json!("canceled");
    std::fs::write(
        repo.run_root(RUN_ID).join("state.json"),
        serde_json::to_vec_pretty(&state).expect("serialize canceled fixture"),
    )
    .expect("persist canceled fixture");
}

#[test]
fn runs_cancel_reports_failed_cleanup_without_rewriting_canceled_outcome() {
    let repo = setup_flow(": > cleanup.started; exit 23");
    let _release = ReleaseMarkers(repo.repo_root.clone());
    let launch = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args([
            "workflows",
            "run",
            "cancel_cleanup_demo@v1",
            "--run-ref",
            RUN_ID,
        ])
        .output()
        .expect("launch workflow");
    assert_success(&launch);
    assert!(
        wait_for(Duration::from_secs(15), || {
            repo.repo_root.join("primary.started").exists()
        }),
        "primary invocation never started"
    );
    assert_eq!(
        wait_for_status(&repo, RUN_ID, &["running"], Duration::from_secs(5)).as_deref(),
        Some("running")
    );

    let output = cancel(&repo);
    assert!(!output.status.success(), "failed cleanup must fail cancel");
    let report = cancel_report(&output);
    assert_eq!(report["run_id"], RUN_ID);
    assert_eq!(report["root_run_id"], RUN_ID);
    assert_eq!(report["cancellation_requested"], true);
    assert_eq!(report["settled"], false);
    assert_eq!(report["final_status"], "canceled");
    assert_eq!(report["cleanup_state"], "failed");
    let guidance = format!("transist runs cleanup {RUN_ID}");
    assert!(
        report["error"]
            .as_str()
            .expect("cleanup failure guidance")
            .contains(&guidance),
        "missing recovery command: {report}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains(&guidance));
    assert!(repo.repo_root.join("cleanup.started").exists());
    assert!(!repo.repo_root.join("primary.finished").exists());
    assert!(wait_for(Duration::from_secs(10), || {
        !repo.lease_path(RUN_ID).exists()
    }));
    let state = repo.read_state(RUN_ID);
    assert_eq!(state["status"], "canceled");
    assert_eq!(state["cleanup"]["state"], "failed");
    let cleanup_step = &state["cleanup"]["steps"]["cleanup_on_cancel"];
    assert_eq!(cleanup_step["status"], "failed");
    assert_eq!(cleanup_step["executions"], 1);

    // Repeating cancellation observes the failure; only explicit cleanup retries.
    let repeated = cancel(&repo);
    assert!(!repeated.status.success());
    let repeated = cancel_report(&repeated);
    assert_eq!(repeated["settled"], false);
    assert_eq!(repeated["final_status"], "canceled");
    assert_eq!(repeated["cleanup_state"], "failed");
    assert!(repeated["recovery_runner_pid"].is_null());
    assert!(repeated["active_execution_cancellation"].is_null());
    assert_eq!(repo.read_state(RUN_ID)["cleanup"], state["cleanup"]);
}

#[test]
fn runs_cancel_recovers_missing_cleanup_and_waits_for_commit_and_lease_release() {
    let repo = setup_flow(CLEANUP_SCRIPT);
    let _release = ReleaseMarkers(repo.repo_root.clone());
    prepare_canceled_without_cleanup(&repo);
    let mut caller = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args(["runs", "cancel", RUN_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cancellation caller");

    assert!(
        wait_for(Duration::from_secs(10), || {
            repo.repo_root.join("cleanup.started").exists()
        }),
        "recovery never dispatched cleanup; runner.log:\n{}",
        std::fs::read_to_string(repo.runner_log(RUN_ID)).unwrap_or_default()
    );
    assert!(
        caller.try_wait().expect("poll gated caller").is_none(),
        "cancel returned before cleanup was released"
    );
    assert!(
        repo.lease_path(RUN_ID).exists(),
        "cleanup must own the lease"
    );
    let running = repo.read_state(RUN_ID);
    assert_eq!(running["status"], "canceled");
    assert_eq!(running["cleanup"]["trigger"], "canceled");
    assert_eq!(running["cleanup"]["state"], "running");
    let running_step = &running["cleanup"]["steps"]["cleanup_on_cancel"];
    assert_eq!(running_step["status"], "running");
    assert!(running_step["last_request"]["invoke_id"].is_string());
    let execution_path = repo
        .run_root(RUN_ID)
        .join("step_executions/cleanup_on_cancel/execution_001.json");
    assert!(
        !execution_path.exists(),
        "gated cleanup cannot have committed its result yet"
    );
    assert!(!repo.repo_root.join("cleanup.finished").exists());
    assert!(!repo.repo_root.join("primary.started").exists());
    assert_eq!(
        transist::resolve_cancellation_target_in_home(&repo.home_dir, RUN_ID)
            .expect("resolve target while canceled cleanup is active"),
        None,
        "canceled cleanup must never become a primary cancellation target"
    );
    assert!(
        !repo
            .home_dir
            .join("control/runs")
            .join(RUN_ID)
            .join("cancel-request.json")
            .exists(),
        "repeated cancellation must not publish another control request"
    );

    std::fs::write(repo.repo_root.join("cleanup.release"), b"").expect("release cleanup gate");
    let output = wait_for_caller(caller);
    assert_success(&output);
    let report = cancel_report(&output);
    assert_eq!(report["run_id"], RUN_ID);
    assert_eq!(report["root_run_id"], RUN_ID);
    assert_eq!(report["cancellation_requested"], true);
    assert_eq!(report["settled"], true);
    assert_eq!(report["final_status"], "canceled");
    assert_eq!(report["cleanup_state"], "succeeded");
    assert!(
        report["recovery_runner_pid"].is_u64(),
        "no recovery: {report}"
    );
    assert!(report["active_execution_cancellation"].is_null());
    assert!(report["active_execution_cancellation_error"].is_null());
    assert!(report["error"].is_null());
    assert!(repo.repo_root.join("cleanup.finished").exists());
    assert!(
        !repo.lease_path(RUN_ID).exists(),
        "settled before lease release"
    );
    let state = repo.read_state(RUN_ID);
    assert_eq!(state["status"], "canceled");
    assert_eq!(state["cleanup"]["state"], "succeeded");
    let cleanup_step = &state["cleanup"]["steps"]["cleanup_on_cancel"];
    assert_eq!(cleanup_step["executions"], 1);
    assert_eq!(cleanup_step["status"], "succeeded");
    assert_eq!(state["steps"]["primary"]["executions"], 0);
    assert!(!repo.repo_root.join("primary.started").exists());
    let execution: Value = serde_json::from_slice(
        &std::fs::read(&execution_path).expect("read committed cleanup execution"),
    )
    .expect("parse committed cleanup execution");
    assert_eq!(execution["result"]["status"], "success");
    assert_eq!(
        execution["result"]["output"]["stdout"],
        "cleanup committed\n"
    );

    let repeated = cancel(&repo);
    assert_success(&repeated);
    let repeated = cancel_report(&repeated);
    assert_eq!(repeated["settled"], true);
    assert_eq!(repeated["cleanup_state"], "succeeded");
    assert!(repeated["recovery_runner_pid"].is_null());
    assert!(repeated["active_execution_cancellation"].is_null());
    assert_eq!(repo.read_state(RUN_ID)["cleanup"], state["cleanup"]);
    assert!(!repo.lease_path(RUN_ID).exists());
}
