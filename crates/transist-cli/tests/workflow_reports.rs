// SPDX-License-Identifier: MPL-2.0
mod support;

use serde_json::Value;
use std::process::Output;
use support::TestRepo;

const FLOW: &str = "schema: transist.flow.v1\nid: report.demo\nversion: v1\nsteps:\n  - id: gate\n    control: control.gate.manual\n    with:\n      gate_id: review\n";

fn fixture() -> TestRepo {
    let repo = TestRepo::new();
    let path = repo.write_flow("workflow.yaml", FLOW);
    repo.compile(&path);
    repo
}

fn check(repo: &TestRepo, flags: &[&str]) -> Output {
    repo.cli()
        .current_dir(&repo.repo_root)
        .args(["workflows", "check", "report.demo@v1"])
        .args(flags)
        .output()
        .unwrap()
}

fn concise(output: &Output) {
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Workflow: report.demo@v1"), "{text}");
    assert!(
        text.contains("Full report: rerun this command with --format json."),
        "{text}"
    );
    assert!(serde_json::from_slice::<Value>(&output.stdout).is_err());
}

#[test]
fn default_and_plain_captured_stdout_are_concise_in_every_check_mode() {
    let repo = fixture();
    for flags in [&[][..], &["--format", "plain"][..]] {
        concise(&check(&repo, flags));
        for bound in [false, true] {
            let mut cmd = repo.cli();
            cmd.current_dir(&repo.repo_root)
                .args(["workflows", "check", "--path"])
                .arg(repo.repo_root.join("workflow.yaml"))
                .args(flags);
            if bound {
                let inputs = repo.write_flow("inputs.json", "{}");
                cmd.arg("--inputs-file").arg(inputs);
            }
            concise(&cmd.output().unwrap());
        }
    }
}

#[test]
fn explicit_json_preserves_full_diagnostics_in_every_check_mode() {
    let repo = fixture();
    let output = check(&repo, &["--format", "json"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!report["checks"].as_array().unwrap().is_empty());
    assert_eq!(report["checks"], report["validation"]["checks"]);
    assert_eq!(report["schema"], "transist.workflow_readiness.v1");
    for bound in [false, true] {
        let mut cmd = repo.cli();
        cmd.current_dir(&repo.repo_root)
            .args(["workflows", "check", "--format", "json", "--path"])
            .arg(repo.repo_root.join("workflow.yaml"));
        if bound {
            cmd.arg("--inputs-file")
                .arg(repo.write_flow("inputs.json", "{}"));
        }
        let output = cmd.output().unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["checks"].is_array());
        assert_eq!(report["mode"], if bound { "bound" } else { "authored" });
    }
}

#[test]
fn evidence_output_is_canonical_and_reusable_with_both_stdout_formats() {
    let repo = fixture();
    for (index, flags) in [vec![], vec!["--format", "json"]].into_iter().enumerate() {
        let path = repo.repo_root.join(format!("readiness-{index}.json"));
        let mut flags = flags;
        flags.extend(["--output", path.to_str().unwrap()]);
        let output = check(&repo, &flags);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = std::fs::read(&path).unwrap();
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&report).unwrap());
        assert!(report["fingerprint"].is_string());
        if index == 0 {
            concise(&output);
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                report
            );
        }
        let prepared = repo
            .cli()
            .current_dir(&repo.repo_root)
            .args([
                "workflows",
                "prepare",
                "report.demo@v1",
                "--run-ref",
                &format!("evidence-{index}"),
                "--readiness-file",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stderr)
        );
    }
}

#[test]
fn authorization_summary_and_json_do_not_write_without_noninteractive_approval() {
    let repo = fixture();
    std::fs::create_dir_all(repo.home_dir.join("policy/workflow-authorizations")).unwrap();
    std::fs::write(
        repo.home_dir.join("config.yaml"),
        "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n",
    )
    .unwrap();
    for format in [None, Some("plain"), Some("json")] {
        let mut cmd = repo.cli();
        cmd.current_dir(&repo.repo_root)
            .args(["workflows", "authorize", "report.demo@v1"]);
        if let Some(format) = format {
            cmd.args(["--format", format]);
        }
        let output = cmd.output().unwrap();
        assert!(!output.status.success());
        if format == Some("json") {
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["status"], "planned");
            assert!(report["readiness"]["checks"].is_array());
        } else {
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("Grants to add"),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
    assert_eq!(
        std::fs::read_dir(repo.home_dir.join("policy/workflow-authorizations"))
            .unwrap()
            .count(),
        0
    );
    assert!(
        transist::TransistContext::with_home(&repo.repo_root, &repo.home_dir)
            .runs_list()
            .unwrap()
            .is_empty()
    );
    let declined = repo
        .cli()
        .current_dir(&repo.repo_root)
        .env(
            "TRANSIST_CLI_TEST_PROMPT",
            r#"{"picker_available":true,"action_confirmations":[false]}"#,
        )
        .args(["workflows", "authorize", "report.demo@v1"])
        .output()
        .unwrap();
    assert!(declined.status.success());
    assert!(String::from_utf8_lossy(&declined.stdout).contains("No grants written"));
    assert_eq!(
        std::fs::read_dir(repo.home_dir.join("policy/workflow-authorizations"))
            .unwrap()
            .count(),
        0
    );
    assert!(
        transist::TransistContext::with_home(&repo.repo_root, &repo.home_dir)
            .runs_list()
            .unwrap()
            .is_empty()
    );
    let output = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args([
            "workflows",
            "authorize",
            "report.demo@v1",
            "--approve",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["status"],
        "applied"
    );
    let already = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args(["workflows", "authorize", "report.demo@v1", "--dry-run"])
        .output()
        .unwrap();
    assert!(already.status.success());
    assert!(String::from_utf8_lossy(&already.stdout).contains("No changes needed"));
}

#[test]
fn blocked_authorization_writes_no_grants_and_creates_no_run() {
    let repo = fixture();
    let output = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args(["workflows", "authorize", "missing@v1", "--approve"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("No grants written"));
    assert!(
        !repo
            .home_dir
            .join("policy/workflow-authorizations")
            .exists()
    );
    assert!(
        transist::TransistContext::with_home(&repo.repo_root, &repo.home_dir)
            .runs_list()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn redirected_check_and_authorization_receipt_stay_concise() {
    let repo = fixture();
    let stdout_path = repo.repo_root.join("stdout.txt");
    let output = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args(["workflows", "check", "report.demo@v1"])
        .stdout(std::fs::File::create(&stdout_path).unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = std::fs::read_to_string(&stdout_path).unwrap();
    assert!(text.contains("Workflow: report.demo@v1"));
    assert!(serde_json::from_str::<Value>(&text).is_err());
    std::fs::create_dir_all(repo.home_dir.join("policy/workflow-authorizations")).unwrap();
    std::fs::write(repo.home_dir.join("config.yaml"), "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: allowlist\n  sources: [workflow-authorizations]\n").unwrap();
    let planned = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args(["workflows", "authorize", "report.demo@v1", "--dry-run"])
        .output()
        .unwrap();
    assert!(planned.status.success());
    assert!(String::from_utf8_lossy(&planned.stdout).contains("Grants to add"));
    assert_eq!(
        std::fs::read_dir(repo.home_dir.join("policy/workflow-authorizations"))
            .unwrap()
            .count(),
        0
    );
    let applied = repo
        .cli()
        .current_dir(&repo.repo_root)
        .args(["workflows", "authorize", "report.demo@v1", "--approve"])
        .output()
        .unwrap();
    assert!(applied.status.success());
    let text = String::from_utf8_lossy(&applied.stdout);
    assert!(text.contains("Grants added"));
    assert!(!text.contains("Missing grants:"));
    assert!(serde_json::from_str::<Value>(&text).is_err());
}
