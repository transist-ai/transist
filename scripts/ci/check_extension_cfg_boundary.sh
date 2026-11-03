#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

scan_paths=(
  crates/transist-protocol
  crates/transist-prerequisites
  crates/transist-plugins
  crates/transist-trust-digest
  crates/transist-core
  crates/transist-compiler
  crates/transist-policy
  crates/transist-storage-local
  crates/transist-adapters
  crates/transist-inspection
  crates/transist-metrics
  src/application/inspection_snapshot.rs
)

pattern='#\s*\[cfg[^]]*feature\s*=\s*"(managed-mode|central-inspection-sync)"'

if rg -n --hidden -S "$pattern" "${scan_paths[@]}"; then
  echo "Extension feature flag found in a core runtime crate — remove the #[cfg(feature=...)] gating." >&2
  exit 1
fi

if rg -n --hidden -g '!target/**' -S '#\s*\[cfg[^]]*feature\s*=\s*"(concurrent-runs|detached-runner)"' crates/transist-cli/src crates/transist-cli/tests; then
  echo "Detached-runner feature flags are obsolete; detached mutation execution is unconditional." >&2
  exit 1
fi

echo "Extension feature flag boundary check passed."
