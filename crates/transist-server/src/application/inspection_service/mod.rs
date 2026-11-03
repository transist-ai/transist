// SPDX-License-Identifier: MPL-2.0
//! Inspection application services and HTTP-facing mapping helpers.

mod content;
mod empty_metrics_query;
mod mapping;
mod outputs;
mod pagination;
mod resource_ids;
mod service;
mod time;

pub use content::{BlobContentResponse, InlineContentResponse};
pub use empty_metrics_query::EmptyMetricsQuery;
pub use outputs::OutputExecution;
pub(crate) use resource_ids::{parse_artifact_id, parse_evidence_id, parse_workflow_file_id};
pub use service::{ApiProblem, InspectionApiService, MetricsApiService};
pub(crate) use time::{format_timestamp, parse_rfc3339_to_ms};

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::io;
    use std::sync::{Arc, Mutex};
    use transist::{
        BlobFamily, BlobListScope, EventListQuery, InspectionBlob, InspectionEvent, InspectionGate,
        InspectionQueryPort, InspectionRunDetail, InspectionRunSummary, InspectionStep,
        InspectionStepExecution, InspectionWorkflowContent, InspectionWorkflowDetail,
        InspectionWorkflowSummary, Page, PaginationQuery, RunListQuery,
    };

    #[derive(Default)]
    struct FailingQuery;

    impl InspectionQueryPort for FailingQuery {
        fn list_runs(&self, _query: &RunListQuery) -> Result<Page<InspectionRunSummary>> {
            Err(anyhow::anyhow!(
                "failed reading /tmp/hidden/state.json: invalid serde payload"
            ))
        }

        fn get_run(&self, _run_id: &str) -> Result<Option<InspectionRunDetail>> {
            unreachable!()
        }

        fn list_events(
            &self,
            _run_id: &str,
            _query: &EventListQuery,
        ) -> Result<Option<Page<InspectionEvent>>> {
            unreachable!()
        }

        fn list_steps(&self, _run_id: &str) -> Result<Option<Vec<InspectionStep>>> {
            unreachable!()
        }

        fn get_step(&self, _run_id: &str, _step_id: &str) -> Result<Option<InspectionStep>> {
            unreachable!()
        }

        fn list_step_executions(
            &self,
            _run_id: &str,
            _step_id: &str,
        ) -> Result<Option<Vec<InspectionStepExecution>>> {
            unreachable!()
        }

        fn get_workflow_file(
            &self,
            _run_id: &str,
            _step_id: &str,
            _locator_kind: transist::InspectionWorkflowFileLocatorKind,
            _relative_path: &str,
        ) -> Result<Option<transist::InspectionWorkflowFileRef>> {
            unreachable!()
        }

        fn read_workflow_file_content(
            &self,
            _run_id: &str,
            _step_id: &str,
            _locator_kind: transist::InspectionWorkflowFileLocatorKind,
            _relative_path: &str,
            _workspace_root: Option<&std::path::Path>,
        ) -> Result<Option<transist::InspectionWorkflowFileContent>> {
            unreachable!()
        }

        fn list_gates(&self, _run_id: &str) -> Result<Option<Vec<InspectionGate>>> {
            unreachable!()
        }

        fn list_failures(&self, _run_id: &str) -> Result<Option<Vec<transist::FailureRecord>>> {
            unreachable!()
        }

        fn get_failure(
            &self,
            _run_id: &str,
            _failure_id: &str,
        ) -> Result<Option<transist::FailureRecord>> {
            unreachable!()
        }

        fn list_blobs(
            &self,
            _run_id: &str,
            _family: BlobFamily,
            _scope: BlobListScope,
        ) -> Result<Option<Vec<InspectionBlob>>> {
            unreachable!()
        }

        fn get_blob(
            &self,
            _run_id: &str,
            _family: BlobFamily,
            _step_id: &str,
            _execution: u32,
            _relative_path: &str,
        ) -> Result<Option<InspectionBlob>> {
            unreachable!()
        }

        fn read_blob_content(
            &self,
            _run_id: &str,
            _family: BlobFamily,
            _step_id: &str,
            _execution: u32,
            _relative_path: &str,
        ) -> Result<Option<transist::InspectionBlobContent>> {
            unreachable!()
        }

        fn list_workflows(
            &self,
            _query: &PaginationQuery,
        ) -> Result<Page<InspectionWorkflowSummary>> {
            unreachable!()
        }

        fn get_workflow(&self, _workflow_ref: &str) -> Result<Option<InspectionWorkflowDetail>> {
            unreachable!()
        }

        fn read_workflow_source(
            &self,
            _workflow_ref: &str,
        ) -> Result<Option<InspectionWorkflowContent>> {
            unreachable!()
        }

        fn read_workflow_compiled_recipe(
            &self,
            _workflow_ref: &str,
        ) -> Result<Option<InspectionWorkflowContent>> {
            unreachable!()
        }
    }

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    struct SharedWriterGuard(Arc<Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedWriter {
        type Writer = SharedWriterGuard;

        fn make_writer(&'a self) -> Self::Writer {
            SharedWriterGuard(self.0.clone())
        }
    }

    impl io::Write for SharedWriterGuard {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn storage_query_failures_are_sanitized_and_logged() {
        let logs = SharedWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(logs.clone())
            .finish();
        let service = InspectionApiService::from_query(Arc::new(FailingQuery));

        let result = tracing::subscriber::with_default(subscriber, || {
            service.list_runs(&RunListQuery::default())
        });

        let problem = result.expect_err("storage failure");
        assert_eq!(problem.code, "storage_query_failure");
        assert_eq!(problem.message, service::STORAGE_QUERY_FAILURE_MESSAGE);

        let output = String::from_utf8(logs.0.lock().expect("lock").clone()).expect("utf8");
        assert!(output.contains("/tmp/hidden/state.json"));
        assert!(output.contains("invalid serde payload"));
    }

    #[test]
    fn parse_rfc3339_supports_offsets_and_fractional_seconds() {
        assert_eq!(
            time::parse_rfc3339_timestamp_ms("2025-01-01T00:00:01.987Z"),
            Ok(1_735_689_601_987)
        );
        assert_eq!(
            time::parse_rfc3339_timestamp_ms("2025-01-01T02:30:01.987+02:30"),
            Ok(1_735_689_601_987)
        );
        assert_eq!(
            time::parse_rfc3339_timestamp_ms("2025-01-01T00:00:01.9Z"),
            Ok(1_735_689_601_900)
        );
    }

    #[test]
    fn parse_rfc3339_rejects_invalid_and_pre_epoch_values() {
        assert_eq!(
            time::parse_rfc3339_timestamp_ms("nope"),
            Err(time::Rfc3339ParseError::Invalid)
        );
        assert_eq!(
            time::parse_rfc3339_timestamp_ms("1969-12-31T23:59:59Z"),
            Err(time::Rfc3339ParseError::BeforeUnixEpoch)
        );
    }

    #[test]
    fn format_rfc3339_renders_utc_timestamps() {
        assert_eq!(
            time::format_rfc3339_timestamp_ms(1_735_689_601_000),
            Some("2025-01-01T00:00:01Z".to_string())
        );
        assert_eq!(
            time::format_rfc3339_timestamp_ms(1_735_689_601_987),
            Some("2025-01-01T00:00:01.987Z".to_string())
        );
    }
}
