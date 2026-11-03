// SPDX-License-Identifier: MPL-2.0
//! Shared main/cleanup document boundary. Storage owns bytes; execution records own publication.
use super::*;
use crate::RemoteSecretPolicy;
use crate::ports::InvocationStagingPort;

impl RuntimeKernel {
    /// Stages authored bindings through the composed store without exposing durable paths.
    pub(super) fn stage_request_documents(
        &self,
        staging: &dyn InvocationStagingPort,
        request: &mut ExecutionRequest,
        home: &Path,
    ) -> Result<()> {
        if transist_protocol::contains_document_binding(&request.input) {
            self.validate_document_approvals(home, request)?;
            let documents = self
                .services
                .run_store
                .document_store()
                .ok_or_else(|| anyhow::anyhow!("document storage unavailable"))?;
            staging.materialize_documents(request, documents)?;
        }
        Ok(())
    }

    /// Validates the complete string-path request before the consumer's first probe.
    pub(super) fn validate_staged_documents_before_dispatch(
        &self,
        paths: RuntimePaths<'_>,
        request: &ExecutionRequest,
        secret_refs: &[String],
        remote_secret_policy: RemoteSecretPolicy,
    ) -> Result<()> {
        if request.metadata.get("document_bindings").is_some() {
            self.validate_deferred_dispatch(paths, request, secret_refs, remote_secret_policy)?;
        }
        Ok(())
    }

    /// Prepares one complete successful output batch; the execution record publishes it.
    pub(super) fn capture_staged_documents(
        &self,
        request: &ExecutionRequest,
        result: &ExecutionResult,
        staging: &dyn InvocationStagingPort,
        redactor: &Redactor,
    ) -> Result<()> {
        let outputs = staging.document_outputs();
        if result.status == "success" && !outputs.is_empty() {
            self.services
                .run_store
                .document_store()
                .ok_or_else(|| anyhow::anyhow!("document storage unavailable"))?
                .prepare_document_revisions(
                    &request.run_id,
                    &request.step.id,
                    request.step.execution,
                    &outputs,
                    redactor,
                )
                .map_err(|error| anyhow::anyhow!("document output validation failed: {error:#}"))?;
        }
        Ok(())
    }
}
