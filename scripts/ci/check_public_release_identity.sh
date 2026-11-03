#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
exec python3 "$ROOT_DIR/scripts/ci/check_public_release_identity.py" "$@"
