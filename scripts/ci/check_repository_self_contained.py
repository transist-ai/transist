#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Reject references to separate catalogs in source, docs, skills and CI inputs.

The plugin workspace crate is local and intentionally retains its package name.
This is a catalog-boundary check, not a ban on normal dependency/tool downloads.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_CATALOG = "transist-" + "workflows"
PLUGIN_CATALOG = "transist-" + "plugins"
PATTERNS = (
    re.compile(rf"\b{WORKFLOW_CATALOG}\b", re.IGNORECASE),
    # Qualified repository URLs/paths, but not crates/<plugin-crate>/src, etc.
    re.compile(rf"(?<!crates)/{PLUGIN_CATALOG}\b", re.IGNORECASE),
    re.compile(rf"(?<!crates/){PLUGIN_CATALOG}/", re.IGNORECASE),
    re.compile(
        rf"(?:/\s*|\b(?:Path|join|joinpath)\(\s*)['\"]{PLUGIN_CATALOG}['\"]",
        re.IGNORECASE,
    ),
    re.compile(
        rf"\b(?:repo(?:sitory)?|checkout|catalog)\s+[`'\"]?{PLUGIN_CATALOG}\b",
        re.IGNORECASE,
    ),
    re.compile(
        rf"\b{PLUGIN_CATALOG}[`'\"]?\s+(?:repo(?:sitory)?|checkout|catalog)\b",
        re.IGNORECASE,
    ),
    re.compile(r"\bTRANSIST_(?:WORKFLOWS|PLUGINS)_(?:REPO|DIR|ROOT)\b"),
)


def check_text(path: str, text: str) -> list[str]:
    """Return line-numbered catalog-reference diagnostics without echoing data."""
    return [
        f"{path}:{number}: external catalog reference; use bundled resources"
        for number, line in enumerate(text.splitlines(), 1)
        if any(pattern.search(line) for pattern in PATTERNS)
    ]


def check_repository(root: Path) -> list[str]:
    """Scan tracked and non-ignored new files without following symlinks.

    Git supplies the inventory, so build outputs and local runtime homes are not
    inputs. Deleted files and binary payloads are not scanned as text. Git errors
    propagate to the caller rather than producing an empty passing inventory.
    """
    inventory = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=root,
    )
    failures: list[str] = []
    for name in sorted(set(inventory.decode("utf-8").split("\0")) - {""}):
        failures.extend(check_text(name, name))
        path = root / name
        if path.is_symlink():
            failures.append(f"{name}: symlink is not a self-contained source input")
            continue
        if not path.exists():
            continue
        if not path.is_file():
            failures.append(f"{name}: source input must be a regular file, not a submodule")
            continue
        data = path.read_bytes()
        if b"\0" in data:
            continue
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            continue
        failures.extend(check_text(name, text))
    return failures


def main() -> int:
    """Check this checkout and fail closed on inventory or file-read errors."""
    try:
        failures = check_repository(ROOT)
    except (OSError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f"Cannot check repository self-containment: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("Repository catalog references are self-contained.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
