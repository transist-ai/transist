#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Run the serial local CI profiles; --help describes prerequisites/bootstrap.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export CI_TOOL_ROOT="${CI_TOOL_ROOT:-$HOME/.local/share/transist-ci-tools}"
export PATH="$CI_TOOL_ROOT/bin:$PATH"
export PYTHONPATH="$CI_TOOL_ROOT/pythonlib${PYTHONPATH:+:$PYTHONPATH}"
# Merely running a check must never trigger rustup's automatic installation.
export RUSTUP_AUTO_INSTALL=0
export RUSTUP_TOOLCHAIN=1.88.0
export GIT_NO_LAZY_FETCH=1
exec python3 "$ROOT_DIR/scripts/ci/ci_local.py" "$@"
