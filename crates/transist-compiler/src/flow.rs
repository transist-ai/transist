// SPDX-License-Identifier: MPL-2.0
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use transist_prerequisites::PrerequisiteSet;
use transist_protocol::{FilesystemPolicy, NetworkPolicy, WorkspaceBinding};

/// Stable schema identifier for authored flows.
pub const FLOW_SCHEMA_V1: &str = "transist.flow.v1";
/// Stable schema identifier for compiled recipes.
pub const RECIPE_SCHEMA_V1: &str = "transist.recipe.v1";
/// Internal marker used until flow-level plugin declarations are applied.
pub(crate) const UNDECLARED_PLUGIN_MARKER: &str = "__transist_undeclared_plugin";
/// Reserved workflow version used to mark non-production test workflows.
pub const TEST_WORKFLOW_VERSION: &str = "v0";

/// Returns whether the authored workflow version is reserved for development-only test behavior.
pub fn is_reserved_test_workflow_version(version: &str) -> bool {
    version == TEST_WORKFLOW_VERSION
}

/// Returns whether the authored workflow version enables development-only test-workflow behavior.
pub fn is_test_workflow(version: &str) -> bool {
    cfg!(feature = "dev-version-bypass") && is_reserved_test_workflow_version(version)
}

/// Parsed `transist.flow.v1` document.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    /// Root-lexical managed documents; these declarations grant no host-path authority.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub documents: BTreeMap<String, transist_protocol::DocumentDeclaration>,
    /// Authored flow schema identifier.
    pub schema: String,
    /// Stable workflow id.
    pub id: String,
    /// Workflow version string.
    pub version: String,
    /// Exact plugin versions available to invocation and cleanup steps.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, String>,
    /// Workflow-level host prerequisites enforced before step dispatch.
    #[serde(default, skip_serializing_if = "PrerequisiteSet::is_empty")]
    pub prerequisites: PrerequisiteSet,
    /// Declared workflow inputs keyed by input name.
    #[serde(default)]
    pub inputs: BTreeMap<String, FlowInputSpec>,
    /// Immutable author-defined values that may be referenced from bindings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, yaml_serde::Value>,
    /// Named module exports evaluated against its successful current execution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, yaml_serde::Value>,
    /// Named artifact locations available to authored bindings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, FlowArtifactSpec>,
    /// Reusable invoke defaults bound to one exact action.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, FlowProfile>,
    /// Optional declaration of the execution workspace for fresh runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceBinding>,
    /// Optional workflow-level filesystem policy override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Optional workflow-level network policy override.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "network")]
    pub network_policy: Option<NetworkPolicy>,
    /// Ordered authored workflow steps.
    #[serde(default)]
    pub steps: Vec<FlowStep>,
    /// Optional authored cleanup steps executed after terminal outcomes.
    #[serde(default)]
    pub cleanup: Vec<FlowCleanupStep>,
    /// Source path used for relative file and subflow resolution.
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

/// One declared workflow input.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlowInputSpec {
    /// Kernel value type expected for the input.
    #[serde(rename = "type")]
    pub value_type: String,
    /// Whether callers must provide the input explicitly.
    #[serde(default)]
    pub required: bool,
    /// Optional default value used when the caller omits the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<yaml_serde::Value>,
    /// Optional human-oriented description retained for docs and inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One named artifact location declared by a workflow.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlowArtifactSpec {
    /// The lifecycle scope that owns this artifact location.
    pub scope: FlowArtifactScope,
    /// Literal or input-templated path supplied when the artifact is referenced.
    pub path: String,
}

/// Lifecycle scope for a declared artifact location.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowArtifactScope {
    /// The artifact is private to one workflow run.
    Run,
    /// The artifact is associated with the workflow repository.
    Repository,
}

/// Reusable defaults for steps invoking one exact action.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlowProfile {
    /// Exact action target using `<adapter>:<plugin>/<action>` syntax.
    pub uses: String,
    /// Default static input bindings supplied by this profile.
    #[serde(default)]
    pub with: BTreeMap<String, yaml_serde::Value>,
    /// Default inline prompt for profile users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Default prompt file for profile users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    /// Default skills for profile users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Default filesystem policy for profile users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Default network policy for profile users.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "network")]
    pub network_policy: Option<NetworkPolicy>,
}

/// One authored workflow step.
#[derive(Debug, Clone, Serialize)]
pub struct FlowStep {
    /// Stable step id.
    pub id: String,
    /// Adapter family for invoke steps, derived from the authored `uses` target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// Plugin id for invoke steps, derived from the authored `uses` target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// Action id for invoke steps, derived from the authored `uses` target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Runtime recipe execution kind for invoke steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_kind: Option<String>,
    /// Optional named profile supplying non-conflicting invoke defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Built-in control primitive for control steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// Nested subflow call for subflow steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subflow: Option<FlowSubflowCall>,
    /// Inline prompt text for prompt-aware actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Prompt file path for prompt-aware actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    /// Skill references for skills-aware actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Optional manual-gate decision source injected by the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_decision: Option<FlowGateDecisionBinding>,
    /// Exact plugin version declared by the workflow and materialized internally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    /// Static input bindings supplied to the step.
    #[serde(default)]
    pub with: BTreeMap<String, yaml_serde::Value>,
    /// Optional `if` block attached to the step.
    #[serde(default, rename = "if", skip_serializing_if = "Option::is_none")]
    pub if_block: Option<FlowIfBlock>,
    /// Optional loop block attached to the step.
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_block: Option<FlowLoopBlock>,
    /// Optional variable assignments evaluated after the step completes.
    ///
    /// Keys name the workflow-scoped variables to write; values may be
    /// literals (string/number/boolean/object/array), `{{inputs.*}}` references,
    /// or `{{steps.<prior-id>.out.*}}` references. Only valid on invoke steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<BTreeMap<String, yaml_serde::Value>>,
    /// Optional filesystem policy override for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Optional network policy override for this step.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "network")]
    pub network_policy: Option<NetworkPolicy>,
}

impl<'de> Deserialize<'de> for FlowStep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawFlowStep {
            id: String,
            #[serde(default)]
            uses: Option<String>,
            #[serde(default)]
            profile: Option<String>,
            #[serde(default)]
            control: Option<String>,
            #[serde(default)]
            prompt: Option<String>,
            #[serde(default)]
            prompt_file: Option<String>,
            #[serde(default)]
            skills: Option<Vec<String>>,
            #[serde(default)]
            gate_decision: Option<FlowGateDecisionBinding>,
            #[serde(default)]
            with: BTreeMap<String, yaml_serde::Value>,
            #[serde(default, rename = "if")]
            if_block: Option<FlowIfBlock>,
            #[serde(default, rename = "loop")]
            loop_block: Option<FlowLoopBlock>,
            #[serde(default)]
            set: Option<BTreeMap<String, yaml_serde::Value>>,
            #[serde(default)]
            filesystem_policy: Option<FilesystemPolicy>,
            #[serde(default, rename = "network")]
            network_policy: Option<NetworkPolicy>,
        }

        let raw = RawFlowStep::deserialize(deserializer)?;
        let uses_declared = raw.uses.is_some();
        let (adapter, plugin, action, execution_kind) = match raw.uses {
            Some(uses) => {
                let (adapter, plugin, action) =
                    parse_uses(&uses).map_err(serde::de::Error::custom)?;
                (
                    Some(adapter),
                    Some(plugin),
                    Some(action),
                    Some("external".to_string()),
                )
            }
            None => (None, None, None, None),
        };

        Ok(Self {
            id: raw.id,
            adapter,
            plugin,
            action,
            execution_kind,
            profile: raw.profile,
            control: raw.control,
            subflow: None,
            prompt: raw.prompt,
            prompt_file: raw.prompt_file,
            skills: raw.skills,
            gate_decision: raw.gate_decision,
            plugin_version: uses_declared.then(|| UNDECLARED_PLUGIN_MARKER.to_string()),
            with: raw.with,
            if_block: raw.if_block,
            loop_block: raw.loop_block,
            set: raw.set,
            filesystem_policy: raw.filesystem_policy,
            network_policy: raw.network_policy,
        })
    }
}

/// Authored reference to the manual gate whose latest decision is injected into an invoke step.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FlowGateDecisionBinding {
    /// Id of a manual-gate step in the same workflow or module.
    pub step: String,
}

/// One authored cleanup step.
#[derive(Debug, Clone, Serialize)]
pub struct FlowCleanupStep {
    /// Stable cleanup step id.
    pub id: String,
    /// Outcome triggers that activate this cleanup step.
    #[serde(default)]
    pub when: Vec<FlowCleanupWhen>,
    /// Adapter family for invoke cleanup steps, derived from `uses`.
    pub adapter: String,
    /// Plugin id for invoke cleanup steps, derived from `uses`.
    pub plugin: String,
    /// Action id for invoke cleanup steps, derived from `uses`.
    pub action: String,
    /// Runtime recipe execution kind for invoke cleanup steps.
    pub execution_kind: String,
    /// Exact plugin version declared by the workflow and materialized internally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    /// Inline prompt text for prompt-aware actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Prompt file path for prompt-aware actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    /// Skill references for skills-aware actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Static input bindings supplied to the cleanup step.
    #[serde(default)]
    pub with: BTreeMap<String, yaml_serde::Value>,
    /// Optional filesystem policy override for this cleanup step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy: Option<FilesystemPolicy>,
    /// Optional network policy override for this cleanup step.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "network")]
    pub network_policy: Option<NetworkPolicy>,
}

impl<'de> Deserialize<'de> for FlowCleanupStep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawFlowCleanupStep {
            id: String,
            #[serde(default)]
            when: Vec<FlowCleanupWhen>,
            #[serde(default)]
            uses: Option<String>,
            #[serde(default)]
            prompt: Option<String>,
            #[serde(default)]
            prompt_file: Option<String>,
            #[serde(default)]
            skills: Option<Vec<String>>,
            #[serde(default)]
            with: BTreeMap<String, yaml_serde::Value>,
            #[serde(default)]
            filesystem_policy: Option<FilesystemPolicy>,
            #[serde(default, rename = "network")]
            network_policy: Option<NetworkPolicy>,
        }

        let raw = RawFlowCleanupStep::deserialize(deserializer)?;
        let (adapter, plugin, action) = match raw.uses {
            Some(uses) => {
                let (adapter, plugin, action) =
                    parse_uses(&uses).map_err(serde::de::Error::custom)?;
                (adapter, plugin, action)
            }
            None => return Err(serde::de::Error::custom("cleanup step requires uses")),
        };

        Ok(Self {
            id: raw.id,
            when: raw.when,
            adapter,
            plugin,
            action,
            execution_kind: "external".to_string(),
            plugin_version: Some(UNDECLARED_PLUGIN_MARKER.to_string()),
            prompt: raw.prompt,
            prompt_file: raw.prompt_file,
            skills: raw.skills,
            with: raw.with,
            filesystem_policy: raw.filesystem_policy,
            network_policy: raw.network_policy,
        })
    }
}

fn parse_uses(value: &str) -> Result<(String, String, String), String> {
    if value.trim() != value {
        return Err("uses must not contain leading or trailing whitespace".to_string());
    }
    let (adapter, target) = value
        .split_once(':')
        .ok_or_else(|| "uses must use '<adapter>:<plugin>/<action>'".to_string())?;
    let (plugin, action) = target
        .split_once('/')
        .ok_or_else(|| "uses must use '<adapter>:<plugin>/<action>'".to_string())?;
    if adapter.is_empty()
        || plugin.is_empty()
        || action.is_empty()
        || adapter.trim() != adapter
        || plugin.trim() != plugin
        || action.trim() != action
        || target.matches('/').count() != 1
        || value.matches(':').count() != 1
    {
        return Err(
            "uses must use '<adapter>:<plugin>/<action>' with non-empty components".to_string(),
        );
    }
    Ok((adapter.to_string(), plugin.to_string(), action.to_string()))
}

impl Flow {
    pub(crate) fn with_declarations_applied(&self) -> Result<Self> {
        for (name, declaration) in &self.documents {
            declaration.validate(name).map_err(anyhow::Error::msg)?;
        }
        let mut outside = self.clone();
        for step in &mut outside.steps {
            if step.control.is_none() && step.subflow.is_none() {
                step.with.clear();
            }
        }
        for step in &mut outside.cleanup {
            step.with.clear();
        }
        if transist_protocol::contains_document_binding(&serde_json::to_value(outside)?) {
            bail!("$document bindings are allowed only inside invoke action data");
        }
        validate_context(&self.context, &self.inputs)?;
        validate_artifacts(&self.artifacts, &self.inputs)?;
        for (name, profile) in &self.profiles {
            validate_declaration_name("profiles", name)?;
            parse_uses(&profile.uses)
                .map_err(|error| anyhow::anyhow!("profile '{name}' has invalid uses: {error}"))?;
        }

        let mut normalized = self.clone();
        normalized.outputs = expand_value_map(&self.outputs, &self.context, &self.artifacts)?;
        for step in &mut normalized.steps {
            if let Some(profile_name) = step.profile.clone() {
                let profile = self.profiles.get(&profile_name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "flow step '{}' references unknown profile '{}'",
                        step.id,
                        profile_name
                    )
                })?;
                apply_profile(step, profile, &profile_name)?;
            }
            step.profile = None;
            expand_step_declarations(step, &self.context, &self.artifacts)?;
        }
        for cleanup in &mut normalized.cleanup {
            cleanup.with = expand_value_map(&cleanup.with, &self.context, &self.artifacts)?;
        }
        // Plugin versions are declared once at the workflow boundary and copied
        // into the compiler-only step model before recipe construction.
        for step in &mut normalized.steps {
            if let Some(version) = step
                .plugin
                .as_ref()
                .and_then(|plugin| self.plugins.get(plugin))
            {
                step.plugin_version = Some(version.clone());
            }
        }
        for cleanup in &mut normalized.cleanup {
            if let Some(version) = self.plugins.get(&cleanup.plugin) {
                cleanup.plugin_version = Some(version.clone());
            }
        }
        normalized.context.clear();
        normalized.artifacts.clear();
        normalized.profiles.clear();
        // Strip only invoke action data from a copy. Every other authored surface
        // (including module inputs, defaults, controls and policy) remains forbidden.
        let mut outside = normalized.clone();
        for step in &mut outside.steps {
            if step.control.is_none() && step.subflow.is_none() {
                step.with.clear();
            }
        }
        for step in &mut outside.cleanup {
            step.with.clear();
        }
        if transist_protocol::contains_document_binding(&serde_json::to_value(outside)?) {
            bail!("$document bindings are allowed only inside invoke action data");
        }
        Ok(normalized)
    }
}

fn validate_context(
    context: &BTreeMap<String, yaml_serde::Value>,
    inputs: &BTreeMap<String, FlowInputSpec>,
) -> Result<()> {
    for (name, value) in context {
        validate_declaration_name("context", name)?;
        validate_immutable_value(value, inputs, &format!("context.{name}"))?;
    }
    Ok(())
}

fn validate_artifacts(
    artifacts: &BTreeMap<String, FlowArtifactSpec>,
    inputs: &BTreeMap<String, FlowInputSpec>,
) -> Result<()> {
    for (name, artifact) in artifacts {
        validate_declaration_name("artifacts", name)?;
        validate_immutable_string(&artifact.path, inputs, &format!("artifacts.{name}.path"))?;
    }
    Ok(())
}

fn validate_declaration_name(kind: &str, name: &str) -> Result<()> {
    if name.is_empty()
        || name.trim() != name
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        bail!("flow {kind} key '{name}' must be a non-empty unpadded identifier");
    }
    Ok(())
}

fn validate_immutable_value(
    value: &yaml_serde::Value,
    inputs: &BTreeMap<String, FlowInputSpec>,
    path: &str,
) -> Result<()> {
    match value {
        yaml_serde::Value::String(value) => validate_immutable_string(value, inputs, path),
        yaml_serde::Value::Sequence(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_immutable_value(value, inputs, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        yaml_serde::Value::Mapping(values) => {
            for (key, value) in values {
                let key = key
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("{path} object keys must be strings"))?;
                validate_immutable_value(value, inputs, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
        yaml_serde::Value::Tagged(_) => bail!("{path} cannot use YAML tags"),
        _ => Ok(()),
    }
}

fn validate_immutable_string(
    value: &str,
    inputs: &BTreeMap<String, FlowInputSpec>,
    path: &str,
) -> Result<()> {
    let mut remaining = value;
    while let Some(start) = remaining.find("{{") {
        let after = &remaining[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| anyhow::anyhow!("{path} contains an unclosed template"))?;
        let template = after[..end].trim();
        let Some(input) = template
            .strip_prefix("inputs.")
            .filter(|name| !name.is_empty())
        else {
            bail!("{path} only permits literals and '{{{{inputs.<name>}}}}' templates");
        };
        if !inputs.contains_key(input) {
            bail!("{path} references unknown workflow input '{input}'");
        }
        remaining = &after[end + 2..];
    }
    Ok(())
}

fn apply_profile(step: &mut FlowStep, profile: &FlowProfile, profile_name: &str) -> Result<()> {
    let profile_uses = parse_uses(&profile.uses)
        .map_err(|error| anyhow::anyhow!("profile '{profile_name}' has invalid uses: {error}"))?;
    let step_uses = match (
        step.adapter.as_deref(),
        step.plugin.as_deref(),
        step.action.as_deref(),
        step.execution_kind.as_deref(),
    ) {
        (Some(adapter), Some(plugin), Some(action), Some("external")) => (adapter, plugin, action),
        _ => bail!(
            "flow step '{}' uses profile '{}' but must declare its own uses",
            step.id,
            profile_name
        ),
    };
    if profile_uses.0 != step_uses.0
        || profile_uses.1 != step_uses.1
        || profile_uses.2 != step_uses.2
    {
        bail!(
            "flow step '{}' uses '{}:{}/{}' but profile '{}' is bound to '{}'; profiles cannot change a step's uses",
            step.id,
            step_uses.0,
            step_uses.1,
            step_uses.2,
            profile_name,
            profile.uses,
        );
    }
    for (key, value) in &profile.with {
        if step.with.contains_key(key) {
            bail!(
                "flow step '{}' conflicts with profile '{}' on with.{}",
                step.id,
                profile_name,
                key
            );
        }
        step.with.insert(key.clone(), value.clone());
    }
    if (profile.prompt.is_some() || profile.prompt_file.is_some())
        && (step.prompt.is_some() || step.prompt_file.is_some())
    {
        bail!(
            "flow step '{}' conflicts with profile '{}' on prompt configuration",
            step.id,
            profile_name
        );
    }
    if profile.skills.is_some() && step.skills.is_some() {
        bail!(
            "flow step '{}' conflicts with profile '{}' on skills",
            step.id,
            profile_name
        );
    }
    if profile.filesystem_policy.is_some() && step.filesystem_policy.is_some() {
        bail!(
            "flow step '{}' conflicts with profile '{}' on filesystem_policy",
            step.id,
            profile_name
        );
    }
    if profile.network_policy.is_some() && step.network_policy.is_some() {
        bail!(
            "flow step '{}' conflicts with profile '{}' on network",
            step.id,
            profile_name
        );
    }
    step.prompt = step.prompt.take().or_else(|| profile.prompt.clone());
    step.prompt_file = step
        .prompt_file
        .take()
        .or_else(|| profile.prompt_file.clone());
    step.skills = step.skills.take().or_else(|| profile.skills.clone());
    step.filesystem_policy = step
        .filesystem_policy
        .take()
        .or_else(|| profile.filesystem_policy.clone());
    step.network_policy = step
        .network_policy
        .take()
        .or_else(|| profile.network_policy.clone());
    Ok(())
}

fn expand_step_declarations(
    step: &mut FlowStep,
    context: &BTreeMap<String, yaml_serde::Value>,
    artifacts: &BTreeMap<String, FlowArtifactSpec>,
) -> Result<()> {
    step.with = expand_value_map(&step.with, context, artifacts)?;
    if let Some(set) = step.set.as_ref() {
        step.set = Some(expand_value_map(set, context, artifacts)?);
    }
    if let Some(subflow) = step.subflow.as_mut() {
        subflow.inputs = expand_value_map(&subflow.inputs, context, artifacts)?;
    }
    if let Some(prompt) = step.prompt.as_ref() {
        step.prompt = Some(expand_string(prompt, context, artifacts)?);
    }
    if let Some(prompt_file) = step.prompt_file.as_ref() {
        step.prompt_file = Some(expand_string(prompt_file, context, artifacts)?);
    }
    Ok(())
}

fn expand_value_map(
    values: &BTreeMap<String, yaml_serde::Value>,
    context: &BTreeMap<String, yaml_serde::Value>,
    artifacts: &BTreeMap<String, FlowArtifactSpec>,
) -> Result<BTreeMap<String, yaml_serde::Value>> {
    values
        .iter()
        .map(|(key, value)| {
            expand_value(value, context, artifacts).map(|value| (key.clone(), value))
        })
        .collect()
}

fn expand_value(
    value: &yaml_serde::Value,
    context: &BTreeMap<String, yaml_serde::Value>,
    artifacts: &BTreeMap<String, FlowArtifactSpec>,
) -> Result<yaml_serde::Value> {
    match value {
        yaml_serde::Value::String(value) => {
            if let Some(expanded) = exact_declaration_reference(value, context, artifacts)? {
                return Ok(expanded);
            }
            Ok(yaml_serde::Value::String(expand_string(
                value, context, artifacts,
            )?))
        }
        yaml_serde::Value::Sequence(values) => values
            .iter()
            .map(|value| expand_value(value, context, artifacts))
            .collect::<Result<Vec<_>>>()
            .map(yaml_serde::Value::Sequence),
        yaml_serde::Value::Mapping(values) => {
            let mut expanded = yaml_serde::Mapping::new();
            for (key, value) in values {
                expanded.insert(key.clone(), expand_value(value, context, artifacts)?);
            }
            Ok(yaml_serde::Value::Mapping(expanded))
        }
        _ => Ok(value.clone()),
    }
}

fn exact_declaration_reference(
    value: &str,
    context: &BTreeMap<String, yaml_serde::Value>,
    artifacts: &BTreeMap<String, FlowArtifactSpec>,
) -> Result<Option<yaml_serde::Value>> {
    let Some(reference) = value
        .strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))
    else {
        return Ok(None);
    };
    let reference = reference.trim();
    if let Some(name) = reference.strip_prefix("context.") {
        return context
            .get(name)
            .cloned()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("references unknown context '{name}'"));
    }
    if let Some(name) = reference.strip_prefix("artifacts.") {
        return artifacts
            .get(name)
            .map(|artifact| Some(yaml_serde::Value::String(artifact.path.clone())))
            .ok_or_else(|| anyhow::anyhow!("references unknown artifact '{name}'"));
    }
    Ok(None)
}

fn expand_string(
    value: &str,
    context: &BTreeMap<String, yaml_serde::Value>,
    artifacts: &BTreeMap<String, FlowArtifactSpec>,
) -> Result<String> {
    let mut rendered = String::new();
    let mut remaining = value;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];
        let Some(end) = after.find("}}") else {
            rendered.push_str(&remaining[start..]);
            return Ok(rendered);
        };
        let reference = after[..end].trim();
        let replacement = if let Some(name) = reference.strip_prefix("context.") {
            context
                .get(name)
                .map(|value| declaration_scalar(value, "context", name))
        } else if let Some(name) = reference.strip_prefix("artifacts.") {
            artifacts
                .get(name)
                .map(|artifact| Ok(artifact.path.clone()))
        } else {
            None
        };
        match replacement {
            Some(replacement) => rendered.push_str(&replacement?),
            None if reference.starts_with("context.") || reference.starts_with("artifacts.") => {
                bail!("references unknown declaration '{{{{{reference}}}}}'")
            }
            None => rendered.push_str(&remaining[start..start + 2 + end + 2]),
        }
        remaining = &after[end + 2..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

fn declaration_scalar(value: &yaml_serde::Value, kind: &str, name: &str) -> Result<String> {
    match value {
        yaml_serde::Value::String(value) => Ok(value.clone()),
        yaml_serde::Value::Bool(value) => Ok(value.to_string()),
        yaml_serde::Value::Number(value) => Ok(value.to_string()),
        yaml_serde::Value::Null => Ok("null".to_string()),
        yaml_serde::Value::Sequence(_) | yaml_serde::Value::Mapping(_) => {
            bail!("{kind}.{name} is structured and can only be used as an exact template")
        }
        yaml_serde::Value::Tagged(_) => bail!("{kind}.{name} cannot use YAML tags"),
    }
}

/// Authored cleanup outcome selector.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FlowCleanupWhen {
    /// Run completed successfully.
    Success,
    /// Run was canceled.
    Canceled,
    /// Run failed and requires explicit cleanup.
    Failed,
}

impl FlowCleanupStep {
    /// Converts the cleanup step into an invoke-shaped flow step for shared validation and compilation helpers.
    pub fn as_flow_step(&self) -> FlowStep {
        FlowStep {
            id: self.id.clone(),
            adapter: Some(self.adapter.clone()),
            plugin: Some(self.plugin.clone()),
            action: Some(self.action.clone()),
            execution_kind: Some(self.execution_kind.clone()),
            profile: None,
            plugin_version: self.plugin_version.clone(),
            control: None,
            subflow: None,
            prompt: self.prompt.clone(),
            prompt_file: self.prompt_file.clone(),
            skills: self.skills.clone(),
            gate_decision: None,
            with: self.with.clone(),
            if_block: None,
            loop_block: None,
            set: None,
            filesystem_policy: self.filesystem_policy.clone(),
            network_policy: self.network_policy.clone(),
        }
    }
}

/// Subflow invocation descriptor attached to one step.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSubflowCall {
    /// Relative or absolute path to the child workflow file.
    pub path: String,
    /// Inputs passed into the child workflow.
    #[serde(default)]
    pub inputs: BTreeMap<String, yaml_serde::Value>,
    /// Optional rerun mode for completed child runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun_mode: Option<String>,
}

/// Authored `if` control block.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlowIfBlock {
    /// Guard expression controlling which branch is taken.
    pub guard: String,
    /// Step references executed when the guard evaluates truthy.
    #[serde(rename = "then")]
    pub then_branch: Vec<FlowRef>,
    /// Step references executed when the guard evaluates falsy.
    #[serde(default, rename = "else")]
    pub else_branch: Vec<FlowRef>,
}

/// Authored loop control block.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlowLoopBlock {
    /// Step references that form the loop body.
    #[serde(default)]
    pub body: Vec<FlowRef>,
    /// Optional continue condition that must remain true.
    #[serde(default, rename = "while", skip_serializing_if = "Option::is_none")]
    pub while_expr: Option<String>,
    /// Optional completion condition that ends the loop when true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// Maximum iteration count for the loop.
    pub max_iters: u32,
    /// Optional authored duration cap preserved for validation and compilation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<String>,
}

/// Reference to another authored step.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlowRef {
    /// Referenced step id.
    #[serde(rename = "ref")]
    pub ref_step: String,
}

#[cfg(test)]
mod tests {
    use super::{
        FLOW_SCHEMA_V1, Flow, TEST_WORKFLOW_VERSION, is_reserved_test_workflow_version,
        is_test_workflow,
    };

    #[test]
    fn detects_reserved_test_workflow_version() {
        assert!(is_reserved_test_workflow_version(TEST_WORKFLOW_VERSION));
        assert!(!is_reserved_test_workflow_version("1.0.0"));
        assert!(!is_reserved_test_workflow_version("test"));
    }

    #[test]
    fn test_workflow_behavior_is_feature_gated() {
        assert_eq!(
            is_test_workflow(TEST_WORKFLOW_VERSION),
            cfg!(feature = "dev-version-bypass")
        );
        assert!(!is_test_workflow("1.0.0"));
    }

    #[test]
    fn gate_decision_binding_deserializes_and_requires_plugin_aware_compilation() {
        let flow: Flow = yaml_serde::from_str(
            r#"
schema: transist.flow.v1
id: gate-decision-demo
version: 1.0.0
plugins:
  agent: 1.0.0
steps:
  - id: implement
    uses: process:agent/execute
    gate_decision:
      step: review
  - id: review
    control: control.gate.manual
    with:
      gate_id: implementation-review
"#,
        )
        .expect("flow parses");

        assert_eq!(flow.schema, FLOW_SCHEMA_V1);
        assert_eq!(
            flow.steps[0].gate_decision.as_ref().expect("binding").step,
            "review"
        );
        let error = crate::compile::compile_flow(&flow)
            .expect_err("generic compilation must not discard the binding");
        assert!(
            error
                .to_string()
                .contains("requires plugin-aware compilation in a concrete home")
        );
    }

    #[test]
    fn authored_gate_decision_compiler_metadata_is_rejected() {
        let flow: Flow = yaml_serde::from_str(
            r#"
schema: transist.flow.v1
id: reserved-gate-binding-input
version: 1.0.0
plugins:
  agent: 1.0.0
steps:
  - id: implement
    uses: process:agent/execute
    with:
      __transist_gate_decision:
        decision_field: gate_decision
        gate_step_id: review
        gate_id: review
"#,
        )
        .expect("flow parses");

        let error = crate::compile::compile_flow(&flow).expect_err("reserved input must fail");
        assert!(
            error
                .to_string()
                .contains("cannot define reserved input '__transist_gate_decision'"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn gate_decision_binding_rejects_ambiguous_effective_gate_id() {
        let flow: Flow = yaml_serde::from_str(
            r#"
schema: transist.flow.v1
id: ambiguous-gate-binding
version: 1.0.0
plugins:
  agent: 1.0.0
steps:
  - id: implement
    uses: process:agent/execute
    gate_decision:
      step: implementation_review
  - id: implementation_review
    control: control.gate.manual
    with:
      gate_id: review
  - id: documentation_review
    control: control.gate.manual
    with:
      gate_id: review
"#,
        )
        .expect("flow parses");

        let error = crate::compile::compile_flow(&flow).expect_err("ambiguous gate id must fail");
        assert!(
            error.to_string().contains(
                "gate_decision.step 'implementation_review' resolves to gate_id 'review' which is also used by manual gate step 'documentation_review'"
            ),
            "unexpected error: {error:#}"
        );
    }
}
