#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Unit tests for PR-gate archive host selection. Host and tool commands are
# stubbed only inside subshell fixtures; this is not native platform validation.
# No build, packaging, container, or full-gate command is executed.
set -euo pipefail

ci_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Exercise the production function without running the gate's top-level stages.
stage_definition="$(awk '/^stage_archive[(][)] [{]$/ { capture = 1 } capture { print } capture && /^[}]$/ { exit }' \
  "$ci_dir/check_pr_ready.sh")"
eval "$stage_definition"
if ! declare -F stage_archive >/dev/null; then
  echo 'FAIL (unit): could not extract the production archive stage' >&2
  exit 1
fi

# Run the production stage with unit-only command stubs in a private subshell.
# Args: OS, architecture, Rust host (strings), archive exit status (integer).
# Output: captured stage diagnostics and selected target; returns the stage status.
# Side effects: none; the archive command is intercepted before any work starts.
run_stage_fixture() (
  local fixture_os="$1" fixture_arch="$2" fixture_rust_host="$3" fixture_status="$4"

  # Args: -s or -m. Print the fixture OS/architecture; reject other probes.
  uname() {
    case "$*" in
      -s) printf '%s\n' "$fixture_os" ;;
      -m) printf '%s\n' "$fixture_arch" ;;
      *) return 2 ;;
    esac
  }

  # Args: -vV. Print fixture compiler metadata without invoking a compiler.
  rustc() {
    [[ "$*" == "-vV" ]] || return 2
    printf 'host: %s\n' "$fixture_rust_host"
  }

  # Args: archive script and target. Report selection, return the fixture status;
  # reject any other command so the unit test cannot launch real gate work.
  bash() {
    [[ "$#" -eq 2 && "$1" == scripts/ci/test_archive_reproducibility.sh ]] || return 2
    printf 'UNIT archive target: %s\n' "$2"
    return "$fixture_status"
  }

  stage_archive
)

# Args: OS, architecture, Rust host, expected target (strings), optional status
# (integer, default 0). Assert exact target/status; print unit result, fail on drift.
assert_target() {
  local output status=0 expected_status="${5:-0}"
  output="$(run_stage_fixture "$1" "$2" "$3" "$expected_status" 2>&1)" || status=$?
  if [[ "$status" -ne "$expected_status" || "$output" != *"UNIT archive target: $4" ]]; then
    printf 'FAIL (unit): %s/%s, rustc=%s: expected %s (exit %s), got exit %s\n%s\n' \
      "$1" "$2" "$3" "$4" "$expected_status" "$status" "$output" >&2
    return 1
  fi
  printf 'PASS (unit): %s/%s, rustc=%s -> %s (exit %s)\n' \
    "$1" "$2" "$3" "$4" "$status"
}

# Args: OS, architecture, Rust host, expected diagnostic (strings). Assert
# rejection before the archive command; print unit result, fail on acceptance.
assert_rejected() {
  local output status=0 target
  output="$(run_stage_fixture "$1" "$2" "$3" 0 2>&1)" || status=$?
  if [[ "$status" -eq 0 || "$output" != *"$4"* || "$output" == *"UNIT archive target:"* ]]; then
    printf 'FAIL (unit): expected rejection for %s/%s, rustc=%s (exit %s)\n%s\n' \
      "$1" "$2" "$3" "$status" "$output" >&2
    return 1
  fi
  if [[ "$4" == "unsupported host" ]]; then
    for target in aarch64-apple-darwin x86_64-apple-darwin \
      x86_64-unknown-linux-gnu x86_64-unknown-linux-musl \
      aarch64-unknown-linux-gnu aarch64-unknown-linux-musl; do
      if [[ "$output" != *"$target"* ]]; then
        printf 'FAIL (unit): unsupported-host diagnostic omits %s\n%s\n' "$target" "$output" >&2
        return 1
      fi
    done
  fi
  printf 'PASS (unit): rejected %s/%s, rustc=%s before archive work\n' "$1" "$2" "$3"
}

# Every existing release target, preserving the GNU/musl compiler ABI.
for fixture in \
  'Darwin|arm64|aarch64-apple-darwin' \
  'Darwin|x86_64|x86_64-apple-darwin' \
  'Linux|x86_64|x86_64-unknown-linux-gnu' \
  'Linux|x86_64|x86_64-unknown-linux-musl' \
  'Linux|aarch64|aarch64-unknown-linux-gnu' \
  'Linux|aarch64|aarch64-unknown-linux-musl' \
  'Linux|arm64|aarch64-unknown-linux-gnu' \
  'Linux|arm64|aarch64-unknown-linux-musl'; do
  IFS='|' read -r fixture_os fixture_arch fixture_target <<< "$fixture"
  assert_target "$fixture_os" "$fixture_arch" "$fixture_target" "$fixture_target"
done

# The native fallback retains GNU on Linux when compiler metadata is unavailable.
for rust_host in '' unrecognized-rust-host; do
  assert_target Darwin arm64 "$rust_host" aarch64-apple-darwin
  assert_target Darwin x86_64 "$rust_host" x86_64-apple-darwin
  assert_target Linux x86_64 "$rust_host" x86_64-unknown-linux-gnu
  assert_target Linux aarch64 "$rust_host" aarch64-unknown-linux-gnu
  assert_target Linux arm64 "$rust_host" aarch64-unknown-linux-gnu
done

# A recognized compiler target cannot substitute another architecture or OS.
for fixture in \
  'Darwin|x86_64|aarch64-apple-darwin' \
  'Darwin|arm64|x86_64-apple-darwin' \
  'Linux|x86_64|aarch64-unknown-linux-gnu' \
  'Linux|x86_64|aarch64-unknown-linux-musl' \
  'Linux|aarch64|x86_64-unknown-linux-gnu' \
  'Linux|arm64|x86_64-unknown-linux-musl' \
  'Linux|aarch64|aarch64-apple-darwin' \
  'Linux|x86_64|x86_64-apple-darwin' \
  'Darwin|arm64|aarch64-unknown-linux-gnu' \
  'Darwin|x86_64|x86_64-unknown-linux-musl'; do
  IFS='|' read -r fixture_os fixture_arch fixture_target <<< "$fixture"
  assert_rejected "$fixture_os" "$fixture_arch" "$fixture_target" 'does not match native host'
done

assert_rejected Linux armv7l armv7-unknown-linux-gnueabihf 'unsupported host'
assert_rejected FreeBSD x86_64 x86_64-unknown-freebsd 'unsupported host'
assert_rejected Darwin ppc64 powerpc-apple-darwin 'unsupported host'
assert_target Linux aarch64 aarch64-unknown-linux-gnu aarch64-unknown-linux-gnu 42

echo 'PR-gate archive host unit tests passed (32 cases; no native archive validation).'
