// SPDX-License-Identifier: MPL-2.0
//! Checked OS process helpers used by adapter spawn and cancellation paths.

// SAFETY: OS-integration: checked process signaling and identity helpers via libc syscalls.
#![allow(unsafe_code)]

use anyhow::Context;
use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use anyhow::bail;

/// Result of probing or signalling a process target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignalStatus {
    /// The signal was accepted by the kernel.
    Sent,
    /// No process or process group exists.
    Absent,
    /// A process exists but this executor lacks permission to signal it.
    PermissionDenied,
    /// The OS rejected the operation for another reason.
    Failed { errno: i32, message: String },
}

#[cfg(unix)]
fn status_from_errno() -> SignalStatus {
    let error = std::io::Error::last_os_error();
    let errno = error.raw_os_error().unwrap_or(0);
    match errno {
        libc::ESRCH => SignalStatus::Absent,
        libc::EPERM => SignalStatus::PermissionDenied,
        _ => SignalStatus::Failed {
            errno,
            message: error.to_string(),
        },
    }
}

/// Probes a PID without sending a signal.
#[cfg(unix)]
pub(crate) fn probe_pid(pid: libc::pid_t) -> SignalStatus {
    // SAFETY: kill with signal 0 does not deliver a signal.
    if unsafe { libc::kill(pid, 0) } == 0 {
        SignalStatus::Sent
    } else {
        status_from_errno()
    }
}

/// Sends `signal` to one PID and preserves the exact OS failure class.
#[cfg(unix)]
pub(crate) fn signal_pid(pid: libc::pid_t, signal: libc::c_int) -> SignalStatus {
    // SAFETY: the raw pid and signal are validated by the caller's record and
    // this helper returns the OS result without assuming delivery succeeded.
    if unsafe { libc::kill(pid, signal) } == 0 {
        SignalStatus::Sent
    } else {
        status_from_errno()
    }
}

/// Sends `signal` to one process group and preserves the exact OS failure class.
#[cfg(unix)]
pub(crate) fn signal_process_group(pgid: libc::pid_t, signal: libc::c_int) -> SignalStatus {
    // SAFETY: killpg targets only the supplied process group; failures are
    // returned to the caller rather than being treated as successful delivery.
    if unsafe { libc::killpg(pgid, signal) } == 0 {
        SignalStatus::Sent
    } else {
        status_from_errno()
    }
}

/// Probes a process group without delivering a signal.
///
/// Linux and macOS retain exited child processes as zombies until their parent
/// reaps them. A zombie still makes `killpg(pgid, 0)` succeed, but it cannot
/// perform more work and must not make cancellation report a timeout.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn probe_process_group(pgid: libc::pid_t) -> SignalStatus {
    match process_group_has_live_members(pgid) {
        Ok(true) => SignalStatus::Sent,
        Ok(false) => SignalStatus::Absent,
        Err(error) => signal_status_from_error(&error),
    }
}

/// Probes a process group without delivering a signal.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn probe_process_group(pgid: libc::pid_t) -> SignalStatus {
    signal_process_group(pgid, 0)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn signal_status_from_error(error: &anyhow::Error) -> SignalStatus {
    let errno = error
        .root_cause()
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        .unwrap_or(0);
    match errno {
        libc::ESRCH => SignalStatus::Absent,
        libc::EPERM => SignalStatus::PermissionDenied,
        _ => SignalStatus::Failed {
            errno,
            message: error.to_string(),
        },
    }
}

/// Returns the current process group for `pid`, preserving errno on failure.
#[cfg(unix)]
pub(crate) fn process_group(pid: libc::pid_t) -> Result<libc::pid_t> {
    // SAFETY: getpgid only inspects the supplied PID.
    let pgid = unsafe { libc::getpgid(pid) };
    if pgid >= 0 {
        Ok(pgid)
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

/// Configures a spawned child as the leader of a new process group.
#[cfg(unix)]
pub(crate) fn set_own_process_group() -> std::io::Result<()> {
    // SAFETY: setpgid(0, 0) affects only the calling child before exec.
    if unsafe { libc::setpgid(0, 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Confirms that `pid` is an exited, still-unreaped child of this process.
/// WNOWAIT preserves the kernel PID/group anchor. This does not signal or reap;
/// macOS proc_pidinfo cannot supply birth/state information for such zombies.
/// A successful `Ok(false)` also proves ownership of a still-live child. An
/// already-reaped or unrelated PID returns an error, not a false ownership proof.
#[cfg(target_os = "macos")]
pub(crate) fn child_exited_unreaped(pid: u32) -> std::io::Result<bool> {
    // SAFETY: siginfo_t contains only C scalars/pointers; zero is a valid initial
    // value and also represents no report for the nonblocking wait.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: waitid initializes this correctly sized buffer. P_PID addresses
    // only our supplied child, and WNOWAIT explicitly forbids reaping it.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(info.si_pid == pid as i32)
    }
}

/// Owned Linux process handle: signals and exit probes address the captured
/// task, not a potentially reused numeric PID. Descriptors are close-on-exec.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct PidFd(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl PidFd {
    /// Opens a stable handle for `pid`; returns the exact allocation/OS error.
    pub(crate) fn open(pid: u32) -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: pidfd_open takes a PID and flags=0 and returns a new descriptor
        // or -1. It neither signals nor changes the supplied process.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as i32, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful pidfd_open returned this new owned descriptor.
        Ok(Self(unsafe {
            std::os::fd::OwnedFd::from_raw_fd(fd as i32)
        }))
    }

    /// Sends `signal` to the captured task, preserving the OS failure class.
    pub(crate) fn signal(&self, signal: i32) -> SignalStatus {
        use std::os::fd::AsRawFd;
        // SAFETY: this owned pidfd is valid; null siginfo and flags=0 request a
        // normal signal to that exact task without numeric PID lookup.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == 0 {
            SignalStatus::Sent
        } else {
            status_from_errno()
        }
    }

    /// Nonblockingly checks whether the captured task exited, including zombies.
    pub(crate) fn has_exited(&self) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd;
        let mut descriptor = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: poll inspects one initialized descriptor with timeout=0.
            let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
            if result >= 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if descriptor.revents & (libc::POLLNVAL | libc::POLLERR) != 0 {
            return Err(std::io::Error::other("invalid process pidfd exit probe"));
        }
        Ok(descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0)
    }
}

/// Pinned Linux leader stat file. Revalidation needs no new descriptor even
/// under EMFILE/ENFILE; a reaped/reused numeric PID cannot replace this inode.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct LinuxProcessAnchor {
    stat: std::sync::Mutex<std::fs::File>,
}

/// Current birth, group, and exit state read from a pinned Linux process file.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct LinuxProcessSnapshot {
    pub(crate) birth: String,
    pub(crate) pgid: u32,
    pub(crate) exited: bool,
}

#[cfg(target_os = "linux")]
impl LinuxProcessAnchor {
    /// Pins the stat inode for the still-unreaped spawned leader `pid`.
    pub(crate) fn open(pid: u32) -> Result<Self> {
        Ok(Self {
            stat: std::sync::Mutex::new(std::fs::File::open(format!("/proc/{pid}/stat"))?),
        })
    }

    /// Reads the pinned identity/state without allocating a descriptor. Serializes
    /// the seek/read pair internally; the file offset is the only side effect.
    pub(crate) fn snapshot(&self) -> Result<LinuxProcessSnapshot> {
        use std::io::{Read, Seek};
        let mut stat = self.stat.lock().unwrap_or_else(|error| error.into_inner());
        stat.rewind()?;
        let mut raw = String::new();
        stat.read_to_string(&mut raw)?;
        let end = raw.rfind(')').context("invalid pinned process stat")?;
        let fields: Vec<_> = raw[end + 1..].split_whitespace().collect();
        if fields.len() < 20 {
            anyhow::bail!("incomplete pinned process stat");
        }
        Ok(LinuxProcessSnapshot {
            birth: format!("linux:{}", fields[19].parse::<u64>()?),
            pgid: fields[2].parse()?,
            exited: matches!(fields[0], "Z" | "X"),
        })
    }
}

/// Changes only the calling test subprocess's soft descriptor limit. Never
/// called by the parent test runner; preserves the existing hard limit.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn set_test_fd_limit(limit: u64) -> std::io::Result<()> {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit initializes this correctly sized, writable structure.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    current.rlim_cur = limit.min(current.rlim_max);
    // SAFETY: setrlimit affects only the calling subprocess and the value does
    // not exceed its existing hard limit.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &current) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Returns an opaque OS process-birth token for PID-reuse detection.
#[cfg(target_os = "linux")]
/// Returns the opaque OS birth identity for a live process.
pub fn process_birth_token(pid: u32) -> Result<String> {
    let path = format!("/proc/{pid}/stat");
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading process birth identity at {path}"))?;
    let end_comm = raw
        .rfind(')')
        .ok_or_else(|| anyhow::anyhow!("invalid process stat record at {path}"))?;
    let start_ticks = raw[end_comm + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow::anyhow!("missing process start time at {path}"))?;
    let start_ticks = start_ticks
        .parse::<u64>()
        .map_err(|error| anyhow::anyhow!("invalid process start time at {path}: {error}"))?;
    Ok(format!("linux:{start_ticks}"))
}

/// Returns an opaque OS process-birth token for PID-reuse detection.
#[cfg(target_os = "macos")]
/// Returns the opaque OS birth identity for a live process.
pub fn process_birth_token(pid: u32) -> Result<String> {
    let info = process_bsdinfo(pid)?;
    Ok(format!(
        "macos:{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

/// Returns whether a process remains present only as an unreaped zombie.
#[cfg(target_os = "linux")]
pub(crate) fn process_is_zombie(pid: u32) -> Result<bool> {
    let path = format!("/proc/{pid}/stat");
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading process state at {path}"))?;
    let end_comm = raw
        .rfind(')')
        .ok_or_else(|| anyhow::anyhow!("invalid process stat record at {path}"))?;
    let state = raw[end_comm + 1..]
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing process state at {path}"))?;
    Ok(state == "Z")
}

/// Returns whether a process remains present only as an unreaped zombie.
#[cfg(target_os = "macos")]
pub(crate) fn process_is_zombie(pid: u32) -> Result<bool> {
    Ok(process_bsdinfo(pid)?.pbi_status == libc::SZOMB)
}

/// Returns whether a process group still has a member that has not exited.
#[cfg(target_os = "linux")]
pub(crate) fn process_group_has_live_members(pgid: libc::pid_t) -> Result<bool> {
    Ok(!live_process_group_members(pgid)?.is_empty())
}

/// Lists live numeric group members only; never reads arguments or environment.
#[cfg(target_os = "linux")]
pub(crate) fn live_process_group_members(pgid: libc::pid_t) -> Result<Vec<u32>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
            continue;
        };
        match process_group(pid) {
            Ok(member_pgid) if member_pgid == pgid => match process_is_zombie(pid as u32) {
                Ok(false) => members.push(pid as u32),
                Ok(true) => {}
                // A process can exit while `/proc` is being enumerated.
                Err(error) if error_is_absent(&error) => {}
                Err(error) => return Err(error),
            },
            Ok(_) => {}
            // A process can exit while `/proc` is being enumerated.
            Err(error) if error_is_absent(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(members)
}

/// Returns whether a process group still has a member that has not exited.
#[cfg(target_os = "macos")]
pub(crate) fn process_group_has_live_members(pgid: libc::pid_t) -> Result<bool> {
    Ok(!live_process_group_members(pgid)?.is_empty())
}

/// Lists live numeric group members only; never reads arguments or environment.
#[cfg(target_os = "macos")]
pub(crate) fn live_process_group_members(pgid: libc::pid_t) -> Result<Vec<u32>> {
    // `PROC_PGRP_ONLY` is declared by libproc.h but is not exposed by libc.
    const PROC_PGRP_ONLY: u32 = 2;
    let byte_count = {
        // SAFETY: a null buffer with size zero asks libproc for the required size.
        unsafe { libc::proc_listpids(PROC_PGRP_ONLY, pgid as u32, std::ptr::null_mut(), 0) }
    };
    if byte_count < 0 {
        bail!(
            "listing processes in process group {pgid} failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut pids = vec![0 as libc::pid_t; byte_count as usize / std::mem::size_of::<libc::pid_t>()];
    // SAFETY: `pids` provides exactly the capacity reported by libproc.
    let read = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            pgid as u32,
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if read < 0 {
        bail!(
            "listing processes in process group {pgid} failed: {}",
            std::io::Error::last_os_error()
        );
    }
    pids.truncate(read as usize / std::mem::size_of::<libc::pid_t>());
    let mut members = Vec::new();
    for pid in pids {
        if pid == 0 {
            continue;
        }
        // libproc can omit BSD metadata for our unreaped zombie and leave an
        // unrelated errno. WNOWAIT supplies a positive, non-reaping proof; a
        // live child or a nonchild still takes the ordinary metadata path.
        if matches!(child_exited_unreaped(pid as u32), Ok(true)) {
            continue;
        }
        match process_is_zombie(pid as u32) {
            Ok(false) => members.push(pid as u32),
            Ok(true) => {}
            // A member can exit after proc_listpids returns and before its
            // individual inspection. It is no longer a live group member.
            Err(error)
                if error
                    .root_cause()
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::raw_os_error)
                    == Some(libc::ESRCH) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(members)
}

#[cfg(target_os = "linux")]
fn error_is_absent(error: &anyhow::Error) -> bool {
    error
        .root_cause()
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        .is_some_and(|errno| matches!(errno, libc::ESRCH | libc::ENOENT))
}

#[cfg(target_os = "macos")]
fn process_bsdinfo(pid: u32) -> Result<libc::proc_bsdinfo> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: proc_pidinfo initializes the provided buffer when it returns the
    // expected byte count; the pointer and size match proc_bsdinfo exactly.
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
        )
    };
    if read != std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int {
        let error = std::io::Error::last_os_error();
        let detail = format!(
            "reading process birth identity for pid {pid} failed (returned {read} of {} bytes): {error}",
            std::mem::size_of::<libc::proc_bsdinfo>()
        );
        return Err(error).context(detail);
    }
    // SAFETY: the exact expected structure size was returned above.
    Ok(unsafe { info.assume_init() })
}

/// Unsupported Unix targets cannot safely prove process identity.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
/// Returns the opaque OS birth identity for a live process.
pub fn process_birth_token(pid: u32) -> Result<String> {
    bail!("process birth identity is unsupported for pid {pid} on this platform")
}

/// Non-Unix adapters do not support process-group cancellation.
#[cfg(not(unix))]
/// Returns the opaque OS birth identity for a live process.
pub fn process_birth_token(pid: u32) -> Result<String> {
    bail!("process birth identity is unsupported for pid {pid} on this platform")
}
