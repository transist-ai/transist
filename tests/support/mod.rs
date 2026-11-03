// SPDX-License-Identifier: MPL-2.0
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::Value;

pub mod process_lifecycle;

/// Installs the shared APP lifecycle fixture in the isolated test runtime home.
pub fn install_process_lifecycle_plugin(repo_root: &Path) {
    let dir = repo_root.join("fixture-lifecycle-plugin");
    process_lifecycle::write_plugin(&dir);
    transist::TransistContext::repo_local(repo_root)
        .install_plugin_file(&dir.join("plugin.yaml"))
        .expect("install lifecycle fixture");
}

pub fn init_repo(repo_root: &Path) {
    transist::init_home_dir_with_mode(
        &transist::repo_local_home_dir(repo_root),
        transist::RuntimeOperatingMode::Standalone,
    )
    .expect("init repo");
}

pub fn install_shell_plugin(repo_root: &Path) {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let shell_plugin = [
        manifest_dir.join("plugins/shell"),
        manifest_dir.join("../..").join("plugins/shell"),
        manifest_dir.join("../../..").join("plugins/shell"),
        manifest_dir.join("../../../..").join("plugins/shell"),
    ]
    .into_iter()
    .find(|path| path.exists())
    .expect("locate shell plugin fixture");
    transist::TransistContext::repo_local(repo_root)
        .install_plugin_file(&shell_plugin.join("plugin.yaml"))
        .expect("install shell plugin");
}

pub fn install_fixture_process_plugin(repo_root: &Path) {
    let plugin_dir = repo_root.join("fixture-process-plugin");
    write_fixture_process_plugin(&plugin_dir);
    transist::TransistContext::repo_local(repo_root)
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install fixture plugin");
}

pub fn write_fixture_process_plugin_dir(plugin_dir: &Path) {
    write_fixture_process_plugin(plugin_dir);
}

pub fn install_fixture_local_process_plugin(repo_root: &Path) {
    let plugin_dir = repo_root.join("fixture-local-process-plugin");
    write_fixture_local_process_plugin(&plugin_dir);
    transist::TransistContext::repo_local(repo_root)
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install fixture local plugin");
}

pub fn install_fixture_secret_process_plugin(repo_root: &Path) {
    let plugin_dir = repo_root.join("fixture-secret-process-plugin");
    write_fixture_secret_process_plugin(&plugin_dir);
    transist::TransistContext::repo_local(repo_root)
        .install_plugin_file(&plugin_dir.join("plugin.yaml"))
        .expect("install fixture secret plugin");
}

pub fn write_flow(repo_root: &Path, file_name: &str, contents: &str) -> PathBuf {
    let path = repo_root.join(file_name);
    fs::write(&path, contents).expect("write flow");
    path
}

pub fn compile_prepare(
    repo_root: &Path,
    flow_path: &Path,
    workflow_ref: &str,
    run_id: &str,
    inputs: BTreeMap<String, transist::KernelValue>,
) -> transist::PrepareReport {
    transist::TransistContext::repo_local(repo_root)
        .compile_flow_file(flow_path)
        .expect("compile flow");
    transist::TransistContext::repo_local(repo_root)
        .prepare_workflow(workflow_ref, Some(run_id), inputs)
        .expect("prepare run")
}

pub fn run_root(repo_root: &Path, run_id: &str) -> PathBuf {
    transist::TransistContext::repo_local(repo_root)
        .run_root(run_id)
        .expect("resolve run root")
}

pub fn read_state(repo_root: &Path, run_id: &str) -> Value {
    read_json(&run_root(repo_root, run_id).join("state.json"))
}

pub fn write_state(repo_root: &Path, run_id: &str, value: &Value) {
    write_json(&run_root(repo_root, run_id).join("state.json"), value);
}

pub fn read_execution(repo_root: &Path, run_id: &str, step_id: &str, execution: u32) -> Value {
    read_json(
        &run_root(repo_root, run_id)
            .join("step_executions")
            .join(step_id)
            .join(format!("execution_{execution:03}.json")),
    )
}

pub fn read_run_inputs(repo_root: &Path, run_id: &str) -> Value {
    read_json(&run_root(repo_root, run_id).join("context/run_inputs.json"))
}

pub fn read_provenance(repo_root: &Path, run_id: &str) -> Value {
    read_json(&run_root(repo_root, run_id).join("provenance.json"))
}

pub fn read_events(repo_root: &Path, run_id: &str) -> Vec<Value> {
    let path = run_root(repo_root, run_id).join("events.jsonl");
    let raw = fs::read_to_string(&path).expect("read events");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("event json"))
        .collect()
}

pub fn read_recipe(repo_root: &Path, run_id: &str) -> String {
    fs::read_to_string(run_root(repo_root, run_id).join("recipe.yaml")).expect("read recipe")
}

pub fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .expect("read text file")
        .lines()
        .map(str::to_string)
        .collect()
}

pub fn collect_text(root: &Path) -> String {
    fn walk(path: &Path, out: &mut String) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if let Ok(contents) = fs::read_to_string(&path) {
                out.push_str(&contents);
            }
        }
    }

    let mut out = String::new();
    walk(root, &mut out);
    out
}

pub fn assert_no_secret_variants(root: &Path, secret: &str) {
    let mut variants = vec![secret.as_bytes().to_vec()];
    for engine in [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::URL_SAFE,
    ] {
        let encoded = engine.encode(secret);
        variants.push(encoded.as_bytes().to_vec());
        variants.push(encoded.trim_end_matches('=').as_bytes().to_vec());
    }

    fn walk(path: &Path, variants: &[Vec<u8>]) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let path_text = path.to_string_lossy();
            assert!(
                variants.iter().all(|variant| {
                    std::str::from_utf8(variant)
                        .map(|variant| !path_text.contains(variant))
                        .unwrap_or(true)
                }),
                "secret variant must not appear in persisted path: {}",
                path.display()
            );
            if path.is_dir() {
                walk(&path, variants);
            } else if let Ok(bytes) = fs::read(&path) {
                for variant in variants {
                    assert!(
                        !bytes.windows(variant.len()).any(|window| window == variant),
                        "secret variant must not appear in persisted file: {}",
                        path.display()
                    );
                }
            }
        }
    }

    walk(root, &variants);
}

pub fn read_json(path: &Path) -> Value {
    let raw = fs::read_to_string(path).expect("read json");
    serde_json::from_str(&raw).expect("parse json")
}

pub fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize json"),
    )
    .expect("write json");
}

fn write_fixture_process_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input schema");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output schema");
    fs::write(
        dir.join("plugin.py"),
        r#"#!/usr/bin/env python3
import json
import sys

APP_PROTOCOL = "app.v1"


def emit(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def read_frame():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


def success_result(request, payload):
    stdout = payload.get("stdout", "fixture-ok")
    artifact_contents = payload.get("artifact_contents", "artifact-ok")
    evidence_contents = payload.get("evidence_contents", "evidence-ok")
    usage = payload.get("usage")
    return {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": request["invoke_id"],
        "status": "success",
        "output": {
            "stdout": stdout,
            "stderr": "",
            "exit_code": 0,
        },
        "artifact_hints": [
            {"name": "stdout.txt", "contents": artifact_contents},
        ],
        "evidence_hints": [
            {"name": "trace.log", "contents": evidence_contents},
        ],
        "metrics": {},
        "usage": usage,
        "error": None,
    }


handshake = read_frame()
if handshake is None:
    raise SystemExit(1)

emit(
    {
        "type": "app.handshake.response.v1",
        "protocol_version": APP_PROTOCOL,
        "plugin": {"id": "fixture.process", "version": "1.0.0"},
        "adapter_kind": "process",
        "capabilities": {
            "actions": ["emit", "malformed_type"],
            "supports_cancel": True,
            "supports_health": False,
            "supports_artifact_hints": True,
            "supports_evidence_hints": True,
        },
    }
)

request = read_frame()
if request is None:
    raise SystemExit(1)

action = request.get("action")
payload = request.get("input") or {}

if action == "emit":
    emit(success_result(request, payload))
    raise SystemExit(0)

if action == "malformed_type":
    result = success_result(request, payload)
    result["type"] = "app.unexpected.result.v1"
    emit(result)
    raise SystemExit(0)

emit(
    {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": request["invoke_id"],
        "status": "failed",
        "output": {},
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {},
        "usage": None,
        "error": {
            "code": "validation_error",
            "class": "validation",
            "message": f"unsupported action {action}",
            "retryability": "none",
        },
    }
)
"#,
    )
    .expect("plugin script");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: fixture.process
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
  - name: emit
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {}
  - name: malformed_type
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: true
  supports_evidence_hints: true
"#,
    )
    .expect("plugin manifest");
}

fn write_fixture_local_process_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input schema");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output schema");
    fs::write(
        dir.join("plugin.py"),
        r#"#!/usr/bin/env python3
import json
import sys

APP_PROTOCOL = "app.v1"


def emit(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def read_frame():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


handshake = read_frame()
if handshake is None:
    raise SystemExit(1)

emit(
    {
        "type": "app.handshake.response.v1",
        "protocol_version": APP_PROTOCOL,
        "plugin": {"id": "fixture.local", "version": "1.0.0"},
        "adapter_kind": "process",
        "capabilities": {
            "actions": ["emit"],
            "supports_cancel": True,
            "supports_health": False,
            "supports_artifact_hints": True,
            "supports_evidence_hints": True,
        },
    }
)

request = read_frame()
if request is None:
    raise SystemExit(1)

emit(
    {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": request["invoke_id"],
        "status": "success",
        "output": {
            "stdout": "local-ok",
            "stderr": "",
            "exit_code": 0,
        },
        "artifact_hints": [
            {"name": "stdout.txt", "contents": "local-artifact"},
        ],
        "evidence_hints": [
            {"name": "trace.log", "contents": "local-evidence"},
        ],
        "metrics": {},
        "usage": None,
        "error": None,
    }
)
"#,
    )
    .expect("plugin script");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: fixture.local
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
default_provider_kind: local
actions:
  - name: emit
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs: []
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: true
  supports_evidence_hints: true
"#,
    )
    .expect("plugin manifest");
}

fn write_fixture_secret_process_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input schema");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output schema");
    fs::write(
        dir.join("plugin.py"),
        r#"#!/usr/bin/env python3
import json
import os
import sys

APP_PROTOCOL = "app.v1"


def emit(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def read_frame():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


handshake = read_frame()
if handshake is None:
    raise SystemExit(1)

emit(
    {
        "type": "app.handshake.response.v1",
        "protocol_version": APP_PROTOCOL,
        "plugin": {"id": "fixture.secret", "version": "1.0.0"},
        "adapter_kind": "process",
        "capabilities": {
            "actions": ["emit_secret_echo"],
            "supports_cancel": True,
            "supports_health": False,
            "supports_artifact_hints": True,
            "supports_evidence_hints": True,
        },
    }
)

request = read_frame()
if request is None:
    raise SystemExit(1)

secret = os.environ.get("FIXTURE_SECRET", "")

emit(
    {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": request["invoke_id"],
        "status": "success",
        "output": {
            "stdout": secret,
            "stderr": "",
            "exit_code": 0,
        },
        "artifact_hints": [
            {"name": "secret.txt", "contents": secret},
        ],
        "evidence_hints": [
            {"name": "secret-trace.log", "contents": secret},
        ],
        "metrics": {},
        "usage": None,
        "error": None,
    }
)
"#,
    )
    .expect("plugin script");
    make_executable(&dir.join("plugin.py"));
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: fixture.secret
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
  - name: emit_secret_echo
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs:
      - name: API_TOKEN
        required: true
        binding:
          type: env
          name: FIXTURE_SECRET
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: true
  supports_evidence_hints: true
"#,
    )
    .expect("plugin manifest");
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
