#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""DCO regression tests using isolated local repositories; no network or user config."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check_commit_signoffs.sh")


class CommitSignoffs(unittest.TestCase):
    """Exercise the actual shell script against real commit ranges and failures."""

    def setUp(self) -> None:
        """Initialize a temporary Git repository with a single unsigned base commit."""
        self.directory = tempfile.TemporaryDirectory(prefix="transist-dco-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.env = dict(
            os.environ,
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_AUTHOR_NAME="Test",
            GIT_AUTHOR_EMAIL="test@example.invalid",
            GIT_COMMITTER_NAME="Test",
            GIT_COMMITTER_EMAIL="test@example.invalid",
        )
        self.git("init", "-q")
        self.base = self.commit("unsigned base")

    def git(self, *args: str) -> str:
        """Run fixture-local Git, disabling hooks and signing without changing config."""
        return subprocess.check_output(
            [
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                *args,
            ],
            cwd=self.root,
            env=self.env,
            text=True,
            stderr=subprocess.PIPE,
        ).strip()

    def commit(self, body: str) -> str:
        """Create a fixture-only empty commit and return its object ID."""
        self.git("commit", "-q", "--allow-empty", "-m", body)
        return self.git("rev-parse", "HEAD")

    def check(
        self,
        base: str | None = None,
        head: str = "HEAD",
        env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        """Invoke the real DCO script, capturing both diagnostics and exit status."""
        return subprocess.run(
            ["bash", str(SCRIPT), base or self.base, head],
            cwd=self.root,
            env=env or self.env,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_signed_range_and_empty_range(self) -> None:
        """Only the selected range is checked, and an empty valid range passes."""
        self.assertEqual(self.check().returncode, 0)
        self.commit("feature\n\nSigned-off-by: Test <test@example.invalid>")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DCO check passed", result.stdout)

    def test_every_commit_and_exact_signoff_spelling(self) -> None:
        """A later signed commit cannot hide any unsigned or malformed predecessor."""
        for message in (
            "unsigned",
            "signed-off-by: Test <test@example.invalid>",
            "Signed-off-by: Test <no-email>",
        ):
            with self.subTest(message=message):
                base = self.git("rev-parse", "HEAD")
                unsigned = self.commit("feature\n\n" + message)
                self.commit("signed\n\nSigned-off-by: Test <test@example.invalid>")
                result = self.check(base)
                self.assertEqual(result.returncode, 1)
                self.assertIn(unsigned, result.stderr)

    def test_invalid_refs_and_shallow_history_fail(self) -> None:
        """Missing/option-like refs and incomplete history cannot pass as empty ranges."""
        for base, head in (
            ("missing-base", "HEAD"),
            (self.base, "missing-head"),
            ("--all", "HEAD"),
        ):
            with self.subTest(base=base, head=head):
                self.assertNotEqual(self.check(base, head).returncode, 0)
        (self.root / ".git/shallow").write_text(self.base + "\n", encoding="ascii")
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("complete local history", result.stderr)

    def test_disconnected_history_fails(self) -> None:
        """A valid but unrelated base is not accepted as evidence of a PR range."""
        self.git("checkout", "--orphan", "unrelated")
        self.commit("signed orphan\n\nSigned-off-by: Test <test@example.invalid>")
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("common ancestor", result.stderr)

    def test_traversal_failure_propagates(self) -> None:
        """A failed rev-list is an error, rather than an empty passing process substitution."""
        executable = shutil.which("git")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        stub = bin_dir / "git"
        stub.write_text(
            f'#!/bin/sh\nif [ "$1" = rev-list ]; then exit 43; fi\nexec "{executable}" "$@"\n',
            encoding="utf-8",
        )
        stub.chmod(0o755)
        result = self.check(
            env=dict(self.env, PATH=str(bin_dir) + os.pathsep + self.env["PATH"])
        )
        self.assertEqual(result.returncode, 43)
        self.assertNotIn("DCO check passed", result.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
