// SPDX-License-Identifier: MPL-2.0
//! Local-only policy wrapper that denies external-provider actions before dispatch.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use transist_core::ports::{PolicyDecision, PolicyPort, RemoteSecretPolicy};
use transist_protocol::KernelValue;

/// Policy wrapper that denies external invocation steps when local-only mode is
/// active.
///
/// Invocation descriptors contain an invocation field such as `action`,
/// `adapter_kind`, or `plugin_id` and must contain a `provider_kind` field
/// (`"local"` or `"external"`) resolved by the kernel from the installed
/// plugin manifest. Missing or invalid provider metadata on an invocation
/// fails closed. Control descriptors, including manual-gate checkpoints, are
/// delegated to the wrapped policy unchanged.
#[derive(Clone)]
pub struct LocalOnlyPolicy {
    inner: Arc<dyn PolicyPort>,
}

impl LocalOnlyPolicy {
    /// Wraps a base policy with local-only enforcement.
    pub fn new(inner: Arc<dyn PolicyPort>) -> Self {
        Self { inner }
    }
}

impl std::fmt::Debug for LocalOnlyPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalOnlyPolicy").finish_non_exhaustive()
    }
}

impl PolicyPort for LocalOnlyPolicy {
    fn validate_run_start(&self, repo_root: &Path, workflow_ref: &str) -> Result<PolicyDecision> {
        self.inner.validate_run_start(repo_root, workflow_ref)
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
        if !is_invocation_descriptor(descriptor) {
            return Ok(base);
        }

        let provider_kind = extract_provider_kind(descriptor)?;
        if provider_kind == "external" {
            return Ok(PolicyDecision {
                allowed: false,
                reason_code: Some("local_only_denied_external".to_string()),
                message: Some(format!(
                    "local-only mode denied external action '{}' in step '{}'",
                    extract_action(descriptor)?,
                    step_id
                )),
                payload: base.payload,
                remote_secret_policy: RemoteSecretPolicy::default(),
            });
        }
        Ok(base)
    }
}

fn is_invocation_descriptor(descriptor: &KernelValue) -> bool {
    let KernelValue::Object(map) = descriptor else {
        return false;
    };

    ["action", "adapter_kind", "plugin_id"]
        .iter()
        .any(|field| map.contains_key(*field))
}

fn extract_provider_kind(descriptor: &KernelValue) -> Result<String> {
    let KernelValue::Object(map) = descriptor else {
        anyhow::bail!("policy descriptor must be an object")
    };
    let Some(KernelValue::String(value)) = map.get("provider_kind") else {
        anyhow::bail!("policy descriptor is missing string field 'provider_kind'")
    };
    Ok(value.clone())
}

fn extract_action(descriptor: &KernelValue) -> Result<String> {
    let KernelValue::Object(map) = descriptor else {
        anyhow::bail!("policy descriptor must be an object")
    };
    let Some(KernelValue::String(value)) = map.get("action") else {
        anyhow::bail!("policy descriptor is missing string field 'action'")
    };
    Ok(value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn local_only_denies_external_provider_kind() {
        let policy = LocalOnlyPolicy::new(Arc::new(crate::StaticPolicy::permissive()));
        let descriptor = KernelValue::Object(BTreeMap::from([
            (
                "action".to_string(),
                KernelValue::String("fetch".to_string()),
            ),
            (
                "provider_kind".to_string(),
                KernelValue::String("external".to_string()),
            ),
        ]));
        let decision = policy
            .validate_step_execution("run-1", "step-1", &descriptor)
            .expect("decision");
        assert!(!decision.allowed);
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("local_only_denied_external")
        );
    }

    #[test]
    fn local_only_allows_local_provider_kind() {
        let policy = LocalOnlyPolicy::new(Arc::new(crate::StaticPolicy::permissive()));
        let descriptor = KernelValue::Object(BTreeMap::from([
            (
                "action".to_string(),
                KernelValue::String("shell".to_string()),
            ),
            (
                "provider_kind".to_string(),
                KernelValue::String("local".to_string()),
            ),
        ]));
        let decision = policy
            .validate_step_execution("run-1", "step-1", &descriptor)
            .expect("decision");
        assert!(decision.allowed);
    }

    #[test]
    fn local_only_fails_safe_when_provider_kind_absent() {
        let policy = LocalOnlyPolicy::new(Arc::new(crate::StaticPolicy::permissive()));
        let descriptor = KernelValue::Object(BTreeMap::from([(
            "action".to_string(),
            KernelValue::String("fetch".to_string()),
        )]));
        let error = policy
            .validate_step_execution("run-1", "step-1", &descriptor)
            .expect_err("missing provider kind must fail closed");
        assert!(error.to_string().contains("provider_kind"));
    }

    #[test]
    fn local_only_allows_manual_gate_descriptor_without_provider_kind() {
        let policy = LocalOnlyPolicy::new(Arc::new(crate::StaticPolicy::permissive()));
        let descriptor = KernelValue::Object(BTreeMap::from([
            (
                "workflow_ref".to_string(),
                KernelValue::String("demo_08_manual_gate@0.1.0".to_string()),
            ),
            (
                "requested_capabilities".to_string(),
                KernelValue::Array(vec![KernelValue::String("approval.request".to_string())]),
            ),
        ]));

        let decision = policy
            .validate_step_execution("run-1", "review_gate", &descriptor)
            .expect("manual gate descriptor delegates to wrapped policy");

        assert!(decision.allowed);
    }
}
