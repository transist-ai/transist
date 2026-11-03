#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# Guards the companion server's inspection-only boundary by blocking
# execution/mutation dependencies, non-GET route registration, and any
# filesystem-write or home-creation calls in the server crate.
#
# Every filesystem-mutation API family that could reach the disk is scanned:
# `fs::write` (std and tokio), `File::create`, writable `OpenOptions`,
# `remove_file`, `rename`, `create_dir_all`, `set_permissions`, and the
# `std::io::Write` trait methods (`write` / `write_all`). Inline
# `#[cfg(test)]` modules are excluded so tests may legitimately touch the
# filesystem without weakening the boundary.
#
# Set COMPANION_BOUNDARY_SCOPE to a different path to check an arbitrary
# non-test `.rs` tree (used by the negative fixture test).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

if ! command -v rg >/dev/null 2>&1; then
  echo "Companion server boundary check requires ripgrep (rg)." >&2
  exit 2
fi

scope="${COMPANION_BOUNDARY_SCOPE:-crates/transist-server/src}"
fail=0
tmp_scope=""

cleanup() {
  if [[ -n "$tmp_scope" && -d "$tmp_scope" ]]; then
    rm -rf -- "$tmp_scope"
  fi
}
trap cleanup EXIT

# Strip inline `#[cfg(test)]` modules from a source file to stdout so that
# test-only filesystem access is never flagged as a boundary violation.
strip_tests_from() {
  local src="$1"
  python3 - "$src" <<'PY'
import re
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    lines = f.read().split("\n")

start_re = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\b")
out = []
pending = False
in_test = False
brace = 0

for line in lines:
    if in_test:
        brace += line.count("{") - line.count("}")
        if brace <= 0:
            in_test = False
            brace = 0
        continue
    if pending:
        pending = False
        brace = line.count("{") - line.count("}")
        if brace > 0:
            in_test = True
        continue
    if start_re.search(line):
        pending = True
        continue
    out.append(line)

sys.stdout.write("\n".join(out))
PY
}

# Build a sanitized copy of the scope with test modules removed, so all
# checks below scan only non-test server source (and negative fixtures).
scan_dir="$scope"
if [[ -d "$scope" ]]; then
  scan_dir="$(mktemp -d)"
  tmp_scope="$scan_dir"
  while IFS= read -r f; do
    rel="${f#"$scope"/}"
    mkdir -p "$(dirname "$scan_dir/$rel")"
    strip_tests_from "$f" >"$scan_dir/$rel"
  done < <(find "$scope" -type f -name '*.rs')
fi

check_forbidden() {
  local pattern="$1"
  local message="$2"
  if rg -n --hidden -g '!target/**' -e "$pattern" "$scan_dir" >/dev/null; then
    rg -n --hidden -g '!target/**' -e "$pattern" "$scan_dir" || true
    echo "$message" >&2
    fail=1
  fi
}

check_forbidden \
  'TransistContext|RunReport|PrepareReport|prepare_workflow|launch_workflow|run_prepared_run|run_started_run|runs_resume|runs_replay|approve_gate|reject_gate|request_changes_gate|compile_flow_file|validate_flow_file' \
  "Companion server boundary violation: server source must not depend on execution or mutation APIs."

check_forbidden \
  'transist_(inspection|metrics|storage_local)::' \
  "Companion server boundary violation: production composition must use the root transist facade."

check_forbidden \
  '\bpost\(|\bput\(|\bpatch\(|\bdelete\(' \
  "Companion server boundary violation: server routes must remain GET-only."

check_forbidden \
  'load_or_create' \
  "Companion server boundary violation: server must not use load_or_create (which creates files); use Installation::load instead."

check_forbidden \
  'write_json_atomic|write_string_atomic' \
  "Companion server boundary violation: server source must not call filesystem-write helpers."

check_forbidden \
  'fs::write|File::create' \
  "Companion server boundary violation: server source must not write files (std::fs::write / tokio::fs::write / File::create)."

check_forbidden \
  'OpenOptions' \
  "Companion server boundary violation: server source must not open files for writing (OpenOptions)."

check_forbidden \
  'remove_file|rename\s*\(|create_dir_all|set_permissions' \
  "Companion server boundary violation: server source must not mutate the filesystem (remove_file / rename / create_dir_all / set_permissions)."

check_forbidden \
  'use\s+(std|tokio)::io::Write|\.write_all\s*\(|\.write\s*\(' \
  "Companion server boundary violation: server source must not stream bytes to a file through the std::io::Write trait."

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "Companion server boundary check passed."
