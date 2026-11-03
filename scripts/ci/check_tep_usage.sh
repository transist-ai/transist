#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

fail=0

check_forbidden() {
  local scope="$1"
  local pattern="$2"
  local message="$3"
  # shellcheck disable=SC2206
  local paths=($scope)
  if rg -n --hidden -g '!target/**' -e "$pattern" "${paths[@]}" >/dev/null; then
    rg -n --hidden -g '!target/**' -e "$pattern" "${paths[@]}" || true
    echo "$message" >&2
    fail=1
  fi
}

if [[ ! -f crates/transist-protocol/src/app.rs ]]; then
  echo "TEP/APP usage violation: missing APP protocol module." >&2
  fail=1
fi

check_forbidden \
  "schemas/plugin/plugin-manifest.schema.json" \
  '"url"' \
  "TEP/APP usage violation: remote MCP URL surface remains in the plugin manifest schema."

if ! rg -n --hidden -g '!target/**' -e 'adapter_kind' crates/transist-core/src crates/transist-protocol/src >/dev/null; then
  echo "TEP/APP usage violation: adapter_kind is not present in TEP request handling." >&2
  fail=1
fi

if ! rg -n --hidden -g '!target/**' -e 'plugin_id' crates/transist-core/src crates/transist-adapters/src >/dev/null; then
  echo "TEP/APP usage violation: plugin metadata is not threaded through runtime/adapters." >&2
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "TEP/APP usage check passed."
