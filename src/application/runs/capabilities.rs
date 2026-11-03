// SPDX-License-Identifier: MPL-2.0
//! Read-only capability allowlist guidance for persisted runs.

use super::helpers::load_persisted_recipe;
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use transist_policy::PluginCapabilityGrant;
use transist_protocol::CapabilityName;
use transist_protocol::CapabilityRequest;

/// A copyable local-policy fragment containing the grants still required.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CapabilityPolicyHint {
    /// Schema understood by policy-fragment composition.
    pub schema: String,
    /// Named, workflow-scoped grants required to cover the report.
    pub grant_sets: BTreeMap<String, CapabilityAllowHint>,
    /// Exact workflow binding for the suggested grant set.
    pub workflow_grants: Vec<WorkflowGrantHint>,
}

pub(crate) fn capability_policy_hint(
    workflow: &str,
    workflow_id: &str,
    missing_capabilities: &[CapabilityRequest],
    configured_plugin_grants: &BTreeSet<PluginCapabilityGrant>,
) -> CapabilityPolicyHint {
    let mut plugin_grants = BTreeSet::new();
    for request in missing_capabilities {
        if request.capability != CapabilityName::Action {
            continue;
        }
        let Some((package, action)) = request.resource.rsplit_once('/') else {
            continue;
        };
        if !configured_plugin_grants
            .iter()
            .any(|grant| grant.plugin == package && grant.actions.contains(action))
        {
            plugin_grants.insert((package.to_string(), action.to_string()));
        }
    }
    let plugin_grants = plugin_grants
        .into_iter()
        .fold(
            BTreeMap::<String, BTreeSet<String>>::new(),
            |mut grants, (plugin, action)| {
                grants.entry(plugin).or_default().insert(action);
                grants
            },
        )
        .into_iter()
        .map(|(plugin, actions)| {
            PluginCapabilityGrant::new(plugin, actions).expect("projected action is canonical")
        })
        .collect::<Vec<_>>();
    let capabilities: Vec<CapabilityRequest> = missing_capabilities
        .iter()
        .filter(|request| match request.capability {
            CapabilityName::Action => !plugin_grants.iter().any(|grant| {
                request
                    .resource
                    .rsplit_once('/')
                    .is_some_and(|(package, action)| {
                        grant.plugin == package && grant.actions.contains(action)
                    })
            }),
            CapabilityName::Plugin => !plugin_grants
                .iter()
                .any(|grant| grant.plugin == request.resource),
            CapabilityName::Adapter | CapabilityName::Provider => plugin_grants.is_empty(),
            _ => true,
        })
        .cloned()
        .collect();
    let grant_set_name = if workflow_id.is_empty() {
        "suggested".to_string()
    } else {
        format!("suggested-{}", workflow_id.replace('.', "-"))
    };
    CapabilityPolicyHint {
        schema: "transist.policy_fragment.v1".to_string(),
        grant_sets: BTreeMap::from([(
            grant_set_name.clone(),
            CapabilityAllowHint {
                plugin_grants,
                capabilities,
            },
        )]),
        workflow_grants: (!workflow.is_empty())
            .then(|| WorkflowGrantHint {
                workflow: workflow.to_string(),
                grant_sets: vec![grant_set_name],
            })
            .into_iter()
            .collect(),
    }
}

/// One workflow-to-grant-set binding in a remediation fragment.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowGrantHint {
    pub workflow: String,
    #[serde(rename = "use")]
    pub grant_sets: Vec<String>,
}

/// The capability grants in a local-policy hint.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CapabilityAllowHint {
    /// Plugin invocation authority grouped by pinned package and selected actions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugin_grants: Vec<PluginCapabilityGrant>,
    /// Exact normalized grants that remain missing from the current policy.
    pub capabilities: Vec<CapabilityRequest>,
}

/// The normalized capabilities required by a persisted run and its current policy gap.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunCapabilityReport {
    /// Run whose persisted recipe supplied the requirements.
    pub run_id: String,
    /// Effective runtime policy profile.
    pub policy_profile: String,
    /// Whether capability matching is enforced or bypassed for the profile.
    pub capability_enforcement: String,
    /// All exact normalized capability requests in the persisted recipe.
    pub required_capabilities: Vec<CapabilityRequest>,
    /// Current configured grants that cover at least one required request.
    pub granted_capabilities: Vec<CapabilityRequest>,
    /// Full requests not covered by the effective current allowlist.
    pub missing_capabilities: Vec<CapabilityRequest>,
    /// Copy this block under a local runtime policy when granting the missing authority.
    pub policy: CapabilityPolicyHint,
}

/// Reads persisted authority and compares it with the current local allowlist.
pub(crate) fn run_capabilities_in_home(
    home_dir: &Path,
    run_id: &str,
) -> Result<RunCapabilityReport> {
    let recipe = load_persisted_recipe(home_dir, run_id)?;
    let policy = crate::runtime_policy::load_capability_policy_snapshot(home_dir)?;
    let workflow = format!("{}@{}", recipe.workflow.id, recipe.workflow.version);
    let (grants, plugin_grants) =
        transist_policy::effective_grants_for_workflow(&policy.allow, &workflow)?;
    let mut report = build_run_capability_report(
        run_id,
        recipe.capabilities.values().flatten().cloned(),
        policy.profile,
        policy.enforcement,
        grants,
        plugin_grants.clone(),
    );
    report.policy = capability_policy_hint(
        &workflow,
        &recipe.workflow.id,
        &report.missing_capabilities,
        &plugin_grants,
    );
    Ok(report)
}

fn build_run_capability_report(
    run_id: &str,
    required: impl IntoIterator<Item = CapabilityRequest>,
    policy_profile: String,
    capability_enforcement: String,
    configured_grants: BTreeSet<CapabilityRequest>,
    configured_plugin_grants: BTreeSet<PluginCapabilityGrant>,
) -> RunCapabilityReport {
    let required = required.into_iter().collect::<BTreeSet<_>>();
    let required_capabilities = required.iter().cloned().collect::<Vec<_>>();
    let granted_capabilities = configured_grants
        .iter()
        .filter(|grant| required.iter().any(|request| grant.grants(request)))
        .cloned()
        .collect::<Vec<_>>();
    let missing_capabilities = if capability_enforcement == "enforced" {
        required
            .iter()
            .filter(|request| !configured_grants.iter().any(|grant| grant.grants(request)))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    RunCapabilityReport {
        run_id: run_id.to_string(),
        policy_profile,
        capability_enforcement,
        required_capabilities,
        granted_capabilities,
        policy: capability_policy_hint("", "", &missing_capabilities, &configured_plugin_grants),
        missing_capabilities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::CapabilityName;

    fn capability(
        capability: CapabilityName,
        resource: &str,
        operations: &[&str],
    ) -> CapabilityRequest {
        CapabilityRequest::new(capability, resource, operations).expect("capability")
    }

    #[test]
    fn report_deduplicates_requirements_and_lists_missing_grants() {
        let adapter = capability(CapabilityName::Adapter, "process", &["invoke"]);
        let plugin = capability(CapabilityName::Plugin, "shell@0.1.0", &["load"]);

        let report = build_run_capability_report(
            "run-1",
            vec![adapter.clone(), plugin.clone(), adapter.clone()],
            "allowlist".to_string(),
            "enforced".to_string(),
            BTreeSet::from([adapter.clone()]),
            BTreeSet::new(),
        );

        assert_eq!(
            report.required_capabilities,
            vec![adapter.clone(), plugin.clone()]
        );
        assert_eq!(report.granted_capabilities, vec![adapter]);
        assert_eq!(report.missing_capabilities, vec![plugin.clone()]);
        assert_eq!(report.policy.schema, "transist.policy_fragment.v1");
        assert_eq!(
            report.policy.grant_sets["suggested"].capabilities,
            vec![plugin]
        );
    }

    #[test]
    fn report_keeps_full_request_when_a_grant_is_partial() {
        let required = capability(
            CapabilityName::Filesystem,
            "run://artifacts",
            &["create", "write"],
        );
        let partial = capability(CapabilityName::Filesystem, "run://artifacts", &["create"]);

        let report = build_run_capability_report(
            "run-1",
            vec![required.clone()],
            "allowlist".to_string(),
            "enforced".to_string(),
            BTreeSet::from([partial]),
            BTreeSet::new(),
        );

        assert!(report.granted_capabilities.is_empty());
        assert_eq!(report.missing_capabilities, vec![required]);
    }

    #[test]
    fn report_has_no_missing_grants_when_enforcement_is_bypassed() {
        let required = capability(CapabilityName::Adapter, "process", &["invoke"]);

        let report = build_run_capability_report(
            "run-1",
            vec![required],
            "permissive".to_string(),
            "bypassed".to_string(),
            BTreeSet::new(),
            BTreeSet::new(),
        );

        assert!(report.missing_capabilities.is_empty());
        assert!(
            report.policy.grant_sets["suggested"]
                .capabilities
                .is_empty()
        );
    }

    #[test]
    fn hint_retains_structural_grants_without_a_generated_plugin_grant() {
        let adapter = capability(CapabilityName::Adapter, "process", &["invoke"]);

        let hint = capability_policy_hint("demo@v1", "demo", &[adapter.clone()], &BTreeSet::new());

        assert_eq!(
            hint.grant_sets["suggested-demo"].capabilities,
            vec![adapter]
        );
        assert!(hint.grant_sets["suggested-demo"].plugin_grants.is_empty());
    }

    #[test]
    fn hint_uses_generated_plugin_grant_for_invoke_structure() {
        let plugin = capability(CapabilityName::Plugin, "shell@0.1.0", &["load"]);
        let action = capability(CapabilityName::Action, "shell@0.1.0/echo", &["invoke"]);
        let adapter = capability(CapabilityName::Adapter, "process", &["invoke"]);
        let provider = capability(CapabilityName::Provider, "local", &["use"]);

        let hint = capability_policy_hint(
            "demo@v1",
            "demo",
            &[plugin, action, adapter, provider],
            &BTreeSet::new(),
        );

        assert!(hint.grant_sets["suggested-demo"].capabilities.is_empty());
        assert_eq!(hint.grant_sets["suggested-demo"].plugin_grants.len(), 1);
    }
}
