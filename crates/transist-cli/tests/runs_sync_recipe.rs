// SPDX-License-Identifier: MPL-2.0
#[cfg(feature = "dev-version-bypass")]
mod support;

#[cfg(feature = "dev-version-bypass")]
#[test]
fn runs_sync_recipe_refreshes_v0_snapshot_from_registry() {
    let flow_v1 = r#"
schema: transist.flow.v1
id: sync_recipe_demo
version: v0
steps:
  - id: finish
    control: control.finish
"#;
    let flow_v2 = r#"
schema: transist.flow.v1
id: sync_recipe_demo
version: v0
steps:
  - id: finish_now
    control: control.finish
"#;
    let repo = support::TestRepo::new();
    let flow_path = repo.write_flow("sync-recipe.flow.yaml", flow_v1);
    repo.compile(&flow_path);

    let prepare = repo
        .cli()
        .args([
            "workflows",
            "prepare",
            "sync_recipe_demo@v0",
            "--run-ref",
            "sync-recipe-run",
        ])
        .output()
        .expect("run prepare");
    assert!(
        prepare.status.success(),
        "prepare failed: {}",
        String::from_utf8_lossy(&prepare.stderr)
    );

    std::fs::write(&flow_path, flow_v2).expect("rewrite flow");
    let install = repo
        .cli()
        .args(["workflows", "install", "--path"])
        .arg(&flow_path)
        .arg("--approve")
        .output()
        .expect("install updated workflow");
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let sync = repo
        .cli()
        .args(["runs", "sync-recipe", "sync-recipe-run"])
        .output()
        .expect("run sync-recipe");
    assert!(
        sync.status.success(),
        "sync-recipe failed: {}",
        String::from_utf8_lossy(&sync.stderr)
    );

    let stdout: serde_json::Value = serde_json::from_slice(&sync.stdout).expect("parse sync json");
    assert_eq!(stdout["run_id"], "sync-recipe-run");
    assert_eq!(stdout["workflow_version"], "v0");
    assert!(
        String::from_utf8_lossy(&sync.stderr).contains("is a TEST run"),
        "expected test-run warning in stderr, got: {}",
        String::from_utf8_lossy(&sync.stderr)
    );

    let run_recipe = std::fs::read_to_string(repo.run_root("sync-recipe-run").join("recipe.yaml"))
        .expect("read run recipe");
    assert!(run_recipe.contains("finish_now"));
}
