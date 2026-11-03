#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# admin_sign_verify.sh — Isolated admin-host conformance: generate an
# ephemeral key, sign the shell plugin and quickstart workflow, verify
# the detached signatures, export the public root, then destroy the
# private key. The staging directory contains ONLY public material.
#
# Usage: admin_sign_verify.sh <transist-admin-bin> <output-dir>
#
# Output directory contents (all public):
#   trust/quickstart-demo.pub        — public trust root
#   plugins/shell.sig                — detached plugin signature
#   examples/quickstart/quickstart.workflow.yaml.sig — detached workflow signature
#
# The private key is generated in a temp directory and deleted before exit.

if [[ "$#" -ne 2 ]]; then
  echo "Usage: $0 <transist-admin-bin> <output-dir>" >&2
  exit 1
fi

admin_bin="$1"
output_dir="$2"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

if [[ ! -x "$admin_bin" ]]; then
  echo "FAIL: transist-admin not found or not executable: $admin_bin" >&2
  exit 1
fi

# Work in a temp directory; the private key never touches the output dir.
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

key_path="$work_dir/quickstart-demo.key"
pub_path="$work_dir/quickstart-demo.pub"

echo "=== Admin sign/verify conformance ==="
echo "  admin binary: $admin_bin"
echo "  output dir:   $output_dir"
echo ""

# 1. Generate the ephemeral private key.
echo "--- Generate ephemeral key ---"
"$admin_bin" key generate --name quickstart-demo --private-key-file "$key_path"
echo "  PASS  key generated"

# 2. Export the public root.
echo "--- Export public root ---"
"$admin_bin" key export-public --private-key-file "$key_path" --trust-root-file "$pub_path"
echo "  PASS  public root exported"

# 3. Sign the shell plugin.
echo "--- Sign shell plugin ---"
"$admin_bin" plugin sign "$repo_root/plugins/shell" \
   --private-key-file "$key_path" --output "$work_dir/shell.sig"
echo "  PASS  plugin signed"

# 4. Sign the quickstart workflow.
echo "--- Sign quickstart workflow ---"
"$admin_bin" workflow sign "$repo_root/examples/quickstart/quickstart.workflow.yaml" \
   --private-key-file "$key_path" --output "$work_dir/quickstart.workflow.yaml.sig"
echo "  PASS  workflow signed"

# 5. Verify both signatures against the public root.
echo "--- Verify signatures ---"
"$admin_bin" plugin verify "$repo_root/plugins/shell" \
   --signature "$work_dir/shell.sig" --trust-root-file "$pub_path"
echo "  PASS  plugin signature verified"

"$admin_bin" workflow verify "$repo_root/examples/quickstart/quickstart.workflow.yaml" \
   --signature "$work_dir/quickstart.workflow.yaml.sig" --trust-root-file "$pub_path"
echo "  PASS  workflow signature verified"

# 6. Inspect the signature envelope.
echo "--- Inspect signature envelope ---"
"$admin_bin" signature inspect "$work_dir/quickstart.workflow.yaml.sig" >/dev/null
echo "  PASS  signature envelope inspected"

# 7. Delete the private key (it is never copied to the output dir).
echo "--- Destroy private key ---"
rm -f "$key_path"
if [[ ! -f "$key_path" ]]; then
  echo "  PASS  private key destroyed"
else
  echo "  FAIL  private key still exists" >&2
  exit 1
fi

# 8. Verify no private key material remains in the work directory.
if find "$work_dir" -type f -name '*.key' | grep -q .; then
  echo "  FAIL  private key material found in work directory" >&2
  exit 1
fi
if grep -rlE '-----BEGIN (AES|EC|OPENSSH|PGP|RSA|DSA|ECDSA|ENCRYPTED) PRIVATE KEY-----' "$work_dir" 2>/dev/null | grep -q .; then
  echo "  FAIL  PEM private key content found in work directory" >&2
  exit 1
fi
echo "  PASS  no private key material in work directory"

# 9. Copy only public material to the output directory.
echo "--- Stage public-only output ---"
mkdir -p "$output_dir/trust" "$output_dir/plugins" "$output_dir/examples/quickstart"
cp "$pub_path" "$output_dir/trust/quickstart-demo.pub"
cp "$work_dir/shell.sig" "$output_dir/plugins/shell.sig"
cp "$work_dir/quickstart.workflow.yaml.sig" \
   "$output_dir/examples/quickstart/quickstart.workflow.yaml.sig"
echo "  PASS  public-only output staged"

# 10. Verify the output directory contains no private key material.
if find "$output_dir" -type f -name '*.key' | grep -q .; then
  echo "  FAIL  private key material found in output directory" >&2
  exit 1
fi
if grep -rlE '-----BEGIN (AES|EC|OPENSSH|PGP|RSA|DSA|ECDSA|ENCRYPTED) PRIVATE KEY-----' "$output_dir" 2>/dev/null | grep -q .; then
  echo "  FAIL  PEM private key content found in output directory" >&2
  exit 1
fi
echo "  PASS  output directory is private-key-free"

echo ""
echo "=== Admin sign/verify conformance passed ==="
