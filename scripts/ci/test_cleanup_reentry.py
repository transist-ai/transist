#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Hermetic cleanup-refusal composition tests using built source CLI/admin.

Run after building transist-cli and transist-admin. Optional --transist-bin and
--admin-bin select binaries. Only fresh scratch homes and disposable fixture keys
are used; no user trust material, network service or shared runtime home is read.
"""

import argparse
from pathlib import Path
import shutil
import subprocess
import time
import unittest

import run_shipped_workflows as driver

ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / "target/debug/transist"
ADMIN = ROOT / "target/debug/transist-admin"


class CleanupReentryTests(unittest.TestCase):
    """Real trust refresh precedes refusal without primary or cleanup dispatch."""

    def admin(self, session: driver.Session, args: list[str]) -> None:
        """Run admin solely on disposable test inputs outside the runtime home."""
        result = subprocess.run(
            [str(ADMIN), *args],
            cwd=session.workspace,
            env=session.env(),
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def setup_mode(self, session: driver.Session, mode: str) -> None:
        """Install one failed-primary/failed-cleanup fixture with exact authority."""
        governed = mode == "governed"
        trusted = mode != "untrusted"
        session.cli(
            ["home", "init", "--mode", "governed" if governed else "standalone"]
        )
        source = session.temp_dir / "source"
        source.mkdir()
        plugin = source / "shell"
        shutil.copytree(ROOT / "plugins/shell", plugin)
        flow = source / "workflow.yaml"
        flow.write_text(
            "schema: transist.flow.v1\nid: cleanup_reentry\nversion: 0.1.0\n"
            "workspace:\n  path: '" + str(session.workspace) + "'\n  access: full\n"
            "network:\n  mode: none\nplugins:\n  shell: 0.1.0\nsteps:\n"
            "  - id: evidence\n    uses: process:shell/exec\n    with:\n"
            "      command: sh\n      args: ['-c', 'echo retained > \"$TRANSIST_EVIDENCE_DIR/proof.txt\"']\n"
            "  - id: primary\n    uses: process:shell/exec\n    with:\n"
            "      command: sh\n      args: ['-c', 'echo primary >> primary.log; exit 17']\n"
            "cleanup:\n  - id: teardown\n    when: [failed]\n"
            "    uses: process:shell/exec\n    with:\n      command: sh\n"
            "      args: ['-c', 'echo cleanup >> cleanup.log; test -f cleanup.release']\n",
            encoding="utf-8",
        )
        trust_args = ["--trusted-execution"] if trusted else []
        if trusted:
            key = source / "fixture.key"
            public = source / "fixture.pub"
            self.admin(
                session,
                [
                    "key",
                    "generate",
                    "--name",
                    "cleanup-reentry-fixture",
                    "--private-key-file",
                    str(key),
                ],
            )
            self.admin(
                session,
                [
                    "key",
                    "export-public",
                    "--private-key-file",
                    str(key),
                    "--trust-root-file",
                    str(public),
                ],
            )
            self.admin(
                session, ["plugin", "sign", str(plugin), "--private-key-file", str(key)]
            )
            package = source / "package"
            self.admin(
                session,
                [
                    "workflow",
                    "package",
                    str(flow),
                    "--version",
                    "0.1.0",
                    "--private-key-file",
                    str(key),
                    "--output",
                    str(package),
                ],
            )
            session.cli(["trust", "import", str(public)])
            install = ["workflows", "install", "--package", str(package), "--approve"]
        else:
            install = ["workflows", "install", "--path", str(flow), "--approve"]
        session.cli(
            ["plugins", "install", "--path", str(plugin / "plugin.yaml"), *trust_args]
        )
        session.cli([*install, *trust_args])
        if governed:
            session.cli(
                ["workflows", "authorize", "cleanup_reentry@0.1.0", "--approve"]
            )
        launch = session.cli(
            [
                "workflows",
                "run",
                "cleanup_reentry@0.1.0",
                "--run-ref",
                "fixture",
                *trust_args,
            ],
            must=False,
        )
        self.assertEqual(launch.returncode, 0, launch.stdout + launch.stderr)
        session.wait_terminal("fixture", "failed", timeout=30)
        session.wait_runner_finished("fixture")
        self.assertEqual(
            session.state("fixture")["runtime_mode"],
            "governed" if governed else "standalone",
        )

    def assert_refused(
        self, session: driver.Session, recovery: str, trusted: bool
    ) -> None:
        """Assert real refusal, audit refresh and byte-identical durable history."""
        before = session.state("fixture")
        self.assertIs(before["trusted_execution_mode"], trusted)
        self.assertEqual(before["recovery_state"], recovery)
        self.assertEqual(before["steps"]["primary"]["executions"], 1)
        self.assertEqual(before["steps"]["evidence"]["executions"], 1)
        self.assertTrue(before["steps"]["evidence"]["last_result"]["evidence"])
        if trusted:
            self.assertEqual(before["trust_decision"]["decision"], "allowed")
            # Make actual refresh observable even on a coarse clock. Same-ms
            # acceptance is covered separately by the assertion's unit matrix.
            deadline = time.monotonic() + 2
            while int(time.time() * 1000) <= max(
                before["updated_at_ms"], before["trust_decision"]["decided_at_ms"]
            ):
                self.assertLess(time.monotonic(), deadline, "clock did not advance")
                time.sleep(0.001)
        run_root = session.state_path("fixture").parent
        history = driver.recovery_history_snapshot(run_root)
        workspace = {
            p.name: p.read_bytes() for p in session.workspace.iterdir() if p.is_file()
        }
        # Omit --trusted-execution to exercise persisted sticky trust, including
        # standalone. Governed also enables trust through the home policy.
        result = session.cli(["runs", "resume", "fixture"], must=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cleanup has begun", (result.stdout + result.stderr).lower())
        after = session.state("fixture")
        driver.assert_rejected_resume_state_unchanged(before, after)
        self.assertEqual(driver.recovery_history_snapshot(run_root), history)
        self.assertEqual(
            {
                p.name: p.read_bytes()
                for p in session.workspace.iterdir()
                if p.is_file()
            },
            workspace,
        )
        events = (run_root / "events.jsonl").read_text(encoding="utf-8")
        self.assertNotIn('"run_resumed"', events)
        if trusted:
            self.assertGreater(after["updated_at_ms"], before["updated_at_ms"])
            self.assertGreater(
                after["trust_decision"]["decided_at_ms"],
                before["trust_decision"]["decided_at_ms"],
            )
        else:
            self.assertEqual(before, after)

    def test_refusal_in_all_modes_before_and_after_cleanup_completion(self) -> None:
        """Both durable recovery guards compose with real signature verification."""
        for mode in ("untrusted", "trusted", "governed"):
            with self.subTest(mode=mode):
                session = driver.Session(ROOT, BINARY, True)
                try:
                    self.setup_mode(session, mode)
                    state = session.state("fixture")
                    self.assertEqual(state["cleanup"]["state"], "failed")
                    self.assertEqual(
                        state["cleanup"]["steps"]["teardown"]["executions"], 1
                    )
                    self.assert_refused(
                        session, "cleanup_required", mode != "untrusted"
                    )
                    (session.workspace / "cleanup.release").touch()
                    session.cli(["runs", "cleanup", "fixture"])
                    session.wait_runner_finished("fixture")
                    state = session.state("fixture")
                    self.assertEqual(state["cleanup"]["state"], "succeeded")
                    self.assertEqual(
                        state["cleanup"]["steps"]["teardown"]["executions"], 2
                    )
                    self.assert_refused(
                        session, "cleanup_completed", mode != "untrusted"
                    )
                finally:
                    session.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--transist-bin", type=Path, default=BINARY)
    parser.add_argument("--admin-bin", type=Path, default=ADMIN)
    args, remaining = parser.parse_known_args()
    BINARY = args.transist_bin.resolve()
    ADMIN = args.admin_bin.resolve()
    unittest.main(argv=[__file__, *remaining])
