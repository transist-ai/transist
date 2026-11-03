#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# test_sbom_cli_compatibility.sh — Verify the pinned cargo-cyclonedx version
# supports the exact command-line options used in the release workflow.
#
# This catches regressions where release.yml passes options (e.g. --locked,
# --output) that the pinned cargo-cyclonedx version does not recognize.
# The test is a hosted CI fixture: it runs after install_ci_tools.sh installs
# the pinned cargo-cyclonedx, then checks --help output and the release.yml
# invocation against the supported option set.
#
# Exit status: 0 (all checks pass) or 1 (any check fails).

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

failures=0

# --- Check 1: cargo-cyclonedx is installed and supports required options ---
if ! command -v cargo-cyclonedx >/dev/null 2>&1; then
  echo "FAIL: cargo-cyclonedx is not installed (run install_ci_tools.sh release first)" >&2
  exit 1
fi

help_output="$(cargo cyclonedx --help 2>&1)"

# Required options that the release workflow depends on.
required_options=("--spec-version" "--all" "--format")
for opt in "${required_options[@]}"; do
  if ! echo "$help_output" | grep -q -- "$opt"; then
    echo "FAIL: cargo-cyclonedx does not support $opt" >&2
    failures=$((failures + 1))
  fi
done

# Options that the pinned cargo-cyclonedx 0.5.x does NOT support and must
# never appear in the release workflow invocation.
unsupported_options=("--locked" "--output")
for opt in "${unsupported_options[@]}"; do
  if echo "$help_output" | grep -q -- "$opt"; then
    echo "FAIL: cargo-cyclonedx unexpectedly supports $opt (update the unsupported list)" >&2
    failures=$((failures + 1))
  fi
done

# --- Check 2: release.yml cargo cyclonedx invocation uses only supported options ---
release_yml=".github/workflows/release.yml"
if ! grep -q 'cargo cyclonedx --all --format json --spec-version 1.5' "$release_yml"; then
  echo "FAIL: release.yml does not invoke 'cargo cyclonedx --all --format json --spec-version 1.5'" >&2
  failures=$((failures + 1))
fi

# The release invocation must not use unsupported options.
if grep -q 'cargo cyclonedx.*--locked' "$release_yml"; then
  echo "FAIL: release.yml uses --locked with cargo cyclonedx (not supported by pinned version)" >&2
  failures=$((failures + 1))
fi
if grep -q 'cargo cyclonedx.*--output' "$release_yml"; then
  echo "FAIL: release.yml uses --output with cargo cyclonedx (not supported by pinned version)" >&2
  failures=$((failures + 1))
fi

# --- Check 3: spec-version 1.5 is explicitly requested ---
if ! grep -q 'cargo cyclonedx --all --format json --spec-version 1.5' "$release_yml"; then
  echo "FAIL: release.yml must explicitly request --spec-version 1.5" >&2
  failures=$((failures + 1))
fi

echo ""
echo "============================================================"
if [[ "$failures" -eq 0 ]]; then
  echo "  RESULT: All SBOM CLI compatibility checks passed"
else
  echo "  RESULT: $failures SBOM CLI compatibility check(s) failed"
fi
echo "============================================================"
exit "$failures"
