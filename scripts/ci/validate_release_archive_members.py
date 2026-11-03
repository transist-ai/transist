#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Validate release archive members without extracting the archive."""

from __future__ import annotations

import re
import sys
import tarfile
from pathlib import Path

from release_inventory import expected_members


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
INVENTORY_PATH = SCRIPT_DIR / "release-archive-inventory.json"
VERSION_RE = re.compile(r"^v?[0-9]+\.[0-9]+\.[0-9]+$")
ALLOWED_TYPES = {tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE}


class ValidationError(Exception):
    """Report an archive that is unsafe or differs from the inventory."""


def package_directory(target: str, version_or_tag: str) -> str:
    """Return the expected top-level package directory for a release."""
    if not VERSION_RE.fullmatch(version_or_tag):
        raise ValidationError("version must match [v]<major>.<minor>.<patch>")
    version = version_or_tag.removeprefix("v")
    return f"transist-v{version}-{target}"


def supported_targets() -> set[str]:
    """Return target triples declared by the canonical inventory."""
    import json

    try:
        with INVENTORY_PATH.open(encoding="utf-8") as stream:
            inventory = json.load(stream)
        return {
            entry["target_triple"] for entry in inventory["released_archives"]
        }
    except (OSError, KeyError, TypeError, ValueError) as error:
        raise ValidationError(f"cannot read canonical inventory: {error}") from error


def normalized_member_name(name: str) -> str:
    """Validate a member path and remove only its trailing slash spelling."""
    if not name:
        raise ValidationError("archive contains an empty member name")
    if "\\" in name:
        raise ValidationError(f"archive member uses ambiguous backslashes: {name!r}")
    if name.startswith("/") or re.match(r"^[A-Za-z]:/", name):
        raise ValidationError(f"archive member has an absolute path: {name!r}")

    normalized = name.rstrip("/")
    if not normalized:
        raise ValidationError(f"archive member has an absolute path: {name!r}")
    components = normalized.split("/")
    if any(component in {"", ".", ".."} for component in components):
        raise ValidationError(f"archive member has an unsafe path: {name!r}")
    return normalized


def validate_archive(target: str, version_or_tag: str, archive_path: Path) -> None:
    """Validate tar metadata and exact canonical membership before extraction."""
    targets = supported_targets()
    if target not in targets:
        raise ValidationError(f"unsupported target triple: {target}")

    package_dir = package_directory(target, version_or_tag)
    try:
        with tarfile.open(archive_path, mode="r:gz") as archive:
            members = archive.getmembers()
    except (OSError, tarfile.TarError) as error:
        raise ValidationError(f"archive is unreadable: {error}") from error

    if not members:
        raise ValidationError("archive is empty")

    actual: set[str] = set()
    for member in members:
        name = normalized_member_name(member.name)
        if name in actual:
            raise ValidationError(f"archive contains duplicate member: {name!r}")
        actual.add(name)

        if member.type not in ALLOWED_TYPES:
            raise ValidationError(f"archive member is not a regular file or directory: {name!r}")
        if name != package_dir and not name.startswith(f"{package_dir}/"):
            raise ValidationError(f"archive member is outside expected top directory: {name!r}")

    expected = set(
        expected_members(
            str(INVENTORY_PATH), str(REPO_ROOT), package_dir, target
        )
    )
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing or extra:
        details = []
        if missing:
            details.append(f"missing {len(missing)} member(s): {missing[0]!r}")
        if extra:
            details.append(f"extra {len(extra)} member(s): {extra[0]!r}")
        raise ValidationError("; ".join(details))


def main(argv: list[str]) -> int:
    """Run the command-line archive member validator."""
    if len(argv) != 4:
        print(
            "FAIL: usage: python3 scripts/ci/validate_release_archive_members.py "
            "<target-triple> <version-or-tag> <archive-path>",
            file=sys.stderr,
        )
        return 2

    target, version_or_tag, archive = argv[1:]
    try:
        validate_archive(target, version_or_tag, Path(archive))
    except ValidationError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1

    print(f"PASS: release archive members are valid for {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
