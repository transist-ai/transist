#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
validator="$repo_root/scripts/ci/validate_release_archive_members.py"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

tag="v$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["workspace"]["package"]["version"])' "$repo_root/Cargo.toml")"
targets=(
  x86_64-unknown-linux-gnu
  x86_64-unknown-linux-musl
  aarch64-apple-darwin
  x86_64-apple-darwin
  aarch64-unknown-linux-gnu
  aarch64-unknown-linux-musl
)

make_fixture() {
  local target="$1"
  local fixture="$2"
  local output="$3"
  python3 - "$repo_root" "$target" "$tag" "$fixture" "$output" <<'PY'
import io
import sys
import tarfile
from pathlib import Path

repo_root, target, tag, fixture, output = sys.argv[1:]
sys.path.insert(0, str(Path(repo_root) / "scripts" / "ci"))
from release_inventory import expected_members

package_dir = f"transist-{tag}-{target}"
inventory = Path(repo_root) / "scripts" / "ci" / "release-archive-inventory.json"
members = expected_members(str(inventory), repo_root, package_dir, target)
directories = {
    member
    for member in members
    if member == package_dir or any(other.startswith(member + "/") for other in members)
}

if fixture == "unreadable":
    Path(output).write_bytes(b"not a tar archive")
    raise SystemExit
if fixture == "empty":
    with tarfile.open(output, "w:gz"):
        pass
    raise SystemExit
if fixture == "missing":
    members.remove(next(member for member in members if member not in directories))
if fixture == "missing-skill":
    members.remove(next(member for member in members if member.endswith("/SKILL.md")))
if fixture == "wrong-top":
    members = [member.replace(package_dir, "wrong-top", 1) for member in members]

# Regression: a forbidden top-level path (documentation, schemas, developer
# trees, governance files, etc.) must never enter an archive. Inject one
# first-forbidden path exactly as if an undeclared file leaked into staging.
if fixture == "forbidden":
    members.append(f"{package_dir}/docs/leak.md")
if fixture == "forbidden-second":
    members.append(f"{package_dir}/AGENTS.md")
if fixture == "agents-testing":
    members.append(f"{package_dir}/.agents/testing/leak")
if fixture == "undeclared-skill":
    members.append(f"{package_dir}/.agents/skills/undeclared.txt")

# Regression: a retained-tree README.md (e.g. plugins/shell/README.md) is
# source-only content and must never be packaged. A leak must fail validation.
if fixture == "retained-readme":
    members.append(f"{package_dir}/plugins/shell/README.md")

special_types = {
    "symlink": tarfile.SYMTYPE,
    "hardlink": tarfile.LNKTYPE,
    "character-device": tarfile.CHRTYPE,
    "block-device": tarfile.BLKTYPE,
    "fifo": tarfile.FIFOTYPE,
    "socket": b"s",
    "other-special": b"Z",
}

with tarfile.open(output, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
    for member in members:
        info = tarfile.TarInfo(member + "/" if member in directories else member)
        if member in directories:
            info.type = tarfile.DIRTYPE
            archive.addfile(info)
        elif fixture in special_types and member == f"{package_dir}/LICENSE":
            info.type = special_types[fixture]
            if fixture in {"symlink", "hardlink"}:
                info.linkname = f"{package_dir}/README.md"
            archive.addfile(info)
        else:
            data = b"fixture\n"
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))

    if fixture in {"skill-symlink", "skill-hardlink"}:
        skill_member = next(member for member in members if member.endswith("/SKILL.md"))
        info = tarfile.TarInfo(skill_member)
        info.type = tarfile.SYMTYPE if fixture == "skill-symlink" else tarfile.LNKTYPE
        info.linkname = f"{package_dir}/README.md"
        archive.addfile(info)

    name = f"{package_dir}/LICENSE"
    if fixture == "absolute":
        name = f"/{package_dir}/absolute"
    elif fixture == "drive-absolute":
        name = f"C:/{package_dir}/absolute"
    elif fixture == "dot":
        name = f"{package_dir}/./dot"
    elif fixture == "traversal":
        name = f"{package_dir}/../traversal"
    elif fixture == "backslash":
        name = f"{package_dir}\\ambiguous"
    elif fixture == "extra":
        name = f"{package_dir}/unexpected"
    elif fixture == "duplicate":
        name = package_dir

    if fixture in {"absolute", "drive-absolute", "dot", "traversal", "backslash", "extra"}:
        info = tarfile.TarInfo(name)
        info.size = 1
        archive.addfile(info, io.BytesIO(b"x"))
    elif fixture == "duplicate":
        info = tarfile.TarInfo(name)
        info.type = tarfile.DIRTYPE
        archive.addfile(info)
PY
}

expect_pass() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    printf 'PASS: %s\n' "$label"
  else
    printf 'FAIL: %s\n' "$label" >&2
    return 1
  fi
}

expect_fail() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    printf 'FAIL: %s was accepted\n' "$label" >&2
    return 1
  else
    printf 'PASS: %s rejected\n' "$label"
  fi
}

for target in "${targets[@]}"; do
  archive="$tmp_root/valid-$target.tar.gz"
  make_fixture "$target" valid "$archive"
  expect_pass "valid $target inventory" python3 "$validator" "$target" "$tag" "$archive"
done

malicious_fixtures=(
  unreadable
  empty
  absolute
  drive-absolute
  dot
  traversal
  backslash
  wrong-top
  duplicate
  symlink
  hardlink
  character-device
  block-device
  fifo
  socket
  other-special
  extra
  missing
  missing-skill
  forbidden
  forbidden-second
  agents-testing
  undeclared-skill
  skill-symlink
  skill-hardlink
  retained-readme
)

target="x86_64-unknown-linux-gnu"
for fixture in "${malicious_fixtures[@]}"; do
  archive="$tmp_root/$fixture.tar.gz"
  make_fixture "$target" "$fixture" "$archive"
  expect_fail "$fixture archive" python3 "$validator" "$target" "$tag" "$archive"
done

printf 'PASS: all archive member validation regressions passed\n'
