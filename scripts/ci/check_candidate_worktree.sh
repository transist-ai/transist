#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_candidate_worktree.sh — Fail when a release candidate cannot be built
# and packaged from the committed tree.
#
# Rejects staged, unstaged, untracked, and ignored files unless an ignored path
# is an explicitly allowlisted generated output. This protects build-control
# inputs such as .cargo/config.toml even when a broad ignore rule hides them.
# Generated roots such as target/, dist/, and .tmp/ are intentionally pruned
# from ignored-input inspection for performance; they are build or scratch
# output and are outside the materialized release candidate. Public trust
# signatures/roots, CI-tool node_modules, public fixture outputs, and the
# documented smoke marker files are inspected separately. Candidate-controlled
# ignored paths outside the pruned roots remain release-blocking, including
# config, key, and secret material.
#
# Usage: check_candidate_worktree.sh <repo-root>
#
# Exit status: 0 (clean) or 1 (dirty).

set -euo pipefail

repo_root="${1:?usage: check_candidate_worktree.sh <repo-root>}"
repo_root="$(cd "$repo_root" && pwd)"

if ! git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "FAIL: candidate worktree root is not a Git worktree: $repo_root" >&2
  exit 1
fi

unstaged="$(git -C "$repo_root" diff --name-only 2>/dev/null)"
staged="$(git -C "$repo_root" diff --cached --name-only 2>/dev/null)"
untracked="$(git -C "$repo_root" ls-files --others --exclude-standard 2>/dev/null)"

is_ignored_secret() {
  local path="$1"
  local full_path="$repo_root/$path"
  case "$path" in
    *.key|*.pem|*.p12|*.pfx|*.jks|*.keystore|id_rsa*|id_ed25519*|secrets.*|*/secrets.*|*secret*|*credential*|*token*|*password*)
      return 0
      ;;
  esac
  if [[ -f "$full_path" ]]; then
    [[ -r "$full_path" ]] || return 0
    if grep -qE -- '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----|(^|[^A-Za-z0-9_])(private_key|privateKey|client_secret|access_token|secret_key|credential|token|password)[^A-Za-z0-9_]*[:=]' "$full_path" 2>/dev/null; then
      return 0
    fi
  fi
  return 1
}

is_allowed_ignored_path() {
  local path="$1"
  is_ignored_secret "$path" && return 1
  case "$path" in
    target/*|target-linux/*|dist/*|reports/*|run/*|.transist/*|.transist-*/*|.tmp/*|.tmp_*/*)
      return 0
      ;;
    trust-input/*.pub|trust-input/*.sig|trust-output/*.pub|trust-output/*.sig|demo/trust/*.pub|demo/trust/*.sig)
      return 0
      ;;
    scripts/ci/redocly/node_modules/*|scripts/ci/markdown/node_modules/*|tests/fixtures/*.pub|tests/fixtures/*.sig)
      return 0
      ;;
    artifact.txt|src/input.txt|.retry-ok|cleanup-success.txt)
      return 0
      ;;
    .DS_Store|*/.DS_Store|__pycache__/*|*/__pycache__/*)
      return 0
      ;;
  esac
  return 1
}

ignored=""

# These roots are produced by release, smoke, and local build tooling and are
# outside the materialized release candidate. They are intentionally excluded
# from ignored-input inspection: Git collapses a fully ignored directory to one
# entry with --directory, and walking these potentially enormous trees is both
# unnecessary and expensive. Do not add candidate-controlled release-input
# roots here. Narrow public-output roots are expanded separately because only
# their .pub/.sig files are allowlisted and inspected.
is_pruned_ignored_path() {
  case "$1" in
    target/*|target-linux/*|dist/*|reports/*|run/*|.transist/*|.transist-*/*|.tmp/*|.tmp_*/*)
      return 0
      ;;
    scripts/ci/redocly/node_modules/*|scripts/ci/markdown/node_modules/*)
      return 0
      ;;
  esac
  return 1
}

append_ignored_path() {
  local path="$1"
  is_pruned_ignored_path "$path" && return 0
  if ! is_allowed_ignored_path "$path"; then
    ignored+="${path}\n"
  fi
}

expand_narrow_ignored_directory() {
  local directory="$1"
  case "$directory" in
    trust-input/*|trust-output/*|demo/|demo/trust/*|tests/|tests/fixtures/*|__pycache__/*|*/__pycache__/*)
      while IFS= read -r -d '' path; do
        append_ignored_path "$path"
      done < <(git -C "$repo_root" ls-files --others --ignored --exclude-standard -z -- "${directory%/}/" 2>/dev/null)
      ;;
    *)
      append_ignored_path "$directory"
      ;;
  esac
}

while IFS= read -r -d '' path; do
  if [[ "$path" == */ ]]; then
    expand_narrow_ignored_directory "$path"
  else
    append_ignored_path "$path"
  fi
done < <(git -C "$repo_root" ls-files --others --ignored --exclude-standard --directory -z 2>/dev/null)

if [[ -n "$unstaged" || -n "$staged" || -n "$untracked" || -n "$ignored" ]]; then
  echo "FAIL: worktree is dirty; the release candidate must be built and packaged from a clean committed tree." >&2
  [[ -n "$unstaged" ]]  && printf '  Unstaged:\n%s\n'  "$unstaged"  >&2
  [[ -n "$staged" ]]    && printf '  Staged:\n%s\n'    "$staged"    >&2
  [[ -n "$untracked" ]] && printf '  Untracked:\n%s\n' "$untracked" >&2
  [[ -n "$ignored" ]]   && printf '  Ignored (outside generated-output allowlist):\n%b' "$ignored" >&2
  exit 1
fi

echo "PASS  worktree is clean (no staged, unstaged, untracked, or disallowed ignored files)"
