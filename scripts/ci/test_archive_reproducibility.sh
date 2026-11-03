#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# test_archive_reproducibility.sh — Package identical inputs twice and compare
# SHA-256 digests, tar member lists, modes, and required paths.
#
# Usage: test_archive_reproducibility.sh <target-triple>
#
# The target triple is required and must match the actual host OS and
# architecture. The ABI may differ when the release workflow deliberately
# builds musl binaries with a target toolchain on a GNU host.
#
# All working state is written under an isolated temp directory
# (`TRANSIST_DIST_DIR`); this script never writes through the repository's
# own `dist/` directory.
#
# Exit status: 0 (all checks pass) or 1 (any check fails).

set -euo pipefail
export LC_ALL=C

if [[ "$#" -ne 1 ]]; then
  echo "Usage: $0 <target-triple>" >&2
  echo "  target-triple must be x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl," >&2
  echo "  or aarch64-apple-darwin, and must match the actual host running this script." >&2
  exit 1
fi

target_triple="$1"

case "$target_triple" in
  x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl) ;;
  *)
    echo "Unsupported target triple: $target_triple" >&2
    exit 1
    ;;
esac

host_os="$(uname -s)"
host_arch="$(uname -m)"
case "$target_triple" in
  aarch64-apple-darwin)
    if [[ "$host_os" != "Darwin" || "$host_arch" != "arm64" ]]; then
      echo "FAIL: target '$target_triple' requires host macOS/arm64, got ${host_os}/${host_arch}" >&2
      exit 1
    fi
    ;;
  x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl)
    if [[ "$host_os" != "Linux" || "$host_arch" != "x86_64" ]]; then
      echo "FAIL: target '$target_triple' requires host Linux/x86_64, got ${host_os}/${host_arch}" >&2
      exit 1
    fi
    ;;
  aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
    if [[ "$host_os" != "Linux" || "$host_arch" != "arm64" && "$host_arch" != "aarch64" ]]; then
      echo "FAIL: target '$target_triple' requires host Linux/ARM64, got ${host_os}/${host_arch}" >&2
      exit 1
    fi
    ;;
esac

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

failures=0
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

tag="v$(grep -m1 '^version' Cargo.toml | sed 's/^version *= *"//;s/"//')"
expected_commit="$(git rev-parse 'HEAD^{commit}')"
expected_tree="$(git rev-parse "$expected_commit^{tree}")"

# Linux release archives also require the sandbox launcher. Build into the
# conventional host directory when the requested target is the Rust host, or
# into Cargo's target-specific directory when the ABI requires --target (for
# example Arm64 musl on the native Arm64 GNU release runner).
rustc_host="$(rustc +1.88.0 -vV | awk '/^host:/ { print $2 }')"
release_dir="target/release"
if [[ "$target_triple" != "$rustc_host" ]]; then
  release_dir="target/$target_triple/release"
fi
release_packages=(-p transist-cli -p transist-server -p transist-admin)
release_binaries=("$release_dir/transist" "$release_dir/transist-server" "$release_dir/transist-admin")
if [[ "$target_triple" == *-linux-* ]]; then
  release_packages+=(-p transist-sandbox-launcher)
  release_binaries+=("$release_dir/transist-sandbox-launcher")
fi

# Build binaries if any required binary does not exist.
build_required=0
for binary in "${release_binaries[@]}"; do
  if [[ ! -x "$binary" ]]; then
    build_required=1
    break
  fi
done
if [[ "$build_required" -eq 1 ]]; then
  echo "--- Building release binaries ---"
  if [[ "$target_triple" == "$rustc_host" ]]; then
    cargo +1.88.0 build --locked --release "${release_packages[@]}"
  else
    cargo +1.88.0 build --locked --release --target "$target_triple" "${release_packages[@]}"
  fi
fi

# Prepare provenance
mkdir -p "$tmp_root/dist"
CI_RUNNER_IMAGE="test" bash scripts/ci/record_build_provenance.sh \
  "$tmp_root/dist/build-provenance-${target_triple}.json" \
  "$target_triple" \
  "${release_binaries[@]}"
cat > "$tmp_root/dist/packaging-provenance.json" <<EOF
{
  "kind": "final-packaging",
  "source": {"commit_sha": "$expected_commit", "tree_sha": "$expected_tree"},
  "packaging": {
    "runner_os": "Linux",
    "runner_arch": "x86_64",
    "runner_image": "test",
    "container_image": "test",
    "tar": "tar test",
    "gzip": "gzip test",
    "script": "package_release_archive.sh"
  }
}
EOF

echo ""
echo "--- Packaging run 1 ---"
package_run() {
  local run_dir="$1"
  local out_dir="$2"
  local package_args=(
    "$tag"
    "${release_binaries[0]}"
    "${release_binaries[1]}"
    "${release_binaries[2]}"
    "$target_triple"
    "$tmp_root/dist/packaging-provenance.json"
  )
  rm -rf "$run_dir"
  mkdir -p "$run_dir/dist"
  cp "$tmp_root/dist/build-provenance-${target_triple}.json" "$run_dir/dist/"
  if [[ "$target_triple" == *-linux-* ]]; then
    package_args+=("${release_binaries[3]}")
  fi
  TRANSIST_DIST_DIR="$run_dir/dist" bash scripts/ci/package_release_archive.sh \
    "${package_args[@]}"
  cp "$run_dir/dist/transist-${tag}-${target_triple}.tar.gz" "$out_dir/archive1.tar.gz"
  cp "$run_dir/dist/transist-${tag}-${target_triple}.tar.gz.sha256" "$out_dir/archive1.sha256"
  bash scripts/ci/check_release_archive.sh "$target_triple" "$tag" \
    --expected-commit "$expected_commit" --expected-tree "$expected_tree" \
    --allow-unsigned "$run_dir/dist/transist-${tag}-${target_triple}.tar.gz" \
    > "$out_dir/archive-check.log"
}

mkdir -p "$tmp_root/out1" "$tmp_root/out2"

package_run "$tmp_root/run1" "$tmp_root/out1"
echo "  PASS  first package produced"
echo "  PASS  unsigned package content and provenance checks passed"

echo "--- Packaging run 2 ---"
package_run "$tmp_root/run2" "$tmp_root/out2"
echo "  PASS  second package produced"

echo ""
echo "--- Digest comparison ---"
sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | cut -d' ' -f1
  else
    shasum -a 256 "$path" | cut -d' ' -f1
  fi
}
digest1="$(sha256_file "$tmp_root/out1/archive1.tar.gz")"
digest2="$(sha256_file "$tmp_root/out2/archive1.tar.gz")"
echo "  digest1: $digest1"
echo "  digest2: $digest2"
if [[ "$digest1" == "$digest2" ]]; then
  echo "  PASS  digests match"
else
  echo "  FAIL  digests differ"
  failures=$((failures + 1))
fi

echo ""
echo "--- Tar member comparison ---"
member1=$(tar -tzf "$tmp_root/out1/archive1.tar.gz" | sort)
member2=$(tar -tzf "$tmp_root/out2/archive1.tar.gz" | sort)
if [[ "$member1" == "$member2" ]]; then
  echo "  PASS  tar members identical"
else
  echo "  FAIL  tar members differ"
  diff <(echo "$member1") <(echo "$member2") || true
  failures=$((failures + 1))
fi

echo ""
echo "--- Exact inventory member check ---"
member_file="$tmp_root/members.txt"
tar -tzf "$tmp_root/out1/archive1.tar.gz" | sed 's|/$||' | sort -u > "$member_file"
python3 scripts/ci/release_inventory.py \
  scripts/ci/release-archive-inventory.json "$repo_root" \
  "transist-${tag}-${target_triple}" "$target_triple" \
  > "$tmp_root/expected-members.txt"
if diff -u "$tmp_root/expected-members.txt" "$member_file"; then
  echo "  PASS  archive member set matches canonical inventory"
else
  echo "  FAIL  archive member set differs from canonical inventory"
  failures=$((failures + 1))
fi

echo ""
echo "--- Tracked skill tree check ---"
skill_members="$tmp_root/archive-skill-members.txt"
git ls-files -- '.agents/skills/**' | sort > "$tmp_root/expected-skill-members.txt"
tar -tzf "$tmp_root/out1/archive1.tar.gz" \
  | sed "s|^transist-${tag}-${target_triple}/||" \
  | grep '^\.agents/skills/' \
  | grep -v '/$' \
  | sort -u > "$skill_members"
if diff -u "$tmp_root/expected-skill-members.txt" "$skill_members"; then
  echo "  PASS  archive skill paths match Git-tracked paths"
else
  echo "  FAIL  archive skill paths differ from Git-tracked paths"
  failures=$((failures + 1))
fi

echo ""
echo "--- File mode check ---"
extract_dir="$tmp_root/extract"
mkdir -p "$extract_dir"
tar -xzf "$tmp_root/out1/archive1.tar.gz" -C "$extract_dir"
pkg_dir="$extract_dir/transist-${tag}-${target_triple}"

check_mode() {
  local path="$1"
  local expected="$2"
  local actual
  if [[ "$(uname -s)" == "Darwin" ]]; then
    actual=$(stat -f "%Lp" "$pkg_dir/$path")
  else
    actual=$(stat -c "%a" "$pkg_dir/$path")
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
if [[ "$target_triple" == *-linux-* ]]; then
  check_mode "bin/transist-sandbox-launcher" "755"
fi
check_mode "plugins/shell/plugin.py" "755"
check_mode "plugins/shell/plugin.yaml" "644"
check_mode "LICENSE" "644"
check_mode "build-provenance.json" "644"
check_mode "packaging-provenance.json" "644"
while IFS= read -r rel; do
  [[ -z "$rel" ]] && continue
  if cmp -s "$rel" "$pkg_dir/$rel"; then
    echo "  PASS  $rel bytes match source"
  else
    echo "  FAIL  $rel bytes differ from source"
    failures=$((failures + 1))
  fi
  check_mode "$rel" "644"
done < "$tmp_root/expected-skill-members.txt"
while IFS= read -r dir; do
  relative="${dir#"$pkg_dir/"}"
  check_mode "$relative" "755"
done < <(find "$pkg_dir/.agents/skills" -type d -print)

echo ""
echo "--- Distribution README metadata rejection test ---"
readme_mismatch_root="$tmp_root/readme-mismatch"
readme_mismatch_pkg="$(basename "$pkg_dir")"
mkdir -p "$readme_mismatch_root"
cp -R "$pkg_dir" "$readme_mismatch_root/$readme_mismatch_pkg"
python3 - "$readme_mismatch_root/$readme_mismatch_pkg/README.md" <<'PY'
import re
import sys

readme_path = sys.argv[1]
with open(readme_path, encoding="utf-8") as stream:
    readme = stream.read()
mutated, substitutions = re.subn(
    r"(The bundled `shell` plugin \(`)[^`]+(`,)",
    r"\g<1>9.9.9\2",
    readme,
    count=1,
)
if substitutions != 1:
    raise SystemExit("failed to create distribution README mismatch fixture")
with open(readme_path, "w", encoding="utf-8", newline="\n") as stream:
    stream.write(mutated)
PY
readme_mismatch_archive="$readme_mismatch_root/$readme_mismatch_pkg.tar.gz"
if [[ "$(uname -s)" == "Linux" ]]; then
  tar -C "$readme_mismatch_root" \
    --format=ustar \
    --sort=name \
    --owner=0 --group=0 --numeric-owner \
    --mtime='@0' \
    -cf - "$readme_mismatch_pkg" | \
    gzip -n -9 > "$readme_mismatch_archive"
else
  (
    cd "$readme_mismatch_root"
    find "$readme_mismatch_pkg" -print0 | sort -z | \
      COPYFILE_DISABLE=1 \
      tar --no-recursion --null --files-from=- --uid 0 --gid 0 -cf -
  ) | gzip -n -9 > "$readme_mismatch_archive"
fi
readme_mismatch_digest="$(sha256_file "$readme_mismatch_archive")"
printf '%s  %s\n' "$readme_mismatch_digest" "$(basename "$readme_mismatch_archive")" \
  > "$readme_mismatch_archive.sha256"
readme_mismatch_status=0
readme_mismatch_output="$(bash scripts/ci/check_release_archive.sh \
  "$target_triple" "$tag" \
  --expected-commit "$expected_commit" --expected-tree "$expected_tree" \
  --allow-unsigned "$readme_mismatch_archive" 2>&1)" || readme_mismatch_status=$?
if [[ "$readme_mismatch_status" -ne 0 ]] \
    && [[ "$readme_mismatch_output" == *"README.md must document bundled shell plugin version"* ]]; then
  echo "  PASS  archive validation rejects stale bundled plugin metadata"
else
  echo "  FAIL  archive validation accepted stale bundled plugin metadata"
  printf '%s\n' "$readme_mismatch_output" >&2
  failures=$((failures + 1))
fi

echo ""
echo "--- Provenance rejection test ---"
# Packaging without provenance should fail
prov_reject_dir="$tmp_root/provenance-reject/dist"
mkdir -p "$prov_reject_dir"
provenance_reject_args=(
  "$tag"
  "target/release/transist"
  "target/release/transist-server"
  "target/release/transist-admin"
  "$target_triple"
  "$tmp_root/dist/packaging-provenance.json"
)
if [[ "$target_triple" == *-linux-* ]]; then
  provenance_reject_args+=("target/release/transist-sandbox-launcher")
fi
if TRANSIST_DIST_DIR="$prov_reject_dir" bash scripts/ci/package_release_archive.sh \
      "${provenance_reject_args[@]}" 2>/dev/null; then
  echo "  FAIL  packaging succeeded without provenance"
  failures=$((failures + 1))
else
  echo "  PASS  packaging fails without provenance"
fi

echo ""
echo "--- Bad tag rejection test ---"
bad_tag_dir="$tmp_root/bad-tag/dist"
mkdir -p "$bad_tag_dir"
cp "$tmp_root/dist/build-provenance-${target_triple}.json" "$bad_tag_dir/"
bad_tag_args=(
  "v;echo bad"
  "target/release/transist"
  "target/release/transist-server"
  "target/release/transist-admin"
  "$target_triple"
  "$tmp_root/dist/packaging-provenance.json"
)
if [[ "$target_triple" != "aarch64-apple-darwin" ]]; then
  bad_tag_args+=("target/release/transist-sandbox-launcher")
fi
if TRANSIST_DIST_DIR="$bad_tag_dir" bash scripts/ci/package_release_archive.sh \
      "${bad_tag_args[@]}" 2>/dev/null; then
  echo "  FAIL  packaging accepted bad tag"
  failures=$((failures + 1))
else
  echo "  PASS  packaging rejects bad tag"
fi

echo ""
echo "============================================================"
if [[ "$failures" -eq 0 ]]; then
  echo "  RESULT: All reproducibility checks passed"
else
  echo "  RESULT: $failures reproducibility check(s) failed"
fi
echo "============================================================"
exit "$failures"
