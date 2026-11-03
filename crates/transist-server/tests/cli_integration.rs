// SPDX-License-Identifier: MPL-2.0
use std::process::Command;

#[test]
fn server_help_command_is_available() {
    let output = Command::new(env!("CARGO_BIN_EXE_transist-server"))
        .arg("--help")
        .output()
        .expect("run help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("--workspace-root <path>"));
    assert!(stdout.contains("--home <path>"));
    assert!(stdout.contains("Read-only companion inspection API."));
    assert!(stdout.contains("--allow-non-local-bind"));
    assert!(!stdout.contains("--repo-root <path>"));
}

#[test]
fn server_rejects_invalid_bind_argument() {
    let output = Command::new(env!("CARGO_BIN_EXE_transist-server"))
        .args(["--bind", "localhost"])
        .output()
        .expect("run invalid bind");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("invalid bind ip 'localhost'"));
}

#[test]
fn server_rejects_non_local_bind_without_explicit_opt_in() {
    let output = Command::new(env!("CARGO_BIN_EXE_transist-server"))
        .args(["--bind", "0.0.0.0"])
        .output()
        .expect("run invalid bind policy");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("refusing non-local bind '0.0.0.0'"));
}
