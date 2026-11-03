// SPDX-License-Identifier: MPL-2.0
// SAFETY: Test: uses libc env mutation for sandbox test isolation.
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use std::net::{IpAddr, UdpSocket};

use transist_adapters::{HttpAdapter, McpAdapter, ProcessAdapter, validate_static_preflight_steps};
use transist_core::ports::{
    ExecutionInvocation, ExecutionPort, PreflightInvocation, PreflightPort, RemoteSecretPolicy,
};
use transist_core::{PreflightCheck, PreflightStatus};
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

fn request(
    repo_root: &Path,
    adapter: AdapterKind,
    plugin: &str,
    action: &str,
    input: serde_json::Value,
) -> ExecutionInvocation {
    let run_root = repo_root.join(".transist/runs/run_test");
    let artifacts_dir = run_root.join("artifacts/step");
    let evidence_dir = run_root.join("evidence/step");
    fs::create_dir_all(&artifacts_dir).expect("artifacts");
    fs::create_dir_all(&evidence_dir).expect("evidence");
    write_run_state_and_recipe(&run_root, "wf", "v1", adapter.as_str(), plugin, action);
    ExecutionInvocation {
        request: ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
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
            // Functional fixtures use the ordinary adapter budget. Tests of
            // deadline behavior override it explicitly below.
            timeout_ms: Some(30_000),
            max_timeout_ms: None,
            policy: serde_json::json!({}),
            metadata: serde_json::json!({ "plugin_id": plugin }),
        },
        secret_values: BTreeMap::new(),
        remote_secret_policy: RemoteSecretPolicy::default(),
    }
}

fn preflight_request(
    repo_root: &Path,
    adapter: AdapterKind,
    plugin: &str,
    action: &str,
    input: serde_json::Value,
) -> PreflightInvocation {
    let invocation = request(repo_root, adapter, plugin, action, input);
    PreflightInvocation {
        request: invocation.request,
        available_secret_refs: Vec::new(),
        remote_secret_policy: invocation.remote_secret_policy,
    }
}

fn bind_loopback(test_name: &str) -> TcpListener {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(server) => server,
        Err(error) => panic!("bind {test_name}: {error}"),
    }
}

fn find_check<'a>(checks: &'a [PreflightCheck], kind: &str) -> &'a PreflightCheck {
    checks
        .iter()
        .find(|check| check.kind == kind)
        .unwrap_or_else(|| panic!("missing preflight check '{kind}'"))
}

fn assert_check_occurs_once(checks: &[PreflightCheck], kind: &str) {
    assert_eq!(
        checks.iter().filter(|check| check.kind == kind).count(),
        1,
        "preflight check '{kind}' must occur once: {checks:?}"
    );
}

fn add_plugin_binary_prerequisite(plugin_dir: &Path) {
    let manifest_path = plugin_dir.join("plugin.yaml");
    let manifest = fs::read_to_string(&manifest_path).expect("manifest");
    fs::write(
        manifest_path,
        manifest.replace(
            "actions:",
            "prerequisites:\n  binaries:\n    - name: sh\nactions:",
        ),
    )
    .expect("add prerequisite");
}

fn configure_input_projection(dir: &Path, binding: Option<serde_json::Value>, strict: bool) {
    let path = dir.join("plugin.yaml");
    let mut manifest: transist_plugins::PluginManifest =
        yaml_serde::from_str(&fs::read_to_string(&path).expect("manifest"))
            .expect("parse manifest");
    if let Some(binding) = binding {
        manifest.actions[0]
            .bindings
            .insert("input_projection".into(), binding);
    }
    fs::write(
        path,
        yaml_serde::to_string(&manifest).expect("serialize manifest"),
    )
    .expect("manifest");
    if strict {
        fs::write(dir.join("schemas/input.json"), r#"{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}"#).expect("strict input");
    }
}

fn add_projection_secret(dir: &Path, binding: serde_json::Value) {
    let path = dir.join("plugin.yaml");
    let mut manifest: transist_plugins::PluginManifest =
        yaml_serde::from_str(&fs::read_to_string(&path).expect("manifest"))
            .expect("parse manifest");
    manifest.actions[0].secret_inputs.push(
        serde_json::from_value(serde_json::json!({
            "name": "API_KEY", "required": true, "binding": binding
        }))
        .expect("secret input"),
    );
    fs::write(
        path,
        yaml_serde::to_string(&manifest).expect("serialize manifest"),
    )
    .expect("manifest");
}

#[test]
fn input_projection_static_validation_is_strict_and_never_probes_any_adapter() {
    for adapter in [AdapterKind::Process, AdapterKind::Http, AdapterKind::Mcp] {
        for opted_in in [false, true] {
            let repo = sandbox_tempdir("projection-static-");
            let source = repo.path().join("source");
            let id = match adapter {
                AdapterKind::Process => {
                    write_process_no_handshake_plugin(&source, "process.echo");
                    "process.echo"
                }
                AdapterKind::Http => {
                    write_http_plugin(&source, "http://127.0.0.1:1");
                    "http.echo"
                }
                AdapterKind::Mcp => {
                    write_mcp_plugin(&source);
                    "mcp.echo"
                }
            };
            configure_input_projection(
                &source,
                opted_in.then(|| serde_json::json!("action_data_v1")),
                true,
            );
            install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install");
            let mut invocation = preflight_request(
                repo.path(),
                adapter,
                id,
                "echo",
                serde_json::json!({"query": "hello", "secret_refs": ["API_KEY"]}),
            );
            invocation.available_secret_refs = vec!["API_KEY".into()];
            let original = serde_json::to_vec(&invocation.request).expect("original");
            let before = directory_snapshot(repo.path());
            let checks = validate_static_preflight_steps(&[invocation.clone()]).expect("static");
            assert_eq!(
                find_check(&checks, "input_schema").status,
                if opted_in {
                    PreflightStatus::Passed
                } else {
                    PreflightStatus::Failed
                }
            );
            assert!(checks.iter().all(|check| check.kind != "adapter_probe"));
            assert_eq!(directory_snapshot(repo.path()), before);
            assert_eq!(serde_json::to_vec(&invocation.request).unwrap(), original);
            if opted_in {
                for invalid in [
                    serde_json::json!({"secret_refs": ["API_KEY"]}),
                    serde_json::json!({"query": "hello", "secret_refs": [], "unknown": true}),
                ] {
                    invocation.request.input = invalid;
                    let checks =
                        validate_static_preflight_steps(&[invocation.clone()]).expect("static");
                    assert_eq!(
                        find_check(&checks, "input_schema").status,
                        PreflightStatus::Failed
                    );
                    let execution = ExecutionInvocation {
                        request: invocation.request.clone(),
                        secret_values: BTreeMap::new(),
                        remote_secret_policy: RemoteSecretPolicy::default(),
                    };
                    let result = match adapter {
                        AdapterKind::Process => process_adapter().execute_step(&execution),
                        AdapterKind::Http => http_adapter().execute_step(&execution),
                        AdapterKind::Mcp => mcp_adapter().execute_step(&execution),
                    }
                    .expect("invalid input result");
                    assert_eq!(
                        result.error.expect("error").code,
                        CanonicalErrorCode::ValidationError
                    );
                }
            }
        }
    }
}

#[test]
fn input_projection_rejects_unknown_or_malformed_bindings_at_install_and_adapter_paths() {
    for binding in [
        serde_json::json!(null),
        serde_json::json!(true),
        serde_json::json!(1),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!(""),
        serde_json::json!("action_data_v2"),
        serde_json::json!(" action_data_v1"),
        serde_json::json!("action_data_v1 "),
    ] {
        for adapter in [AdapterKind::Process, AdapterKind::Http, AdapterKind::Mcp] {
            let repo = sandbox_tempdir("projection-invalid-");
            let source = repo.path().join("source");
            let id = match adapter {
                AdapterKind::Process => {
                    write_process_no_handshake_plugin(&source, "process.echo");
                    "process.echo"
                }
                AdapterKind::Http => {
                    write_http_plugin(&source, "http://127.0.0.1:1");
                    "http.echo"
                }
                AdapterKind::Mcp => {
                    write_mcp_plugin(&source);
                    "mcp.echo"
                }
            };
            // Install the valid package, then exercise defensive adapter loading
            // against a malformed installed manifest as well as source install.
            install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install");
            configure_input_projection(&source, Some(binding.clone()), false);
            let error = install_plugin_file(repo.path(), &source.join("plugin.yaml"))
                .expect_err("invalid install");
            assert!(format!("{error:#}").contains("input_projection"));
            let installed_dir = repo.path().join(".transist/plugins").join(id).join("1.0.0");
            configure_input_projection(&installed_dir, Some(binding.clone()), false);
            let execution = request(repo.path(), adapter, id, "echo", serde_json::json!({}));
            let error = match adapter {
                AdapterKind::Process => process_adapter().execute_step(&execution),
                AdapterKind::Http => http_adapter().execute_step(&execution),
                AdapterKind::Mcp => mcp_adapter().execute_step(&execution),
            }
            .expect_err("invalid adapter binding");
            assert!(format!("{error:#}").contains("input_projection"));
            let preflight = PreflightInvocation {
                request: execution.request,
                available_secret_refs: vec![],
                remote_secret_policy: RemoteSecretPolicy::default(),
            };
            let checks = validate_static_preflight_steps(&[preflight]).expect("static");
            assert_eq!(
                find_check(&checks, "plugin_contract").status,
                PreflightStatus::Failed
            );
        }
    }
}

#[test]
fn input_projection_process_transport_opt_in_and_legacy_bytes() {
    for opted_in in [false, true] {
        let repo = sandbox_tempdir("projection-process-");
        let source = repo.path().join("source");
        write_process_preflight_plugin(&source, "process.echo", "process.echo", "{}");
        let script = fs::read_to_string(source.join("plugin.py")).expect("script");
        fs::write(
            source.join("plugin.py"),
            script.replace(
                "time.sleep(10)",
                r#"
invoke = json.loads(sys.stdin.readline())
assert invoke['metadata']['marker'] == 'unchanged'
assert invoke['timeout_ms'] == 5000
assert invoke['max_timeout_ms'] == 10000
print(json.dumps({'type': 'app.invoke.result.v1', 'protocol_version': 'app.v1',
    'invoke_id': invoke['invoke_id'], 'status': 'success', 'output': invoke['input']}), flush=True)
"#,
            ),
        )
        .expect("echo script");
        configure_input_projection(
            &source,
            opted_in.then(|| serde_json::json!("action_data_v1")),
            opted_in,
        );
        install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install");
        let mut invocation = request(
            repo.path(),
            AdapterKind::Process,
            "process.echo",
            "echo",
            serde_json::json!({"query": " text ", "secret_refs": ["API_KEY"]}),
        );
        invocation.request.metadata["marker"] = serde_json::json!("unchanged");
        // This wire-identity test deliberately supplies both deadlines; it must
        // not inherit unrelated changes to the generic functional-test budget.
        invocation.request.timeout_ms = Some(5_000);
        invocation.request.max_timeout_ms = Some(10_000);
        let original = serde_json::to_vec(&invocation.request).unwrap();
        let checks = process_adapter()
            .preflight_steps(&[PreflightInvocation {
                request: invocation.request.clone(),
                available_secret_refs: vec!["API_KEY".into()],
                remote_secret_policy: RemoteSecretPolicy::default(),
            }])
            .expect("live preflight");
        assert_eq!(
            find_check(&checks, "input_schema").status,
            PreflightStatus::Passed
        );
        assert_eq!(
            find_check(&checks, "adapter_handshake").status,
            PreflightStatus::Passed
        );
        let result = process_adapter()
            .execute_step(&invocation)
            .expect("execute");
        assert_eq!(result.status, "success");
        let expected = if opted_in {
            serde_json::json!({"query": " text "})
        } else {
            invocation.request.input.clone()
        };
        assert_eq!(
            serde_json::to_vec(&result.output).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        assert_eq!(serde_json::to_vec(&invocation.request).unwrap(), original);
    }
}

#[test]
fn input_projection_mcp_transport_env_secret_and_legacy_arguments() {
    for opted_in in [false, true] {
        let repo = sandbox_tempdir("projection-mcp-");
        let source = repo.path().join("source");
        write_mcp_plugin(&source);
        let script = fs::read_to_string(source.join("server.py")).expect("script");
        fs::write(source.join("server.py"), script.replace("import json, sys", "import json, sys, os")
            .replace("    elif method == \"tools/call\":", "    elif method == \"tools/call\":\n        assert os.environ['PROJECTION_API_KEY'] == 'fixture-secret'")
            .replace("\"structuredContent\":{\"stdout\":\"mcp-ok\",\"stderr\":\"\",\"exit_code\":0}",
                "\"structuredContent\":message['params']['arguments']")).expect("server");
        configure_input_projection(
            &source,
            opted_in.then(|| serde_json::json!("action_data_v1")),
            opted_in,
        );
        add_projection_secret(
            &source,
            serde_json::json!({"type": "env", "name": "PROJECTION_API_KEY"}),
        );
        install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install");
        let mut invocation = request(
            repo.path(),
            AdapterKind::Mcp,
            "mcp.echo",
            "echo",
            serde_json::json!({"query": "hello", "secret_refs": ["API_KEY"]}),
        );
        invocation
            .secret_values
            .insert("API_KEY".into(), "fixture-secret".into());
        let denied = mcp_adapter().execute_step(&invocation).expect("denied");
        assert_eq!(
            denied.error.expect("error").code,
            CanonicalErrorCode::ConfigurationError
        );
        invocation.remote_secret_policy.mcp = true;
        let original = serde_json::to_vec(&invocation.request).unwrap();
        let checks = mcp_adapter()
            .preflight_steps(&[PreflightInvocation {
                request: invocation.request.clone(),
                available_secret_refs: vec!["API_KEY".into()],
                remote_secret_policy: invocation.remote_secret_policy,
            }])
            .expect("live preflight");
        assert_eq!(
            find_check(&checks, "input_schema").status,
            PreflightStatus::Passed
        );
        assert_eq!(
            find_check(&checks, "adapter_handshake").status,
            PreflightStatus::Passed
        );
        let result = mcp_adapter().execute_step(&invocation).expect("execute");
        assert_eq!(result.status, "success");
        assert_eq!(
            result.output,
            if opted_in {
                serde_json::json!({"query": "hello"})
            } else {
                invocation.request.input.clone()
            }
        );
        assert_eq!(serde_json::to_vec(&invocation.request).unwrap(), original);
    }
}

#[test]
fn input_projection_http_authorization_business_body_and_body_field_injection() {
    for (opted_in, body_field) in [(false, false), (true, false), (true, true)] {
        let repo = sandbox_tempdir("projection-http-");
        let source = repo.path().join("source");
        let server = bind_loopback("input_projection_http");
        let addr = server.local_addr().expect("addr");
        write_http_plugin(&source, &format!("http://{addr}"));
        configure_input_projection(
            &source,
            opted_in.then(|| serde_json::json!("action_data_v1")),
            opted_in,
        );
        add_projection_secret(
            &source,
            serde_json::json!({"type": "header", "name": "Authorization"}),
        );
        if body_field {
            add_projection_secret(
                &source,
                serde_json::json!({"type": "body_field", "name": "api_key"}),
            );
            fs::write(source.join("schemas/input.json"), r#"{"type":"object","properties":{"query":{"type":"string"},"api_key":{"type":"string"}},"required":["query"],"additionalProperties":false}"#).expect("schema");
        }
        install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install");
        let mut invocation = request(
            repo.path(),
            AdapterKind::Http,
            "http.echo",
            "echo",
            serde_json::json!({"query": "hello", "secret_refs": ["API_KEY"]}),
        );
        invocation
            .secret_values
            .insert("API_KEY".into(), "fixture-secret".into());
        let denied = http_adapter().execute_step(&invocation).expect("denied");
        assert_eq!(
            denied.error.expect("error").code,
            CanonicalErrorCode::ConfigurationError
        );
        invocation.remote_secret_policy.http = true;
        let original = serde_json::to_vec(&invocation.request).unwrap();
        let handle = thread::spawn(move || {
            let (mut socket, _) = server.accept().expect("handshake");
            let handshake = read_http_request(&mut socket);
            assert!(handshake.starts_with("GET /app/handshake"));
            write_http_json(
                &mut socket,
                serde_json::json!({
                    "type": "app.handshake.response.v1", "protocol_version": "app.v1",
                    "plugin": {"id": "http.echo", "version": "1.0.0"}, "adapter_kind": "http",
                    "capabilities": {"actions": ["echo"], "supports_cancel": true, "supports_health": true,
                        "supports_artifact_hints": true, "supports_evidence_hints": false}
                }),
            );
            let (mut socket, _) = server.accept().expect("invoke");
            let wire = read_http_request(&mut socket);
            let (headers, body) = wire.split_once("\r\n\r\n").expect("body");
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: fixture-secret")
            );
            let invoke: serde_json::Value = serde_json::from_str(body).expect("invoke");
            let mut expected = serde_json::json!({"query": "hello"});
            if !opted_in {
                expected["secret_refs"] = serde_json::json!(["API_KEY"]);
            }
            if body_field {
                expected["api_key"] = serde_json::json!("fixture-secret");
            }
            assert!(invoke["input"] == expected, "unexpected HTTP action input");
            write_http_json(
                &mut socket,
                serde_json::json!({
                    "type": "app.invoke.result.v1", "protocol_version": "app.v1", "invoke_id": invoke["invoke_id"],
                    "status": "success", "output": {}
                }),
            );
        });
        let result = http_adapter().execute_step(&invocation).expect("execute");
        assert_eq!(result.status, "success");
        handle.join().expect("server");
        assert_eq!(serde_json::to_vec(&invocation.request).unwrap(), original);
    }
}

#[test]
fn static_validation_checks_installed_plugin_without_mutating_home_or_workspace() {
    let repo = sandbox_tempdir("static-validation-");
    fs::create_dir_all(repo.path().join(".transist")).expect("home");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");
    let invocation = preflight_request(
        repo.path(),
        AdapterKind::Process,
        "shell",
        "echo",
        serde_json::json!({ "args": ["static"] }),
    );
    let before = directory_snapshot(repo.path());
    let checks = validate_static_preflight_steps(&[invocation]).expect("static validation");

    assert_eq!(directory_snapshot(repo.path()), before);
    assert_eq!(
        find_check(&checks, "plugin_contract").status,
        PreflightStatus::Passed
    );
    assert_eq!(
        find_check(&checks, "input_schema").status,
        PreflightStatus::Passed
    );
    assert!(checks.iter().all(|check| check.kind != "adapter_probe"));
}

fn directory_snapshot(root: &Path) -> Vec<PathBuf> {
    fn visit(root: &Path, current: &Path, entries: &mut Vec<PathBuf>) {
        let mut children = fs::read_dir(current)
            .expect("read directory")
            .map(|entry| entry.expect("directory entry"))
            .collect::<Vec<_>>();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            entries.push(
                path.strip_prefix(root)
                    .expect("relative path")
                    .to_path_buf(),
            );
            if path.is_dir() {
                visit(root, &path, entries);
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
fn process_adapter_smoke_test() {
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");
    let invocation = request(
        repo.path(),
        AdapterKind::Process,
        "shell",
        "echo",
        serde_json::json!({"args": ["hello", "plugin"]}),
    );
    let result = process_adapter().execute_step(&invocation).expect("result");
    assert_eq!(result.status, "success");
    assert_eq!(result.output["stdout"], "hello plugin");
}

#[test]
fn workspace_binding_process_entrypoint_runs_from_declared_workspace() {
    let repo = sandbox_tempdir("sandbox-");
    let workspace = sandbox_tempdir("workspace-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");
    let mut invocation = request(
        repo.path(),
        AdapterKind::Process,
        "shell",
        "exec",
        serde_json::json!({"command": "pwd", "args": []}),
    );
    invocation.request.paths.workspace_dir = workspace.path().display().to_string();
    invocation.request.policy["workspace_declared"] = serde_json::Value::Bool(true);

    let result = process_adapter().execute_step(&invocation).expect("result");

    assert_eq!(result.status, "success");
    assert_eq!(
        result.output["stdout"].as_str().unwrap().trim(),
        workspace.path().display().to_string()
    );
}

#[test]
fn process_adapter_forwards_runtime_metadata_file_env() {
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let mut invocation = request(
        repo.path(),
        AdapterKind::Process,
        "shell",
        "exec",
        serde_json::json!({
            "command": "sh",
            "args": ["-c", "printf '%s' \"$TRANSIST_RUNTIME_METADATA_FILE\""],
        }),
    );
    let metadata_path = repo.path().join("invocation-runtime-metadata.json");
    invocation.request.metadata["runtime_metadata_file"] =
        serde_json::Value::String(metadata_path.display().to_string());
    let result = process_adapter().execute_step(&invocation).expect("result");

    assert_eq!(result.status, "success");
    assert_eq!(
        result.output["stdout"].as_str(),
        Some(metadata_path.to_string_lossy().as_ref())
    );
}

#[test]
fn process_adapter_forces_private_tmpdir_after_user_environment_processing() {
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");
    let invocation = request(
        repo.path(),
        AdapterKind::Process,
        "shell",
        "exec",
        serde_json::json!({
            "command": "sh",
            "args": [
                "-c",
                "if [ \"$(uname -s)\" = Darwin ]; then temporary_path=$(mktemp 2>/dev/null) && rm -f \"$temporary_path\"; mktemp \"$TMPDIR/tmp.XXXXXX\"; else mktemp; fi",
            ],
        }),
    );
    let expected_root = invocation.request.paths.run_root.clone();

    let result = process_adapter().execute_step(&invocation).expect("result");

    assert_eq!(result.status, "success", "execution result: {result:?}");
    let temporary_path = result.output["stdout"]
        .as_str()
        .expect("mktemp output")
        .trim();
    assert!(
        temporary_path.starts_with(&expected_root),
        "bare mktemp must use the invocation staging root, got {temporary_path}"
    );
    assert!(Path::new(temporary_path).is_file());
}

#[test]
fn process_adapter_preflight_passes_for_installed_shell_plugin() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let checks = process_adapter()
        .preflight_steps(&[preflight_request(
            repo.path(),
            AdapterKind::Process,
            "shell",
            "echo",
            serde_json::json!({"args": ["hello", "preflight"]}),
        )])
        .expect("preflight");

    assert_eq!(
        find_check(&checks, "plugin_contract").status,
        PreflightStatus::Passed
    );
    assert_eq!(
        find_check(&checks, "input_schema").status,
        PreflightStatus::Passed
    );
    assert_eq!(
        find_check(&checks, "adapter_handshake").status,
        PreflightStatus::Passed
    );
}

#[test]
fn process_preflight_groups_identity_only_requests_and_preserves_all_step_evidence() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-preflight-grouped");
    write_process_preflight_plugin(&plugin_src, "process.grouped", "process.grouped", "{}");
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");

    let mut first = preflight_request(
        repo.path(),
        AdapterKind::Process,
        "process.grouped",
        "echo",
        serde_json::json!({}),
    );
    first.request.step.id = "parent/step-b".to_string();
    first.request.workflow_id = "workflow-a".to_string();
    first.request.metadata["step_id"] = serde_json::json!("parent/step-b");
    first.request.metadata["workflow_id"] = serde_json::json!("workflow-a");
    let mut second = first.clone();
    second.request.step.id = "parent/step-a".to_string();
    second.request.workflow_id = "workflow-b".to_string();
    second.request.metadata["step_id"] = serde_json::json!("parent/step-a");
    second.request.metadata["workflow_id"] = serde_json::json!("workflow-b");

    let checks = process_adapter()
        .preflight_steps(&[first.clone(), second, first])
        .expect("preflight");
    let handshake_checks = checks
        .iter()
        .filter(|check| check.kind == "adapter_handshake")
        .collect::<Vec<_>>();
    assert_eq!(handshake_checks.len(), 1, "one live probe per group");
    assert_eq!(
        handshake_checks[0].step_ids,
        ["parent/step-b", "parent/step-a"]
    );

    let persisted = transist_core::PersistedPreflightRecord::from_checks(1, checks);
    let persisted_handshake = persisted
        .checks
        .iter()
        .find(|check| check.kind == "adapter_handshake")
        .expect("persisted handshake");
    assert_eq!(
        persisted_handshake.step_ids,
        ["parent/step-b", "parent/step-a"]
    );
}

#[test]
fn preflight_concurrency_cap_one_preserves_real_process_adapter_output() {
    let repo = sandbox_tempdir("preflight-concurrency-order-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-preflight-concurrency");
    write_process_preflight_plugin(
        &plugin_src,
        "process.concurrency",
        "process.concurrency",
        "{}",
    );
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");
    let invocations = (0..4)
        .map(|index| {
            let mut invocation = preflight_request(
                repo.path(),
                AdapterKind::Process,
                "process.concurrency",
                "echo",
                serde_json::json!({}),
            );
            invocation.request.step.id = format!("step-{index}");
            invocation.request.metadata["probe_group"] = serde_json::json!(index);
            invocation.request.policy =
                filesystem_policy_json("process", repo.path(), "read_only", Vec::new(), Vec::new());
            invocation.request.policy["network"] = serde_json::json!({"mode": "none"});
            invocation
        })
        .collect::<Vec<_>>();
    let store = Arc::new(LocalActiveRecordStore);

    let sequential = ProcessAdapter::with_preflight_probe_concurrency(store.clone(), 1)
        .expect("sequential adapter")
        .preflight_steps(&invocations)
        .expect("sequential preflight");
    let bounded = ProcessAdapter::with_preflight_probe_concurrency(store, 4)
        .expect("bounded adapter")
        .preflight_steps(&invocations)
        .expect("bounded preflight");

    assert_eq!(bounded, sequential);
}

#[test]
fn preflight_concurrency_http_probes_remain_sequential() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let server = bind_loopback("preflight_concurrency_http_probes_remain_sequential");
    let addr = server.local_addr().expect("addr");
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let server_active = Arc::clone(&active);
    let server_maximum = Arc::clone(&maximum);
    let handle = thread::spawn(move || {
        let mut connections = Vec::new();
        for _ in 0..4 {
            let (mut socket, _) = server.accept().expect("accept");
            let active = Arc::clone(&server_active);
            let maximum = Arc::clone(&server_maximum);
            connections.push(thread::spawn(move || {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                let request = read_http_request(&mut socket);
                thread::sleep(Duration::from_millis(40));
                let response = if request.starts_with("GET /app/handshake") {
                    let body = serde_json::to_string(&serde_json::json!({
                            "type": "app.handshake.response.v1",
                            "protocol_version": "app.v1",
                            "plugin": {"id": "http.echo", "version": "1.0.0"},
                            "adapter_kind": "http",
                            "capabilities": {
                                "actions": ["echo"],
                                "supports_cancel": true,
                                "supports_health": true,
                                "supports_artifact_hints": false,
                                "supports_evidence_hints": false
                            }
                        }))
                    .expect("handshake JSON");
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                write_tracked_http_response(&mut socket, response.as_bytes(), &active);
            }));
        }
        for connection in connections {
            connection.join().expect("connection");
        }
    });
    let plugin_src = repo.path().join("http-preflight-sequential");
    write_http_plugin(&plugin_src, &format!("http://127.0.0.1:{}", addr.port()));
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");
    let invocations = (0..2)
        .map(|index| {
            let mut invocation = preflight_request(
                repo.path(),
                AdapterKind::Http,
                "http.echo",
                "echo",
                serde_json::json!({}),
            );
            invocation.request.step.id = format!("http-step-{index}");
            invocation.request.metadata["probe_group"] = serde_json::json!(index);
            invocation
        })
        .collect::<Vec<_>>();

    let checks = http_adapter()
        .preflight_steps(&invocations)
        .expect("HTTP preflight");
    handle.join().expect("server");

    assert_eq!(
        checks
            .iter()
            .filter(|check| check.kind == "adapter_handshake")
            .count(),
        2
    );
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[test]
fn process_adapter_preflight_forces_private_tmpdir_after_user_environment_processing() {
    let repo = tempfile::tempdir().expect("repo");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo home");
    let plugin_src = repo.path().join("process-preflight-mktemp");
    write_process_preflight_plugin(&plugin_src, "process.mktemp", "process.mktemp", "{}");
    let script = fs::read_to_string(plugin_src.join("plugin.py")).expect("script");
    fs::write(
        plugin_src.join("plugin.py"),
        script.replace(
            "import json",
            "import json\nimport os\nimport platform\nimport subprocess\nbare_mktemp = subprocess.run(['mktemp'], capture_output=True, text=True)\nif platform.system() == 'Darwin':\n    if bare_mktemp.returncode == 0:\n        os.unlink(bare_mktemp.stdout.strip())\n    temporary_path = subprocess.check_output(['mktemp', os.path.join(os.environ['TMPDIR'], 'tmp.XXXXXX')], text=True).strip()\nelse:\n    bare_mktemp.check_returncode()\n    temporary_path = bare_mktemp.stdout.strip()\nwith open(temporary_path, 'w', encoding='utf-8') as temporary_file:\n    temporary_file.write('runtime-temp-sentinel')\nwith open(temporary_path, 'r', encoding='utf-8') as temporary_file:\n    if temporary_file.read() != 'runtime-temp-sentinel':\n        raise RuntimeError('runtime temp read/write mismatch')\nopen(os.path.join(os.environ['TRANSIST_WORKSPACE_DIR'], 'preflight-tmpdir.txt'), 'w').write(temporary_path)",
        ),
    )
    .expect("instrument script");
    add_plugin_binary_prerequisite(&plugin_src);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install plugin");
    let invocation = preflight_request(
        repo.path(),
        AdapterKind::Process,
        "process.mktemp",
        "echo",
        serde_json::json!({}),
    );
    let checks = process_adapter()
        .preflight_steps(&[invocation])
        .expect("preflight");
    assert_eq!(
        find_check(&checks, "adapter_handshake").status,
        PreflightStatus::Passed,
        "preflight checks: {checks:?}"
    );
    assert_check_occurs_once(&checks, "plugin_prerequisite");

    let temporary_path =
        fs::read_to_string(repo.path().join("preflight-tmpdir.txt")).expect("TMPDIR marker");
    let temporary_file = PathBuf::from(temporary_path.trim());
    let temporary_root = temporary_file.parent().expect("temporary root");
    assert!(
        temporary_root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("transist-runtime-temp-")),
        "bare mktemp must use the disposable preflight runtime temporary root, got {temporary_path}"
    );
    assert!(
        !temporary_root.exists(),
        "preflight runtime temporary root must be removed after the probe"
    );
}

#[test]
fn process_preflight_rejects_repo_uri_without_repo_path_during_schema_validation() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let checks = process_adapter()
        .preflight_steps(&[preflight_request(
            repo.path(),
            AdapterKind::Process,
            "shell",
            "exec",
            serde_json::json!({"command": "repo://plans/current.md"}),
        )])
        .expect("preflight");

    assert_eq!(
        find_check(&checks, "input_schema").status,
        PreflightStatus::Failed
    );
}

#[test]
fn workspace_binding_process_preflight_runs_from_declared_workspace() {
    let repo = tempfile::tempdir().expect("repo");
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo home");
    let plugin_src = repo.path().join("process-workspace-preflight");
    write_process_preflight_plugin(&plugin_src, "process.workspace", "process.workspace", "{}");
    let script = fs::read_to_string(plugin_src.join("plugin.py")).expect("script");
    fs::write(
        plugin_src.join("plugin.py"),
        script.replace(
            "import json",
            "import json\nimport os\nopen('preflight-cwd.txt', 'w').write(os.getcwd())",
        ),
    )
    .expect("instrument script");
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install plugin");
    let mut invocation = preflight_request(
        repo.path(),
        AdapterKind::Process,
        "process.workspace",
        "echo",
        serde_json::json!({}),
    );
    invocation.request.paths.workspace_dir = workspace.path().display().to_string();
    invocation.request.policy["workspace_declared"] = serde_json::Value::Bool(true);

    process_adapter()
        .preflight_steps(&[invocation])
        .expect("preflight");

    assert_eq!(
        fs::read_to_string(workspace.path().join("preflight-cwd.txt")).expect("cwd marker"),
        fs::canonicalize(workspace.path())
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn process_adapter_child_env_policy_clears_unrelated_parent_env() {
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let unrelated_name = "TRANSIST_STEP2_UNRELATED_PROCESS_ENV";
    // SAFETY: test-only environment mutation with a unique variable name.
    unsafe {
        std::env::set_var(unrelated_name, "should-not-leak");
    }

    let mut invocation = request(
        repo.path(),
        AdapterKind::Process,
        "shell",
        "exec",
        serde_json::json!({
            "command": "sh",
            "args": [
                "-c",
                format!(
                    "printf '%s|%s|%s|%s' \"${{PATH:+present}}\" \"${{HOME:+present}}\" \"${{TRANSIST_MANAGED_RUNTIME_AUTH_ACTIVE:-}}\" \"${{{unrelated_name}:+present}}\""
                )
            ],
        }),
    );
    invocation.request.policy = serde_json::json!({
        "runtime_auth_context": {
            "active": true,
            "policy_id": "local_runtime_policy",
            "child_env_allowlist": ["PATH", "HOME"]
        }
    });

    let result = process_adapter().execute_step(&invocation).expect("result");

    // SAFETY: paired cleanup for the test-only environment mutation above.
    unsafe {
        std::env::remove_var(unrelated_name);
    }

    assert_eq!(result.status, "success");
    assert_eq!(result.output["stdout"].as_str(), Some("present|present|1|"));
}

#[test]
fn process_adapter_preflight_fails_on_handshake_mismatch() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-preflight-bad");
    write_process_preflight_plugin(
        &plugin_src,
        "process.bad",
        "process.other",
        r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}"#,
    );
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");

    let checks = process_adapter()
        .preflight_steps(&[preflight_request(
            repo.path(),
            AdapterKind::Process,
            "process.bad",
            "echo",
            serde_json::json!({"message": "hello"}),
        )])
        .expect("preflight");

    let handshake = find_check(&checks, "adapter_handshake");
    assert_eq!(handshake.status, PreflightStatus::Failed);
    assert!(handshake.message.contains("plugin identity mismatch"));
}

#[test]
fn process_adapter_preflight_fails_invalid_input_schema_before_probe() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-preflight-schema");
    write_process_preflight_plugin(
        &plugin_src,
        "process.schema",
        "process.schema",
        r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}"#,
    );
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");

    let checks = process_adapter()
        .preflight_steps(&[preflight_request(
            repo.path(),
            AdapterKind::Process,
            "process.schema",
            "echo",
            serde_json::json!({"message": 7}),
        )])
        .expect("preflight");

    let schema = find_check(&checks, "input_schema");
    assert_eq!(schema.status, PreflightStatus::Failed);
    assert!(schema.message.contains("instance path '/message'"));
    assert!(schema.message.contains("schema path"));
    assert!(!schema.message.contains("7"));
    assert!(!checks.iter().any(|check| check.kind == "adapter_handshake"));
}

#[test]
fn process_adapter_rejects_invalid_success_output_before_hints() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-invalid-output");
    write_process_invoke_result_plugin(
        &plugin_src,
        "process.invalid_output",
        serde_json::json!({
            "type": "app.invoke.result.v1",
            "protocol_version": "app.v1",
            "invoke_id": "invoke_1",
            "status": "success",
            "output": {"stdout": "untrusted", "stderr": "", "exit_code": 0},
            "artifact_hints": [{"name": "leaked.txt", "contents": "untrusted"}],
            "evidence_hints": [{"name": "leaked.json", "contents": "untrusted"}],
            "metrics": {},
            "usage": null,
            "error": null
        }),
    );
    fs::write(
        plugin_src.join("schemas/output.json"),
        r#"{"type":"object","required":["result"],"properties":{"result":{"type":"string"}}}"#,
    )
    .expect("output schema");
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");

    let result = process_adapter()
        .execute_step(&request(
            repo.path(),
            AdapterKind::Process,
            "process.invalid_output",
            "echo",
            serde_json::json!({"message": "hello"}),
        ))
        .expect("result");

    assert_eq!(result.status, "failed");
    assert_eq!(
        result.error.expect("error").code,
        CanonicalErrorCode::ValidationError
    );
    assert_eq!(result.output, serde_json::json!({}));
    assert!(result.artifacts.is_empty());
    assert!(result.evidence.is_empty());
}

#[test]
fn process_adapter_reports_invalid_invoke_result_as_protocol_error() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-invalid-invoke-result");
    write_process_invoke_result_plugin(
        &plugin_src,
        "process.invalid_result",
        serde_json::json!({
            "type": "app.invoke.result.v1",
            "protocol_version": "app.v1",
            "invoke_id": "invoke_1",
            "status": "failed",
            "output": {
                "stdout": "",
                "stderr": "",
                "exit_code": 1
            },
            "artifact_hints": [],
            "evidence_hints": [],
            "metrics": {},
            "usage": null,
            "error": {
                "code": "approval_threshold_not_met",
                "class": "tool",
                "message": "PR exit approval denied: need 1 current approvals, found 0.",
                "retryability": "safe"
            }
        }),
    );
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");

    let invocation = request(
        repo.path(),
        AdapterKind::Process,
        "process.invalid_result",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let result = process_adapter().execute_step(&invocation).expect("result");

    assert_eq!(result.status, "failed");
    assert_eq!(result.exit_code, Some(2));
    let error = result.error.expect("error");
    assert_eq!(error.code, CanonicalErrorCode::ProtocolError);
    assert!(error.message.contains("APP invoke-result protocol error"));
    assert!(error.message.contains("failed decoding invoke result"));
    assert!(!error.message.contains("timed out"));
    assert!(result.stderr.contains("offending APP invoke-result line"));
    assert!(result.stderr.contains("approval_threshold_not_met"));
}

#[test]
fn process_adapter_refreshes_timeout_on_progress_frames() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-progress-refresh-timeout");
    write_process_progress_then_result_plugin(&plugin_src, "process.progress_refresh");
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");

    let mut invocation = request(
        repo.path(),
        AdapterKind::Process,
        "process.progress_refresh",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    const IDLE_TIMEOUT_MS: u64 = 5_000;
    invocation.request.timeout_ms = Some(IDLE_TIMEOUT_MS);

    let started = Instant::now();
    let result = process_adapter().execute_step(&invocation).expect("result");
    let elapsed = started.elapsed();

    assert_eq!(result.status, "success", "{result:?}");
    assert_eq!(result.output["stdout"], "progress-ok");
    assert!(
        elapsed >= Duration::from_millis(IDLE_TIMEOUT_MS + 1_000),
        "progress must keep the invocation alive beyond its original deadline, got {elapsed:?}; error={:?}; stderr={:?}",
        result.error,
        result.stderr
    );
}

#[test]
fn http_adapter_smoke_test() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let server = bind_loopback("http_adapter_smoke_test");
    let addr = server.local_addr().expect("addr");
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
            }
        }
    });
    let plugin_src = repo.path().join("http-plugin");
    write_http_plugin(&plugin_src, &format!("http://127.0.0.1:{}", addr.port()));
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install http");
    let invocation = request(
        repo.path(),
        AdapterKind::Http,
        "http.echo",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let result = http_adapter().execute_step(&invocation).expect("result");
    handle.join().expect("join");
    assert_eq!(result.status, "success");
}

#[test]
fn http_adapter_preflight_fails_when_health_probe_fails() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let server = bind_loopback("http_adapter_preflight_fails_when_health_probe_fails");
    let addr = server.local_addr().expect("addr");
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
            } else {
                let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                socket.write_all(response.as_bytes()).expect("write");
            }
        }
    });
    let plugin_src = repo.path().join("http-preflight-health");
    write_http_plugin(&plugin_src, &format!("http://127.0.0.1:{}", addr.port()));
    add_plugin_binary_prerequisite(&plugin_src);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install http");

    let checks = http_adapter()
        .preflight_steps(&[preflight_request(
            repo.path(),
            AdapterKind::Http,
            "http.echo",
            "echo",
            serde_json::json!({"message": "hello"}),
        )])
        .expect("preflight");
    handle.join().expect("join");

    assert_eq!(
        find_check(&checks, "adapter_handshake").status,
        PreflightStatus::Passed
    );
    let health = find_check(&checks, "adapter_health");
    assert_eq!(health.status, PreflightStatus::Failed);
    assert!(health.message.contains("http health probe failed"));
    assert_check_occurs_once(&checks, "plugin_prerequisite");
}

#[test]
fn http_adapter_drops_invalid_metrics_without_failing_execution() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let server = bind_loopback("http_adapter_drops_invalid_metrics_without_failing_execution");
    let addr = server.local_addr().expect("addr");
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
            } else {
                write_http_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "app.invoke.result.v1",
                        "protocol_version": "app.v1",
                        "invoke_id": "invoke_1",
                        "status": "success",
                        "output": { "stdout": "http-ok", "stderr": "", "exit_code": 0 },
                        "artifact_hints": [],
                        "evidence_hints": [],
                        "metrics": {
                            "latency_ms": 12.0,
                            "bad": "oops",
                            "nested": { "value": true }
                        },
                        "usage": null,
                        "error": null
                    }),
                );
            }
        }
    });
    let plugin_src = repo.path().join("http-plugin");
    write_http_plugin(&plugin_src, &format!("http://127.0.0.1:{}", addr.port()));
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install http");
    let invocation = request(
        repo.path(),
        AdapterKind::Http,
        "http.echo",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let result = http_adapter().execute_step(&invocation).expect("result");
    handle.join().expect("join");
    assert_eq!(result.status, "success");
    assert_eq!(result.metrics.get("latency_ms"), Some(&12.0));
    assert_eq!(result.metrics_validation_errors, 2);
}

#[test]
fn mcp_adapter_smoke_test() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("mcp-plugin");
    write_mcp_plugin(&plugin_src);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install mcp");
    let invocation = request(
        repo.path(),
        AdapterKind::Mcp,
        "mcp.echo",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    let result = mcp_adapter().execute_step(&invocation).expect("result");
    assert_eq!(result.status, "success");
}

#[test]
fn mcp_adapter_interoperates_with_strict_paginated_jsonrpc_server() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("mcp-strict-plugin");
    let trace = repo.path().join("mcp-trace.json");
    write_strict_mcp_plugin(&plugin_src, &trace);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install mcp");

    let result = mcp_adapter()
        .execute_step(&request(
            repo.path(),
            AdapterKind::Mcp,
            "mcp.strict",
            "echo",
            serde_json::json!({"message": "hello"}),
        ))
        .expect("result");

    assert_eq!(result.status, "success");
    assert_eq!(result.stdout, "firstsecond");
    assert_eq!(result.output, serde_json::json!({"answer": 42}));
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&fs::read_to_string(trace).expect("trace"))
            .expect("trace json"),
        [
            "initialize",
            "initialized",
            "tools/list",
            "tools/list",
            "tools/call"
        ]
    );
}

#[test]
fn mcp_adapter_maps_tool_errors_and_content_only_results() {
    let cases = [
        (
            "content",
            r#"{"result":{"content":[{"type":"text","text":"content-only"}]}}"#,
            None,
            false,
        ),
        (
            "tool-error",
            r#"{"result":{"content":[{"type":"text","text":"failed"}],"isError":true}}"#,
            Some(CanonicalErrorCode::ToolExecutionError),
            false,
        ),
        (
            "rpc-error",
            r#"{"error":{"code":-32000,"message":"server failed"}}"#,
            Some(CanonicalErrorCode::ProtocolError),
            false,
        ),
        (
            "mismatched-id",
            r#"{"result":{"content":[]}}"#,
            Some(CanonicalErrorCode::ProtocolError),
            true,
        ),
    ];
    for (suffix, response, expected_error, mismatch_response_id) in cases {
        let repo = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(repo.path().join(".transist")).expect("repo");
        let plugin_src = repo.path().join("mcp-result-plugin");
        write_mcp_result_plugin(
            &plugin_src,
            &format!("mcp.{suffix}"),
            response,
            mismatch_response_id,
        );
        install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install mcp");
        let result = mcp_adapter()
            .execute_step(&request(
                repo.path(),
                AdapterKind::Mcp,
                &format!("mcp.{suffix}"),
                "echo",
                serde_json::json!({"message": "hello"}),
            ))
            .expect("result");
        if let Some(expected_error) = expected_error {
            assert_eq!(result.status, "failed");
            assert_eq!(result.error.expect("error").code, expected_error);
        } else {
            assert_eq!(result.status, "success");
            assert_eq!(result.stdout, "content-only");
            assert_eq!(
                result.output,
                serde_json::json!({"content": [{"type": "text", "text": "content-only"}]})
            );
        }
    }
}

#[test]
fn workspace_binding_mcp_entrypoint_and_preflight_run_from_declared_workspace() {
    let repo = tempfile::tempdir().expect("repo");
    let execution_workspace = tempfile::tempdir().expect("execution workspace");
    let preflight_workspace = tempfile::tempdir().expect("preflight workspace");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo home");
    let plugin_src = repo.path().join("mcp-workspace");
    write_mcp_plugin(&plugin_src);
    let server = fs::read_to_string(plugin_src.join("server.py")).expect("server");
    fs::write(
        plugin_src.join("server.py"),
        server
            .replace(
                "import json, sys",
                "import json, os, sys\nopen('mcp-cwd.txt', 'w').write(os.getcwd())",
            )
            .replace("\"stdout\":\"mcp-ok\"", "\"stdout\":os.getcwd()"),
    )
    .expect("instrument server");
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install mcp");
    let mut invocation = request(
        repo.path(),
        AdapterKind::Mcp,
        "mcp.echo",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    invocation.request.paths.workspace_dir = execution_workspace.path().display().to_string();
    invocation.request.policy["workspace_declared"] = serde_json::Value::Bool(true);

    let result = mcp_adapter().execute_step(&invocation).expect("execute");

    assert_eq!(result.status, "success");
    assert_eq!(
        fs::read_to_string(execution_workspace.path().join("mcp-cwd.txt")).expect("cwd marker"),
        fs::canonicalize(execution_workspace.path())
            .unwrap()
            .display()
            .to_string()
    );

    let mut preflight = preflight_request(
        repo.path(),
        AdapterKind::Mcp,
        "mcp.echo",
        "echo",
        serde_json::json!({"message": "hello"}),
    );
    preflight.request.paths.workspace_dir = preflight_workspace.path().display().to_string();
    preflight.request.policy["workspace_declared"] = serde_json::Value::Bool(true);
    mcp_adapter()
        .preflight_steps(&[preflight])
        .expect("preflight");
    assert_eq!(
        fs::read_to_string(preflight_workspace.path().join("mcp-cwd.txt"))
            .expect("preflight cwd marker"),
        fs::canonicalize(preflight_workspace.path())
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn http_and_mcp_adapters_reject_invalid_success_output() {
    let http_repo = tempfile::tempdir().expect("http tempdir");
    fs::create_dir_all(http_repo.path().join(".transist")).expect("http repo");
    let server = bind_loopback("http_adapter_invalid_output");
    let addr = server.local_addr().expect("addr");
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
                        "plugin": {"id": "http.echo", "version": "1.0.0"},
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
                        "output": {"stdout": "untrusted", "stderr": "", "exit_code": 0},
                        "artifact_hints": [{"name": "leaked.txt", "contents": "untrusted"}],
                        "evidence_hints": [],
                        "metrics": {},
                        "usage": null,
                        "error": null
                    }),
                );
            }
        }
    });
    let http_plugin = http_repo.path().join("http-plugin");
    write_http_plugin(&http_plugin, &format!("http://127.0.0.1:{}", addr.port()));
    fs::write(
        http_plugin.join("schemas/output.json"),
        r#"{"type":"object","required":["result"]}"#,
    )
    .expect("http output schema");
    install_plugin_file(http_repo.path(), &http_plugin.join("plugin.yaml")).expect("install http");
    let http_result = http_adapter()
        .execute_step(&request(
            http_repo.path(),
            AdapterKind::Http,
            "http.echo",
            "echo",
            serde_json::json!({"message": "hello"}),
        ))
        .expect("http result");
    handle.join().expect("http server");

    let mcp_repo = tempfile::tempdir().expect("mcp tempdir");
    fs::create_dir_all(mcp_repo.path().join(".transist")).expect("mcp repo");
    let mcp_plugin = mcp_repo.path().join("mcp-plugin");
    write_mcp_plugin(&mcp_plugin);
    fs::write(
        mcp_plugin.join("schemas/output.json"),
        r#"{"type":"object","required":["result"]}"#,
    )
    .expect("mcp output schema");
    install_plugin_file(mcp_repo.path(), &mcp_plugin.join("plugin.yaml")).expect("install mcp");
    let mcp_result = mcp_adapter()
        .execute_step(&request(
            mcp_repo.path(),
            AdapterKind::Mcp,
            "mcp.echo",
            "echo",
            serde_json::json!({"message": "hello"}),
        ))
        .expect("mcp result");

    for result in [http_result, mcp_result] {
        assert_eq!(result.status, "failed");
        assert_eq!(
            result.error.expect("error").code,
            CanonicalErrorCode::ValidationError
        );
        assert_eq!(result.output, serde_json::json!({}));
        assert!(result.artifacts.is_empty());
        assert!(result.evidence.is_empty());
    }
}

#[test]
fn mcp_adapter_child_env_policy_clears_unrelated_parent_env() {
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("mcp-exec-plugin");
    write_mcp_exec_plugin(&plugin_src);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install mcp");

    let unrelated_name = "TRANSIST_STEP2_UNRELATED_MCP_ENV";
    // SAFETY: test-only environment mutation with a unique variable name.
    unsafe {
        std::env::set_var(unrelated_name, "should-not-leak");
    }

    let mut invocation = request(
        repo.path(),
        AdapterKind::Mcp,
        "mcp.exec",
        "exec",
        serde_json::json!({
            "command": "sh",
            "args": [
                "-c",
                format!(
                    "printf '%s|%s|%s|%s' \"${{PATH:+present}}\" \"${{HOME:+present}}\" \"${{TRANSIST_MANAGED_RUNTIME_AUTH_ACTIVE:-}}\" \"${{{unrelated_name}:+present}}\""
                )
            ],
        }),
    );
    invocation.request.policy = serde_json::json!({
        "runtime_auth_context": {
            "active": true,
            "policy_id": "local_runtime_policy",
            "child_env_allowlist": ["PATH", "HOME"]
        }
    });

    let result = mcp_adapter().execute_step(&invocation).expect("result");

    // SAFETY: paired cleanup for the test-only environment mutation above.
    unsafe {
        std::env::remove_var(unrelated_name);
    }

    assert_eq!(result.status, "success");
    assert_eq!(
        result.output["stdout"].as_str().map(str::trim_end),
        Some("present|present|1|")
    );
}

#[test]
fn mcp_adapter_preflight_fails_when_required_tool_is_missing() {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("mcp-missing-tool");
    write_mcp_plugin_with_listed_tool(&plugin_src, "other_tool");
    add_plugin_binary_prerequisite(&plugin_src);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install mcp");

    let checks = mcp_adapter()
        .preflight_steps(&[preflight_request(
            repo.path(),
            AdapterKind::Mcp,
            "mcp.echo",
            "echo",
            serde_json::json!({"message": "hello"}),
        )])
        .expect("preflight");

    let handshake = find_check(&checks, "adapter_handshake");
    assert_eq!(handshake.status, PreflightStatus::Failed);
    assert!(
        handshake
            .message
            .contains("MCP tool 'echo_tool' is not available")
    );
    assert_check_occurs_once(&checks, "plugin_prerequisite");
}

#[test]
fn preflight_concurrency_cap_one_preserves_real_mcp_adapter_output() {
    let repo = sandbox_tempdir("mcp-preflight-concurrency-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("mcp-concurrency");
    write_mcp_plugin(&plugin_src);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");
    let invocations = (0..3)
        .map(|index| {
            let mut invocation = preflight_request(
                repo.path(),
                AdapterKind::Mcp,
                "mcp.echo",
                "echo",
                serde_json::json!({}),
            );
            invocation.request.step.id = format!("mcp-step-{index}");
            invocation.request.metadata["probe_group"] = serde_json::json!(index);
            invocation.request.policy =
                filesystem_policy_json("mcp", repo.path(), "read_only", Vec::new(), Vec::new());
            invocation.request.policy["network"] = serde_json::json!({"mode": "none"});
            invocation
        })
        .collect::<Vec<_>>();
    let store = Arc::new(LocalActiveRecordStore);

    let sequential = McpAdapter::with_preflight_probe_concurrency(store.clone(), 1)
        .expect("sequential adapter")
        .preflight_steps(&invocations)
        .expect("sequential preflight");
    let bounded = McpAdapter::with_preflight_probe_concurrency(store, 4)
        .expect("bounded adapter")
        .preflight_steps(&invocations)
        .expect("bounded preflight");

    assert_eq!(bounded, sequential);
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

/// Finishes tracked work before releasing the last HTTP framing byte. The client
/// cannot finish this response and begin its next probe while the server's
/// already-completed handler is still counted as active by a delayed epilogue.
fn write_tracked_http_response(socket: &mut TcpStream, response: &[u8], active: &AtomicUsize) {
    let (last, prefix) = response.split_last().expect("nonempty HTTP response");
    socket.write_all(prefix).expect("response prefix");
    active.fetch_sub(1, Ordering::SeqCst);
    socket.write_all(&[*last]).expect("response completion");
}

#[test]
fn http_response_completion_observes_settled_probe_accounting() {
    let server = bind_loopback("http_response_completion_observes_settled_probe_accounting");
    let address = server.local_addr().expect("address");
    let active = Arc::new(AtomicUsize::new(1));
    let server_active = Arc::clone(&active);
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nend";
    let handler = thread::spawn(move || {
        let (mut socket, _) = server.accept().expect("accept");
        write_tracked_http_response(&mut socket, response, &server_active);
    });
    let mut client = TcpStream::connect(address).expect("connect");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("bounded read");
    let mut received = vec![0; response.len()];
    client.read_exact(&mut received).expect("complete response");
    assert_eq!(received, response);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    handler.join().expect("handler");
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
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if body.len() >= content_length {
                break;
            }
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

fn write_run_state_and_recipe(
    run_root: &Path,
    workflow_id: &str,
    workflow_version: &str,
    adapter_kind: &str,
    plugin_id: &str,
    action: &str,
) {
    fs::write(
        run_root.join("state.json"),
        serde_json::json!({
            "run_id": "run_1",
            "workflow_id": workflow_id,
            "workflow_version": workflow_version,
            "status": "created",
            "steps": {},
            "executions": {},
            "workflow_inputs": {},
            "created_at_ms": 1,
            "updated_at_ms": 1
        })
        .to_string(),
    )
    .expect("state");
    fs::write(
        run_root.join("recipe.yaml"),
        format!(
            r#"
schema: transist.recipe.v1
workflow:
  id: {workflow_id}
  version: {workflow_version}
inputs: {{}}
steps:
  - id: step
    execution:
      kind: invoke
      adapter: {adapter_kind}
      plugin: {plugin_id}
      action: {action}
      execution_kind: external
"#
        ),
    )
    .expect("recipe");
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
package:
  files:
    - schemas/input.json
    - schemas/output.json
adapter: http
entrypoint:
  type: http
  base_url: {base_url}
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

fn write_process_preflight_plugin(
    dir: &Path,
    manifest_id: &str,
    handshake_plugin_id: &str,
    input_schema: &str,
) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), input_schema).expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.py"),
        format!(
            r#"#!/usr/bin/env python3
import json
import sys
import time

json.loads(sys.stdin.readline())
print(json.dumps({{
    "type": "app.handshake.response.v1",
    "protocol_version": "app.v1",
    "plugin": {{"id": "{handshake_plugin_id}", "version": "1.0.0"}},
    "adapter_kind": "process",
    "capabilities": {{
        "actions": ["echo"],
        "supports_cancel": True,
        "supports_health": False,
        "supports_artifact_hints": False,
        "supports_evidence_hints": False
    }}
}}), flush=True)
time.sleep(10)
"#,
            handshake_plugin_id = handshake_plugin_id
        ),
    )
    .expect("plugin");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: {manifest_id}
version: 1.0.0
package:
  files:
    - plugin.py
    - schemas/input.json
    - schemas/output.json
adapter: process
entrypoint:
  type: process
  command: plugin.py
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
        ),
    )
    .expect("manifest");
}

fn write_process_no_handshake_plugin(dir: &Path, manifest_id: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.py"),
        r#"#!/usr/bin/env python3
import sys

sys.stderr.write("plugin exited before handshake\n")
sys.stderr.flush()
sys.exit(2)
"#,
    )
    .expect("plugin");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: {manifest_id}
version: 1.0.0
package:
  files:
    - plugin.py
    - schemas/input.json
    - schemas/output.json
adapter: process
entrypoint:
  type: process
  command: plugin.py
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
        ),
    )
    .expect("manifest");
}

fn write_process_invoke_result_plugin(
    dir: &Path,
    manifest_id: &str,
    invoke_result: serde_json::Value,
) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    let handshake = serde_json::json!({
        "type": "app.handshake.response.v1",
        "protocol_version": "app.v1",
        "plugin": { "id": manifest_id, "version": "1.0.0" },
        "adapter_kind": "process",
        "capabilities": {
            "actions": ["echo"],
            "supports_cancel": true,
            "supports_health": false,
            "supports_artifact_hints": false,
            "supports_evidence_hints": false
        }
    })
    .to_string();
    fs::write(
        dir.join("plugin.py"),
        format!(
            r#"#!/usr/bin/env python3
import json
import sys

json.loads(sys.stdin.readline())
print({handshake_json:?}, flush=True)
json.loads(sys.stdin.readline())
print({invoke_result_json:?}, flush=True)
"#,
            handshake_json = handshake,
            invoke_result_json = invoke_result.to_string()
        ),
    )
    .expect("plugin");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: {manifest_id}
version: 1.0.0
package:
  files:
    - plugin.py
    - schemas/input.json
    - schemas/output.json
adapter: process
entrypoint:
  type: process
  command: plugin.py
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
        ),
    )
    .expect("manifest");
}

fn write_process_progress_then_result_plugin(dir: &Path, manifest_id: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("plugin.py"),
        format!(
            r#"#!/usr/bin/env python3
import json
import sys
import time

json.loads(sys.stdin.readline())
print(json.dumps({{
    "type": "app.handshake.response.v1",
    "protocol_version": "app.v1",
    "plugin": {{"id": "{manifest_id}", "version": "1.0.0"}},
    "adapter_kind": "process",
    "capabilities": {{
        "actions": ["echo"],
        "supports_cancel": True,
        "supports_health": False,
        "supports_artifact_hints": False,
        "supports_evidence_hints": False
    }}
}}), flush=True)
invoke = json.loads(sys.stdin.readline())
invoke_id = invoke.get("invoke_id", "invoke_1")

def emit_progress(message):
    print(json.dumps({{
        "type": "app.invoke.progress.v1",
        "protocol_version": "app.v1",
        "invoke_id": invoke_id,
        "stream": "status",
        "delta_lines": 0,
        "line_preview": message
    }}), flush=True)

for phase in range(12):
    time.sleep(0.5)
    emit_progress(f"phase-{{phase + 1}}")
time.sleep(0.5)
print(json.dumps({{
    "type": "app.invoke.result.v1",
    "protocol_version": "app.v1",
    "invoke_id": invoke_id,
    "status": "success",
    "output": {{"stdout": "progress-ok", "stderr": "", "exit_code": 0}},
    "artifact_hints": [],
    "evidence_hints": [],
    "metrics": {{}},
    "usage": None,
    "error": None
}}), flush=True)
"#,
            manifest_id = manifest_id,
        ),
    )
    .expect("plugin");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: {manifest_id}
version: 1.0.0
package:
  files:
    - plugin.py
    - schemas/input.json
    - schemas/output.json
adapter: process
entrypoint:
  type: process
  command: plugin.py
actions:
  - name: echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {{}}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
            manifest_id = manifest_id,
        ),
    )
    .expect("manifest");
}

fn write_mcp_plugin(dir: &Path) {
    write_mcp_plugin_with_listed_tool(dir, "echo_tool");
}

fn write_strict_mcp_plugin(dir: &Path, trace: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("server.py"),
        format!(
            r#"#!/usr/bin/env python3
import json
import sys

trace = []

def read_message():
    line = sys.stdin.buffer.readline()
    return json.loads(line.decode()) if line else None

def write_message(payload):
    sys.stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stdout.flush()

message = read_message()
assert message["jsonrpc"] == "2.0"
assert message["method"] == "initialize"
assert message["params"] == {{
    "protocolVersion": "2025-06-18",
    "capabilities": {{}},
    "clientInfo": {{"name": "transist", "version": "{transist_version}"}},
}}
trace.append("initialize")
write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{
    "protocolVersion":"2025-06-18",
    "capabilities":{{"tools":{{}}}},
    "serverInfo":{{"name":"strict","version":"1.0.0"}},
}}}})

message = read_message()
assert message == {{"jsonrpc":"2.0","method":"notifications/initialized","params":{{}}}}
trace.append("initialized")

message = read_message()
assert message["method"] == "tools/list" and message["params"] == {{}}
trace.append("tools/list")
write_message({{"jsonrpc":"2.0","method":"notifications/message","params":{{"level":"info","data":"ignored"}}}})
write_message({{"jsonrpc":"2.0","id":"server-ping","method":"ping","params":{{}}}})
assert read_message() == {{"jsonrpc":"2.0","id":"server-ping","result":{{}}}}
write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"other_tool"}}],"nextCursor":"page-2"}}}})

message = read_message()
assert message["method"] == "tools/list" and message["params"] == {{"cursor":"page-2"}}
trace.append("tools/list")
write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"echo_tool"}}]}}}})

message = read_message()
assert message["method"] == "tools/call"
assert message["params"] == {{"name":"echo_tool","arguments":{{"message":"hello"}}}}
trace.append("tools/call")
write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{
    "content":[{{"type":"text","text":"first"}},{{"type":"image","data":"opaque"}},{{"type":"text","text":"second"}}],
    "structuredContent":{{"answer":42}},
}}}})
open(r"{trace}", "w", encoding="utf-8").write(json.dumps(trace))
"#,
            trace = trace.display(),
            transist_version = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("server");
    make_executable(&dir.join("server.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: mcp.strict
version: 1.0.0
package:
  files:
    - server.py
    - schemas/input.json
    - schemas/output.json
adapter: mcp
entrypoint:
  type: mcp
  command: server.py
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

fn write_mcp_result_plugin(
    dir: &Path,
    plugin_id: &str,
    response: &str,
    mismatch_response_id: bool,
) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    fs::write(
        dir.join("server.py"),
        format!(
            r#"#!/usr/bin/env python3
import json
import sys

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
    if message.get("method") == "initialize":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}}}}}})
    elif message.get("method") == "tools/list":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"echo_tool"}}]}}}})
    elif message.get("method") == "tools/call":
        envelope = json.loads(r'''{response}''')
        response_id = message["id"] + 1 if {mismatch_response_id} else message["id"]
        write_message({{"jsonrpc":"2.0","id":response_id, **envelope}})
        break
"#,
            response = response,
            mismatch_response_id = if mismatch_response_id { "True" } else { "False" },
        ),
    )
    .expect("server");
    make_executable(&dir.join("server.py"));
    fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"
schema: transist.plugin.v1
id: {plugin_id}
version: 1.0.0
package:
  files:
    - server.py
    - schemas/input.json
    - schemas/output.json
adapter: mcp
entrypoint:
  type: mcp
  command: server.py
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
        ),
    )
    .expect("manifest");
}

fn write_mcp_exec_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(
        dir.join("schemas/input.json"),
        r#"{"type":"object","properties":{"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}},"required":["command"]}"#,
    )
    .expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    let server = r#"#!/usr/bin/env python3
import json, os, subprocess, sys

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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1.0.0"}}})
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"exec_tool"}]}})
    elif method == "tools/call":
        arguments = message["params"].get("arguments") or {}
        completed = subprocess.run(
            [arguments["command"], *(arguments.get("args") or [])],
            capture_output=True,
            text=True,
            cwd=os.environ["TRANSIST_WORKSPACE_DIR"],
        )
        write_message({
            "jsonrpc":"2.0",
            "id":message["id"],
            "result":{"content":[{"type":"text","text":completed.stdout}],
                "structuredContent":{
                    "stdout":completed.stdout,
                    "stderr":completed.stderr,
                    "exit_code":completed.returncode,
                    "ok":completed.returncode == 0
                },
                "isError":completed.returncode != 0}
        })
        break
"#;
    fs::write(dir.join("server.py"), server).expect("server");
    make_executable(&dir.join("server.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: mcp.exec
version: 1.0.0
package:
  files:
    - server.py
    - schemas/input.json
    - schemas/output.json
adapter: mcp
entrypoint:
  type: mcp
  command: server.py
actions:
  - name: exec
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings:
      tool_name: exec_tool
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
    )
    .expect("manifest");
}

fn write_mcp_plugin_with_listed_tool(dir: &Path, listed_tool: &str) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output");
    let server = r#"#!/usr/bin/env python3
import json, sys

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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1.0.0"}}})
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"__LISTED_TOOL__"}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"mcp-ok"}],"structuredContent":{"stdout":"mcp-ok","stderr":"","exit_code":0}}})
        break
"#
    .replace("__LISTED_TOOL__", listed_tool);
    fs::write(dir.join("server.py"), server).expect("server");
    make_executable(&dir.join("server.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: mcp.echo
version: 1.0.0
package:
  files:
    - server.py
    - schemas/input.json
    - schemas/output.json
adapter: mcp
entrypoint:
  type: mcp
  command: server.py
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
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

// ── Sandbox integration tests ────────────────────────────────────────────────

/// Returns true when the host has a usable sandbox backend for the current
/// platform (sandbox-exec on macOS, bwrap on Linux).
fn have_sandbox_backend() -> bool {
    static SANDBOX_BACKEND_AVAILABLE: OnceLock<bool> = OnceLock::new();
    *SANDBOX_BACKEND_AVAILABLE.get_or_init(|| {
        if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .arg("--help")
                .output()
                .is_ok()
        } else if cfg!(target_os = "linux") {
            linux_bwrap_is_usable()
        } else {
            false
        }
    })
}

#[cfg(target_os = "linux")]
fn linux_bwrap_is_usable() -> bool {
    match std::process::Command::new("bwrap")
        .args([
            "--die-with-parent",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
            "--ro-bind",
            "/",
            "/",
            "/bin/true",
        ])
        .output()
    {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "sandbox backend unavailable: bwrap is installed but unusable: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            false
        }
        Err(error) => {
            eprintln!("sandbox backend unavailable: failed to run bwrap probe: {error}");
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn linux_bwrap_is_usable() -> bool {
    false
}

/// Finds the configured, profile-local, or installed sandbox launcher. Coverage
/// tests use their own target directory, so they must also discover the matching
/// launcher installed at the production system location by CI.
#[cfg(target_os = "linux")]
fn find_sandbox_launcher_for_test() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("TRANSIST_SANDBOX_LAUNCHER") {
        let path = std::path::PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("transist-sandbox-launcher");
        if sibling.is_file() {
            return Some(sibling);
        }
        if let Some(profile_dir) = exe.parent().and_then(|path| path.parent()) {
            let candidate = profile_dir.join("transist-sandbox-launcher");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    [
        "/usr/libexec/transist/transist-sandbox-launcher",
        "/usr/local/libexec/transist/transist-sandbox-launcher",
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .find(|path| path.is_file())
}

/// Restores (or removes) a process-global environment variable on drop so a
/// test never leaks its override into concurrently running tests.
#[cfg(target_os = "linux")]
struct EnvGuard {
    key: &'static str,
    old: Option<std::ffi::OsString>,
}

#[cfg(target_os = "linux")]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test-only env mutation; cleaned up deterministically via Drop.
        unsafe {
            match &self.old {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn sandbox_tempdir(prefix: &str) -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp-sandbox-tests");
    fs::create_dir_all(&root).expect("sandbox temp root");
    let root = fs::canonicalize(&root).expect("canonical sandbox temp root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("sandbox tempdir")
}

fn filesystem_policy_json(
    adapter: &str,
    workspace_root: &Path,
    mode: &str,
    read_allow_paths: Vec<&str>,
    write_allow_paths: Vec<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "execution_kind": "external",
        "adapter": adapter,
        "filesystem": {
            "source": "step",
            "workspace_root": workspace_root.display().to_string(),
            "workspace_access": {
                "mode": mode,
            },
            "read_allow_paths": read_allow_paths,
            "write_allow_paths": write_allow_paths,
        },
    })
}

fn local_network_policy_json(adapter: &str, mode: &str) -> serde_json::Value {
    serde_json::json!({
        "execution_kind": "external",
        "adapter": adapter,
        "network": {
            "source": "step",
            "mode": mode,
            "allow_hosts": [],
            "allow_ports": [],
            "tls_required": false,
            "max_redirects": 0,
        }
    })
}

fn sandbox_process_request(
    repo_root: &Path,
    command: &str,
    mode: &str,
    read_allow_paths: Vec<&str>,
    write_allow_paths: Vec<&str>,
) -> ExecutionInvocation {
    let mut invocation = request(
        repo_root,
        AdapterKind::Process,
        "shell",
        "exec",
        serde_json::json!({
            "command": "sh",
            "args": ["-c", command],
        }),
    );
    invocation.request.policy = filesystem_policy_json(
        "process",
        repo_root,
        mode,
        read_allow_paths,
        write_allow_paths,
    );
    invocation
}

fn sandbox_mcp_request(
    repo_root: &Path,
    plugin: &str,
    action: &str,
    input: serde_json::Value,
    mode: &str,
    read_allow_paths: Vec<&str>,
    write_allow_paths: Vec<&str>,
) -> ExecutionInvocation {
    let mut invocation = request(repo_root, AdapterKind::Mcp, plugin, action, input);
    invocation.request.policy =
        filesystem_policy_json("mcp", repo_root, mode, read_allow_paths, write_allow_paths);
    invocation
}

fn sandbox_process_request_with_network(
    repo_root: &Path,
    command: &str,
    mode: &str,
) -> ExecutionInvocation {
    let mut invocation = request(
        repo_root,
        AdapterKind::Process,
        "shell",
        "exec",
        serde_json::json!({
            "command": "/bin/bash",
            "args": ["-lc", command],
        }),
    );
    invocation.request.policy = local_network_policy_json("process", mode);
    invocation
}

#[cfg(target_os = "linux")]
fn non_loopback_ip(test_name: &str) -> IpAddr {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(socket) => socket,
        Err(error) => panic!("udp bind: {error}"),
    };
    if let Err(error) = socket.connect("8.8.8.8:80") {
        panic!("{test_name}: failed to determine host IP: {error}");
    }
    match socket.local_addr() {
        Ok(addr) if !addr.ip().is_loopback() => addr.ip(),
        Ok(_) => panic!("{test_name}: only loopback address available"),
        Err(error) => panic!("{test_name}: failed to read host IP: {error}"),
    }
}

#[test]
fn process_sandbox_denies_outside_workspace_write() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    // The sandbox keeps ordinary host reads available outside protected runtime
    // roots, but writes outside the workspace and staging are denied.
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    // A writable target outside the workspace (same sandbox_tempdir root, but
    // not /tmp or /var/folders, so writes are denied by the sandbox).
    let outside_dir = sandbox_tempdir("outside-");
    let target = outside_dir.path().join("canary.txt");

    let command = format!("printf 'nope' > '{}'", target.display());
    let invocation = sandbox_process_request(repo.path(), &command, "full", vec![], vec![]);
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");
    assert_eq!(result.status, "failed");
    let error = result.error.expect("error must be present");
    assert_eq!(error.code, CanonicalErrorCode::ToolExecutionError);
    assert!(
        !target.exists(),
        "sandbox must deny outside-workspace write"
    );
    assert!(result.output.get("__sandbox_failed__").is_none());
}

#[test]
fn process_sandbox_denies_outside_workspace_write_to_arbitrary_dir() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let outside_dir = sandbox_tempdir("outside-dir-");
    let target = outside_dir.path().join("canary.txt");

    let command = format!("printf 'nope' > '{}'", target.display());
    let invocation = sandbox_process_request(repo.path(), &command, "full", vec![], vec![]);
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");
    assert_eq!(result.status, "failed");
    let error = result.error.expect("error must be present");
    assert_eq!(error.code, CanonicalErrorCode::ToolExecutionError);
    assert!(
        !target.exists(),
        "sandbox must deny outside-workspace write"
    );
}

#[test]
fn process_sandbox_allows_workspace_read_and_write_in_full_mode() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let src_file = repo.path().join("src/input.txt");
    let out_file = repo.path().join("reports/output.txt");
    fs::create_dir_all(src_file.parent().expect("parent")).expect("src dir");
    fs::create_dir_all(out_file.parent().expect("parent")).expect("reports dir");
    fs::write(&src_file, b"input").expect("src file");

    let command = format!(
        "cat '{}' >/dev/null && printf 'ok' > '{}'",
        src_file.display(),
        out_file.display()
    );
    let invocation = sandbox_process_request(repo.path(), &command, "full", vec![], vec![]);
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");

    assert_eq!(result.status, "success", "unexpected result: {result:?}");
    assert_eq!(fs::read_to_string(&out_file).expect("out file"), "ok");
}

/// Combined process-adapter and MCP-adapter smoke test with `loopback` network
/// policy on Linux. Verifies that the `transist-sandbox-launcher` handoff works
/// for both adapter types: the launcher raises `lo`, drops all capabilities, and
/// `exec`s the plugin (process shell or MCP server) correctly.
///
/// For the process adapter the probe self-connects on loopback to confirm lo is
/// up. For the MCP adapter, a successful echo action confirms the launcher
/// handed off to the MCP server without breaking its stdio communication channel.
#[cfg(target_os = "linux")]
#[test]
fn process_and_mcp_execute_loopback_policy_with_launcher() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    // The production launcher resolver only checks the direct `current_exe`
    // sibling and `/usr/libexec`; under `cargo test` the workspace-built
    // launcher lives at the target profile root. Locate it and export the
    // documented `TRANSIST_SANDBOX_LAUNCHER` override so the loopback handoff
    // resolves. A missing launcher is a missing required test prerequisite.
    let launcher = find_sandbox_launcher_for_test()
        .expect("build transist-sandbox-launcher before running Linux adapter tests");
    let _env_guard = EnvGuard {
        key: "TRANSIST_SANDBOX_LAUNCHER",
        old: std::env::var_os("TRANSIST_SANDBOX_LAUNCHER"),
    };
    // SAFETY: test-only env mutation; the guard restores the prior value on drop.
    unsafe { std::env::set_var("TRANSIST_SANDBOX_LAUNCHER", &launcher) };

    // --- Process adapter: loopback self-connect ---
    let repo = sandbox_tempdir("sandbox-loopback-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    // Python3 self-connect: binds a server on sandbox-internal 127.0.0.1, then
    // connects to it. Succeeds only when the launcher has raised loopback.
    let loopback_probe = r#"python3 -c '
import socket, sys, threading
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 0))
srv.listen(1)
port = srv.getsockname()[1]
def serve():
    try: conn, _ = srv.accept(); conn.close()
    except Exception: pass
t = threading.Thread(target=serve, daemon=True)
t.start()
try:
    c = socket.create_connection(("127.0.0.1", port), timeout=2)
    c.close(); srv.close(); sys.exit(0)
except OSError as e:
    print(f"loopback self-connect failed: {e}", file=sys.stderr)
    srv.close(); sys.exit(1)
'"#;
    let invocation = sandbox_process_request_with_network(repo.path(), loopback_probe, "loopback");
    let proc_result = process_adapter()
        .execute_step(&invocation)
        .expect("process sandbox execute");
    assert_eq!(
        proc_result.status, "success",
        "process adapter: loopback self-connect must succeed with launcher: {proc_result:?}"
    );

    // --- MCP adapter: echo action with loopback network policy ---
    // The MCP server communicates via stdio (not loopback), so this test
    // verifies the launcher handoff does not break the stdio communication
    // channel used by the MCP protocol.
    let mcp_repo = sandbox_tempdir("sandbox-mcp-loopback-");
    fs::create_dir_all(mcp_repo.path().join(".transist")).expect("mcp repo");
    let mcp_plugin_src = mcp_repo.path().join("mcp-echo");
    write_mcp_plugin(&mcp_plugin_src);
    install_plugin_file(mcp_repo.path(), &mcp_plugin_src.join("plugin.yaml")).expect("install mcp");

    let mut mcp_invocation = request(
        mcp_repo.path(),
        AdapterKind::Mcp,
        "mcp.echo",
        "echo",
        serde_json::json!({"message": "loopback-launcher-test"}),
    );
    // Combined filesystem + loopback network policy for the MCP sandbox.
    mcp_invocation.request.policy = serde_json::json!({
        "execution_kind": "external",
        "adapter": "mcp",
        "filesystem": {
            "source": "step",
            "workspace_root": mcp_repo.path().display().to_string(),
            "workspace_access": { "mode": "full" },
            "read_allow_paths": [],
            "write_allow_paths": [],
        },
        "network": {
            "source": "step",
            "mode": "loopback",
            "allow_hosts": [],
            "allow_ports": [],
            "tls_required": false,
            "max_redirects": 0,
        }
    });
    let mcp_result = mcp_adapter()
        .execute_step(&mcp_invocation)
        .expect("mcp sandbox execute");
    assert_eq!(
        mcp_result.status, "success",
        "MCP adapter: echo action with loopback policy must succeed with launcher: {mcp_result:?}"
    );
}

#[test]
fn process_sandbox_allows_runtime_staging_outside_run_root() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let staging_dir = repo.path().join(".transist-staging/invoke-test");
    let artifacts_dir = staging_dir.join("artifacts");
    let evidence_dir = staging_dir.join("evidence");
    fs::create_dir_all(&artifacts_dir).expect("staging artifacts");
    fs::create_dir_all(&evidence_dir).expect("staging evidence");

    let mut invocation = sandbox_process_request(
        repo.path(),
        "printf 'ok' > \"$TRANSIST_ARTIFACTS_DIR/output.txt\"",
        "full",
        vec![],
        vec![],
    );
    invocation.request.paths.artifacts_dir = artifacts_dir.display().to_string();
    invocation.request.paths.evidence_dir = evidence_dir.display().to_string();
    invocation.request.metadata["__transist_staging_dir"] =
        serde_json::Value::String(staging_dir.display().to_string());

    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");
    assert_eq!(result.status, "success", "unexpected result: {result:?}");
    assert_eq!(
        fs::read_to_string(artifacts_dir.join("output.txt")).expect("output"),
        "ok"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn process_sandbox_denies_runtime_home_and_sibling_staging_reads() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    let workspace = sandbox_tempdir("workspace-");
    let home = repo.path().join(".transist");
    fs::create_dir_all(home.join("runs/other")).expect("runtime home");
    fs::write(home.join("runs/other/state.json"), "durable-state").expect("state");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let current_staging = home.join(".transist-staging/invoke-current");
    let sibling_staging = home.join(".transist-staging/invoke-sibling");
    fs::create_dir_all(current_staging.join("artifacts")).expect("current artifacts");
    fs::create_dir_all(current_staging.join("evidence")).expect("current evidence");
    fs::create_dir_all(&sibling_staging).expect("sibling staging");
    let sibling_file = sibling_staging.join("secret.tmp");
    fs::write(&sibling_file, "sibling-secret").expect("sibling secret");

    for target in [home.join("runs/other/state.json"), sibling_file] {
        let command = format!("cat '{}'", target.display());
        let mut invocation = sandbox_process_request(repo.path(), &command, "full", vec![], vec![]);
        invocation.request.paths.workspace_dir = workspace.path().display().to_string();
        invocation.request.paths.run_root = current_staging.display().to_string();
        invocation.request.paths.artifacts_dir =
            current_staging.join("artifacts").display().to_string();
        invocation.request.paths.evidence_dir =
            current_staging.join("evidence").display().to_string();
        invocation.request.metadata["__transist_staging_dir"] =
            serde_json::Value::String(current_staging.display().to_string());

        let result = process_adapter()
            .execute_step(&invocation)
            .expect("sandbox execute");
        assert_eq!(result.status, "failed", "unexpected result: {result:?}");
        assert!(
            !result.output["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("secret")
        );
        assert!(
            !result.output["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("durable")
        );
    }
}

#[test]
fn process_sandbox_denies_workspace_write_in_read_only_mode() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let out_file = repo.path().join("reports/readonly.txt");
    fs::create_dir_all(out_file.parent().expect("parent")).expect("reports dir");

    let command = format!("printf 'nope' > '{}'", out_file.display());
    let invocation = sandbox_process_request(repo.path(), &command, "read_only", vec![], vec![]);
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");
    assert_eq!(result.status, "failed");
    assert!(
        !out_file.exists(),
        "read_only sandbox must deny workspace writes"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn process_sandbox_custom_mode_is_rejected_before_dispatch() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let out_file = repo.path().join("reports/output.txt");
    fs::create_dir_all(out_file.parent().expect("parent")).expect("reports dir");

    // custom mode with only read_allow_paths used to be accepted on macOS via
    // the allow-default profile; it is now rejected uniformly before dispatch
    // with filesystem_custom_mode_unsupported on both Linux and macOS.
    let command = format!(
        "cat '{}' >/dev/null",
        repo.path().join("src/input.txt").display()
    );
    let invocation =
        sandbox_process_request(repo.path(), &command, "custom", vec!["src/**"], vec![]);
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");
    assert_eq!(result.status, "failed");
    let error = result.error.expect("error");
    assert_eq!(error.code, CanonicalErrorCode::PolicyDenied);
    assert!(result.output.get("__sandbox_failed__").is_some());
    assert!(
        result.output["__sandbox_failed__"]
            .to_string()
            .contains("filesystem_custom_mode_unsupported")
    );
}

#[test]
fn process_sandbox_custom_mode_with_write_allow_paths_is_rejected_before_dispatch() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let out_file = repo.path().join("reports/output.txt");
    fs::create_dir_all(out_file.parent().expect("parent")).expect("reports dir");

    let command = format!("printf 'ok' > '{}'", out_file.display());
    let invocation = sandbox_process_request(
        repo.path(),
        &command,
        "custom",
        vec!["src/**"],
        vec!["reports/**"],
    );
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");
    assert_eq!(result.status, "failed");
    let error = result.error.expect("error");
    assert_eq!(error.code, CanonicalErrorCode::PolicyDenied);
    assert!(result.output.get("__sandbox_failed__").is_some());
    assert!(
        result.output["__sandbox_failed__"]
            .to_string()
            .contains("filesystem_custom_mode_unsupported")
    );
    assert!(!out_file.exists());
}

#[test]
fn process_preflight_reports_filesystem_custom_mode_unsupported() {
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let mut invocation = preflight_request(
        repo.path(),
        AdapterKind::Process,
        "shell",
        "exec",
        serde_json::json!({"command": "sh", "args": ["-c", "true"]}),
    );
    invocation.request.policy = serde_json::json!({
        "execution_kind": "external",
        "adapter": "process",
        "filesystem": {
            "workspace_access": {
                "mode": "custom"
            }
        },
    });

    let checks = process_adapter()
        .preflight_steps(&[invocation])
        .expect("preflight");
    let fs_check = find_check(&checks, "filesystem_workspace_boundary");
    assert_eq!(fs_check.status, PreflightStatus::Failed);
    assert!(
        fs_check
            .message
            .contains("filesystem_custom_mode_unsupported")
    );
}

#[test]
fn mcp_sandbox_denies_outside_workspace_write() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("mcp-exec-plugin");
    write_mcp_exec_plugin(&plugin_src);
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install mcp exec");

    let outside_dir = sandbox_tempdir("mcp-outside-");
    let target = outside_dir.path().join("canary.txt");

    let invocation = sandbox_mcp_request(
        repo.path(),
        "mcp.exec",
        "exec",
        serde_json::json!({"command": "sh", "args": ["-c", format!("printf nope > '{}'", target.display())]}),
        "full",
        vec![],
        vec![],
    );
    let result = mcp_adapter().execute_step(&invocation).expect("result");
    assert_eq!(result.status, "failed");
    assert!(
        !target.exists(),
        "sandbox must deny outside-workspace write"
    );
}

/// Verifies that a sandbox setup failure (non-existent workspace) produces a
/// PolicyDenied result with `__sandbox_failed__` metadata so the kernel can
/// emit a durable `filesystem_policy.sandbox_failed` event.
#[test]
fn process_sandbox_setup_failure_produces_policy_denied() {
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let run_root = repo.path().join(".transist/runs/run_sandbox_fail");
    let artifacts_dir = run_root.join("artifacts/step");
    let evidence_dir = run_root.join("evidence/step");
    fs::create_dir_all(&artifacts_dir).expect("artifacts");
    fs::create_dir_all(&evidence_dir).expect("evidence");
    write_run_state_and_recipe(&run_root, "wf", "v1", "process", "shell", "exec");

    let invocation = ExecutionInvocation {
        request: ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_sandbox_fail".to_string(),
            run_id: "run_sandbox_fail".to_string(),
            workflow_id: "wf".to_string(),
            step: InvokeStep {
                id: "sandbox_fail_step".to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "exec".to_string(),
            input: serde_json::json!({
                "command": "sh",
                "args": ["-c", "true"],
            }),
            paths: InvokePaths {
                run_root: run_root.display().to_string(),
                artifacts_dir: artifacts_dir.display().to_string(),
                evidence_dir: evidence_dir.display().to_string(),
                workspace_dir: "/nonexistent/workspace/path".to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: Some(10_000),
            max_timeout_ms: None,
            policy: filesystem_policy_json(
                "process",
                Path::new("/nonexistent/workspace/path"),
                "full",
                vec![],
                vec![],
            ),
            metadata: serde_json::json!({ "plugin_id": "shell" }),
        },
        secret_values: BTreeMap::new(),
        remote_secret_policy: RemoteSecretPolicy::default(),
    };

    let result = process_adapter().execute_step(&invocation).expect("result");
    assert_eq!(result.status, "failed");
    let error = result.error.expect("error");
    assert_eq!(error.code, CanonicalErrorCode::PolicyDenied);
    assert!(result.output.get("__sandbox_failed__").is_some());
}

#[test]
fn process_sandbox_plugin_exit_before_handshake_is_not_policy_denied() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let plugin_src = repo.path().join("process-no-handshake");
    write_process_no_handshake_plugin(&plugin_src, "process.no_handshake");
    install_plugin_file(repo.path(), &plugin_src.join("plugin.yaml")).expect("install");

    let mut invocation = request(
        repo.path(),
        AdapterKind::Process,
        "process.no_handshake",
        "echo",
        serde_json::json!({}),
    );
    invocation.request.policy =
        filesystem_policy_json("process", repo.path(), "full", vec![], vec![]);

    let result = process_adapter().execute_step(&invocation).expect("result");
    let error = result.error.expect("error");
    assert_eq!(result.status, "failed");
    // The plugin may exit before or after the host writes its handshake frame.
    // Both outcomes are transport-level failures; neither is sandbox policy
    // denial. Do not make the test depend on process scheduling.
    assert!(matches!(
        error.code,
        CanonicalErrorCode::TimeoutError | CanonicalErrorCode::TransportError
    ));
    assert!(result.output.get("__sandbox_failed__").is_none());
}

#[test]
fn process_sandbox_write_denial_produces_failed_result_with_error() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    // Verifies that a write-denial from the sandbox surfaces as a
    // ToolExecutionError result (not PolicyDenied / __sandbox_failed__).
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let outside_dir = sandbox_tempdir("denial-outside-");
    let target = outside_dir.path().join("denied.txt");

    let invocation = sandbox_process_request(
        repo.path(),
        &format!("printf 'nope' > '{}'", target.display()),
        "full",
        vec![],
        vec![],
    );
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");
    assert_eq!(result.status, "failed");
    let error = result.error.expect("error must be present");
    assert_eq!(error.code, CanonicalErrorCode::ToolExecutionError);
    assert!(error.message.contains("exited with code") || !error.message.is_empty());
    assert!(result.output.get("__sandbox_failed__").is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn process_network_loopback_allows_loopback_reachability() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let command = r#"python3 -c 'import socket; server = socket.socket(); server.bind(("127.0.0.1", 0)); server.listen(1); client = socket.create_connection(("127.0.0.1", server.getsockname()[1]), timeout=1); peer, _ = server.accept(); client.sendall(b"ok"); assert peer.recv(2) == b"ok"'"#;
    let invocation = sandbox_process_request_with_network(repo.path(), command, "loopback");
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");

    assert_eq!(result.status, "success", "unexpected result: {result:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn process_network_none_blocks_loopback_reachability() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    // Bind a loopback listener outside the sandbox on an ephemeral port, then
    // attempt to connect to it from inside the sandboxed process under
    // `network.mode: none`. This must fail: `none` denies outbound
    // connections including loopback, unlike `loopback`.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    let port = listener.local_addr().expect("listener addr").port();
    // Accept in the background so a (denied) connection attempt does not hang
    // this test if the sandbox regresses and the connection is allowed.
    thread::spawn(move || {
        let _ = listener.accept();
    });

    let command = format!("if exec 4<>/dev/tcp/127.0.0.1/{port}; then exit 17; else exit 0; fi");
    let invocation = sandbox_process_request_with_network(repo.path(), &command, "none");
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");

    assert_eq!(
        result.status, "success",
        "network.mode: none must deny loopback connections; result={result:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn process_network_none_fast_fails_remote_egress_on_macos() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    // 192.0.2.0/24 is reserved TEST-NET-1. Under the macOS sandbox egress deny,
    // this outbound attempt should fail immediately at the kernel boundary.
    // Measure the actual connection denial, independently of package/schema
    // loading, sandbox startup, and host scheduling before the probe starts.
    let command = r#"python3 - <<'PY'
import errno, json, socket, sys, time
with socket.socket() as connection:
    connection.settimeout(10)
    started = time.monotonic()
    code = connection.connect_ex(('192.0.2.1', 80))
    elapsed = time.monotonic() - started
denied = code in (errno.EPERM, errno.EACCES)
print(json.dumps({'policy_denied': denied, 'elapsed_seconds': elapsed, 'errno': code}))
sys.exit(0 if denied else 17)
PY"#;
    let invocation = sandbox_process_request_with_network(repo.path(), command, "none");
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");

    assert_eq!(result.status, "success", "unexpected result: {result:?}");
    let observed: serde_json::Value = serde_json::from_str(
        result.output["stdout"]
            .as_str()
            .expect("socket probe output"),
    )
    .expect("socket probe JSON");
    assert_eq!(observed["policy_denied"], true, "{observed}");
    assert!(
        observed["elapsed_seconds"]
            .as_f64()
            .expect("probe duration")
            < 2.0,
        "expected kernel network deny to fail quickly: {observed}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn process_network_none_blocks_non_loopback_egress_on_linux() {
    assert!(have_sandbox_backend(), "no sandbox backend available");

    let non_loopback_ip =
        non_loopback_ip("process_network_none_blocks_non_loopback_egress_on_linux");
    let external_server = match TcpListener::bind((non_loopback_ip, 0)) {
        Ok(server) => server,
        Err(error) => panic!("bind external: {error}"),
    };

    let external_port = external_server.local_addr().expect("external addr").port();
    let repo = sandbox_tempdir("sandbox-");
    fs::create_dir_all(repo.path().join(".transist")).expect("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/shell");
    install_plugin_file(repo.path(), &source.join("plugin.yaml")).expect("install shell");

    let command = format!(
        "if exec 4<>/dev/tcp/{non_loopback_ip}/{external_port}; then exit 17; else exit 0; fi"
    );
    let invocation = sandbox_process_request_with_network(repo.path(), &command, "none");
    let result = process_adapter()
        .execute_step(&invocation)
        .expect("sandbox execute");

    assert_eq!(result.status, "success", "unexpected result: {result:?}");
}
