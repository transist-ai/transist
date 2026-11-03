#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

output_dir="${1:-coverage}"
rm -rf "$output_dir"
mkdir -p "$output_dir"

# Keep compiler locations portable in LCOV, text, and HTML output. The final
# assertion below is deliberately retained as a regression test for this
# artifact boundary.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${ROOT_DIR}=."

echo "Running default-feature library, binary, and integration-test targets with pinned cargo-llvm-cov instrumentation..."
# Do not add --all-features here. Coverage intentionally measures the
# production-default feature surface; development-only bypass behavior is
# covered by focused feature-enabled tests in the security regression gate.
# Keep the test invocation separate from report generation so every report uses
# the same captured profiles. No fail-under option is intentional: this job
# publishes evidence and does not turn an uncalibrated percentage into a gate.
cargo llvm-cov --locked --workspace --lib --bins --tests --no-report

echo "Generating HTML coverage report..."
cargo llvm-cov report --html --output-dir "$output_dir"

echo "Generating LCOV coverage report..."
cargo llvm-cov report --lcov --output-path "$output_dir/lcov.info"

echo "Generating textual coverage summary..."
# The default report is the compact per-file/total summary. Do not use
# `--text` here: that format is a complete line-annotated source listing and
# streaming it below can exceed hosted CI log limits. HTML and LCOV retain the
# full-detail artifacts.
cargo llvm-cov report > "$output_dir/summary.txt"

# Some llvm-cov versions retain the checkout prefix in report source names even
# when rustc debug paths are remapped. Normalize every generated artifact so
# reports can be downloaded and compared across workspaces.
python3 - "$ROOT_DIR" "$output_dir" <<'PY'
from pathlib import Path
import sys

root = sys.argv[1].encode()
output_dir = Path(sys.argv[2])
for path in output_dir.rglob("*"):
    if path.is_file():
        data = path.read_bytes()
        if root in data:
            path.write_bytes(data.replace(root, b"."))
PY

if grep -R -F -n -- "$ROOT_DIR" "$output_dir" >/dev/null; then
  echo "FAIL: coverage reports contain the workspace absolute path: $ROOT_DIR" >&2
  exit 1
fi

cat "$output_dir/summary.txt"

echo "Coverage reports written to $output_dir"
