#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Suite: run-cancellation
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
TRANSIST_BIN="$REPO_ROOT/target/debug/transist"
export TRANSIST_HOME="$(mktemp -d -t ".transist-run-cancellation-XXXXXX")"
export TRANSIST_HOME="$(cd "$TRANSIST_HOME" && pwd -P)"
SUITE_ROOT="$(mktemp -d -t "transist-run-cancellation-workspaces-XXXXXX")"
SUITE_ROOT="$(cd "$SUITE_ROOT" && pwd -P)"
trap 'rm -rf "$TRANSIST_HOME" "$SUITE_ROOT"' EXIT

"$TRANSIST_BIN" --home "$TRANSIST_HOME" home init --mode standalone
"$TRANSIST_BIN" --home "$TRANSIST_HOME" plugins install --path "$REPO_ROOT/plugins/shell/plugin.yaml"
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows install --path "$SCRIPT_DIR/direct.flow.yaml" --approve
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows install --path "$SCRIPT_DIR/nested.flow.yaml" --approve
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check e2e_cancel_direct@0.1.0 >/dev/null
"$TRANSIST_BIN" --home "$TRANSIST_HOME" workflows check e2e_cancel_nested@0.1.0 >/dev/null

python3 "$SCRIPT_DIR/run_cases.py" "$TRANSIST_BIN" "$TRANSIST_HOME" "$SUITE_ROOT"

echo "PASS: run-cancellation"
