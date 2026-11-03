#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# Verifies that a minisign secret key can produce a signature trusted by the
# committed release verification key. The probe is temporary and never leaves
# the runner, so this checks the configured signing secret without exposing it.

set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "Usage: $0 <secret-key-path> <committed-public-key-path>" >&2
  exit 1
fi

secret_key="$1"
public_key="$2"

[[ -f "$secret_key" ]] || { echo "FAIL: signing secret not found" >&2; exit 1; }
[[ -f "$public_key" ]] || { echo "FAIL: committed public key not found" >&2; exit 1; }

key_id="$(sed -n '1s/^untrusted comment: minisign public key //p' "$public_key")"
[[ "$key_id" =~ ^[0-9A-F]{16}$ ]] || { echo "FAIL: invalid committed minisign key ID" >&2; exit 1; }

probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
printf 'transist release signing-key probe\n' > "$probe_dir/probe"
minisign -S -q -s "$secret_key" -m "$probe_dir/probe" -x "$probe_dir/probe.minisig"
minisign -Vq -p "$public_key" -m "$probe_dir/probe" -x "$probe_dir/probe.minisig"

echo "PASS: configured signing secret matches committed key ID $key_id"
