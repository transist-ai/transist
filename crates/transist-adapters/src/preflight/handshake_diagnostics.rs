// SPDX-License-Identifier: MPL-2.0
//! Offline APP handshake diagnostics through the real adapter entrypoints.

use super::tests::target;
use crate::ProcessAdapter;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf, process::Command, sync::Arc};
use transist_core::PreflightStatus;
use transist_core::ports::{
    ExecutionInvocation, ExecutionPort, PreflightInvocation, PreflightPort,
};
use transist_plugins::PluginEntrypoint;
use transist_protocol::{CanonicalErrorCode, ExecutionRequest};
use transist_storage_local::LocalActiveRecordStore;

const STDERR_SENTINEL: &str = "synthetic-handshake-stderr-sentinel";
const CLOSED: &str = "plugin stdout closed before handshake response";
const TIMEOUT: &str = "timed out waiting for handshake response";

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    request: ExecutionRequest,
}

impl Fixture {
    /// Builds an installed package from the shared manifest/request fixture.
    fn new(mode: &str) -> Self {
        let root = tempfile::tempdir_in(std::env::temp_dir()).expect("fixture root");
        // /tmp is a symlink on macOS; sandbox grants must use canonical paths.
        let base = root.path().canonicalize().expect("canonical fixture root");
        let home = base.join("home");
        let workspace = base.join("workspace");
        let plugin = home.join("plugins/plugin/1.0.0");
        let run = home.join("runs/run-1");
        for dir in [
            &plugin,
            &workspace,
            &run.join("artifacts"),
            &run.join("evidence"),
        ] {
            fs::create_dir_all(dir).expect("fixture directory");
        }
        let mut fixture = target();
        fixture.manifest.entrypoint = PluginEntrypoint::Process {
            command: "plugin".into(),
            args: vec![mode.into()],
            cwd: None,
            env: BTreeMap::new(),
        };
        fixture.manifest.package.files =
            vec!["plugin".into(), "input.json".into(), "output.json".into()];
        fixture.manifest.actions = vec![fixture.action];
        fs::write(plugin.join("input.json"), r#"{"type":"object"}"#).unwrap();
        fs::write(plugin.join("output.json"), r#"{"type":"object"}"#).unwrap();
        fs::write(
            plugin.join("plugin.yaml"),
            yaml_serde::to_string(&fixture.manifest).unwrap(),
        )
        .unwrap();
        fs::write(plugin.join("plugin"), SCRIPT).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(plugin.join("plugin"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let request = &mut fixture.request;
        request.paths.run_root = run.display().to_string();
        request.paths.artifacts_dir = run.join("artifacts").display().to_string();
        request.paths.evidence_dir = run.join("evidence").display().to_string();
        request.paths.workspace_dir = workspace.display().to_string();
        request.policy = json!({"workspace_declared": true});
        request.metadata["plugin_version"] = "1.0.0".into();
        // Enough for sandbox/interpreter startup, far below the fixture's alarm.
        request.timeout_ms = Some(if mode == "timeout" { 1_000 } else { 5_000 });
        Self {
            _root: root,
            workspace,
            request: fixture.request,
        }
    }

    /// Checks process state after each entrypoint, then returns recorded frames.
    fn finished_messages(&self) -> Vec<Value> {
        let pid = fs::read_to_string(self.workspace.join("fixture.pid"))
            .expect("fixture must start, not silently skip sandbox setup");
        let state = process_state(&pid);
        assert!(
            state.is_empty() || state.starts_with('Z'),
            "fixture survived adapter return: pid={pid}, state={state}"
        );
        fs::read_to_string(self.workspace.join("stdin.jsonl"))
            .expect("recorded stdin")
            .lines()
            .map(|line| serde_json::from_str(line).expect("recorded JSON frame"))
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A failing cleanup assertion must not leak the bounded fixture while
        // the rest of the library tests continue. Do not mask the assertion.
        if let Ok(pid) = fs::read_to_string(self.workspace.join("fixture.pid")) {
            let state = process_state(&pid);
            if !state.is_empty() && !state.starts_with('Z') {
                let _ = Command::new("kill").args(["-KILL", pid.trim()]).status();
            }
        }
    }
}

/// Observe whether a recorded fixture PID remains live without assuming a signal.
fn process_state(pid: &str) -> String {
    let pid: u32 = pid.trim().parse().expect("numeric fixture PID");
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("probe fixture process state");
    // ps exits 1 for a PID which is already gone.
    assert!(output.status.success() || output.status.code() == Some(1));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Runs both public boundaries with separate disposable processes and homes.
fn assert_case(mode: &str, expected_error: Option<CanonicalErrorCode>, exit_code: Option<i32>) {
    let adapter = ProcessAdapter::new(Arc::new(LocalActiveRecordStore));
    let preflight = Fixture::new(mode);
    let checks = adapter
        .preflight_steps(&[PreflightInvocation {
            request: preflight.request.clone(),
            available_secret_refs: vec![],
            remote_secret_policy: Default::default(),
        }])
        .expect("preflight report");
    let frames = preflight.finished_messages();
    assert_eq!(frames.len(), 1, "preflight must never invoke: {frames:?}");
    assert_eq!(frames[0]["type"], "app.handshake.request.v1");
    let handshakes: Vec<_> = checks
        .iter()
        .filter(|c| c.kind == "adapter_handshake")
        .collect();
    assert_eq!(handshakes.len(), 1, "checks: {checks:?}");
    let check = handshakes[0];
    assert_eq!(check.step_ids, ["step-1"]);
    assert_eq!(check.adapter_kind.as_deref(), Some("process"));
    assert_eq!(check.plugin_id.as_deref(), Some("plugin"));
    assert_eq!(check.action.as_deref(), Some("run"));
    assert_eq!(
        check.status,
        if expected_error.is_some() {
            PreflightStatus::Failed
        } else {
            PreflightStatus::Passed
        }
    );
    assert!(
        checks
            .iter()
            .all(|c| c.kind == "adapter_handshake" || c.status == PreflightStatus::Passed),
        "{checks:?}"
    );
    assert!(checks.iter().all(|c| !c.message.contains(STDERR_SENTINEL)));
    assert_message(mode, &check.message);
    if mode == "exit7" {
        assert!(
            check
                .message
                .contains("plugin process exited with exit status: 7"),
            "{}",
            check.message
        );
    }

    let invocation = Fixture::new(mode);
    let result = adapter
        .execute_step(&ExecutionInvocation {
            request: invocation.request.clone(),
            secret_values: BTreeMap::new(),
            remote_secret_policy: Default::default(),
        })
        .expect("execution result");
    let frames = invocation.finished_messages();
    assert_eq!(frames[0]["type"], "app.handshake.request.v1");
    assert_eq!(result.exit_code, exit_code);
    if let Some(code) = expected_error {
        assert_eq!(result.status, "failed");
        let error = result.error.expect("canonical handshake error");
        assert_eq!(error.code, code);
        assert_message(mode, &error.message);
        assert_eq!(
            frames.len(),
            1,
            "failed handshake must not invoke: {frames:?}"
        );
        assert_eq!(result.output, json!({}));
        if mode == "exit7" {
            assert!(result.stderr.contains(STDERR_SENTINEL));
        }
    } else {
        assert_eq!(result.status, "success");
        assert!(result.error.is_none());
        assert_eq!(result.output, json!({"value": "offline-success"}));
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1]["type"], "app.invoke.request.v1");
    }
}

/// Check the expected diagnostic without conflating transport and timeout failures.
fn assert_message(mode: &str, message: &str) {
    match mode {
        "exit7" => assert!(message.contains(CLOSED), "{message}"),
        "timeout" => assert!(message.contains(TIMEOUT), "{message}"),
        "malformed" => assert!(message.contains("failed decoding JSON line"), "{message}"),
        "success" => assert_eq!(message, "APP handshake succeeded"),
        _ => panic!("unknown fixture mode"),
    }
    if mode != "timeout" {
        assert!(!message.contains("timed out"), "{message}");
    }
    if mode != "exit7" {
        assert!(!message.contains("stdout closed"), "{message}");
    }
}

#[test]
fn handshake_diagnostic_early_exit_is_transport_error() {
    assert_case("exit7", Some(CanonicalErrorCode::TransportError), Some(2));
}

#[test]
fn handshake_diagnostic_open_stdout_is_timeout() {
    assert_case("timeout", Some(CanonicalErrorCode::TimeoutError), Some(124));
}

#[test]
fn handshake_diagnostic_valid_handshake_and_result_succeed() {
    assert_case("success", None, None);
}

#[test]
fn handshake_diagnostic_malformed_json_is_protocol_error() {
    assert_case(
        "malformed",
        Some(CanonicalErrorCode::ProtocolError),
        Some(2),
    );
}

const SCRIPT: &str = r#"#!/usr/bin/env python3
import json, os, signal, sys

signal.alarm(15)
mode = sys.argv[1]
with open('fixture.pid', 'w') as f:
    f.write(str(os.getpid()))

def receive():
    line = sys.stdin.readline()
    if not line:
        return None
    with open('stdin.jsonl', 'a') as f:
        f.write(line)
    return json.loads(line)

def emit(value):
    print(json.dumps(value), flush=True)

handshake = receive()
assert handshake['type'] == 'app.handshake.request.v1'
if mode == 'exit7':
    print('synthetic-handshake-stderr-sentinel', file=sys.stderr, flush=True)
    sys.exit(7)
if mode == 'timeout':
    # Record any unexpected invoke while keeping stdout open past the deadline,
    # including after stdin closes. The alarm bounds a broken cleanup path.
    while receive() is not None:
        pass
    while True:
        signal.pause()
if mode == 'malformed':
    print('{malformed-json', flush=True)
    sys.exit(0)

emit({'type':'app.handshake.response.v1', 'protocol_version':'app.v1',
      'plugin':{'id':'plugin','version':'1.0.0'}, 'adapter_kind':'process',
      'capabilities':{'actions':['run'], 'supports_cancel':False,
                      'supports_health':False, 'supports_artifact_hints':False,
                      'supports_evidence_hints':False}})
request = receive()
if request is not None:
    assert request['type'] == 'app.invoke.request.v1'
    emit({'type':'app.invoke.result.v1', 'protocol_version':'app.v1',
          'invoke_id':request['invoke_id'], 'status':'success',
          'output':{'value':'offline-success'}, 'artifact_hints':[],
          'evidence_hints':[], 'metrics':{}, 'usage':None, 'error':None})
"#;
