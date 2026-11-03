// SPDX-License-Identifier: MPL-2.0
//! Cached metrics-index state and aggregation primitives.
//!
//! The index stores **self-only** contributions per run. Aggregated ("subtree-inclusive")
//! summaries are produced exclusively by [`MetricsIndex::fold`], a pure function that
//! returns a fresh [`FoldedMetrics`] view without ever mutating the stored contributions.
//!
//! This separation is deliberate: it removes the "self vs folded" aliasing that previously
//! allowed repeated fold passes to inflate totals, and it makes aggregation idempotent by
//! construction regardless of how many times refresh paths run.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SampleKey {
    pub(crate) name: String,
    pub(crate) labels: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RunFingerprint {
    pub(crate) files: Vec<(String, u64, u64)>,
}

/// Self-only metrics contribution for a single run.
///
/// The `run_list_summary`, `run_summary`, and `step_summaries` fields hold values
/// derived from this run's own persisted state only — they never include totals from
/// child runs. Subtree aggregation is performed on demand by [`MetricsIndex::fold`].
#[derive(Debug, Clone)]
pub(crate) struct RunContribution {
    pub(crate) fingerprint: Option<RunFingerprint>,
    pub(crate) parent_run_id: Option<String>,
    pub(crate) parent_step_id: Option<String>,
    pub(crate) run_list_summary: RunListMetricsSummary,
    pub(crate) run_summary: RunMetricsSummary,
    pub(crate) step_summaries: BTreeMap<String, StepMetricsSummary>,
    pub(crate) samples: BTreeMap<SampleKey, f64>,
}

/// Subtree-folded view over the full run index.
///
/// Every field is freshly computed from the current self-only contributions — the map
/// values are safe to clone, expose, or discard without affecting stored state.
#[derive(Debug, Default)]
pub(crate) struct FoldedMetrics {
    pub(crate) run_list: BTreeMap<String, RunListMetricsSummary>,
    pub(crate) runs: BTreeMap<String, RunMetricsSummary>,
    pub(crate) steps: BTreeMap<String, BTreeMap<String, StepMetricsSummary>>,
}

impl FoldedMetrics {
    pub(crate) fn into_snapshot(self) -> MetricsSnapshot {
        MetricsSnapshot {
            run_list: self.run_list,
            runs: self.runs,
            steps: self.steps,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct MetricsIndex {
    pub(crate) runs: BTreeMap<String, RunContribution>,
    pub(crate) samples: BTreeMap<SampleKey, f64>,
}

impl MetricsIndex {
    pub(crate) fn insert_run(&mut self, contribution: RunContribution) {
        let run_id = contribution.run_summary.run_id.clone();
        if let Some(previous) = self.runs.remove(&run_id) {
            self.merge_samples(&previous.samples, -1.0);
        }
        self.merge_samples(&contribution.samples, 1.0);
        self.runs.insert(run_id, contribution);
    }

    pub(crate) fn remove_run(&mut self, run_id: &str) {
        if let Some(previous) = self.runs.remove(run_id) {
            self.merge_samples(&previous.samples, -1.0);
        }
    }

    /// Produces a fresh subtree-folded view over the current self-only contributions.
    ///
    /// The fold walks subflow edges (parent step → child run) in post-order so each
    /// child's folded totals are known before its parent is processed. For every
    /// subflow edge, the child's already-folded run summary is added into the parent
    /// run summary *and* the specific subflow step summary. Orphan child runs (missing
    /// from the index) contribute nothing; cycles are ignored.
    ///
    /// This function is pure: it allocates new summary maps and never writes back into
    /// [`RunContribution`]. Calling it any number of times yields identical output.
    pub(crate) fn fold(&self) -> FoldedMetrics {
        let mut run_list: BTreeMap<String, RunListMetricsSummary> = self
            .runs
            .iter()
            .map(|(run_id, contribution)| (run_id.clone(), contribution.run_list_summary.clone()))
            .collect();
        let mut runs: BTreeMap<String, RunMetricsSummary> = self
            .runs
            .iter()
            .map(|(run_id, contribution)| (run_id.clone(), contribution.run_summary.clone()))
            .collect();
        let mut steps: BTreeMap<String, BTreeMap<String, StepMetricsSummary>> = self
            .runs
            .iter()
            .map(|(run_id, contribution)| (run_id.clone(), contribution.step_summaries.clone()))
            .collect();

        if self.runs.is_empty() {
            return FoldedMetrics {
                run_list,
                runs,
                steps,
            };
        }

        let mut children: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        let mut linked_children: BTreeSet<String> = BTreeSet::new();
        for (run_id, contribution) in self.runs.iter() {
            let Some(parent_run_id) = contribution.parent_run_id.as_ref() else {
                continue;
            };
            let Some(parent_step_id) = contribution.parent_step_id.as_ref() else {
                continue;
            };
            if !self.runs.contains_key(parent_run_id) {
                continue;
            }
            children
                .entry(parent_run_id.clone())
                .or_default()
                .push((parent_step_id.clone(), run_id.clone()));
            linked_children.insert(run_id.clone());
        }
        for (run_id, contribution) in self.runs.iter() {
            for (step_id, step) in contribution.step_summaries.iter() {
                if let Some(child_run_id) = step.child_run_id.as_ref() {
                    if self.runs.contains_key(child_run_id)
                        && linked_children.insert(child_run_id.clone())
                    {
                        children
                            .entry(run_id.clone())
                            .or_default()
                            .push((step_id.clone(), child_run_id.clone()));
                    }
                }
            }
        }

        let run_ids: Vec<String> = self.runs.keys().cloned().collect();
        let mut order: Vec<String> = Vec::with_capacity(run_ids.len());
        let mut visiting: BTreeSet<String> = BTreeSet::new();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        for run_id in &run_ids {
            visit_postorder(run_id, &children, &mut visiting, &mut visited, &mut order);
        }

        for run_id in order {
            let Some(edges) = children.get(&run_id) else {
                continue;
            };
            if edges.is_empty() {
                continue;
            }
            let edges = edges.clone();

            let children_snapshot: Vec<(String, SubtreeAccumulator)> = edges
                .iter()
                .filter_map(|(step_id, child_run_id)| {
                    runs.get(child_run_id).map(|folded_child| {
                        (step_id.clone(), SubtreeAccumulator::from_run(folded_child))
                    })
                })
                .collect();

            let (Some(parent_run), Some(parent_list), Some(parent_steps)) = (
                runs.get_mut(&run_id),
                run_list.get_mut(&run_id),
                steps.get_mut(&run_id),
            ) else {
                continue;
            };

            for (step_id, child_acc) in &children_snapshot {
                if let Some(step) = parent_steps.get_mut(step_id) {
                    child_acc.merge_into_step(step);
                }
                child_acc.merge_into_run(parent_run, parent_list);
            }
        }

        FoldedMetrics {
            run_list,
            runs,
            steps,
        }
    }

    pub(crate) fn increment_global_counter(&mut self, metric_name: &str) {
        *self
            .samples
            .entry(SampleKey {
                name: metric_name.to_string(),
                labels: Vec::new(),
            })
            .or_insert(0.0) += 1.0;
    }

    fn merge_samples(&mut self, samples: &BTreeMap<SampleKey, f64>, direction: f64) {
        for (key, value) in samples {
            let next = self.samples.entry(key.clone()).or_insert(0.0);
            *next += value * direction;
            if next.abs() < 1e-9 {
                self.samples.remove(key);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StepDescriptor {
    pub(crate) step_kind: String,
    pub(crate) adapter_kind: Option<String>,
    pub(crate) action: Option<String>,
    pub(crate) is_manual_gate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UsageKey {
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct UsageAccumulator {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) reports: u64,
}

impl UsageAccumulator {
    pub(crate) fn add(&mut self, result: &ExecutionResult) {
        let Some(usage) = result.usage.as_ref() else {
            return;
        };
        self.reports = self.reports.saturating_add(1);
        add_option_u64(&mut self.input_tokens, usage.input_tokens);
        add_option_u64(&mut self.output_tokens, usage.output_tokens);
        add_option_u64(&mut self.total_tokens, usage.total_tokens);
    }

    pub(crate) fn into_entry(self, key: UsageKey) -> UsageBreakdownEntry {
        UsageBreakdownEntry {
            provider: key.provider,
            model: key.model,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.total_tokens,
            usage_reports: self.reports,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct BlobExecutionCounts {
    pub(crate) artifacts: usize,
    pub(crate) evidence: usize,
}

fn add_option_u64(target: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or_default().saturating_add(value));
    }
}

fn merge_accumulator(target: &mut UsageAccumulator, source: &UsageAccumulator) {
    target.reports = target.reports.saturating_add(source.reports);
    add_option_u64(&mut target.input_tokens, source.input_tokens);
    add_option_u64(&mut target.output_tokens, source.output_tokens);
    add_option_u64(&mut target.total_tokens, source.total_tokens);
}

fn merge_breakdown_entries(
    target: &mut BTreeMap<UsageKey, UsageAccumulator>,
    entries: &[UsageBreakdownEntry],
) {
    for entry in entries {
        let key = UsageKey {
            provider: entry.provider.clone(),
            model: entry.model.clone(),
        };
        let slot = target.entry(key).or_default();
        slot.reports = slot.reports.saturating_add(entry.usage_reports);
        add_option_u64(&mut slot.input_tokens, entry.input_tokens);
        add_option_u64(&mut slot.output_tokens, entry.output_tokens);
        add_option_u64(&mut slot.total_tokens, entry.total_tokens);
    }
}

fn breakdown_to_entries(
    breakdown: BTreeMap<UsageKey, UsageAccumulator>,
) -> Vec<UsageBreakdownEntry> {
    breakdown
        .into_iter()
        .map(|(key, acc)| acc.into_entry(key))
        .collect()
}

fn visit_postorder(
    run_id: &str,
    children: &BTreeMap<String, Vec<(String, String)>>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    order: &mut Vec<String>,
) {
    if visited.contains(run_id) || visiting.contains(run_id) {
        return;
    }
    visiting.insert(run_id.to_string());
    if let Some(edges) = children.get(run_id) {
        for (_step_id, child_run_id) in edges {
            visit_postorder(child_run_id, children, visiting, visited, order);
        }
    }
    visiting.remove(run_id);
    visited.insert(run_id.to_string());
    order.push(run_id.to_string());
}

/// Snapshot of a child run's subtree totals that a parent fold step merges in.
#[derive(Debug, Clone, Default)]
struct SubtreeAccumulator {
    usage: UsageAccumulator,
    breakdown: Vec<UsageBreakdownEntry>,
    execution_count: u32,
    succeeded_step_executions: u32,
    failed_step_executions: u32,
    cancelled_step_executions: u32,
    artifacts: usize,
    evidence: usize,
    blocked_gates: usize,
    active_steps: usize,
    active_execution_duration_ms: u64,
    blocked_wait_duration_ms: u64,
    status_counts: BTreeMap<String, u32>,
}

impl SubtreeAccumulator {
    fn from_run(run: &RunMetricsSummary) -> Self {
        let usage = UsageAccumulator {
            input_tokens: run.input_tokens,
            output_tokens: run.output_tokens,
            total_tokens: run.total_tokens,
            reports: run.usage_breakdown.iter().map(|e| e.usage_reports).sum(),
        };
        let mut status_counts: BTreeMap<String, u32> = BTreeMap::new();
        if run.succeeded_step_executions > 0 {
            status_counts.insert("succeeded".to_string(), run.succeeded_step_executions);
        }
        if run.failed_step_executions > 0 {
            status_counts.insert("failed".to_string(), run.failed_step_executions);
        }
        if run.cancelled_step_executions > 0 {
            status_counts.insert("cancelled".to_string(), run.cancelled_step_executions);
        }
        Self {
            usage,
            breakdown: run.usage_breakdown.clone(),
            execution_count: run.total_step_executions,
            succeeded_step_executions: run.succeeded_step_executions,
            failed_step_executions: run.failed_step_executions,
            cancelled_step_executions: run.cancelled_step_executions,
            artifacts: run.total_artifacts_produced,
            evidence: run.total_evidence_files_produced,
            blocked_gates: run.blocked_gate_count,
            active_steps: run.current_active_step_count,
            active_execution_duration_ms: run.active_execution_duration_ms.unwrap_or(0),
            blocked_wait_duration_ms: run.blocked_wait_duration_ms.unwrap_or(0),
            status_counts,
        }
    }

    fn merge_into_run(&self, run: &mut RunMetricsSummary, list: &mut RunListMetricsSummary) {
        let mut acc = UsageAccumulator {
            input_tokens: run.input_tokens,
            output_tokens: run.output_tokens,
            total_tokens: run.total_tokens,
            reports: run.usage_breakdown.iter().map(|e| e.usage_reports).sum(),
        };
        merge_accumulator(&mut acc, &self.usage);
        run.input_tokens = acc.input_tokens;
        run.output_tokens = acc.output_tokens;
        run.total_tokens = acc.total_tokens;

        let mut breakdown: BTreeMap<UsageKey, UsageAccumulator> = BTreeMap::new();
        merge_breakdown_entries(&mut breakdown, &run.usage_breakdown);
        merge_breakdown_entries(&mut breakdown, &self.breakdown);
        run.usage_breakdown = breakdown_to_entries(breakdown);

        run.total_step_executions = run
            .total_step_executions
            .saturating_add(self.execution_count);
        run.succeeded_step_executions = run
            .succeeded_step_executions
            .saturating_add(self.succeeded_step_executions);
        run.failed_step_executions = run
            .failed_step_executions
            .saturating_add(self.failed_step_executions);
        run.cancelled_step_executions = run
            .cancelled_step_executions
            .saturating_add(self.cancelled_step_executions);
        run.total_artifacts_produced = run.total_artifacts_produced.saturating_add(self.artifacts);
        run.total_evidence_files_produced = run
            .total_evidence_files_produced
            .saturating_add(self.evidence);
        run.blocked_gate_count = run.blocked_gate_count.saturating_add(self.blocked_gates);
        run.current_active_step_count = run
            .current_active_step_count
            .saturating_add(self.active_steps);
        run.active_execution_duration_ms = Some(
            run.active_execution_duration_ms
                .unwrap_or(0)
                .saturating_add(self.active_execution_duration_ms),
        )
        .filter(|duration| *duration > 0);
        run.blocked_wait_duration_ms = Some(
            run.blocked_wait_duration_ms
                .unwrap_or(0)
                .saturating_add(self.blocked_wait_duration_ms),
        )
        .filter(|duration| *duration > 0);

        list.total_tokens = run.total_tokens;
        list.total_step_executions = run.total_step_executions;
        list.total_artifacts_produced = run.total_artifacts_produced;
        list.total_evidence_files_produced = run.total_evidence_files_produced;
        list.active_execution_duration_ms = run.active_execution_duration_ms;
        list.blocked_wait_duration_ms = run.blocked_wait_duration_ms;
    }

    fn merge_into_step(&self, step: &mut StepMetricsSummary) {
        let mut acc = UsageAccumulator {
            input_tokens: step.input_tokens,
            output_tokens: step.output_tokens,
            total_tokens: step.total_tokens,
            reports: step.usage_breakdown.iter().map(|e| e.usage_reports).sum(),
        };
        merge_accumulator(&mut acc, &self.usage);
        step.input_tokens = acc.input_tokens;
        step.output_tokens = acc.output_tokens;
        step.total_tokens = acc.total_tokens;

        let mut breakdown: BTreeMap<UsageKey, UsageAccumulator> = BTreeMap::new();
        merge_breakdown_entries(&mut breakdown, &step.usage_breakdown);
        merge_breakdown_entries(&mut breakdown, &self.breakdown);
        step.usage_breakdown = breakdown_to_entries(breakdown);

        step.execution_count = step.execution_count.saturating_add(self.execution_count);
        step.artifacts_produced = step.artifacts_produced.saturating_add(self.artifacts);
        step.evidence_files_produced = step.evidence_files_produced.saturating_add(self.evidence);
        step.active_execution_duration_ms = Some(
            step.active_execution_duration_ms
                .unwrap_or(0)
                .saturating_add(self.active_execution_duration_ms),
        )
        .filter(|duration| *duration > 0);
        step.blocked_wait_duration_ms = Some(
            step.blocked_wait_duration_ms
                .unwrap_or(0)
                .saturating_add(self.blocked_wait_duration_ms),
        )
        .filter(|duration| *duration > 0);

        for (status, count) in &self.status_counts {
            let slot = step.status_counts.entry(status.clone()).or_insert(0);
            *slot = slot.saturating_add(*count);
        }
    }
}
