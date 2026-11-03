// SPDX-License-Identifier: MPL-2.0
//! Detached runner for Transist runs.
//!
//! Owns the "any TUI can manage any run" boundary: when the user invokes a
//! mutating command (`transist runs start`, `transist workflows run`, `transist runs retry`,
//! `transist runs cleanup`, and gate decisions), the foreground process spawns
//! a detached child via `fork` + `setsid` + `exec`. The child re-execs the
//! same `transist` binary with a hidden `--runner` flag and runs the kernel
//! headlessly. Its stdio is redirected to a log file: for new runs, a
//! bootstrap log under `<home>/.transist-runner-bootstrap/` is used until
//! run birth succeeds, then moved to `<run-root>/runner.log`; for existing
//! runs (resume/cleanup), the log is appended directly in the run root.
//! The foreground process returns immediately after printing
//! `{ run_id, runner_pid }`, optionally auto-attaching a poll-based viewer
//! when stdout is a TTY.
//!
//! The detached child is the parent of all plugin processes for the duration
//! of the run, holds the lease, and exits cleanly when the run reaches a
//! terminal/blocked state. Any other CLI/TUI process can cancel or decide
//! gates against the run via the durable on-disk control channel
//! (`<home>/control/runs/<root>/cancel-request.json` and the gate decision
//! files persisted by `record_gate_decision`); the detached runner re-enters
//! the run on its own when those files appear.

// SAFETY: OS-integration: detached runner uses fork/close/waitpid via libc.
#![allow(unsafe_code)]

#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, anyhow};
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::io::{IntoRawFd, RawFd};
use std::path::{Path, PathBuf};

use crate::cli::CliUiMode;
use crate::prompt::PromptUi;

/// Filename under `<run-root>/` that captures the detached runner's stdio.
pub(crate) const RUNNER_LOG_FILENAME: &str = "runner.log";

/// Directory used for runner bootstrap logs before the run root is claimed.
/// Keeps the log file out of the final run-root path so the parent process
/// does not pre-create the run root before `create_run_staged` runs.
const RUNNER_BOOTSTRAP_DIR: &str = ".transist-runner-bootstrap";

/// Returns the bootstrap log path for a run that does not yet have a locator.
fn runner_bootstrap_log_path(home_dir: &Path, run_id: &str) -> PathBuf {
    home_dir
        .join(RUNNER_BOOTSTRAP_DIR)
        .join(format!("run_{run_id}"))
        .join(RUNNER_LOG_FILENAME)
}

/// What kind of mutating action the runner should perform on the run.
#[derive(Debug, Clone)]
pub(crate) enum RunnerAction {
    /// Continue an existing started run (`transist runs start <id>`).
    Run {
        run_id: String,
        trusted_execution: bool,
        local_only: bool,
    },
    /// Resume a failed run (`transist runs resume <id>` / `transist runs retry <id>`).
    Resume {
        run_id: String,
        trusted_execution: bool,
        local_only: bool,
    },
    /// Continue cleanup (`transist runs cleanup <id>`).
    Cleanup {
        run_id: String,
        trusted_execution: bool,
        local_only: bool,
    },
    /// Replay a previously recorded run into a fresh run id.
    Replay {
        source_run_id: String,
        replay_run_id: String,
        trusted_execution: bool,
        local_only: bool,
    },
}

impl RunnerAction {
    /// The run id the foreground announces and the viewer attaches to. For
    /// replay this is the *new* (replay) run id, not the source.
    pub(crate) fn run_id(&self) -> &str {
        match self {
            Self::Run { run_id, .. }
            | Self::Resume { run_id, .. }
            | Self::Cleanup { run_id, .. } => run_id,
            Self::Replay { replay_run_id, .. } => replay_run_id,
        }
    }
}

/// Fork + setsid + exec a headless `transist runs start --runner <id>` (or the
/// equivalent for resume / cleanup). Returns the runner's PID.
///
/// On Unix this performs a `fork()`. The child:
/// 1. Calls `setsid()` so it leaves the controlling terminal.
/// 2. Redirects stdin to `/dev/null` and stdout/stderr to
///    `<run-root>/runner.log` (created if absent, append mode).
/// 3. `execv`s the current binary (`/proc/self/exe` or `argv[0]`) with the
///    appropriate `--runner` arguments.
///
/// The parent returns the child PID immediately. If the child fails to exec it
/// will exit with non-zero; the parent does not wait for it.
#[cfg(unix)]
pub(crate) fn spawn_detached_runner(
    home_dir: &Path,
    repo_root: &Path,
    action: &RunnerAction,
) -> Result<u32> {
    use std::ffi::OsString;

    let exe = std::env::current_exe().context("resolving current executable for runner spawn")?;
    let log_path = runner_log_path(home_dir, action.run_id());
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating runner log dir {}", parent.display()))?;
    }

    // Build the argv we'll execv.
    let mut argv: Vec<OsString> = vec![
        exe.clone().into_os_string(),
        // Globals
        OsString::from("--home"),
        home_dir.as_os_str().to_os_string(),
        OsString::from("--ui"),
        OsString::from("plain"),
    ];
    // Subcommand
    match action {
        RunnerAction::Run {
            run_id,
            trusted_execution,
            local_only,
        } => {
            argv.push(OsString::from("runs"));
            argv.push(OsString::from("start"));
            if *trusted_execution {
                argv.push(OsString::from("--trusted-execution"));
            }
            if *local_only {
                argv.push(OsString::from("--local-only"));
            }
            argv.push(OsString::from("--runner"));
            argv.push(OsString::from(run_id));
        }
        RunnerAction::Resume {
            run_id,
            trusted_execution,
            local_only,
        } => {
            argv.push(OsString::from("runs"));
            argv.push(OsString::from("retry"));
            if *trusted_execution {
                argv.push(OsString::from("--trusted-execution"));
            }
            if *local_only {
                argv.push(OsString::from("--local-only"));
            }
            argv.push(OsString::from("--runner"));
            argv.push(OsString::from(run_id));
        }
        RunnerAction::Cleanup {
            run_id,
            trusted_execution,
            local_only,
        } => {
            argv.push(OsString::from("runs"));
            argv.push(OsString::from("cleanup"));
            if *trusted_execution {
                argv.push(OsString::from("--trusted-execution"));
            }
            if *local_only {
                argv.push(OsString::from("--local-only"));
            }
            argv.push(OsString::from("--runner"));
            argv.push(OsString::from(run_id));
        }
        RunnerAction::Replay {
            source_run_id,
            replay_run_id,
            trusted_execution,
            local_only,
        } => {
            argv.push(OsString::from("runs"));
            argv.push(OsString::from("replay"));
            if *trusted_execution {
                argv.push(OsString::from("--trusted-execution"));
            }
            if *local_only {
                argv.push(OsString::from("--local-only"));
            }
            argv.push(OsString::from("--runner"));
            argv.push(OsString::from("--run-ref"));
            argv.push(OsString::from(replay_run_id));
            argv.push(OsString::from(source_run_id));
        }
    }

    // Convert argv to CStrings up-front (parent side, fail before fork).
    let c_argv: Vec<CString> = argv
        .into_iter()
        .map(|os| {
            let bytes = os.into_string().map_err(|_| {
                anyhow!("runner argv contains non-UTF-8 bytes; refusing to spawn detached runner")
            })?;
            CString::new(bytes).context("runner argv contains an interior NUL byte")
        })
        .collect::<Result<_>>()?;
    let c_argv_ptrs: Vec<*const libc::c_char> = c_argv
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let c_exe = CString::new(exe.as_os_str().as_encoded_bytes())
        .context("current_exe path contains a NUL byte")?;
    let mut env_map = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        env_map.insert(key, value);
    }
    for (key, value) in transist::detached_runner_management_env()? {
        env_map.insert(key.into(), value.into());
    }
    let c_env: Vec<CString> = env_map
        .into_iter()
        .map(|(key, value)| {
            let key = key.into_string().map_err(|_| {
                anyhow!("runner env key contains non-UTF-8 bytes; refusing to spawn detached runner")
            })?;
            let value = value.into_string().map_err(|_| {
                anyhow!(
                    "runner env value for '{}' contains non-UTF-8 bytes; refusing to spawn detached runner",
                    key
                )
            })?;
            CString::new(format!("{key}={value}"))
                .context("runner env contains an interior NUL byte")
        })
        .collect::<Result<_>>()?;
    let c_env_ptrs: Vec<*const libc::c_char> = c_env
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // Pre-open the log fd in the parent so we can dup2 it in the child without
    // any allocator activity between fork and exec.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening runner log {}", log_path.display()))?;
    let log_fd: RawFd = log_file.into_raw_fd();

    // Pre-open /dev/null for stdin.
    let dev_null = std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/null")
        .context("opening /dev/null for runner stdin")?;
    let null_fd: RawFd = dev_null.into_raw_fd();

    // Suppress unused warnings in non-relevant repo_root context (we may want
    // it later if we cwd into repo_root inside the child).
    let _ = repo_root;

    // SAFETY: fork() is async-signal-safe; the child performs only
    // async-signal-safe operations (dup2, close, setsid, execv) before exec.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let err = std::io::Error::last_os_error();
        // Close the fds we opened; the parent owns them now.
        let _ = close_fd(log_fd);
        let _ = close_fd(null_fd);
        return Err(anyhow!("fork() failed: {err}"));
    }

    if pid == 0 {
        // ---- CHILD ----
        // Detach from controlling terminal.
        // SAFETY: in the post-fork child; setsid/dup2/close are
        // async-signal-safe and any failure calls child_setup_failed
        // which _exit's without unwinding.
        unsafe {
            if libc::setsid() < 0 {
                child_setup_failed();
            }
            // Redirect stdio.
            if libc::dup2(null_fd, 0) < 0 {
                child_setup_failed();
            }
            if libc::dup2(log_fd, 1) < 0 {
                child_setup_failed();
            }
            if libc::dup2(log_fd, 2) < 0 {
                child_setup_failed();
            }
            // Close the original fds; they are duplicated onto 0/1/2 now.
            if null_fd > 2 && libc::close(null_fd) < 0 {
                child_setup_failed();
            }
            if log_fd > 2 && libc::close(log_fd) < 0 {
                child_setup_failed();
            }
            // Exec the runner.
            libc::execve(c_exe.as_ptr(), c_argv_ptrs.as_ptr(), c_env_ptrs.as_ptr());
            child_setup_failed();
        }
        // Unreachable; libc::_exit never returns.
        #[allow(unreachable_code)]
        std::process::exit(127);
    }

    // ---- PARENT ----
    // Close fds in the parent (they are inherited & dup'd in the child).
    close_fd(log_fd).context("closing detached runner log fd in parent")?;
    close_fd(null_fd).context("closing detached runner stdin fd in parent")?;
    Ok(pid as u32)
}

#[cfg(unix)]
fn close_fd(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: the caller owns `fd` and the result is checked before returning.
    if unsafe { libc::close(fd) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Terminates the post-fork child without invoking allocation or unwinding.
///
/// The child may only execute async-signal-safe operations after `fork`; a
/// fixed `write` plus `_exit` keeps setup failures from continuing into exec.
#[cfg(unix)]
// SAFETY: this function is only called in the post-fork child before exec;
// `write` and `_exit` are async-signal-safe per POSIX.
unsafe fn child_setup_failed() -> ! {
    let message = b"transist: detached runner child setup failed\n";
    // SAFETY: stderr is an inherited file descriptor and write/_exit are
    // async-signal-safe. Any write error is intentionally ignored here.
    unsafe {
        let _ = libc::write(2, message.as_ptr().cast(), message.len());
        libc::_exit(127);
    }
}

/// Non-Unix stub: detached spawn is currently Unix-only. Falls back to the
/// in-process execution path on platforms that don't support `fork`+`setsid`.
#[cfg(not(unix))]
pub(crate) fn spawn_detached_runner(
    _home_dir: &Path,
    _repo_root: &Path,
    _action: &RunnerAction,
) -> Result<u32> {
    Err(anyhow!(
        "detached runner mode is unix-only; transist v1+ requires a unix host"
    ))
}

/// Best-effort tail of the runner's stdio log, used to surface dispatch
/// failures in the TUI status line. Reads up to `max_bytes` from the end of
/// the file (default: 4 KiB) and returns the last `max_lines` non-empty
/// lines joined by `; `. Returns an empty string when the log is missing or
/// unreadable — the caller is responsible for falling back to a generic
/// message.
pub(crate) fn tail_runner_log(home_dir: &Path, run_id: &str, max_lines: usize) -> String {
    let path = runner_log_path(home_dir, run_id);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return String::new();
    };
    let len = metadata.len();
    let max_bytes: u64 = 4096;
    let start = len.saturating_sub(max_bytes);
    let Ok(mut file) = std::fs::File::open(&path) else {
        return String::new();
    };
    use std::io::{Read, Seek, SeekFrom};
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::with_capacity((len - start) as usize);
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        // Skip the runner_event JSON envelopes (`{"schema":"transist.runner_event.v1",…}`)
        // because they reproduce the run status without explaining the failure.
        .filter(|line| !line.contains("\"schema\":\"transist.runner_event.v1\""))
        .collect();
    if lines.len() > max_lines {
        let skip = lines.len() - max_lines;
        lines.drain(..skip);
    }
    lines.join("; ")
}

/// Snapshot the current size of a runner's log file. Used as a "spawn marker"
/// by `wait_for_runner_decision`: any bytes appended past this offset belong
/// to the spawn we just started. Returns 0 when the log is missing — the
/// child will create it on first write.
pub(crate) fn runner_log_offset(home_dir: &Path, run_id: &str) -> u64 {
    let path = runner_log_path(home_dir, run_id);
    std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
}

/// Scan the runner log for failure-signal lines appended after `start_offset`.
/// A failure signal is any non-empty line that is NOT a `transist.runner_event.v1`
/// JSON envelope — in practice, the kernel prints `Error: command dispatch failed`
/// (and an `anyhow` `Caused by:` chain) to stderr before exiting non-zero, which
/// lands in the runner log.
///
/// Returns the joined failure lines (`; `-separated, last `max_lines` kept) when
/// a signal is found, or an empty string otherwise.
pub(crate) fn runner_log_failure_signal_since(
    home_dir: &Path,
    run_id: &str,
    start_offset: u64,
    max_lines: usize,
) -> String {
    let path = runner_log_path(home_dir, run_id);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return String::new();
    };
    let len = metadata.len();
    if len <= start_offset {
        return String::new();
    }
    let Ok(mut file) = std::fs::File::open(&path) else {
        return String::new();
    };
    use std::io::{Read, Seek, SeekFrom};
    if file.seek(SeekFrom::Start(start_offset)).is_err() {
        return String::new();
    }
    let mut buf = Vec::with_capacity((len - start_offset) as usize);
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains("\"schema\":\"transist.runner_event.v1\""))
        .collect();
    if lines.len() > max_lines {
        let skip = lines.len() - max_lines;
        lines.drain(..skip);
    }
    lines.join("; ")
}

/// Outcome of waiting for the detached runner to declare a verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerEarlyVerdict {
    /// Runner exited with the given code (signal-terminated children are
    /// reported as `128 + signo`).
    Exited(i32),
    /// Runner wrote a non-event line to its log — typically a kernel
    /// `Error: command dispatch failed` chain. The child is about to exit but
    /// may not have been reaped yet. Caller should treat this as a failure.
    FailureSignal(String),
    /// Timeout elapsed with no exit and no failure signal — the runner is
    /// healthy and still running.
    StillRunning,
}

/// Wait for the detached runner to either exit OR write a failure-signal line
/// to its log. Used by the foreground TUI to catch early dispatch failures
/// even when the runner takes longer than the modal-acceptable wait budget to
/// actually exit (debug builds can take 1.5–3s on a warm host).
///
/// `start_offset` should be captured BEFORE the runner is spawned so we only
/// observe content this spawn appended.
///
/// Returns `Ok(verdict)` once any signal fires or `timeout` elapses, or `Err`
/// only if `waitpid` itself fails unexpectedly.
#[cfg(unix)]
pub(crate) fn wait_for_runner_decision(
    pid: u32,
    home_dir: &Path,
    run_id: &str,
    start_offset: u64,
    timeout: std::time::Duration,
) -> Result<RunnerEarlyVerdict> {
    use std::thread::sleep;
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    let poll_interval = std::time::Duration::from_millis(25);
    loop {
        // 1. Check waitpid first — authoritative when the child has actually exited.
        let mut status: libc::c_int = 0;
        // SAFETY: waitpid with WNOHANG returns immediately; we own no
        // mutable state shared with the child.
        let rc = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if rc == pid as libc::pid_t {
            if libc::WIFEXITED(status) {
                return Ok(RunnerEarlyVerdict::Exited(libc::WEXITSTATUS(status)));
            }
            if libc::WIFSIGNALED(status) {
                return Ok(RunnerEarlyVerdict::Exited(128 + libc::WTERMSIG(status)));
            }
            // Stopped/continued — fall through to log check + sleep.
        } else if rc < 0 {
            let err = std::io::Error::last_os_error();
            let raw = err.raw_os_error().unwrap_or(0);
            if raw == libc::ECHILD {
                // Already reaped by something else; we cannot recover the
                // exit code. Still check the log for a failure signal — the
                // tail tells the same story even without the exit status.
                let tail = runner_log_failure_signal_since(home_dir, run_id, start_offset, 3);
                if !tail.is_empty() {
                    return Ok(RunnerEarlyVerdict::FailureSignal(tail));
                }
                return Ok(RunnerEarlyVerdict::StillRunning);
            }
            if raw != libc::EINTR {
                return Err(anyhow!("waitpid({pid}) failed: {err}"));
            }
        }

        // 2. Check log for failure-signal lines — fires earlier than waitpid
        //    on slow debug builds because the kernel prints the error chain
        //    *before* the process exit syscall.
        let tail = runner_log_failure_signal_since(home_dir, run_id, start_offset, 3);
        if !tail.is_empty() {
            return Ok(RunnerEarlyVerdict::FailureSignal(tail));
        }

        if Instant::now() >= deadline {
            return Ok(RunnerEarlyVerdict::StillRunning);
        }
        sleep(poll_interval);
    }
}

#[cfg(not(unix))]
pub(crate) fn wait_for_runner_decision(
    _pid: u32,
    _home_dir: &Path,
    _run_id: &str,
    _start_offset: u64,
    _timeout: std::time::Duration,
) -> Result<RunnerEarlyVerdict> {
    Ok(RunnerEarlyVerdict::StillRunning)
}

/// Returns the `<run-root>/runner.log` path for a run.
///
/// When the run root cannot be resolved (e.g. the run hasn't been created yet),
/// falls back to a bootstrap directory that does NOT coincide with the final
/// run root, so the parent process does not pre-create the run root before
/// `create_run_staged` claims it. After run birth, the runner moves the
/// bootstrap log into the run root.
pub(crate) fn runner_log_path(home_dir: &Path, run_id: &str) -> PathBuf {
    if let Ok(root) = transist::TransistContext::with_home(home_dir, home_dir).run_root(run_id) {
        return root.join(RUNNER_LOG_FILENAME);
    }
    runner_bootstrap_log_path(home_dir, run_id)
}

/// Spawn a detached runner, emit the standard spawn announcement, and
/// attach the run viewer when the active prompt does not support inline
/// attach.
pub(crate) fn spawn_and_attach(
    context: &transist::TransistContext,
    prompt: &mut dyn PromptUi,
    action: &RunnerAction,
    ui_mode: CliUiMode,
) -> Result<u32> {
    let pid = spawn_detached_runner(context.home_dir(), context.repo_root(), action)?;
    let run_id = action.run_id();
    announce_spawned_runner(run_id, pid, &runner_log_path(context.home_dir(), run_id));
    if !prompt.attach_run_viewer_inline(context, run_id)? {
        crate::ui::watch::maybe_attach_run_viewer(context, run_id, ui_mode)?;
    }
    Ok(pid)
}

/// Headless run loop used by the detached runner process. Calls the kernel
/// directly and exits when the run reaches a terminal/blocked state. Emits one
/// JSON line per major state transition to stdout (which is the runner.log).
///
/// This function:
/// - does not spawn a worker thread (the kernel call runs on this process's
///   main thread);
/// - does not draw any TUI;
/// - does not install a Ctrl-C handler (cancellation comes through the durable
///   `cancel-request.json` file consumed by the kernel's control watcher).
pub(crate) fn run_runner_loop(
    context: &transist::TransistContext,
    action: &RunnerAction,
) -> Result<()> {
    emit_runner_event("started", action.run_id(), None);
    let report = match action {
        RunnerAction::Run {
            run_id,
            trusted_execution,
            local_only,
        } => {
            let report =
                context.run_started_run_with_trust(run_id, *trusted_execution, *local_only)?;
            relocate_bootstrap_log(context.home_dir(), run_id);
            report
        }
        RunnerAction::Resume {
            run_id,
            trusted_execution,
            local_only,
        } => context.runs_resume_with_trust(run_id, *trusted_execution, *local_only)?,
        RunnerAction::Cleanup {
            run_id,
            trusted_execution,
            local_only,
        } => context.runs_cleanup_with_trust(run_id, *trusted_execution, *local_only)?,
        RunnerAction::Replay {
            source_run_id,
            replay_run_id,
            trusted_execution,
            local_only,
        } => {
            let report = context.runs_replay_with_trust(
                source_run_id,
                Some(replay_run_id),
                *trusted_execution,
                *local_only,
            )?;
            relocate_bootstrap_log(context.home_dir(), replay_run_id);
            report
        }
    };
    emit_runner_event(
        "finished",
        action.run_id(),
        Some(serde_json::json!({
            "status": report.status,
            "run_id": report.run_id,
            "lease_release_warning": report.lease_release_warning,
        })),
    );
    Ok(())
}

/// Moves the bootstrap log file into the run root after run birth succeeds.
///
/// On Unix, the file descriptor opened by the parent remains valid after the
/// file is renamed, so the runner's stdout/stderr continue to flow to the
/// same fd (now at the canonical `<run-root>/runner.log` path).
fn relocate_bootstrap_log(home_dir: &Path, run_id: &str) {
    let bootstrap = runner_bootstrap_log_path(home_dir, run_id);
    if !bootstrap.exists() {
        return;
    }
    if let Ok(run_root) = transist::TransistContext::with_home(home_dir, home_dir).run_root(run_id)
    {
        let final_log = run_root.join(RUNNER_LOG_FILENAME);
        if final_log == bootstrap {
            return;
        }
        if let Err(err) = std::fs::rename(&bootstrap, &final_log) {
            // If rename fails (e.g. cross-device), copy then remove as fallback.
            if std::fs::copy(&bootstrap, &final_log).is_ok() {
                let _ = std::fs::remove_file(&bootstrap);
            } else {
                eprintln!("transist: warning: failed to relocate bootstrap runner log: {err}");
            }
        }
    }
}

fn emit_runner_event(event: &str, run_id: &str, extra: Option<serde_json::Value>) {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "schema".to_string(),
        serde_json::Value::String("transist.runner_event.v1".to_string()),
    );
    payload.insert(
        "event".to_string(),
        serde_json::Value::String(event.to_string()),
    );
    payload.insert(
        "run_id".to_string(),
        serde_json::Value::String(run_id.to_string()),
    );
    payload.insert(
        "pid".to_string(),
        serde_json::Value::Number(serde_json::Number::from(std::process::id())),
    );
    if let Some(serde_json::Value::Object(map)) = extra {
        for (k, v) in map {
            payload.insert(k, v);
        }
    }
    let line = serde_json::Value::Object(payload).to_string();
    println!("{line}");
}

/// Print the foreground "spawned runner" announcement on stdout in a
/// human-friendly + machine-readable form.
///
/// **Suppressed when called from inside an active fullscreen TUI** — in
/// that case, both `eprintln!` and `println!` would write directly into
/// the alt-screen buffer and corrupt the live frame (one of the symptoms
/// of the run-detail "freeze" bug). The runner.log on disk still records
/// the spawn for diagnostics, so suppression here loses no operator
/// information.
pub(crate) fn announce_spawned_runner(run_id: &str, runner_pid: u32, log_path: &Path) {
    if crate::ui::fullscreen::fullscreen_prompt_active() {
        return;
    }
    let payload = serde_json::json!({
        "schema": "transist.runner_spawned.v1",
        "run_id": run_id,
        "runner_pid": runner_pid,
        "runner_log": log_path,
    });
    eprintln!(
        "transist: spawned detached runner pid={runner_pid} for run {run_id}\n  logs: {}",
        log_path.display()
    );
    println!("{payload}");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    /// A runner that exits non-zero
    /// within the wait window (e.g. kernel rejects the dispatch because the
    /// root run is terminal) must be observed via `Exited` so the TUI can
    /// surface a status message instead of silently leaving the modal up.
    #[test]
    fn wait_for_runner_decision_catches_subsecond_exit() {
        // /bin/sh -c "exit 1" — exits ~immediately with code 1. We use
        // std::process::Command (not raw fork) so the test stays robust on
        // hosts with strict syscall sandboxes; then std::mem::forget the
        // Child so the std runtime does not reap it before waitpid sees it.
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 1")
            .spawn()
            .expect("spawn /bin/sh -c 'exit 1'");
        let pid = child.id();
        std::mem::forget(child);

        let tmp = tempfile::tempdir().expect("tempdir");
        let verdict = wait_for_runner_decision(
            pid,
            tmp.path(),
            "no-such-run",
            0,
            Duration::from_millis(2000),
        )
        .expect("decision should not error");
        assert_eq!(
            verdict,
            RunnerEarlyVerdict::Exited(1),
            "expected /bin/sh to exit with code 1 within the wait window"
        );
    }

    /// Healthy path: a child that outlives the timeout *and* writes no
    /// failure-signal lines returns `StillRunning`, so the TUI falls
    /// through to the optimistic "Spawned detached runner" status message
    /// instead of misreporting a failure.
    #[test]
    fn wait_for_runner_decision_returns_still_running_for_live_silent_child() {
        // sleep 5 — comfortably outlives our 200ms test budget and writes
        // nothing to the (non-existent) runner log.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .spawn()
            .expect("spawn /bin/sh -c 'sleep 5'");
        let pid = child.id();

        let tmp = tempfile::tempdir().expect("tempdir");
        let verdict = wait_for_runner_decision(
            pid,
            tmp.path(),
            "no-such-run",
            0,
            Duration::from_millis(200),
        )
        .expect("decision should not error");
        assert_eq!(
            verdict,
            RunnerEarlyVerdict::StillRunning,
            "live silent child should not be reported as failed within 200ms"
        );

        // Clean up — kill + reap so we do not leak a sleep process.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Failure-signal path: even if the runner has NOT yet exited (debug
    /// builds can take 1.5–3s to actually exit after writing the kernel
    /// error chain), the presence of a non-event line in `runner.log` past
    /// `start_offset` lets us surface the failure within the modal-acceptable
    /// wait budget. This covers the failure signal that the prior
    /// waitpid-only path could miss before the wait window closed.
    #[test]
    fn wait_for_runner_decision_catches_log_failure_signal_before_exit() {
        use std::io::Write;
        use transist_storage_local::run_paths::{plan_run_locator, write_run_locator};

        // Spawn a still-running child so waitpid will NOT fire within the
        // wait window — the decision must come from the log content.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .spawn()
            .expect("spawn /bin/sh -c 'sleep 5'");
        let pid = child.id();

        let tmp = tempfile::tempdir().expect("tempdir");
        let run_id = "test-run-failure-signal";
        // Write a locator so runner_log_path resolves to <run-root>/runner.log.
        let locator = plan_run_locator(tmp.path(), run_id, None).expect("plan locator");
        write_run_locator(tmp.path(), &locator).expect("write locator");
        let log_path = runner_log_path(tmp.path(), run_id);
        std::fs::create_dir_all(log_path.parent().unwrap()).expect("mkdir log parent");

        // Pre-populate the log with a benign event envelope (simulating a
        // prior spawn of the same run id) and capture the offset.
        let mut log = std::fs::File::create(&log_path).expect("create log");
        writeln!(
            log,
            "{{\"schema\":\"transist.runner_event.v1\",\"event\":\"finished\"}}"
        )
        .expect("write prior event");
        log.sync_all().expect("sync");
        drop(log);
        let start_offset = std::fs::metadata(&log_path).expect("stat").len();

        // Now append an `Error:` line as if the kernel rejected dispatch.
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .expect("reopen log");
        writeln!(log, "Error: command dispatch failed").expect("write error");
        log.sync_all().expect("sync");
        drop(log);

        let verdict = wait_for_runner_decision(
            pid,
            tmp.path(),
            run_id,
            start_offset,
            Duration::from_millis(500),
        )
        .expect("decision should not error");
        match &verdict {
            RunnerEarlyVerdict::FailureSignal(tail) => {
                assert!(
                    tail.contains("command dispatch failed"),
                    "expected failure-signal tail to contain the kernel error, got: {tail}"
                );
            }
            other => panic!("expected FailureSignal, got {other:?}"),
        }

        let _ = child.kill();
        let _ = child.wait();
    }

    /// The pre-spawn `start_offset` must hide failure lines that belong to a
    /// PRIOR spawn of the same run id — otherwise the TUI would falsely
    /// attribute historical errors to a healthy current spawn.
    #[test]
    fn wait_for_runner_decision_ignores_log_lines_before_offset() {
        use transist_storage_local::run_paths::{plan_run_locator, write_run_locator};

        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .spawn()
            .expect("spawn /bin/sh -c 'sleep 5'");
        let pid = child.id();

        let tmp = tempfile::tempdir().expect("tempdir");
        let run_id = "test-run-stale-error";
        // Write a locator so runner_log_path resolves to <run-root>/runner.log.
        let locator = plan_run_locator(tmp.path(), run_id, None).expect("plan locator");
        write_run_locator(tmp.path(), &locator).expect("write locator");
        let log_path = runner_log_path(tmp.path(), run_id);
        std::fs::create_dir_all(log_path.parent().unwrap()).expect("mkdir log parent");

        // Pre-existing error line from an earlier spawn.
        std::fs::write(&log_path, "Error: stale dispatch failure from earlier\n")
            .expect("seed stale error");
        let start_offset = std::fs::metadata(&log_path).expect("stat").len();

        // Healthy current spawn: nothing appended past start_offset.
        let verdict = wait_for_runner_decision(
            pid,
            tmp.path(),
            run_id,
            start_offset,
            Duration::from_millis(150),
        )
        .expect("decision should not error");
        assert_eq!(
            verdict,
            RunnerEarlyVerdict::StillRunning,
            "stale error from before start_offset must not trigger FailureSignal"
        );

        let _ = child.kill();
        let _ = child.wait();
    }
}
