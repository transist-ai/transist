// SPDX-License-Identifier: MPL-2.0
//! Restart recovery helpers for persisted run snapshots.

use crate::types::{KernelRunSnapshot, KernelRunStatus, KernelStepStatus};

/// Rebuilds transient step/run state after restart.
pub fn recover_snapshot(snapshot: &mut KernelRunSnapshot, now_ms: u64) {
    if snapshot.status.is_terminal() {
        return;
    }

    for step in snapshot.steps.values_mut() {
        match step.status {
            KernelStepStatus::Dispatching | KernelStepStatus::Running => {
                // Explicit recovery rule: transient in-flight steps without a persisted result are
                // re-queued as runnable and executed as a new execution.
                if let Some(result) = step.last_result.as_ref() {
                    if result.status == "success" {
                        step.status = KernelStepStatus::Succeeded;
                    } else if let Some(wait_until) = step.retry_wait_until_ms {
                        if wait_until > now_ms {
                            step.status = KernelStepStatus::RetryWait;
                        } else {
                            step.status = KernelStepStatus::Failed;
                            step.retry_wait_until_ms = None;
                        }
                    } else {
                        step.status = KernelStepStatus::Failed;
                    }
                    if step.finished_at_ms.is_none() {
                        step.finished_at_ms = Some(now_ms);
                    }
                } else {
                    step.status = KernelStepStatus::Runnable;
                    step.finished_at_ms = None;
                }
            }
            KernelStepStatus::RetryWait
                if step.retry_wait_until_ms.unwrap_or_default() <= now_ms =>
            {
                step.status = KernelStepStatus::Runnable;
            }
            _ => {}
        }
    }

    let mut has_failed = false;
    let mut all_terminal = true;
    let mut has_waiting_retry = false;
    for step in snapshot.steps.values() {
        if step.status == KernelStepStatus::Failed {
            has_failed = true;
        }
        if !step.status.is_terminal() {
            all_terminal = false;
        }
        if step.status == KernelStepStatus::RetryWait {
            has_waiting_retry = true;
        }
    }

    snapshot.status = if all_terminal {
        if has_failed {
            KernelRunStatus::Failed
        } else {
            KernelRunStatus::Success
        }
    } else if has_waiting_retry {
        KernelRunStatus::Waiting
    } else {
        KernelRunStatus::Running
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::KernelStepState;

    #[test]
    fn recover_snapshot_requeues_inflight_steps() {
        let mut snapshot = KernelRunSnapshot::new("run", "wf", "v1", &["a".to_string()]);
        snapshot.status = KernelRunStatus::Running;
        snapshot.steps.insert(
            "a".to_string(),
            KernelStepState {
                step_id: "a".to_string(),
                status: KernelStepStatus::Dispatching,
                executions: 1,
                retry_wait_until_ms: None,
                skip_reason: None,
                last_request: None,
                last_result: None,
                started_at_ms: None,
                finished_at_ms: None,
                activation_executions_baseline: 0,
            },
        );

        recover_snapshot(&mut snapshot, 0);
        assert_eq!(snapshot.status, KernelRunStatus::Running);
        assert_eq!(
            snapshot.steps.get("a").map(|step| step.status),
            Some(KernelStepStatus::Runnable)
        );
    }

    #[test]
    fn recover_snapshot_preserves_terminal_runs() {
        let mut snapshot = KernelRunSnapshot::new("run", "wf", "v1", &["a".to_string()]);
        snapshot.status = KernelRunStatus::Success;
        recover_snapshot(&mut snapshot, 0);
        assert_eq!(snapshot.status, KernelRunStatus::Success);
    }

    #[test]
    fn recover_snapshot_finalizes_transient_step_when_result_is_persisted() {
        let mut snapshot = KernelRunSnapshot::new("run", "wf", "v1", &["a".to_string()]);
        snapshot.status = KernelRunStatus::Running;
        if let Some(step) = snapshot.steps.get_mut("a") {
            step.status = KernelStepStatus::Running;
            step.last_result = Some(transist_protocol::ExecutionResult {
                message_type: transist_protocol::TEP_INVOKE_RESULT_TYPE_V1.to_string(),
                protocol_version: transist_protocol::TEP_PROTOCOL_VERSION_V1.to_string(),
                invoke_id: "invoke".to_string(),
                run_id: "run".to_string(),
                step_id: "a".to_string(),
                execution: 1,
                status: "success".to_string(),
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                timing: transist_protocol::InvokeTiming {
                    started_at: "0".to_string(),
                    finished_at: "0".to_string(),
                    duration_ms: 0,
                },
                artifacts: Vec::new(),
                evidence: Vec::new(),
                metrics: std::collections::BTreeMap::new(),
                output: serde_json::json!({}),
                metrics_validation_errors: 0,
                usage: None,
                error: None,
            });
        }
        recover_snapshot(&mut snapshot, 10);
        assert_eq!(snapshot.status, KernelRunStatus::Success);
        assert_eq!(
            snapshot.steps.get("a").map(|step| step.status),
            Some(KernelStepStatus::Succeeded)
        );
    }
}
