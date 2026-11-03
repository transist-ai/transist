// SPDX-License-Identifier: MPL-2.0
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use transist::workflow_definition_sha256;
#[cfg(not(target_env = "musl"))]
use transist_plugins::plugin_dir_sha256;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FixtureMetadata {
    workflow: FixtureSubject,
    plugin: FixtureSubject,
}

#[derive(Debug, Deserialize)]
struct FixtureSubject {
    subject_key: String,
    digest: String,
}

#[test]
fn committed_workflow_fixture_remains_invalid_until_resigned() {
    let fixture_root = fixture_root();
    let metadata = load_fixture_metadata(&fixture_root);
    let expected = fs::read_to_string(fixture_root.join("digest.expected"))
        .expect("read digest.expected")
        .trim()
        .to_string();
    let digest = workflow_definition_sha256(&fixture_root.join("demo.workflow.yaml"))
        .expect("workflow digest");
    assert_eq!(expected, metadata.workflow.digest);
    assert_eq!(
        format!("workflow:demo.workflow@v1@{expected}"),
        metadata.workflow.subject_key
    );
    assert_ne!(
        digest, expected,
        "authored fixture must be re-signed before trusted use"
    );
}

#[test]
#[cfg(not(target_env = "musl"))]
fn committed_plugin_fixture_matches_runtime_plugin_digest() {
    let fixture_root = fixture_root();
    let metadata = load_fixture_metadata(&fixture_root);
    let digest = plugin_dir_sha256(&fixture_root.join("demo-plugin")).expect("plugin digest");
    assert_eq!(digest, metadata.plugin.digest);
    assert_eq!(
        format!("plugin:demo.plugin@v1@{digest}"),
        metadata.plugin.subject_key
    );
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/admin_tooling")
}

fn load_fixture_metadata(root: &Path) -> FixtureMetadata {
    serde_json::from_str(
        &fs::read_to_string(root.join("fixture-metadata.json")).expect("fixture metadata"),
    )
    .expect("parse fixture metadata")
}
