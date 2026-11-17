#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Render public release text from an already verified exact-source checkout."""

from pathlib import Path
import posixpath
import re
import subprocess
from urllib.parse import quote, urlsplit


def render(root: Path, repository: str, version: str) -> dict[str, str]:
    """Return a final title/body from versioned notes and the changelog heading.

    The caller must verify the checkout's commit and tree first. Local Markdown
    links are resolved within that checkout and pinned to the final immutable
    tag. Missing notes, ambiguous headings and escaping links fail closed.
    """
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid release repository")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("invalid release version")

    def source(path: str) -> str:
        # HEAD was bound to the candidate by the caller. Never consume generated
        # or locally edited notes left behind by build/package steps.
        return subprocess.check_output(
            ["git", "show", f"HEAD:{path}"], cwd=root, text=True
        )

    heading = re.findall(
        r"^## \[" + re.escape(version) + r"\](?: — ([^\n]+))?\s*$",
        source("CHANGELOG.md"),
        re.MULTILINE,
    )
    if len(heading) != 1:
        raise ValueError("missing or ambiguous release changelog heading")
    notes_path = f"docs/public/releases/{version}.md"
    notes = source(notes_path)
    lines = notes.splitlines()
    if not lines or lines[0] != f"# Transist {version}":
        raise ValueError("release notes version heading mismatch")
    base = f"https://github.com/{repository}/blob/v{version}/"

    def destination(value: str) -> str:
        parts = urlsplit(value)
        if parts.scheme or parts.netloc:
            if parts.scheme not in ("https", "http", "mailto"):
                raise ValueError("unsupported release note link scheme")
            return value
        path = (
            posixpath.normpath(
                posixpath.join(posixpath.dirname(notes_path), parts.path)
            )
            if parts.path
            else notes_path
        )
        if (
            value.startswith("/")
            or path.startswith("../")
            or path == ".."
            or "\\" in value
        ):
            raise ValueError("release note link escapes repository")
        resolved = (root / path).resolve()
        if not resolved.is_relative_to(root.resolve()) or not resolved.exists():
            raise ValueError("release note link target missing or outside checkout")
        return (
            base
            + quote(path, safe="/.-_")
            + ("?" + parts.query if parts.query else "")
            + ("#" + parts.fragment if parts.fragment else "")
        )

    body: list[str] = []
    fence = None
    for line in lines[1:]:
        marker = re.match(r"^\s*(`{3,}|~{3,})", line)
        if marker:
            if fence is None:
                fence = marker[1][0]
            elif marker[1][0] == fence:
                fence = None
        if fence is None and not marker:
            # Preserve code spans: their examples are not Markdown destinations.
            spans = re.split(r"(`+[^`]*`+)", line)
            for index in range(0, len(spans), 2):
                spans[index] = re.sub(
                    r"(\]\()([^\s()]+)(\))",
                    lambda m: m[1] + destination(m[2]) + m[3],
                    spans[index],
                )
                spans[index] = re.sub(
                    r"^(\s*\[[^\]]+\]:\s*)(\S+)",
                    lambda m: m[1] + destination(m[2]),
                    spans[index],
                )
            line = "".join(spans)
        body.append(line)
    text = "\n".join(body).strip()
    if not text:
        raise ValueError("release notes are empty")
    text += (
        f"\n\n[Full release notes]({base}{notes_path}) · "
        f"[Artifact verification]({base}docs/public/reference/release-artifact-verification.md)"
    )
    title = f"Transist v{version}" + (f" — {heading[0]}" if heading[0] else "")
    if len(title) > 256 or len(text.encode("utf-8")) > 120000:
        raise ValueError("release metadata exceeds GitHub limits")
    return {"name": title, "body": text}
