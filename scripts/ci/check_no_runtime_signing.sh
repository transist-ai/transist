#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# Scope intentionally excludes tests/ because fixture signing helpers are
# allowed there; the boundary is that runtime implementation code never signs.
targets=(src crates)
patterns=(
  'ed25519_dalek::SigningKey'
  'SigningKey::generate'
  'EncodePrivateKey'
  'DecodePrivateKey'
  'pkcs8::'
  '\.try_sign\('
  'sign_prehashed'
  '\bkeygen\b'
)

fail=0

for pattern in "${patterns[@]}"; do
  if rg -n --hidden -g '!target/**' -e "$pattern" "${targets[@]}" >/dev/null; then
    rg -n --hidden -g '!target/**' -e "$pattern" "${targets[@]}" || true
    echo "Boundary violation: runtime signing or keygen symbol detected: $pattern" >&2
    fail=1
  fi
done

if rg -n --glob 'Cargo.toml' -g '!transist-admin/Cargo.toml' 'ed25519-dalek.*rand_core' . >/dev/null; then
  rg -n --glob 'Cargo.toml' -g '!transist-admin/Cargo.toml' 'ed25519-dalek.*rand_core' . || true
  echo "Boundary violation: runtime manifests must not enable ed25519 signing/keygen features such as rand_core." >&2
  fail=1
fi

# Verification-only crypto dependencies are allowed in later stages. This guard
# intentionally blocks signing and private-key APIs at the source level instead
# of banning whole crates such as `ed25519-dalek`.

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "Runtime signing boundary check passed."
