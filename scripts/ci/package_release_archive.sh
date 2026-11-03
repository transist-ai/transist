#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0

set -euo pipefail

# package_release_archive.sh — Build the public release archive for Transist.
#
# Usage: package_release_archive.sh <tag> <cli-binary-path> <server-binary-path> <admin-binary-path> <target-triple> <packaging-provenance-path> [launcher-binary-path]
#
# [launcher-binary-path] is required exactly when the canonical target metadata
# declares `includes_sandbox_launcher`, and must otherwise be omitted. When
# provided, the launcher is packaged as
# bin/transist-sandbox-launcher (mode 0755).
#
# Archive layout (minimal evaluation bundle):
#   bin/transist, bin/transist-server, bin/transist-admin
#   bin/transist-sandbox-launcher   (Linux targets only)
#   plugins/shell/            (signed bundled shell plugin; plugin-local schemas
#                               are kept, README.md is not packaged)
#   plugins/shell.sig         (detached demo-only signature over plugins/shell)
#   examples/quickstart/      (offline trusted local-only quickstart fixture)
#   workflows/demos/getting_started/  (getting-started demos)
#   .agents/skills/           (complete Git-tracked ancillary authoring guidance)
#   README.md                 (distribution root README; the only packaged README)
#   LICENSE, NOTICE, THIRD_PARTY_NOTICES.md
#   build-provenance.json       (native target build environment)
#   packaging-provenance.json   (final archive packaging environment)
#
# No private key material, symlink, or special file is ever packaged.
#
# Packaging is deterministic: sorted paths, normalized ownership/modes/
# timestamps, and gzip -n to suppress embedded mtime/filename metadata.

if [[ "$#" -lt 6 || "$#" -gt 7 ]]; then
  echo "Usage: $0 <tag> <cli-binary-path> <server-binary-path> <admin-binary-path> <target-triple> <packaging-provenance-path> [launcher-binary-path]" >&2
  exit 1
fi

tag="$1"
cli_binary_path="$2"
server_binary_path="$3"
admin_binary_path="$4"
target_triple="$5"
packaging_provenance_path="$6"
launcher_binary_path="${7:-}"

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Release tag must match v<major>.<minor>.<patch>. Got: $tag" >&2
  exit 1
fi

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

dist_dir="${TRANSIST_DIST_DIR:-$repo_root/dist}"

inventory_path="$repo_root/scripts/ci/release-archive-inventory.json"
if [[ ! -f "$inventory_path" ]]; then
  echo "ERROR: missing canonical release inventory: $inventory_path" >&2
  exit 1
fi

target_field() {
  python3 "$repo_root/scripts/ci/release_inventory.py" target-field \
    "$inventory_path" "$target_triple" "$1"
}

if ! target_os="$(target_field os)"; then
  echo "Unsupported target triple: $target_triple" >&2
  exit 1
fi
includes_sandbox_launcher="$(target_field includes_sandbox_launcher)"

reject_symlinks() {
  local path="$1"
  local symlinks
  symlinks="$(find "$path" -type l -print 2>/dev/null || true)"
  if [[ -n "$symlinks" ]]; then
    echo "ERROR: symlinks are not allowed in release inputs under $path:" >&2
    printf '  %s\n' "$symlinks" >&2
    exit 1
  fi
}

# Distribution root README: the archive ships a release-specific README, not
# the repository README. It is authored under scripts/ci/ (source-side) and is
# the only Markdown README packaged in an archive.
distribution_readme="$repo_root/scripts/ci/distribution-README.md"
shell_plugin_manifest="$repo_root/plugins/shell/plugin.yaml"

for path in \
  "$cli_binary_path" \
  "$server_binary_path" \
  "$admin_binary_path" \
  "$packaging_provenance_path" \
  "$distribution_readme" \
  "$repo_root/LICENSE" \
  "$repo_root/NOTICE" \
  "$repo_root/THIRD_PARTY_NOTICES.md" \
  "$shell_plugin_manifest" \
  "$repo_root/plugins/shell.sig"; do
  if [[ ! -e "$path" || -L "$path" ]]; then
    echo "Required release input is missing: $path" >&2
    exit 1
  fi
done

# Derive the bundled plugin version from the manifest copied into the archive.
shell_plugin_version="$(python3 - "$shell_plugin_manifest" <<'PY'
import re
import sys

manifest_path = sys.argv[1]
with open(manifest_path, encoding="utf-8") as stream:
    manifest = stream.read()

matches = re.findall(r"^version:[ \t]*([^#\s]+)[ \t]*(?:#.*)?$", manifest, re.MULTILINE)
if len(matches) != 1 or re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", matches[0]) is None:
    raise SystemExit(f"ERROR: {manifest_path} must declare exactly one semantic version")
print(matches[0])
PY
)"
# The sandbox launcher contract is target metadata, not an inverse macOS test.
if [[ "$includes_sandbox_launcher" == true ]]; then
  if [[ -z "$launcher_binary_path" ]]; then
    echo "ERROR: launcher-binary-path is required for $target_os target $target_triple" >&2
    exit 1
  fi
  if [[ ! -f "$launcher_binary_path" || -L "$launcher_binary_path" ]]; then
    echo "Required release input is missing: $launcher_binary_path" >&2
    exit 1
  fi
elif [[ -n "$launcher_binary_path" ]]; then
  echo "ERROR: launcher-binary-path must not be passed for target $target_triple" >&2
  exit 1
fi

reject_symlinks "$repo_root/plugins/shell"
reject_symlinks "$repo_root/examples/quickstart"
reject_symlinks "$repo_root/workflows/demos/getting_started"
reject_symlinks "$repo_root/.agents/skills"

# Reject any private key material in content copied into the archive; only
# public roots and detached signatures may ship.
scan_for_private_keys() {
  local found=""
  local path by_name by_content
  for path in "$@"; do
    # Filename-based check.
    if [[ -d "$path" ]]; then
      by_name="$(find "$path" -type f \( -name '*.key' -o -name '*.pem' -o -name '*.p12' -o -name '*.pfx' -o -name '*.jks' -o -name '*.keystore' \) -print 2>/dev/null || true)"
    elif [[ "$(basename "$path")" == *.key || "$(basename "$path")" == *.pem || "$(basename "$path")" == *.p12 || "$(basename "$path")" == *.pfx || "$(basename "$path")" == *.jks || "$(basename "$path")" == *.keystore ]]; then
      by_name="$path"
    else
      by_name=""
    fi
    [[ -n "$by_name" ]] && found="$found$by_name\n"

    # Content-based check for PEM, JSON/JWK, YAML, and base64 credentials.
    if [[ -d "$path" ]]; then
      by_content="$(grep -rIlE --exclude='*.minisig' --exclude='*.pub' -- \
        '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----|(private_key|privateKey|client_secret|access_token|secret_key|credential)[^A-Za-z0-9]*[:=][^A-Za-z0-9]*[A-Za-z0-9+/]{40,}={0,2}|(^|[^A-Za-z0-9_])d[^A-Za-z0-9_]*:[^A-Za-z0-9]*[A-Za-z0-9_-]{32,}' \
        "$path" 2>/dev/null || true)"
    else
      by_content="$(grep -IlE -- \
        '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----|(private_key|privateKey|client_secret|access_token|secret_key|credential)[^A-Za-z0-9]*[:=][^A-Za-z0-9]*[A-Za-z0-9+/]{40,}={0,2}|(^|[^A-Za-z0-9_])d[^A-Za-z0-9_]*:[^A-Za-z0-9]*[A-Za-z0-9_-]{32,}' \
        "$path" 2>/dev/null || true)"
    fi
    [[ -n "$by_content" ]] && found="$found$by_content\n"
  done
  if [[ -n "$found" ]]; then
    printf '%b' "$found" | while IFS= read -r line; do
      [[ -n "$line" ]] && echo "  $line"
    done
    return 1
  fi
  return 0
}
if ! scan_for_private_keys \
  "$repo_root/plugins/shell" \
  "$repo_root/examples/quickstart" \
  "$repo_root/workflows/demos/getting_started" \
  "$repo_root/.agents/skills" \
  "$distribution_readme" \
  "$repo_root/LICENSE" \
  "$repo_root/NOTICE" \
  "$repo_root/THIRD_PARTY_NOTICES.md"; then
  echo "ERROR: private key material present in release inputs" >&2
  exit 1
fi

archive_name="transist-${tag}-${target_triple}.tar.gz"
checksum_name="${archive_name}.sha256"
archive_path="${dist_dir}/${archive_name}"
staging_root="$(mktemp -d)"
package_dir="${staging_root}/transist-${tag}-${target_triple}"

cleanup() {
  rm -rf "$staging_root"
}
trap cleanup EXIT

mkdir -p "$dist_dir" "$package_dir/bin" "$package_dir/plugins" "$package_dir/examples" "$package_dir/workflows/demos" "$package_dir/.agents/skills"

# ---- Binaries ----
cp "$cli_binary_path"    "$package_dir/bin/transist"
cp "$server_binary_path" "$package_dir/bin/transist-server"
cp "$admin_binary_path"  "$package_dir/bin/transist-admin"
# Linux-only: internal sandbox launcher for loopback network enforcement.
if [[ -n "$launcher_binary_path" ]]; then
  cp "$launcher_binary_path" "$package_dir/bin/transist-sandbox-launcher"
fi

# ---- Retained trees (recursively copied, README.md excluded) ----
# plugins/shell: the bundled shell plugin, including plugin-local schemas. The
# plugin-local README is source-only and never packaged.
mkdir -p "$package_dir/plugins/shell"
cp -R "$repo_root/plugins/shell/." "$package_dir/plugins/shell/"
find "$package_dir/plugins/shell" -name 'README.md' -type f -delete

# examples/quickstart: the offline trusted local-only quickstart fixture.
cp -R "$repo_root/examples/quickstart" "$package_dir/examples/quickstart"
find "$package_dir/examples/quickstart" -name 'README.md' -type f -delete

# workflows/demos/getting_started: the getting-started demo workflows.
cp -R "$repo_root/workflows/demos/getting_started" "$package_dir/workflows/demos/getting_started"
find "$package_dir/workflows/demos/getting_started" -name 'README.md' -type f -delete

# ---- Git-tracked ancillary skills (complete tree, including Markdown) ----
# A release-candidate dry run extracts the exact committed tree with
# `git archive`, which intentionally does not include a `.git` directory. In
# that immutable source tree every file is committed, so enumerate the skill
# tree directly. Ordinary packaging still uses Git so untracked local files
# can never enter a release archive.
list_tracked_skill_files() {
  if git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$repo_root" ls-files -z -- '.agents/skills/**'
  else
    (
      cd "$repo_root"
      find .agents/skills -type f -print0
    )
  fi
}

tracked_skill_count=0
while IFS= read -r -d '' rel; do
  source="$repo_root/$rel"
  destination="$package_dir/$rel"
  if [[ ! -f "$source" || -L "$source" ]]; then
    echo "ERROR: tracked skill source must be a regular non-symlink file: $rel" >&2
    exit 1
  fi
  mkdir -p "$(dirname "$destination")"
  cp "$source" "$destination"
  tracked_skill_count=$((tracked_skill_count + 1))
done < <(list_tracked_skill_files)
if [[ "$tracked_skill_count" -eq 0 ]]; then
  echo "ERROR: .agents/skills contains no Git-tracked files" >&2
  exit 1
fi

# ---- Plugin signature ----
cp "$repo_root/plugins/shell.sig" "$package_dir/plugins/shell.sig"

# ---- Legal & distribution README ----
cp "$repo_root/LICENSE" "$package_dir/LICENSE"
cp "$repo_root/NOTICE" "$package_dir/NOTICE"
cp "$repo_root/THIRD_PARTY_NOTICES.md" "$package_dir/THIRD_PARTY_NOTICES.md"
python3 - "$distribution_readme" "$package_dir/README.md" "$tag" "$shell_plugin_version" <<'PY'
import re
import sys

template_path, output_path, release_tag, shell_plugin_version = sys.argv[1:]
with open(template_path, encoding="utf-8") as stream:
    rendered = stream.read()

replacements = {
    "@TRANSIST_RELEASE_TAG@": release_tag,
    "@SHELL_PLUGIN_VERSION@": shell_plugin_version,
}
for placeholder, value in replacements.items():
    if placeholder not in rendered:
        raise SystemExit(f"ERROR: distribution README template is missing {placeholder}")
    rendered = rendered.replace(placeholder, value)

unresolved = sorted(set(re.findall(r"@[A-Z][A-Z0-9_]*@", rendered)))
if unresolved:
    raise SystemExit(
        "ERROR: distribution README contains unresolved placeholders: "
        + ", ".join(unresolved)
    )

with open(output_path, "w", encoding="utf-8", newline="\n") as stream:
    stream.write(rendered)
PY

# ---- Build provenance (required) ----
provenance_source=""
if [[ -f "$dist_dir/build-provenance-${target_triple}.json" ]]; then
  provenance_source="$dist_dir/build-provenance-${target_triple}.json"
elif [[ -f "$dist_dir/build-provenance.json" ]]; then
  provenance_source="$dist_dir/build-provenance.json"
fi
if [[ -z "$provenance_source" ]]; then
  echo "ERROR: build-provenance-${target_triple}.json (or build-provenance.json) is required in ${dist_dir}" >&2
  exit 1
fi
cp "$provenance_source" "$package_dir/build-provenance.json"
if [[ ! -f "$packaging_provenance_path" || -L "$packaging_provenance_path" ]]; then
  echo "Required packaging provenance is missing: $packaging_provenance_path" >&2
  exit 1
fi
cp "$packaging_provenance_path" "$package_dir/packaging-provenance.json"

# ---- Verify no packaged Markdown doc links to a path missing from the
# archive or escaping its root. A relative link is checked whenever both the
# linking file and the resolved target would be Markdown/text documentation;
# links to external URLs, mailto:, and in-page anchors are skipped. This
# catches a doc-referenced path that packaging forgot, or a link that would
# escape the shipped artifact. ----
check_doc_link_completeness() {
  local pkg="$1"
  python3 "$repo_root/scripts/ci/check_doc_links.py" "$pkg"
}
check_doc_link_completeness "$package_dir"

# ---- Retained-tree README exclusion ----
# Source READMEs beneath retained trees stay source-only. Complete tracked skill
# trees intentionally retain their Markdown and README files.
leaked_readmes="$(find "$package_dir" -name README.md -type f ! -path "$package_dir/README.md" ! -path "$package_dir/.agents/skills/*" -print 2>/dev/null || true)"
if [[ -n "$leaked_readmes" ]]; then
  echo "ERROR: retained-tree README files must not be packaged:" >&2
  printf '  %s\n' "$leaked_readmes" >&2
  exit 1
fi

# ---- Normalize ownership, modes, timestamps for deterministic output ----
normalize_tree() {
  local dir="$1"
  local pkg="$2"
  # Directories: 0755
  find "$dir" -type d -exec chmod 0755 {} +
  # Data files: 0644
  find "$dir" -type f ! -path "$pkg/bin/*" ! -path "$pkg/plugins/shell/plugin.py" -exec chmod 0644 {} +
  # Executable binaries: 0755
  chmod 0755 "$pkg/bin/transist" "$pkg/bin/transist-server" "$pkg/bin/transist-admin"
  # Linux-only sandbox launcher: 0755
  if [[ -f "$pkg/bin/transist-sandbox-launcher" ]]; then
    chmod 0755 "$pkg/bin/transist-sandbox-launcher"
  fi
  # Plugin entrypoint: 0755
  chmod 0755 "$pkg/plugins/shell/plugin.py"
  # Normalize timestamps to epoch (GNU tar --mtime also does this on Linux)
  if [[ "$(uname -s)" == "Linux" ]]; then
    find "$dir" -exec touch -m -d @0 {} +
  else
    find "$dir" -exec touch -t 197001010000.00 {} + 2>/dev/null || true
  fi
}

normalize_tree "$package_dir" "$package_dir"

# ---- Create deterministic archive ----
pkg_basename="$(basename "$package_dir")"
if [[ "$(uname -s)" == "Linux" ]]; then
  tar -C "$staging_root" \
    --format=ustar \
    --sort=name \
    --owner=0 --group=0 --numeric-owner \
    --mtime='@0' \
    -cf - "$pkg_basename" | \
    gzip -n -9 > "$archive_path"
else
  # BSD tar (macOS local testing only; CI packages on Linux with GNU tar).
  # Prevent copyfile from synthesizing AppleDouble `._*` archive members for
  # extended attributes that are not part of the canonical release inventory.
  (cd "$staging_root" && find "$pkg_basename" -print0 | sort -z | \
    COPYFILE_DISABLE=1 \
    tar --no-recursion --null --files-from=- \
      --uid 0 --gid 0 \
      -cf -) | \
    gzip -n -9 > "$archive_path"
fi

# ---- Checksum ----
(
  cd "$dist_dir"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$archive_name" > "$checksum_name"
  else
    shasum -a 256 "$archive_name" > "$checksum_name"
  fi
)

# ---- Output ----
emit_output() {
  local line="$1"
  printf '%s\n' "$line"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s\n' "$line" >> "$GITHUB_OUTPUT"
  fi
}

emit_output "target_triple=${target_triple}"
emit_output "archive_name=${archive_name}"
emit_output "checksum_name=${checksum_name}"
