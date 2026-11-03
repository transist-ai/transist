#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Regression tests for the repository-local catalog boundary."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from check_repository_self_contained import (
    PLUGIN_CATALOG,
    ROOT,
    WORKFLOW_CATALOG,
    check_repository,
    check_text,
)


class RepositorySelfContainedTests(unittest.TestCase):
    """Keep external catalogs out while allowing the internal plugin crate."""

    def test_current_checkout(self) -> None:
        self.assertEqual(check_repository(ROOT), [])

    def test_external_urls_paths_and_prose_are_rejected(self) -> None:
        for catalog in (WORKFLOW_CATALOG, PLUGIN_CATALOG):
            for text in (
                f"https://github.com/transist-ai/{catalog}",
                f"git@github.com:transist-ai/{catalog}.git",
                f"${{ROOT}}/../{catalog}",
                f"/workspace/{catalog}/plugins/example/plugin.yaml",
                f"{catalog}/scripts/check_fixtures.py",
                f'ROOT.parent / "{catalog}"',
                f'ROOT.parent.joinpath("{catalog}")',
                f'root.join("{catalog}")',
                f'Path("{catalog}")',
                f"Use the `{catalog}` repository.",
                f"Checkout {catalog}",
            ):
                with self.subTest(text=text):
                    self.assertTrue(check_text("README.md", text))

    def test_local_crate_and_bundled_resources_are_allowed(self) -> None:
        for text in (
            '"crates/' + PLUGIN_CATALOG + '"',
            f"crates/{PLUGIN_CATALOG}/src/lib.rs",
            f'ROOT / "crates/{PLUGIN_CATALOG}"',
            f'root.join("crates/{PLUGIN_CATALOG}")',
            f"cargo test --locked -p {PLUGIN_CATALOG}",
            f"use {PLUGIN_CATALOG.replace('-', '_')}::PluginManifest;",
            f"### `{PLUGIN_CATALOG}`",
            "plugins/shell/plugin.yaml",
            "workflows/demos/getting_started/workflow.yaml",
            'ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"',
            "https://github.com/actions/checkout",
        ):
            with self.subTest(text=text):
                self.assertEqual(check_text("fixture.txt", text), [])

    def test_repository_override_is_rejected(self) -> None:
        for kind in ("WORKFLOWS", "PLUGINS"):
            for suffix in ("REPO", "DIR", "ROOT"):
                with self.subTest(kind=kind, suffix=suffix):
                    self.assertTrue(
                        check_text("run.sh", f"TRANSIST_{kind}_{suffix}=/checkout")
                    )

    def test_diagnostics_identify_line_without_echoing_content(self) -> None:
        failures = check_text("guide.md", f"heading\n../{PLUGIN_CATALOG} secret\n")
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("guide.md:2:"))
        self.assertNotIn("secret", failures[0])

    def test_git_inventory_includes_hidden_tracked_and_new_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", directory], check=True)
            (root / ".gitignore").write_text("target/\n", encoding="utf-8")
            tracked = root / ".agents/skills/example.md"
            tracked.parent.mkdir(parents=True)
            tracked.write_text(f"../{PLUGIN_CATALOG}\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            (root / "new.md").write_text(WORKFLOW_CATALOG, encoding="utf-8")
            (root / "target").mkdir()
            (root / "target/generated.md").write_text(
                WORKFLOW_CATALOG, encoding="utf-8"
            )
            (root / "binary").write_bytes(b"\0" + WORKFLOW_CATALOG.encode())
            failures = check_repository(root)
            self.assertEqual(len(failures), 2)
            self.assertTrue(
                any(line.startswith(".agents/skills/example.md:1:") for line in failures)
            )
            self.assertTrue(
                any(line.startswith("new.md:1:") for line in failures)
            )
            tracked.unlink()
            self.assertEqual(len(check_repository(root)), 1)

    def test_symlinks_are_rejected_without_reading_the_target(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", directory], check=True)
            (root / "link").symlink_to("missing-target")
            self.assertEqual(
                check_repository(root),
                ["link: symlink is not a self-contained source input"],
            )

    def test_ci_and_release_gates_keep_the_check(self) -> None:
        for script in ("run_maintainability_checks.sh", "check_release_candidate.sh"):
            with self.subTest(script=script):
                self.assertIn(
                    "python3 scripts/ci/check_repository_self_contained.py",
                    (ROOT / "scripts/ci" / script).read_text(),
                )
        self.assertIn(
            "python3 scripts/ci/test_repository_self_contained.py",
            (ROOT / "scripts/ci/run_maintainability_checks.sh").read_text(),
        )


if __name__ == "__main__":
    unittest.main()
