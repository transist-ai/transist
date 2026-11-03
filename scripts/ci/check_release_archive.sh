#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_release_archive.sh — Verify a packaged release archive for a supported
# target triple.
#
# Usage: check_release_archive.sh <target-triple> <expected-version> --expected-commit <sha> --expected-tree <sha> [--allow-unsigned] [--host-independent] [archive-path]
#
# When archive-path is omitted, the script looks for
# dist/transist-*<target-triple>.tar.gz in the repository root.
#
# <expected-version> is the version string each binary must report via --version
# (e.g. "0.1.0"). A leading "v" is stripped automatically.
#
# Checks:
#   - checksum sidecar exists and matches archive digest (verified first)
#   - archive has exactly one top-level directory matching the expected name
#   - archive contains every file/directory in the canonical release inventory
#     (scripts/ci/release-archive-inventory.json)
#   - plugins/shell/plugin.py is executable
#   - each binary exits 0 with --help and --version (native mode)
#   - each --version output matches the expected version (native mode)
#   - binary architecture matches the target triple (native mode)
#   - no symlinks, device nodes, or unsafe paths in the archive
#   - no private key material in the archive
#   - archive signatures are mandatory unless --allow-unsigned is explicitly
#     supplied for the pre-sign validation stage
#
# Exit status: 0 (all checks pass) or 1 (any check fails).

set -euo pipefail

if [[ "$#" -lt 2 ]]; then
  echo "Usage: $0 <target-triple> <expected-version> --expected-commit <sha> --expected-tree <sha> [--allow-unsigned] [--host-independent] [archive-path]" >&2
  exit 1
fi

target_triple="$1"
expected_version="$2"
shift 2
allow_unsigned=0
host_independent=0
archive_path=""
expected_commit=""
expected_tree=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --expected-commit)
      [[ "$#" -ge 2 && -n "$2" ]] || { echo "--expected-commit requires a value" >&2; exit 1; }
      expected_commit="$2"
      shift 2
      ;;
    --expected-tree)
      [[ "$#" -ge 2 && -n "$2" ]] || { echo "--expected-tree requires a value" >&2; exit 1; }
      expected_tree="$2"
      shift 2
      ;;
    --allow-unsigned)
      allow_unsigned=1
      shift
      ;;
    --host-independent)
      host_independent=1
      shift
      ;;
    --*)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
    *)
      [[ -z "$archive_path" ]] || { echo "Only one archive path is allowed" >&2; exit 1; }
      archive_path="$1"
      shift
      ;;
  esac
done
if [[ -z "$expected_commit" || -z "$expected_tree" ]]; then
  echo "FAIL: expected release commit and tree are required for provenance verification" >&2
  exit 1
fi
# Strip a leading "v" so callers can pass either "v1.2.3" or "1.2.3".
expected_version="${expected_version#v}"

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
dist_dir="$repo_root/dist"

inventory_path="$repo_root/scripts/ci/release-archive-inventory.json"
if [[ ! -f "$inventory_path" ]]; then
  echo "FAIL: missing canonical release inventory: $inventory_path" >&2
  exit 1
fi

# The supported-target set comes from the canonical inventory, never from a
# hardcoded list here.
if ! python3 - "$target_triple" "$inventory_path" <<'PY'
import json
import sys

triple, inventory_path = sys.argv[1], sys.argv[2]
with open(inventory_path, encoding="utf-8") as stream:
    inventory = json.load(stream)
supported = {entry["target_triple"] for entry in inventory["released_archives"]}
if triple not in supported:
    print(f"Unsupported target triple: {triple}", file=sys.stderr)
    print(f"Supported by the release inventory: {', '.join(sorted(supported))}", file=sys.stderr)
    sys.exit(1)
PY
then
  exit 1
fi

read -r target_os target_architecture _target_abi includes_sandbox_launcher < <(
  python3 - "$target_triple" "$inventory_path" <<'PY'
import json
import sys

target, inventory_path = sys.argv[1:]
with open(inventory_path, encoding="utf-8") as stream:
    entries = json.load(stream)["released_archives"]
entry = next(item for item in entries if item["target_triple"] == target)
print(entry["os"], entry["architecture"], entry["abi"], str(entry["includes_sandbox_launcher"]).lower())
PY
)

if [[ -z "$archive_path" ]]; then
  shopt -s nullglob
  candidates=("$dist_dir"/transist-*"${target_triple}".tar.gz)
  if [[ "${#candidates[@]}" -eq 1 ]]; then
    archive_path="${candidates[0]}"
  elif [[ "${#candidates[@]}" -gt 1 ]]; then
    echo "FAIL: multiple archives found for target ${target_triple}" >&2
    printf '  %s\n' "${candidates[@]}" >&2
    exit 1
  fi
fi

if [[ -z "$archive_path" || ! -f "$archive_path" || -L "$archive_path" ]]; then
  echo "FAIL: no archive found for target ${target_triple}" >&2
  echo "  searched: ${dist_dir}/transist-*${target_triple}.tar.gz" >&2
  exit 1
fi

checksum_path="${archive_path}.sha256"
expected_pkg_dir="transist-v${expected_version}-${target_triple}"

failures=0

echo "============================================================"
echo "  Release Archive Check"
echo "  target:  $target_triple"
echo "  version: $expected_version"
echo "  archive: $archive_path"
echo "============================================================"

# ---- Checksum verification (before extraction) ----
echo ""
echo "--- Checksum verification ---"
if [[ ! -f "$checksum_path" || -L "$checksum_path" ]]; then
  echo "  FAIL  checksum sidecar not found: $checksum_path"
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  expected_digest="$(sha256sum "$archive_path" | cut -d' ' -f1)"
  recorded_digest="$(cut -d' ' -f1 "$checksum_path")"
else
  expected_digest="$(shasum -a 256 "$archive_path" | cut -d' ' -f1)"
  recorded_digest="$(cut -d' ' -f1 "$checksum_path")"
fi
if [[ "$expected_digest" == "$recorded_digest" ]]; then
  echo "  PASS  checksum matches ($expected_digest)"
else
  echo "  FAIL  checksum mismatch"
  echo "    expected: $expected_digest"
  echo "    recorded: $recorded_digest"
  exit 1
fi
checksum_name="$(awk 'NF == 2 { print $2 } NF != 2 { exit 1 }' "$checksum_path")" || {
  echo "  FAIL  checksum sidecar must contain exactly one digest and filename" >&2
  exit 1
}
checksum_name="${checksum_name#\*}"
if [[ "$checksum_name" != "$(basename "$archive_path")" ]]; then
  echo "  FAIL  checksum sidecar names '$checksum_name', expected '$(basename "$archive_path")'" >&2
  exit 1
fi

# ---- Validate archive members before extraction ----
echo ""
echo "--- Archive member validation ---"
if python3 "$repo_root/scripts/ci/validate_release_archive_members.py" \
    "$target_triple" "$expected_version" "$archive_path"; then
  echo "  PASS  archive members are safe and match the canonical inventory"
else
  echo "  FAIL  archive member validation failed; refusing extraction" >&2
  exit 1
fi

signature_path="${archive_path}.minisig"
public_key_path="$repo_root/transist-release.minisign.pub"
if [[ "$allow_unsigned" -eq 1 ]]; then
  if [[ -f "$signature_path" ]]; then
    echo "  FAIL  --allow-unsigned cannot validate an archive with a signature" >&2
    exit 1
  fi
  echo "  PASS  unsigned pre-sign validation explicitly enabled"
elif [[ ! -f "$signature_path" || ! -f "$public_key_path" ]]; then
  if [[ ! -f "$signature_path" ]]; then
    echo "  FAIL  archive signature is mandatory for post-sign validation" >&2
  else
    echo "  FAIL  committed archive verification key is missing" >&2
  fi
  exit 1
else
  if ! minisign -Vq -p "$public_key_path" -m "$archive_path" -x "$signature_path"; then
    echo "  FAIL  archive signature verification failed" >&2
    exit 1
  fi
  echo "  PASS  archive signature verifies"
fi

# ---- Extract archive ----
echo ""
echo "--- Extraction ---"
extract_dir="$(mktemp -d)"
trap 'rm -rf "$extract_dir"' EXIT

if tar -xzf "$archive_path" -C "$extract_dir" 2>/dev/null; then
  echo "  PASS  archive extracts successfully"
else
  echo "  FAIL  archive extraction failed"
  exit 1
fi

pkg_dir="$extract_dir/$expected_pkg_dir"
if [[ ! -d "$pkg_dir" ]]; then
  echo "  FAIL  expected package directory not found: $expected_pkg_dir"
  exit 1
fi

extracted_specials="$(find "$pkg_dir" \( -type l -o -type b -o -type c -o -type p -o -type s \) -print 2>/dev/null || true)"
if [[ -n "$extracted_specials" ]]; then
  echo "  FAIL  extracted package contains links or special files:"
  printf '%s\n' "$extracted_specials"
  failures=$((failures + 1))
else
  echo "  PASS  extracted package contains only regular files and directories"
fi

# ---- Required files ----
echo ""
echo "--- Required files ---"
# The exact archive contents come from the canonical release inventory, so an
# inventory change is the single place that defines what must ship.
# (while-read keeps this compatible with macOS's shipped bash 3.2.)
required_files=()
while IFS= read -r rel; do
  [[ -z "$rel" ]] && continue
  required_files+=("$rel")
done < <(python3 - "$inventory_path" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as stream:
    inventory = json.load(stream)
print("\n".join(inventory["archive_contents"]["required_files"]))
PY
)
for rel in "${required_files[@]}"; do
  if [[ -f "$pkg_dir/$rel" ]]; then
    echo "  PASS  $rel"
  else
    echo "  FAIL  missing: $rel"
    failures=$((failures + 1))
  fi
done

# Target-specific required files (currently the Linux sandbox launcher).
if [[ "$includes_sandbox_launcher" == true ]]; then
  linux_required_files=()
  while IFS= read -r rel; do
    [[ -z "$rel" ]] && continue
    linux_required_files+=("$rel")
  done < <(python3 - "$inventory_path" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    inventory = json.load(stream)
print("\n".join(inventory["archive_contents"].get("linux_required_files", [])))
PY
  )
  for rel in "${linux_required_files[@]}"; do
    if [[ -f "$pkg_dir/$rel" ]]; then
      echo "  PASS  $rel (linux-only)"
    else
      echo "  FAIL  missing linux-only file: $rel"
      failures=$((failures + 1))
    fi
  done
fi

# Provenance is split deliberately: native build details must not be confused
# with the single environment that produced the final tarball.
if python3 - "$pkg_dir/build-provenance.json" "$target_triple" "$expected_commit" "$expected_tree" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    value = json.load(stream)
assert value.get("kind") == "native-build"
assert value.get("target") == sys.argv[2], "provenance target does not match archive target"
assert value.get("source", {}).get("commit_sha") == sys.argv[3], "native provenance commit does not match expected release commit"
assert value.get("source", {}).get("tree_sha") == sys.argv[4], "native provenance tree does not match expected release tree"
assert value.get("runner", {}).get("os")
assert isinstance(value.get("binaries"), list) and value["binaries"], \
    "native provenance must record at least one binary with SHA-256"
for b in value["binaries"]:
    assert b.get("name") and b.get("sha256")
PY
then
  echo "  PASS  native build provenance is structured"
else
  echo "  FAIL  native build provenance is missing required fields"
  failures=$((failures + 1))
fi
if python3 - "$pkg_dir/packaging-provenance.json" "$expected_commit" "$expected_tree" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    value = json.load(stream)
assert value.get("kind") == "final-packaging"
assert value.get("source", {}).get("commit_sha") == sys.argv[2], "packaging provenance commit does not match expected release commit"
assert value.get("source", {}).get("tree_sha") == sys.argv[3], "packaging provenance tree does not match expected release tree"
assert value.get("packaging", {}).get("container_image")
PY
then
  echo "  PASS  final packaging provenance is structured and bound to the expected release commit/tree"
else
  echo "  FAIL  final packaging provenance is missing required fields or is not bound to the expected release commit/tree"
  failures=$((failures + 1))
fi

# ---- Packaged binary matches native provenance digest ----
# The bundle must be the exact verified binary recorded in native provenance,
# never a different build. Compute the packaged binary digest and compare it to
# the recorded per-binary SHA-256; a mismatch fails closed.
echo ""
echo "--- Packaged binary vs native provenance ---"
if ! python3 - "$pkg_dir/build-provenance.json" "$pkg_dir" "$target_triple" "$includes_sandbox_launcher" <<'PY'
import hashlib
import json
import os
import sys

provenance_path, pkg_dir, target, includes_launcher = sys.argv[1:]
with open(provenance_path, encoding="utf-8") as stream:
    value = json.load(stream)
recorded = {b["name"]: b["sha256"] for b in value.get("binaries", [])}
expect = {"transist": "bin/transist", "transist-server": "bin/transist-server",
          "transist-admin": "bin/transist-admin"}
# The Linux-only sandbox launcher ships in the Linux archives and is recorded
# in native provenance; verify its digest too so a swapped launcher fails closed.
if includes_launcher == "true":
    expect["transist-sandbox-launcher"] = "bin/transist-sandbox-launcher"
missing = [rel for rel in expect.values() if not os.path.isfile(os.path.join(pkg_dir, rel))]
assert not missing, f"packaged binaries missing from archive: {missing}"
matched = True
for name, rel in expect.items():
    if name not in recorded:
        print(f"  missing recorded digest for {name}", file=sys.stderr)
        matched = False
        continue
    actual = hashlib.sha256(open(os.path.join(pkg_dir, rel), "rb").read()).hexdigest()
    if actual != recorded[name]:
        print(f"  digest mismatch: {name} packaged {actual} != recorded {recorded[name]}", file=sys.stderr)
        matched = False
assert matched, "packaged binary does not match native provenance digest"
PY
then
  echo "  FAIL  packaged binary does not match native provenance digest"
  failures=$((failures + 1))
else
  echo "  PASS  packaged binary matches native provenance digest"
fi

required_dirs=()
while IFS= read -r rel; do
  [[ -z "$rel" ]] && continue
  required_dirs+=("$rel")
done < <(python3 - "$inventory_path" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as stream:
    inventory = json.load(stream)
print("\n".join(inventory["archive_contents"]["required_dirs"]))
PY
)
for rel in "${required_dirs[@]}"; do
  if [[ -d "$pkg_dir/$rel" ]]; then
    echo "  PASS  $rel/ (directory)"
  else
    echo "  FAIL  missing directory: $rel/"
    failures=$((failures + 1))
  fi
done

# No private key material may ship in the archive. Check filenames and common
# PEM, JSON/JWK, YAML, and base64 credential forms.
priv_keys="$(find "$pkg_dir" -type f \( -name '*.key' -o -name '*.pem' -o -name '*.p12' -o -name '*.pfx' -o -name '*.jks' -o -name '*.keystore' \) -print 2>/dev/null || true)"
priv_content="$(grep -rIlE --exclude='*.minisig' --exclude='*.pub' -- \
  '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----|(private_key|privateKey|client_secret|access_token|secret_key|credential)[^A-Za-z0-9]*[:=][^A-Za-z0-9]*[A-Za-z0-9+/]{40,}={0,2}|(^|[^A-Za-z0-9_])d[^A-Za-z0-9_]*:[^A-Za-z0-9]*[A-Za-z0-9_-]{32,}' \
  "$pkg_dir" 2>/dev/null || true)"
if [[ -n "$priv_keys" || -n "$priv_content" ]]; then
  echo "  FAIL  private key material present in archive:"
  [[ -n "$priv_keys" ]] && echo "$priv_keys"
  [[ -n "$priv_content" ]] && echo "$priv_content"
  failures=$((failures + 1))
else
  echo "  PASS  no private key material in archive"
fi

# Markdown is limited to root distribution/legal documents and complete tracked
# ancillary skills. Retained-tree Markdown remains source-only.
retained_docs="$(find "$pkg_dir" -name '*.md' -type f \
  ! -path "$pkg_dir/README.md" \
  ! -path "$pkg_dir/THIRD_PARTY_NOTICES.md" \
  ! -path "$pkg_dir/.agents/skills/*" \
  -print 2>/dev/null || true)"
if [[ -n "$retained_docs" ]]; then
  echo "  FAIL  archive contains non-root Markdown documentation:"
  printf '    %s\n' "$retained_docs"
  failures=$((failures + 1))
else
  echo "  PASS  archive contains only expected Markdown files"
fi

# Every Git-tracked skill file ships byte-for-byte. Untracked paths cannot
# appear because archive member validation compares the exact inventory.
tracked_skill_count=0
while IFS= read -r -d '' rel; do
  source="$repo_root/$rel"
  archived="$pkg_dir/$rel"
  if [[ ! -f "$archived" ]] || ! cmp -s "$source" "$archived"; then
    echo "  FAIL  tracked skill is missing or differs: $rel"
    failures=$((failures + 1))
  fi
  tracked_skill_count=$((tracked_skill_count + 1))
done < <(git -C "$repo_root" ls-files -z -- '.agents/skills/**')
if [[ "$tracked_skill_count" -eq 0 ]]; then
  echo "  FAIL  .agents/skills contains no Git-tracked files"
  failures=$((failures + 1))
else
  echo "  PASS  all tracked skill files match source bytes"
fi

# Forbidden paths come from the canonical inventory and must never appear in an
# archive. They remain in the source repository; only their inclusion in release
# archives changes.
forbidden_found="$(python3 - "$inventory_path" "$pkg_dir" <<'PY'
import json
import os
import sys

inventory_path, pkg_dir = sys.argv[1], sys.argv[2]
with open(inventory_path, encoding="utf-8") as stream:
    inventory = json.load(stream)
forbidden = inventory["archive_contents"].get("forbidden_paths", [])
for rel in forbidden:
    rel = rel.rstrip("/")
    if os.path.exists(os.path.join(pkg_dir, rel)):
        print(rel)
PY
)"
if [[ -n "$forbidden_found" ]]; then
  echo "  FAIL  archive contains forbidden paths:"
  printf '    %s\n' "$forbidden_found" >&2
  failures=$((failures + 1))
else
  echo "  PASS  no forbidden paths in archive"
fi

# The bundled shell plugin must declare a local provider so the quickstart
# cannot make non-loopback connections under --local-only.
if grep -Eq 'default_provider_kind:[[:space:]]*local' \
    "$pkg_dir/plugins/shell/plugin.yaml" 2>/dev/null; then
  echo "  PASS  plugins/shell declares default_provider_kind: local"
else
  echo "  FAIL  plugins/shell/plugin.yaml must declare default_provider_kind: local"
  failures=$((failures + 1))
fi

# The README must describe this archive and the exact bundled plugin manifest.
if python3 - "$pkg_dir/README.md" "$pkg_dir/plugins/shell/plugin.yaml" "$expected_version" <<'PY'
import re
import sys

readme_path, manifest_path, expected_release_version = sys.argv[1:]
with open(readme_path, encoding="utf-8") as stream:
    readme = stream.read()
with open(manifest_path, encoding="utf-8") as stream:
    manifest = stream.read()

manifest_versions = re.findall(
    r"^version:[ \t]*([^#\s]+)[ \t]*(?:#.*)?$", manifest, re.MULTILINE
)
if len(manifest_versions) != 1:
    raise SystemExit("bundled shell manifest must declare exactly one version")
plugin_version = manifest_versions[0]

documented_plugin_versions = re.findall(
    r"The bundled `shell` plugin \(`([^`]+)`,", readme
)
if documented_plugin_versions != [plugin_version]:
    raise SystemExit(
        "README.md must document bundled shell plugin version " + plugin_version
    )

documented_release_versions = set(re.findall(r"\bv([0-9]+\.[0-9]+\.[0-9]+)\b", readme))
if documented_release_versions != {expected_release_version}:
    raise SystemExit(
        "README.md release versions must all match v" + expected_release_version
    )

unresolved = sorted(set(re.findall(r"@[A-Z][A-Z0-9_]*@", readme)))
if unresolved:
    raise SystemExit(
        "README.md contains unresolved placeholders: " + ", ".join(unresolved)
    )
PY
then
  echo "  PASS  README.md release and bundled plugin versions match packaged metadata"
else
  echo "  FAIL  README.md release or bundled plugin version does not match packaged metadata"
  failures=$((failures + 1))
fi

# ---- File mode checks ----
echo ""
echo "--- File mode checks ---"
check_mode() {
  local path="$1"
  local expected="$2"
  local full="$pkg_dir/$path"
  if [[ ! -e "$full" ]]; then
    echo "  SKIP  $path (not present)"
    return
  fi
  if [[ "$(uname -s)" == "Darwin" ]]; then
    actual="$(stat -f "%Lp" "$full")"
  else
    actual="$(stat -c "%a" "$full")"
  fi
  if [[ "$actual" == "$expected" ]]; then
    echo "  PASS  $path ($actual)"
  else
    echo "  FAIL  $path: expected $expected, got $actual"
    failures=$((failures + 1))
  fi
}

check_mode "bin/transist" "755"
check_mode "bin/transist-server" "755"
check_mode "bin/transist-admin" "755"
# Target-specific sandbox launcher: present and executable only where declared.
if [[ "$includes_sandbox_launcher" == true ]]; then
  check_mode "bin/transist-sandbox-launcher" "755"
fi
check_mode "plugins/shell/plugin.py" "755"
check_mode "plugins/shell/plugin.yaml" "644"
check_mode "plugins/shell.sig" "644"
check_mode "README.md" "644"
check_mode "LICENSE" "644"
check_mode "NOTICE" "644"
check_mode "THIRD_PARTY_NOTICES.md" "644"
check_mode "build-provenance.json" "644"
check_mode "packaging-provenance.json" "644"
while IFS= read -r -d '' rel; do
  check_mode "$rel" "644"
done < <(git -C "$repo_root" ls-files -z -- '.agents/skills/**')
while IFS= read -r dir; do
  [[ "$dir" == "$pkg_dir/.agents/skills" || "$dir" == "$pkg_dir/.agents/skills/"* ]] || continue
  relative="${dir#"$pkg_dir/"}"
  check_mode "$relative" "755"
done < <(find "$pkg_dir/.agents/skills" -type d -print)

# Centralized packaging validates every target on one host. Preserve all
# byte/member/provenance checks there, but leave execution, architecture, and
# linkage checks to the required native archive-smoke matrix.
if [[ "$host_independent" -eq 1 ]]; then
  echo ""
  echo "--- Host-specific binary validation ---"
  echo "  PASS  deferred to the required native archive-smoke matrix"
else
  # ---- Binary smoke tests (require zero exit status) ----
  echo ""
  echo "--- Binary smoke tests ---"

base_version="$expected_version"

expected_cli_version="transist $base_version"
expected_server_version="transist-server $base_version"
expected_admin_version="transist-admin $base_version"


for bin_name in transist transist-server transist-admin; do
  bin_path="$pkg_dir/bin/$bin_name"
  if [[ ! -x "$bin_path" ]]; then
    echo "  FAIL  $bin_name not executable"
    failures=$((failures + 1))
    continue
  fi

  # --help must exit 0
  if "$bin_path" --help > /dev/null 2>&1; then
    echo "  PASS  $bin_name --help (exit 0)"
  else
    echo "  FAIL  $bin_name --help (nonzero exit)"
    failures=$((failures + 1))
    continue
  fi

  # --version must exit 0 and match expected output
  version_output="$("$bin_path" --version 2>&1)" && version_status=0 || version_status=$?
  if [[ $version_status -ne 0 ]]; then
    echo "  FAIL  $bin_name --version (nonzero exit: $version_status)"
    failures=$((failures + 1))
    continue
  fi

  case "$bin_name" in
    transist)        expected="$expected_cli_version" ;;
    transist-server) expected="$expected_server_version" ;;
    transist-admin)  expected="$expected_admin_version" ;;
  esac

  # Normalize whitespace for comparison
  actual_normalized="$(echo "$version_output" | tr -s '[:space:]' ' ' | sed 's/^ //;s/ $//')"
  expected_normalized="$(echo "$expected" | tr -s '[:space:]' ' ' | sed 's/^ //;s/ $//')"

  if [[ "$actual_normalized" == "$expected_normalized" ]]; then
    echo "  PASS  $bin_name --version ($version_output)"
  else
    echo "  FAIL  $bin_name --version: expected '$expected', got '$version_output'"
    failures=$((failures + 1))
  fi
done

# ---- Binary architecture check ----
echo ""
echo "--- Binary architecture check ---"
bin_path="$pkg_dir/bin/transist"
bin_arch="$(file -b "$bin_path" 2>/dev/null | head -1)"
host_os="$(uname -s)"
case "$host_os" in
  Darwin)
    case "$target_os:$target_architecture" in
      macos:aarch64)
        if echo "$bin_arch" | grep -qi 'arm64\|aarch64'; then
          echo "  PASS  binary architecture matches ($bin_arch)"
        else
          echo "  FAIL  binary architecture mismatch: expected arm64, got '$bin_arch'"
          failures=$((failures + 1))
        fi
        ;;
      macos:x86_64)
        if echo "$bin_arch" | grep -qi 'x86-64\|x86_64'; then
          echo "  PASS  binary architecture matches ($bin_arch)"
        else
          echo "  FAIL  binary architecture mismatch: expected x86-64, got '$bin_arch'"
          failures=$((failures + 1))
        fi
        ;;
      *)
        echo "  FAIL  target '$target_triple' is not a native macOS ARM64 target"
        failures=$((failures + 1))
        ;;
    esac
    ;;
  Linux)
    case "$target_os:$target_architecture" in
      linux:x86_64)
        if echo "$bin_arch" | grep -qi 'x86-64\|x86_64'; then
          echo "  PASS  binary architecture matches ($bin_arch)"
        else
          echo "  FAIL  binary architecture mismatch: expected x86-64, got '$bin_arch'"
          failures=$((failures + 1))
        fi
        ;;
      linux:aarch64)
        if echo "$bin_arch" | grep -qi 'arm64\|aarch64'; then
          echo "  PASS  binary architecture matches ($bin_arch)"
        else
          echo "  FAIL  binary architecture mismatch: expected ARM64, got '$bin_arch'"
          failures=$((failures + 1))
        fi
        ;;
      *)
        echo "  FAIL  target '$target_triple' is not a native Linux x86-64 target"
        failures=$((failures + 1))
        ;;
    esac
    ;;
  *)
    echo "  FAIL  unsupported host OS for architecture verification: $host_os"
    failures=$((failures + 1))
    ;;
esac

# ---- musl glibc-independence check ----
# The musl archive is a self-contained static build of the runtime; it must
# not link against glibc, so it can run on musl-only distributions such as
# Alpine. GNU and macOS archives are exempt. Every shipped musl binary is
# checked (never just the primary CLI), and the check never SKIPs: if no
# glibc-detection mechanism is available the musl leg fails closed.
if [[ "$target_triple" == *"-musl" ]]; then
  echo ""
  echo "--- musl glibc-independence check ---"
  ldd_available=0
  readelf_available=0
  command -v ldd >/dev/null 2>&1 && ldd_available=1
  command -v readelf >/dev/null 2>&1 && readelf_available=1
  # ldd is a shell script that needs a runtime loader; a read-only executable
  # copied out of a tarball may not be runnable, so readelf (a pure parser) is
  # the authoritative mechanism.
  musl_bins=(transist transist-server transist-admin transist-sandbox-launcher)
  for bin_name in "${musl_bins[@]}"; do
    musl_bin="$pkg_dir/bin/$bin_name"
    if [[ ! -f "$musl_bin" ]]; then
      continue
    fi
    detected_glibc=0
    if [[ "$readelf_available" -eq 1 ]] && readelf -l "$musl_bin" >/dev/null 2>&1; then
      if readelf -l "$musl_bin" 2>/dev/null | grep -q 'interpreter.*ld-linux'; then
        echo "  FAIL  $bin_name contains a glibc ELF interpreter"
        detected_glibc=1
      fi
    elif [[ "$ldd_available" -eq 1 ]]; then
      if ldd "$musl_bin" 2>&1 | grep -q -i 'libc\.so\|ld-linux'; then
        echo "  FAIL  $bin_name links against glibc:"
        ldd "$musl_bin" 2>&1 | sed 's/^/    /'
        detected_glibc=1
      fi
    else
      echo "  FAIL  neither ldd nor readelf is available to verify musl glibc-independence for $bin_name (failing closed)"
      failures=$((failures + 1))
      continue
    fi
    if [[ "$detected_glibc" -eq 0 ]]; then
      echo "  PASS  $bin_name is glibc-independent"
    else
      failures=$((failures + 1))
    fi
  done
fi
fi

# ---- Doc-link completeness check ----
# Verify that no packaged Markdown doc links to a path missing from the archive.
echo ""
echo "--- Doc-link completeness ---"
if python3 "$repo_root/scripts/ci/check_doc_links.py" "$pkg_dir"
then
  echo "  PASS  no broken packaged-doc links"
else
  echo "  FAIL  packaged docs contain broken or archive-escaping links (see above)"
  failures=$((failures + 1))
fi

# ---- Summary ----
echo ""
echo "============================================================"
if [[ "$failures" -eq 0 ]]; then
  echo "  RESULT: All release archive checks passed"
else
  echo "  RESULT: $failures release archive check(s) failed"
fi
echo "============================================================"
exit "$failures"
