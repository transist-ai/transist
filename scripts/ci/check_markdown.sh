#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Use the locked library with the shared root/nested JSONC selection and rules.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"
# shellcheck disable=SC1091
source scripts/ci/tool-versions.env
library="$ROOT_DIR/scripts/ci/markdown/node_modules/markdownlint/lib/exports.mjs"
if [[ ! -f "$library" ]]; then
  echo "FAIL: locked Markdown library is missing; run bash scripts/ci/install_ci_tools.sh markdown" >&2
  exit 1
fi
# Keep selection/rule regression fixtures on the hosted and local Markdown gate.
node --test "$ROOT_DIR/scripts/ci/markdown/test_markdown.mjs"
exec node "$ROOT_DIR/scripts/ci/markdown/run.mjs" "$MARKDOWNLINT_VERSION"
