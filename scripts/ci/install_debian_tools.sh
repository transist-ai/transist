#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# install_debian_tools.sh — Install Debian (bookworm) system/CI packages with
# exact pinned versions inside the digest-pinned CI_CONTAINER_IMAGE container.
#
# All package versions are declared in scripts/ci/tool-versions.env as
# APT_<NAME>_PACKAGE=<package>=<exact version>. This script maps each requested
# tool name to its pinned version, so workflows never call an unpinned
# `apt-get install` directly and the installed tool set stays deterministic.
#
# Usage: install_debian_tools.sh <tool> [<tool> ...]
#
# Supported tools: curl unzip nodejs npm bubblewrap ripgrep git jq python3 pip3 musl-tools
#
# This script fails closed if a requested tool has no pin in tool-versions.env.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck disable=SC1091
source "$ROOT_DIR/scripts/ci/tool-versions.env"

if [[ "$#" -eq 0 ]]; then
  echo "Usage: $0 <tool> [<tool> ...]" >&2
  exit 2
fi

map_tool_to_var() {
  local name="$1"
  case "$name" in
    curl) echo "APT_CURL_PACKAGE" ;;
    unzip) echo "APT_UNZIP_PACKAGE" ;;
    nodejs) echo "APT_NODEJS_PACKAGE" ;;
    npm) echo "APT_NPM_PACKAGE" ;;
    bubblewrap) echo "APT_BUBBLEWRAP_PACKAGE" ;;
    ripgrep) echo "APT_RIPGREP_PACKAGE" ;;
    git) echo "APT_GIT_PACKAGE" ;;
    jq) echo "APT_JQ_PACKAGE" ;;
    python3) echo "APT_PYTHON3_PACKAGE" ;;
    pip3) echo "APT_PYTHON3_PIP_PACKAGE" ;;
    musl-tools) echo "APT_MUSL_TOOLS_PACKAGE" ;;
    *)
      echo "FAIL: no pinned apt package for tool '$name'" >&2
      exit 1
      ;;
  esac
}

packages=()
for tool in "$@"; do
  var="$(map_tool_to_var "$tool")"
  if [[ -z "${!var:-}" ]]; then
    echo "FAIL: $var is not declared in tool-versions.env" >&2
    exit 1
  fi
  packages+=("${!var}")
done

apt-get update
apt-get install -y "${packages[@]}"

# Verify the requested tools actually resolve to a binary so a renamed/missing
# package fails closed instead of silently degrading. Some packages install a
# binary whose name differs from the Debian source package name (e.g. the
# bubblewrap package installs the `bwrap` binary); map each tool to its real
# command name here.
binary_for() {
  case "$1" in
    bubblewrap) echo "bwrap" ;;
    ripgrep) echo "rg" ;;
    musl-tools) echo "musl-gcc" ;;
    nodejs) echo "node" ;;
    pip3) echo "pip3" ;;
    *) echo "$1" ;;
  esac
}

for tool in "$@"; do
  if ! command -v "$(binary_for "$tool")" >/dev/null 2>&1; then
    echo "FAIL: '$tool' is unavailable after pinned apt install" >&2
    exit 1
  fi
done

echo "Debian tools installed: $*"
