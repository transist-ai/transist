// SPDX-License-Identifier: MPL-2.0
use crate::run_paths::{
    ACTIVE_INVOCATION_RECORD_SCHEMA_V1, ActiveInvocationRecord,
    active_invocation_cancel_sentinel_exists, list_active_invocation_records,
    remove_active_invocation_cancel_sentinel, remove_active_invocation_record,
    write_active_invocation_cancel_sentinel, write_active_invocation_record,
};
use anyhow::Result;
use std::path::Path;
use transist_core::ports::{ActiveRecordPort, ActiveRecordWrite};

/// Local filesystem-backed active-invocation sentinel store.
#[derive(Debug, Default, Clone)]
pub struct LocalActiveRecordStore;

impl ActiveRecordPort for LocalActiveRecordStore {
    fn write_record(&self, run_root: &Path, record: &ActiveRecordWrite) -> Result<()> {
        write_active_invocation_record(run_root, &to_run_path_record(record))
    }

    fn remove_record(&self, run_root: &Path, invoke_id: &str) -> Result<()> {
        remove_active_invocation_record(run_root, invoke_id)
    }

    fn list_records(&self, run_root: &Path) -> Result<Vec<ActiveRecordWrite>> {
        Ok(list_active_invocation_records(run_root)?
            .into_iter()
            .map(from_run_path_record)
            .collect())
    }

    fn write_cancel_sentinel(&self, run_root: &Path, invoke_id: &str) -> Result<()> {
        write_active_invocation_cancel_sentinel(run_root, invoke_id)
    }

    fn remove_cancel_sentinel(&self, run_root: &Path, invoke_id: &str) -> Result<()> {
        remove_active_invocation_cancel_sentinel(run_root, invoke_id)
    }

    fn cancel_sentinel_exists(&self, run_root: &Path, invoke_id: &str) -> bool {
        active_invocation_cancel_sentinel_exists(run_root, invoke_id)
    }
}

fn to_run_path_record(record: &ActiveRecordWrite) -> ActiveInvocationRecord {
    ActiveInvocationRecord {
        schema: ACTIVE_INVOCATION_RECORD_SCHEMA_V1.to_string(),
        invoke_id: record.invoke_id.clone(),
        run_id: record.run_id.clone(),
        step_id: record.step_id.clone(),
        adapter_kind: record.adapter_kind.clone(),
        pid: record.pid,
        pgid: record.pgid,
        executor_pid: record.executor_pid,
        started_at_ms: record.started_at_ms,
        process_birth_token: record.process_birth_token.clone(),
    }
}

fn from_run_path_record(record: ActiveInvocationRecord) -> ActiveRecordWrite {
    ActiveRecordWrite {
        invoke_id: record.invoke_id,
        run_id: record.run_id,
        step_id: record.step_id,
        adapter_kind: record.adapter_kind,
        pid: record.pid,
        pgid: record.pgid,
        executor_pid: record.executor_pid,
        started_at_ms: record.started_at_ms,
        process_birth_token: record.process_birth_token,
    }
}
