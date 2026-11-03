// SPDX-License-Identifier: MPL-2.0
//! Integration tests for the no-home error path.
//!
//! These tests verify that commands requiring a Transist home produce a
//! clear, actionable error when none can be resolved rather than crashing
//! or falling back to a silent default.

mod support;

use assert_cmd::cargo::CommandCargoExt;
use std::process::Command;
use tempfile::TempDir;

/// Spawns a `transist` command in a directory with no persisted user home,
/// no `TRANSIST_HOME` env var, and a `HOME` pointing at the temp dir so
/// there is no pre-existing user-level settings file and no pre-existing
/// per-user default home directory. This still resolves to an
/// (uninitialized) per-user default home, so the error path exercised here
/// is "resolved but not initialized" rather than "no source resolves at
/// all" — the actionable message is the same either way.
fn transist_no_home(work_dir: &TempDir, home_dir: &TempDir, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::cargo_bin("transist").expect("transist binary");
    cmd.current_dir(work_dir.path());
    cmd.env_remove("TRANSIST_HOME");
    // Override HOME so the settings file path and per-user default home both
    // resolve inside tmp and are absent, ensuring no persisted user home or
    // real per-user default home leaks from the real user account.
    cmd.env("HOME", home_dir.path());
    cmd.env_remove("XDG_STATE_HOME");
    cmd.args(args);
    cmd.output().expect("run transist")
}

#[test]
fn runs_list_with_no_home_exits_nonzero_with_actionable_message() {
    let work = TempDir::new().expect("work dir");
    let fake_home = TempDir::new().expect("fake home");
    let output = transist_no_home(&work, &fake_home, &["runs", "list"]);

    assert!(
        !output.status.success(),
        "runs list without a home should exit non-zero"
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        combined.contains("transist home init") || combined.contains("no Transist home"),
        "error output should mention 'transist home init' or 'no Transist home'; got: {combined}"
    );
}

#[test]
fn plugins_list_with_no_home_exits_nonzero_with_actionable_message() {
    let work = TempDir::new().expect("work dir");
    let fake_home = TempDir::new().expect("fake home");
    let output = transist_no_home(&work, &fake_home, &["plugins", "list"]);

    assert!(
        !output.status.success(),
        "plugins list without a home should exit non-zero"
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        combined.contains("transist home init") || combined.contains("no Transist home"),
        "error output should mention 'transist home init' or 'no Transist home'; got: {combined}"
    );
}

#[test]
fn home_init_succeeds_and_then_runs_list_works() {
    let work = TempDir::new().expect("work dir");
    let fake_home = TempDir::new().expect("fake home");

    // home init should succeed and resolve to the per-user default under the
    // isolated fake $HOME (no --home flag, matching the documented sequence).
    let init_output = {
        let mut cmd = Command::cargo_bin("transist").expect("transist binary");
        cmd.current_dir(work.path());
        cmd.env_remove("TRANSIST_HOME");
        cmd.env("HOME", fake_home.path());
        cmd.env_remove("XDG_STATE_HOME");
        cmd.args(["home", "init"]);
        cmd.output().expect("run home init")
    };

    assert!(
        init_output.status.success(),
        "home init should succeed in a fresh directory; stderr={}",
        String::from_utf8_lossy(&init_output.stderr)
    );
    let expected_home = fake_home
        .path()
        .join(".local")
        .join("state")
        .join("transist");
    assert!(
        expected_home.is_dir(),
        "home init should have created the per-user default home at {}",
        expected_home.display()
    );

    // runs list should now work (exit 0) because the per-user default home exists.
    let list_output = {
        let mut cmd = Command::cargo_bin("transist").expect("transist binary");
        cmd.current_dir(work.path());
        cmd.env_remove("TRANSIST_HOME");
        cmd.env("HOME", fake_home.path());
        cmd.env_remove("XDG_STATE_HOME");
        cmd.args(["runs", "list"]);
        cmd.output().expect("run runs list")
    };

    assert!(
        list_output.status.success(),
        "runs list should succeed after home init; stderr={}",
        String::from_utf8_lossy(&list_output.stderr)
    );
}
