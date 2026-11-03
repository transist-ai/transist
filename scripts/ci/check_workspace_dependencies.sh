#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

workspace_packages=(
  transist
  transist-prerequisites
  transist-protocol
  transist-plugins
  transist-trust-digest
  transist-core
  transist-compiler
  transist-policy
  transist-storage-local
  transist-adapters
  transist-inspection
  transist-metrics
  transist-cli
  transist-admin
  transist-server
  transist-sandbox-launcher
)

get_workspace_direct_deps() {
  local package="$1"
  cargo tree --locked -p "$package" --depth 1 --edges normal,build --prefix none 2>/dev/null \
    | tail -n +2 \
    | sed -E 's/^[^A-Za-z0-9_-]*//' \
    | sed -E 's/ v[0-9].*$//' \
    | while read -r dep; do
        for ws in "${workspace_packages[@]}"; do
          if [[ "$dep" == "$ws" ]]; then
            echo "$dep"
          fi
        done
      done \
    | sort -u
}

assert_exact_deps() {
  local package="$1"
  shift
  local expected=("$@")
  local actual
  actual="$(get_workspace_direct_deps "$package" | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g' | sed -E 's/^ //; s/ $//')"
  local want
  if [[ "${#expected[@]}" -eq 0 ]]; then
    want=""
  else
    want="$(printf '%s\n' "${expected[@]}" | sed '/^$/d' | sort -u | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g' | sed -E 's/^ //; s/ $//')"
  fi

  if [[ "$actual" != "$want" ]]; then
    echo "Workspace dependency violation for $package" >&2
    echo "  expected: ${want:-<none>}" >&2
    echo "  actual:   ${actual:-<none>}" >&2
    exit 1
  fi
}

check_transist_core_banned_deps() {
  local direct
  direct="$(cargo tree --locked -p transist-core --depth 1 --edges normal,build --prefix none 2>/dev/null)"
  local banned='(axum|clap|rusqlite|uuid|transist($| )|transist-adapters|transist-storage-local|transist-policy|transist-compiler|transist-server)'
  if echo "$direct" | rg -n "$banned" >/dev/null; then
    echo "transist-core has forbidden dependencies." >&2
    echo "$direct" >&2
    exit 1
  fi
}

assert_exact_deps transist \
  transist-adapters \
  transist-compiler \
  transist-core \
  transist-policy \
  transist-inspection \
  transist-metrics \
  transist-plugins \
  transist-prerequisites \
  transist-protocol \
  transist-storage-local
assert_exact_deps transist-prerequisites transist-protocol
assert_exact_deps transist-protocol
assert_exact_deps transist-plugins transist-prerequisites transist-protocol transist-trust-digest
assert_exact_deps transist-trust-digest
assert_exact_deps transist-core transist-prerequisites transist-protocol
assert_exact_deps transist-compiler transist-plugins transist-prerequisites transist-protocol transist-trust-digest
assert_exact_deps transist-policy transist-core transist-protocol
assert_exact_deps transist-storage-local transist-core transist-protocol
assert_exact_deps transist-adapters transist-core transist-plugins transist-prerequisites transist-protocol transist-trust-digest
assert_exact_deps transist-inspection transist-compiler transist-core transist-metrics transist-protocol transist-storage-local
assert_exact_deps transist-metrics transist-core transist-protocol transist-storage-local
assert_exact_deps transist-cli transist
assert_exact_deps transist-admin transist-compiler transist-core transist-plugins transist-protocol
assert_exact_deps transist-server transist
assert_exact_deps transist-sandbox-launcher
check_transist_core_banned_deps

echo "Workspace dependency check passed."
