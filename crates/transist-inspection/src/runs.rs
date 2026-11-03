// SPDX-License-Identifier: MPL-2.0
//! Run-focused inspection helpers.

use super::*;
use crate::gates::build_gates;
use crate::steps::step_status_name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveRunProjection {
    pub status: String,
    pub current_step_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveChildProjection {
    step_id: String,
    status: String,
    updated_at_ms: u64,
}

pub(crate) fn projected_run_summaries(
    records: &[PersistedRunSummaryRecord],
) -> Vec<InspectionRunSummary> {
    let summaries_by_id = records
        .iter()
        .cloned()
        .map(|record| (record.run_id.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let mut children_by_parent = BTreeMap::<String, Vec<String>>::new();
    for record in summaries_by_id.values() {
        if let Some(parent_run_id) = record.parent_run_id.as_ref() {
            children_by_parent
                .entry(parent_run_id.clone())
                .or_default()
                .push(record.run_id.clone());
        }
    }
    let mut cache = BTreeMap::new();
    let local_has_blocked_gates = summaries_by_id
        .values()
        .map(|record| (record.run_id.clone(), record.has_blocked_gates))
        .collect::<BTreeMap<_, _>>();
    let mut blocked_gates_cache = BTreeMap::new();
    summaries_by_id
        .values()
        .map(|record| {
            let effective = effective_run_projection_from_summaries(
                &summaries_by_id,
                &children_by_parent,
                &record.run_id,
                &mut cache,
            );
            InspectionRunSummary {
                run_id: record.run_id.clone(),
                workflow_id: record.workflow_id.clone(),
                workflow_version: record.workflow_version.clone(),
                status: effective.status,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.updated_at_ms,
                started_at_ms: record.started_at_ms,
                finished_at_ms: record.finished_at_ms,
                current_step_id: effective.current_step_id,
                has_blocked_gates: run_has_blocked_gates_from_children(
                    &children_by_parent,
                    &local_has_blocked_gates,
                    &record.run_id,
                    &mut blocked_gates_cache,
                ),
                has_artifacts: record.has_artifacts,
                has_events: record.has_events,
                artifact_count: record.artifact_count,
                evidence_count: record.evidence_count,
                root_run_id: record.root_run_id.clone(),
                parent_run_id: record.parent_run_id.clone(),
            }
        })
        .collect()
}

pub(crate) fn build_run_summary(
    store: &LocalInspectionStore,
    record: &PersistedRunRecord,
) -> Result<InspectionRunSummary> {
    let gates = build_gates(store, record, &record.run_id)?;
    let artifacts = store.list_blobs(&record.run_id, BlobFamily::Artifact)?;
    let evidence = store.list_blobs(&record.run_id, BlobFamily::Evidence)?;
    let events = store.list_events(&record.run_id)?;
    let (started_at_ms, finished_at_ms) = step_timestamps(record.steps.values());
    let root_run_id = effective_root_run_id(store, record);
    let effective = effective_run_projection(store, record)?;
    let has_blocked_gates = LocalRunSummaryStore::new(store.home_dir())
        .list_run_summaries()
        .ok()
        .map(|summaries| projected_run_summaries(&summaries))
        .and_then(|summaries| {
            summaries
                .into_iter()
                .find(|summary| summary.run_id == record.run_id)
                .map(|summary| summary.has_blocked_gates)
        })
        .unwrap_or_else(|| gates.iter().any(|gate| gate.status == "blocked"));
    Ok(InspectionRunSummary {
        run_id: record.run_id.clone(),
        workflow_id: record.workflow_id.clone(),
        workflow_version: record.workflow_version.clone(),
        status: effective.status,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        started_at_ms,
        finished_at_ms,
        current_step_id: effective.current_step_id,
        has_blocked_gates,
        has_artifacts: !artifacts.is_empty(),
        has_events: !events.is_empty(),
        artifact_count: artifacts.len(),
        evidence_count: evidence.len(),
        parent_run_id: embedded_parent_run_id(store, &record.run_id, &root_run_id),
        root_run_id,
    })
}

pub(crate) fn matches_run_filters(record: &InspectionRunSummary, query: &RunListQuery) -> bool {
    if let Some(status) = query.status.as_deref() {
        if record.status.as_str() != status {
            return false;
        }
    }
    if let Some(workflow) = query.workflow.as_deref() {
        let canonical = format!("{}@{}", record.workflow_id, record.workflow_version);
        if workflow.contains('@') {
            if canonical != workflow {
                return false;
            }
        } else if record.workflow_id != workflow {
            return false;
        }
    }
    if let Some(created_after_ms) = query.created_after_ms {
        if record.created_at_ms <= created_after_ms {
            return false;
        }
    }
    if let Some(created_before_ms) = query.created_before_ms {
        if record.created_at_ms >= created_before_ms {
            return false;
        }
    }
    if let Some(updated_after_ms) = query.updated_after_ms {
        if record.updated_at_ms <= updated_after_ms {
            return false;
        }
    }
    if let Some(updated_before_ms) = query.updated_before_ms {
        if record.updated_at_ms >= updated_before_ms {
            return false;
        }
    }
    true
}

pub(crate) fn list_contained_runs_from_summaries(
    summaries: &[InspectionRunSummary],
    root_run_id: &str,
    current_run_id: &str,
) -> Vec<InspectionRunSummary> {
    let mut children_by_parent = BTreeMap::<String, Vec<InspectionRunSummary>>::new();
    for summary in summaries
        .iter()
        .filter(|summary| summary.root_run_id == root_run_id)
    {
        let Some(parent_run_id) = summary.parent_run_id.as_ref() else {
            continue;
        };
        children_by_parent
            .entry(parent_run_id.clone())
            .or_default()
            .push(summary.clone());
    }

    for children in children_by_parent.values_mut() {
        children.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then(left.run_id.cmp(&right.run_id))
        });
    }

    let mut runs = Vec::new();
    collect_descendant_runs(&children_by_parent, current_run_id, &mut runs);
    runs
}

fn collect_descendant_runs(
    children_by_parent: &BTreeMap<String, Vec<InspectionRunSummary>>,
    current_run_id: &str,
    runs: &mut Vec<InspectionRunSummary>,
) {
    let Some(children) = children_by_parent.get(current_run_id) else {
        return;
    };

    for child in children {
        runs.push(child.clone());
        collect_descendant_runs(children_by_parent, &child.run_id, runs);
    }
}

pub(crate) fn resolve_actionable_run_id(
    store: &LocalInspectionStore,
    summaries: &[InspectionRunSummary],
    run_id: &str,
) -> Result<Option<String>> {
    let summary_by_id = summaries
        .iter()
        .cloned()
        .map(|summary| (summary.run_id.clone(), summary))
        .collect::<BTreeMap<_, _>>();
    let children_by_parent = summaries.iter().fold(
        BTreeMap::<String, Vec<InspectionRunSummary>>::new(),
        |mut acc, summary| {
            if let Some(parent_run_id) = summary.parent_run_id.as_ref() {
                acc.entry(parent_run_id.clone())
                    .or_default()
                    .push(summary.clone());
            }
            acc
        },
    );
    let mut current_run_id = run_id.to_string();
    let mut visited = BTreeSet::new();

    loop {
        if !visited.insert(current_run_id.clone()) {
            return Ok(None);
        }

        let record = store.load_run_record(&current_run_id)?;
        let gates = build_gates(store, &record, &current_run_id)?;
        if gates.iter().any(|gate| gate.status == "blocked") {
            return Ok((current_run_id != run_id).then_some(current_run_id));
        }

        let current_summary = child_summary_for_run(store, &summary_by_id, &current_run_id)?;
        let next = next_actionable_child_run(
            store,
            &summary_by_id,
            &children_by_parent,
            &current_summary,
        )?;
        if let Some(next_run_id) = next {
            current_run_id = next_run_id;
            continue;
        }

        return Ok((current_run_id != run_id).then_some(current_run_id));
    }
}

fn next_actionable_child_run(
    store: &LocalInspectionStore,
    summary_by_id: &BTreeMap<String, InspectionRunSummary>,
    children_by_parent: &BTreeMap<String, Vec<InspectionRunSummary>>,
    current_summary: &InspectionRunSummary,
) -> Result<Option<String>> {
    let mut preferred = None;
    let mut fallback = None;

    for child in children_by_parent
        .get(&current_summary.run_id)
        .into_iter()
        .flatten()
    {
        let child_summary = child_summary_for_run(store, summary_by_id, &child.run_id)?;
        if child_summary.status == "success" {
            continue;
        }

        let parent_step_id = direct_embedded_step_id(&current_summary.run_id, &child.run_id);
        if child_summary.status == "blocked"
            || parent_step_id.as_deref() == current_summary.current_step_id.as_deref()
        {
            preferred = Some(child.run_id.clone());
            break;
        }

        match &fallback {
            Some((best_updated_at_ms, _)) if *best_updated_at_ms >= child.updated_at_ms => {}
            _ => fallback = Some((child.updated_at_ms, child.run_id.clone())),
        }
    }

    Ok(preferred.or_else(|| fallback.map(|(_, run_id)| run_id)))
}

fn child_summary_for_run(
    store: &LocalInspectionStore,
    summary_by_id: &BTreeMap<String, InspectionRunSummary>,
    run_id: &str,
) -> Result<InspectionRunSummary> {
    if let Some(summary) = summary_by_id.get(run_id) {
        return Ok(summary.clone());
    }
    let record = store.load_run_record(run_id)?;
    build_run_summary(store, &record)
}

pub(crate) fn effective_root_run_id(
    store: &LocalInspectionStore,
    record: &PersistedRunRecord,
) -> String {
    let persisted_root = record
        .lineage
        .as_ref()
        .map(|lineage| lineage.root_run_id.trim())
        .filter(|root_run_id| !root_run_id.is_empty())
        .map(ToOwned::to_owned);

    match persisted_root {
        Some(root_run_id) if root_run_id != record.run_id => root_run_id,
        Some(_) | None => infer_embedded_root_run_id(store, &record.run_id)
            .unwrap_or_else(|| record.run_id.clone()),
    }
}

pub(crate) fn embedded_parent_run_id(
    store: &LocalInspectionStore,
    run_id: &str,
    root_run_id: &str,
) -> Option<String> {
    store
        .load_run_record(run_id)
        .ok()
        .and_then(|record| embedded_parent_run_id_for_record(store, &record, root_run_id))
}

pub(crate) fn infer_embedded_root_run_id(
    store: &LocalInspectionStore,
    run_id: &str,
) -> Option<String> {
    let (root_run_id, _) = run_id.split_once("__")?;
    if root_run_id.is_empty() || root_run_id == run_id || !store.run_exists(root_run_id) {
        return None;
    }
    Some(root_run_id.to_string())
}

pub(crate) fn ordered_step_ids(
    record: &PersistedRunRecord,
    recipe: Option<&Recipe>,
) -> Vec<String> {
    // Prefer recipe order when available, then append any extra persisted steps so
    // inspection remains stable even for older or partially reconstructed runs.
    let mut out = recipe
        .map(|recipe| {
            recipe
                .steps
                .iter()
                .map(|step| step.id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let seen = out.iter().cloned().collect::<BTreeSet<_>>();
    for step_id in record.steps.keys() {
        if !seen.contains(step_id) {
            out.push(step_id.clone());
        }
    }
    if out.is_empty() {
        out.extend(record.steps.keys().cloned());
    }
    out
}

pub(crate) fn step_timestamps<'a>(
    steps: impl IntoIterator<Item = &'a KernelStepState>,
) -> (Option<u64>, Option<u64>) {
    let mut started: Option<u64> = None;
    let mut finished: Option<u64> = None;
    for step in steps {
        started = match (started, step.started_at_ms) {
            (Some(current), Some(candidate)) => Some(current.min(candidate)),
            (None, Some(candidate)) => Some(candidate),
            (current, None) => current,
        };
        finished = match (finished, step.finished_at_ms) {
            (Some(current), Some(candidate)) => Some(current.max(candidate)),
            (None, Some(candidate)) => Some(candidate),
            (current, None) => current,
        };
    }
    (started, finished)
}

pub(crate) fn current_step_id(steps: &BTreeMap<String, KernelStepState>) -> Option<String> {
    steps
        .iter()
        .max_by(|left, right| {
            // Choose the most active-looking step first; timestamps break ties so
            // newer running/blocked steps win over older ones.
            step_priority(left.1.status)
                .cmp(&step_priority(right.1.status))
                .then(left.1.started_at_ms.cmp(&right.1.started_at_ms))
        })
        .and_then(|(step_id, state)| (step_priority(state.status) > 0).then_some(step_id.clone()))
}

pub(crate) fn effective_run_projection(
    store: &LocalInspectionStore,
    record: &PersistedRunRecord,
) -> Result<EffectiveRunProjection> {
    // `failed` and `canceled` both opt into descendant projection: after a
    // `runs resume`, the kernel may have already started executing a child
    // before it has rewritten the root's `summary.json` away from its prior
    // terminal status. Surfacing the live descendant status here lets the
    // run-detail action picker offer what the operator can actually do
    // (e.g. gate decisions on a `blocked` child) instead of stale resume/
    // cleanup/replay actions that no longer apply.
    //
    // `success` remains irreversibly terminal: descendants must not
    // resurrect it. `most_active_embedded_child` only matches
    // `running`/`waiting`/`blocked` children, so abandoned terminal trees
    // continue to surface as their root's status.
    let active_child = if matches!(
        record.status,
        KernelRunStatus::Failed | KernelRunStatus::Canceled
    ) {
        most_active_embedded_child(store, &record.run_id)?
    } else {
        None
    };
    let local_current_step_id = (!record.status.is_terminal())
        .then(|| current_step_id(&record.steps))
        .flatten();
    let status = match active_child.as_ref() {
        Some(child)
            if matches!(
                record.status,
                KernelRunStatus::Failed | KernelRunStatus::Canceled
            ) =>
        {
            child.status.clone()
        }
        _ => record.status.as_str().to_string(),
    };
    Ok(EffectiveRunProjection {
        status,
        current_step_id: local_current_step_id
            .or_else(|| active_child.as_ref().map(|child| child.step_id.clone())),
    })
}

fn effective_run_projection_from_summaries(
    summaries_by_id: &BTreeMap<String, PersistedRunSummaryRecord>,
    children_by_parent: &BTreeMap<String, Vec<String>>,
    run_id: &str,
    cache: &mut BTreeMap<String, EffectiveRunProjection>,
) -> EffectiveRunProjection {
    if let Some(cached) = cache.get(run_id) {
        return cached.clone();
    }

    let Some(summary) = summaries_by_id.get(run_id) else {
        let fallback = EffectiveRunProjection {
            status: "created".to_string(),
            current_step_id: None,
        };
        cache.insert(run_id.to_string(), fallback.clone());
        return fallback;
    };

    let active_child = if matches!(summary.status.as_str(), "failed" | "canceled") {
        most_active_embedded_child_from_summaries(
            summaries_by_id,
            children_by_parent,
            run_id,
            cache,
        )
    } else {
        None
    };
    let local_current_step_id = (!is_terminal_run_status(&summary.status))
        .then(|| summary.current_step_id.clone())
        .flatten();
    let projection = EffectiveRunProjection {
        status: match active_child.as_ref() {
            Some(child) if matches!(summary.status.as_str(), "failed" | "canceled") => {
                child.status.clone()
            }
            _ => summary.status.clone(),
        },
        current_step_id: local_current_step_id
            .or_else(|| active_child.as_ref().map(|child| child.step_id.clone())),
    };
    cache.insert(run_id.to_string(), projection.clone());
    projection
}

pub(crate) fn active_child_status_for_step(
    store: &LocalInspectionStore,
    run_id: &str,
    step_id: &str,
    parent_is_irreversibly_done: bool,
) -> Result<Option<String>> {
    // Success is irreversible; failed/canceled may later project active from
    // resumed children.
    if parent_is_irreversibly_done {
        return Ok(None);
    }
    let mut best = None;
    for child in store.list_run_records()? {
        let matches_parent = child
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.parent_run_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            == Some(run_id)
            && child
                .lineage
                .as_ref()
                .and_then(|lineage| lineage.parent_step_id.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                == Some(step_id);
        if !matches_parent {
            continue;
        }
        let effective = effective_run_projection(store, &child)?;
        if !is_active_run_status(&effective.status) {
            continue;
        }
        let candidate = ActiveChildProjection {
            step_id: step_id.to_string(),
            status: effective.status,
            updated_at_ms: child.updated_at_ms,
        };
        match &best {
            Some(current) if compare_child_activity(current, &candidate).is_ge() => {}
            _ => best = Some(candidate),
        }
    }
    Ok(best.map(|child| child.status))
}

pub(crate) fn is_terminal_run_status(status: &str) -> bool {
    matches!(status, "success" | "failed" | "canceled")
}

fn step_priority(status: KernelStepStatus) -> u8 {
    match status {
        KernelStepStatus::Running => 6,
        KernelStepStatus::Dispatching => 5,
        KernelStepStatus::Blocked => 4,
        KernelStepStatus::RetryWait => 3,
        KernelStepStatus::Runnable => 2,
        KernelStepStatus::Pending => 1,
        _ => 0,
    }
}

fn most_active_embedded_child(
    store: &LocalInspectionStore,
    run_id: &str,
) -> Result<Option<ActiveChildProjection>> {
    let mut best = None;
    for child in store.list_run_records()? {
        let Some(step_id) = parent_step_id_for_child(run_id, &child) else {
            continue;
        };
        let effective = effective_run_projection(store, &child)?;
        if !is_active_run_status(&effective.status) {
            continue;
        }
        let candidate = ActiveChildProjection {
            step_id,
            status: effective.status,
            updated_at_ms: child.updated_at_ms,
        };
        match &best {
            Some(current) if compare_child_activity(current, &candidate).is_ge() => {}
            _ => best = Some(candidate),
        }
    }
    Ok(best)
}

fn most_active_embedded_child_from_summaries(
    summaries_by_id: &BTreeMap<String, PersistedRunSummaryRecord>,
    children_by_parent: &BTreeMap<String, Vec<String>>,
    run_id: &str,
    cache: &mut BTreeMap<String, EffectiveRunProjection>,
) -> Option<ActiveChildProjection> {
    let mut best = None;
    let child_ids = children_by_parent.get(run_id)?;
    for child_id in child_ids {
        let Some(child) = summaries_by_id.get(child_id) else {
            continue;
        };
        let Some(step_id) = child
            .parent_step_id
            .clone()
            .or_else(|| direct_embedded_step_id(run_id, &child.run_id))
        else {
            continue;
        };
        let effective = effective_run_projection_from_summaries(
            summaries_by_id,
            children_by_parent,
            &child.run_id,
            cache,
        );
        if !is_active_run_status(&effective.status) {
            continue;
        }
        let candidate = ActiveChildProjection {
            step_id,
            status: effective.status,
            updated_at_ms: child.updated_at_ms,
        };
        match &best {
            Some(current) if compare_child_activity(current, &candidate).is_ge() => {}
            _ => best = Some(candidate),
        }
    }
    best
}

fn parent_step_id_for_child(parent_run_id: &str, child: &PersistedRunRecord) -> Option<String> {
    if let Some(lineage) = child.lineage.as_ref() {
        let matches_parent = lineage
            .parent_run_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            == Some(parent_run_id);
        if matches_parent {
            return lineage
                .parent_step_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
    }
    direct_embedded_step_id(parent_run_id, &child.run_id)
}

fn direct_embedded_step_id(run_id: &str, child_run_id: &str) -> Option<String> {
    let prefix = format!("{run_id}__");
    let remainder = child_run_id.strip_prefix(&prefix)?;
    (!remainder.is_empty() && !remainder.contains("__")).then_some(remainder.to_string())
}

fn run_has_blocked_gates_from_children(
    children_by_parent: &BTreeMap<String, Vec<String>>,
    local_has_blocked_gates_by_run: &BTreeMap<String, bool>,
    run_id: &str,
    cache: &mut BTreeMap<String, bool>,
) -> bool {
    if let Some(cached) = cache.get(run_id) {
        return *cached;
    }

    let has_blocked_gates = local_has_blocked_gates_by_run
        .get(run_id)
        .copied()
        .unwrap_or(false)
        || children_by_parent
            .get(run_id)
            .into_iter()
            .flatten()
            .any(|child_run_id| {
                run_has_blocked_gates_from_children(
                    children_by_parent,
                    local_has_blocked_gates_by_run,
                    child_run_id,
                    cache,
                )
            });
    cache.insert(run_id.to_string(), has_blocked_gates);
    has_blocked_gates
}

fn embedded_parent_run_id_for_record(
    store: &LocalInspectionStore,
    record: &PersistedRunRecord,
    root_run_id: &str,
) -> Option<String> {
    if let Some(parent_run_id) = record
        .lineage
        .as_ref()
        .and_then(|lineage| lineage.parent_run_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    {
        return store.run_exists(&parent_run_id).then_some(parent_run_id);
    }
    if record.run_id == root_run_id {
        return None;
    }
    let prefix = format!("{root_run_id}__");
    if !record.run_id.starts_with(&prefix) {
        return None;
    }
    let (parent_run_id, _) = record.run_id.rsplit_once("__")?;
    if parent_run_id.is_empty() || !store.run_exists(parent_run_id) {
        return None;
    }
    Some(parent_run_id.to_string())
}

fn compare_child_activity(
    left: &ActiveChildProjection,
    right: &ActiveChildProjection,
) -> std::cmp::Ordering {
    active_run_priority(left.status.as_str())
        .cmp(&active_run_priority(right.status.as_str()))
        .then(left.updated_at_ms.cmp(&right.updated_at_ms))
        .then(left.step_id.cmp(&right.step_id))
}

pub(crate) fn is_active_run_status(status: &str) -> bool {
    // Fresh or planned child runs can be historical byproducts of earlier
    // subflow executions, so only truly in-flight states should roll up into a
    // terminal parent's effective status.
    matches!(status, "running" | "waiting" | "blocked")
}

fn active_run_priority(status: &str) -> u8 {
    match status {
        "running" => 5,
        "waiting" => 4,
        "blocked" => 3,
        _ => 0,
    }
}

pub(crate) fn terminal_outcome(record: &PersistedRunRecord) -> Option<String> {
    if !record.status.is_terminal() {
        return None;
    }
    match record.status {
        KernelRunStatus::Success => Some("success".to_string()),
        KernelRunStatus::Canceled => Some("canceled".to_string()),
        KernelRunStatus::Failed => {
            let failed_step = record
                .steps
                .values()
                .find(|step| step.status == KernelStepStatus::Failed)
                .map(|step| step.step_id.clone());
            Some(match failed_step {
                Some(step_id) => format!("failed at step '{step_id}'"),
                None => "failed".to_string(),
            })
        }
        _ => Some(record.status.as_str().to_string()),
    }
}

pub(crate) fn count_step_statuses(steps: &[KernelStepState]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for step in steps {
        *counts
            .entry(step_status_name(step.status).to_string())
            .or_default() += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal [`PersistedRunSummaryRecord`] for projection tests. Only
    /// the fields read by `effective_run_projection_from_summaries` need to be
    /// realistic; the rest are defaulted.
    fn summary_for_test(
        run_id: &str,
        status: &str,
        parent_run_id: Option<&str>,
        current_step_id: Option<&str>,
    ) -> PersistedRunSummaryRecord {
        PersistedRunSummaryRecord {
            run_id: run_id.to_string(),
            workflow_id: "wf".to_string(),
            workflow_version: "v1".to_string(),
            status: status.to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
            current_step_id: current_step_id.map(str::to_string),
            has_blocked_gates: false,
            has_artifacts: false,
            has_events: false,
            artifact_count: 0,
            evidence_count: 0,
            event_count: 0,
            root_run_id: parent_run_id.unwrap_or(run_id).to_string(),
            parent_run_id: parent_run_id.map(str::to_string),
            parent_step_id: parent_run_id.map(|_| "plan_loop".to_string()),
            manual_gate_step_ids: Vec::new(),
        }
    }

    /// After `runs resume` against a
    /// canceled root, the resumed embedded child can be actively `blocked`
    /// on a manual gate while the kernel hasn't yet rewritten the root's
    /// `summary.json` status. The run-detail action picker must reflect
    /// what the operator can actually do (decide the gate), not the stale
    /// terminal status. So a `canceled` root with an active `blocked`
    /// child projects as `blocked`. Abandoned canceled trees (where no
    /// child is active) still surface as `canceled` because
    /// `most_active_embedded_child` only matches running/waiting/blocked.
    #[test]
    fn canceled_root_inherits_active_blocked_child_status() {
        let root_id = "root";
        let child_id = "root__plan_loop";
        let summaries = BTreeMap::from([
            (
                root_id.to_string(),
                summary_for_test(root_id, "canceled", None, None),
            ),
            (
                child_id.to_string(),
                summary_for_test(child_id, "blocked", Some(root_id), Some("review_plan")),
            ),
        ]);
        let children = BTreeMap::from([(root_id.to_string(), vec![child_id.to_string()])]);
        let mut cache = BTreeMap::new();

        let projection =
            effective_run_projection_from_summaries(&summaries, &children, root_id, &mut cache);

        assert_eq!(
            projection.status, "blocked",
            "canceled root must project active descendant status after resume"
        );
        assert_eq!(
            projection.current_step_id.as_deref(),
            Some("plan_loop"),
            "descendant projection points current_step_id at the parent step \
             that owns the child run"
        );
    }

    /// Abandoned canceled trees (no active descendant) still surface as
    /// `canceled`. The descendant filter (`is_active_run_status`) only
    /// matches `running`/`waiting`/`blocked`; a canceled root whose
    /// children are all terminal stays canceled.
    #[test]
    fn canceled_root_with_terminal_children_stays_canceled() {
        let root_id = "root";
        let child_id = "root__plan_loop";
        let summaries = BTreeMap::from([
            (
                root_id.to_string(),
                summary_for_test(root_id, "canceled", None, None),
            ),
            (
                child_id.to_string(),
                summary_for_test(child_id, "canceled", Some(root_id), None),
            ),
        ]);
        let children = BTreeMap::from([(root_id.to_string(), vec![child_id.to_string()])]);
        let mut cache = BTreeMap::new();

        let projection =
            effective_run_projection_from_summaries(&summaries, &children, root_id, &mut cache);

        assert_eq!(projection.status, "canceled");
    }

    /// Sibling check: a `success` root continues to win over any descendant
    /// (success is irreversible). This was already the behaviour and remains
    /// correct after the fix.
    #[test]
    fn success_root_does_not_inherit_blocked_child_status() {
        let root_id = "root";
        let child_id = "root__plan_loop";
        let summaries = BTreeMap::from([
            (
                root_id.to_string(),
                summary_for_test(root_id, "success", None, None),
            ),
            (
                child_id.to_string(),
                summary_for_test(child_id, "blocked", Some(root_id), Some("review_plan")),
            ),
        ]);
        let children = BTreeMap::from([(root_id.to_string(), vec![child_id.to_string()])]);
        let mut cache = BTreeMap::new();

        let projection =
            effective_run_projection_from_summaries(&summaries, &children, root_id, &mut cache);

        assert_eq!(projection.status, "success");
    }

    /// `failed` is the one terminal state that still opts into descendant
    /// projection: a resumed child may legitimately surface a fresh
    /// actionable status (e.g. `blocked` on a manual gate) the operator can
    /// resolve. This preserves the existing resume-after-failure flow.
    #[test]
    fn failed_root_still_projects_blocked_child_status() {
        let root_id = "root";
        let child_id = "root__plan_loop";
        let summaries = BTreeMap::from([
            (
                root_id.to_string(),
                summary_for_test(root_id, "failed", None, None),
            ),
            (
                child_id.to_string(),
                summary_for_test(child_id, "blocked", Some(root_id), Some("review_plan")),
            ),
        ]);
        let children = BTreeMap::from([(root_id.to_string(), vec![child_id.to_string()])]);
        let mut cache = BTreeMap::new();

        let projection =
            effective_run_projection_from_summaries(&summaries, &children, root_id, &mut cache);

        assert_eq!(
            projection.status, "blocked",
            "failed root must still project descendant actionable status"
        );
        assert_eq!(
            projection.current_step_id.as_deref(),
            Some("plan_loop"),
            "descendant projection points current_step_id at the parent step \
             that owns the child run, not the child's own current step"
        );
    }

    /// Non-terminal roots (`running`, `blocked`, etc.) always keep their own
    /// status — the descendant override only applies to terminal roots.
    #[test]
    fn running_root_keeps_own_status_regardless_of_child() {
        let root_id = "root";
        let child_id = "root__plan_loop";
        let summaries = BTreeMap::from([
            (
                root_id.to_string(),
                summary_for_test(root_id, "running", None, Some("plan_loop")),
            ),
            (
                child_id.to_string(),
                summary_for_test(child_id, "blocked", Some(root_id), Some("review_plan")),
            ),
        ]);
        let children = BTreeMap::from([(root_id.to_string(), vec![child_id.to_string()])]);
        let mut cache = BTreeMap::new();

        let projection =
            effective_run_projection_from_summaries(&summaries, &children, root_id, &mut cache);

        assert_eq!(projection.status, "running");
        assert_eq!(projection.current_step_id.as_deref(), Some("plan_loop"));
    }
}
