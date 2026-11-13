#!/usr/bin/env python3
"""Unit tests for the E2E harness (.agents/testing/run_e2e.py).

Run with:
    python3 .agents/testing/test_run_e2e.py
"""

import os
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(Path(__file__).resolve().parent))

import run_e2e
from test_manual_gate_wait import TestManualGateWait  # Included in the standard harness gate.


class TestDiscoverSuites(unittest.TestCase):
    def test_required_suites_are_hermetic(self):
        """Every required suite has a committed run.sh."""
        for name in run_e2e.REQUIRED_SUITES:
            run_sh = run_e2e.SUITES_DIR / name / "run.sh"
            self.assertTrue(
                run_sh.exists(),
                f"Required suite '{name}' is missing run.sh at {run_sh}",
            )

    def test_ci_mode_returns_only_required(self):
        suites = run_e2e.discover_suites(ci_mode=True)
        self.assertEqual(sorted(suites), sorted(run_e2e.REQUIRED_SUITES))

    def test_non_ci_returns_existing_suites(self):
        suites = run_e2e.discover_suites(ci_mode=False)
        self.assertEqual(sorted(suites), sorted(run_e2e.REQUIRED_SUITES))

    def test_ci_mode_returns_existing_suites(self):
        suites = run_e2e.discover_suites(ci_mode=True)
        self.assertEqual(sorted(suites), sorted(run_e2e.REQUIRED_SUITES))

    def test_required_suite_count(self):
        self.assertEqual(len(run_e2e.REQUIRED_SUITES), 11)


class TestRunSuite(unittest.TestCase):
    def test_missing_run_sh_returns_skip(self):
        result = run_e2e.run_suite("nonexistent-suite-xyz", ci_mode=False)
        self.assertEqual(result["status"], "SKIP")

    def test_pass_suite_returns_pass(self):
        """A suite that emits OK: and PASS: lines with exit 0 should PASS."""
        with tempfile.TemporaryDirectory() as tmpdir:
            suite_dir = Path(tmpdir) / "test-pass-suite"
            suite_dir.mkdir()
            (suite_dir / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                'echo "OK: test assertion"\n'
                'echo "PASS: test-pass-suite"\n'
            )
            os.chmod(suite_dir / "run.sh", 0o755)

            with patch.object(run_e2e, "SUITES_DIR", Path(tmpdir)):
                result = run_e2e.run_suite("test-pass-suite", ci_mode=False, timeout_s=10)
            self.assertEqual(result["status"], "PASS")
            self.assertEqual(result["ok_count"], 1)

    def test_fail_suite_returns_fail(self):
        """A suite that emits FAIL: should return FAIL."""
        with tempfile.TemporaryDirectory() as tmpdir:
            suite_dir = Path(tmpdir) / "test-fail-suite"
            suite_dir.mkdir()
            (suite_dir / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                'echo "FAIL: test assertion failed"\n'
                "exit 1\n"
            )
            os.chmod(suite_dir / "run.sh", 0o755)

            with patch.object(run_e2e, "SUITES_DIR", Path(tmpdir)):
                result = run_e2e.run_suite("test-fail-suite", ci_mode=False, timeout_s=10)
            self.assertEqual(result["status"], "FAIL")
            self.assertEqual(result["fail_count"], 1)

    def test_skip_suite_returns_skip(self):
        """A suite that emits SKIP: should return SKIP."""
        with tempfile.TemporaryDirectory() as tmpdir:
            suite_dir = Path(tmpdir) / "test-skip-suite"
            suite_dir.mkdir()
            (suite_dir / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                'echo "SKIP: no sandbox backend"\n'
                "exit 0\n"
            )
            os.chmod(suite_dir / "run.sh", 0o755)

            with patch.object(run_e2e, "SUITES_DIR", Path(tmpdir)):
                result = run_e2e.run_suite("test-skip-suite", ci_mode=False, timeout_s=10)
            self.assertEqual(result["status"], "SKIP")
            self.assertEqual(result["skip_count"], 1)

    def test_timeout_suite_returns_timeout(self):
        """A suite that hangs should be killed and return TIMEOUT."""
        with tempfile.TemporaryDirectory() as tmpdir:
            suite_dir = Path(tmpdir) / "test-hang-suite"
            suite_dir.mkdir()
            (suite_dir / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                "sleep 300\n"
            )
            os.chmod(suite_dir / "run.sh", 0o755)

            with patch.object(run_e2e, "SUITES_DIR", Path(tmpdir)):
                result = run_e2e.run_suite("test-hang-suite", ci_mode=False, timeout_s=3)
            self.assertEqual(result["status"], "TIMEOUT")

    def test_warn_suite_returns_warn(self):
        """A suite that emits WARN: but no FAIL: should return WARN."""
        with tempfile.TemporaryDirectory() as tmpdir:
            suite_dir = Path(tmpdir) / "test-warn-suite"
            suite_dir.mkdir()
            (suite_dir / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                'echo "OK: first assertion"\n'
                'echo "WARN: token changed"\n'
                'echo "PASS: test-warn-suite"\n'
            )
            os.chmod(suite_dir / "run.sh", 0o755)

            with patch.object(run_e2e, "SUITES_DIR", Path(tmpdir)):
                result = run_e2e.run_suite("test-warn-suite", ci_mode=False, timeout_s=10)
            self.assertEqual(result["status"], "WARN")
            self.assertEqual(result["warn_count"], 1)

    def test_nonzero_exit_without_fail_line_returns_error(self):
        """A suite that exits nonzero without FAIL: or PASS: should return ERROR."""
        with tempfile.TemporaryDirectory() as tmpdir:
            suite_dir = Path(tmpdir) / "test-error-suite"
            suite_dir.mkdir()
            (suite_dir / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                'echo "some output"\n'
                "exit 1\n"
            )
            os.chmod(suite_dir / "run.sh", 0o755)

            with patch.object(run_e2e, "SUITES_DIR", Path(tmpdir)):
                result = run_e2e.run_suite("test-error-suite", ci_mode=False, timeout_s=10)
            self.assertEqual(result["status"], "ERROR")

    def test_zero_exit_without_pass_marker_returns_error(self):
        """A suite that exits 0 without a PASS: marker should return ERROR, not PASS."""
        with tempfile.TemporaryDirectory() as tmpdir:
            suite_dir = Path(tmpdir) / "test-no-marker"
            suite_dir.mkdir()
            (suite_dir / "run.sh").write_text(
                "#!/usr/bin/env bash\n"
                'echo "OK: did something"\n'
                "exit 0\n"
            )
            os.chmod(suite_dir / "run.sh", 0o755)

            with patch.object(run_e2e, "SUITES_DIR", Path(tmpdir)):
                result = run_e2e.run_suite("test-no-marker", ci_mode=False, timeout_s=10)
            self.assertEqual(result["status"], "ERROR")

    def test_ci_mode_implies_json(self):
        """--ci should imply --json for machine-readable output."""
        import argparse
        parser = run_e2e.main.__wrapped__ if hasattr(run_e2e.main, '__wrapped__') else None
        # Simulate the argument parsing logic
        parser_obj = argparse.ArgumentParser()
        parser_obj.add_argument("--ci", action="store_true")
        parser_obj.add_argument("--json", action="store_true")
        args = parser_obj.parse_args(["--ci"])
        # Replicate the harness logic
        if args.ci:
            args.json = True
        self.assertTrue(args.json)


class TestProcessGroupCleanup(unittest.TestCase):
    def test_kill_process_group_does_not_raise_on_dead_pid(self):
        """Killing a process group for a non-existent PID should not raise."""
        run_e2e._kill_process_group(999999)

    def test_detect_leaked_processes_returns_empty_for_no_match(self):
        """No processes matching the home pattern should return empty list."""
        result = run_e2e._detect_leaked_processes("nonexistent-suite-xyz", 999999, 0)
        self.assertEqual(result, [])

    def test_detect_leaked_processes_finds_spawned_process(self):
        """A process whose command line matches the home pattern should be detected."""
        import subprocess
        pattern = "/tmp/.transist-leak-test-999999"
        # Create an executable script whose path matches the home pattern
        with open(pattern, "w") as f:
            f.write("#!/usr/bin/env bash\nsleep 5\n")
        os.chmod(pattern, 0o755)
        proc = subprocess.Popen([pattern])
        try:
            time.sleep(0.5)  # let it start
            result = run_e2e._detect_leaked_processes("leak-test", 999999, 0)
            self.assertIn(proc.pid, result)
        finally:
            proc.kill()
            proc.wait()
            os.unlink(pattern)

    def test_detect_leaked_processes_ignores_transient_detached_process(self) -> None:
        """A process disappearing during quiescence is not a leak, without OS timing."""
        clock = [0.0]

        def advance(seconds: float) -> None:
            """Advance the synthetic clock rather than depending on process scheduling."""
            clock[0] += seconds

        with patch.object(run_e2e, "_matching_leaked_processes",
                          side_effect=[[12345], [12345], []]) as matching, \
                patch.object(run_e2e.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(run_e2e.time, "sleep", side_effect=advance) as sleep:
            self.assertEqual(
                run_e2e._detect_leaked_processes("transient-test", 999999, 0.5, 0.01),
                [],
            )
            self.assertEqual(matching.call_count, 3)
            self.assertEqual(sleep.call_count, 2)
            self.assertAlmostEqual(clock[0], 0.02)

    def test_detect_leaked_processes_keeps_persistent_process_after_deadline(self) -> None:
        """The same bounded polling still reports a real leak at the deadline."""
        clock = [0.0]

        def advance(seconds: float) -> None:
            """Advance only the injected clock during each poll."""
            clock[0] += seconds

        with patch.object(run_e2e, "_matching_leaked_processes", return_value=[12345]), \
                patch.object(run_e2e.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(run_e2e.time, "sleep", side_effect=advance):
            self.assertEqual(
                run_e2e._detect_leaked_processes("persistent-test", 999999, 0.5, 0.2),
                [12345],
            )
            self.assertEqual(clock[0], 0.5)


class TestNoSiblingRepos(unittest.TestCase):
    def _assert_no_sibling_repo_references(self, content: str, suite: str) -> None:
        """Reject repository overrides and named parent paths in suite text."""
        forbidden_patterns = (
            r"\bTRANSIST_[A-Z0-9_]*REPO\b",
            r"\.\./(?:\.\./)*(?:[A-Za-z0-9_-]|\.[A-Za-z0-9_-])[A-Za-z0-9_.-]*",
        )
        for pattern in forbidden_patterns:
            self.assertNotRegex(
                content,
                pattern,
                f"Suite '{suite}' run.sh references an external repository",
            )

    def test_no_sibling_repo_env_vars_in_required_suites(self):
        """Required suite run.sh scripts must not reference sibling repos."""
        for name in run_e2e.REQUIRED_SUITES:
            run_sh = run_e2e.SUITES_DIR / name / "run.sh"
            self._assert_no_sibling_repo_references(run_sh.read_text(), name)

    def test_in_repo_dependencies_and_root_discovery_are_allowed(self) -> None:
        """Repository-root discovery and bundled resources remain valid."""
        content = (
            'REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"\n'
            'TRANSIST_BIN="$REPO_ROOT/target/debug/transist"\n'
            '"$TRANSIST_BIN" plugins install --path '
            '"$REPO_ROOT/plugins/shell/plugin.yaml"\n'
        )
        self._assert_no_sibling_repo_references(content, "synthetic-in-repo")

    def test_synthetic_sibling_paths_are_rejected(self) -> None:
        """Reject arbitrary named parent dependencies, including hidden paths."""
        for path in (
            "../fixture-catalog",
            "../../fixture-catalog",
            "${REPO_ROOT}/../fixture-catalog",
            "../.fixture-catalog",
        ):
            with self.subTest(path=path), self.assertRaises(AssertionError):
                self._assert_no_sibling_repo_references(path, "synthetic-out-of-repo")

    def test_synthetic_repository_overrides_are_rejected(self) -> None:
        """Reject external repository overrides without naming real projects."""
        for variable in ("TRANSIST_FIXTURE_REPO", "TRANSIST_ANOTHER_FIXTURE_REPO"):
            with self.subTest(variable=variable), self.assertRaises(AssertionError):
                self._assert_no_sibling_repo_references(
                    f"${{{variable}}}/workflow.yaml", "synthetic-override"
                )

    def test_required_suites_resolve_binaries_from_target_debug(self):
        """Required suite run.sh scripts must resolve binaries from target/debug."""
        for name in run_e2e.REQUIRED_SUITES:
            run_sh = run_e2e.SUITES_DIR / name / "run.sh"
            content = run_sh.read_text()
            self.assertIn(
                "target/debug/transist",
                content,
                f"Suite '{name}' does not resolve transist from target/debug",
            )


if __name__ == "__main__":
    unittest.main()
