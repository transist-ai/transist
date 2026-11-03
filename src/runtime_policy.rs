// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use transist_core::PolicyPort;
use transist_core::{RemoteSecretPolicy, RuntimeOperatingMode, RuntimePosture};
use transist_policy::{
    AllowedAction, CapabilityGrantSet, ChildEnvPolicy, LocalAllowConfig, LocalAllowlistPolicy,
    PluginCapabilityGrant, StaticPolicy,
};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeConfig {
    // Retain the version marker emitted by `home init` while rejecting other
    // unknown top-level configuration fields.
    #[serde(default, rename = "version")]
    _version: Option<String>,
    #[serde(default)]
    runtime: RuntimeModeConfig,
    #[serde(default)]
    policy: RuntimePolicyConfig,
    #[serde(default, skip_serializing_if = "ManualGatesConfig::is_empty")]
    manual_gates: ManualGatesConfig,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManualGatesConfig {
    #[serde(default)]
    actor: Option<String>,
}

impl ManualGatesConfig {
    fn is_empty(&self) -> bool {
        self.actor.is_none()
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeModeConfig {
    /// The authored operating mode. `None` means the `runtime.mode` key was
    /// omitted entirely; this is a configuration error everywhere except the
    /// `home init` writer, which always writes an explicit value.
    #[serde(default)]
    mode: Option<RuntimeOperatingMode>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimePolicyConfig {
    /// The authored policy profile. `None` (key omitted) is a configuration
    /// error; an explicit empty string is also rejected — both previously
    /// resolved silently to `permissive`.
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    remote_secret_delivery: RemoteSecretDeliveryConfig,
    #[serde(default)]
    allow: LocalAllowConfigFile,
    #[serde(default)]
    grant_sets: std::collections::BTreeMap<String, CapabilityGrantSetFile>,
    #[serde(default)]
    workflow_grants: Vec<WorkflowGrantFile>,
    #[serde(default)]
    child_env: ChildEnvConfigFile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sources: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteSecretDeliveryConfig {
    #[serde(default)]
    process: Option<bool>,
    #[serde(default)]
    http: Option<bool>,
    #[serde(default)]
    mcp: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalAllowConfigFile {
    #[serde(default)]
    workflows: Option<Vec<String>>,
    #[serde(default)]
    adapters: Option<Vec<String>>,
    #[serde(default)]
    plugins: Option<Vec<String>>,
    #[serde(default)]
    actions: Option<Vec<AllowedActionFile>>,
    #[serde(default)]
    secret_refs: Option<Vec<String>>,
    #[serde(default)]
    capabilities: Option<Vec<transist_protocol::CapabilityRequest>>,
    #[serde(default)]
    plugin_grants: Option<Vec<PluginCapabilityGrantFile>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginCapabilityGrantFile {
    plugin: String,
    actions: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilityGrantSetFile {
    #[serde(default)]
    capabilities: Vec<transist_protocol::CapabilityRequest>,
    #[serde(default)]
    plugin_grants: Vec<PluginCapabilityGrantFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowGrantFile {
    workflow: String,
    #[serde(rename = "use")]
    grant_sets: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AllowedActionFile {
    plugin: String,
    action: String,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ChildEnvConfigFile {
    #[serde(default)]
    inherit: bool,
    #[serde(default)]
    allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFragment {
    schema: String,
    #[serde(default)]
    grant_sets: std::collections::BTreeMap<String, CapabilityGrantSetFile>,
    #[serde(default)]
    workflow_grants: Vec<WorkflowGrantFile>,
}

/// Read-only capability grants configured for the effective runtime policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityPolicySnapshot {
    pub profile: String,
    pub enforcement: String,
    pub allow: LocalAllowConfig,
}

/// Loads runtime policy from the required `<home>/config.yaml`.
pub(crate) fn load_policy(home_dir: &Path) -> Result<Arc<dyn PolicyPort>> {
    let config = load_runtime_config(home_dir)?;
    let posture = posture_from_config(&config)?;
    let base = match posture.policy_profile.as_str() {
        "deny_all" => StaticPolicy::deny_all(),
        "permissive" => StaticPolicy::permissive(),
        "allowlist" => StaticPolicy::permissive(),
        profile => anyhow::bail!("unsupported policy profile '{}'", profile),
    };
    let defaults = if posture.operating_mode == RuntimeOperatingMode::Governed {
        RemoteSecretPolicy {
            process: false,
            http: false,
            mcp: false,
        }
    } else {
        RemoteSecretPolicy::default()
    };
    let base = base.with_remote_secret_policy(RemoteSecretPolicy {
        process: config
            .policy
            .remote_secret_delivery
            .process
            .unwrap_or(defaults.process),
        http: config
            .policy
            .remote_secret_delivery
            .http
            .unwrap_or(defaults.http),
        mcp: config
            .policy
            .remote_secret_delivery
            .mcp
            .unwrap_or(defaults.mcp),
    });
    let enforce_capabilities = posture.policy_profile == "allowlist";
    let RuntimePolicyConfig {
        allow,
        grant_sets,
        workflow_grants,
        child_env,
        ..
    } = config.policy;
    let capability_grants = allow
        .capabilities
        .clone()
        .map(|values| values.into_iter().collect());
    let allow = materialize_allow_config(RuntimePolicyConfig {
        allow,
        grant_sets,
        workflow_grants,
        ..RuntimePolicyConfig::default()
    })?;
    Ok(Arc::new(
        LocalAllowlistPolicy::new(
            Arc::new(base),
            allow,
            materialize_child_env_policy(child_env),
        )
        .with_capabilities(capability_grants, enforce_capabilities),
    ))
}

/// Applies the trust and local-only wrappers used by live runtime composition.
pub(crate) fn wrap_policy_for_modes(
    home_dir: &Path,
    mut policy: Arc<dyn PolicyPort>,
    trust_decision: Option<transist_core::TrustDecisionRecord>,
    local_only: bool,
) -> Result<Arc<dyn PolicyPort>> {
    let posture = load_posture(home_dir)?;
    if matches!(posture.operating_mode, RuntimeOperatingMode::Governed) && trust_decision.is_none()
    {
        policy = Arc::new(StaticPolicy::deny_all());
    } else if trust_decision.is_some() {
        policy = Arc::new(transist_policy::TrustedExecutionPolicy::new(
            policy,
            trust_decision,
        ));
    }
    if local_only {
        policy = Arc::new(transist_policy::LocalOnlyPolicy::new(policy));
    }
    Ok(policy)
}

/// Loads and validates the effective runtime posture for a home.
pub(crate) fn load_posture(home_dir: &Path) -> Result<RuntimePosture> {
    posture_from_config(&load_runtime_config(home_dir)?)
}

/// Returns the fully composed runtime configuration without source directives.
pub(crate) fn effective_runtime_config_yaml(home_dir: &Path) -> Result<String> {
    let mut config = load_runtime_config(home_dir)?;
    config.policy.sources.clear();
    Ok(yaml_serde::to_string(&config)?)
}

/// Loads the exact local capability grants without constructing a policy port.
/// Inspection commands use this to explain governed allowlist denials.
pub(crate) fn load_capability_policy_snapshot(home_dir: &Path) -> Result<CapabilityPolicySnapshot> {
    let config = load_runtime_config(home_dir)?;
    let posture = posture_from_config(&config)?;
    let allow = materialize_allow_config(config.policy)?;
    Ok(CapabilityPolicySnapshot {
        profile: posture.policy_profile,
        enforcement: posture.capability_enforcement,
        allow,
    })
}

/// Returns whether trusted workflow and plugin verification is required.
pub(crate) fn trusted_execution_enabled(home_dir: &Path, requested: bool) -> Result<bool> {
    Ok(requested || load_posture(home_dir)?.operating_mode == RuntimeOperatingMode::Governed)
}

fn posture_from_config(config: &RuntimeConfig) -> Result<RuntimePosture> {
    let mode = config.runtime.mode.ok_or_else(|| {
        anyhow::anyhow!(
            "missing 'runtime.mode' in config.yaml; an explicit mode (governed, standalone, or development) is required"
        )
    })?;
    let profile = config
        .policy
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing or empty 'policy.profile' in config.yaml; an explicit profile (allowlist, permissive, or deny_all) is required"
            )
        })?;
    if mode == RuntimeOperatingMode::Governed && profile != "allowlist" {
        anyhow::bail!(
            "governed runtime requires policy.profile: allowlist; permissive policy is only valid in standalone or development mode"
        );
    }
    if !matches!(profile, "allowlist" | "permissive" | "deny_all") {
        anyhow::bail!("unsupported policy profile '{}'", profile);
    }
    Ok(RuntimePosture {
        operating_mode: mode,
        policy_profile: profile.to_string(),
        capability_enforcement: if profile == "allowlist" {
            "enforced".to_string()
        } else {
            "bypassed".to_string()
        },
    })
}

fn load_runtime_config(home_dir: &Path) -> Result<RuntimeConfig> {
    let path = home_dir.join("config.yaml");
    if !path.exists() {
        anyhow::bail!(
            "missing Transist home configuration: {}; run `transist home init` before using this home",
            path.display()
        );
    }
    let raw = fs::read_to_string(&path)?;
    let mut config: RuntimeConfig = yaml_serde::from_str(&raw)?;
    let policy_root = home_dir.join("policy");
    let mut loaded = std::collections::BTreeSet::new();
    for source in config.policy.sources.clone() {
        for fragment_path in resolve_policy_source(&policy_root, &source)? {
            if !loaded.insert(fragment_path.clone()) {
                anyhow::bail!(
                    "policy source '{}' is loaded more than once",
                    fragment_path.display()
                );
            }
            let raw = fs::read_to_string(&fragment_path).with_context(|| {
                format!(
                    "failed reading policy fragment '{}'",
                    fragment_path.display()
                )
            })?;
            let fragment: PolicyFragment = yaml_serde::from_str(&raw).with_context(|| {
                format!("invalid policy fragment '{}'", fragment_path.display())
            })?;
            if fragment.schema != "transist.policy_fragment.v1" {
                anyhow::bail!(
                    "unsupported policy fragment schema '{}' in '{}'",
                    fragment.schema,
                    fragment_path.display()
                );
            }
            if fragment.grant_sets.is_empty() && fragment.workflow_grants.is_empty() {
                anyhow::bail!(
                    "policy fragment '{}' must define grant_sets or workflow_grants",
                    fragment_path.display()
                );
            }
            for (name, grant_set) in fragment.grant_sets {
                if config
                    .policy
                    .grant_sets
                    .insert(name.clone(), grant_set)
                    .is_some()
                {
                    anyhow::bail!("duplicate policy grant set '{}'", name);
                }
            }
            for binding in fragment.workflow_grants {
                if let Some(existing) = config
                    .policy
                    .workflow_grants
                    .iter_mut()
                    .find(|existing| existing.workflow == binding.workflow)
                {
                    let mut combined = existing
                        .grant_sets
                        .iter()
                        .chain(&binding.grant_sets)
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    existing.grant_sets = std::mem::take(&mut combined).into_iter().collect();
                } else {
                    config.policy.workflow_grants.push(binding);
                }
            }
        }
    }
    Ok(config)
}

fn resolve_policy_source(policy_root: &Path, source: &str) -> Result<Vec<std::path::PathBuf>> {
    let source_path = Path::new(source);
    if source.trim().is_empty()
        || source_path.is_absolute()
        || source_path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir
                    | std::path::Component::CurDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        anyhow::bail!(
            "policy source '{}' must be a non-empty relative path beneath <home>/policy",
            source
        );
    }
    let path = policy_root.join(source_path);
    reject_symlink_components(policy_root, source_path)?;
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("policy source '{}' does not exist", path.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("policy source '{}' must not be a symlink", path.display());
    }
    if metadata.is_file() {
        if path.extension().and_then(|value| value.to_str()) != Some("yaml") {
            anyhow::bail!("policy source '{}' must be a .yaml file", path.display());
        }
        return Ok(vec![path]);
    }
    if !metadata.is_dir() {
        anyhow::bail!(
            "policy source '{}' must be a regular .yaml file or directory",
            path.display()
        );
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.extension().and_then(|value| value.to_str()) != Some("yaml") {
            continue;
        }
        let metadata = fs::symlink_metadata(&entry_path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            anyhow::bail!(
                "policy fragment '{}' must be a regular file",
                entry_path.display()
            );
        }
        files.push(entry_path);
    }
    files.sort();
    Ok(files)
}

fn reject_symlink_components(policy_root: &Path, source: &Path) -> Result<()> {
    let mut path = policy_root.to_path_buf();
    for component in source.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        path.push(component);
        if fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            anyhow::bail!("policy source path '{}' contains a symlink", path.display());
        }
    }
    Ok(())
}

fn materialize_allow_config(config: RuntimePolicyConfig) -> Result<LocalAllowConfig> {
    let LocalAllowConfigFile {
        workflows,
        adapters,
        plugins,
        actions,
        secret_refs,
        capabilities,
        plugin_grants,
    } = config.allow;
    let plugin_grants = plugin_grants
        .map(|grants| {
            grants
                .into_iter()
                .map(|grant| PluginCapabilityGrant::new(grant.plugin, grant.actions))
                .collect::<Result<BTreeSet<_>>>()
        })
        .transpose()?;
    let grant_sets = config
        .grant_sets
        .into_iter()
        .map(|(name, grant_set)| {
            if name.trim().is_empty() {
                anyhow::bail!("grant set name cannot be empty");
            }
            let plugin_grants = grant_set
                .plugin_grants
                .into_iter()
                .map(|grant| PluginCapabilityGrant::new(grant.plugin, grant.actions))
                .collect::<Result<BTreeSet<_>>>()?;
            Ok((
                name,
                CapabilityGrantSet {
                    capabilities: grant_set.capabilities.into_iter().collect(),
                    plugin_grants,
                },
            ))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
    let workflow_grants = config
        .workflow_grants
        .into_iter()
        .map(|binding| {
            let workflow_parts = binding.workflow.split('@').collect::<Vec<_>>();
            if workflow_parts.len() != 2
                || workflow_parts.iter().any(|part| part.trim().is_empty())
                || binding.workflow.chars().any(char::is_whitespace)
                || binding.grant_sets.is_empty()
            {
                anyhow::bail!(
                    "workflow grants require an exact workflow@version and at least one grant set"
                );
            }
            let grant_set_count = binding.grant_sets.len();
            let selected_grant_sets = binding.grant_sets.into_iter().collect::<BTreeSet<_>>();
            if selected_grant_sets.len() != grant_set_count {
                anyhow::bail!("workflow grants must not repeat grant set names");
            }
            if selected_grant_sets
                .iter()
                .any(|name| name.trim().is_empty())
                || !selected_grant_sets
                    .iter()
                    .all(|name| grant_sets.contains_key(name))
            {
                anyhow::bail!("workflow grants must reference named grant sets");
            }
            Ok((binding.workflow, selected_grant_sets))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
    Ok(LocalAllowConfig {
        workflows: materialize_string_set(workflows),
        adapters: materialize_string_set(adapters),
        plugins: materialize_string_set(plugins),
        actions: actions.map(|actions| {
            actions
                .into_iter()
                .map(|action| AllowedAction {
                    plugin: action.plugin,
                    action: action.action,
                })
                .collect()
        }),
        secret_refs: materialize_string_set(secret_refs),
        capabilities: capabilities.map(|values| values.into_iter().collect()),
        plugin_grants,
        grant_sets,
        workflow_grants,
        enforce_capabilities: false,
    })
}

fn materialize_child_env_policy(config: ChildEnvConfigFile) -> ChildEnvPolicy {
    let default = ChildEnvPolicy::default();
    ChildEnvPolicy {
        inherit: config.inherit,
        allowlist: config
            .allowlist
            .map(|values| values.into_iter().collect())
            .unwrap_or(default.allowlist),
    }
}

fn materialize_string_set(values: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    values.map(|values| values.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::KernelValue;

    fn write_policy_fragment(home: &Path, relative: &str, contents: &str) {
        let path = home.join("policy").join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("fragment parent");
        fs::write(path, contents).expect("fragment");
    }

    #[test]
    fn policy_sources_loads_sorted_fragments_and_forward_references() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("home");
        fs::write(home.join("config.yaml"), "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [workflows]\n").expect("config");
        write_policy_fragment(
            &home,
            "workflows/a.yaml",
            "schema: transist.policy_fragment.v1\nworkflow_grants:\n  - workflow: demo@1.0.0\n    use: [delivery]\n",
        );
        write_policy_fragment(
            &home,
            "workflows/b.yaml",
            "schema: transist.policy_fragment.v1\ngrant_sets:\n  delivery:\n    plugin_grants:\n      - plugin: shell@0.1.0\n        actions: [exec]\n",
        );
        let snapshot = load_capability_policy_snapshot(&home).expect("policy");
        assert!(snapshot.allow.workflow_grants.contains_key("demo@1.0.0"));
        assert!(snapshot.allow.grant_sets.contains_key("delivery"));
    }

    #[test]
    fn policy_sources_rejects_duplicate_grant_sets() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("home");
        fs::write(home.join("config.yaml"), "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [one.yaml, two.yaml]\n").expect("config");
        let fragment = "schema: transist.policy_fragment.v1\ngrant_sets:\n  delivery: {}\n";
        write_policy_fragment(&home, "one.yaml", fragment);
        write_policy_fragment(&home, "two.yaml", fragment);
        let error = match load_policy(&home) {
            Ok(_) => panic!("duplicate must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("duplicate policy grant set"));
    }

    #[test]
    fn policy_sources_union_distinct_grants_for_one_workflow() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("home");
        fs::write(home.join("config.yaml"), "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [one.yaml, two.yaml]\n").expect("config");
        write_policy_fragment(
            &home,
            "one.yaml",
            "schema: transist.policy_fragment.v1\ngrant_sets:\n  first: {}\nworkflow_grants:\n  - workflow: demo@1.0.0\n    use: [first]\n",
        );
        write_policy_fragment(
            &home,
            "two.yaml",
            "schema: transist.policy_fragment.v1\ngrant_sets:\n  second: {}\nworkflow_grants:\n  - workflow: demo@1.0.0\n    use: [second]\n",
        );

        let snapshot = load_capability_policy_snapshot(&home).expect("policy");
        assert_eq!(
            snapshot.allow.workflow_grants["demo@1.0.0"],
            BTreeSet::from(["first".to_string(), "second".to_string()])
        );
    }

    #[test]
    fn policy_sources_rejects_parent_traversal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("home");
        fs::write(home.join("config.yaml"), "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [../outside.yaml]\n").expect("config");
        let error = match load_policy(&home) {
            Ok(_) => panic!("traversal must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("relative path"));
    }

    #[test]
    fn load_policy_reads_remote_secret_delivery_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(crate::repo_local_home_dir(tmp.path())).expect("transist dir");
        fs::write(
            crate::repo_local_home_dir(tmp.path()).join("config.yaml"),
            r#"version: v1
runtime:
  mode: standalone
policy:
  profile: permissive
  remote_secret_delivery:
    http: true
    mcp: true
"#,
        )
        .expect("config");
        let policy = load_policy(&crate::repo_local_home_dir(tmp.path())).expect("policy");
        let decision = policy
            .validate_step_execution("run", "step", &transist_protocol::KernelValue::Null)
            .expect("decision");
        assert!(decision.remote_secret_policy.http);
        assert!(decision.remote_secret_policy.mcp);
        assert!(decision.remote_secret_policy.process);
    }

    #[test]
    fn load_policy_rejects_blank_profile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(crate::repo_local_home_dir(tmp.path())).expect("transist dir");
        fs::write(
            crate::repo_local_home_dir(tmp.path()).join("config.yaml"),
            r#"version: v1
runtime:
  mode: standalone
policy:
  profile: ""
"#,
        )
        .expect("config");
        let error = match load_policy(&crate::repo_local_home_dir(tmp.path())) {
            Ok(_) => panic!("must be rejected"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("missing or empty 'policy.profile'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn load_policy_rejects_missing_profile_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(crate::repo_local_home_dir(tmp.path())).expect("transist dir");
        fs::write(
            crate::repo_local_home_dir(tmp.path()).join("config.yaml"),
            "version: v1\nruntime:\n  mode: standalone\n",
        )
        .expect("config");
        let error = match load_policy(&crate::repo_local_home_dir(tmp.path())) {
            Ok(_) => panic!("must be rejected"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("missing or empty 'policy.profile'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn load_posture_rejects_missing_mode_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(crate::repo_local_home_dir(tmp.path())).expect("transist dir");
        fs::write(
            crate::repo_local_home_dir(tmp.path()).join("config.yaml"),
            "version: v1\npolicy:\n  profile: permissive\n",
        )
        .expect("config");
        let error = load_posture(&crate::repo_local_home_dir(tmp.path())).unwrap_err();
        assert!(
            error.to_string().contains("missing 'runtime.mode'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn load_posture_accepts_explicit_standalone_permissive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(crate::repo_local_home_dir(tmp.path())).expect("transist dir");
        fs::write(
            crate::repo_local_home_dir(tmp.path()).join("config.yaml"),
            "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: permissive\n",
        )
        .expect("config");
        let posture = load_posture(&crate::repo_local_home_dir(tmp.path())).expect("posture");
        assert_eq!(posture.operating_mode, RuntimeOperatingMode::Standalone);
        assert_eq!(posture.policy_profile, "permissive");
    }

    #[test]
    fn load_posture_accepts_explicit_governed_allowlist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(crate::repo_local_home_dir(tmp.path())).expect("transist dir");
        fs::write(
            crate::repo_local_home_dir(tmp.path()).join("config.yaml"),
            "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n",
        )
        .expect("config");
        let posture = load_posture(&crate::repo_local_home_dir(tmp.path())).expect("posture");
        assert_eq!(posture.operating_mode, RuntimeOperatingMode::Governed);
        assert_eq!(posture.policy_profile, "allowlist");
    }

    #[test]
    fn missing_config_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let error = load_posture(&crate::repo_local_home_dir(tmp.path())).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("missing Transist home configuration")
        );
    }

    #[test]
    fn governed_permissive_config_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("home");
        fs::write(
            home.join("config.yaml"),
            "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: permissive\n",
        )
        .expect("config");
        let error = load_posture(&home).unwrap_err();
        assert!(error.to_string().contains("governed runtime requires"));
    }

    #[test]
    fn load_policy_reads_local_allow_and_child_env_settings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(crate::repo_local_home_dir(tmp.path())).expect("transist dir");
        fs::write(
            crate::repo_local_home_dir(tmp.path()).join("config.yaml"),
            r#"version: v1
runtime:
  mode: standalone
policy:
  profile: permissive
  allow:
    workflows: ["demo.workflow@v1"]
    adapters: ["process"]
    plugins: ["shell"]
    actions:
      - plugin: shell
        action: exec
    secret_refs: ["API_TOKEN"]
  child_env:
    inherit: false
    allowlist: ["PATH"]
"#,
        )
        .expect("config");
        let policy = load_policy(&crate::repo_local_home_dir(tmp.path())).expect("policy");
        let allowed = policy
            .validate_step_execution(
                "run",
                "step",
                &KernelValue::Object(std::collections::BTreeMap::from([
                    (
                        "workflow_ref".to_string(),
                        KernelValue::String("demo.workflow@v1".to_string()),
                    ),
                    (
                        "adapter_kind".to_string(),
                        KernelValue::String("process".to_string()),
                    ),
                    (
                        "plugin_id".to_string(),
                        KernelValue::String("shell".to_string()),
                    ),
                    (
                        "action".to_string(),
                        KernelValue::String("exec".to_string()),
                    ),
                    (
                        "secret_refs".to_string(),
                        KernelValue::Array(vec![KernelValue::String("API_TOKEN".to_string())]),
                    ),
                ])),
            )
            .expect("decision");
        assert!(allowed.allowed);

        let KernelValue::Object(payload) = allowed.payload else {
            panic!("expected policy payload");
        };
        let Some(KernelValue::Object(runtime_auth)) = payload.get("runtime_auth_context") else {
            panic!("missing runtime auth payload");
        };
        assert_eq!(runtime_auth.get("active"), Some(&KernelValue::Bool(true)));
        assert_eq!(
            runtime_auth.get("child_env_allowlist"),
            Some(&KernelValue::Array(vec![KernelValue::String(
                "PATH".to_string()
            )]))
        );

        let denied = policy
            .validate_step_execution(
                "run",
                "step",
                &KernelValue::Object(std::collections::BTreeMap::from([
                    (
                        "workflow_ref".to_string(),
                        KernelValue::String("demo.workflow@v1".to_string()),
                    ),
                    (
                        "adapter_kind".to_string(),
                        KernelValue::String("process".to_string()),
                    ),
                    (
                        "plugin_id".to_string(),
                        KernelValue::String("shell".to_string()),
                    ),
                    (
                        "action".to_string(),
                        KernelValue::String("exec".to_string()),
                    ),
                    (
                        "secret_refs".to_string(),
                        KernelValue::Array(vec![KernelValue::String("OTHER".to_string())]),
                    ),
                ])),
            )
            .expect("decision");
        assert!(!denied.allowed);
        assert_eq!(
            denied.reason_code.as_deref(),
            Some("local_allow_secret_ref_not_allowed")
        );
    }

    #[test]
    fn load_policy_reads_canonical_plugin_grants() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("transist dir");
        fs::write(
            home.join("config.yaml"),
            r#"version: v1
runtime:
  mode: governed
policy:
  profile: allowlist
  allow:
    plugin_grants:
      - plugin: SHELL@0.1.0
        actions: [" exec ", echo, exec]
    capabilities:
      - capability: secret
        resource: API_TOKEN
        operations: [read]
"#,
        )
        .expect("config");

        let policy = load_capability_policy_snapshot(&home).expect("policy");
        let debug = format!("{policy:?}");
        assert!(debug.contains("shell@0.1.0"));
        assert!(debug.contains("exec"));
        assert!(debug.contains("echo"));
    }

    #[test]
    fn load_policy_rejects_invalid_plugin_grants() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("transist dir");
        fs::write(
            home.join("config.yaml"),
            r#"version: v1
runtime:
  mode: governed
policy:
  profile: allowlist
  allow:
    plugin_grants:
      - plugin: shell
        actions: []
"#,
        )
        .expect("config");

        let error = match load_policy(&home) {
            Ok(_) => panic!("invalid grant must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("invalid plugin grant"));
    }

    #[test]
    fn load_policy_rejects_unknown_grant_set_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("home");
        fs::write(
            home.join("config.yaml"),
            "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  grant_sets:\n    delivery:\n      capabilites: []\n",
        )
        .expect("config");
        assert!(load_policy(&home).is_err());
    }

    #[test]
    fn load_policy_rejects_unknown_workflow_grant_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = crate::repo_local_home_dir(tmp.path());
        fs::create_dir_all(&home).expect("home");
        fs::write(
            home.join("config.yaml"),
            "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  workflow_grants:\n    - workflow: demo@v1\n      use: [missing]\n",
        )
        .expect("config");
        assert!(load_policy(&home).is_err());
    }
}
