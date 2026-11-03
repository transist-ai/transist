// SPDX-License-Identifier: MPL-2.0
use crate::paths::manifest_path;
use crate::validate::validate_manifest;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use transist_prerequisites::{PrerequisiteReport, PrerequisiteSet};

/// File name that every plugin manifest must use.
pub const PLUGIN_MANIFEST_FILE_NAME: &str = "plugin.yaml";

/// Stable schema identifier for `plugin.yaml` manifests.
pub const PLUGIN_MANIFEST_SCHEMA_V1: &str = "transist.plugin.v1";

/// Exact URI prefix for runtime-owned, invocation-private temporary directories.
pub const RUNTIME_TEMP_URI_PREFIX: &str = "runtime://temp";

/// Parses a runtime temporary-directory declaration.
///
/// Returns `Ok(None)` for literal environment values and `Ok(Some(path))` for
/// a valid declaration, where an empty path denotes the temporary root. Values
/// beginning with `runtime://temp` must be complete declarations;
/// malformed values are rejected rather than treated as literals.
pub fn runtime_temp_relative_path(value: &str) -> Result<Option<&str>> {
    let Some(suffix) = value.strip_prefix(RUNTIME_TEMP_URI_PREFIX) else {
        return Ok(None);
    };
    if suffix.is_empty() {
        return Ok(Some(""));
    }
    let Some(relative_path) = suffix.strip_prefix('/') else {
        bail!(
            "runtime temporary declaration '{value}' must be '{}' or '{}/<relative-path>'",
            RUNTIME_TEMP_URI_PREFIX,
            RUNTIME_TEMP_URI_PREFIX,
        );
    };
    if relative_path.is_empty() {
        bail!("runtime temporary declaration '{value}' must not end with '/'");
    }
    for segment in relative_path.split('/') {
        let valid = !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment.chars().enumerate().all(|(index, character)| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '.' | '_' | '-') && index > 0
            });
        if !valid {
            bail!(
                "runtime temporary declaration '{value}' contains an invalid path segment '{segment}'"
            );
        }
    }
    if relative_path
        .split('/')
        .next()
        .is_some_and(|part| part.eq_ignore_ascii_case("documents"))
    {
        bail!("runtime temporary declaration uses reserved managed-document namespace 'documents'");
    }
    Ok(Some(relative_path))
}

/// Supported plugin adapter kinds.
/// Adapter family declared by a plugin manifest.
///
/// Re-exported from `transist-protocol` so that authoring crates and runtime
/// crates share one canonical enum for adapter routing.
pub use transist_protocol::AdapterKind;

/// Re-exported from `transist-protocol` so that manifest authoring and runtime
/// enforcement share one canonical enum for declared provider locality.
pub use transist_protocol::ProviderKind;

/// Secret delivery binding declared by a plugin action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SecretBinding {
    /// Deliver the secret through an environment variable.
    Env {
        /// Environment variable name that receives the secret value.
        name: String,
    },
    /// Materialize the secret into a temporary file and pass the file path.
    TempFile {
        /// Environment variable name that receives the temporary file path.
        path_env: String,
    },
    /// Deliver the secret through an HTTP header.
    Header {
        /// Header name that receives the secret value.
        name: String,
    },
    /// Deliver the secret through a request body field.
    BodyField {
        /// Body field name that receives the secret value.
        name: String,
    },
    /// Deliver the secret through a command-line argument flag.
    Arg {
        /// Flag name that precedes or identifies the secret value.
        flag: String,
    },
}

/// One named secret input consumed by an action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretInput {
    /// Stable secret input name referenced by workflows.
    pub name: String,
    /// Whether the action refuses execution when the secret is absent.
    pub required: bool,
    /// Delivery strategy used to materialize the secret for the adapter.
    pub binding: SecretBinding,
}

/// Action metadata exposed by one installed plugin version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionDescriptor {
    /// Action id referenced by workflow invoke steps.
    pub name: String,
    /// Relative path to the input schema file.
    pub input_schema: String,
    /// Relative path to the output schema file.
    pub output_schema: String,
    /// Secrets that must or may be supplied for the action.
    #[serde(default)]
    pub secret_inputs: Vec<SecretInput>,
    /// Extension bindings used by higher-level compiler or runtime features.
    #[serde(default)]
    pub bindings: BTreeMap<String, Value>,
    /// Action-level host prerequisites evaluated before dispatch.
    #[serde(default, skip_serializing_if = "PrerequisiteSet::is_empty")]
    pub prerequisites: PrerequisiteSet,
    /// Declared provider locality for this action. When absent, the
    /// manifest-level [`PluginManifest::default_provider_kind`] applies;
    /// when both are absent the effective value is [`ProviderKind::External`]
    /// (fail-safe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<ProviderKind>,
}

/// Binding that enables first-class prompt fields for an action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActionPromptBinding {
    /// Input field that receives inline prompt text.
    pub inline_field: String,
    /// Optional input field that receives a prompt file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_field: Option<String>,
}

/// Binding that enables first-class skills fields for an action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActionSkillsBinding {
    /// Input field that receives the structured skills bundle.
    pub bundle_field: String,
    /// Input field that receives rendered skill text.
    pub rendered_field: String,
}

/// Binding that enables runtime injection of one immutable manual-gate decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActionGateDecisionBinding {
    /// Input field that receives the `transist.gate_decision.v1` decision record.
    pub decision_field: String,
}

/// Capability flags reported by the plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCapabilities {
    /// Whether the adapter supports cancellation for in-flight work.
    pub supports_cancel: bool,
    /// Whether the adapter exposes a health probe.
    pub supports_health: bool,
    /// Whether action results may emit artifact hints.
    pub supports_artifact_hints: bool,
    /// Whether action results may emit evidence hints.
    pub supports_evidence_hints: bool,
}

/// Concrete adapter entrypoint configuration for one plugin version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginEntrypoint {
    /// Local process entrypoint configuration.
    Process {
        /// Executable or script path to launch.
        command: String,
        /// Additional arguments passed to the executable.
        #[serde(default)]
        args: Vec<String>,
        /// Legacy package-relative working directory for workflows without a
        /// declared workspace. Declared-workspace execution allows only `.`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Additional environment variables for the child process.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// HTTP entrypoint configuration.
    Http {
        /// Base URL for requests routed to the plugin.
        base_url: String,
        /// Optional CA certificate path for custom trust roots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ca_cert: Option<String>,
    },
    /// MCP command entrypoint configuration.
    Mcp {
        /// Executable or script path to launch.
        command: String,
        /// Additional arguments passed to the MCP process.
        #[serde(default)]
        args: Vec<String>,
        /// Legacy package-relative working directory for workflows without a
        /// declared workspace. Declared-workspace execution allows only `.`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Additional environment variables for the child process.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
}

/// Explicit executable payload selected for a plugin package.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginPackage {
    /// Exact package-relative regular-file paths included in the payload.
    pub files: Vec<String>,
}

/// Parsed `plugin.yaml` document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifest {
    /// Manifest schema version string.
    pub schema: String,
    /// Stable plugin package id.
    pub id: String,
    /// Plugin version string.
    pub version: String,
    /// Adapter family implemented by this plugin version.
    pub adapter: AdapterKind,
    /// Entrypoint configuration for reaching the adapter.
    pub entrypoint: PluginEntrypoint,
    /// Explicit package payload selected for installation and signing.
    pub package: PluginPackage,
    /// Plugin-level host prerequisites evaluated during install and preflight.
    #[serde(default, skip_serializing_if = "PrerequisiteSet::is_empty")]
    pub prerequisites: PrerequisiteSet,
    /// Actions exposed by this plugin version.
    pub actions: Vec<ActionDescriptor>,
    /// Capability flags advertised to callers.
    pub capabilities: PluginCapabilities,
    /// Manifest-level fallback provider locality applied to actions that omit
    /// `provider_kind`. When both this and the action-level field are absent,
    /// the effective value is [`ProviderKind::External`] (fail-safe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider_kind: Option<ProviderKind>,
}

/// One installed plugin directory under `.transist/plugins`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPlugin {
    /// Stable plugin package id.
    pub id: String,
    /// Installed plugin version.
    pub version: String,
    /// Absolute directory path for the installed plugin version.
    pub dir: PathBuf,
}

/// Result returned after a successful plugins install.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginInstallReport {
    /// Installed plugin descriptor.
    pub installed: InstalledPlugin,
    /// Plugin-level prerequisite evaluation recorded during install.
    pub prerequisites: PrerequisiteReport,
}

impl ActionDescriptor {
    /// Checks this action's optional input projection binding without changing data.
    ///
    /// Returns false when absent and true only for the exact string
    /// `action_data_v1`. Malformed or unknown values fail closed; no trimming or
    /// schema inference is performed. This method has no side effects.
    pub fn action_data_projection_enabled(&self) -> Result<bool> {
        match self.bindings.get("input_projection") {
            None => Ok(false),
            Some(Value::String(value)) if value == "action_data_v1" => Ok(true),
            Some(_) => bail!(
                "invalid input_projection binding for action '{}': expected string 'action_data_v1'",
                self.name
            ),
        }
    }

    /// Returns the effective provider kind for this action, falling back to
    /// the manifest-level default, then to [`ProviderKind::External`] (fail-safe).
    pub fn effective_provider_kind(&self, default: Option<ProviderKind>) -> ProviderKind {
        self.provider_kind
            .or(default)
            .unwrap_or(ProviderKind::External)
    }

    /// Parses the action's prompt binding, when one exists.
    pub fn prompt_binding(&self) -> Result<Option<ActionPromptBinding>> {
        let Some(binding) = self.bindings.get("transist_prompt_v1") else {
            return Ok(None);
        };
        let parsed: ActionPromptBinding =
            serde_json::from_value(binding.clone()).with_context(|| {
                format!(
                    "invalid transist_prompt_v1 binding for action '{}'",
                    self.name
                )
            })?;
        if parsed.inline_field.trim().is_empty() {
            bail!(
                "invalid transist_prompt_v1 binding for action '{}': inline_field cannot be empty",
                self.name
            );
        }
        if let Some(file_field) = parsed.file_field.as_deref()
            && file_field.trim().is_empty()
        {
            bail!(
                "invalid transist_prompt_v1 binding for action '{}': file_field cannot be empty",
                self.name
            );
        }
        Ok(Some(parsed))
    }

    /// Parses the action's skills binding, when one exists.
    pub fn skills_binding(&self) -> Result<Option<ActionSkillsBinding>> {
        let Some(binding) = self.bindings.get("transist_skills_v1") else {
            return Ok(None);
        };
        let parsed: ActionSkillsBinding =
            serde_json::from_value(binding.clone()).with_context(|| {
                format!(
                    "invalid transist_skills_v1 binding for action '{}'",
                    self.name
                )
            })?;
        if parsed.bundle_field.trim().is_empty() {
            bail!(
                "invalid transist_skills_v1 binding for action '{}': bundle_field cannot be empty",
                self.name
            );
        }
        if parsed.rendered_field.trim().is_empty() {
            bail!(
                "invalid transist_skills_v1 binding for action '{}': rendered_field cannot be empty",
                self.name
            );
        }
        if parsed.bundle_field == parsed.rendered_field {
            bail!(
                "invalid transist_skills_v1 binding for action '{}': bundle_field and rendered_field must differ",
                self.name
            );
        }
        Ok(Some(parsed))
    }

    /// Parses the action's manual-gate decision binding, when one exists.
    pub fn gate_decision_binding(&self) -> Result<Option<ActionGateDecisionBinding>> {
        let Some(binding) = self.bindings.get("transist_gate_decision_v1") else {
            return Ok(None);
        };
        let parsed: ActionGateDecisionBinding = serde_json::from_value(binding.clone())
            .with_context(|| {
                format!(
                    "invalid transist_gate_decision_v1 binding for action '{}'",
                    self.name
                )
            })?;
        if parsed.decision_field.trim().is_empty() {
            bail!(
                "invalid transist_gate_decision_v1 binding for action '{}': decision_field cannot be empty",
                self.name
            );
        }
        if parsed.decision_field.contains("{{") || parsed.decision_field.contains("}}") {
            bail!(
                "invalid transist_gate_decision_v1 binding for action '{}': decision_field must be a literal field name",
                self.name
            );
        }
        Ok(Some(parsed))
    }
}

impl PluginManifest {
    /// Returns the effective provider kind for the named action, falling back
    /// to [`PluginManifest::default_provider_kind`], then to
    /// [`ProviderKind::External`] (fail-safe). Returns `None` when the named
    /// action is not found in this manifest.
    pub fn effective_provider_kind_for_action(&self, action_name: &str) -> Option<ProviderKind> {
        self.actions
            .iter()
            .find(|action| action.name == action_name)
            .map(|action| action.effective_provider_kind(self.default_provider_kind))
    }
}

/// Loads and validates a plugin manifest from an explicit manifest file.
///
/// The file must be named `plugin.yaml`. The plugin directory is derived from
/// the manifest file's parent directory and used to validate relative schema
/// and entrypoint references.
///
/// This is the canonical manifest-loading entrypoint and mirrors
/// `workflows install --path path/to/workflow.yaml --approve` semantics. Prefer it over
/// [`load_manifest`] whenever the caller already has a manifest file path.
pub fn load_manifest_file(manifest_file: &Path) -> Result<PluginManifest> {
    crate::record_validation_work(crate::ValidationWork::PackageLoad);
    if manifest_file.file_name() != Some(std::ffi::OsStr::new(PLUGIN_MANIFEST_FILE_NAME)) {
        bail!(
            "plugin manifest must be named '{}'; got '{}'",
            PLUGIN_MANIFEST_FILE_NAME,
            manifest_file.display()
        );
    }
    let plugin_dir = manifest_file
        .parent()
        .with_context(|| format!("manifest path has no parent: {}", manifest_file.display()))?
        .to_path_buf();
    let raw = fs::read_to_string(manifest_file).with_context(|| {
        format!(
            "failed reading plugin manifest: {}",
            manifest_file.display()
        )
    })?;
    let manifest: PluginManifest = yaml_serde::from_str(&raw)
        .with_context(|| format!("invalid plugin manifest: {}", manifest_file.display()))?;
    validate_manifest(&plugin_dir, &manifest)?;
    Ok(manifest)
}

/// Loads and validates a plugin manifest from `plugin_dir`.
///
/// Directory-based compatibility wrapper around [`load_manifest_file`]. New
/// callers should prefer [`load_manifest_file`] with an explicit `plugin.yaml`
/// path so that install semantics match `workflows install --path workflow.yaml --approve`.
pub fn load_manifest(plugin_dir: &Path) -> Result<PluginManifest> {
    load_manifest_file(&manifest_path(plugin_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_temp_accepts_root_and_valid_child_paths() {
        assert_eq!(
            runtime_temp_relative_path("literal").expect("literal"),
            None
        );
        assert_eq!(
            runtime_temp_relative_path("runtime://temp").expect("root"),
            Some("")
        );
        assert_eq!(
            runtime_temp_relative_path("runtime://temp/copilot/session-state")
                .expect("nested child"),
            Some("copilot/session-state")
        );
    }

    #[test]
    fn runtime_temp_rejects_malformed_declarations() {
        for value in [
            "runtime://temp/",
            "runtime://temp//child",
            "runtime://temp/.",
            "runtime://temp/..",
            "runtime://temp/child/..",
            "runtime://temp/child\\nested",
            "runtime://temp/child?query",
            "runtime://temp/child#fragment",
            "runtime://temp/child%2Fencoded",
            "runtime://temporary",
            "runtime://temp-child",
        ] {
            assert!(runtime_temp_relative_path(value).is_err(), "{value}");
        }
    }

    #[test]
    fn gate_decision_binding_parses_its_destination_field() {
        let action = ActionDescriptor {
            name: "execute".to_string(),
            input_schema: "schemas/input.json".to_string(),
            output_schema: "schemas/output.json".to_string(),
            secret_inputs: Vec::new(),
            bindings: BTreeMap::from([(
                "transist_gate_decision_v1".to_string(),
                serde_json::json!({"decision_field": "gate_decision"}),
            )]),
            prerequisites: PrerequisiteSet::default(),
            provider_kind: None,
        };

        assert_eq!(
            action
                .gate_decision_binding()
                .expect("binding parses")
                .expect("binding exists")
                .decision_field,
            "gate_decision"
        );
    }

    #[test]
    fn gate_decision_binding_rejects_templated_destination_field() {
        let action = ActionDescriptor {
            name: "execute".to_string(),
            input_schema: "schemas/input.json".to_string(),
            output_schema: "schemas/output.json".to_string(),
            secret_inputs: Vec::new(),
            bindings: BTreeMap::from([(
                "transist_gate_decision_v1".to_string(),
                serde_json::json!({"decision_field": "{{inputs.destination}}"}),
            )]),
            prerequisites: PrerequisiteSet::default(),
            provider_kind: None,
        };

        let error = action
            .gate_decision_binding()
            .expect_err("templated destination must fail");
        assert!(error.to_string().contains("must be a literal field name"));
    }
}
