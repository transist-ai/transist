// SPDX-License-Identifier: MPL-2.0
//! Empty metrics query port implementation used by router constructors.

/// A metrics query implementation that always returns empty responses.
#[derive(Debug)]
pub struct EmptyMetricsQuery;

impl transist::MetricsQueryPort for EmptyMetricsQuery {
    fn snapshot(&self) -> anyhow::Result<transist::MetricsSnapshot> {
        Ok(transist::MetricsSnapshot::default())
    }

    fn get_run_metrics(
        &self,
        _run_id: &str,
    ) -> anyhow::Result<Option<transist::RunMetricsSummary>> {
        Ok(None)
    }

    fn get_step_metrics(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> anyhow::Result<Option<transist::StepMetricsSummary>> {
        Ok(None)
    }

    fn run_list_metrics(
        &self,
    ) -> anyhow::Result<std::collections::BTreeMap<String, transist::RunListMetricsSummary>> {
        Ok(std::collections::BTreeMap::new())
    }

    fn render_prometheus(&self) -> anyhow::Result<String> {
        Ok(String::new())
    }
}
