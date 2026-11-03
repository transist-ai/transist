#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# verify_downloaded_draft.sh — Independently re-validate a downloaded release
# draft asset set without re-uploading anything.
#
# The release draft ships the six archives in release-archive-inventory.json
# (Linux GNU/musl and macOS, each on x86_64/ARM64) plus checksum sidecars,
# native build provenance, final packaging provenance, a deterministic SBOM
# archive, the verification key, and the signed release manifest. This script
# reruns, on the downloaded (published) asset set and for all six targets:
#
#   - SHA-256 checksum sidecar verification (digest and filename)
#   - canonical archive member validation (inventory, paths, and entry types)
#   - provenance-to-packaged-binary comparison: every binary shipped in an
#     archive must byte-match the per-binary SHA-256 recorded in that target's
#     native build provenance
#   - SBOM semantic validation of every CycloneDX document against the pinned
#     schema
#
# These checks are host-independent (pure byte/digest/member inspection), so
# they run uniformly for all six targets on any host. Checks that require
# executing a target's binaries (--help/--version) or architecture are host
# specific by nature and are left to the targeted host runners (archive-smoke
# matrix); callers may additionally run check_release_archive.sh on the
# targets their host can execute.
#
# It intentionally does NOT touch the remote: no GitHub API calls, no upload.
#
# Usage:
#   verify_downloaded_draft.sh <tag> <downloaded-dist-dir> --expected-commit <sha> --expected-tree <sha>
#
# Exit status: 0 (all checks pass) or 1 (any check fails).

set -euo pipefail

if [[ "$#" -lt 2 ]]; then
  echo "Usage: $0 <tag> <downloaded-dist-dir> --expected-commit <sha> --expected-tree <sha>" >&2
  exit 1
fi

tag="$1"
dist="$2"
shift 2
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
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done
if [[ -z "$expected_commit" || -z "$expected_tree" ]]; then
  echo "FAIL: expected release commit and tree are required for provenance verification" >&2
  exit 1
fi
tag="${tag#v}"

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

inventory_path="scripts/ci/release-archive-inventory.json"
[[ -f "$inventory_path" ]] || { echo "FAIL: missing release inventory $inventory_path" >&2; exit 1; }

packaging_provenance="$dist/packaging-provenance.json"
if python3 - "$packaging_provenance" "$expected_commit" "$expected_tree" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    value = json.load(fh)
assert value.get("kind") == "final-packaging"
assert value.get("source", {}).get("commit_sha") == sys.argv[2]
assert value.get("source", {}).get("tree_sha") == sys.argv[3]
assert value.get("packaging", {}).get("container_image")
PY
then
  echo "PASS  downloaded packaging provenance is bound to the expected release commit/tree"
else
  echo "FAIL: downloaded packaging provenance is missing or is not bound to the expected release commit/tree" >&2
  exit 1
fi

failures=0

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | cut -d' ' -f1
  else
    shasum -a 256 "$path" | cut -d' ' -f1
  fi
}

targets=()
while IFS= read -r t; do
  [[ -z "$t" ]] && continue
  targets+=("$t")
done < <(python3 - "$inventory_path" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print("\n".join(e["target_triple"] for e in json.load(fh)["released_archives"]))
PY
)

target_includes_sandbox_launcher() {
  python3 scripts/ci/release_inventory.py target-field \
    "$inventory_path" "$1" includes_sandbox_launcher
}

# Validate every archive before extracting any member from the downloaded set.
echo ""
echo "--- Validating downloaded archive members ---"
for target in "${targets[@]}"; do
  archive="$dist/transist-v${tag}-${target}.tar.gz"
  if ! python3 scripts/ci/validate_release_archive_members.py "$target" "$tag" "$archive"; then
    echo "  FAIL  archive member validation failed: $archive" >&2
    failures=$((failures + 1))
  else
    echo "  PASS  $target members are safe and match the canonical inventory"
  fi
done
if [[ "$failures" -ne 0 ]]; then
  echo "FAIL: refusing to extract an unsafe or malformed downloaded archive" >&2
  exit "$failures"
fi

# The exact set of per-target archives is derived from the canonical inventory;
# no target may be invented or forgotten by a hardcoded list.
for target in "${targets[@]}"; do
  archive="$dist/transist-v${tag}-${target}.tar.gz"
  checksum="$archive.sha256"
  provenance="$dist/build-provenance-${target}.json"
  echo ""
  echo "--- Validating downloaded draft target: $target ---"

  [[ -f "$archive" ]] || { echo "  FAIL  archive missing: $archive" >&2; failures=$((failures + 1)); continue; }
  [[ -f "$checksum" ]] || { echo "  FAIL  checksum sidecar missing: $checksum" >&2; failures=$((failures + 1)); continue; }
  [[ -f "$provenance" ]] || { echo "  FAIL  build provenance missing: $provenance" >&2; failures=$((failures + 1)); continue; }

  # 1. Checksum sidecar verification
  actual_digest="$(sha256_file "$archive")"
  recorded_digest="$(awk '{print $1}' "$checksum")"
  recorded_name="$(awk '{print $2}' "$checksum")"
  if [[ "$actual_digest" == "$recorded_digest" && "$recorded_name" == "$(basename "$archive")" ]]; then
    echo "  PASS  checksum sidecar matches archive ($actual_digest)"
  else
    echo "  FAIL  checksum mismatch or filename mismatch for $archive" >&2
    failures=$((failures + 1))
  fi

  # 2. Provenance-to-packaged-binary comparison
  expected_pkg="transist-v${tag}-${target}"
  includes_sandbox_launcher="$(target_includes_sandbox_launcher "$target")"
  extract="$(mktemp -d)"
  trap 'rm -rf "$extract"' EXIT
  if ! tar -xzf "$archive" -C "$extract" 2>/dev/null; then
    echo "  FAIL  archive extraction failed: $archive" >&2
    failures=$((failures + 1))
    continue
  fi
  if ! python3 - "$provenance" "$extract/$expected_pkg" "$target" "$expected_commit" "$expected_tree" "$includes_sandbox_launcher" <<'PY'
import hashlib, json, os, sys
provenance, pkg, target, expected_commit, expected_tree, includes_launcher = sys.argv[1:]
with open(provenance, encoding="utf-8") as fh:
    value = json.load(fh)
assert value.get("kind") == "native-build"
assert value.get("target") == target
assert value.get("source", {}).get("commit_sha") == expected_commit
assert value.get("source", {}).get("tree_sha") == expected_tree
recorded = {b["name"]: b["sha256"] for b in value.get("binaries", [])}
expect = ["transist", "transist-server", "transist-admin"]
if includes_launcher == "true":
    expect.append("transist-sandbox-launcher")
failed = False
for name in expect:
    path = os.path.join(pkg, "bin", name)
    if name not in recorded:
        print(f"  FAIL  no recorded digest for {name}", file=sys.stderr)
        failed = True
        continue
    if not os.path.isfile(path):
        print(f"  FAIL  packaged binary missing: {name}", file=sys.stderr)
        failed = True
        continue
    actual = hashlib.sha256(open(path, "rb").read()).hexdigest()
    if actual != recorded[name]:
        print(f"  FAIL  digest mismatch for {name}: packaged {actual} != recorded {recorded[name]}", file=sys.stderr)
        failed = True
assert not failed, "packaged binary does not match native provenance digest"
PY
  then
    failures=$((failures + 1))
  else
    echo "  PASS  every packaged binary matches native provenance digest"
  fi
  rm -rf "$extract"
done

# 3. SBOM semantic validation (host independent)
echo ""
echo "--- Validating downloaded draft SBOM ---"
sbom_archive="$dist/transist-v${tag}-sbom.tar.gz"
if [[ ! -f "$sbom_archive" ]]; then
  echo "  FAIL  SBOM archive missing: $sbom_archive" >&2
  failures=$((failures + 1))
else
  sbom_extract="$(mktemp -d)"
  trap 'rm -rf "$sbom_extract"' EXIT
  if ! tar -C "$sbom_extract" -xzf "$sbom_archive" 2>/dev/null; then
    echo "  FAIL  SBOM archive could not be extracted" >&2
    failures=$((failures + 1))
  else
    sbom_files=()
    while IFS= read -r -d '' f; do
      sbom_files+=("$f")
    done < <(find "$sbom_extract" -name '*.json' -print0)
    if [[ "${#sbom_files[@]}" -eq 0 ]]; then
      echo "  FAIL  SBOM archive contains no JSON documents" >&2
      failures=$((failures + 1))
    elif python3 scripts/ci/normalize_and_validate_sbom.py --validate "${sbom_files[@]}"; then
      echo "  PASS  every SBOM document is semantically valid CycloneDX"
    else
      echo "  FAIL  SBOM semantic validation failed" >&2
      failures=$((failures + 1))
    fi
    rm -rf "$sbom_extract"
  fi
fi

echo ""
echo "============================================================"
if [[ "$failures" -eq 0 ]]; then
  echo "  RESULT: All downloaded-draft validation checks passed"
else
  echo "  RESULT: $failures downloaded-draft validation check(s) failed"
fi
echo "============================================================"
exit "$failures"
