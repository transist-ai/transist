#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Required finite, offline APP/sandbox suite; missing prerequisites are failures.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
exec python3 -B "$SCRIPT_DIR/run.py" \
  "${TRANSIST_BIN:-$REPO_ROOT/target/debug/transist}" \
  "${TRANSIST_ADMIN_BIN:-$REPO_ROOT/target/debug/transist-admin}"
