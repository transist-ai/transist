#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Evaluate GitHub job-result environment bindings without running any checks.
# Usage: bash scripts/ci/check_gate_results.sh preflight|pr|main
# Missing, empty, failed, cancelled, and unexpected results fail closed. Only
# draft PRs, Dependabot sign-off skips, and disabled dependency-review skips
# receive the existing PR exceptions. Exit 0 means the selected gate passed.
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "Usage: $0 preflight|pr|main" >&2
  exit 2
fi
mode="$1"
case "$mode" in
  preflight|pr|main) ;;
  *) echo "Unknown gate mode: $mode" >&2; exit 2 ;;
esac

# Require job name ($1) and result ($2); emit a diagnostic and return 1 unless
# the result is success. This helper has no side effects beyond stderr output.
require_success() {
  local job="$1" result="$2"
  if [[ "$result" != success ]]; then
    echo "$job did not succeed: ${result:-missing}" >&2
    return 1
  fi
}

if [[ "$mode" == main ]]; then
  require_success platform-smoke "${PLATFORM_RESULT:-}"
  require_success musl-smoke "${MUSL_RESULT:-}"
  require_success coverage "${COVERAGE_RESULT:-}"
  echo "Mainline Validation gate passed for ${GITHUB_SHA:-unknown}."
  exit 0
fi

if [[ "${IS_DRAFT:-false}" == true ]]; then
  echo "PR is a draft; upstream validation skipped until it is marked ready."
  exit 0
fi

require_success ci-static "${CI_STATIC_RESULT:-}"
if [[ "${IS_DEPENDABOT:-false}" != true || "${COMMIT_SIGNOFF_RESULT:-}" != skipped ]]; then
  require_success commit-signoff "${COMMIT_SIGNOFF_RESULT:-}"
fi
if [[ "${DEPENDENCY_REVIEW_ENABLED:-}" == true || "${DEPENDENCY_REVIEW_RESULT:-}" != skipped ]]; then
  require_success dependency-review "${DEPENDENCY_REVIEW_RESULT:-}"
fi
require_success cargo-deny "${CARGO_DENY_RESULT:-}"
require_success markdownlint "${MARKDOWNLINT_RESULT:-}"

if [[ "$mode" == pr ]]; then
  require_success preflight-gate "${PREFLIGHT_RESULT:-}"
  require_success validate-full "${VALIDATE_RESULT:-}"
  require_success schema-api-contracts "${SCHEMA_RESULT:-}"
  require_success coverage "${COVERAGE_RESULT:-}"
fi
echo "$mode gate passed."
