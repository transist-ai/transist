// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use serde_json::{Value, to_string_pretty};
use transist::{BlobFamily, EventListQuery, InspectionEvent, SortOrder, TransistInspectionContext};

use crate::FAILED_STEP_EVENT_PAGER_LIMIT;
use crate::display::output::{blob_refs_to_paths, workflow_file_paths};

pub(super) fn build_failed_step_details_page(
    context: &transist::TransistContext,
    run_id: &str,
    step_id: &str,
) -> Result<(String, String)> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let Some(step) = inspection
        .list_steps(run_id)?
        .and_then(|steps| steps.into_iter().find(|step| step.step_id == step_id))
    else {
        anyhow::bail!("failed step not found");
    };
    let artifacts = blob_refs_to_paths(
        context.home_dir(),
        run_id,
        BlobFamily::Artifact,
        &step.output_summary.artifact_refs,
    );
    let evidence = blob_refs_to_paths(
        context.home_dir(),
        run_id,
        BlobFamily::Evidence,
        &step.output_summary.evidence_refs,
    );
    let workflow_files = workflow_file_paths(&step.output_summary.workflow_files);
    let payload = serde_json::json!({
        "run_id": run_id,
        "step_id": step.step_id,
        "step_kind": step.step_kind,
        "status": step.status,
        "execution": step.execution,
        "started_at_ms": step.started_at_ms,
        "finished_at_ms": step.finished_at_ms,
        "input_summary": {
            "request_present": step.input_summary.request_present,
            "invoke_id": step.input_summary.invoke_id,
            "adapter_kind": step.input_summary.adapter_kind,
            "plugin_id": step.input_summary.plugin_id,
            "action": step.input_summary.action,
            "input_object_keys": step.input_summary.input_object_keys,
            "metadata_object_keys": step.input_summary.metadata_object_keys,
            "filesystem_policy": step.input_summary.filesystem_policy,
        },
        "output_summary": {
            "result_present": step.output_summary.result_present,
            "invoke_id": step.output_summary.invoke_id,
            "status": step.output_summary.status,
            "exit_code": step.output_summary.exit_code,
            "error_code": step.output_summary.error_code,
            "error_message": step.output_summary.error_message,
            "child_run_id": step.output_summary.child_run_id,
            "output_object_keys": step.output_summary.output_object_keys,
            "artifact_count": step.output_summary.artifact_count,
            "evidence_count": step.output_summary.evidence_count,
            "workflow_file_paths": workflow_files,
            "artifact_paths": artifacts,
            "evidence_paths": evidence,
        }
    });
    Ok((
        format!("Failed step details: {} -> {}", run_id, step_id),
        to_string_pretty(&payload)?,
    ))
}

pub(super) fn build_failed_step_events_page(
    context: &transist::TransistContext,
    run_id: &str,
    step_id: &str,
    limit: usize,
) -> Result<(String, String)> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let events = inspection
        .list_events(
            run_id,
            &EventListQuery {
                limit: FAILED_STEP_EVENT_PAGER_LIMIT.max(limit),
                before: None,
                after: None,
                order: SortOrder::Desc,
            },
        )?
        .map(|page| page.data)
        .unwrap_or_default()
        .into_iter()
        .filter(|event| event.step_id.as_deref() == Some(step_id))
        .take(limit)
        .map(inspection_event_to_json)
        .collect::<Vec<_>>();
    Ok((
        format!("Failed step events: {} -> {}", run_id, step_id),
        to_string_pretty(&Value::Array(events))?,
    ))
}

fn inspection_event_to_json(event: InspectionEvent) -> Value {
    serde_json::json!({
        "event_id": event.event_id,
        "timestamp_unix_ms": event.timestamp_unix_ms,
        "event_type": event.event_type,
        "step_id": event.step_id,
        "payload": Value::from(event.payload),
    })
}

/// Page builder for `InspectFailedStepStderr`. Locates the stderr-style
/// artifact on disk (matching the same naming heuristics the kernel uses for
/// its `stderr_link`) and renders the full file contents in a pager. Falls
/// back to a clear error message when the artifact has been pruned or the
/// file is missing.
pub(super) fn build_failed_step_stderr_page(
    context: &transist::TransistContext,
    run_id: &str,
    step_id: &str,
    artifact_relative_path: &str,
) -> Result<(String, String)> {
    let inspection = TransistInspectionContext::with_home(
        context.repo_root().to_path_buf(),
        context.home_dir().to_path_buf(),
    );
    let Some(step) = inspection
        .list_steps(run_id)?
        .and_then(|steps| steps.into_iter().find(|step| step.step_id == step_id))
    else {
        anyhow::bail!("failed step not found");
    };
    let Some(artifact) = step
        .output_summary
        .artifact_refs
        .iter()
        .find(|artifact| artifact.relative_path == artifact_relative_path)
    else {
        anyhow::bail!(
            "stderr artifact '{}' not found on failed step",
            artifact_relative_path
        );
    };
    let run_root = context.run_root_or_default(run_id)?;
    let path = run_root
        .join("artifacts")
        .join(&artifact.step_id)
        .join(format!("execution_{:03}", artifact.execution))
        .join(BlobFamily::Artifact.dir_name())
        .join(&artifact.relative_path);
    let body = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => format!(
            "(unable to read stderr artifact at {}: {})",
            path.display(),
            err
        ),
    };
    Ok((
        format!("Failed step stderr: {} -> {}", run_id, step_id),
        body,
    ))
}
