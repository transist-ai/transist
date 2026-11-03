#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# record_build_provenance.sh — Record the native build provenance for one
# supported release target.
#
# Usage:
#   record_build_provenance.sh [output_path] [target_triple] [binary...] \
#       [--commit <sha>] [--tree <sha>]
#
#   output_path   JSON output path (default: dist/build-provenance.json)
#   target_triple Release target triple (for example x86_64-unknown-linux-musl).
#                 When omitted it is inferred from the rustc host, which is only
#                 valid for native (non-cross) builds.
#   binary...     Paths of the raw binaries produced by this build. Each binary
#                 is recorded with its SHA-256, so a downstream consumer can
#                 reject provenance that describes a different binary.
#   --commit / --tree
#                 Explicit source commit and tree SHAs. When omitted they are
#                 read from the local git HEAD.
#
# The commit/tree SHAs anchor this record to a specific materialized source
# tree; they must match the tree the binaries were built from.

output_path="dist/build-provenance.json"
target_triple=""
binaries=()
commit_sha=""
tree_sha=""

# Simpler parse loop (avoids shift-arithmetic pitfalls).
output_path="${1:-dist/build-provenance.json}"
shift || true
if [[ "$#" -gt 0 && "$1" != "--"* ]]; then
  target_triple="$1"
  shift || true
fi
needs_output=1
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --commit)
      commit_sha="$2"
      shift 2
      ;;
    --tree)
      tree_sha="$2"
      shift 2
      ;;
    --output)
      output_path="$2"
      shift 2
      ;;
    --)
      shift
      ;;
    *)
      binaries+=("$1")
      shift
      ;;
  esac
done

mkdir -p "$(dirname "$output_path")"

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

# Container runners mount the workspace under a different owner; without this,
# every git call fails with "detected dubious ownership in repository".
git config --global --add safe.directory '*' >/dev/null 2>&1 || true

if [[ -z "$commit_sha" ]]; then
  commit_sha="$(git -C "$repo_root" rev-parse --verify 'HEAD^{commit}')"
fi
if [[ -z "$tree_sha" ]]; then
  tree_sha="$(git -C "$repo_root" rev-parse --verify 'HEAD^{tree}')"
fi

json_string() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

rustc_host="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -z "$target_triple" ]]; then
  target_triple="$rustc_host"
fi

# Per-binary SHA-256 records. Each binary must exist and be a regular file.
build_binaries_json() {
  local entries=()
  local binary binary_name binary_sha
  for binary in "${binaries[@]:-}"; do
    [[ -n "$binary" ]] || continue
    if [[ ! -f "$binary" || -L "$binary" ]]; then
      echo "FAIL: build binary not found or is a symlink: $binary" >&2
      exit 1
    fi
    binary_name="$(basename "$binary")"
    if command -v sha256sum >/dev/null 2>&1; then
      binary_sha="$(sha256sum "$binary" | cut -d' ' -f1)"
    else
      binary_sha="$(shasum -a 256 "$binary" | cut -d' ' -f1)"
    fi
    entries+=( "$(printf '    { "name": "%s", "sha256": "%s" }' "$(json_string "$binary_name")" "$binary_sha")" )
  done
  if [[ "${#entries[@]}" -eq 0 ]]; then
    printf '  []'
    return
  fi
  printf '  [\n'
  local idx=0
  for entry in "${entries[@]}"; do
    printf '%s' "$entry"
    if [[ $((idx + 1)) -lt "${#entries[@]}" ]]; then
      printf ','
    fi
    printf '\n'
    idx=$((idx + 1))
  done
  printf '  ]'
}
binary_sha_json="$(build_binaries_json)"

rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
runner_os="${RUNNER_OS:-$(uname -s)}"
runner_arch="${RUNNER_ARCH:-$(uname -m)}"
runner_image_label="${CI_RUNNER_IMAGE:-unknown}"
runner_image_version="${ImageVersion:-unknown}"
runner_image_os="${ImageOS:-unknown}"
xcode_version="unknown"
xcode_build="unknown"
if command -v xcodebuild >/dev/null 2>&1 && xcode_output="$(xcodebuild -version 2>/dev/null)"; then
  xcode_version="$(printf '%s\n' "$xcode_output" | head -1 | awk '{print $2}')"
  xcode_build="$(printf '%s\n' "$xcode_output" | awk '$1 == "Build version" { print $3 }')"
fi

tool_version() {
  local tool="$1"
  if command -v "$tool" >/dev/null 2>&1; then
    local output
    output="$("$tool" --version 2>&1 || true)"
    printf '%s\n' "$output" | awk 'NR == 1 { print; exit }'
  else
    printf 'unavailable'
  fi
}

tar_version="$(tool_version tar)"
gzip_version="$(tool_version gzip)"
bubblewrap_version="$(tool_version bwrap)"
ripgrep_version="$(tool_version rg)"
gh_version="$(tool_version gh)"
minisign_version="$(tool_version minisign)"

cat > "$output_path" <<EOF
{
  "kind": "native-build",
  "target": "$(json_string "$target_triple")",
  "source": {
    "commit_sha": "$(json_string "$commit_sha")",
    "tree_sha": "$(json_string "$tree_sha")"
  },
  "rustc": "$(json_string "$rustc_version")",
  "cargo": "$(json_string "$cargo_version")",
  "runner": {
    "os": "$(json_string "$runner_os")",
    "arch": "$(json_string "$runner_arch")",
    "image": "$(json_string "$runner_image_label")",
    "image_version": "$(json_string "$runner_image_version")",
    "image_os": "$(json_string "$runner_image_os")"
  },
  "os_tools": {
    "tar": "$(json_string "$tar_version")",
    "gzip": "$(json_string "$gzip_version")",
    "bubblewrap": "$(json_string "$bubblewrap_version")",
    "ripgrep": "$(json_string "$ripgrep_version")",
    "gh": "$(json_string "$gh_version")",
    "minisign": "$(json_string "$minisign_version")"
  },
  "xcode": {
    "version": "$(json_string "$xcode_version")",
    "build": "$(json_string "$xcode_build")"
  },
  "binaries": $binary_sha_json
}
EOF

echo "Build provenance recorded in $output_path"
