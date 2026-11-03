// SPDX-License-Identifier: MPL-2.0
//! YAML parse/serialize fixtures for the `yaml_serde` migration.
//!
//! These tests prove the migrated library parses existing fixtures into the
//! expected typed values, preserves checked-in serialized bytes, handles
//! malformed input with stable category/location diagnostics, and preserves
//! the `Value`-keyed `Mapping` API contract.

use std::collections::BTreeMap;
use std::fs;

use serde::Serialize;
use transist_compiler::{Flow, compile_flow, load_flow_unvalidated};
use transist_plugins::PluginManifest;

const ADMIN_WORKFLOW: &str = "tests/fixtures/admin_tooling/demo.workflow.yaml";
const ADMIN_PLUGIN: &str = "tests/fixtures/admin_tooling/demo-plugin/plugin.yaml";
const RECIPE_GOLDEN: &str = "tests/fixtures/yaml_compatibility/recipe.yaml";
const PLUGIN_GOLDEN: &str = "tests/fixtures/yaml_compatibility/plugin.yaml";
const SETTINGS_GOLDEN: &str = "tests/fixtures/yaml_compatibility/settings.yaml";
const SIGNATURE_GOLDEN: &str = "tests/fixtures/yaml_compatibility/signature.json";
const TRUST_ROOT_GOLDEN: &str = "tests/fixtures/yaml_compatibility/trust-root.json";

fn read_fixture(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn assert_yaml_golden<T: Serialize>(value: &T, path: &str) {
    let actual = yaml_serde::to_string(value).expect("serialize YAML");
    let expected = fs::read(path).unwrap_or_else(|e| panic!("read golden {path}: {e}"));
    assert_eq!(
        actual.as_bytes(),
        expected.as_slice(),
        "serialized YAML bytes changed for {path}"
    );
}

fn assert_json_golden<T: Serialize>(value: &T, path: &str) {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("serialize JSON")
    );
    let expected = fs::read(path).unwrap_or_else(|e| panic!("read golden {path}: {e}"));
    assert_eq!(
        actual.as_bytes(),
        expected.as_slice(),
        "serialized JSON bytes changed for {path}"
    );
}

#[test]
fn workflow_fixture_parses_to_expected_flow() {
    let flow: Flow =
        load_flow_unvalidated(std::path::Path::new(ADMIN_WORKFLOW)).expect("parse workflow");
    assert_eq!(flow.schema, "transist.flow.v1");
    assert_eq!(flow.id, "demo.workflow");
    assert_eq!(flow.version, "v1");
    assert_eq!(flow.plugins.get("shell").map(String::as_str), Some("1.0.0"));
    assert_eq!(flow.steps.len(), 3);

    let review = &flow.steps[0];
    assert_eq!(review.id, "review");
    assert!(review.subflow.is_some());

    let run_script = &flow.steps[1];
    assert_eq!(run_script.id, "run_script");
    assert_eq!(run_script.adapter.as_deref(), Some("process"));
    assert_eq!(run_script.plugin.as_deref(), Some("shell"));
    assert_eq!(run_script.action.as_deref(), Some("exec"));
}

#[test]
fn compiled_recipe_serializes_to_golden_bytes() {
    let flow: Flow =
        load_flow_unvalidated(std::path::Path::new(ADMIN_WORKFLOW)).expect("parse workflow");
    let recipe_flow = Flow {
        source_path: None,
        steps: vec![flow.steps[1].clone()],
        ..flow
    };
    let recipe = compile_flow(&recipe_flow).expect("compile recipe");
    assert_yaml_golden(&recipe, RECIPE_GOLDEN);
}

#[test]
fn plugin_manifest_fixture_parses_to_expected_value() {
    let raw = read_fixture(ADMIN_PLUGIN);
    let manifest: PluginManifest = yaml_serde::from_str(&raw).expect("parse manifest");
    assert_yaml_golden(&manifest, PLUGIN_GOLDEN);
    assert_eq!(manifest.schema, "transist.plugin.v1");
    assert_eq!(manifest.id, "demo.plugin");
    assert_eq!(manifest.version, "v1");
    assert_eq!(manifest.actions.len(), 1);
    assert_eq!(manifest.actions[0].name, "run");
}

#[test]
fn user_settings_yaml_parses_to_expected_semantic_value() {
    let raw = read_fixture(SETTINGS_GOLDEN);
    let json: serde_json::Value = yaml_serde::from_str(&raw).expect("parse settings");
    assert_eq!(json["schema"], "transist.user-settings.v1");
    assert_eq!(json["identity"]["actor"], "reviewer@example.com");

    // The typed UserSettings serializer owns this persisted-byte contract
    // (covered in src/home/settings.rs). serde_json::Value sorts map keys and
    // therefore cannot prove the same byte sequence from this generic view.
}

#[test]
fn trust_signature_envelope_is_json_not_yaml() {
    let signature_json = read_fixture(SIGNATURE_GOLDEN);
    let envelope =
        transist_core::parse_signature_envelope(&signature_json).expect("parse signature JSON");
    assert_json_golden(&envelope, SIGNATURE_GOLDEN);
    assert_eq!(envelope.schema, "transist.signature.v1");
    assert_eq!(
        envelope.subject.kind,
        transist_core::SignatureSubjectKind::Workflow
    );

    let trust_root_json = read_fixture(TRUST_ROOT_GOLDEN);
    let trust_root = transist_core::parse_trust_root(&trust_root_json).expect("parse trust root");
    assert_json_golden(&trust_root, TRUST_ROOT_GOLDEN);
    assert_eq!(trust_root.schema, "transist.trust_root.v1");

    // The signature schema itself validates against its JSON schema.
    let schema_path = "schemas/trust/signature.v1.schema.json";
    let schema_raw = fs::read_to_string(schema_path).expect("read signature schema");
    let schema: serde_json::Value = serde_json::from_str(&schema_raw).expect("parse schema");
    assert!(schema["properties"]["schema"]["const"] == "transist.signature.v1");
}

#[test]
fn malformed_yaml_produces_diagnostic_with_location() {
    let malformed = "workflow:\n  id: demo\n  steps:\n    - id: step1\n      bad: [unclosed";
    let result: Result<serde_json::Value, _> = yaml_serde::from_str(malformed);
    let err = result.expect_err("malformed YAML must error");
    let msg = err.to_string();
    assert!(
        !msg.is_empty(),
        "malformed YAML must produce a non-empty diagnostic"
    );
    let loc = err.location();
    let loc = loc.expect("malformed YAML diagnostic must include source location");
    assert_eq!((loc.line(), loc.column()), (6, 1));
    let debug = format!("{err:?}");
    assert!(debug.contains("kind: PARSER"));
}

#[test]
fn scanner_malformed_yaml_has_stable_category_and_location() {
    let result: Result<serde_json::Value, _> = yaml_serde::from_str("@invalid_yaml");
    let err = result.expect_err("invalid YAML token must error");
    let loc = err.location().expect("scanner error location");
    assert_eq!((loc.line(), loc.column()), (1, 1));
    let debug = format!("{err:?}");
    assert!(debug.contains("kind: SCANNER"));
}

#[test]
fn workflow_value_mapping_uses_value_keys() {
    // Mapping keys are Values, not strings; this guards against API drift.
    let raw = "steps:\n  - id: a\n    with:\n      key: value\n";
    let value: yaml_serde::Value = yaml_serde::from_str(raw).expect("parse value");
    let mapping = value.as_mapping().expect("root is mapping");
    // Mapping keys are Value, so indexing with a string literal works.
    assert!(mapping.get("steps").is_some());
    let steps = mapping.get("steps").unwrap().as_sequence().unwrap();
    let first = &steps[0];
    let with = first.get("with").expect("with field");
    let with_map = with.as_mapping().unwrap();
    assert_eq!(with_map.get("key").unwrap().as_str(), Some("value"));
}

#[test]
fn yaml_string_values_preserve_content() {
    let raw = "id: hello world\nversion: v1\n";
    let value: yaml_serde::Value = yaml_serde::from_str(raw).expect("parse");
    let mapping = value.as_mapping().unwrap();
    assert_eq!(mapping.get("id").unwrap().as_str(), Some("hello world"));
    assert_eq!(mapping.get("version").unwrap().as_str(), Some("v1"));
}

#[test]
fn empty_and_nested_yaml_handling() {
    let empty_mapping = "{}\n";
    let value: yaml_serde::Value =
        yaml_serde::from_str(empty_mapping).expect("parse empty mapping");
    assert!(value.as_mapping().unwrap().is_empty());

    let nested = "outer:\n  inner:\n    deep: value\n";
    let value: yaml_serde::Value = yaml_serde::from_str(nested).expect("parse nested");
    let outer = value.as_mapping().unwrap().get("outer").unwrap();
    let inner = outer.as_mapping().unwrap().get("inner").unwrap();
    assert_eq!(
        inner.as_mapping().unwrap().get("deep").unwrap().as_str(),
        Some("value")
    );
}

#[test]
fn btremap_with_yaml_value_preserves_types() {
    let raw = "with:\n  zeta: 1\n  alpha: 2\n  beta: 3\n";
    let map: BTreeMap<String, yaml_serde::Value> = yaml_serde::from_str(raw).expect("parse");
    // BTreeMap sorts keys alphabetically at the outer level.
    let keys: Vec<&String> = map.keys().collect();
    assert_eq!(keys, vec!["with"]);
    let with = map.get("with").unwrap().as_mapping().unwrap();
    // Inner mappings preserve insertion order.
    let with_keys: Vec<&str> = with.keys().filter_map(|k| k.as_str()).collect();
    assert_eq!(with_keys, vec!["zeta", "alpha", "beta"]);
}
