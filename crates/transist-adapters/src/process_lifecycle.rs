// SPDX-License-Identifier: MPL-2.0
//! Post-terminal process-boundary diagnostics, separate from executable output.

use crate::*;

/// Retention eligibility and pending protocol/schema failure, captured as soon
/// as a terminal frame arrives, before observing or terminating its boundary.
pub(crate) struct TerminalResultValidation {
    /// Whether the APP type, version and invocation identity matched the request.
    pub(crate) identity_valid: bool,
    /// Whether output may be retained under the declared action schema.
    pub(crate) output_valid: bool,
    /// Pending identity or successful-output failure that must survive cleanup.
    pub(crate) failure: Option<protocol::ProtocolError>,
}

/// Validates terminal identity and action output without writing hints or files.
/// Schema-invalid successful output is a validation failure; schema-invalid
/// failed output only loses retention eligibility, preserving its provider error.
pub(crate) fn validate_terminal_result(
    request: &ExecutionRequest,
    plugin_dir: &Path,
    action: &ActionDescriptor,
    terminal: &AppInvokeResult,
) -> TerminalResultValidation {
    if let Err(error) = validate_app_result_identity(request, terminal) {
        return TerminalResultValidation {
            identity_valid: false,
            output_valid: false,
            failure: Some(protocol::ProtocolError::new(
                CanonicalErrorCode::ProtocolError,
                error.to_string(),
            )),
        };
    }
    let output = transist_plugins::validate_action_output(plugin_dir, action, &terminal.output);
    let output_valid = output.is_ok();
    let failure = if terminal.status == "success" {
        output.err().map(|error| {
            protocol::ProtocolError::new(CanonicalErrorCode::ValidationError, error.to_string())
        })
    } else {
        None
    };
    TerminalResultValidation {
        identity_valid: true,
        output_valid,
        failure,
    }
}

/// Builds a fail-closed lifecycle result and stages redacted, validated evidence.
/// Hints remain subject to the ordinary kernel checksum/redaction promotion.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lifecycle_failure(
    invocation: &ExecutionInvocation,
    terminal: AppInvokeResult,
    validation: TerminalResultValidation,
    snapshot: Result<serde_json::Value>,
    cleanup_confirmed: bool,
    exit_observation_failed: bool,
    grace: Duration,
    timing: protocol::InvokeTiming,
) -> Result<ExecutionResult> {
    let request = &invocation.request;
    let secrets = &invocation.secret_values;
    let snapshot_failed = snapshot.is_err();
    // OS errors may include host paths or arbitrary text. Persist only a stable
    // diagnostic reason, never the raw observation error.
    let snapshot = snapshot.unwrap_or(serde_json::Value::Null);
    let message = if !cleanup_confirmed {
        "process boundary cleanup was not confirmed after terminal APP result".to_string()
    } else if exit_observation_failed {
        "failed observing process boundary exit after terminal APP result".to_string()
    } else if snapshot_failed {
        "failed observing process boundary snapshot after terminal APP result".to_string()
    } else {
        format!(
            "process boundary did not exit within {} ms after terminal APP result",
            grace.as_millis()
        )
    };
    let internal_failure = !cleanup_confirmed || exit_observation_failed || snapshot_failed;
    let (code, message) = if !cleanup_confirmed {
        (CanonicalErrorCode::AdapterInternalError, message)
    } else if let Some(error) = &validation.failure {
        (error.code, redact_text(&error.message, secrets))
    } else if internal_failure {
        (CanonicalErrorCode::AdapterInternalError, message)
    } else {
        (CanonicalErrorCode::ProtocolError, message)
    };
    let mut result = failure_result(request, code, &message, String::new(), Some(2), timing);
    // Failed APP output may legitimately be empty and not satisfy the success
    // schema. Preserve its safe error summary, never that invalid output/hints.
    if validation.output_valid && !internal_failure && validation.failure.is_none() {
        let (artifacts, evidence) = canonicalize_hints(
            request,
            &terminal.artifact_hints,
            &terminal.evidence_hints,
            secrets,
        )?;
        result.artifacts = artifacts;
        result.evidence = evidence;
    }
    let mut evidence = serde_json::json!({
        "schema": "transist.process_lifecycle.v1",
        "phase": "post_result_exit",
        "reason": if exit_observation_failed { "process_exit_observation_failed" } else { "process_exit_grace_exceeded" },
        "exit_grace_ms": grace.as_millis(),
        "terminal_result_received": true,
        "snapshot": snapshot,
        "snapshot_observation": if snapshot_failed { "failed" } else { "observed" },
        "cleanup": if cleanup_confirmed { "confirmed" } else { "unconfirmed" },
    });
    if snapshot_failed {
        evidence["snapshot_observation_reason"] =
            serde_json::json!("process_lifecycle_snapshot_observation_failed");
    }
    if let Some(error) = validation.failure {
        evidence["terminal_validation_error"] = serde_json::to_value(error)?;
    }
    if validation.identity_valid {
        evidence["original_terminal_status"] = serde_json::json!(terminal.status);
        // Redact the original error summary before evidence becomes persistent.
        evidence["original_terminal_error"] = serde_json::to_value(&terminal.error)?;
        if validation.output_valid {
            evidence["original_output"] = terminal.output;
        }
    }
    let bytes = serde_json::to_vec_pretty(&redact_json(evidence, secrets))?;
    let dir = Path::new(&request.paths.evidence_dir);
    fs::create_dir_all(dir)?;
    // Never follow or overwrite a plugin-created diagnostic path. The kernel
    // performs final regular-file, containment, checksum and redaction checks.
    let mut file = NamedTempFile::new_in(dir)?;
    file.write_all(&bytes)?;
    let path = dir.join("transist_process_lifecycle_evidence.json");
    file.persist_noclobber(&path)?;
    result.evidence.push(protocol::FileRef {
        path: path.display().to_string(),
        sha256: transist_trust_digest::sha256_hex(&bytes),
        bytes: bytes.len() as u64,
    });
    Ok(result)
}

#[cfg(test)]
#[path = "../../../tests/support/process_lifecycle.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use transist_core::ports::RemoteSecretPolicy;

    /// Owns isolated schema files and staging paths for direct lifecycle faults.
    struct LifecycleFixture {
        _root: tempfile::TempDir,
        invocation: ExecutionInvocation,
        plugin_dir: PathBuf,
        action: ActionDescriptor,
        terminal: AppInvokeResult,
    }

    impl LifecycleFixture {
        /// Creates a terminal success carrying secret-bearing output and hints.
        /// No plugin is installed or spawned; only local test files are written.
        fn new() -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let plugin_dir = root.path().join("plugin");
            fixture::write_plugin(&plugin_dir);
            let manifest = transist_plugins::load_manifest_file(&plugin_dir.join("plugin.yaml"))
                .expect("manifest");
            let request: ExecutionRequest = serde_json::from_value(json!({
                "type": "tep.invoke.request.v1", "protocol_version": "tep.v1",
                "invoke_id": "fault-invoke", "run_id": "fault-run", "workflow_id": "fault",
                "step": {"id": "invoke", "execution": 1}, "adapter_kind": "process",
                "action": "invoke", "input": {}, "requested_capabilities": [],
                "paths": {
                    "run_root": root.path(), "workspace_dir": root.path(),
                    "artifacts_dir": root.path().join("artifacts"),
                    "evidence_dir": root.path().join("evidence"),
                },
                "policy": {}, "metadata": {},
            }))
            .expect("request");
            let terminal: AppInvokeResult = serde_json::from_value(json!({
                "type": "app.invoke.result.v1", "protocol_version": "app.v1",
                "invoke_id": "fault-invoke", "status": "success",
                "output": {"stdout": "fault-secret ZmF1bHQtc2VjcmV0"},
                "artifact_hints": [{"name": "output.txt", "contents": "fault-secret"}],
                "evidence_hints": [{"name": "trace.txt", "contents": "fault-secret"}],
                "metrics": {},
            }))
            .expect("terminal");
            Self {
                invocation: ExecutionInvocation {
                    request,
                    secret_values: BTreeMap::from([("SECRET".into(), "fault-secret".into())]),
                    remote_secret_policy: RemoteSecretPolicy::default(),
                },
                plugin_dir,
                action: manifest.actions.into_iter().next().expect("action"),
                terminal,
                _root: root,
            }
        }

        /// Injects cleanup/observation outcomes directly into the evidence builder.
        /// Returns the fail-closed result without any OS fault hooks or processes.
        fn fail(&self, cleanup: bool, exit_failed: bool, snapshot_failed: bool) -> ExecutionResult {
            let validation = validate_terminal_result(
                &self.invocation.request,
                &self.plugin_dir,
                &self.action,
                &self.terminal,
            );
            let snapshot = if snapshot_failed {
                Err(anyhow!(
                    "fault-secret ZmF1bHQtc2VjcmV0 /secret/executable/path"
                ))
            } else {
                Ok(json!({"leader_alive": true, "live_pids": [42], "live_pgids": [42]}))
            };
            let timing = protocol::InvokeTiming {
                started_at: "1000".into(),
                finished_at: "900".into(),
                duration_ms: 100,
            };
            let result = lifecycle_failure(
                &self.invocation,
                self.terminal.clone(),
                validation,
                snapshot,
                cleanup,
                exit_failed,
                Duration::from_millis(250),
                timing.clone(),
            )
            .expect("lifecycle failure");
            assert_eq!(result.timing.started_at, timing.started_at);
            assert_eq!(result.timing.finished_at, timing.finished_at);
            assert_eq!(result.timing.duration_ms, timing.duration_ms);
            result
        }
    }

    /// Reads the diagnostic and verifies its explicit checksum and byte count.
    fn read_evidence(result: &ExecutionResult) -> Value {
        let file = result.evidence.last().expect("diagnostic ref");
        let bytes = fs::read(&file.path).expect("bytes");
        assert_eq!(file.sha256, transist_trust_digest::sha256_hex(&bytes));
        assert_eq!(file.bytes, bytes.len() as u64);
        let text = String::from_utf8(bytes).expect("text");
        for forbidden in [
            "fault-secret",
            "ZmF1bHQtc2VjcmV0",
            "/secret/executable/path",
        ] {
            assert!(!text.contains(forbidden));
        }
        serde_json::from_str(&text).expect("evidence")
    }

    #[test]
    fn cleanup_failure_retains_unconfirmed_evidence_without_hints() {
        let fixture = LifecycleFixture::new();
        let result = fixture.fail(false, false, false);
        assert_eq!(
            result.error.as_ref().unwrap().code,
            CanonicalErrorCode::AdapterInternalError
        );
        assert_eq!(result.output, json!({}));
        assert!(result.artifacts.is_empty());
        assert_eq!(result.evidence.len(), 1);
        assert_eq!(read_evidence(&result)["cleanup"], "unconfirmed");
    }

    #[test]
    fn exit_observation_failure_is_internal_and_never_promotes_hints() {
        for cleanup in [false, true] {
            let fixture = LifecycleFixture::new();
            let result = fixture.fail(cleanup, true, false);
            assert_eq!(
                result.error.as_ref().unwrap().code,
                CanonicalErrorCode::AdapterInternalError
            );
            assert!(result.artifacts.is_empty());
            assert_eq!(result.evidence.len(), 1);
            let evidence = read_evidence(&result);
            assert_eq!(evidence["reason"], "process_exit_observation_failed");
            assert_eq!(
                evidence["cleanup"],
                if cleanup { "confirmed" } else { "unconfirmed" }
            );
        }
    }

    #[test]
    fn snapshot_observation_failure_has_safe_explicit_diagnostics() {
        let fixture = LifecycleFixture::new();
        let result = fixture.fail(true, false, true);
        assert_eq!(
            result.error.as_ref().unwrap().code,
            CanonicalErrorCode::AdapterInternalError
        );
        assert!(result.artifacts.is_empty());
        assert_eq!(result.evidence.len(), 1);
        let evidence = read_evidence(&result);
        assert!(evidence["snapshot"].is_null());
        assert_eq!(evidence["snapshot_observation"], "failed");
        assert_eq!(
            evidence["snapshot_observation_reason"],
            "process_lifecycle_snapshot_observation_failed"
        );
    }

    #[test]
    fn pending_output_validation_failure_wins_after_confirmed_termination() {
        for exit_failed in [false, true] {
            let mut fixture = LifecycleFixture::new();
            fixture.terminal.output = json!({});
            let result = fixture.fail(true, exit_failed, false);
            assert_eq!(
                result.error.as_ref().unwrap().code,
                CanonicalErrorCode::ValidationError
            );
            assert_eq!(result.output, json!({}));
            assert!(result.artifacts.is_empty());
            assert_eq!(result.evidence.len(), 1);
            let evidence = read_evidence(&result);
            assert!(evidence.get("original_output").is_none());
            assert_eq!(
                evidence["terminal_validation_error"]["code"],
                "validation_error"
            );
        }
    }

    #[test]
    fn unconfirmed_cleanup_overrides_pending_validation_without_retaining_output() {
        let mut fixture = LifecycleFixture::new();
        fixture.terminal.output = json!({});
        let result = fixture.fail(false, false, false);
        assert_eq!(
            result.error.as_ref().unwrap().code,
            CanonicalErrorCode::AdapterInternalError
        );
        assert!(result.artifacts.is_empty());
        assert_eq!(result.evidence.len(), 1);
        let evidence = read_evidence(&result);
        assert_eq!(evidence["cleanup"], "unconfirmed");
        assert!(evidence.get("original_output").is_none());
        assert_eq!(
            evidence["terminal_validation_error"]["code"],
            "validation_error"
        );
    }

    #[test]
    fn exit_grace_message_formats_the_actual_duration() {
        let fixture = LifecycleFixture::new();
        let validation = validate_terminal_result(
            &fixture.invocation.request,
            &fixture.plugin_dir,
            &fixture.action,
            &fixture.terminal,
        );
        let result = lifecycle_failure(
            &fixture.invocation,
            fixture.terminal.clone(),
            validation,
            Ok(json!({"leader_alive": true, "live_pids": [42], "live_pgids": [42]})),
            true,
            false,
            Duration::from_millis(42),
            InvocationTimer::new().snapshot(),
        )
        .expect("lifecycle result");
        assert_eq!(
            result.error.as_ref().unwrap().message,
            "process boundary did not exit within 42 ms after terminal APP result"
        );
        assert_eq!(read_evidence(&result)["exit_grace_ms"], 42);
    }
}
