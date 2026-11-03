#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_secrets.sh — Repository history and tree secret scan.
#
# Scans the entire commit history and the final working tree with separate
# Gitleaks configurations. This is the release-blocking scan
# referenced by check_release_readiness.sh.
#
# Ignored build and local runtime paths are excluded only from the final-tree
# scan. They are not allowlisted in the history configuration, so a committed
# secret in one of those paths remains release-blocking.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

history_config="$ROOT_DIR/.gitleaks.toml"
tree_config="$ROOT_DIR/.gitleaks-working-tree.toml"

[[ -f "$history_config" ]] || { echo "missing history Gitleaks config" >&2; exit 1; }
[[ -f "$tree_config" ]] || { echo "missing working-tree Gitleaks config" >&2; exit 1; }

if ! command -v gitleaks >/dev/null 2>&1; then
  echo "gitleaks is required for secret scanning." >&2
  exit 1
fi

# Full git-history scan: gitleaks traverses every commit by default when
# --no-git is absent. --redact prevents leaking secret values in the scan
# output itself. Do not use the working-tree config here.
history_status=0
gitleaks detect \
  --source . \
  --config "$history_config" \
  --redact \
  --exit-code 1 || history_status=$?

tree_status=0
gitleaks dir . \
  --config "$tree_config" \
  --redact \
  --exit-code 1 || tree_status=$?

if [[ "$history_status" -ne 0 || "$tree_status" -ne 0 ]]; then
  echo "Secret scan failed (history=$history_status, final_tree=$tree_status)." >&2
  exit 1
fi

echo "Secret scan (full git history and final tree) passed."
