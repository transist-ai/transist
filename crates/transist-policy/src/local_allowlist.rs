// SPDX-License-Identifier: MPL-2.0
//! Local allowlist policy wrapper and child-process environment controls.

use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use transist_core::ports::{PolicyDecision, PolicyPort};
use transist_protocol::{CapabilityName, CapabilityRequest, KernelValue};

const LOCAL_POLICY_ID: &str = "local_runtime_policy";

/// Local allowlist configuration loaded from the runtime home config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalAllowConfig {
    /// Explicitly allowed workflow refs.
    pub workflows: Option<BTreeSet<String>>,
    /// Explicitly allowed adapter kinds.
    pub adapters: Option<BTreeSet<String>>,
    /// Explicitly allowed plugin ids.
    pub plugins: Option<BTreeSet<String>>,
    /// Explicitly allowed `plugin + action` pairs.
    pub actions: Option<BTreeSet<AllowedAction>>,
    /// Explicitly allowed logical secret refs.
    pub secret_refs: Option<BTreeSet<String>>,
    /// Exact capability grants used by the governed allowlist profile.
    pub capabilities: Option<BTreeSet<CapabilityRequest>>,
    /// Version-pinned plugin grants that compact structural invoke authority.
    pub plugin_grants: Option<BTreeSet<PluginCapabilityGrant>>,
    /// Named reusable capability grant sets.
    pub grant_sets: BTreeMap<String, CapabilityGrantSet>,
    /// Exact workflow references mapped to grant-set names.
    pub workflow_grants: BTreeMap<String, BTreeSet<String>>,
    /// Whether missing capability grants deny step execution.
    pub enforce_capabilities: bool,
}

/// Reusable operator-owned grants for one or more workflows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityGrantSet {
    /// Exact sensitive and structural capability grants.
    pub capabilities: BTreeSet<CapabilityRequest>,
    /// Version-pinned plugin invocation grants.
    pub plugin_grants: BTreeSet<PluginCapabilityGrant>,
}

/// One allowed `plugin + action` pair.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AllowedAction {
    /// Plugin id.
    pub plugin: String,
    /// Action name.
    pub action: String,
}

/// A version-pinned plugin and the actions it may invoke through local policy.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PluginCapabilityGrant {
    /// Canonical plugin package identity, including its exact version.
    pub plugin: String,
    /// Explicit actions that this grant permits.
    pub actions: BTreeSet<String>,
}

impl PluginCapabilityGrant {
    /// Creates a canonical plugin grant with a non-empty explicit action set.
    pub fn new(
        plugin: impl AsRef<str>,
        actions: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self> {
        let plugin = CapabilityRequest::new(CapabilityName::Plugin, plugin, ["load"])
            .map_err(|error| anyhow::anyhow!("invalid plugin grant: {error}"))?
            .resource;
        let mut canonical_actions = BTreeSet::new();
        for action in actions {
            let action = action.as_ref().trim();
            let resource = CapabilityRequest::new(
                CapabilityName::Action,
                format!("{plugin}/{action}"),
                ["invoke"],
            )
            .map_err(|error| anyhow::anyhow!("invalid plugin grant action: {error}"))?
            .resource;
            let action = resource
                .strip_prefix(&format!("{plugin}/"))
                .expect("canonical action uses canonical plugin prefix")
                .to_string();
            canonical_actions.insert(action);
        }
        if canonical_actions.is_empty() {
            anyhow::bail!("plugin grant actions cannot be empty");
        }
        Ok(Self {
            plugin,
            actions: canonical_actions,
        })
    }
}

/// Child-process environment policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildEnvPolicy {
    /// Whether child processes inherit the full parent environment.
    pub inherit: bool,
    /// Parent env vars that may be reintroduced when inheritance is disabled.
    pub allowlist: BTreeSet<String>,
}

impl Default for ChildEnvPolicy {
    fn default() -> Self {
        Self {
            inherit: false,
            allowlist: BTreeSet::from([
                "HOME".to_string(),
                "PATH".to_string(),
                "SSL_CERT_DIR".to_string(),
                "SSL_CERT_FILE".to_string(),
                "TMPDIR".to_string(),
            ]),
        }
    }
}

/// Policy wrapper that enforces local allowlists and child-env isolation.
#[derive(Clone)]
pub struct LocalAllowlistPolicy {
    inner: Arc<dyn PolicyPort>,
    allow: LocalAllowConfig,
    child_env: ChildEnvPolicy,
}

impl LocalAllowlistPolicy {
    /// Wraps a base policy with local allowlist and child-env behavior.
    pub fn new(
        inner: Arc<dyn PolicyPort>,
        allow: LocalAllowConfig,
        child_env: ChildEnvPolicy,
    ) -> Self {
        Self {
            inner,
            allow,
            child_env,
        }
    }

    /// Enables exact capability-grant evaluation for the allowlist profile.
    pub fn with_capabilities(
        mut self,
        capabilities: Option<BTreeSet<CapabilityRequest>>,
        enforce: bool,
    ) -> Self {
        self.allow.capabilities = capabilities;
        self.allow.enforce_capabilities = enforce;
        self
    }
}

impl std::fmt::Debug for LocalAllowlistPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalAllowlistPolicy")
            .field("allow", &self.allow)
            .field("child_env", &self.child_env)
            .finish_non_exhaustive()
    }
}

impl PolicyPort for LocalAllowlistPolicy {
    fn validate_run_start(&self, repo_root: &Path, workflow_ref: &str) -> Result<PolicyDecision> {
        let base = self.inner.validate_run_start(repo_root, workflow_ref)?;
        if !base.allowed {
            return Ok(base);
        }
        if let Some(workflows) = self.allow.workflows.as_ref() {
            if !workflows.contains(workflow_ref) {
                return Ok(deny(
                    base,
                    "local_allow_workflow_not_allowed",
                    format!("workflow '{}' is not allowed by local policy", workflow_ref),
                ));
            }
        }
        Ok(base)
    }

    fn validate_step_execution(
        &self,
        run_id: &str,
        step_id: &str,
        descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        let base = self
            .inner
            .validate_step_execution(run_id, step_id, descriptor)?;
        if !base.allowed {
            return Ok(base);
        }

        if let Some(workflows) = self.allow.workflows.as_ref() {
            let workflow_ref = extract_string(descriptor, "workflow_ref")?;
            if !workflows.contains(&workflow_ref) {
                return Ok(deny(
                    base,
                    "local_allow_workflow_not_allowed",
                    format!("workflow '{}' is not allowed by local policy", workflow_ref),
                ));
            }
        }

        if let Some(adapters) = self.allow.adapters.as_ref() {
            let adapter_kind = extract_string(descriptor, "adapter_kind")?;
            if !adapters.contains(&adapter_kind) {
                return Ok(deny(
                    base,
                    "local_allow_adapter_not_allowed",
                    format!("adapter '{}' is not allowed by local policy", adapter_kind),
                ));
            }
        }

        if let Some(plugins) = self.allow.plugins.as_ref() {
            let plugin_id = extract_string(descriptor, "plugin_id")?;
            if !plugins.contains(&plugin_id) {
                return Ok(deny(
                    base,
                    "local_allow_plugin_not_allowed",
                    format!("plugin '{}' is not allowed by local policy", plugin_id),
                ));
            }
        }

        if let Some(actions) = self.allow.actions.as_ref() {
            let candidate = AllowedAction {
                plugin: extract_string(descriptor, "plugin_id")?,
                action: extract_string(descriptor, "action")?,
            };
            if !actions.contains(&candidate) {
                return Ok(deny(
                    base,
                    "local_allow_action_not_allowed",
                    format!(
                        "action '{}::{}' is not allowed by local policy",
                        candidate.plugin, candidate.action
                    ),
                ));
            }
        }

        if let Some(secret_refs) = self.allow.secret_refs.as_ref() {
            for secret_ref in extract_string_array(descriptor, "secret_refs")? {
                if !secret_refs.contains(&secret_ref) {
                    return Ok(deny(
                        base,
                        "local_allow_secret_ref_not_allowed",
                        format!("secret ref '{}' is not allowed by local policy", secret_ref),
                    ));
                }
            }
        }

        if self.allow.enforce_capabilities {
            let (bound_exact_grants, bound_plugin_grants) = bound_grants(&self.allow, descriptor)?;
            let configured_exact_grants = self.allow.capabilities.as_ref();
            let configured_plugin_grants = self.allow.plugin_grants.as_ref();
            if configured_exact_grants.is_none()
                && configured_plugin_grants.is_none()
                && bound_exact_grants.is_empty()
                && bound_plugin_grants.is_empty()
            {
                return Ok(deny(
                    base,
                    "capability_not_granted",
                    "policy.allow.capabilities and policy.allow.plugin_grants are missing"
                        .to_string(),
                ));
            }
            let empty_grants = BTreeSet::new();
            let exact_grants = configured_exact_grants
                .into_iter()
                .flatten()
                .chain(bound_exact_grants.iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            let plugin_grants = configured_plugin_grants
                .into_iter()
                .flatten()
                .chain(bound_plugin_grants.iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            if is_unpinned_invoke(descriptor) {
                return deny_with_capabilities(
                    base,
                    "capability_not_granted",
                    "allowlist execution requires a pinned plugin version".to_string(),
                    Vec::new(),
                    &empty_grants,
                );
            }
            let requested = match extract_capabilities(descriptor) {
                Ok(requested) => requested,
                Err(error) => {
                    return deny_with_capabilities(
                        base,
                        "capability_not_granted",
                        format!("invalid generated capability request: {error}"),
                        Vec::new(),
                        &empty_grants,
                    );
                }
            };
            if requested.is_empty() {
                return Ok(deny(
                    base,
                    "capability_not_granted",
                    "step has no generated capability requests".to_string(),
                ));
            }
            if network_policy_requires_grant(descriptor)
                && !requested
                    .iter()
                    .any(|request| request.capability == CapabilityName::Network)
            {
                return deny_with_capabilities(
                    base,
                    "capability_not_granted",
                    "network policy has no generated destination grant".to_string(),
                    requested,
                    &empty_grants,
                );
            }
            let granted = match granted_capabilities_for_descriptor(
                descriptor,
                &requested,
                &exact_grants,
                &plugin_grants,
            ) {
                Ok(granted) => granted,
                Err(error) => {
                    return deny_with_capabilities(
                        base,
                        "capability_not_granted",
                        format!("invalid plugin grant coverage: {error}"),
                        requested,
                        &empty_grants,
                    );
                }
            };
            let denied = requested.iter().find(|request| !granted.contains(*request));
            if let Some(denied) = denied {
                return Ok(with_runtime_auth_context(
                    deny_with_capabilities(
                        base,
                        "capability_not_granted",
                        format!(
                            "capability '{}' for '{}' is not granted",
                            denied.capability, denied.resource
                        ),
                        requested,
                        &granted,
                    )?,
                    &self.child_env,
                ));
            }
            return Ok(with_runtime_auth_context(
                with_capability_payload(base, requested, &granted)?,
                &self.child_env,
            ));
        }

        Ok(with_runtime_auth_context(base, &self.child_env))
    }
}

fn bound_grants(
    allow: &LocalAllowConfig,
    descriptor: &KernelValue,
) -> Result<(BTreeSet<CapabilityRequest>, BTreeSet<PluginCapabilityGrant>)> {
    let workflow_ref = extract_string(descriptor, "workflow_ref")?;
    effective_grants_for_workflow(allow, &workflow_ref)
}

/// Resolves global grants plus grant sets bound to one exact workflow reference.
pub fn effective_grants_for_workflow(
    allow: &LocalAllowConfig,
    workflow_ref: &str,
) -> Result<(BTreeSet<CapabilityRequest>, BTreeSet<PluginCapabilityGrant>)> {
    let mut capabilities = allow.capabilities.clone().unwrap_or_default();
    let mut plugin_grants = allow.plugin_grants.clone().unwrap_or_default();
    let Some(names) = allow.workflow_grants.get(workflow_ref) else {
        return Ok((capabilities, plugin_grants));
    };
    for name in names {
        let grant_set = allow.grant_sets.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "workflow '{}' references unknown grant set '{}'",
                workflow_ref,
                name
            )
        })?;
        capabilities.extend(grant_set.capabilities.iter().cloned());
        plugin_grants.extend(grant_set.plugin_grants.iter().cloned());
    }
    Ok((capabilities, plugin_grants))
}

/// Returns requested capabilities covered by exact and version-pinned plugin grants.
pub fn granted_capabilities_for_descriptor(
    descriptor: &KernelValue,
    requested: &[CapabilityRequest],
    exact_grants: &BTreeSet<CapabilityRequest>,
    plugin_grants: &BTreeSet<PluginCapabilityGrant>,
) -> Result<BTreeSet<CapabilityRequest>> {
    let mut granted = requested
        .iter()
        .filter(|request| exact_grants.iter().any(|grant| grant.grants(request)))
        .cloned()
        .collect::<BTreeSet<_>>();
    granted.extend(plugin_grant_coverage(descriptor, requested, plugin_grants)?);
    Ok(granted)
}

fn plugin_grant_coverage(
    descriptor: &KernelValue,
    requested: &[CapabilityRequest],
    plugin_grants: &BTreeSet<PluginCapabilityGrant>,
) -> Result<BTreeSet<CapabilityRequest>> {
    let KernelValue::Object(fields) = descriptor else {
        return Ok(BTreeSet::new());
    };
    if !fields.contains_key("plugin_id") {
        return Ok(BTreeSet::new());
    }

    let plugin_id = extract_string(descriptor, "plugin_id")?;
    let plugin_version = extract_string(descriptor, "plugin_version")?;
    let action = extract_string(descriptor, "action")?;
    let package = CapabilityRequest::new(
        CapabilityName::Plugin,
        format!("{plugin_id}@{plugin_version}"),
        ["load"],
    )
    .map_err(|error| anyhow::anyhow!("invalid invoke descriptor plugin: {error}"))?
    .resource;
    if !plugin_grants
        .iter()
        .any(|grant| grant.plugin == package && grant.actions.contains(&action))
    {
        return Ok(BTreeSet::new());
    }

    let expected = [
        CapabilityRequest::new(CapabilityName::Plugin, &package, ["load"]),
        CapabilityRequest::new(
            CapabilityName::Action,
            format!("{package}/{action}"),
            ["invoke"],
        ),
        CapabilityRequest::new(
            CapabilityName::Adapter,
            extract_string(descriptor, "adapter_kind")?,
            ["invoke"],
        ),
        CapabilityRequest::new(
            CapabilityName::Provider,
            extract_string(descriptor, "provider_kind")?,
            ["use"],
        ),
    ]
    .into_iter()
    .collect::<std::result::Result<BTreeSet<_>, _>>()
    .map_err(|error| anyhow::anyhow!("invalid invoke descriptor authority: {error}"))?;

    if !expected.iter().all(|request| requested.contains(request)) {
        anyhow::bail!("matching plugin grant does not match persisted invocation authority");
    }
    Ok(expected)
}

fn network_policy_requires_grant(descriptor: &KernelValue) -> bool {
    let KernelValue::Object(map) = descriptor else {
        return false;
    };
    let Some(KernelValue::Object(policy)) = map.get("network_policy") else {
        return false;
    };
    matches!(
        policy.get("mode"),
        Some(KernelValue::String(mode)) if mode == "any" || mode == "allowlist"
    )
}

fn deny(base: PolicyDecision, reason_code: &str, message: String) -> PolicyDecision {
    PolicyDecision {
        allowed: false,
        reason_code: Some(reason_code.to_string()),
        message: Some(message),
        payload: base.payload,
        remote_secret_policy: base.remote_secret_policy,
    }
}

fn deny_with_capabilities(
    base: PolicyDecision,
    reason_code: &str,
    message: String,
    requested: Vec<CapabilityRequest>,
    grants: &BTreeSet<CapabilityRequest>,
) -> Result<PolicyDecision> {
    let mut decision = deny(base, reason_code, message);
    decision.payload = capability_payload(requested, grants, Some(reason_code))?;
    Ok(decision)
}

fn with_capability_payload(
    base: PolicyDecision,
    requested: Vec<CapabilityRequest>,
    grants: &BTreeSet<CapabilityRequest>,
) -> Result<PolicyDecision> {
    let payload = capability_payload(requested, grants, None)?;
    Ok(PolicyDecision { payload, ..base })
}

fn capability_payload(
    requested: Vec<CapabilityRequest>,
    grants: &BTreeSet<CapabilityRequest>,
    denial_reason: Option<&str>,
) -> Result<KernelValue> {
    let mut payload = BTreeMap::from([
        (
            "policy_id".to_string(),
            KernelValue::String("local_runtime_policy".to_string()),
        ),
        (
            "policy_profile".to_string(),
            KernelValue::String("allowlist".to_string()),
        ),
        (
            "requested_capabilities".to_string(),
            KernelValue::from_json(serde_json::to_value(&requested)?),
        ),
        (
            "granted_capabilities".to_string(),
            KernelValue::from_json(serde_json::to_value(grants)?),
        ),
    ]);
    if let Some(reason) = denial_reason {
        payload.insert(
            "denial_reason".to_string(),
            KernelValue::String(reason.to_string()),
        );
    }
    Ok(KernelValue::Object(payload))
}

fn extract_capabilities(descriptor: &KernelValue) -> Result<Vec<CapabilityRequest>> {
    let KernelValue::Object(map) = descriptor else {
        return Ok(Vec::new());
    };
    let Some(value) = map.get("requested_capabilities") else {
        return Ok(Vec::new());
    };
    let json = serde_json::to_value(value)?;
    let requests: Vec<CapabilityRequest> = serde_json::from_value(json)
        .map_err(|error| anyhow::anyhow!("invalid generated capability request: {error}"))?;
    Ok(requests)
}

fn is_unpinned_invoke(descriptor: &KernelValue) -> bool {
    let KernelValue::Object(map) = descriptor else {
        return false;
    };
    map.contains_key("plugin_id")
        && map.contains_key("action")
        && !matches!(
            map.get("plugin_version"),
            Some(KernelValue::String(version)) if !version.is_empty()
        )
}

fn with_runtime_auth_context(base: PolicyDecision, child_env: &ChildEnvPolicy) -> PolicyDecision {
    if child_env.inherit {
        return base;
    }

    let runtime_auth_context = KernelValue::Object(BTreeMap::from([
        ("active".to_string(), KernelValue::Bool(true)),
        (
            "policy_id".to_string(),
            KernelValue::String(LOCAL_POLICY_ID.to_string()),
        ),
        (
            "child_env_allowlist".to_string(),
            KernelValue::Array(
                child_env
                    .allowlist
                    .iter()
                    .cloned()
                    .map(KernelValue::String)
                    .collect(),
            ),
        ),
    ]));

    let payload = match base.payload {
        KernelValue::Object(mut object) => {
            object.insert("runtime_auth_context".to_string(), runtime_auth_context);
            KernelValue::Object(object)
        }
        _ => KernelValue::Object(BTreeMap::from([(
            "runtime_auth_context".to_string(),
            runtime_auth_context,
        )])),
    };

    PolicyDecision { payload, ..base }
}

fn extract_string(descriptor: &KernelValue, key: &str) -> Result<String> {
    let KernelValue::Object(map) = descriptor else {
        anyhow::bail!("policy descriptor must be an object")
    };
    let Some(KernelValue::String(value)) = map.get(key) else {
        anyhow::bail!("policy descriptor is missing string field '{key}'")
    };
    Ok(value.clone())
}

fn extract_string_array(descriptor: &KernelValue, key: &str) -> Result<Vec<String>> {
    let KernelValue::Object(map) = descriptor else {
        anyhow::bail!("policy descriptor must be an object")
    };
    let Some(value) = map.get(key) else {
        return Ok(Vec::new());
    };
    let KernelValue::Array(values) = value else {
        anyhow::bail!("policy descriptor field '{key}' must be an array")
    };
    values
        .iter()
        .map(|value| match value {
            KernelValue::String(value) => Ok(value.clone()),
            _ => anyhow::bail!("policy descriptor field '{key}' must contain strings"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_descriptor() -> KernelValue {
        KernelValue::Object(BTreeMap::from([
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
        ]))
    }

    fn invoke_descriptor(action: &str, extra: Vec<CapabilityRequest>) -> KernelValue {
        let package = "shell@0.1.0";
        let mut requested = vec![
            CapabilityRequest::new(CapabilityName::Adapter, "process", ["invoke"])
                .expect("adapter"),
            CapabilityRequest::new(CapabilityName::Provider, "local", ["use"]).expect("provider"),
            CapabilityRequest::new(CapabilityName::Plugin, package, ["load"]).expect("plugin"),
            CapabilityRequest::new(
                CapabilityName::Action,
                format!("{package}/{action}"),
                ["invoke"],
            )
            .expect("action"),
        ];
        requested.extend(extra);
        KernelValue::Object(BTreeMap::from([
            (
                "workflow_ref".to_string(),
                KernelValue::String("demo@v1".to_string()),
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
                "plugin_version".to_string(),
                KernelValue::String("0.1.0".to_string()),
            ),
            (
                "action".to_string(),
                KernelValue::String(action.to_string()),
            ),
            (
                "provider_kind".to_string(),
                KernelValue::String("local".to_string()),
            ),
            ("secret_refs".to_string(), KernelValue::Array(Vec::new())),
            (
                "requested_capabilities".to_string(),
                KernelValue::from_json(serde_json::to_value(requested).expect("json")),
            ),
        ]))
    }

    fn plugin_grant_policy(actions: &[&str]) -> LocalAllowlistPolicy {
        LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig {
                plugin_grants: Some(BTreeSet::from([PluginCapabilityGrant::new(
                    "shell@0.1.0",
                    actions,
                )
                .expect("grant")])),
                enforce_capabilities: true,
                ..LocalAllowConfig::default()
            },
            ChildEnvPolicy::default(),
        )
    }

    #[test]
    fn allowlist_denies_missing_secret_ref() {
        let policy = LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig {
                secret_refs: Some(BTreeSet::from(["OTHER_TOKEN".to_string()])),
                ..LocalAllowConfig::default()
            },
            ChildEnvPolicy::default(),
        );
        let decision = policy
            .validate_step_execution("run-1", "step-1", &base_descriptor())
            .expect("decision");
        assert!(!decision.allowed);
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("local_allow_secret_ref_not_allowed")
        );
    }

    #[test]
    fn allowlist_adds_runtime_auth_context_when_inheritance_disabled() {
        let policy = LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig::default(),
            ChildEnvPolicy::default(),
        );
        let decision = policy
            .validate_step_execution("run-1", "step-1", &base_descriptor())
            .expect("decision");
        assert!(decision.allowed);
        let KernelValue::Object(payload) = decision.payload else {
            panic!("expected object payload");
        };
        let Some(KernelValue::Object(runtime_auth)) = payload.get("runtime_auth_context") else {
            panic!("missing runtime_auth_context");
        };
        assert_eq!(runtime_auth.get("active"), Some(&KernelValue::Bool(true)));
    }

    #[test]
    fn workflow_allowlist_denies_run_start() {
        let policy = LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig {
                workflows: Some(BTreeSet::from(["other.workflow@v1".to_string()])),
                ..LocalAllowConfig::default()
            },
            ChildEnvPolicy::default(),
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let decision = policy
            .validate_run_start(tmp.path(), "demo.workflow@v1")
            .expect("decision");
        assert!(!decision.allowed);
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("local_allow_workflow_not_allowed")
        );
    }

    #[test]
    fn capability_profile_denies_missing_or_partial_grants() {
        let requested = CapabilityRequest::new(
            transist_protocol::CapabilityName::Adapter,
            "process",
            ["invoke"],
        )
        .expect("request");
        let partial = CapabilityRequest::new(
            transist_protocol::CapabilityName::Filesystem,
            "workspace:///tmp/repo",
            ["read"],
        )
        .expect("partial grant");
        let policy = LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig {
                capabilities: Some(BTreeSet::from([partial])),
                enforce_capabilities: true,
                ..LocalAllowConfig::default()
            },
            ChildEnvPolicy::default(),
        );
        let descriptor = KernelValue::Object(BTreeMap::from([
            (
                "workflow_ref".to_string(),
                KernelValue::String("demo@v1".to_string()),
            ),
            (
                "requested_capabilities".to_string(),
                KernelValue::from_json(serde_json::to_value([requested]).expect("json")),
            ),
        ]));
        let decision = policy
            .validate_step_execution("run", "step", &descriptor)
            .expect("decision");
        assert!(!decision.allowed);
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("capability_not_granted")
        );
    }

    #[test]
    fn capability_profile_denies_unpinned_invokes() {
        let requested = CapabilityRequest::new(
            transist_protocol::CapabilityName::Adapter,
            "process",
            ["invoke"],
        )
        .expect("request");
        let policy = LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig {
                capabilities: Some(BTreeSet::from([requested.clone()])),
                enforce_capabilities: true,
                ..LocalAllowConfig::default()
            },
            ChildEnvPolicy::default(),
        );
        let mut descriptor = base_descriptor();
        if let KernelValue::Object(map) = &mut descriptor {
            map.insert(
                "requested_capabilities".to_string(),
                KernelValue::from_json(serde_json::to_value([requested]).expect("json")),
            );
        }
        let decision = policy
            .validate_step_execution("run", "step", &descriptor)
            .expect("decision");
        assert!(!decision.allowed);
        assert!(
            decision
                .message
                .as_deref()
                .is_some_and(|message| message.contains("pinned plugin version"))
        );
    }

    #[test]
    fn capability_profile_denies_malformed_requests() {
        let grant = CapabilityRequest::new(
            transist_protocol::CapabilityName::Adapter,
            "process",
            ["invoke"],
        )
        .expect("grant");
        let policy = LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig {
                capabilities: Some(BTreeSet::from([grant])),
                enforce_capabilities: true,
                ..LocalAllowConfig::default()
            },
            ChildEnvPolicy::default(),
        );
        let descriptor = KernelValue::Object(BTreeMap::from([
            (
                "workflow_ref".to_string(),
                KernelValue::String("demo@v1".to_string()),
            ),
            (
                "requested_capabilities".to_string(),
                KernelValue::String("not-an-array".to_string()),
            ),
        ]));
        let decision = policy
            .validate_step_execution("run", "step", &descriptor)
            .expect("decision");
        assert!(!decision.allowed);
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("capability_not_granted")
        );
    }

    #[test]
    fn plugin_capability_grant_canonicalizes_package_and_actions() {
        let grant =
            PluginCapabilityGrant::new("SHELL@0.1.0", [" exec ", "echo", "exec"]).expect("grant");

        assert_eq!(grant.plugin, "shell@0.1.0");
        assert_eq!(
            grant.actions,
            BTreeSet::from(["echo".to_string(), "exec".to_string()])
        );
    }

    #[test]
    fn plugin_capability_grant_rejects_unpinned_or_empty_values() {
        for (plugin, actions) in [
            ("shell", vec!["exec"]),
            ("shell@*", vec!["exec"]),
            ("shell@0.1.0", Vec::new()),
            ("shell@0.1.0", vec![""]),
            ("shell@0.1.0", vec!["*"]),
        ] {
            let display_actions = actions.clone();
            assert!(
                PluginCapabilityGrant::new(plugin, actions).is_err(),
                "{plugin:?} with {display_actions:?} must be rejected"
            );
        }
    }

    #[test]
    fn plugin_grant_covers_only_selected_invoke_structure() {
        let policy = plugin_grant_policy(&["exec"]);
        let decision = policy
            .validate_step_execution("run", "step", &invoke_descriptor("exec", Vec::new()))
            .expect("decision");
        assert!(decision.allowed);

        let unselected = policy
            .validate_step_execution("run", "step", &invoke_descriptor("echo", Vec::new()))
            .expect("decision");
        assert!(!unselected.allowed);
        assert_eq!(
            unselected.reason_code.as_deref(),
            Some("capability_not_granted")
        );
    }

    #[test]
    fn plugin_grant_does_not_cover_another_plugin_version() {
        let policy = plugin_grant_policy(&["exec"]);
        let mut descriptor = invoke_descriptor("exec", Vec::new());
        let KernelValue::Object(fields) = &mut descriptor else {
            panic!("descriptor");
        };
        fields.insert(
            "plugin_version".to_string(),
            KernelValue::String("1.2.1".to_string()),
        );
        let decision = policy
            .validate_step_execution("run", "step", &descriptor)
            .expect("decision");
        assert!(!decision.allowed);
    }

    #[test]
    fn plugin_grant_does_not_cover_sensitive_capabilities() {
        let policy = plugin_grant_policy(&["exec"]);
        for request in [
            CapabilityRequest::new(CapabilityName::Secret, "API_TOKEN", ["read"]).expect("secret"),
            CapabilityRequest::new(
                CapabilityName::Filesystem,
                "workspace:///tmp/repo",
                ["list", "read"],
            )
            .expect("filesystem"),
            CapabilityRequest::new(
                CapabilityName::Network,
                "https://api.example.com:443",
                ["connect"],
            )
            .expect("network"),
            CapabilityRequest::new(CapabilityName::Approval, "demo/gate", ["request"])
                .expect("approval"),
        ] {
            let decision = policy
                .validate_step_execution("run", "step", &invoke_descriptor("exec", vec![request]))
                .expect("decision");
            assert!(!decision.allowed);
        }
    }

    #[test]
    fn workflow_binding_applies_only_its_named_grant_set() {
        let grant_set = CapabilityGrantSet {
            plugin_grants: BTreeSet::from([
                PluginCapabilityGrant::new("shell@0.1.0", ["exec"]).expect("grant")
            ]),
            ..CapabilityGrantSet::default()
        };
        let policy = LocalAllowlistPolicy::new(
            Arc::new(crate::StaticPolicy::permissive()),
            LocalAllowConfig {
                grant_sets: BTreeMap::from([("delivery".to_string(), grant_set)]),
                workflow_grants: BTreeMap::from([(
                    "demo@v1".to_string(),
                    BTreeSet::from(["delivery".to_string()]),
                )]),
                enforce_capabilities: true,
                ..LocalAllowConfig::default()
            },
            ChildEnvPolicy::default(),
        );
        assert!(
            policy
                .validate_step_execution("run", "step", &invoke_descriptor("exec", Vec::new()))
                .expect("bound decision")
                .allowed
        );

        let mut unbound = invoke_descriptor("exec", Vec::new());
        let KernelValue::Object(fields) = &mut unbound else {
            panic!("descriptor");
        };
        fields.insert(
            "workflow_ref".to_string(),
            KernelValue::String("other@v1".to_string()),
        );
        assert!(
            !policy
                .validate_step_execution("run", "step", &unbound)
                .expect("unbound decision")
                .allowed
        );
    }

    #[test]
    fn plugin_grant_rejects_descriptor_authority_mismatch() {
        let policy = plugin_grant_policy(&["exec"]);
        let mut descriptor = invoke_descriptor("exec", Vec::new());
        let KernelValue::Object(fields) = &mut descriptor else {
            panic!("descriptor");
        };
        fields.insert(
            "requested_capabilities".to_string(),
            KernelValue::from_json(
                serde_json::to_value([CapabilityRequest::new(
                    CapabilityName::Adapter,
                    "process",
                    ["invoke"],
                )
                .expect("adapter")])
                .expect("json"),
            ),
        );
        let decision = policy
            .validate_step_execution("run", "step", &descriptor)
            .expect("decision");
        assert!(!decision.allowed);
        assert!(
            decision
                .message
                .as_deref()
                .is_some_and(|message| message.contains("plugin grant coverage"))
        );
    }
}
