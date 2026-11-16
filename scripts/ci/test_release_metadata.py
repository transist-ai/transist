#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exact-source public release text, link pinning and fail-closed fixtures."""

from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from release_metadata import render

VERSION = "1.2.3"


class ReleaseMetadataTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "CHANGELOG.md").write_text(
            "## [Unreleased]\n\n## [1.2.3] — Diagnostics\n"
        )
        self.notes = self.root / "docs/public/releases/1.2.3.md"
        self.notes.parent.mkdir(parents=True)
        target = self.root / "docs/public/reference/app.md"
        target.parent.mkdir()
        target.write_text("# Handshakes\n")
        self.notes.write_text(
            f"# Transist {VERSION}\n\nFixes.\n\n[APP](../reference/app.md#handshakes)\n"
        )
        self.source_calls = []

        def source(command: list[str], **kwargs) -> str:
            self.assertEqual(command[:2], ["git", "show"])
            self.assertEqual(kwargs, {"cwd": self.root, "text": True})
            self.assertTrue(command[2].startswith("HEAD:"))
            self.source_calls.append(command[2])
            return (self.root / command[2].removeprefix("HEAD:")).read_text()

        mock = patch("release_metadata.subprocess.check_output", side_effect=source)
        mock.start()
        self.addCleanup(mock.stop)

    def test_final_title_and_version_pinned_links(self) -> None:
        value = render(self.root, "owner/repo", "1.2.3")
        self.assertEqual(value["name"], f"Transist v{VERSION} — Diagnostics")
        self.assertIn(
            "https://github.com/owner/repo/blob/v1.2.3/docs/public/reference/app.md#handshakes",
            value["body"],
        )
        self.assertNotIn(f"# Transist {VERSION}", value["body"])
        self.assertIn("[Full release notes]", value["body"])
        self.assertEqual(
            self.source_calls,
            ["HEAD:CHANGELOG.md", "HEAD:docs/public/releases/1.2.3.md"],
        )

    def test_external_reference_anchor_and_code_links(self) -> None:
        self.notes.write_text(
            f"# Transist {VERSION}\n\n[Site](https://example.com)\n"
            "[here](#details)\n[ref]: ../reference/app.md\n"
            "`[example](missing.md)`\n```md\n[example](missing.md)\n```\n"
        )
        body = render(self.root, "owner/repo", "1.2.3")["body"]
        self.assertIn("[Site](https://example.com)", body)
        self.assertIn("releases/1.2.3.md#details", body)
        self.assertIn("[ref]: https://github.com/owner/repo/blob/v1.2.3/", body)
        self.assertEqual(body.count("[example](missing.md)"), 2)

    def test_invalid_notes_or_links_fail_closed(self) -> None:
        for text in (
            f"# Transist {VERSION}-wrong\nFixes",
            f"# Transist {VERSION}\n",
            f"# Transist {VERSION}\n[bad](../../../../outside.md)",
            f"# Transist {VERSION}\n[bad](missing.md)",
            f"# Transist {VERSION}\n[bad](javascript:bad)",
        ):
            with self.subTest(text=text):
                self.notes.write_text(text)
                with self.assertRaises(ValueError):
                    render(self.root, "owner/repo", "1.2.3")

    def test_duplicate_or_missing_changelog_heading(self) -> None:
        for changelog in ("## [Unreleased]\n", "## [1.2.3]\n\n## [1.2.3]\n"):
            (self.root / "CHANGELOG.md").write_text(changelog)
            with self.assertRaises(ValueError):
                render(self.root, "owner/repo", "1.2.3")


if __name__ == "__main__":
    unittest.main()
