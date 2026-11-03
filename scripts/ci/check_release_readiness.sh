#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_release_readiness.sh — Canonical aggregate gate for public release
# readiness.
#
# Orchestrates every required local check in one invocation:
#   - format (cargo +1.88.0 fmt --all -- --check)
#   - clippy (cargo +1.88.0 clippy --locked --workspace --all-targets)
#   - rustdoc (RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace
#     --all-features --no-deps)
#   - workspace tests (cargo +1.88.0 test --locked --workspace --all-targets
#     and --all-targets --all-features)
#   - focused security regression suite
#   - maintainability and boundary checks (kernel, runtime signing, companion
#     server boundary + fixtures, workspace deps, publish policy, pre-1.0
#     claims, extension feature flags, missing docs, docs/skills validation,
#     no prerequisite execution, TEP/APP usage)
#   - supply-chain pins (pinned Actions, tools, digests)
#   - legal notices (SPDX headers, license audit)
#   - secret scan (full git history, not only current tree)
#   - public-tree scan (tracked candidate tree)
#   - schema and OpenAPI contract parity
#   - release archive inventory (canonical matrix vs docs drift)
#   - release build (three common binaries plus the Linux sandbox launcher)
#   - admin sign/verify conformance
#   - archive reproducibility (deterministic packaging)
#   - quickstart smoke (offline local-only contract)
#   - coverage evidence and quality signals
#   - release invariants (manifest sorting, tamper rejection, draft comparison,
#     SBOM reproducibility, release-candidate dry-run)
#   - E2E suites (all required suites via run_e2e.py --ci)
#   - shipped-workflow coverage (every workflow under workflows/ runs hermetically)
#
# This script is the canonical aggregate referenced by check_pr_ready.sh,
# CONTRIBUTING.md, and the PR template.
#
# Exit status: 0 (all checks pass) or 1 (any check fails).

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

step=0
total=28
hosted_checks=("Mainline Validation / main-gate (Linux + macOS + Alpine/musl release smoke)" "Security Scan / security-gate (weekly, manual, release staging)")

ADMIN_SIGN_OUTPUT_DIR=""
COVERAGE_OUTPUT_DIR=""
TRUST_FIXTURES_PARENT=""
cleanup() {
  [[ -z "$ADMIN_SIGN_OUTPUT_DIR" ]] || rm -rf "$ADMIN_SIGN_OUTPUT_DIR"
  [[ -z "$COVERAGE_OUTPUT_DIR" ]] || rm -rf "$COVERAGE_OUTPUT_DIR"
  [[ -z "$TRUST_FIXTURES_PARENT" ]] || rm -rf "$TRUST_FIXTURES_PARENT"
}
trap cleanup EXIT

pass() {
  echo "PASS  $1"
}

run_step() {
  step=$((step + 1))
  local label="$1"
  echo ""
  echo "=== [$step/$total] $label ==="
  shift
  "$@"
  pass "$label completed"
}

# ---------------------------------------------------------------------------
# 1. Candidate identity and clean tree
# ---------------------------------------------------------------------------
candidate_sha="$(git rev-parse HEAD)"
candidate_tag="v$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' Cargo.toml | head -n 1)"
run_step "Candidate worktree is clean" bash scripts/ci/check_candidate_worktree.sh "$ROOT_DIR"

# ---------------------------------------------------------------------------
# 2. Format
# ---------------------------------------------------------------------------
run_step "Format" cargo +1.88.0 fmt --all -- --check

# ---------------------------------------------------------------------------
# 3. Clippy
# ---------------------------------------------------------------------------
run_step "Clippy" cargo +1.88.0 clippy --locked --workspace --all-targets --all-features -- -D warnings

# ---------------------------------------------------------------------------
# 4. Rustdoc
# ---------------------------------------------------------------------------
run_step "Rustdoc" env RUSTDOCFLAGS="-D warnings" cargo +1.88.0 doc --locked --workspace --all-features --no-deps

# ---------------------------------------------------------------------------
# 5. Workspace tests (default features)
# ---------------------------------------------------------------------------
run_step "Workspace tests" cargo +1.88.0 test --locked --workspace --all-targets

# ---------------------------------------------------------------------------
# 5b. Workspace tests (all features)
# ---------------------------------------------------------------------------
run_step "Workspace tests (all features)" cargo +1.88.0 test --locked --workspace --all-targets --all-features

# ---------------------------------------------------------------------------
# 6. Security regression suite
# ---------------------------------------------------------------------------
run_step "Security regression suite" bash scripts/ci/run_security_regression_suite.sh

# ---------------------------------------------------------------------------
# 7. Maintainability and boundary checks
# ---------------------------------------------------------------------------
run_step "Maintainability and boundary checks" bash scripts/ci/run_maintainability_checks.sh

# ---------------------------------------------------------------------------
# 8. No executable prerequisite execution
# ---------------------------------------------------------------------------
run_step "No prerequisite execution" bash scripts/ci/check_no_prerequisite_execution.sh

# ---------------------------------------------------------------------------
# 9. TEP/APP usage
# ---------------------------------------------------------------------------
run_step "TEP/APP usage" bash scripts/ci/check_tep_usage.sh

# ---------------------------------------------------------------------------
# 10. Supply-chain pins
# ---------------------------------------------------------------------------
run_step "Supply-chain pins" bash scripts/ci/check_supply_chain_pins.sh

# ---------------------------------------------------------------------------
# 11. SPDX headers
# ---------------------------------------------------------------------------
run_step "SPDX headers" bash scripts/ci/check_spdx_headers.sh

# ---------------------------------------------------------------------------
# 12. License audit
# ---------------------------------------------------------------------------
run_step "License audit" bash scripts/ci/check_licenses.sh

# ---------------------------------------------------------------------------
# 13. Secret scan (full git history)
# ---------------------------------------------------------------------------
run_step "Secret scan (full git history)" bash scripts/ci/check_secrets.sh

# ---------------------------------------------------------------------------
# 14. Public-tree scan (release-blocking)
# ---------------------------------------------------------------------------
run_step "Public-tree scan" bash scripts/ci/check_public_tree.sh "$ROOT_DIR" "$(git rev-parse HEAD)"
run_step "Public-tree regression tests" bash scripts/ci/test_public_tree.sh

# ---------------------------------------------------------------------------
# 15. Schema and API contracts
# ---------------------------------------------------------------------------
run_step "Schema and API contracts" bash scripts/ci/check_schema_api_contracts.sh

# ---------------------------------------------------------------------------
# 16. Release archive inventory (canonical matrix vs docs drift)
# ---------------------------------------------------------------------------
run_step "Release archive inventory" bash scripts/ci/check_release_inventory.sh

# ---------------------------------------------------------------------------
# 17. Release build
# ---------------------------------------------------------------------------
local_target="$(rustc -vV | sed -n 's/^host: //p')"
release_packages=(-p transist-cli -p transist-server -p transist-admin)
case "$local_target" in
  x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl)
    release_packages+=(-p transist-sandbox-launcher)
    ;;
esac
run_step "Release build" cargo +1.88.0 build --locked --release "${release_packages[@]}"

# ---------------------------------------------------------------------------
# 18. Admin sign/verify conformance
# ---------------------------------------------------------------------------
# Exercises transist-admin key generation, public-root export, and signature
# creation/verification for the shell plugin and quickstart workflow, then
# destroys the ephemeral private key and asserts no private-key material leaks
# into the output directory.
ADMIN_SIGN_OUTPUT_DIR="$(mktemp -d)"
run_step "Admin sign/verify conformance" \
  bash scripts/ci/admin_sign_verify.sh "$PWD/target/release/transist-admin" "$ADMIN_SIGN_OUTPUT_DIR"

# ---------------------------------------------------------------------------
# 19. Archive reproducibility
# ---------------------------------------------------------------------------
case "$local_target" in
  x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-apple-darwin)
    run_step "Archive reproducibility ($local_target)" bash scripts/ci/test_archive_reproducibility.sh "$local_target"
    ;;
  *)
    echo ""
    echo "=== [$((step + 1))/$total] Archive reproducibility (unsupported host) ==="
    echo "FAIL  local target $local_target is not a release archive host." >&2
    echo "      Supported archive hosts: x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-apple-darwin." >&2
    echo "      This host cannot produce or verify release archives; refusing to print a passing verdict." >&2
    step=$((step + 1))
    exit 1
    ;;
esac

# ---------------------------------------------------------------------------
# 20. Quickstart smoke
# ---------------------------------------------------------------------------
# Prepare trust fixtures in an isolated admin step (private key destroyed after).
command -v jq >/dev/null || { echo "FAIL: jq not found"; exit 1; }
command -v python3 >/dev/null || { echo "FAIL: python3 not found"; exit 1; }
TRUST_FIXTURES_PARENT="$(mktemp -d)"
TRUST_FIXTURES_DIR="$TRUST_FIXTURES_PARENT/trust-fixtures"
bash scripts/ci/prepare_e2e_trust_fixtures.sh "$PWD/target/debug/transist-admin" "$TRUST_FIXTURES_DIR" "$PWD"
export TRUST_FIXTURES_DIR
run_step "Quickstart smoke" bash scripts/ci/run_quickstart_smoke.sh --trust-dir "$TRUST_FIXTURES_DIR" --require-isolation

# ---------------------------------------------------------------------------
# 21. Coverage and quality evidence
# ---------------------------------------------------------------------------
COVERAGE_OUTPUT_DIR="$(mktemp -d -t transist-release-coverage-XXXXXX)"
run_step "Coverage reports" bash scripts/ci/run_coverage.sh "$COVERAGE_OUTPUT_DIR"

# ---------------------------------------------------------------------------
# 22. Release invariants
# ---------------------------------------------------------------------------
run_step "Release invariants" bash scripts/ci/test_release_invariants.sh

# ---------------------------------------------------------------------------
# 23. Exact candidate dry-run
# ---------------------------------------------------------------------------
run_step "Exact release-candidate dry-run" \
  bash scripts/ci/check_release_candidate.sh --dry-run "$candidate_tag" --sha "$candidate_sha"

# ---------------------------------------------------------------------------
# 24. E2E suites
# ---------------------------------------------------------------------------
run_step "E2E harness unit tests" python3 .agents/testing/test_run_e2e.py
run_step "E2E suites (--ci)" python3 .agents/testing/run_e2e.py --ci

# ---------------------------------------------------------------------------
# 18. Shipped workflows (hermetic execution coverage for workflows/)
# ---------------------------------------------------------------------------
run_step "Shipped workflows" bash scripts/ci/run_shipped_workflows.sh

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "============================================================"
echo "  RELEASE READINESS GATE PASSED"
echo "  Commit SHA: $(git rev-parse HEAD)"
echo "  All $step host-local checks ran and completed."
echo "  Required hosted checks: ${hosted_checks[*]}."
echo "  This host cannot attest to platform checks it did not run."
echo "============================================================"
