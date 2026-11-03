// SPDX-License-Identifier: MPL-2.0
//! Real CLI witnesses for the read-only authoring boundary, independent of run storage.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    witness: PathBuf,
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let to = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let witness = root.join("adapter-startup-witness");
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../transist-compiler/tests/fixtures/explanation"),
            &root.join("source"),
        );
        fs::write(root.join("inputs.json"), json!({"repo": "/declared/repository", "host": "review.example", "token_ref": "TOKEN", "ticket": "ISSUE-42", "payload": {"items": [1, true, null]}}).to_string()).unwrap();
        Self {
            _temp: temp,
            root,
            home,
            witness,
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_transist"));
        command
            .current_dir(&self.root)
            .env("XDG_CONFIG_HOME", self.root.join("user-config"))
            .env("TRANSIST_HOME", &self.home)
            .env_remove("TRANSIST_CLI_TEST_PROMPT")
            .env("TRANSIST_UI", "plain");
        command
    }
    fn explain(&self, extra: &[&str]) -> Output {
        self.command()
            .args([
                "workflows",
                "explain",
                "--path",
                "source/workflow.yaml",
                "--inputs-file",
                "inputs.json",
                "--format",
                "json",
            ])
            .args(extra)
            .output()
            .unwrap()
    }
    fn install_fixture(&self, id: &str, adapter: &str, url: Option<String>) {
        // Test setup writes a package snapshot directly. The command under test
        // has no installation or initialized-home precondition.
        let package = self.home.join("plugins").join(id).join("0.1.0");
        fs::create_dir_all(&package).unwrap();
        let entrypoint = if let Some(url) = url {
            json!({"type": "http", "base_url": url})
        } else {
            json!({"type": adapter, "command": "witness.sh", "env": {"WITNESS": self.witness}})
        };
        let action = |name: &str| {
            json!({"name": name, "input_schema": "input.json", "output_schema": "output.json",
            "bindings": {"tool_name": "witness", "transist_prompt_v1": {"inline_field": "prompt", "file_field": "prompt_file"}, "transist_gate_decision_v1": {"decision_field": "gate_decision"}},
            "secret_inputs": [{"name": "TOKEN", "required": true, "binding": {"type": "env", "name": "API_TOKEN"}}],
            "prerequisites": {"binaries": [{"name": "does-not-exist-explanation-prerequisite"}]}})
        };
        let manifest = json!({"schema": "transist.plugin.v1", "id": id, "version": "0.1.0", "adapter": adapter,
            "default_provider_kind": "local", "entrypoint": entrypoint, "package": {"files": ["input.json", "output.json", "witness.sh"]},
            "actions": [action("produce"), action("review")],
            "capabilities": {"supports_cancel": true, "supports_health": true, "supports_artifact_hints": true, "supports_evidence_hints": true}});
        fs::write(
            package.join("plugin.yaml"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            package.join("input.json"),
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#,
        )
        .unwrap();
        fs::write(package.join("output.json"), r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"answer":{"type":"string"},"ready":{"type":"boolean"}}}"#).unwrap();
        fs::write(
            package.join("witness.sh"),
            "#!/bin/sh\nprintf 'STARTED\\n' >> \"$WITNESS\"\nexit 99\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                package.join("witness.sh"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }
}

fn parsed(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    kind: String,
    bytes: Vec<u8>,
    mtime: SystemTime,
    len: u64,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
}
fn inventory(root: &Path) -> BTreeMap<String, Entry> {
    fn walk(root: &Path, path: &Path, entries: &mut BTreeMap<String, Entry>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let kind = if metadata.is_dir() {
            "directory"
        } else if metadata.is_symlink() {
            "symlink"
        } else {
            "file"
        };
        let bytes = if metadata.is_file() {
            fs::read(path).unwrap()
        } else if metadata.is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        entries.insert(
            path.strip_prefix(root).unwrap().to_string_lossy().into(),
            Entry {
                kind: kind.into(),
                bytes,
                mtime: metadata.modified().unwrap(),
                len: metadata.len(),
                readonly: metadata.permissions().readonly(),
                #[cfg(unix)]
                mode: {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode()
                },
            },
        );
        if metadata.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                walk(root, &child.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    if root.exists() {
        walk(root, root, &mut entries);
    }
    entries
}

#[test]
fn missing_home_is_not_created_and_identical_json_is_byte_identical() {
    let f = Fixture::new();
    let before = inventory(&f.root);
    let first = f.explain(&[]);
    let view = parsed(&first);
    assert_eq!(first.stdout, f.explain(&[]).stdout);
    assert_eq!(
        view["pins"]["fixture@0.1.0"]["status"],
        "unresolved_missing"
    );
    assert!(!f.home.exists());
    assert!(!f.witness.exists());
    assert_eq!(before, inventory(&f.root));
    assert!(
        view["notice"]
            .as_str()
            .unwrap()
            .contains("not validation, authorization, readiness, or execution consent")
    );
}

#[test]
fn read_only_uninitialized_home_resolves_actual_contracts_with_zero_writes_or_startups() {
    let f = Fixture::new();
    f.install_fixture("fixture", "process", None);
    // Invalid config and secret bytes prove explain does not load policy/home
    // posture, secret availability, installation identity or secret values.
    fs::write(f.home.join("config.yaml"), "deliberately invalid config").unwrap();
    fs::create_dir_all(f.home.join("secrets")).unwrap();
    fs::write(
        f.home.join("secrets/TOKEN"),
        "NEVER_LOOK_UP_THIS_SECRET_VALUE",
    )
    .unwrap();
    #[cfg(unix)]
    set_read_only(&f.home, true);
    let before = inventory(&f.home);
    let output = f.explain(&[]);
    let view = parsed(&output);
    assert_eq!(view["pins"]["fixture@0.1.0"]["status"], "resolved");
    assert_eq!(
        view["pins"]["fixture@0.1.0"]["sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        view["steps"]["root/steps/consume"]["contract"]["action"]["name"],
        "review"
    );
    assert_eq!(
        view["steps"]["root/steps/consume"]["prompt"]["contract_binding"]["inline_field"],
        "prompt"
    );
    assert_eq!(
        view["steps"]["root/steps/consume"]["gate_decision"]["contract_binding"]["decision_field"],
        "gate_decision"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("NEVER_LOOK_UP_THIS_SECRET_VALUE"));
    assert_eq!(output.stdout, f.explain(&[]).stdout);
    assert_eq!(before, inventory(&f.home));
    assert!(!f.witness.exists());
    #[cfg(unix)]
    set_read_only(&f.home, false);
}

#[cfg(unix)]
fn set_read_only(path: &Path, read_only: bool) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        for child in fs::read_dir(path).unwrap() {
            set_read_only(&child.unwrap().path(), read_only);
        }
    }
    let executable = metadata.permissions().mode() & 0o111 != 0;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if read_only {
            if metadata.is_dir() || executable {
                0o555
            } else {
                0o444
            }
        } else {
            0o755
        }),
    )
    .unwrap();
}

#[test]
fn process_mcp_and_http_contract_reads_make_zero_adapter_calls() {
    let f = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    f.install_fixture("fixture", "process", None);
    f.install_fixture("witness.mcp", "mcp", None);
    f.install_fixture(
        "witness.http",
        "http",
        Some(format!("http://{}", listener.local_addr().unwrap())),
    );
    // Calibrate the external witness before the operation. These fixture
    // entrypoints write on startup, before reading any APP request.
    for id in ["fixture", "witness.mcp"] {
        let package = f.home.join("plugins").join(id).join("0.1.0");
        let output = Command::new(package.join("witness.sh"))
            .env("WITNESS", &f.witness)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(99));
        assert_eq!(fs::read_to_string(&f.witness).unwrap(), "STARTED\n");
        fs::remove_file(&f.witness).unwrap();
        let manifest_path = package.join("plugin.yaml");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        for action in manifest["actions"].as_array_mut().unwrap() {
            action["secret_inputs"] = json!([]);
            action["prerequisites"] = json!({});
        }
        fs::write(manifest_path, manifest.to_string()).unwrap();
    }
    fs::write(f.root.join("adapters.yaml"), "schema: transist.flow.v1\nid: adapters\nversion: 0.1.0\nplugins: {fixture: 0.1.0, witness.mcp: 0.1.0, witness.http: 0.1.0}\nsteps:\n  - {id: process, uses: 'process:fixture/review'}\n  - {id: mcp, uses: 'mcp:witness.mcp/review'}\n  - {id: http, uses: 'http:witness.http/review'}\n").unwrap();
    let before = inventory(&f.home);
    let output = f
        .command()
        .args([
            "workflows",
            "explain",
            "--path",
            "adapters.yaml",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let view = parsed(&output);
    for step in ["process", "mcp", "http"] {
        assert_eq!(
            view["steps"][format!("root/steps/{step}")]["contract_status"],
            "resolved"
        );
    }
    assert!(!f.witness.exists());
    assert_eq!(before, inventory(&f.home));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn same_version_contract_schema_changes_change_actual_pin_digest() {
    let f = Fixture::new();
    f.install_fixture("fixture", "process", None);
    let first = parsed(&f.explain(&[]));
    fs::write(
        f.home.join("plugins/fixture/0.1.0/output.json"),
        r#"{"type":"object","properties":{"answer":{"type":"number"}}}"#,
    )
    .unwrap();
    let second = parsed(&f.explain(&[]));
    assert_ne!(
        first["pins"]["fixture@0.1.0"]["sha256"],
        second["pins"]["fixture@0.1.0"]["sha256"]
    );
    // Explanation reports observed bytes; it makes no install-index/trust claim.
    assert!(!f.witness.exists());
}

#[test]
fn missing_action_is_unresolved_and_never_invented() {
    let f = Fixture::new();
    f.install_fixture("fixture", "process", None);
    let path = f.root.join("source/workflow.yaml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("process:fixture/produce", "process:fixture/invented"),
    )
    .unwrap();
    let view = parsed(&f.explain(&[]));
    assert_eq!(
        view["steps"]["root/steps/produce"]["contract_status"],
        "unresolved_action"
    );
    assert_eq!(view["steps"]["root/steps/produce"]["contract"], Value::Null);
}

#[test]
fn remote_schema_references_are_unresolved_without_a_download() {
    let f = Fixture::new();
    f.install_fixture("fixture", "process", None);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::write(
        f.home.join("plugins/fixture/0.1.0/input.json"),
        json!({"$ref": format!("http://{}/schema.json", listener.local_addr().unwrap())})
            .to_string(),
    )
    .unwrap();
    let before = inventory(&f.home);
    let view = parsed(&f.explain(&[]));
    assert_eq!(
        view["pins"]["fixture@0.1.0"]["status"],
        "unresolved_invalid"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(before, inventory(&f.home));
    assert!(!f.witness.exists());
}

#[test]
fn cli_compare_reports_the_current_and_previous_compiled_bindings() {
    let f = Fixture::new();
    copy_tree(&f.root.join("source"), &f.root.join("previous"));
    let path = f.root.join("source/workflow.yaml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("default: 2", "default: 3"),
    )
    .unwrap();
    let view = parsed(&f.explain(&["--compare-to", "previous/workflow.yaml"]));
    let changes = view["comparison"]["changes"].as_array().unwrap();
    assert!(changes.iter().any(|change| change["path"]
        == "/steps/root~1steps~1consume/bound_bindings/approval_count"
        && change["before"] == 2
        && change["after"] == 3));
    assert!(!f.home.exists());
}

#[test]
fn text_help_and_parser_errors_preserve_the_boundary() {
    let f = Fixture::new();
    let before = inventory(&f.root);
    let text = f
        .command()
        .args([
            "workflows",
            "explain",
            "--path",
            "source/workflow.yaml",
            "--format",
            "text",
        ])
        .output()
        .unwrap();
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    for label in [
        "Workflow: explain.fixture@0.1.0",
        "requested_not_authorized",
        "opaque_prompt",
        "unresolved_missing",
        "not_created",
    ] {
        assert!(text.contains(label), "{label}");
    }
    let help = f
        .command()
        .args(["workflows", "explain", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("execution consent"));
    for flags in [
        vec![],
        vec!["--live"],
        vec!["--path", "source/workflow.yaml", "--approve"],
        vec!["--path", "source/workflow.yaml", "--format", "yaml"],
    ] {
        let output = f
            .command()
            .args(["workflows", "explain"])
            .args(flags)
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert_eq!(before, inventory(&f.root));
}

#[test]
fn an_entirely_unresolved_home_and_definition_errors_never_initialize_paths() {
    let f = Fixture::new();
    let before = inventory(&f.root);
    let output = f
        .command()
        .env_remove("TRANSIST_HOME")
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .args([
            "workflows",
            "explain",
            "--path",
            "source/workflow.yaml",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    parsed(&output);
    assert_eq!(before, inventory(&f.root));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing-prompt", f.root.join("source/prompts/alias.md"))
            .unwrap();
        let path = f.root.join("source/workflow.yaml");
        fs::write(
            &path,
            fs::read_to_string(&path)
                .unwrap()
                .replace("prompts/review.md", "prompts/alias.md"),
        )
        .unwrap();
        let before = inventory(&f.root);
        assert!(!f.explain(&[]).status.success());
        assert_eq!(before, inventory(&f.root));
    }
    assert!(!f.home.exists());
    assert!(!f.witness.exists());
}

#[test]
fn actual_scalar_bindings_and_prompt_digests_or_binding_errors_preserve_the_read_only_boundary() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    f.install_fixture("fixture", "process", None);
    let path = f.root.join("binding-parity.yaml");
    let mut flow = json!({"schema": "transist.flow.v1", "id": "explanation.parity", "version": "0.1.0", "plugins": {"fixture": "0.1.0"},
        "inputs": {"n": {"type": "number", "default": 2}, "flag": {"type": "boolean", "default": false}, "payload": {"type": "object", "default": {"x": 1}}, "nil": {"type": "string"}},
        "steps": [{"id": "call", "uses": "process:fixture/review", "prompt": "{{inputs.n}}", "with": {"args": ["{{inputs.n}}", "{{inputs.flag}}"]}}]});
    for (raw, text) in [
        ("{{inputs.n}}", "2"),
        ("{{ inputs.flag }}", "false"),
        ("n={{inputs.n}}", "n=2"),
    ] {
        flow["steps"][0]["prompt"] = json!(raw);
        fs::write(&path, flow.to_string()).unwrap();
        let before = inventory(&f.home);
        let view = parsed(
            &f.command()
                .args([
                    "workflows",
                    "explain",
                    "--path",
                    "binding-parity.yaml",
                    "--format",
                    "json",
                ])
                .output()
                .unwrap(),
        );
        assert_eq!(
            view["steps"]["root/steps/call"]["bound_bindings"]["args"],
            json!(["2", "false"])
        );
        assert_eq!(
            view["steps"]["root/steps/call"]["prompt"]["bound_sha256"],
            format!("{:x}", Sha256::digest(text.as_bytes()))
        );
        assert_eq!(
            view["steps"]["root/steps/call"]["prompt"]["binding_state"],
            "birth_known"
        );
        assert_eq!(before, inventory(&f.home));
        assert!(!f.witness.exists());
    }
    for (prompt, bindings, error) in [
        (
            "valid",
            json!({"value": "{{ inputs.payload }}"}),
            "structured output can only be used as a whole-value reference",
        ),
        (
            "{{inputs.payload}}",
            json!({}),
            "structured output can only be used as a whole-value reference",
        ),
        ("{{inputs.nil}}", json!({}), "prompt cannot resolve to null"),
        (
            "valid",
            json!({"value": "nil={{inputs.nil}}"}),
            "is null and cannot be interpolated",
        ),
    ] {
        flow["steps"][0]["prompt"] = json!(prompt);
        flow["steps"][0]["with"] = bindings;
        fs::write(&path, flow.to_string()).unwrap();
        let before = inventory(&f.home);
        let output = f
            .command()
            .args([
                "workflows",
                "explain",
                "--path",
                "binding-parity.yaml",
                "--format",
                "json",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(before, inventory(&f.home));
        assert!(!f.witness.exists());
    }
}
