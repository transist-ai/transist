// SPDX-License-Identifier: MPL-2.0
//! Per-invocation state used by the built-in adapters to coordinate
//! cancellation between the executing thread and the cancel API.
//!
//! Process and MCP adapters share [`LocalProcess`] (a child handle plus a
//! checked termination boundary); HTTP needs a richer state machine that also
//! tracks request finalization and the configuration required to issue the
//! adapter-specific cancel call. The two types remain distinct on purpose —
//! folding them into a generic `ActiveInvocation<T>` would save little code
//! while making each adapter's cancellation semantics harder to read.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use transist_protocol::CanonicalErrorCode;

use crate::http_runtime::HttpRequestPolicy;
use crate::mcp_runtime::{McpStdin, send_mcp_notification};

/// Owns the live cancellation boundary, including serialized termination.
/// On supported Unix hosts the unreaped leader anchors the PGID until every
/// group member exits. Only confirmed teardown releases that anchor.
#[derive(Debug)]
pub(crate) struct LocalProcess {
    child: Arc<Mutex<Child>>,
    #[cfg(target_os = "linux")]
    birth: String,
    #[cfg(target_os = "macos")]
    birth: Option<String>,
    #[cfg(target_os = "linux")]
    anchor: crate::os_process::LinuxProcessAnchor,
    #[cfg(target_os = "linux")]
    targets: Mutex<BTreeMap<u32, LinuxGroupMember>>,
    terminated: AtomicBool,
}

impl LocalProcess {
    /// Observes numeric lifecycle evidence under the same unreaped identity lock
    /// used for termination. This snapshot never authorizes a later signal.
    pub(crate) fn lifecycle_snapshot(&self) -> anyhow::Result<serde_json::Value> {
        let child = self.child.lock().unwrap_or_else(|error| error.into_inner());
        if self.terminated.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(
                serde_json::json!({"leader_alive": false, "live_pids": [], "live_pgids": []}),
            );
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.validate_leader(&child)?;
            #[cfg(target_os = "linux")]
            let leader_alive = !self.anchor.snapshot()?.exited;
            #[cfg(target_os = "macos")]
            let leader_alive = !crate::os_process::child_exited_unreaped(child.id())?;
            let mut pids = crate::os_process::live_process_group_members(child.id() as i32)?;
            if leader_alive && !pids.contains(&child.id()) {
                pids.push(child.id());
            }
            pids.sort_unstable();
            let pgids = if pids.is_empty() {
                vec![]
            } else {
                vec![child.id()]
            };
            Ok(
                serde_json::json!({"leader_alive": leader_alive, "live_pids": pids, "live_pgids": pgids}),
            )
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("process lifecycle snapshots are unsupported on this platform")
    }

    /// Captures the child's birth before any thread can reap it. The caller must
    /// spawn it with configure_child_process_group; only this owner may reap it.
    pub(crate) fn new(child: Arc<Mutex<Child>>) -> anyhow::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let identity = {
            let mut guard = child.lock().unwrap_or_else(|error| error.into_inner());
            #[cfg(target_os = "linux")]
            let identity = (|| -> anyhow::Result<_> {
                let anchor = crate::os_process::LinuxProcessAnchor::open(guard.id())?;
                let snapshot = anchor.snapshot()?;
                if snapshot.pgid != guard.id() {
                    anyhow::bail!("spawned local process is not its group leader");
                }
                let _probe = crate::os_process::PidFd::open(guard.id())?;
                Ok((snapshot.birth, anchor))
            })();
            #[cfg(target_os = "macos")]
            let identity = crate::os_process::process_birth_token(guard.id())
                .map(Some)
                .or_else(|_error| -> anyhow::Result<Option<String>> {
                    // Some sandboxed children expose no BSD birth report. A
                    // successful WNOWAIT query proves our child relationship
                    // for both live and exited children; it never fabricates a
                    // birth token and never accepts an already-reaped PID.
                    crate::os_process::child_exited_unreaped(guard.id())?;
                    Ok(None)
                })
                .and_then(
                    |birth| match crate::os_process::process_group(guard.id() as i32) {
                        Ok(group) if group == guard.id() as i32 => Ok(birth),
                        Err(error) if macos_unreaped_anchor(guard.id(), &error)? => Ok(birth),
                        Err(error) => Err(error),
                        Ok(_) => anyhow::bail!("spawned local process is not its group leader"),
                    },
                );
            match identity {
                Ok(identity) => identity,
                Err(error) => {
                    // This is still our unreaped, freshly spawned child. Refuse
                    // dispatch on capture failure. Its unreaped PID cannot be
                    // reused; check group membership without opening any file.
                    if crate::os_process::process_group(guard.id() as i32)? == guard.id() as i32 {
                        ensure_local_signal(
                            crate::os_process::signal_process_group(
                                guard.id() as i32,
                                libc::SIGKILL,
                            ),
                            "spawn teardown SIGKILL",
                        )?;
                    } else {
                        guard.kill()?;
                    }
                    guard.wait()?;
                    return Err(error);
                }
            }
        };
        Ok(Self {
            child,
            #[cfg(target_os = "linux")]
            birth: identity.0,
            #[cfg(target_os = "macos")]
            birth: identity,
            #[cfg(target_os = "linux")]
            anchor: identity.1,
            #[cfg(target_os = "linux")]
            targets: Mutex::new(BTreeMap::new()),
            terminated: AtomicBool::new(false),
        })
    }

    /// Observes exit without releasing the PGID anchor while descendants live.
    pub(crate) fn has_exited(&self) -> anyhow::Result<bool> {
        let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
        if self.terminated.load(std::sync::atomic::Ordering::SeqCst) {
            drop(child);
            return crate::child_has_exited(&self.child);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.validate_leader(&child)?;
            if !self.boundary_exited(child.id())? {
                return Ok(false);
            }
            self.finish_teardown(&mut child)?;
            Ok(true)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let exited = child.try_wait()?.is_some();
            self.terminated
                .store(exited, std::sync::atomic::Ordering::SeqCst);
            Ok(exited)
        }
    }

    /// Waits for confirmed exit of the local boundary up to the supplied deadline.
    pub(crate) fn wait_for_exit(&self, deadline: std::time::Instant) -> anyhow::Result<bool> {
        loop {
            if self.has_exited()? {
                return Ok(true);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Gives payload members grace, then kills and confirms the entire boundary.
    /// Concurrent cancel/failure cleanup shares the child lock and cannot reap
    /// the supervisor or send a second signal sequence during the first grace.
    pub(crate) fn terminate(&self, grace: std::time::Duration) -> anyhow::Result<()> {
        let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
        if self.terminated.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.validate_leader(&child)?;
            match self.graceful_teardown(child.id(), grace) {
                Ok(true) => self.finish_teardown(&mut child),
                Ok(false) => self.force_teardown(&mut child, grace),
                Err(error) => {
                    // Discovery/descriptor exhaustion must never bypass KILL.
                    // The pinned anchor can be checked without allocating an FD.
                    // Retain the original error even if forced exit is confirmed.
                    match self.force_teardown(&mut child, grace) {
                        Ok(()) => Err(error
                            .context("forced teardown confirmed after graceful teardown failure")),
                        Err(teardown) => {
                            Err(error
                                .context(format!("forced teardown not confirmed: {teardown:#}")))
                        }
                    }
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
            self.terminated
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn validate_leader(&self, child: &Child) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        let birth_matches = {
            let snapshot = self.anchor.snapshot()?;
            snapshot.birth == self.birth && snapshot.pgid == child.id()
        };
        #[cfg(target_os = "macos")]
        let birth_matches = match &self.birth {
            Some(expected) => match crate::os_process::process_birth_token(child.id()) {
                Ok(birth) => expected == &birth,
                Err(_) => {
                    // The kernel child relationship still pins the same PID
                    // lifetime when libproc metadata is unavailable.
                    crate::os_process::child_exited_unreaped(child.id())?;
                    true
                }
            },
            None => {
                crate::os_process::child_exited_unreaped(child.id())?;
                true
            }
        };
        if !birth_matches {
            anyhow::bail!("local process leader birth or group identity changed");
        }
        match crate::os_process::process_group(child.id() as i32) {
            Ok(group) if group == child.id() as i32 => {}
            #[cfg(target_os = "macos")]
            Err(error) if macos_unreaped_anchor(child.id(), &error)? => {
                // getpgid also excludes macOS zombies. WNOWAIT proves that our
                // still-unreaped child reserves this spawn-assigned PGID.
            }
            Err(error) => return Err(error),
            Ok(_) => anyhow::bail!("local process leader birth or group identity changed"),
        }
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn finish_teardown(&self, child: &mut Child) -> anyhow::Result<()> {
        child.wait()?;
        self.terminated
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn boundary_exited(&self, pgid: u32) -> anyhow::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            let mut targets = self
                .targets
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            prune_linux_targets(&mut targets)?;
            if !targets.is_empty() || !self.anchor.snapshot()?.exited {
                return Ok(false);
            }
        }
        #[cfg(target_os = "macos")]
        if !crate::os_process::child_exited_unreaped(pgid)? {
            // Group enumeration may briefly omit an exiting child. Do not
            // block in wait/reap until its exit event is actually available.
            return Ok(false);
        }
        Ok(!crate::os_process::process_group_has_live_members(
            pgid as i32,
        )?)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn graceful_teardown(&self, pgid: u32, grace: std::time::Duration) -> anyhow::Result<bool> {
        #[cfg(target_os = "macos")]
        {
            if self.boundary_exited(pgid)? {
                return Ok(true);
            }
            ensure_local_signal(
                crate::os_process::signal_process_group(pgid as i32, libc::SIGTERM),
                "SIGTERM",
            )?;
        }
        let deadline = std::time::Instant::now() + grace;
        loop {
            #[cfg(target_os = "linux")]
            {
                // Preserve the Bubblewrap supervisor during grace. Snapshots
                // reuse live pidfds and close exited handles before discovery.
                let mut targets = self
                    .targets
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                prune_linux_targets(&mut targets)?;
                linux_group_members(pgid, &mut targets)?;
            }
            if self.boundary_exited(pgid)? {
                return Ok(true);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn force_teardown(&self, child: &mut Child, grace: std::time::Duration) -> anyhow::Result<()> {
        let group_signal = self.validate_leader(child).and_then(|()| {
            ensure_local_signal(
                crate::os_process::signal_process_group(child.id() as i32, libc::SIGKILL),
                "SIGKILL",
            )
        });
        // Attempt every captured stable target even if another signal failed.
        #[cfg(target_os = "linux")]
        let mut signal_error = group_signal.err();
        #[cfg(target_os = "macos")]
        let signal_error = group_signal.err();
        #[cfg(target_os = "linux")]
        for member in self
            .targets
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
        {
            if let Err(error) =
                ensure_local_signal(member.pidfd.signal(libc::SIGKILL), "pidfd SIGKILL")
            {
                signal_error.get_or_insert(error);
            }
        }
        let deadline = std::time::Instant::now() + grace;
        loop {
            let exited = self.boundary_exited(child.id()).map_err(|error| {
                if let Some(signal_error) = &signal_error {
                    error.context(format!("force signalling also failed: {signal_error:#}"))
                } else {
                    error
                }
            })?;
            if exited {
                self.finish_teardown(child)?;
                return signal_error.map_or(Ok(()), Err);
            }
            if std::time::Instant::now() >= deadline {
                return Err(signal_error.unwrap_or_else(|| {
                    anyhow::anyhow!("local process boundary remained live after SIGKILL")
                }));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_unreaped_anchor(pid: u32, _error: &anyhow::Error) -> anyhow::Result<bool> {
    // BSD/group metadata can disappear before the exit event becomes waitable.
    // A successful WNOWAIT query still proves the unreaped child relationship,
    // even with no exit event yet. That lifetime reserves its spawn-assigned
    // numeric PGID; an unrelated or already-reaped PID instead returns an error.
    crate::os_process::child_exited_unreaped(pid)?;
    Ok(true)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ensure_local_signal(
    status: crate::os_process::SignalStatus,
    signal: &str,
) -> anyhow::Result<()> {
    match status {
        crate::os_process::SignalStatus::Sent | crate::os_process::SignalStatus::Absent => Ok(()),
        status => anyhow::bail!("{signal} was not delivered: {status:?}"),
    }
}

// At most this many live, observed targets plus one leader anchor and three
// transient discovery FDs per invocation. Exceeding the budget forces teardown.
#[cfg(target_os = "linux")]
const MAX_LINUX_TARGETS: usize = 128;

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxGroupMember {
    birth: String,
    pidfd: crate::os_process::PidFd,
}

#[cfg(target_os = "linux")]
fn prune_linux_targets(targets: &mut BTreeMap<u32, LinuxGroupMember>) -> anyhow::Result<()> {
    let mut failure = None;
    targets.retain(|_, member| match member.pidfd.has_exited() {
        Ok(exited) => !exited,
        Err(error) => {
            failure.get_or_insert(error);
            true
        }
    });
    failure.map_or(Ok(()), |error| Err(error.into()))
}

/// Snapshots live members through pidfds, rechecking birth and group after open.
/// Exited /proc entries are a normal race; every other observation error fails.
#[cfg(target_os = "linux")]
fn linux_group_members(
    pgid: u32,
    targets: &mut BTreeMap<u32, LinuxGroupMember>,
) -> anyhow::Result<()> {
    let pids = std::fs::read_dir("/proc")?.filter_map(|entry| match entry {
        Ok(entry) => entry
            .file_name()
            .to_string_lossy()
            .parse::<u32>()
            .ok()
            .map(Ok),
        Err(error) => Some(Err(error.into())),
    });
    visit_linux_group(
        pids,
        pgid,
        |pid| crate::os_process::process_group(pid as i32),
        |pid| {
            if pid == pgid {
                return Ok(());
            }
            // Pin the proc directory as well: start ticks alone can collide when a
            // PID is reused within one clock tick. A pinned old proc directory goes
            // away with its task instead of resolving to a reused numeric PID.
            let directory = match std::fs::File::open(format!("/proc/{pid}")) {
                Ok(directory) => directory,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ESRCH) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };
            let Some(birth) = linux_member_birth(&directory, pgid)? else {
                return Ok(());
            };
            if let Some(previous) = targets.get(&pid) {
                if !previous.pidfd.has_exited()? {
                    if previous.birth != birth {
                        anyhow::bail!("live pidfd target birth changed");
                    }
                    return Ok(());
                }
            }
            targets.remove(&pid);
            if targets.len() >= MAX_LINUX_TARGETS {
                anyhow::bail!("local process pidfd target budget exceeded");
            }
            let pidfd = match crate::os_process::PidFd::open(pid) {
                Ok(pidfd) => pidfd,
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            if linux_member_birth(&directory, pgid)?.as_ref() == Some(&birth) {
                // Retain the stable handle before signalling, including on failure.
                targets.insert(pid, LinuxGroupMember { birth, pidfd });
                ensure_local_signal(targets[&pid].pidfd.signal(libc::SIGTERM), "pidfd SIGTERM")?;
            }
            Ok(())
        },
    )
}

/// Prefilters with getpgid before any procfs read. Only disappearance races are
/// skipped; permission and identity errors for actual members remain failures.
#[cfg(target_os = "linux")]
fn visit_linux_group<I, G, V>(pids: I, pgid: u32, mut group: G, mut visit: V) -> anyhow::Result<()>
where
    I: IntoIterator<Item = anyhow::Result<u32>>,
    G: FnMut(u32) -> anyhow::Result<i32>,
    V: FnMut(u32) -> anyhow::Result<()>,
{
    for pid in pids {
        let pid = pid?;
        match group(pid) {
            Ok(actual) if actual == pgid as i32 => visit(pid)?,
            Ok(_) => {}
            Err(error)
                if error
                    .root_cause()
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.raw_os_error() == Some(libc::ESRCH)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_member_birth(directory: &std::fs::File, pgid: u32) -> anyhow::Result<Option<String>> {
    use std::os::fd::AsRawFd;
    let raw = match std::fs::read_to_string(format!("/proc/self/fd/{}/stat", directory.as_raw_fd()))
    {
        Ok(raw) => raw,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let end = raw
        .rfind(')')
        .ok_or_else(|| anyhow::anyhow!("invalid process stat"))?;
    let fields: Vec<_> = raw[end + 1..].split_whitespace().collect();
    if fields.len() < 20 {
        anyhow::bail!("incomplete process stat");
    }
    if matches!(fields[0], "Z" | "X") || fields[2].parse::<u32>()? != pgid {
        return Ok(None);
    }
    Ok(Some(format!("linux:{}", fields[19].parse::<u64>()?)))
}

#[derive(Debug)]
pub(crate) struct ProcessInvocationState {
    pub(crate) process: LocalProcess,
    pub(crate) cancelled: AtomicBool,
}

/// MCP adapter invocation state, including the transport required to cancel a
/// currently active tool request before its process is terminated.
#[derive(Debug)]
pub(crate) struct McpInvocationState {
    pub(crate) process: LocalProcess,
    pub(crate) cancelled: AtomicBool,
    pub(crate) stdin: Arc<McpStdin>,
    pub(crate) active_request_id: Mutex<Option<u64>>,
}

impl McpInvocationState {
    pub(crate) fn set_active_request_id(&self, request_id: u64) {
        *self
            .active_request_id
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(request_id);
    }

    pub(crate) fn clear_active_request_id(&self) {
        *self
            .active_request_id
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    pub(crate) fn cancel_active_tool_call(&self) {
        let request_id = *self
            .active_request_id
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(request_id) = request_id {
            let _ = send_mcp_notification(
                &self.stdin,
                "notifications/cancelled",
                serde_json::json!({"requestId": request_id}),
            );
        }
        self.stdin.close();
    }
}

#[derive(Debug)]
pub(crate) struct HttpInvocationState {
    pub(crate) cancel_requested: AtomicBool,
    pub(crate) cancelled: AtomicBool,
    pub(crate) cancel_finalized: AtomicBool,
    pub(crate) cancel_failure: Mutex<Option<(CanonicalErrorCode, String)>>,
    pub(crate) cancel_config: HttpCancelConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct HttpCancelConfig {
    pub(crate) base_url: String,
    pub(crate) ca_cert: Option<PathBuf>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) policy: HttpRequestPolicy,
    pub(crate) invoke_id: String,
}

/// Registry of active adapter invocations keyed by `invoke_id`.
///
/// Each adapter keeps an independent registry whose value type captures
/// transport-specific cancellation state. The registry only owns the
/// bookkeeping (mutex + map) so callers can register, look up, and remove
/// entries without re-implementing the same poison-recovery / locking dance.
#[derive(Debug)]
pub(crate) struct ActiveInvocations<T> {
    inner: Arc<Mutex<BTreeMap<String, Arc<T>>>>,
}

impl<T> Clone for ActiveInvocations<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> Default for ActiveInvocations<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl<T> ActiveInvocations<T> {
    pub(crate) fn register(&self, invoke_id: String, active: Arc<T>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(invoke_id, active);
    }

    pub(crate) fn remove(&self, invoke_id: &str) -> Option<Arc<T>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(invoke_id)
    }

    pub(crate) fn get(&self, invoke_id: &str) -> Option<Arc<T>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(invoke_id)
            .cloned()
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    struct ProcessFixture {
        process: Arc<LocalProcess>,
        term: PathBuf,
        _root: tempfile::TempDir,
    }

    impl Drop for ProcessFixture {
        fn drop(&mut self) {
            let _ = self.process.terminate(Duration::ZERO);
        }
    }

    fn process_fixture(exit_supervisor: bool) -> ProcessFixture {
        process_fixture_with_churn(exit_supervisor, false)
    }

    fn process_fixture_with_churn(exit_supervisor: bool, churn: bool) -> ProcessFixture {
        let root = tempfile::tempdir().expect("fixture root");
        let term = root.path().join("term");
        let payload = r#"import os, signal, sys, time
from pathlib import Path
term = Path(sys.argv[1])
count = 0
def handle_term(signum, frame):
    global count
    count += 1
    term.write_text(str(count))
signal.signal(signal.SIGTERM, handle_term)
print(os.getpid(), flush=True)
while True:
    if sys.argv[2] == 'churn':
        pid = os.fork()
        if pid == 0:
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            time.sleep(0.02)
            os._exit(0)
        while True:
            try:
                if os.waitpid(-1, os.WNOHANG)[0] == 0:
                    break
            except ChildProcessError:
                break
        time.sleep(0.003)
    else:
        time.sleep(0.01)
"#;
        let supervisor = r#"import subprocess, sys, time
from pathlib import Path
child = subprocess.Popen([sys.executable, '-c', sys.argv[1], sys.argv[2], sys.argv[4]])
if sys.argv[3] == 'exit':
    time.sleep(0.1)
else:
    child.wait()
"#;
        let mut command = Command::new("python3");
        command
            .args(["-c", supervisor, payload])
            .arg(&term)
            .arg(if exit_supervisor { "exit" } else { "wait" })
            .arg(if churn { "churn" } else { "wait" })
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        crate::configure_child_process_group(&mut command);
        let mut child = command.spawn().expect("spawn supervisor");
        let stdout = child.stdout.take().expect("stdout");
        let process = Arc::new(
            LocalProcess::new(Arc::new(Mutex::new(child))).expect("capture leader identity"),
        );
        let mut ready = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut ready)
            .expect("payload readiness");
        ready.trim().parse::<u32>().expect("ready payload pid");
        ProcessFixture {
            process,
            term,
            _root: root,
        }
    }

    #[test]
    fn concurrent_termination_shares_one_grace_and_signal_sequence() {
        let fixture = process_fixture(false);
        let grace = Duration::from_millis(crate::DEFAULT_TERMINATION_GRACE_MS);
        let start = Instant::now();
        let first = Arc::clone(&fixture.process);
        let first = std::thread::spawn(move || first.terminate(grace));
        fixture.process.terminate(grace).expect("second cancel");
        first
            .join()
            .expect("join first cancel")
            .expect("first cancel");
        assert!(start.elapsed() >= grace);
        assert_eq!(
            std::fs::read_to_string(&fixture.term).expect("SIGTERM"),
            "1"
        );
        assert!(fixture.process.has_exited().expect("confirmed group exit"));
    }

    #[test]
    fn exited_supervisor_does_not_hide_a_live_group_member() {
        let fixture = process_fixture(true);
        let pid = fixture.process.child.lock().expect("child").id();
        let deadline = Instant::now() + Duration::from_secs(5);
        #[cfg(target_os = "linux")]
        let exited = || crate::os_process::process_is_zombie(pid);
        #[cfg(target_os = "macos")]
        let exited = || crate::os_process::child_exited_unreaped(pid).map_err(anyhow::Error::from);
        while !exited().expect("supervisor is unreaped") {
            assert!(Instant::now() < deadline, "supervisor did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!fixture.process.has_exited().expect("group observation"));
        fixture
            .process
            .terminate(Duration::from_millis(crate::DEFAULT_TERMINATION_GRACE_MS))
            .expect("kill surviving member");
        assert!(fixture.process.has_exited().expect("confirmed group exit"));
        assert_eq!(
            std::fs::read_to_string(&fixture.term).expect("SIGTERM"),
            "1"
        );
    }

    #[test]
    fn leader_birth_mismatch_refuses_signals() {
        let mut fixture = process_fixture(false);
        let process = Arc::get_mut(&mut fixture.process).expect("exclusive fixture");
        let birth = process.birth.clone();
        #[cfg(target_os = "linux")]
        {
            process.birth = "different birth".to_string();
        }
        #[cfg(target_os = "macos")]
        {
            process.birth = Some("different birth".to_string());
        }
        let error = process
            .terminate(Duration::ZERO)
            .expect_err("identity mismatch");
        process.birth = birth;
        assert!(error.to_string().contains("identity changed"));
        assert!(
            !fixture.term.exists(),
            "unverified targets must not receive SIGTERM"
        );
    }

    #[test]
    fn natural_exit_then_repeated_teardown_never_revalidates_reaped_pid() {
        let mut command = Command::new("python3");
        command.args(["-c", "pass"]).stdin(Stdio::null());
        crate::configure_child_process_group(&mut command);
        let child = command.spawn().expect("spawn short-lived leader");
        #[cfg(target_os = "macos")]
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !crate::os_process::child_exited_unreaped(child.id())
                .expect("non-reaping exit probe")
            {
                assert!(Instant::now() < deadline, "short-lived child did not exit");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let mut process = LocalProcess::new(Arc::new(Mutex::new(child))).expect("capture identity");
        #[cfg(target_os = "macos")]
        assert!(
            process.birth.is_none(),
            "an exited child must use its kernel anchor, not a fabricated birth"
        );
        assert!(
            process
                .wait_for_exit(Instant::now() + Duration::from_secs(5))
                .expect("natural exit")
        );
        // A completed boundary must not even inspect the old numeric PID again.
        #[cfg(target_os = "linux")]
        {
            process.birth = "reaped identity must not be consulted".to_string();
        }
        #[cfg(target_os = "macos")]
        {
            process.birth = Some("reaped identity must not be consulted".to_string());
        }
        process
            .terminate(Duration::ZERO)
            .expect("first cleanup after exit");
        process
            .terminate(Duration::ZERO)
            .expect("repeated cleanup after exit");
        assert!(process.has_exited().expect("cached completion"));
    }

    #[test]
    fn forced_teardown_then_repeated_cleanup_is_idempotent() {
        let fixture = process_fixture(false);
        fixture
            .process
            .terminate(Duration::from_millis(crate::DEFAULT_TERMINATION_GRACE_MS))
            .expect("first forced cancellation");
        fixture
            .process
            .terminate(Duration::ZERO)
            .expect("failure-path cleanup after cancel");
        fixture
            .process
            .terminate(Duration::ZERO)
            .expect("repeated cancel");
        assert!(fixture.process.has_exited().expect("confirmed exit"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_child_without_bsd_metadata_keeps_its_kernel_identity_anchor() {
        let mut fixture = process_fixture(false);
        let process = Arc::get_mut(&mut fixture.process).expect("exclusive fixture");
        process.birth = None;
        assert!(!process.has_exited().expect("owned live child"));
        process
            .terminate(Duration::from_millis(crate::DEFAULT_TERMINATION_GRACE_MS))
            .expect("terminate the same owned process group");
        assert!(process.has_exited().expect("confirmed group exit"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kernel_identity_fallback_refuses_an_externally_reaped_child() {
        let mut command = Command::new("python3");
        command.args(["-c", "pass"]).stdin(Stdio::null());
        crate::configure_child_process_group(&mut command);
        let child = command.spawn().expect("spawn child");
        let mut process = LocalProcess::new(Arc::new(Mutex::new(child))).expect("owned child");
        process.birth = None;
        process
            .child
            .lock()
            .expect("child")
            .wait()
            .expect("external reap");
        assert!(
            process.terminate(Duration::ZERO).is_err(),
            "a reaped numeric PID must never authorize signalling"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inaccessible_foreign_proc_is_not_opened_but_member_errors_propagate() {
        let mut opened = Vec::new();
        visit_linux_group(
            [Ok(1), Ok(2)],
            77,
            |pid| Ok(if pid == 1 { 88 } else { 77 }),
            |pid| {
                opened.push(pid);
                if pid == 1 {
                    return Err(std::io::Error::from_raw_os_error(libc::EACCES).into());
                }
                Ok(())
            },
        )
        .expect("unrelated hidepid-style denial must not affect our group");
        assert_eq!(opened, [2]);
        let error = visit_linux_group(
            [Ok(2)],
            77,
            |_| Ok(77),
            |_| Err(std::io::Error::from_raw_os_error(libc::EACCES).into()),
        )
        .expect_err("actual member inspection must fail closed");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(libc::EACCES)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn constrained_descriptors_force_safely_and_churn_does_not_accumulate_handles() {
        const CASE: &str = "TRANSIST_CANCELLATION_FD_TEST_CHILD";
        if let Ok(case) = std::env::var(CASE) {
            let fixture = process_fixture_with_churn(false, case == "churn");
            crate::os_process::set_test_fd_limit(48).expect("limit only this subprocess");
            let grace = Duration::from_millis(crate::DEFAULT_TERMINATION_GRACE_MS);
            if case == "churn" {
                let start = Instant::now();
                fixture
                    .process
                    .terminate(grace)
                    .expect("bounded pidfd reuse under churn");
                assert!(
                    start.elapsed() >= grace,
                    "resource leak must not short-circuit grace"
                );
                assert!(
                    fixture.process.targets.lock().unwrap().is_empty(),
                    "exited pidfds must be pruned"
                );
            } else {
                let mut bank = Vec::new();
                loop {
                    match std::fs::File::open("/dev/null") {
                        Ok(file) => bank.push(file),
                        Err(error) => {
                            assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                            break;
                        }
                    }
                }
                let error = fixture
                    .process
                    .terminate(grace)
                    .expect_err("exhaustion preserves error/evidence");
                assert_eq!(
                    error
                        .root_cause()
                        .downcast_ref::<std::io::Error>()
                        .expect("preserved allocation error")
                        .raw_os_error(),
                    Some(libc::EMFILE)
                );
                assert!(
                    !fixture
                        .process
                        .terminated
                        .load(std::sync::atomic::Ordering::SeqCst),
                    "unconfirmed exit must keep its anchor"
                );
                let deadline = Instant::now() + Duration::from_secs(5);
                while !fixture
                    .process
                    .anchor
                    .snapshot()
                    .expect("allocation-free pinned probe")
                    .exited
                {
                    assert!(Instant::now() < deadline, "FD exhaustion bypassed SIGKILL");
                    std::thread::sleep(Duration::from_millis(10));
                }
                drop(bank);
                fixture
                    .process
                    .terminate(grace)
                    .expect("confirm/reap after resources return");
            }
            assert!(fixture.process.has_exited().expect("confirmed group exit"));
            return;
        }
        for case in ["exhaust", "churn"] {
            let output = Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "active_invocation::tests::constrained_descriptors_force_safely_and_churn_does_not_accumulate_handles", "--nocapture", "--test-threads=1"])
                .env(CASE, case)
                .output().expect("isolated RLIMIT subprocess");
            assert!(
                output.status.success(),
                "{case}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
