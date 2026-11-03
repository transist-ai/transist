#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Verify packaged Markdown doc links.

A relative link is checked whenever both the linking file and the resolved
target would be Markdown/text documentation; links to external URLs, mailto:,
and in-page anchors are skipped. A link that resolves to a path missing from
the package, or that escapes the package root entirely, is a packaging defect
and fails the check.

Usage: check_doc_links.py <package-dir>
Exit status: 0 (all links resolve inside the package) or 1 (broken/escaping link).
"""

import pathlib
import re
import sys


def main(argv):
    pkg = pathlib.Path(argv[1]).resolve()
    link_pattern = re.compile(r"\]\(([^)]+)\)")
    broken = []

    for md_file in pkg.rglob("*.md"):
        try:
            text = md_file.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for match in link_pattern.finditer(text):
            target = match.group(1).strip()
            if not target:
                continue
            if target.startswith("#"):
                continue
            if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target):
                # Has a URL scheme (http:, https:, mailto:, etc.) - skip.
                continue
            # Strip an in-page anchor and any surrounding angle brackets.
            target = target.split("#", 1)[0].strip("<>")
            if not target:
                continue
            resolved = (md_file.parent / target).resolve()
            try:
                resolved.relative_to(pkg)
            except ValueError:
                # A link that escapes the package root would point outside the
                # shipped artifact and is a packaging defect, not a skip.
                broken.append(f"{md_file.relative_to(pkg)} -> {target} (escapes archive root)")
                continue
            if not resolved.exists():
                broken.append(f"{md_file.relative_to(pkg)} -> {target}")

    if broken:
        print("FAIL: packaged docs link to missing or archive-escaping paths:", file=sys.stderr)
        for entry in sorted(set(broken)):
            print(f"  {entry}", file=sys.stderr)
        return 1
    print(f"OK: no broken packaged-doc links found under {pkg}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))