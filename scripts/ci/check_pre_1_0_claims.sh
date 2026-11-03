#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# Pre-1.0 (public beta) claims gate.
#
# Fails if any project document calls a pre-1.0 surface stable or
# production-ready. Transist is released as `0.y.z` public beta; stability
# begins only at `1.0.0`. `v1` in any schema or route identifier is a format
# version, not a SemVer 1.0 stability promise.
#
# This gate matches specific overclaim *claim phrases*, not the bare words
# "stable" or "production-ready". Policy prose that *denies* or *describes*
# the prohibition (e.g. "no surface is called ... production-ready", "not a
# SemVer 1.0 stability promise", "stability begins only at `1.0.0`") is
# allowed. See docs/public/reference/compatibility-and-stability.md.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

fail=0

# Claim phrases that constitute a pre-1.0 stability or production-ready
# overclaim. Each is matched case-insensitively as a fixed string.
overclaims=(
  'production-ready for'
  'is production-ready'
  'are production-ready'
  'stable for v1'
  'stable v1 contract'
  'stable public contract'
  'stable public packaging'
  'stable contract for'
  'stable extension surface'
  'stable boundary'
  'stable `adapter'
  'treated as stable'
  'intended to be stable'
  'are stable across'
  'schema is stable'
  'Stable Surfaces'
  'the execution layer is stable'
  'our stability promise'
)

# Files scanned: project root docs and everything under docs/, schemas/, and
# .agents/skills/, plus the in-repo skills metadata.
scan_files=(
  README.md ROADMAP.md CHANGELOG.md AGENTS.md ARCHITECTURE.md
  VISION.md SUPPORT.md SECURITY.md MAINTAINERS.md
  CODE_OF_CONDUCT.md CONTRIBUTING.md NOTICE
  schemas/README.md
)
# Tracked docs and skill markdown only (git ls-files respects .gitignore).
while IFS= read -r -d '' f; do
  scan_files+=("$f")
done < <(git ls-files -z -- 'docs/**/*.md' '.agents/skills/**/*.md')

for phrase in "${overclaims[@]}"; do
  # rg -i -F treats the phrase as a fixed string. The backtick in
  # 'stable `adapter' is literal.
  matches="$(rg -i -F -n --no-heading "$phrase" "${scan_files[@]}" 2>/dev/null || true)"
  if [[ -n "$matches" ]]; then
    echo "FAIL: pre-1.0 overclaim phrase '$phrase' found:" >&2
    printf '%s\n' "$matches" >&2
    fail=1
  fi
done

if [[ "$fail" -ne 0 ]]; then
  echo "Pre-1.0 claims check failed. No project document may call a pre-1.0 surface stable or production-ready." >&2
  exit 1
fi

echo "Pre-1.0 claims check passed."
