// SPDX-License-Identifier: MPL-2.0
//! Subflow execution and subflow-state mapping helpers.

use super::step_execution::kernel_step_status_name;
use super::*;
use crate::domain::taxonomy::{RUN_ID_NESTING_SEPARATOR, SUBFLOW_META_KEY};
use crate::{RecipeInputBindingError, bind_recipe_inputs};
use serde::{Deserialize, Serialize};

impl RuntimeKernel {
    pub(crate) fn execute_subflow_step(
        &self,
        context: SubflowExecutionContext<'_>,
    ) -> Result<ExecutionResult> {
        let SubflowExecutionContext {
            paths,
            run_id,
            step,
            execution,
            rerun_completed_run,
            previous_child_run_id,
            recipe,
            workflow_inputs,
            workflow_vars,
            snapshot,
            replay_source_run_id,
        } = context;
        let prefix = &step.id;
        let child_run_id = subflow_child_run_id(
            run_id,
            prefix,
            execution,
            rerun_completed_run,
            previous_child_run_id.as_deref(),
            step,
        );
        let resolved_child_inputs = match crate::resolve_action_value(
            &KernelValue::Object(subflow_provided_inputs(&step.with)),
            workflow_inputs,
            workflow_vars,
            Some(snapshot),
            false,
        ) {
            Ok(KernelValue::Object(inputs)) => inputs,
            Ok(_) => unreachable!("child input object"),
            Err(error) => {
                return Ok(subflow_start_failed_result(
                    run_id,
                    step,
                    execution,
                    recipe,
                    &child_run_id,
                    &BTreeMap::new(),
                    &error,
                ));
            }
        };
        let provided_child_inputs = subflow_provided_inputs(&resolved_child_inputs);
        let status = if let Some(record) =
            load_run_record(self.services.run_store.as_ref(), &child_run_id)?
        {
            let rebound = match bind_recipe_inputs(recipe, provided_child_inputs.clone()) {
                Ok(inputs) => inputs,
                Err(error) => {
                    return Ok(subflow_input_binding_failed_result(
                        run_id,
                        step,
                        execution,
                        recipe,
                        &child_run_id,
                        &provided_child_inputs,
                        &error,
                    ));
                }
            };
            if rebound != record.workflow_inputs {
                return Ok(subflow_start_failed_result(
                    run_id,
                    step,
                    execution,
                    recipe,
                    &child_run_id,
                    &rebound,
                    &anyhow::anyhow!("reused child inputs changed; author rerun_mode: fresh"),
                ));
            }
            if rerun_completed_run
                && record.status == KernelRunStatus::Success
                && !subflow_fresh_child_on_rerun(step)
            {
                let summary =
                    self.rerun_started_run(paths.repo_root, paths.home_dir, &child_run_id)?;
                subflow_status_from_run_status(summary.status)
            } else if matches!(
                record.status,
                KernelRunStatus::Failed | KernelRunStatus::Canceled
            ) {
                let summary = self.resume_run(paths.repo_root, paths.home_dir, &child_run_id)?;
                subflow_status_from_run_status(summary.status)
            } else if record.status == KernelRunStatus::Blocked
                && !run_tree_has_pending_gate_decision(
                    self.services.run_store.as_ref(),
                    paths.home_dir,
                    &child_run_id,
                )
            {
                subflow_status_from_run_status(record.status)
            } else if subflow_requires_reentry(record.status) {
                let summary =
                    self.run_started_run(paths.repo_root, paths.home_dir, &child_run_id)?;
                subflow_status_from_run_status(summary.status)
            } else {
                subflow_status_from_run_status(record.status)
            }
        } else {
            let bound_child_inputs = match bind_recipe_inputs(recipe, provided_child_inputs.clone())
            {
                Ok(bound_child_inputs) => bound_child_inputs,
                Err(error) => {
                    return Ok(subflow_input_binding_failed_result(
                        run_id,
                        step,
                        execution,
                        recipe,
                        &child_run_id,
                        &provided_child_inputs,
                        &error,
                    ));
                }
            };
            let started_run_id = match self.start_embedded_subflow_run(
                paths,
                run_id,
                &step.id,
                recipe,
                &child_run_id,
                bound_child_inputs.clone(),
                replay_source_run_id,
            ) {
                Ok(started_run_id) => started_run_id,
                Err(error) => {
                    if error
                        .chain()
                        .any(|cause| cause.is::<crate::PluginLifecycleReservationCanceled>())
                    {
                        return Ok(subflow_start_canceled_result(
                            run_id,
                            step,
                            execution,
                            recipe,
                            &child_run_id,
                            &bound_child_inputs,
                            &error,
                        ));
                    }
                    return Ok(subflow_start_failed_result(
                        run_id,
                        step,
                        execution,
                        recipe,
                        &child_run_id,
                        &bound_child_inputs,
                        &error,
                    ));
                }
            };
            let summary = self.run_started_run(paths.repo_root, paths.home_dir, &started_run_id)?;
            subflow_status_from_run_status(summary.status)
        };
        let child_record = load_run_record(self.services.run_store.as_ref(), &child_run_id)?
            .ok_or_else(|| anyhow::anyhow!("subflow child run '{}' disappeared", child_run_id))?;
        let child_steps = subflow_child_steps_output(&child_record);
        let bound_inputs = child_record.workflow_inputs.clone();
        let mut output = serde_json::json!({
            "subflow_id": recipe.workflow.id,
            "subflow_version": recipe.workflow.version,
            "child_run_id": child_run_id,
            "status": status,
            "bound_inputs": serde_json::Value::from(KernelValue::Object(bound_inputs)),
            "child_steps": child_steps,
        });
        if status == "success"
            && let Some(KernelValue::Object(metadata)) = step.with.get(SUBFLOW_META_KEY)
            && let Some(KernelValue::Object(exports)) = metadata.get("outputs")
        {
            let child_snapshot = child_record.snapshot();
            for (name, binding) in exports {
                match crate::resolve_action_value(
                    binding,
                    &child_record.workflow_inputs,
                    &child_record.workflow_vars,
                    Some(&child_snapshot),
                    false,
                ) {
                    Ok(value) => {
                        output[name] = serde_json::Value::from(value);
                    }
                    Err(error) => {
                        return Ok(subflow_start_failed_result(
                            run_id,
                            step,
                            execution,
                            recipe,
                            &child_run_id,
                            &child_record.workflow_inputs,
                            &error,
                        ));
                    }
                }
            }
        }
        if status == "failed"
            && let Some(failure) =
                subflow_failure_descriptor(&child_record).map(SubflowFailureDescriptor::into_json)
            && let Some(object) = output.as_object_mut()
        {
            object.insert("failure".to_string(), failure);
        }
        Ok(ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: format!("subflow_{run_id}_{prefix}"),
            run_id: run_id.to_string(),
            step_id: step.id.clone(),
            execution,
            status: status.to_string(),
            exit_code: Some(if status == "success" { 0 } else { 1 }),
            stdout: String::new(),
            stderr: String::new(),
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
        })
    }

    #[allow(clippy::too_many_arguments)] // Parent and replay lineage inputs are distinct kernel state.
    fn start_embedded_subflow_run(
        &self,
        paths: RuntimePaths<'_>,
        parent_run_id: &str,
        parent_step_id: &str,
        recipe: &Recipe,
        child_run_id: &str,
        workflow_inputs: BTreeMap<String, KernelValue>,
        replay_source_run_id: Option<&str>,
    ) -> Result<String> {
        let parent_record = load_run_record(self.services.run_store.as_ref(), parent_run_id)?
            .ok_or_else(|| anyhow::anyhow!("run '{}' not found", parent_run_id))?;
        let parent_recipe = self.load_persisted_recipe(parent_run_id, &parent_record)?;
        let root_run_id = parent_record
            .lineage
            .as_ref()
            .map(|lineage| lineage.root_run_id.trim())
            .filter(|root_run_id| !root_run_id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| parent_run_id.to_string());
        let (mode, source_run_id, preset_asset_locks, preset_workflow_assets) =
            match replay_source_run_id {
                Some(source_parent_run_id) => {
                    let source_parent =
                        load_run_record(self.services.run_store.as_ref(), source_parent_run_id)?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "replay source parent run '{}' not found",
                                    source_parent_run_id
                                )
                            })?;
                    let source_child_run_id = source_parent
                    .steps
                    .get(parent_step_id)
                    .and_then(|step| step.last_result.as_ref())
                    .and_then(|result| result.output.get("child_run_id"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!(
                        "replay source parent run '{}' has no child run for subflow step '{}'",
                        source_parent_run_id,
                        parent_step_id
                    ))?;
                    let source_child =
                        load_run_record(self.services.run_store.as_ref(), source_child_run_id)?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "replay source child run '{}' not found",
                                    source_child_run_id
                                )
                            })?;
                    let source_recipe =
                        self.load_persisted_recipe(source_child_run_id, &source_child)?;
                    self.verify_asset_locks(&source_child, &source_recipe, paths.home_dir)?;
                    (
                        RunLineageMode::Replay,
                        Some(source_child_run_id.to_string()),
                        Some((
                            source_child.asset_lock_version,
                            source_child.asset_locks.clone(),
                        )),
                        self.load_workflow_asset_bundle(source_child_run_id, &source_child)?,
                    )
                }
                None => {
                    let preset_workflow_assets = if parent_record.trusted_execution_mode
                        && parent_record.uses_workflow_asset_snapshot()
                    {
                        self.load_workflow_asset_bundle(parent_run_id, &parent_record)?
                            .map(|bundle| {
                                super::run_execution::select_preset_workflow_assets(
                                    &parent_recipe,
                                    recipe,
                                    bundle,
                                )
                            })
                            .transpose()?
                            .flatten()
                    } else {
                        None
                    };
                    (RunLineageMode::Fresh, None, None, preset_workflow_assets)
                }
            };
        let record = self.start_recipe_internal(
            paths.repo_root,
            paths.home_dir,
            recipe,
            Some(child_run_id),
            workflow_inputs,
            parent_record.trusted_execution_mode,
            parent_record.trust_decision.clone(),
            parent_record.local_only_mode,
            RunLineage {
                mode,
                source_run_id,
                root_run_id,
                parent_run_id: Some(parent_run_id.to_string()),
                parent_step_id: Some(parent_step_id.to_string()),
            },
            preset_asset_locks,
            preset_workflow_assets,
        )?;
        Ok(record.run_id)
    }
}

fn subflow_start_canceled_result(
    run_id: &str,
    step: &transist_protocol::RecipeStep,
    execution: u32,
    recipe: &Recipe,
    child_run_id: &str,
    bound_child_inputs: &BTreeMap<String, KernelValue>,
    error: &anyhow::Error,
) -> ExecutionResult {
    let message = error.to_string();
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("subflow_{run_id}_{}", step.id),
        run_id: run_id.to_string(),
        step_id: step.id.clone(),
        execution,
        status: "cancelled".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: message.clone(),
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({
            "subflow_id": recipe.workflow.id,
            "subflow_version": recipe.workflow.version,
            "child_run_id": child_run_id,
            "status": "cancelled",
            "bound_inputs": serde_json::Value::from(KernelValue::Object(bound_child_inputs.clone())),
            "child_steps": {},
        }),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError::new(CanonicalErrorCode::Cancelled, message)),
    }
}

fn subflow_input_binding_failed_result(
    run_id: &str,
    step: &transist_protocol::RecipeStep,
    execution: u32,
    recipe: &Recipe,
    child_run_id: &str,
    provided_child_inputs: &BTreeMap<String, KernelValue>,
    error: &RecipeInputBindingError,
) -> ExecutionResult {
    let message = error.to_string();
    let failure = SubflowFailureDescriptor {
        child_run_id: child_run_id.to_string(),
        failed_step_id: None,
        error_code: Some("subflow_input_binding_failed".to_string()),
        error_message: compact_failure_message(Some(message.as_str())),
        stderr: None,
        preflight_blocker_count: None,
        preflight_warning_count: None,
        preflight_first_blocker: None,
    };
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("subflow_{run_id}_{}", step.id),
        run_id: run_id.to_string(),
        step_id: step.id.clone(),
        execution,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: message.clone(),
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({
            "subflow_id": recipe.workflow.id,
            "subflow_version": recipe.workflow.version,
            "status": "failed",
            "bound_inputs": serde_json::Value::from(KernelValue::Object(provided_child_inputs.clone())),
            "child_steps": {},
            "failure": failure.into_json(),
        }),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError::new(
            CanonicalErrorCode::SubflowInputBindingFailed,
            message,
        )),
    }
}

fn subflow_start_failed_result(
    run_id: &str,
    step: &transist_protocol::RecipeStep,
    execution: u32,
    recipe: &Recipe,
    child_run_id: &str,
    bound_child_inputs: &BTreeMap<String, KernelValue>,
    error: &anyhow::Error,
) -> ExecutionResult {
    let message = error.to_string();
    let failure = SubflowFailureDescriptor {
        child_run_id: child_run_id.to_string(),
        failed_step_id: None,
        error_code: Some("subflow_start_failed".to_string()),
        error_message: compact_failure_message(Some(message.as_str())),
        stderr: None,
        preflight_blocker_count: None,
        preflight_warning_count: None,
        preflight_first_blocker: None,
    };
    ExecutionResult {
        message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
        protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
        invoke_id: format!("subflow_{run_id}_{}", step.id),
        run_id: run_id.to_string(),
        step_id: step.id.clone(),
        execution,
        status: "failed".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: message.clone(),
        timing: transist_protocol::InvokeTiming {
            started_at: "0".to_string(),
            finished_at: "0".to_string(),
            duration_ms: 0,
        },
        artifacts: Vec::new(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        output: serde_json::json!({
            "subflow_id": recipe.workflow.id,
            "subflow_version": recipe.workflow.version,
            "status": "failed",
            "bound_inputs": serde_json::Value::from(KernelValue::Object(bound_child_inputs.clone())),
            "child_steps": {},
            "failure": failure.into_json(),
        }),
        metrics_validation_errors: 0,
        usage: None,
        error: Some(ProtocolError::new(
            CanonicalErrorCode::AdapterInternalError,
            message,
        )),
    }
}

/// Removes runtime-only subflow metadata from resolved child inputs.
pub fn subflow_provided_inputs(
    resolved_child_inputs: &BTreeMap<String, KernelValue>,
) -> BTreeMap<String, KernelValue> {
    resolved_child_inputs
        .iter()
        .filter(|(key, _)| !key.starts_with("__transist_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn subflow_child_run_id(
    run_id: &str,
    step_id: &str,
    execution: u32,
    rerun_completed_run: bool,
    previous_child_run_id: Option<&str>,
    step: &transist_protocol::RecipeStep,
) -> String {
    if rerun_completed_run && subflow_fresh_child_on_rerun(step) {
        return format!(
            "{run_id}{RUN_ID_NESTING_SEPARATOR}{step_id}{RUN_ID_NESTING_SEPARATOR}r{execution:03}"
        );
    }
    previous_child_run_id
        .map(str::to_string)
        .unwrap_or_else(|| format!("{run_id}{RUN_ID_NESTING_SEPARATOR}{step_id}"))
}

fn subflow_fresh_child_on_rerun(step: &transist_protocol::RecipeStep) -> bool {
    let Some(KernelValue::Object(meta)) = step.with.get(SUBFLOW_META_KEY) else {
        return false;
    };
    matches!(
        meta.get("rerun_mode"),
        Some(KernelValue::String(value)) if value.trim() == "fresh"
    )
}

pub(crate) fn subflow_status_from_run_status(status: KernelRunStatus) -> &'static str {
    match status {
        KernelRunStatus::Success => "success",
        KernelRunStatus::Failed => "failed",
        KernelRunStatus::Canceled => "cancelled",
        KernelRunStatus::Created
        | KernelRunStatus::Planned
        | KernelRunStatus::Running
        | KernelRunStatus::Waiting
        | KernelRunStatus::Blocked => "blocked",
    }
}

pub(crate) fn subflow_requires_reentry(status: KernelRunStatus) -> bool {
    matches!(
        status,
        KernelRunStatus::Created
            | KernelRunStatus::Planned
            | KernelRunStatus::Running
            | KernelRunStatus::Waiting
            | KernelRunStatus::Blocked
    )
}

pub(crate) fn subflow_child_steps_output(record: &PersistedRunRecord) -> serde_json::Value {
    serde_json::Value::Object(
        record
            .steps
            .iter()
            .map(|(step_id, step)| {
                (
                    step_id.clone(),
                    serde_json::json!({
                        "status": kernel_step_status_name(step.status),
                        "executions": step.executions,
                        "result": {
                            "status": step
                                .last_result
                                .as_ref()
                                .map(|result| result.status.clone())
                                .unwrap_or_else(|| "unknown".to_string()),
                        },
                        "out": step
                            .last_result
                            .as_ref()
                            .map(|result| result.output.clone())
                            .unwrap_or(serde_json::Value::Null),
                    }),
                )
            })
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SubflowFailureDescriptor {
    child_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preflight_blocker_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preflight_warning_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preflight_first_blocker: Option<String>,
}

impl SubflowFailureDescriptor {
    fn into_json(self) -> serde_json::Value {
        serde_json::to_value(self).expect("subflow failure serialization must be infallible")
    }
}

fn subflow_failure_descriptor(record: &PersistedRunRecord) -> Option<SubflowFailureDescriptor> {
    // Prefer failed steps that carry a `last_result` so we can extract a
    // concrete reason. Fall back to any failed step so we can at least point
    // at it if none of them persisted a result yet.
    if let Some(failed_step) = failed_step_with_result(record) {
        // Safe: `failed_step_with_result` only yields steps whose `last_result`
        // is set.
        let result = failed_step.last_result.as_ref()?;
        if let Some(nested_failure) = result
            .output
            .get("failure")
            .and_then(subflow_failure_descriptor_from_value)
        {
            return Some(nested_failure);
        }

        return Some(SubflowFailureDescriptor {
            child_run_id: record.run_id.clone(),
            failed_step_id: Some(failed_step.step_id.clone()),
            error_code: result
                .error
                .as_ref()
                .and_then(|error| serde_json::to_value(error.code).ok())
                .and_then(|value| value.as_str().map(str::to_string)),
            error_message: compact_failure_message(
                result.error.as_ref().map(|error| error.message.as_str()),
            )
            .or_else(|| compact_failure_message(Some(result.stderr.as_str()))),
            stderr: compact_failure_message(Some(result.stderr.as_str())),
            preflight_blocker_count: None,
            preflight_warning_count: None,
            preflight_first_blocker: None,
        });
    }

    if let Some(failed_step) = latest_failed_step(record) {
        return Some(SubflowFailureDescriptor {
            child_run_id: record.run_id.clone(),
            failed_step_id: Some(failed_step.step_id.clone()),
            error_code: None,
            error_message: None,
            stderr: None,
            preflight_blocker_count: None,
            preflight_warning_count: None,
            preflight_first_blocker: None,
        });
    }

    if failed_in_preflight_without_failed_step(record) {
        let (preflight_blocker_count, preflight_warning_count, preflight_first_blocker) = record
            .preflight
            .as_ref()
            .map(|preflight| {
                (
                    Some(preflight.blocker_count as u32),
                    Some(preflight.warning_count as u32),
                    preflight
                        .checks
                        .iter()
                        .find(|check| check.status == PreflightStatus::Failed)
                        .and_then(|check| compact_failure_message(Some(check.message.as_str()))),
                )
            })
            .unwrap_or((None, None, None));
        return Some(SubflowFailureDescriptor {
            child_run_id: record.run_id.clone(),
            failed_step_id: None,
            error_code: None,
            error_message: Some("preflight failed before step dispatch".to_string()),
            stderr: None,
            preflight_blocker_count,
            preflight_warning_count,
            preflight_first_blocker,
        });
    }

    None
}

fn subflow_failure_descriptor_from_value(
    value: &serde_json::Value,
) -> Option<SubflowFailureDescriptor> {
    let mut failure: SubflowFailureDescriptor = serde_json::from_value(value.clone()).ok()?;
    failure.child_run_id = normalize_non_empty(&failure.child_run_id)?;
    failure.failed_step_id = failure
        .failed_step_id
        .and_then(|value| normalize_non_empty(&value));
    failure.error_code = failure
        .error_code
        .and_then(|value| normalize_non_empty(&value));
    failure.error_message = compact_failure_message(failure.error_message.as_deref());
    failure.stderr = compact_failure_message(failure.stderr.as_deref());
    failure.preflight_first_blocker =
        compact_failure_message(failure.preflight_first_blocker.as_deref());
    Some(failure)
}

/// Return the failed step whose latest `finished_at`/`started_at` is the
/// largest. When timestamps are equal or absent, breaks ties by execution
/// counter and then step id so the choice is deterministic across map
/// iteration orders.
fn latest_failed_step(record: &PersistedRunRecord) -> Option<&KernelStepState> {
    record
        .steps
        .values()
        .filter(|step| step_failed(step))
        .max_by(compare_failed_step_recency)
}

/// Same as [`latest_failed_step`] but restricted to steps that persisted a
/// `last_result`, so a compact failure descriptor can be extracted. Prefer
/// this so a still-transient failed step (no result yet) does not shadow a
/// sibling that already carries a durable reason.
fn failed_step_with_result(record: &PersistedRunRecord) -> Option<&KernelStepState> {
    record
        .steps
        .values()
        .filter(|step| step_failed(step) && step.last_result.is_some())
        .max_by(compare_failed_step_recency)
}

fn compare_failed_step_recency(
    left: &&KernelStepState,
    right: &&KernelStepState,
) -> std::cmp::Ordering {
    let left_key = (
        left.finished_at_ms.or(left.started_at_ms).unwrap_or(0),
        left.executions,
        left.step_id.as_str(),
    );
    let right_key = (
        right.finished_at_ms.or(right.started_at_ms).unwrap_or(0),
        right.executions,
        right.step_id.as_str(),
    );
    left_key.cmp(&right_key)
}

fn step_failed(step: &KernelStepState) -> bool {
    step.status == KernelStepStatus::Failed
        || step
            .last_result
            .as_ref()
            .map(|result| result.status == "failed")
            .unwrap_or(false)
}

fn failed_in_preflight_without_failed_step(record: &PersistedRunRecord) -> bool {
    record.status == KernelRunStatus::Failed
        && latest_failed_step(record).is_none()
        && record
            .preflight
            .as_ref()
            .map(|preflight| preflight.status == PreflightStatus::Failed)
            .unwrap_or(false)
}

fn compact_failure_message(message: Option<&str>) -> Option<String> {
    message
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .and_then(|message| {
            message
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })
}

fn normalize_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn run_tree_has_pending_gate_decision(
    store: &dyn RunStorePort,
    home_dir: &Path,
    run_id: &str,
) -> bool {
    if run_has_pending_gate_decision(store, home_dir, run_id) {
        return true;
    }
    let Ok(Some(record)) = load_run_record(store, run_id) else {
        return false;
    };
    record.steps.values().any(|step| {
        step.last_result
            .as_ref()
            .and_then(|result| result.output.get("child_run_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|child_run_id| run_tree_has_pending_gate_decision(store, home_dir, child_run_id))
            .unwrap_or(false)
    })
}

fn run_has_pending_gate_decision(store: &dyn RunStorePort, home_dir: &Path, run_id: &str) -> bool {
    store.has_pending_gate_decision(home_dir, run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{
        CanonicalErrorCode, ErrorClass, ExecutionResult, InvokeTiming, ProtocolError, Retryability,
        TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
    };

    fn minimal_recipe(id: &str, version: &str) -> Recipe {
        Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: transist_protocol::WorkflowRef {
                id: id.to_string(),
                version: version.to_string(),
            },
            source_dir: None,
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps: Vec::new(),
            cleanup: Vec::new(),
            graph: None,
            capabilities: Default::default(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    fn control_recipe_step(id: &str) -> transist_protocol::RecipeStep {
        transist_protocol::RecipeStep {
            id: id.to_string(),
            execution: transist_protocol::StepExecutionDescriptor::Control {
                primitive: "control.noop".to_string(),
            },
            with: BTreeMap::new(),
            prompt_binding: None,
            skills_binding: None,
            vars: BTreeMap::new(),
            filesystem_policy: Default::default(),
            network_policy: Default::default(),
            effective_filesystem_policy: Default::default(),
            effective_filesystem_policy_source: None,
            effective_network_policy: Default::default(),
            effective_network_policy_source: None,
        }
    }

    #[test]
    fn subflow_input_binding_failed_result_builds_failed_execution_result() {
        // Directly covers the execution-time binding-failure builder, which
        // fires when the parent subflow step resolves child inputs at dispatch
        // time (after preflight could not evaluate them) and binding rejects
        // them before any child run is created.
        let recipe = minimal_recipe("child-inputs", "v1");
        let step = control_recipe_step("child");
        let provided = BTreeMap::from([(
            "unexpected".to_string(),
            KernelValue::String("boom".to_string()),
        )]);
        let error = RecipeInputBindingError {
            workflow_id: "child-inputs".to_string(),
            workflow_version: "v1".to_string(),
            declared_inputs: vec!["required_ticket".to_string()],
            kind: crate::RecipeInputBindingErrorKind::UndeclaredInput {
                input_name: "unexpected".to_string(),
            },
        };

        let result = subflow_input_binding_failed_result(
            "parent-run",
            &step,
            1,
            &recipe,
            "parent-run__child",
            &provided,
            &error,
        );

        assert_eq!(result.status, "failed");
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.step_id, "child");
        assert_eq!(
            result.error.as_ref().map(|error| error.code),
            Some(CanonicalErrorCode::SubflowInputBindingFailed)
        );
        assert_eq!(
            result.output["failure"]["error_code"],
            "subflow_input_binding_failed"
        );
        assert_eq!(
            result.output["failure"]["error_message"],
            "workflow input 'unexpected' is not declared by 'child-inputs@v1'"
        );
        assert_eq!(
            result.output["failure"]["child_run_id"],
            "parent-run__child"
        );
        assert_eq!(result.output["bound_inputs"]["unexpected"], "boom");
        assert_eq!(result.output["status"], "failed");
    }

    #[test]
    fn subflow_start_failed_result_builds_failed_execution_result() {
        let recipe = minimal_recipe("child-start", "v1");
        let step = control_recipe_step("child");
        let bound_inputs = BTreeMap::from([(
            "ticket".to_string(),
            KernelValue::String("HB-186".to_string()),
        )]);

        let result = subflow_start_failed_result(
            "parent-run",
            &step,
            1,
            &recipe,
            "parent-run__child",
            &bound_inputs,
            &anyhow::anyhow!("run start denied by policy"),
        );

        assert_eq!(result.status, "failed");
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(
            result.error.as_ref().map(|error| error.code),
            Some(CanonicalErrorCode::AdapterInternalError)
        );
        assert_eq!(
            result.output["failure"]["error_code"],
            "subflow_start_failed"
        );
        assert_eq!(
            result.output["failure"]["child_run_id"],
            "parent-run__child"
        );
        assert_eq!(result.output["bound_inputs"]["ticket"], "HB-186");
    }

    fn failed_execution_result(
        run_id: &str,
        step_id: &str,
        error_message: &str,
        stderr: &str,
    ) -> ExecutionResult {
        ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: format!("{run_id}-{step_id}-001"),
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
            execution: 1,
            status: "failed".to_string(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: stderr.to_string(),
            timing: InvokeTiming {
                started_at: "1".to_string(),
                finished_at: "2".to_string(),
                duration_ms: 1,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output: serde_json::json!({}),
            metrics_validation_errors: 0,
            usage: None,
            error: Some(ProtocolError {
                code: CanonicalErrorCode::ToolExecutionError,
                class: ErrorClass::Tool,
                message: error_message.to_string(),
                retryability: Retryability::Safe,
            }),
        }
    }

    fn failed_run_record(
        run_id: &str,
        step_id: &str,
        result: ExecutionResult,
    ) -> PersistedRunRecord {
        let mut snapshot = KernelRunSnapshot::new(run_id, "wf", "v1", &[step_id.to_string()]);
        let step = snapshot.steps.get_mut(step_id).expect("step");
        step.status = KernelStepStatus::Failed;
        step.executions = 1;
        step.started_at_ms = Some(1);
        step.finished_at_ms = Some(2);
        step.last_result = Some(result);
        snapshot.status = KernelRunStatus::Failed;
        PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 2)
    }

    #[test]
    fn builds_compact_failure_descriptor_from_failed_leaf_step() {
        let record = failed_run_record(
            "child-run",
            "nested",
            failed_execution_result(
                "child-run",
                "nested",
                "first error line\nsecond line",
                "first stderr line\nsecond stderr line",
            ),
        );

        assert_eq!(
            subflow_failure_descriptor(&record),
            Some(SubflowFailureDescriptor {
                child_run_id: "child-run".to_string(),
                failed_step_id: Some("nested".to_string()),
                error_code: Some("tool_execution_error".to_string()),
                error_message: Some("first error line".to_string()),
                stderr: Some("first stderr line".to_string()),
                preflight_blocker_count: None,
                preflight_warning_count: None,
                preflight_first_blocker: None,
            })
        );
    }

    #[test]
    fn propagates_nested_failure_descriptor_when_present() {
        let mut result = failed_execution_result("child-run", "nested-subflow", "outer", "outer");
        result.output = serde_json::json!({
            "failure": {
                "child_run_id": "grandchild-run",
                "failed_step_id": "leaf",
                "error_code": "tool_execution_error",
                "error_message": "deep failure\nignored",
                "stderr": "deep stderr\nignored"
            }
        });
        let record = failed_run_record("child-run", "nested-subflow", result);

        assert_eq!(
            subflow_failure_descriptor(&record),
            Some(SubflowFailureDescriptor {
                child_run_id: "grandchild-run".to_string(),
                failed_step_id: Some("leaf".to_string()),
                error_code: Some("tool_execution_error".to_string()),
                error_message: Some("deep failure".to_string()),
                stderr: Some("deep stderr".to_string()),
                preflight_blocker_count: None,
                preflight_warning_count: None,
                preflight_first_blocker: None,
            })
        );
    }

    #[test]
    fn builds_preflight_failure_descriptor_without_failed_step() {
        let mut snapshot = KernelRunSnapshot::new("child-run", "wf", "v1", &["nested".to_string()]);
        snapshot.status = KernelRunStatus::Failed;
        let mut record =
            PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 1);
        record.preflight = Some(PersistedPreflightRecord::from_checks(
            1,
            vec![PreflightCheck {
                kind: "policy".to_string(),
                status: PreflightStatus::Failed,
                step_ids: vec!["nested".to_string()],
                adapter_kind: Some("process".to_string()),
                plugin_id: Some("shell".to_string()),
                action: Some("echo".to_string()),
                message: "denied".to_string(),
            }],
        ));

        assert_eq!(
            subflow_failure_descriptor(&record),
            Some(SubflowFailureDescriptor {
                child_run_id: "child-run".to_string(),
                failed_step_id: None,
                error_code: None,
                error_message: Some("preflight failed before step dispatch".to_string()),
                stderr: None,
                preflight_blocker_count: Some(1),
                preflight_warning_count: Some(0),
                preflight_first_blocker: Some("denied".to_string()),
            })
        );
    }

    #[test]
    fn preflight_failure_descriptor_compacts_first_blocker_message() {
        let mut snapshot = KernelRunSnapshot::new("child-run", "wf", "v1", &["nested".to_string()]);
        snapshot.status = KernelRunStatus::Failed;
        let mut record =
            PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 1);
        record.preflight = Some(PersistedPreflightRecord::from_checks(
            1,
            vec![PreflightCheck {
                kind: "policy".to_string(),
                status: PreflightStatus::Failed,
                step_ids: vec!["nested".to_string()],
                adapter_kind: Some("process".to_string()),
                plugin_id: Some("shell".to_string()),
                action: Some("echo".to_string()),
                message: "first blocker line\nsecond line".to_string(),
            }],
        ));

        let descriptor = subflow_failure_descriptor(&record).expect("descriptor");

        assert_eq!(descriptor.preflight_blocker_count, Some(1));
        assert_eq!(descriptor.preflight_warning_count, Some(0));
        assert_eq!(
            descriptor.preflight_first_blocker.as_deref(),
            Some("first blocker line")
        );
    }

    #[test]
    fn prefers_failed_step_with_result_over_transient_failed_step() {
        // Two failed steps: `pending_result` marked failed but with no
        // persisted result yet, and `has_result` marked failed with a
        // populated `last_result`. The descriptor must extract the concrete
        // reason from `has_result` instead of returning `None` because the
        // resultless sibling happens to have a later timestamp.
        let mut snapshot = KernelRunSnapshot::new(
            "child-run",
            "wf",
            "v1",
            &["has_result".to_string(), "pending_result".to_string()],
        );
        snapshot.status = KernelRunStatus::Failed;

        if let Some(has_result) = snapshot.steps.get_mut("has_result") {
            has_result.status = KernelStepStatus::Failed;
            has_result.executions = 1;
            has_result.started_at_ms = Some(10);
            has_result.finished_at_ms = Some(20);
            has_result.last_result = Some(failed_execution_result(
                "child-run",
                "has_result",
                "real error",
                "real stderr",
            ));
        }
        if let Some(pending_result) = snapshot.steps.get_mut("pending_result") {
            pending_result.status = KernelStepStatus::Failed;
            pending_result.executions = 1;
            pending_result.started_at_ms = Some(30);
            pending_result.finished_at_ms = Some(40);
        }

        let record =
            PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 2);

        assert_eq!(
            subflow_failure_descriptor(&record),
            Some(SubflowFailureDescriptor {
                child_run_id: "child-run".to_string(),
                failed_step_id: Some("has_result".to_string()),
                error_code: Some("tool_execution_error".to_string()),
                error_message: Some("real error".to_string()),
                stderr: Some("real stderr".to_string()),
                preflight_blocker_count: None,
                preflight_warning_count: None,
                preflight_first_blocker: None,
            })
        );
    }

    #[test]
    fn tie_breaks_failed_steps_deterministically_when_timestamps_match() {
        // Two failed steps with identical finished timestamps must resolve to
        // a stable choice regardless of the underlying step map iteration
        // order. The tie-breaker uses (finished_at, executions, step_id), so
        // the higher-execution attempt with the lexicographically larger id
        // wins.
        let mut snapshot = KernelRunSnapshot::new(
            "child-run",
            "wf",
            "v1",
            &["alpha".to_string(), "beta".to_string()],
        );
        snapshot.status = KernelRunStatus::Failed;

        if let Some(alpha) = snapshot.steps.get_mut("alpha") {
            alpha.status = KernelStepStatus::Failed;
            alpha.executions = 1;
            alpha.started_at_ms = Some(10);
            alpha.finished_at_ms = Some(20);
            alpha.last_result = Some(failed_execution_result(
                "child-run",
                "alpha",
                "alpha error",
                "alpha stderr",
            ));
        }
        if let Some(beta) = snapshot.steps.get_mut("beta") {
            beta.status = KernelStepStatus::Failed;
            beta.executions = 1;
            beta.started_at_ms = Some(10);
            beta.finished_at_ms = Some(20);
            beta.last_result = Some(failed_execution_result(
                "child-run",
                "beta",
                "beta error",
                "beta stderr",
            ));
        }

        let record =
            PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 2);

        assert_eq!(
            subflow_failure_descriptor(&record).and_then(|d| d.failed_step_id),
            Some("beta".to_string()),
            "with equal timestamps and executions the larger step id wins"
        );
    }

    #[test]
    fn points_at_failed_step_even_without_last_result() {
        // A failed step with no persisted result should still surface as a
        // pointer-only descriptor (`failed_step_id` populated, no reason)
        // instead of returning `None`. This keeps the fallback resilient
        // against runs where the runtime marked the step failed before it
        // wrote the invoke result to disk.
        let mut snapshot =
            KernelRunSnapshot::new("child-run", "wf", "v1", &["lonely_failed".to_string()]);
        snapshot.status = KernelRunStatus::Failed;
        if let Some(step) = snapshot.steps.get_mut("lonely_failed") {
            step.status = KernelStepStatus::Failed;
            step.executions = 1;
            step.started_at_ms = Some(5);
            step.finished_at_ms = Some(10);
        }
        let record =
            PersistedRunRecord::from_snapshot(snapshot, None, None, BTreeMap::new(), 0, 1, 2);

        assert_eq!(
            subflow_failure_descriptor(&record),
            Some(SubflowFailureDescriptor {
                child_run_id: "child-run".to_string(),
                failed_step_id: Some("lonely_failed".to_string()),
                error_code: None,
                error_message: None,
                stderr: None,
                preflight_blocker_count: None,
                preflight_warning_count: None,
                preflight_first_blocker: None,
            })
        );
    }
}
