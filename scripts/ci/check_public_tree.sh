#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_public_tree.sh - release-blocking scan of the tracked candidate tree.
#
# This deliberately uses `git ls-files`, rather than the filesystem, so ignored
# build output cannot hide a tracked release problem. File contents are only
# tested for a match; secret values are never printed.

set -euo pipefail

ROOT_DIR="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
ROOT_DIR="$(cd "$ROOT_DIR" && pwd)"
TREEISH="${2:-}"

if ! command -v rg >/dev/null 2>&1; then
  echo "FAIL: ripgrep (rg) is required for public-tree content scanning but is not installed" >&2
  exit 1
fi

if ! git -C "$ROOT_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "FAIL: public-tree scan root is not a Git worktree: $ROOT_DIR" >&2
  exit 1
fi

failures=0

record_failure() {
  local category="$1"
  local path="$2"
  local detail="$3"
  printf 'FAIL [%s] %s: %s\n' "$category" "$path" "$detail" >&2
  failures=$((failures + 1))
}

is_checker() {
  [[ "$1" == "scripts/ci/check_public_tree.sh" || "$1" == "scripts/ci/test_public_tree.sh" || "$1" == ".agents/testing/test_run_e2e.py" ]]
}

matches_content() {
  local path="$1"
  local pattern="$2"
  local content_file="$3"
  [[ -f "$content_file" ]] && rg -q -I -P -- "$pattern" "$content_file"
}

tracked_count=0
scan_path() {
  local path="$1"
  local index_mode="$2"
  local content_file="$3"
  tracked_count=$((tracked_count + 1))
  if is_checker "$path"; then
    return
  fi

  # Symlinks can point outside the candidate tree and are not acceptable in a
  # release archive whose contents must be independently inspectable.
  if [[ "$index_mode" == "120000" ]]; then
    record_failure "forbidden file" "$path" "tracked symlink"
  fi

  case "$path" in
    target/*|dist/*|reports/*|run/*|.transist/*|.transist-*/*|.tmp/*|.tmp_*/*)
      record_failure "forbidden file" "$path" "private development or generated release output"
      ;;
    .gitmodules|*.gitmodules|*.key|*.pem|*.p12|*.pfx|*.jks|*.keystore|id_rsa*|id_ed25519*)
      record_failure "private key or credential file" "$path" "private material must not be tracked"
      ;;
    *.log|*.trace|*release-evidence*|*candidate-record*)
      record_failure "internal release evidence" "$path" "release evidence belongs outside the candidate tree"
      ;;
  esac
  if matches_content "$path" '(^|[^A-Za-z0-9_])/Users/[A-Za-z0-9._-]+(/|[^A-Za-z0-9._-]|$)|(^|[^A-Za-z0-9_])/home/(?!user(?:/|$)|testuser(?:/|$)|transist(?:/|$))[A-Za-z0-9._-]+/|(^|[^A-Za-z0-9_])/private/var/folders/[A-Za-z0-9._-]+/|(^|[^A-Za-z0-9_])/private/var/tmp/[A-Za-z0-9._-]+/' "$content_file"; then
    record_failure "local absolute path" "$path" "contains a machine-local developer path"
  fi
  if matches_content "$path" '-----BEGIN (?:OPENSSH |RSA |EC )?PRIVATE KEY-----|(private_key|privateKey|client_secret|access_token|secret_key|credential)[^A-Za-z0-9]*[:=][^A-Za-z0-9]*[A-Za-z0-9+/]{40,}={0,2}|(^|[^A-Za-z0-9_])d[^A-Za-z0-9_]*:[^A-Za-z0-9]*[A-Za-z0-9_-]{32,}' "$content_file"; then
    record_failure "private key or credential content" "$path" "contains private key or credential material"
  fi
}

if [[ -n "$TREEISH" ]]; then
  git -C "$ROOT_DIR" rev-parse --verify --end-of-options "$TREEISH^{commit}" >/dev/null
  while IFS=$'\t' read -r -d '' metadata path; do
    mode="${metadata%% *}"
    blob_file="$(mktemp)"
    trap 'rm -f "$blob_file"' RETURN
    git -C "$ROOT_DIR" cat-file blob "$TREEISH:$path" >"$blob_file"
    scan_path "$path" "$mode" "$blob_file"
    rm -f "$blob_file"
    trap - RETURN
  done < <(git -C "$ROOT_DIR" ls-tree -r -z "$TREEISH")
else
  while IFS= read -r -d '' path; do
    if [[ ! -e "$ROOT_DIR/$path" && ! -L "$ROOT_DIR/$path" ]]; then
      continue
    fi
    index_entry="$(git -C "$ROOT_DIR" ls-files -s -- "$path")"
    scan_path "$path" "${index_entry%% *}" "$ROOT_DIR/$path"
  done < <(git -C "$ROOT_DIR" ls-files -z)
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'Public-tree check failed with %d violation(s).\n' "$failures" >&2
  exit 1
fi

echo "Public-tree check passed: $tracked_count tracked files scanned."
