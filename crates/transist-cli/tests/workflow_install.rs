// SPDX-License-Identifier: MPL-2.0
mod support;

use support::TestRepo;

const TEST_FLOW: &str = r#"
schema: transist.flow.v1
id: install_test_flow
version: v0
steps:
  - id: gate
    control: control.gate.manual
    with:
      gate_id: review
"#;

const RELEASE_FLOW: &str = r#"
schema: transist.flow.v1
id: compile_release_flow
version: 1.0.0
steps:
  - id: gate
    control: control.gate.manual
    with:
      gate_id: review
"#;

#[test]
fn workflow_check_path_without_inputs_emits_full_authored_report() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow(
        "check-authored.yaml",
        r#"
schema: transist.flow.v1
id: check_authored
version: v1
inputs:
  second:
    type: string
    required: true
  first:
    type: string
    required: true
steps:
  - id: finish
    control: control.finish
"#,
    );
    let output = repo
        .cli()
        .args(["workflows", "check", "--format", "json", "--path"])
        .arg(&flow_path)
        .output()
        .expect("run workflows check");

    assert!(
        output.status.success(),
        "workflows check should succeed; stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("authored validation JSON");
    assert_eq!(report["validated"], flow_path.display().to_string());
    assert_eq!(report["schema"], "transist.flow.v1");
    assert_eq!(report["report_schema"], "transist.workflow_validation.v1");
    assert_eq!(report["mode"], "authored");
    assert_eq!(report["workflow"], "check_authored@v1");
    assert_eq!(report["valid"], true);
    assert_eq!(report["checks"], report["report"]["checks"]);
    assert_eq!(report["report"]["status"], "not_evaluated");
    assert_eq!(
        report["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .map(|check| (
                check["code"].as_str().expect("code"),
                check["status"].as_str().expect("status"),
                check["message"].as_str().expect("message"),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "authored_definition",
                "passed",
                "captured workflow definition passed authored validation",
            ),
            (
                "required_input",
                "not_evaluated",
                "required workflow input 'first' is not evaluated in authored mode",
            ),
            (
                "required_input",
                "not_evaluated",
                "required workflow input 'second' is not evaluated in authored mode",
            ),
        ]
    );
    assert!(
        !repo
            .home_dir
            .join("registry/recipes/check_authored-v1.yaml")
            .exists()
    );
}

#[cfg(feature = "dev-version-bypass")]
#[test]
fn workflow_install_accepts_v0_workflow_with_dev_bypass() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-test.yaml", TEST_FLOW);
    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--approve")
        .output()
        .expect("run workflows install");

    assert!(
        output.status.success(),
        "workflows install should accept v0 with the development bypass; stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is a TEST workflow"),
        "expected test-workflow warning in stderr, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(not(feature = "dev-version-bypass"))]
#[test]
fn standalone_source_install_plans_v0_workflow_without_input_binding() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-test.yaml", TEST_FLOW);
    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--dry-run")
        .output()
        .expect("run workflows install");

    assert!(
        output.status.success(),
        "workflows install should plan an unsigned standalone source; stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("install plan JSON");
    assert_eq!(report["schema"], "transist.workflow_install.v1");
    assert_eq!(report["status"], "planned");
}

#[cfg(not(feature = "dev-version-bypass"))]
#[test]
fn workflow_install_rejects_v0_workflow_without_dev_bypass() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-test.yaml", TEST_FLOW);
    // Planning validates authored input without publishing a compiled recipe.
    // The reserved-version guard belongs to the actual installation boundary.
    let planned = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--dry-run")
        .output()
        .expect("plan v0 installation");
    assert!(planned.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&planned.stdout).expect("installation plan JSON");
    assert_eq!(report["status"], "planned");
    assert_eq!(report["workflow"], "install_test_flow@v0");
    assert_eq!(report["run_created"], false);

    let applied = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--approve")
        .output()
        .expect("apply v0 installation without the development bypass");
    assert!(
        !applied.status.success(),
        "approved v0 installation must be refused; stderr={} stdout={}",
        String::from_utf8_lossy(&applied.stderr),
        String::from_utf8_lossy(&applied.stdout)
    );
    let stderr = String::from_utf8_lossy(&applied.stderr);
    assert!(
        stderr.contains("workflow 'install_test_flow' uses reserved test version 'v0'")
            && stderr.contains("rebuild with dev-version-bypass"),
        "expected the reserved-version publication guard, got: {stderr}"
    );
    for relative in [
        "registry/recipes/install_test_flow-v0.yaml",
        "registry/workflow-install-index.json",
    ] {
        assert!(
            !repo.home_dir.join(relative).exists(),
            "refused installation must not publish {relative}"
        );
    }
    assert!(
        repo.home_dir
            .join("runs")
            .read_dir()
            .expect("read runs")
            .next()
            .is_none(),
        "refused installation must not create a run"
    );
    assert_eq!(std::fs::read_to_string(flow_path).unwrap(), TEST_FLOW);
}

#[test]
fn workflow_install_dry_run_reports_a_plan_without_mutation() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-dry-run.yaml", RELEASE_FLOW);
    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--dry-run")
        .output()
        .expect("run workflows install");

    assert!(
        output.status.success(),
        "workflows install --dry-run should succeed; stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("install dry-run JSON");
    assert!(
        report["schema"] == "transist.workflow_install.v1"
            && report["status"] == "planned"
            && report["run_created"] == false,
        "unexpected report: {report}"
    );
    assert!(
        !repo
            .home_dir
            .join("registry/recipes/compile_release_flow-1.0.0.yaml")
            .exists()
    );
    assert!(
        !repo
            .home_dir
            .join("registry/workflow-install-index.json")
            .exists()
    );
}

#[test]
fn workflow_install_requires_noninteractive_approval_without_mutation() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-no-approval.yaml", RELEASE_FLOW);
    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .output()
        .expect("run workflows install");

    assert!(
        !output.status.success(),
        "noninteractive install must require approval; stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("noninteractive install JSON");
    assert!(
        report["status"] == "planned" && report["run_created"] == false,
        "unexpected report: {report}"
    );
    assert!(
        !repo
            .home_dir
            .join("registry/recipes/compile_release_flow-1.0.0.yaml")
            .exists()
    );
}

#[test]
fn workflow_install_approve_applies_a_valid_standalone_plan() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-approve.yaml", RELEASE_FLOW);
    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--approve")
        .output()
        .expect("run workflows install");

    assert!(
        output.status.success(),
        "approved install should succeed; stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("approved install JSON");
    assert!(
        report["status"] == "applied" && report["run_created"] == false,
        "unexpected report: {report}"
    );
    assert!(
        repo.home_dir
            .join("registry/recipes/compile_release_flow-1.0.0.yaml")
            .is_file()
    );
    assert!(
        repo.home_dir
            .join("registry/workflow-install-index.json")
            .is_file()
    );
}

#[test]
fn workflow_install_reports_an_unchanged_snapshot_as_already_installed() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-idempotent.yaml", RELEASE_FLOW);
    let applied = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--approve")
        .output()
        .expect("apply workflows install");
    assert!(applied.status.success());

    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--dry-run")
        .output()
        .expect("rerun workflows install");

    assert!(output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("idempotent install JSON");
    assert_eq!(report["schema"], "transist.workflow_install.v1");
    assert_eq!(report["status"], "already_installed");
    assert_eq!(report["run_created"], false);
}

#[test]
fn workflow_install_does_not_report_a_modified_recipe_as_already_installed() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-corrupt-recipe.yaml", RELEASE_FLOW);
    let applied = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--approve")
        .output()
        .expect("apply workflows install");
    assert!(applied.status.success());
    std::fs::write(
        repo.home_dir
            .join("registry/recipes/compile_release_flow-1.0.0.yaml"),
        "corrupted recipe",
    )
    .expect("corrupt installed recipe");

    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--dry-run")
        .output()
        .expect("replan workflows install");

    assert!(output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("install plan JSON");
    assert_eq!(report["status"], "planned");
}

#[test]
fn workflow_install_does_not_bind_required_inputs() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow(
        "install-missing-input.yaml",
        r#"
schema: transist.flow.v1
id: install_requires_input
version: v1
inputs:
  subject:
    type: string
    required: true
steps:
  - id: finish
    control: control.finish
    with: {}
"#,
    );
    let output = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--dry-run")
        .output()
        .expect("run workflows install");

    assert!(output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("blocked install JSON");
    assert_eq!(report["schema"], "transist.workflow_install.v1");
    assert_eq!(report["status"], "planned");
    assert_eq!(report["run_created"], false);
    assert!(
        !repo
            .home_dir
            .join("registry/recipes/install_requires_input-v1.yaml")
            .exists()
    );
}

#[test]
fn workflow_install_rejects_removed_readiness_and_authorization_options() {
    let repo = TestRepo::new();
    let flow_path = repo.write_flow("install-removed-options.yaml", RELEASE_FLOW);
    let inputs_path = repo.repo_root.join("inputs.yaml");
    std::fs::write(&inputs_path, "subject: demo\n").expect("write inputs");
    let signature_path = repo.repo_root.join("workflow.sig");
    std::fs::write(&signature_path, "not a signature\n").expect("write signature");
    let policy_path = repo.repo_root.join("policy.yaml");

    let cases: Vec<(&str, Vec<&std::ffi::OsStr>)> = vec![
        ("--input", vec!["--input".as_ref(), "subject=demo".as_ref()]),
        (
            "--inputs-file",
            vec!["--inputs-file".as_ref(), inputs_path.as_os_str()],
        ),
        ("--local-only", vec!["--local-only".as_ref()]),
        (
            "--signature",
            vec!["--signature".as_ref(), signature_path.as_os_str()],
        ),
        (
            "--policy-output",
            vec!["--policy-output".as_ref(), policy_path.as_os_str()],
        ),
    ];

    for (option, arguments) in cases {
        let mut command = repo.cli();
        command
            .args(["workflows", "install", "--path"])
            .arg(&flow_path)
            .args(arguments);
        let output = command.output().expect("run workflows install");

        assert!(
            !output.status.success(),
            "{option} must be rejected; stderr={} stdout={}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(option)
                && stderr.contains("workflows check")
                && stderr.contains("workflows authorize"),
            "expected migration guidance for {option}, got: {stderr}"
        );
    }

    assert!(
        !repo
            .home_dir
            .join("registry/workflow-install-index.json")
            .exists(),
        "removed install options must not persist a workflow"
    );
    assert!(
        repo.home_dir
            .join("runs")
            .read_dir()
            .expect("read runs")
            .next()
            .is_none(),
        "removed install options must not create runs"
    );
}
