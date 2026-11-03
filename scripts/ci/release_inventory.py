#!/usr/bin/env python3
"""Expand the canonical release inventory into exact archive members.

Members come from three sources in ``archive_contents``:

- ``required_files`` and ``linux_required_files``: explicit files that must be
  present (binaries, legal notices, the distribution README, provenance).
- ``retained_trees``: directories copied recursively into the archive. Every
  regular tree member is represented; ``README.md`` files are excluded
  recursively (``exclude_readmes_in_trees``) because the distribution README is
  the only Markdown README allowed in an archive.
- ``tracked_trees``: complete Git-backed trees copied at their tracked paths.
  Markdown and README files are retained.
- Parent directories of every explicit file are included so the archive has a
  complete directory skeleton.

``forbidden_paths`` lists paths that must never appear in an archive (source
repository documentation, developer trees, governance files, private keys,
etc.). A forbidden path is never enumerated, so arbitrary files dropped under a
forbidden path cannot leak into the expected member set.
"""

import json
import os
import subprocess
import sys

README_BASENAME = "README.md"


def target_metadata(inventory_path, target):
    """Return the canonical metadata for one released target triple.

    Args:
        inventory_path: Path to ``release-archive-inventory.json``.
        target: Rust target triple declared in ``released_archives``.

    Raises:
        ValueError: If the target is absent from the canonical inventory.
    """
    with open(inventory_path, encoding="utf-8") as stream:
        inventory = json.load(stream)
    for entry in inventory["released_archives"]:
        if entry.get("target_triple") == target:
            return entry
    raise ValueError(f"target '{target}' is not declared in the release inventory")


def is_forbidden(rel_path, forbidden):
    """Return True when a repository-relative path is on the forbidden list."""
    rel = rel_path.rstrip("/")
    for entry in forbidden:
        entry = entry.rstrip("/")
        if rel == entry or rel.startswith(entry + "/"):
            return True
    return False


def expected_members(inventory_path, repo_root, package_dir, target):
    with open(inventory_path, encoding="utf-8") as stream:
        inventory = json.load(stream)

    contents = inventory["archive_contents"]
    forbidden = set(contents.get("forbidden_paths", []))
    files = set(contents.get("required_files", []))
    if target_metadata(inventory_path, target)["includes_sandbox_launcher"]:
        files.update(contents.get("linux_required_files", []))

    members = {package_dir}

    def add_file(rel):
        if is_forbidden(rel, forbidden):
            return
        members.add(os.path.join(package_dir, rel))
        parent = os.path.dirname(rel)
        while parent and parent != ".":
            members.add(os.path.join(package_dir, parent))
            parent = os.path.dirname(parent)

    # Explicit file requirements (binaries, legal/provenance/docs).
    for rel in files:
        add_file(rel)

    # Recursively retained trees: every regular file except README.md.
    exclude_readmes = contents.get("exclude_readmes_in_trees", True)
    for source_root in contents.get("retained_trees", []):
        root = os.path.join(repo_root, source_root)
        if not os.path.isdir(root):
            continue
        # Parent directories of a retained tree are part of the archive
        # skeleton (e.g. examples/ for examples/quickstart/).
        parent = os.path.dirname(source_root)
        while parent and parent != ".":
            members.add(os.path.join(package_dir, parent))
            parent = os.path.dirname(parent)
        for dirpath, dirnames, filenames in os.walk(root):
            rel_dir = os.path.relpath(dirpath, repo_root).replace(os.sep, "/")
            if is_forbidden(rel_dir, forbidden):
                dirnames[:] = []
                continue
            dirnames[:] = [name for name in dirnames if not is_forbidden(os.path.join(rel_dir, name), forbidden)]
            members.add(os.path.join(package_dir, rel_dir))
            for dirname in dirnames:
                members.add(os.path.join(package_dir, rel_dir, dirname))
            for filename in filenames:
                if exclude_readmes and filename == README_BASENAME:
                    continue
                members.add(os.path.join(package_dir, rel_dir, filename))

    # Complete Git-backed trees: only tracked, regular non-symlink files ship.
    # Do not apply retained-tree README filtering to these ancillary assets.
    for source_root in contents.get("tracked_trees", []):
        result = subprocess.run(
            ["git", "-C", repo_root, "ls-files", "-z", "--", f"{source_root}/**"],
            check=True,
            stdout=subprocess.PIPE,
        )
        tracked_paths = [path.decode("utf-8") for path in result.stdout.split(b"\0") if path]
        if not tracked_paths:
            raise ValueError(f"tracked tree '{source_root}' contains no Git-tracked files")
        for rel in tracked_paths:
            source = os.path.join(repo_root, rel)
            if os.path.islink(source) or not os.path.isfile(source):
                raise ValueError(f"tracked tree member '{rel}' must be a regular non-symlink file")
            add_file(rel)

    return sorted(members)


if __name__ == "__main__":
    if sys.argv[1:2] == ["target-field"]:
        if len(sys.argv) != 5:
            raise SystemExit("usage: release_inventory.py target-field INVENTORY TARGET FIELD")
        value = target_metadata(sys.argv[2], sys.argv[3]).get(sys.argv[4])
        if value is None:
            raise SystemExit(f"target field '{sys.argv[4]}' is not declared")
        if isinstance(value, bool):
            print(str(value).lower())
        else:
            print(value)
    else:
        if len(sys.argv) != 5:
            raise SystemExit("usage: release_inventory.py INVENTORY REPO_ROOT PACKAGE_DIR TARGET")
        print("\n".join(expected_members(*sys.argv[1:])))
