#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_release_candidate.sh — Pre-stage validation for release publication.
#
# Usage:
#   check_release_candidate.sh --dry-run <tag> --sha <sha>
#   check_release_candidate.sh <tag> --sha <sha>
#
# --dry-run: performs all local build/package/manifest checks without creating
#            a Git ref, GitHub release, or uploaded asset. No GitHub API calls
#            are made; remote tag, live rules, release environment, and
#            release-candidate gate checks are skipped.
#
# Non-dry-run: performs full pre-stage validation:
#   1. tag is v<Cargo workspace version>
#   2. tag commit is reachable from origin/main
#   3. remote refs/tags/<tag> object SHA equals local annotated tag-object SHA
#   4. local and remote peeled refs/tags/<tag>^{} commit SHA both equal the
#      release-candidate SHA
#   5. live v* rules block tag update and deletion
#   6. changelog contains the version
#   7. full release-candidate gate passed for the same SHA
#   8. release environment has required reviewer, tag restriction, no bypass
#
# Exit status: 0 (all checks pass) or 1 (any check fails).

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
dry_run=0
tag=""
sha=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --dry-run)
      dry_run=1
      shift
      ;;
    --sha)
      sha="${2:-}"
      if [[ -z "$sha" ]]; then
        echo "FAIL: --sha requires a value" >&2
        exit 1
      fi
      shift 2
      ;;
    *)
      if [[ -z "$tag" ]]; then
        tag="$1"
        shift
      else
        echo "FAIL: unexpected argument: $1" >&2
        exit 1
      fi
      ;;
  esac
done

if [[ -z "$tag" ]]; then
  echo "Usage: $0 [--dry-run] <tag> --sha <sha>" >&2
  exit 1
fi
if [[ -z "$sha" ]]; then
  echo "Usage: $0 [--dry-run] <tag> --sha <sha>" >&2
  exit 1
fi
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "FAIL: tag must match v<major>.<minor>.<patch>. Got: $tag" >&2
  exit 1
fi
if [[ "$sha" == -* ]]; then
  echo "FAIL: --sha must not start with '-'" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | cut -d' ' -f1
  else
    shasum -a 256 "$path" | cut -d' ' -f1
  fi
}

fail() {
  echo "FAIL  $1" >&2
  exit 1
}

pass() {
  echo "PASS  $1"
}

# Release candidates must be packaged and inspected on a host represented by
# the published archive matrix. Skipping this step would produce a false pass.
local_target="$(rustc -vV | sed -n 's/^host: //p')"
case "$local_target" in
  x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
    ;;
  *)
    fail "local target $local_target is not a supported release archive host (supported: x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-apple-darwin)"
    ;;
esac

# ---------------------------------------------------------------------------
# 1. Validate tag format: v<Cargo workspace version>
# ---------------------------------------------------------------------------
echo ""
echo "=== Tag and version validation ==="

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  fail "tag must match v<major>.<minor>.<patch>. Got: $tag"
fi

workspace_version="$(grep -m1 '^version' Cargo.toml | sed 's/^version *= *"*//;s/"//')"
if [[ -z "$workspace_version" ]]; then
  fail "could not determine workspace version from Cargo.toml"
fi

tag_version="${tag#v}"
if [[ "$tag_version" != "$workspace_version" ]]; then
  fail "tag version '$tag_version' does not match workspace version '$workspace_version'"
fi
pass "tag matches workspace version (v$workspace_version)"

# ---------------------------------------------------------------------------
# 2. Validate changelog contains the version
# ---------------------------------------------------------------------------
if ! grep -qF "## [$tag_version]" CHANGELOG.md; then
  fail "CHANGELOG.md does not contain version section '## [$tag_version]'"
fi
pass "changelog contains version $tag_version"

# ---------------------------------------------------------------------------
# 3. Validate committed verification key
# ---------------------------------------------------------------------------
pub_key_path="$repo_root/transist-release.minisign.pub"
if [[ ! -f "$pub_key_path" ]]; then
  fail "committed verification key not found: $pub_key_path"
fi

if ! head -1 "$pub_key_path" | grep -q '^untrusted comment: minisign public key '; then
  fail "verification key missing valid minisign public key comment header"
fi

key_id="$(head -1 "$pub_key_path" | sed 's/^untrusted comment: minisign public key //')"
if [[ -z "$key_id" ]] || [[ ${#key_id} -lt 16 ]]; then
  fail "could not extract valid key ID from verification key"
fi
pass "committed verification key is well-formed (key ID: $key_id)"

# ---------------------------------------------------------------------------
# 4. Resolve release-candidate SHA
# ---------------------------------------------------------------------------
resolved_sha="$(git rev-parse --verify --end-of-options "$sha^{commit}" 2>/dev/null || true)"
if [[ -z "$resolved_sha" ]]; then
  fail "could not resolve SHA: $sha"
fi
pass "release-candidate SHA resolved: $resolved_sha"

resolved_tree="$(git rev-parse --verify "$resolved_sha^{tree}")"
current_sha="$(git rev-parse --verify 'HEAD^{commit}')"
current_tree="$(git rev-parse --verify 'HEAD^{tree}')"
if [[ "$current_sha" != "$resolved_sha" || "$current_tree" != "$resolved_tree" ]]; then
  fail "current checkout ($current_sha, tree $current_tree) does not match --sha candidate ($resolved_sha, tree $resolved_tree)"
fi
pass "current checkout is the exact release candidate commit and tree"

# The candidate scan is bound to the exact Git tree being validated. It must
# not inspect a different worktree snapshot or an unstaged filesystem copy.
run_step() {
  local label="$1"
  shift
  "$@"
  pass "$label"
}
run_step "public candidate tree passed" bash scripts/ci/check_public_tree.sh "$repo_root" "$resolved_sha"

# ---------------------------------------------------------------------------
# Dirty worktree guard — reject staged, unstaged, untracked, or ignored changes
# so the release candidate is always built and packaged from the committed
# tree. Ignored files are included because release inputs (`plugins/shell`,
# `examples/quickstart`, `workflows/demos/getting_started`) are copied into the
# archive verbatim; an ignored file dropped under any of them would silently
# change release bytes.
# ---------------------------------------------------------------------------
echo ""
echo "=== Dirty worktree check (staged, unstaged, untracked, ignored) ==="
bash scripts/ci/check_candidate_worktree.sh "$repo_root"

# The clean checkout above matches the exact candidate tree, including all
# packaged skills. Reject external catalog references before staging assets.
run_step "repository self-containment passed" python3 scripts/ci/check_repository_self_contained.py

# ---------------------------------------------------------------------------
# Dry-run mode: local build/package/manifest checks only
# ---------------------------------------------------------------------------
if [[ "$dry_run" -eq 1 ]]; then
  echo ""
  echo "=== Dry-run: local build/package/manifest checks ==="
  echo "(No Git ref, GitHub release, or asset upload will be created.)"

  dry_run_dir="$(mktemp -d -t transist-release-dry-run-XXXXXX)"
  # Materialize the committed SHA into an isolated source tree so the build
  # and archive packaging are never affected by uncommitted local changes.
  source_dir="$(mktemp -d -t transist-release-src-XXXXXX)"
  trap 'rm -rf "$dry_run_dir" "$source_dir"' EXIT
  dry_dist="$dry_run_dir/dist"
  mkdir -p "$dry_dist"

  command -v minisign >/dev/null 2>&1 || fail "minisign is required for the dry-run signing round trip"

  # Extract the committed SHA into an isolated source tree.
  echo ""
  echo "--- Materializing committed SHA ($resolved_sha) ---"
  git archive "$resolved_sha" | tar -xf - -C "$source_dir"
  pass "committed SHA materialized"

  release_packages=(-p transist-cli -p transist-server -p transist-admin)
  release_binaries=(
    "$source_dir/target/release/transist"
    "$source_dir/target/release/transist-server"
    "$source_dir/target/release/transist-admin"
  )
  case "$local_target" in
    x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
      release_packages+=(-p transist-sandbox-launcher)
      release_binaries+=("$source_dir/target/release/transist-sandbox-launcher")
      ;;
  esac

  # Build the release binaries required for the local target.
  echo ""
  echo "--- Building release binaries from committed SHA ---"
  (cd "$source_dir" && cargo +1.88.0 build --locked --release "${release_packages[@]}")
  pass "release binaries built"

  command -v jq >/dev/null 2>&1 || fail "jq is required to verify candidate provenance"

  # Record provenance for the materialized candidate, never for whichever
  # checkout a caller happens to use as the script's repository root.
  CI_RUNNER_IMAGE="dry-run" bash scripts/ci/record_build_provenance.sh \
    "$dry_dist/build-provenance-${local_target}.json" \
    "$local_target" \
    "${release_binaries[@]}" \
    --commit "$resolved_sha" \
    --tree "$resolved_tree"
  jq -e --arg commit "$resolved_sha" --arg tree "$resolved_tree" \
    '.source.commit_sha == $commit and .source.tree_sha == $tree' \
    "$dry_dist/build-provenance-${local_target}.json" >/dev/null \
    || fail "build provenance is not bound to the release candidate"
  CI_RUNNER_IMAGE="dry-run" CI_CONTAINER_IMAGE="dry-run" bash scripts/ci/record_packaging_provenance.sh \
    "$dry_dist/packaging-provenance.json" \
    --commit "$resolved_sha" \
    --tree "$resolved_tree"
  jq -e --arg commit "$resolved_sha" --arg tree "$resolved_tree" \
    '.source.commit_sha == $commit and .source.tree_sha == $tree' \
    "$dry_dist/packaging-provenance.json" >/dev/null \
    || fail "packaging provenance is not bound to the release candidate"

  # Package the archive for the supported local target. Uses the materialized
  # source tree so only committed files are packaged.
  case "$local_target" in
    x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
      echo ""
      echo "--- Packaging archive for $local_target ---"
      package_args=(
        "$tag"
        "$source_dir/target/release/transist"
        "$source_dir/target/release/transist-server"
        "$source_dir/target/release/transist-admin"
        "$local_target"
        "$dry_dist/packaging-provenance.json"
      )
      case "$local_target" in
        x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
          package_args+=("$source_dir/target/release/transist-sandbox-launcher")
          ;;
      esac
      TRANSIST_DIST_DIR="$dry_dist" bash "$source_dir/scripts/ci/package_release_archive.sh" \
        "${package_args[@]}"

      archive_name="transist-${tag}-${local_target}.tar.gz"
      checksum_name="${archive_name}.sha256"

      [[ -f "$dry_dist/${archive_name}" ]] || fail "packaged archive not found: $dry_dist/${archive_name}"
      [[ -f "$dry_dist/${checksum_name}" ]] || fail "checksum not found: $dry_dist/${checksum_name}"

      # Verify checksum matches
      expected_digest="$(sha256_file "$dry_dist/${archive_name}")"
      recorded_digest="$(cut -d' ' -f1 "$dry_dist/${checksum_name}")"
      if [[ "$expected_digest" != "$recorded_digest" ]]; then
        fail "checksum mismatch for ${archive_name}"
      fi
      pass "archive packaged and checksum verified: ${archive_name}"
      ;;
  esac

  # Verify manifest structure and a real ephemeral-key sign/verify round trip
  # (never the committed production key or a real production secret).
  echo ""
  echo "--- Verifying manifest structure and signing round trip ---"

  # Confirm a configured signing secret (if present in this environment)
  # actually matches the committed public key, using the same probe
  # verify_release_signing_key.sh uses. This never runs against a real
  # production secret unless the caller's own environment provides one; the
  # dry-run itself never requires or generates a production-trusted key.
  if [[ -n "${MINISIGN_SECRET_KEY:-}" ]]; then
    configured_secret_path="$dry_run_dir/configured-secret.key"
    printf '%s' "$MINISIGN_SECRET_KEY" > "$configured_secret_path"
    bash scripts/ci/verify_release_signing_key.sh "$configured_secret_path" "$pub_key_path"
    rm -f "$configured_secret_path"
    pass "configured signing secret matches the committed public key"
  fi

  # Generate an ephemeral, dry-run-only minisign keypair. This proves the
  # sign -> manifest -> verify round trip actually works end to end without
  # ever touching the committed production key or a real production secret.
  ephemeral_secret="$dry_run_dir/ephemeral-release.key"
  ephemeral_public="$dry_run_dir/ephemeral-release.pub"
  minisign -G -f -W -p "$ephemeral_public" -s "$ephemeral_secret" >/dev/null
  pass "ephemeral dry-run signing key generated"

  # Create mock checksum and archive files for targets that were not
  # packaged locally so the manifest can be generated, then sign every
  # archive with the ephemeral key and verify each signature immediately.
  for target in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu aarch64-unknown-linux-musl; do
    archive="transist-${tag}-${target}.tar.gz"
    if [[ ! -f "$dry_dist/${archive}" ]]; then
      printf 'mock-archive-%s\n' "$target" > "$dry_dist/${archive}"
     fi
    if [[ ! -f "$dry_dist/${archive}.sha256" ]]; then
      local_digest="$(sha256_file "$dry_dist/${archive}")"
      printf '%s  %s\n' "$local_digest" "$archive" > "$dry_dist/${archive}.sha256"
     fi
    minisign -S -q -s "$ephemeral_secret" \
      -m "$dry_dist/${archive}" -x "$dry_dist/${archive}.minisig"
    minisign -Vq -p "$ephemeral_public" \
      -m "$dry_dist/${archive}" -x "$dry_dist/${archive}.minisig"
   done
  pass "every archive signed with the ephemeral key and verified"
  # Create mock SBOM, build provenance, and copy verification key
  mkdir -p "$dry_dist/sbom"
  : > "$dry_dist/transist-${tag}-sbom.tar.gz"
  for target in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu aarch64-unknown-linux-musl; do
    if [[ ! -f "$dry_dist/build-provenance-${target}.json" ]]; then
      : > "$dry_dist/build-provenance-${target}.json"
    fi
  done
  if [[ ! -f "$dry_dist/packaging-provenance.json" ]]; then
    : > "$dry_dist/packaging-provenance.json"
  fi
  cp "$pub_key_path" "$dry_dist/transist-release.minisign.pub"

  # Generate manifest
  manifest_path="$(bash scripts/ci/release_manifest.sh generate "$tag" "$dry_dist")"
  pass "manifest generated: $(basename "$manifest_path")"

  # Verify manifest invariants
  bash scripts/ci/release_manifest.sh verify "$tag" "$dry_dist"
  pass "manifest invariants verified"

  # Clean up mock files
  rm -f "$dry_dist/release-manifest.txt" "$dry_dist/release-manifest.txt.minisig"
  for target in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu aarch64-unknown-linux-musl; do
    archive="transist-${tag}-${target}.tar.gz"
    rm -f "$dry_dist/${archive}.minisig"
  done
  rm -f "$dry_dist/transist-${tag}-sbom.tar.gz" "$dry_dist/transist-release.minisign.pub"

  echo ""
  echo "=== Dry-run checks passed ==="
  exit 0
fi

# ---------------------------------------------------------------------------
# Non-dry-run: full pre-stage validation
# ---------------------------------------------------------------------------
echo ""
echo "=== Full pre-stage validation ==="

# 5. Validate tag commit is reachable from origin/main
git fetch --no-tags origin main >/dev/null 2>&1 || fail "could not fetch origin/main"
if ! git merge-base --is-ancestor "$resolved_sha" refs/remotes/origin/main 2>/dev/null; then
  fail "release-candidate SHA $resolved_sha is not reachable from origin/main"
fi
pass "release-candidate SHA is reachable from origin/main"

# 6. Validate remote tag object SHA equals local annotated tag-object SHA
remote_tag_sha="$(git ls-remote origin "refs/tags/${tag}" 2>/dev/null | cut -f1 || true)"
if [[ -z "$remote_tag_sha" ]]; then
  fail "tag $tag not found on remote (refs/tags/${tag})"
fi

local_tag_sha="$(git rev-parse "refs/tags/${tag}^{tag}" 2>/dev/null || true)"
if [[ -z "$local_tag_sha" ]]; then
  fail "tag $tag must be an annotated tag"
fi
if [[ -z "$local_tag_sha" ]]; then
  fail "tag $tag not found locally"
fi

if [[ "$remote_tag_sha" != "$local_tag_sha" ]]; then
  fail "remote tag SHA ($remote_tag_sha) does not match local tag SHA ($local_tag_sha)"
fi
pass "remote tag object SHA matches local tag object SHA"

# 7. Validate peeled commit SHA matches release-candidate SHA
local_peeled_sha="$(git rev-parse "refs/tags/${tag}^{}" 2>/dev/null || true)"
if [[ -z "$local_peeled_sha" ]]; then
  fail "could not peel tag $tag to commit"
fi
if [[ "$local_peeled_sha" != "$resolved_sha" ]]; then
  fail "peeled tag SHA ($local_peeled_sha) does not match release-candidate SHA ($resolved_sha)"
fi
pass "local peeled tag commit SHA matches release-candidate SHA"

# Validate remote peeled SHA (via ls-remote dereferencing). Query both the tag
# ref and its peeled ^{} ref so an annotated tag is validated against the
# release-candidate SHA; a missing peel is a hard failure.
remote_peel_line="$(git ls-remote origin "refs/tags/${tag}" "refs/tags/${tag}^{}" 2>/dev/null || true)"
# ls-remote output for annotated tags includes a second line with ^{} suffix
remote_peeled_sha="$(echo "$remote_peel_line" | grep '\^{}' | cut -f1 || true)"
if [[ -z "$remote_peeled_sha" ]]; then
  fail "remote peeled SHA for tag $tag is not available via ls-remote; the tag must be an annotated tag pointing at the release-candidate commit"
fi
if [[ "$remote_peeled_sha" != "$resolved_sha" ]]; then
  fail "remote peeled tag SHA ($remote_peeled_sha) does not match release-candidate SHA ($resolved_sha)"
fi
pass "remote peeled tag commit SHA matches release-candidate SHA"

# 8-11. Validate the live release policy and exact-SHA hosted gates.
run_step "live release policy and exact-SHA hosted gates" \
  bash scripts/ci/check_release_policy.sh "$resolved_sha"

echo ""
echo "=== Pre-stage validation passed ==="
exit 0
