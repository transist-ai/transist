// SPDX-License-Identifier: MPL-2.0
// SAFETY: Test: uses libc env mutation for test isolation.
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::thread;

fn e2e_plugin_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn process_shell_plugin_can_be_installed_and_used_without_persisting_secrets() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-plugin-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let plugin_src = repo.path().join("shell-plugin-src");
    write_process_shell_plugin(&plugin_src);
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&plugin_src.join("plugin.yaml"))
        .expect("install");

    let flow = repo.path().join("shell.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: shell_demo
version: v1
plugins:
  shell: 0.1.0
steps:
  - id: invoke
    uses: process:shell/echo
    with:
      args: ["hello", "world"]
      secret_refs: ["API_TOKEN"]
"#,
    )
    .expect("flow");

    // SAFETY: test-only env mutation; no other thread accesses this variable
    // during this synchronous test section.
    unsafe {
        std::env::set_var("API_TOKEN", "topsecret");
    }
    let report = run_compiled_flow(repo.path(), &flow, "shell_demo@v1", "shell-run");
    let run = transist::TransistContext::repo_local(repo.path())
        .runs_show("shell-run")
        .expect("show");
    let run_json = serde_json::Value::from(run);
    if report.status == "success" {
        assert_eq!(
            run_json["steps"]["invoke"]["last_request"]["adapter_kind"],
            "process"
        );
        assert_eq!(
            run_json["steps"]["invoke"]["last_request"]["metadata"]["plugin_id"],
            "shell"
        );
    } else {
        assert!(
            run_json["preflight"]["checks"]
                .as_array()
                .map(|checks| checks.iter().any(|check| {
                    (check["kind"] == "adapter_handshake"
                        && check["status"] == "failed"
                        && check["message"]
                            .as_str()
                            .is_some_and(|message| message.contains("handshake")))
                        || ((check["kind"] == "plugin_contract"
                            || check["kind"] == "request_preparation")
                            && check["status"] == "failed"
                            && check["message"].as_str().is_some_and(|message| {
                                message.contains("escapes plugin directory")
                            }))
                }))
                .unwrap_or(false),
            "unexpected report: {:?}\nrun: {:?}",
            report,
            run_json
        );
    }
    let run_root = repo.path().join(".transist/runs/run_shell-run");
    assert!(!collect_text(&run_root).contains("topsecret"));
}

#[test]
fn process_plugin_request_snapshot_includes_network_policy() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-plugin-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let plugin_src = repo.path().join("local-echo-plugin-src");
    write_local_echo_process_plugin(&plugin_src);
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&plugin_src.join("plugin.yaml"))
        .expect("install");

    let flow = repo.path().join("network.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: shell_network_demo
version: v1
plugins:
  local.echo: 1.0.0
steps:
  - id: invoke
    uses: process:local.echo/echo
    network:
      mode: allowlist
      allow_hosts: [api.example.com]
      allow_ports: [443]
      tls_required: true
      max_redirects: 5
    with:
      args: ["hello"]
"#,
    )
    .expect("flow");
    let report = run_compiled_flow(
        repo.path(),
        &flow,
        "shell_network_demo@v1",
        "shell-network-run",
    );
    let run = transist::TransistContext::repo_local(repo.path())
        .runs_show("shell-network-run")
        .expect("show");
    let run_json = serde_json::Value::from(run);
    assert!(
        run_json["steps"]["invoke"]["last_request"].is_object(),
        "expected last_request snapshot, report={report:?}, run={run_json:?}"
    );
    assert_eq!(
        run_json["steps"]["invoke"]["last_request"]["policy"]["network"]["mode"],
        "allowlist"
    );
    assert_eq!(
        run_json["steps"]["invoke"]["last_request"]["policy"]["network"]["allow_hosts"][0],
        "api.example.com"
    );
    assert_eq!(
        run_json["steps"]["invoke"]["last_request"]["policy"]["network"]["allow_ports"][0],
        443
    );
}

#[test]
fn gate_decision_binding_reaches_the_replayed_staged_plugin_request() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-gate-decision-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());

    let plugin_src = repo.path().join("gate-decision-plugin-src");
    write_gate_decision_process_plugin(&plugin_src);
    let context = transist::TransistContext::repo_local(repo.path());
    context
        .install_plugin_file(&plugin_src.join("plugin.yaml"))
        .expect("install plugin");

    let flow = repo.path().join("gate-decision.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: gate_decision_demo
version: v1
plugins:
  gate.decision.echo: 1.0.0
steps:
  - id: revision_loop
    control: control.noop
    loop:
      body:
        - ref: apply_changes
        - ref: operator_review
      until: 'steps.operator_review.out.decision == "approved"'
      max_iters: 3
      max_duration: 1h
  - id: apply_changes
    uses: process:gate.decision.echo/echo
    gate_decision:
      step: operator_review
    with:
      message: apply the requested changes
  - id: operator_review
    control: control.gate.manual
    with:
      gate_id: implementation-review
  - id: observe_approval
    uses: process:gate.decision.echo/echo
    gate_decision:
      step: operator_review
    with:
      message: confirm approval replaces revision feedback
  - id: finish
    control: control.finish
"#,
    )
    .expect("flow");
    context.compile_flow_file(&flow).expect("compile");
    context
        .prepare_workflow(
            "gate_decision_demo@v1",
            Some("gate-decision-run"),
            BTreeMap::new(),
        )
        .expect("prepare");

    let first = context.run_started_run("gate-decision-run").expect("run");
    assert_eq!(first.status, "blocked");
    let first_run = serde_json::Value::from(context.runs_show("gate-decision-run").expect("show"));
    let first_observed = &first_run["steps"]["apply_changes"]["last_result"]["output"]["observed"];
    assert!(first_observed["input"].get("gate_decision").is_none());
    assert_eq!(first_observed["sandbox"]["private_run_root"], true);
    assert_eq!(first_observed["sandbox"]["durable_state_absent"], true);
    assert_eq!(first_observed["paths"]["run_root"], "[REDACTED]");

    let changed = context
        .request_changes_gate_with_input(
            "gate-decision-run",
            "implementation-review",
            transist::GateDecisionInput {
                actor: Some("reviewer@example.com".to_string()),
                actor_source: Some("test".to_string()),
                reason: Some("retain the boundary regression".to_string()),
                decided_at_ms: Some(1_700_000_000_000),
            },
        )
        .expect("request changes");
    assert_eq!(changed.status, "blocked");

    let replayed_run =
        serde_json::Value::from(context.runs_show("gate-decision-run").expect("show"));
    assert_eq!(replayed_run["steps"]["apply_changes"]["executions"], 2);
    let replayed_observed =
        &replayed_run["steps"]["apply_changes"]["last_result"]["output"]["observed"];
    assert_eq!(
        replayed_observed["input"]["gate_decision"],
        serde_json::json!({
            "schema": "transist.gate_decision.v1",
            "run_id": "gate-decision-run",
            "gate_id": "implementation-review",
            "step_id": "operator_review",
            "decision": "request_changes",
            "actor": "reviewer@example.com",
            "actor_source": "test",
            "reason": "retain the boundary regression",
            "decided_at_ms": 1_700_000_000_000_i64,
        })
    );
    assert_eq!(replayed_observed["sandbox"]["private_run_root"], true);
    assert_eq!(replayed_observed["sandbox"]["durable_state_absent"], true);
    assert_eq!(replayed_observed["paths"]["run_root"], "[REDACTED]");

    let approved = context
        .approve_gate_with_input(
            "gate-decision-run",
            "implementation-review",
            transist::GateDecisionInput {
                actor: Some("reviewer@example.com".to_string()),
                actor_source: Some("test".to_string()),
                reason: Some("approved".to_string()),
                decided_at_ms: Some(1_700_000_000_001),
            },
        )
        .expect("approve");
    assert_eq!(approved.status, "success");
    let completed_run =
        serde_json::Value::from(context.runs_show("gate-decision-run").expect("show"));
    assert_eq!(completed_run["steps"]["apply_changes"]["executions"], 2);
    assert_eq!(completed_run["steps"]["observe_approval"]["executions"], 1);
    let approval_observed =
        &completed_run["steps"]["observe_approval"]["last_result"]["output"]["observed"];
    assert_eq!(
        approval_observed["input"]["gate_decision"],
        serde_json::json!({
            "schema": "transist.gate_decision.v1",
            "run_id": "gate-decision-run",
            "gate_id": "implementation-review",
            "step_id": "operator_review",
            "decision": "approved",
            "actor": "reviewer@example.com",
            "actor_source": "test",
            "reason": "approved",
            "decided_at_ms": 1_700_000_000_001_i64,
        })
    );
}

#[test]
fn local_only_local_process_request_defaults_network_policy_to_loopback() {
    // This test is a required Linux sandbox contract. A missing or broken
    // bwrap loopback handshake fails through the actual execution assertion.
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-plugin-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let plugin_src = repo.path().join("local-echo-plugin-src");
    write_local_echo_process_plugin(&plugin_src);
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&plugin_src.join("plugin.yaml"))
        .expect("install");

    let flow = repo.path().join("local-only-network.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: shell_local_only_demo
version: v1
plugins:
  local.echo: 1.0.0
steps:
  - id: invoke
    uses: process:local.echo/echo
    with:
      args: ["hello"]
"#,
    )
    .expect("flow");
    let report = run_compiled_flow_with_trust(
        repo.path(),
        &flow,
        "shell_local_only_demo@v1",
        "shell-local-only-run",
        false,
        true,
    );
    let run = transist::TransistContext::repo_local(repo.path())
        .runs_show("shell-local-only-run")
        .expect("show");
    let run_json = serde_json::Value::from(run);
    assert!(
        run_json["steps"]["invoke"]["last_request"].is_object(),
        "expected last_request snapshot, report={report:?}, run={run_json:?}"
    );
    assert_eq!(run_json["local_only_mode"], true);
    assert_eq!(
        run_json["steps"]["invoke"]["last_request"]["policy"]["network"]["mode"],
        "loopback"
    );
}

#[test]
fn http_plugin_can_be_installed_and_invoked_under_local_only() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-plugin-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let server = match TcpListener::bind("127.0.0.1:0") {
        Ok(server) => server,
        Err(error) => panic!("bind: {error}"),
    };
    let addr = server.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        // The runtime may refresh handshake and health evidence during both
        // preparation and execution. Serve probes until the one invocation.
        loop {
            let (mut socket, _) = server.accept().expect("accept");
            let request = read_http_request(&mut socket);
            if request.starts_with("GET /app/handshake") {
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.handshake.response.v1",
                        "protocol_version": "app.v1",
                        "plugin": { "id": "http.echo", "version": "1.0.0" },
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
            } else if request.starts_with("GET /app/health") {
                write_http_json(&mut socket, serde_json::json!({ "status": "ok" }));
            } else {
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
                        "artifact_hints": [{ "name": "stdout.txt", "contents": "http-ok" }],
                        "evidence_hints": [],
                        "metrics": {},
                        "usage": null,
                        "error": null
                    }),
                );
                break;
            }
        }
    });
    let plugin_src = repo.path().join("http-plugin-src");
    write_http_plugin(&plugin_src, &format!("http://127.0.0.1:{}", addr.port()));
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&plugin_src.join("plugin.yaml"))
        .expect("install");
    let flow = repo.path().join("http.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: http_demo
version: v1
plugins:
  http.echo: 1.0.0
steps:
  - id: invoke
    uses: http:http.echo/echo
    with:
      message: hello
"#,
    )
    .expect("flow");
    let report =
        run_compiled_flow_with_trust(repo.path(), &flow, "http_demo@v1", "http-run", false, true);
    let run = transist::TransistContext::repo_local(repo.path())
        .runs_show("http-run")
        .expect("show local-only HTTP run");
    let run_json = serde_json::Value::from(run);
    handle.join().expect("join");
    assert_eq!(report.status, "success");
    assert_eq!(
        run_json["steps"]["invoke"]["last_request"]["policy"]["network"]["mode"],
        "loopback"
    );
}

#[test]
fn mcp_plugin_can_be_installed_and_invoked() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-plugin-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let plugin_src = repo.path().join("mcp-plugin-src");
    write_mcp_plugin(&plugin_src);
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&plugin_src.join("plugin.yaml"))
        .expect("install");
    let flow = repo.path().join("mcp.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: mcp_demo
version: v1
plugins:
  mcp.echo: 1.0.0
steps:
  - id: invoke
    uses: mcp:mcp.echo/echo
    with:
      message: hello
"#,
    )
    .expect("flow");
    let report = run_compiled_flow(repo.path(), &flow, "mcp_demo@v1", "mcp-run");
    let run = transist::TransistContext::repo_local(repo.path())
        .runs_show("mcp-run")
        .expect("show");
    let run_json = serde_json::Value::from(run);
    if report.status != "success" {
        assert!(
            run_json["preflight"]["checks"]
                .as_array()
                .map(|checks| checks.iter().any(|check| {
                    (check["kind"] == "adapter_handshake"
                        && check["status"] == "failed"
                        && check["message"]
                            .as_str()
                            .is_some_and(|message| message.contains("handshake")))
                        || ((check["kind"] == "plugin_contract"
                            || check["kind"] == "request_preparation")
                            && check["status"] == "failed"
                            && check["message"].as_str().is_some_and(|message| {
                                message.contains("escapes plugin directory")
                            }))
                }))
                .unwrap_or(false),
            "unexpected report: {:?}\nrun: {:?}",
            report,
            run_json
        );
    }
}

#[test]
fn runtime_temp_process_plugin_uses_private_children_without_persisting_host_paths() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-runtime-temp-process-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let plugin_src = repo.path().join("runtime-temp-process-plugin-src");
    write_local_echo_process_plugin(&plugin_src);
    let manifest_path = plugin_src.join("plugin.yaml");
    let manifest = fs::read_to_string(&manifest_path).expect("manifest");
    fs::write(
        &manifest_path,
        manifest.replace(
            "  command: plugin.sh\n",
            "  command: plugin.sh\n  env:\n    CACHE_DIR: runtime://temp/provider/cache\n    TMPDIR: runtime://temp/provider/cache/tmp\n",
        ),
    )
    .expect("runtime temp manifest");
    let script_path = plugin_src.join("plugin.sh");
    let script = fs::read_to_string(&script_path).expect("script");
    fs::write(
        &script_path,
        script.replace(
            "invoke_id=${invoke_id%%\\\"*}\n",
            "invoke_id=${invoke_id%%\\\"*}\ntest -d \"$CACHE_DIR\" && test -d \"$TMPDIR\" && [ \"$TMPDIR\" = \"$CACHE_DIR/tmp\" ] || exit 1\nprintf 'runtime-temp-sentinel' > \"$TMPDIR/read-write.txt\" && [ \"$(cat \"$TMPDIR/read-write.txt\")\" = 'runtime-temp-sentinel' ] || exit 1\n",
        ),
    )
    .expect("runtime temp script");
    set_executable(&script_path);
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&manifest_path)
        .expect("install");

    let flow = repo.path().join("runtime-temp-process.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: runtime_temp_process
version: v1
plugins:
  local.echo: 1.0.0
steps:
  - id: invoke
    uses: process:local.echo/echo
    with: {}
"#,
    )
    .expect("flow");
    let report = run_compiled_flow(
        repo.path(),
        &flow,
        "runtime_temp_process@v1",
        "runtime-temp-process-run",
    );
    assert_eq!(report.status, "success", "report={report:?}");

    let run_root = repo
        .path()
        .join(".transist/runs/run_runtime-temp-process-run");
    let durable_contents = collect_text(&run_root);
    assert!(
        !durable_contents.contains("transist-runtime-temp-"),
        "durable run data must not contain resolved host paths"
    );
    let installed_manifest = fs::read_to_string(
        transist::repo_local_home_dir(repo.path()).join("plugins/local.echo/1.0.0/plugin.yaml"),
    )
    .expect("installed manifest");
    assert!(installed_manifest.contains("runtime://temp/provider/cache"));
}

#[test]
fn runtime_temp_mcp_plugin_uses_private_children() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-runtime-temp-mcp-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let plugin_src = repo.path().join("runtime-temp-mcp-plugin-src");
    write_mcp_plugin(&plugin_src);
    let manifest_path = plugin_src.join("plugin.yaml");
    let manifest = fs::read_to_string(&manifest_path).expect("manifest");
    fs::write(
        &manifest_path,
        manifest.replace(
            "  command: mcp.sh\n",
            "  command: mcp.sh\n  env:\n    CACHE_DIR: runtime://temp/provider/cache\n    TMPDIR: runtime://temp/provider/cache/tmp\n",
        ),
    )
    .expect("runtime temp manifest");
    let script_path = plugin_src.join("mcp.sh");
    let script = fs::read_to_string(&script_path).expect("script");
    fs::write(
        &script_path,
        script.replace(
            "body=$(read_message) || exit 1\nwrite_message '{\"jsonrpc\":\"2.0\",\"id\":3",
            "body=$(read_message) || exit 1\ntest -d \"$CACHE_DIR\" && test -d \"$TMPDIR\" && [ \"$TMPDIR\" = \"$CACHE_DIR/tmp\" ] || exit 1\nprintf 'runtime-temp-sentinel' > \"$TMPDIR/read-write.txt\" && [ \"$(cat \"$TMPDIR/read-write.txt\")\" = 'runtime-temp-sentinel' ] || exit 1\nwrite_message '{\"jsonrpc\":\"2.0\",\"id\":3",
        ),
    )
    .expect("runtime temp script");
    set_executable(&script_path);
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&manifest_path)
        .expect("install");

    let flow = repo.path().join("runtime-temp-mcp.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: runtime_temp_mcp
version: v1
plugins:
  mcp.echo: 1.0.0
steps:
  - id: invoke
    uses: mcp:mcp.echo/echo
    with: {}
"#,
    )
    .expect("flow");
    let report = run_compiled_flow(
        repo.path(),
        &flow,
        "runtime_temp_mcp@v1",
        "runtime-temp-mcp-run",
    );
    assert_eq!(report.status, "success", "report={report:?}");
}

#[test]
fn old_flow_syntax_using_transport_is_rejected() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-plugin-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let flow = repo.path().join("old.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: old_demo
version: v1
steps:
  - id: invoke
    action: shell.echo
    transport: command
"#,
    )
    .expect("flow");
    assert!(
        transist::TransistContext::repo_local(repo.path())
            .compile_flow_file(&flow)
            .is_err()
    );
}

#[test]
fn built_in_command_adapter_path_is_removed() {
    let _guard = e2e_plugin_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = repo_local_tempdir("e2e-plugin-");
    transist::init_repo(repo.path()).expect("init");
    disable_home_filesystem_policy(repo.path());
    let shell_plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/shell");
    transist::TransistContext::repo_local(repo.path())
        .install_plugin_file(&shell_plugin.join("plugin.yaml"))
        .expect("install");
    let flow = repo.path().join("bad-adapter.flow.yaml");
    fs::write(
        &flow,
        r#"
schema: transist.flow.v1
id: bad_adapter_demo
version: v1
steps:
  - id: invoke
    adapter: command
    plugin: shell
    action: echo
    execution_kind: external
    with:
      args: ["hello"]
"#,
    )
    .expect("flow");
    let err = transist::TransistContext::repo_local(repo.path())
        .compile_flow_file(&flow)
        .expect_err("compile should reject 'command' adapter");
    assert!(
        err.to_string().contains("command") || err.to_string().to_lowercase().contains("adapter"),
        "expected error to mention adapter, got: {err}"
    );
}

fn run_compiled_flow(
    repo_root: &Path,
    flow_path: &Path,
    workflow_ref: &str,
    run_id: &str,
) -> transist::RunReport {
    transist::TransistContext::repo_local(repo_root)
        .compile_flow_file(flow_path)
        .expect("compile");
    let prepared = transist::TransistContext::repo_local(repo_root)
        .prepare_workflow(workflow_ref, Some(run_id), BTreeMap::new())
        .expect("prepare");
    if prepared.status == "failed" {
        return transist::RunReport {
            run_id: prepared.run_id,
            status: prepared.status,
            recovery_state: None,
            cleanup: None,
            steps: prepared.steps,
            lease_release_warning: prepared.lease_release_warning,
        };
    }
    transist::TransistContext::repo_local(repo_root)
        .run_prepared_run(run_id)
        .expect("run")
}

fn run_compiled_flow_with_trust(
    repo_root: &Path,
    flow_path: &Path,
    workflow_ref: &str,
    run_id: &str,
    trusted_execution: bool,
    local_only: bool,
) -> transist::RunReport {
    transist::TransistContext::repo_local(repo_root)
        .compile_flow_file(flow_path)
        .expect("compile");
    let prepared = transist::TransistContext::repo_local(repo_root)
        .prepare_workflow_with_trust(
            workflow_ref,
            Some(run_id),
            BTreeMap::new(),
            trusted_execution,
            local_only,
        )
        .expect("prepare");
    if prepared.status == "failed" {
        return transist::RunReport {
            run_id: prepared.run_id,
            status: prepared.status,
            recovery_state: None,
            cleanup: None,
            steps: prepared.steps,
            lease_release_warning: prepared.lease_release_warning,
        };
    }
    transist::TransistContext::repo_local(repo_root)
        .run_prepared_run_with_trust(run_id, trusted_execution, local_only)
        .expect("run")
}

fn repo_local_tempdir(prefix: &str) -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp-e2e-plugin-runtime");
    fs::create_dir_all(&root).expect("e2e temp root");
    let root = fs::canonicalize(&root).expect("canonical e2e temp root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("tempdir")
}

fn collect_text(root: &Path) -> String {
    let mut out = String::new();
    for entry in walk(root) {
        if entry.is_file()
            && let Ok(contents) = fs::read_to_string(&entry)
        {
            out.push_str(&contents);
        }
    }
    out
}

fn walk(root: &Path) -> Vec<std::path::PathBuf> {
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

fn write_http_json(socket: &mut TcpStream, body: serde_json::Value) {
    let body = serde_json::to_string(&body).expect("json");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).expect("write");
}

fn read_http_request(socket: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = socket.read(&mut chunk).expect("read");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some((headers, body)) = split_http_message(&buffer) {
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")
                        .or_else(|| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if body.len() >= content_length {
                break;
            }
        } else if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(buffer).expect("utf8")
}

fn split_http_message(buffer: &[u8]) -> Option<(String, Vec<u8>)> {
    let split = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let headers = String::from_utf8(buffer[..split].to_vec()).ok()?;
    let body = buffer[split + 4..].to_vec();
    Some((headers, body))
}

fn write_http_plugin(dir: &Path, base_url: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: http.echo
version: 1.0.0
adapter: http
default_provider_kind: local
entrypoint:
  type: http
  base_url: {base_url}
package:
  files: [schemas/input.json, schemas/output.json]
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: true
  supports_artifact_hints: true
  supports_evidence_hints: false
"#
        ),
    )
    .expect("manifest");
}

fn write_process_shell_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(
        dir.join("schemas/echo.input.schema.json"),
        r#"{"type":"object","properties":{"args":{"type":"array","items":{"type":"string"}},"secret_refs":{"type":"array","items":{"type":"string"}}},"required":[],"additionalProperties":true}"#,
    )
    .expect("input");
    fs::write(dir.join("schemas/echo.output.schema.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.sh"),
        r#"#!/bin/sh
IFS= read -r _handshake
printf '%s\n' '{"type":"app.handshake.response.v1","protocol_version":"app.v1","plugin":{"id":"shell","version":"0.1.0"},"adapter_kind":"process","capabilities":{"actions":["echo"],"supports_cancel":true,"supports_health":false,"supports_artifact_hints":true,"supports_evidence_hints":true}}'
IFS= read -r request
invoke_id=${request#*\"invoke_id\":\"}
invoke_id=${invoke_id%%\"*}
secret=${API_TOKEN:-}
printf '%s\n' "{\"type\":\"app.invoke.result.v1\",\"protocol_version\":\"app.v1\",\"invoke_id\":\"$invoke_id\",\"status\":\"success\",\"output\":{\"ok\":true,\"stdout\":\"$secret\",\"stderr\":\"\",\"exit_code\":0},\"artifact_hints\":[{\"name\":\"stdout.txt\",\"contents\":\"$secret\"}],\"evidence_hints\":[],\"metrics\":{},\"usage\":null,\"error\":null}"
"#,
    )
    .expect("plugin script");
    set_executable(&dir.join("plugin.sh"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: shell
version: 0.1.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
package:
  files: [plugin.sh, schemas/echo.input.schema.json, schemas/echo.output.schema.json]
actions:
  - name: echo
    input_schema: schemas/echo.input.schema.json
    output_schema: schemas/echo.output.schema.json
    secret_inputs:
      - name: API_TOKEN
        required: true
        binding:
          type: env
          name: API_TOKEN
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: true
  supports_evidence_hints: true
"#,
    )
    .expect("manifest");
}

fn write_local_echo_process_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(
        dir.join("schemas/echo.input.schema.json"),
        r#"{"type":"object","properties":{"args":{"type":"array","items":{"type":"string"}}},"required":[],"additionalProperties":true}"#,
    )
    .expect("input");
    fs::write(dir.join("schemas/echo.output.schema.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.sh"),
        r#"#!/bin/sh
IFS= read -r _handshake
printf '%s\n' '{"type":"app.handshake.response.v1","protocol_version":"app.v1","plugin":{"id":"local.echo","version":"1.0.0"},"adapter_kind":"process","capabilities":{"actions":["echo"],"supports_cancel":true,"supports_health":false,"supports_artifact_hints":true,"supports_evidence_hints":false}}'
IFS= read -r request
invoke_id=${request#*\"invoke_id\":\"}
invoke_id=${invoke_id%%\"*}
printf '%s\n' "{\"type\":\"app.invoke.result.v1\",\"protocol_version\":\"app.v1\",\"invoke_id\":\"$invoke_id\",\"status\":\"success\",\"output\":{\"stdout\":\"ok\",\"stderr\":\"\",\"exit_code\":0},\"artifact_hints\":[],\"evidence_hints\":[],\"metrics\":{},\"usage\":null,\"error\":null}"
"#,
    )
    .expect("script");
    set_executable(&dir.join("plugin.sh"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: local.echo
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
package:
  files: [plugin.sh, schemas/echo.input.schema.json, schemas/echo.output.schema.json]
actions:
  - name: echo
    input_schema: schemas/echo.input.schema.json
    output_schema: schemas/echo.output.schema.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: true
  supports_evidence_hints: false
"#,
    )
    .expect("manifest");
}

fn write_gate_decision_process_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(
        dir.join("schemas/echo.input.schema.json"),
        r#"{"type":"object","properties":{"message":{"type":"string"},"gate_decision":{"type":"object"}},"required":["message"],"additionalProperties":false}"#,
    )
    .expect("input");
    fs::write(dir.join("schemas/echo.output.schema.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.sh"),
        r#"#!/bin/sh
IFS= read -r _handshake
printf '%s\n' '{"type":"app.handshake.response.v1","protocol_version":"app.v1","plugin":{"id":"gate.decision.echo","version":"1.0.0"},"adapter_kind":"process","capabilities":{"actions":["echo"],"supports_cancel":true,"supports_health":false,"supports_artifact_hints":true,"supports_evidence_hints":false}}'
IFS= read -r request
invoke_id=${request#*\"invoke_id\":\"}
invoke_id=${invoke_id%%\"*}
run_root=${request#*\"run_root\":\"}
run_root=${run_root%%\"*}
private_run_root=false
case "$run_root" in
  */.transist-staging/invoke-*) [ -d "$run_root" ] && private_run_root=true ;;
esac
durable_state_absent=false
[ ! -e "$run_root/state.json" ] && durable_state_absent=true
# Record only nonsecret observations of the raw sandbox request. Echoed private
# paths themselves must still be redacted by the persistence boundary.
observed=${request%\}}
printf '{"type":"app.invoke.result.v1","protocol_version":"app.v1","invoke_id":"%s","status":"success","output":{"observed":%s,"sandbox":{"private_run_root":%s,"durable_state_absent":%s}}},"artifact_hints":[],"evidence_hints":[],"metrics":{},"usage":null,"error":null}\n' "$invoke_id" "$observed" "$private_run_root" "$durable_state_absent"
"#,
    )
    .expect("launcher");
    set_executable(&dir.join("plugin.sh"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: gate.decision.echo
version: 1.0.0
adapter: process
default_provider_kind: local
entrypoint:
  type: process
  command: plugin.sh
package:
  files: [plugin.sh, schemas/echo.input.schema.json, schemas/echo.output.schema.json]
actions:
  - name: echo
    input_schema: schemas/echo.input.schema.json
    output_schema: schemas/echo.output.schema.json
    secret_inputs: []
    bindings:
      transist_gate_decision_v1:
        decision_field: gate_decision
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: true
  supports_evidence_hints: false
"#,
    )
    .expect("manifest");
}

fn write_mcp_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("mcp.sh"),
        r#"#!/bin/bash
read_message() {
    IFS= read -r line || return 1
    printf '%s' "$line"
}

write_message() {
    printf '%s\n' "$1"
}

request_id() {
    python3 -c 'import json, sys; print(json.loads(sys.argv[1])["id"])' "$1"
}

body=$(read_message) || exit 1
write_message "{\"jsonrpc\":\"2.0\",\"id\":$(request_id "$body"),\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"test\",\"version\":\"1.0.0\"}}}"

body=$(read_message) || exit 1
body=$(read_message) || exit 1
write_message "{\"jsonrpc\":\"2.0\",\"id\":$(request_id "$body"),\"result\":{\"tools\":[{\"name\":\"echo_tool\"}]}}"

body=$(read_message) || exit 1
write_message "{\"jsonrpc\":\"2.0\",\"id\":$(request_id "$body"),\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"mcp-ok\"}],\"structuredContent\":{\"stdout\":\"mcp-ok\",\"stderr\":\"\",\"exit_code\":0}}}"
"#,
    )
    .expect("script");
    set_executable(&dir.join("mcp.sh"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: mcp.echo
version: 1.0.0
adapter: mcp
entrypoint:
  type: mcp
  command: mcp.sh
package:
  files: [mcp.sh, schemas/input.json, schemas/output.json]
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
  supports_artifact_hints: true
  supports_evidence_hints: false
"#,
    )
    .expect("manifest");
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

fn disable_home_filesystem_policy(repo_root: &Path) {
    fs::write(
        transist::repo_local_home_dir(repo_root).join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: permissive\n  remote_secret_delivery:\n    process: true\n    http: false\n    mcp: false\n",
    )
    .expect("disable home filesystem policy");
}
