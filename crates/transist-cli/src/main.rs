// SPDX-License-Identifier: MPL-2.0
#![deny(missing_docs)]

//! Thin binary composition root for the Transist CLI.

#[allow(unsafe_code)]
fn main() {
    #[cfg(target_os = "macos")]
    {
        // Deny debugger attachment via ptrace(PT_DENY_ATTACH).
        // Silently ignored on systems where ptrace is unavailable or restricted.
        // Defeated with a binary patch but eliminates casual lldb/gdb attachment.
        // SAFETY: `PT_DENY_ATTACH` is a macOS-specific ptrace request that
        // takes no pointer arguments and cannot fail in a way that affects
        // memory safety; the process simply refuses debugger attachment.
        unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) };
    }

    let classifier = transist_cli::OssExitCodeClassifier;
    let exit_code = transist_cli::run_cli_process(transist_cli::CliSurface::Core, &classifier);
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
