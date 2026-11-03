// SPDX-License-Identifier: MPL-2.0
//! Step-focused inspection helpers.

use super::*;
use crate::runs::{
    active_child_status_for_step, current_step_id as run_local_current_step_id,
    is_active_run_status, is_terminal_run_status, ordered_step_ids,
};
use std::path::{Path, PathBuf};
use transist_storage_local::run_paths::{
    home_dir_from_run_root, lineage_root_run_dir_from_run_root, runs_root,
};

pub(crate) fn build_steps(
    store: &LocalInspectionStore,
    record: &PersistedRunRecord,
    run_id: &str,
    stored_summaries: &[PersistedRunSummaryRecord],
    projected_by_id: &BTreeMap<String, InspectionRunSummary>,
) -> Result<Vec<InspectionStep>> {
    let recipe = store.load_run_recipe(run_id)?;
    let artifacts = store.list_blobs(run_id, BlobFamily::Artifact)?;
    let evidence = store.list_blobs(run_id, BlobFamily::Evidence)?;
    let mut artifact_refs = BTreeMap::<(String, u32), Vec<InspectionStepBlobRef>>::new();
    let mut evidence_refs = BTreeMap::<(String, u32), Vec<InspectionStepBlobRef>>::new();
    for blob in artifacts {
        artifact_refs
            .entry((blob.step_id.clone(), blob.execution))
            .or_default()
            .push(step_blob_ref_from_indexed_blob(blob));
    }
    for blob in evidence {
        evidence_refs
            .entry((blob.step_id.clone(), blob.execution))
            .or_default()
            .push(step_blob_ref_from_indexed_blob(blob));
    }

    let ordered_step_ids = ordered_step_ids(record, recipe.as_ref());
    // Prefer the already-projected current step from the cached summary maps so
    // we avoid the FS-walking `effective_run_projection` on the hot path used
    // by the TUI run-detail loader. Falls back to a summary-derived projection
    // when this run is not represented in the projected map yet (e.g. tests).
    let current =
        current_step_id_from_projection(record, run_id, stored_summaries, projected_by_id);
    let mut steps = Vec::new();
    for step_id in ordered_step_ids {
        let Some(state) = record.steps.get(&step_id) else {
            continue;
        };
        let execution_history = store.list_step_executions(run_id, &step_id)?;
        let latest_output = build_latest_output(run_id, &step_id, state, &execution_history)?;
        let latest_execution = execution_history
            .iter()
            .max_by_key(|record| record.execution);
        let selected_execution = latest_execution
            .as_ref()
            .map(|execution| execution.execution)
            .or_else(|| (state.executions > 0).then_some(state.executions));
        let request = latest_execution
            .as_ref()
            .and_then(|execution| execution.request.as_ref())
            .or(state.last_request.as_ref());
        let result = latest_execution
            .as_ref()
            .and_then(|execution| execution.result.as_ref())
            .or(state.last_result.as_ref());
        let artifact_refs_for_execution = selected_execution
            .and_then(|execution| artifact_refs.get(&(step_id.clone(), execution)).cloned())
            .unwrap_or_default();
        let evidence_refs_for_execution = selected_execution
            .and_then(|execution| evidence_refs.get(&(step_id.clone(), execution)).cloned())
            .unwrap_or_default();
        let recipe_step = recipe
            .as_ref()
            .and_then(|recipe| recipe.steps.iter().find(|step| step.id == step_id));
        let child_status = active_child_status_for_step_from_summaries(
            stored_summaries,
            projected_by_id,
            run_id,
            &step_id,
            record.status == KernelRunStatus::Success,
        );
        let status = match child_status {
            Some(status) => status,
            None if stored_summaries.is_empty() => active_child_status_for_step(
                store,
                run_id,
                &step_id,
                record.status == KernelRunStatus::Success,
            )?
            .unwrap_or_else(|| step_status_name(state.status).to_string()),
            None => step_status_name(state.status).to_string(),
        };

        // Load the complete execution history for this step, sorted ascending
        // by execution number. This is the append-only record that survives
        // reruns, retries, and reused subflow re-dispatches.
        let input_summary = build_step_input_summary(request);
        let output_summary = build_step_output_summary(
            request,
            result,
            artifact_refs_for_execution,
            evidence_refs_for_execution,
        );
        let inspection_executions: Vec<InspectionStepExecution> = execution_history
            .into_iter()
            .map(|record| InspectionStepExecution {
                execution: record.execution,
                status: record.status,
                started_at_ms: record.started_at_ms,
                finished_at_ms: record.finished_at_ms,
                request: record.request,
                result: record.result,
                state_mutations: record.state_mutations,
            })
            .collect();

        steps.push(InspectionStep {
            step_id: step_id.clone(),
            step_kind: recipe_step
                .map(step_kind_from_recipe)
                .unwrap_or_else(|| "unknown".to_string()),
            status,
            execution: selected_execution,
            started_at_ms: state.started_at_ms,
            finished_at_ms: state.finished_at_ms,
            input_summary,
            output_summary,
            is_current_active: current.as_deref() == Some(step_id.as_str()),
            latest_output,
            executions: inspection_executions,
        });
    }
    Ok(steps)
}

fn build_latest_output(
    run_id: &str,
    step_id: &str,
    state: &KernelStepState,
    records: &[transist_core::StepExecutionRecord],
) -> Result<Option<InspectionStepOutputSnapshot>> {
    for record in records {
        anyhow::ensure!(
            record.run_id == run_id && record.step_id == step_id,
            "step execution identity disagrees with requested run/step"
        );
        if let Some(result) = &record.result {
            anyhow::ensure!(
                result.run_id == run_id
                    && result.step_id == step_id
                    && result.execution == record.execution,
                "step execution result identity disagrees with execution record"
            );
        }
        if let Some(request) = &record.request {
            anyhow::ensure!(
                request.run_id == run_id
                    && request.step.id == step_id
                    && request.step.execution == record.execution,
                "step execution request identity disagrees with execution record"
            );
        }
    }
    let ordinal = records
        .iter()
        .map(|record| record.execution)
        .max()
        .unwrap_or(0)
        .max(state.executions);
    let execution = (ordinal > 0).then_some(ordinal);
    let record =
        execution.and_then(|ordinal| records.iter().find(|record| record.execution == ordinal));
    let (status, source, result) = if let Some(record) = record {
        (
            record.status.clone(),
            "execution_record",
            record.result.as_ref(),
        )
    } else {
        let result = state.last_result.as_ref().filter(|result| {
            execution == Some(result.execution)
                && result.run_id == run_id
                && result.step_id == step_id
                && matches!(
                    state.status,
                    KernelStepStatus::Succeeded
                        | KernelStepStatus::Failed
                        | KernelStepStatus::RetryWait
                        | KernelStepStatus::Canceled
                )
        });
        (
            step_status_name(state.status).to_string(),
            "state_snapshot",
            result,
        )
    };
    if execution.is_none() && state.last_result.is_none() {
        return Ok(None);
    }
    Ok(Some(InspectionStepOutputSnapshot {
        execution,
        status,
        source: source.to_string(),
        result_present: result.is_some(),
        value: result.map(|result| result.output.clone()),
    }))
}

fn current_step_id_from_projection(
    record: &PersistedRunRecord,
    run_id: &str,
    stored_summaries: &[PersistedRunSummaryRecord],
    projected_by_id: &BTreeMap<String, InspectionRunSummary>,
) -> Option<String> {
    // Fast path: the projected summary map is already computed by the caller
    // and carries the effective current step rolled up from active embedded
    // children. This avoids the FS-walking `effective_run_projection` on the
    // hot TUI run-detail path.
    if let Some(summary) = projected_by_id.get(run_id) {
        if let Some(step_id) = summary.current_step_id.clone() {
            return Some(step_id);
        }
        if is_terminal_run_status(&summary.status) {
            return None;
        }
    }
    // Fallback: derive locally from the persisted record without touching the
    // filesystem; further, scan stored summaries for an active embedded child
    // to recover the parent's current step when the record itself is terminal.
    let local = (!record.status.is_terminal())
        .then(|| run_local_current_step_id(&record.steps))
        .flatten();
    if local.is_some() {
        return local;
    }
    most_active_child_step_from_summaries(stored_summaries, run_id)
}

fn most_active_child_step_from_summaries(
    stored_summaries: &[PersistedRunSummaryRecord],
    run_id: &str,
) -> Option<String> {
    let mut best: Option<(u64, String)> = None;
    for child in stored_summaries {
        let parent_match = child.parent_run_id.as_deref() == Some(run_id);
        let direct_match = direct_embedded_step_id(run_id, &child.run_id).is_some();
        if !parent_match && !direct_match {
            continue;
        }
        if !is_active_run_status(&child.status) {
            continue;
        }
        let step_id = child
            .parent_step_id
            .clone()
            .or_else(|| direct_embedded_step_id(run_id, &child.run_id).map(str::to_string))?;
        match &best {
            Some((updated_at, _)) if *updated_at >= child.updated_at_ms => {}
            _ => best = Some((child.updated_at_ms, step_id)),
        }
    }
    best.map(|(_, step_id)| step_id)
}

fn active_child_status_for_step_from_summaries(
    stored_summaries: &[PersistedRunSummaryRecord],
    projected_by_id: &BTreeMap<String, InspectionRunSummary>,
    run_id: &str,
    step_id: &str,
    parent_is_irreversibly_done: bool,
) -> Option<String> {
    if parent_is_irreversibly_done {
        return None;
    }

    let mut best: Option<(u64, String)> = None;
    for child in stored_summaries {
        let matches_parent_from_lineage = child.parent_run_id.as_deref() == Some(run_id)
            && child.parent_step_id.as_deref() == Some(step_id);
        let matches_parent_from_id =
            direct_embedded_step_id(run_id, &child.run_id) == Some(step_id);
        let matches_parent = matches_parent_from_lineage || matches_parent_from_id;
        if !matches_parent {
            continue;
        }
        let status = projected_by_id
            .get(&child.run_id)
            .map(|summary| summary.status.as_str())
            .unwrap_or(child.status.as_str());
        if !is_active_run_status(status) {
            continue;
        }
        match &best {
            Some((updated_at, _)) if *updated_at >= child.updated_at_ms => {}
            _ => best = Some((child.updated_at_ms, status.to_string())),
        }
    }
    best.map(|(_, status)| status)
}

fn direct_embedded_step_id<'a>(run_id: &'a str, child_run_id: &'a str) -> Option<&'a str> {
    let prefix = format!("{run_id}__");
    let remainder = child_run_id.strip_prefix(&prefix)?;
    (!remainder.is_empty() && !remainder.contains("__")).then_some(remainder)
}

fn build_step_input_summary(request: Option<&ExecutionRequest>) -> InspectionStepInputSummary {
    let filesystem_policy = request.and_then(|request| {
        request.policy.get("filesystem").and_then(|value| {
            let workspace_access =
                serde_json::from_value(value.get("workspace_access")?.clone()).ok()?;
            let read_allow_paths = value
                .get("read_allow_paths")
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default();
            let write_allow_paths = value
                .get("write_allow_paths")
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default();
            Some(InspectionFilesystemPolicySummary {
                source: value
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                workspace_root: value
                    .get("workspace_root")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                workspace_access,
                read_allow_paths,
                write_allow_paths,
            })
        })
    });
    InspectionStepInputSummary {
        request_present: request.is_some(),
        invoke_id: request.map(|request| request.invoke_id.clone()),
        adapter_kind: request.map(|request| request.adapter_kind.to_string()),
        plugin_id: request.and_then(|request| {
            request
                .metadata
                .get("plugin_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }),
        action: request.map(|request| request.action.clone()),
        input_object_keys: request
            .map(|request| sorted_object_keys(&request.input))
            .unwrap_or_default(),
        metadata_object_keys: request
            .map(|request| sorted_object_keys(&request.metadata))
            .unwrap_or_default(),
        filesystem_policy,
    }
}

fn build_step_output_summary(
    request: Option<&ExecutionRequest>,
    result: Option<&ExecutionResult>,
    artifact_refs: Vec<InspectionStepBlobRef>,
    evidence_refs: Vec<InspectionStepBlobRef>,
) -> InspectionStepOutputSummary {
    let workflow_files = collect_workflow_files(request, result);
    InspectionStepOutputSummary {
        result_present: result.is_some(),
        invoke_id: result.map(|result| result.invoke_id.clone()),
        status: result.map(|result| result.status.clone()),
        exit_code: result.and_then(|result| result.exit_code),
        error_code: result.and_then(step_error_code),
        error_message: result.and_then(step_error_message),
        stderr_tail: result.and_then(step_stderr_tail),
        child_run_id: result.and_then(step_child_run_id),
        child_failure: result.and_then(step_child_failure),
        output_object_keys: result
            .map(|result| sorted_object_keys(&result.output))
            .unwrap_or_default(),
        artifact_count: artifact_refs.len(),
        evidence_count: evidence_refs.len(),
        artifact_refs,
        evidence_refs,
        workflow_files,
    }
}

fn step_error_code(result: &ExecutionResult) -> Option<String> {
    result
        .error
        .as_ref()
        .and_then(|error| serde_json::to_value(error.code).ok())
        .and_then(|value| value.as_str().map(str::to_string))
}

fn step_error_message(result: &ExecutionResult) -> Option<String> {
    if let Some(error) = result.error.as_ref() {
        let message = error.message.trim();
        if !message.is_empty() {
            return Some(message.to_string());
        }
    }
    let stderr = result.stderr.trim();
    if result.status != "success" && !stderr.is_empty() {
        return Some(stderr.to_string());
    }
    None
}

/// Maximum number of characters retained in the stderr tail shown to operators.
///
/// Chosen to fit in a single TUI Details-pane line at typical widths while
/// still preserving enough context for paths and short diagnostic messages.
const STDERR_TAIL_MAX_LEN: usize = 300;

/// Extract a compact, single-line tail of the result's stderr when stderr is
/// non-empty.
///
/// Returns the last non-empty trimmed line of stderr, truncated to roughly
/// `STDERR_TAIL_MAX_LEN` characters (appending a trailing `…` when truncated).
/// Returns `None` when stderr has no non-empty lines after trimming. The check
/// is independent of step status so that callers may always render stderr
/// context (e.g. on warning-style outcomes), and independent of `error.message`
/// so that generic protocol errors do not mask the real diagnostic.
fn step_stderr_tail(result: &ExecutionResult) -> Option<String> {
    let stderr = result.stderr.as_str();
    let last_non_empty = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())?;
    if last_non_empty.chars().count() <= STDERR_TAIL_MAX_LEN {
        return Some(last_non_empty.to_string());
    }
    let truncated: String = last_non_empty.chars().take(STDERR_TAIL_MAX_LEN).collect();
    Some(format!("{truncated}…"))
}

fn step_child_run_id(result: &ExecutionResult) -> Option<String> {
    result
        .output
        .get("child_run_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn step_child_failure(result: &ExecutionResult) -> Option<InspectionChildFailure> {
    let failure = result.output.get("failure")?;
    Some(InspectionChildFailure {
        child_run_id: failure
            .get("child_run_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)?,
        failed_step_id: failure
            .get("failed_step_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        error_code: failure
            .get("error_code")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        error_message: failure
            .get("error_message")
            .and_then(serde_json::Value::as_str)
            .and_then(compact_message),
        stderr: failure
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .and_then(compact_message),
        preflight_blocker_count: failure
            .get("preflight_blocker_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        preflight_warning_count: failure
            .get("preflight_warning_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        preflight_first_blocker: failure
            .get("preflight_first_blocker")
            .and_then(serde_json::Value::as_str)
            .and_then(compact_message),
    })
}

fn compact_message(message: &str) -> Option<String> {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn step_blob_ref_from_indexed_blob(blob: IndexedBlob) -> InspectionStepBlobRef {
    InspectionStepBlobRef {
        step_id: blob.step_id,
        execution: blob.execution,
        name: Path::new(&blob.relative_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&blob.relative_path)
            .to_string(),
        relative_path: blob.relative_path,
        bytes: blob.bytes,
    }
}

const REQUEST_WORKFLOW_FILE_KEYS: &[&str] = &[
    "output_file",
    "response_file",
    "report_file",
    "content_file",
    "summary_title_file",
    "pr_body_file",
    "change_metadata_file",
];

const RESULT_WORKFLOW_FILE_KEYS: &[&str] = &[
    "output_file",
    "response_file",
    "report_file",
    "content_file",
    "proposal_file",
    "tasks_file",
    "design_file",
    "summary_title_file",
    "pr_body_file",
    "change_metadata_file",
];

const RESULT_WORKFLOW_FILE_LIST_KEYS: &[&str] = &["spec_files"];

fn collect_workflow_files(
    request: Option<&ExecutionRequest>,
    result: Option<&ExecutionResult>,
) -> Vec<InspectionWorkflowFileRef> {
    let Some(request) = request else {
        return Vec::new();
    };

    let mut refs = Vec::new();
    let mut path_memo = BTreeMap::<String, Option<ResolvedWorkflowPath>>::new();
    append_workflow_files_from_object(
        &mut refs,
        request,
        request.input.as_object(),
        REQUEST_WORKFLOW_FILE_KEYS,
        &[],
        &mut path_memo,
    );
    append_workflow_files_from_object(
        &mut refs,
        request,
        result.and_then(|result| result.output.as_object()),
        RESULT_WORKFLOW_FILE_KEYS,
        RESULT_WORKFLOW_FILE_LIST_KEYS,
        &mut path_memo,
    );
    refs
}

#[derive(Clone)]
struct ResolvedWorkflowPath {
    locator_kind: InspectionWorkflowFileLocatorKind,
    relative_path: String,
    local_path: Option<PathBuf>,
    name: String,
    bytes: u64,
}

fn append_workflow_files_from_object(
    output: &mut Vec<InspectionWorkflowFileRef>,
    request: &ExecutionRequest,
    object: Option<&serde_json::Map<String, serde_json::Value>>,
    single_keys: &[&str],
    list_keys: &[&str],
    path_memo: &mut BTreeMap<String, Option<ResolvedWorkflowPath>>,
) {
    let Some(object) = object else {
        return;
    };

    for key in single_keys {
        let Some(raw_path) = object.get(*key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if let Some(reference) = workflow_file_ref(request, key, raw_path, path_memo) {
            push_unique_workflow_file(output, reference);
        }
    }

    for key in list_keys {
        let Some(values) = object.get(*key).and_then(serde_json::Value::as_array) else {
            continue;
        };
        for raw_path in values.iter().filter_map(serde_json::Value::as_str) {
            if let Some(reference) = workflow_file_ref(request, key, raw_path, path_memo) {
                push_unique_workflow_file(output, reference);
            }
        }
    }
}

fn push_unique_workflow_file(
    output: &mut Vec<InspectionWorkflowFileRef>,
    reference: InspectionWorkflowFileRef,
) {
    if output.iter().any(|existing| {
        existing.locator_kind == reference.locator_kind
            && existing.relative_path == reference.relative_path
            && existing.field_name == reference.field_name
    }) {
        return;
    }
    output.push(reference);
}

fn workflow_file_ref(
    request: &ExecutionRequest,
    field_name: &str,
    raw_path: &str,
    path_memo: &mut BTreeMap<String, Option<ResolvedWorkflowPath>>,
) -> Option<InspectionWorkflowFileRef> {
    let resolved = path_memo
        .entry(raw_path.to_string())
        .or_insert_with(|| resolve_workflow_file_ref(request, raw_path))
        .as_ref()?;
    Some(InspectionWorkflowFileRef {
        step_id: request.step.id.clone(),
        field_name: field_name.to_string(),
        locator_kind: resolved.locator_kind,
        relative_path: resolved.relative_path.clone(),
        local_path: resolved.local_path.clone(),
        name: resolved.name.clone(),
        bytes: resolved.bytes,
    })
}

/// Anchor for a resolved workflow file read.
///
/// `Run` reads are descriptor-anchored at the Transist home so every run-tree
/// component is crossed one `openat` at a time with `O_NOFOLLOW`, closing the
/// root-swap race that a pathname-based `open(root)` would leave open. `Path`
/// reads anchor at an already-trusted single directory (workspace, repository
/// state, or repository source root).
enum WorkflowAnchor {
    Run {
        home_dir: PathBuf,
        run_relative_path: String,
    },
    Path {
        root: PathBuf,
    },
}

fn resolve_workflow_file_ref(
    request: &ExecutionRequest,
    raw_path: &str,
) -> Option<ResolvedWorkflowPath> {
    let (locator_kind, relative_path, anchor) = resolve_workflow_locator(request, raw_path)?;
    let bytes = match &anchor {
        WorkflowAnchor::Run {
            home_dir,
            run_relative_path,
        } => crate::secure_file::read_relative(home_dir, run_relative_path, &relative_path),
        WorkflowAnchor::Path { root } => {
            crate::secure_file::read_relative_to_root(root, &relative_path)
        }
    }
    .ok()?;
    if bytes.is_empty() {
        return None;
    }
    // Use the same validated anchor that supplied the file metadata. Do not
    // resolve a locator against the inspecting terminal's working directory.
    let local_path = match &anchor {
        WorkflowAnchor::Run {
            home_dir,
            run_relative_path,
        } => runs_root(home_dir)
            .join(run_relative_path)
            .join(&relative_path),
        WorkflowAnchor::Path { root } => root.join(&relative_path),
    };
    let local_path = local_path.is_absolute().then_some(local_path);
    let name = Path::new(&relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(raw_path)
        .to_string();
    Some(ResolvedWorkflowPath {
        locator_kind,
        relative_path,
        local_path,
        name,
        bytes: bytes.len() as u64,
    })
}

fn resolve_workflow_locator(
    request: &ExecutionRequest,
    raw_path: &str,
) -> Option<(InspectionWorkflowFileLocatorKind, String, WorkflowAnchor)> {
    let candidate = raw_path.trim();
    if candidate.is_empty() {
        return None;
    }
    if let Some(rest) = candidate.strip_prefix("run://") {
        let anchor = run_anchor_from_run_root(&request.paths.run_root)?;
        let relative = normalize_relative_locator(rest)?;
        return Some((InspectionWorkflowFileLocatorKind::Run, relative, anchor));
    }
    if let Some(rest) = candidate.strip_prefix("repo://") {
        let repo_path = request
            .input
            .get("repo_path")
            .and_then(serde_json::Value::as_str)?;
        if Path::new(repo_path.trim()).is_absolute() {
            // Repository-state files are exposed only for repositories anchored
            // beneath the configured workspace root.
            return None;
        }
        let root = resolve_repo_root_path(&request.paths.workspace_dir, Some(repo_path))?
            .join(".transist");
        let relative = normalize_relative_locator(rest)?;
        return Some((
            InspectionWorkflowFileLocatorKind::RepositoryState,
            relative,
            WorkflowAnchor::Path { root },
        ));
    }
    let run_anchor = run_anchor_from_run_root(&request.paths.run_root);
    let workspace_root = PathBuf::from(&request.paths.workspace_dir);
    if is_absolute_portable_path(candidate) {
        if let Some(WorkflowAnchor::Run {
            home_dir,
            run_relative_path,
        }) = &run_anchor
        {
            let run_root_path = runs_root(home_dir).join(run_relative_path);
            if let Some(relative) = relative_to_root(candidate, &run_root_path) {
                return Some((
                    InspectionWorkflowFileLocatorKind::Run,
                    relative,
                    WorkflowAnchor::Run {
                        home_dir: home_dir.clone(),
                        run_relative_path: run_relative_path.clone(),
                    },
                ));
            }
        }
        let relative = relative_to_root(candidate, &workspace_root)?;
        return Some((
            InspectionWorkflowFileLocatorKind::Workspace,
            relative,
            WorkflowAnchor::Path {
                root: workspace_root,
            },
        ));
    }

    Some((
        InspectionWorkflowFileLocatorKind::Workspace,
        normalize_relative_locator(candidate)?,
        WorkflowAnchor::Path {
            root: workspace_root,
        },
    ))
}

/// Builds a descriptor-anchored [`WorkflowAnchor::Run`] from a persisted run
/// root path. The run root may be nested under `contained_runs/`, so the
/// anchor is expressed relative to `<home>/runs/` and traversed one component
/// at a time by the secure reader rather than opened as a single path.
fn run_anchor_from_run_root(run_root: &str) -> Option<WorkflowAnchor> {
    let run_root_path = Path::new(run_root);
    let lineage_root = lineage_root_run_dir_from_run_root(run_root_path).ok()?;
    let home_dir = home_dir_from_run_root(&lineage_root).ok()?;
    let runs_root_path = runs_root(&home_dir);
    let run_relative_path = lineage_root
        .strip_prefix(&runs_root_path)
        .ok()?
        .to_string_lossy()
        .to_string();
    Some(WorkflowAnchor::Run {
        home_dir,
        run_relative_path,
    })
}

fn relative_to_root(path: &str, root: &Path) -> Option<String> {
    let path = normalize_portable_path(path)?;
    let root = normalize_portable_path(&root.to_string_lossy())?;
    path.strip_prefix(&format!("{root}/"))
        .and_then(normalize_relative_locator)
}

fn normalize_relative_locator(raw: &str) -> Option<String> {
    let normalized = normalize_portable_path(raw)?;
    is_safe_relative_locator(&normalized).then_some(normalized)
}

fn normalize_portable_path(raw: &str) -> Option<String> {
    let mut value = raw.replace('\\', "/");
    while let Some(stripped) = value.strip_prefix("./") {
        value = stripped.to_string();
    }
    let drive = is_drive_absolute(&value).then(|| value[..2].to_string());
    let components_value = drive
        .as_deref()
        .map_or(value.as_str(), |drive| &value[drive.len() + 1..]);
    let absolute = value.starts_with('/') || drive.is_some();
    let mut components = Vec::new();
    for component in components_value.split('/') {
        match component {
            "" | "." => {}
            ".." if components.is_empty() => return None,
            ".." => {
                components.pop();
            }
            component => components.push(component),
        }
    }
    if components.is_empty() {
        return absolute.then_some("/".to_string());
    }
    let joined = components.join("/");
    Some(if absolute {
        if let Some(drive) = drive {
            format!("{drive}/{joined}")
        } else {
            format!("/{joined}")
        }
    } else {
        joined
    })
}

fn is_drive_absolute(value: &str) -> bool {
    value.len() >= 3
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1] == b':'
        && value.as_bytes()[2] == b'/'
}

fn is_absolute_portable_path(value: &str) -> bool {
    let normalized = value.replace('\\', "/");
    normalized.starts_with('/') || is_drive_absolute(&normalized)
}

fn is_safe_relative_locator(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !is_drive_absolute(value)
        && !value.starts_with("~/")
        && !value.split('/').any(|component| component == "..")
}

pub(crate) fn resolve_repo_root_path(
    workspace_dir: &str,
    repo_path: Option<&str>,
) -> Option<PathBuf> {
    let raw = repo_path?.trim();
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || is_drive_absolute(&raw.replace('\\', "/"))
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(Path::new(workspace_dir).join(path))
}

fn sorted_object_keys(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

pub(crate) fn step_kind_from_recipe(step: &RecipeStep) -> String {
    match &step.execution {
        transist_protocol::StepExecutionDescriptor::Invoke { .. } => "invoke".to_string(),
        transist_protocol::StepExecutionDescriptor::Control { .. } => "control".to_string(),
        transist_protocol::StepExecutionDescriptor::Subflow { .. } => "subflow".to_string(),
    }
}

pub(crate) fn step_status_name(status: KernelStepStatus) -> &'static str {
    match status {
        KernelStepStatus::Pending => "pending",
        KernelStepStatus::Runnable => "runnable",
        KernelStepStatus::Dispatching => "dispatching",
        KernelStepStatus::Running => "running",
        KernelStepStatus::Blocked => "blocked",
        KernelStepStatus::Succeeded => "success",
        KernelStepStatus::Failed => "failed",
        KernelStepStatus::RetryWait => "retry_wait",
        KernelStepStatus::Skipped => "skipped",
        KernelStepStatus::Canceled => "canceled",
    }
}

#[cfg(test)]
#[path = "step_output_tests.rs"]
mod output_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use transist_protocol::{
        AdapterKind, CanonicalErrorCode, ExecutionRequest, ExecutionResult, InvokePaths,
        InvokeStep, InvokeTiming, ProtocolError, TEP_INVOKE_REQUEST_TYPE_V1,
        TEP_INVOKE_RESULT_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
    };

    pub(super) fn inspection_request(input: serde_json::Value) -> ExecutionRequest {
        ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
            workflow_id: "workflow".to_string(),
            step: InvokeStep {
                id: "plan_loop".to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "exec".to_string(),
            input,
            paths: InvokePaths {
                run_root: "/tmp/.transist/runs/run_run_1/contained_runs/run_run_1__plan_loop"
                    .to_string(),
                artifacts_dir:
                    "/tmp/.transist/runs/run_run_1/contained_runs/run_run_1__plan_loop/artifacts"
                        .to_string(),
                evidence_dir:
                    "/tmp/.transist/runs/run_run_1/contained_runs/run_run_1__plan_loop/evidence"
                        .to_string(),
                workspace_dir: "/tmp/workspace".to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: None,
            max_timeout_ms: None,
            policy: serde_json::Value::Null,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn output_summary_exposes_failure_message_and_child_run_id() {
        let mut request = inspection_request(serde_json::json!({}));
        request.metadata = serde_json::json!({"plugin_id": "shell"});
        request.policy = serde_json::json!({
            "filesystem": {
                "source": "step",
                "workspace_root": "/tmp/workspace",
                "workspace_access": {
                    "mode": "read_only",
                }
            }
        });
        let result = ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
            step_id: "plan_loop".to_string(),
            execution: 1,
            status: "failed".to_string(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            timing: InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "1".to_string(),
                duration_ms: 1,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output: serde_json::json!({
                "child_run_id": "run_1__plan_loop",
                "status": "failed"
            }),
            metrics_validation_errors: 0,
            usage: None,
            error: Some(ProtocolError::new(
                CanonicalErrorCode::ValidationError,
                "child flow failed",
            )),
        };

        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());
        let input_summary = build_step_input_summary(Some(&request));

        assert_eq!(input_summary.plugin_id.as_deref(), Some("shell"));
        assert_eq!(
            input_summary
                .filesystem_policy
                .as_ref()
                .and_then(|policy| policy.source.as_deref()),
            Some("step")
        );
        assert_eq!(
            input_summary
                .filesystem_policy
                .as_ref()
                .map(|policy| policy.workspace_access.mode),
            Some(transist_protocol::WorkspaceAccessMode::ReadOnly)
        );
        assert_eq!(summary.error_code.as_deref(), Some("validation_error"));
        assert_eq!(summary.error_message.as_deref(), Some("child flow failed"));
        assert_eq!(summary.child_run_id.as_deref(), Some("run_1__plan_loop"));
        assert!(summary.child_failure.is_none());
    }

    #[test]
    fn output_summary_maps_persisted_child_failure() {
        let request = inspection_request(serde_json::json!({}));
        let result = ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
            step_id: "plan_loop".to_string(),
            execution: 1,
            status: "failed".to_string(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            timing: InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "1".to_string(),
                duration_ms: 1,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output: serde_json::json!({
                "child_run_id": "run_1__plan_loop",
                "failure": {
                    "child_run_id": "run_1__plan_loop__nested",
                    "failed_step_id": "draft",
                    "error_code": "tool_execution_error",
                    "error_message": "first line\nsecond line",
                    "stderr": "stderr line\nextra"
                }
            }),
            metrics_validation_errors: 0,
            usage: None,
            error: None,
        };

        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());

        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .map(|failure| failure.child_run_id.as_str()),
            Some("run_1__plan_loop__nested")
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.failed_step_id.as_deref()),
            Some("draft")
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.error_code.as_deref()),
            Some("tool_execution_error")
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.error_message.as_deref()),
            Some("first line")
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.stderr.as_deref()),
            Some("stderr line")
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.preflight_blocker_count),
            None
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.preflight_warning_count),
            None
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.preflight_first_blocker.as_deref()),
            None
        );
    }

    #[test]
    fn output_summary_maps_preflight_child_failure_enrichment() {
        let request = inspection_request(serde_json::json!({}));
        let result = ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
            step_id: "plan_loop".to_string(),
            execution: 1,
            status: "failed".to_string(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            timing: InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "1".to_string(),
                duration_ms: 1,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output: serde_json::json!({
                "child_run_id": "run_1__plan_loop",
                "failure": {
                    "child_run_id": "run_1__plan_loop",
                    "error_message": "preflight failed before step dispatch",
                    "preflight_blocker_count": 1,
                    "preflight_warning_count": 0,
                    "preflight_first_blocker": "input schema validation failed at instance path '/model' and schema path '/properties/model/type'\nignored"
                }
            }),
            metrics_validation_errors: 0,
            usage: None,
            error: None,
        };

        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());

        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.preflight_blocker_count),
            Some(1)
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.preflight_warning_count),
            Some(0)
        );
        assert_eq!(
            summary
                .child_failure
                .as_ref()
                .and_then(|failure| failure.preflight_first_blocker.as_deref()),
            Some(
                "input schema validation failed at instance path '/model' and schema path '/properties/model/type'"
            )
        );
    }

    #[test]
    fn output_summary_falls_back_to_stderr_for_failed_steps_without_protocol_error() {
        let request = inspection_request(serde_json::json!({}));
        let result = ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
            step_id: "generate_plan".to_string(),
            execution: 1,
            status: "failed".to_string(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "stderr failure".to_string(),
            timing: InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "1".to_string(),
                duration_ms: 1,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output: serde_json::json!({}),
            metrics_validation_errors: 0,
            usage: None,
            error: None,
        };

        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());

        assert_eq!(summary.error_code, None);
        assert_eq!(summary.error_message.as_deref(), Some("stderr failure"));
    }

    #[test]
    fn output_summary_collects_existing_non_empty_workflow_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp_root = fs::canonicalize(tmp.path()).expect("canonical tempdir");
        let workspace = temp_root.join("workspace");
        let run_root = temp_root.join(".transist/runs/run_run_1");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&run_root).expect("run root");
        let repo = workspace.join("project");
        fs::create_dir_all(repo.join(".transist")).expect("repo transist");
        fs::write(run_root.join("plan.md"), "plan").expect("plan");
        fs::write(repo.join(".transist/spec.md"), "spec").expect("spec");
        fs::create_dir_all(workspace.join("reports")).expect("reports");
        fs::write(workspace.join("reports/output with spaces.md"), "report").expect("report");

        let mut request = inspection_request(serde_json::json!({
            "repo_path": "project",
            "response_file": "run://plan.md",
        }));
        request.paths.run_root = run_root.display().to_string();
        request.paths.workspace_dir = workspace.display().to_string();
        let result = ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
            step_id: "plan_loop".to_string(),
            execution: 1,
            status: "success".to_string(),
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timing: InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "1".to_string(),
                duration_ms: 1,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output: serde_json::json!({
                "spec_files": ["repo://spec.md", "repo://missing.md"],
                "report_file": "reports/output with spaces.md",
            }),
            metrics_validation_errors: 0,
            usage: None,
            error: None,
        };

        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());

        assert_eq!(summary.workflow_files.len(), 3);
        assert!(
            summary
                .workflow_files
                .iter()
                .any(|file| file.field_name == "response_file" && file.name == "plan.md")
        );
        let run_file = summary
            .workflow_files
            .iter()
            .find(|file| file.field_name == "response_file")
            .expect("run locator");
        assert_eq!(
            run_file.locator_kind,
            InspectionWorkflowFileLocatorKind::Run
        );
        assert_eq!(run_file.relative_path, "plan.md");
        assert_eq!(
            run_file.local_path.as_ref(),
            Some(&run_root.join("plan.md"))
        );
        assert!(
            summary
                .workflow_files
                .iter()
                .any(|file| file.field_name == "spec_files" && file.name == "spec.md")
        );
        let repository_file = summary
            .workflow_files
            .iter()
            .find(|file| file.field_name == "spec_files")
            .expect("repository-state locator");
        assert_eq!(
            repository_file.locator_kind,
            InspectionWorkflowFileLocatorKind::RepositoryState
        );
        assert_eq!(repository_file.relative_path, "spec.md");
        let workspace_file = summary
            .workflow_files
            .iter()
            .find(|file| file.field_name == "report_file")
            .expect("workspace locator");
        assert_eq!(
            workspace_file.relative_path,
            "reports/output with spaces.md"
        );
        assert_eq!(
            workspace_file.local_path.as_ref(),
            Some(&workspace.join("reports/output with spaces.md"))
        );
        assert_eq!(
            repository_file.local_path.as_ref(),
            Some(&repo.join(".transist/spec.md"))
        );
        let serialized = serde_json::to_value(&summary).expect("serialize summary");
        for file in serialized["workflow_files"].as_array().expect("files") {
            assert!(file.get("local_path").is_none());
        }
        let round_trip: InspectionStepOutputSummary =
            serde_json::from_value(serialized).expect("deserialize summary");
        assert!(
            round_trip
                .workflow_files
                .iter()
                .all(|file| file.local_path.is_none())
        );
    }

    #[test]
    fn repo_locator_rejects_absolute_repo_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let repo = tmp.path().join("external-repo");
        fs::create_dir_all(repo.join(".transist")).expect("repo transist");
        fs::write(repo.join(".transist/spec.md"), "spec").expect("spec");

        let mut request = inspection_request(serde_json::json!({
            "repo_path": repo.display().to_string(),
        }));
        request.paths.workspace_dir = workspace.display().to_string();
        let result = ExecutionResult {
            output: serde_json::json!({"spec_files": ["repo://spec.md"]}),
            ..result_with_stderr("", "success", None)
        };

        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());
        assert!(summary.workflow_files.is_empty());
    }

    #[test]
    fn workflow_file_locators_reject_escape_and_normalize_windows_paths() {
        assert_eq!(
            normalize_relative_locator(r"reports\nested\output.md"),
            Some("reports/nested/output.md".to_string())
        );
        assert_eq!(
            relative_to_root(
                r"D:\build\workspace\reports\output.md",
                Path::new(r"D:\build\workspace")
            ),
            Some("reports/output.md".to_string())
        );
        assert_eq!(normalize_relative_locator("../outside.txt"), None);
        assert_eq!(
            normalize_relative_locator(r"C:\Users\alice\secret.txt"),
            None
        );
    }

    pub(super) fn result_with_stderr(
        stderr: &str,
        status: &str,
        error: Option<ProtocolError>,
    ) -> ExecutionResult {
        ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke_1".to_string(),
            run_id: "run_1".to_string(),
            step_id: "stderr_step".to_string(),
            execution: 1,
            status: status.to_string(),
            exit_code: Some(if status == "success" { 0 } else { 2 }),
            stdout: String::new(),
            stderr: stderr.to_string(),
            timing: InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "1".to_string(),
                duration_ms: 1,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            output: serde_json::json!({}),
            metrics_validation_errors: 0,
            usage: None,
            error,
        }
    }

    #[test]
    fn stderr_tail_populated_when_stderr_non_empty_regardless_of_error_message() {
        let request = inspection_request(serde_json::json!({}));
        let result = result_with_stderr(
            "first line\n\nFileNotFoundError: /tmp/missing.py\n",
            "failed",
            Some(ProtocolError::new(
                CanonicalErrorCode::ToolExecutionError,
                "command exited with code 2",
            )),
        );

        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());

        assert_eq!(
            summary.error_message.as_deref(),
            Some("command exited with code 2"),
            "error_message should still come from the protocol error"
        );
        assert_eq!(
            summary.stderr_tail.as_deref(),
            Some("FileNotFoundError: /tmp/missing.py"),
            "stderr_tail must surface the real diagnostic even when error_message is the generic wrapper"
        );
    }

    #[test]
    fn stderr_tail_is_none_when_stderr_blank() {
        let request = inspection_request(serde_json::json!({}));
        let result = result_with_stderr("   \n\n   ", "success", None);
        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());
        assert!(summary.stderr_tail.is_none());
    }

    #[test]
    fn stderr_tail_truncates_to_max_length_with_ellipsis() {
        let request = inspection_request(serde_json::json!({}));
        let long = "x".repeat(STDERR_TAIL_MAX_LEN + 50);
        let result = result_with_stderr(&long, "failed", None);
        let summary =
            build_step_output_summary(Some(&request), Some(&result), Vec::new(), Vec::new());
        let tail = summary.stderr_tail.expect("stderr_tail");
        assert!(
            tail.ends_with('…'),
            "expected trailing ellipsis, got: {tail}"
        );
        // STDERR_TAIL_MAX_LEN ASCII chars + one ellipsis char
        assert_eq!(tail.chars().count(), STDERR_TAIL_MAX_LEN + 1);
    }
}
