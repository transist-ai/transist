#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# release_manifest.sh — Generate and verify the sorted release manifest.
#
# The manifest lists each payload asset digest: archives, checksum files,
# archive signatures, native build provenance, final-packaging provenance,
# SBOM, and the committed verification key. It excludes
# itself (release-manifest.txt) and its signature (release-manifest.txt.minisig)
# by definition.
#
# Manifest format: standard shasum -a 256 output, one entry per line:
#   <sha256-digest>  <filename>
#
# Entries are sorted alphabetically by filename for deterministic output.
#
# Usage:
#   release_manifest.sh generate <tag> <dist-dir>
#       Generate the sorted manifest at <dist-dir>/release-manifest.txt.
#       Every payload file listed below must exist in <dist-dir>.
#       Outputs the manifest path on stdout.
#
#   release_manifest.sh verify <tag> <dist-dir>
#       Verify the manifest at <dist-dir>/release-manifest.txt:
#         - entries are sorted by filename
#         - contains exactly the expected payload entries
#         - excludes itself and its signature
#         - every entry digest matches the actual file digest
#       Exit 0 on success, 1 on any failure.
#
#   release_manifest.sh compare-draft <tag> <dist-dir>
#       Compare draft release assets against the manifest.
#       Reads <dist-dir>/release-manifest.txt and verifies that every
#       file in <dist-dir> is either a manifest entry or one of
#       release-manifest.txt / release-manifest.txt.minisig.
#       Fails on any missing or extra asset.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INVENTORY_PATH="$SCRIPT_DIR/release-archive-inventory.json"

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | cut -d' ' -f1
  else
    shasum -a 256 "$path" | cut -d' ' -f1
  fi
}

# The supported-target set comes from the canonical release inventory, so the
# release manifest cannot drift from the shipped archive matrix.
inventory_targets() {
  python3 - "$INVENTORY_PATH" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as stream:
    inventory = json.load(stream)
for entry in inventory["released_archives"]:
    print(entry["target_triple"])
PY
}

expected_payload_files() {
  local tag="$1"
  local files=()
  local target
  while IFS= read -r target; do
    [[ -z "$target" ]] && continue
    local archive="transist-${tag}-${target}.tar.gz"
    files+=("$archive" "${archive}.sha256" "${archive}.minisig")
    files+=("build-provenance-${target}.json")
  done < <(inventory_targets)
  files+=(
    "packaging-provenance.json"
    "transist-${tag}-sbom.tar.gz"
    "transist-release.minisign.pub"
  )
  printf '%s\n' "${files[@]}" | sort
}

MANIFEST_NAME="release-manifest.txt"
MANIFEST_SIG_NAME="release-manifest.txt.minisig"

cmd_generate() {
  local tag="$1"
  local dist_dir="$2"
  local manifest_path="$dist_dir/$MANIFEST_NAME"

  if [[ ! -d "$dist_dir" ]]; then
    echo "FAIL: dist directory not found: $dist_dir" >&2
    return 1
  fi

  local sorted_files
  sorted_files="$(expected_payload_files "$tag")"

  : > "$manifest_path"
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if [[ ! -f "$dist_dir/$file" ]]; then
      echo "FAIL: expected payload file not found: $dist_dir/$file" >&2
      return 1
    fi
    local digest
    digest="$(cd "$dist_dir" && sha256_file "$file")"
    printf '%s  %s\n' "$digest" "$file" >> "$manifest_path"
  done <<< "$sorted_files"

  echo "$manifest_path"
}

cmd_verify() {
  local tag="$1"
  local dist_dir="$2"
  local manifest_path="$dist_dir/$MANIFEST_NAME"
  local failures=0

  if [[ ! -f "$manifest_path" ]]; then
    echo "FAIL: manifest not found: $manifest_path" >&2
    return 1
  fi
  # Extract filenames from manifest (field 2, shasum format: "<digest>  <filename>")
  local manifest_filenames
  manifest_filenames="$(awk '{print $2}' "$manifest_path")"

  # 1. Verify manifest is sorted by filename
  local sorted_filenames
  sorted_filenames="$(echo "$manifest_filenames" | sort)"
  if [[ "$manifest_filenames" != "$sorted_filenames" ]]; then
    echo "FAIL: manifest entries are not sorted by filename" >&2
    failures=$((failures + 1))
  else
    echo "PASS  manifest is sorted by filename"
  fi

  # 2. Verify expected entries are present
  local expected
  expected="$(expected_payload_files "$tag")"
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if ! echo "$manifest_filenames" | grep -qxF "$file"; then
      echo "FAIL: manifest missing expected entry: $file" >&2
      failures=$((failures + 1))
    fi
  done <<< "$expected"

  local expected_count
  expected_count="$(echo "$expected" | grep -c .)"

  # 3. Verify manifest excludes itself and its signature
  if echo "$manifest_filenames" | grep -qxF "$MANIFEST_NAME"; then
    echo "FAIL: manifest contains itself ($MANIFEST_NAME)" >&2
    failures=$((failures + 1))
  fi
  if echo "$manifest_filenames" | grep -qxF "$MANIFEST_SIG_NAME"; then
    echo "FAIL: manifest contains its signature ($MANIFEST_SIG_NAME)" >&2
    failures=$((failures + 1))
  fi

  # 4. Verify no unexpected extra entries
  local actual_count
  actual_count="$(echo "$manifest_filenames" | grep -c .)"
  if [[ "$actual_count" -ne "$expected_count" ]]; then
    echo "FAIL: manifest has $actual_count entries, expected $expected_count" >&2
    failures=$((failures + 1))
  fi

  # 5. Verify each entry digest matches the actual file
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    local digest filename
    digest="$(echo "$line" | awk '{print $1}')"
    filename="$(echo "$line" | awk '{print $2}')"
    if [[ ! -f "$dist_dir/$filename" ]]; then
      echo "FAIL: manifest entry file not found: $filename" >&2
      failures=$((failures + 1))
      continue
    fi
    local actual_digest
    actual_digest="$(cd "$dist_dir" && sha256_file "$filename")"
    if [[ "$digest" != "$actual_digest" ]]; then
      echo "FAIL: digest mismatch for $filename" >&2
      echo "  manifest:  $digest" >&2
      echo "  actual:    $actual_digest" >&2
      failures=$((failures + 1))
    fi
  done < "$manifest_path"

  if [[ "$failures" -eq 0 ]]; then
    echo "PASS  manifest verification passed ($actual_count entries)"
  else
    echo "FAIL  manifest verification failed ($failures error(s))" >&2
  fi
  return "$failures"
}

cmd_compare_draft() {
  local tag="$1"
  local dist_dir="$2"
  local manifest_path="$dist_dir/$MANIFEST_NAME"
  local failures=0

  if [[ ! -f "$manifest_path" ]]; then
    echo "FAIL: manifest not found: $manifest_path" >&2
    return 1
  fi
  if [[ ! -f "$dist_dir/$MANIFEST_SIG_NAME" ]]; then
    echo "FAIL: signed manifest is required for draft comparison: $dist_dir/$MANIFEST_SIG_NAME" >&2
    failures=$((failures + 1))
  fi

  # Collect manifest entries
  local manifest_filenames
  manifest_filenames="$(awk '{print $2}' "$manifest_path")"

  # Collect actual files in dist_dir (basename only, no subdirectories)
  local actual_files
  actual_files="$(find "$dist_dir" -maxdepth 1 -type f -exec basename {} \; | sort)"

  # Allowed non-manifest assets
  local allowed_extras="$MANIFEST_NAME $MANIFEST_SIG_NAME"

  # Check for missing assets (in manifest but not in dist_dir)
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if [[ ! -f "$dist_dir/$file" ]]; then
      echo "FAIL: manifest asset missing from draft: $file" >&2
      failures=$((failures + 1))
    fi
  done <<< "$manifest_filenames"

  # Check for extra assets (in dist_dir but not in manifest and not allowed)
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    if echo "$manifest_filenames" | grep -qxF "$file"; then
      continue
    fi
    if [[ " $allowed_extras " == *" $file "* ]]; then
      continue
    fi
    echo "FAIL: unexpected extra asset in draft: $file" >&2
    failures=$((failures + 1))
  done <<< "$actual_files"

  if [[ "$failures" -eq 0 ]]; then
    echo "PASS  draft assets match manifest exactly"
  else
    echo "FAIL  draft asset comparison failed ($failures error(s))" >&2
  fi
  return "$failures"
}

usage() {
  cat >&2 <<EOF
Usage:
  $0 generate <tag> <dist-dir>
  $0 verify <tag> <dist-dir>
  $0 compare-draft <tag> <dist-dir>
EOF
  exit 1
}

subcommand="${1:-}"
shift || true
case "$subcommand" in
  generate)   [[ "$#" -eq 2 ]] || usage; cmd_generate "$1" "$2" ;;
  verify)     [[ "$#" -eq 2 ]] || usage; cmd_verify "$1" "$2" ;;
  compare-draft) [[ "$#" -eq 2 ]] || usage; cmd_compare_draft "$1" "$2" ;;
  *)           usage ;;
esac
