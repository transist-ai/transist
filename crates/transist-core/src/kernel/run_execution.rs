// SPDX-License-Identifier: MPL-2.0
//! Main run execution progression and persisted-run entry points.

use super::*;
use crate::ControlStepKind;
use crate::ports::{
    InvocationStagingPort, LeaseConflict, PluginLifecycleReservationCanceled,
    PluginLifecycleReservationTimeout, RemoteSecretPolicy, WorkflowAssetBlob,
};
use crate::templating::resolve_set_kernel_value;
use crate::types::RunProvenancePolicyDecision;

const PLUGIN_LIFECYCLE_RESERVATION_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
const PLUGIN_LIFECYCLE_RESERVATION_RETRY: std::time::Duration =
    std::time::Duration::from_millis(100);

fn acquire_plugin_lifecycle_reservation_for_run_birth(
    lease: &dyn LeasePort,
    control: &dyn ControlRequestPort,
    cancellation_root_run_id: Option<&str>,
    timeout: std::time::Duration,
) -> Result<LeaseHandle> {
    let deadline = std::time::Instant::now() + timeout;
    let latest_live_lease = loop {
        if let Some(root_run_id) = cancellation_root_run_id
            && control.inspect_cancel_request(root_run_id)?.is_some()
        {
            return Err(PluginLifecycleReservationCanceled {
                root_run_id: root_run_id.to_string(),
            }
            .into());
        }

        match lease.acquire_root_run_tree_lease(PLUGIN_LIFECYCLE_LEASE_ID) {
            Ok(handle) => return Ok(handle),
            Err(error) => {
                let Some(conflict) = error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<LeaseConflict>())
                else {
                    return Err(error);
                };
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break conflict.live_lease.clone();
                }
                std::thread::sleep(PLUGIN_LIFECYCLE_RESERVATION_RETRY.min(remaining));
            }
        }
    };

    Err(PluginLifecycleReservationTimeout {
        timeout_ms: timeout.as_millis() as u64,
        live_lease: latest_live_lease,
    }
    .into())
}

fn capture_recipe_workflow_assets(
    recipe: &transist_protocol::Recipe,
    prompt_files: &dyn crate::ports::PromptFilePort,
) -> Result<(Option<Vec<u8>>, Vec<WorkflowAssetBlob>)> {
    let mut entries = std::collections::BTreeMap::new();
    let mut blobs = std::collections::BTreeMap::new();
    let source_dir = recipe.source_dir.as_deref().map(std::path::Path::new);

    for step in recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|cleanup| &cleanup.step))
    {
        if let Some(transist_protocol::RecipePromptSource::File { path }) =
            step.prompt_binding.as_ref().map(|binding| &binding.source)
            && !path.contains("{{")
        {
            let prompt = prompt_files.resolve_prompt_file(path, source_dir, &step.id)?;
            insert_captured_workflow_asset(
                &mut entries,
                &mut blobs,
                format!("prompts/{}", step.id),
                prompt.content.into_bytes(),
            );
        }
        for value in step.with.values() {
            capture_source_uris_from_value(
                value,
                source_dir,
                &step.id,
                prompt_files,
                &mut entries,
                &mut blobs,
            )?;
        }
    }

    if entries.is_empty() {
        return Ok((None, Vec::new()));
    }
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema": "transist.workflow_asset_snapshot.v1",
        "entries": entries
            .into_iter()
            .map(|(path, sha256)| serde_json::json!({ "path": path, "sha256": sha256 }))
            .collect::<Vec<_>>(),
    }))?;
    Ok((Some(manifest), blobs.into_values().collect()))
}

fn capture_source_uris_from_value(
    value: &transist_protocol::KernelValue,
    source_dir: Option<&std::path::Path>,
    step_id: &str,
    prompt_files: &dyn crate::ports::PromptFilePort,
    entries: &mut std::collections::BTreeMap<String, String>,
    blobs: &mut std::collections::BTreeMap<String, WorkflowAssetBlob>,
) -> Result<()> {
    match value {
        transist_protocol::KernelValue::String(value) => {
            let Some(relative) = value.strip_prefix("source://") else {
                return Ok(());
            };
            if relative.contains("{{") {
                bail!("source:// paths with placeholders cannot be snapshotted");
            }
            let source_dir = source_dir.ok_or_else(|| {
                anyhow::anyhow!("source:// asset requires a source-backed recipe")
            })?;
            let logical = relative.trim_start_matches('/').replace('\\', "/");
            let bytes = prompt_files.read_source_asset(&logical, Some(source_dir), step_id)?;
            insert_captured_workflow_asset(entries, blobs, format!("source/{logical}"), bytes);
        }
        transist_protocol::KernelValue::Array(values) => {
            for value in values {
                capture_source_uris_from_value(
                    value,
                    source_dir,
                    step_id,
                    prompt_files,
                    entries,
                    blobs,
                )?;
            }
        }
        transist_protocol::KernelValue::Object(values) => {
            for value in values.values() {
                capture_source_uris_from_value(
                    value,
                    source_dir,
                    step_id,
                    prompt_files,
                    entries,
                    blobs,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn insert_captured_workflow_asset(
    entries: &mut std::collections::BTreeMap<String, String>,
    blobs: &mut std::collections::BTreeMap<String, WorkflowAssetBlob>,
    logical_path: String,
    bytes: Vec<u8>,
) {
    use sha2::Digest;

    let sha256 = format!("{:x}", sha2::Sha256::digest(&bytes));
    entries.insert(logical_path, sha256.clone());
    blobs
        .entry(sha256.clone())
        .or_insert(WorkflowAssetBlob { sha256, bytes });
}

pub(crate) fn select_preset_workflow_assets(
    parent_recipe: &Recipe,
    child_recipe: &Recipe,
    bundle: (Vec<u8>, Vec<WorkflowAssetBlob>),
) -> Result<Option<(Vec<u8>, Vec<WorkflowAssetBlob>)>> {
    let (manifest, blobs) = bundle;
    let mut value: serde_json::Value =
        serde_json::from_slice(&manifest).context("workflow asset manifest is not valid JSON")?;
    let parent_source = parent_recipe
        .source_dir
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("preset workflow assets require a source-backed recipe"))?;
    let child_source = child_recipe.source_dir.as_deref().ok_or_else(|| {
        anyhow::anyhow!("preset workflow assets require a source-backed child recipe")
    })?;
    let child_prefix = Path::new(child_source)
        .strip_prefix(parent_source)
        .with_context(|| {
            format!(
                "child workflow source '{}' is outside parent source '{}'",
                child_source, parent_source
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");
    let mut required = BTreeMap::new();
    collect_preset_workflow_asset_paths(
        child_recipe,
        Path::new(child_source),
        &child_prefix,
        &mut required,
    )?;
    if required.is_empty() {
        return Ok(None);
    }
    let entries = value
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("workflow asset manifest has no entries array"))?;
    let available = entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(|path| (path, entry))
        })
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::with_capacity(required.len());
    let mut selected_digests = BTreeSet::new();
    for (parent_path, child_path) in required {
        let entry = available.get(parent_path.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "verified workflow asset manifest is missing child asset '{parent_path}'"
            )
        })?;
        let digest = entry
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("workflow asset '{parent_path}' has no sha256"))?;
        let mut rebased = (*entry).clone();
        rebased["path"] = serde_json::Value::String(child_path);
        selected.push(rebased);
        selected_digests.insert(digest.to_string());
    }
    value["entries"] = serde_json::Value::Array(selected);
    let blobs = blobs
        .into_iter()
        .filter(|blob| selected_digests.contains(blob.sha256.as_str()))
        .collect::<Vec<_>>();
    if blobs.len() != selected_digests.len() {
        bail!("verified workflow asset bundle is missing a child asset blob");
    }
    Ok(Some((serde_json::to_vec(&value)?, blobs)))
}

fn collect_preset_workflow_asset_paths(
    recipe: &Recipe,
    child_source: &Path,
    child_prefix: &str,
    paths: &mut BTreeMap<String, String>,
) -> Result<()> {
    let recipe_source = recipe.source_dir.as_deref().ok_or_else(|| {
        anyhow::anyhow!("preset workflow assets require a source-backed child recipe")
    })?;
    let recipe_prefix = Path::new(recipe_source)
        .strip_prefix(child_source)
        .with_context(|| {
            format!(
                "nested workflow source '{}' is outside child source '{}'",
                recipe_source,
                child_source.display()
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");
    for step in recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|cleanup| &cleanup.step))
    {
        if let Some(transist_protocol::RecipePromptSource::File { path }) =
            step.prompt_binding.as_ref().map(|binding| &binding.source)
        {
            insert_preset_workflow_asset_path(paths, child_prefix, &recipe_prefix, path)?;
        }
        for value in step.with.values() {
            collect_preset_source_asset_paths(value, child_prefix, &recipe_prefix, paths)?;
        }
        if let StepExecutionDescriptor::Subflow { recipe } = &step.execution {
            collect_preset_workflow_asset_paths(recipe, child_source, child_prefix, paths)?;
        }
    }
    Ok(())
}

fn collect_preset_source_asset_paths(
    value: &transist_protocol::KernelValue,
    child_prefix: &str,
    recipe_prefix: &str,
    paths: &mut BTreeMap<String, String>,
) -> Result<()> {
    match value {
        transist_protocol::KernelValue::String(value) => {
            if let Some(relative) = value.strip_prefix("source://") {
                insert_preset_workflow_asset_path(paths, child_prefix, recipe_prefix, relative)?;
            }
        }
        transist_protocol::KernelValue::Array(values) => {
            for value in values {
                collect_preset_source_asset_paths(value, child_prefix, recipe_prefix, paths)?;
            }
        }
        transist_protocol::KernelValue::Object(values) => {
            for value in values.values() {
                collect_preset_source_asset_paths(value, child_prefix, recipe_prefix, paths)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn insert_preset_workflow_asset_path(
    paths: &mut BTreeMap<String, String>,
    child_prefix: &str,
    recipe_prefix: &str,
    raw: &str,
) -> Result<()> {
    let relative = normalized_asset_relative_path(raw.trim_start_matches('/'))?;
    let child_relative = [recipe_prefix, &relative]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let parent_relative = [child_prefix, &child_relative]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    paths.insert(
        format!("source/{parent_relative}"),
        format!("source/{child_relative}"),
    );
    Ok(())
}

fn build_asset_locks_from_preset(
    recipe: &Recipe,
    manifest: &[u8],
) -> Result<Vec<crate::types::AssetLock>> {
    let value: serde_json::Value =
        serde_json::from_slice(manifest).context("workflow asset manifest is not valid JSON")?;
    let entries = value
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("workflow asset manifest has no entries array"))?;
    let digests = entries
        .iter()
        .map(|entry| {
            let path = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("workflow asset manifest entry has no path"))?;
            let sha256 = entry
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("workflow asset manifest entry has no sha256"))?;
            Ok((path.to_string(), sha256.to_string()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut locks = Vec::new();
    for step in recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|cleanup| &cleanup.step))
    {
        if matches!(step.execution, StepExecutionDescriptor::Subflow { .. }) {
            continue;
        }
        if let StepExecutionDescriptor::Invoke {
            plugin,
            plugin_version: Some(version),
            plugin_sha256: Some(sha256),
            ..
        } = &step.execution
        {
            locks.push(crate::types::AssetLock {
                step_id: step.id.clone(),
                kind: crate::types::AssetLockKind::Plugin,
                ref_id: plugin.clone(),
                version: Some(version.clone()),
                sha256: sha256.clone(),
            });
        }
        if let Some(transist_protocol::RecipePromptSource::File { path }) =
            step.prompt_binding.as_ref().map(|binding| &binding.source)
        {
            let logical = format!("source/{}", normalized_asset_relative_path(path)?);
            let sha256 = digests.get(&logical).ok_or_else(|| {
                anyhow::anyhow!(
                    "verified workflow asset manifest is missing prompt for step '{}'",
                    step.id
                )
            })?;
            locks.push(crate::types::AssetLock {
                step_id: step.id.clone(),
                kind: crate::types::AssetLockKind::PromptFile,
                ref_id: path.clone(),
                version: None,
                sha256: sha256.clone(),
            });
        }
    }
    Ok(locks)
}

fn normalized_asset_relative_path(raw: &str) -> Result<String> {
    let path = Path::new(raw.trim());
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("workflow asset path must be a non-empty relative path");
    }
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => normalized.push(name),
            _ => bail!("workflow asset path must not contain traversal or special components"),
        }
    }
    Ok(normalized.to_string_lossy().replace('\\', "/"))
}

pub(crate) struct ControlWatcherGuard {
    stop_tx: Option<std::sync::mpsc::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

pub(crate) struct MutationLeaseGuard<'a> {
    kernel: &'a RuntimeKernel,
    lease: Option<LeaseHandle>,
}

impl<'a> MutationLeaseGuard<'a> {
    pub(crate) fn new(kernel: &'a RuntimeKernel, lease: LeaseHandle) -> Self {
        Self {
            kernel,
            lease: Some(lease),
        }
    }

    pub(crate) fn into_inner(mut self) -> Result<LeaseHandle> {
        self.lease
            .take()
            .ok_or_else(|| anyhow::anyhow!("mutation lease guard was already finalized"))
    }

    pub(crate) fn release_after_error(
        mut self,
        operation_error: anyhow::Error,
        operation: &str,
    ) -> anyhow::Error {
        let Some(lease) = self.lease.take() else {
            return operation_error.context("mutation lease guard was already finalized");
        };
        match self.kernel.release_mutation_lease(&lease) {
            Ok(()) => operation_error,
            Err(release_error) => operation_error.context(format!(
                "{operation} lease release also failed: {release_error:#}"
            )),
        }
    }
}

impl Drop for MutationLeaseGuard<'_> {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take()
            && let Err(error) = self.kernel.release_mutation_lease(&lease)
        {
            tracing::error!(error = %error, root_run_id = %lease.root_run_id, "mutation lease release failed during preparation cleanup");
        }
    }
}

impl Drop for ControlWatcherGuard {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchOutcome {
    Dispatched,
    NotDispatched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepExecutionControl {
    Continue,
    Break,
}

struct StepExecutionResult {
    result: ExecutionResult,
    state_mutations: Vec<crate::failures::StateMutation>,
    validation: LoadedResultValidationMetadata,
}

struct LoadedRunState {
    graph: ExecutionGraph,
    snapshot: KernelRunSnapshot,
    persisted_record: PersistedRunRecord,
    last_stable_step_id: Option<String>,
}

struct RecoveredLoadedRun {
    state: LoadedRunState,
}

struct ActivatedLoadedRun {
    state: LoadedRunState,
}

struct PreflightedLoadedRun {
    state: LoadedRunState,
}

struct ScheduledLoadedRun {
    state: LoadedRunState,
}

struct CanceledLoadedRun {
    snapshot: KernelRunSnapshot,
    persisted_record: PersistedRunRecord,
}

enum LoadedRunActivation {
    Ready(ActivatedLoadedRun),
    Canceled(CanceledLoadedRun),
}

enum LoadedRunPreflight {
    Ready(Box<PreflightedLoadedRun>),
    Finished(KernelRunSummary),
}

enum LoadedRunScheduling {
    Ready(ScheduledLoadedRun),
    Canceled(CanceledLoadedRun),
}

struct AuthorizedLoadedInvoke {
    request: ExecutionRequest,
    secret_refs: Vec<String>,
    remote_secret_policy: RemoteSecretPolicy,
}

enum LoadedInvokeAuthorization {
    Allowed(AuthorizedLoadedInvoke),
    Denied(ExecutionResult),
}

enum LoadedInvokeSecretResolution {
    Ready(BTreeMap<String, String>),
    Denied(Box<ExecutionResult>),
}

struct LoadedInvokeDispatched {
    request: ExecutionRequest,
    result: ExecutionResult,
    staging: Box<dyn InvocationStagingPort>,
    redactor: Redactor,
}

struct LoadedStepExecution {
    result: ExecutionResult,
    dispatch_outcome: DispatchOutcome,
    validation: Option<LoadedResultValidationMetadata>,
}

#[derive(Default)]
struct LoadedResultValidationMetadata {
    filesystem_denial: Option<crate::filesystem_policy::FilesystemDenial>,
    trusted_result_file_mismatch: Option<String>,
    sandbox_failure: Option<serde_json::Value>,
}

struct LoadedInvokeResultValidationInput<'a> {
    request: &'a ExecutionRequest,
    result: ExecutionResult,
    workspace_root: &'a Path,
}

struct ValidatedLoadedInvokeResult {
    request: ExecutionRequest,
    result: ExecutionResult,
    metadata: LoadedResultValidationMetadata,
}

enum LoadedInvokeResultValidationResult {
    Accepted(ValidatedLoadedInvokeResult),
    Denied(ValidatedLoadedInvokeResult),
}

struct LoadedInvokeResultPromotionInput {
    validated: ValidatedLoadedInvokeResult,
    staging: Box<dyn InvocationStagingPort>,
    redactor: Redactor,
}

struct PromotedLoadedInvokeResult {
    request: ExecutionRequest,
    result: ExecutionResult,
    redactor: Redactor,
    metadata: LoadedResultValidationMetadata,
}

enum LoadedInvokeResultPromotionResult {
    Promoted(PromotedLoadedInvokeResult),
    Failed(PromotedLoadedInvokeResult),
}

struct LoadedInvokeResultNormalizationInput {
    request: ExecutionRequest,
    result: ExecutionResult,
    redactor: Redactor,
    metadata: LoadedResultValidationMetadata,
    trusted_execution_mode: bool,
}

struct NormalizedLoadedInvokeResult {
    result: ExecutionResult,
    metadata: LoadedResultValidationMetadata,
}

enum LoadedInvokeResultNormalizationResult {
    Normalized(NormalizedLoadedInvokeResult),
    Failed(NormalizedLoadedInvokeResult),
}

struct LoadedTerminalTransitionPlan {
    execution_record: StepExecutionRecord,
    result: ExecutionResult,
    status: KernelStepStatus,
    retry_wait_until_ms: Option<u64>,
    reset_loop_body_steps: Vec<String>,
    activated_loop_targets: Vec<String>,
    finished_at_ms: u64,
    workflow_vars: BTreeMap<String, KernelValue>,
    succeeded: bool,
    blocked: bool,
    cancelled: bool,
}

struct LoadedExecutionPersistenceInput {
    execution_record: StepExecutionRecord,
}

enum LoadedExecutionPersistenceResult {
    Committed(StepExecutionRecord),
}

struct LoadedTerminalTransitionPlanningInput<'a> {
    graph: &'a ExecutionGraph,
    persisted_record: &'a PersistedRunRecord,
    snapshot: &'a KernelRunSnapshot,
    step: &'a transist_protocol::RecipeStep,
    step_id: String,
    result: ExecutionResult,
    state_mutations: Vec<crate::failures::StateMutation>,
}

struct LoadedTerminalTransitionInput<'a> {
    paths: RuntimePaths<'a>,
    persisted_record: &'a mut PersistedRunRecord,
    snapshot: &'a mut KernelRunSnapshot,
    last_stable_step_id: &'a mut Option<String>,
    step: &'a transist_protocol::RecipeStep,
    step_id: String,
    plan: LoadedTerminalTransitionPlan,
    validation: LoadedResultValidationMetadata,
}

enum LoadedTerminalTransitionResult {
    Continue,
    Break,
}

enum LoadedInvokeAdapterResult {
    Dispatched(Box<LoadedInvokeDispatched>),
    Failed(Box<ExecutionResult>),
}

/// Immutable execution-record status written for an execution ordinal that was
/// consumed (dispatched) but never completed, so the ordinal is never silently
/// dropped. Recovery creates this marker for an in-flight ordinal lacking a
/// completion record, and a replacement dispatch begins only at `N+1`.
pub(crate) const STEP_EXECUTION_STATUS_INTERRUPTED: &str = "interrupted";

/// Returns whether an immutable execution record represents a completed
/// execution (a committed result) as opposed to an `interrupted` placeholder.
pub(crate) fn record_is_completed(record: &StepExecutionRecord) -> bool {
    record.status != STEP_EXECUTION_STATUS_INTERRUPTED && record.result.is_some()
}

/// Applies the terminal step state (status, retry deadline, result,
/// timestamps) committed by an immutable execution record onto a step state,
/// without any persistence. Used by recovery reconciliation for both primary
/// steps and cleanup steps.
pub(crate) fn apply_terminal_state_from_record(
    step_state: &mut KernelStepState,
    record: &StepExecutionRecord,
    now_ms: u64,
) {
    let (status, wait_until_ms) = terminal_status_from_record(record, now_ms);
    step_state.status = status;
    step_state.retry_wait_until_ms = wait_until_ms;
    step_state.last_result = record.result.clone();
    step_state.started_at_ms = step_state.started_at_ms.or(record.started_at_ms);
    step_state.finished_at_ms = step_state.finished_at_ms.or(record.finished_at_ms);
    step_state.skip_reason = None;
}

/// Validates that an immutable execution record returned by
/// `load_step_execution` actually belongs to the requested `(run, step,
/// execution)` triple. A mismatched identity fails closed because recovery
/// must never reconcile a step from someone else's record.
pub(crate) fn validate_record_identity(
    record: &StepExecutionRecord,
    run_id: &str,
    step_id: &str,
    execution: u32,
) -> Result<()> {
    if record.run_id != run_id || record.step_id != step_id || record.execution != execution {
        bail!(
            "execution record identity mismatch for run '{}' step '{}' execution {}",
            run_id,
            step_id,
            execution
        );
    }
    Ok(())
}

/// Maps a committed execution record to its completion event kind
/// (`step_finished`/`step_blocked`/`step_cancelled`/`step_failed`, or the
/// `cleanup_*` variants). Returns `None` when the record carries no result.
pub(crate) fn completion_event_kind(record: &StepExecutionRecord, cleanup: bool) -> Option<String> {
    let status = record.result.as_ref()?.status.as_str();
    let base = match status {
        "success" => "step_finished",
        "blocked" => "step_blocked",
        "cancelled" => "step_cancelled",
        _ => "step_failed",
    };
    Some(if cleanup {
        format!("cleanup_{base}")
    } else {
        base.to_string()
    })
}

/// Returns whether an event payload carries the given execution number in its
/// `execution` field, used to dedupe recovery event backfill by
/// `(run, step, execution, kind)`.
pub(crate) fn event_execution_matches(payload: &KernelValue, execution: u32) -> bool {
    match payload {
        KernelValue::Object(map) => match map.get("execution") {
            Some(KernelValue::Number(number)) => number.as_u64() == Some(execution as u64),
            _ => false,
        },
        _ => false,
    }
}

/// Maps an immutable execution record to the terminal step status and retry
/// deadline it committed. A committed success is `Succeeded`; a committed
/// block is `Blocked`; a committed cancellation is `Canceled`; any committed
/// failure becomes `Failed` (with the `RetryWait` deadline when the record
/// carries a retry deadline). Used to reconcile terminal step state from the
/// completion commit point after restart.
fn terminal_status_from_record(
    record: &StepExecutionRecord,
    now_ms: u64,
) -> (KernelStepStatus, Option<u64>) {
    let result_status = record.result.as_ref().map(|result| result.status.as_str());
    match result_status {
        Some("success") => (KernelStepStatus::Succeeded, None),
        Some("blocked") => (KernelStepStatus::Blocked, None),
        Some("cancelled") => (KernelStepStatus::Canceled, None),
        _ => {
            // A committed failure. The completion path computed a retry
            // deadline when the step transitioned to `RetryWait`; the record
            // carries it as the persisted retry deadline. When the deadline is
            // still in the future the step is reconciled as `RetryWait` with
            // that deadline; otherwise (or when the record predates the
            // deadline projection) the step is failed, never re-dispatched, and
            // left for the run's next dispatch plan.
            match record.retry_wait_until_ms {
                Some(wait_until) if wait_until > now_ms => {
                    (KernelStepStatus::RetryWait, Some(wait_until))
                }
                _ => (KernelStepStatus::Failed, None),
            }
        }
    }
}

impl RuntimeKernel {
    pub(crate) fn prepare_with_mutation_lease<T>(
        &self,
        home_dir: &Path,
        run_id: &str,
        operation_name: &str,
        prepare: impl FnOnce(&LeaseHandle) -> Result<T>,
    ) -> Result<(T, LeaseHandle)> {
        let lease = self.acquire_mutation_lease(home_dir, run_id)?;
        match prepare(&lease) {
            Ok(value) => Ok((value, lease)),
            Err(error) => Err(match self.release_mutation_lease(&lease) {
                Ok(()) => error,
                Err(release_error) => error.context(format!(
                    "{operation_name} failed and lease release also failed: {release_error:#}"
                )),
            }),
        }
    }

    /// Replaces a run-scoped recipe snapshot while holding the root-tree
    /// mutation lease. The caller supplies the compiled registry bytes; all
    /// protected run-state mutation remains kernel-owned.
    pub fn sync_recipe_snapshot(
        &self,
        home_dir: &Path,
        run_id: &str,
        source_registry_path: &Path,
        recipe_yaml: &str,
    ) -> Result<KernelOperationOutcome<RecipeSyncResult>> {
        let recipe: Recipe = yaml_serde::from_str(recipe_yaml).with_context(|| {
            format!(
                "invalid compiled registry recipe yaml: {}",
                source_registry_path.display()
            )
        })?;

        let lease = self.acquire_mutation_lease(home_dir, run_id)?;
        let operation = (|| {
            // Re-read after lease acquisition. The pre-lease state may have
            // changed while another owner was finishing its mutation session.
            let record = load_run_record(self.services.run_store.as_ref(), run_id)?
                .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
            if !crate::kernel::test_mode::is_test_workflow(&record.workflow_version) {
                bail!(
                    "run '{}' cannot sync its recipe snapshot because workflow {}@{} is not a test workflow",
                    run_id,
                    record.workflow_id,
                    record.workflow_version
                );
            }
            if matches!(
                record.status,
                KernelRunStatus::Running | KernelRunStatus::Waiting
            ) {
                bail!(
                    "run '{}' cannot sync its recipe snapshot while status is '{}'",
                    run_id,
                    record.status.as_str()
                );
            }
            if recipe.workflow.id != record.workflow_id
                || recipe.workflow.version != record.workflow_version
            {
                bail!(
                    "registry recipe '{}' does not match run '{}' workflow {}@{}",
                    source_registry_path.display(),
                    run_id,
                    record.workflow_id,
                    record.workflow_version
                );
            }
            if recipe.workspace != record.workspace {
                bail!(
                    "registry recipe '{}' workspace does not match persisted workspace for run '{}'",
                    source_registry_path.display(),
                    run_id
                );
            }
            let run_recipe_path = self
                .services
                .run_store
                .resolve_run_root(home_dir, run_id)?
                .join("recipe.yaml");

            let mut updated = record.clone();
            updated.recipe_snapshot = Some(RecipeSnapshotRef {
                path: "recipe.yaml".to_string(),
                sha256: sha256_hex(recipe_yaml.as_bytes()),
            });
            let step_ids = recipe
                .steps
                .iter()
                .map(|step| step.id.clone())
                .collect::<BTreeSet<_>>();
            updated
                .steps
                .retain(|step_id, _| step_ids.contains(step_id));
            for step_id in step_ids {
                updated
                    .steps
                    .entry(step_id.clone())
                    .or_insert_with(|| KernelStepState::pending(&step_id));
            }
            updated.preflight = None;
            updated.cleanup = None;
            updated.trust_decision = None;
            updated.asset_locks = crate::kernel::build_asset_locks(
                &recipe,
                &updated.workflow_inputs,
                &updated.workflow_vars,
                self.services.prompt_files.as_ref(),
            )?;
            updated.asset_lock_version = 1;
            // Test-only recipe synchronization deliberately rebuilds source
            // locks; it does not replace the captured workflow-asset bundle.
            updated.workflow_asset_snapshot = false;
            updated.refresh_executions();
            updated.updated_at_ms = self.services.clock.now_unix_ms();
            self.services
                .run_store
                .sync_recipe_snapshot(run_id, &updated, recipe_yaml)?;
            Ok(RecipeSyncResult {
                run_id: run_id.to_string(),
                workflow_id: updated.workflow_id,
                workflow_version: updated.workflow_version,
                source_registry_path: source_registry_path.to_path_buf(),
                run_recipe_path,
            })
        })();
        self.finish_mutation_operation(operation, lease, "recipe snapshot synchronization")
    }

    /// Refreshes and persists trusted-execution state for a persisted run while
    /// holding the root-tree mutation lease.
    pub fn refresh_trusted_execution_for_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
        trusted_execution: bool,
        resolver: &dyn TrustDecisionPort,
    ) -> Result<KernelOperationOutcome<()>> {
        let initial = load_run_record(self.services.run_store.as_ref(), run_id)?
            .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
        if !initial.trusted_execution_mode && !trusted_execution {
            return Ok(KernelOperationOutcome {
                value: (),
                lease_release_warning: None,
            });
        }

        let lease = self.acquire_mutation_lease(home_dir, run_id)?;
        let operation = (|| {
            let mut record = load_run_record(self.services.run_store.as_ref(), run_id)?
                .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
            let recipe = self.load_persisted_recipe(run_id, &record)?;
            let decision = resolver.resolve_run_decision(repo_root, home_dir, &record, &recipe)?;
            record.trusted_execution_mode = true;
            record.trust_decision = Some(decision.clone());
            record.updated_at_ms = self.services.clock.now_unix_ms();
            self.services.run_store.update_run_state(run_id, &record)?;
            if decision.denied() {
                self.publish_event(
                    "trust.denied",
                    run_id,
                    None,
                    serde_json::json!({
                        "reason_code": decision
                            .reason_code
                            .as_deref()
                            .unwrap_or("trusted_execution_denied"),
                        "failed_verification_type": decision.failed_verification_type,
                        "workflow_ref": format!("{}@{}", record.workflow_id, record.workflow_version),
                    })
                    .into(),
                )?;
                bail!(
                    "trusted execution denied (code={}, verification={})",
                    decision
                        .reason_code
                        .as_deref()
                        .unwrap_or("trusted_execution_denied"),
                    decision
                        .failed_verification_type
                        .as_deref()
                        .unwrap_or("unknown")
                );
            }
            Ok(())
        })();
        self.finish_mutation_operation(operation, lease, "trusted-execution refresh")
    }

    pub(crate) fn finish_mutation_operation<T>(
        &self,
        operation: Result<T>,
        lease: LeaseHandle,
        operation_name: &str,
    ) -> Result<KernelOperationOutcome<T>> {
        let release = self.release_mutation_lease(&lease);
        match (operation, release) {
            (Ok(value), Ok(())) => Ok(KernelOperationOutcome {
                value,
                lease_release_warning: None,
            }),
            (Ok(value), Err(error)) => {
                let warning = LeaseReleaseWarning {
                    message: format!("{operation_name} lease release failed: {error:#}"),
                };
                tracing::error!(operation = operation_name, error = %warning.message, "durable operation completed but lease release failed");
                Ok(KernelOperationOutcome {
                    value,
                    lease_release_warning: Some(warning),
                })
            }
            (Err(operation_error), Ok(())) => Err(operation_error),
            (Err(operation_error), Err(release_error)) => Err(operation_error.context(format!(
                "{operation_name} lease release also failed: {release_error:#}"
            ))),
        }
    }

    /// Cancels an active invoke step by resolving the stored invoke id for the step.
    pub fn cancel_step(&self, run_id: &str, step_id: &str) -> Result<ExecutionCancelResult> {
        let record = load_run_record(self.services.run_store.as_ref(), run_id)?
            .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
        let step = record
            .steps
            .get(step_id)
            .ok_or_else(|| anyhow::anyhow!("step '{}' not found in run '{}'", step_id, run_id))?;
        let invoke_id = step
            .last_request
            .as_ref()
            .map(|request| request.invoke_id.clone())
            .ok_or_else(|| anyhow::anyhow!("step '{}' has no active invoke request", step_id))?;
        self.services
            .execution
            .cancel_step(&ExecutionCancelRequest { invoke_id })
    }

    /// Persists a cancellation request for the root run tree containing a run.
    pub fn request_run_cancellation(
        &self,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<ControlRequestRecord> {
        let root_run_id = self
            .services
            .run_store
            .resolve_root_run_id(home_dir, run_id)?;
        self.services.control.request_cancel(&root_run_id)
    }

    /// Executes a fresh recipe run and returns a run summary.
    pub fn execute_recipe(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
    ) -> Result<KernelRunSummary> {
        self.execute_recipe_with_inputs(repo_root, home_dir, recipe, run_ref, BTreeMap::new())
    }

    /// Executes a fresh recipe run with bound workflow inputs and returns a run summary.
    pub fn execute_recipe_with_inputs(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
    ) -> Result<KernelRunSummary> {
        self.execute_recipe_with_inputs_and_trust(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            false,
            None,
            false,
        )
    }

    /// Executes a fresh recipe run with optional trusted-execution metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_recipe_with_inputs_and_trust(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
    ) -> Result<KernelRunSummary> {
        self.execute_recipe_with_inputs_and_trust_and_assets(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
            None,
        )
    }

    /// Executes a trusted fresh run with workflow assets captured by the verifier.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_recipe_with_inputs_and_trust_and_assets(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
        preset_workflow_assets: Option<(Vec<u8>, Vec<WorkflowAssetBlob>)>,
    ) -> Result<KernelRunSummary> {
        let mut resolved_recipe = recipe.clone();
        resolve_recipe_authority_fields(&mut resolved_recipe, &workflow_inputs)?;
        let execution_workspace =
            resolve_recipe_workspace(home_dir, repo_root, &mut resolved_recipe, &workflow_inputs)?;
        resolve_recipe_filesystem_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut resolved_recipe,
        )?;
        resolve_recipe_network_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut resolved_recipe,
        )?;
        populate_recipe_capabilities(
            home_dir,
            &execution_workspace,
            &mut resolved_recipe,
            &workflow_inputs,
            local_only_mode,
            self.services.home_settings.as_ref(),
            self.services.plugin_metadata.as_ref(),
        )?;
        let run_id = run_ref
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| self.services.ids.generate_run_id());
        let lease = self.services.lease.acquire_root_run_tree_lease(&run_id)?;
        let record = match self.start_recipe_internal(
            &execution_workspace,
            home_dir,
            &resolved_recipe,
            Some(&run_id),
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
            RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: String::new(),
                parent_run_id: None,
                parent_step_id: None,
            },
            None,
            preset_workflow_assets,
        ) {
            Ok(record) => record,
            Err(error) => {
                return self
                    .finish_mutation_operation(Err(error), lease, "fresh run creation")
                    .map(|outcome| outcome.value);
            }
        };
        let preflight_override = record.preflight.clone();
        self.execute_loaded_run(
            RuntimePaths {
                repo_root: &execution_workspace,
                home_dir,
            },
            &resolved_recipe,
            record,
            lease,
            ExecuteLoadedRunOptions {
                run_started_payload: Some(serde_json::json!({ "mode": "fresh" }).into()),
                apply_recovery: true,
                preflight_override,
            },
        )
    }

    /// Creates and prepares a fresh recipe run while holding one root-run-tree lease session.
    pub fn prepare_recipe(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
    ) -> Result<(String, PreflightReport)> {
        self.prepare_recipe_with_trust(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            false,
            None,
            false,
        )
    }

    /// Creates and prepares a fresh recipe run, retaining lease finalization state.
    pub fn prepare_recipe_with_outcome(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
    ) -> Result<KernelOperationOutcome<(String, PreflightReport)>> {
        self.prepare_recipe_with_trust_outcome(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            false,
            None,
            false,
        )
    }

    /// Creates and prepares a fresh recipe run while persisting trusted-execution metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_recipe_with_trust(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
    ) -> Result<(String, PreflightReport)> {
        self.prepare_recipe_with_trust_outcome(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
        )
        .map(|outcome| outcome.value)
    }

    /// Creates and prepares a fresh recipe run while retaining lease finalization state.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_recipe_with_trust_outcome(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
    ) -> Result<KernelOperationOutcome<(String, PreflightReport)>> {
        self.prepare_recipe_with_trust_and_assets_outcome(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
            None,
        )
    }

    /// Creates and prepares a trusted run with workflow assets captured by the verifier.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_recipe_with_trust_and_assets_outcome(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
        preset_workflow_assets: Option<(Vec<u8>, Vec<WorkflowAssetBlob>)>,
    ) -> Result<KernelOperationOutcome<(String, PreflightReport)>> {
        let mut resolved_recipe = recipe.clone();
        resolve_recipe_authority_fields(&mut resolved_recipe, &workflow_inputs)?;
        let execution_workspace =
            resolve_recipe_workspace(home_dir, repo_root, &mut resolved_recipe, &workflow_inputs)?;
        resolve_recipe_filesystem_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut resolved_recipe,
        )?;
        resolve_recipe_network_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut resolved_recipe,
        )?;
        populate_recipe_capabilities(
            home_dir,
            &execution_workspace,
            &mut resolved_recipe,
            &workflow_inputs,
            local_only_mode,
            self.services.home_settings.as_ref(),
            self.services.plugin_metadata.as_ref(),
        )?;
        let run_id = run_ref
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| self.services.ids.generate_run_id());
        let lease = self.services.lease.acquire_root_run_tree_lease(&run_id)?;
        let mut record = match self.start_recipe_internal(
            &execution_workspace,
            home_dir,
            &resolved_recipe,
            Some(&run_id),
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
            RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: String::new(),
                parent_run_id: None,
                parent_step_id: None,
            },
            None,
            preset_workflow_assets,
        ) {
            Ok(record) => record,
            Err(error) => {
                return Err(match self.release_mutation_lease(&lease) {
                    Ok(()) => error,
                    Err(release_error) => error.context(format!(
                        "run preparation failed and lease release also failed: {release_error:#}"
                    )),
                });
            }
        };
        let result = (|| {
            let preflight = record.preflight.clone().ok_or_else(|| {
                anyhow::anyhow!("fresh run is missing successful birth preflight")
            })?;
            record.status = KernelRunStatus::Planned;
            record.updated_at_ms = self.services.clock.now_unix_ms();
            persist_run_record(self.services.run_store.as_ref(), &record)?;
            Ok(preflight.to_report(record.run_id.clone()))
        })();
        self.finish_mutation_operation(
            result.map(|report| (run_id, report)),
            lease,
            "run preparation",
        )
    }

    /// Creates a fresh run record and persists the run-scoped recipe snapshot without executing it.
    pub fn start_recipe(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
    ) -> Result<String> {
        self.start_recipe_with_outcome(repo_root, home_dir, recipe, run_ref, workflow_inputs)
            .map(|outcome| outcome.value)
    }

    /// Creates a fresh run while retaining lease finalization state.
    pub fn start_recipe_with_outcome(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
    ) -> Result<KernelOperationOutcome<String>> {
        self.start_recipe_with_trust_outcome(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            false,
            None,
            false,
        )
    }

    /// Creates a fresh run record and persists trusted-execution metadata without executing it.
    #[allow(clippy::too_many_arguments)]
    pub fn start_recipe_with_trust(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
    ) -> Result<String> {
        self.start_recipe_with_trust_outcome(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
        )
        .map(|outcome| outcome.value)
    }

    /// Creates a fresh run while retaining trusted-execution and lease state.
    #[allow(clippy::too_many_arguments)]
    pub fn start_recipe_with_trust_outcome(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
    ) -> Result<KernelOperationOutcome<String>> {
        self.start_recipe_with_trust_and_assets_outcome(
            repo_root,
            home_dir,
            recipe,
            run_ref,
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
            None,
        )
    }

    /// Creates a trusted fresh run with workflow assets captured by the verifier.
    #[allow(clippy::too_many_arguments)]
    pub fn start_recipe_with_trust_and_assets_outcome(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
        preset_workflow_assets: Option<(Vec<u8>, Vec<WorkflowAssetBlob>)>,
    ) -> Result<KernelOperationOutcome<String>> {
        let mut resolved_recipe = recipe.clone();
        resolve_recipe_authority_fields(&mut resolved_recipe, &workflow_inputs)?;
        resolve_recipe_filesystem_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut resolved_recipe,
        )?;
        resolve_recipe_network_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut resolved_recipe,
        )?;
        populate_recipe_capabilities(
            home_dir,
            repo_root,
            &mut resolved_recipe,
            &workflow_inputs,
            local_only_mode,
            self.services.home_settings.as_ref(),
            self.services.plugin_metadata.as_ref(),
        )?;
        let run_id = run_ref
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| self.services.ids.generate_run_id());
        let lease = self.services.lease.acquire_root_run_tree_lease(&run_id)?;
        let result = self.start_recipe_internal(
            repo_root,
            home_dir,
            &resolved_recipe,
            Some(&run_id),
            workflow_inputs,
            trusted_execution_mode,
            trust_decision,
            local_only_mode,
            RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: String::new(),
                parent_run_id: None,
                parent_step_id: None,
            },
            None,
            preset_workflow_assets,
        );
        self.finish_mutation_operation(result.map(|record| record.run_id), lease, "run creation")
    }

    /// Executes a previously started run from its persisted run-scoped recipe snapshot.
    pub fn run_started_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<KernelRunSummary> {
        let ((record, recipe), lease) =
            self.prepare_with_mutation_lease(home_dir, run_id, "run re-entry preparation", |lease| {
                let mut record = load_and_rebase_run_record(
                    self.services.run_store.as_ref(),
                    home_dir,
                    repo_root,
                    run_id,
                )?
                .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
                if matches!(
                    record.status,
                    KernelRunStatus::Success | KernelRunStatus::Canceled
                ) {
                    bail!(
                        "run '{}' cannot be executed because status is '{}'",
                        run_id,
                        record.status.as_str()
                    );
                }
                if matches!(
                    record.recovery_state,
                    Some(
                        KernelRunRecoveryState::CleanupRequired
                            | KernelRunRecoveryState::CleanupCompleted
                    )
                ) {
                    bail!(
                        "run '{}' cannot be executed because explicit cleanup has begun for this run; retry `transist runs cleanup {}`",
                        run_id,
                        run_id
                    );
                }
                if record.status == KernelRunStatus::Blocked {
                    for step in record.steps.values_mut() {
                        if step.status == KernelStepStatus::Blocked
                            && step_transition_allowed(
                                KernelStepStatus::Blocked,
                                KernelStepStatus::Runnable,
                            )
                        {
                            step.status = KernelStepStatus::Runnable;
                        }
                    }
                }
                let recipe = self.load_persisted_recipe(run_id, &record)?;
                // A durable cancellation never dispatches work, so it can settle from
                // the persisted snapshot even when current re-entry validation would
                // reject an ordinary resume (for example, after policy drift).
                if self.cancel_request_pending(&lease.root_run_id)? {
                    return Ok((record, recipe));
                }
                // Enforce the runtime policy before proceeding. The policy is reconstructed
                // from the current runtime environment (including any RAC) on each `run`
                // invocation, so a denied workflow ref here means the current credentials
                // do not permit executing this run.
                let workflow_ref = self.policy_workflow_ref(&record)?;
                let run_decision = self
                    .services
                    .policy
                    .validate_run_start(repo_root, &workflow_ref)?;
                if !run_decision.allowed {
                    bail!(
                        "run start denied (code={}, message={})",
                        run_decision
                            .reason_code
                            .as_deref()
                            .unwrap_or("policy_denied"),
                        run_decision.message.as_deref().unwrap_or("denied")
                    );
                }
                // Verify run-level asset locks before dispatching any work so a
                // changed plugin or prompt file is caught before re-entry rather than
                // mid-execution.
                self.verify_asset_locks(&record, &recipe, home_dir)?;
                self.prepare_request_changes_reentry(
                    home_dir,
                    repo_root,
                    run_id,
                    &recipe,
                    &mut record,
                )?;
                Ok((record, recipe))
            })?;
        self.execute_loaded_run(
            RuntimePaths {
                repo_root,
                home_dir,
            },
            &recipe,
            record,
            lease,
            ExecuteLoadedRunOptions {
                run_started_payload: None,
                apply_recovery: true,
                preflight_override: None,
            },
        )
    }

    /// Executes a previously prepared run while reusing its stored successful preflight result.
    pub fn run_prepared_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<KernelRunSummary> {
        let ((record, recipe, preflight), lease) = self.prepare_with_mutation_lease(
            home_dir,
            run_id,
            "prepared run re-entry preparation",
            |_| {
        let mut record = load_and_rebase_run_record(
            self.services.run_store.as_ref(),
            home_dir,
            repo_root,
            run_id,
        )?
        .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
        if matches!(
            record.status,
            KernelRunStatus::Success | KernelRunStatus::Canceled
        ) {
            bail!(
                "run '{}' cannot be executed because status is '{}'",
                run_id,
                record.status.as_str()
            );
        }
        if matches!(
            record.recovery_state,
            Some(
                KernelRunRecoveryState::CleanupRequired
                    | KernelRunRecoveryState::CleanupCompleted
            )
        ) {
            bail!(
                "run '{}' cannot be executed because explicit cleanup has begun for this run; retry `transist runs cleanup {}`",
                run_id,
                run_id
            );
        }
        let Some(preflight) = record.preflight.clone() else {
            bail!(
                "run '{}' cannot be launched because no persisted preflight result is available",
                run_id
            );
        };
        if preflight.status == PreflightStatus::Failed {
            bail!(
                "run '{}' cannot be launched because persisted preflight status is 'failed'",
                run_id
            );
        }
        if record.status == KernelRunStatus::Blocked {
            for step in record.steps.values_mut() {
                if step.status == KernelStepStatus::Blocked
                    && step_transition_allowed(
                        KernelStepStatus::Blocked,
                        KernelStepStatus::Runnable,
                    )
                {
                    step.status = KernelStepStatus::Runnable;
                }
            }
        }
        let recipe = self.load_persisted_recipe(run_id, &record)?;
        // Even though a prepared run carries a stored preflight summary, we
        // must re-verify asset locks because the installed plugin bytes or
        // prompt-file contents may have changed between prepare and launch.
        self.verify_asset_locks(&record, &recipe, home_dir)?;
        self.prepare_request_changes_reentry(home_dir, repo_root, run_id, &recipe, &mut record)?;
        Ok((record, recipe, preflight))
            },
        )?;
        self.execute_loaded_run(
            RuntimePaths {
                repo_root,
                home_dir,
            },
            &recipe,
            record,
            lease,
            ExecuteLoadedRunOptions {
                run_started_payload: None,
                apply_recovery: true,
                preflight_override: Some(preflight),
            },
        )
    }

    /// Resumes a failed run using the persisted run-scoped recipe snapshot.
    pub fn resume_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<KernelRunSummary> {
        let lease_guard =
            MutationLeaseGuard::new(self, self.acquire_mutation_lease(home_dir, run_id)?);
        let preparation = (|| -> Result<(PersistedRunRecord, Recipe)> {
            let record = load_and_rebase_run_record(
                self.services.run_store.as_ref(),
                home_dir,
                repo_root,
                run_id,
            )?
            .ok_or_else(|| anyhow::anyhow!("run '{}' not found", run_id))?;
            if !matches!(
                record.status,
                KernelRunStatus::Failed | KernelRunStatus::Canceled
            ) {
                bail!(
                    "run '{}' cannot be resumed because status is '{}' (expected 'failed' or 'canceled')",
                    run_id,
                    record.status.as_str()
                );
            }
            if matches!(
                record.recovery_state,
                Some(
                    KernelRunRecoveryState::CleanupRequired
                        | KernelRunRecoveryState::CleanupCompleted
                )
            ) {
                bail!(
                    "run '{}' cannot be resumed because explicit cleanup has begun for this run; retry `transist runs cleanup {}`",
                    run_id,
                    run_id
                );
            }
            let recipe = self.load_persisted_recipe(run_id, &record)?;
            // Verify run-level asset locks so a changed plugin or prompt file is
            // caught before resume dispatches any work.
            self.verify_asset_locks(&record, &recipe, home_dir)?;
            validate_persisted_authority(
                home_dir,
                repo_root,
                &recipe,
                &record.workflow_inputs,
                record.local_only_mode,
                record.capability_authority_version == 1 && !record.test_mode,
                self.services.home_settings.as_ref(),
                self.services.plugin_metadata.as_ref(),
            )?;
            let next_record: PersistedRunRecord = (|| -> Result<PersistedRunRecord> {
                // Only an intentional root resume consumes the root's prior
                // cancellation. A reused canceled child must still honor a
                // cancellation that arrived during the parent's new activation.
                let root_run_id = &lease_guard
                    .lease
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("mutation lease guard was already finalized"))?
                    .root_run_id;
                if record.status == KernelRunStatus::Canceled && record.run_id == *root_run_id {
                    self.services.control.clear_cancel_request(root_run_id)?;
                }
                let graph = build_execution_graph(&recipe);
                let mut snapshot = record.snapshot();
                self.ensure_snapshot_steps(
                    &mut snapshot,
                    &recipe
                        .steps
                        .iter()
                        .map(|step| step.id.clone())
                        .collect::<Vec<_>>(),
                );
                let reset_steps = if let Some(reset_steps) = self.prepare_approved_gate_reentry(
                    home_dir,
                    run_id,
                    &recipe,
                    &graph,
                    &record,
                    &mut snapshot,
                )? {
                    reset_steps
                } else {
                    self.reset_failed_run(&graph, &mut snapshot)
                };
                if run_transition_allowed(snapshot.status, KernelRunStatus::Running) {
                    snapshot.status = KernelRunStatus::Running;
                }

                let mut next_record = record.clone();
                self.reset_cleanup_for_primary_activation(&recipe, &mut next_record)?;
                next_record.resume_count = next_record.resume_count.saturating_add(1);
                next_record.status = snapshot.status;
                next_record.steps = snapshot.steps.clone();
                next_record.updated_at_ms = self.services.clock.now_unix_ms();
                persist_run_snapshot(self.services.run_store.as_ref(), &next_record, &snapshot)?;
                self.publish_event(
                    "run_resumed",
                    run_id,
                    None,
                    serde_json::json!({
                        "resume_count": next_record.resume_count,
                        "reset_steps": reset_steps,
                    })
                    .into(),
                )?;
                Ok(next_record)
            })()?;

            Ok((next_record, recipe))
        })();
        let (next_record, recipe) = match preparation {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(lease_guard.release_after_error(error, "run resume preparation"));
            }
        };

        self.execute_loaded_run(
            RuntimePaths {
                repo_root,
                home_dir,
            },
            &recipe,
            next_record,
            lease_guard.into_inner()?,
            ExecuteLoadedRunOptions {
                run_started_payload: None,
                apply_recovery: false,
                preflight_override: None,
            },
        )
    }

    /// Replays a stored run from the beginning using its persisted run-scoped recipe snapshot.
    pub fn replay_run(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        source_run_id: &str,
        new_run_ref: Option<&str>,
    ) -> Result<KernelRunSummary> {
        if let Some(new_run_ref) = new_run_ref {
            transist_protocol::validate_identifier(
                transist_protocol::IdentifierKind::RunRef,
                new_run_ref,
            )
            .context("invalid run reference")?;
        }
        let lease_guard =
            MutationLeaseGuard::new(self, self.acquire_mutation_lease(home_dir, source_run_id)?);
        let preparation =
            (|| -> Result<(PersistedRunRecord, Recipe, String, std::path::PathBuf)> {
                let source_record = load_and_rebase_run_record(
                    self.services.run_store.as_ref(),
                    home_dir,
                    repo_root,
                    source_run_id,
                )?
                .ok_or_else(|| anyhow::anyhow!("run '{}' not found", source_run_id))?;
                let workspace = persisted_workspace_or_legacy(home_dir, repo_root, &source_record)?;
                let mut recipe = self.load_persisted_recipe(source_run_id, &source_record)?;
                resolve_recipe_authority_fields(&mut recipe, &source_record.workflow_inputs)?;
                resolve_recipe_filesystem_policies(
                    home_dir,
                    self.services.home_settings.as_ref(),
                    &mut recipe,
                )?;
                resolve_recipe_network_policies(
                    home_dir,
                    self.services.home_settings.as_ref(),
                    &mut recipe,
                )?;
                populate_recipe_capabilities(
                    home_dir,
                    &workspace,
                    &mut recipe,
                    &source_record.workflow_inputs,
                    source_record.local_only_mode,
                    self.services.home_settings.as_ref(),
                    self.services.plugin_metadata.as_ref(),
                )?;
                // Verify the source run's asset locks before replaying so a changed
                // plugin or prompt file is caught before the replayed run starts.
                self.verify_asset_locks(&source_record, &recipe, home_dir)?;
                let run_id = new_run_ref
                    .map(std::string::ToString::to_string)
                    .unwrap_or_else(|| self.services.ids.generate_run_id());
                if self.services.run_store.run_exists(&run_id)? {
                    bail!(
                        "run '{}' already exists; replay requires a new run id",
                        run_id
                    );
                }

                Ok((source_record, recipe, run_id, workspace))
            })();
        let (source_record, recipe, run_id, workspace) = match preparation {
            Ok(prepared) => prepared,
            Err(error) => return Err(lease_guard.release_after_error(error, "replay preparation")),
        };
        let source_lease = lease_guard.into_inner()?;
        self.release_mutation_lease(&source_lease)
            .context("replay source preparation lease release failed")?;
        let target_lease_guard =
            MutationLeaseGuard::new(self, self.acquire_mutation_lease(home_dir, &run_id)?);
        let record = match self.start_recipe_internal(
            &workspace,
            home_dir,
            &recipe,
            Some(&run_id),
            source_record.workflow_inputs.clone(),
            source_record.trusted_execution_mode,
            source_record.trust_decision.clone(),
            source_record.local_only_mode,
            RunLineage {
                mode: RunLineageMode::Replay,
                source_run_id: Some(source_run_id.to_string()),
                root_run_id: String::new(),
                parent_run_id: None,
                parent_step_id: None,
            },
            Some((
                source_record.asset_lock_version,
                source_record.asset_locks.clone(),
            )),
            self.load_workflow_asset_bundle(source_run_id, &source_record)?,
        ) {
            Ok(record) => record,
            Err(error) => {
                return Err(target_lease_guard.release_after_error(error, "replay run creation"));
            }
        };
        let target_lease = target_lease_guard.into_inner()?;

        self.execute_loaded_run(
            RuntimePaths {
                repo_root: &workspace,
                home_dir,
            },
            &recipe,
            record,
            target_lease,
            ExecuteLoadedRunOptions {
                run_started_payload: Some(
                    serde_json::json!({
                        "mode": "replay",
                        "source_run_id": source_run_id,
                    })
                    .into(),
                ),
                apply_recovery: true,
                preflight_override: None,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_recipe_internal(
        &self,
        repo_root: &Path,
        home_dir: &Path,
        recipe: &Recipe,
        run_ref: Option<&str>,
        workflow_inputs: BTreeMap<String, KernelValue>,
        trusted_execution_mode: bool,
        trust_decision: Option<crate::types::TrustDecisionRecord>,
        local_only_mode: bool,
        mut lineage: RunLineage,
        preset_asset_locks: Option<(u32, Vec<crate::types::AssetLock>)>,
        preset_workflow_assets: Option<(Vec<u8>, Vec<WorkflowAssetBlob>)>,
    ) -> Result<PersistedRunRecord> {
        let measurement_scope = crate::PreflightMeasurementScope::RunBirth;
        let _total_measurement =
            crate::phase_span(measurement_scope, crate::PreflightMeasurementPhase::Total);
        let cancellation_root_run_id =
            (!lineage.root_run_id.trim().is_empty()).then_some(lineage.root_run_id.as_str());
        let _plugin_lifecycle_reservation = MutationLeaseGuard::new(
            self,
            acquire_plugin_lifecycle_reservation_for_run_birth(
                self.services.lease.as_ref(),
                self.services.control.as_ref(),
                cancellation_root_run_id,
                PLUGIN_LIFECYCLE_RESERVATION_WAIT,
            )?,
        );
        if let Some(run_ref) = run_ref {
            transist_protocol::validate_identifier(
                transist_protocol::IdentifierKind::RunRef,
                run_ref,
            )
            .context("invalid run reference")?;
        }
        let run_id = run_ref
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| self.services.ids.generate_run_id());
        crate::kernel::test_mode::ensure_workflow_version_allowed(
            &recipe.workflow.id,
            &recipe.workflow.version,
        )?;
        if self.services.run_store.run_exists(&run_id)? {
            bail!(
                "run '{}' already exists; use 'transist runs retry {}' or 'transist runs replay {} --run-ref <new-run-id>'",
                run_id,
                run_id,
                run_id
            );
        }

        // Detect whether this run will be its own root. Subflow children
        // arrive with a populated `lineage.root_run_id` and must NOT touch
        // the root's control marker; only fresh root launches reach the
        // defensive cleanup below.
        let is_self_rooted = lineage.root_run_id.trim().is_empty();

        // Use the canonical id@version format so the policy (including any
        // ManagedRuntimePolicy RAC check) sees a ref that matches the
        // `workflow_refs` entries stored in the auth context.
        let workflow_ref = if is_self_rooted {
            format!("{}@{}", recipe.workflow.id, recipe.workflow.version)
        } else {
            let root_record =
                load_run_record(self.services.run_store.as_ref(), &lineage.root_run_id)?
                    .ok_or_else(|| {
                        anyhow::anyhow!("root run '{}' not found", lineage.root_run_id)
                    })?;
            self.policy_workflow_ref(&root_record)?
        };
        let run_decision = self
            .services
            .policy
            .validate_run_start(repo_root, &workflow_ref)?;
        if !run_decision.allowed {
            bail!(
                "run start denied (code={}, message={})",
                run_decision
                    .reason_code
                    .as_deref()
                    .unwrap_or("policy_denied"),
                run_decision.message.as_deref().unwrap_or("denied")
            );
        }

        // Defensively drop any stale cancel-request marker for this root id
        // before persisting the new run record. A previous run sharing this
        // root may have left a `cancel-request.json` behind (e.g. a process
        // crashed before terminal cleanup ran, or the marker was authored by
        // older Transist versions that did not clear on completion). If we
        // skip this step the kernel's pre-loop cancel check would honor the
        // stale marker on the very first iteration and produce a run with
        // every step canceled and zero executions. This is the symmetric
        // partner of the resume path, which already clears the marker when
        // continuing a previously-canceled run. Errors propagate so an I/O
        // failure surfaces immediately instead of corrupting the new run.
        if is_self_rooted {
            self.services.control.clear_cancel_request(&run_id)?;
        }

        let now_ms = self.services.clock.now_unix_ms();
        let mut recipe = recipe.clone();
        resolve_recipe_authority_fields(&mut recipe, &workflow_inputs)?;
        let execution_workspace =
            resolve_recipe_workspace(home_dir, repo_root, &mut recipe, &workflow_inputs)?;
        resolve_recipe_filesystem_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut recipe,
        )?;
        resolve_recipe_network_policies(
            home_dir,
            self.services.home_settings.as_ref(),
            &mut recipe,
        )?;
        populate_recipe_capabilities(
            home_dir,
            &execution_workspace,
            &mut recipe,
            &workflow_inputs,
            local_only_mode,
            self.services.home_settings.as_ref(),
            self.services.plugin_metadata.as_ref(),
        )?;
        // Every authored and persisted manual-gate id must satisfy the
        // identifier rule before the run record is created. Invalid gate ids
        // (traversal, leading punctuation, whitespace, non-ASCII) fail closed
        // before run birth.
        validate_recipe_gate_ids(&recipe)?;
        let recipe_yaml = serialize_recipe_snapshot(&recipe)?;
        let recipe_snapshot = RecipeSnapshotRef {
            path: "recipe.yaml".to_string(),
            sha256: sha256_hex(recipe_yaml.as_bytes()),
        };
        let step_ids: Vec<String> = recipe.steps.iter().map(|step| step.id.clone()).collect();
        let snapshot = KernelRunSnapshot::new(
            &run_id,
            &recipe.workflow.id,
            &recipe.workflow.version,
            &step_ids,
        );
        if lineage.root_run_id.is_empty() {
            lineage.root_run_id = run_id.clone();
        }
        let mut record = PersistedRunRecord::from_snapshot(
            snapshot,
            Some(recipe_snapshot),
            Some(lineage),
            workflow_inputs,
            0,
            now_ms,
            now_ms,
        );
        record.trusted_execution_mode = trusted_execution_mode;
        record.trust_decision = trust_decision;
        record.local_only_mode = local_only_mode;
        record.runtime_mode = self.posture.operating_mode.as_str().to_string();
        record.policy_profile = self.posture.policy_profile.clone();
        record.capability_enforcement = self.posture.capability_enforcement.clone();
        record.test_mode = crate::kernel::test_mode::is_test_workflow(&recipe.workflow.version);
        record.capability_authority_version = 1;
        record.workspace = recipe.workspace.clone();
        if record
            .trust_decision
            .as_ref()
            .is_some_and(crate::types::TrustDecisionRecord::denied)
        {
            bail!(
                "trusted execution denied before run birth (code={}, verification={})",
                record
                    .trust_decision
                    .as_ref()
                    .and_then(|decision| decision.reason_code.as_deref())
                    .unwrap_or("trusted_execution_denied"),
                record
                    .trust_decision
                    .as_ref()
                    .and_then(|decision| decision.failed_verification_type.as_deref())
                    .unwrap_or("unknown")
            );
        }
        // Replay preserves the source run's persisted asset locks instead of
        // rebuilding them from live prompt/plugin files. Rebuilding would let
        // a prompt changed between source verification and replay lock
        // construction become the replay's accepted baseline. Fresh runs
        // (no preset) still build locks from current bytes.
        let has_preset_asset_locks = preset_asset_locks.is_some();
        let (asset_lock_version, asset_locks) = match preset_asset_locks {
            Some((version, locks)) => (version, locks),
            None if preset_workflow_assets.is_some() => (
                1,
                build_asset_locks_from_preset(
                    &recipe,
                    &preset_workflow_assets.as_ref().expect("checked above").0,
                )?,
            ),
            None => (
                1,
                crate::kernel::build_asset_locks(
                    &recipe,
                    &record.workflow_inputs,
                    &record.workflow_vars,
                    self.services.prompt_files.as_ref(),
                )?,
            ),
        };
        record.asset_locks = asset_locks;
        record.asset_lock_version = asset_lock_version;
        if record
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.mode == RunLineageMode::Fresh)
        {
            // Fresh-run readiness is evaluated against the complete in-memory
            // candidate before any durable run path or locator is published.
            // Asset snapshots are not available from the run store until birth,
            // so candidate preflight uses the equivalent source-backed v1 lock
            // lookup. The persisted run is marked snapshot-backed below when
            // captured workflow assets are available.
            let mut preflight_candidate = record.clone();
            preflight_candidate.asset_lock_version = 1;
            preflight_candidate.workflow_asset_snapshot = false;
            let preflight = crate::measure_preflight_phase(
                measurement_scope,
                crate::PreflightMeasurementPhase::Preflight,
                || {
                    self.build_preflight_record(
                        RuntimePaths {
                            repo_root: &execution_workspace,
                            home_dir,
                        },
                        &recipe,
                        &preflight_candidate,
                        false,
                    )
                },
            )?;
            if preflight.status == PreflightStatus::Failed {
                let blockers = preflight
                    .checks
                    .iter()
                    .filter(|check| check.status == PreflightStatus::Failed)
                    .map(|check| format!("{}: {}", check.kind, check.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                bail!("preflight failed before run birth: {blockers}");
            }
            record.preflight = Some(preflight);
        }
        let has_preset_workflow_assets = preset_workflow_assets.is_some();
        let supports_workflow_assets = self.services.run_store.supports_workflow_asset_snapshots();
        let (workflow_asset_manifest, workflow_asset_blobs) = crate::measure_preflight_phase(
            measurement_scope,
            crate::PreflightMeasurementPhase::AssetCapture,
            || -> Result<_> {
                Ok(match preset_workflow_assets {
                    Some((manifest, blobs)) => (Some(manifest), blobs),
                    None if supports_workflow_assets => capture_recipe_workflow_assets(
                        &recipe,
                        self.services.prompt_files.as_ref(),
                    )?,
                    None => (None, Vec::new()),
                })
            },
        )?;
        if (has_preset_workflow_assets
            || (!has_preset_asset_locks && workflow_asset_manifest.is_some()))
            && supports_workflow_assets
        {
            record.asset_lock_version = 1;
            record.workflow_asset_snapshot = true;
        } else if has_preset_workflow_assets {
            bail!("workflow asset snapshot storage is unavailable for replay");
        }
        let run_inputs_json =
            serialize_run_inputs_record(&run_id, &recipe, &record.workflow_inputs)?;
        // Fail before birth when the composed backend cannot enforce document publication.
        if !recipe.documents.is_empty() && self.services.run_store.document_store().is_none() {
            bail!("document storage unavailable");
        }
        for (name, declaration) in &recipe.documents {
            declaration.validate(name).map_err(anyhow::Error::msg)?;
        }
        let provenance_json = serialize_birth_provenance_record(
            home_dir,
            &record,
            RunProvenancePolicyDecision {
                allowed: true,
                reason_code: None,
                message: None,
                mode: "run_start".to_string(),
            },
            now_ms,
        )?;
        let birth_artifacts = RunBirthArtifacts {
            // Document initialization is part of this same root birth transaction.
            record: &record,
            recipe_yaml: &recipe_yaml,
            run_inputs_json: &run_inputs_json,
            provenance_json: &provenance_json,
            workflow_asset_manifest_json: workflow_asset_manifest.as_deref(),
            workflow_asset_blobs: &workflow_asset_blobs,
        };
        crate::measure_preflight_phase(
            measurement_scope,
            crate::PreflightMeasurementPhase::DurablePersistence,
            || {
                self.services
                    .run_store
                    .create_run_staged(&birth_artifacts)?;
                self.services
                    .run_store
                    .publish_run_locator(home_dir, &record)
            },
        )?;
        Ok(record)
    }

    pub(crate) fn execute_loaded_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        record: PersistedRunRecord,
        lease: LeaseHandle,
        options: ExecuteLoadedRunOptions,
    ) -> Result<KernelRunSummary> {
        let workspace = persisted_workspace_or_legacy(paths.home_dir, paths.repo_root, &record)?;
        self.dispatch_loaded_run(
            RuntimePaths {
                repo_root: &workspace,
                home_dir: paths.home_dir,
            },
            recipe,
            record,
            lease,
            options,
        )
    }

    fn dispatch_loaded_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        record: PersistedRunRecord,
        lease: LeaseHandle,
        options: ExecuteLoadedRunOptions,
    ) -> Result<KernelRunSummary> {
        let control_watcher = self.start_control_watcher(paths.home_dir, &lease);
        let result = (|| {
            let recovered = self.prepare_loaded_run_recovery(
                paths.home_dir,
                recipe,
                &record,
                options.apply_recovery,
            )?;
            let activated = match self.activate_loaded_run(recovered, &lease, &options)? {
                LoadedRunActivation::Ready(activated) => activated,
                LoadedRunActivation::Canceled(canceled) => {
                    return self.finish_canceled_loaded_run(paths, recipe, &lease, canceled);
                }
            };
            let preflighted = match self.preflight_loaded_run(
                paths,
                recipe,
                activated,
                &lease,
                options.preflight_override,
                options.apply_recovery,
            )? {
                LoadedRunPreflight::Ready(preflighted) => *preflighted,
                LoadedRunPreflight::Finished(summary) => return Ok(summary),
            };
            let scheduled = match self.schedule_loaded_run(paths, recipe, preflighted, &lease)? {
                LoadedRunScheduling::Ready(scheduled) => scheduled,
                LoadedRunScheduling::Canceled(canceled) => {
                    return self.finish_canceled_loaded_run(paths, recipe, &lease, canceled);
                }
            };
            self.finalize_scheduled_loaded_run(paths, recipe, &lease, scheduled)
        })();
        drop(control_watcher);
        self.finish_mutation_operation(result, lease, "run execution")
            .map(|outcome| {
                let mut summary = outcome.value;
                summary.lease_release_warning = outcome.lease_release_warning;
                summary
            })
    }

    fn activate_loaded_run(
        &self,
        recovered: RecoveredLoadedRun,
        lease: &LeaseHandle,
        options: &ExecuteLoadedRunOptions,
    ) -> Result<LoadedRunActivation> {
        let LoadedRunState {
            graph,
            mut snapshot,
            mut persisted_record,
            last_stable_step_id,
        } = recovered.state;
        if self.cancel_request_pending(&lease.root_run_id)? {
            return Ok(LoadedRunActivation::Canceled(CanceledLoadedRun {
                snapshot,
                persisted_record,
            }));
        }
        if run_transition_allowed(snapshot.status, KernelRunStatus::Running) {
            snapshot.status = KernelRunStatus::Running;
        }
        persisted_record.updated_at_ms = self.services.clock.now_unix_ms();
        persist_run_snapshot(
            self.services.run_store.as_ref(),
            &persisted_record,
            &snapshot,
        )?;
        if let Some(payload) = options.run_started_payload.clone() {
            self.publish_event("run_started", &snapshot.run_id, None, payload)?;
        }
        self.services
            .telemetry
            .record(&TelemetryEvent::RunSessionStarted {
                run_id: snapshot.run_id.clone(),
                workflow_id: snapshot.workflow_id.clone(),
                workflow_version: snapshot.workflow_version.clone(),
                lineage_mode: persisted_record
                    .lineage
                    .as_ref()
                    .map(|lineage| match lineage.mode {
                        RunLineageMode::Fresh => "fresh".to_string(),
                        RunLineageMode::Replay => "replay".to_string(),
                    })
                    .unwrap_or_else(|| "fresh".to_string()),
                resume_count: persisted_record.resume_count,
            });
        Ok(LoadedRunActivation::Ready(ActivatedLoadedRun {
            state: LoadedRunState {
                graph,
                snapshot,
                persisted_record,
                last_stable_step_id,
            },
        }))
    }

    fn preflight_loaded_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        activated: ActivatedLoadedRun,
        lease: &LeaseHandle,
        preflight_override: Option<PersistedPreflightRecord>,
        apply_recovery: bool,
    ) -> Result<LoadedRunPreflight> {
        let LoadedRunState {
            graph,
            mut snapshot,
            mut persisted_record,
            last_stable_step_id,
        } = activated.state;
        let preflight = self.prepare_loaded_run_preflight(
            paths,
            recipe,
            &persisted_record,
            &snapshot.run_id,
            preflight_override,
            apply_recovery,
        )?;
        persisted_record.preflight = Some(preflight.clone());
        persisted_record.updated_at_ms = self.services.clock.now_unix_ms();
        if preflight.status == PreflightStatus::Failed
            && run_transition_allowed(snapshot.status, KernelRunStatus::Failed)
        {
            snapshot.status = KernelRunStatus::Failed;
        }
        persisted_record.status = snapshot.status;
        persisted_record.steps = snapshot.steps.clone();
        if preflight.status == PreflightStatus::Failed {
            self.initialize_failed_cleanup_state(recipe, &mut persisted_record)?;
        }
        persist_run_record(self.services.run_store.as_ref(), &persisted_record)?;
        if preflight.status == PreflightStatus::Failed {
            self.maybe_execute_automatic_cleanup(
                paths,
                recipe,
                &mut persisted_record,
                lease,
                CleanupTrigger::Failed,
            )?;
            self.publish_event(
                "run_stopped",
                &snapshot.run_id,
                None,
                serde_json::json!({ "status": snapshot.status.as_str() }).into(),
            )?;
            self.services
                .telemetry
                .record(&TelemetryEvent::RunSessionFinished {
                    run_id: snapshot.run_id.clone(),
                    terminal_status: snapshot.status.as_str().to_string(),
                });
            if snapshot.run_id == lease.root_run_id {
                self.services
                    .control
                    .clear_cancel_request(&lease.root_run_id)?;
            }
            return Ok(LoadedRunPreflight::Finished(run_summary_from_snapshot(
                &snapshot,
            )));
        }
        Ok(LoadedRunPreflight::Ready(Box::new(PreflightedLoadedRun {
            state: LoadedRunState {
                graph,
                snapshot,
                persisted_record,
                last_stable_step_id,
            },
        })))
    }

    fn schedule_loaded_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        preflighted: PreflightedLoadedRun,
        lease: &LeaseHandle,
    ) -> Result<LoadedRunScheduling> {
        let LoadedRunState {
            graph,
            mut snapshot,
            mut persisted_record,
            mut last_stable_step_id,
        } = preflighted.state;
        let max_iterations = graph.max_iterations.unwrap_or(10_000);
        let mut iteration = 0_u32;
        while iteration < max_iterations {
            iteration += 1;
            if self.cancel_request_pending(&lease.root_run_id)? {
                return Ok(LoadedRunScheduling::Canceled(CanceledLoadedRun {
                    snapshot,
                    persisted_record,
                }));
            }
            let now = self.services.clock.now_unix_ms();
            let frontier = runnable_frontier(
                &graph,
                &snapshot,
                &persisted_record.workflow_inputs,
                &persisted_record.workflow_vars,
                now,
            );
            if frontier.is_empty() {
                let changed = self.mark_non_dispatchable_skipped(
                    &graph,
                    &mut snapshot,
                    &persisted_record.workflow_inputs,
                    &persisted_record.workflow_vars,
                );
                if !changed.is_empty() {
                    for step_id in changed {
                        if let Some(step) = snapshot.steps.get(&step_id) {
                            persist_step_state(
                                self.services.run_store.as_ref(),
                                &snapshot.run_id,
                                step,
                            )?;
                        }
                    }
                    continue;
                }
                snapshot.status = self.terminal_status(&snapshot);
                break;
            }
            for step_id in frontier {
                if self.cancel_request_pending(&lease.root_run_id)? {
                    return Ok(LoadedRunScheduling::Canceled(CanceledLoadedRun {
                        snapshot,
                        persisted_record,
                    }));
                }
                if let Some(step_result) = self.execute_loaded_step(
                    paths,
                    recipe,
                    &mut snapshot,
                    &persisted_record,
                    step_id.clone(),
                )? && self.persist_loaded_step_result(
                    paths,
                    recipe,
                    &graph,
                    &mut persisted_record,
                    &mut snapshot,
                    &mut last_stable_step_id,
                    step_id,
                    step_result,
                )? == StepExecutionControl::Break
                {
                    break;
                }
            }
            if snapshot.status == KernelRunStatus::Blocked {
                break;
            }
        }
        Ok(LoadedRunScheduling::Ready(ScheduledLoadedRun {
            state: LoadedRunState {
                graph,
                snapshot,
                persisted_record,
                last_stable_step_id,
            },
        }))
    }

    fn finalize_scheduled_loaded_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        lease: &LeaseHandle,
        scheduled: ScheduledLoadedRun,
    ) -> Result<KernelRunSummary> {
        let LoadedRunState {
            snapshot,
            mut persisted_record,
            ..
        } = scheduled.state;
        self.finalize_loaded_run(paths, recipe, lease, &mut persisted_record, &snapshot)?;
        Ok(run_summary_from_snapshot(&snapshot))
    }

    fn finish_canceled_loaded_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        lease: &LeaseHandle,
        canceled: CanceledLoadedRun,
    ) -> Result<KernelRunSummary> {
        let CanceledLoadedRun {
            mut snapshot,
            mut persisted_record,
        } = canceled;
        self.finish_canceled_run(paths, recipe, lease, &mut persisted_record, &mut snapshot)
    }

    fn execute_loaded_step(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        snapshot: &mut KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step_id: String,
    ) -> Result<Option<StepExecutionResult>> {
        let Some(step) = recipe.steps.iter().find(|step| step.id == step_id) else {
            return Ok(None);
        };
        let Some((snapshot_for_invoke, rerun_completed_subflow)) =
            self.prepare_loaded_step_state(snapshot, step, &step_id)?
        else {
            return Ok(None);
        };
        let execution = {
            let Some(step_state) = snapshot.steps.get_mut(&step_id) else {
                return Ok(None);
            };
            self.execute_loaded_step_descriptor(
                paths,
                recipe,
                &snapshot_for_invoke,
                persisted_record,
                step,
                step_state,
                rerun_completed_subflow,
            )?
        };
        let (result, state_mutations, validation) = self.complete_loaded_step_result(
            paths,
            snapshot,
            persisted_record,
            &step_id,
            execution,
        )?;
        Ok(Some(StepExecutionResult {
            result,
            state_mutations,
            validation,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_loaded_step_descriptor(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        snapshot: &KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step: &transist_protocol::RecipeStep,
        step_state: &mut KernelStepState,
        rerun_completed_subflow: bool,
    ) -> Result<LoadedStepExecution> {
        Ok(match &step.execution {
            StepExecutionDescriptor::Invoke { .. } => self.execute_loaded_invoke_step(
                paths,
                recipe,
                snapshot,
                persisted_record,
                step,
                step_state,
            )?,
            StepExecutionDescriptor::Control { primitive }
                if primitive == ControlStepKind::GateManual.as_wire_str() =>
            {
                let gate_id = validated_gate_id_for_step(step)?;
                let operation = if load_gate_decision_record(
                    self.services.run_store.as_ref(),
                    paths.home_dir,
                    &snapshot.run_id,
                    &gate_id,
                )?
                .is_some()
                {
                    "decide"
                } else {
                    "request"
                };
                let capabilities =
                    persisted_capabilities_for_step(recipe, &step.id, Some(operation))?;
                let policy_workflow_ref = self.policy_workflow_ref(persisted_record)?;
                let descriptor = super::build_gate_policy_descriptor_for_workflow(
                    &policy_workflow_ref,
                    &capabilities,
                )?;
                let decision = self.services.policy.validate_step_execution(
                    &snapshot.run_id,
                    &step.id,
                    &descriptor,
                )?;
                let mut result = self.execute_control_step(
                    paths.home_dir,
                    &snapshot.run_id,
                    step,
                    step_state.executions,
                    step_state.activation_executions(),
                    step_state.last_result.as_ref(),
                );
                if !decision.allowed {
                    result.status = "failed".to_string();
                    result.exit_code = Some(2);
                    result.stderr = "approval capability denied".to_string();
                    result.output["policy"] = serde_json::Value::from(decision.payload.clone());
                    result.error = Some(ProtocolError::new(
                        CanonicalErrorCode::CapabilityNotGranted,
                        decision
                            .message
                            .unwrap_or_else(|| "approval capability was not granted".to_string()),
                    ));
                }
                LoadedStepExecution {
                    result,
                    dispatch_outcome: DispatchOutcome::NotDispatched,
                    validation: None,
                }
            }
            StepExecutionDescriptor::Control { .. } => LoadedStepExecution {
                result: self.execute_control_step(
                    paths.home_dir,
                    &snapshot.run_id,
                    step,
                    step_state.executions,
                    step_state.activation_executions(),
                    step_state.last_result.as_ref(),
                ),
                dispatch_outcome: DispatchOutcome::NotDispatched,
                validation: None,
            },
            StepExecutionDescriptor::Subflow { recipe } => LoadedStepExecution {
                result: self.execute_subflow_step(SubflowExecutionContext {
                    paths,
                    run_id: &snapshot.run_id,
                    step,
                    execution: step_state.executions,
                    rerun_completed_run: rerun_completed_subflow,
                    previous_child_run_id: step_state.last_result.as_ref().and_then(|result| {
                        result
                            .output
                            .get("child_run_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                    }),
                    recipe,
                    workflow_inputs: &persisted_record.workflow_inputs,
                    workflow_vars: &persisted_record.workflow_vars,
                    snapshot,
                    replay_source_run_id: persisted_record
                        .lineage
                        .as_ref()
                        .filter(|lineage| lineage.mode == RunLineageMode::Replay)
                        .and_then(|lineage| lineage.source_run_id.as_deref()),
                })?,
                dispatch_outcome: DispatchOutcome::NotDispatched,
                validation: None,
            },
        })
    }

    fn complete_loaded_step_result(
        &self,
        paths: RuntimePaths<'_>,
        snapshot: &mut KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step_id: &str,
        execution: LoadedStepExecution,
    ) -> Result<(
        ExecutionResult,
        Vec<crate::failures::StateMutation>,
        LoadedResultValidationMetadata,
    )> {
        let Some(step_state) = snapshot.steps.get(step_id) else {
            return Ok((execution.result, Vec::new(), Default::default()));
        };
        let LoadedStepExecution {
            mut result,
            dispatch_outcome,
            validation,
        } = execution;
        let needs_validation = validation.is_none();
        let mut validation = validation.unwrap_or_default();
        if needs_validation {
            if let Some(request) = step_state.last_request.as_ref() {
                match self.validate_loaded_invoke_result(LoadedInvokeResultValidationInput {
                    request,
                    result,
                    workspace_root: paths.repo_root,
                }) {
                    LoadedInvokeResultValidationResult::Denied(denied) => {
                        result = denied.result;
                        validation = denied.metadata;
                    }
                    LoadedInvokeResultValidationResult::Accepted(accepted) => {
                        let normalized = self.normalize_loaded_invoke_result(
                            LoadedInvokeResultNormalizationInput {
                                request: accepted.request,
                                result: accepted.result,
                                redactor: Redactor::default(),
                                metadata: accepted.metadata,
                                trusted_execution_mode: persisted_record.trusted_execution_mode,
                            },
                        )?;
                        let normalized = match normalized {
                            LoadedInvokeResultNormalizationResult::Normalized(normalized)
                            | LoadedInvokeResultNormalizationResult::Failed(normalized) => {
                                normalized
                            }
                        };
                        result = normalized.result;
                        validation = normalized.metadata;
                    }
                }
            } else if let Err(error) = normalize_result_file_refs(
                &mut result,
                persisted_record.trusted_execution_mode,
                |path| self.services.run_store.read_result_file(path),
            ) {
                result = failed_step_result(
                    &snapshot.run_id,
                    step_id,
                    step_state.executions,
                    error.to_string(),
                );
            }
        }
        if validation.sandbox_failure.is_none() {
            validation.sandbox_failure = result
                .output
                .get("__sandbox_failed__")
                .filter(|value| value.is_object())
                .cloned();
        }
        let artifact_mutation_count = result.artifacts.len();
        let evidence_mutation_count = result.evidence.len();
        self.append_runtime_invoke_evidence(
            step_state.last_request.as_ref(),
            persisted_record,
            &mut result,
        )?;
        if dispatch_outcome == DispatchOutcome::Dispatched {
            self.append_external_transfer_evidence(
                step_state.last_request.as_ref(),
                persisted_record,
                &mut result,
            )?;
        }
        let state_mutations =
            self.persist_result_files(&result, artifact_mutation_count, evidence_mutation_count)?;
        Ok((result, state_mutations, validation))
    }

    fn prepare_loaded_step_state(
        &self,
        snapshot: &mut KernelRunSnapshot,
        step: &transist_protocol::RecipeStep,
        step_id: &str,
    ) -> Result<Option<(KernelRunSnapshot, bool)>> {
        let snapshot_for_invoke = snapshot.clone();
        let Some(step_state) = snapshot.steps.get_mut(step_id) else {
            return Ok(None);
        };
        let rerun_completed_subflow =
            matches!(step.execution, StepExecutionDescriptor::Subflow { .. })
                && step_state.status == KernelStepStatus::Pending
                && step_state.executions > 0;
        if step_state.status == KernelStepStatus::Pending {
            if step_transition_allowed(KernelStepStatus::Pending, KernelStepStatus::Runnable) {
                step_state.status = KernelStepStatus::Runnable;
            } else {
                return Ok(None);
            }
        }
        if step_state.status == KernelStepStatus::RetryWait {
            if step_transition_allowed(KernelStepStatus::RetryWait, KernelStepStatus::Runnable) {
                step_state.status = KernelStepStatus::Runnable;
            } else {
                return Ok(None);
            }
        }
        step_state.skip_reason = None;
        if !step_transition_allowed(step_state.status, KernelStepStatus::Dispatching) {
            return Ok(None);
        }
        step_state.status = KernelStepStatus::Dispatching;
        persist_step_state(
            self.services.run_store.as_ref(),
            &snapshot.run_id,
            step_state,
        )?;
        if step_transition_allowed(step_state.status, KernelStepStatus::Running) {
            step_state.status = KernelStepStatus::Running;
        }
        step_state.executions = step_state.executions.checked_add(1).ok_or_else(|| {
            anyhow::anyhow!(
                "execution counter overflow for run '{}' step '{}'",
                snapshot.run_id,
                step_id
            )
        })?;
        if self.services.run_store.step_execution_record_exists(
            &snapshot.run_id,
            step_id,
            step_state.executions,
        )? {
            bail!(
                "execution record already exists for run '{}' step '{}' execution {}; cannot dispatch over an immutable prior execution",
                snapshot.run_id,
                step_id,
                step_state.executions
            );
        }
        step_state.started_at_ms = Some(self.services.clock.now_unix_ms());
        step_state.finished_at_ms = None;
        persist_step_state(
            self.services.run_store.as_ref(),
            &snapshot.run_id,
            step_state,
        )?;
        crate::infra::execution_faults::check_fault(
            crate::infra::execution_faults::FAULT_STEP_DISPATCH_INTENT,
        )?;
        self.publish_event(
            "step_started",
            &snapshot.run_id,
            Some(step_id),
            serde_json::json!({ "execution": step_state.executions }).into(),
        )?;
        crate::infra::execution_faults::check_fault(
            crate::infra::execution_faults::FAULT_STEP_START_EVENT,
        )?;
        let (step_adapter_kind, step_action) = self.step_descriptor_labels(&step.execution);
        self.services
            .telemetry
            .record(&TelemetryEvent::StepStarted {
                run_id: snapshot.run_id.clone(),
                step_id: step_id.to_string(),
                execution: step_state.executions,
                descriptor: step.execution.clone(),
                adapter_kind: step_adapter_kind,
                action: step_action,
            });
        Ok(Some((snapshot_for_invoke, rerun_completed_subflow)))
    }

    fn execute_loaded_invoke_step(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        snapshot: &KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step: &transist_protocol::RecipeStep,
        step_state: &mut KernelStepState,
    ) -> Result<LoadedStepExecution> {
        self.execute_loaded_invoke_step_dispatch(
            paths,
            recipe,
            snapshot,
            persisted_record,
            step,
            step_state,
        )
    }

    fn execute_loaded_invoke_step_dispatch(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        snapshot: &KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step: &transist_protocol::RecipeStep,
        step_state: &mut KernelStepState,
    ) -> Result<LoadedStepExecution> {
        let step_id = step.id.clone();
        let prepared = match self.prepare_loaded_invoke_request(
            paths,
            recipe,
            snapshot,
            persisted_record,
            step,
            step_state,
        ) {
            Ok(Some(prepared)) => prepared,
            Ok(None) => {
                return Err(anyhow::anyhow!(
                    "invoke step '{}' did not produce an execution request",
                    step_id
                ));
            }
            Err(error) => {
                let (result, dispatch_outcome) =
                    self.failed_loaded_invoke_request(snapshot, step, step_state, error);
                return Ok(LoadedStepExecution {
                    result,
                    dispatch_outcome,
                    validation: None,
                });
            }
        };
        let authorized = match self.authorize_loaded_invoke_request(
            paths,
            snapshot,
            persisted_record,
            step,
            step_state,
            prepared,
        )? {
            LoadedInvokeAuthorization::Allowed(authorized) => authorized,
            LoadedInvokeAuthorization::Denied(result) => {
                return Ok(LoadedStepExecution {
                    result,
                    dispatch_outcome: DispatchOutcome::NotDispatched,
                    validation: None,
                });
            }
        };
        if authorized
            .request
            .metadata
            .get("document_bindings")
            .is_none()
            && !transist_protocol::step_deferred_bindings(step)
                .map_err(anyhow::Error::msg)?
                .is_empty()
        {
            if let Err(error) = self.validate_deferred_dispatch(
                paths,
                &authorized.request,
                &authorized.secret_refs,
                authorized.remote_secret_policy,
            ) {
                let mut result = failed_step_result(
                    &snapshot.run_id,
                    &step.id,
                    step_state.executions,
                    error.to_string(),
                );
                result.error = Some(ProtocolError::new(
                    CanonicalErrorCode::ValidationError,
                    error.to_string(),
                ));
                return Ok(LoadedStepExecution {
                    result,
                    dispatch_outcome: DispatchOutcome::NotDispatched,
                    validation: None,
                });
            }
        }
        let secret_values = match self.resolve_loaded_invoke_secrets(
            paths,
            snapshot,
            persisted_record,
            step,
            &authorized,
        )? {
            LoadedInvokeSecretResolution::Ready(values) => values,
            LoadedInvokeSecretResolution::Denied(result) => {
                return Ok(LoadedStepExecution {
                    result: *result,
                    dispatch_outcome: DispatchOutcome::NotDispatched,
                    validation: None,
                });
            }
        };
        match self.stage_and_execute_loaded_invoke(&authorized, secret_values, paths) {
            LoadedInvokeAdapterResult::Dispatched(dispatched) => {
                let LoadedInvokeDispatched {
                    request,
                    result,
                    staging,
                    redactor,
                } = *dispatched;
                let validated =
                    self.validate_loaded_invoke_result(LoadedInvokeResultValidationInput {
                        request: &request,
                        result,
                        workspace_root: paths.repo_root,
                    });
                let (result, validation) = match validated {
                    LoadedInvokeResultValidationResult::Denied(mut denied) => {
                        redactor.redact_execution_result(&mut denied.result);
                        (denied.result, denied.metadata)
                    }
                    LoadedInvokeResultValidationResult::Accepted(mut validated) => {
                        // Verify the adapter's checksum while references still
                        // identify staging files. Promotion rewrites references
                        // with hashes of durable, possibly redacted bytes.
                        if let Err(error) = verify_result_file_refs(
                            &validated.result,
                            persisted_record.trusted_execution_mode,
                            |path| self.services.run_store.read_result_file(path),
                        ) {
                            if persisted_record.trusted_execution_mode {
                                validated.metadata.trusted_result_file_mismatch = Some(
                                    metadata_string(&validated.request.metadata, "plugin_id")?,
                                );
                            }
                            let mut result = failed_step_result(
                                &validated.request.run_id,
                                &validated.request.step.id,
                                validated.request.step.execution,
                                error.to_string(),
                            );
                            redactor.redact_execution_result(&mut result);
                            return Ok(LoadedStepExecution {
                                result,
                                dispatch_outcome: DispatchOutcome::Dispatched,
                                validation: Some(validated.metadata),
                            });
                        }
                        let promoted =
                            self.promote_loaded_invoke_result(LoadedInvokeResultPromotionInput {
                                validated,
                                staging,
                                redactor,
                            });
                        let promoted = match promoted {
                            LoadedInvokeResultPromotionResult::Promoted(promoted)
                            | LoadedInvokeResultPromotionResult::Failed(promoted) => promoted,
                        };
                        let normalized = self.normalize_loaded_invoke_result(
                            LoadedInvokeResultNormalizationInput {
                                request: promoted.request,
                                result: promoted.result,
                                redactor: promoted.redactor,
                                metadata: promoted.metadata,
                                trusted_execution_mode: persisted_record.trusted_execution_mode,
                            },
                        )?;
                        match normalized {
                            LoadedInvokeResultNormalizationResult::Normalized(normalized)
                            | LoadedInvokeResultNormalizationResult::Failed(normalized) => {
                                (normalized.result, normalized.metadata)
                            }
                        }
                    }
                };
                crate::infra::execution_faults::check_fault(
                    crate::infra::execution_faults::FAULT_STEP_RESULT_PROMOTION,
                )?;
                Ok(LoadedStepExecution {
                    result,
                    dispatch_outcome: DispatchOutcome::Dispatched,
                    validation: Some(validation),
                })
            }
            LoadedInvokeAdapterResult::Failed(result) => Ok(LoadedStepExecution {
                result: *result,
                dispatch_outcome: DispatchOutcome::NotDispatched,
                validation: None,
            }),
        }
    }

    fn prepare_loaded_invoke_request(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        snapshot: &KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step: &transist_protocol::RecipeStep,
        step_state: &KernelStepState,
    ) -> Result<Option<crate::dispatch::PreparedExecution>> {
        persisted_capabilities_for_step(recipe, &step.id, None).and_then(|persisted| {
            make_execution_request_with_capabilities(
                ExecutionRequestContext {
                    repo_root: paths.repo_root,
                    home_dir: paths.home_dir,
                    recipe_source_dir: recipe.source_dir.as_deref().map(Path::new),
                    run_id: &snapshot.run_id,
                    workflow_id: &snapshot.workflow_id,
                    workflow_inputs: &persisted_record.workflow_inputs,
                    workflow_vars: &persisted_record.workflow_vars,
                    execution: step_state.executions,
                    invoke_id: &self.services.ids.generate_step_id(),
                    local_only_mode: persisted_record.local_only_mode,
                },
                step,
                recipe.workspace.is_some(),
                &persisted,
                persisted_record,
                self.services.run_store.as_ref(),
                self.services.plugin_metadata.as_ref(),
                self.services.prompt_files.as_ref(),
                Some(snapshot),
                false,
                &BTreeSet::new(),
            )
        })
    }

    pub(crate) fn validate_deferred_dispatch(
        &self,
        paths: RuntimePaths<'_>,
        request: &ExecutionRequest,
        secret_refs: &[String],
        remote_secret_policy: RemoteSecretPolicy,
    ) -> Result<()> {
        let available_secret_refs =
            self.check_secret_refs_available(secret_refs)
                .map_err(|error| match error {
                    SecretResolveError::ManagedPolicyDenied { error, .. }
                    | SecretResolveError::Other(error) => anyhow::anyhow!("{}", error.message),
                })?;
        if !available_secret_refs.is_empty()
            && !remote_secret_policy.allows_adapter_kind(request.adapter_kind.as_str())
        {
            bail!("remote secret delivery denied before deferred dispatch");
        }
        let mut invocation = PreflightInvocation {
            request: request.clone(),
            available_secret_refs,
            remote_secret_policy,
        };
        // Full schema validation must precede even an APP readiness probe.
        let checks = self
            .services
            .preflight
            .validate_deferred_input(&invocation, &[])?;
        if let Some(check) = checks.iter().find(|c| c.status == PreflightStatus::Failed) {
            bail!("{}: {}", check.kind, check.message);
        }
        let staging = if crate::dispatch::request_uses_workflow_asset_snapshot(&invocation.request)
        {
            let staging = self
                .services
                .run_store
                .create_invocation_staging(paths.home_dir, &invocation.request)?;
            let assets = crate::dispatch::workflow_source_assets_for_request(
                self.services.run_store.as_ref(),
                &invocation.request.run_id,
                &invocation.request,
            )?;
            staging.materialize_source_assets(&assets)?;
            staging.apply_to_request(&mut invocation.request);
            Some(staging)
        } else {
            None
        };
        let checks = self.services.preflight.preflight_steps(&[invocation])?;
        drop(staging);
        if let Some(check) = checks.iter().find(|c| c.status != PreflightStatus::Passed) {
            bail!("{}: {}", check.kind, check.message);
        }
        Ok(())
    }

    fn authorize_loaded_invoke_request(
        &self,
        paths: RuntimePaths<'_>,
        snapshot: &KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step: &transist_protocol::RecipeStep,
        step_state: &mut KernelStepState,
        prepared: crate::dispatch::PreparedExecution,
    ) -> Result<LoadedInvokeAuthorization> {
        let policy_workflow_ref = self.policy_workflow_ref(persisted_record)?;
        let descriptor = super::build_step_policy_descriptor(
            &policy_workflow_ref,
            &prepared.request,
            &prepared.secret_refs,
        )?;
        let decision = self.services.policy.validate_step_execution(
            &snapshot.run_id,
            &step.id,
            &descriptor,
        )?;
        let mut request = prepared.request;
        let execution_kind = request
            .policy
            .get("execution_kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("execution policy is missing execution_kind"))?
            .to_string();
        let runtime_auth_payload = match &decision.payload {
            KernelValue::Object(map) => map.get("runtime_auth_context").cloned(),
            _ => None,
        };
        let workspace_declared = request
            .policy
            .get("workspace_declared")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut policy = serde_json::json!({
            "execution_kind": execution_kind,
            "adapter": request.adapter_kind,
            "workspace_declared": workspace_declared,
            "remote_secret_delivery": decision.remote_secret_policy.as_kernel_value(),
        });
        if let Some(runtime_auth_payload) = runtime_auth_payload {
            policy["runtime_auth_context"] = serde_json::Value::from(runtime_auth_payload);
        }
        if let KernelValue::Object(payload) = &decision.payload {
            for key in [
                "policy_id",
                "policy_profile",
                "requested_capabilities",
                "granted_capabilities",
                "denial_reason",
            ] {
                if let Some(value) = payload.get(key) {
                    policy[key] = serde_json::Value::from(value.clone());
                }
            }
        }
        if let Some(filesystem) = request.policy.get("filesystem").cloned()
            && !filesystem.is_null()
        {
            policy["filesystem"] = filesystem;
        }
        if let Some(network) = request.policy.get("network").cloned()
            && !network.is_null()
        {
            policy["network"] = network;
        }
        request.policy = policy;
        step_state.last_request = Some(request.clone());
        persist_step_state(
            self.services.run_store.as_ref(),
            &snapshot.run_id,
            step_state,
        )?;
        if !decision.allowed {
            let plugin_id = metadata_string(&request.metadata, "plugin_id")?;
            let mut event_payload = serde_json::json!({
                "reason_code": decision.reason_code.as_deref().unwrap_or("policy_denied"),
                "workflow_ref": snapshot.workflow_id.clone(),
                "adapter_kind": request.adapter_kind.to_string(),
                "plugin_id": plugin_id,
                "secret_ref": serde_json::Value::Null,
            });
            add_optional_policy_id(&mut event_payload, &request.policy);
            let _ = self.publish_event(
                "policy.denied",
                &snapshot.run_id,
                Some(&step.id),
                event_payload.into(),
            );
            let _ = record_step_dispatch_denied_provenance(
                self.services.run_store.as_ref(),
                paths.home_dir,
                persisted_record,
                &step.id,
                decision.reason_code.as_deref().unwrap_or("policy_denied"),
                "policy_denied",
                self.services.clock.now_unix_ms(),
            );
            let result = if decision.reason_code.as_deref() == Some("capability_not_granted") {
                policy_denied_result_with_code(
                    &request,
                    decision.message,
                    CanonicalErrorCode::CapabilityNotGranted,
                )
            } else {
                policy_denied_result(&request, decision.message)
            };
            return Ok(LoadedInvokeAuthorization::Denied(result));
        }
        Ok(LoadedInvokeAuthorization::Allowed(AuthorizedLoadedInvoke {
            request,
            secret_refs: prepared.secret_refs,
            remote_secret_policy: decision.remote_secret_policy,
        }))
    }

    fn resolve_loaded_invoke_secrets(
        &self,
        paths: RuntimePaths<'_>,
        snapshot: &KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step: &transist_protocol::RecipeStep,
        authorized: &AuthorizedLoadedInvoke,
    ) -> Result<LoadedInvokeSecretResolution> {
        match self.resolve_secret_refs(&authorized.secret_refs) {
            Ok(values) => {
                if !values.is_empty()
                    && !authorized
                        .remote_secret_policy
                        .allows_adapter_kind(authorized.request.adapter_kind.as_str())
                {
                    let reason_code = "remote_secret_policy";
                    let message = format!(
                        "remote secret delivery is denied by policy for adapter '{}'",
                        authorized.request.adapter_kind
                    );
                    self.record_loaded_secret_denial(
                        paths,
                        snapshot,
                        persisted_record,
                        step,
                        authorized,
                        reason_code,
                        serde_json::Value::Null,
                    )?;
                    return Ok(LoadedInvokeSecretResolution::Denied(Box::new(
                        policy_denied_result(&authorized.request, Some(message)),
                    )));
                }
                Ok(LoadedInvokeSecretResolution::Ready(values))
            }
            Err(SecretResolveError::ManagedPolicyDenied { secret_name, error }) => {
                self.record_loaded_secret_denial(
                    paths,
                    snapshot,
                    persisted_record,
                    step,
                    authorized,
                    MANAGED_SECRET_DENIAL_CODE,
                    serde_json::Value::String(secret_name),
                )?;
                Ok(LoadedInvokeSecretResolution::Denied(Box::new(
                    secret_resolution_failed_result(&authorized.request, error),
                )))
            }
            Err(SecretResolveError::Other(error)) => Ok(LoadedInvokeSecretResolution::Denied(
                Box::new(secret_resolution_failed_result(&authorized.request, error)),
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_loaded_secret_denial(
        &self,
        paths: RuntimePaths<'_>,
        snapshot: &KernelRunSnapshot,
        persisted_record: &PersistedRunRecord,
        step: &transist_protocol::RecipeStep,
        authorized: &AuthorizedLoadedInvoke,
        reason_code: &str,
        secret_ref: serde_json::Value,
    ) -> Result<()> {
        let plugin_id = metadata_string(&authorized.request.metadata, "plugin_id")?;
        let mut event_payload = serde_json::json!({
            "reason_code": reason_code,
            "workflow_ref": snapshot.workflow_id.clone(),
            "adapter_kind": authorized.request.adapter_kind.to_string(),
            "plugin_id": plugin_id,
            "secret_ref": secret_ref,
        });
        add_optional_policy_id(&mut event_payload, &authorized.request.policy);
        self.publish_event(
            "policy.denied",
            &snapshot.run_id,
            Some(&step.id),
            event_payload.into(),
        )?;
        record_step_dispatch_denied_provenance(
            self.services.run_store.as_ref(),
            paths.home_dir,
            persisted_record,
            &step.id,
            reason_code,
            "policy_denied",
            self.services.clock.now_unix_ms(),
        )?;
        Ok(())
    }

    fn stage_and_execute_loaded_invoke(
        &self,
        authorized: &AuthorizedLoadedInvoke,
        secret_values: BTreeMap<String, String>,
        paths: RuntimePaths<'_>,
    ) -> LoadedInvokeAdapterResult {
        let staging = match self
            .services
            .run_store
            .create_invocation_staging(paths.home_dir, &authorized.request)
        {
            Ok(staging) => staging,
            Err(error) => {
                return LoadedInvokeAdapterResult::Failed(Box::new(execution_failed_result(
                    &authorized.request,
                    error.to_string(),
                )));
            }
        };
        let mut adapter_request = authorized.request.clone();
        let mut redaction_values = secret_values.values().cloned().collect::<Vec<_>>();
        redaction_values.extend(staging.transient_redaction_values());
        let redactor = Redactor::from_secrets(redaction_values.iter().map(String::as_str));
        if let Err(error) =
            self.stage_request_documents(staging.as_ref(), &mut adapter_request, paths.home_dir)
        {
            return LoadedInvokeAdapterResult::Failed(Box::new(execution_failed_result(
                &authorized.request,
                redactor.redact_str(&error.to_string()),
            )));
        }
        if crate::dispatch::request_uses_workflow_asset_snapshot(&authorized.request) {
            let assets = match crate::dispatch::workflow_source_assets_for_request(
                self.services.run_store.as_ref(),
                &authorized.request.run_id,
                &authorized.request,
            ) {
                Ok(assets) => assets,
                Err(error) => {
                    return LoadedInvokeAdapterResult::Failed(Box::new(execution_failed_result(
                        &authorized.request,
                        error.to_string(),
                    )));
                }
            };
            if let Err(error) = staging.materialize_source_assets(&assets) {
                return LoadedInvokeAdapterResult::Failed(Box::new(execution_failed_result(
                    &authorized.request,
                    error.to_string(),
                )));
            }
        }
        staging.apply_to_request(&mut adapter_request);
        if let Err(error) = self.validate_staged_documents_before_dispatch(
            paths,
            &adapter_request,
            &authorized.secret_refs,
            authorized.remote_secret_policy,
        ) {
            let mut result = execution_failed_result(&authorized.request, error.to_string());
            result.error = Some(ProtocolError::new(
                CanonicalErrorCode::ValidationError,
                error.to_string(),
            ));
            redactor.redact_execution_result(&mut result);
            return LoadedInvokeAdapterResult::Failed(Box::new(result));
        }
        let invocation = ExecutionInvocation {
            request: adapter_request.clone(),
            secret_values,
            remote_secret_policy: authorized.remote_secret_policy,
        };
        match self.services.execution.execute_step(&invocation) {
            Ok(result) => LoadedInvokeAdapterResult::Dispatched(Box::new(LoadedInvokeDispatched {
                request: adapter_request,
                result,
                staging,
                redactor,
            })),
            Err(error) => {
                let mut result = execution_failed_result(&authorized.request, error.to_string());
                redactor.redact_execution_result(&mut result);
                LoadedInvokeAdapterResult::Failed(Box::new(result))
            }
        }
    }

    fn validate_loaded_invoke_result(
        &self,
        input: LoadedInvokeResultValidationInput<'_>,
    ) -> LoadedInvokeResultValidationResult {
        let sandbox_failure = input
            .result
            .output
            .get("__sandbox_failed__")
            .filter(|value| value.is_object())
            .cloned();
        let mut metadata = LoadedResultValidationMetadata {
            sandbox_failure,
            ..Default::default()
        };
        if let Err(denial) = self.guard_result_files_against_policy(
            input.request,
            &input.result,
            input.workspace_root,
        ) {
            let result = policy_denied_result(input.request, Some(denial.to_string()));
            metadata.filesystem_denial = Some(denial);
            return LoadedInvokeResultValidationResult::Denied(ValidatedLoadedInvokeResult {
                request: input.request.clone(),
                result,
                metadata,
            });
        }
        LoadedInvokeResultValidationResult::Accepted(ValidatedLoadedInvokeResult {
            request: input.request.clone(),
            result: input.result,
            metadata,
        })
    }

    fn promote_loaded_invoke_result(
        &self,
        input: LoadedInvokeResultPromotionInput,
    ) -> LoadedInvokeResultPromotionResult {
        let LoadedInvokeResultPromotionInput {
            validated,
            staging,
            redactor,
        } = input;
        let ValidatedLoadedInvokeResult {
            request,
            mut result,
            metadata,
        } = validated;
        let outcome = match staging.promote(&mut result, &redactor).and_then(|()| {
            self.capture_staged_documents(&request, &result, staging.as_ref(), &redactor)
        }) {
            Ok(()) => true,
            Err(error) => {
                if result.status == "success" {
                    result = failed_step_result(
                        &request.run_id,
                        &request.step.id,
                        request.step.execution,
                        error.to_string(),
                    );
                } else {
                    result.artifacts.clear();
                    result.evidence.clear();
                }
                false
            }
        };
        if let Err(error) = staging.normalize_result(&mut result) {
            result = execution_failed_result(&request, error.to_string());
        }
        redactor.redact_execution_result(&mut result);
        let promoted = PromotedLoadedInvokeResult {
            request,
            result,
            redactor,
            metadata,
        };
        if outcome {
            LoadedInvokeResultPromotionResult::Promoted(promoted)
        } else {
            LoadedInvokeResultPromotionResult::Failed(promoted)
        }
    }

    fn normalize_loaded_invoke_result(
        &self,
        input: LoadedInvokeResultNormalizationInput,
    ) -> Result<LoadedInvokeResultNormalizationResult> {
        let LoadedInvokeResultNormalizationInput {
            request,
            mut result,
            redactor,
            mut metadata,
            trusted_execution_mode,
        } = input;
        let outcome =
            match normalize_result_file_refs(&mut result, trusted_execution_mode, |path| {
                self.services.run_store.read_result_file(path)
            }) {
                Ok(()) => true,
                Err(error) => {
                    if trusted_execution_mode {
                        metadata.trusted_result_file_mismatch =
                            Some(metadata_string(&request.metadata, "plugin_id")?);
                    }
                    result = failed_step_result(
                        &request.run_id,
                        &request.step.id,
                        request.step.execution,
                        error.to_string(),
                    );
                    false
                }
            };
        redactor.redact_execution_result(&mut result);
        let normalized = NormalizedLoadedInvokeResult { result, metadata };
        Ok(if outcome {
            LoadedInvokeResultNormalizationResult::Normalized(normalized)
        } else {
            LoadedInvokeResultNormalizationResult::Failed(normalized)
        })
    }

    fn failed_loaded_invoke_request(
        &self,
        snapshot: &KernelRunSnapshot,
        step: &transist_protocol::RecipeStep,
        step_state: &KernelStepState,
        error: anyhow::Error,
    ) -> (ExecutionResult, DispatchOutcome) {
        if let Some(denial) = error.downcast_ref::<crate::filesystem_policy::FilesystemDenial>() {
            let _ = self.publish_filesystem_policy_denial(&snapshot.run_id, denial);
            (
                filesystem_policy_denied_result(
                    &snapshot.run_id,
                    &step.id,
                    step_state.executions,
                    denial,
                ),
                DispatchOutcome::NotDispatched,
            )
        } else {
            (
                failed_step_result(
                    &snapshot.run_id,
                    &step.id,
                    step_state.executions,
                    error.to_string(),
                ),
                DispatchOutcome::NotDispatched,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_loaded_step_result(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        graph: &ExecutionGraph,
        persisted_record: &mut PersistedRunRecord,
        snapshot: &mut KernelRunSnapshot,
        last_stable_step_id: &mut Option<String>,
        step_id: String,
        step_result: StepExecutionResult,
    ) -> Result<StepExecutionControl> {
        let Some(step) = recipe.steps.iter().find(|step| step.id == step_id) else {
            return Ok(StepExecutionControl::Continue);
        };
        let StepExecutionResult {
            result,
            state_mutations,
            validation,
        } = step_result;
        let plan = self.plan_loaded_terminal_transition(LoadedTerminalTransitionPlanningInput {
            graph,
            persisted_record,
            snapshot,
            step,
            step_id: step_id.clone(),
            result,
            state_mutations,
        })?;
        let committed = self.persist_loaded_execution(LoadedExecutionPersistenceInput {
            execution_record: plan.execution_record.clone(),
        })?;
        let LoadedExecutionPersistenceResult::Committed(execution_record) = committed;
        let mut plan = plan;
        plan.execution_record = execution_record;
        match self.transition_loaded_terminal_state(LoadedTerminalTransitionInput {
            paths,
            persisted_record,
            snapshot,
            last_stable_step_id,
            step,
            step_id,
            plan,
            validation,
        })? {
            LoadedTerminalTransitionResult::Continue => Ok(StepExecutionControl::Continue),
            LoadedTerminalTransitionResult::Break => Ok(StepExecutionControl::Break),
        }
    }

    fn plan_loaded_terminal_transition(
        &self,
        input: LoadedTerminalTransitionPlanningInput<'_>,
    ) -> Result<LoadedTerminalTransitionPlan> {
        let mut projected_snapshot = input.snapshot.clone();
        let mut projected_record = input.persisted_record.clone();
        let Some(step_state) = projected_snapshot.steps.get_mut(&input.step_id) else {
            bail!(
                "step '{}' disappeared while planning completion",
                input.step_id
            );
        };
        let finished_at_ms = self.services.clock.now_unix_ms();
        step_state.last_result = Some(input.result.clone());
        step_state.finished_at_ms = Some(finished_at_ms);
        step_state.skip_reason = None;

        let succeeded = input.result.status == "success";
        let blocked = input.result.status == "blocked";
        let cancelled = input.result.status == "cancelled";
        if succeeded && !input.step.vars.is_empty() {
            apply_set_vars(
                input.step,
                &projected_record.workflow_inputs,
                &projected_snapshot,
                &mut projected_record.workflow_vars,
            );
        }
        let loop_state = if succeeded {
            projected_snapshot
                .steps
                .get(&input.step_id)
                .map(|step_state| {
                    self.evaluate_step_loop_state(
                        input.step,
                        input.graph,
                        &projected_snapshot,
                        &projected_record.workflow_inputs,
                        &projected_record.workflow_vars,
                        step_state,
                    )
                })
        } else {
            None
        };
        let reset_loop_body_steps = if matches!(loop_state, Some(GateState::LoopActive { .. })) {
            self.reset_loop_body_steps(input.step, &mut projected_snapshot)
        } else {
            Vec::new()
        };
        let activated_loop_targets = if succeeded {
            self.activate_loop_targets(input.graph, &input.step_id, &mut projected_snapshot)
        } else {
            Vec::new()
        };

        let Some(step_state) = projected_snapshot.steps.get_mut(&input.step_id) else {
            bail!("step '{}' disappeared while completing", input.step_id);
        };
        if loop_state.is_some() {
            step_state.status = KernelStepStatus::Succeeded;
            step_state.retry_wait_until_ms = None;
        } else if blocked {
            step_state.status = KernelStepStatus::Blocked;
            step_state.retry_wait_until_ms = None;
            projected_snapshot.status = KernelRunStatus::Blocked;
        } else if cancelled {
            step_state.status = KernelStepStatus::Canceled;
            step_state.retry_wait_until_ms = None;
        } else if let Some(wait_until) = self.next_retry_wait(input.step, step_state) {
            step_state.status = KernelStepStatus::RetryWait;
            step_state.retry_wait_until_ms = Some(wait_until);
        } else {
            step_state.status = KernelStepStatus::Failed;
            step_state.retry_wait_until_ms = None;
        }
        let execution_record = StepExecutionRecord {
            document_revisions: Vec::new(),
            run_id: projected_snapshot.run_id.clone(),
            step_id: input.step_id,
            execution: step_state.executions,
            status: input.result.status.clone(),
            started_at_ms: step_state.started_at_ms,
            finished_at_ms: step_state.finished_at_ms,
            retry_wait_until_ms: step_state.retry_wait_until_ms,
            request: step_state.last_request.clone(),
            result: step_state.last_result.clone(),
            metrics_validation_errors: step_state.last_result.as_ref().and_then(|result| {
                (result.metrics_validation_errors > 0).then_some(result.metrics_validation_errors)
            }),
            state_mutations: input.state_mutations,
        };
        Ok(LoadedTerminalTransitionPlan {
            execution_record,
            result: input.result,
            status: step_state.status,
            retry_wait_until_ms: step_state.retry_wait_until_ms,
            reset_loop_body_steps,
            activated_loop_targets,
            finished_at_ms,
            workflow_vars: projected_record.workflow_vars,
            succeeded,
            blocked,
            cancelled,
        })
    }

    fn persist_loaded_execution(
        &self,
        input: LoadedExecutionPersistenceInput,
    ) -> Result<LoadedExecutionPersistenceResult> {
        // The immutable execution record is the completion commit point. No
        // state projection, event, or telemetry work belongs before this write.
        persist_step_execution(self.services.run_store.as_ref(), &input.execution_record)?;
        crate::infra::execution_faults::check_fault(
            crate::infra::execution_faults::FAULT_STEP_EXECUTION_RECORD,
        )?;
        Ok(LoadedExecutionPersistenceResult::Committed(
            input.execution_record,
        ))
    }

    fn transition_loaded_terminal_state(
        &self,
        input: LoadedTerminalTransitionInput<'_>,
    ) -> Result<LoadedTerminalTransitionResult> {
        let LoadedTerminalTransitionInput {
            paths,
            persisted_record,
            snapshot,
            last_stable_step_id,
            step,
            step_id,
            plan,
            validation,
        } = input;
        persisted_record.workflow_vars = plan.workflow_vars;
        let Some(step_state) = snapshot.steps.get_mut(&step_id) else {
            return Ok(LoadedTerminalTransitionResult::Continue);
        };
        step_state.last_result = Some(plan.result.clone());
        step_state.finished_at_ms = Some(plan.finished_at_ms);
        step_state.skip_reason = None;
        step_state.status = plan.status;
        step_state.retry_wait_until_ms = plan.retry_wait_until_ms;
        if plan.blocked {
            snapshot.status = KernelRunStatus::Blocked;
        }
        for reset_step_id in plan.reset_loop_body_steps {
            if let Some(reset_step) = snapshot.steps.get_mut(&reset_step_id) {
                reset_step_for_rerun(reset_step);
                persist_step_state(
                    self.services.run_store.as_ref(),
                    &snapshot.run_id,
                    reset_step,
                )?;
            }
        }
        for activated_step_id in plan.activated_loop_targets {
            if let Some(activated_step) = snapshot.steps.get_mut(&activated_step_id) {
                reset_step_for_rerun(activated_step);
                persist_step_state(
                    self.services.run_store.as_ref(),
                    &snapshot.run_id,
                    activated_step,
                )?;
            }
        }
        // The immutable record is the completion commit point. Make every
        // derived effect durable before exposing the state-projection fault
        // window, so recovery only needs to rebuild truly in-flight records.
        persisted_record.steps = snapshot.steps.clone();
        persist_run_record(self.services.run_store.as_ref(), persisted_record)?;
        let Some(step_state) = snapshot.steps.get(&step_id) else {
            return Ok(LoadedTerminalTransitionResult::Continue);
        };
        persist_step_state(
            self.services.run_store.as_ref(),
            &snapshot.run_id,
            step_state,
        )?;
        crate::infra::execution_faults::check_fault(
            crate::infra::execution_faults::FAULT_STEP_STATE_PROJECTION,
        )?;
        if let Some(failure_record) = self.failure_record_for_step(
            &step.execution,
            &plan.execution_record,
            last_stable_step_id.clone(),
        ) {
            self.services
                .run_store
                .store_failure_record(&failure_record)?;
            crate::infra::execution_faults::check_fault(
                crate::infra::execution_faults::FAULT_STEP_FAILURE_APPEND,
            )?;
        } else if plan.succeeded {
            *last_stable_step_id = Some(step_id.clone());
        }
        if let Some(payload) = validation.sandbox_failure {
            let _ = self.publish_event(
                "filesystem_policy.sandbox_failed",
                &snapshot.run_id,
                Some(&step_id),
                payload.into(),
            );
        }
        if let Some(denial) = validation.filesystem_denial {
            let _ = self.publish_filesystem_policy_denial(&snapshot.run_id, &denial);
            let _ = record_step_dispatch_denied_provenance(
                self.services.run_store.as_ref(),
                paths.home_dir,
                persisted_record,
                &step_id,
                "filesystem_policy_denied",
                "filesystem_policy",
                self.services.clock.now_unix_ms(),
            );
        }
        if let Some(plugin_id) = validation.trusted_result_file_mismatch {
            let _ = self.publish_event(
                "trust.denied",
                &snapshot.run_id,
                Some(&step_id),
                serde_json::json!({
                    "reason_code": "result_file_sha256_mismatch",
                    "failed_verification_type": "result_file_sha256",
                    "workflow_ref": snapshot.workflow_id.clone(),
                    "plugin_id": plugin_id,
                })
                .into(),
            );
            let _ = record_step_dispatch_denied_provenance(
                self.services.run_store.as_ref(),
                paths.home_dir,
                persisted_record,
                &step_id,
                "result_file_sha256_mismatch",
                "result_file_sha256",
                self.services.clock.now_unix_ms(),
            );
        }
        self.publish_event(
            if plan.succeeded {
                "step_finished"
            } else if plan.blocked {
                "step_blocked"
            } else if plan.cancelled {
                "step_cancelled"
            } else {
                "step_failed"
            },
            &snapshot.run_id,
            Some(&step_id),
            serde_json::json!({
                "execution": plan.execution_record.execution,
                "status": plan.result.status,
                "exit_code": plan.result.exit_code
            })
            .into(),
        )?;
        crate::infra::execution_faults::check_fault(
            crate::infra::execution_faults::FAULT_STEP_COMPLETION_EVENT,
        )?;
        if plan.blocked {
            self.services
                .telemetry
                .record(&TelemetryEvent::GateWaitStarted {
                    run_id: snapshot.run_id.clone(),
                    step_id: step_id.clone(),
                    execution: plan.execution_record.execution,
                });
        }
        let (step_adapter_kind, step_action) = self.step_descriptor_labels(&step.execution);
        self.services
            .telemetry
            .record(&TelemetryEvent::StepFinished {
                run_id: snapshot.run_id.clone(),
                step_id: step_id.clone(),
                execution: plan.execution_record.execution,
                status: plan.result.status.clone(),
                duration_ms: plan.result.timing.duration_ms,
                descriptor: step.execution.clone(),
                adapter_kind: step_adapter_kind,
                action: step_action,
                provider: plan
                    .result
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.provider.clone()),
                request_model: plan
                    .result
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.model.clone()),
                response_model: plan
                    .result
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.model.clone()),
                input_tokens: plan
                    .result
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.input_tokens),
                output_tokens: plan
                    .result
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.output_tokens),
                total_tokens: plan
                    .result
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.total_tokens),
                error_type: plan
                    .result
                    .error
                    .as_ref()
                    .map(|error| format!("{:?}", error.class))
                    .or_else(|| {
                        plan.result
                            .error
                            .as_ref()
                            .map(|error| format!("{:?}", error.code))
                    }),
            });
        if let StepExecutionDescriptor::Control { primitive } = &step.execution
            && primitive == crate::ControlStepKind::GateManual.as_wire_str()
            && !plan.blocked
        {
            let decision = plan
                .result
                .output
                .get("decision")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("approved")
                .to_string();
            self.services
                .telemetry
                .record(&TelemetryEvent::GateWaitFinished {
                    run_id: snapshot.run_id.clone(),
                    step_id,
                    execution: plan.execution_record.execution,
                    decision,
                });
        }
        Ok(if plan.blocked {
            LoadedTerminalTransitionResult::Break
        } else {
            LoadedTerminalTransitionResult::Continue
        })
    }

    fn finalize_loaded_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        lease: &LeaseHandle,
        persisted_record: &mut PersistedRunRecord,
        snapshot: &KernelRunSnapshot,
    ) -> Result<()> {
        persisted_record.status = snapshot.status;
        persisted_record.steps = snapshot.steps.clone();
        // Publish the primary terminal outcome before entering cleanup. The
        // watcher must distinguish canceled cleanup from primary execution even
        // while the original durable cancellation request is still present.
        if snapshot.status == KernelRunStatus::Failed {
            self.initialize_failed_cleanup_state(recipe, persisted_record)?;
        }
        persist_run_record(self.services.run_store.as_ref(), persisted_record)?;
        match snapshot.status {
            KernelRunStatus::Success => {
                self.maybe_execute_automatic_cleanup(
                    paths,
                    recipe,
                    persisted_record,
                    lease,
                    CleanupTrigger::Success,
                )?;
            }
            KernelRunStatus::Canceled => {
                self.maybe_execute_automatic_cleanup(
                    paths,
                    recipe,
                    persisted_record,
                    lease,
                    CleanupTrigger::Canceled,
                )?;
            }
            KernelRunStatus::Failed => {
                self.maybe_execute_automatic_cleanup(
                    paths,
                    recipe,
                    persisted_record,
                    lease,
                    CleanupTrigger::Failed,
                )?;
            }
            _ => {}
        }
        persisted_record.updated_at_ms = self.services.clock.now_unix_ms();
        persist_run_record(self.services.run_store.as_ref(), persisted_record)?;
        finalize_run_provenance_record(
            self.services.run_store.as_ref(),
            paths.home_dir,
            persisted_record,
            persisted_record.updated_at_ms,
        )?;
        self.publish_event(
            if snapshot.status == KernelRunStatus::Success {
                "run_finished"
            } else if snapshot.status == KernelRunStatus::Blocked {
                "run_blocked"
            } else {
                "run_stopped"
            },
            &snapshot.run_id,
            None,
            serde_json::json!({ "status": snapshot.status.as_str() }).into(),
        )?;
        self.services
            .telemetry
            .record(&TelemetryEvent::RunSessionFinished {
                run_id: snapshot.run_id.clone(),
                terminal_status: snapshot.status.as_str().to_string(),
            });
        if snapshot.run_id == lease.root_run_id
            && matches!(
                snapshot.status,
                KernelRunStatus::Success | KernelRunStatus::Failed | KernelRunStatus::Canceled
            )
        {
            self.services
                .control
                .clear_cancel_request(&lease.root_run_id)?;
        }
        Ok(())
    }

    fn prepare_loaded_run_recovery(
        &self,
        home_dir: &Path,
        recipe: &Recipe,
        record: &PersistedRunRecord,
        apply_recovery: bool,
    ) -> Result<RecoveredLoadedRun> {
        self.services.run_store.cleanup_stale_staging(home_dir)?;
        let step_ids: Vec<String> = recipe.steps.iter().map(|step| step.id.clone()).collect();
        let graph = build_execution_graph(recipe);
        let mut persisted_record = record.clone();
        let mut snapshot = record.snapshot();
        self.ensure_snapshot_steps(&mut snapshot, &step_ids);
        if apply_recovery {
            self.recover_execution_records(
                &record.run_id,
                recipe,
                &graph,
                &mut persisted_record,
                &mut snapshot,
            )?;
            recover_snapshot(&mut snapshot, self.services.clock.now_unix_ms());
        }
        self.ensure_entry_runnable(&graph, &mut snapshot);
        Ok(RecoveredLoadedRun {
            state: LoadedRunState {
                last_stable_step_id: last_stable_step_id_from_snapshot(recipe, &snapshot),
                graph,
                snapshot,
                persisted_record,
            },
        })
    }

    fn prepare_loaded_run_preflight(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        persisted_record: &PersistedRunRecord,
        run_id: &str,
        preflight_override: Option<PersistedPreflightRecord>,
        apply_recovery: bool,
    ) -> Result<PersistedPreflightRecord> {
        if let Some(mut preflight) = preflight_override {
            // Re-check this declaration even when reusing a persisted preflight.
            let custom_checks = super::preflight::filesystem_custom_mode_checks(
                paths,
                recipe,
                self.services.home_settings.as_ref(),
            );
            if !custom_checks.is_empty() {
                let mut checks = preflight.checks;
                checks.extend(custom_checks);
                preflight = PersistedPreflightRecord::from_checks(
                    self.services.clock.now_unix_ms(),
                    checks,
                );
            }
            Ok(preflight)
        } else {
            self.publish_event("run_preflight_started", run_id, None, KernelValue::Null)?;
            let preflight =
                self.build_preflight_record(paths, recipe, persisted_record, apply_recovery)?;
            self.publish_preflight_outcome(run_id, &preflight)?;
            Ok(preflight)
        }
    }

    /// Store-aware recovery for execution-completion crash points.
    ///
    /// Runs before [`recover_snapshot`] on every re-entry whose `apply_recovery`
    /// flag is set. For each step with a consumed ordinal, it inspects the
    /// immutable execution record for that ordinal. Only records whose step is
    /// still transient (`Dispatching`/`Running`) can rebuild snapshot state;
    /// all completed records can still repair append-only effects:
    ///
    /// - If a completed record exists for an in-flight ordinal, its complete
    ///   semantic projection is rebuilt from the record. A committed result is
    ///   never dispatched again: the step moves to its recorded terminal status
    ///   with the recorded result, timestamps, and retry deadline rather than
    ///   being re-queued.
    /// - If an `interrupted` placeholder record already exists (from a prior
    ///   interrupted recovery), the step is left for [`recover_snapshot`] to
    ///   re-queue, so the replacement dispatch begins only at `N+1`.
    /// - If no immutable record exists for the consumed ordinal, an immutable
    ///   `interrupted` record is created for that ordinal first (so every
    ///   consumed execution ordinal has exactly one immutable record), then the
    ///   step is left for [`recover_snapshot`] to re-queue at `N+1`.
    ///
    /// This makes the immutable execution record the completion commit point:
    /// a crash between result promotion, the execution record, the state
    /// projection, the failure append, and the completion event is reconciled
    /// from the already-committed record on restart.
    fn recover_execution_records(
        &self,
        run_id: &str,
        recipe: &Recipe,
        graph: &ExecutionGraph,
        persisted_record: &mut PersistedRunRecord,
        snapshot: &mut KernelRunSnapshot,
    ) -> Result<()> {
        let now_ms = self.services.clock.now_unix_ms();
        let step_ids: Vec<String> = snapshot.steps.keys().cloned().collect();
        if let Some(documents) = self.services.run_store.document_store() {
            documents.reconcile_document_heads(run_id)?;
        } else if !recipe.documents.is_empty() {
            bail!("document storage unavailable");
        }
        for step_id in step_ids {
            let Some(step_state) = snapshot.steps.get(&step_id) else {
                continue;
            };
            let ordinal = step_state.executions;
            let in_flight = matches!(
                step_state.status,
                KernelStepStatus::Dispatching | KernelStepStatus::Running
            );

            // Guard the transient ordinal-zero case: a step showing as
            // in-flight with a zero execution counter never consumed an
            // execution (dispatch increments the counter before running), so
            // there is nothing to make durable and no spurious `execution_000`
            // record may be created. Recovery leaves such a step for
            // `recover_snapshot` to re-queue.
            if ordinal == 0 {
                continue;
            }

            let existing = self
                .services
                .run_store
                .load_step_execution(run_id, &step_id, ordinal)?;
            if let Some(record) = existing.as_ref() {
                validate_record_identity(record, run_id, &step_id, ordinal)?;
            }

            match existing {
                Some(record) if record_is_completed(&record) => {
                    // Terminal records are authoritative only while their
                    // snapshot step is still transient. Re-projecting a
                    // blocked terminal record would erase an approved manual
                    // gate re-entry transition.
                    if in_flight {
                        let Some(step_state) = snapshot.steps.get_mut(&step_id) else {
                            continue;
                        };
                        self.reconcile_terminal_state_from_record(
                            run_id, step_state, &record, now_ms,
                        );
                        let Some(step) = recipe.steps.iter().find(|step| step.id == step_id) else {
                            continue;
                        };
                        if record
                            .result
                            .as_ref()
                            .is_some_and(|result| result.status == "success")
                        {
                            apply_set_vars(
                                step,
                                &persisted_record.workflow_inputs,
                                snapshot,
                                &mut persisted_record.workflow_vars,
                            );
                            let loop_state = snapshot.steps.get(&step_id).map(|step_state| {
                                self.evaluate_step_loop_state(
                                    step,
                                    graph,
                                    snapshot,
                                    &persisted_record.workflow_inputs,
                                    &persisted_record.workflow_vars,
                                    step_state,
                                )
                            });
                            if matches!(loop_state, Some(GateState::LoopActive { .. })) {
                                for reset_step_id in self.reset_loop_body_steps(step, snapshot) {
                                    if let Some(reset_step) = snapshot.steps.get(&reset_step_id) {
                                        persist_step_state(
                                            self.services.run_store.as_ref(),
                                            run_id,
                                            reset_step,
                                        )?;
                                    }
                                }
                            }
                            for activated_step_id in
                                self.activate_loop_targets(graph, &step_id, snapshot)
                            {
                                if let Some(activated_step) = snapshot.steps.get(&activated_step_id)
                                {
                                    persist_step_state(
                                        self.services.run_store.as_ref(),
                                        run_id,
                                        activated_step,
                                    )?;
                                }
                            }
                        }
                        persisted_record.steps = snapshot.steps.clone();
                        persist_run_record(self.services.run_store.as_ref(), persisted_record)?;
                    }
                    // Append-only effects are independently recoverable for
                    // both transient and already-terminal records.
                    self.backfill_completion_event(&record, false)?;
                    // Backfill the classified failure record if the run was torn
                    // after the immutable record but before `store_failure_record`.
                    if let Some(step) = recipe.steps.iter().find(|step| step.id == step_id) {
                        self.backfill_failure_record(&step.execution, &record)?;
                    }
                }
                Some(_) => {
                    // An `interrupted` placeholder already exists for this
                    // ordinal; the replacement will begin at N+1.
                }
                None if in_flight => {
                    // The consumed ordinal was in flight with no completion
                    // record. Create the immutable `interrupted` marker so the
                    // ordinal is never silently dropped, then let recovery
                    // re-queue the step so the replacement begins at N+1.
                    if let Some(step_state) = snapshot.steps.get(&step_id) {
                        self.persist_interrupted_execution_record(run_id, step_state, ordinal)?;
                    }
                }
                None => {}
            }
        }
        Ok(())
    }

    fn reconcile_terminal_state_from_record(
        &self,
        run_id: &str,
        step_state: &mut KernelStepState,
        record: &StepExecutionRecord,
        now_ms: u64,
    ) {
        apply_terminal_state_from_record(step_state, record, now_ms);

        // Reconciliation writes are durable best-effort: the run store may
        // have been torn. The immutable record remains the commit point, so
        // re-entry will reconcile again.
        let _ = persist_step_state(self.services.run_store.as_ref(), run_id, step_state);
    }

    pub(crate) fn persist_interrupted_execution_record(
        &self,
        run_id: &str,
        step_state: &KernelStepState,
        ordinal: u32,
    ) -> Result<()> {
        let record = StepExecutionRecord {
            document_revisions: Vec::new(),
            run_id: run_id.to_string(),
            step_id: step_state.step_id.clone(),
            execution: ordinal,
            status: STEP_EXECUTION_STATUS_INTERRUPTED.to_string(),
            started_at_ms: step_state.started_at_ms,
            finished_at_ms: None,
            retry_wait_until_ms: None,
            request: step_state.last_request.clone(),
            result: None,
            metrics_validation_errors: None,
            state_mutations: Vec::new(),
        };
        persist_step_execution(self.services.run_store.as_ref(), &record)
    }

    /// Maps a committed execution record to the completion event kind
    /// (`step_finished`/`step_blocked`/`step_cancelled`/`step_failed`, or the
    /// `cleanup_*` variants) and, if that event is not already durably
    /// published for `(run, step, execution, kind)`, backfills it. Recovery
    /// reaches here when a completion record exists but the run was torn
    /// before its completion event reached the events stream.
    pub(crate) fn backfill_completion_event(
        &self,
        record: &StepExecutionRecord,
        cleanup: bool,
    ) -> Result<()> {
        let Some(kind) = completion_event_kind(record, cleanup) else {
            return Ok(());
        };
        if self.completion_event_published(
            &record.run_id,
            &record.step_id,
            record.execution,
            &kind,
        )? {
            return Ok(());
        }
        let status = record
            .result
            .as_ref()
            .map(|result| result.status.as_str())
            .unwrap_or("");
        let exit_code = record.result.as_ref().and_then(|result| result.exit_code);
        self.publish_event(
            &kind,
            &record.run_id,
            Some(&record.step_id),
            serde_json::json!({
                "execution": record.execution,
                "status": status,
                "exit_code": exit_code,
            })
            .into(),
        )
    }

    /// Backfills the classified `failures.jsonl` record for a committed failed
    /// or cancelled execution when the run was torn after the immutable
    /// execution record but before `store_failure_record`. Deduplicated by
    /// `(step_id, execution_attempt)` so recovery never appends a second record
    /// for a failure that was already durably classified. A committed success
    /// or block produces no failure record.
    pub(crate) fn backfill_failure_record(
        &self,
        descriptor: &StepExecutionDescriptor,
        record: &StepExecutionRecord,
    ) -> Result<()> {
        let Some(failure) = self.failure_record_for_step(descriptor, record, None) else {
            return Ok(());
        };
        let existing = self.services.run_store.load_run_failures(&record.run_id)?;
        if existing
            .iter()
            .any(|f| f.step_id == record.step_id && f.execution_attempt == record.execution)
        {
            return Ok(());
        }
        self.services.run_store.store_failure_record(&failure)
    }

    fn completion_event_published(
        &self,
        run_id: &str,
        step_id: &str,
        execution: u32,
        kind: &str,
    ) -> Result<bool> {
        let events = self.services.run_store.load_run_events(run_id)?;
        Ok(events.iter().any(|event| {
            event.kind == kind
                && event.step_id.as_deref() == Some(step_id)
                && event_execution_matches(&event.payload, execution)
        }))
    }

    pub(crate) fn acquire_mutation_lease(
        &self,
        home_dir: &Path,
        run_id: &str,
    ) -> Result<LeaseHandle> {
        let root_run_id = self
            .services
            .run_store
            .resolve_root_run_id(home_dir, run_id)?;
        self.services
            .lease
            .acquire_root_run_tree_lease(&root_run_id)
    }

    pub(crate) fn release_mutation_lease(&self, lease: &LeaseHandle) -> Result<()> {
        self.services.lease.release_root_run_tree_lease(lease)
    }

    pub(crate) fn start_control_watcher(
        &self,
        home_dir: &Path,
        lease: &LeaseHandle,
    ) -> ControlWatcherGuard {
        let control = self.services.control.clone();
        let execution = self.services.execution.clone();
        let run_store = self.services.run_store.clone();
        let home_dir = home_dir.to_path_buf();
        let root_run_id = lease.root_run_id.clone();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let join_handle = std::thread::spawn(move || {
            let mut last_invoke_id: Option<String> = None;
            loop {
                match stop_rx.recv_timeout(std::time::Duration::from_millis(500)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                let has_cancel_request = control
                    .inspect_cancel_request(&root_run_id)
                    .ok()
                    .flatten()
                    .is_some();
                if !has_cancel_request {
                    continue;
                }
                let active_invoke_id = resolve_active_invoke_id_in_root_run_tree(
                    run_store.as_ref(),
                    &home_dir,
                    &root_run_id,
                )
                .ok()
                .flatten();
                let Some(invoke_id) = active_invoke_id else {
                    continue;
                };
                if last_invoke_id.as_deref() == Some(invoke_id.as_str()) {
                    continue;
                }
                // Only advance the dedup guard when the cancel was accepted.
                // If the adapter hasn't registered the invocation yet (race
                // between persist_step_state and active.register), accepted
                // will be false and we must retry on the next poll cycle.
                let accepted = execution
                    .cancel_step(&ExecutionCancelRequest {
                        invoke_id: invoke_id.clone(),
                    })
                    .map(|r| r.accepted)
                    .unwrap_or(false);
                if accepted {
                    last_invoke_id = Some(invoke_id);
                }
            }
        });
        ControlWatcherGuard {
            stop_tx: Some(stop_tx),
            join_handle: Some(join_handle),
        }
    }

    pub(crate) fn cancel_request_pending(&self, root_run_id: &str) -> Result<bool> {
        Ok(self
            .services
            .control
            .inspect_cancel_request(root_run_id)?
            .is_some())
    }

    /// Settles existing descendants deepest-first under the already-held root
    /// lease. Recovery may repair committed results/history, but this path never
    /// dispatches primary work, executes child cleanup, or clears root control.
    pub(crate) fn settle_canceled_descendants(
        &self,
        home_dir: &Path,
        root_run_id: &str,
    ) -> Result<()> {
        let mut descendants = BTreeMap::new();
        for run_root in self
            .services
            .run_store
            .list_run_tree_roots(home_dir, root_run_id)?
        {
            let Some(run_id) = self.services.run_store.run_id_from_run_root(&run_root) else {
                continue;
            };
            if run_id == root_run_id {
                continue;
            }
            if self
                .services
                .run_store
                .resolve_root_run_id(home_dir, &run_id)?
                != root_run_id
            {
                bail!(
                    "run '{}' in selected tree has inconsistent root lineage",
                    run_id
                );
            }
            let record = load_run_record(self.services.run_store.as_ref(), &run_id)?
                .ok_or_else(|| anyhow::anyhow!("descendant run '{}' not found", run_id))?;
            descendants.insert(run_id, record);
        }
        // Validate every ancestry chain before mutating anything. Physical
        // containment comes from the store; persisted parents order settlement.
        let mut ordered = Vec::new();
        for (run_id, record) in &descendants {
            let mut seen = BTreeSet::from([run_id.as_str()]);
            let mut ancestor = record;
            let mut depth = 1;
            loop {
                let parent = ancestor
                    .lineage
                    .as_ref()
                    .and_then(|lineage| lineage.parent_run_id.as_deref())
                    .ok_or_else(|| anyhow::anyhow!("descendant run '{}' has no parent", run_id))?;
                if parent == root_run_id {
                    break;
                }
                if !seen.insert(parent) {
                    bail!("descendant run '{}' has cyclic ancestry", run_id);
                }
                ancestor = descendants.get(parent).ok_or_else(|| {
                    anyhow::anyhow!(
                        "descendant run '{}' has missing parent '{}'",
                        run_id,
                        parent
                    )
                })?;
                depth += 1;
            }
            ordered.push((std::cmp::Reverse(depth), run_id.clone()));
        }
        ordered.sort();
        for (_, run_id) in ordered {
            let mut record = descendants.remove(&run_id).ok_or_else(|| {
                anyhow::anyhow!("descendant run '{}' lost before cancellation", run_id)
            })?;
            if record.status.is_terminal() {
                continue;
            }
            let recipe = self.load_persisted_recipe(&run_id, &record)?;
            let graph = build_execution_graph(&recipe);
            let mut snapshot = record.snapshot();
            self.recover_execution_records(&run_id, &recipe, &graph, &mut record, &mut snapshot)?;
            if snapshot
                .steps
                .values()
                .all(|step| step.status.is_terminal())
            {
                snapshot.status = self.terminal_status(&snapshot);
            } else {
                self.cancel_snapshot(&mut snapshot);
            }
            record.status = snapshot.status;
            record.steps = snapshot.steps;
            record.updated_at_ms = self.services.clock.now_unix_ms();
            persist_run_record(self.services.run_store.as_ref(), &record)?;
            finalize_run_provenance_record(
                self.services.run_store.as_ref(),
                home_dir,
                &record,
                record.updated_at_ms,
            )?;
            self.publish_event(
                if record.status == KernelRunStatus::Success {
                    "run_finished"
                } else {
                    "run_stopped"
                },
                &run_id,
                None,
                serde_json::json!({ "status": record.status.as_str() }).into(),
            )?;
        }
        Ok(())
    }

    fn finish_canceled_run(
        &self,
        paths: RuntimePaths<'_>,
        recipe: &Recipe,
        lease: &LeaseHandle,
        persisted_record: &mut PersistedRunRecord,
        snapshot: &mut KernelRunSnapshot,
    ) -> Result<KernelRunSummary> {
        let changed = self.cancel_snapshot(snapshot);
        for step_id in changed {
            if let Some(step) = snapshot.steps.get(&step_id) {
                persist_step_state(self.services.run_store.as_ref(), &snapshot.run_id, step)?;
            }
        }
        persisted_record.status = snapshot.status;
        persisted_record.steps = snapshot.steps.clone();
        persist_run_record(self.services.run_store.as_ref(), persisted_record)?;
        // A failure committed before cancellation remains a failure. Cleanup
        // selection follows that primary outcome, never the control request.
        if snapshot.status == KernelRunStatus::Failed {
            if snapshot.run_id == lease.root_run_id {
                self.settle_canceled_descendants(paths.home_dir, &lease.root_run_id)?;
            }
            self.finalize_loaded_run(paths, recipe, lease, persisted_record, snapshot)?;
            return Ok(run_summary_from_snapshot(snapshot));
        }
        self.maybe_execute_automatic_cleanup(
            paths,
            recipe,
            persisted_record,
            lease,
            CleanupTrigger::Canceled,
        )?;
        persisted_record.updated_at_ms = self.services.clock.now_unix_ms();
        persist_run_record(self.services.run_store.as_ref(), persisted_record)?;
        self.publish_event(
            "run_stopped",
            &snapshot.run_id,
            None,
            serde_json::json!({ "status": snapshot.status.as_str() }).into(),
        )?;
        self.services
            .telemetry
            .record(&TelemetryEvent::RunSessionFinished {
                run_id: snapshot.run_id.clone(),
                terminal_status: snapshot.status.as_str().to_string(),
            });
        // Clear the durable cancel-request marker now that it has been
        // honored. Only do this when finalizing the root run itself; child
        // subflow runs share the root's marker and must not erase it on
        // their own behalf. Errors are surfaced — leaving a stale marker
        // would re-cancel the next launch that reuses this root id.
        if snapshot.run_id == lease.root_run_id {
            self.services
                .control
                .clear_cancel_request(&lease.root_run_id)?;
        }
        Ok(run_summary_from_snapshot(snapshot))
    }

    fn cancel_snapshot(&self, snapshot: &mut KernelRunSnapshot) -> Vec<String> {
        let finished_at_ms = self.services.clock.now_unix_ms();
        let mut changed = Vec::new();
        for (step_id, step) in &mut snapshot.steps {
            if step.status.is_terminal() {
                continue;
            }
            step.status = KernelStepStatus::Canceled;
            step.retry_wait_until_ms = None;
            step.finished_at_ms.get_or_insert(finished_at_ms);
            changed.push(step_id.clone());
        }
        snapshot.status = if snapshot.status == KernelRunStatus::Failed
            || snapshot
                .steps
                .values()
                .any(|step| step.status == KernelStepStatus::Failed)
        {
            KernelRunStatus::Failed
        } else {
            KernelRunStatus::Canceled
        };
        changed
    }

    pub(crate) fn publish_event(
        &self,
        kind: &str,
        run_id: &str,
        step_id: Option<&str>,
        payload: KernelValue,
    ) -> Result<()> {
        self.services.events.publish(&RuntimeEvent {
            kind: kind.to_string(),
            run_id: run_id.to_string(),
            step_id: step_id.map(std::string::ToString::to_string),
            timestamp_unix_ms: self.services.clock.now_unix_ms(),
            payload,
        })
    }

    pub(crate) fn publish_filesystem_policy_denial(
        &self,
        run_id: &str,
        denial: &crate::filesystem_policy::FilesystemDenial,
    ) -> Result<()> {
        self.publish_event(
            "filesystem_policy.denied",
            run_id,
            Some(&denial.step_id),
            denial.to_event_payload().into(),
        )
    }

    pub(crate) fn publish_sandbox_failure_event(
        &self,
        run_id: &str,
        step_id: &str,
        result: &ExecutionResult,
    ) -> Result<()> {
        let Some(payload) = result
            .output
            .get("__sandbox_failed__")
            .filter(|value| value.is_object())
            .cloned()
        else {
            return Ok(());
        };
        self.publish_event(
            "filesystem_policy.sandbox_failed",
            run_id,
            Some(step_id),
            payload.into(),
        )
    }

    pub(crate) fn step_descriptor_labels(
        &self,
        descriptor: &StepExecutionDescriptor,
    ) -> (String, String) {
        match descriptor {
            StepExecutionDescriptor::Invoke {
                adapter, action, ..
            } => (adapter.to_string(), action.clone()),
            StepExecutionDescriptor::Control { primitive } => {
                ("control".to_string(), primitive.clone())
            }
            StepExecutionDescriptor::Subflow { .. } => ("subflow".to_string(), "run".to_string()),
        }
    }

    pub(crate) fn resolve_secret_refs(
        &self,
        secret_refs: &[String],
    ) -> std::result::Result<BTreeMap<String, String>, SecretResolveError> {
        let mut resolved_values = BTreeMap::new();
        for secret_name in secret_refs {
            let resolved = self
                .services
                .secrets
                .get_secret(secret_name)
                .map_err(|err| {
                    let err_str = err.to_string();
                    let protocol_error = ProtocolError::new(
                        CanonicalErrorCode::SecretResolutionError,
                        format!("failed resolving secret '{}': {}", secret_name, err),
                    );
                    // Detect managed auth-context secret-ref policy denials so the
                    // caller can emit a `policy.denied` event with the secret name.
                    if err_str.contains(MANAGED_SECRET_DENIAL_CODE) {
                        SecretResolveError::ManagedPolicyDenied {
                            secret_name: secret_name.clone(),
                            error: protocol_error,
                        }
                    } else {
                        SecretResolveError::Other(protocol_error)
                    }
                })?;
            let Some(value) = resolved else {
                return Err(SecretResolveError::Other(ProtocolError::new(
                    CanonicalErrorCode::SecretResolutionError,
                    format!("secret '{}' is not available", secret_name),
                )));
            };
            resolved_values.insert(secret_name.clone(), value);
        }
        Ok(resolved_values)
    }

    /// Checks declared secret references without materializing their values.
    pub(crate) fn check_secret_refs_available(
        &self,
        secret_refs: &[String],
    ) -> std::result::Result<Vec<String>, SecretResolveError> {
        for secret_name in secret_refs {
            let available = self
                .services
                .secrets
                .has_secret(secret_name)
                .map_err(|err| {
                    let err_str = err.to_string();
                    let error = ProtocolError::new(
                        CanonicalErrorCode::SecretResolutionError,
                        format!("failed checking secret '{}': {}", secret_name, err),
                    );
                    if err_str.contains(MANAGED_SECRET_DENIAL_CODE) {
                        SecretResolveError::ManagedPolicyDenied {
                            secret_name: secret_name.clone(),
                            error,
                        }
                    } else {
                        SecretResolveError::Other(error)
                    }
                })?;
            if !available {
                return Err(SecretResolveError::Other(ProtocolError::new(
                    CanonicalErrorCode::SecretResolutionError,
                    format!("secret '{}' is not available", secret_name),
                )));
            }
        }
        Ok(secret_refs.to_vec())
    }

    pub(crate) fn persist_result_files(
        &self,
        result: &ExecutionResult,
        artifact_mutation_count: usize,
        evidence_mutation_count: usize,
    ) -> Result<Vec<crate::failures::StateMutation>> {
        let mut state_mutations = Vec::new();
        for (index, artifact) in result.artifacts.iter().enumerate() {
            let mutation = self.persist_file_ref(artifact, "artifacts", index, result)?;
            if index < artifact_mutation_count {
                state_mutations.push(mutation);
            }
        }
        for (index, evidence) in result.evidence.iter().enumerate() {
            let mutation = self.persist_file_ref(evidence, "evidence", index, result)?;
            if index < evidence_mutation_count {
                state_mutations.push(mutation);
            }
        }
        Ok(state_mutations)
    }

    pub(crate) fn append_runtime_invoke_evidence(
        &self,
        request: Option<&ExecutionRequest>,
        record: &PersistedRunRecord,
        result: &mut ExecutionResult,
    ) -> Result<()> {
        let Some(request) = request else {
            return Ok(());
        };

        let artifact_dir = Path::new(&request.paths.artifacts_dir);
        let evidence_dir = Path::new(&request.paths.evidence_dir);
        let plugin_id = request
            .metadata
            .get("plugin_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let plugin_version = request
            .metadata
            .get("plugin_version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let plugin_digest = request
            .metadata
            .get("plugin_sha256")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let workflow_digest = record
            .trust_decision
            .as_ref()
            .map(|decision| decision.workflow.digest.clone())
            .or_else(|| {
                record
                    .recipe_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.sha256.clone())
            });

        let mut payload = serde_json::Map::from_iter([
            (
                "schema".to_string(),
                serde_json::Value::String("transist.runtime.invoke_evidence.v1".to_string()),
            ),
            (
                "run_id".to_string(),
                serde_json::Value::String(request.run_id.clone()),
            ),
            (
                "workflow_id".to_string(),
                serde_json::Value::String(request.workflow_id.clone()),
            ),
            (
                "step_id".to_string(),
                serde_json::Value::String(request.step.id.clone()),
            ),
            (
                "execution".to_string(),
                serde_json::Value::from(request.step.execution),
            ),
            (
                "invoke_id".to_string(),
                serde_json::Value::String(request.invoke_id.clone()),
            ),
            (
                "adapter_kind".to_string(),
                serde_json::Value::String(request.adapter_kind.to_string()),
            ),
            (
                "action".to_string(),
                serde_json::Value::String(request.action.clone()),
            ),
            (
                "status".to_string(),
                serde_json::Value::String(result.status.clone()),
            ),
            (
                "producing_workflow".to_string(),
                serde_json::json!({
                    "id": record.workflow_id.clone(),
                    "version": record.workflow_version.clone(),
                    "digest": workflow_digest,
                }),
            ),
            (
                "timing".to_string(),
                serde_json::to_value(&result.timing).context("serialize invoke timing")?,
            ),
            (
                "input_object_keys".to_string(),
                serde_json::to_value(sorted_json_object_keys(&request.input))
                    .context("serialize input object keys")?,
            ),
            (
                "metadata_object_keys".to_string(),
                serde_json::to_value(sorted_json_object_keys(&request.metadata))
                    .context("serialize metadata object keys")?,
            ),
            (
                "output_object_keys".to_string(),
                serde_json::to_value(sorted_json_object_keys(&result.output))
                    .context("serialize output object keys")?,
            ),
            (
                "artifact_refs".to_string(),
                serde_json::Value::Array(
                    result
                        .artifacts
                        .iter()
                        .map(|artifact| runtime_blob_ref_summary(artifact, artifact_dir))
                        .collect(),
                ),
            ),
            (
                "plugin_evidence_refs".to_string(),
                serde_json::Value::Array(
                    result
                        .evidence
                        .iter()
                        .map(|evidence| runtime_blob_ref_summary(evidence, evidence_dir))
                        .collect(),
                ),
            ),
            (
                "metrics".to_string(),
                serde_json::to_value(&result.metrics).context("serialize invoke metrics")?,
            ),
            (
                "metrics_validation_errors".to_string(),
                serde_json::Value::from(result.metrics_validation_errors),
            ),
        ]);
        if let Some(plugin_id) = plugin_id {
            payload.insert(
                "plugin_id".to_string(),
                serde_json::Value::String(plugin_id),
            );
        }
        payload.insert(
            "producing_plugin".to_string(),
            serde_json::json!({
                "id": request.metadata.get("plugin_id").and_then(serde_json::Value::as_str),
                "version": plugin_version,
                "digest": plugin_digest,
            }),
        );
        if let Some(exit_code) = result.exit_code {
            payload.insert("exit_code".to_string(), serde_json::Value::from(exit_code));
        }
        if let Some(usage) = result.usage.as_ref() {
            payload.insert(
                "usage".to_string(),
                serde_json::to_value(usage).context("serialize invoke usage")?,
            );
        }
        if let Some(filesystem) = request.policy.get("filesystem") {
            let source = filesystem
                .get("source")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("filesystem policy is missing source"))?;
            let workspace_access = filesystem
                .get("workspace_access")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("filesystem policy is missing workspace_access"))?;
            let read_allow_paths = filesystem
                .get("read_allow_paths")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("filesystem policy is missing read_allow_paths"))?;
            let write_allow_paths = filesystem
                .get("write_allow_paths")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("filesystem policy is missing write_allow_paths"))?;
            payload.insert(
                "filesystem_policy".to_string(),
                serde_json::json!({
                    "source": source,
                    "workspace_root": request.paths.workspace_dir.clone(),
                    "workspace_access": workspace_access,
                    "read_allow_paths": read_allow_paths,
                    "write_allow_paths": write_allow_paths,
                }),
            );
        }
        if let Some(error) = result.error.as_ref() {
            payload.insert(
                "error".to_string(),
                serde_json::json!({
                    "code": error.code,
                    "class": error.class,
                    "message": error.message,
                    "retryability": error.retryability,
                }),
            );
        }

        let payload = serde_json::Value::Object(payload);
        let body =
            serde_json::to_vec_pretty(&payload).context("serialize runtime invoke evidence")?;
        let path = self
            .services
            .artifacts
            .write_artifact(&ArtifactWriteRequest {
                run_id: result.run_id.clone(),
                step_id: result.step_id.clone(),
                execution: result.execution,
                logical_path: "evidence/transist_runtime_invoke_evidence.json".to_string(),
                payload: payload.into(),
            })?;
        result.evidence.push(FileRef {
            path: path.display().to_string(),
            sha256: sha256_hex(&body),
            bytes: body.len() as u64,
        });
        Ok(())
    }

    /// Records external-transfer evidence for one completed invoke when the
    /// effective `provider_kind` is `external`. Local-only runs never reach
    /// this point because external steps are denied before dispatch; thus
    /// local-only runs produce `external_transfers: []`.
    pub(crate) fn append_external_transfer_evidence(
        &self,
        request: Option<&ExecutionRequest>,
        record: &PersistedRunRecord,
        result: &mut ExecutionResult,
    ) -> Result<()> {
        let Some(request) = request else {
            return Ok(());
        };
        if !record.trusted_execution_mode && !record.local_only_mode {
            return Ok(());
        }
        let provider_kind = request
            .metadata
            .get("provider_kind")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<transist_protocol::ProviderKind>().ok())
            .unwrap_or(transist_protocol::ProviderKind::External);
        if !provider_kind.is_external() {
            return Ok(());
        }
        let provider = result
            .usage
            .as_ref()
            .and_then(|usage| usage.provider.clone())
            .or_else(|| {
                request
                    .metadata
                    .get("plugin_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        let destination = Self::infer_external_transfer_destination(request, provider.as_deref());
        let content_hash = {
            let input_bytes =
                serde_json::to_vec(&request.input).context("serialize transfer input payload")?;
            Some(sha256_hex(&input_bytes))
        };
        let now_ms = self.services.clock.now_unix_ms();
        let mode = if record.local_only_mode {
            "local_only"
        } else if record.trusted_execution_mode {
            "trusted_execution"
        } else {
            "default"
        };
        let transfer = transist_protocol::ExternalTransferRecord {
            schema: transist_protocol::EXTERNAL_TRANSFER_SCHEMA_V1.to_string(),
            run_id: result.run_id.clone(),
            step_id: result.step_id.clone(),
            execution: result.execution,
            timestamp_ms: now_ms,
            destination,
            adapter_kind: request.adapter_kind,
            provider,
            provider_kind,
            bytes_sent: None,
            content_hash,
            redaction_applied: false,
            policy_decision: transist_protocol::ExternalTransferPolicyDecision {
                allowed: true,
                mode: mode.to_string(),
            },
        };
        let body =
            serde_json::to_vec_pretty(&transfer).context("serialize external transfer evidence")?;
        let path = self
            .services
            .artifacts
            .write_artifact(&ArtifactWriteRequest {
                run_id: result.run_id.clone(),
                step_id: result.step_id.clone(),
                execution: result.execution,
                logical_path: format!(
                    "evidence/transist_external_transfer_{}.json",
                    result.execution
                ),
                payload: serde_json::from_slice(&body)
                    .context("deserialize external transfer evidence")?,
            })?;
        result.evidence.push(FileRef {
            path: path.display().to_string(),
            sha256: sha256_hex(&body),
            bytes: body.len() as u64,
        });
        Ok(())
    }

    fn infer_external_transfer_destination(
        request: &ExecutionRequest,
        provider: Option<&str>,
    ) -> Option<String> {
        request
            .metadata
            .get("entrypoint_base_url")
            .and_then(serde_json::Value::as_str)
            .and_then(Self::external_destination_from_url_like)
            .or_else(|| {
                for key in ["site_url", "base_url", "url"] {
                    if let Some(value) = request.input.get(key).and_then(serde_json::Value::as_str)
                    {
                        if let Some(destination) = Self::external_destination_from_url_like(value) {
                            return Some(destination);
                        }
                    }
                }
                None
            })
            .or_else(|| {
                request
                    .input
                    .get("repo_path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| provider.map(str::to_string))
    }

    fn external_destination_from_url_like(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let without_scheme = trimmed
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(trimmed);
        let authority = without_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .trim();
        if authority.is_empty() {
            return None;
        }
        let host = authority.rsplit('@').next().unwrap_or(authority);
        if host.is_empty() {
            return None;
        }
        Some(host.to_string())
    }

    pub(crate) fn persist_file_ref(
        &self,
        file_ref: &FileRef,
        family: &str,
        index: usize,
        result: &ExecutionResult,
    ) -> Result<crate::failures::StateMutation> {
        let base_name = Path::new(&file_ref.path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("descriptor");
        let logical_path = format!("{family}/{index:03}_{base_name}.json");
        self.services
            .artifacts
            .write_artifact(&crate::ports::ArtifactWriteRequest {
                run_id: result.run_id.clone(),
                step_id: result.step_id.clone(),
                execution: result.execution,
                logical_path: logical_path.clone(),
                payload: serde_json::json!({
                    "kind": family,
                    "source_path": file_ref.path,
                    "sha256": file_ref.sha256,
                    "bytes": file_ref.bytes,
                    "status": result.status,
                    "invoke_id": result.invoke_id,
                    "execution": result.execution
                })
                .into(),
            })?;
        Ok(crate::failures::StateMutation::ArtifactWritten {
            logical_path,
            sha256: file_ref.sha256.clone(),
        })
    }

    pub(crate) fn guard_result_files_against_policy(
        &self,
        request: &ExecutionRequest,
        result: &ExecutionResult,
        workspace_root: &Path,
    ) -> std::result::Result<(), crate::filesystem_policy::FilesystemDenial> {
        let Some(fs) = request.policy.get("filesystem") else {
            return Ok(());
        };
        if fs.is_null() {
            return Ok(());
        }
        #[derive(serde::Deserialize)]
        struct RequestFilesystemPolicy {
            #[serde(default)]
            source: Option<transist_protocol::FilesystemPolicySource>,
            #[serde(default)]
            workspace_access: Option<transist_protocol::WorkspaceAccessPolicy>,
            #[serde(default)]
            read_allow_paths: Vec<String>,
            #[serde(default)]
            write_allow_paths: Vec<String>,
        }
        let Ok(policy_json) = serde_json::from_value::<RequestFilesystemPolicy>(fs.clone()) else {
            return Ok(());
        };
        let policy_obj = transist_protocol::filesystem_policy::FilesystemPolicy {
            workspace_access: policy_json.workspace_access,
            read_allow_paths: policy_json.read_allow_paths,
            write_allow_paths: policy_json.write_allow_paths,
        };
        let source = policy_json
            .source
            .unwrap_or(transist_protocol::FilesystemPolicySource::Builtin);
        for file_ref in &result.artifacts {
            let path = resolve_policy_checked_file_ref_path(&file_ref.path, workspace_root);
            if file_ref_is_run_owned(self.services.run_store.as_ref(), &path, request) {
                continue;
            }
            crate::filesystem_policy::check_path(
                &policy_obj,
                source,
                crate::filesystem_policy::FilesystemOperation::Write,
                &path,
                &request.step.id,
                "artifact",
                workspace_root,
            )?;
        }
        for file_ref in &result.evidence {
            let path = resolve_policy_checked_file_ref_path(&file_ref.path, workspace_root);
            if file_ref_is_run_owned(self.services.run_store.as_ref(), &path, request) {
                continue;
            }
            crate::filesystem_policy::check_path(
                &policy_obj,
                source,
                crate::filesystem_policy::FilesystemOperation::Write,
                &path,
                &request.step.id,
                "evidence",
                workspace_root,
            )?;
        }
        Ok(())
    }

    /// Builds a classified failure record for failed or cancelled step executions.
    pub(crate) fn failure_record_for_step(
        &self,
        descriptor: &StepExecutionDescriptor,
        execution: &StepExecutionRecord,
        last_stable_step_id: Option<String>,
    ) -> Option<crate::failures::FailureRecord> {
        let result = execution.result.as_ref()?;
        if matches!(result.status.as_str(), "success" | "blocked") {
            return None;
        }

        let error_code = failure_error_code(result);
        let failure_class = crate::failures::classify(&error_code, descriptor);
        let mut flags = crate::failures::default_flags(failure_class);
        if matches!(
            failure_class,
            crate::failures::FailureClass::ToolError
                | crate::failures::FailureClass::Timeout
                | crate::failures::FailureClass::TransportError
        ) {
            flags.safe_to_retry = execution.state_mutations.is_empty();
        }
        let (adapter_kind, adapter_name, tool_name) = failure_attribution(descriptor);
        let timestamp = execution
            .finished_at_ms
            .or(execution.started_at_ms)
            .unwrap_or_else(|| self.services.clock.now_unix_ms());

        Some(crate::failures::FailureRecord {
            failure_id: crate::failures::new_failure_id(timestamp),
            run_id: execution.run_id.clone(),
            step_id: execution.step_id.clone(),
            execution_attempt: execution.execution,
            last_stable_step_id,
            failure_class,
            error_code,
            message_redacted: failure_message(result),
            adapter_kind,
            adapter_name,
            tool_name,
            retryable: flags.retryable,
            safe_to_retry: flags.safe_to_retry,
            requires_manual_attention: flags.requires_manual_attention,
            state_mutation_detected: !execution.state_mutations.is_empty(),
            first_seen_at_unix_ms: timestamp,
            last_seen_at_unix_ms: timestamp,
            occurrence_count: 1,
        })
    }
}

pub(crate) fn normalize_result_file_refs(
    result: &mut ExecutionResult,
    trusted_execution_mode: bool,
    read_result_file: impl Fn(&Path) -> Result<Vec<u8>>,
) -> Result<()> {
    if !trusted_execution_mode {
        return Ok(());
    }
    for file_ref in result
        .artifacts
        .iter_mut()
        .chain(result.evidence.iter_mut())
    {
        let path = Path::new(&file_ref.path);
        let bytes = read_result_file(path)
            .with_context(|| format!("failed reading persisted result file: {}", path.display()))?;
        let digest = sha256_hex(&bytes);
        if !file_ref.sha256.trim().is_empty() && file_ref.sha256 != digest {
            bail!(
                "trusted execution denied: result file checksum mismatch for '{}'",
                path.display()
            );
        }
        file_ref.sha256 = digest;
        file_ref.bytes = bytes.len() as u64;
    }
    Ok(())
}

fn verify_result_file_refs(
    result: &ExecutionResult,
    trusted_execution_mode: bool,
    read_result_file: impl Fn(&Path) -> Result<Vec<u8>>,
) -> Result<()> {
    if !trusted_execution_mode {
        return Ok(());
    }
    for file_ref in result.artifacts.iter().chain(result.evidence.iter()) {
        if file_ref.sha256.trim().is_empty() {
            continue;
        }
        let path = Path::new(&file_ref.path);
        let bytes = read_result_file(path)
            .with_context(|| format!("failed reading staged result file: {}", path.display()))?;
        let digest = sha256_hex(&bytes);
        if file_ref.sha256 != digest {
            bail!(
                "trusted execution denied: result file checksum mismatch for '{}'",
                path.display()
            );
        }
    }
    Ok(())
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::{
        RuntimeKernel, acquire_plugin_lifecycle_reservation_for_run_birth,
        normalize_result_file_refs, select_preset_workflow_assets, sha256_hex,
        verify_result_file_refs,
    };
    use crate::ports::{
        ControlRequestPort, ControlRequestRecord, ControlRequestType, LeaseBreakOptions,
        LeaseConflict, LeaseHandle, LeasePort, ManagementMode, PluginLifecycleReservationCanceled,
        PluginLifecycleReservationTimeout, RunTreeLeaseRecord, RuntimeOwnerIdentity,
        WorkflowAssetBlob,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use transist_protocol::{
        AdapterKind, ExecutionRequest, ExecutionResult, FileRef, InvokePaths, InvokeStep,
        InvokeTiming, Recipe, RecipePromptBinding, RecipePromptSource, RecipeStep,
        StepExecutionDescriptor, TEP_INVOKE_REQUEST_TYPE_V1, TEP_INVOKE_RESULT_TYPE_V1,
        TEP_PROTOCOL_VERSION_V1, WorkflowRef,
    };

    fn asset_recipe(id: &str, source_dir: &str, prompt: Option<&str>) -> Recipe {
        Recipe {
            schema: "transist.recipe.v1".to_string(),
            workflow: WorkflowRef {
                id: id.to_string(),
                version: "v1".to_string(),
            },
            source_dir: Some(source_dir.to_string()),
            workspace: None,
            repositories: Vec::new(),
            prerequisites: Default::default(),
            filesystem_policy: None,
            network_policy: None,
            inputs: BTreeMap::new(),
            steps: vec![RecipeStep {
                id: "step".to_string(),
                execution: StepExecutionDescriptor::Control {
                    primitive: "noop".to_string(),
                },
                with: BTreeMap::new(),
                prompt_binding: prompt.map(|path| RecipePromptBinding {
                    inline_field: "prompt".to_string(),
                    file_field: Some("prompt_file".to_string()),
                    source: RecipePromptSource::File {
                        path: path.to_string(),
                    },
                }),
                skills_binding: None,
                vars: BTreeMap::new(),
                filesystem_policy: None,
                network_policy: None,
                effective_filesystem_policy: Default::default(),
                effective_filesystem_policy_source: None,
                effective_network_policy: Default::default(),
                effective_network_policy_source: None,
            }],
            cleanup: Vec::new(),
            graph: None,
            capabilities: BTreeMap::new(),
            capabilities_persisted: false,
            documents: Default::default(),
        }
    }

    fn sample_result(path: &std::path::Path, sha256: &str) -> ExecutionResult {
        ExecutionResult {
            message_type: TEP_INVOKE_RESULT_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke-1".to_string(),
            run_id: "run-1".to_string(),
            step_id: "step-1".to_string(),
            execution: 1,
            status: "success".to_string(),
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timing: InvokeTiming {
                started_at: "0".to_string(),
                finished_at: "0".to_string(),
                duration_ms: 0,
            },
            artifacts: vec![FileRef {
                path: path.display().to_string(),
                sha256: sha256.to_string(),
                bytes: 0,
            }],
            evidence: Vec::new(),
            metrics: Default::default(),
            output: serde_json::json!({}),
            metrics_validation_errors: 0,
            usage: None,
            error: None,
        }
    }

    fn sample_request(input: serde_json::Value, metadata: serde_json::Value) -> ExecutionRequest {
        ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke-1".to_string(),
            run_id: "run-1".to_string(),
            workflow_id: "workflow-1".to_string(),
            step: InvokeStep {
                id: "step-1".to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "demo".to_string(),
            input,
            paths: InvokePaths {
                run_root: "/tmp/run-1".to_string(),
                artifacts_dir: "/tmp/run-1/artifacts".to_string(),
                evidence_dir: "/tmp/run-1/evidence".to_string(),
                workspace_dir: "/tmp/workspace".to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: None,
            max_timeout_ms: None,
            policy: serde_json::json!({}),
            metadata,
        }
    }

    #[derive(Clone, Copy)]
    enum ReservationOutcome {
        Conflict,
        Success,
        Failure,
    }

    struct ScriptedReservationLease {
        outcomes: Mutex<Vec<ReservationOutcome>>,
        attempts: AtomicUsize,
        inspections: AtomicUsize,
        breaks: AtomicUsize,
    }

    impl ScriptedReservationLease {
        fn new(outcomes: Vec<ReservationOutcome>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes),
                attempts: AtomicUsize::new(0),
                inspections: AtomicUsize::new(0),
                breaks: AtomicUsize::new(0),
            }
        }

        fn owner() -> RuntimeOwnerIdentity {
            RuntimeOwnerIdentity {
                management_mode: ManagementMode::Standalone,
                owner_id: "run-birth-owner".to_string(),
                lease_id: "run-birth-lease".to_string(),
                pid: 7,
                hostname: "run-birth-host".to_string(),
                command: "transist workflows prepare".to_string(),
                transist_version: "test".to_string(),
                external: None,
                runtime_id: None,
                organization_id: None,
            }
        }

        fn live_lease() -> RunTreeLeaseRecord {
            RunTreeLeaseRecord {
                schema_version: RunTreeLeaseRecord::SCHEMA_VERSION.to_string(),
                root_run_id: "plugin-lifecycle-reservation".to_string(),
                lease_id: "live-lease".to_string(),
                owner_id: "live-owner".to_string(),
                management_mode: ManagementMode::Standalone,
                pid: 42,
                hostname: "live-host".to_string(),
                command: "helper process".to_string(),
                transist_version: "test".to_string(),
                acquired_at_ms: 1,
                last_heartbeat_ms: 1,
                external: None,
            }
        }
    }

    impl LeasePort for ScriptedReservationLease {
        fn owner_identity(&self) -> RuntimeOwnerIdentity {
            Self::owner()
        }

        fn acquire_root_run_tree_lease(&self, root_run_id: &str) -> anyhow::Result<LeaseHandle> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            match self.outcomes.lock().expect("outcomes").remove(0) {
                ReservationOutcome::Conflict => Err(anyhow::Error::new(LeaseConflict {
                    root_run_id: root_run_id.to_string(),
                    live_lease: Self::live_lease(),
                })
                .context("scripted reservation conflict")),
                ReservationOutcome::Success => Ok(LeaseHandle {
                    root_run_id: root_run_id.to_string(),
                    lease_id: Self::owner().lease_id,
                    owner: Self::owner(),
                }),
                ReservationOutcome::Failure => anyhow::bail!("scripted non-conflict failure"),
            }
        }

        fn refresh_root_run_tree_lease(
            &self,
            handle: &LeaseHandle,
        ) -> anyhow::Result<RunTreeLeaseRecord> {
            Ok(RunTreeLeaseRecord {
                root_run_id: handle.root_run_id.clone(),
                ..Self::live_lease()
            })
        }

        fn inspect_root_run_tree_lease(
            &self,
            _root_run_id: &str,
        ) -> anyhow::Result<Option<RunTreeLeaseRecord>> {
            self.inspections.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Self::live_lease()))
        }

        fn break_root_run_tree_lease(
            &self,
            _root_run_id: &str,
            _options: LeaseBreakOptions,
        ) -> anyhow::Result<Option<RunTreeLeaseRecord>> {
            self.breaks.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Self::live_lease()))
        }

        fn release_root_run_tree_lease(&self, _handle: &LeaseHandle) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct ScriptedCancellation {
        cancel_on_check: usize,
        checks: AtomicUsize,
    }

    impl ScriptedCancellation {
        fn never() -> Self {
            Self {
                cancel_on_check: usize::MAX,
                checks: AtomicUsize::new(0),
            }
        }
    }

    impl ControlRequestPort for ScriptedCancellation {
        fn request_cancel(&self, root_run_id: &str) -> anyhow::Result<ControlRequestRecord> {
            Ok(ControlRequestRecord {
                schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
                request_id: "cancel-request".to_string(),
                request_type: ControlRequestType::Cancel,
                root_run_id: root_run_id.to_string(),
                requested_at_ms: 1,
                requester: None,
            })
        }

        fn inspect_cancel_request(
            &self,
            root_run_id: &str,
        ) -> anyhow::Result<Option<ControlRequestRecord>> {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if check >= self.cancel_on_check {
                Ok(Some(ControlRequestRecord {
                    schema_version: ControlRequestRecord::SCHEMA_VERSION.to_string(),
                    request_id: "cancel-request".to_string(),
                    request_type: ControlRequestType::Cancel,
                    root_run_id: root_run_id.to_string(),
                    requested_at_ms: 1,
                    requester: None,
                }))
            } else {
                Ok(None)
            }
        }

        fn clear_cancel_request(&self, _root_run_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn plugin_lifecycle_reservation_retries_conflicts_then_succeeds_without_inspection_or_breaking()
    {
        let lease = ScriptedReservationLease::new(vec![
            ReservationOutcome::Conflict,
            ReservationOutcome::Conflict,
            ReservationOutcome::Success,
        ]);
        let cancellation = ScriptedCancellation::never();

        let handle = acquire_plugin_lifecycle_reservation_for_run_birth(
            &lease,
            &cancellation,
            None,
            std::time::Duration::from_secs(1),
        )
        .expect("third acquisition succeeds");

        assert_eq!(handle.root_run_id, "plugin-lifecycle-reservation");
        assert_eq!(lease.attempts.load(Ordering::SeqCst), 3);
        assert_eq!(lease.inspections.load(Ordering::SeqCst), 0);
        assert_eq!(lease.breaks.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn plugin_lifecycle_reservation_timeout_keeps_the_latest_conflicting_owner() {
        let lease = ScriptedReservationLease::new(vec![ReservationOutcome::Conflict]);
        let cancellation = ScriptedCancellation::never();

        let error = acquire_plugin_lifecycle_reservation_for_run_birth(
            &lease,
            &cancellation,
            None,
            std::time::Duration::ZERO,
        )
        .expect_err("zero wait times out after the immediate acquisition");
        let timeout = error
            .downcast_ref::<PluginLifecycleReservationTimeout>()
            .expect("typed timeout");

        assert_eq!(lease.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(timeout.timeout_ms, 0);
        assert_eq!(timeout.live_lease.owner_id, "live-owner");
        assert!(timeout.to_string().contains("pid 42"));
        assert!(timeout.to_string().contains("helper process"));
    }

    #[test]
    fn plugin_lifecycle_reservation_cancels_a_child_before_retrying() {
        let lease = ScriptedReservationLease::new(vec![
            ReservationOutcome::Conflict,
            ReservationOutcome::Success,
        ]);
        let cancellation = ScriptedCancellation {
            cancel_on_check: 2,
            checks: AtomicUsize::new(0),
        };

        let error = acquire_plugin_lifecycle_reservation_for_run_birth(
            &lease,
            &cancellation,
            Some("root-run"),
            std::time::Duration::from_secs(1),
        )
        .expect_err("cancellation stops the next acquisition attempt");
        let canceled = error
            .downcast_ref::<PluginLifecycleReservationCanceled>()
            .expect("typed cancellation");

        assert_eq!(canceled.root_run_id, "root-run");
        assert_eq!(lease.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(cancellation.checks.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn plugin_lifecycle_reservation_propagates_non_conflicts_without_retrying() {
        let lease = ScriptedReservationLease::new(vec![ReservationOutcome::Failure]);
        let cancellation = ScriptedCancellation::never();

        let error = acquire_plugin_lifecycle_reservation_for_run_birth(
            &lease,
            &cancellation,
            None,
            std::time::Duration::from_secs(1),
        )
        .expect_err("non-conflict acquisition error propagates");

        assert_eq!(error.to_string(), "scripted non-conflict failure");
        assert_eq!(lease.attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn normalize_result_file_refs_rejects_checksum_mismatch_in_trusted_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("artifact.txt");
        fs::write(&file_path, "artifact payload").expect("write artifact");
        let mut result = sample_result(&file_path, "not-the-real-digest");

        let error = normalize_result_file_refs(&mut result, true, |path| {
            fs::read(path).map_err(Into::into)
        })
        .expect_err("checksum mismatch");
        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn verify_result_file_refs_rejects_adapter_checksum_before_promotion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("artifact.txt");
        fs::write(&file_path, "artifact payload").expect("write artifact");
        let result = sample_result(&file_path, "adapter-reported-digest");

        let error =
            verify_result_file_refs(&result, true, |path| fs::read(path).map_err(Into::into))
                .expect_err("adapter checksum mismatch");
        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn normalize_result_file_refs_preserves_pass_through_when_trusted_mode_is_off() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("artifact.txt");
        fs::write(&file_path, "artifact payload").expect("write artifact");
        let mut result = sample_result(&file_path, "adapter-reported-digest");

        normalize_result_file_refs(&mut result, false, |path| {
            fs::read(path).map_err(Into::into)
        })
        .expect("trusted mode disabled");
        assert_eq!(result.artifacts[0].sha256, "adapter-reported-digest");
    }

    #[test]
    fn normalize_result_file_refs_preserves_file_order_and_bytes() {
        let mut result = sample_result(std::path::Path::new("artifact-a"), "");
        result.artifacts.push(FileRef {
            path: "artifact-b".to_string(),
            sha256: String::new(),
            bytes: 0,
        });
        result.evidence.push(FileRef {
            path: "evidence-a".to_string(),
            sha256: String::new(),
            bytes: 0,
        });
        let reads = Arc::new(Mutex::new(Vec::new()));
        let reads_for_loader = Arc::clone(&reads);
        normalize_result_file_refs(&mut result, true, move |path| {
            reads_for_loader
                .lock()
                .unwrap()
                .push(path.display().to_string());
            Ok(match path.to_str().unwrap() {
                "artifact-a" => b"a\0".to_vec(),
                "artifact-b" => b"b\xff".to_vec(),
                "evidence-a" => b"e\n".to_vec(),
                other => panic!("unexpected result file: {other}"),
            })
        })
        .expect("normalize result files");

        assert_eq!(
            *reads.lock().unwrap(),
            vec![
                "artifact-a".to_string(),
                "artifact-b".to_string(),
                "evidence-a".to_string(),
            ]
        );
        assert_eq!(result.artifacts[0].bytes, 2);
        assert_eq!(result.artifacts[1].bytes, 2);
        assert_eq!(result.evidence[0].bytes, 2);
        assert_eq!(result.artifacts[0].sha256, sha256_hex(b"a\0"));
        assert_eq!(result.artifacts[1].sha256, sha256_hex(b"b\xff"));
        assert_eq!(result.evidence[0].sha256, sha256_hex(b"e\n"));
    }

    #[test]
    fn preset_workflow_assets_fail_closed_when_child_asset_is_missing() {
        let parent = asset_recipe("parent", "/workflow", None);
        let child = asset_recipe("child", "/workflow", Some("prompts/child.md"));
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema": "transist.workflow_asset_snapshot.v1",
            "entries": [{"path": "source/prompts/other.md", "sha256": "other"}],
        }))
        .expect("manifest");

        let error = select_preset_workflow_assets(
            &parent,
            &child,
            (manifest, Vec::<WorkflowAssetBlob>::new()),
        )
        .expect_err("missing declared child asset must fail closed");

        assert!(
            error
                .to_string()
                .contains("missing child asset 'source/prompts/child.md'")
        );
    }

    #[test]
    fn external_destination_from_url_like_extracts_host() {
        assert_eq!(
            RuntimeKernel::external_destination_from_url_like(
                "https://example.com/rest/api/3/issue/ISSUE-186"
            ),
            Some("example.com".to_string())
        );
        assert_eq!(
            RuntimeKernel::external_destination_from_url_like(
                "https://user:pass@example.com:8443/foo"
            ),
            Some("example.com:8443".to_string())
        );
        assert_eq!(RuntimeKernel::external_destination_from_url_like(""), None);
    }

    #[test]
    fn infer_external_transfer_destination_prefers_site_url_then_repo_path_then_provider() {
        let jira_request = sample_request(
            serde_json::json!({
                "site_url": "https://example.com/rest/api/3/issue/ISSUE-186",
                "repo_path": "/tmp/repo"
            }),
            serde_json::json!({"plugin_id": "jira.rest"}),
        );
        assert_eq!(
            RuntimeKernel::infer_external_transfer_destination(&jira_request, Some("jira.rest")),
            Some("example.com".to_string())
        );

        let repo_request = sample_request(
            serde_json::json!({"repo_path": "/tmp/repo"}),
            serde_json::json!({"plugin_id": "git.cli"}),
        );
        assert_eq!(
            RuntimeKernel::infer_external_transfer_destination(&repo_request, Some("git.cli")),
            Some("/tmp/repo".to_string())
        );

        let provider_request = sample_request(serde_json::json!({}), serde_json::json!({}));
        assert_eq!(
            RuntimeKernel::infer_external_transfer_destination(&provider_request, Some("openspec")),
            Some("openspec".to_string())
        );
    }
}

fn last_stable_step_id_from_snapshot(
    recipe: &Recipe,
    snapshot: &KernelRunSnapshot,
) -> Option<String> {
    recipe.steps.iter().rev().find_map(|step| {
        snapshot
            .steps
            .get(&step.id)
            .filter(|state| state.status == KernelStepStatus::Succeeded)
            .map(|_| step.id.clone())
    })
}

fn resolve_policy_checked_file_ref_path<'a>(
    path: &'a str,
    workspace_root: &'a Path,
) -> std::borrow::Cow<'a, Path> {
    let path = Path::new(path);
    if path.is_absolute() {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Owned(workspace_root.join(path))
    }
}

fn file_ref_is_run_owned(
    run_store: &dyn RunStorePort,
    path: &Path,
    request: &ExecutionRequest,
) -> bool {
    run_store.result_file_is_within(path, Path::new(&request.paths.artifacts_dir))
        || run_store.result_file_is_within(path, Path::new(&request.paths.evidence_dir))
}

fn failure_error_code(result: &ExecutionResult) -> String {
    if let Some(code) = result
        .error
        .as_ref()
        .and_then(|error| serde_json::to_value(error.code).ok())
        .and_then(|value| value.as_str().map(str::to_string))
    {
        return code;
    }
    if result.status == "cancelled" {
        return "cancelled".to_string();
    }
    "unknown_error".to_string()
}

fn failure_message(result: &ExecutionResult) -> String {
    let message = result
        .error
        .as_ref()
        .map(|error| error.message.trim())
        .filter(|message| !message.is_empty())
        .or_else(|| {
            let stderr = result.stderr.trim();
            (result.status != "success" && !stderr.is_empty()).then_some(stderr)
        })
        .unwrap_or("unknown error");

    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "unknown error".to_string()
    } else {
        collapsed
    }
}

fn failure_attribution(
    descriptor: &StepExecutionDescriptor,
) -> (String, Option<String>, Option<String>) {
    match descriptor {
        StepExecutionDescriptor::Invoke {
            adapter,
            plugin,
            action,
            ..
        } => (
            adapter.to_string(),
            Some(plugin.clone()),
            Some(action.clone()),
        ),
        StepExecutionDescriptor::Control { primitive } => (
            "control".to_string(),
            Some("control".to_string()),
            Some(primitive.clone()),
        ),
        StepExecutionDescriptor::Subflow { recipe } => (
            "subflow".to_string(),
            Some("subflow".to_string()),
            Some(format!(
                "{}@{}",
                recipe.workflow.id, recipe.workflow.version
            )),
        ),
    }
}

fn resolve_active_invoke_id_in_root_run_tree(
    store: &dyn RunStorePort,
    home_dir: &Path,
    root_run_id: &str,
) -> Result<Option<String>> {
    let mut active_steps = Vec::new();
    for run_root in store.list_run_tree_roots(home_dir, root_run_id)? {
        let Some(run_id) = store.run_id_from_run_root(&run_root) else {
            continue;
        };
        if store.resolve_root_run_id(home_dir, &run_id)? != root_run_id {
            bail!(
                "run '{}' in selected tree has inconsistent root lineage",
                run_id
            );
        }
        let Some(record) = load_run_record(store, &run_id)? else {
            continue;
        };
        // Main/child work is canceled before the root owns terminal cleanup.
        // Once canceled cleanup starts, another cancel is idempotent: it must
        // never signal that cleanup invocation. Other cleanup triggers remain
        // interruptible without changing the primary terminal outcome.
        let cleanup_steps = record.cleanup.as_ref().filter(|cleanup| {
            cleanup.state == CleanupPhaseState::Running
                && !(record.status == KernelRunStatus::Canceled
                    && cleanup.trigger == CleanupTrigger::Canceled)
        });
        for step in record.steps.values().chain(
            cleanup_steps
                .into_iter()
                .flat_map(|cleanup| cleanup.steps.values()),
        ) {
            let Some(request) = step.last_request.as_ref() else {
                continue;
            };
            if !matches!(
                step.status,
                KernelStepStatus::Dispatching | KernelStepStatus::Running
            ) {
                continue;
            }
            active_steps.push((
                step.started_at_ms.unwrap_or_default(),
                request.invoke_id.clone(),
            ));
        }
    }
    active_steps.sort();
    Ok(active_steps.pop().map(|(_, invoke_id)| invoke_id))
}

/// Resolves and applies the `set` block of a completed invoke step into the
/// run-scoped `workflow_vars` map. Each value in `step.vars` may contain
/// `{{inputs.<key>}}` or `{{steps.<id>.out.<key>}}` placeholders that are
/// expanded against the current `workflow_inputs` and the `snapshot` step
/// outputs respectively. Non-string values (booleans, numbers, objects,
/// arrays) are stored as-is after recursive resolution.
///
/// This function is called only when the step has succeeded and its `vars`
/// map is non-empty. Writes are last-write-wins within a single step.
fn apply_set_vars(
    step: &transist_protocol::RecipeStep,
    workflow_inputs: &BTreeMap<String, KernelValue>,
    snapshot: &KernelRunSnapshot,
    workflow_vars: &mut BTreeMap<String, KernelValue>,
) {
    for (var_key, template_value) in &step.vars {
        let resolved = resolve_set_kernel_value(template_value, workflow_inputs, snapshot);
        workflow_vars.insert(var_key.clone(), resolved);
    }
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> Result<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("execution metadata is missing non-empty string '{key}'"))
}

fn add_optional_policy_id(payload: &mut serde_json::Value, policy: &serde_json::Value) {
    if let Some(policy_id) = policy
        .get("policy_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    {
        payload["policy_id"] = serde_json::Value::String(policy_id.to_string());
    }
}
