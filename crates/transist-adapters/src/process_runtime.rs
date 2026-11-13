// SPDX-License-Identifier: MPL-2.0
//! Shared child-process runtime helpers for process-backed adapters.

// SAFETY: OS-integration: child-process runtime helpers via libc syscalls.
#![allow(unsafe_code)]

use crate::*;

const LIVE_PROGRESS_RECORD_SCHEMA_V1: &str = "transist.live_progress.v1";

/// Handshake stdout receive failures, independent of the child's exit cause.
#[derive(Debug)]
pub(crate) enum HandshakeReceiveError {
    Timeout { what: String },
    Disconnected { what: String },
    ReadError(anyhow::Error),
}

impl std::fmt::Display for HandshakeReceiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { what } => write!(formatter, "timed out waiting for {what}"),
            Self::Disconnected { what } => {
                write!(formatter, "plugin stdout closed before {what}")
            }
            Self::ReadError(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for HandshakeReceiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadError(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) enum InvokeResultReceiveError {
    NoTerminalFrame {
        message: String,
    },
    Protocol {
        message: String,
        raw_line: Option<String>,
    },
    Transport {
        message: String,
    },
    Internal {
        message: String,
    },
}

pub(crate) fn resolve_execution_cwd(
    request: &ExecutionRequest,
    plugin_dir: &Path,
    package_cwd: Option<&str>,
) -> Result<PathBuf> {
    let workspace_declared = request
        .policy
        .get("workspace_declared")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if workspace_declared {
        if package_cwd.is_some_and(|cwd| !cwd.trim().is_empty() && cwd != ".") {
            bail!(
                "entrypoint.cwd is not supported when a workflow declares a workspace; resolve package support assets through manifest package paths"
            );
        }
        return Ok(PathBuf::from(&request.paths.workspace_dir));
    }
    Ok(package_cwd
        .filter(|cwd| !cwd.trim().is_empty())
        .map(|cwd| plugin_dir.join(cwd))
        .unwrap_or_else(|| plugin_dir.to_path_buf()))
}

pub(crate) fn configure_child_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Put plugin processes in their own group so cancellation can target the whole tree.
        // SAFETY: pre_exec runs the closure once in the child immediately after fork,
        // before exec, where calling setpgid(0,0) only re-parents the child itself and
        // cannot disturb the parent or sibling threads. The closure does no shared-memory
        // or heap work, so it is safe to run in the single-threaded fork child.
        unsafe {
            command.pre_exec(crate::os_process::set_own_process_group);
        }
    }
}

fn terminate_process(child: &Arc<Mutex<Child>>, grace: Duration) -> Result<()> {
    #[cfg(unix)]
    {
        let pid = child.lock().unwrap_or_else(|e| e.into_inner()).id() as i32;
        ensure_signal_sent(
            crate::os_process::signal_process_group(pid, libc::SIGTERM),
            "SIGTERM",
        )?;
        let deadline = Instant::now() + grace;
        if wait_for_exit(child, deadline).unwrap_or(false) {
            return Ok(());
        }
        ensure_signal_sent(
            crate::os_process::signal_process_group(pid, libc::SIGKILL),
            "SIGKILL",
        )?;
        child.lock().unwrap_or_else(|e| e.into_inner()).wait()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let mut guard = child.lock().unwrap_or_else(|e| e.into_inner());
        guard.kill()?;
        guard.wait()?;
        Ok(())
    }
}

#[cfg(unix)]
fn ensure_signal_sent(status: crate::os_process::SignalStatus, signal: &str) -> Result<()> {
    match status {
        crate::os_process::SignalStatus::Sent => Ok(()),
        other => bail!("{signal} was not delivered: {other:?}"),
    }
}

pub(crate) fn write_json_line<T: serde::Serialize, W: Write>(
    writer: &mut W,
    value: &T,
) -> Result<()> {
    writer.write_all(serde_json::to_string(value)?.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn parse_json_line<T: serde::de::DeserializeOwned>(line: &str) -> Result<T> {
    serde_json::from_str(line.trim_end()).context("failed decoding JSON line")
}

pub(crate) fn spawn_line_reader<R: Read + Send + 'static>(reader: R) -> Receiver<Result<String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(anyhow!("failed reading plugin stdout: {}", err)));
                    break;
                }
            }
        }
    });
    rx
}

pub(crate) fn spawn_stderr_reader(stderr: ChildStderr) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut out = String::new();
        let _ = reader.read_to_string(&mut out);
        out
    })
}

/// Receive one handshake line by the deadline, preserving transport failures.
pub(crate) fn recv_line(
    rx: &Receiver<Result<String>>,
    deadline: Instant,
    what: &str,
) -> std::result::Result<String, HandshakeReceiveError> {
    let now = Instant::now();
    let timeout = deadline.saturating_duration_since(now);
    match rx.recv_timeout(timeout) {
        Ok(line) => line.map_err(HandshakeReceiveError::ReadError),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(HandshakeReceiveError::Timeout {
            what: what.to_string(),
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(HandshakeReceiveError::Disconnected {
            what: what.to_string(),
        }),
    }
}

pub(crate) fn recv_invoke_result(
    rx: &Receiver<Result<String>>,
    inactivity_timeout: Duration,
    max_timeout_ms: Option<u64>,
    request: &ExecutionRequest,
) -> std::result::Result<AppInvokeResult, InvokeResultReceiveError> {
    let mut deadline = Instant::now() + inactivity_timeout;
    let hard_deadline = max_timeout_ms
        .map(Duration::from_millis)
        .and_then(|duration| Instant::now().checked_add(duration));
    loop {
        let recv_deadline = hard_deadline
            .map(|limit| limit.min(deadline))
            .unwrap_or(deadline);
        let line = match recv_invoke_stdout_line(rx, recv_deadline, "invoke result") {
            Ok(line) => line,
            Err(InvokeResultReceiveError::NoTerminalFrame { message: _ })
                if hard_deadline.is_some_and(|limit| Instant::now() >= limit) =>
            {
                let seconds = max_timeout_ms.unwrap_or_default() / 1000;
                return Err(InvokeResultReceiveError::NoTerminalFrame {
                    message: format!(
                        "max wall-clock timeout of {}s exceeded while waiting for invoke result",
                        seconds
                    ),
                });
            }
            Err(err) => return Err(err),
        };
        let raw_line = line.trim_end().to_string();
        let raw: serde_json::Value =
            parse_json_line(&line).map_err(|err| InvokeResultReceiveError::Protocol {
                message: err.to_string(),
                raw_line: Some(raw_line.clone()),
            })?;
        if raw.get("type").and_then(serde_json::Value::as_str) == Some(APP_INVOKE_PROGRESS_TYPE_V1)
        {
            let progress: AppInvokeProgress = serde_json::from_value(raw)
                .context("failed decoding invoke progress")
                .map_err(|err| InvokeResultReceiveError::Protocol {
                    message: err.to_string(),
                    raw_line: Some(raw_line.clone()),
                })?;
            validate_invoke_progress(request, &progress).map_err(|err| {
                InvokeResultReceiveError::Protocol {
                    message: err.to_string(),
                    raw_line: Some(raw_line.clone()),
                }
            })?;
            persist_live_progress(request, &progress).map_err(|err| {
                InvokeResultReceiveError::Internal {
                    message: err.to_string(),
                }
            })?;
            deadline = Instant::now() + inactivity_timeout;
            continue;
        }
        return serde_json::from_value(raw)
            .context("failed decoding invoke result")
            .map_err(|err| InvokeResultReceiveError::Protocol {
                message: err.to_string(),
                raw_line: Some(raw_line),
            });
    }
}

fn recv_invoke_stdout_line(
    rx: &Receiver<Result<String>>,
    deadline: Instant,
    what: &str,
) -> std::result::Result<String, InvokeResultReceiveError> {
    let now = Instant::now();
    let timeout = deadline.saturating_duration_since(now);
    match rx.recv_timeout(timeout) {
        Ok(line) => line.map_err(|err| InvokeResultReceiveError::Transport {
            message: err.to_string(),
        }),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(InvokeResultReceiveError::NoTerminalFrame {
            message: format!("timed out waiting for {}", what),
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(InvokeResultReceiveError::NoTerminalFrame {
                message: format!("plugin stdout closed before {}", what),
            })
        }
    }
}

fn validate_invoke_progress(
    request: &ExecutionRequest,
    progress: &AppInvokeProgress,
) -> Result<()> {
    if progress.protocol_version != APP_PROTOCOL_VERSION_V1 {
        bail!(
            "unexpected invoke progress protocol version '{}'",
            progress.protocol_version
        );
    }
    if progress.invoke_id != request.invoke_id {
        bail!(
            "invoke progress id '{}' did not match invocation '{}'",
            progress.invoke_id,
            request.invoke_id
        );
    }
    Ok(())
}

pub(crate) fn persist_live_progress(
    request: &ExecutionRequest,
    progress: &AppInvokeProgress,
) -> Result<()> {
    if progress.delta_lines == 0 && progress.line_preview.is_none() {
        return Ok(());
    }
    #[derive(serde::Deserialize, serde::Serialize)]
    struct LiveProgressSnapshot {
        #[serde(default = "default_live_progress_schema")]
        schema: String,
        total_lines: u64,
        updated_at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_line_preview: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_stream: Option<String>,
    }

    fn default_live_progress_schema() -> String {
        LIVE_PROGRESS_RECORD_SCHEMA_V1.to_string()
    }

    let path = Path::new(&request.paths.artifacts_dir).join(LIVE_PROGRESS_FILE_NAME);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating progress dir: {}", parent.display()))?;
    }
    let mut snapshot = if path.exists() {
        let existing: LiveProgressSnapshot =
            serde_json::from_str(&fs::read_to_string(&path).with_context(|| {
                format!("failed reading live progress file: {}", path.display())
            })?)
            .with_context(|| format!("invalid live progress json: {}", path.display()))?;
        if existing.schema != LIVE_PROGRESS_RECORD_SCHEMA_V1 {
            bail!(
                "unsupported live progress schema '{}' at {} (expected '{}')",
                existing.schema,
                path.display(),
                LIVE_PROGRESS_RECORD_SCHEMA_V1
            );
        }
        existing
    } else {
        LiveProgressSnapshot {
            schema: default_live_progress_schema(),
            total_lines: 0,
            updated_at_ms: 0,
            last_line_preview: None,
            last_stream: None,
        }
    };
    snapshot.total_lines = snapshot.total_lines.saturating_add(progress.delta_lines);
    if !progress.stream.trim().is_empty() {
        snapshot.last_stream = Some(progress.stream.trim().to_string());
    }
    if let Some(preview) = progress
        .line_preview
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        snapshot.last_line_preview = Some(preview);
    }
    snapshot.updated_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    fs::write(
        &path,
        serde_json::to_vec(&snapshot).context("failed serializing live progress")?,
    )
    .with_context(|| format!("failed writing live progress: {}", path.display()))?;
    Ok(())
}

pub(crate) fn join_stderr(handle: thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}

pub(crate) fn child_has_exited(child: &Arc<Mutex<Child>>) -> Result<bool> {
    Ok(child
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .try_wait()?
        .is_some())
}

pub(crate) fn wait_for_exit(child: &Arc<Mutex<Child>>, deadline: Instant) -> Result<bool> {
    loop {
        if child_has_exited(child)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn terminate_child(child: &Arc<Mutex<Child>>, grace: Duration) -> Result<()> {
    terminate_process(child, grace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use transist_protocol::{
        AdapterKind, InvokePaths, InvokeStep, TEP_INVOKE_REQUEST_TYPE_V1, TEP_PROTOCOL_VERSION_V1,
    };

    #[test]
    fn handshake_receive_valid_line() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok("handshake\n".to_string())).unwrap();
        assert_eq!(
            recv_line(&rx, Instant::now(), "handshake response").unwrap(),
            "handshake\n"
        );
    }

    #[test]
    fn handshake_receive_buffered_line_before_eof() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok("handshake\n".to_string())).unwrap();
        drop(tx);
        assert_eq!(
            recv_line(&rx, Instant::now(), "handshake response").unwrap(),
            "handshake\n"
        );
    }

    #[test]
    fn handshake_receive_expired_deadline() {
        let (_tx, rx) = mpsc::channel();
        let error = recv_line(
            &rx,
            Instant::now() - Duration::from_secs(1),
            "handshake response",
        )
        .unwrap_err();
        assert!(matches!(error, HandshakeReceiveError::Timeout { .. }));
        assert_eq!(
            error.to_string(),
            "timed out waiting for handshake response"
        );
    }

    #[test]
    fn handshake_receive_eof() {
        let (tx, rx) = mpsc::channel();
        drop(tx);
        let error = recv_line(
            &rx,
            Instant::now() + Duration::from_secs(30),
            "handshake response",
        )
        .unwrap_err();
        assert!(matches!(error, HandshakeReceiveError::Disconnected { .. }));
        assert_eq!(
            error.to_string(),
            "plugin stdout closed before handshake response"
        );
    }

    #[test]
    fn handshake_receive_reader_failure() {
        let (tx, rx) = mpsc::channel();
        let diagnostic = "failed reading plugin stdout: synthetic read failure";
        tx.send(Err(anyhow!(diagnostic))).unwrap();
        let error = recv_line(&rx, Instant::now(), "handshake response").unwrap_err();
        assert!(matches!(error, HandshakeReceiveError::ReadError(_)));
        assert_eq!(error.to_string(), diagnostic);
        assert!(!error.to_string().contains("timed out"));
        assert!(std::error::Error::source(&error).is_some());
    }

    fn sample_request(max_timeout_ms: Option<u64>) -> ExecutionRequest {
        ExecutionRequest {
            message_type: TEP_INVOKE_REQUEST_TYPE_V1.to_string(),
            protocol_version: TEP_PROTOCOL_VERSION_V1.to_string(),
            invoke_id: "invoke-1".to_string(),
            run_id: "run-1".to_string(),
            workflow_id: "wf".to_string(),
            step: InvokeStep {
                id: "step-1".to_string(),
                execution: 1,
            },
            adapter_kind: AdapterKind::Process,
            action: "exec".to_string(),
            input: serde_json::json!({}),
            paths: InvokePaths {
                run_root: "/tmp/run".to_string(),
                artifacts_dir: "/tmp/artifacts".to_string(),
                evidence_dir: "/tmp/evidence".to_string(),
                workspace_dir: "/tmp/workspace".to_string(),
            },
            requested_capabilities: Vec::new(),
            timeout_ms: Some(5_000),
            max_timeout_ms,
            policy: serde_json::json!({}),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn recv_invoke_result_honors_max_timeout() {
        let (_tx, rx) = mpsc::channel::<Result<String>>();
        let request = sample_request(Some(25));
        let result = recv_invoke_result(&rx, Duration::from_secs(10), Some(25), &request);
        match result {
            Err(InvokeResultReceiveError::NoTerminalFrame { message }) => {
                assert!(message.contains("max wall-clock timeout"));
            }
            other => panic!("expected max-timeout failure, got {other:?}"),
        }
    }

    #[test]
    fn declared_workspace_is_the_cwd_and_rejects_package_subdirectories() {
        let plugin_dir = Path::new("/tmp/plugin");
        let mut request = sample_request(None);
        request.policy["workspace_declared"] = serde_json::Value::Bool(true);

        assert_eq!(
            resolve_execution_cwd(&request, plugin_dir, None).unwrap(),
            Path::new("/tmp/workspace")
        );
        assert_eq!(
            resolve_execution_cwd(&request, plugin_dir, Some(".")).unwrap(),
            Path::new("/tmp/workspace")
        );
        let error = resolve_execution_cwd(&request, plugin_dir, Some("support"))
            .expect_err("package cwd must not override a declared workspace");
        assert!(
            error
                .to_string()
                .contains("entrypoint.cwd is not supported")
        );
    }

    #[test]
    fn legacy_request_preserves_package_local_cwd() {
        let request = sample_request(None);
        assert_eq!(
            resolve_execution_cwd(&request, Path::new("/tmp/plugin"), Some("support")).unwrap(),
            Path::new("/tmp/plugin/support")
        );
    }
}
