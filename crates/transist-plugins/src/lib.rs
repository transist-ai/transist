// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Plugin manifest and local installation support for Transist.
//!
//! This crate owns plugin manifest models, manifest validation, local plugin
//! installation/listing/removal, and plugin resolution helpers. It may depend on
//! protocol-level value contracts, but it must not own runtime execution or
//! interface-specific behavior.
//!
//! The documented crate-root manifest and lifecycle APIs are the intended stable
//! surface. Internal path helpers and module layout are implementation details.

mod catalog;
mod digest;
mod index;
mod inspect;
mod install;
mod list;
mod manifest;
mod measurement;
mod paths;
mod remove;
mod resolve;
mod schema;
mod staged;
mod validate;

pub use catalog::{ValidatedPluginCatalog, ValidatedPluginPackage};
pub use digest::plugin_dir_sha256;
pub use inspect::inspect_plugin_in_home;
pub use install::{
    install_plugin, install_plugin_file, install_plugin_file_in_home,
    install_plugin_file_in_home_with_target, install_plugin_in_home, stage_plugin_package,
};
pub use list::list_plugins_in_home;
pub use manifest::{
    ActionDescriptor, ActionGateDecisionBinding, ActionPromptBinding, ActionSkillsBinding,
    AdapterKind, InstalledPlugin, PLUGIN_MANIFEST_FILE_NAME, PLUGIN_MANIFEST_SCHEMA_V1,
    PluginCapabilities, PluginEntrypoint, PluginInstallReport, PluginManifest, PluginPackage,
    ProviderKind, SecretBinding, SecretInput, load_manifest, load_manifest_file,
    runtime_temp_relative_path,
};
pub use measurement::{ValidationWork, capture_validation_work, record_validation_work};
pub use remove::{remove_plugin_in_home, uninstall_plugin_in_home};
pub use resolve::{
    PluginVersionSelector, resolve_plugin_in_home, resolve_plugin_with_selector_in_home,
};
pub use schema::{
    schema_types_at_path, validate_action_input, validate_action_input_partial,
    validate_action_output, validate_action_schemas,
};
pub use staged::{
    StagedPluginPackage, publish_staged_plugin, publish_staged_plugin_with_sidecar,
    stage_plugin_package_in_home,
};
pub use validate::validate_manifest;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::Path;
    #[cfg(unix)]
    use std::process::Command;
    use transist_prerequisites::PrerequisiteStatus;

    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod");
    }

    #[cfg(not(unix))]
    fn set_executable(_path: &Path) {}

    #[cfg(unix)]
    fn create_fifo(path: &Path) {
        let status = Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed: {status}");
    }

    fn write_fixture(dir: &Path) {
        write_fixture_with_extra_top_level(dir, "");
    }

    #[test]
    fn validation_reuse_catalog_shares_package_and_validates_changed_inputs() {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let dir = home.path().join("plugins/shell/v1");
        fs::create_dir_all(&dir).unwrap();
        write_fixture(&dir);
        let manifest = load_manifest_file(&dir.join("plugin.yaml")).unwrap();
        let actual = home
            .path()
            .join("plugins")
            .join(&manifest.id)
            .join(&manifest.version);
        if actual != dir {
            fs::create_dir_all(actual.parent().unwrap()).unwrap();
            fs::rename(&dir, &actual).unwrap();
        }
        let catalog = ValidatedPluginCatalog::new(
            home.path().to_path_buf(),
            std::collections::BTreeMap::new(),
        );
        let first = catalog.load(&manifest.id, &manifest.version).unwrap();
        let second = catalog.load(&manifest.id, &manifest.version).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        catalog
            .validate_input(&first, &manifest.actions[0], &serde_json::json!({}))
            .unwrap();
        catalog
            .validate_input(
                &second,
                &manifest.actions[0],
                &serde_json::json!({"fresh": true}),
            )
            .unwrap();
    }

    #[test]
    fn validation_reuse_receipt_rejects_transitive_schema_tampering() {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let source = home.path().join("source");
        fs::create_dir(&source).unwrap();
        write_fixture(&source);
        let mut manifest = load_manifest_file(&source.join("plugin.yaml")).unwrap();
        fs::write(
            source.join("schemas/input.json"),
            r#"{"$ref":"value.json"}"#,
        )
        .unwrap();
        fs::write(source.join("schemas/value.json"), r#"{"type":"object"}"#).unwrap();
        manifest.package.files.push("schemas/value.json".into());
        fs::write(
            source.join("plugin.yaml"),
            yaml_serde::to_string(&manifest).unwrap(),
        )
        .unwrap();
        load_manifest_file(&source.join("plugin.yaml")).unwrap();
        let dir = home
            .path()
            .join("plugins")
            .join(&manifest.id)
            .join(&manifest.version);
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        fs::rename(source, &dir).unwrap();
        let digests = std::collections::BTreeMap::from([(
            format!("{}@{}", manifest.id, manifest.version),
            plugin_dir_sha256(&dir).unwrap(),
        )]);
        let catalog = ValidatedPluginCatalog::new(home.path().to_path_buf(), digests.clone());
        catalog.load(&manifest.id, &manifest.version).unwrap();
        fs::write(dir.join("schemas/value.json"), r#"{"type":"integer"}"#).unwrap();
        let fresh = ValidatedPluginCatalog::new(home.path().to_path_buf(), digests);
        assert!(
            fresh
                .load(&manifest.id, &manifest.version)
                .unwrap_err()
                .to_string()
                .contains("plugin_pin_mismatch")
        );
    }

    fn write_fixture_with_extra_top_level(dir: &Path, extra_top_level: &str) {
        fs::create_dir_all(dir.join("schemas")).expect("schemas");
        fs::write(dir.join("schemas/input.json"), "{}").expect("input");
        fs::write(dir.join("schemas/output.json"), "{}").expect("output");
        fs::write(dir.join("plugin.sh"), "#!/bin/sh\nexit 0\n").expect("script");
        set_executable(&dir.join("plugin.sh"));
        let extra_top_level = if extra_top_level.is_empty() {
            String::new()
        } else {
            format!("{extra_top_level}\n")
        };
        fs::write(
            dir.join("plugin.yaml"),
            format!(
                r#"
schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: plugin.sh
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
{extra_top_level}actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
            ),
        )
        .expect("manifest");
    }

    #[test]
    fn load_manifest_validates_fixture() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        let manifest = load_manifest(tmp.path()).expect("manifest");
        assert_eq!(manifest.id, "shell");
    }

    fn replace_package_files(dir: &Path, files: &str) {
        let path = dir.join("plugin.yaml");
        let manifest = fs::read_to_string(&path).expect("manifest");
        let start = manifest.find("package:\n").expect("package section");
        let end = manifest[start..]
            .find("actions:\n")
            .map(|offset| start + offset)
            .expect("actions section");
        let replacement = format!("package:\n  files:\n{files}");
        fs::write(
            &path,
            format!("{}{}{}", &manifest[..start], replacement, &manifest[end..]),
        )
        .expect("manifest package files");
    }

    #[test]
    fn load_manifest_requires_explicit_package_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        let path = tmp.path().join("plugin.yaml");
        let manifest = fs::read_to_string(&path).expect("manifest");
        fs::write(&path, manifest.replace("package:\n", "legacy_package:\n")).expect("manifest");

        let error = load_manifest(tmp.path()).expect_err("package.files should be required");
        assert!(format!("{error:#}").contains("package"), "{error:#}");
    }

    #[test]
    fn load_manifest_accepts_unreferenced_selected_support_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        fs::write(tmp.path().join("support.txt"), "runtime support").expect("support file");
        replace_package_files(
            tmp.path(),
            "    - plugin.sh\n    - schemas/input.json\n    - schemas/output.json\n    - support.txt\n",
        );

        load_manifest(tmp.path()).expect("unreferenced selected support file should pass");
    }

    #[test]
    fn load_manifest_rejects_invalid_package_file_paths() {
        for files in [
            "    - \"\"\n",
            "    - /tmp/plugin.sh\n",
            "    - .\n",
            "    - ..\n",
            "    - schemas\n",
            "    - plugin.sh\n    - plugin.sh\n",
            "    - plugin.yaml\n",
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            write_fixture(tmp.path());
            replace_package_files(tmp.path(), files);
            let error = load_manifest(tmp.path()).expect_err("invalid package file should fail");
            assert!(!error.to_string().is_empty(), "{error:#}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_manifest_rejects_selected_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        symlink(tmp.path().join("plugin.sh"), tmp.path().join("linked.sh")).expect("symlink");
        replace_package_files(
            tmp.path(),
            "    - linked.sh\n    - schemas/input.json\n    - schemas/output.json\n",
        );

        let error = load_manifest(tmp.path()).expect_err("selected symlink should fail");
        assert!(error.to_string().contains("symlink"), "{error:#}");
    }

    #[test]
    fn load_manifest_rejects_invalid_action_schema() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        fs::write(
            tmp.path().join("schemas/input.json"),
            r#"{"type":"not-a-json-schema-type"}"#,
        )
        .expect("invalid input schema");

        let error = load_manifest(tmp.path()).expect_err("invalid schema should fail");
        assert!(error.to_string().contains("invalid input/output schemas"));
        assert!(error.to_string().contains("input"));
    }

    #[test]
    fn action_schema_supports_package_local_refs_and_full_constraints() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        fs::write(
            tmp.path().join("schemas/input.json"),
            r#"{
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {"message": {"$ref": "shared.json#/$defs/message"}},
                "required": ["message"],
                "additionalProperties": false
            }"#,
        )
        .expect("input schema");
        fs::write(
            tmp.path().join("schemas/shared.json"),
            r#"{
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "$defs": {"message": {"type": "string", "minLength": 3, "pattern": "^[a-z]+$"}}
            }"#,
        )
        .expect("shared schema");
        let manifest_path = tmp.path().join("plugin.yaml");
        let manifest = fs::read_to_string(&manifest_path).expect("manifest");
        fs::write(
            &manifest_path,
            manifest.replace(
                "    - schemas/output.json\n",
                "    - schemas/output.json\n    - schemas/shared.json\n",
            ),
        )
        .expect("manifest package files");

        let manifest = load_manifest(tmp.path()).expect("manifest");
        let action = &manifest.actions[0];
        validate_action_input(tmp.path(), action, &serde_json::json!({"message": "hello"}))
            .expect("valid input");
        let error =
            validate_action_input(tmp.path(), action, &serde_json::json!({"message": "NO"}))
                .expect_err("invalid input");
        assert!(error.to_string().contains("instance path '/message'"));
        assert!(error.to_string().contains("schema path"));
        assert!(!error.to_string().contains("NO"));
    }

    #[test]
    fn action_schema_rejects_remote_and_parent_refs() {
        for reference in [
            "https://example.com/schema.json",
            "../outside.json",
            "/tmp/schema.json",
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            write_fixture(tmp.path());
            fs::write(
                tmp.path().join("schemas/input.json"),
                format!(r#"{{"$ref":"{reference}"}}"#),
            )
            .expect("input schema");
            let error = load_manifest(tmp.path()).expect_err("unsafe ref should fail");
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("relative package path")
                    || rendered.contains("forbidden path component"),
                "unexpected error for {reference}: {error}"
            );
        }
    }

    #[test]
    fn load_manifest_file_accepts_plugin_yaml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        let manifest = load_manifest_file(&tmp.path().join("plugin.yaml")).expect("manifest file");
        assert_eq!(manifest.id, "shell");
    }
    #[test]
    fn load_manifest_file_rejects_non_plugin_yaml_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        let bad = tmp.path().join("not-plugin.yaml");
        fs::rename(tmp.path().join("plugin.yaml"), &bad).expect("rename");
        let error = load_manifest_file(&bad).expect_err("non-plugin.yaml name should fail");
        assert!(error.to_string().contains("plugin.yaml"));
    }

    #[test]
    fn load_manifest_file_rejects_schema_reference_escaping_plugin_dir() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let plugin_dir = parent.path().join("plugin");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        write_fixture(&plugin_dir);
        let outside = parent.path().join("outside.json");
        fs::write(&outside, "{}").expect("outside schema");
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: plugin.sh
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: run
    input_schema: ../outside.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        let error =
            load_manifest_file(&plugin_dir.join("plugin.yaml")).expect_err("escape should fail");
        assert!(error.to_string().contains("escapes plugin directory"));
    }

    #[test]
    fn load_manifest_file_rejects_entrypoint_escaping_plugin_dir() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let plugin_dir = parent.path().join("plugin");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        write_fixture(&plugin_dir);
        let sibling = parent.path().join("sibling");
        fs::create_dir_all(&sibling).expect("sibling dir");
        let outside_script = sibling.join("evil.sh");
        fs::write(&outside_script, "#!/bin/sh\nexit 0\n").expect("outside script");
        set_executable(&outside_script);
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: ../sibling/evil.sh
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        let error =
            load_manifest_file(&plugin_dir.join("plugin.yaml")).expect_err("escape should fail");
        assert!(error.to_string().contains("escapes plugin directory"));
    }

    #[test]
    fn load_manifest_file_rejects_duplicate_action_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        fs::write(
            tmp.path().join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: plugin.sh
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        let error = load_manifest_file(&tmp.path().join("plugin.yaml"))
            .expect_err("duplicate action names should fail");
        assert!(
            error
                .to_string()
                .contains("duplicate plugin action name 'run'")
        );
    }

    #[test]
    fn load_manifest_file_rejects_mcp_action_without_tool_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join("schemas")).expect("schemas");
        fs::write(tmp.path().join("schemas/input.json"), "{}").expect("input");
        fs::write(tmp.path().join("schemas/output.json"), "{}").expect("output");
        fs::write(tmp.path().join("plugin.sh"), "#!/bin/sh\nexit 0\n").expect("script");
        set_executable(&tmp.path().join("plugin.sh"));
        fs::write(
            tmp.path().join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: mcp.echo
version: v1
adapter: mcp
entrypoint:
  type: mcp
  command: plugin.sh
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: false
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        let error = load_manifest_file(&tmp.path().join("plugin.yaml"))
            .expect_err("missing tool_name should fail");
        assert!(
            error
                .to_string()
                .contains("action 'run' for adapter 'mcp' requires bindings.tool_name")
        );
    }

    #[test]
    fn load_manifest_file_rejects_cwd_escaping_plugin_dir() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let plugin_dir = parent.path().join("plugin");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        write_fixture(&plugin_dir);
        fs::write(
            plugin_dir.join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: plugin.sh
  cwd: ../outside
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        let error = load_manifest_file(&plugin_dir.join("plugin.yaml"))
            .expect_err("cwd escape should fail");
        assert!(error.to_string().contains("escapes plugin directory"));
    }

    #[test]
    fn load_manifest_file_rejects_malformed_http_base_urls() {
        for (base_url, expected) in [
            ("ftp://example.com", "must use http:// or https://"),
            ("http://[::1", "valid URL"),
            (
                "http://user:password@example.com",
                "must not contain userinfo",
            ),
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            write_fixture(tmp.path());
            fs::write(
                tmp.path().join("plugin.yaml"),
                format!(
                    r#"
schema: transist.plugin.v1
id: http.invalid
version: v1
adapter: http
entrypoint:
  type: http
  base_url: "{base_url}"
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#
                ),
            )
            .expect("manifest");
            let error = load_manifest_file(&tmp.path().join("plugin.yaml"))
                .expect_err("malformed HTTP base URL should fail");
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[test]
    fn load_manifest_file_accepts_cwd_inside_plugin_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        fs::create_dir_all(tmp.path().join("work")).expect("work dir");
        fs::write(tmp.path().join("work/marker"), "runtime cwd").expect("cwd marker");
        fs::write(
            tmp.path().join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: plugin.sh
  cwd: work
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
    - work/marker
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        let manifest =
            load_manifest_file(&tmp.path().join("plugin.yaml")).expect("valid cwd should pass");
        assert_eq!(manifest.id, "shell");
    }

    #[test]
    fn load_manifest_file_accepts_nonexistent_cwd_inside_plugin_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        fs::create_dir_all(tmp.path().join("will_be_created_at_runtime")).expect("runtime cwd");
        fs::write(
            tmp.path().join("will_be_created_at_runtime/marker"),
            "runtime cwd",
        )
        .expect("cwd marker");
        fs::write(
            tmp.path().join("plugin.yaml"),
            r#"
schema: transist.plugin.v1
id: shell
version: v1
adapter: process
entrypoint:
  type: process
  command: plugin.sh
  cwd: will_be_created_at_runtime
package:
  files:
    - plugin.sh
    - schemas/input.json
    - schemas/output.json
    - will_be_created_at_runtime/marker
actions:
  - name: run
    input_schema: schemas/input.json
    output_schema: schemas/output.json
capabilities:
  supports_cancel: false
  supports_health: true
  supports_artifact_hints: false
  supports_evidence_hints: false
"#,
        )
        .expect("manifest");
        let manifest = load_manifest_file(&tmp.path().join("plugin.yaml"))
            .expect("cwd created by selected file should pass");
        assert_eq!(manifest.id, "shell");
    }

    #[test]
    fn install_list_inspect_remove_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        let installed = install_plugin_in_home(tmp.path(), fixture.path()).expect("install");
        assert_eq!(installed.installed.id, "shell");

        let listed = list_plugins_in_home(tmp.path()).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "shell");

        let inspected = inspect_plugin_in_home(tmp.path(), "shell").expect("inspect");
        assert_eq!(inspected.len(), 1);
        assert_eq!(inspected[0].version, "v1");

        let (manifest, dir) = resolve_plugin_in_home(tmp.path(), "shell").expect("resolve");
        assert_eq!(manifest.id, "shell");
        assert!(dir.exists());

        remove_plugin_in_home(tmp.path(), "shell", "v1").expect("remove");
        assert!(
            list_plugins_in_home(tmp.path())
                .expect("list after remove")
                .is_empty()
        );
    }

    #[test]
    fn install_fails_when_required_plugin_prerequisite_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture_with_extra_top_level(
            fixture.path(),
            "prerequisites:
  paths:
    - path: missing-required.tool
      kind: file
      required: true",
        );

        let error = install_plugin_in_home(tmp.path(), fixture.path())
            .expect_err("required prerequisite should block install");
        assert!(error.to_string().contains("prerequisites failed"));
        assert!(error.to_string().contains("missing-required.tool"));
    }

    #[test]
    fn install_rejects_legacy_executable_prerequisite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture_with_extra_top_level(
            fixture.path(),
            "prerequisites:
  commands:
    - argv: [\"false\"]
      required: true",
        );

        let error = install_plugin_in_home(tmp.path(), fixture.path())
            .expect_err("legacy executable prerequisite should block install");
        assert!(
            error
                .to_string()
                .contains("legacy_executable_prerequisite_unsupported")
        );
    }

    #[test]
    fn install_plugin_file_copies_only_declared_payload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        fs::write(fixture.path().join(".transist.lock"), "ignored").expect("lock");
        fs::write(fixture.path().join("README.md"), "documentation").expect("documentation");

        let (installed, target_dir) = install_plugin_file_in_home_with_target(
            tmp.path(),
            &fixture.path().join("plugin.yaml"),
        )
        .expect("file install");
        assert_eq!(installed.installed.id, "shell");
        assert_eq!(installed.installed.version, "v1");
        assert!(installed.installed.dir.join("plugin.sh").exists());
        assert!(installed.installed.dir.join("schemas/input.json").exists());
        assert!(!target_dir.join(".transist.lock").exists());
        assert!(!target_dir.join("README.md").exists());
        assert_eq!(
            plugin_dir_sha256(fixture.path()).expect("source digest"),
            plugin_dir_sha256(&target_dir).expect("installed digest")
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_plugin_file_rejects_symlinks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        symlink(
            fixture.path().join("plugin.sh"),
            fixture.path().join("linked.sh"),
        )
        .expect("symlink");
        replace_package_files(
            fixture.path(),
            "    - plugin.sh\n    - linked.sh\n    - schemas/input.json\n    - schemas/output.json\n",
        );

        let error = install_plugin_file_in_home(tmp.path(), &fixture.path().join("plugin.yaml"))
            .expect_err("symlink should fail");
        assert!(format!("{error:#}").contains("symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn install_plugin_file_rejects_special_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        create_fifo(&fixture.path().join("plugin.fifo"));
        replace_package_files(
            fixture.path(),
            "    - plugin.sh\n    - plugin.fifo\n    - schemas/input.json\n    - schemas/output.json\n",
        );

        let error = install_plugin_file_in_home(tmp.path(), &fixture.path().join("plugin.yaml"))
            .expect_err("special file should fail");
        assert!(format!("{error:#}").contains("not a regular file"));
    }

    #[test]
    fn install_plugin_dir_remains_supported_for_compatibility() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        let installed =
            install_plugin_in_home(tmp.path(), fixture.path()).expect("directory install");
        assert_eq!(installed.installed.id, "shell");
    }

    #[test]
    fn install_plugin_file_rejects_non_plugin_yaml_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());
        let bad = fixture.path().join("not-plugin.yaml");
        fs::rename(fixture.path().join("plugin.yaml"), &bad).expect("rename");

        let error = install_plugin_file_in_home(tmp.path(), &bad)
            .expect_err("non-plugin.yaml name should fail");
        assert!(error.to_string().contains("plugin.yaml"));
    }

    #[test]
    fn install_plugin_file_blocks_duplicate_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        install_plugin_file_in_home(tmp.path(), &fixture.path().join("plugin.yaml"))
            .expect("first install");
        let error = install_plugin_file_in_home(tmp.path(), &fixture.path().join("plugin.yaml"))
            .expect_err("duplicate install should fail");
        assert!(error.to_string().contains("already installed"));
    }

    #[test]
    fn reinstall_same_version_with_different_bytes_is_rejected_after_removal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        install_plugin_file_in_home(tmp.path(), &fixture.path().join("plugin.yaml"))
            .expect("first install");
        remove_plugin_in_home(tmp.path(), "shell", "v1").expect("remove");

        fs::write(
            fixture.path().join("plugin.sh"),
            "#!/bin/sh\necho changed\n",
        )
        .expect("rewrite script");
        set_executable(&fixture.path().join("plugin.sh"));

        let error = install_plugin_file_in_home(tmp.path(), &fixture.path().join("plugin.yaml"))
            .expect_err("reinstall with different bytes should fail");
        assert!(
            error
                .to_string()
                .contains("already installed with different content")
        );
    }

    #[test]
    fn install_plugin_file_evaluates_prerequisites() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture_with_extra_top_level(
            fixture.path(),
            "prerequisites:
  paths:
    - path: missing-optional.tool
      kind: file
      required: false",
        );

        let installed =
            install_plugin_file_in_home(tmp.path(), &fixture.path().join("plugin.yaml"))
                .expect("optional prerequisites should not block install");
        assert_eq!(installed.prerequisites.warning_count, 1);
        assert_eq!(installed.prerequisites.blocker_count, 0);
    }

    #[test]
    fn install_reports_optional_plugin_prerequisite_warnings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture_with_extra_top_level(
            fixture.path(),
            "prerequisites:
  paths:
    - path: missing-optional.tool
      kind: file
      required: false",
        );

        let installed = install_plugin_in_home(tmp.path(), fixture.path())
            .expect("optional prerequisite warnings should not block install");
        assert_eq!(installed.prerequisites.status, PrerequisiteStatus::Warning);
        assert_eq!(installed.prerequisites.warning_count, 1);
        assert_eq!(installed.prerequisites.blocker_count, 0);
    }

    #[test]
    fn uninstall_removes_all_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = tempfile::tempdir().expect("fixture");
        write_fixture(fixture.path());

        install_plugin_in_home(tmp.path(), fixture.path()).expect("install");
        let versions = uninstall_plugin_in_home(tmp.path(), "shell").expect("uninstall");

        assert_eq!(versions, vec!["v1"]);
        assert!(
            list_plugins_in_home(tmp.path())
                .expect("list after uninstall")
                .is_empty()
        );
    }

    #[test]
    fn uninstall_errors_when_plugin_not_installed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let error = uninstall_plugin_in_home(tmp.path(), "nonexistent")
            .expect_err("should error when plugin is not installed");
        assert!(error.to_string().contains("nonexistent"));
        assert!(error.to_string().contains("not installed"));
    }

    #[test]
    fn legacy_manifest_parses_without_provider_kind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture(tmp.path());
        let manifest = load_manifest(tmp.path()).expect("manifest");
        assert_eq!(manifest.default_provider_kind, None);
        for action in &manifest.actions {
            assert_eq!(action.provider_kind, None);
            assert_eq!(
                action.effective_provider_kind(manifest.default_provider_kind),
                ProviderKind::External,
            );
        }
    }

    #[test]
    fn manifest_action_provider_kind_is_parsed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture_with_extra_top_level(tmp.path(), "default_provider_kind: local\n");
        // Rewrite actions to include provider_kind
        let manifest_path = tmp.path().join("plugin.yaml");
        let raw = fs::read_to_string(&manifest_path).expect("read");
        let raw = raw.replace(
            "    input_schema: schemas/input.json\n    output_schema: schemas/output.json",
            "    input_schema: schemas/input.json\n    output_schema: schemas/output.json\n    provider_kind: external",
        );
        fs::write(&manifest_path, raw).expect("write");
        let manifest = load_manifest(tmp.path()).expect("manifest");
        assert_eq!(manifest.default_provider_kind, Some(ProviderKind::Local));
        let action = &manifest.actions[0];
        assert_eq!(action.provider_kind, Some(ProviderKind::External));
        // Action-level overrides manifest-level default
        assert_eq!(
            action.effective_provider_kind(manifest.default_provider_kind),
            ProviderKind::External,
        );
        // Manifest-level effective_provider_kind_for_action respects override
        assert_eq!(
            manifest.effective_provider_kind_for_action("run"),
            Some(ProviderKind::External),
        );
    }

    #[test]
    fn manifest_default_provider_kind_applies_when_action_omits_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture_with_extra_top_level(tmp.path(), "default_provider_kind: local\n");
        let manifest = load_manifest(tmp.path()).expect("manifest");
        assert_eq!(manifest.default_provider_kind, Some(ProviderKind::Local));
        assert_eq!(
            manifest.effective_provider_kind_for_action("run"),
            Some(ProviderKind::Local),
        );
    }

    #[test]
    fn manifest_unknown_provider_kind_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture_with_extra_top_level(tmp.path(), "default_provider_kind: somewhere\n");
        assert!(load_manifest(tmp.path()).is_err());
    }
}
