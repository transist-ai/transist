// SPDX-License-Identifier: MPL-2.0
use crate::application::metrics::LocalMetricsQuery;
use std::sync::Arc;
use transist_core::ports::{EventObserver, RunId, RunWriteObserver};
use transist_protocol::RuntimeEvent;
use transist_storage_local::LocalRunSummaryStore;
/// Refreshes persisted run summaries after writes and events.
pub struct SummaryObserver {
    summaries: LocalRunSummaryStore,
}
impl SummaryObserver {
    /// Creates a summary observer.
    pub fn new(summaries: LocalRunSummaryStore) -> Self {
        Self { summaries }
    }
}
impl RunWriteObserver for SummaryObserver {
    fn on_run_write(&self, run_id: &RunId) {
        if let Err(error) = self.summaries.refresh_run(run_id) {
            tracing::error!(run_id, error = %error, "summary refresh failed after durable write");
        }
    }
}
impl EventObserver for SummaryObserver {
    fn on_event(&self, event: &RuntimeEvent) {
        if let Err(error) = self.summaries.note_event(event) {
            tracing::error!(
                run_id = %event.run_id,
                kind = %event.kind,
                error = %error,
                "summary event update failed after publish"
            );
        }
    }
}
/// Refreshes metrics indexes after writes and events.
pub struct MetricsObserver {
    metrics: Arc<LocalMetricsQuery>,
}
impl MetricsObserver {
    /// Creates a metrics observer.
    pub fn new(metrics: Arc<LocalMetricsQuery>) -> Self {
        Self { metrics }
    }
}
impl RunWriteObserver for MetricsObserver {
    fn on_run_write(&self, run_id: &RunId) {
        if let Err(error) = self.metrics.refresh_run(run_id) {
            tracing::error!(run_id, error = %error, "metrics refresh failed after durable write");
            let _ = self
                .metrics
                .increment_global_counter("transist_metrics_export_errors_total");
        }
    }
}

impl EventObserver for MetricsObserver {
    fn on_event(&self, event: &RuntimeEvent) {
        self.on_run_write(&event.run_id);
    }
}
