// SPDX-License-Identifier: MPL-2.0
//! Network policy types for workflow-scoped network governance.

use serde::{Deserialize, Serialize};

/// Network access mode for a workflow or step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccessMode {
    /// No runtime-owned network restriction is requested.
    #[default]
    Any,
    /// Deny non-loopback egress while preserving loopback access.
    Loopback,
    /// Cooperative allow-list metadata for plugins that enforce their own egress rules.
    Allowlist,
    /// Deny all egress.
    None,
}

/// Network policy attached to a workflow or step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NetworkPolicy {
    /// Network access mode.
    #[serde(default)]
    pub mode: NetworkAccessMode,
    /// Cooperative host allow-list metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_hosts: Vec<String>,
    /// Cooperative port allow-list metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_ports: Vec<u16>,
    /// Whether plugins should require TLS for remote destinations.
    #[serde(default)]
    pub tls_required: bool,
    /// Cooperative redirect limit for plugins that support redirects.
    #[serde(default)]
    pub max_redirects: u32,
}

impl NetworkPolicy {
    /// Returns true when the policy is the builtin default.
    pub fn is_default(&self) -> bool {
        self.mode == NetworkAccessMode::Any
            && self.allow_hosts.is_empty()
            && self.allow_ports.is_empty()
            && !self.tls_required
            && self.max_redirects == 0
    }

    /// Resolves the effective policy for one step.
    ///
    /// `mode` uses whole-policy replacement (step > workflow > global > builtin).
    /// The allow lists are merged additively across all levels with duplicates
    /// removed while preserving first-seen order. `tls_required` and
    /// `max_redirects` follow the most-specific replacement policy source.
    pub fn resolve(
        global: Option<&Self>,
        workflow: Option<&Self>,
        step: Option<&Self>,
    ) -> (Self, NetworkPolicySource) {
        let (base, source) = if let Some(policy) = step {
            (policy, NetworkPolicySource::Step)
        } else if let Some(policy) = workflow {
            (policy, NetworkPolicySource::Workflow)
        } else if let Some(policy) = global {
            (policy, NetworkPolicySource::Global)
        } else {
            return (Self::default(), NetworkPolicySource::Builtin);
        };

        (
            Self {
                mode: base.mode,
                allow_hosts: merge_strings([
                    global.map(|p| p.allow_hosts.as_slice()),
                    workflow.map(|p| p.allow_hosts.as_slice()),
                    step.map(|p| p.allow_hosts.as_slice()),
                ]),
                allow_ports: merge_ports([
                    global.map(|p| p.allow_ports.as_slice()),
                    workflow.map(|p| p.allow_ports.as_slice()),
                    step.map(|p| p.allow_ports.as_slice()),
                ]),
                tls_required: base.tls_required,
                max_redirects: base.max_redirects,
            },
            source,
        )
    }
}

fn merge_strings<const N: usize>(levels: [Option<&[String]>; N]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for level in levels.into_iter().flatten() {
        for value in level {
            if seen.insert(value.clone()) {
                merged.push(value.clone());
            }
        }
    }
    merged
}

fn merge_ports<const N: usize>(levels: [Option<&[u16]>; N]) -> Vec<u16> {
    let mut seen = std::collections::BTreeSet::new();
    let mut merged = Vec::new();
    for level in levels.into_iter().flatten() {
        for port in level {
            if seen.insert(*port) {
                merged.push(*port);
            }
        }
    }
    merged
}

/// Source of an effective network policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicySource {
    /// Builtin default policy.
    Builtin,
    /// Policy loaded from `transist/settings.yaml`.
    Global,
    /// Top-level workflow policy.
    Workflow,
    /// Step-level workflow policy.
    Step,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(
        mode: NetworkAccessMode,
        allow_hosts: &[&str],
        allow_ports: &[u16],
        tls_required: bool,
        max_redirects: u32,
    ) -> NetworkPolicy {
        NetworkPolicy {
            mode,
            allow_hosts: allow_hosts.iter().map(|value| value.to_string()).collect(),
            allow_ports: allow_ports.to_vec(),
            tls_required,
            max_redirects,
        }
    }

    #[test]
    fn resolve_builtin_when_nothing_is_set() {
        let (policy, source) = NetworkPolicy::resolve(None, None, None);
        assert_eq!(source, NetworkPolicySource::Builtin);
        assert!(policy.is_default());
    }

    #[test]
    fn resolve_mode_uses_most_specific_level() {
        let global = policy(
            NetworkAccessMode::Loopback,
            &["global.example"],
            &[443],
            false,
            1,
        );
        let workflow = policy(
            NetworkAccessMode::Allowlist,
            &["workflow.example"],
            &[8443],
            true,
            3,
        );
        let step = policy(NetworkAccessMode::None, &["step.example"], &[9443], true, 5);

        let (policy, source) = NetworkPolicy::resolve(Some(&global), Some(&workflow), Some(&step));
        assert_eq!(source, NetworkPolicySource::Step);
        assert_eq!(policy.mode, NetworkAccessMode::None);
        assert!(policy.tls_required);
        assert_eq!(policy.max_redirects, 5);

        let (policy, source) = NetworkPolicy::resolve(Some(&global), Some(&workflow), None);
        assert_eq!(source, NetworkPolicySource::Workflow);
        assert_eq!(policy.mode, NetworkAccessMode::Allowlist);
        assert!(policy.tls_required);
        assert_eq!(policy.max_redirects, 3);
    }

    #[test]
    fn resolve_allow_lists_are_merged_across_all_levels() {
        let global = policy(
            NetworkAccessMode::Any,
            &["api.example.com", "auth.example.com"],
            &[80],
            false,
            0,
        );
        let workflow = policy(
            NetworkAccessMode::Allowlist,
            &["api.example.com", "internal.example.com"],
            &[80, 443],
            true,
            7,
        );
        let step = policy(
            NetworkAccessMode::Allowlist,
            &["step.example.com"],
            &[443, 8443],
            true,
            9,
        );

        let (policy, _) = NetworkPolicy::resolve(Some(&global), Some(&workflow), Some(&step));
        assert_eq!(
            policy.allow_hosts,
            vec![
                "api.example.com",
                "auth.example.com",
                "internal.example.com",
                "step.example.com"
            ]
        );
        assert_eq!(policy.allow_ports, vec![80, 443, 8443]);
    }

    #[test]
    fn is_default_false_when_any_metadata_is_present() {
        assert!(!policy(NetworkAccessMode::Any, &["api.example.com"], &[], false, 0).is_default());
        assert!(!policy(NetworkAccessMode::Any, &[], &[443], false, 0).is_default());
        assert!(!policy(NetworkAccessMode::Any, &[], &[], true, 0).is_default());
        assert!(!policy(NetworkAccessMode::Any, &[], &[], false, 1).is_default());
    }
}
