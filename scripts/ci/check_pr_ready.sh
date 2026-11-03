#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# check_pr_ready.sh — Local PR-readiness gate.
#
# Runs the core local checks needed before opening a pull request:
# format, clippy, maintainability checks, workspace tests, release build,
# archive reproducibility, quickstart smoke, required E2E suites, and
# shipped-workflow coverage.
#
# The canonical aggregate gate for public release readiness is
# scripts/ci/check_release_readiness.sh. It runs every check in this script
# plus supply-chain pins, legal notices, full-history secret scanning,
# schema/OpenAPI contracts, release archive inventory, admin sign/verify
# conformance, and release invariants.
# Run check_release_readiness.sh before tagging a release.
# Hosted CI runs a lightweight PR gate on Linux and a cross-platform
# release build + quickstart smoke on main (Linux x86-64/glibc, macOS Apple
# Silicon, Alpine/musl). Security scans run weekly, on demand, and at release
# staging. Release staging additionally requires Mainline Validation / main-gate
# and Security Scan / security-gate for the exact tagged SHA.
#
# ---------------------------------------------------------------------------
# Notes for local AI agents (and humans) driving this script:
#
# - DEFAULT (no flags) == the full local gate. Run it, unchanged, before
#   opening a pull request. Its core compile/test/release-set matches the
#   checks in .github/workflows/pr-ci.yml; run_maintainability_checks.sh is a
#   local subset of CI's gate scripts, and the archive-reproducibility stage is
#   a local release contract not run in the PR validate job. Run --help for the
#   exact stage list.
#
# - --fast is the tight inner-loop mode. It runs only
#   workspace tests -> fmt -> clippy -> rustdoc -> maintainability and stops. It
#   does NOT release-build, does NOT verify archive reproducibility, does NOT
#   quickstart smoke, does NOT run the E2E suites, and does NOT run
#   shipped-workflow coverage. Never treat --fast as PR-ready; the skipped
#   stages are release-readiness contracts.
#
# - --skip-archive / --skip-smoke / --skip-e2e / --skip-workflows shave
#   individual release-tail stages for targeted debugging. They are debugging
#   aids, not a PR gate.
#
# - --parallel only backgrounds the pure-shell maintainability checks next to
#   the debug compile phase. cargo stages are never run concurrently: cargo
#   holds a file lock on the target directory, so parallel cargo invocations
#   would serialize on that lock while also contending for CPU. Output
#   interleaves in parallel mode by design.
#
# - Fail-fast is the DEFAULT when running locally: the first failing stage
#   stops the run immediately (timing summary still prints via the EXIT trap).
#   cargo test already stops the workspace-test stage at the first failing
#   test binary by default, so fail-fast mode does not need to pass any extra
#   flag. Use --no-fail-fast to run every scheduled stage (and pass
#   `cargo test --no-fail-fast` so every test runs) and report all failures at
#   the end; the script still exits non-zero if anything failed.
#   --no-fail-fast is useful for harvesting the full list of failures in one
#   pass, but the run is no longer interrupted early.
#
# - In a CI environment (auto-detected, e.g. GitHub Actions, GitLab CI,
#   CircleCI, ...) the default is the OPPOSITE: the script does NOT fail fast,
#   so a single stage failure cannot hide unrelated failures from the same CI
#   run. All failures are collected and reported (and `cargo test` runs every
#   test); the script exits non-zero if anything failed. Pass --fail-fast to
#   force the early-stop behavior even in CI, or --no-fail-fast to force the
#   collect-everything behavior anywhere.
#
# - --help / -h prints the flag usage and exits without running anything.
#
# - Feature sets are PINNED to pr-ci.yml parity on purpose: clippy runs
#   --workspace --all-targets --all-features, tests run
#   --locked --workspace --all-targets. Do not "optimize" these away; they are
#   the CI contract. Cache-efficient ordering comes from stage grouping, not
#   from changing flags.
# ---------------------------------------------------------------------------

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# GitHub's container jobs mount a checkout owned by the host runner while this
# gate runs as the container user. Pass a repository-specific safe-directory
# entry through Git's environment configuration so every nested archive and
# inventory script can inspect tracked files without mutating global config.
git_config_count="${GIT_CONFIG_COUNT:-0}"
if [[ ! "$git_config_count" =~ ^[0-9]+$ ]]; then
  echo "FAIL: GIT_CONFIG_COUNT must be a non-negative integer" >&2
  exit 2
fi
export GIT_CONFIG_COUNT="$((git_config_count + 1))"
export "GIT_CONFIG_KEY_${git_config_count}=safe.directory"
export "GIT_CONFIG_VALUE_${git_config_count}=$ROOT_DIR"

fast=0
skip_archive=0
skip_smoke=0
skip_e2e=0
skip_workflows=0
parallel=0
# Fail-fast policy, resolved after flag parsing:
#   "" (empty) = auto: fail-fast locally, no-fail-fast in a CI environment.
#   "1"        = --fail-fast: always stop at the first failing stage.
#   "0"        = --no-fail-fast: always run every scheduled stage.
fail_fast=""

usage() {
  cat <<'EOF'
Usage: check_pr_ready.sh [flags]

Local PR-readiness gate. Default (no flags) runs the full local gate.

Flags:
  --fast          Inner-loop mode: workspace tests + fmt + clippy + rustdoc + maintainability only.
                  Skips release build, archive reproducibility, quickstart
                  smoke, and E2E. Do NOT open a PR on --fast alone.
  --skip-archive  Skip the release-archive reproducibility check.
  --skip-smoke    Skip the quickstart smoke suites.
  --skip-e2e      Skip the E2E suites.
  --skip-workflows  Skip the shipped-workflow coverage execution.
  --parallel      Run the pure-shell maintainability checks concurrently with
                  the debug compile phase. Output interleaves.
  --fail-fast     Stop at the first failing stage (always, even in CI).
  --no-fail-fast  Do not stop at the first failing stage: run every scheduled
                  stage and report all failures at the end. This is the
                  default in CI environments; fail-fast is the default locally.
  --help, -h      Show this help and exit.

Run the full gate (no flags) before opening a pull request.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    # --fast: inner-loop mode — stop after fmt/clippy/maintainability/tests.
    --fast) fast=1 ;;
    # The --skip-* flags carve individual stages out of the release tail.
    # They only matter when --fast is not set (fast already drops that tail).
    --skip-archive) skip_archive=1 ;;
    --skip-smoke) skip_smoke=1 ;;
    --skip-e2e) skip_e2e=1 ;;
    --skip-workflows) skip_workflows=1 ;;
    # --parallel: run the pure-shell maintainability checks concurrently with
    # the debug compile phase. cargo stages stay serial (target-dir lock).
    --parallel) parallel=1 ;;
    # --fail-fast / --no-fail-fast override the auto default (fail-fast
    # locally, no-fail-fast in CI). Last flag wins.
    --fail-fast) fail_fast=1 ;;
    --no-fail-fast) fail_fast=0 ;;
    --help|-h) usage; exit 0 ;;
    *) # Reject unknown flags loudly instead of silently running the full gate.
      echo "unknown flag: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

# Detect a CI environment. Returns 0 if the script appears to run on a
# continuous-integration host. Providers that set a dedicated truthy variable
# are matched explicitly; the conventional CI=true covers the rest.
is_ci() {
  local v
  for v in GITHUB_ACTIONS GITLAB_CI CIRCLECI TRAVIS BUILDKITE JENKINS_URL TEAMCITY_VERSION TF_BUILD CODEBUILD_BUILD_ID; do
    if [[ -n "${!v:-}" ]]; then
      return 0
    fi
  done
  if [[ "${CI:-}" == "true" || "${CI:-}" == "1" ]]; then
    return 0
  fi
  return 1
}

# Resolve the fail-fast policy default: fail-fast locally, collect-all
# failures in CI. Explicit --fail-fast / --no-fail-fast flags already set
# fail_fast to 0 or 1, so only the empty (auto) case reaches here.
if [[ -z "$fail_fast" ]]; then
  if is_ci; then
    fail_fast=0
    echo "NOTE: CI environment detected; not failing fast. Every scheduled stage"
    echo "      runs and all failures are reported. Pass --fail-fast to override."
  else
    fail_fast=1
  fi
fi

if [[ "$fast" -eq 1 ]]; then
  # --fast always drops the whole release tail (release build, archive, smoke,
  # E2E, workflows). The explicit --skip-* flags also still apply if combined.
  skip_archive=1
  skip_smoke=1
  skip_e2e=1
  skip_workflows=1
  echo "NOTE: --fast mode: workspace tests + fmt + clippy + maintainability ONLY."
  echo "      Release build, archive reproducibility, quickstart smoke, E2E, and"
  echo "      shipped-workflow coverage are SKIPPED. The full gate (no flags) is"
  echo "      still required before opening a pull request."
elif [[ "$skip_archive" -eq 1 || "$skip_smoke" -eq 1 || "$skip_e2e" -eq 1 || "$skip_workflows" -eq 1 ]]; then
  echo "NOTE: one or more release-tail stages are SKIPPED via --skip-*."
  echo "      Run the full gate (no flags) before opening a pull request."
fi

# --- Timing instrumentation --------------------------------------------------
# Per-stage wall-clock times (seconds). No external deps; each stage is wrapped
# in run_stage, which records "label|elapsed" into stage_times. A summary table
# prints at exit, including for partial (failed) runs.
stage_times=()
# Labels of stages that failed while --no-fail-fast kept running. Used to
# report every failure at the end and to force a non-zero exit status.
stage_failures=()

now_epoch() {
  date +%s
}

print_summary() {
  local label elapsed total=0 entry
  echo ""
  echo "=== check_pr_ready.sh stage timing ==="
  # Guard against an empty array: under `set -u` on bash 3.2 (macOS system
  # bash), expanding "${stage_times[@]}" when the array has no entries raises
  # "unbound variable". This happens when the very first stage (fmt) fails
  # before any stage is recorded. ${#arr[@]} is safe on empty arrays.
  if [[ ${#stage_times[@]} -gt 0 ]]; then
    for entry in "${stage_times[@]}"; do
      label="${entry%%|*}"
      elapsed="${entry##*|}"
      printf '  %-22s %6ss\n' "$label" "$elapsed"
      total=$((total + elapsed))
    done
  fi
  printf '  %-22s %6ss\n' "TOTAL" "$total"
}

# Run one stage, always record its timing, and handle failure per the
# fail-fast policy:
#   - fail_fast=1 (default): return the stage's exit status so the caller's
#     `set -e` aborts immediately; the timing summary still prints via the
#     EXIT trap.
#   - fail_fast=0 (--no-fail-fast): record the label in stage_failures, return
#     0, and let the caller run the remaining stages; the end of the script
#     reports all failures and exits non-zero.
run_stage() {
  local label="$1"
  shift
  local t0 t1 elapsed rc=0
  t0="$(now_epoch)"
  echo ""
  echo "  >>> [$label] starting"
  "$@" || rc=$?
  t1="$(now_epoch)"
  elapsed=$((t1 - t0))
  if [[ "$rc" -eq 0 ]]; then
    echo "  <<< [$label] finished in ${elapsed}s"
  else
    echo "  <<< [$label] FAILED (exit ${rc}) in ${elapsed}s"
  fi
  stage_times+=("${label}|${elapsed}")
  if [[ "$rc" -ne 0 ]]; then
    if [[ "$fail_fast" -eq 1 ]]; then
      return "$rc"
    fi
    stage_failures+=("$label")
  fi
  return 0
}

trap print_summary EXIT

# --- Stage implementations ---------------------------------------------------

stage_fmt() {
  echo "    Checking formatting..."
  cargo fmt --all -- --check
}

stage_clippy() {
  echo "    Running clippy..."
  # Pinned to pr-ci.yml: --all-features must stay.
  cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
}

stage_docs() {
  echo "    Running rustdoc..."
  RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
}

stage_maintainability() {
  echo "    Running maintainability checks..."
  bash scripts/ci/run_maintainability_checks.sh
}

stage_test() {
  local target_dir
  local -a test_args=(test --locked --workspace --all-targets)

  echo "    Running full workspace tests..."
  echo "    This step can take a while. Test progress will be printed below."
  # Pinned to pr-ci.yml: default features + --all-targets must stay.
  # Propagate the fail-fast policy into the test runner. cargo test defaults to
  # --no-fail-fast (run every test and report all failures at the end); when
  # --no-fail-fast is requested pass it explicitly. There is no `--fail-fast`
  # flag on cargo test: the default (fail-fast locally) means the workspace
  # test stage simply runs the normal default and stops the gate at the first
  # failing stage.
  if [[ "$fail_fast" -eq 0 ]]; then
    test_args+=(--no-fail-fast)
  fi

  # Linux local-only adapter tests exercise the same loopback boundary that
  # shipped archives provide through the internal launcher. Build it before
  # testing and bind the path explicitly so Cargo's test harness can locate it
  # even when CARGO_TARGET_DIR is redirected by CI or container parity jobs.
  if [[ "$(uname -s)" == "Linux" ]]; then
    cargo build --locked -p transist-sandbox-launcher
    target_dir="${CARGO_TARGET_DIR:-target}"
    if [[ "$target_dir" != /* ]]; then
      target_dir="$PWD/$target_dir"
    fi
    TRANSIST_SANDBOX_LAUNCHER="$target_dir/debug/transist-sandbox-launcher" \
      cargo "${test_args[@]}"
  else
    cargo "${test_args[@]}"
  fi
}

stage_release() {
  echo "    Building release binaries..."
  cargo build --locked --release -p transist-cli -p transist-server -p transist-admin
}

stage_inventory() {
  echo "    Checking canonical release archive inventory vs docs..."
  bash scripts/ci/check_release_inventory.sh
}

stage_archive() {
  local host_os host_arch host_target_triple rustc_host
  echo "    Testing archive reproducibility..."
  host_os="$(uname -s)"
  host_arch="$(uname -m)"
  rustc_host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
  # Preserve the compiler ABI for every target in the release archive inventory.
  case "$rustc_host" in
    aarch64-apple-darwin) host_target_triple="aarch64-apple-darwin" ;;
    x86_64-apple-darwin) host_target_triple="x86_64-apple-darwin" ;;
    x86_64-unknown-linux-gnu) host_target_triple="x86_64-unknown-linux-gnu" ;;
    x86_64-unknown-linux-musl) host_target_triple="x86_64-unknown-linux-musl" ;;
    aarch64-unknown-linux-gnu) host_target_triple="aarch64-unknown-linux-gnu" ;;
    aarch64-unknown-linux-musl) host_target_triple="aarch64-unknown-linux-musl" ;;
    *)
      case "${host_os}/${host_arch}" in
        Darwin/arm64) host_target_triple="aarch64-apple-darwin" ;;
        Darwin/x86_64) host_target_triple="x86_64-apple-darwin" ;;
        Linux/x86_64) host_target_triple="x86_64-unknown-linux-gnu" ;;
        Linux/aarch64|Linux/arm64) host_target_triple="aarch64-unknown-linux-gnu" ;;
        *)
          echo "FAIL: unsupported host for archive reproducibility: ${host_os}/${host_arch} (rustc host $rustc_host)" >&2
          echo "      supported Rust hosts: aarch64-apple-darwin, x86_64-apple-darwin," >&2
          echo "        x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl," >&2
          echo "        aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl" >&2
          return 1
          ;;
      esac
      ;;
  esac
  # Host selection must stay native: a recognized compiler triple cannot stand
  # in for another OS or architecture. GNU/musl ABI selection remains supported.
  case "$host_target_triple" in
    aarch64-apple-darwin) [[ "${host_os}/${host_arch}" == "Darwin/arm64" ]] ;;
    x86_64-apple-darwin) [[ "${host_os}/${host_arch}" == "Darwin/x86_64" ]] ;;
    x86_64-unknown-linux-*) [[ "${host_os}/${host_arch}" == "Linux/x86_64" ]] ;;
    aarch64-unknown-linux-*) [[ "$host_os" == "Linux" && ( "$host_arch" == "aarch64" || "$host_arch" == "arm64" ) ]] ;;
  esac || {
    echo "FAIL: archive target '$host_target_triple' does not match native host ${host_os}/${host_arch}" >&2
    return 1
  }
  bash scripts/ci/test_archive_reproducibility.sh "$host_target_triple"
}

stage_smoke() {
  echo "    Running quickstart smoke..."
  bash scripts/ci/run_quickstart_smoke.sh
}

stage_e2e() {
  echo "    Running E2E suites..."
  command -v jq >/dev/null || { echo "FAIL: jq not found" >&2; return 1; }
  command -v python3 >/dev/null || { echo "FAIL: python3 not found" >&2; return 1; }
  python3 .agents/testing/test_run_e2e.py
  python3 .agents/testing/run_e2e.py --ci
}

stage_workflows() {
  echo "    Running shipped-workflow coverage..."
  command -v python3 >/dev/null || { echo "FAIL: python3 not found" >&2; return 1; }
  bash scripts/ci/run_shipped_workflows.sh
}

echo ""
echo "Transist local PR-readiness gate starting."

# ---------------------------------------------------------------------------
# Phase A — core checks (debug/test profile)
# ---------------------------------------------------------------------------
# Ordering is Test-first so a broken change surfaces the most informative
# failure before paying the fmt/clippy compile; fmt and clippy then run
# back-to-back. maintainability is pure shell and runs next to a compile stage
# without disturbing the build cache; under --parallel it runs concurrently
# with Test (see below). Feature sets are intentionally unchanged from
# pr-ci.yml.

if [[ "$parallel" -eq 1 ]]; then
  # Only maintainability is safe to background next to a compile stage: it is
  # pure shell. cargo stages are never spawned concurrently here because cargo
  # holds a file lock on the target directory; two cargo processes would just
  # serialize on that lock (no speedup) while also contending for CPU.
  maint_start="$(now_epoch)"
  stage_maintainability &
  maint_pid=$!
  run_stage "test" stage_test
  run_stage "fmt" stage_fmt
  run_stage "clippy" stage_clippy
  run_stage "docs" stage_docs
  maint_rc=0
  if ! wait "$maint_pid"; then
    maint_rc=1
  fi
  maint_elapsed=$(( $(now_epoch) - maint_start ))
  stage_times+=("maintainability|${maint_elapsed}")
  if [[ "$maint_rc" -ne 0 ]]; then
    echo "  <<< [maintainability] FAILED (ran concurrently with compile phase)" >&2
    if [[ "$fail_fast" -eq 1 ]]; then
      exit 1
    fi
    stage_failures+=("maintainability")
  fi
else
  run_stage "test" stage_test
  run_stage "fmt" stage_fmt
  run_stage "clippy" stage_clippy
  run_stage "docs" stage_docs
  run_stage "maintainability" stage_maintainability
fi

# ---------------------------------------------------------------------------
# Phase B — release-readiness tail (release profile + packaging + runtime E2E)
# ---------------------------------------------------------------------------
# These are the expensive release contracts that --fast and the --skip-* flags
# remove. release build and archive reproducibility stay adjacent: the archive
# repackaging reuses target/release, and running them back-to-back minimizes
# release-profile cache churn.
if [[ "$fast" -eq 0 ]]; then
  run_stage "inventory" stage_inventory
  run_stage "release" stage_release
  if [[ "$skip_archive" -eq 0 ]]; then
    run_stage "archive" stage_archive
  fi
  if [[ "$skip_smoke" -eq 0 ]]; then
    run_stage "smoke" stage_smoke
  fi
  if [[ "$skip_e2e" -eq 0 ]]; then
    run_stage "e2e" stage_e2e
  fi
  if [[ "$skip_workflows" -eq 0 ]]; then
    run_stage "workflows" stage_workflows
  fi
else
  echo ""
  echo "NOTE: --fast mode stopped after core checks; release tail skipped."
fi

if [[ ${#stage_failures[@]} -gt 0 ]]; then
  echo ""
  echo "PR readiness checks FAILED. Failed stage(s):"
  for label in "${stage_failures[@]}"; do
    echo "  - $label"
  done
  echo "Run the full gate (no flags) after fixing the failures; also consider"
  echo "check_release_readiness.sh before tagging a release."
  exit 1
fi

echo ""
echo "PR readiness checks passed."
echo "NOTE  For full release readiness, run: bash scripts/ci/check_release_readiness.sh"
