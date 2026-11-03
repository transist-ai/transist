#!/usr/bin/env sh
# SPDX-License-Identifier: MPL-2.0
#
# install_alpine_tools.sh — Install the runtime and build dependencies needed to
# build and validate the musl (Alpine) release archive inside the digest-pinned
# Alpine container.
#
# All package versions are pinned in scripts/ci/tool-versions.env. The base
# container (rust:1.88.0-alpine@<digest>) is immutable; only pinned packages are
# added at install time so the environment stays deterministic.
#
# This script runs as /bin/sh (Alpine) and fails closed if any pinned package
# cannot be installed.

set -eu

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
ROOT_DIR="$(CDPATH='' cd -- "$SCRIPT_DIR/../.." && pwd)"

# shellcheck disable=SC1091
. "$ROOT_DIR/scripts/ci/tool-versions.env"

if [ "$(uname -s)" != "Linux" ]; then
  echo "FAIL: install_alpine_tools.sh must run inside the Alpine Linux container" >&2
  exit 1
fi
if [ ! -f /etc/alpine-release ]; then
  echo "FAIL: expected an Alpine Linux environment (/etc/alpine-release missing)" >&2
  exit 1
fi

apk update

# Pin each package by exact version. bubblewrap provides the Linux sandbox
# backend on Alpine; python3 runs the E2E and shipped-workflow suites. Every
# package version lives in tool-versions.env so check_supply_chain_pins.sh
# can single-source the pins.
apk add --no-cache \
  "$ALPINE_BASH_PACKAGE" \
  "$ALPINE_GIT_PACKAGE" \
  "$ALPINE_TAR_PACKAGE" \
  "$ALPINE_MUSL_DEV_PACKAGE" \
  "$ALPINE_BUBBLEWRAP_PACKAGE" \
  "$ALPINE_FILE_PACKAGE" \
  "$ALPINE_JQ_PACKAGE" \
  "$ALPINE_PYTHON3_PACKAGE" \
  "$ALPINE_PY3_PIP_PACKAGE" \
  "$ALPINE_CURL_PACKAGE" \
  "$ALPINE_RIPGREP_PACKAGE"

# Deterministic Linux packaging relies on GNU-specific tar flags. BusyBox tar
# must never satisfy this dependency silently.
if ! tar --version 2>/dev/null | grep -q '^tar (GNU tar)'; then
  echo "FAIL: GNU tar is unavailable after pinned install" >&2
  exit 1
fi

# Verify the pinned bubblewrap binary actually exists so the sandbox backend
# fails closed rather than silently degrading.
if ! command -v bwrap >/dev/null 2>&1; then
  echo "FAIL: bwrap (bubblewrap) is unavailable after pinned install" >&2
  exit 1
fi

# Pin the Python jsonschema used for SBOM validation to the same version the
# other platforms use (see tool-versions.env), so SBOM validation bytes and
# behaviour are consistent on the musl build/validation host too.
#
# PEP 668 forbids modifying the system Python environment on Alpine. Install
# jsonschema into an isolated target directory and propagate it via GITHUB_ENV
# so later GitHub Actions steps find it through PYTHONPATH. Outside GitHub
# Actions, the exact PYTHONPATH is printed for callers to export manually;
# the parent shell is NOT modified.
pythonlib="${TRANSIST_CI_PYTHONLIB:-${RUNNER_TEMP:-/tmp}/transist-ci-pythonlib}"
rm -rf "$pythonlib"
mkdir -p "$pythonlib"
python3 -m pip install --disable-pip-version-check \
  --target "$pythonlib" "jsonschema==${PY_JSONSCHEMA_VERSION}" \
  || { echo "FAIL: could not install pinned jsonschema on Alpine" >&2; exit 1; }
PYTHONPATH="$pythonlib${PYTHONPATH:+:$PYTHONPATH}" python3 -c 'import jsonschema' \
  || { echo "FAIL: pinned jsonschema did not import from isolated target" >&2; exit 1; }
if [ -n "${GITHUB_ENV:-}" ]; then
  printf 'PYTHONPATH=%s\n' "$pythonlib${PYTHONPATH:+:$PYTHONPATH}" >> "$GITHUB_ENV"
else
  # shellcheck disable=SC2016
  echo "PYTHONPATH (export in caller): PYTHONPATH=$pythonlib"'${PYTHONPATH:+:$PYTHONPATH}'
fi

echo "Alpine tools installed (GNU tar $ALPINE_TAR_PACKAGE, bwrap $ALPINE_BUBBLEWRAP_PACKAGE, jsonschema @ $pythonlib)."
