#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# Negative fixtures for the public root facade. Each fixture adds one
# forbidden re-export to a clean copy of src/lib.rs and verifies rejection.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
failures=0

record_failure() {
  echo "FAIL  $1" >&2
  failures=$((failures + 1))
}

record_pass() {
  echo "PASS  $1"
}

expect_reject() {
  local name="$1"
  local snippet="$2"
  local fixture
  fixture="$(mktemp -d)"
  mkdir -p "$fixture/src" "$fixture/scripts/ci"
  cp "$repo_root/src/lib.rs" "$fixture/src/lib.rs"
  cp "$repo_root/scripts/ci/check_root_facade.sh" "$fixture/scripts/ci/check_root_facade.sh"
  printf '\n%s\n' "$snippet" >>"$fixture/src/lib.rs"

  if (cd "$fixture" && bash scripts/ci/check_root_facade.sh) >/dev/null 2>&1; then
    record_failure "$name (boundary check accepted forbidden re-export)"
  else
    record_pass "$name"
  fi
  rm -rf -- "$fixture"
}

baseline="$(mktemp -d)"
mkdir -p "$baseline/src" "$baseline/scripts/ci"
cp "$repo_root/src/lib.rs" "$baseline/src/lib.rs"
cp "$repo_root/scripts/ci/check_root_facade.sh" "$baseline/scripts/ci/check_root_facade.sh"
if (cd "$baseline" && bash scripts/ci/check_root_facade.sh) >/dev/null 2>&1; then
  record_pass "current DTO, port, report, manifest, recipe, policy, and inspection exports accepted"
else
  record_failure "current allowed root facade exports rejected"
fi
rm -rf -- "$baseline"

expect_reject "plugin directory digest" \
  'pub use transist_plugins::plugin_dir_sha256;'
expect_reject "concrete Local implementation" \
  'pub use crate::composition::LocalHomeSettings;'
expect_reject "compiler implementation helper" \
  'pub use transist_compiler::compile_flow;'
expect_reject "storage-local helper" \
  'pub use transist_storage_local::write_json_atomic;'
expect_reject "adapter constructor" \
  'pub use transist_adapters::AdapterRouter;'

if [[ "$failures" -ne 0 ]]; then
  echo "Root facade boundary regression tests failed with $failures failure(s)." >&2
  exit 1
fi

echo "Root facade boundary regression tests passed."
