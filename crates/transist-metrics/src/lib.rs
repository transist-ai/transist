// SPDX-License-Identifier: MPL-2.0
#![deny(missing_docs)]
//! Durable and live metrics rebuild and query support.
//!
//! Metrics in Transist are derived from persisted run state, append-only step executions,
//! gate decisions, and adapter-reported usage/metrics. This module maintains a cached
//! per-home metrics index that can be rebuilt at startup, refreshed per-run after
//! durable writes, and rendered for HTTP or Prometheus consumers without rescanning
//! the full runs tree on every request.

mod fingerprints;
mod index;
mod models;
mod prometheus;
mod rebuild;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::UNIX_EPOCH;
use transist_core::{KernelRunStatus, KernelStepStatus, PersistedRunRecord, StepExecutionRecord};
use transist_protocol::{ExecutionRequest, ExecutionResult, Recipe, RecipeStep};
use transist_storage_local::{BlobFamily, LocalInspectionStore};

use self::fingerprints::{list_run_ids, run_fingerprint, run_root};
use self::index::MetricsIndex;
pub use self::models::{
    MetricsQueryPort, MetricsSnapshot, RunListMetricsSummary, RunMetricsSummary,
    StepExecutionMetricsSummary, StepMetricsSummary, UsageBreakdownEntry,
};
use self::prometheus::render_prometheus;
use self::rebuild::build_run_contribution;

const RUN_DURATION_BUCKETS_MS: &[u64] = &[
    10, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000, 300_000, 900_000, 3_600_000,
];
const STEP_DURATION_BUCKETS_MS: &[u64] = &[
    1, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000, 300_000,
];
const GATE_WAIT_BUCKETS_MS: &[u64] = &[
    1_000, 5_000, 10_000, 30_000, 60_000, 300_000, 900_000, 3_600_000, 14_400_000, 86_400_000,
];

/// Local filesystem-backed metrics query service with an in-memory rebuild index.
#[derive(Debug)]
pub struct LocalMetricsQuery {
    home_dir: PathBuf,
    index: RwLock<MetricsIndex>,
}

impl LocalMetricsQuery {
    /// Creates a local metrics query service and eagerly rebuilds its in-memory index.
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        let home_dir = home_dir.into();
        let query = Self {
            home_dir,
            index: RwLock::new(MetricsIndex::default()),
        };
        if let Err(error) = query.refresh_all() {
            tracing::error!(error = %error, "initial metrics rebuild failed");
        }
        query
    }

    /// Rebuilds the entire metrics index from persisted runs under the current home directory.
    pub fn refresh_all(&self) -> Result<()> {
        let run_ids = list_run_ids(&self.home_dir)?;
        let mut next = MetricsIndex::default();
        for run_id in run_ids {
            let fingerprint = run_fingerprint(&self.home_dir, &run_id)?;
            match build_run_contribution(&self.home_dir, &run_id, fingerprint.clone()) {
                Ok(Some(contribution)) => next.insert_run(contribution),
                Ok(None) => {}
                Err(error) => {
                    let path = run_root(&self.home_dir, &run_id)
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| format!("<unresolved:{}>", run_id));
                    tracing::error!(
                        run_id,
                        path = %path,
                        error = %error,
                        "metrics rebuild skipped corrupt or partial run"
                    );
                    next.increment_global_counter("transist_metrics_rebuild_errors_total");
                }
            }
        }
        *self
            .index
            .write()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))? = next;
        Ok(())
    }

    /// Rebuilds metrics for one run and replaces its cached contribution.
    pub fn refresh_run(&self, run_id: &str) -> Result<()> {
        let fingerprint = run_fingerprint(&self.home_dir, run_id)?;
        let mut guard = self
            .index
            .write()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
        match build_run_contribution(&self.home_dir, run_id, fingerprint)? {
            Some(contribution) => guard.insert_run(contribution),
            None => guard.remove_run(run_id),
        }
        Ok(())
    }

    /// Rebuilds metrics only for runs whose persisted fingerprint changed.
    pub fn refresh_changed_runs(&self) -> Result<()> {
        let run_ids = list_run_ids(&self.home_dir)?;
        let fingerprints = run_ids
            .iter()
            .map(|run_id| {
                run_fingerprint(&self.home_dir, run_id)
                    .map(|fingerprint| (run_id.clone(), fingerprint))
            })
            .collect::<Result<Vec<_>>>()?;
        let existing = {
            let guard = self
                .index
                .read()
                .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
            guard
                .runs
                .iter()
                .map(|(run_id, contribution)| (run_id.clone(), contribution.fingerprint.clone()))
                .collect::<BTreeMap<_, _>>()
        };

        let current_ids = fingerprints
            .iter()
            .map(|(run_id, _)| run_id.clone())
            .collect::<BTreeSet<_>>();

        for (run_id, fingerprint) in &fingerprints {
            if existing.get(run_id) != Some(fingerprint) {
                if let Err(error) = self.refresh_run(run_id) {
                    let path = run_root(&self.home_dir, run_id)
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| format!("<unresolved:{}>", run_id));
                    tracing::error!(
                        run_id,
                        path = %path,
                        error = %error,
                        "incremental metrics refresh failed"
                    );
                    self.increment_global_counter("transist_metrics_rebuild_errors_total")?;
                }
            }
        }

        let stale_ids = existing
            .keys()
            .filter(|run_id| !current_ids.contains(*run_id))
            .cloned()
            .collect::<Vec<_>>();
        if !stale_ids.is_empty() {
            let mut guard = self
                .index
                .write()
                .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
            for run_id in stale_ids {
                guard.remove_run(&run_id);
            }
        }

        Ok(())
    }

    /// Increments one global metrics counter inside the in-memory index.
    pub fn increment_global_counter(&self, metric_name: &str) -> Result<()> {
        let mut guard = self
            .index
            .write()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
        guard.increment_global_counter(metric_name);
        Ok(())
    }
}

impl MetricsQueryPort for LocalMetricsQuery {
    fn snapshot(&self) -> Result<MetricsSnapshot> {
        let guard = self
            .index
            .read()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
        Ok(guard.fold().into_snapshot())
    }

    fn get_run_metrics(&self, run_id: &str) -> Result<Option<RunMetricsSummary>> {
        let guard = self
            .index
            .read()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
        Ok(guard.fold().runs.get(run_id).cloned())
    }

    fn get_step_metrics(&self, run_id: &str, step_id: &str) -> Result<Option<StepMetricsSummary>> {
        let guard = self
            .index
            .read()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
        Ok(guard
            .fold()
            .steps
            .get(run_id)
            .and_then(|steps| steps.get(step_id).cloned()))
    }

    fn run_list_metrics(&self) -> Result<BTreeMap<String, RunListMetricsSummary>> {
        let guard = self
            .index
            .read()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
        Ok(guard.fold().run_list)
    }

    fn render_prometheus(&self) -> Result<String> {
        let guard = self
            .index
            .read()
            .map_err(|_| anyhow::anyhow!("metrics index lock poisoned"))?;
        render_prometheus(&guard.samples)
    }
}

#[cfg(test)]
mod tests;
