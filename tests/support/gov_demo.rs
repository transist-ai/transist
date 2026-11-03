// SPDX-License-Identifier: MPL-2.0
#![allow(dead_code)]

use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use transist::{
    GateDecisionInput, KernelValue, RunEvidenceExport, RuntimeOperatingMode, TransistContext,
    import_trust_root_file, init_home_dir_with_mode, signature_sidecar_path,
};
use transist_admin::{
    export_public_key, generate_key, sign_plugin as admin_sign_plugin,
    sign_workflow as admin_sign_workflow,
};

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("gov_demo")
}

pub fn init_gov_repo(repo_root: &Path) -> PathBuf {
    let home_dir = transist::repo_local_home_dir(repo_root);
    init_home_dir_with_mode(&home_dir, RuntimeOperatingMode::Standalone).expect("init gov home");
    let config = home_dir.join("config.yaml");
    let config_text = fs::read_to_string(&config).expect("read gov config");
    fs::write(
        &config,
        config_text.replace("process: false", "process: true"),
    )
    .expect("enable local gov secret delivery");
    let trust_dir = repo_root.join(".test-trust");
    fs::create_dir_all(&trust_dir).expect("create test trust directory");
    let key_path = trust_dir.join("gov-test.key");
    let public_key_path = trust_dir.join("gov-test.pub");
    generate_key("gov-test", &key_path).expect("generate gov test key");
    export_public_key(&key_path, &public_key_path).expect("export gov test key");
    import_trust_root_file(&home_dir, &public_key_path).expect("import gov trust root");
    home_dir
}

pub fn context(repo_root: &Path, home_dir: &Path) -> TransistContext {
    TransistContext::with_home(repo_root, home_dir)
}

pub fn run_root(home_dir: &Path, run_id: &str) -> PathBuf {
    transist::TransistContext::with_home(home_dir, home_dir)
        .run_root(run_id)
        .expect("resolve run root")
}

pub fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read json")).expect("parse json")
}

pub fn read_state(home_dir: &Path, run_id: &str) -> serde_json::Value {
    read_json(&run_root(home_dir, run_id).join("state.json"))
}

pub fn read_provenance(home_dir: &Path, run_id: &str) -> serde_json::Value {
    read_json(&run_root(home_dir, run_id).join("provenance.json"))
}

pub fn read_gate(home_dir: &Path, run_id: &str, gate_id: &str) -> serde_json::Value {
    read_json(
        &run_root(home_dir, run_id)
            .join("gates")
            .join(format!("{gate_id}.json")),
    )
}

pub fn read_latest_gate_history(home_dir: &Path, run_id: &str, gate_id: &str) -> serde_json::Value {
    fn collect_history_dirs(path: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                if child.file_name().and_then(|name| name.to_str()) == Some("history")
                    && child
                        .parent()
                        .and_then(|parent| parent.file_name())
                        .and_then(|name| name.to_str())
                        == Some("gates")
                {
                    out.push(child.clone());
                }
                collect_history_dirs(&child, out);
            }
        }
    }

    let mut history_dirs = Vec::new();
    collect_history_dirs(&run_root(home_dir, run_id), &mut history_dirs);
    history_dirs.sort();

    for history_dir in history_dirs {
        let mut entries = fs::read_dir(&history_dir)
            .expect("read gate history dir")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect gate history entries");
        entries.sort_by_key(|entry| entry.file_name());
        if let Some(path) = entries
            .into_iter()
            .map(|entry| entry.path())
            .rev()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&format!("{gate_id}-")) && name.ends_with("-approved.json")
                    })
            })
        {
            return read_json(&path);
        }
    }

    panic!("matching gate history file for {gate_id} not found beneath run tree");
}

pub fn export_evidence(home_dir: &Path, run_id: &str) -> RunEvidenceExport {
    TransistContext::with_home(home_dir, home_dir)
        .run_evidence_export(run_id)
        .expect("export evidence")
}

pub fn approve_gate_with_identity(ctx: &TransistContext, run_id: &str, gate_id: &str) {
    ctx.approve_gate_with_input(
        run_id,
        gate_id,
        GateDecisionInput {
            actor: Some("test-reviewer <test@example.invalid>".to_string()),
            actor_source: Some("test".to_string()),
            reason: Some("integration test approval".to_string()),
            decided_at_ms: Some(42),
        },
    )
    .expect("approve gate with identity");
}

fn signing_key_path(path: &Path) -> PathBuf {
    path.parent()
        .expect("test subject parent")
        .join(".test-trust/gov-test.key")
}

pub fn sign_workflow(path: &Path, _workflow_id: &str, _workflow_version: &str) -> Result<()> {
    admin_sign_workflow(path, &signing_key_path(path), &signature_sidecar_path(path))?;
    Ok(())
}

pub fn sign_plugin(path: &Path, plugin_id: &str, plugin_version: &str) -> Result<()> {
    let _ = (plugin_id, plugin_version);
    admin_sign_plugin(path, &signing_key_path(path), &signature_sidecar_path(path))?;
    Ok(())
}

pub fn install_signed_external_plugin(ctx: &TransistContext, repo_root: &Path) -> PathBuf {
    let dir = repo_root.join("gov-external-plugin");
    write_stub_plugin(&dir, "gov.external", "1.0.0", Some("external"), true);
    sign_plugin(&dir, "gov.external", "1.0.0").expect("sign external plugin");
    ctx.install_plugin_dir_with_trust(&dir, true)
        .expect("install signed external plugin");
    dir
}

pub fn install_signed_local_plugin(ctx: &TransistContext, repo_root: &Path) -> PathBuf {
    let dir = repo_root.join("gov-local-plugin");
    write_stub_plugin(&dir, "gov.local", "1.0.0", Some("local"), false);
    sign_plugin(&dir, "gov.local", "1.0.0").expect("sign local plugin");
    ctx.install_plugin_dir_with_trust(&dir, true)
        .expect("install signed local plugin");
    dir
}

pub fn write_unsigned_external_plugin(repo_root: &Path) -> PathBuf {
    let dir = repo_root.join("gov-unsigned-plugin");
    write_stub_plugin(&dir, "gov.unsigned", "1.0.0", Some("external"), true);
    dir
}

pub fn install_test_plugin_v0(ctx: &TransistContext, repo_root: &Path) -> PathBuf {
    let dir = repo_root.join("gov-test-plugin");
    write_stub_plugin(&dir, "gov.test", "v0", Some("external"), true);
    ctx.install_plugin_dir(&dir)
        .expect("install v0 test plugin");
    dir
}

pub fn write_positive_workflow(repo_root: &Path) -> PathBuf {
    let module_dir = repo_root.join("modules");
    fs::create_dir_all(&module_dir).expect("create module dir");
    fs::write(
        module_dir.join("review.yaml"),
        r#"schema: transist.module.v1
id: gov_review_subflow
version: v1
steps:
  - id: plan_review
    control: control.gate.manual
    with:
      gate_id: plan-review
"#,
    )
    .expect("write review subflow");

    let workflow = repo_root.join("gov-positive.workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_positive
version: v1
plugins:
  gov.external: 1.0.0
  gov.local: 1.0.0
modules:
  review:
    path: modules/review.yaml
steps:
  - id: collect_external
    uses: process:gov.external/emit
    with:
      stdout: trusted-positive
      artifact_contents: positive-artifact
      evidence_contents: positive-evidence
      usage:
        provider: openai
        model: gpt-5.4
        input_tokens: 10
        total_tokens: 16

  - id: review_subflow
    module: review

  - id: collect_local
    uses: process:gov.local/emit
    with: {}
"#,
    )
    .expect("write positive workflow");
    workflow
}

pub fn write_local_only_workflow(repo_root: &Path) -> PathBuf {
    let workflow = repo_root.join("gov-local-only.workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_local_only
version: v1
plugins:
  gov.local: 1.0.0
steps:
  - id: local_step
    uses: process:gov.local/emit
    with: {}
"#,
    )
    .expect("write local-only workflow");
    workflow
}

pub fn write_v0_bypass_workflow(repo_root: &Path) -> PathBuf {
    let workflow = repo_root.join("gov-v0-bypass.workflow.yaml");
    fs::write(
        &workflow,
        r#"schema: transist.flow.v1
id: gov_demo_bypass
version: v0
plugins:
  gov.test: v0
steps:
  - id: collect_external
    uses: process:gov.test/emit
    with:
      stdout: bypassed
"#,
    )
    .expect("write v0 workflow");
    workflow
}

pub fn kernel_inputs() -> BTreeMap<String, KernelValue> {
    BTreeMap::new()
}

fn write_stub_plugin(
    dir: &Path,
    plugin_id: &str,
    plugin_version: &str,
    provider_kind: Option<&str>,
    emit_usage: bool,
) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input schema");
    fs::write(dir.join("schemas/output.json"), "{}").expect("output schema");
    let script = r#"#!/usr/bin/env python3
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
        "plugin": {"id": "__PLUGIN_ID__", "version": "__PLUGIN_VERSION__"},
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

payload = request.get("input") or {}
result = {
    "type": "app.invoke.result.v1",
    "protocol_version": APP_PROTOCOL,
    "invoke_id": request["invoke_id"],
    "status": "success",
    "output": {
        "stdout": payload.get("stdout", "stub-ok"),
        "stderr": "",
        "exit_code": 0,
    },
    "artifact_hints": [
        {"name": "stdout.txt", "contents": payload.get("artifact_contents", "artifact-ok")},
    ],
    "evidence_hints": [
        {"name": "trace.log", "contents": payload.get("evidence_contents", "evidence-ok")},
    ],
    "metrics": {},
    "usage": payload.get("usage"),
    "error": None,
}
emit(result)
"#
    .replace("__PLUGIN_ID__", plugin_id)
    .replace("__PLUGIN_VERSION__", plugin_version);
    fs::write(dir.join("plugin.py"), script).expect("plugin script");
    make_executable(&dir.join("plugin.py"));

    let provider_block = provider_kind
        .map(|kind| format!("default_provider_kind: {kind}\n"))
        .unwrap_or_default();
    let manifest = format!(
        "schema: transist.plugin.v1\nid: {plugin_id}\nversion: {plugin_version}\nadapter: process\n{provider_block}entrypoint:\n  type: process\n  command: plugin.py\npackage:\n  files: [plugin.py, schemas/input.json, schemas/output.json]\nactions:\n  - name: emit\n    input_schema: schemas/input.json\n    output_schema: schemas/output.json\n    secret_inputs: []\n    bindings: {{}}\ncapabilities:\n  supports_cancel: true\n  supports_health: false\n  supports_artifact_hints: true\n  supports_evidence_hints: true\n"
    );
    fs::write(dir.join("plugin.yaml"), manifest).expect("plugin manifest");

    let _ = emit_usage;
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
