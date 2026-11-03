// SPDX-License-Identifier: MPL-2.0
//! Cross-process cancellation by reading durable active-invocation records.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use transist_core::ports::{ActiveRecordPort, ActiveRecordWrite};

use crate::os_process::SignalStatus;

/// Outcome of attempting to cancel a single recorded invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The recorded PID was absent before cancellation began.
    Absent,
    /// SIGTERM terminated the requested invocation.
    Terminated,
    /// SIGKILL terminated the requested invocation after the grace period.
    Killed,
    /// The executor lacked permission to inspect or signal the target.
    PermissionDenied {
        /// Operation that was denied.
        operation: String,
    },
    /// The recorded process or process group no longer matches its identity.
    IdentityMismatch {
        /// Why the identity could not be confirmed.
        reason: String,
    },
    /// A signal or process-inspection operation failed without proving that the
    /// process terminated.
    SignalFailure {
        /// Signal that failed, or zero for an inspection operation.
        signal: i32,
        /// Human-readable OS error.
        error: String,
    },
    /// The target remained present after the requested signal and grace period.
    Timeout {
        /// Signal after which the target remained present.
        signal: i32,
    },
    /// The cancellation sentinel could not be persisted.
    SentinelFailure {
        /// Human-readable persistence error.
        error: String,
    },
}

/// Result of [`cancel_active_invocations_on_disk`] for a single record.
#[derive(Debug, Clone)]
pub struct CancelEntryResult {
    /// The record that was inspected.
    pub record: ActiveRecordWrite,
    /// Outcome of the cancellation attempt.
    pub outcome: CancelOutcome,
}

/// Scans active invocation records for `step_id` and cancels only processes
/// whose identity is proven to match the birth token captured at spawn time.
///
/// The record is removed only after confirmed termination. Every other outcome
/// retains it for operator recovery. Cancellation sentinels are written only
/// after identity validation and removed whenever cancellation is not proven.
pub fn cancel_active_invocations_on_disk(
    active_record_store: &dyn ActiveRecordPort,
    run_root: &Path,
    step_id: &str,
    grace: Duration,
) -> Result<Vec<CancelEntryResult>> {
    #[cfg(unix)]
    {
        cancel_active_invocations_on_disk_with(
            active_record_store,
            run_root,
            step_id,
            grace,
            &RealProcessControl,
        )
    }

    #[cfg(not(unix))]
    {
        let records = active_record_store.list_records(run_root)?;
        let mut results = Vec::new();
        for record in records {
            if record.step_id == step_id {
                results.push(CancelEntryResult {
                    record,
                    outcome: CancelOutcome::SignalFailure {
                        signal: 0,
                        error: "process cancellation is only implemented on Unix".to_string(),
                    },
                });
            }
        }
        Ok(results)
    }
}

#[cfg(unix)]
fn cancel_active_invocations_on_disk_with(
    active_record_store: &dyn ActiveRecordPort,
    run_root: &Path,
    step_id: &str,
    grace: Duration,
    process: &dyn ProcessControl,
) -> Result<Vec<CancelEntryResult>> {
    let records = active_record_store.list_records(run_root)?;
    let mut results = Vec::new();
    for record in records {
        if record.step_id != step_id {
            continue;
        }

        let target = match validate_identity(process, &record) {
            Ok(target) => target,
            Err(outcome) => {
                if matches!(outcome, CancelOutcome::Absent) {
                    // An absent process-group leader does not prove its
                    // descendants exited. Retain the record while its group
                    // remains present so a later cancellation cannot silently
                    // abandon the surviving processes.
                    if let Some(pgid) = record.pgid
                        && matches!(
                            process.probe_process_group(pgid as libc::pid_t),
                            SignalStatus::Sent
                        )
                    {
                        results.push(CancelEntryResult {
                            record,
                            outcome: CancelOutcome::IdentityMismatch {
                                reason: "process-group leader is absent while group remains active"
                                    .to_string(),
                            },
                        });
                        continue;
                    }
                    let _ = active_record_store.remove_record(run_root, &record.invoke_id);
                }
                results.push(CancelEntryResult { record, outcome });
                continue;
            }
        };

        if let Err(error) = active_record_store.write_cancel_sentinel(run_root, &record.invoke_id) {
            let cleanup = active_record_store
                .remove_cancel_sentinel(run_root, &record.invoke_id)
                .err()
                .map(|cleanup| format!("; failed removing partial sentinel: {cleanup:#}"))
                .unwrap_or_default();
            results.push(CancelEntryResult {
                record,
                outcome: CancelOutcome::SentinelFailure {
                    error: format!("failed publishing cancellation sentinel: {error:#}{cleanup}"),
                },
            });
            continue;
        }

        // Publishing the sentinel is deliberately before signalling so an
        // adapter cannot persist a completed result after cancellation wins.
        // Revalidate immediately afterwards: sentinel I/O may otherwise leave
        // enough time for a PID or process-group identifier to be recycled.
        match validate_identity(process, &record) {
            Ok(revalidated) if revalidated == target => {}
            Ok(_) => {
                let cleanup =
                    active_record_store.remove_cancel_sentinel(run_root, &record.invoke_id);
                results.push(CancelEntryResult {
                    record,
                    outcome: cleanup
                        .err()
                        .map(|error| CancelOutcome::SentinelFailure {
                            error: format!("process target changed before SIGTERM and sentinel cleanup failed: {error:#}"),
                        })
                        .unwrap_or(CancelOutcome::IdentityMismatch {
                            reason: "process target changed before SIGTERM".to_string(),
                        }),
                });
                continue;
            }
            Err(outcome) => {
                let cleanup =
                    active_record_store.remove_cancel_sentinel(run_root, &record.invoke_id);
                results.push(CancelEntryResult {
                    record,
                    outcome: cleanup
                        .err()
                        .map(|error| CancelOutcome::SentinelFailure {
                            error: format!("identity validation before SIGTERM failed ({outcome:?}) and sentinel cleanup failed: {error:#}"),
                        })
                        .unwrap_or(outcome),
                });
                continue;
            }
        }

        let outcome = cancel_one(process, &record, target, grace);
        if matches!(
            outcome,
            CancelOutcome::Absent | CancelOutcome::Terminated | CancelOutcome::Killed
        ) {
            // Cleanup is best-effort only after absence is proven. A failed
            // cleanup cannot change the already-confirmed cancellation result.
            let _ = active_record_store.remove_record(run_root, &record.invoke_id);
        } else {
            // A failed or unconfirmed cancellation must not poison a later
            // result by leaving a stale cancellation request behind.
            if let Err(error) =
                active_record_store.remove_cancel_sentinel(run_root, &record.invoke_id)
            {
                results.push(CancelEntryResult {
                    record,
                    outcome: CancelOutcome::SentinelFailure {
                        error: format!(
                            "cancellation was not confirmed and sentinel cleanup failed: {error:#}"
                        ),
                    },
                });
                continue;
            }
        }
        results.push(CancelEntryResult { record, outcome });
    }
    Ok(results)
}

#[cfg(unix)]
fn cancel_one(
    process: &dyn ProcessControl,
    record: &ActiveRecordWrite,
    target: SignalTarget,
    grace: Duration,
) -> CancelOutcome {
    match process.send(target, libc::SIGTERM) {
        SignalStatus::Sent => {}
        SignalStatus::PermissionDenied => {
            return CancelOutcome::PermissionDenied {
                operation: "SIGTERM".to_string(),
            };
        }
        status => {
            return CancelOutcome::SignalFailure {
                signal: libc::SIGTERM,
                error: describe(status),
            };
        }
    }
    match wait_for_termination(process, target, grace) {
        TerminationObservation::Absent => return CancelOutcome::Terminated,
        TerminationObservation::StillRunning => {}
        TerminationObservation::PermissionDenied => {
            return CancelOutcome::PermissionDenied {
                operation: "probe after SIGTERM".to_string(),
            };
        }
        TerminationObservation::ProbeFailed { errno, message } => {
            return CancelOutcome::SignalFailure {
                signal: 0,
                error: format!("errno {errno}: {message}"),
            };
        }
    }

    // Revalidate both PID birth and process-group identity immediately before
    // escalation. A recycled PID or PGID must never receive SIGKILL.
    match validate_identity(process, record) {
        Ok(revalidated) if revalidated == target => {}
        Ok(_) => {
            return CancelOutcome::IdentityMismatch {
                reason: "process target changed before SIGKILL".to_string(),
            };
        }
        Err(CancelOutcome::Absent) => {
            // The recorded leader may exit while descendants in its process
            // group survive SIGTERM. The group was identity-validated before
            // SIGTERM and its PGID cannot be recycled while members remain.
            if let SignalTarget::Group(pgid) = target {
                match process.probe_process_group(pgid) {
                    SignalStatus::Absent => return CancelOutcome::Terminated,
                    SignalStatus::Sent => {}
                    SignalStatus::PermissionDenied => {
                        return CancelOutcome::PermissionDenied {
                            operation: "probe process group".to_string(),
                        };
                    }
                    status => {
                        return CancelOutcome::SignalFailure {
                            signal: 0,
                            error: describe(status),
                        };
                    }
                }
            } else {
                return CancelOutcome::Terminated;
            }
        }
        Err(outcome) => return outcome,
    }

    match process.send(target, libc::SIGKILL) {
        SignalStatus::Sent => {}
        SignalStatus::PermissionDenied => {
            return CancelOutcome::PermissionDenied {
                operation: "SIGKILL".to_string(),
            };
        }
        status => {
            return CancelOutcome::SignalFailure {
                signal: libc::SIGKILL,
                error: describe(status),
            };
        }
    }
    match wait_for_termination(process, target, grace) {
        TerminationObservation::Absent => CancelOutcome::Killed,
        TerminationObservation::StillRunning => CancelOutcome::Timeout {
            signal: libc::SIGKILL,
        },
        TerminationObservation::PermissionDenied => CancelOutcome::PermissionDenied {
            operation: "probe after SIGKILL".to_string(),
        },
        TerminationObservation::ProbeFailed { errno, message } => CancelOutcome::SignalFailure {
            signal: 0,
            error: format!("errno {errno}: {message}"),
        },
    }
}

#[cfg(unix)]
fn validate_identity(
    process: &dyn ProcessControl,
    record: &ActiveRecordWrite,
) -> Result<SignalTarget, CancelOutcome> {
    let pid = record.pid as libc::pid_t;
    match process.probe_pid(pid) {
        SignalStatus::Absent => return Err(CancelOutcome::Absent),
        SignalStatus::PermissionDenied => {
            return Err(CancelOutcome::PermissionDenied {
                operation: "probe PID".to_string(),
            });
        }
        SignalStatus::Failed { errno, message } => {
            return Err(CancelOutcome::SignalFailure {
                signal: 0,
                error: format!("errno {errno}: {message}"),
            });
        }
        SignalStatus::Sent => {}
    }

    let Some(expected_birth) = record.process_birth_token.as_deref() else {
        return Err(CancelOutcome::IdentityMismatch {
            reason: "active invocation record has no process birth identity".to_string(),
        });
    };
    let actual_birth = match process.process_birth_token(record.pid) {
        Ok(token) => token,
        Err(error) if error_is_absent(&error) => return Err(CancelOutcome::Absent),
        Err(error) => {
            return Err(CancelOutcome::SignalFailure {
                signal: 0,
                error: format!("unable to verify process birth identity: {error:#}"),
            });
        }
    };
    if actual_birth != expected_birth {
        return Err(CancelOutcome::IdentityMismatch {
            reason: "recorded process birth identity does not match current PID".to_string(),
        });
    }

    match record.pgid {
        Some(expected_pgid) => match process.process_group(pid) {
            Ok(actual_pgid) if actual_pgid as u32 == expected_pgid => {
                Ok(SignalTarget::Group(actual_pgid))
            }
            Ok(actual_pgid) => Err(CancelOutcome::IdentityMismatch {
                reason: format!(
                    "recorded process group {} does not match current process group {}",
                    expected_pgid, actual_pgid
                ),
            }),
            Err(error) if error_is_absent(&error) => Err(CancelOutcome::Absent),
            Err(error) => Err(CancelOutcome::SignalFailure {
                signal: 0,
                error: format!("failed reading process group: {error}"),
            }),
        },
        None => Ok(SignalTarget::Pid(pid)),
    }
}

#[cfg(unix)]
fn error_is_absent(error: &anyhow::Error) -> bool {
    error
        .root_cause()
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        == Some(libc::ESRCH)
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignalTarget {
    Pid(libc::pid_t),
    Group(libc::pid_t),
}

#[cfg(unix)]
trait ProcessControl {
    fn probe_pid(&self, pid: libc::pid_t) -> SignalStatus;
    fn process_birth_token(&self, pid: u32) -> Result<String>;
    fn process_group(&self, pid: libc::pid_t) -> Result<libc::pid_t>;
    fn send(&self, target: SignalTarget, signal: libc::c_int) -> SignalStatus;
    fn probe_process_group(&self, pgid: libc::pid_t) -> SignalStatus;
    fn is_terminated_despite_presence(&self, target: SignalTarget) -> Result<bool>;
}

#[cfg(unix)]
struct RealProcessControl;

#[cfg(unix)]
impl ProcessControl for RealProcessControl {
    fn probe_pid(&self, pid: libc::pid_t) -> SignalStatus {
        crate::os_process::probe_pid(pid)
    }

    fn process_birth_token(&self, pid: u32) -> Result<String> {
        crate::os_process::process_birth_token(pid)
    }

    fn process_group(&self, pid: libc::pid_t) -> Result<libc::pid_t> {
        crate::os_process::process_group(pid)
    }

    fn send(&self, target: SignalTarget, signal: libc::c_int) -> SignalStatus {
        match target {
            SignalTarget::Pid(pid) => crate::os_process::signal_pid(pid, signal),
            SignalTarget::Group(pgid) => crate::os_process::signal_process_group(pgid, signal),
        }
    }

    fn probe_process_group(&self, pgid: libc::pid_t) -> SignalStatus {
        crate::os_process::probe_process_group(pgid)
    }

    fn is_terminated_despite_presence(&self, target: SignalTarget) -> Result<bool> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            match target {
                SignalTarget::Pid(pid) => match crate::os_process::process_is_zombie(pid as u32) {
                    Ok(zombie) => Ok(zombie),
                    // The process can exit between kill(0) and proc_pidinfo.
                    // Once proc_pidinfo reports ESRCH, termination is conclusive.
                    Err(error)
                        if error
                            .root_cause()
                            .downcast_ref::<std::io::Error>()
                            .and_then(std::io::Error::raw_os_error)
                            == Some(libc::ESRCH) =>
                    {
                        Ok(true)
                    }
                    Err(error) => Err(error),
                },
                SignalTarget::Group(pgid) => {
                    Ok(!crate::os_process::process_group_has_live_members(pgid)?)
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = target;
            Ok(false)
        }
    }
}

#[cfg(unix)]
enum TerminationObservation {
    Absent,
    StillRunning,
    PermissionDenied,
    ProbeFailed { errno: i32, message: String },
}

#[cfg(unix)]
fn wait_for_termination(
    process: &dyn ProcessControl,
    target: SignalTarget,
    grace: Duration,
) -> TerminationObservation {
    let deadline = std::time::Instant::now() + grace;
    loop {
        let status = match target {
            SignalTarget::Pid(pid) => process.probe_pid(pid),
            SignalTarget::Group(pgid) => process.probe_process_group(pgid),
        };
        match status {
            SignalStatus::Absent => return TerminationObservation::Absent,
            SignalStatus::Sent => match process.is_terminated_despite_presence(target) {
                Ok(true) => return TerminationObservation::Absent,
                Ok(false) => {}
                Err(error) => {
                    return TerminationObservation::ProbeFailed {
                        errno: error
                            .root_cause()
                            .downcast_ref::<std::io::Error>()
                            .and_then(std::io::Error::raw_os_error)
                            .unwrap_or(0),
                        message: error.to_string(),
                    };
                }
            },
            SignalStatus::PermissionDenied => return TerminationObservation::PermissionDenied,
            SignalStatus::Failed { errno, message } => {
                return TerminationObservation::ProbeFailed { errno, message };
            }
        }
        if std::time::Instant::now() >= deadline {
            return TerminationObservation::StillRunning;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn describe(status: SignalStatus) -> String {
    match status {
        SignalStatus::Sent => "sent".to_string(),
        SignalStatus::Absent => "process is absent".to_string(),
        SignalStatus::PermissionDenied => "permission denied".to_string(),
        SignalStatus::Failed { errno, message } => format!("errno {errno}: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[cfg(unix)]
    struct FakeProcess {
        probes: Mutex<VecDeque<SignalStatus>>,
        births: Mutex<VecDeque<Result<String>>>,
        sends: Mutex<Vec<libc::c_int>>,
        send_statuses: Mutex<VecDeque<SignalStatus>>,
        terminated_despite_presence: Mutex<VecDeque<Result<bool>>>,
    }

    #[cfg(unix)]
    impl ProcessControl for FakeProcess {
        fn probe_pid(&self, _pid: libc::pid_t) -> SignalStatus {
            self.probes
                .lock()
                .expect("probe lock")
                .pop_front()
                .unwrap_or(SignalStatus::Sent)
        }

        fn process_birth_token(&self, _pid: u32) -> Result<String> {
            self.births
                .lock()
                .expect("birth lock")
                .pop_front()
                .unwrap_or_else(|| Ok("birth".to_string()))
        }

        fn process_group(&self, _pid: libc::pid_t) -> Result<libc::pid_t> {
            Ok(123)
        }

        fn send(&self, _target: SignalTarget, signal: libc::c_int) -> SignalStatus {
            self.sends.lock().expect("send lock").push(signal);
            self.send_statuses
                .lock()
                .expect("send status lock")
                .pop_front()
                .unwrap_or(SignalStatus::Sent)
        }

        fn probe_process_group(&self, _pgid: libc::pid_t) -> SignalStatus {
            self.probe_pid(0)
        }

        fn is_terminated_despite_presence(&self, _target: SignalTarget) -> Result<bool> {
            self.terminated_despite_presence
                .lock()
                .expect("effective termination lock")
                .pop_front()
                .unwrap_or(Ok(false))
        }
    }

    #[cfg(unix)]
    fn fake_record() -> ActiveRecordWrite {
        ActiveRecordWrite {
            invoke_id: "invoke".to_string(),
            run_id: "run".to_string(),
            step_id: "step".to_string(),
            adapter_kind: "process".to_string(),
            pid: 42,
            pgid: None,
            executor_pid: 1,
            started_at_ms: 1,
            process_birth_token: Some("birth".to_string()),
        }
    }

    #[test]
    fn permission_denied_is_typed_and_not_success() {
        let outcome = CancelOutcome::PermissionDenied {
            operation: "signal".to_string(),
        };
        assert!(!matches!(
            outcome,
            CancelOutcome::Terminated | CancelOutcome::Killed | CancelOutcome::Absent
        ));
    }

    #[test]
    fn failed_signal_is_typed_and_not_success() {
        let outcome = CancelOutcome::SignalFailure {
            signal: libc::SIGTERM,
            error: "invalid signal".to_string(),
        };
        assert!(matches!(outcome, CancelOutcome::SignalFailure { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn pid_identity_change_between_term_and_kill_blocks_escalation() {
        let process = FakeProcess {
            probes: Mutex::new(VecDeque::from([SignalStatus::Sent, SignalStatus::Sent])),
            births: Mutex::new(VecDeque::from([Ok("reused".to_string())])),
            sends: Mutex::new(Vec::new()),
            send_statuses: Mutex::new(VecDeque::from([SignalStatus::Sent])),
            terminated_despite_presence: Mutex::new(VecDeque::new()),
        };

        let outcome = cancel_one(
            &process,
            &fake_record(),
            SignalTarget::Pid(42),
            Duration::ZERO,
        );

        assert!(matches!(outcome, CancelOutcome::IdentityMismatch { .. }));
        assert_eq!(
            *process.sends.lock().expect("send lock"),
            vec![libc::SIGTERM]
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_sigterm_does_not_escalate() {
        let process = FakeProcess {
            probes: Mutex::new(VecDeque::new()),
            births: Mutex::new(VecDeque::new()),
            sends: Mutex::new(Vec::new()),
            send_statuses: Mutex::new(VecDeque::from([SignalStatus::PermissionDenied])),
            terminated_despite_presence: Mutex::new(VecDeque::new()),
        };

        let outcome = cancel_one(
            &process,
            &fake_record(),
            SignalTarget::Pid(42),
            Duration::ZERO,
        );

        assert!(matches!(outcome, CancelOutcome::PermissionDenied { .. }));
        assert_eq!(
            *process.sends.lock().expect("send lock"),
            vec![libc::SIGTERM]
        );
    }

    #[cfg(unix)]
    #[test]
    fn unconfirmed_sigkill_is_a_timeout() {
        let process = FakeProcess {
            probes: Mutex::new(VecDeque::from([
                SignalStatus::Sent,
                SignalStatus::Sent,
                SignalStatus::Sent,
            ])),
            births: Mutex::new(VecDeque::from([Ok("birth".to_string())])),
            sends: Mutex::new(Vec::new()),
            send_statuses: Mutex::new(VecDeque::from([SignalStatus::Sent, SignalStatus::Sent])),
            terminated_despite_presence: Mutex::new(VecDeque::new()),
        };

        let outcome = cancel_one(
            &process,
            &fake_record(),
            SignalTarget::Pid(42),
            Duration::ZERO,
        );

        assert_eq!(
            outcome,
            CancelOutcome::Timeout {
                signal: libc::SIGKILL,
            }
        );
        assert_eq!(
            *process.sends.lock().expect("send lock"),
            vec![libc::SIGTERM, libc::SIGKILL]
        );
    }

    #[cfg(unix)]
    #[test]
    fn zombie_after_sigterm_is_confirmed_terminated_without_escalation() {
        let process = FakeProcess {
            probes: Mutex::new(VecDeque::from([SignalStatus::Sent])),
            births: Mutex::new(VecDeque::new()),
            sends: Mutex::new(Vec::new()),
            send_statuses: Mutex::new(VecDeque::from([SignalStatus::Sent])),
            terminated_despite_presence: Mutex::new(VecDeque::from([Ok(true)])),
        };

        let outcome = cancel_one(
            &process,
            &fake_record(),
            SignalTarget::Pid(42),
            Duration::ZERO,
        );

        assert_eq!(outcome, CancelOutcome::Terminated);
        assert_eq!(
            *process.sends.lock().expect("send lock"),
            vec![libc::SIGTERM]
        );
    }
}
