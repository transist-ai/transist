#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Hermetic candidate-worktree regressions using real Git and the real guard."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check_candidate_worktree.sh").resolve()
TOOLS = ("markdown", "redocly")


class CandidateWorktree(unittest.TestCase):
    """Check generated-output boundaries without builds, downloads, or user config."""

    def setUp(self) -> None:
        """Commit a minimal fixture; all Git writes stay in the temporary repository."""
        self.directory = tempfile.TemporaryDirectory(prefix="transist-candidate-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("GIT_")
        }
        self.env.update(
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_AUTHOR_NAME="Test",
            GIT_AUTHOR_EMAIL="test@example.invalid",
            GIT_COMMITTER_NAME="Test",
            GIT_COMMITTER_EMAIL="test@example.invalid",
            GIT_CONFIG_COUNT="3",
            GIT_CONFIG_KEY_0="core.hooksPath",
            GIT_CONFIG_VALUE_0=os.devnull,
            GIT_CONFIG_KEY_1="commit.gpgsign",
            GIT_CONFIG_VALUE_1="false",
            GIT_CONFIG_KEY_2="core.excludesFile",
            GIT_CONFIG_VALUE_2=os.devnull,
        )
        self.git("init", "-q", "--template=")
        self.write(".gitignore", "other/node_modules/\n")
        for tool in TOOLS:
            self.write(
                f"scripts/ci/{tool}/.gitignore",
                "node_modules/\nnode_modules-cache/\n.local.config\naccess.token\n",
            )
            self.write(f"scripts/ci/{tool}/package.json", "{}\n")
        self.git("add", ".")
        self.git("commit", "-q", "-m", "candidate fixture")

    def git(self, *args: str) -> str:
        """Run fixture-only Git with isolated configuration and capture its output."""
        return subprocess.check_output(
            ["git", *args],
            cwd=self.root,
            env=self.env,
            text=True,
            stderr=subprocess.PIPE,
            timeout=10,
        ).strip()

    def write(self, path: str, contents: str = "fixture\n") -> Path:
        """Create a fixture file and return its path for cleanup or modification."""
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(contents, encoding="utf-8")
        return destination

    def check(self) -> subprocess.CompletedProcess[str]:
        """Invoke the actual guard against the fixture and capture status/diagnostics."""
        return subprocess.run(
            ["bash", str(SCRIPT), str(self.root)],
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def generated_trees(self) -> None:
        """Populate both exact generated roots, including secret-like dependency names."""
        for tool in TOOLS:
            self.write(f"scripts/ci/{tool}/node_modules/package/index.js")
            self.write(
                f"scripts/ci/{tool}/node_modules/tokenizer/index.js",
                "const token = 'fixture';\n",
            )
            self.write(f"scripts/ci/{tool}/node_modules/.cache/nested/output")

    def assert_dirty(self, category: str, path: str) -> None:
        """Require rejection with the expected dirty category and offending path."""
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(category, result.stderr)
        self.assertIn(path, result.stderr)
        self.assertNotIn("PASS", result.stdout)

    def test_clean_committed_tree_passes(self) -> None:
        """A clean baseline passes before any generated outputs are present."""
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("PASS", result.stdout)

    def test_each_generated_ignored_tree_passes(self) -> None:
        """Markdown and Redocly each pass alone, and together, as ignored trees."""
        for tool in TOOLS:
            with self.subTest(tool=tool):
                path = f"scripts/ci/{tool}/node_modules/package/index.js"
                self.write(path)
                try:
                    self.assertEqual(self.git("check-ignore", "--", path), path)
                    result = self.check()
                    self.assertEqual(result.returncode, 0, result.stderr)
                finally:
                    shutil.rmtree(self.root / f"scripts/ci/{tool}/node_modules")
        self.generated_trees()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_adjacent_ignored_config_and_token_fail(self) -> None:
        """Pruning generated trees never hides adjacent ignored configuration or tokens."""
        self.generated_trees()
        for tool in TOOLS:
            for name in (".local.config", "access.token"):
                with self.subTest(tool=tool, name=name):
                    path = f"scripts/ci/{tool}/{name}"
                    ignored = self.write(path)
                    self.assertEqual(self.git("check-ignore", "--", path), path)
                    self.assert_dirty(
                        "Ignored (outside generated-output allowlist):", path
                    )
                    ignored.unlink()

    def test_similar_ignored_directories_fail(self) -> None:
        """Only the two exact node_modules roots pass, not prefixes or other locations."""
        for directory in (
            "scripts/ci/markdown/node_modules-cache",
            "scripts/ci/redocly/node_modules-cache",
            "other/node_modules",
        ):
            with self.subTest(directory=directory):
                self.write(f"{directory}/index.js")
                try:
                    self.assert_dirty(
                        "Ignored (outside generated-output allowlist):", directory
                    )
                finally:
                    shutil.rmtree(self.root / directory)

    def test_staged_change_fails_with_generated_trees(self) -> None:
        """Generated outputs cannot mask a staged tracked change."""
        self.generated_trees()
        path = "scripts/ci/markdown/package.json"
        self.write(path, '{"changed": true}\n')
        self.git("add", "--", path)
        self.assert_dirty("Staged:", path)

    def test_unstaged_change_fails_with_generated_trees(self) -> None:
        """Generated outputs cannot mask an unstaged tracked change."""
        self.generated_trees()
        path = "scripts/ci/markdown/package.json"
        self.write(path, '{"changed": true}\n')
        self.assert_dirty("Unstaged:", path)

    def test_untracked_file_fails_with_generated_trees(self) -> None:
        """Generated outputs cannot mask an ordinary untracked input."""
        self.generated_trees()
        path = "scripts/ci/markdown/new.config"
        self.write(path)
        self.assert_dirty("Untracked:", path)


if __name__ == "__main__":
    unittest.main(verbosity=2)
