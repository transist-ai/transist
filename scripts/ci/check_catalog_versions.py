#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Enforce release 0.1.0 in the maintained bundled catalog and shared plugins.

Schema identifiers remain v1. Runtime crate versions are independent. Signed
admin_tooling/YAML goldens, negative inputs, provenance, and historical releases
are deliberately outside this catalog; never rewrite them to satisfy this check.
Catalog documents require unquoted top-level schema/version/plugins keys; quoted
values remain supported. Reject quoted keys rather than silently skipping pins.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RELEASE = "0.1.0"
SCHEMA = re.compile(
    r"^\s*(?:schema|['\"]schema['\"])\s*:\s*"
    r"['\"]?(transist\.(flow|module|plugin)\.v\d+)['\"]?\s*(?:#.*)?$"
)
FIELD = re.compile(r"^(schema|version):\s*(.*?)\s*(?:#.*)?$")
PIN = re.compile(r"^\s+([^\s:]+):\s*(.*?)\s*(?:#.*)?$")
QUOTED_KEY = re.compile(r"^['\"](?:schema|version|plugins)['\"]\s*:")


def runtime_version(root: Path) -> str:
    """Read the workspace runtime release without changing the catalog policy."""
    with (root / "Cargo.toml").open("rb") as manifest:
        return tomllib.load(manifest)["workspace"]["package"]["version"]


def scalar(value: str) -> str:
    """Return an unquoted catalog scalar without interpreting YAML types."""
    return value.strip().strip("'\"")


def check_document(path: Path, root: Path) -> list[str]:
    """Check a catalog document's release, schema, and exact plugin pins."""
    lines = path.read_text(encoding="utf-8").splitlines()
    schemas = [match for line in lines if (match := SCHEMA.fullmatch(line))]
    if not schemas:
        return []  # Config, inputs, and unrelated YAML are not release artifacts.
    failures: list[str] = []
    relative = path.relative_to(root)
    for number, line in enumerate(lines, 1):
        if QUOTED_KEY.match(line):
            failures.append(f"{relative}:{number}: use unquoted catalog keys")
    fields = {
        match.group(1): scalar(match.group(2))
        for line in lines
        if (match := FIELD.fullmatch(line))
    }
    expected_schema = f"transist.{schemas[0].group(2)}.v1"
    if fields.get("schema") != expected_schema:
        failures.append(f"{relative}: schema must remain {expected_schema}")
    if fields.get("version") != RELEASE:
        failures.append(f"{relative}: release version must be {RELEASE}")
    in_plugins = False
    for number, line in enumerate(lines, 1):
        if line.startswith("plugins:"):
            in_plugins = True
            if line.partition(":")[2].strip() not in ("", "{}"):
                failures.append(
                    f"{relative}:{number}: use a block map of exact plugin pins"
                )
            continue
        if in_plugins and line and not line[0].isspace() and not line.startswith("#"):
            in_plugins = False
        if in_plugins and (match := PIN.fullmatch(line)):
            if scalar(match.group(2)) != RELEASE:
                failures.append(
                    f"{relative}:{number}: plugin {match.group(1)} must pin {RELEASE}"
                )
    return failures


def check_catalog(root: Path) -> list[str]:
    """Discover maintained bundle YAML and return version-policy violations."""
    paths = {
        path
        for directory in (
            root / "plugins",
            root / "workflows",
            root / "examples",
            root / "tests/manual",
            root / "tests/fixtures/plugins",
            root / ".agents/testing/fixtures/plugins",
        )
        for pattern in ("*.yaml", "*.yml")
        for path in directory.rglob(pattern)
    }
    return [failure for path in sorted(paths) for failure in check_document(path, root)]


def main() -> int:
    """Check this checkout's catalog and print actionable diagnostics."""
    failures = check_catalog(ROOT)
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(
        f"Catalog releases and plugin pins are {RELEASE}; schemas remain v1; "
        f"runtime release is independently {runtime_version(ROOT)}."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
