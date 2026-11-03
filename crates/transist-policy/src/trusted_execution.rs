// SPDX-License-Identifier: MPL-2.0
use crate::{
    SharedTrustDecision, merge_trusted_execution_run_decision,
    merge_trusted_execution_step_decision,
};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use transist_core::TrustDecisionRecord;
use transist_core::ports::{PolicyDecision, PolicyPort};
use transist_protocol::KernelValue;

/// Policy wrapper that composes trusted-execution run-start denial onto an existing policy.
#[derive(Clone)]
pub struct TrustedExecutionPolicy {
    inner: Arc<dyn PolicyPort>,
    trust_decision: SharedTrustDecision,
}

impl TrustedExecutionPolicy {
    /// Wraps one base policy with an optional trusted-execution decision.
    pub fn new(inner: Arc<dyn PolicyPort>, trust_decision: Option<TrustDecisionRecord>) -> Self {
        Self {
            inner,
            trust_decision: SharedTrustDecision::new(trust_decision),
        }
    }
}

impl std::fmt::Debug for TrustedExecutionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustedExecutionPolicy")
            .finish_non_exhaustive()
    }
}

impl PolicyPort for TrustedExecutionPolicy {
    fn validate_run_start(&self, repo_root: &Path, workflow_ref: &str) -> Result<PolicyDecision> {
        let base = self.inner.validate_run_start(repo_root, workflow_ref)?;
        Ok(merge_trusted_execution_run_decision(
            base,
            self.trust_decision.as_ref(),
        ))
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
        Ok(merge_trusted_execution_step_decision(base))
    }
}
