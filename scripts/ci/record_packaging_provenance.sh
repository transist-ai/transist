#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# Record the environment that creates final release archives. Build provenance
# is recorded on the native target runner by record_build_provenance.sh; this
# record deliberately describes only the final packaging environment.

set -euo pipefail

# Usage: record_packaging_provenance.sh [output_path] [--commit <sha>] [--tree <sha>]
# The commit/tree SHAs are passed explicitly whenever the caller has already
# materialized a specific source tree; when omitted they are read from git HEAD.

output_path="${1:-dist/packaging-provenance.json}"
shift || true
commit_sha=""
tree_sha=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --commit) commit_sha="$2"; shift 2 ;;
    --tree) tree_sha="$2"; shift 2 ;;
    *) shift ;;
  esac
done

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$(dirname "$output_path")"

# Container runners mount the workspace under a different owner; without this,
# every git call fails with "detected dubious ownership in repository".
git config --global --add safe.directory '*' >/dev/null 2>&1 || true

if [[ -z "$commit_sha" ]]; then
  source_commit_sha="$(git -C "$repo_root" rev-parse --verify 'HEAD^{commit}')"
else
  source_commit_sha="$commit_sha"
fi
if [[ -z "$tree_sha" ]]; then
  source_tree_sha="$(git -C "$repo_root" rev-parse --verify 'HEAD^{tree}')"
else
  source_tree_sha="$tree_sha"
fi

json_string() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

tool_version() {
  local tool="$1"
  if command -v "$tool" >/dev/null 2>&1; then
    local output
    output="$($tool --version 2>&1 || true)"
    printf '%s\n' "$output" | awk 'NR == 1 { print; exit }'
  else
    printf 'unavailable'
  fi
}

cat > "$output_path" <<EOF
{
  "kind": "final-packaging",
  "source": {
    "commit_sha": "$(json_string "$source_commit_sha")",
    "tree_sha": "$(json_string "$source_tree_sha")"
  },
  "packaging": {
    "runner_os": "$(json_string "${RUNNER_OS:-$(uname -s)}")",
    "runner_arch": "$(json_string "${RUNNER_ARCH:-$(uname -m)}")",
    "runner_image": "$(json_string "${CI_RUNNER_IMAGE:-unknown}")",
    "container_image": "$(json_string "${CI_CONTAINER_IMAGE:-unknown}")",
    "tar": "$(json_string "$(tool_version tar)")",
    "gzip": "$(json_string "$(tool_version gzip)")",
    "script": "package_release_archive.sh"
  }
}
EOF

echo "Packaging provenance recorded in $output_path"
