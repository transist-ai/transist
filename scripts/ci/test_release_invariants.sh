#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# test_release_invariants.sh — Release-workflow invariant tests.
#
# Tests that the release manifest is:
#   - sorted alphabetically by filename
#   - contains exactly the expected payload entries
#   - excludes itself and its signature
#   - verifies each entry digest against the actual file
#   - rejects tampered, missing, and extra assets
#   - preserves the workflow invariants that keep staging draft-only
#
# Uses mock files and fixture-backed GitHub API responses in temporary test
# state; no real archives or signatures are needed, and no remote is mutated.
#
# Exit status: 0 (all tests pass) or 1 (any test fails).

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

failures=0
test_dir="$(mktemp -d)"
candidate_worktree="$test_dir/release-candidate-worktree"
trap 'git -C "$repo_root" worktree remove --force "$candidate_worktree" >/dev/null 2>&1 || true; rm -rf "$test_dir"' EXIT

record_fail() {
  echo "FAIL  $1" >&2
  failures=$((failures + 1))
}

record_pass() {
  echo "PASS  $1"
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | cut -d' ' -f1
  else
    shasum -a 256 "$path" | cut -d' ' -f1
  fi
}

TAG="v$(grep -m1 '^version' Cargo.toml | sed 's/^version *= *"//;s/"//')"
EXPECTED_COMMIT="$(git rev-parse 'HEAD^{commit}')"
EXPECTED_TREE="$(git rev-parse "$EXPECTED_COMMIT^{tree}")"
DIST="$test_dir/dist"
mkdir -p "$DIST"

# Create mock payload files for all supported targets.
create_mock_payloads() {
  local dist="$1"
  local tag="$2"
  for target in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu aarch64-unknown-linux-musl; do
    local archive="transist-${tag}-${target}.tar.gz"
    printf 'mock-archive-%s\n' "$target" > "$dist/$archive"
    printf '%s-checksum\n' "$target" > "$dist/${archive}.sha256"
    # Write real checksum for the archive
    local digest
    digest="$(sha256_file "$dist/$archive")"
    printf '%s  %s\n' "$digest" "$archive" > "$dist/${archive}.sha256"
    printf 'mock-sig-%s\n' "$target" > "$dist/${archive}.minisig"
  done
  for target in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu aarch64-unknown-linux-musl; do
    printf 'mock-provenance-%s\n' "$target" > "$dist/build-provenance-${target}.json"
  done
  printf 'mock-packaging-provenance\n' > "$dist/packaging-provenance.json"
  printf 'mock-sbom\n' > "$dist/transist-${tag}-sbom.tar.gz"
  printf 'untrusted comment: minisign public key TESTKEYID00000000\nRWRRCHIGibbOBcBmXVUsq4rEQOks9JISOcPXOe9Nxo9HB1wS4DObOVlF\n' \
    > "$dist/transist-release.minisign.pub"
}

# ---------------------------------------------------------------------------
echo "--- Test 1: Manifest generation produces sorted output ---"
create_mock_payloads "$DIST" "$TAG"
manifest_path="$(bash scripts/ci/release_manifest.sh generate "$TAG" "$DIST")"
if [[ ! -f "$manifest_path" ]]; then
  record_fail "manifest was not generated"
else
  # Verify sorted by filename (field 2)
  manifest_filenames="$(awk '{print $2}' "$manifest_path")"
  sorted_filenames="$(echo "$manifest_filenames" | sort)"
  if [[ "$manifest_filenames" == "$sorted_filenames" ]]; then
    record_pass "manifest is sorted by filename"
  else
    record_fail "manifest is not sorted by filename"
    echo "  expected:" >&2
    echo "$sorted_filenames" | awk '{print "    " $0}' >&2
    echo "  actual:" >&2
    echo "$manifest_filenames" | awk '{print "    " $0}' >&2
  fi
fi

# ---------------------------------------------------------------------------
echo "--- Test 2: Manifest contains all expected entries ---"
expected_count=27  # 3 assets + one provenance per target, plus packaging, SBOM, and key
actual_count="$(awk '{print $2}' "$manifest_path" | grep -c .)"
if [[ "$actual_count" -eq "$expected_count" ]]; then
  record_pass "manifest has $expected_count entries"
else
  record_fail "manifest has $actual_count entries, expected $expected_count"
fi

for target in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu aarch64-unknown-linux-musl; do
  archive="transist-${TAG}-${target}.tar.gz"
  for suffix in "" ".sha256" ".minisig"; do
    file="${archive}${suffix}"
    if grep -qxF "$file" <(awk '{print $2}' "$manifest_path"); then
      : # pass
    else
      record_fail "manifest missing expected entry: $file"
    fi
  done
done
for file in \
  "build-provenance-x86_64-unknown-linux-gnu.json" \
  "build-provenance-x86_64-unknown-linux-musl.json" \
  "build-provenance-aarch64-apple-darwin.json" \
  "build-provenance-x86_64-apple-darwin.json" \
  "build-provenance-aarch64-unknown-linux-gnu.json" \
  "build-provenance-aarch64-unknown-linux-musl.json" \
  "packaging-provenance.json" \
  "transist-${TAG}-sbom.tar.gz" \
  "transist-release.minisign.pub"; do
  if grep -qxF "$file" <(awk '{print $2}' "$manifest_path"); then
    : # pass
  else
    record_fail "manifest missing expected entry: $file"
  fi
done

# ---------------------------------------------------------------------------
echo "--- Test 3: Manifest excludes itself and its signature ---"
manifest_names="$(awk '{print $2}' "$manifest_path")"
if echo "$manifest_names" | grep -qxF "release-manifest.txt"; then
  record_fail "manifest contains itself"
else
  record_pass "manifest excludes itself"
fi
if echo "$manifest_names" | grep -qxF "release-manifest.txt.minisig"; then
  record_fail "manifest contains its signature"
else
  record_pass "manifest excludes its signature"
fi

# ---------------------------------------------------------------------------
echo "--- Test 4: Manifest verification passes on valid manifest ---"
if bash scripts/ci/release_manifest.sh verify "$TAG" "$DIST" > /dev/null 2>&1; then
  record_pass "manifest verification passes on valid manifest"
else
  record_fail "manifest verification failed on valid manifest"
fi

# ---------------------------------------------------------------------------
echo "--- Test 5: Manifest verification rejects tampered digest ---"
tampered_dist="$test_dir/tampered"
mkdir -p "$tampered_dist"
cp "$DIST"/* "$tampered_dist/"
# Tamper with an archive but keep the manifest's original digest
printf 'tampered-content\n' > "$tampered_dist/transist-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
if bash scripts/ci/release_manifest.sh verify "$TAG" "$tampered_dist" > /dev/null 2>&1; then
  record_fail "manifest verification accepted tampered archive digest"
else
  record_pass "manifest verification rejects tampered archive digest"
fi

# ---------------------------------------------------------------------------
echo "--- Test 6: Manifest verification rejects missing asset ---"
missing_dist="$test_dir/missing"
mkdir -p "$missing_dist"
cp "$DIST"/* "$missing_dist/"
rm "$missing_dist/transist-${TAG}-aarch64-apple-darwin.tar.gz"
if bash scripts/ci/release_manifest.sh verify "$TAG" "$missing_dist" > /dev/null 2>&1; then
  record_fail "manifest verification accepted missing asset"
else
  record_pass "manifest verification rejects missing asset"
fi

# ---------------------------------------------------------------------------
echo "--- Test 7: Manifest generation rejects missing payload file ---"
genfail_dist="$test_dir/genfail"
mkdir -p "$genfail_dist"
cp "$DIST"/* "$genfail_dist/"
rm "$genfail_dist/transist-${TAG}-sbom.tar.gz"
if bash scripts/ci/release_manifest.sh generate "$TAG" "$genfail_dist" > /dev/null 2>&1; then
  record_fail "manifest generation succeeded with missing payload file"
else
  record_pass "manifest generation rejects missing payload file"
fi

# ---------------------------------------------------------------------------
echo "--- Test 8: Draft comparison passes on exact match ---"
# Restore manifest + signature as allowed extras
compare_dist="$test_dir/compare"
mkdir -p "$compare_dist"
cp "$DIST"/* "$compare_dist/"
cp "$manifest_path" "$compare_dist/release-manifest.txt"
printf 'mock-manifest-sig\n' > "$compare_dist/release-manifest.txt.minisig"
if bash scripts/ci/release_manifest.sh compare-draft "$TAG" "$compare_dist" > /dev/null 2>&1; then
  record_pass "draft comparison passes on exact match"
else
  record_fail "draft comparison failed on exact match"
fi

# ---------------------------------------------------------------------------
echo "--- Test 9: Draft comparison rejects extra asset ---"
extra_dist="$test_dir/extra"
mkdir -p "$extra_dist"
cp "$compare_dist"/* "$extra_dist/"
printf 'unexpected-file\n' > "$extra_dist/unexpected-asset.txt"
if bash scripts/ci/release_manifest.sh compare-draft "$TAG" "$extra_dist" > /dev/null 2>&1; then
  record_fail "draft comparison accepted extra asset"
else
  record_pass "draft comparison rejects extra asset"
fi

# ---------------------------------------------------------------------------
echo "--- Test 10: Draft comparison rejects missing manifest asset ---"
draft_missing_dist="$test_dir/draft_missing"
mkdir -p "$draft_missing_dist"
cp "$compare_dist"/* "$draft_missing_dist/"
rm "$draft_missing_dist/transist-${TAG}-x86_64-unknown-linux-gnu.tar.gz.minisig"
if bash scripts/ci/release_manifest.sh compare-draft "$TAG" "$draft_missing_dist" > /dev/null 2>&1; then
  record_fail "draft comparison accepted missing manifest asset"
else
  record_pass "draft comparison rejects missing manifest asset"
fi

# ---------------------------------------------------------------------------
echo "--- Test 11: check_release_candidate.sh --dry-run validates tag/version ---"
# The candidate guard correctly rejects ignored local files that could alter
# release inputs. Exercise a detached checkout of the exact candidate instead
# of depending on the developer's primary worktree being free of unrelated
# ignored directories.
if ! git worktree add --detach --quiet "$candidate_worktree" "$EXPECTED_COMMIT"; then
  record_fail "could not create a clean worktree for release-candidate validation"
  dry_run_status=1
  dry_run_output=""
else
  # Capture the dry-run output so a CI-only failure shows its root cause (the
  # dry-run is a full release build; it is only echoed on failure, not on success).
  dry_run_status=0
  dry_run_output="$(
    cd "$candidate_worktree"
    bash scripts/ci/check_release_candidate.sh --dry-run "$TAG" --sha HEAD 2>&1
  )" || dry_run_status=$?
  git -C "$repo_root" worktree remove --force "$candidate_worktree"
fi
if [[ "$dry_run_status" -eq 0 ]]; then
  record_pass "check_release_candidate.sh --dry-run accepted valid tag"
else
  record_fail "check_release_candidate.sh --dry-run failed (tag validation, build, or packaging did not pass)"
  printf '%s\n' "$dry_run_output" | sed 's/^/    /' >&2
fi

# ---------------------------------------------------------------------------
echo "--- Test 12: check_release_candidate.sh rejects wrong tag version ---"
if bash scripts/ci/check_release_candidate.sh --dry-run "v99.99.99" --sha HEAD > /dev/null 2>&1; then
  record_fail "check_release_candidate.sh accepted wrong tag version"
else
  record_pass "check_release_candidate.sh rejects wrong tag version"
fi

# ---------------------------------------------------------------------------
echo "--- Test 12b: check_release_candidate.sh rejects unsupported archive hosts in both modes ---"
fake_rustc_dir="$test_dir/unsupported-host-bin"
mkdir -p "$fake_rustc_dir"
fake_rustc="$fake_rustc_dir/rustc"
printf '#!/usr/bin/env bash\nprintf "host: wasm32-unknown-unknown\\n"\n' > "$fake_rustc"
chmod +x "$fake_rustc"
for mode in dry-run non-dry-run; do
  unsupported_host_args=("$TAG" --sha HEAD)
  if [[ "$mode" == "dry-run" ]]; then
    unsupported_host_args=(--dry-run "${unsupported_host_args[@]}")
  fi
  unsupported_host_status=0
  unsupported_host_output="$(PATH="$fake_rustc_dir:$PATH" \
    bash scripts/ci/check_release_candidate.sh "${unsupported_host_args[@]}" 2>&1)" \
    || unsupported_host_status=$?
  if [[ "$unsupported_host_status" -ne 0 ]] \
    && [[ "$unsupported_host_output" == *"not a supported release archive host"* ]] \
    && [[ "$unsupported_host_output" != *"Tag and version validation"* ]] \
    && [[ "$unsupported_host_output" != *"Materializing committed SHA"* ]]; then
    record_pass "check_release_candidate.sh fails before validation in $mode mode on an unsupported archive host"
  else
    record_fail "check_release_candidate.sh did not fail closed in $mode mode on an unsupported archive host"
    printf '%s\n' "$unsupported_host_output" >&2
  fi
done

# ---------------------------------------------------------------------------
echo "--- Test 12c: Gitleaks historical exception is finding-scoped ---"
gitleaks_config=".gitleaks.toml"
gitleaks_ignore=".gitleaksignore"
historical_fingerprint_one="c0a1a37d23eacbd2f9b13f165f9d863e51788b37:crates/transist-core/src/infra/staging.rs:generic-api-key:384"
historical_fingerprint_two="f6e034c32ee5d678fd575b767afbe17d48fc2739:crates/transist-core/src/infra/staging.rs:generic-api-key:384"
expected_gitleaks_ignore="$(printf '%s\n%s' "$historical_fingerprint_one" "$historical_fingerprint_two")"
actual_gitleaks_ignore="$(<"$gitleaks_ignore")"
if [[ "$actual_gitleaks_ignore" == "$expected_gitleaks_ignore" ]] \
  && ! grep -Eq '^\[\[allowlists\]\]|^commits\s*=|^paths\s*=|^regexes\s*=' "$gitleaks_config"; then
  record_pass "Gitleaks historical exception is limited to exact findings"
else
  record_fail "Gitleaks historical exception can suppress more than the fixture findings"
fi

# ---------------------------------------------------------------------------
echo "--- Test 13: a clean SHA B checkout cannot claim SHA A ---"
candidate_clone="$test_dir/candidate-sha-binding"
git clone --quiet --no-hardlinks "$repo_root" "$candidate_clone"
cp scripts/ci/check_release_candidate.sh "$candidate_clone/scripts/ci/check_release_candidate.sh"
git -C "$candidate_clone" config user.email "test@example.com"
git -C "$candidate_clone" config user.name "Candidate SHA Test"
candidate_sha_a="$(git -C "$candidate_clone" rev-parse HEAD)"
candidate_tag="v$(grep -m1 '^version' "$candidate_clone/Cargo.toml" | sed 's/^version *= *"//;s/"//')"
printf '%s\n' "candidate SHA B mutation" > "$candidate_clone/sha-binding-regression.txt"
git -C "$candidate_clone" add sha-binding-regression.txt
git -C "$candidate_clone" commit -q -m "candidate SHA B"
candidate_sha_b="$(git -C "$candidate_clone" rev-parse HEAD)"
candidate_status=0
candidate_output="$(
  cd "$candidate_clone"
  bash scripts/ci/check_release_candidate.sh --dry-run "$candidate_tag" --sha "$candidate_sha_a" 2>&1
)" || candidate_status=$?
if [[ "$candidate_status" -ne 0 ]] \
  && [[ "$candidate_output" == *"does not match --sha candidate"* ]] \
  && [[ "$candidate_output" != *"Materializing committed SHA"* ]] \
  && [[ "$candidate_sha_a" != "$candidate_sha_b" ]]; then
  record_pass "clean SHA B checkout cannot claim or build SHA A"
else
  record_fail "check_release_candidate.sh allowed SHA B checkout to claim SHA A"
  printf '%s\n' "$candidate_output" >&2
fi

# ---------------------------------------------------------------------------
echo "--- Test 14: checksum sidecar follows an archive outside dist ---"
outside_dir="$test_dir/outside-archive"
mkdir -p "$outside_dir/payload"
printf 'release-payload\n' > "$outside_dir/payload/README"
archive_path="$outside_dir/transist-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
tar -C "$outside_dir" -czf "$archive_path" payload
digest="$(sha256_file "$archive_path")"
printf '%s  %s\n' "$digest" "$(basename "$archive_path")" > "${archive_path}.sha256"
checksum_output="$(bash scripts/ci/check_release_archive.sh x86_64-unknown-linux-gnu "${TAG#v}" \
  --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" "$archive_path" 2>&1 || true)"
if echo "$checksum_output" | grep -q 'PASS  checksum matches'; then
  record_pass "checksum sidecar is resolved from the archive directory"
else
  record_fail "checksum sidecar was not resolved from the archive directory"
fi

# ---------------------------------------------------------------------------
echo "--- Test 15: archive inspection rejects symlinks before extraction ---"
unsafe_archive_dir="$test_dir/unsafe-archive"
mkdir -p "$unsafe_archive_dir/transist-${TAG}-x86_64-unknown-linux-gnu"
printf 'safe\n' > "$unsafe_archive_dir/transist-${TAG}-x86_64-unknown-linux-gnu/README.md"
ln -s README.md "$unsafe_archive_dir/transist-${TAG}-x86_64-unknown-linux-gnu/README-link"
unsafe_archive="$unsafe_archive_dir/transist-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
COPYFILE_DISABLE=1 tar -C "$unsafe_archive_dir" -czf "$unsafe_archive" \
  transist-${TAG}-x86_64-unknown-linux-gnu
unsafe_digest="$(sha256_file "$unsafe_archive")"
printf '%s  %s\n' "$unsafe_digest" "$(basename "$unsafe_archive")" > "$unsafe_archive.sha256"
unsafe_status=0
unsafe_output="$(bash scripts/ci/check_release_archive.sh \
  x86_64-unknown-linux-gnu "${TAG#v}" \
  --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" \
  "$unsafe_archive" 2>&1)" || unsafe_status=$?
if [[ "$unsafe_status" -ne 0 ]] \
  && echo "$unsafe_output" | grep -q 'not a regular file or directory'; then
  record_pass "archive inspection rejects symlinks before extraction"
else
  record_fail "archive inspection accepted a symlink"
  printf '%s\n' "$unsafe_output" >&2
fi

# ---------------------------------------------------------------------------
echo "--- Test 16: release tag input cannot become shell syntax ---"
marker="$test_dir/tag-injection-marker"
printf 'packaging-provenance\n' > "$test_dir/packaging-provenance.json"
if bash scripts/ci/package_release_archive.sh "v\$(touch $marker)" \
     /missing/transist /missing/transist-server /missing/transist-admin \
     x86_64-unknown-linux-gnu "$test_dir/packaging-provenance.json" > /dev/null 2>&1; then
  record_fail "package script accepted shell syntax in release tag"
elif [[ -e "$marker" ]]; then
  record_fail "release tag caused command substitution"
else
  record_pass "package script rejects shell syntax in release tag"
fi

# ---------------------------------------------------------------------------
echo "--- Test 17: provenance records commit, tree, target, and binaries ---"
provenance_path="$test_dir/build-provenance.json"
if bash scripts/ci/record_build_provenance.sh "$provenance_path" > /dev/null 2>&1 \
  && jq -e --arg commit "$(git rev-parse HEAD)" \
       --arg tree "$(git rev-parse 'HEAD^{tree}')" \
       '.source.commit_sha == $commit and .source.tree_sha == $tree and (.target | length) > 0' \
       "$provenance_path" > /dev/null; then
  record_pass "provenance records the source commit and tree"
  if jq -e '.binaries | type == "array"' "$provenance_path" > /dev/null; then
    record_pass "provenance records a binaries digest array"
  else
    record_fail "provenance is missing the binaries digest array"
  fi
else
  record_fail "provenance does not bind the source commit and tree"
fi

# ---------------------------------------------------------------------------
echo "--- Test 18: Draft comparison rejects an unsigned manifest ---"
draft_missing_manifest_sig="$test_dir/draft_missing_manifest_sig"
mkdir -p "$draft_missing_manifest_sig"
cp "$compare_dist"/* "$draft_missing_manifest_sig/"
rm "$draft_missing_manifest_sig/release-manifest.txt.minisig"
if bash scripts/ci/release_manifest.sh compare-draft "$TAG" "$draft_missing_manifest_sig" > /dev/null 2>&1; then
  record_fail "draft comparison accepted a missing manifest signature"
else
  record_pass "draft comparison rejects a missing manifest signature"
fi

# ---------------------------------------------------------------------------
# Build a deterministic, canonical-inventory-compliant unsigned archive so the
# signature-enforcement tests (18/19/19b) reach the signature stage. The
# member set is materialized from release_inventory.py, so this fixture stays
# in sync with the inventory without shipping a redundant static tree. The
# signature always fails first, so placeholder regular files are sufficient:
# the binary smoke/provenance checks are never reached.
build_unsigned_archive() {
  local target="x86_64-unknown-linux-gnu"
  local pkg="transist-${TAG}-x86_64-unknown-linux-gnu"
  local packaging_commit="${1:-$EXPECTED_COMMIT}"
  local packaging_tree="${2:-$EXPECTED_TREE}"
  local staging="$test_dir/unsigned-tree"
  rm -rf "$staging"
  mkdir -p "$staging/$pkg"
  local members=()
  while IFS= read -r member; do
    [[ -z "$member" ]] && continue
    [[ "$member" != "$pkg"/* ]] && continue
    members+=("${member#"$pkg"/}")
  done < <(python3 scripts/ci/release_inventory.py \
    scripts/ci/release-archive-inventory.json . "$pkg" "$target")

  local m rel full is_dir
  for m in "${members[@]}"; do
    is_dir=0
    for rel in "${members[@]}"; do
      if [[ "$rel" == "$m/"* ]]; then
        is_dir=1
        break
      fi
    done
    full="$staging/$pkg/$m"
    mkdir -p "$(dirname "$full")"
    if [[ "$is_dir" -eq 1 ]]; then
      mkdir -p "$full"
    else
      printf 'placeholder\n' > "$full"
    fi
  done
  cat > "$staging/$pkg/packaging-provenance.json" <<EOF
{
  "kind": "final-packaging",
  "source": {"commit_sha": "$packaging_commit", "tree_sha": "$packaging_tree"},
  "packaging": {"container_image": "test"}
}
EOF

  unsigned_archive_dir="$test_dir/unsigned"
  mkdir -p "$unsigned_archive_dir"
  # BSD tar (macOS) uses --uid/--gid; GNU tar (Linux) uses --owner/--group.
  if [[ "$(uname -s)" == "Darwin" ]]; then
    (cd "$staging" && find "$pkg" -print0 | sort -z | \
      COPYFILE_DISABLE=1 tar --no-recursion --null --files-from=- --uid 0 --gid 0 -cf -) | \
      gzip -n > "$unsigned_archive_dir/$pkg.tar.gz"
  else
    (cd "$staging" && find "$pkg" -print0 | sort -z | \
      tar --no-recursion --null --files-from=- --owner=0 --group=0 --numeric-owner -cf -) | \
      gzip -n > "$unsigned_archive_dir/$pkg.tar.gz"
  fi
  unsigned_archive="$unsigned_archive_dir/$pkg.tar.gz"
  unsigned_digest="$(sha256_file "$unsigned_archive")"
  printf '%s  %s\n' "$unsigned_digest" "$(basename "$unsigned_archive")" \
    > "$unsigned_archive.sha256"
}

# ---------------------------------------------------------------------------
echo "--- Test 19: a valid unsigned archive reaches mandatory signature enforcement ---"
build_unsigned_archive
unsigned_output="$(bash scripts/ci/check_release_archive.sh \
  x86_64-unknown-linux-gnu "${TAG#v}" \
  --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" \
  "$unsigned_archive" 2>&1 || true)"
if echo "$unsigned_output" | grep -q 'archive signature is mandatory for post-sign validation'; then
  record_pass "valid unsigned archive is rejected at signature enforcement"
else
  record_fail "unsigned archive did not reach mandatory signature enforcement"
  printf '%s\n' "$unsigned_output" >&2
fi

# ---------------------------------------------------------------------------
echo "--- Test 20c: archive packaging provenance must match the expected candidate ---"
build_unsigned_archive "wrong-commit"
mismatched_packaging_output="$(bash scripts/ci/check_release_archive.sh \
  x86_64-unknown-linux-gnu "${TAG#v}" \
  --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" \
  --allow-unsigned "$unsigned_archive" 2>&1 || true)"
if echo "$mismatched_packaging_output" | grep -q 'final packaging provenance is missing required fields or is not bound'; then
  record_pass "archive inspection rejects packaging provenance for the wrong commit"
else
  record_fail "archive inspection accepted packaging provenance for the wrong commit"
  printf '%s\n' "$mismatched_packaging_output" >&2
fi

build_unsigned_archive "$EXPECTED_COMMIT" "wrong-tree"
mismatched_tree_output="$(bash scripts/ci/check_release_archive.sh \
  x86_64-unknown-linux-gnu "${TAG#v}" \
  --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" \
  --allow-unsigned "$unsigned_archive" 2>&1 || true)"
if echo "$mismatched_tree_output" | grep -q 'final packaging provenance is missing required fields or is not bound'; then
  record_pass "archive inspection rejects packaging provenance for the wrong tree"
else
  record_fail "archive inspection accepted packaging provenance for the wrong tree"
  printf '%s\n' "$mismatched_tree_output" >&2
fi

# ---------------------------------------------------------------------------
echo "--- Test 20d: downloaded-draft verification requires explicit candidate provenance ---"
missing_expected_output="$(bash scripts/ci/verify_downloaded_draft.sh "$TAG" "$test_dir" 2>&1 || true)"
if echo "$missing_expected_output" | grep -q 'expected release commit and tree are required'; then
  record_pass "downloaded-draft verification fails closed without expected commit/tree"
else
  record_fail "downloaded-draft verification did not require expected commit/tree"
  printf '%s\n' "$missing_expected_output" >&2
fi

downloaded_provenance_dir="$test_dir/downloaded-provenance"
mkdir -p "$downloaded_provenance_dir"
cat > "$downloaded_provenance_dir/packaging-provenance.json" <<'EOF'
{
  "kind": "final-packaging",
  "source": {"commit_sha": "wrong-commit", "tree_sha": "wrong-tree"},
  "packaging": {"container_image": "test"}
}
EOF
mismatched_downloaded_output="$(bash scripts/ci/verify_downloaded_draft.sh "$TAG" "$downloaded_provenance_dir" \
  --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" 2>&1 || true)"
if echo "$mismatched_downloaded_output" | grep -q 'downloaded packaging provenance is missing or is not bound'; then
  record_pass "downloaded-draft verification rejects mismatched packaging provenance"
else
  record_fail "downloaded-draft verification accepted mismatched packaging provenance"
  printf '%s\n' "$mismatched_downloaded_output" >&2
fi

# ---------------------------------------------------------------------------
echo "--- Test 20: explicit unsigned mode rejects signed inputs ---"
printf 'not-a-signature\n' > "$unsigned_archive.minisig"
signed_mode_output="$(bash scripts/ci/check_release_archive.sh \
  x86_64-unknown-linux-gnu "${TAG#v}" \
  --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" \
  --allow-unsigned "$unsigned_archive" 2>&1 || true)"
if echo "$signed_mode_output" | grep -q -- '--allow-unsigned cannot validate an archive with a signature'; then
  record_pass "unsigned mode cannot bypass a signed archive"
else
  record_fail "unsigned mode accepted a signed archive"
  printf '%s\n' "$signed_mode_output" >&2
fi

# ---------------------------------------------------------------------------
echo "--- Test 20b: an actually invalid signature fails verification ---"
if command -v minisign >/dev/null 2>&1; then
  wrong_key_dir="$test_dir/wrong-signing-key"
  mkdir -p "$wrong_key_dir"
  wrong_secret="$wrong_key_dir/wrong.key"
  wrong_public="$wrong_key_dir/wrong.pub"
  minisign -G -f -W -p "$wrong_public" -s "$wrong_secret" >/dev/null 2>&1
  # A real, well-formed minisig produced by a key that is not the committed
  # release key: syntactically valid, cryptographically invalid against
  # transist-release.minisign.pub.
  minisign -S -q -s "$wrong_secret" \
    -m "$unsigned_archive" -x "$unsigned_archive.minisig"
  invalid_sig_output="$(bash scripts/ci/check_release_archive.sh \
    x86_64-unknown-linux-gnu "${TAG#v}" \
    --expected-commit "$EXPECTED_COMMIT" --expected-tree "$EXPECTED_TREE" \
    "$unsigned_archive" 2>&1 || true)"
  if echo "$invalid_sig_output" | grep -q 'archive signature verification failed'; then
    record_pass "an actually invalid (wrong-key) signature fails verification"
  else
    record_fail "an invalid signature was not rejected by signature verification"
    printf '%s\n' "$invalid_sig_output" >&2
  fi
else
  record_fail "minisign is required to test an actually invalid signature"
fi

# ---------------------------------------------------------------------------
echo "--- Test 21: fixture-driven GitHub API release policy checks ---"
fixture_file="tests/fixtures/release/github-api.json"
fake_gh="$test_dir/gh"
cp "tests/fixtures/release/fake_gh.sh" "$fake_gh"
chmod +x "$fake_gh"
candidate_sha="$(jq -r '.candidate_sha' "$fixture_file")"
fixture_reviewers="$test_dir/release-reviewers.json"
jq -c '.base.environment.reviewers' "$fixture_file" > "$fixture_reviewers"

run_policy_case() {
  local case_name="$1"
  local expected="$2"
  local output status
  output="$(PATH="$test_dir:$PATH" GH_API_FIXTURE_FILE="$fixture_file" \
    GH_API_CASE="$case_name" GITHUB_REPOSITORY="fixture-owner/fixture-repo" \
    RELEASE_REVIEWERS_FILE="$fixture_reviewers" \
    bash scripts/ci/check_release_policy.sh "$candidate_sha" 2>&1)" && status=0 || status=$?
  if [[ "$expected" == "pass" && "$status" -eq 0 ]]; then
    record_pass "GitHub API fixture accepted: $case_name"
  elif [[ "$expected" == "fail" && "$status" -ne 0 ]]; then
    record_pass "GitHub API fixture rejected: $case_name"
  else
    record_fail "GitHub API fixture had unexpected result: $case_name"
    printf '%s\n' "$output" >&2
  fi
}

run_policy_case valid pass
run_policy_case wrong-sha fail
run_policy_case wrong-provider fail
run_policy_case latest-failure fail
run_policy_case latest-incomplete fail
run_policy_case invalid-ruleset fail
run_policy_case extra-ruleset-rule fail
run_policy_case ruleset-bypass fail
run_policy_case wrong-reviewer fail
run_policy_case extra-reviewer fail
run_policy_case admin-bypass fail
run_policy_case extra-policy fail
run_policy_case wrong-tag-type fail
run_policy_case empty-policy fail
run_policy_case permissive-update-rule fail

# ---------------------------------------------------------------------------
echo "--- Test 21a: draft release lookup includes untagged URL slugs ---"
release_lookup_bin="$test_dir/release-lookup-bin"
mkdir -p "$release_lookup_bin"
cat > "$release_lookup_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'api --paginate repos/fixture-owner/fixture-repo/releases?per_page=100 --jq .[] | [.id, .tag_name] | @tsv' ]]
printf '101\tv0.10.15\n202\tv0.10.16\n'
EOF
chmod +x "$release_lookup_bin/gh"

resolved_release_id="$(PATH="$release_lookup_bin:$PATH" \
  bash scripts/ci/resolve_github_release_id.sh \
    fixture-owner/fixture-repo v0.10.16)"
if [[ "$resolved_release_id" == "202" ]]; then
  record_pass "release lookup resolves a draft from the paginated release list"
else
  record_fail "release lookup returned unexpected ID: $resolved_release_id"
fi

if PATH="$release_lookup_bin:$PATH" bash scripts/ci/resolve_github_release_id.sh \
  fixture-owner/fixture-repo v0.10.17 >/dev/null 2>&1; then
  record_fail "release lookup accepted a missing tag_name"
else
  record_pass "release lookup rejects a missing tag_name"
fi

lookup_call_count="$(grep -c 'resolve_github_release_id.sh' .github/workflows/release.yml || true)"
if [[ "$lookup_call_count" -eq 3 ]] \
    && ! grep -Fq 'releases/tags/$RELEASE_TAG' .github/workflows/release.yml; then
  record_pass "publish workflow consistently uses untagged-safe release lookup"
else
  record_fail "publish workflow bypasses the untagged-safe release lookup"
fi

if python3 - .github/workflows/release.yml <<'PY'
import sys

text = open(sys.argv[1], encoding="utf-8").read()
job = text.split("  attest-public-assets:\n", 1)[1].split("\n  publish-release:\n", 1)[0]
publish = text.split("  publish-release:\n", 1)[1]
valid = (
    "      contents: write\n" in job
    and "install_ci_tools.sh verification" in job
    and "install_ci_tools.sh release" not in job
    and "id: repository" in job
    and "if: steps.repository.outputs.visibility == 'public'" in job
    and "install_ci_tools.sh verification" in publish
    and "install_ci_tools.sh release" not in publish
)
raise SystemExit(0 if valid else 1)
PY
then
  record_pass "publish jobs use bounded tools and gate public attestations by visibility"
else
  record_fail "publish jobs lack draft access, bounded tools, or visibility-gated attestations"
fi

# ---------------------------------------------------------------------------
echo "--- Test 22: packaged-doc link escaping the archive root fails ---"
escape_pkg="$test_dir/escape-pkg"
mkdir -p "$escape_pkg/docs"
printf '%s\n' '[root](README.md)' > "$escape_pkg/README.md"
printf '%s\n' 'outside file exists' > "$test_dir/outside.md"
printf '%s\n' '[outside](../../outside.md)' > "$escape_pkg/docs/guide.md"
if python3 scripts/ci/check_doc_links.py "$escape_pkg" >/dev/null 2>&1; then
  record_fail "doc-link check accepted a link escaping the archive root"
else
  record_pass "doc-link check rejects a link escaping the archive root"
fi

# ---------------------------------------------------------------------------
echo "--- Test 23: packaged-doc link resolving inside the archive passes ---"
ok_pkg="$test_dir/ok-pkg"
mkdir -p "$ok_pkg/docs"
printf '%s\n' '[root](docs/guide.md)' > "$ok_pkg/README.md"
printf '%s\n' '[back](../README.md)' > "$ok_pkg/docs/guide.md"
if python3 scripts/ci/check_doc_links.py "$ok_pkg" >/dev/null 2>&1; then
  record_pass "doc-link check accepts a link resolving inside the package"
else
  record_fail "doc-link check rejected a valid in-package link"
fi

# ---------------------------------------------------------------------------
echo "--- Test 24: smoke exit-status propagates when a leg fails ---"
# Regression test for the `if ! cmd; then rc=$?` exit-status bug:
# `!` negates the exit status so $? captures 0 (success) even when the
# command failed, causing the smoke script to exit 0 despite a failing leg.
# The fixed pattern `if cmd; then ...; else rc=$?` captures the real code.
fail_leg="$test_dir/fail_leg.sh"
printf '#!/usr/bin/env bash\nexit 42\n' > "$fail_leg"
chmod +x "$fail_leg"

# Minimal smoke harness using the fixed pattern from run_quickstart_smoke.sh.
fixed_harness="$test_dir/fixed_harness.sh"
{
  printf '#!/usr/bin/env bash\nset -euo pipefail\n'
  printf 'run_isolated() { bash "$1"; return $?; }\n'
  printf 'if run_isolated %q; then\n' "$fail_leg"
  printf '  echo passed\n'
  printf 'else\n'
  printf '  rc=$?\n'
  printf '  exit "$rc"\n'
  printf 'fi\n'
} > "$fixed_harness"
chmod +x "$fixed_harness"

smoke_exit=0
bash "$fixed_harness" >/dev/null 2>&1 || smoke_exit=$?
if [[ "$smoke_exit" -eq 42 ]]; then
  record_pass "smoke exit-status propagates the leg's real exit code (42)"
elif [[ "$smoke_exit" -ne 0 ]]; then
  record_pass "smoke exits non-zero when a leg fails (exit $smoke_exit)"
else
  record_fail "smoke did not fail when leg exited 42 (exit-status propagation bug)"
fi

# ---------------------------------------------------------------------------
echo "--- Test 25: SBOM normalization is byte-for-byte reproducible ---"
if bash scripts/ci/test_sbom_reproducibility.sh > /dev/null 2>&1; then
  record_pass "SBOM normalization is reproducible across generations"
else
  record_fail "SBOM normalization reproducibility test failed"
  bash scripts/ci/test_sbom_reproducibility.sh >&2 || true
fi

# ---------------------------------------------------------------------------
echo "--- Test 26: candidate worktree guard rejects an ignored file under a release input root ---"
candidate_tree="$(mktemp -d)"
git -C "$candidate_tree" init -q
git -C "$candidate_tree" config user.email "test@example.com"
git -C "$candidate_tree" config user.name "Candidate Tree Test"
mkdir -p "$candidate_tree/plugins/shell"
printf '%s\n' "intentional" > "$candidate_tree/plugins/shell/plugin.yaml"
printf '%s\n' "plugins/shell/ignored.txt" > "$candidate_tree/.gitignore"
git -C "$candidate_tree" add .
git -C "$candidate_tree" commit -q -m "init"
printf '%s\n' "ignored content that packaging would copy" > "$candidate_tree/plugins/shell/ignored.txt"
if bash scripts/ci/check_candidate_worktree.sh "$candidate_tree" > /dev/null 2>&1; then
  record_fail "candidate worktree guard accepted an ignored file under a release input root"
else
  record_pass "candidate worktree guard rejects an ignored file under a release input root"
fi
rm -rf "$candidate_tree"

# ---------------------------------------------------------------------------
echo "--- Test 27: candidate worktree guard rejects an ignored build-control file ---"
control_tree="$(mktemp -d)"
git -C "$control_tree" init -q
git -C "$control_tree" config user.email "test@example.com"
git -C "$control_tree" config user.name "Candidate Tree Test"
printf '%s\n' '.cargo/' > "$control_tree/.gitignore"
git -C "$control_tree" add .gitignore
git -C "$control_tree" commit -q -m "init"
mkdir -p "$control_tree/.cargo"
printf '%s\n' '[build]' > "$control_tree/.cargo/config.toml"
if bash scripts/ci/check_candidate_worktree.sh "$control_tree" > /dev/null 2>&1; then
  record_fail "candidate worktree guard accepted ignored .cargo/config.toml"
else
  record_pass "candidate worktree guard rejects ignored .cargo/config.toml"
fi
rm -rf "$control_tree"

# ---------------------------------------------------------------------------
echo "--- Test 28: candidate worktree guard allows ignored generated output ---"
generated_tree="$(mktemp -d)"
git -C "$generated_tree" init -q
git -C "$generated_tree" config user.email "test@example.com"
git -C "$generated_tree" config user.name "Candidate Tree Test"
printf '%s\n' '/target/' '/dist/' '/.tmp/' > "$generated_tree/.gitignore"
git -C "$generated_tree" add .gitignore
git -C "$generated_tree" commit -q -m "init"
for generated_root in target dist .tmp; do
  mkdir -p "$generated_tree/$generated_root/debug"
  printf '%s\n' "generated $generated_root output" \
    > "$generated_tree/$generated_root/debug/secret_persistence_guardrails"
done
# These roots are intentionally pruned from ignored-input inspection because
# they are outside the materialized release candidate. Generated filenames and
# contents are therefore not treated as candidate-controlled input here.
if ! bash scripts/ci/check_candidate_worktree.sh "$generated_tree" > /dev/null 2>&1; then
  record_fail "candidate worktree guard rejected pruned target/dist/.tmp output"
else
  record_pass "candidate worktree guard prunes generated target/dist/.tmp output"
fi
rm -rf "$generated_tree"

# ---------------------------------------------------------------------------
echo "--- Test 29: candidate worktree guard accepts a clean committed tree ---"
clean_candidate_tree="$(mktemp -d)"
git -C "$clean_candidate_tree" init -q
git -C "$clean_candidate_tree" config user.email "test@example.com"
git -C "$clean_candidate_tree" config user.name "Candidate Tree Test"
mkdir -p "$clean_candidate_tree/plugins/shell"
printf '%s\n' "intentional" > "$clean_candidate_tree/plugins/shell/plugin.yaml"
git -C "$clean_candidate_tree" add .
git -C "$clean_candidate_tree" commit -q -m "init"
if ! bash scripts/ci/check_candidate_worktree.sh "$clean_candidate_tree" > /dev/null 2>&1; then
  record_fail "candidate worktree guard rejected a clean committed tree"
else
  record_pass "candidate worktree guard accepts a clean committed tree"
fi
rm -rf "$clean_candidate_tree"

# ---------------------------------------------------------------------------
echo "--- Test 29b: candidate worktree guard preserves public generated fixture outputs ---"
public_fixture_tree="$(mktemp -d)"
git -C "$public_fixture_tree" init -q
git -C "$public_fixture_tree" config user.email "test@example.com"
git -C "$public_fixture_tree" config user.name "Candidate Tree Test"
printf '%s\n' 'tests/fixtures/**' > "$public_fixture_tree/.gitignore"
git -C "$public_fixture_tree" add .gitignore
git -C "$public_fixture_tree" commit -q -m "init"
mkdir -p "$public_fixture_tree/tests/fixtures/generated"
printf '%s\n' 'public fixture' > "$public_fixture_tree/tests/fixtures/generated/test.pub"
printf '%s\n' 'signature fixture' > "$public_fixture_tree/tests/fixtures/generated/test.sig"
if ! bash scripts/ci/check_candidate_worktree.sh "$public_fixture_tree" > /dev/null 2>&1; then
  record_fail "candidate worktree guard rejected allowed public fixture outputs"
else
  record_pass "candidate worktree guard allows public fixture outputs"
fi
rm -rf "$public_fixture_tree"

# ---------------------------------------------------------------------------
echo "--- Test 29c: candidate worktree guard rejects ignored fixture private keys ---"
fixture_key_tree="$(mktemp -d)"
git -C "$fixture_key_tree" init -q
git -C "$fixture_key_tree" config user.email "test@example.com"
git -C "$fixture_key_tree" config user.name "Candidate Tree Test"
printf '%s\n' 'tests/fixtures/**' > "$fixture_key_tree/.gitignore"
git -C "$fixture_key_tree" add .gitignore
git -C "$fixture_key_tree" commit -q -m "init"
mkdir -p "$fixture_key_tree/tests/fixtures/generated"
printf '%s\n' 'private fixture key' > "$fixture_key_tree/tests/fixtures/generated/test.key"
if bash scripts/ci/check_candidate_worktree.sh "$fixture_key_tree" > /dev/null 2>&1; then
  record_fail "candidate worktree guard accepted ignored tests/fixtures/**/*.key"
else
  record_pass "candidate worktree guard rejects ignored tests/fixtures/**/*.key"
fi
rm -rf "$fixture_key_tree"

# ---------------------------------------------------------------------------
echo "--- Test 29d: candidate worktree guard rejects ignored trust key material ---"
trust_key_tree="$(mktemp -d)"
git -C "$trust_key_tree" init -q
git -C "$trust_key_tree" config user.email "test@example.com"
git -C "$trust_key_tree" config user.name "Candidate Tree Test"
printf '%s\n' 'trust-input/**' 'trust-output/**' > "$trust_key_tree/.gitignore"
git -C "$trust_key_tree" add .gitignore
git -C "$trust_key_tree" commit -q -m "init"
mkdir -p "$trust_key_tree/trust-input" "$trust_key_tree/trust-output"
printf '%s\n' 'private trust key' > "$trust_key_tree/trust-input/generated.key"
private_key_marker_prefix='-----BEGIN '
private_key_marker_type='PRIVATE KEY'
private_key_marker_suffix='-----'
printf '%s%s%s\n' "$private_key_marker_prefix" "$private_key_marker_type" \
  "$private_key_marker_suffix" > "$trust_key_tree/trust-output/material.txt"
if bash scripts/ci/check_candidate_worktree.sh "$trust_key_tree" > /dev/null 2>&1; then
  record_fail "candidate worktree guard accepted ignored trust key material"
else
  record_pass "candidate worktree guard rejects ignored trust key material"
fi
rm -rf "$trust_key_tree"

# ---------------------------------------------------------------------------
echo "--- Test 30: Alpine packaging tooling requires pinned GNU tar ---"
# shellcheck disable=SC1091
source scripts/ci/tool-versions.env
if [[ "$ALPINE_TAR_PACKAGE" != tar=* ]]; then
  record_fail "Alpine GNU tar package is not pinned by exact package version"
elif ! grep -Fq "\"\$ALPINE_TAR_PACKAGE\"" scripts/ci/install_alpine_tools.sh; then
  record_fail "Alpine installer does not consume the GNU tar package pin"
elif ! grep -Fq "grep -q '^tar (GNU tar)'" scripts/ci/install_alpine_tools.sh; then
  record_fail "Alpine installer does not fail closed when tar is not GNU tar"
elif ! grep -Fq 'ALPINE_TAR_PACKAGE' scripts/ci/check_supply_chain_pins.sh; then
  record_fail "supply-chain checks do not cover the Alpine GNU tar pin"
else
  record_pass "Alpine deterministic packaging requires exact pinned GNU tar"
fi

# Fixture checker for the Linux-only launcher contract. It recognizes the
# production array wiring so the candidate dry-run entry point and removal
# fixtures use the same static contract.
check_linux_launcher_wiring_fixture() {
  local fixture="$1"
  grep -Fq 'release_packages+=(-p transist-sandbox-launcher)' "$fixture" \
    && grep -Eq 'release_binaries\+=\(.*transist-sandbox-launcher' "$fixture" \
    && grep -Eq 'package_args\+=\(.*transist-sandbox-launcher' "$fixture"
}

write_linux_launcher_wiring_fixture() {
  local source="$1"
  local fixture="$2"
  local omitted_edge="$3"
  awk -v omitted_edge="$omitted_edge" '
    omitted_edge == "build" && /release_packages\+=\(-p transist-sandbox-launcher\)/ { next }
    omitted_edge == "provenance" && /release_binaries\+=\(.*transist-sandbox-launcher/ { next }
    omitted_edge == "package" && /package_args\+=\(.*transist-sandbox-launcher/ { next }
    { print }
  ' "$source" > "$fixture"
}

release_entry_point=candidate-dry-run
case "$release_entry_point" in
  candidate-dry-run) release_source="scripts/ci/check_release_candidate.sh" ;;
esac
fixture="$test_dir/${release_entry_point}-launcher-wiring.sh"
echo "--- Linux launcher wiring: $release_entry_point ---"
if check_linux_launcher_wiring_fixture "$release_source"; then
  record_pass "$release_entry_point preserves Linux launcher build/provenance/package wiring"
else
  record_fail "$release_entry_point is missing Linux launcher build/provenance/package wiring"
fi

for removed_edge in build provenance package; do
  write_linux_launcher_wiring_fixture "$release_source" "$fixture" "$removed_edge"
  if check_linux_launcher_wiring_fixture "$fixture"; then
    record_fail "$release_entry_point fixture checker accepted removed launcher $removed_edge wiring"
  else
    record_pass "$release_entry_point fixture checker rejects removed launcher $removed_edge wiring"
  fi
done

# ---------------------------------------------------------------------------
echo "--- Test 28: host-independent archive member validation regressions ---"
if bash scripts/ci/test_archive_member_validation.sh > /dev/null 2>&1; then
  record_pass "archive member validator rejects unsafe and non-canonical archives"
else
  record_fail "archive member validation regression suite failed"
  bash scripts/ci/test_archive_member_validation.sh >&2 || true
fi

# ---------------------------------------------------------------------------
echo "--- Test 29: required gates aggregate every required job result ---"
if python3 scripts/ci/test_ci_contracts.py; then
  record_pass "focused CI DAG, gate wiring, and actual evaluator contracts pass"
else
  record_fail "focused CI contract suite failed"
fi

if grep -Fq 'release.yml/badge.svg?event=workflow_dispatch' README.md \
    && ! grep -Fq 'release.yml/badge.svg?branch=main' README.md; then
  record_pass "release badge reports manually dispatched tag workflows"
else
  record_fail "release badge is filtered to a branch that cannot trigger Release CD"
fi

security_gate="scripts/ci/run_security_regression_suite.sh"
if ! grep -Fq 'cargo test --locked -p transist-cli --test workflow_install workflow_install_rejects_v0_workflow_without_dev_bypass' "$security_gate"; then
  record_fail "security gate does not explicitly execute default-feature v0 refusal coverage"
elif ! grep -Fq 'cargo test --locked --features dev-version-bypass --test gov_demo_bypass' "$security_gate"; then
  record_fail "security gate does not exercise audited runtime version bypass behavior"
elif ! grep -Fq 'cargo test --locked -p transist-cli --features dev-version-bypass --test workflow_install workflow_install_accepts_v0_workflow_with_dev_bypass' "$security_gate"; then
  record_fail "security gate does not exercise CLI version bypass behavior"
elif ! grep -Fq 'run_step "Security regression suite" bash scripts/ci/run_security_regression_suite.sh' scripts/ci/check_release_readiness.sh; then
  record_fail "release readiness does not run the security regression gate"
else
  record_pass "security and release gates cover default refusal and development bypass behavior"
fi

# ---------------------------------------------------------------------------
echo "--- Test 30: workflow shell contract rejects missing workflow-level Bash ---"
# Mirror of the workflow shell contract enforced by check_supply_chain_pins.sh.
# A workflow must declare workflow-level defaults.run.shell: bash so
# Bash-authored `run` blocks resolve to Bash (GitHub container jobs default to
# `sh`). Each Alpine bootstrap step must invoke sh scripts/ci/install_alpine_tools.sh
# with `shell: sh` explicitly, before the pinned Bash package is installed.
shell_contract_violations() {
  local file="$1"
  python3 - "$file" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
errors = []
if not re.search(r'^defaults:\s*\n  run:\s*\n    shell:\s*bash\s*$', text, re.M):
    errors.append("missing workflow-level defaults.run.shell: bash")
for chunk in re.split(r'\n(?=      - )', text):
    if re.search(r'^      - name: Install Alpine runtime and sandbox tools', chunk, re.M):
        if not re.search(r'shell:\s*sh\b', chunk):
            errors.append("Alpine bootstrap step missing 'shell: sh'")
        if not re.search(r'run:\s*sh scripts/ci/install_alpine_tools\.sh', chunk):
            errors.append("Alpine bootstrap step must invoke 'sh scripts/ci/install_alpine_tools.sh'")
print("\n".join(errors))
PY
}

# The clean PR workflow satisfies the contract.
if [[ -z "$(shell_contract_violations .github/workflows/pr-ci.yml)" ]]; then
  record_pass "clean pr-ci.yml satisfies the workflow shell contract"
else
  record_fail "clean pr-ci.yml violates the workflow shell contract"
  shell_contract_violations .github/workflows/pr-ci.yml >&2
fi

# Mutation: remove the workflow-level defaults.run.shell: bash block. The
# contract must detect the missing workflow-level Bash default while the
# Alpine bootstrap step still carries shell: sh.
mutated_no_default="$test_dir/pr-ci-no-workflow-bash.yml"
sed '/^defaults:$/,/^    shell: bash$/d' .github/workflows/pr-ci.yml > "$mutated_no_default"
if [[ -z "$(shell_contract_violations "$mutated_no_default")" ]]; then
  record_fail "workflow shell contract accepted a missing workflow-level Bash default"
else
  record_pass "workflow shell contract rejects a missing workflow-level Bash default"
fi

# Mutation: downgrade the Alpine bootstrap shell: sh to shell: bash. The
# contract must detect that the Alpine bootstrap step no longer runs under sh
# before the pinned Bash package is installed.
# Note: pr-ci.yml no longer has an Alpine step (governance plan Step 1 moved
# Alpine to main-ci.yml only). Test the mutation against main-ci.yml instead.
mutated_no_sh="$test_dir/main-ci-no-alpine-sh.yml"
sed 's/shell: sh/shell: bash/' .github/workflows/main-ci.yml > "$mutated_no_sh"
if [[ -z "$(shell_contract_violations "$mutated_no_sh")" ]]; then
  record_fail "workflow shell contract accepted a missing Alpine bootstrap shell: sh"
else
  record_pass "workflow shell contract rejects a missing Alpine bootstrap shell: sh"
fi

# ---------------------------------------------------------------------------
echo "--- Test 31: Bookworm container jobs install required pinned Debian tools ---"
# Mirror of the Debian bootstrap contract enforced by check_supply_chain_pins.sh.
# Each Bookworm container job must install jq, python3, and (where needed) pip3
# explicitly through install_debian_tools.sh; no job may rely on the base image
# or a transitive package for these tools.
debian_bootstrap_missing() {
  local file="$1"
  local job="$2"
  local required="$3"
  python3 - "$file" "$job" "$required" <<'PY'
import re, sys
from pathlib import Path
file_path, job_key, required_str = sys.argv[1], sys.argv[2], sys.argv[3]
required = set(required_str.split())
text = Path(file_path).read_text(encoding="utf-8")
m = re.search(rf'^  {re.escape(job_key)}:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)', text, re.M | re.S)
if not m:
    print(f"job '{job_key}' not found in {file_path}")
    sys.exit(0)
job_body = m.group("body")
installed = set()
for mm in re.finditer(r'install_debian_tools\.sh\s+([^\n]+)', job_body):
    for t in mm.group(1).strip().rstrip('\\').strip().split():
        installed.add(t)
missing = required - installed
if missing:
    print(f"missing: {', '.join(sorted(missing))}")
PY
}

# Valid: each affected job class installs its complete required set.
# main-ci.yml now runs only platform smoke: Linux release build + quickstart
# smoke (bubblewrap, git, python3); the prior validate/security-regression-msrv
# jobs are gone.
for case in \
  ".github/workflows/pr-ci.yml validate-full bubblewrap,ripgrep,git,jq,python3,pip3" \
  ".github/workflows/main-ci.yml platform-smoke bubblewrap,git,python3" \
  ".github/workflows/security.yml security-audit bubblewrap,git,jq,python3,pip3" \
  ".github/workflows/release.yml build jq python3" \
  ".github/workflows/release.yml package jq python3" \
  ".github/workflows/release.yml archive-smoke jq python3" \
  ".github/workflows/release.yml stage-and-sign curl,git,jq,pip3,python3,unzip"; do
  read -r wf job required <<< "$case"
  required="${required//,/ }"
  if [[ -z "$(debian_bootstrap_missing "$wf" "$job" "$required")" ]]; then
    record_pass "$wf/$job installs required Debian tools"
  else
    record_fail "$wf/$job is missing required Debian tools"
    debian_bootstrap_missing "$wf" "$job" "$required" >&2
  fi
done

# Mutation: remove python3 from the main-ci platform-smoke job. The contract
# must detect it; the quickstart smoke needs python3 for its state assertions.
mutated_no_py_smoke="$test_dir/main-ci-no-platform-python3.yml"
sed 's/install_debian_tools.sh bubblewrap git python3/install_debian_tools.sh bubblewrap git/' \
  .github/workflows/main-ci.yml > "$mutated_no_py_smoke"
if [[ -z "$(debian_bootstrap_missing "$mutated_no_py_smoke" platform-smoke "bubblewrap git python3")" ]]; then
  record_fail "Debian bootstrap contract accepted a missing python3 in main-ci platform-smoke"
else
  record_pass "Debian bootstrap contract rejects a missing python3 in main-ci platform-smoke"
fi

# Mutation: remove unzip from stage-and-sign. The pinned signing-tool installer
# must not depend on a utility supplied transitively by the base image.
mutated_no_signing_unzip="$test_dir/release-no-signing-unzip.yml"
sed 's/install_debian_tools.sh curl git jq pip3 python3 unzip/install_debian_tools.sh curl git jq pip3 python3/' \
  .github/workflows/release.yml > "$mutated_no_signing_unzip"
if [[ -z "$(debian_bootstrap_missing "$mutated_no_signing_unzip" stage-and-sign "curl git jq pip3 python3 unzip")" ]]; then
  record_fail "Debian bootstrap contract accepted missing unzip in stage-and-sign"
else
  record_pass "Debian bootstrap contract rejects missing unzip in stage-and-sign"
fi

# Mutation: remove python3 from the security-audit job. The contract must
# detect it.
mutated_no_py_audit="$test_dir/security-no-python3.yml"
sed 's/bubblewrap curl git jq ripgrep unzip nodejs npm python3 pip3/bubblewrap curl git jq ripgrep unzip nodejs npm/' \
  .github/workflows/security.yml > "$mutated_no_py_audit"
if [[ -z "$(debian_bootstrap_missing "$mutated_no_py_audit" security-audit "python3 pip3")" ]]; then
  record_fail "Debian bootstrap contract accepted a missing python3 in security-audit"
else
  record_pass "Debian bootstrap contract rejects a missing python3 in security-audit"
fi

# Mutation: remove jq from the release package job. The contract must detect it.
mutated_no_jq_pkg="$test_dir/release-no-jq-package.yml"
python3 -c '
import re, sys
text = open(".github/workflows/release.yml", encoding="utf-8").read()
# Replace only the package job'"'"'s install line (the second occurrence of
# "jq python3" in an install_debian_tools.sh call).
m = re.search(r"^(  package:.*?)bash scripts/ci/install_debian_tools\.sh jq python3",
               text, re.M | re.S)
assert m, "could not find package job jq install"
start = m.start()
text = text[:start] + m.group(1) + "bash scripts/ci/install_debian_tools.sh python3" + text[m.end():]
open(sys.argv[1], "w", encoding="utf-8").write(text)
' "$mutated_no_jq_pkg"
if [[ -z "$(debian_bootstrap_missing "$mutated_no_jq_pkg" package "jq python3")" ]]; then
  record_fail "Debian bootstrap contract accepted a missing jq in release package"
else
  record_pass "Debian bootstrap contract rejects a missing jq in release package"
fi

# ---------------------------------------------------------------------------
echo "--- Test 32: Alpine jsonschema isolation contract ---"
# Alpine jsonschema must be installed with --target into an isolated directory,
# verified by import, and propagated via GITHUB_ENV. No global pip install.
alpine_isolation_violations() {
  local file="$1"
  local errors=""
  if grep -qE 'python3 -m pip install' "$file" && ! grep -Fq -- '--target' "$file"; then
    errors+="global pip install without --target; "
  fi
  if ! grep -Fq -- '--target' "$file"; then
    errors+="missing --target flag; "
  fi
  if ! grep -Fq "python3 -c 'import jsonschema'" "$file"; then
    errors+="missing import verification; "
  fi
  if ! grep -Fq 'GITHUB_ENV' "$file"; then
    errors+="missing GITHUB_ENV propagation; "
  fi
  echo "$errors"
}

if [[ -z "$(alpine_isolation_violations scripts/ci/install_alpine_tools.sh)" ]]; then
  record_pass "clean install_alpine_tools.sh satisfies jsonschema isolation"
else
  record_fail "clean install_alpine_tools.sh violates jsonschema isolation"
  alpine_isolation_violations scripts/ci/install_alpine_tools.sh >&2
fi

# Mutation: remove --target (global install, PEP 668 violation).
mutated_global_pip="$test_dir/install_alpine_global_pip.sh"
sed 's/--target "$pythonlib"//' scripts/ci/install_alpine_tools.sh > "$mutated_global_pip"
if [[ -z "$(alpine_isolation_violations "$mutated_global_pip")" ]]; then
  record_fail "Alpine isolation contract accepted a global pip install (missing --target)"
else
  record_pass "Alpine isolation contract rejects a global pip install (missing --target)"
fi

# Mutation: remove import verification.
mutated_no_import="$test_dir/install_alpine_no_import.sh"
sed "s/python3 -c 'import jsonschema'/true/" scripts/ci/install_alpine_tools.sh > "$mutated_no_import"
if [[ -z "$(alpine_isolation_violations "$mutated_no_import")" ]]; then
  record_fail "Alpine isolation contract accepted missing import verification"
else
  record_pass "Alpine isolation contract rejects missing import verification"
fi

# Mutation: remove GITHUB_ENV propagation.
mutated_no_ghenv="$test_dir/install_alpine_no_ghenv.sh"
sed '/GITHUB_ENV/d' scripts/ci/install_alpine_tools.sh > "$mutated_no_ghenv"
if [[ -z "$(alpine_isolation_violations "$mutated_no_ghenv")" ]]; then
  record_fail "Alpine isolation contract accepted missing GITHUB_ENV propagation"
else
  record_pass "Alpine isolation contract rejects missing GITHUB_ENV propagation"
fi

# ---------------------------------------------------------------------------
echo "--- Test 33: GITHUB_ENV stores an expanded Python path ---"
github_env_pythonpath_violations() {
  local file="$1"
  local root_expr="$2"
  python3 - "$file" "$root_expr" <<'PY'
import sys
from pathlib import Path

path, root = sys.argv[1:]
text = Path(path).read_text(encoding="utf-8")
expected = "printf 'PYTHONPATH=%s\\n' \"" + root + '${PYTHONPATH:+:$PYTHONPATH}" >> "$GITHUB_ENV"'
malformed = "printf 'PYTHONPATH=%s${PYTHONPATH:+:$PYTHONPATH}\\n'"
errors = []
if expected not in text:
    errors.append("PYTHONPATH is not expanded before writing GITHUB_ENV")
if malformed in text:
    errors.append("GITHUB_ENV contains a literal shell parameter expression")
print("; ".join(errors))
PY
}

for case in \
  'scripts/ci/install_alpine_tools.sh|$pythonlib' \
  'scripts/ci/install_ci_tools.sh|$tool_root/pythonlib'; do
  file="${case%%|*}"
  root_expr="${case#*|}"
  if [[ -z "$(github_env_pythonpath_violations "$file" "$root_expr")" ]]; then
    record_pass "$file expands PYTHONPATH before writing GITHUB_ENV"
  else
    record_fail "$file writes a malformed PYTHONPATH to GITHUB_ENV"
  fi
done

mutated_literal_pythonpath="$test_dir/install-alpine-literal-pythonpath.sh"
python3 - scripts/ci/install_alpine_tools.sh "$mutated_literal_pythonpath" <<'PY'
import sys
from pathlib import Path

source, output = map(Path, sys.argv[1:])
text = source.read_text(encoding="utf-8")
text = text.replace(
    "printf 'PYTHONPATH=%s\\n' \"$pythonlib${PYTHONPATH:+:$PYTHONPATH}\" >> \"$GITHUB_ENV\"",
    "printf 'PYTHONPATH=%s${PYTHONPATH:+:$PYTHONPATH}\\n' \"$pythonlib\" >> \"$GITHUB_ENV\"",
)
output.write_text(text, encoding="utf-8")
PY
if [[ -z "$(github_env_pythonpath_violations "$mutated_literal_pythonpath" '$pythonlib')" ]]; then
  record_fail "GITHUB_ENV contract accepted a literal PYTHONPATH expression"
else
  record_pass "GITHUB_ENV contract rejects a literal PYTHONPATH expression"
fi

# ---------------------------------------------------------------------------
echo "--- Test 34: centralized packaging uses only host-independent archive checks ---"
release_validation_contract_violations() {
  local file="$1"
  python3 - "$file" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
errors = []
blocks = {}
for job in ("package", "archive-smoke", "stage-and-sign"):
    match = re.search(
        rf'^  {re.escape(job)}:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)',
        text,
        re.M | re.S,
    )
    if not match:
        errors.append(f"missing job: {job}")
        continue
    blocks[job] = match.group("body")

package = blocks.get("package", "")
native = blocks.get("archive-smoke", "")
signing = blocks.get("stage-and-sign", "")
if "check_release_archive.sh" not in package or "--host-independent" not in package:
    errors.append("package job must use host-independent archive validation")
if "--expected-commit" not in package or "--expected-tree" not in package:
    errors.append("package job must bind archive provenance to the resolved release commit/tree")
if "check_release_archive.sh" not in native or "--host-independent" in native:
    errors.append("archive-smoke must retain full native archive validation")
if "--expected-commit" not in native or "--expected-tree" not in native:
    errors.append("archive-smoke must bind archive provenance to the resolved release commit/tree")
if "verify_downloaded_draft.sh" not in signing or "--expected-commit" not in signing or "--expected-tree" not in signing:
    errors.append("stage-and-sign must bind downloaded-draft provenance to the resolved release commit/tree")
if not re.search(r'container:\s*\n\s+image:\s*rust:1\.88\.0-bookworm@sha256:[0-9a-f]{64}\s*$', signing, re.M):
    errors.append("stage-and-sign must use the digest-pinned Bookworm container")
print("\n".join(errors))
PY
}

if [[ -z "$(release_validation_contract_violations .github/workflows/release.yml)" ]]; then
  record_pass "release workflow separates centralized and native archive validation"
else
  record_fail "release workflow archive validation contract is broken"
fi

mutated_native_package="$test_dir/release-native-package-validation.yml"
sed '/--host-independent/s/ --host-independent//' .github/workflows/release.yml > "$mutated_native_package"
if [[ -z "$(release_validation_contract_violations "$mutated_native_package")" ]]; then
  record_fail "release contract accepted native cross-target validation in package job"
else
  record_pass "release contract rejects native cross-target validation in package job"
fi

mutated_no_signing_container="$test_dir/release-no-signing-container.yml"
python3 - .github/workflows/release.yml "$mutated_no_signing_container" <<'PY'
import re
import sys
from pathlib import Path

source, output = map(Path, sys.argv[1:])
text = source.read_text(encoding="utf-8")
match = re.search(r'(^  stage-and-sign:\s*\n)(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)', text, re.M | re.S)
assert match
body = re.sub(r'^    container:\s*\n      image:.*\n', '', match.group("body"), count=1, flags=re.M)
text = text[:match.start("body")] + body + text[match.end("body"):]
output.write_text(text, encoding="utf-8")
PY
if [[ -z "$(release_validation_contract_violations "$mutated_no_signing_container")" ]]; then
  record_fail "release contract accepted stage-and-sign without pinned Bookworm"
else
  record_pass "release contract rejects stage-and-sign without pinned Bookworm"
fi

# ---------------------------------------------------------------------------
echo "--- Test 35: release staging reuses exact-SHA gates and bounds build state ---"
staging_resource_contract_violations() {
  local installer="$1"
  local workflow="$2"
  python3 - "$installer" "$workflow" <<'PY'
import re
import sys
from pathlib import Path

installer_path, workflow_path = map(Path, sys.argv[1:])
installer = installer_path.read_text(encoding="utf-8")
workflow = workflow_path.read_text(encoding="utf-8")
errors = []
staging_installer_match = re.search(
    r'^install_staging_tools\(\) \{(?P<body>.*?)(?=^}\n)',
    installer,
    re.M | re.S,
)
if not staging_installer_match:
    errors.append("install_staging_tools() is missing")
else:
    body = staging_installer_match.group("body")
    for required in (
        'install_cargo_tool cargo-cyclonedx "$CARGO_CYCLONEDX_VERSION"',
        "install_github_cli",
        "install_py_jsonschema",
        "minisign",
    ):
        if required not in body:
            errors.append(f"staging installer omits {required}")
    for forbidden in ("cargo-deny", "cargo-about", "install_coverage_tools", "install_actionlint"):
        if forbidden in body:
            errors.append(f"staging installer includes non-staging tool {forbidden}")
preflight_match = re.search(
    r'^  release-preflight:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)',
    workflow,
    re.M | re.S,
)
if not preflight_match:
    errors.append("release-preflight job is missing")
else:
    preflight = preflight_match.group("body")
    install_at = preflight.find("install_ci_tools.sh preflight")
    verify_at = preflight.find("check_release_candidate.sh")
    if install_at < 0 or verify_at < 0 or install_at > verify_at:
        errors.append("release preflight must install pinned gh/rg before exact-SHA verification")
build_match = re.search(
    r'^  build:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)',
    workflow,
    re.M | re.S,
)
if not build_match or "needs: release-preflight" not in build_match.group("body"):
    errors.append("native builds do not fail fast behind release preflight")
stage_match = re.search(
    r'^  stage-and-sign:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)',
    workflow,
    re.M | re.S,
)
if not stage_match:
    errors.append("stage-and-sign job is missing")
else:
    body = stage_match.group("body")
    if "install_ci_tools.sh staging" not in body:
        errors.append("stage-and-sign does not use the bounded staging toolset")
    for forbidden in (
        "check_release_readiness.sh",
        "run_coverage.sh",
        "install_ci_tools.sh all",
        "install_ci_tools.sh release",
        "check_release_candidate.sh",
        "cargo build",
    ):
        if forbidden in body:
            errors.append(f"stage-and-sign reintroduces aggregate build work: {forbidden}")
if re.search(r'^  release-security:\s*$', workflow, re.M):
    errors.append("Release CD duplicates the exact-SHA Security Scan")
print("\n".join(errors))
PY
}

if [[ -z "$(staging_resource_contract_violations scripts/ci/install_ci_tools.sh .github/workflows/release.yml)" ]]; then
  record_pass "release staging verifies prior gates and performs only release-specific work"
else
  record_fail "release staging resource contract is broken"
  staging_resource_contract_violations scripts/ci/install_ci_tools.sh .github/workflows/release.yml >&2
fi

mutated_aggregate_staging="$test_dir/release-aggregate-staging.yml"
python3 - .github/workflows/release.yml "$mutated_aggregate_staging" <<'PY'
import sys
from pathlib import Path

source, output = map(Path, sys.argv[1:])
text = source.read_text(encoding="utf-8")
needle = "          bash scripts/ci/install_ci_tools.sh staging\n"
assert needle in text
output.write_text(
    text.replace(
        needle,
        needle + "          bash scripts/ci/check_release_readiness.sh\n",
        1,
    ),
    encoding="utf-8",
)
PY
if [[ -z "$(staging_resource_contract_violations scripts/ci/install_ci_tools.sh "$mutated_aggregate_staging")" ]]; then
  record_fail "staging resource contract accepted the aggregate readiness gate"
else
  record_pass "staging resource contract rejects the aggregate readiness gate"
fi

# ---------------------------------------------------------------------------
echo "--- Test 35: SBOM CLI options are compatible with the pinned cargo-cyclonedx ---"
if command -v cargo >/dev/null 2>&1 && bash scripts/ci/test_sbom_cli_compatibility.sh > /dev/null 2>&1; then
  record_pass "release workflow cargo cyclonedx options are supported by the pinned version"
else
  record_fail "release workflow uses unsupported cargo cyclonedx options"
  bash scripts/ci/test_sbom_cli_compatibility.sh >&2 || true
fi

# ---------------------------------------------------------------------------
echo "--- Test 36: release readiness fails closed on unsupported archive hosts ---"
readiness_archive_block="$(awk '/19\. Archive reproducibility/{capture=1} capture{print} /^esac$/{if(capture) exit}' scripts/ci/check_release_readiness.sh)"
if grep -Fq 'x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-apple-darwin' <<< "$readiness_archive_block" \
    && grep -q 'test_archive_reproducibility.sh "$local_target"' <<< "$readiness_archive_block"; then
  record_pass "release readiness runs archive reproducibility on supported archive hosts"
else
  record_fail "release readiness does not gate archive reproducibility on supported archive hosts"
fi
if grep -qF 'host-skipped' <<< "$readiness_archive_block"; then
  record_fail "release readiness still silently host-skips unsupported archive hosts"
else
  record_pass "release readiness does not silently skip unsupported archive hosts"
fi
if grep -q 'exit 1' <<< "$readiness_archive_block" && ! grep -q 'skipped=$((skipped + 1))' <<< "$readiness_archive_block"; then
  record_pass "unsupported archive host produces an explicit non-passing result"
else
  record_fail "unsupported archive host must fail closed, not increment a skip counter"
fi
if grep -q 'RELEASE READINESS GATE PASSED (with host-skipped checks)' scripts/ci/check_release_readiness.sh; then
  record_fail "release readiness can still print a passing verdict with skipped checks"
else
  record_pass "release readiness has no host-skipped passing verdict"
fi

# ---------------------------------------------------------------------------
echo "--- Test 36: release workflow action SHAs resolve to existing commits ---"
# This is a hosted CI fixture that requires network access. Skip locally
# when neither gh nor curl can reach GitHub (e.g. offline development).
if bash scripts/ci/test_action_resolution.sh > /dev/null 2>&1; then
  record_pass "all release workflow action SHAs resolve to existing commits"
else
  record_fail "one or more release workflow action SHAs do not resolve"
  bash scripts/ci/test_action_resolution.sh >&2 || true
fi

# ---------------------------------------------------------------------------
echo "--- Test 37: six-target release contract is complete and fail-closed ---"
release_target_contract_violations() {
  local inventory="$1"
  local workflow="$2"
  python3 - "$inventory" "$workflow" <<'PY'
import json
import re
import sys
from pathlib import Path

inventory_path, workflow_path = map(Path, sys.argv[1:])
inventory = json.loads(inventory_path.read_text(encoding="utf-8"))
workflow = workflow_path.read_text(encoding="utf-8")
targets = [entry.get("target_triple") for entry in inventory.get("released_archives", [])]
expected_targets = [
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
]
errors = []
required_release_env = '''env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0
  CARGO_NET_RETRY: 10
  RUST_BACKTRACE: short'''
if required_release_env not in workflow:
    errors.append("release workflow must disable Cargo incremental compilation to preserve runner disk")
if targets != expected_targets:
    errors.append(f"unexpected current release targets: {targets}")

for entry in inventory.get("released_archives", []):
    target = entry.get("target_triple", "")
    archive = entry.get("archive_file", "")
    if archive != f"transist-<tag>-{target}.tar.gz":
        errors.append(f"archive name does not preserve target triple: {archive}")

contents = inventory.get("archive_contents", {})
if contents.get("linux_required_files") != ["bin/transist-sandbox-launcher"]:
    errors.append("Linux launcher inventory is not exact")

for target in expected_targets:
    archive = f'transist-${{RELEASE_TAG}}-{target}.tar.gz'
    for suffix in ("", ".sha256", ".minisig"):
        if f'"{archive}{suffix}"' not in workflow:
            errors.append(f"exact staged asset list omits {archive}{suffix}")
    if f'"build-provenance-{target}.json"' not in workflow:
        errors.append(f"exact staged asset list omits {target} provenance")

if 'needs: [validate-input, release-preflight, package, archive-smoke]' not in workflow:
    errors.append("staging does not require preflight, every build, and native smoke result")
if 'needs: [validate-input, attest-public-assets]' not in workflow:
    errors.append("publication does not require attestation")
if workflow.find('bash scripts/ci/release_manifest.sh compare-draft') > workflow.find('gh release upload'):
    errors.append("draft asset comparison does not precede upload")
if workflow.count('bash scripts/ci/release_manifest.sh compare-draft') < 3:
    errors.append("draft validation is not repeated before upload, attestation, and publication")
if 'if [[ "${{ matrix.target }}" == *-linux-* ]]; then' not in workflow:
    errors.append("release build does not exclude the Linux launcher explicitly for macOS")
arm_musl_matrix = '''          - os: ubuntu-24.04-arm
            target: aarch64-unknown-linux-musl
            host: aarch64-unknown-linux-gnu'''
if arm_musl_matrix not in workflow:
    errors.append("Arm64 musl must use the native GNU-hosted Rust toolchain")
for required_probe in (
    "rustup target list --installed --toolchain 1.88.0 | grep -Fx aarch64-unknown-linux-musl",
    "command -v musl-gcc",
    "musl-gcc -x c - -o /tmp/musl-probe",
    "/tmp/musl-probe",
):
    if required_probe not in workflow:
        errors.append(f"Arm64 musl toolchain probe is missing: {required_probe}")
arm_musl_smoke = '''          - os: ubuntu-24.04-arm
            target: aarch64-unknown-linux-musl
            # JavaScript actions cannot run in Alpine containers on Arm64.
            # Smoke the static musl archive from pinned Debian with musl-tools.
            container_image: rust:1.88.0-bookworm@sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0'''
if arm_musl_smoke not in workflow:
    errors.append("Arm64 musl archive smoke must use pinned Debian, not Alpine")
for required_smoke_setup in (
    "Install Debian runtime and sandbox tools",
    "Install Arm64 musl reproducibility toolchain",
    "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER: musl-gcc",
):
    if required_smoke_setup not in workflow:
        errors.append(f"Arm64 musl archive smoke setup is missing: {required_smoke_setup}")
archive_smoke_match = re.search(
    r'^  archive-smoke:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)',
    workflow,
    re.M | re.S,
)
if not archive_smoke_match:
    errors.append("archive-smoke job is missing")
else:
    smoke_body = archive_smoke_match.group("body")
    recheckout_at = smoke_body.find("Re-checkout as a real git repo")
    trust_at = smoke_body.find('git config --global --add safe.directory "$GITHUB_WORKSPACE"')
    inspect_at = smoke_body.find('release_sha="$(git rev-parse')
    if recheckout_at < 0 or trust_at < recheckout_at or inspect_at < 0 or trust_at > inspect_at:
        errors.append("archive smoke must trust the checkout after Alpine re-checkout and before Git inspection")
print("\n".join(errors))
PY
}

if [[ -z "$(release_target_contract_violations scripts/ci/release-archive-inventory.json .github/workflows/release.yml)" ]]; then
  record_pass "current target names, sidecars, provenance, launcher split, and publication gates are complete"
else
  record_fail "current six-target release contract is incomplete"
  release_target_contract_violations scripts/ci/release-archive-inventory.json .github/workflows/release.yml >&2
fi

missing_target_assets="$test_dir/release-missing-apple-assets.yml"
sed '/transist-${RELEASE_TAG}-aarch64-apple-darwin\.tar\.gz/ d; /build-provenance-aarch64-apple-darwin\.json/ d' \
  .github/workflows/release.yml > "$missing_target_assets"
if [[ -z "$(release_target_contract_violations scripts/ci/release-archive-inventory.json "$missing_target_assets")" ]]; then
  record_fail "release contract accepted a missing target asset"
else
  record_pass "release contract rejects a missing target asset before publication"
fi

missing_native_verification="$test_dir/release-missing-native-verification.yml"
sed 's/needs: \[validate-input, release-preflight, package, archive-smoke\]/needs: [validate-input, release-preflight, package]/' \
  .github/workflows/release.yml > "$missing_native_verification"
if [[ -z "$(release_target_contract_violations scripts/ci/release-archive-inventory.json "$missing_native_verification")" ]]; then
  record_fail "release contract accepted staging without native archive verification"
else
  record_pass "release contract rejects staging without native archive verification"
fi

incremental_release="$test_dir/release-incremental-enabled.yml"
sed '/  CARGO_INCREMENTAL: 0/ d' .github/workflows/release.yml > "$incremental_release"
if [[ -z "$(release_target_contract_violations scripts/ci/release-archive-inventory.json "$incremental_release")" ]]; then
  record_fail "release contract accepted incremental compilation on constrained hosted runners"
else
  record_pass "release contract rejects incremental compilation on constrained hosted runners"
fi

# ---------------------------------------------------------------------------
echo "--- Test 38: reproducibility builds preserve the requested target ABI ---"
reproducibility_target_violations() {
  local script="$1"
  local required
  for required in \
    'rustc_host="$(rustc +1.88.0 -vV' \
    'release_dir="target/$target_triple/release"' \
    'cargo +1.88.0 build --locked --release --target "$target_triple"' \
    '"${release_binaries[0]}"' \
    'package_args+=("${release_binaries[3]}")'; do
    if ! grep -Fq -- "$required" "$script"; then
      printf 'missing target-preserving reproducibility behavior: %s\n' "$required"
    fi
  done
}

if [[ -z "$(reproducibility_target_violations scripts/ci/test_archive_reproducibility.sh)" ]]; then
  record_pass "reproducibility builds and packages binaries from the requested Rust target"
else
  record_fail "reproducibility can package host-ABI binaries under another target name"
  reproducibility_target_violations scripts/ci/test_archive_reproducibility.sh >&2
fi

missing_repro_target="$test_dir/reproducibility-missing-target.sh"
sed 's/ --target "$target_triple"//' scripts/ci/test_archive_reproducibility.sh > "$missing_repro_target"
if [[ -z "$(reproducibility_target_violations "$missing_repro_target")" ]]; then
  record_fail "reproducibility contract accepted a cross-ABI build without --target"
else
  record_pass "reproducibility contract rejects a cross-ABI build without --target"
fi

# ---------------------------------------------------------------------------
echo "--- Test 39: coverage evidence does not flood aggregate CI logs ---"
coverage_summary_violations() {
  local script="$1"
  if ! grep -Fq 'cargo llvm-cov report > "$output_dir/summary.txt"' "$script"; then
    echo "coverage summary is not generated from the compact report"
  fi
  if grep -Fq -- '--text --output-path "$output_dir/summary.txt"' "$script"; then
    echo "coverage summary uses the full line-annotated text report"
  fi
}

if [[ -z "$(coverage_summary_violations scripts/ci/run_coverage.sh)" ]]; then
  record_pass "coverage logs print only the compact summary while HTML and LCOV retain detail"
else
  record_fail "coverage evidence can exceed aggregate hosted CI log limits"
  coverage_summary_violations scripts/ci/run_coverage.sh >&2
fi

verbose_coverage="$test_dir/coverage-verbose-summary.sh"
sed 's#cargo llvm-cov report > "$output_dir/summary.txt"#cargo llvm-cov report --text --output-path "$output_dir/summary.txt"#' \
  scripts/ci/run_coverage.sh > "$verbose_coverage"
if [[ -z "$(coverage_summary_violations "$verbose_coverage")" ]]; then
  record_fail "coverage contract accepted a full source listing as the CI summary"
else
  record_pass "coverage contract rejects full source listings in aggregate CI logs"
fi

# ---------------------------------------------------------------------------
echo ""
echo "============================================================"
if [[ "$failures" -eq 0 ]]; then
  echo "  RESULT: All release invariant tests passed"
else
  echo "  RESULT: $failures test(s) failed"
fi
echo "============================================================"
exit "$failures"
