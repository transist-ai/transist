#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check_release_inventory.sh — single-source drift gate for the exact release
# archive inventory.
#
# release-archive-inventory.json defines the canonical release matrix (target
# triples + archive names) and archive contents. `check_release_archive.sh`
# and `release_manifest.sh` read that file directly, so they cannot drift from
# it. This script additionally proves the public documentation
# (docs/public/reference/supported-platforms.md) matches the same inventory:
# any documented target or archive name that is not in the inventory, and any
# inventory target or archive name that is not documented, fails CI.
#
# Exit status: 0 (inventory valid and docs match) or 1 (any check fails).

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

inventory="scripts/ci/release-archive-inventory.json"
platform_doc="docs/public/reference/supported-platforms.md"

fail=0

if [[ ! -f "$inventory" ]]; then
  echo "FAIL: missing canonical release-archive inventory '$inventory'" >&2
  exit 1
fi
if [[ ! -f "$platform_doc" ]]; then
  echo "FAIL: missing supported-platforms documentation '$platform_doc'" >&2
  exit 1
fi

python3 - "$inventory" "$platform_doc" "$ROOT_DIR" <<'PY'
import json
import re
import sys
from pathlib import Path

inventory_path, doc_path, repo_root = sys.argv[1], sys.argv[2], sys.argv[3]
fail = 0

with open(inventory_path, encoding="utf-8") as stream:
    inventory = json.load(stream)

if inventory.get("version") != 1:
    print("FAIL: release-archive-inventory.json must declare version 1", file=sys.stderr)
    fail += 1

targets = inventory.get("released_archives")
if not isinstance(targets, list) or not targets:
    print("FAIL: release-archive-inventory.json released_archives must be a non-empty array", file=sys.stderr)
    fail += 1
    targets = []

archive_files = set()
target_triples = set()
expected_metadata = {
    "x86_64-unknown-linux-gnu": ("linux", "x86_64", "gnu", True),
    "x86_64-unknown-linux-musl": ("linux", "x86_64", "musl", True),
    "aarch64-apple-darwin": ("macos", "aarch64", "darwin", False),
    "x86_64-apple-darwin": ("macos", "x86_64", "darwin", False),
    "aarch64-unknown-linux-gnu": ("linux", "aarch64", "gnu", True),
    "aarch64-unknown-linux-musl": ("linux", "aarch64", "musl", True),
}
for entry in targets:
    triple = entry.get("target_triple")
    archive_file = entry.get("archive_file")
    if not triple or not archive_file:
        print(f"FAIL: inventory archive entry missing target_triple/archive_file: {entry}", file=sys.stderr)
        fail += 1
        continue
    if "<tag>" not in archive_file:
        print(f"FAIL: inventory archive_file '{archive_file}' must contain the <tag> placeholder", file=sys.stderr)
        fail += 1
    if triple in target_triples:
        print(f"FAIL: duplicate release target '{triple}'", file=sys.stderr)
        fail += 1
    if archive_file in archive_files:
        print(f"FAIL: duplicate release archive '{archive_file}'", file=sys.stderr)
        fail += 1
    target_triples.add(triple)
    archive_files.add(archive_file)
    metadata = (
        entry.get("os"),
        entry.get("architecture"),
        entry.get("abi"),
        entry.get("includes_sandbox_launcher"),
    )
    if metadata != expected_metadata.get(triple):
        print(f"FAIL: release metadata for '{triple}' must be {expected_metadata.get(triple)}, got {metadata}", file=sys.stderr)
        fail += 1

expected_targets = {
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
}
if target_triples != expected_targets:
    print(
        f"FAIL: release targets must be exactly {sorted(expected_targets)}, "
        f"got {sorted(target_triples)}",
        file=sys.stderr,
    )
    fail += 1

contents = inventory.get("archive_contents", {})
required_files = contents.get("required_files")
required_dirs = contents.get("required_dirs")
linux_required_files = contents.get("linux_required_files")
retained_trees = contents.get("retained_trees")
tracked_trees = contents.get("tracked_trees")
forbidden_paths = contents.get("forbidden_paths")
release_contents_docs = contents.get("release_contents_docs")
if not isinstance(required_files, list) or not required_files:
    print("FAIL: archive_contents.required_files must be a non-empty array", file=sys.stderr)
    fail += 1
if not isinstance(required_dirs, list) or not required_dirs:
    print("FAIL: archive_contents.required_dirs must be a non-empty array", file=sys.stderr)
    for rel in required_files or []:
        if rel.startswith("bin/") or rel.startswith("docs") or rel.startswith("plugins/") or rel.startswith("examples/"):
            pass
    fail += 1
if linux_required_files != ["bin/transist-sandbox-launcher"]:
    print(
        "FAIL: archive_contents.linux_required_files must contain exactly "
        "bin/transist-sandbox-launcher",
        file=sys.stderr,
    )
    fail += 1

# Retained trees are directories copied recursively into the archive. They must
# be listed explicitly (never a broad repository root) and must be non-empty.
if not isinstance(retained_trees, list) or not retained_trees:
    print("FAIL: archive_contents.retained_trees must be a non-empty array", file=sys.stderr)
    fail += 1
else:
    if any(tree.startswith(("docs", "schemas", "scripts", "transist-admin")) for tree in retained_trees):
        print("FAIL: retained_trees must not include broad excluded roots (docs/, schemas/, scripts/, transist-admin/)", file=sys.stderr)
        fail += 1
    for tree in retained_trees:
        if not (Path(repo_root) / tree).is_dir():
            print(f"FAIL: retained_tree '{tree}' does not exist in the repository", file=sys.stderr)
            fail += 1

if tracked_trees != [".agents/skills"]:
    print("FAIL: archive_contents.tracked_trees must equal exactly ['.agents/skills']", file=sys.stderr)
    fail += 1
else:
    skill_root = Path(repo_root) / ".agents/skills"
    if not skill_root.is_dir():
        print("FAIL: tracked_tree '.agents/skills' does not exist in the repository", file=sys.stderr)
        fail += 1
    else:
        import subprocess

        tracked = subprocess.run(
            ["git", "-C", repo_root, "ls-files", "-z", "--", ".agents/skills/**"],
            check=True,
            stdout=subprocess.PIPE,
        ).stdout.split(b"\0")
        if not any(tracked):
            print("FAIL: tracked_tree '.agents/skills' contains no Git-tracked files", file=sys.stderr)
            fail += 1

# Forbidden paths must cover every excluded top-level path so an undeclared
# member is always rejected.
expected_forbidden = {
    ".agents/CORE_CHANGE_CHECKLIST.md",
    ".agents/development",
    ".agents/testing",
    "docs",
    "schemas",
    "scripts",
    "transist-admin",
    "workflows/inspection_e2e",
    "examples/inspection-snapshot",
    "AGENTS.md",
    "ARCHITECTURE.md",
    "CODE_OF_CONDUCT.md",
    "CONTRIBUTING.md",
    "MAINTAINERS.md",
    "ROADMAP.md",
    "SECURITY.md",
    "SUPPORT.md",
    "VISION.md",
    "transist-release.minisign.pub",
}
if not isinstance(forbidden_paths, list):
    print("FAIL: archive_contents.forbidden_paths must be an array", file=sys.stderr)
    fail += 1
else:
    if set(forbidden_paths) != expected_forbidden:
        print(
            f"FAIL: forbidden_paths must be exactly {sorted(expected_forbidden)}",
            file=sys.stderr,
        )
        fail += 1
    for rel in required_files or []:
        for entry in forbidden_paths:
            entry = entry.rstrip("/")
            if rel == entry or rel.startswith(entry + "/"):
                print(f"FAIL: required file '{rel}' is forbidden by forbidden_paths", file=sys.stderr)
                fail += 1
    for tree in tracked_trees or []:
        for entry in forbidden_paths:
            entry = entry.rstrip("/")
            if tree == entry or tree.startswith(entry + "/"):
                print(f"FAIL: tracked tree '{tree}' overlaps forbidden path '{entry}'", file=sys.stderr)
                fail += 1

# Retained-tree README files are source-only; the archive carries only the
# distribution root README.md.
if contents.get("exclude_readmes_in_trees") is not True:
    print("FAIL: archive_contents.exclude_readmes_in_trees must be true", file=sys.stderr)
    fail += 1
for rel in required_files or []:
    if rel.endswith("/README.md") or rel.endswith("README.md") and "/" in rel:
        print(f"FAIL: required file '{rel}' is a retained-tree README; only the root README.md may ship", file=sys.stderr)
        fail += 1

with open(doc_path, encoding="utf-8") as stream:
    doc = stream.read()

for tree in (retained_trees or []) + (tracked_trees or []):
    if tree not in doc:
        print(f"FAIL: archive tree '{tree}' is not documented in {doc_path}", file=sys.stderr)
        fail += 1

expected_release_contents_docs = [
    "README.md",
    "scripts/ci/distribution-README.md",
    "docs/public/reference/supported-platforms.md",
    "docs/public/reference/release-artifact-verification.md",
    "docs/public/reference/compatibility-and-stability.md",
    "docs/public/reference/reproducibility.md",
    "docs/public/guides/getting-started/installation-and-local-setup.md",
    "docs/public/guides/getting-started/quickstart.md",
    "docs/public/guides/trust/trusted-execution-walkthrough.md",
    "docs/public/guides/authoring/ai-assisted-authoring-with-repo-skills.md",
    ".agents/skills/transist-runtime-setup/SKILL.md",
    ".agents/skills/transist-runtime-setup/references/runtime-installation.md",
]
if release_contents_docs != expected_release_contents_docs:
    print("FAIL: archive_contents.release_contents_docs must match the canonical release documentation list", file=sys.stderr)
    fail += 1
else:
    obsolete_claims = [
        "not included in release archives",
        "not shipped in release archives",
        ".agents/skills directory will not be present",
    ]
    for rel in release_contents_docs:
        path = Path(repo_root) / rel
        if not path.is_file():
            print(f"FAIL: declared release contents document '{rel}' does not exist", file=sys.stderr)
            fail += 1
            continue
        text = path.read_text(encoding="utf-8")
        if ".agents/skills/" not in text:
            print(f"FAIL: declared release contents document '{rel}' does not mention .agents/skills/", file=sys.stderr)
            fail += 1
        for claim in obsolete_claims:
            if claim in text:
                print(f"FAIL: declared release contents document '{rel}' contains obsolete claim '{claim}'", file=sys.stderr)
                fail += 1

for rel in [
    "docs/public/reference/supported-platforms.md",
    "docs/public/reference/release-artifact-verification.md",
]:
    text = (Path(repo_root) / rel).read_text(encoding="utf-8")
    if "ancillary guidance, not a runtime, API, or schema contract" not in text:
        print(f"FAIL: {rel} must classify skills as ancillary guidance outside runtime/API/schema contracts", file=sys.stderr)
        fail += 1

# Every inventory target triple and archive name must be documented.
distribution_doc = Path(repo_root) / "scripts/ci/distribution-README.md"
distribution_targets = set(re.findall(
    r"\b[a-z0-9_]+-(?:unknown|apple)-[a-z0-9_-]+\b",
    distribution_doc.read_text(encoding="utf-8"),
))
if distribution_targets != target_triples:
    print(
        f"FAIL: distribution README targets must match the release inventory: "
        f"missing={sorted(target_triples - distribution_targets)}, "
        f"extra={sorted(distribution_targets - target_triples)}",
        file=sys.stderr,
    )
    fail += 1

for entry in targets:
    triple = entry.get("target_triple") or ""
    if triple and triple not in doc:
        print(f"FAIL: {triple} is in the release inventory but not documented in {doc_path}", file=sys.stderr)
        fail += 1
    archive_file = entry.get("archive_file") or ""
    if archive_file and archive_file not in doc:
        print(
            f"FAIL: archive '{archive_file}' is in the release inventory "
            f"but not documented in {doc_path}",
            file=sys.stderr,
        )
        fail += 1

for required in linux_required_files or []:
    if required not in doc:
        print(f"FAIL: Linux-only archive member '{required}' is not documented in {doc_path}", file=sys.stderr)
        fail += 1

# No extra documented release targets or archive names beyond the inventory.
doc_targets = re.findall(r"\|\s*([a-z0-9_]+-(?:unknown|apple)-[a-z0-9_-]+)\s*\|", doc)
for target in doc_targets:
    if not any(entry.get("target_triple") == target for entry in targets):
        print(f"FAIL: {doc_path} documents target '{target}' that is not in the release inventory", file=sys.stderr)
        fail += 1

doc_archives = re.findall(r"transist-<tag>-([a-z0-9_-]+)\.tar\.gz", doc)
for archive_file in {f"transist-<tag>-{t}.tar.gz" for t in doc_archives}:
    if archive_file not in archive_files:
        print(f"FAIL: {doc_path} documents archive '{archive_file}' that is not in the release inventory", file=sys.stderr)
        fail += 1

sys.exit(1 if fail else 0)
PY
python_ok=$?

if [[ "$fail" -ne 0 || "$python_ok" -ne 0 ]]; then
  echo "Release archive inventory checks failed." >&2
  exit 1
fi

echo "Release archive inventory checks passed."
