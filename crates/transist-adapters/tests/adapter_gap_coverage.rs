// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};
use time::OffsetDateTime;
use transist_adapters::{DEFAULT_TERMINATION_GRACE_MS, HttpAdapter, McpAdapter, ProcessAdapter};
use transist_core::ports::{
    ExecutionCancelRequest, ExecutionInvocation, ExecutionPort, RemoteSecretPolicy,
};
use transist_plugins::install_plugin_file;
use transist_protocol::{
    AdapterKind, CanonicalErrorCode, ExecutionRequest, InvokePaths, InvokeStep,
    TEP_INVOKE_REQUEST_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
};
use transist_storage_local::LocalActiveRecordStore;

fn process_adapter() -> ProcessAdapter {
    ProcessAdapter::new(Arc::new(LocalActiveRecordStore))
}

fn http_adapter() -> HttpAdapter {
    HttpAdapter::new(Arc::new(LocalActiveRecordStore))
}

fn mcp_adapter() -> McpAdapter {
    McpAdapter::new(Arc::new(LocalActiveRecordStore))
}

fn base_invocation(
    repo_root: &Path,
    invoke_id: &str,
    adapter: AdapterKind,
    plugin: &str,
    action: &str,
    input: serde_json::Value,
) -> ExecutionInvocation {
    let run_root = repo_root.join(".transist/runs/run_gap");
    let artifacts_dir = run_root.join("artifacts/step");
    let evidence_dir = run_root.join("evidence/step");
    fs::create_dir_all(&artifacts_dir).expect("artifacts");
    fs::create_dir_all(&evidence_dir).expect("evidence");
    ExecutionInvocation {
        request: ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: invoke_id.to_string(),
            run_id: "run_gap".to_string(),
            workflow_id: "wf".to_string(),
            step: InvokeStep {
                id: "step".to_string(),
                execution: 1,
            },
            adapter_kind: adapter,
            action: action.to_string(),
            input,
            paths: InvokePaths {
                run_root: run_root.display().to_string(),
                artifacts_dir: artifacts_dir.display().to_string(),
                evidence_dir: evidence_dir.display().to_string(),
                workspace_dir: repo_root.display().to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: Some(5_000),
            max_timeout_ms: None,
            policy: serde_json::json!({}),
            metadata: serde_json::json!({ "plugin_id": plugin }),
        },
        secret_values: BTreeMap::new(),
        remote_secret_policy: RemoteSecretPolicy::default(),
    }
}

fn bind_loopback(test_name: &str) -> TcpListener {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(server) => server,
        Err(error) => panic!("bind {test_name}: {error}"),
    }
}

#[test]
fn process_cancel_returns_cancelled() {
    for network_mode in ["any", "none", "loopback"] {
        assert_cooperative_process_cancel(network_mode);
    }
}

fn assert_cooperative_process_cancel(network_mode: &str) {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_dir = repo.path().join("process-cancel");
    let ready = plugin_dir.join("ready");
    let term = plugin_dir.join("term");
    write_process_wait_plugin(&plugin_dir, &ready, &term, false);
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let adapter = process_adapter();
    let mut invocation = base_invocation(
        repo.path(),
        "invoke_process_cancel",
        AdapterKind::Process,
        "process.wait",
        "wait",
        serde_json::json!({}),
    );
    invocation.request.policy = serde_json::json!({"network": {"mode": network_mode}});
    let adapter_run = adapter.clone();
    let handle = thread::spawn(move || adapter_run.execute_step(&invocation).expect("execute"));
    wait_for_path(&ready);
    assert_eq!(
        fs::read_to_string(&ready).expect("ready"),
        "handles-sigterm"
    );
    let cancel = adapter
        .cancel_step(&ExecutionCancelRequest {
            invoke_id: "invoke_process_cancel".to_string(),
        })
        .expect("cancel");
    assert!(cancel.accepted);
    let result = handle.join().expect("join");
    assert_eq!(result.status, "cancelled");
    assert_eq!(
        result.error.expect("error").code,
        CanonicalErrorCode::Cancelled
    );
    assert!(term.exists());
    assert!(
        term.with_extension("exited").exists(),
        "the cooperative plugin must finish normal SIGTERM cleanup"
    );
    assert!(term.with_extension("child-term").exists());
    assert!(term.with_extension("child-exited").exists());
    assert_process_probe_exited(&ready);
}

#[test]
fn process_termination_executions_graceful_then_force() {
    for network_mode in ["any", "none", "loopback"] {
        assert_forced_process_cancel(network_mode);
    }
}

fn assert_forced_process_cancel(network_mode: &str) {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_dir = repo.path().join("process-force");
    let ready = plugin_dir.join("ready");
    let term = plugin_dir.join("term");
    write_process_wait_plugin(&plugin_dir, &ready, &term, true);
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let adapter = process_adapter();
    let mut invocation = base_invocation(
        repo.path(),
        "invoke_process_force",
        AdapterKind::Process,
        "process.wait",
        "wait",
        serde_json::json!({}),
    );
    invocation.request.policy = serde_json::json!({"network": {"mode": network_mode}});
    let adapter_run = adapter.clone();
    let handle = thread::spawn(move || adapter_run.execute_step(&invocation).expect("execute"));
    wait_for_path(&ready);
    assert_eq!(
        fs::read_to_string(&ready).expect("ready"),
        "ignores-sigterm"
    );
    let start = Instant::now();
    let cancel = adapter
        .cancel_step(&ExecutionCancelRequest {
            invoke_id: "invoke_process_force".to_string(),
        })
        .expect("cancel");
    assert!(cancel.accepted);
    let result = handle.join().expect("join");
    let elapsed = start.elapsed();
    eprintln!(
        "{network_mode}: SIGTERM-ignoring plugin cancelled after {elapsed:?}; configured grace: {DEFAULT_TERMINATION_GRACE_MS} ms"
    );
    assert_eq!(result.status, "cancelled");
    assert_eq!(
        result.error.expect("error").code,
        CanonicalErrorCode::Cancelled
    );
    assert!(
        term.exists(),
        "SIGTERM marker should be written before force kill"
    );
    assert!(
        !term.with_extension("exited").exists(),
        "the SIGTERM-ignoring plugin must be forcibly killed, not exit normally"
    );
    assert!(
        elapsed >= Duration::from_millis(DEFAULT_TERMINATION_GRACE_MS),
        "force kill must wait through the configured {DEFAULT_TERMINATION_GRACE_MS} ms grace; observed {elapsed:?}"
    );
    assert!(term.with_extension("child-term").exists());
    assert!(!term.with_extension("child-exited").exists());
    assert_process_probe_exited(&ready);
}

#[test]
fn http_adapter_injects_header_and_body_secrets_when_policy_allows() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let capture = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = bind_loopback("http_adapter_injects_header_and_body_secrets_when_policy_allows");
    let addr = server.local_addr().expect("addr");
    let capture_thread = capture.clone();
    let handle = thread::spawn(move || {
        for _ in 0..2 {
            let (mut socket, _) = server.accept().expect("accept");
            let request = read_http_request(&mut socket);
            if request.starts_with("GET /app/handshake") {
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.handshake.response.v1",
                        "protocol_version": "app.v1",
                        "plugin": { "id": "http.secure", "version": "1.0.0" },
                        "adapter_kind": "http",
                        "capabilities": {
                            "actions": ["echo"],
                            "supports_cancel": true,
                            "supports_health": true,
                            "supports_artifact_hints": true,
                            "supports_evidence_hints": false
                        }
                    }),
                );
            } else {
                capture_thread.lock().expect("lock").push(request.clone());
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or("{}");
                let invoke: serde_json::Value = serde_json::from_str(body).expect("invoke");
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.invoke.result.v1",
                        "protocol_version": "app.v1",
                        "invoke_id": invoke["invoke_id"],
                        "status": "success",
                        "output": { "stdout": "http-ok", "stderr": "", "exit_code": 0 },
                        "artifact_hints": [],
                        "evidence_hints": [],
                        "metrics": {},
                        "usage": null,
                        "error": null
                    }),
                );
            }
        }
    });
    let plugin_dir = repo.path().join("http-secure");
    write_http_secret_plugin(
        &plugin_dir,
        &format!("http://127.0.0.1:{}", addr.port()),
        None,
    );
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let mut invocation = base_invocation(
        repo.path(),
        "invoke_http_secret",
        AdapterKind::Http,
        "http.secure",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    invocation
        .secret_values
        .insert("AUTH_TOKEN".to_string(), "secret-header".to_string());
    invocation
        .secret_values
        .insert("BODY_TOKEN".to_string(), "secret-body".to_string());
    invocation.remote_secret_policy.http = true;
    let result = http_adapter().execute_step(&invocation).expect("result");
    handle.join().expect("join");
    assert_eq!(result.status, "success");
    let captured = capture.lock().expect("lock").join("\n");
    let captured_lower = captured.to_ascii_lowercase();
    assert!(captured_lower.contains("authorization: secret-header"));
    assert!(captured.contains("\"api_key\":\"secret-body\""));
    assert!(!collect_text(&repo.path().join(".transist/runs")).contains("secret-header"));
    assert!(!collect_text(&repo.path().join(".transist/runs")).contains("secret-body"));
}

#[test]
fn http_adapter_strips_body_field_secret_on_allowed_cross_authority_redirect() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let first_server = bind_loopback("http_adapter_redirect_source");
    let first_addr = first_server.local_addr().expect("first address");
    let second_server = bind_loopback("http_adapter_redirect_target");
    let second_addr = second_server.local_addr().expect("second address");
    let handle = thread::spawn(move || {
        let (mut handshake, _) = first_server.accept().expect("accept handshake");
        let handshake_request = read_http_request(&mut handshake);
        assert!(handshake_request.starts_with("GET /app/handshake"));
        write_http_json(
            &mut handshake,
            serde_json::json!({
                "type": "app.handshake.response.v1",
                "protocol_version": "app.v1",
                "plugin": { "id": "http.secure", "version": "1.0.0" },
                "adapter_kind": "http",
                "capabilities": {
                    "actions": ["echo"],
                    "supports_cancel": true,
                    "supports_health": true,
                    "supports_artifact_hints": false,
                    "supports_evidence_hints": false
                }
            }),
        );

        let (mut invoke, _) = first_server.accept().expect("accept invoke");
        let invoke_request = read_http_request(&mut invoke);
        assert!(invoke_request.starts_with("POST /app/invoke"));
        write_http_redirect(
            &mut invoke,
            &format!("http://127.0.0.1:{}/app/invoke", second_addr.port()),
        );

        let (mut redirected, _) = second_server.accept().expect("accept redirected invoke");
        let redirected_request = read_http_request(&mut redirected);
        assert!(!redirected_request.contains("secret-body"));
        let body = redirected_request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("{}");
        let invoke: serde_json::Value = serde_json::from_str(body).expect("redirected invoke");
        assert!(invoke["input"].get("api_key").is_none());
        write_http_json(
            &mut redirected,
            serde_json::json!({
                "type": "app.invoke.result.v1",
                "protocol_version": "app.v1",
                "invoke_id": invoke["invoke_id"],
                "status": "success",
                "output": { "stdout": "redirected", "stderr": "", "exit_code": 0 },
                "artifact_hints": [],
                "evidence_hints": [],
                "metrics": {},
                "usage": null,
                "error": null
            }),
        );
    });
    let plugin_dir = repo.path().join("http-redirect");
    write_http_secret_plugin(
        &plugin_dir,
        &format!("http://127.0.0.1:{}", first_addr.port()),
        None,
    );
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let mut invocation = base_invocation(
        repo.path(),
        "invoke_http_redirect_secret",
        AdapterKind::Http,
        "http.secure",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    invocation
        .secret_values
        .insert("AUTH_TOKEN".to_string(), "secret-header".to_string());
    invocation
        .secret_values
        .insert("BODY_TOKEN".to_string(), "secret-body".to_string());
    invocation.remote_secret_policy.http = true;
    invocation.request.policy = serde_json::json!({
        "network": {
            "mode": "allowlist",
            "allow_hosts": ["127.0.0.1"],
            "allow_ports": [first_addr.port(), second_addr.port()]
        }
    });
    let result = http_adapter()
        .execute_step(&invocation)
        .expect("adapter result");
    handle.join().expect("join redirect server");
    assert_eq!(result.status, "success");
    assert!(!collect_text(&repo.path().join(".transist/runs")).contains("secret-body"));
    assert!(!collect_text(&repo.path().join(".transist/runs")).contains("secret-header"));
}

#[test]
fn http_adapter_denies_remote_secret_delivery_by_policy() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_dir = repo.path().join("http-deny");
    write_http_secret_plugin(&plugin_dir, "http://127.0.0.1:9", None);
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let mut invocation = base_invocation(
        repo.path(),
        "invoke_http_deny",
        AdapterKind::Http,
        "http.secure",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    invocation
        .secret_values
        .insert("AUTH_TOKEN".to_string(), "secret-header".to_string());
    let result = http_adapter().execute_step(&invocation).expect("result");
    assert_eq!(result.status, "failed");
    assert_eq!(
        result.error.expect("error").code,
        CanonicalErrorCode::ConfigurationError
    );
}

#[test]
fn http_adapter_enforces_network_policy_before_connecting() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let server = bind_loopback("http_adapter_enforces_none_and_allowlist_before_connecting");
    server.set_nonblocking(true).expect("nonblocking listener");
    let addr = server.local_addr().expect("addr");
    let plugin_dir = repo.path().join("http-policy");
    write_basic_http_plugin(
        &plugin_dir,
        &format!("http://127.0.0.1:{}", addr.port()),
        None,
        "http.policy",
    );
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");

    for policy in [
        serde_json::json!({"network": {"mode": "none"}}),
        serde_json::json!({
            "network": {
                "mode": "allowlist",
                "allow_hosts": ["127.0.0.1"],
                "allow_ports": [addr.port() + 1]
            }
        }),
    ] {
        let mut invocation = base_invocation(
            repo.path(),
            "invoke_http_policy",
            AdapterKind::Http,
            "http.policy",
            "echo",
            serde_json::json!({"message": "hello"}),
        );
        invocation.request.policy = policy;
        let result = http_adapter()
            .execute_step(&invocation)
            .expect("adapter result");
        assert_eq!(result.status, "failed");
        assert_eq!(
            result.error.expect("error").code,
            CanonicalErrorCode::TransportError
        );
        assert!(
            matches!(server.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
}

#[test]
fn http_cancel_calls_remote_endpoint_and_returns_cancelled() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let cancel_called = Arc::new(AtomicBool::new(false));
    let invoke_seen = Arc::new(AtomicBool::new(false));
    let server = bind_loopback("http_cancel_calls_remote_endpoint_and_returns_cancelled");
    let addr = server.local_addr().expect("addr");
    let cancel_called_thread = cancel_called.clone();
    let invoke_seen_thread = invoke_seen.clone();
    let handle = thread::spawn(move || {
        for _ in 0..3 {
            let (mut socket, _) = server.accept().expect("accept");
            let request = read_http_request(&mut socket);
            if request.starts_with("GET /app/handshake") {
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.handshake.response.v1",
                        "protocol_version": "app.v1",
                        "plugin": { "id": "http.cancel", "version": "1.0.0" },
                        "adapter_kind": "http",
                        "capabilities": {
                            "actions": ["echo"],
                            "supports_cancel": true,
                            "supports_health": true,
                            "supports_artifact_hints": true,
                            "supports_evidence_hints": false
                        }
                    }),
                );
            } else if request.starts_with("POST /app/cancel") {
                cancel_called_thread.store(true, Ordering::SeqCst);
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or("{}");
                let cancel: serde_json::Value = serde_json::from_str(body).expect("cancel");
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.cancel.result.v1",
                        "protocol_version": "app.v1",
                        "invoke_id": cancel["invoke_id"],
                        "accepted": true
                    }),
                );
            } else {
                invoke_seen_thread.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_secs(2));
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or("{}");
                let invoke: serde_json::Value = serde_json::from_str(body).expect("invoke");
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.invoke.result.v1",
                        "protocol_version": "app.v1",
                        "invoke_id": invoke["invoke_id"],
                        "status": "success",
                        "output": { "stdout": "late", "stderr": "", "exit_code": 0 },
                        "artifact_hints": [],
                        "evidence_hints": [],
                        "metrics": {},
                        "usage": null,
                        "error": null
                    }),
                );
            }
        }
    });
    let plugin_dir = repo.path().join("http-cancel");
    write_basic_http_plugin(
        &plugin_dir,
        &format!("http://127.0.0.1:{}", addr.port()),
        None,
        "http.cancel",
    );
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let adapter = http_adapter();
    let invocation = base_invocation(
        repo.path(),
        "invoke_http_cancel",
        AdapterKind::Http,
        "http.cancel",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let adapter_run = adapter.clone();
    let execute = thread::spawn(move || adapter_run.execute_step(&invocation).expect("execute"));
    wait_for_flag(&invoke_seen);
    let cancel = adapter
        .cancel_step(&ExecutionCancelRequest {
            invoke_id: "invoke_http_cancel".to_string(),
        })
        .expect("cancel");
    assert!(cancel.accepted);
    let result = execute.join().expect("join");
    handle.join().expect("join");
    assert_eq!(result.status, "cancelled");
    assert!(cancel_called.load(Ordering::SeqCst));
}

#[test]
fn http_handshake_and_cancel_do_not_leak_other_action_headers() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let handshake_request = Arc::new(Mutex::new(String::new()));
    let invoke_request = Arc::new(Mutex::new(String::new()));
    let cancel_request = Arc::new(Mutex::new(String::new()));
    let invoke_seen = Arc::new(AtomicBool::new(false));
    let server = bind_loopback("http_handshake_and_cancel_do_not_leak_other_action_headers");
    let addr = server.local_addr().expect("addr");
    let handshake_request_thread = handshake_request.clone();
    let invoke_request_thread = invoke_request.clone();
    let cancel_request_thread = cancel_request.clone();
    let invoke_seen_thread = invoke_seen.clone();
    let handle = thread::spawn(move || {
        for _ in 0..3 {
            let (mut socket, _) = server.accept().expect("accept");
            let request = read_http_request(&mut socket);
            if request.starts_with("GET /app/handshake") {
                *handshake_request_thread.lock().expect("lock") = request;
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.handshake.response.v1",
                        "protocol_version": "app.v1",
                        "plugin": { "id": "http.multi", "version": "1.0.0" },
                        "adapter_kind": "http",
                        "capabilities": {
                            "actions": ["echo", "admin"],
                            "supports_cancel": true,
                            "supports_health": true,
                            "supports_artifact_hints": false,
                            "supports_evidence_hints": false
                        }
                    }),
                );
            } else if request.starts_with("POST /app/cancel") {
                *cancel_request_thread.lock().expect("lock") = request.clone();
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or("{}");
                let cancel: serde_json::Value = serde_json::from_str(body).expect("cancel");
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.cancel.result.v1",
                        "protocol_version": "app.v1",
                        "invoke_id": cancel["invoke_id"],
                        "accepted": true
                    }),
                );
            } else {
                invoke_seen_thread.store(true, Ordering::SeqCst);
                *invoke_request_thread.lock().expect("lock") = request.clone();
                thread::sleep(Duration::from_secs(2));
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or("{}");
                let invoke: serde_json::Value = serde_json::from_str(body).expect("invoke");
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.invoke.result.v1",
                        "protocol_version": "app.v1",
                        "invoke_id": invoke["invoke_id"],
                        "status": "success",
                        "output": { "stdout": "late", "stderr": "", "exit_code": 0 },
                        "artifact_hints": [],
                        "evidence_hints": [],
                        "metrics": {},
                        "usage": null,
                        "error": null
                    }),
                );
            }
        }
    });
    let plugin_dir = repo.path().join("http-multi");
    write_multi_action_http_plugin(&plugin_dir, &format!("http://127.0.0.1:{}", addr.port()));
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let adapter = http_adapter();
    let mut invocation = base_invocation(
        repo.path(),
        "invoke_http_multi",
        AdapterKind::Http,
        "http.multi",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    invocation
        .secret_values
        .insert("ACTION_TOKEN".to_string(), "current-secret".to_string());
    invocation
        .secret_values
        .insert("OTHER_TOKEN".to_string(), "other-secret".to_string());
    invocation.remote_secret_policy.http = true;
    let adapter_run = adapter.clone();
    let execute = thread::spawn(move || adapter_run.execute_step(&invocation).expect("execute"));
    wait_for_flag(&invoke_seen);
    let cancel = adapter
        .cancel_step(&ExecutionCancelRequest {
            invoke_id: "invoke_http_multi".to_string(),
        })
        .expect("cancel");
    assert!(cancel.accepted);
    let result = execute.join().expect("join");
    handle.join().expect("join");
    assert_eq!(result.status, "cancelled");

    let handshake = handshake_request.lock().expect("lock").clone();
    let invoke = invoke_request.lock().expect("lock").clone();
    let cancel = cancel_request.lock().expect("lock").clone();
    let invoke_lower = invoke.to_ascii_lowercase();
    let cancel_lower = cancel.to_ascii_lowercase();

    assert!(!handshake.contains("current-secret"));
    assert!(!handshake.contains("other-secret"));

    assert!(invoke_lower.contains("authorization: current-secret"));
    assert!(!invoke.contains("X-Admin-Token: other-secret"));

    assert!(cancel_lower.contains("authorization: current-secret"));
    assert!(!cancel.contains("X-Admin-Token: other-secret"));

    let persisted = collect_text(&repo.path().join(".transist/runs"));
    assert!(!persisted.contains("current-secret"));
    assert!(!persisted.contains("other-secret"));
}

#[test]
fn http_cancel_failure_maps_to_transport_error() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let invoke_seen = Arc::new(AtomicBool::new(false));
    let server = bind_loopback("http_cancel_failure_maps_to_transport_error");
    let addr = server.local_addr().expect("addr");
    let invoke_seen_thread = invoke_seen.clone();
    let handle = thread::spawn(move || {
        for _ in 0..3 {
            let (mut socket, _) = server.accept().expect("accept");
            let request = read_http_request(&mut socket);
            if request.starts_with("GET /app/handshake") {
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.handshake.response.v1",
                        "protocol_version": "app.v1",
                        "plugin": { "id": "http.cancel.fail", "version": "1.0.0" },
                        "adapter_kind": "http",
                        "capabilities": {
                            "actions": ["echo"],
                            "supports_cancel": true,
                            "supports_health": true,
                            "supports_artifact_hints": false,
                            "supports_evidence_hints": false
                        }
                    }),
                );
            } else if request.starts_with("POST /app/cancel") {
                let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                socket.write_all(response.as_bytes()).expect("write");
            } else {
                invoke_seen_thread.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_secs(2));
            }
        }
    });
    let plugin_dir = repo.path().join("http-cancel-fail");
    write_basic_http_plugin(
        &plugin_dir,
        &format!("http://127.0.0.1:{}", addr.port()),
        None,
        "http.cancel.fail",
    );
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let adapter = http_adapter();
    let invocation = base_invocation(
        repo.path(),
        "invoke_http_cancel_fail",
        AdapterKind::Http,
        "http.cancel.fail",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let adapter_run = adapter.clone();
    let execute = thread::spawn(move || adapter_run.execute_step(&invocation).expect("execute"));
    wait_for_flag(&invoke_seen);
    assert!(
        adapter
            .cancel_step(&ExecutionCancelRequest {
                invoke_id: "invoke_http_cancel_fail".to_string(),
            })
            .is_err()
    );
    let result = execute.join().expect("join");
    handle.join().expect("join");
    assert_eq!(result.status, "failed");
    assert_eq!(
        result.error.expect("error").code,
        CanonicalErrorCode::TransportError
    );
}

#[test]
fn http_adapter_supports_https_and_tls_failures_map_to_transport_error() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let cert = repo.path().join("cert.pem");
    let key = repo.path().join("key.pem");
    generate_test_server_certificate(&cert, &key, false);
    let extra_cert = repo.path().join("extra-cert.pem");
    let extra_key = repo.path().join("extra-key.pem");
    generate_test_server_certificate(&extra_cert, &extra_key, false);
    let server = spawn_python_https_server(&cert, &key);
    let port = server.port;
    wait_for_http_server(&format!("https://localhost:{port}/app/handshake"));

    let plugin_dir = repo.path().join("http-https");
    write_basic_http_plugin(
        &plugin_dir,
        &format!("https://localhost:{port}"),
        Some("cert-bundle.pem"),
        "http.https",
    );
    let ca_bundle = format!(
        "{}{}",
        fs::read_to_string(&extra_cert).expect("read extra cert"),
        fs::read_to_string(&cert).expect("read server cert")
    );
    fs::write(plugin_dir.join("cert-bundle.pem"), ca_bundle).expect("write cert bundle");
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");
    let invocation = base_invocation(
        repo.path(),
        "invoke_https_ok",
        AdapterKind::Http,
        "http.https",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let result = http_adapter().execute_step(&invocation).expect("result");
    assert_eq!(result.status, "success", "{:?}", result.error);

    let wrong_host_plugin_dir = repo.path().join("http-https-wrong-host");
    write_basic_http_plugin(
        &wrong_host_plugin_dir,
        &format!("https://127.0.0.1:{port}"),
        Some("cert.pem"),
        "http.https.wrong-host",
    );
    fs::copy(&cert, wrong_host_plugin_dir.join("cert.pem")).expect("copy wrong-host cert");
    install_plugin_file(repo.path(), &wrong_host_plugin_dir.join("plugin.yaml"))
        .expect("install wrong-host plugin");
    let wrong_host_invocation = base_invocation(
        repo.path(),
        "invoke_https_wrong_host",
        AdapterKind::Http,
        "http.https.wrong-host",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let wrong_host = http_adapter()
        .execute_step(&wrong_host_invocation)
        .expect("result");
    assert_eq!(wrong_host.status, "failed");
    assert_eq!(
        wrong_host.error.expect("error").code,
        CanonicalErrorCode::TransportError
    );

    let bad_plugin_dir = repo.path().join("http-https-bad");
    write_basic_http_plugin(
        &bad_plugin_dir,
        &format!("https://localhost:{port}"),
        None,
        "http.https.bad",
    );
    install_plugin_file(repo.path(), &bad_plugin_dir.join("plugin.yaml")).expect("install bad");
    let bad_invocation = base_invocation(
        repo.path(),
        "invoke_https_bad",
        AdapterKind::Http,
        "http.https.bad",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let bad = http_adapter()
        .execute_step(&bad_invocation)
        .expect("result");
    assert_eq!(bad.status, "failed");
    assert_eq!(
        bad.error.expect("error").code,
        CanonicalErrorCode::TransportError
    );
    let expired_cert = repo.path().join("expired-cert.pem");
    let expired_key = repo.path().join("expired-key.pem");
    generate_test_server_certificate(&expired_cert, &expired_key, true);
    let expired_server = spawn_python_https_server(&expired_cert, &expired_key);
    let expired_port = expired_server.port;
    wait_for_http_server(&format!("https://localhost:{expired_port}/app/handshake"));

    let expired_plugin_dir = repo.path().join("http-https-expired");
    write_basic_http_plugin(
        &expired_plugin_dir,
        &format!("https://localhost:{expired_port}"),
        Some("cert.pem"),
        "http.https.expired",
    );
    fs::copy(&expired_cert, expired_plugin_dir.join("cert.pem")).expect("copy expired cert");
    install_plugin_file(repo.path(), &expired_plugin_dir.join("plugin.yaml"))
        .expect("install expired plugin");
    let expired_invocation = base_invocation(
        repo.path(),
        "invoke_https_expired",
        AdapterKind::Http,
        "http.https.expired",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let expired = http_adapter()
        .execute_step(&expired_invocation)
        .expect("result");
    assert_eq!(expired.status, "failed");
    assert_eq!(
        expired.error.expect("error").code,
        CanonicalErrorCode::TransportError
    );
}

#[test]
fn mcp_cancel_returns_cancelled() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_dir = repo.path().join("mcp-cancel");
    let ready = plugin_dir.join("ready");
    let term = plugin_dir.join("term");
    let cancelled = plugin_dir.join("cancelled");
    write_wait_mcp_plugin(&plugin_dir, &ready, &term, &cancelled);
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");

    let adapter = mcp_adapter();
    let invocation = base_invocation(
        repo.path(),
        "invoke_mcp_cancel",
        AdapterKind::Mcp,
        "mcp.wait",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let adapter_run = adapter.clone();
    let execute = thread::spawn(move || adapter_run.execute_step(&invocation).expect("execute"));
    wait_for_path(&ready);
    let cancel = adapter
        .cancel_step(&ExecutionCancelRequest {
            invoke_id: "invoke_mcp_cancel".to_string(),
        })
        .expect("cancel");
    assert!(cancel.accepted);
    let result = execute.join().expect("join");
    assert_eq!(result.status, "cancelled");
    assert_eq!(
        result.error.expect("error").code,
        CanonicalErrorCode::Cancelled
    );
    assert!(cancelled.exists());
    assert_eq!(
        fs::read_to_string(&cancelled).expect("cancel notification"),
        fs::read_to_string(&ready).expect("active request id")
    );
    assert!(term.exists());
    assert!(term.with_extension("exited").exists());
}

#[test]
fn mcp_timeout_notifies_server_and_terminates_it() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_dir = repo.path().join("mcp-timeout");
    let ready = plugin_dir.join("ready");
    let term = plugin_dir.join("term");
    let cancelled = plugin_dir.join("cancelled");
    write_wait_mcp_plugin(&plugin_dir, &ready, &term, &cancelled);
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");

    let mut invocation = base_invocation(
        repo.path(),
        "invoke_mcp_timeout",
        AdapterKind::Mcp,
        "mcp.wait",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    // Leave startup headroom, but still force the waiting tool request to time
    // out and prove the exact active request receives cancellation and exits.
    invocation.request.timeout_ms = Some(10_000);
    let result = mcp_adapter().execute_step(&invocation).expect("execute");

    assert_eq!(result.status, "failed");
    assert_eq!(
        result.error.expect("error").code,
        CanonicalErrorCode::TimeoutError
    );
    wait_for_path(&ready);
    assert!(cancelled.exists());
    assert_eq!(
        fs::read_to_string(&cancelled).expect("cancel notification"),
        fs::read_to_string(&ready).expect("active request id")
    );
    assert!(term.exists());
    assert!(term.with_extension("exited").exists());
}

#[test]
fn mcp_secret_delivery_is_policy_gated() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_dir = repo.path().join("mcp-secret");
    let capture = repo.path().join("mcp-secret.txt");
    write_secret_mcp_plugin(&plugin_dir, &capture);
    install_plugin_file(repo.path(), &plugin_dir.join("plugin.yaml")).expect("install");

    let mut denied = base_invocation(
        repo.path(),
        "invoke_mcp_deny",
        AdapterKind::Mcp,
        "mcp.secret",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    denied
        .secret_values
        .insert("MCP_TOKEN".to_string(), "mcp-secret".to_string());
    let denied_result = mcp_adapter().execute_step(&denied).expect("result");
    assert_eq!(denied_result.status, "failed");
    assert_eq!(
        denied_result.error.expect("error").code,
        CanonicalErrorCode::ConfigurationError
    );
    assert!(!capture.exists());

    let mut allowed = base_invocation(
        repo.path(),
        "invoke_mcp_allow",
        AdapterKind::Mcp,
        "mcp.secret",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    allowed
        .secret_values
        .insert("MCP_TOKEN".to_string(), "mcp-secret".to_string());
    allowed.remote_secret_policy.mcp = true;
    let allowed_result = mcp_adapter().execute_step(&allowed).expect("result");
    assert_eq!(allowed_result.status, "success");
    assert_eq!(fs::read_to_string(&capture).expect("capture"), "mcp-secret");
    assert!(!collect_text(&repo.path().join(".transist/runs")).contains("mcp-secret"));
}

fn write_process_wait_plugin(dir: &Path, ready: &Path, term: &Path, ignore_term: bool) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.py"),
        format!(
            r#"#!/usr/bin/env python3
import atexit
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

ready = Path(r"{ready}")
term = Path(r"{term}")
ignore_term = {ignore_term}
atexit.register(lambda: term.with_suffix(".exited").write_text("normal-exit"))

def handle_term(signum, frame):
    term.write_text("sigterm")
    if not ignore_term:
        # Cooperative cleanup must have time to complete before parent death.
        time.sleep(0.05)
        sys.exit(0)

signal.signal(signal.SIGTERM, handle_term)

handshake = json.loads(sys.stdin.readline())
print(json.dumps({{
    "type": "app.handshake.response.v1",
    "protocol_version": "app.v1",
    "plugin": {{"id": "process.wait", "version": "1.0.0"}},
    "adapter_kind": "process",
    "capabilities": {{
        "actions": ["wait"],
        "supports_cancel": True,
        "supports_health": False,
        "supports_artifact_hints": False,
        "supports_evidence_hints": False
    }}
}}), flush=True)
invoke = json.loads(sys.stdin.readline())
child_script = '''import atexit, os, signal, sys, time
from pathlib import Path
term = Path(sys.argv[1])
ignore_term = sys.argv[2] == "True"
atexit.register(lambda: term.with_suffix(".child-exited").write_text("normal-exit"))
def handle_term(signum, frame):
    term.with_suffix(".child-term").write_text("sigterm")
    if not ignore_term:
        time.sleep(0.1)
        sys.exit(0)
signal.signal(signal.SIGTERM, handle_term)
term.with_suffix(".child-ready").write_text(str(os.getpid()))
while True:
    time.sleep(0.01)
'''
child = subprocess.Popen([sys.executable, "-c", child_script, str(term), str(ignore_term)], stdin=subprocess.DEVNULL)
deadline = time.monotonic() + 5
while not term.with_suffix(".child-ready").exists():
    if child.poll() is not None or time.monotonic() >= deadline:
        raise RuntimeError("descendant did not install its SIGTERM handler")
    time.sleep(0.01)
ready.with_suffix(".pids").write_text(json.dumps([os.getpid(), child.pid]))
# Publish readiness atomically only after the SIGTERM handler is installed.
ready_tmp = ready.with_suffix(".tmp")
ready_tmp.write_text("ignores-sigterm" if ignore_term else "handles-sigterm")
ready_tmp.replace(ready)
while True:
    time.sleep(0.1)
"#,
            ready = ready.display(),
            term = term.display(),
            ignore_term = if ignore_term { "True" } else { "False" }
        ),
    )
    .expect("plugin");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: process.wait
version: 1.0.0
adapter: process
entrypoint:
  type: process
  command: plugin.py
package:
  files:
    - plugin.py
    - schemas/input.json
    - schemas/output.json
actions:
  - name: wait
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    )
    .expect("manifest");
}

// A zombie has exited and cannot do more work; a live descendant is a leak.
fn assert_process_probe_exited(ready: &Path) {
    #[cfg(target_os = "linux")]
    {
        let pids: Vec<u32> = serde_json::from_str(
            &fs::read_to_string(ready.with_extension("pids")).expect("probe pids"),
        )
        .expect("pid list");
        for pid in pids {
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(stat) => assert!(
                    matches!(
                        stat[stat.rfind(')').expect("comm") + 1..]
                            .split_whitespace()
                            .next(),
                        Some("Z" | "X")
                    ),
                    "plugin process {pid} still live after cancellation: {stat}"
                ),
                Err(error) => panic!("inspect plugin process {pid}: {error}"),
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = ready;
}

fn write_http_secret_plugin(dir: &Path, base_url: &str, ca_cert: Option<&str>) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    let ca_cert_block = ca_cert
        .map(|path| format!("  ca_cert: {path}\n"))
        .unwrap_or_default();
    let package_files = ca_cert.map_or_else(
        || "package:\n  files:\n    - schemas/input.json\n    - schemas/output.json\n".to_string(),
        |path| format!("package:\n  files:\n    - schemas/input.json\n    - schemas/output.json\n    - {path}\n"),
    );
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: http.secure
version: 1.0.0
adapter: http
entrypoint:
  type: http
  base_url: {base_url}
{ca_cert_block}{package_files}actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs:
      - name: AUTH_TOKEN
        required: true
        binding:
          type: header
          name: Authorization
      - name: BODY_TOKEN
        required: false
        binding:
          type: body_field
          name: api_key
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
        ),
    )
    .expect("manifest");
}

fn write_basic_http_plugin(dir: &Path, base_url: &str, ca_cert: Option<&str>, plugin_id: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    let ca_cert_block = ca_cert
        .map(|path| format!("  ca_cert: {path}\n"))
        .unwrap_or_default();
    let package_files = ca_cert.map_or_else(
        || "package:\n  files:\n    - schemas/input.json\n    - schemas/output.json\n".to_string(),
        |path| format!("package:\n  files:\n    - schemas/input.json\n    - schemas/output.json\n    - {path}\n"),
    );
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: {plugin_id}
version: 1.0.0
adapter: http
entrypoint:
  type: http
  base_url: {base_url}
{ca_cert_block}{package_files}actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
        ),
    )
    .expect("manifest");
}

fn write_multi_action_http_plugin(dir: &Path, base_url: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: http.multi
version: 1.0.0
adapter: http
entrypoint:
  type: http
  base_url: {base_url}
package:
  files:
    - schemas/input.json
    - schemas/output.json
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs:
      - name: ACTION_TOKEN
        required: true
        binding:
          type: header
          name: Authorization
    bindings: {{}}
  - name: admin
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs:
      - name: OTHER_TOKEN
        required: true
        binding:
          type: header
          name: X-Admin-Token
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
        ),
    )
    .expect("manifest");
}

fn write_secret_mcp_plugin(dir: &Path, capture: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("server.py"),
        format!(
            r#"#!/usr/bin/env python3
import json
import os
import sys

capture = r"{capture}"

def read_message():
    line = sys.stdin.buffer.readline()
    return json.loads(line.decode()) if line else None

def write_message(payload):
    sys.stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stdout.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"test","version":"1.0.0"}}}}}})
    elif method == "tools/list":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"echo_tool"}}]}}}})
    elif method == "tools/call":
        with open(capture, "w", encoding="utf-8") as handle:
            handle.write(os.environ.get("MCP_TOKEN_ENV", ""))
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"content":[{{"type":"text","text":"mcp-ok"}}],"structuredContent":{{"stdout":"mcp-ok","stderr":"","exit_code":0}}}}}})
        break
"#,
            capture = capture.display()
        ),
    )
    .expect("server");
    make_executable(&dir.join("server.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: mcp.secret
version: 1.0.0
adapter: mcp
entrypoint:
  type: mcp
  command: server.py
package:
  files:
    - server.py
    - schemas/input.json
    - schemas/output.json
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs:
      - name: MCP_TOKEN
        required: true
        binding:
          type: env
          name: MCP_TOKEN_ENV
    bindings:
      tool_name: echo_tool
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    )
    .expect("manifest");
}

fn write_wait_mcp_plugin(dir: &Path, ready: &Path, term: &Path, cancelled: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("server.py"),
        format!(
            r#"#!/usr/bin/env python3
import atexit
import json
import signal
import sys
import time
from pathlib import Path

ready = Path(r"{ready}")
term = Path(r"{term}")
cancelled = Path(r"{cancelled}")
term_received = False
atexit.register(lambda: term.with_suffix(".exited").write_text("normal-exit"))

def handle_term(signum, frame):
    # Never re-enter Python's buffered reader from a signal handler. The main
    # reader drains cancellation before EOF; the handler only records intent.
    global term_received
    term_received = True

signal.signal(signal.SIGTERM, handle_term)

def read_message():
    line = sys.stdin.buffer.readline()
    return json.loads(line.decode()) if line else None

def write_message(payload):
    sys.stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stdout.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"test","version":"1.0.0"}}}}}})
    elif method == "tools/list":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"echo_tool"}}]}}}})
    elif method == "tools/call":
        ready_tmp = ready.with_suffix(".tmp")
        ready_tmp.write_text(str(message["id"]))
        ready_tmp.replace(ready)
        while True:
            cancellation = read_message()
            if cancellation is None:
                break
            if cancellation.get("method") == "notifications/cancelled":
                cancelled.write_text(str(cancellation["params"]["requestId"]))
        while not term_received:
            time.sleep(0.01)
        term.write_text("sigterm")
        break
"#,
            ready = ready.display(),
            term = term.display(),
            cancelled = cancelled.display()
        ),
    )
    .expect("server");
    make_executable(&dir.join("server.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: mcp.wait
version: 1.0.0
adapter: mcp
entrypoint:
  type: mcp
  command: server.py
package:
  files:
    - server.py
    - schemas/input.json
    - schemas/output.json
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings:
      tool_name: echo_tool
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    )
    .expect("manifest");
}

fn generate_test_server_certificate(cert: &Path, key: &Path, expired: bool) {
    let mut params =
        CertificateParams::new(vec!["localhost".to_string()]).expect("certificate parameters");
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    if expired {
        params.not_before = OffsetDateTime::from_unix_timestamp(946_684_800).expect("not before");
        params.not_after = OffsetDateTime::from_unix_timestamp(946_684_801).expect("not after");
    }
    let key_pair = KeyPair::generate().expect("generate test key");
    let certificate = params
        .self_signed(&key_pair)
        .expect("generate test certificate");
    fs::write(cert, certificate.pem()).expect("write test certificate");
    fs::write(key, key_pair.serialize_pem()).expect("write test key");
}

struct PythonHttpsServer {
    child: Child,
    port: u16,
}

impl Drop for PythonHttpsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_python_https_server(cert: &Path, key: &Path) -> PythonHttpsServer {
    let mut child = Command::new("python3")
        .arg("-u")
        .arg("-c")
        .arg(
            r#"
import http.server
import json
import ssl
import sys

cert = sys.argv[1]
key = sys.argv[2]

class Handler(http.server.BaseHTTPRequestHandler):
    def _write(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/app/handshake":
            self._write({
                "type": "app.handshake.response.v1",
                "protocol_version": "app.v1",
                "plugin": {"id": "http.https", "version": "1.0.0"},
                "adapter_kind": "http",
                "capabilities": {
                    "actions": ["echo"],
                    "supports_cancel": True,
                    "supports_health": True,
                    "supports_artifact_hints": False,
                    "supports_evidence_hints": False,
                },
            })
        else:
            self.send_error(404)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        payload = json.loads(body or b"{}")
        if self.path == "/app/invoke":
            self._write({
                "type": "app.invoke.result.v1",
                "protocol_version": "app.v1",
                "invoke_id": payload["invoke_id"],
                "status": "success",
                "output": {"stdout": "https-ok", "stderr": "", "exit_code": 0},
                "artifact_hints": [],
                "evidence_hints": [],
                "metrics": {},
                "usage": None,
                "error": None,
            })
        else:
            self.send_error(404)

    def log_message(self, fmt, *args):
        pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(certfile=cert, keyfile=key)
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_address[1], flush=True)
server.serve_forever()
"#,
        )
        .arg(cert)
        .arg(key)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("python https server");
    let stdout = child.stdout.take().expect("python https server stdout");
    let mut port = String::new();
    BufReader::new(stdout)
        .read_line(&mut port)
        .expect("read python https server port");

    PythonHttpsServer {
        child,
        port: port.trim().parse().expect("python https server port"),
    }
}

fn wait_for_http_server(url: &str) {
    let port = url
        .strip_prefix("https://localhost:")
        .and_then(|value| value.split('/').next())
        .and_then(|value| value.parse::<u16>().ok())
        .expect("test server URL should contain a port");
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("server did not start in time");
}

fn write_http_json(socket: &mut TcpStream, body: serde_json::Value) {
    let body = serde_json::to_string(&body).expect("json");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).expect("write");
}

fn write_http_redirect(socket: &mut TcpStream, location: &str) {
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    socket
        .write_all(response.as_bytes())
        .expect("write redirect");
}

const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;

fn read_http_request(socket: &mut TcpStream) -> String {
    read_http_request_with_timeout(socket, Duration::from_secs(5)).expect("read mock HTTP request")
}

// These fixtures use Content-Length framing and one request per connection.
// Bound both size and total read time so incomplete requests fail instead of hanging.
fn read_http_request_with_timeout(socket: &mut TcpStream, timeout: Duration) -> io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let mut request_length = None;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(length) = request_length {
            if buffer.len() >= length {
                buffer.truncate(length);
                return String::from_utf8(buffer).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 HTTP request")
                });
            }
        } else if buffer.len() >= MAX_HTTP_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mock HTTP headers exceed size limit",
            ));
        }

        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "mock HTTP request timed out")
            })?;
        socket.set_read_timeout(Some(remaining))?;
        let read_limit = chunk
            .len()
            .min(request_length.unwrap_or(MAX_HTTP_HEADER_BYTES) - buffer.len());
        let read = match socket.read(&mut chunk[..read_limit]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "mock HTTP request timed out",
                ));
            }
            result => result?,
        };
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete mock HTTP request",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if request_length.is_none() {
            if let Some(split) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&buffer[..split]).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 HTTP headers")
                })?;
                request_length = Some(split + 4 + http_content_length(headers)?);
            }
        }
    }
}

fn http_content_length(headers: &str) -> io::Result<usize> {
    let mut content_length = None;
    for line in headers.lines().skip(1) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed HTTP header"))?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mock HTTP fixture does not support Transfer-Encoding",
            ));
        }
        if !name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = value.trim();
        if content_length.is_some()
            || value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid or duplicate Content-Length",
            ));
        }
        let length = value
            .parse::<usize>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
        if length > MAX_HTTP_BODY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mock HTTP body exceeds size limit",
            ));
        }
        content_length = Some(length);
    }
    Ok(content_length.unwrap_or(0))
}

fn mock_http_connection() -> (TcpStream, TcpStream) {
    let server = bind_loopback("mock HTTP request reader");
    let client = TcpStream::connect(server.local_addr().expect("address")).expect("connect");
    let (socket, _) = server.accept().expect("accept");
    (socket, client)
}

// Keep the client open unless testing EOF; independently bound each regression's wait.
fn capture_mock_http_request(
    request: &[u8],
    close_write: bool,
    timeout: Duration,
) -> io::Result<String> {
    let (mut socket, mut client) = mock_http_connection();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(read_http_request_with_timeout(&mut socket, timeout))
            .expect("send request read result");
    });
    client.write_all(request).expect("write request");
    if close_write {
        client.shutdown(Shutdown::Write).expect("shutdown write");
    }
    let result = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("mock HTTP request reader must finish within the bound");
    handle.join().expect("join request reader");
    result
}

#[test]
fn mock_http_reader_reads_fragmented_large_body_by_content_length() {
    let payload = serde_json::json!({"message": "λ".repeat(4096), "suffix": "complete"});
    let body = serde_json::to_string(&payload).expect("body");
    assert!(body.len() > 1024);
    for header_name in ["content-length", "CoNtEnT-LeNgTh", "Content-Length"] {
        let headers = format!(
            "POST /app/invoke HTTP/1.1\r\nHost: localhost\r\n{header_name}: {}\r\n\r\n",
            body.len()
        );
        let (mut socket, mut client) = mock_http_connection();
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            sender
                .send(read_http_request(&mut socket))
                .expect("send request");
        });

        // Split the header terminator, then pause with only part of the body sent.
        let split = headers.len() - 2;
        client
            .write_all(&headers.as_bytes()[..split])
            .expect("write partial headers");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        client
            .write_all(&headers.as_bytes()[split..])
            .expect("write remaining headers");
        client
            .write_all(&body.as_bytes()[..256])
            .expect("write partial body");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let mut remaining = body.as_bytes()[256..].to_vec();
        remaining.extend_from_slice(b"not part of the declared body");
        client.write_all(&remaining).expect("write remaining body");
        // The peer is still open: completing the request must not require EOF.
        let request = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("read complete request without waiting for EOF");
        handle.join().expect("join request reader");
        assert_eq!(request, format!("{headers}{body}"));
        let (_, captured_body) = request.split_once("\r\n\r\n").expect("body separator");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(captured_body).expect("complete JSON body"),
            payload
        );
    }
}

#[test]
fn mock_http_reader_finishes_bodyless_requests_without_eof() {
    for headers in [
        "GET /app/handshake HTTP/1.1\r\nHost: localhost\r\n\r\n",
        "POST /app/cancel HTTP/1.1\r\ncontent-length: 0\r\n\r\n",
    ] {
        let request = format!("{headers}not part of the declared body");
        assert_eq!(
            capture_mock_http_request(request.as_bytes(), false, Duration::from_secs(1))
                .expect("bodyless request"),
            headers
        );
    }
}

#[test]
fn mock_http_reader_rejects_malformed_and_oversized_framing() {
    for header in [
        "content-length: invalid".to_string(),
        "content-length: ".to_string(),
        "content-length: +1".to_string(),
        "content-length: -1".to_string(),
        "content-length: 1\r\nContent-Length: 2".to_string(),
        format!("content-length: {}0", usize::MAX),
        format!("content-length: {}", MAX_HTTP_BODY_BYTES + 1),
        "transfer-encoding: chunked".to_string(),
        "malformed-header".to_string(),
        format!("X-Large: {}", "x".repeat(MAX_HTTP_HEADER_BYTES)),
    ] {
        let request = format!("POST /app/invoke HTTP/1.1\r\n{header}\r\n\r\n");
        let error = capture_mock_http_request(request.as_bytes(), false, Duration::from_secs(1))
            .expect_err("invalid framing must fail while the peer remains open");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

#[test]
fn mock_http_reader_rejects_truncated_headers_and_body() {
    for request in [
        "POST /app/invoke HTTP/1.1\r\ncontent-length: 4\r\n",
        "POST /app/invoke HTTP/1.1\r\ncontent-length: 4\r\n\r\nab",
    ] {
        let error = capture_mock_http_request(request.as_bytes(), true, Duration::from_secs(1))
            .expect_err("truncated request must fail");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}

#[test]
fn mock_http_reader_times_out_on_stalled_headers_and_body() {
    for request in [
        "POST /app/invoke HTTP/1.1\r\ncontent-length: 4\r\n",
        "POST /app/invoke HTTP/1.1\r\ncontent-length: 4\r\n\r\nab",
    ] {
        let error =
            capture_mock_http_request(request.as_bytes(), false, Duration::from_millis(100))
                .expect_err("stalled request must time out while the peer remains open");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("path '{}' was not created in time", path.display());
}

fn wait_for_flag(flag: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("flag was not raised in time");
}

fn collect_text(root: &Path) -> String {
    let mut out = String::new();
    for entry in walk(root) {
        if entry.is_file() {
            if let Ok(contents) = fs::read_to_string(&entry) {
                out.push_str(&contents);
            }
        }
    }
    out
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                paths.extend(walk(&path));
            } else {
                paths.push(path);
            }
        }
    }
    paths
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
