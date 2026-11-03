#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# Crash-durability regression suite for the local persistence layer:
# staged run birth, JSONL tail repair, malformed-record rejection,
# and v1 run readability without migration.
cargo +1.88.0 test --locked -p transist-storage-local --test run_store_recovery
cargo +1.88.0 test --locked -p transist-storage-local --test storage_integration
cargo +1.88.0 test --locked --test runtime_failure_path_regressions persistence
