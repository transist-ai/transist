// SPDX-License-Identifier: MPL-2.0
//! Exact revision approvals, anchored to immutable waiting executions and current gate activation.
use super::*;
use transist_protocol::{GateDecisionRecord, GateReviewRequest, gate_review_documents};

impl RuntimeKernel {
    pub(super) fn gate_review_request(
        &self,
        run_id: &str,
        step: &transist_protocol::RecipeStep,
        execution: u32,
        previous: Option<&ExecutionResult>,
    ) -> Result<Option<GateReviewRequest>> {
        let names = gate_review_documents(step).map_err(anyhow::Error::msg)?;
        if names.is_empty() {
            return Ok(None);
        }
        if let Some(previous) = previous.filter(|r| r.status == "blocked") {
            if previous.run_id != run_id
                || previous.step_id != step.id
                || previous.execution >= execution
            {
                bail!("waiting gate result identity mismatch");
            }
            let committed = self
                .services
                .run_store
                .load_step_execution(run_id, &step.id, previous.execution)?
                .context("waiting gate execution missing")?;
            if committed.status != "blocked"
                || serde_json::to_value(&committed.result)? != serde_json::to_value(Some(previous))?
            {
                bail!("waiting gate projection differs from its immutable request");
            }
            let request =
                review_from_result(previous)?.context("waiting gate lost its review request")?;
            self.validate_review_request(step, &request)?;
            return Ok(Some(request));
        }
        let documents = self
            .services
            .run_store
            .document_store()
            .context("document storage unavailable")?;
        let heads = documents.document_heads(run_id)?;
        let revisions = names
            .iter()
            .map(|name| {
                let revision = heads
                    .get(name)
                    .with_context(|| format!("review document '{name}' is unpublished"))?;
                documents.read_document_blob(run_id, revision)?;
                Ok(revision.clone())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(GateReviewRequest {
            schema: "transist.gate_review_request.v1".into(),
            run_id: run_id.into(),
            gate_id: validated_gate_id_for_step(step)?,
            step_id: step.id.clone(),
            execution,
            review_documents: revisions,
        }))
    }

    fn validate_review_request(
        &self,
        step: &transist_protocol::RecipeStep,
        request: &GateReviewRequest,
    ) -> Result<()> {
        if request.schema != "transist.gate_review_request.v1"
            || request.execution == 0
            || request.step_id != step.id
            || request.gate_id != validated_gate_id_for_step(step)?
            || request
                .review_documents
                .iter()
                .map(|r| r.document.clone())
                .collect::<Vec<_>>()
                != gate_review_documents(step).map_err(anyhow::Error::msg)?
        {
            bail!("gate review request does not match the pinned gate");
        }
        let anchor = self
            .services
            .run_store
            .load_step_execution(&request.run_id, &step.id, request.execution)?
            .context("missing immutable gate review request execution")?;
        if anchor.run_id != request.run_id
            || anchor.step_id != request.step_id
            || anchor.execution != request.execution
            || anchor.status != "blocked"
            || anchor
                .result
                .as_ref()
                .map(review_from_result)
                .transpose()?
                .flatten()
                .as_ref()
                != Some(request)
        {
            bail!("gate review request differs from its immutable waiting execution");
        }
        let documents = self
            .services
            .run_store
            .document_store()
            .context("document storage unavailable")?;
        for revision in &request.review_documents {
            documents.read_document_blob(&request.run_id, revision)?;
        }
        Ok(())
    }

    pub(super) fn bind_gate_review_decision(
        &self,
        home: &Path,
        root: &str,
        decision: &mut GateDecisionRecord,
    ) -> Result<()> {
        let store = self.services.run_store.as_ref();
        let Some(run) = store.load_run(&decision.run_id)? else {
            if decision.review_request.is_some() {
                bail!("review decision run is missing");
            }
            return Ok(());
        };
        let recipe = self.load_persisted_recipe(&decision.run_id, &run)?;
        let step = recipe
            .steps
            .iter()
            .find(|s| s.id == decision.step_id)
            .context("decision gate step is missing")?;
        if gate_review_documents(step)
            .map_err(anyhow::Error::msg)?
            .is_empty()
        {
            if decision.review_request.is_some() {
                bail!("unexpected gate review request");
            }
            return Ok(());
        }
        if store.resolve_root_run_id(home, &run.run_id)? != root {
            bail!("gate review root mismatch");
        }
        let state = run
            .steps
            .get(&step.id)
            .context("gate review state missing")?;
        if state.status != KernelStepStatus::Blocked {
            bail!("review gate is not waiting");
        }
        let result = state
            .last_result
            .as_ref()
            .context("waiting gate result missing")?;
        if result.run_id != run.run_id
            || result.step_id != step.id
            || result.execution != state.executions
            || result.status != "blocked"
        {
            bail!("waiting gate execution identity mismatch");
        }
        let committed = store
            .load_step_execution(&run.run_id, &step.id, state.executions)?
            .context("waiting gate execution missing")?;
        if serde_json::to_value(&committed.result)? != serde_json::to_value(Some(result))? {
            bail!("waiting gate state differs from committed request");
        }
        let request = review_from_result(result)?.context("waiting gate review request missing")?;
        if request.run_id != run.run_id
            || request.execution > result.execution
            || request.execution <= state.activation_executions_baseline
        {
            bail!("review request belongs to another run or gate activation");
        }
        self.validate_review_request(step, &request)?;
        if decision
            .review_request
            .as_ref()
            .is_some_and(|r| r != &request)
        {
            bail!("supplied review request differs from runtime request");
        }
        let mut existing = None;
        for raw in store.load_gate_decision_history(home, &run.run_id, &request.gate_id)? {
            let prior: GateDecisionRecord = serde_json::from_slice(&raw)?;
            if prior
                .review_request
                .as_ref()
                .is_some_and(|r| r.execution == request.execution)
                && existing.replace(prior).is_some()
            {
                bail!("ambiguous document review decisions");
            }
        }
        if let Some(prior) = existing {
            self.validate_review_decision_history(home, &prior, &request)?;
            if prior.decision != decision.decision
                || prior.actor != decision.actor
                || prior.actor_source != decision.actor_source
                || prior.reason != decision.reason
            {
                bail!("document review request already has a different immutable decision");
            }
            // A durable history publication may precede an interrupted active
            // replacement. Retry that exact decision, preserving its timestamp.
            *decision = prior;
            return Ok(());
        }
        decision.review_request = Some(request);
        Ok(())
    }

    /// Requires current successful gate state and its exact immutable decision history before copying inputs.
    pub(super) fn validate_document_approvals(
        &self,
        home: &Path,
        request: &ExecutionRequest,
    ) -> Result<()> {
        let bindings = transist_protocol::collect_document_bindings(&request.input)
            .map_err(anyhow::Error::msg)?;
        if !bindings.iter().any(|l| l.binding.reviewed_by.is_some()) {
            return Ok(());
        }
        let store = self.services.run_store.as_ref();
        let run = store
            .load_run(&request.run_id)?
            .context("document consumer run missing")?;
        let recipe = self.load_persisted_recipe(&run.run_id, &run)?;
        transist_protocol::validate_document_reviews(&recipe).map_err(anyhow::Error::msg)?;
        let heads = store
            .document_store()
            .context("document storage unavailable")?
            .document_heads(&run.run_id)?;
        for location in bindings {
            let Some(gate_step_id) = &location.binding.reviewed_by else {
                continue;
            };
            let step = recipe
                .steps
                .iter()
                .find(|s| &s.id == gate_step_id)
                .context("unknown recipe-local reviewed_by gate")?;
            let state = run
                .steps
                .get(gate_step_id)
                .context("review gate state missing")?;
            let result = state
                .last_result
                .as_ref()
                .context("document review approval missing")?;
            if state.status != KernelStepStatus::Succeeded
                || result.status != "success"
                || result.run_id != run.run_id
                || result.step_id != step.id
                || result.execution != state.executions
                || state.activation_executions() == 0
            {
                bail!("document review gate has no current successful approval");
            }
            let committed = store
                .load_step_execution(&run.run_id, &step.id, state.executions)?
                .context("approved gate execution missing")?;
            if committed.status != "success"
                || result
                    .output
                    .get("decision")
                    .and_then(serde_json::Value::as_str)
                    != Some("approved")
                || serde_json::to_value(&committed.result)? != serde_json::to_value(Some(result))?
            {
                bail!("approved gate state differs from immutable execution");
            }
            let review =
                review_from_result(result)?.context("approved gate review request missing")?;
            if review.run_id != run.run_id
                || review.execution <= state.activation_executions_baseline
            {
                bail!("stale gate review activation");
            }
            self.validate_review_request(step, &review)?;
            let decision: GateDecisionRecord = serde_json::from_value(
                result
                    .output
                    .get("review_decision")
                    .context("approved gate decision missing")?
                    .clone(),
            )?;
            if decision.decision != "approved" || decision.review_request.as_ref() != Some(&review)
            {
                bail!("document review was not approved");
            }
            self.validate_review_decision_history(home, &decision, &review)?;
            let selected = heads
                .get(&location.binding.document)
                .context("reviewed document is unpublished")?;
            if !review.review_documents.contains(selected) {
                bail!(
                    "document revision is not covered by the current approval (exact revision ID and digest required)"
                );
            }
        }
        Ok(())
    }

    pub(super) fn validate_review_decision_history(
        &self,
        home: &Path,
        decision: &GateDecisionRecord,
        request: &GateReviewRequest,
    ) -> Result<()> {
        if decision.run_id != request.run_id
            || decision.step_id != request.step_id
            || decision.gate_id != request.gate_id
            || decision.review_request.as_ref() != Some(request)
        {
            bail!("gate decision does not match review request");
        }
        let records = self.services.run_store.load_gate_decision_history(
            home,
            &request.run_id,
            &request.gate_id,
        )?;
        let mut matching = 0;
        for raw in records {
            let record: GateDecisionRecord = serde_json::from_slice(&raw)?;
            super::gates::validate_gate_decision_record(&record)?;
            if record.run_id != request.run_id
                || record.step_id != request.step_id
                || record.gate_id != request.gate_id
            {
                bail!("gate decision history identity mismatch");
            }
            let historical_request = record
                .review_request
                .as_ref()
                .context("document gate history lost its review request")?;
            let run = self
                .services
                .run_store
                .load_run(&request.run_id)?
                .context("review run missing")?;
            let recipe = self.load_persisted_recipe(&run.run_id, &run)?;
            let step = recipe
                .steps
                .iter()
                .find(|s| s.id == request.step_id)
                .context("review gate missing")?;
            self.validate_review_request(step, historical_request)?;
            if record
                .review_request
                .as_ref()
                .is_some_and(|r| r.execution == request.execution)
            {
                if record != *decision {
                    bail!("ambiguous or changed review decision history");
                }
                matching += 1;
            }
        }
        if matching != 1 {
            bail!("missing or ambiguous immutable review decision");
        }
        Ok(())
    }

    pub(super) fn load_reviewed_gate_decision(
        &self,
        home: &Path,
        run_id: &str,
        step: &transist_protocol::RecipeStep,
        execution: u32,
        previous: Option<&ExecutionResult>,
        output: &mut serde_json::Value,
    ) -> Result<Option<GateDecisionRecord>> {
        let review = self.gate_review_request(run_id, step, execution, previous)?;
        if let Some(review) = &review {
            output["review_request"] = serde_json::to_value(review)?;
        }
        let decision = load_gate_decision_record(
            self.services.run_store.as_ref(),
            home,
            run_id,
            &validated_gate_id_for_step(step)?,
        )?;
        if let Some(decision) = &decision {
            if decision.step_id != step.id {
                bail!("gate decision step identity mismatch");
            }
            if let Some(review) = &review {
                self.validate_review_decision_history(home, decision, review)?;
                output["review_decision"] = serde_json::to_value(decision)?;
            } else if decision.review_request.is_some() {
                bail!("unexpected document review decision");
            }
        }
        Ok(decision)
    }
}

fn review_from_result(result: &ExecutionResult) -> Result<Option<GateReviewRequest>> {
    result
        .output
        .get("review_request")
        .map(|v| serde_json::from_value(v.clone()).map_err(Into::into))
        .transpose()
}
