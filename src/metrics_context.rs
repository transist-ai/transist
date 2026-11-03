// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::application::metrics::{
    LocalMetricsQuery, MetricsQueryPort, MetricsSnapshot, RunListMetricsSummary, RunMetricsSummary,
    StepMetricsSummary,
};

/// Root-facade access to the local metrics query surface.
#[derive(Clone)]
pub struct TransistMetricsContext {
    query: Arc<LocalMetricsQuery>,
}

impl TransistMetricsContext {
    /// Creates a metrics facade for an explicit Transist home directory.
    pub fn with_home(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            query: Arc::new(LocalMetricsQuery::new(home_dir)),
        }
    }

    /// Returns the complete metrics snapshot.
    pub fn snapshot(&self) -> Result<MetricsSnapshot> {
        self.query.snapshot()
    }

    /// Returns metrics for one run when available.
    pub fn run(&self, run_id: &str) -> Result<Option<RunMetricsSummary>> {
        self.query.get_run_metrics(run_id)
    }

    /// Returns metrics for one step when available.
    pub fn step(&self, run_id: &str, step_id: &str) -> Result<Option<StepMetricsSummary>> {
        self.query.get_step_metrics(run_id, step_id)
    }

    /// Returns the compact metrics rows used by run-list views.
    pub fn run_list(&self) -> Result<BTreeMap<String, RunListMetricsSummary>> {
        self.query.run_list_metrics()
    }

    /// Renders the current metrics in Prometheus exposition format.
    pub fn render_prometheus(&self) -> Result<String> {
        self.query.render_prometheus()
    }

    /// Refreshes metrics for runs whose persisted data changed.
    pub fn refresh_changed_runs(&self) -> Result<()> {
        self.query.refresh_changed_runs()
    }

    /// Increments a process-local global metrics counter.
    pub fn increment_global_counter(&self, metric_name: &str) -> Result<()> {
        self.query.increment_global_counter(metric_name)
    }

    pub(crate) fn query_port(&self) -> &dyn MetricsQueryPort {
        self.query.as_ref()
    }
}

impl MetricsQueryPort for TransistMetricsContext {
    fn snapshot(&self) -> Result<MetricsSnapshot> {
        self.query.snapshot()
    }

    fn get_run_metrics(&self, run_id: &str) -> Result<Option<RunMetricsSummary>> {
        self.query.get_run_metrics(run_id)
    }

    fn get_step_metrics(&self, run_id: &str, step_id: &str) -> Result<Option<StepMetricsSummary>> {
        self.query.get_step_metrics(run_id, step_id)
    }

    fn run_list_metrics(&self) -> Result<BTreeMap<String, RunListMetricsSummary>> {
        self.query.run_list_metrics()
    }

    fn render_prometheus(&self) -> Result<String> {
        self.query.render_prometheus()
    }
}
