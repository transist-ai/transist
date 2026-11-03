#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Verify every link inside GitHub issue-form templates resolves.

GitHub renders the `markdown` body blocks of issue-form templates and
resolves relative links against the template's own directory
(`.github/ISSUE_TEMPLATE/`), so `../../SECURITY.md` points at the repository
root. This checker extracts every `[text](target)` from each committed
`.github/ISSUE_TEMPLATE/*.yml`, skips URL/anchor-only targets, resolves the
rest relative to the template directory, and fails when any target is missing
or escapes the repository.

Exit status: 0 (all links resolve) or 1 (broken or escaping link).
"""

import pathlib
import re
import sys
from urllib.parse import urlparse


def main(argv):
    repo_root = pathlib.Path(argv[1] if len(argv) > 1 else ".").resolve()
    template_dir = repo_root / ".github" / "ISSUE_TEMPLATE"
    if not template_dir.is_dir():
        print(f"FAIL: issue template directory not found: {template_dir}", file=sys.stderr)
        return 1

    link_pattern = re.compile(r"\]\(([^)]*)\)")
    broken = []

    template_files = sorted(set(template_dir.glob("*.yml")) | set(template_dir.glob("*.yaml")))
    for yml_file in template_files:
        try:
            text = yml_file.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            broken.append(f"{yml_file.relative_to(repo_root)} (unreadable: {exc})")
            continue
        for match in link_pattern.finditer(text):
            target = match.group(1).strip().strip("<>")
            if not target or target.startswith("#"):
                continue
            if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target):
                parsed = urlparse(target)
                if parsed.scheme not in {"http", "https", "mailto"}:
                    broken.append(f"{yml_file.relative_to(repo_root)} -> {target} (unsupported URL scheme)")
                elif parsed.scheme in {"http", "https"} and not parsed.netloc:
                    broken.append(f"{yml_file.relative_to(repo_root)} -> {target} (URL has no host)")
                elif parsed.scheme == "mailto" and not parsed.path:
                    broken.append(f"{yml_file.relative_to(repo_root)} -> {target} (mailto URL has no address)")
                continue
            resolved = (template_dir / target).resolve()
            try:
                resolved.relative_to(repo_root)
            except ValueError:
                broken.append(
                    f"{yml_file.relative_to(repo_root)} -> {target} (escapes repository root)"
                )
                continue
            if not resolved.exists():
                broken.append(f"{yml_file.relative_to(repo_root)} -> {target}")

    if broken:
        print("FAIL: issue-template links that do not resolve:", file=sys.stderr)
        for entry in sorted(set(broken)):
            print(f"  {entry}", file=sys.stderr)
        return 1
    print(f"OK: every issue-template link resolves under {template_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
