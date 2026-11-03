#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "cargo-deny is required for license checks." >&2
  exit 1
fi

if ! command -v cargo-about >/dev/null 2>&1; then
  echo "cargo-about is required to regenerate third-party notices." >&2
  exit 1
fi

temporary_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$temporary_dir"
}
trap cleanup EXIT

cargo +1.88.0 about generate --locked about.hbs \
  | tr -d '\r' > "$temporary_dir/THIRD_PARTY_NOTICES.md"

while IFS= read -r package; do
  if rg -Fq -- "- \`$package " "$temporary_dir/THIRD_PARTY_NOTICES.md"; then
    echo "THIRD_PARTY_NOTICES.md includes first-party workspace package '$package'." >&2
    exit 1
  fi
done < <(
  cargo +1.88.0 metadata --locked --no-deps --format-version 1 \
    | python3 -c 'import json, sys; print("\n".join(package["name"] for package in json.load(sys.stdin)["packages"]))'
)

if ! tr -d '\r' < THIRD_PARTY_NOTICES.md | cmp -s - "$temporary_dir/THIRD_PARTY_NOTICES.md"; then
  echo "THIRD_PARTY_NOTICES.md is stale; regenerate it with the pinned cargo-about version." >&2
  diff -u THIRD_PARTY_NOTICES.md "$temporary_dir/THIRD_PARTY_NOTICES.md" || true
  exit 1
fi

cargo deny check

echo "License audit and third-party notice comparison passed."
