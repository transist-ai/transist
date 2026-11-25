#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise main DCO validation in isolated Git repositories."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check_main_dco.sh")
SIGNED = "Signed-off-by: Test Person <test@example.invalid>"


class MainDcoTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="transist-main-dco-")
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

    def commit(self, message: str) -> str:
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def check(
        self, before: str, after: str, env: dict | None = None
    ) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", str(SCRIPT), before, after],
            cwd=self.root,
            env=env or self.env,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_signed_squash_excludes_unsigned_before(self) -> None:
        after = self.commit("Squash feature\n\nSummary of changes.\n\n" + SIGNED)
        result = self.check(self.base, after)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_all_new_commits_are_checked(self) -> None:
        first = self.commit("unsigned one")
        self.commit("signed\n\n" + SIGNED)
        last = self.commit("unsigned two")
        result = self.check(self.base, last)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(first, result.stderr)
        self.assertIn(last, result.stderr)

    def test_body_and_malformed_trailers_do_not_count(self) -> None:
        for trailer in (
            SIGNED + "\n\nMore body text.",
            "signed-off-by: Test <test@example.invalid>",
            "Signed-off-by:   <test@example.invalid>",
            "Signed-off-by: Test <missing-at>",
            "Signed-off-by: Test <@example.invalid>",
            "Signed-off-by: Test <test@>",
            "Signed-off-by: Test <test@@example.invalid>",
            "Signed-off-by: Test <test user@example.invalid>",
            "Signed-off-by: <Test> <test@example.invalid>",
            "Signed-off-by: Test <test@example.invalid> extra",
        ):
            with self.subTest(trailer=trailer):
                before = self.git("rev-parse", "HEAD")
                after = self.commit("feature\n\n" + trailer)
                result = self.check(before, after)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(after, result.stderr)

    def test_one_valid_trailer_is_sufficient(self) -> None:
        after = self.commit("feature\n\nSigned-off-by: invalid\n" + SIGNED)
        result = self.check(self.base, after)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_invalid_endpoints_and_nonforward_ranges(self) -> None:
        after = self.commit("feature\n\n" + SIGNED)
        blob = self.git("hash-object", "-w", "--stdin")
        for before, end in (
            ("0" * 40, after),
            (self.base, "0" * 40),
            ("HEAD", after),
            (self.base[:8], after),
            ("f" * 40, after),
            (self.base, "f" * 40),
            (blob, after),
            (self.base, blob),
            (self.base, self.base),
            (after, self.base),
        ):
            with self.subTest(before=before, after=end):
                self.assertNotEqual(self.check(before, end).returncode, 0)
        self.git("checkout", "--orphan", "unrelated")
        unrelated = self.commit("orphan\n\n" + SIGNED)
        self.assertNotEqual(self.check(self.base, unrelated).returncode, 0)

    def test_git_failures_do_not_pass(self) -> None:
        after = self.commit("feature\n\n" + SIGNED)
        executable = shutil.which("git")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        stub = bin_dir / "git"
        for command in ("rev-list", "show", "interpret-trailers"):
            with self.subTest(command=command):
                stub.write_text(
                    f'#!/bin/sh\nif [ "$1" = "{command}" ]; then exit 43; fi\n'
                    f'exec "{executable}" "$@"\n',
                    encoding="utf-8",
                )
                stub.chmod(0o755)
                result = self.check(
                    self.base,
                    after,
                    dict(self.env, PATH=str(bin_dir) + os.pathsep + self.env["PATH"]),
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("DCO check passed", result.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
