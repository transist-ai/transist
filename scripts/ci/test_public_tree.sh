#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# Focused regression tests for check_public_tree.sh. Each case is staged in a
# tiny temporary Git tree so the test exercises the tracked-tree boundary.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCANNER="$ROOT_DIR/scripts/ci/check_public_tree.sh"
failures=0

new_tree() {
  local tree
  tree="$(mktemp -d)"
  git -C "$tree" init -q
  git -C "$tree" config user.email "test@example.com"
  git -C "$tree" config user.name "Public Tree Test"
  printf '%s\n' "$tree"
}

stage_tree() {
  local tree="$1"
  git -C "$tree" add .
  git -C "$tree" commit -q -m "init"
}

# Runs the scanner in BOTH default (working-tree) mode and committed-tree
# (TREEISH) mode against `HEAD` of the fixture tree. `label` names the case.
run_scanner_modes() {
  local label="$1"
  local tree="$2"
  local fail_expected="$3"
  local worktree_ok committed_ok

  if ! bash "$SCANNER" "$tree" >/tmp/public-tree-test.out 2>&1; then
    worktree_ok=0
  else
    worktree_ok=1
  fi

  if ! bash "$SCANNER" "$tree" HEAD >/tmp/public-tree-test.out 2>&1; then
    committed_ok=0
  else
    committed_ok=1
  fi

  if [[ "$fail_expected" -eq 1 ]]; then
    [[ "$worktree_ok" -eq 0 ]] || { printf 'FAIL: violation case passed in worktree mode: %s\n' "$label" >&2; failures=$((failures + 1)); }
    [[ "$committed_ok" -eq 0 ]] || { printf 'FAIL: violation case passed in committed-tree mode: %s\n' "$label" >&2; failures=$((failures + 1)); }
  else
    [[ "$worktree_ok" -eq 1 ]] || { printf 'FAIL: clean-tree case failed in worktree mode: %s\n' "$label" >&2; failures=$((failures + 1)); }
    [[ "$committed_ok" -eq 1 ]] || { printf 'FAIL: clean-tree case failed in committed-tree mode: %s\n' "$label" >&2; failures=$((failures + 1)); }
  fi
}

expect_pass() {
  local name="$1"
  local tree="$2"
  run_scanner_modes "$name" "$tree" 0
  rm -f /tmp/public-tree-test.out
}

expect_fail() {
  local name="$1"
  local tree="$2"
  run_scanner_modes "$name" "$tree" 1
  rm -f /tmp/public-tree-test.out
}

clean="$(new_tree)"
mkdir -p "$clean/trust"
printf '%s\n' '{"schema":"transist.trust_root.v1","public_key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}' >"$clean/trust/example.pub"
printf '%s\n' '{"schema":"transist.signature.v1","signature":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}' >"$clean/workflow.yaml.sig"
stage_tree "$clean"
expect_pass "public trust material" "$clean"
rm -rf "$clean"

run_case() {
  local name="$1"
  local relative_path="$2"
  local contents="$3"
  local tree
  tree="$(new_tree)"
  mkdir -p "$(dirname "$tree/$relative_path")"
  printf '%s\n' "$contents" >"$tree/$relative_path"
  stage_tree "$tree"
  expect_fail "$name" "$tree"
  rm -rf "$tree"
}

run_clean_case() {
  local name="$1"
  local relative_path="$2"
  local contents="$3"
  local tree
  tree="$(new_tree)"
  mkdir -p "$(dirname "$tree/$relative_path")"
  printf '%s\n' "$contents" >"$tree/$relative_path"
  stage_tree "$tree"
  expect_pass "$name" "$tree"
  rm -rf "$tree"
}

run_case "local absolute developer path" README.md 'checkout: /Users/example/Documents/transist'
run_case "internal release evidence" reports/release-evidence.json '{}'
run_case "candidate scratch output" .tmp/scratch.bin 'scratch'
run_case "private key filename" trust/test.key '{}'
run_case "PEM private key" trust/material.txt '-----BEGIN PRIVATE KEY-----'
run_case "JSON private key" config.json '{"private_key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}'
run_case "forbidden generated file" target/output.bin 'generated'

run_clean_case "neutral example domain" tests/input.txt 'host: example.com'
run_clean_case "neutral example repository" tests/input.txt 'repository: example-org/example-repo'
run_clean_case "neutral issue fixture" tests/input.txt 'ticket: ISSUE-1234'
run_clean_case "release readiness gate output" scripts/ci/gate.sh 'RELEASE READINESS GATE PASSED'
run_clean_case "neutral governed wording" docs/notes.md 'governed runs'

# Committed-tree mode scans commit blobs, not the edited working tree. A
# violating file committed at HEAD must still be rejected by the TREEISH scan
# even after the working tree removes it (the reverse proves the worktree scan
# is not reading blobs).
committed_blob_tree="$(new_tree)"
mkdir -p "$committed_blob_tree/trust"
printf '%s\n' '{"private_key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}' >"$committed_blob_tree/trust/leak.json"
stage_tree "$committed_blob_tree"
rm -f "$committed_blob_tree/trust/leak.json"
if bash "$SCANNER" "$committed_blob_tree" >/tmp/public-tree-test.out 2>&1; then
  echo "OK: worktree mode scans only the current tree (deleted blob not scanned)"
else
  printf 'FAIL: worktree mode rejected the current tree after the violation was removed\n' >&2
  failures=$((failures + 1))
fi
if bash "$SCANNER" "$committed_blob_tree" HEAD >/tmp/public-tree-test.out 2>&1; then
  printf 'FAIL: committed-tree mode ignored the violating committed blob\n' >&2
  failures=$((failures + 1))
fi
rm -f /tmp/public-tree-test.out
rm -rf "$committed_blob_tree"

if [[ "$failures" -ne 0 ]]; then
  printf 'Public-tree regression tests failed with %d failure(s).\n' "$failures" >&2
  exit 1
fi

echo "Public-tree regression tests passed."
