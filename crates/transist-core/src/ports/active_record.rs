// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use std::path::Path;

/// Persisted active-invocation payload written by adapters when a plugin process starts.
#[derive(Debug, Clone)]
pub struct ActiveRecordWrite {
    /// Stable invoke id assigned by the kernel.
    pub invoke_id: String,
    /// Run id that owns the invocation.
    pub run_id: String,
    /// Step id within the run.
    pub step_id: String,
    /// Adapter kind that owns the spawned process (for example `process`, `http`, or `mcp`).
    pub adapter_kind: String,
    /// PID of the spawned root process.
    pub pid: u32,
    /// Process-group id on Unix; absent on platforms without group signalling.
    pub pgid: Option<u32>,
    /// PID of the Transist executor process that spawned the plugin.
    pub executor_pid: u32,
    /// Wall-clock spawn time in milliseconds since Unix epoch.
    pub started_at_ms: u64,
    /// Opaque OS-provided process-birth token captured immediately after spawn.
    ///
    /// Cancellation compares this token before signalling so a recycled PID is
    /// never mistaken for the invocation that originally wrote this record.
    /// Legacy records omit the token and must not be signalled.
    pub process_birth_token: Option<String>,
}

/// Port that owns active-invocation sentinel persistence and lookup policy.
pub trait ActiveRecordPort: Send + Sync {
    /// Writes one active invocation record for `run_root`.
    fn write_record(&self, run_root: &Path, record: &ActiveRecordWrite) -> Result<()>;
    /// Removes one active invocation record if present.
    fn remove_record(&self, run_root: &Path, invoke_id: &str) -> Result<()>;
    /// Lists active invocation records for cross-process cancellation.
    fn list_records(&self, run_root: &Path) -> Result<Vec<ActiveRecordWrite>>;
    /// Writes a cancel sentinel for one invocation.
    fn write_cancel_sentinel(&self, run_root: &Path, invoke_id: &str) -> Result<()>;
    /// Removes a cancel sentinel if present.
    fn remove_cancel_sentinel(&self, run_root: &Path, invoke_id: &str) -> Result<()>;
    /// Returns whether a cancel sentinel currently exists.
    fn cancel_sentinel_exists(&self, run_root: &Path, invoke_id: &str) -> bool;
}
