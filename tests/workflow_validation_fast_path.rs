// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};

use transist::{
    KernelValue, RuntimeOperatingMode, RuntimeServiceOverlay, RuntimeServiceOverlayInput,
    RuntimeServiceOverlayOutput, TransistContext, ValidationStatus, WorkflowEvaluationMode,
    WorkflowReadinessReport, WorkflowReadinessStatus, init_home_dir_with_mode,
    set_runtime_service_overlay,
};

struct AvailabilityOnly;

static REMOVABLE_SECRET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

impl transist_core::SecretProviderPort for AvailabilityOnly {
    fn get_secret(&self, _: &str) -> anyhow::Result<Option<String>> {
        panic!("workflow evaluation must never resolve secret values")
    }

    fn has_secret(&self, name: &str) -> anyhow::Result<bool> {
        Ok(name == "AVAILABLE"
            || (name == "REMOVABLE" && REMOVABLE_SECRET.load(std::sync::atomic::Ordering::SeqCst)))
    }
}

struct SecretOverlay;

impl RuntimeServiceOverlay for SecretOverlay {
    fn apply(
        &self,
        input: RuntimeServiceOverlayInput,
    ) -> anyhow::Result<RuntimeServiceOverlayOutput> {
        Ok(RuntimeServiceOverlayOutput {
            policy: input.policy,
            secrets: Arc::new(AvailabilityOnly),
        })
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    context: TransistContext,
}

impl Fixture {
    fn new() -> Self {
        static OVERLAY: Once = Once::new();
        OVERLAY.call_once(|| set_runtime_service_overlay(Arc::new(SecretOverlay)).unwrap());
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).unwrap();
        init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone).unwrap();
        fs::write(home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: permissive\n  remote_secret_delivery:\n    process: true\n    http: false\n    mcp: false\n").unwrap();
        let context = TransistContext::with_home(&workspace, &home);
        Self {
            _temp: temp,
            root,
            home,
            workspace,
            context,
        }
    }

    fn plugin(&self, adapter: &str, secret: bool) {
        let dir = self.root.join("plugin");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("input.json"), r#"{"type":"object","properties":{"message":{"type":"string","minLength":1},"secret_refs":{"type":"array","items":{"type":"string"}}},"required":["message"],"additionalProperties":false}"#).unwrap();
        fs::write(dir.join("output.json"), "{}").unwrap();
        fs::write(
            dir.join("probe.py"),
            "#!/usr/bin/env python3\nraise RuntimeError('live probe invoked')\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join("probe.py"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let entrypoint = match adapter {
            "http" => "  type: http\n  base_url: https://example.invalid\n",
            "mcp" => "  type: mcp\n  command: probe.py\n",
            _ => "  type: process\n  command: probe.py\n",
        };
        let secrets = if secret {
            match adapter {
                "http" => {
                    "\n      - name: AVAILABLE\n        required: false\n        binding: {type: header, name: X-Token}"
                }
                "mcp" => {
                    "\n      - name: AVAILABLE\n        required: false\n        binding: {type: env, name: TOKEN}\n      - name: MISSING\n        required: false\n        binding: {type: env, name: MISSING_TOKEN}\n      - name: REMOVABLE\n        required: false\n        binding: {type: env, name: REMOVABLE_TOKEN}"
                }
                _ => {
                    "\n      - name: AVAILABLE\n        required: false\n        binding: {type: env, name: TOKEN}\n      - name: MISSING\n        required: false\n        binding: {type: env, name: MISSING_TOKEN}"
                }
            }
        } else {
            " []"
        };
        let bindings = if adapter == "mcp" {
            "{tool_name: echo}"
        } else {
            "{}"
        };
        fs::write(dir.join("plugin.yaml"), format!("schema: transist.plugin.v1\nid: fast.plugin\nversion: 1.0.0\nadapter: {adapter}\ndefault_provider_kind: local\nentrypoint:\n{entrypoint}package:\n  files: [probe.py, input.json, output.json]\nactions:\n  - name: echo\n    input_schema: input.json\n    output_schema: output.json\n    secret_inputs:{secrets}\n    bindings: {bindings}\ncapabilities:\n  supports_cancel: false\n  supports_health: false\n  supports_artifact_hints: false\n  supports_evidence_hints: false\n")).unwrap();
        self.context
            .install_plugin_file(&dir.join("plugin.yaml"))
            .unwrap();
    }

    fn compile(&self, extra: &str, steps: &str) {
        let workflow = self.root.join("workflow.yaml");
        fs::write(&workflow, format!("schema: transist.flow.v1\nid: fast.demo\nversion: v1\nworkspace:\n  path: {}\n  access: read_only\n{extra}steps:\n{steps}", self.workspace.display())).unwrap();
        self.context.compile_flow_file(&workflow).unwrap();
    }

    fn validation(&self, inputs: BTreeMap<String, KernelValue>) -> WorkflowReadinessReport {
        let before = snapshot(&self.home);
        let (full, summary) = self
            .context
            .workflow_validation_report_with_summary("fast.demo@v1", inputs.clone(), false, false)
            .unwrap();
        assert_eq!(
            summary,
            self.context
                .workflow_validation_summary("fast.demo@v1", inputs.clone(), false, false)
                .unwrap()
        );
        assert_eq!(
            full,
            self.context
                .workflow_validation_report("fast.demo@v1", inputs, false, false)
                .unwrap()
        );
        assert_eq!(full.evaluation_mode, WorkflowEvaluationMode::Validation);
        assert_eq!(full.validation_status, summary.validation_status);
        assert_eq!(Some(full.status), summary.readiness_status);
        assert_eq!(full.is_validation_passed(), summary.is_validation_passed());
        assert!(!full.is_ready());
        assert!(!summary.is_ready());
        for finding in &summary.findings {
            let checks: Vec<_> = full
                .checks
                .iter()
                .filter(|check| {
                    check.code == finding.code
                        && check.status == finding.status
                        && check.message == finding.message
                        && check.remediation == finding.remediation
                })
                .collect();
            assert_eq!(checks.len(), finding.occurrences);
            for path in &finding.example_step_paths {
                assert!(
                    checks
                        .iter()
                        .any(|check| check.step_path.as_ref() == Some(path))
                );
            }
        }
        assert_eq!(
            snapshot(&self.home),
            before,
            "validation must leave home bytes unchanged"
        );
        full
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), Vec::new());
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

fn installed_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\n", "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: hello}\n  - id: again\n    uses: process:fast.plugin/echo\n    with: {message: second}\n");
    fixture
        .context
        .install_flow_file_with_trust(&fixture.root.join("workflow.yaml"), false)
        .unwrap();
    fixture
}

fn receipt_index(fixture: &Fixture) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(fixture.home.join("registry/workflow-install-index.json")).unwrap(),
    )
    .unwrap()
}

fn write_receipt_index(fixture: &Fixture, index: &serde_json::Value) {
    fs::write(
        fixture.home.join("registry/workflow-install-index.json"),
        serde_json::to_vec(index).unwrap(),
    )
    .unwrap();
}

#[test]
fn reuse_receipt_survives_context_and_idempotent_install_uninstall() {
    let fixture = installed_fixture();
    let index = receipt_index(&fixture);
    let receipt = &index["entries"]["fast.demo@v1"]["definition_validation"];
    assert_eq!(receipt["validator_revision"], 2);
    assert_eq!(receipt["recipe_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        receipt["plugin_package_digests"].as_object().unwrap().len(),
        1
    );
    assert_eq!(
        receipt["plugin_package_digests"]["fast.plugin@1.0.0"],
        transist_plugins::plugin_dir_sha256(&fixture.home.join("plugins/fast.plugin/1.0.0"))
            .unwrap()
    );
    let fresh = TransistContext::with_home(&fixture.workspace, &fixture.home);
    assert!(
        fresh
            .workflow_validation_report("fast.demo@v1", BTreeMap::new(), false, false)
            .unwrap()
            .is_validation_passed()
    );
    fixture
        .context
        .install_flow_file_with_trust(&fixture.root.join("workflow.yaml"), false)
        .unwrap();
    assert_eq!(receipt_index(&fixture), index);
    fixture
        .context
        .uninstall_workflow("fast.demo@v1", false)
        .unwrap();
    assert!(
        receipt_index(&fixture)["entries"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn reuse_legacy_fallback_is_read_only_and_reinstall_upgrades() {
    let fixture = installed_fixture();
    let mut index = receipt_index(&fixture);
    index["entries"]["fast.demo@v1"]
        .as_object_mut()
        .unwrap()
        .remove("definition_validation");
    write_receipt_index(&fixture, &index);
    assert!(fixture.validation(BTreeMap::new()).is_validation_passed());
    assert_eq!(receipt_index(&fixture), index);
    fixture
        .context
        .install_flow_file_with_trust(&fixture.root.join("workflow.yaml"), false)
        .unwrap();
    assert_eq!(
        receipt_index(&fixture)["entries"]["fast.demo@v1"]["definition_validation"]["validator_revision"],
        2
    );
}

#[test]
fn reuse_older_revision_fallback_is_read_only() {
    let fixture = installed_fixture();
    let mut index = receipt_index(&fixture);
    index["entries"]["fast.demo@v1"]["definition_validation"]["validator_revision"] = 0.into();
    write_receipt_index(&fixture, &index);
    assert!(fixture.validation(BTreeMap::new()).is_validation_passed());
    assert_eq!(receipt_index(&fixture), index);
}

#[test]
fn reuse_recipe_package_and_malformed_receipts_fail_closed() {
    for mutation in [
        "recipe", "identity", "package", "schema", "receipt", "type", "pins",
    ] {
        let fixture = installed_fixture();
        match mutation {
            "identity" => {
                let path = fixture.home.join("registry/recipes/fast.demo-v1.yaml");
                fs::write(
                    &path,
                    fs::read_to_string(&path)
                        .unwrap()
                        .replace("fast.demo", "altered.demo"),
                )
                .unwrap();
            }
            "recipe" => {
                let path = fixture.home.join("registry/recipes/fast.demo-v1.yaml");
                let mut bytes = fs::read(&path).unwrap();
                bytes.extend_from_slice(b"\n# tamper\n");
                fs::write(path, bytes).unwrap();
            }
            "package" => fs::write(
                fixture.home.join("plugins/fast.plugin/1.0.0/probe.py"),
                "tamper",
            )
            .unwrap(),
            "schema" => fs::write(
                fixture.home.join("plugins/fast.plugin/1.0.0/input.json"),
                "{}",
            )
            .unwrap(),
            _ => {
                let mut index = receipt_index(&fixture);
                let receipt = &mut index["entries"]["fast.demo@v1"]["definition_validation"];
                match mutation {
                    "type" => *receipt = serde_json::json!({"validator_revision": "bad"}),
                    "pins" => receipt["plugin_package_digests"] = serde_json::json!({}),
                    _ => receipt["recipe_sha256"] = "bad".into(),
                };
                write_receipt_index(&fixture, &index);
            }
        }
        let before = snapshot(&fixture.home);
        let report = fixture
            .context
            .workflow_validation_report("fast.demo@v1", BTreeMap::new(), false, false)
            .unwrap();
        assert_eq!(
            report.status,
            WorkflowReadinessStatus::InvalidDefinition,
            "{mutation}: {report:#?}"
        );
        assert_eq!(before, snapshot(&fixture.home));
    }
}

#[test]
fn reuse_current_inputs_and_source_assets_are_checked_every_time() {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\ninputs:\n  asset: {type: string, required: true}\n", "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: '{{inputs.asset}}'}\n");
    fs::write(fixture.root.join("present.txt"), "asset").unwrap();
    fixture
        .context
        .install_flow_file_with_trust(&fixture.root.join("workflow.yaml"), false)
        .unwrap();
    let inputs =
        |value: &str| BTreeMap::from([("asset".into(), KernelValue::String(value.into()))]);
    assert!(
        fixture
            .validation(inputs("source://present.txt"))
            .is_validation_passed()
    );
    let missing = fixture.validation(inputs("source://missing.txt"));
    let invalid = fixture.validation(inputs(""));
    assert!(
        invalid
            .checks
            .iter()
            .any(|c| c.code == "input_schema" && c.status == ValidationStatus::Failed)
    );
    assert!(
        missing
            .checks
            .iter()
            .any(|c| c.code == "source_asset" && c.status == ValidationStatus::Failed)
    );
    fs::remove_file(fixture.root.join("present.txt")).unwrap();
    assert!(
        !fixture
            .validation(inputs("source://present.txt"))
            .is_validation_passed()
    );
    assert!(
        !fixture
            .validation(BTreeMap::from([(
                "asset".into(),
                KernelValue::Number(42.into())
            )]))
            .is_validation_passed()
    );
}

#[test]
fn reuse_v0_bypasses_receipts_and_revalidates_changed_contracts() {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile(
        "plugins:\n  fast.plugin: 1.0.0\n",
        "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: hello}\n",
    );
    let flow = fixture.root.join("workflow.yaml");
    fs::write(
        &flow,
        fs::read_to_string(&flow)
            .unwrap()
            .replace("version: v1", "version: v0"),
    )
    .unwrap();
    let installation = fixture.context.install_flow_file_with_trust(&flow, false);
    if !cfg!(feature = "dev-version-bypass") {
        assert!(
            installation
                .unwrap_err()
                .to_string()
                .contains("reserved test version")
        );
        assert!(
            !fixture
                .home
                .join("registry/workflow-install-index.json")
                .exists()
        );
        return;
    }
    installation.unwrap();
    assert!(receipt_index(&fixture)["entries"]["fast.demo@v0"]["definition_validation"].is_null());
    let report = fixture
        .context
        .workflow_validation_report("fast.demo@v0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(report.is_validation_passed(), "{report:#?}");
    fs::write(
        fixture.home.join("plugins/fast.plugin/1.0.0/input.json"),
        r#"{"type":"integer"}"#,
    )
    .unwrap();
    let report = fixture
        .context
        .workflow_validation_report("fast.demo@v0", BTreeMap::new(), false, false)
        .unwrap();
    assert!(!report.is_validation_passed());
}

#[test]
fn reuse_failed_install_leaves_no_partial_receipt() {
    let fixture = Fixture::new();
    let flow = fixture.root.join("workflow.yaml");
    fs::write(&flow, "schema: transist.flow.v1\nid: failed.receipt\nversion: v1\nsteps:\n  - id: finish\n    control: control.finish\n").unwrap();
    transist_storage_local::fault_injection::set(
        transist_storage_local::fault_injection::FAULT_WORKFLOW_INSTALL_AFTER_RECIPE,
    );
    let result = fixture.context.install_flow_file_with_trust(&flow, false);
    transist_storage_local::fault_injection::clear();
    assert!(result.is_err());
    assert!(
        !fixture
            .home
            .join("registry/workflow-install-index.json")
            .exists()
    );
    assert!(
        !fixture
            .home
            .join("registry/recipes/failed.receipt-v1.yaml")
            .exists()
    );
}

#[test]
fn reuse_upgrade_and_new_receipts_follow_ownership_rollback() {
    for legacy in [false, true] {
        let fixture = installed_fixture();
        let mut index = receipt_index(&fixture);
        if legacy {
            index["entries"]["fast.demo@v1"]
                .as_object_mut()
                .unwrap()
                .remove("definition_validation");
            write_receipt_index(&fixture, &index);
        } else {
            fixture
                .context
                .uninstall_workflow("fast.demo@v1", false)
                .unwrap();
        }
        let package =
            transist::capture_workflow_package(&fixture.root.join("workflow.yaml")).unwrap();
        let lock = transist::lock_workflow_installation(&fixture.home).unwrap();
        let mut ownership = transist::WorkflowInstallOwnershipJournal::new();
        fixture
            .context
            .install_workflow_package_snapshot_with_trust_and_ownership(
                &package,
                false,
                &lock,
                &mut ownership,
            )
            .unwrap();
        assert_eq!(
            receipt_index(&fixture)["entries"]["fast.demo@v1"]["definition_validation"]["validator_revision"],
            2
        );
        ownership.rollback(&fixture.home).unwrap();
        ownership.rollback(&fixture.home).unwrap();
        if legacy {
            assert_eq!(receipt_index(&fixture), index);
        } else {
            assert!(
                receipt_index(&fixture)["entries"]
                    .as_object()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                !fixture
                    .home
                    .join("registry/recipes/fast.demo-v1.yaml")
                    .exists()
            );
        }
    }
}

#[test]
fn reuse_prompt_assets_are_fresh_after_installation() {
    let fixture = Fixture::new();
    let plugin =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/goose.cli/plugin.yaml");
    fixture.context.install_plugin_file(&plugin).unwrap();
    fs::write(fixture.root.join("prompt.md"), "Fresh prompt").unwrap();
    fixture.compile(
        "plugins:\n  goose.cli: 0.1.0\n",
        "  - id: task\n    uses: process:goose.cli/run_task\n    prompt_file: prompt.md\n",
    );
    fixture
        .context
        .install_flow_file_with_trust(&fixture.root.join("workflow.yaml"), false)
        .unwrap();
    assert!(fixture.validation(BTreeMap::new()).is_validation_passed());
    fs::remove_file(fixture.root.join("prompt.md")).unwrap();
    assert!(!fixture.validation(BTreeMap::new()).is_validation_passed());
}

#[test]
fn fast_validation_omits_probes_but_live_readiness_still_probes() {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile(
        "plugins:\n  fast.plugin: 1.0.0\n",
        "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: hello}\n",
    );
    let report = fixture.validation(BTreeMap::new());
    assert!(report.is_validation_passed(), "{report:#?}");
    assert_eq!(report.status, WorkflowReadinessStatus::NotReady);
    assert!(
        !report
            .checks
            .iter()
            .any(|check| check.code == "external_state")
    );
    let live = fixture
        .context
        .workflow_readiness_report("fast.demo@v1", BTreeMap::new(), false, false)
        .unwrap();
    assert_eq!(live.evaluation_mode, WorkflowEvaluationMode::LiveReadiness);
    assert!(
        !live.is_ready(),
        "the deliberately broken plugin must fail a live probe"
    );
    assert!(
        live.checks
            .iter()
            .any(|check| check.status == ValidationStatus::Failed
                && check.code == "adapter_handshake"),
        "{live:#?}"
    );
    assert_eq!(
        fixture
            .context
            .workflow_readiness_summary("fast.demo@v1", BTreeMap::new(), false, false)
            .unwrap()
            .validation_status,
        live.validation_status
    );
    assert!(fixture.context.runs_list().unwrap().is_empty());
}

#[test]
fn fast_secret_availability_without_value_resolution() {
    let fixture = Fixture::new();
    fixture.plugin("process", true);
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\ninputs:\n  secret:\n    type: string\n    required: true\n", "  - id: echo\n    uses: process:fast.plugin/echo\n    with:\n      message: hello\n      secret_refs: ['{{inputs.secret}}']\n");
    let available = fixture.validation(BTreeMap::from([(
        "secret".into(),
        KernelValue::String("AVAILABLE".into()),
    )]));
    assert!(available.is_validation_passed(), "{available:#?}");
    let missing = fixture.validation(BTreeMap::from([(
        "secret".into(),
        KernelValue::String("MISSING".into()),
    )]));
    assert_eq!(missing.status, WorkflowReadinessStatus::NotDeliverable);
    assert!(
        missing
            .checks
            .iter()
            .any(|check| check.code == "secret_resolution"
                && check.step_path.as_deref() == Some("echo")
                && check.status == ValidationStatus::Failed)
    );
}

#[test]
fn fast_denied_secret_delivery_blocks_http_and_mcp_without_probes() {
    for adapter in ["http", "mcp"] {
        let fixture = Fixture::new();
        fixture.plugin(adapter, true);
        fixture.compile("plugins:\n  fast.plugin: 1.0.0\n", &format!("  - id: echo\n    uses: {adapter}:fast.plugin/echo\n    with: {{message: hello, secret_refs: [AVAILABLE]}}\n"));
        let report = fixture.validation(BTreeMap::new());
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.code == "remote_secret_policy"
                    && check.status == ValidationStatus::Failed
                    && check.step_path.as_deref() == Some("echo")),
            "{report:#?}"
        );
    }
}

#[test]
fn fast_invalid_inputs_and_action_schema_remain_blocking() {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\ninputs:\n  message:\n    type: string\n    required: true\n", "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: '{{inputs.message}}'}\n");
    for inputs in [
        BTreeMap::new(),
        BTreeMap::from([("message".into(), KernelValue::Bool(true))]),
    ] {
        let report = fixture.validation(inputs);
        assert_eq!(report.status, WorkflowReadinessStatus::InvalidDefinition);
        assert_eq!(report.validation_status, ValidationStatus::Failed);
    }
    let schema = Fixture::new();
    schema.plugin("process", false);
    schema.compile(
        "plugins:\n  fast.plugin: 1.0.0\n",
        "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: 123}\n",
    );
    let report = schema.validation(BTreeMap::new());
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "input_schema" && check.status == ValidationStatus::Failed),
        "{report:#?}"
    );
}

#[test]
fn fast_prerequisites_authority_and_current_trust_remain_blocking() {
    for required in [true, false] {
        let fixture = Fixture::new();
        fixture.compile(&format!("prerequisites:\n  paths:\n    - path: absent\n      kind: file\n      required: {required}\n"), "  - id: finish\n    control: control.finish\n");
        let report = fixture.validation(BTreeMap::new());
        assert_eq!(
            report.validation_status,
            if required {
                ValidationStatus::Failed
            } else {
                ValidationStatus::Warning
            }
        );
        assert!(!report.is_validation_passed());
    }
    let fixture = Fixture::new();
    fixture.compile(
        "",
        "  - id: review\n    control: control.gate.manual\n    with: {gate_id: review}\n",
    );
    assert!(fixture.validation(BTreeMap::new()).is_validation_passed());
    fs::write(
        fixture.home.join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n",
    )
    .unwrap();
    let denied = fixture.validation(BTreeMap::new());
    assert_eq!(denied.validation_status, ValidationStatus::Failed);
    assert!(!denied.validation.unwrap().missing_capabilities.is_empty());
    fs::write(
        fixture.home.join("config.yaml"),
        "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n",
    )
    .unwrap();
    assert_eq!(
        fixture.validation(BTreeMap::new()).status,
        WorkflowReadinessStatus::InvalidDefinition
    );
}

#[test]
fn fast_root_child_and_cleanup_findings_keep_scope() {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fs::write(fixture.root.join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: v1\nsteps:\n  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: 123}\n").unwrap();
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\nmodules:\n  child: {path: child.yaml}\ncleanup:\n  - id: cleanup_echo\n    when: [success, failed, canceled]\n    uses: process:fast.plugin/echo\n    with: {message: 123}\n", "  - id: root_echo\n    uses: process:fast.plugin/echo\n    with: {message: 123}\n  - id: nested\n    module: child\n");
    let report = fixture.validation(BTreeMap::new());
    let paths: Vec<_> = report
        .checks
        .iter()
        .filter(|check| check.code == "input_schema" && check.status == ValidationStatus::Failed)
        .filter_map(|check| check.step_path.as_deref())
        .collect();
    assert_eq!(paths, ["root_echo", "nested/echo", "cleanup_echo"]);
}

#[test]
fn fast_runtime_dependent_validation_is_explicitly_deferred() {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\n", "  - id: produce\n    uses: process:fast.plugin/echo\n    with: {message: hello}\n    set:\n      dynamic: '{{steps.produce.out.message}}'\n  - id: consume\n    uses: process:fast.plugin/echo\n    with: {message: '{{vars.dynamic}}'}\n");
    let report = fixture.validation(BTreeMap::new());
    assert_eq!(report.validation_status, ValidationStatus::Passed);
    assert!(report.is_validation_passed());
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.status == ValidationStatus::DeferredToDispatch)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "mutable_variable"
                && check.step_path.as_deref() == Some("consume"))
    );
}

#[test]
fn revised_authoring_all_adapters_defer_data_but_check_secret_delivery() {
    for adapter in ["process", "http", "mcp"] {
        for secret in [false, true] {
            let fixture = Fixture::new();
            fixture.plugin(adapter, secret);
            let secret_refs = if secret {
                ", secret_refs: [AVAILABLE]"
            } else {
                ""
            };
            fixture.compile("plugins:\n  fast.plugin: 1.0.0\n", &format!("  - id: produce\n    uses: {adapter}:fast.plugin/echo\n    with: {{message: hello}}\n  - id: consume\n    uses: {adapter}:fast.plugin/echo\n    with: {{message: '{{{{steps.produce.out.message}}}}'{secret_refs}}}\n"));
            let report = fixture.validation(BTreeMap::new());
            if secret && adapter != "process" {
                assert!(
                    report
                        .checks
                        .iter()
                        .any(|c| c.code == "remote_secret_policy"
                            && c.status == ValidationStatus::Failed
                            && c.step_path.as_deref() == Some("consume")),
                    "{report:#?}"
                );
            } else {
                assert!(report.is_validation_passed(), "{report:#?}");
                assert!(
                    report
                        .checks
                        .iter()
                        .any(|c| c.status == ValidationStatus::DeferredToDispatch
                            && c.step_path.as_deref() == Some("consume"))
                );
            }
        }
    }
}

#[test]
fn fast_all_adapter_families_validate_without_startup_or_connections() {
    for adapter in ["process", "http", "mcp"] {
        let fixture = Fixture::new();
        fixture.plugin(adapter, false);
        fixture.compile(
            "plugins:\n  fast.plugin: 1.0.0\n",
            &format!(
                "  - id: echo\n    uses: {adapter}:fast.plugin/echo\n    with: {{message: hello}}\n"
            ),
        );
        let report = fixture.validation(BTreeMap::new());
        assert!(report.is_validation_passed(), "{report:#?}");
        assert!(!report.checks.iter().any(|check| matches!(
            check.code.as_str(),
            "adapter_probe" | "adapter_handshake" | "adapter_health"
        )));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["evaluation_mode"], "validation");
        assert_eq!(json["validation_status"], "passed");
        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["run_created"], false);
    }
}

#[test]
fn fast_child_prerequisite_diagnostics_preserve_parent_scope() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("child.yaml"), "schema: transist.module.v1\nid: child\nversion: v1\nprerequisites:\n  paths:\n    - path: absent\n      kind: file\n      required: true\nsteps:\n  - id: finish\n    control: control.finish\n").unwrap();
    fixture.compile(
        "modules:\n  child: {path: child.yaml}\n",
        "  - id: nested\n    module: child\n",
    );
    let report = fixture.validation(BTreeMap::new());
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "workflow_prerequisite.path"
                && check.status == ValidationStatus::Failed
                && check.step_path.as_deref() == Some("nested")),
        "{report:#?}"
    );
}

#[test]
fn authorization_static_surface_and_fast_authorization_have_distinct_scope() {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile(
        "plugins:\n  fast.plugin: 1.0.0\n",
        "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: hello}\n",
    );
    let legacy = fixture
        .context
        .validate_bound_workflow_summary("fast.demo@v1", BTreeMap::new(), false, false)
        .unwrap();
    assert_eq!(legacy.status, ValidationStatus::NotEvaluated);
    assert!(legacy.readiness_status.is_none());
    assert!(
        legacy
            .findings
            .iter()
            .any(|finding| finding.code == "external_state")
    );
    let authorization = fixture
        .context
        .authorize_workflow("fast.demo@v1", BTreeMap::new(), None, false)
        .unwrap();
    assert_eq!(
        authorization.readiness.evaluation_mode,
        WorkflowEvaluationMode::Validation
    );
    assert_eq!(
        authorization.status,
        transist::WorkflowAuthorizationStatus::AlreadyAuthorized
    );
    let compact = fixture
        .context
        .authorize_workflow_summary("fast.demo@v1", BTreeMap::new(), None, false)
        .unwrap();
    assert_eq!(
        compact.readiness.evaluation_mode,
        WorkflowEvaluationMode::Validation
    );
    assert_eq!(compact.status, authorization.status);
    assert!(fixture.context.runs_list().unwrap().is_empty());
}

fn authorization_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.plugin("process", false);
    let workflow = fixture.root.join("workflow.yaml");
    fs::write(&workflow, "schema: transist.flow.v1\nid: fast.demo\nversion: v1\nnetwork: {mode: none}\nplugins:\n  fast.plugin: 1.0.0\ninputs:\n  repo_path: {type: string, required: true}\nworkspace:\n  path: '{{inputs.repo_path}}'\n  access: read_only\nsteps:\n  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: hello}\n").unwrap();
    fixture
        .context
        .install_flow_file_with_trust(&workflow, false)
        .unwrap();
    fs::create_dir_all(fixture.home.join("policy/workflow-authorizations")).unwrap();
    fs::write(fixture.home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n").unwrap();
    fixture
}

fn workspace_inputs(path: &Path) -> BTreeMap<String, KernelValue> {
    BTreeMap::from([(
        "repo_path".into(),
        KernelValue::String(path.display().to_string()),
    )])
}

fn assert_repeat_work(counts: &BTreeMap<transist_plugins::ValidationWork, usize>, inputs: usize) {
    use transist_plugins::ValidationWork as Work;
    for work in [Work::DefinitionValidation, Work::ContractValidation] {
        assert_eq!(
            counts.get(&work).copied().unwrap_or_default(),
            0,
            "{counts:?}"
        );
    }
    for work in [
        Work::PackageLoad,
        Work::PackageHash,
        Work::SchemaCompilation,
        Work::InputBinding,
        Work::PolicyEvaluation,
    ] {
        assert_eq!(counts.get(&work), Some(&1), "{work:?}: {counts:?}");
    }
    assert_eq!(
        counts.get(&Work::InputValidation),
        Some(&inputs),
        "{counts:?}"
    );
}

#[test]
fn repeat_matching_receipts_all_adapters_have_zero_probes_and_bounded_work() {
    for adapter in ["process", "mcp", "http"] {
        let fixture = Fixture::new();
        fixture.plugin(adapter, false);
        fixture.compile("plugins:\n  fast.plugin: 1.0.0\n", &format!("  - id: first\n    uses: {adapter}:fast.plugin/echo\n    with: {{message: first}}\n  - id: second\n    uses: {adapter}:fast.plugin/echo\n    with: {{message: second}}\n"));
        fixture
            .context
            .install_flow_file_with_trust(&fixture.root.join("workflow.yaml"), false)
            .unwrap();
        assert_eq!(
            receipt_index(&fixture)["entries"]["fast.demo@v1"]["definition_validation"]["validator_revision"],
            2
        );
        let before = snapshot(&fixture.home);
        for _ in 0..10 {
            let fresh = TransistContext::with_home(&fixture.workspace, &fixture.home);
            let ((report, probes), counts) = transist_plugins::capture_validation_work(|| {
                transist::capture_preflight_measurements(|| {
                    fresh
                        .workflow_validation_report("fast.demo@v1", BTreeMap::new(), false, false)
                        .unwrap()
                })
            });
            assert!(report.is_validation_passed(), "{report:#?}");
            assert_repeat_work(&counts, 2);
            assert!(
                !probes.iter().any(|record| matches!(
                    record,
                    transist::PreflightMeasurement::ProbeBatch { .. }
                )),
                "{probes:?}"
            );
            let ((authorization, probes), counts) =
                transist_plugins::capture_validation_work(|| {
                    transist::capture_preflight_measurements(|| {
                        fresh
                            .authorize_workflow_summary(
                                "fast.demo@v1",
                                BTreeMap::new(),
                                None,
                                false,
                            )
                            .unwrap()
                    })
                });
            assert_eq!(
                authorization.status,
                transist::WorkflowAuthorizationStatus::AlreadyAuthorized
            );
            assert_repeat_work(&counts, 2);
            assert!(
                !probes.iter().any(|record| matches!(
                    record,
                    transist::PreflightMeasurement::ProbeBatch { .. }
                ))
            );
        }
        assert_eq!(snapshot(&fixture.home), before);
        assert!(fixture.context.runs_list().unwrap().is_empty());
    }
}

#[test]
fn repeat_work_observer_is_opt_in_thread_local_and_unwind_safe() {
    use transist_plugins::{
        ValidationWork as Work, capture_validation_work, record_validation_work,
    };
    record_validation_work(Work::PackageHash);
    let (_, counts) = capture_validation_work(|| {
        std::thread::spawn(|| record_validation_work(Work::PackageHash))
            .join()
            .unwrap();
        record_validation_work(Work::InputValidation);
    });
    assert_eq!(counts, BTreeMap::from([(Work::InputValidation, 1)]));
    assert!(
        std::panic::catch_unwind(|| capture_validation_work(|| panic!("capture unwind"))).is_err()
    );
    assert!(capture_validation_work(|| ()).1.is_empty());
    assert!(
        std::panic::catch_unwind(|| capture_validation_work(|| capture_validation_work(|| ())))
            .is_err()
    );
    assert!(capture_validation_work(|| ()).1.is_empty());
}

#[test]
fn repeat_counters_observe_real_fallback_and_definition_validation() {
    use transist_plugins::ValidationWork as Work;
    let fixture = installed_fixture();
    let (result, counts) = transist_plugins::capture_validation_work(|| {
        fixture
            .context
            .validate_flow_file(&fixture.root.join("workflow.yaml"))
    });
    result.unwrap();
    assert!(counts[&Work::DefinitionValidation] > 0);
    assert!(counts[&Work::ContractValidation] > 0);
    let mut index = receipt_index(&fixture);
    index["entries"]["fast.demo@v1"]
        .as_object_mut()
        .unwrap()
        .remove("definition_validation");
    write_receipt_index(&fixture, &index);
    let (report, counts) = transist_plugins::capture_validation_work(|| {
        fixture
            .context
            .workflow_validation_report("fast.demo@v1", BTreeMap::new(), false, false)
            .unwrap()
    });
    assert!(report.is_validation_passed());
    assert_eq!(counts[&Work::ContractValidation], 1);
    assert_eq!(counts[&Work::PackageLoad], 1);
    assert_eq!(counts[&Work::PackageHash], 1);
    assert_eq!(
        counts[&Work::SchemaCompilation],
        2,
        "input and output roots"
    );
    assert_eq!(receipt_index(&fixture), index);
}

#[test]
fn repeat_inputs_policy_and_live_non_probe_blockers_remain_fresh() {
    let fixture = authorization_fixture();
    let inputs = workspace_inputs(&fixture.workspace);
    fixture
        .context
        .authorize_workflow("fast.demo@v1", inputs.clone(), None, true)
        .unwrap();
    let other = fixture.root.join("other");
    fs::create_dir(&other).unwrap();
    for inputs in [inputs.clone(), workspace_inputs(&other)] {
        let fast = fixture
            .context
            .workflow_validation_report("fast.demo@v1", inputs.clone(), false, false)
            .unwrap();
        let live = fixture
            .context
            .workflow_readiness_report("fast.demo@v1", inputs, false, false)
            .unwrap();
        assert_eq!(
            fast.validation.as_ref().unwrap().missing_capabilities,
            live.validation.as_ref().unwrap().missing_capabilities
        );
        assert_eq!(
            fast.validation.as_ref().unwrap().policy_suggestion,
            live.validation.as_ref().unwrap().policy_suggestion
        );
        let blockers = |report: &WorkflowReadinessReport| {
            report
                .checks
                .iter()
                .filter(|c| {
                    c.status != ValidationStatus::Passed
                        && !matches!(
                            c.code.as_str(),
                            "adapter_handshake" | "adapter_health" | "adapter_probe"
                        )
                })
                .map(|c| {
                    (
                        c.code.clone(),
                        c.status,
                        c.step_path.clone(),
                        c.message.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(blockers(&fast), blockers(&live));
    }
    fs::write(
        fixture.home.join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n",
    )
    .unwrap();
    let revoked = fixture
        .context
        .workflow_validation_report("fast.demo@v1", inputs, false, false)
        .unwrap();
    assert!(!revoked.is_validation_passed());
    assert!(!revoked.validation.unwrap().missing_capabilities.is_empty());

    let fixture = Fixture::new();
    fixture.plugin("process", false);
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\ninputs:\n  message: {type: string, required: true}\n", "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: '{{inputs.message}}'}\n");
    fixture
        .context
        .install_flow_file_with_trust(&fixture.root.join("workflow.yaml"), false)
        .unwrap();
    for value in ["valid", "", "valid again"] {
        let inputs = BTreeMap::from([("message".into(), KernelValue::String(value.into()))]);
        let (fast, counts) = transist_plugins::capture_validation_work(|| {
            fixture
                .context
                .workflow_validation_report("fast.demo@v1", inputs.clone(), false, false)
                .unwrap()
        });
        assert_repeat_work(&counts, 1);
        assert_eq!(fast.is_validation_passed(), !value.is_empty());
        let live = fixture
            .context
            .workflow_readiness_report("fast.demo@v1", inputs, false, false)
            .unwrap();
        let schema = |report: &WorkflowReadinessReport| {
            report
                .checks
                .iter()
                .filter(|c| c.code == "input_schema")
                .map(|c| (c.status, c.step_path.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(schema(&fast), schema(&live));
    }
    assert!(fixture.context.runs_list().unwrap().is_empty());
}

#[test]
fn authorization_already_authorized_is_probe_free_read_only_and_policy_stays_current() {
    use transist::WorkflowAuthorizationStatus as Status;
    let fixture = authorization_fixture();
    let inputs = workspace_inputs(&fixture.workspace);
    let full = fixture
        .context
        .authorize_workflow("fast.demo@v1", inputs.clone(), None, false)
        .unwrap();
    let compact = fixture
        .context
        .authorize_workflow_summary("fast.demo@v1", inputs.clone(), None, false)
        .unwrap();
    assert_eq!(full.status, Status::Planned);
    assert_eq!(full.policy, compact.policy);
    assert_eq!(full.policy_output, compact.policy_output);
    assert_eq!(
        full.readiness.evaluation_mode,
        WorkflowEvaluationMode::Validation
    );
    assert!(
        !full
            .readiness
            .checks
            .iter()
            .any(|c| c.code.starts_with("adapter_handshake") || c.code == "adapter_health")
    );
    let applied = fixture
        .context
        .apply_reviewed_workflow_authorization(inputs.clone(), &full)
        .unwrap();
    assert_eq!(applied.status, Status::Applied);
    let before = snapshot(&fixture.home);
    for apply in [false, true] {
        let full = fixture
            .context
            .authorize_workflow("fast.demo@v1", inputs.clone(), None, apply)
            .unwrap();
        let compact = fixture
            .context
            .authorize_workflow_summary("fast.demo@v1", inputs.clone(), None, apply)
            .unwrap();
        assert_eq!(full.status, Status::AlreadyAuthorized, "{full:#?}");
        assert_eq!(full.status, compact.status);
        assert!(full.readiness.is_validation_passed());
        assert!(compact.readiness.is_validation_passed());
        assert!(!full.readiness.is_ready());
        assert_eq!(snapshot(&fixture.home), before);
    }
    let other = fixture.root.join("other-repository");
    fs::create_dir(&other).unwrap();
    let other_inputs = workspace_inputs(&other);
    let plan = fixture
        .context
        .authorize_workflow("fast.demo@v1", other_inputs.clone(), None, false)
        .unwrap();
    assert_eq!(plan.status, Status::Planned);
    let missing = &plan
        .readiness
        .validation
        .as_ref()
        .unwrap()
        .missing_capabilities;
    assert!(
        missing
            .iter()
            .any(|c| c.resource == format!("workspace://{}", other.display()))
    );
    assert!(
        !missing
            .iter()
            .any(|c| c.resource == format!("workspace://{}", fixture.root.display()))
    );
    fixture
        .context
        .apply_reviewed_workflow_authorization(other_inputs.clone(), &plan)
        .unwrap();
    assert_eq!(
        fixture
            .context
            .authorize_workflow("fast.demo@v1", other_inputs, None, false)
            .unwrap()
            .status,
        Status::AlreadyAuthorized
    );
    fs::remove_file(applied.policy_output.unwrap()).unwrap();
    assert_eq!(
        fixture
            .context
            .authorize_workflow_summary("fast.demo@v1", inputs.clone(), None, false)
            .unwrap()
            .status,
        Status::Planned
    );
    fs::write(fixture.home.join("config.yaml"), "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n").unwrap();
    assert_eq!(
        fixture
            .context
            .authorize_workflow("fast.demo@v1", inputs, None, false)
            .unwrap()
            .status,
        Status::Blocked
    );
    assert!(fixture.context.runs_list().unwrap().is_empty());
}

#[test]
fn authorization_reviewed_fragment_rejects_changed_inputs_resources_policy_without_writes() {
    for change in ["inputs", "resource", "policy", "trust", "invalid"] {
        let fixture = authorization_fixture();
        let mut inputs = workspace_inputs(&fixture.workspace);
        let full = fixture
            .context
            .authorize_workflow("fast.demo@v1", inputs.clone(), None, false)
            .unwrap();
        let compact = fixture
            .context
            .authorize_workflow_summary("fast.demo@v1", inputs.clone(), None, false)
            .unwrap();
        match change {
            "inputs" => {
                let other = fixture.root.join("other");
                fs::create_dir(&other).unwrap();
                inputs = workspace_inputs(&other);
            }
            "resource" => {
                fs::remove_dir(&fixture.workspace).unwrap();
                let other = fixture.root.join("other");
                fs::create_dir(&other).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(other, &fixture.workspace).unwrap();
            }
            "policy" => {
                fixture.context.authorize_workflow("fast.demo@v1", inputs.clone(), None, true).unwrap();
            }
            "trust" => fs::write(fixture.home.join("config.yaml"), "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n").unwrap(),
            "invalid" => inputs.clear(),
            _ => unreachable!(),
        }
        let before = snapshot(&fixture.home);
        assert!(
            fixture
                .context
                .apply_reviewed_workflow_authorization(inputs.clone(), &full)
                .unwrap_err()
                .to_string()
                .contains("changed since review"),
            "{change}"
        );
        assert!(
            fixture
                .context
                .apply_reviewed_workflow_authorization_summary(inputs, &compact)
                .unwrap_err()
                .to_string()
                .contains("changed since review"),
            "{change}"
        );
        assert_eq!(snapshot(&fixture.home), before, "{change}");
        assert!(fixture.context.runs_list().unwrap().is_empty());
    }
}

#[test]
fn authorization_non_authority_blockers_never_offer_grants() {
    use transist::WorkflowAuthorizationStatus as Status;
    for extra in [
        "prerequisites:\n  paths:\n    - {path: absent, kind: file, required: false}\n",
        "prerequisites:\n  paths:\n    - {path: absent, kind: file, required: true}\n",
    ] {
        let fixture = Fixture::new();
        fixture.compile(
            extra,
            "  - id: review\n    control: control.gate.manual\n    with: {gate_id: review}\n",
        );
        fs::create_dir_all(fixture.home.join("policy/workflow-authorizations")).unwrap();
        fs::write(fixture.home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n").unwrap();
        let before = snapshot(&fixture.home);
        let full = fixture
            .context
            .authorize_workflow("fast.demo@v1", BTreeMap::new(), None, true)
            .unwrap();
        let compact = fixture
            .context
            .authorize_workflow_summary("fast.demo@v1", BTreeMap::new(), None, true)
            .unwrap();
        assert_eq!(full.status, Status::Blocked);
        assert_eq!(compact.status, full.status);
        assert!(!compact.readiness.authorization_eligible);
        assert_eq!(snapshot(&fixture.home), before);
    }
    let fixture = Fixture::new();
    fixture.plugin("process", true);
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\n", "  - id: echo\n    uses: process:fast.plugin/echo\n    with: {message: hello, secret_refs: [MISSING]}\n");
    assert_eq!(
        fixture
            .context
            .authorize_workflow("fast.demo@v1", BTreeMap::new(), None, true)
            .unwrap()
            .status,
        Status::Blocked
    );
}

#[test]
fn authorization_removed_secret_immediately_blocks_full_and_summary_without_writes() {
    use transist::WorkflowAuthorizationStatus as Status;
    let fixture = Fixture::new();
    fixture.plugin("mcp", true);
    fs::write(fixture.home.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: permissive\n  remote_secret_delivery:\n    mcp: true\n").unwrap();
    fixture.compile("plugins:\n  fast.plugin: 1.0.0\n", "  - id: echo\n    uses: mcp:fast.plugin/echo\n    with: {message: hello, secret_refs: [REMOVABLE]}\n");
    let full = fixture
        .context
        .authorize_workflow("fast.demo@v1", BTreeMap::new(), None, false)
        .unwrap();
    let compact = fixture
        .context
        .authorize_workflow_summary("fast.demo@v1", BTreeMap::new(), None, false)
        .unwrap();
    assert_eq!(full.status, Status::AlreadyAuthorized);
    assert_eq!(compact.status, full.status);
    REMOVABLE_SECRET.store(false, std::sync::atomic::Ordering::SeqCst);
    let before = snapshot(&fixture.home);
    for apply in [false, true] {
        let blocked = fixture
            .context
            .authorize_workflow("fast.demo@v1", BTreeMap::new(), None, apply)
            .unwrap();
        let summary = fixture
            .context
            .authorize_workflow_summary("fast.demo@v1", BTreeMap::new(), None, apply)
            .unwrap();
        assert_eq!(blocked.status, Status::Blocked);
        assert_eq!(summary.status, blocked.status);
        assert!(
            blocked
                .readiness
                .checks
                .iter()
                .any(|check| check.code == "secret_resolution"
                    && check.status == ValidationStatus::Failed)
        );
    }
    assert_eq!(
        fixture
            .context
            .apply_reviewed_workflow_authorization(BTreeMap::new(), &full)
            .unwrap()
            .status,
        Status::Blocked
    );
    assert_eq!(
        fixture
            .context
            .apply_reviewed_workflow_authorization_summary(BTreeMap::new(), &compact)
            .unwrap()
            .status,
        Status::Blocked
    );
    assert_eq!(snapshot(&fixture.home), before);
    assert!(fixture.context.runs_list().unwrap().is_empty());
}
