#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# prepare_e2e_trust_fixtures.sh — Isolated admin-host signing for E2E suites.
#
# Generates an ephemeral key, signs the plugins and workflows used by the
# required E2E suites, exports the public trust root, then destroys the
# private key. The output directory contains ONLY public material:
# signatures and the public root. The E2E suites import the public root
# and use the pre-signed artifacts without ever touching a private key.
#
# Usage: prepare_e2e_trust_fixtures.sh <transist-admin-bin> <output-dir> <repo-root>
#
# Workflow signatures are written as $output_dir/workflows/<key>.sig where
# <key> is the stable lookup key the suites use in sign_workflow():
#   - for a workflow whose source file is named workflow.yaml, <key> is the
#     parent directory basename (e.g. local_only_companion, 08_manual_gate);
#   - otherwise <key> is the source file stem with .workflow.yaml stripped
#     (e.g. positive, untrusted-plugin, tampered, external_demo).
# Plugins are written as $output_dir/plugins/<basename>.sig
# (shell.sig, markdown.local.sig, remote.echo.sig).
#
# Output directory contents (all public):
#   trust/gov-org.pub                — public trust root
#   plugins/shell.sig                  — detached shell plugin signature
#   plugins/markdown.local.sig         — detached markdown.local signature
#   plugins/remote.echo.sig            — detached remote.echo signature
#   workflows/<key>.sig                — detached workflow signatures

if [[ "$#" -ne 3 ]]; then
  echo "Usage: $0 <transist-admin-bin> <output-dir> <repo-root>" >&2
  exit 1
fi

admin_bin="$1"
output_dir="$2"
repo_root="$(cd "$3" && pwd)"

if [[ ! -x "$admin_bin" ]]; then
  echo "FAIL: transist-admin not found or not executable: $admin_bin" >&2
  exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

key_path="$work_dir/gov-org.key"
pub_path="$work_dir/gov-org.pub"

echo "=== Preparing E2E trust fixtures ==="
echo "  admin binary: $admin_bin"
echo "  output dir:   $output_dir"
echo ""

# 1. Generate ephemeral key.
echo "--- Generate ephemeral key ---"
"$admin_bin" key generate --name gov-org --private-key-file "$key_path"
echo "  PASS  key generated"

# 2. Export public root.
echo "--- Export public trust root ---"
"$admin_bin" key export-public --private-key-file "$key_path" --trust-root-file "$pub_path"
mkdir -p "$output_dir/trust"
cp "$pub_path" "$output_dir/trust/gov-org.pub"
cp "$pub_path" "$output_dir/trust/quickstart-demo.pub"
echo "  PASS  public root exported"

# 3. Sign plugins.
echo "--- Sign plugins ---"
mkdir -p "$output_dir/plugins"

sign_plugin() {
  local src="$1" out_name="$2"
  "$admin_bin" plugin sign "$src" --private-key-file "$key_path" --output "$output_dir/plugins/$out_name"
}

sign_plugin "$repo_root/plugins/shell" "shell.sig"
echo "  PASS  shell plugin signed"

if [[ -d "$repo_root/.agents/testing/fixtures/plugins/markdown.local" ]]; then
  sign_plugin "$repo_root/.agents/testing/fixtures/plugins/markdown.local" "markdown.local.sig"
  echo "  PASS  markdown.local plugin signed"
fi

# The quickstart smoke uses a stable archive-style sidecar location. Keep this
# signature in the isolated public fixture tree rather than relying on the
# repository's committed development signature.
mkdir -p "$output_dir/examples/quickstart"
"$admin_bin" workflow sign \
  "$repo_root/examples/quickstart/quickstart.workflow.yaml" \
   --private-key-file "$key_path" \
  --output "$output_dir/examples/quickstart/quickstart.workflow.yaml.sig"
echo "  PASS  quickstart workflow signed"

if [[ -d "$repo_root/.agents/testing/fixtures/plugins/remote.echo" ]]; then
  sign_plugin "$repo_root/.agents/testing/fixtures/plugins/remote.echo" "remote.echo.sig"
  echo "  PASS  remote.echo plugin signed"
fi

# 4. Sign workflow fixtures.
#
# The workflow-definition digest covers each authored file's content AND its
# relative path within the workflow root (the parent directory of the signed
# workflow file). To produce signatures the suites can replay, the prepare
# step must sign each workflow with the same relative-path shape the suite
# uses at install time:
#   - workflows stored as <dir>/workflow.yaml (local_only_companion,
#     08_manual_gate) have relative path "workflow.yaml"; the prepare copy
#     is laid out as $work_dir/<key>/workflow.yaml;
#   - flat workflows stored as <name>.workflow.yaml (enforcement fixtures,
#     external_demo) have relative path "<name>.workflow.yaml"; the prepare
#     copy is laid out as $work_dir/<name>.workflow.yaml.
#
# sign_workflow <key> <source-workflow-yaml>: stages the source into the
# matching shape, signs it, and writes the signature to
# $output_dir/workflows/<key>.sig. The suite's sign_workflow() looks up
# exactly that path by the same <key>.
echo "--- Sign workflow fixtures ---"
mkdir -p "$output_dir/workflows"

sign_workflow() {
  local key="$1" src="$2" work_copy
  if [[ "$(basename "$src")" == "workflow.yaml" ]]; then
    mkdir -p "$work_dir/$key"
    work_copy="$work_dir/$key/workflow.yaml"
  else
    work_copy="$work_dir/$(basename "$src")"
  fi
  cp "$src" "$work_copy"
  "$admin_bin" workflow sign "$work_copy" --private-key-file "$key_path" \
    --output "$output_dir/workflows/${key}.sig"
  echo "  PASS  $key workflow signed"
}

# Workflows whose source file is named workflow.yaml (suite key = parent dir).
sign_workflow local_only_companion \
  "$repo_root/.agents/testing/fixtures/workflows/local_only_companion/workflow.yaml"
sign_workflow 08_manual_gate \
  "$repo_root/workflows/demos/getting_started/08_manual_gate/workflow.yaml"

# Enforcement fixture workflows (suite key = file stem).
for key in positive untrusted-plugin tampered; do
  src="$repo_root/.agents/testing/fixtures/workflows/enforcement/${key}.workflow.yaml"
  if [[ -f "$src" ]]; then
    sign_workflow "$key" "$src"
  fi
done

# external_demo is a flat, relative workflow fixture. Keep it in the current
# authored workflow format even while no suite currently consumes it.
external_demo_src="$work_dir/external_demo.workflow.yaml"
cat > "$external_demo_src" <<'YAML'
schema: transist.flow.v1
id: external_demo
version: v1
plugins:
  remote.echo: 0.1.0
steps:
  - id: run_external
    uses: process:remote.echo/echo
    with:
      message: hello
YAML
"$admin_bin" workflow sign "$external_demo_src" --private-key-file "$key_path" \
  --output "$output_dir/workflows/external_demo.sig"
echo "  PASS  external_demo workflow signed"

# 5. Verify signatures are structurally valid (public-only inspection).
echo "--- Verify signatures (public-only) ---"
for sig in "$output_dir"/plugins/*.sig "$output_dir"/workflows/*.sig; do
  [[ -f "$sig" ]] || continue
  "$admin_bin" signature inspect "$sig" > /dev/null
done
echo "  PASS  all signatures verifiable with public root"

# 6. Destroy the private key.
rm -f "$key_path"
echo ""
echo "=== E2E trust fixtures prepared (private key destroyed) ==="
echo "  Public material in: $output_dir"
