// SPDX-License-Identifier: MPL-2.0

use assert_cmd::cargo::CommandCargoExt;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("admin_tooling")
}

fn run_cli(home_dir: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::cargo_bin("transist").expect("transist binary");
    cmd.arg("--home").arg(home_dir);
    cmd.args(args);
    cmd.output().expect("run transist")
}

fn parse_stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("parse stdout json")
}

#[test]
fn trust_lifecycle_commands_round_trip_under_home_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home_dir = temp.path().join("home");
    let fixture = fixture_root().join("test-org.pub");

    let init = run_cli(&home_dir, &["home", "init"]);
    assert!(
        init.status.success(),
        "home init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let import = run_cli(
        &home_dir,
        &[
            "--format",
            "json",
            "trust",
            "import",
            fixture.to_str().expect("fixture utf8"),
        ],
    );
    assert!(
        import.status.success(),
        "trust import failed: {}",
        String::from_utf8_lossy(&import.stderr)
    );
    let imported = parse_stdout_json(&import);
    let key_id = imported["key_id"]
        .as_str()
        .expect("import key id")
        .to_string();
    assert_eq!(imported["name"], "test-org");

    let list = run_cli(&home_dir, &["--format", "json", "trust", "list"]);
    assert!(
        list.status.success(),
        "trust list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let listed = parse_stdout_json(&list);
    assert_eq!(listed.as_array().expect("list array").len(), 1);
    assert_eq!(listed[0]["key_id"], key_id);

    let inspect = run_cli(
        &home_dir,
        &["--format", "json", "trust", "inspect", &key_id],
    );
    assert!(
        inspect.status.success(),
        "trust inspect failed: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let inspected = parse_stdout_json(&inspect);
    assert_eq!(inspected["schema"], "transist.trust_root.v1");
    assert_eq!(inspected["name"], "test-org");
    assert_eq!(inspected["key_id"], key_id);
    assert!(inspected["public_key"].as_str().is_some());

    let export_path = temp.path().join("exported/test-org.pub");
    let export = run_cli(
        &home_dir,
        &[
            "--format",
            "json",
            "trust",
            "export",
            &key_id,
            "--output",
            export_path.to_str().expect("export path utf8"),
        ],
    );
    assert!(
        export.status.success(),
        "trust export failed: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let exported = parse_stdout_json(&export);
    assert_eq!(exported["key_id"], key_id);
    assert_eq!(exported["output"], export_path.display().to_string());

    let exported_file: Value =
        serde_json::from_str(&fs::read_to_string(&export_path).expect("read export"))
            .expect("parse export");
    assert_eq!(exported_file, inspected);

    let export_stdout = run_cli(
        &home_dir,
        &[
            "--format", "json", "trust", "export", &key_id, "--output", "-",
        ],
    );
    assert!(
        export_stdout.status.success(),
        "trust export stdout failed: {}",
        String::from_utf8_lossy(&export_stdout.stderr)
    );
    let exported_stdout = parse_stdout_json(&export_stdout);
    assert_eq!(exported_stdout, inspected);

    let remove = run_cli(
        &home_dir,
        &["--format", "json", "trust", "remove", &key_id, "--yes"],
    );
    assert!(
        remove.status.success(),
        "trust remove failed: {}",
        String::from_utf8_lossy(&remove.stderr)
    );
    let removed = parse_stdout_json(&remove);
    assert_eq!(removed["name"], "test-org");
    assert_eq!(removed["key_id"], key_id);

    let list_after = run_cli(&home_dir, &["--format", "json", "trust", "list"]);
    assert!(
        list_after.status.success(),
        "trust list after remove failed: {}",
        String::from_utf8_lossy(&list_after.stderr)
    );
    let listed_after = parse_stdout_json(&list_after);
    assert!(
        listed_after
            .as_array()
            .expect("list after array")
            .is_empty()
    );
}
