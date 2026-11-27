// SPDX-License-Identifier: MPL-2.0
//! Verifies the launcher diagnostic exposed to its caller on Linux.
#![cfg(target_os = "linux")]

#[test]
fn setup_error_prints_only_the_fixed_diagnostic() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_transist-sandbox-launcher"))
        .output()
        .expect("start launcher");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"transist-sandbox-launcher: setup failed\n");
}
