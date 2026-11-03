#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# Security-critical regression surface for dependency updates and release
# candidates: trust verification and lifecycle, plugin package hardening,
# reserved test-version refusal in release builds, local-only and policy
# enforcement, secret scoping, schema contract, and snapshot export.
cargo test --locked \
  --test admin_round_trip \
  --test trust_lifecycle \
  --test trusted_execution_runtime \
  --test gov_demo_negative \
  --test gov_demo_local_only \
  --test secret_persistence_guardrails \
  --test schema_contract \
  --test snapshot_sync_policy \
  --test snapshot_redaction

cargo test --locked -p transist-cli --test workflow_install --test trust_lifecycle --test snapshot_export
cargo test --locked -p transist-cli --test workflow_install workflow_install_rejects_v0_workflow_without_dev_bypass

# Retain the default-feature refusal test above, then explicitly prove that the
# non-default development feature admits v0 only through the audited bypass.
cargo test --locked --features dev-version-bypass --test gov_demo_bypass
cargo test --locked -p transist-cli --features dev-version-bypass --test workflow_install workflow_install_accepts_v0_workflow_with_dev_bypass

# Covers detached trust signing/verification hardening and plugin package
# symlink/special-file rejection on the admin toolchain surface.
cargo test --locked -p transist-admin --test admin_cli

# Covers install-time plugin package hardening for symlink/special-file rejection.
cargo test --locked -p transist-plugins

# Crash-durability regression: staged run birth, JSONL tail repair,
# malformed-record rejection, and v1 backward readability.
bash scripts/ci/check_crash_recovery.sh

# The companion server read-only boundary must reject every planted
# filesystem-mutation API family in its negative fixtures.
bash scripts/ci/test_companion_server_boundary.sh

echo "Security regression suite passed."
