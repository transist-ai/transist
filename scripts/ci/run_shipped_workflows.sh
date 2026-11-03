#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# run_shipped_workflows.sh — Hermetic execution coverage for every workflow
# shipped under workflows/.
#
# Usage:
#   bash scripts/ci/run_shipped_workflows.sh
#       # source checkout; uses target/debug/transist (fallback: release)
#   bash scripts/ci/run_shipped_workflows.sh --root <dir> --transist-bin <bin>
#   bash scripts/ci/run_shipped_workflows.sh --archive transist-<tag>-<target>.tar.gz
#       # runs the same inventory from an extracted release archive
#
# Every transist.flow.v1 file under workflows/ must be declared with a hermetic
# plan, fixture inputs, and an expected terminal result in
# scripts/ci/run_shipped_workflows.py; a new workflow without coverage fails.
# All tests use only committed fixtures and locally created temp homes and
# workspaces: no sibling repository, network service, or mutable external
# fixture is used.
#
# Exit status: 0 (every shipped workflow reached its declared result) or 1.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 "$ROOT_DIR/scripts/ci/test_shipped_workflows.py"
exec python3 "$ROOT_DIR/scripts/ci/run_shipped_workflows.py" "$@"
