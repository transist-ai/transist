// SPDX-License-Identifier: MPL-2.0
//! Persisted per-invocation records published to `<run-root>/active_invocations/`.
//!
//! When an adapter spawns an external process it writes a small JSON record
//! describing the invocation and the spawned process so external tooling
//! (other Transist processes, an external supervisor, manual cancel commands)
//! can locate and signal the process group without needing in-memory access
//! to the kernel that started it.
//!
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use transist_core::ports::{ActiveRecordPort, ActiveRecordWrite};
use transist_protocol::{AdapterKind, ExecutionRequest};

/// Returns the run-tree root directory for an invocation request.
pub(crate) fn run_root_for_request(request: &ExecutionRequest) -> PathBuf {
    PathBuf::from(&request.paths.run_root)
}

/// Publishes the active-invocation record to disk after an adapter spawns the
/// plugin process. Failures are surfaced to the caller so spawn-time errors
/// remain visible; the caller is responsible for terminating the spawned
/// process if persistence fails.
pub(crate) fn publish_active_invocation(
    active_record_store: &dyn ActiveRecordPort,
    request: &ExecutionRequest,
    adapter_kind: AdapterKind,
    child: &Arc<Mutex<Child>>,
) -> Result<()> {
    let pid = {
        let guard = child.lock().unwrap_or_else(|e| e.into_inner());
        guard.id()
    };
    let pgid = pgid_for_pid(pid)?;
    let process_birth_token = crate::os_process::process_birth_token(pid).with_context(|| {
        format!(
            "failed reading OS birth identity for spawned process {}",
            pid
        )
    })?;
    let executor_pid = std::process::id();
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let record = ActiveRecordWrite {
        invoke_id: request.invoke_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step.id.clone(),
        adapter_kind: adapter_kind.to_string(),
        pid,
        pgid,
        executor_pid,
        started_at_ms,
        process_birth_token: Some(process_birth_token),
    };
    active_record_store.write_record(&run_root_for_request(request), &record)
}

/// Returns `true` if a cross-process cancel was observed for this invocation
/// (i.e. the cancel sentinel file exists at the moment of the check).
///
/// Adapters call this on every cleanup path so that an external cancel — which
/// races with the adapter's own bookkeeping cleanup to remove the record — is
/// reliably distinguished from a spontaneous failure.
pub(crate) fn external_cancel_observed(
    active_record_store: &dyn ActiveRecordPort,
    request: &ExecutionRequest,
) -> bool {
    active_record_store.cancel_sentinel_exists(&run_root_for_request(request), &request.invoke_id)
}

/// Removes the active-invocation record and any cancel sentinel. Missing
/// files and IO failures are silently ignored so this can be called from
/// cleanup paths that must not themselves fail.
pub(crate) fn clear_active_invocation(
    active_record_store: &dyn ActiveRecordPort,
    request: &ExecutionRequest,
) {
    let run_root = run_root_for_request(request);
    let _ = active_record_store.remove_record(&run_root, &request.invoke_id);
    let _ = active_record_store.remove_cancel_sentinel(&run_root, &request.invoke_id);
}

#[cfg(unix)]
fn pgid_for_pid(pid: u32) -> Result<Option<u32>> {
    crate::os_process::process_group(pid as libc::pid_t)
        .map(|pgid| Some(pgid as u32))
        .context("failed reading spawned process group")
}

#[cfg(not(unix))]
fn pgid_for_pid(_pid: u32) -> Result<Option<u32>> {
    Ok(None)
}
