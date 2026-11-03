#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# Negative fixtures for the companion server read-only boundary. Each
# forbidden write/mutation API is planted in a temporary non-test source file,
# the hardened check must reject it, and the fixture is removed afterwards.

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
  local dir
  dir="$(mktemp -d)"
  printf '%s\n' 'fn fixture() {' "$snippet" '}' >"$dir/fixture.rs"
  if COMPANION_BOUNDARY_SCOPE="$dir" bash "$repo_root/scripts/ci/check_companion_server_boundary.sh" >/dev/null 2>&1; then
    record_failure "$name (boundary check accepted forbidden pattern)"
  else
    record_pass "$name"
  fi
  rm -rf -- "$dir"
}

# std::fs::write / tokio::fs::write
expect_reject "std::fs::write" 'std::fs::write("/tmp/x", b"data").unwrap();'
expect_reject "tokio::fs::write" 'tokio::fs::write("/tmp/x", b"data").await.unwrap();'

# File::create
expect_reject "File::create" 'let _f = File::create("/tmp/x").unwrap();'

# OpenOptions (writable open)
expect_reject "OpenOptions" 'let _f = OpenOptions::new().write(true).open("/tmp/x");'

# remove_file / rename / create_dir_all / set_permissions
expect_reject "remove_file" 'std::fs::remove_file("/tmp/x").unwrap();'
expect_reject "rename" 'std::fs::rename("/tmp/a", "/tmp/b").unwrap();'
expect_reject "create_dir_all" 'std::fs::create_dir_all("/tmp/x").unwrap();'
expect_reject "set_permissions" 'std::fs::set_permissions("/tmp/x", perms).unwrap();'

# std::io::Write trait methods used on file objects
expect_reject "use std::io::Write" 'use std::io::Write;'
expect_reject "tokio::io::Write import" 'use tokio::io::Write;'
expect_reject "write_all" 'writer.write_all(b"data").unwrap();'
expect_reject "write" 'writer.write(b"data").unwrap();'

# Sanity: clean non-test source must not be rejected.
dir="$(mktemp -d)"
printf '%s\n' 'fn read_only() {' '  let _s = std::fs::read("/tmp/data").unwrap();' '}' >"$dir/fixture.rs"
if COMPANION_BOUNDARY_SCOPE="$dir" bash "$repo_root/scripts/ci/check_companion_server_boundary.sh" >/dev/null 2>&1; then
  record_pass "clean read-only source accepted"
else
  record_failure "clean read-only source accepted"
fi
rm -rf -- "$dir"

# A helper named `rename` (not a filesystem mutation) is not a false positive.
dir="$(mktemp -d)"
printf '%s\n' 'fn lookup(name: &str) -> &str {' '  let rename = name;' '  rename' '}' >"$dir/fixture.rs"
if COMPANION_BOUNDARY_SCOPE="$dir" bash "$repo_root/scripts/ci/check_companion_server_boundary.sh" >/dev/null 2>&1; then
  record_pass "non-call rename identifier accepted"
else
  record_failure "non-call rename identifier accepted"
fi
rm -rf -- "$dir"

if [[ "$failures" -ne 0 ]]; then
  echo "Companion server boundary regression tests failed with $failures failure(s)." >&2
  exit 1
fi

echo "Companion server boundary regression tests passed."
