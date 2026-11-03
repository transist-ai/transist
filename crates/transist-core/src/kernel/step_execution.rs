// SPDX-License-Identifier: MPL-2.0
//! Step-state progression and control-step execution helpers.

use super::*;
use crate::ControlStepKind;
use crate::domain::taxonomy::LOOP_META_KEY;
use crate::scheduler::incoming_edge_reachability;

impl RuntimeKernel {
    pub(crate) fn terminal_status(&self, snapshot: &KernelRunSnapshot) -> KernelRunStatus {
        if snapshot
            .steps
            .values()
            .all(|step| step.status.is_terminal())
        {
            if snapshot
                .steps
                .values()
                .any(|step| step.status == KernelStepStatus::Failed)
            {
                KernelRunStatus::Failed
            } else if snapshot
                .steps
                .values()
                .any(|step| step.status == KernelStepStatus::Canceled)
            {
                KernelRunStatus::Canceled
            } else {
                KernelRunStatus::Success
            }
        } else if snapshot
            .steps
            .values()
            .any(|step| step.status == KernelStepStatus::Blocked)
        {
            KernelRunStatus::Blocked
        } else if snapshot
            .steps
            .values()
            .any(|step| step.status == KernelStepStatus::RetryWait)
        {
            KernelRunStatus::Waiting
        } else {
            KernelRunStatus::Failed
        }
    }

    pub(crate) fn ensure_entry_runnable(
        &self,
        graph: &ExecutionGraph,
        snapshot: &mut KernelRunSnapshot,
    ) {
        if let Some(step) = snapshot.steps.get_mut(&graph.entry) {
            if step.status == KernelStepStatus::Pending {
                step.status = KernelStepStatus::Runnable;
            }
        }
    }

    pub(crate) fn ensure_snapshot_steps(
        &self,
        snapshot: &mut KernelRunSnapshot,
        step_ids: &[String],
    ) {
        for step_id in step_ids {
            snapshot
                .steps
                .entry(step_id.clone())
                .or_insert_with(|| KernelStepState::pending(step_id));
        }
    }

    pub(crate) fn mark_non_dispatchable_skipped(
        &self,
        graph: &ExecutionGraph,
        snapshot: &mut KernelRunSnapshot,
        workflow_inputs: &BTreeMap<String, KernelValue>,
        workflow_vars: &BTreeMap<String, KernelValue>,
    ) -> Vec<String> {
        let mut changed = Vec::new();
        let step_ids: Vec<String> = snapshot.steps.keys().cloned().collect();
        for step_id in step_ids {
            let status = snapshot
                .steps
                .get(&step_id)
                .map(|step| step.status)
                .unwrap_or(KernelStepStatus::Pending);
            if !matches!(
                status,
                KernelStepStatus::Pending | KernelStepStatus::Runnable
            ) {
                continue;
            }

            let mut all_deps_terminal = true;
            if let Some(node) = graph.nodes.get(&step_id) {
                for dep in &node.dependencies {
                    let Some(dep_state) = snapshot.steps.get(dep) else {
                        all_deps_terminal = false;
                        break;
                    };
                    if !dep_state.status.is_terminal() {
                        all_deps_terminal = false;
                        break;
                    }
                }
            }
            if !all_deps_terminal {
                continue;
            }
            let reachability = incoming_edge_reachability(
                graph,
                snapshot,
                workflow_inputs,
                workflow_vars,
                &step_id,
            );
            if !reachability.has_satisfied_edge
                && !reachability.has_pending_edge
                && (reachability.has_dependency_failed_edge || reachability.has_guard_blocked_edge)
            {
                if let Some(step) = snapshot.steps.get_mut(&step_id) {
                    step.status = KernelStepStatus::Skipped;
                    step.retry_wait_until_ms = None;
                    step.skip_reason = Some(if reachability.has_dependency_failed_edge {
                        KernelSkipReason::DependencyFailed
                    } else {
                        KernelSkipReason::GuardBlocked
                    });
                    changed.push(step_id);
                }
            }
        }
        changed
    }

    pub(crate) fn next_retry_wait(
        &self,
        step: &transist_protocol::RecipeStep,
        state: &KernelStepState,
    ) -> Option<u64> {
        // Protocol violations and unconfirmed adapter teardown are terminal even
        // with retry budget left. Trust canonical codes, not plugin guidance.
        if state
            .last_result
            .as_ref()
            .and_then(|result| result.error.as_ref())
            .is_some_and(|error| {
                matches!(
                    error.code,
                    transist_protocol::CanonicalErrorCode::ProtocolError
                        | transist_protocol::CanonicalErrorCode::AdapterInternalError
                )
            })
        {
            return None;
        }
        let retry_max = step.with.get("retry_max").and_then(|value| match value {
            KernelValue::Number(value) => value.as_u64(),
            _ => None,
        })?;
        if state.activation_executions() > retry_max as u32 {
            return None;
        }
        let wait_ms = step
            .with
            .get("retry_wait_ms")
            .and_then(|value| match value {
                KernelValue::Number(value) => value.as_u64(),
                _ => None,
            })
            .unwrap_or(0);
        Some(self.services.clock.now_unix_ms().saturating_add(wait_ms))
    }

    pub(crate) fn execute_control_step(
        &self,
        home_dir: &Path,
        run_id: &str,
        step: &transist_protocol::RecipeStep,
        execution: u32,
        activation_execution: u32,
        previous: Option<&ExecutionResult>,
    ) -> ExecutionResult {
        let primitive = match &step.execution {
            StepExecutionDescriptor::Control { primitive } => primitive.clone(),
            StepExecutionDescriptor::Invoke { .. } => "invoke".to_string(),
            StepExecutionDescriptor::Subflow { .. } => "subflow".to_string(),
        };
        let mut status = "success".to_string();
        let mut output = serde_json::json!({ "primitive": primitive });
        let mut stderr = String::new();

        if primitive == ControlStepKind::GateManual.as_wire_str() {
            match validated_gate_id_for_step(step) {
                Ok(gate_id) => {
                    output["gate_id"] = serde_json::Value::String(gate_id.clone());
                    match self.load_reviewed_gate_decision(
                        home_dir,
                        run_id,
                        step,
                        execution,
                        previous,
                        &mut output,
                    ) {
                        Ok(Some(record)) => {
                            output["decision"] = serde_json::Value::String(record.decision.clone());
                            output["actor"] = serde_json::Value::String(
                                record
                                    .actor
                                    .as_deref()
                                    .map(str::trim)
                                    .unwrap_or("")
                                    .to_string(),
                            );
                            output["actor_source"] = serde_json::Value::String(
                                record
                                    .actor_source
                                    .as_deref()
                                    .map(str::trim)
                                    .unwrap_or("")
                                    .to_string(),
                            );
                            output["reason"] = serde_json::Value::String(
                                record
                                    .reason
                                    .as_deref()
                                    .map(str::trim)
                                    .unwrap_or("")
                                    .to_string(),
                            );
                            if let Some(decided_at_ms) = record.decided_at_ms {
                                output["decided_at_ms"] =
                                    serde_json::Value::Number(decided_at_ms.into());
                            }
                            if record.decision == "rejected" {
                                status = "failed".to_string();
                                stderr = record
                                    .reason
                                    .unwrap_or_else(|| format!("gate '{}' was rejected", gate_id));
                            }
                            // A consumed decision must never reappear: a
                            // deletion or directory-sync failure is surfaced as
                            // a failed step rather than silently discarded.
                            if let Err(error) = consume_gate_decision_record(
                                self.services.run_store.as_ref(),
                                home_dir,
                                run_id,
                                &gate_id,
                            ) {
                                status = "failed".to_string();
                                stderr = error.to_string();
                            }
                        }
                        Ok(None) => {
                            status = "blocked".to_string();
                        }
                        Err(error) => {
                            status = "failed".to_string();
                            stderr = error.to_string();
                        }
                    }
                }
                Err(error) => {
                    status = "failed".to_string();
                    stderr = error.to_string();
                }
            }
        }

        if primitive == "control.noop" {
            if let Some(KernelValue::Object(loop_meta)) = step.with.get(LOOP_META_KEY) {
                if let Some(KernelValue::Number(max_iters)) = loop_meta.get("max_iters") {
                    if max_iters.as_u64().unwrap_or_default() < activation_execution as u64 {
                        status = "failed".to_string();
                        stderr = "loop max iterations exceeded".to_string();
                    }
                }
            }
        }

        ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: format!("control_{run_id}_{}", step.id),
            run_id: run_id.to_string(),
            step_id: step.id.clone(),
            execution,
            status,
            exit_code: Some(if stderr.is_empty() { 0 } else { 1 }),
            stdout: String::new(),
            stderr,
            timing: transist_protocol::InvokeTiming {
                started_at: self.services.clock.now_unix_ms().to_string(),
                finished_at: self.services.clock.now_unix_ms().to_string(),
                duration_ms: 0,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output,
            metrics_validation_errors: 0,
            usage: None,
            error: None,
        }
    }
}

pub(crate) fn kernel_step_status_name(status: KernelStepStatus) -> &'static str {
    match status {
        KernelStepStatus::Pending => "pending",
        KernelStepStatus::Runnable => "runnable",
        KernelStepStatus::Dispatching => "dispatching",
        KernelStepStatus::Running => "running",
        KernelStepStatus::Blocked => "blocked",
        KernelStepStatus::Succeeded => "succeeded",
        KernelStepStatus::Failed => "failed",
        KernelStepStatus::RetryWait => "retry_wait",
        KernelStepStatus::Skipped => "skipped",
        KernelStepStatus::Canceled => "canceled",
    }
}
