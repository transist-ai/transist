// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Built-in policy implementations for Transist runtime ports.
//!
//! This crate owns reusable policy implementations over the `transist-core`
//! policy port. It may depend on core policy traits and protocol values, but it
//! must not own runtime orchestration, storage, or interface behavior.

/// Policy crate version.
pub const TRANSIST_POLICY_VERSION: &str = env!("CARGO_PKG_VERSION");

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use transist_core::TrustDecisionRecord;
use transist_core::ports::{PolicyDecision, PolicyPort, RemoteSecretPolicy};
use transist_protocol::KernelValue;

mod trusted_execution;
pub use trusted_execution::TrustedExecutionPolicy;

mod local_only;
pub use local_only::LocalOnlyPolicy;

mod local_allowlist;
pub use local_allowlist::{
    AllowedAction, CapabilityGrantSet, ChildEnvPolicy, LocalAllowConfig, LocalAllowlistPolicy,
    PluginCapabilityGrant, effective_grants_for_workflow, granted_capabilities_for_descriptor,
};

/// Simple in-memory policy used by tests, local wiring, and explicit policy setup.
#[derive(Debug, Clone)]
pub struct StaticPolicy {
    allow_run_start: bool,
    allow_step_execution: bool,
    remote_secret_policy: RemoteSecretPolicy,
}

impl StaticPolicy {
    /// Creates a policy that allows run start and step execution.
    pub fn permissive() -> Self {
        Self {
            allow_run_start: true,
            allow_step_execution: true,
            remote_secret_policy: RemoteSecretPolicy::default(),
        }
    }

    /// Creates a policy that denies run start and step execution.
    pub fn deny_all() -> Self {
        Self {
            allow_run_start: false,
            allow_step_execution: false,
            remote_secret_policy: RemoteSecretPolicy::default(),
        }
    }

    /// Replaces the remote-secret delivery policy attached to policy decisions.
    pub fn with_remote_secret_policy(mut self, remote_secret_policy: RemoteSecretPolicy) -> Self {
        self.remote_secret_policy = remote_secret_policy;
        self
    }
}

impl PolicyPort for StaticPolicy {
    fn validate_run_start(&self, _repo_root: &Path, _workflow_ref: &str) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: self.allow_run_start,
            reason_code: (!self.allow_run_start).then(|| "policy_denied".to_string()),
            message: (!self.allow_run_start)
                .then(|| "run start denied by static policy".to_string()),
            payload: KernelValue::Null,
            remote_secret_policy: self.remote_secret_policy,
        })
    }

    fn validate_step_execution(
        &self,
        _run_id: &str,
        _step_id: &str,
        _descriptor: &KernelValue,
    ) -> Result<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: self.allow_step_execution,
            reason_code: (!self.allow_step_execution).then(|| "policy_denied".to_string()),
            message: (!self.allow_step_execution)
                .then(|| "step execution denied by static policy".to_string()),
            payload: KernelValue::Null,
            remote_secret_policy: self.remote_secret_policy,
        })
    }
}

fn trust_policy_denial(decision: &TrustDecisionRecord) -> PolicyDecision {
    PolicyDecision {
        allowed: false,
        reason_code: decision.reason_code.clone(),
        message: Some(format!(
            "trusted execution denied (code={}, verification={})",
            decision
                .reason_code
                .as_deref()
                .unwrap_or("trusted_execution_denied"),
            decision
                .failed_verification_type
                .as_deref()
                .unwrap_or("unknown")
        )),
        payload: KernelValue::Null,
        remote_secret_policy: RemoteSecretPolicy::default(),
    }
}

fn merge_trusted_execution_run_decision(
    base: PolicyDecision,
    trust_decision: &Option<TrustDecisionRecord>,
) -> PolicyDecision {
    if !base.allowed {
        return base;
    }
    if let Some(decision) = trust_decision.as_ref().filter(|decision| decision.denied()) {
        return trust_policy_denial(decision);
    }
    base
}

fn merge_trusted_execution_step_decision(base: PolicyDecision) -> PolicyDecision {
    base
}

#[derive(Clone)]
struct SharedTrustDecision(Arc<Option<TrustDecisionRecord>>);

impl SharedTrustDecision {
    fn new(decision: Option<TrustDecisionRecord>) -> Self {
        Self(Arc::new(decision))
    }

    fn as_ref(&self) -> &Option<TrustDecisionRecord> {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_policy_allows() {
        let policy = StaticPolicy::permissive();
        let tmp = tempfile::tempdir().expect("tempdir");
        let start = policy
            .validate_run_start(tmp.path(), "wf")
            .expect("decision");
        assert!(start.allowed);
    }

    #[test]
    fn deny_all_policy_blocks() {
        let policy = StaticPolicy::deny_all();
        let decision = policy
            .validate_step_execution("run", "step", &KernelValue::Null)
            .expect("decision");
        assert!(!decision.allowed);
    }

    #[test]
    fn trusted_execution_policy_denies_run_start_when_trust_decision_denied() {
        let policy = TrustedExecutionPolicy::new(
            Arc::new(StaticPolicy::permissive()),
            Some(TrustDecisionRecord {
                schema: transist_core::TRUST_DECISION_SCHEMA_V1.to_string(),
                run_id: "run-1".to_string(),
                trusted_execution_mode: true,
                decision: "denied".to_string(),
                workflow: transist_core::TrustSubjectDecision {
                    id: "wf".to_string(),
                    version: "v1".to_string(),
                    digest: "abc".to_string(),
                    signature_verified: false,
                    signer_key_id: None,
                    reason_code: Some("unsigned".to_string()),
                    trust_decision: "denied".to_string(),
                },
                plugins: Vec::new(),
                reason_code: Some("unsigned".to_string()),
                failed_verification_type: Some("workflow_signature".to_string()),
                organization_id: None,
                policy_revision: None,
                decided_at_ms: 1,
            }),
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let decision = policy
            .validate_run_start(tmp.path(), "wf@v1")
            .expect("decision");
        assert!(!decision.allowed);
        assert_eq!(decision.reason_code.as_deref(), Some("unsigned"));
    }
}
