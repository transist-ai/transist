#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# Workspace publish policy gate.
#
# Asserts that every workspace member is non-publishable (publish = false,
# inherited from [workspace.package]) and carries repository and homepage
# metadata. Workspace crates are implementation units; the only supported
# distribution is the signed binary archive. See
# docs/public/reference/compatibility-and-stability.md.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

if ! command -v cargo >/dev/null; then
  echo "FAIL: cargo is required to check the workspace publish policy" >&2
  exit 1
fi

expected_repo="https://github.com/transist-ai/transist"
expected_home="https://transist.ai"

# cargo metadata lists every workspace member. publish is an array; an empty
# array means publish = false. repository and homepage are strings.
cargo metadata --locked --no-deps --format-version 1 2>/dev/null | \
  python3 -c '
import json
import sys

expected_repo = sys.argv[1]
expected_home = sys.argv[2]

data = json.load(sys.stdin)
fail = 0
members = sorted(data.get("packages", []), key=lambda p: p["name"])
if not members:
    print("FAIL: cargo metadata returned no packages", file=sys.stderr)
    sys.exit(1)

for pkg in members:
    name = pkg["name"]
    publish = pkg.get("publish") or []
    if publish:
        print(f"FAIL: {name} is publishable: {publish}", file=sys.stderr)
        fail = 1
    repo = pkg.get("repository")
    home = pkg.get("homepage")
    if repo != expected_repo:
        print(f"FAIL: {name} repository mismatch: {repo!r} != {expected_repo!r}", file=sys.stderr)
        fail = 1
    if home != expected_home:
        print(f"FAIL: {name} homepage mismatch: {home!r} != {expected_home!r}", file=sys.stderr)
        fail = 1

if fail:
    sys.exit(1)
print(f"Workspace publish policy passed: {len(members)} packages non-publishable.")
' "$expected_repo" "$expected_home"

if [[ "${PIPESTATUS[1]}" -ne 0 ]]; then
  echo "Workspace publish policy check failed." >&2
  exit 1
fi
