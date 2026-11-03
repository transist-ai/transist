// SPDX-License-Identifier: MPL-2.0
//! Internal Linux sandbox launcher.
//!
//! `bubblewrap --unshare-net` creates a private network namespace, but may
//! raise its loopback interface. The required internal `--network-mode` selects
//! `none` (explicitly lower `lo`) or `loopback` (raise `lo`). This launcher runs
//! as the sandbox's effective root with `CAP_NET_ADMIN` only in that namespace,
//! configures and verifies the interface, then drops the capability bounding set
//! and switches to the target uid/gid. It explicitly zeroes and verifies the
//! effective/permitted/inheritable sets (including the uid-0 case) before
//! `exec`-ing the plugin.
//!
//! The binary is small and Linux-only in function; on other platforms it fails
//! closed because interface configuration depends on Linux ioctls.

// SAFETY: OS-integration: Linux sandbox launcher uses raw libc syscalls for loopback setup, capability drop, and exec.
#![cfg_attr(not(clippy), deny(missing_docs))]
#![allow(unsafe_code)]

#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

const HELP: &str = "Internal Linux network sandbox launcher.\n\
Usage: transist-sandbox-launcher --network-mode <none|loopback> <uid> <gid> <program> [args...]\n\
Must be invoked by the runtime inside a fresh private network and user namespace.\n\
Configures loopback, drops capabilities, and executes the plugin.\n\
Options: --help, -h; --version, -V";

/// Returns informational output without namespace setup or plugin execution.
fn informational_output(args: &[std::ffi::OsString]) -> Option<String> {
    if args.len() != 1 {
        return None;
    }
    match args[0].to_str()? {
        "--help" | "-h" => Some(HELP.to_string()),
        "--version" | "-V" => Some(format!(
            "transist-sandbox-launcher {}",
            env!("CARGO_PKG_VERSION")
        )),
        _ => None,
    }
}

/// Only strict namespace modes are accepted by this internal executable.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkMode {
    None,
    Loopback,
}

#[cfg(target_os = "linux")]
impl NetworkMode {
    fn parse(value: &std::ffi::OsStr) -> Result<Self, String> {
        match value.to_str() {
            Some("none") => Ok(Self::None),
            Some("loopback") => Ok(Self::Loopback),
            _ => Err("invalid internal network mode; expected none or loopback".to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
const MAX_CAPABILITY: libc::c_int = 63;

/// Linux capability protocol version 3 (covers all 64 capabilities).
#[cfg(target_os = "linux")]
const LINUX_CAPABILITY_VERSION_3: u32 = 0x20080522;

/// Kernel capability header (`__user_cap_header_struct` from
/// `<linux/capability.h>`).
#[cfg(target_os = "linux")]
#[repr(C)]
struct CapUserHeader {
    version: u32,
    pid: libc::c_int,
}

/// Kernel capability data word (`__user_cap_data_struct`). The v3 protocol
/// uses two consecutive words to cover the full 64-cap range.
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Copy, Clone, Default)]
struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(output) = informational_output(&args) {
        println!("{output}");
        return;
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("transist-sandbox-launcher: unsupported platform");
        std::process::exit(2);
    }
    #[cfg(target_os = "linux")]
    match run(args.into_iter()) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("transist-sandbox-launcher: {error}");
            // Preflight diagnostics deliberately never surface child stderr:
            // after the launcher execs a plugin that output is untrusted and
            // may contain a secret. Preserve a safe, machine-readable failure
            // category in the exit status instead, so an early launcher error
            // can be distinguished without exposing stderr.
            std::process::exit(launcher_error_exit_code(&error));
        }
    }
}

/// Returns a stable, non-secret exit code for a launcher setup failure.
#[cfg(target_os = "linux")]
fn launcher_error_exit_code(error: &str) -> i32 {
    if error.starts_with("SIOCGIFFLAGS")
        || error.starts_with("SIOCSIFFLAGS")
        || error.starts_with("failed to open AF_INET")
    {
        20
    } else if error.starts_with("PR_CAPBSET_DROP") {
        21
    } else if error.starts_with("PR_SET_NO_NEW_PRIVS") {
        22
    } else if error.starts_with("failed clearing")
        || error.starts_with("failed inspecting supplementary groups")
    {
        23
    } else if error.starts_with("failed switching to gid") {
        24
    } else if error.starts_with("failed switching to uid") {
        25
    } else if error.starts_with("capset") {
        26
    } else if error.starts_with("capget") || error.starts_with("CAP_NET_ADMIN is still") {
        27
    } else if error.starts_with("missing plugin program argument")
        || error.starts_with("plugin program path contains NUL byte")
        || error.starts_with("failed executing plugin")
    {
        28
    } else {
        2
    }
}

#[cfg(target_os = "linux")]
fn run(mut args: std::vec::IntoIter<std::ffi::OsString>) -> Result<i32, String> {
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--network-mode")) {
        return Err("missing --network-mode argument".to_string());
    }
    let mode = NetworkMode::parse(
        &args
            .next()
            .ok_or_else(|| "missing internal network mode argument".to_string())?,
    )?;
    let target_uid = args
        .next()
        .ok_or_else(|| "missing target uid argument".to_string())?
        .to_string_lossy()
        .parse::<u32>()
        .map_err(|e| format!("invalid target uid: {e}"))?;
    let target_gid = args
        .next()
        .ok_or_else(|| "missing target gid argument".to_string())?
        .to_string_lossy()
        .parse::<u32>()
        .map_err(|e| format!("invalid target gid: {e}"))?;

    if args.as_slice().is_empty() {
        return Err("missing plugin program argument".to_string());
    }
    configure_loopback(mode)?;
    drop_capabilities_and_handoff(target_uid, target_gid, args)
}

/// Sets and verifies `lo` in the current private namespace; any ioctl error
/// aborts setup before untrusted code can execute.
#[cfg(target_os = "linux")]
fn configure_loopback(mode: NetworkMode) -> Result<(), String> {
    // SAFETY: `libc::socket` takes integer arguments and returns a raw fd or -1;
    // the result is checked below and owned by `sock` until closed.
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if socket < 0 {
        return Err("failed to open AF_INET datagram socket".to_string());
    }
    // SAFETY: the successful socket call returns a new, uniquely owned fd.
    // RAII closes it on every path without overwriting an ioctl's errno.
    let sock = unsafe { OwnedFd::from_raw_fd(socket) };

    // SAFETY: `std::mem::zeroed` initializes the stack `ifreq` struct with
    // zeroes; no pointers are dereferenced and every field is set before use.
    let mut ifreq: libc::ifreq = unsafe { std::mem::zeroed() };
    // Copy "lo\0" into ifr_name.
    let name_bytes = b"lo\0";
    for (index, byte) in name_bytes
        .iter()
        .take(libc::IFNAMSIZ.min(name_bytes.len()))
        .enumerate()
    {
        ifreq.ifr_name[index] = *byte as libc::c_char;
    }
    let ifreq_ptr = &mut ifreq as *mut libc::ifreq;

    // SAFETY: `ifreq_ptr` is a valid pointer to the live `ifreq` struct and
    // `sock` is an owned fd; the ioctl return code is checked below.
    if unsafe { libc::ioctl(sock.as_raw_fd(), libc::SIOCGIFFLAGS as _, ifreq_ptr) } < 0 {
        return Err(format!(
            "SIOCGIFFLAGS failed on lo: {}",
            std::io::Error::last_os_error()
        ));
    }

    // Link-layer flags live in ifr_ifru.ifru_flags (a `c_short`). The libc
    // constants are `c_int`; cast both sides explicitly.
    // SAFETY: `ifreq.ifr_ifru.ifru_flags` is a scalar read of a `c_short` field
    // from the ioctl-populated struct; the cast is explicit.
    let raw_flags = unsafe { ifreq.ifr_ifru.ifru_flags } as libc::c_int;
    let flags = match mode {
        NetworkMode::None => raw_flags & !libc::IFF_UP,
        NetworkMode::Loopback => raw_flags | libc::IFF_UP | libc::IFF_RUNNING,
    };
    ifreq.ifr_ifru.ifru_flags = flags as libc::c_short;
    // SAFETY: `ifreq_ptr` remains valid and `sock` remains owned; the ioctl
    // return code is checked below.
    if unsafe { libc::ioctl(sock.as_raw_fd(), libc::SIOCSIFFLAGS as _, ifreq_ptr) } < 0 {
        return Err(format!(
            "SIOCSIFFLAGS failed configuring lo for {mode:?}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the fd and ifreq remain valid; read back the kernel's flags rather
    // than assuming a successful set established the requested interface state.
    if unsafe { libc::ioctl(sock.as_raw_fd(), libc::SIOCGIFFLAGS as _, ifreq_ptr) } < 0 {
        return Err(format!(
            "SIOCGIFFLAGS failed verifying lo: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the preceding ioctl populated this scalar union field.
    let is_up = unsafe { ifreq.ifr_ifru.ifru_flags } as libc::c_int & libc::IFF_UP != 0;
    if is_up != (mode == NetworkMode::Loopback) {
        return Err(format!(
            "SIOCGIFFLAGS found unexpected lo state for {mode:?}"
        ));
    }
    Ok(())
}

/// Drops all capabilities, switches to the target uid/gid, verifies no
/// capabilities survive, and `exec`s the plugin.
///
/// Security properties enforced before `exec`:
/// - Every capability in the bounding set is dropped; `EINVAL` (cap beyond the
///   kernel's `CAP_LAST_CAP`) is treated as absent-and-safe; any other error is
///   fatal.
/// - `PR_SET_NO_NEW_PRIVS` prevents any future `exec` from regaining privilege;
///   failure is fatal.
/// - Effective, permitted, and inheritable capability sets are explicitly zeroed
///   via `capset` after the uid/gid switch (handles the uid-0 case where the uid
///   switch leaves sets unchanged, and clears the inheritable set which the uid
///   switch does not touch for non-root targets).
/// - `capget` verifies every capability is absent from all three
///   sets before `exec`.
#[cfg(target_os = "linux")]
fn drop_capabilities_and_handoff(
    target_uid: u32,
    target_gid: u32,
    mut args: std::vec::IntoIter<std::ffi::OsString>,
) -> Result<i32, String> {
    // Drop every capability from the bounding set. EINVAL means the capability
    // does not exist on this kernel (beyond CAP_LAST_CAP) and is already absent.
    // Any other error indicates a missing CAP_SETPCAP or kernel misconfiguration
    // and is treated as a fatal security failure.
    for cap in 0..=MAX_CAPABILITY {
        // SAFETY: `PR_CAPBSET_DROP` is well-defined for capability numbers in
        // range; the remaining options are zero.
        let rc = unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINVAL) {
                // Capability index beyond CAP_LAST_CAP on this kernel; bounding
                // set does not include it.
                continue;
            }
            return Err(format!("PR_CAPBSET_DROP({cap}) failed: {err}"));
        }
    }

    // Refuse future privilege gain via exec.
    // SAFETY: `PR_SET_NO_NEW_PRIVS` takes one nonzero arg.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc < 0 {
        return Err(format!(
            "PR_SET_NO_NEW_PRIVS failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    // Switch to the target gid then uid (dropping supplementary groups too).
    // For non-root targets, switching away from uid 0 clears the effective and
    // permitted sets; the capset below handles the uid-0 case and clears the
    // inheritable set for all targets.
    // SAFETY: pointers and uids are valid; these are the standard credential
    // syscalls.
    unsafe {
        clear_supplementary_groups()?;
        if libc::setresgid(
            target_gid as libc::gid_t,
            target_gid as libc::gid_t,
            target_gid as libc::gid_t,
        ) < 0
        {
            return Err(format!(
                "failed switching to gid {}: {}",
                target_gid,
                std::io::Error::last_os_error()
            ));
        }
        if libc::setresuid(
            target_uid as libc::uid_t,
            target_uid as libc::uid_t,
            target_uid as libc::uid_t,
        ) < 0
        {
            return Err(format!(
                "failed switching to uid {}: {}",
                target_uid,
                std::io::Error::last_os_error()
            ));
        }
    }

    // Explicitly zero effective, permitted, and inheritable capability sets.
    // setresuid clears E and P when switching away from uid 0, but does not
    // clear I; capset ensures all three are empty for every uid target,
    // including uid 0 where the uid switch would leave the sets unchanged.
    let zero_data = [CapUserData::default(); 2];
    let mut cap_hdr = CapUserHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    // SAFETY: cap_hdr and zero_data are valid stack allocations; SYS_capset
    // accepts a pointer to the header and a pointer to the two-word data array.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_capset,
            &mut cap_hdr as *mut CapUserHeader,
            zero_data.as_ptr(),
        )
    };
    if rc < 0 {
        return Err(format!(
            "capset (zero all capability sets) failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    // Verify no capability survives in any capability set before exec.
    verify_capabilities_dropped()?;

    let program = args
        .next()
        .ok_or_else(|| "missing plugin program argument".to_string())?;
    let program_c = CString::new(program.as_bytes().to_vec())
        .map_err(|_| "plugin program path contains NUL byte".to_string())?;
    let argv_c: Vec<CString> = std::iter::once(program_c.clone())
        .chain(args.filter_map(|arg| CString::new(arg.into_vec().to_vec()).ok()))
        .collect();
    let argv_ptrs: Vec<*const libc::c_char> = argv_c
        .iter()
        .map(|arg| arg.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let argv = argv_ptrs.as_ptr();
    // SAFETY: `program_c` is still alive and `argv` is a NUL-terminated array
    // of valid pointers; `execv` replaces the image or returns an error.
    unsafe { libc::execv(program_c.as_ptr(), argv) };
    Err(format!(
        "failed executing plugin '{}': {}",
        program_c.to_string_lossy(),
        std::io::Error::last_os_error()
    ))
}

/// Clears supplementary groups before handing the sandbox to the plugin.
///
/// bwrap's fresh user namespace deliberately rejects `setgroups` after its
/// gid map is installed. In that namespace supplementary host groups are not
/// mapped and cannot grant access to the sandbox's bind mounts, so that
/// kernel-enforced refusal is safe. Any other failure remains fatal.
#[cfg(target_os = "linux")]
fn clear_supplementary_groups() -> Result<(), String> {
    // SAFETY: a null group-list pointer is valid when the count is zero. The
    // return value and errno are checked immediately.
    if unsafe { libc::setgroups(0, std::ptr::null()) } >= 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EPERM) {
        return Err(format!("failed clearing supplementary groups: {error}"));
    }

    Ok(())
}

/// Reads the calling thread's capability sets via `capget` and returns `Err`
/// if any capability is present in any of the effective,
/// permitted, or inheritable sets.
///
/// This is the definitive post-drop verification: it catches the uid-0 case
/// where the uid switch alone leaves the capability sets intact.
#[cfg(target_os = "linux")]
fn verify_capabilities_dropped() -> Result<(), String> {
    let mut hdr = CapUserHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0, // 0 = calling thread
    };
    let mut data = [CapUserData::default(); 2];
    // SAFETY: hdr and data are valid stack allocations; SYS_capget reads the
    // capability sets of the calling thread into the provided buffers.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_capget,
            &mut hdr as *mut CapUserHeader,
            data.as_mut_ptr(),
        )
    };
    if rc < 0 {
        return Err(format!(
            "capget failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if data
        .iter()
        .any(|word| word.effective != 0 || word.permitted != 0 || word.inheritable != 0)
    {
        return Err("capget found nonzero capability sets after drop".to_string());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn internal_network_mode_is_required_and_rejects_unrestricted_modes() {
        assert_eq!(
            NetworkMode::parse("none".as_ref()).unwrap(),
            NetworkMode::None
        );
        assert_eq!(
            NetworkMode::parse("loopback".as_ref()).unwrap(),
            NetworkMode::Loopback
        );
        for value in ["", "any", "allowlist", "NONE"] {
            assert!(NetworkMode::parse(value.as_ref()).is_err());
        }
        for args in [
            vec!["0", "0", "/bin/true"],
            vec!["--network-mode"],
            vec!["--network-mode", "any", "0", "0", "/bin/true"],
            vec!["--network-mode", "none", "0", "0"],
        ] {
            assert!(
                run(args
                    .into_iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>()
                    .into_iter())
                .is_err()
            );
        }
    }

    #[test]
    fn release_help_and_version_do_not_require_namespace_setup() {
        for flag in ["--version", "-V"] {
            assert_eq!(
                informational_output(&[flag.into()]),
                Some(format!(
                    "transist-sandbox-launcher {}",
                    env!("CARGO_PKG_VERSION")
                ))
            );
        }
        for flag in ["--help", "-h"] {
            let help = informational_output(&[flag.into()]).unwrap();
            assert!(help.contains("--network-mode <none|loopback>"));
        }
        assert_eq!(
            informational_output(&["--version".into(), "0".into()]),
            None
        );
    }

    #[test]
    fn launcher_setup_errors_have_safe_distinct_exit_codes() {
        assert_eq!(
            launcher_error_exit_code("SIOCSIFFLAGS failed bringing up lo: Operation not permitted"),
            20
        );
        assert_eq!(
            launcher_error_exit_code(
                "SIOCSIFFLAGS failed configuring lo for None: Operation not permitted"
            ),
            20
        );
        assert_eq!(
            launcher_error_exit_code("SIOCGIFFLAGS failed verifying lo: No such device"),
            20
        );
        assert_eq!(
            launcher_error_exit_code("capget found nonzero capability sets after drop"),
            27
        );
        assert_eq!(
            launcher_error_exit_code("PR_CAPBSET_DROP(12) failed: Operation not permitted"),
            21
        );
        assert_eq!(
            launcher_error_exit_code(
                "failed clearing supplementary groups: Operation not permitted"
            ),
            23
        );
        assert_eq!(
            launcher_error_exit_code(
                "failed executing plugin '/bin/sh': No such file or directory"
            ),
            28
        );
        assert_eq!(launcher_error_exit_code("missing target uid argument"), 2);
    }
}
