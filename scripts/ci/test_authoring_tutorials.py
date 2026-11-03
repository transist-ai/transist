#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Regress human tutorials against committed sources and the real local CLI.

Run all groups, or select docs/workflow/plugin with --group. Requires a built
CLI (default target/debug/transist; override with --transist-bin). No services,
credentials, signing keys, Python dependencies, or shared runtime home are used.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
EXAMPLES = ROOT / "examples/authoring"
BINARY = ROOT / "target/debug/transist"
DOCS = {
    "workflow": ROOT / "docs/public/guides/authoring/advanced-workflow-authoring.md",
    "plugin": ROOT / "docs/public/guides/plugins/write-your-first-plugin.md",
}
FILE_BLOCK = re.compile(
    r"<!-- tutorial-file: ([^\n]+) -->\n```[^\n]*\n(.*?)```", re.DOTALL
)


class DocumentationTests(unittest.TestCase):
    """Keep complete copyable file blocks and shell syntax executable."""

    def test_file_blocks_match_every_source(self) -> None:
        """Fail on missing/duplicate/stale blocks, not just syntax errors."""
        for group, doc in DOCS.items():
            blocks = FILE_BLOCK.findall(doc.read_text(encoding="utf-8"))
            names = [name for name, _ in blocks]
            sources = {
                path.relative_to(EXAMPLES / group).as_posix()
                for path in (EXAMPLES / group).rglob("*")
                if path.is_file() and "__pycache__" not in path.parts
            }
            with self.subTest(group=group):
                self.assertEqual(len(names), len(set(names)), "Duplicate file block")
                self.assertEqual(set(names), sources)
            for name, text in blocks:
                with self.subTest(group=group, file=name):
                    self.assertEqual(
                        text, (EXAMPLES / group / name).read_text(encoding="utf-8")
                    )

    def test_shell_blocks_parse(self) -> None:
        """Check each command block, including heredocs and polling helpers."""
        for group, doc in DOCS.items():
            blocks = re.findall(r"```bash\n(.*?)```", doc.read_text(), re.DOTALL)
            self.assertTrue(blocks)
            for index, block in enumerate(blocks):
                with self.subTest(group=group, block=index):
                    result = subprocess.run(
                        ["bash", "-n"],
                        input=block,
                        text=True,
                        capture_output=True,
                        timeout=10,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_cleanup_retains_unsettled_or_unreadable_home(self) -> None:
        """Execute documented guards; failed inspection must never delete data."""
        for group, doc in DOCS.items():
            cleanup = [
                block
                for block in re.findall(
                    r"```bash\n(.*?)```", doc.read_text(), re.DOTALL
                )
                if 'rm -rf -- "$TUTORIAL_ROOT"' in block
            ]
            self.assertEqual(len(cleanup), 1)
            scenarios = (
                ("success", "null", "0", True),
                ("blocked", "null", "0", False),
                ("success", '{"owner":"still-live"}', "0", False),
                ("success", "null", "1", False),
                ("invalid-json", "null", "0", False),
            )
            for status, lease, fail_query, removed in scenarios:
                with self.subTest(
                    group=group, status=status, lease=lease, error=fail_query
                ):
                    with tempfile.TemporaryDirectory(
                        prefix="tutorial-guard-", dir="/tmp"
                    ) as tmp:
                        root = Path(tmp)
                        project = root / "project"
                        project.mkdir()
                        (project / "keep.txt").write_text("retained evidence\n")
                        stub = root / "transist"
                        stub.write_text(
                            '#!/bin/sh\n[ "$FAIL_QUERY" = 0 ] || exit 1\n'
                            'case "$*" in\n'
                            '  *lease*) printf "%s\\n" "$LEASE_JSON" ;;\n'
                            '  *) if [ "$STATUS" = invalid-json ]; then printf "bad-json"; '
                            'else printf \'{"status":"%s"}\\n\' "$STATUS"; fi ;;\n'
                            "esac\n"
                        )
                        stub.chmod(0o755)
                        env = dict(
                            os.environ,
                            TUTORIAL_ROOT=str(project),
                            PATH=f"{root}:{os.environ['PATH']}",
                            STATUS=status,
                            LEASE_JSON=lease,
                            FAIL_QUERY=fail_query,
                        )
                        result = subprocess.run(
                            ["bash", "-c", cleanup[0]],
                            env=env,
                            capture_output=True,
                            text=True,
                            timeout=10,
                        )
                        self.assertEqual(result.returncode == 0, removed, result.stderr)
                        self.assertEqual(project.exists(), not removed)


class RuntimeTests(unittest.TestCase):
    """Own an isolated home and bounded subprocesses for each CLI exercise."""

    def setUp(self) -> None:
        """Use a private sibling scratch directory outside the fallback workspace."""
        self.assertTrue(BINARY.is_file(), f"Build the CLI first: {BINARY}")
        self.work = Path(
            tempfile.mkdtemp(prefix="transist-authoring-", dir=ROOT.parent)
        )
        self.addCleanup(self.remove_settled_home)
        self.home = self.work / "home"
        self.home.mkdir()
        self.runs: list[str] = []
        self.cli("home", "init", "--mode", "standalone")

    def remove_settled_home(self) -> None:
        """Retain evidence if settlement fails; never delete beneath a live runner."""
        try:
            self.stop_active_runs()
        except Exception:
            print(f"Retaining unsettled tutorial home: {self.work}", file=sys.stderr)
            raise
        shutil.rmtree(self.work)

    def cli(self, *args: str, ok: bool = True) -> subprocess.CompletedProcess[str]:
        """Run one CLI command with explicit home, capturing actionable failures."""
        env = os.environ.copy()
        env["TRANSIST_HOME"] = str(self.home)
        result = subprocess.run(
            [str(BINARY), "--home", str(self.home), *args],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            timeout=30,
        )
        if ok:
            self.assertEqual(
                result.returncode, 0, f"{args}\n{result.stdout}\n{result.stderr}"
            )
        else:
            self.assertNotEqual(result.returncode, 0, f"Unexpected pass: {args}")
        return result

    def state(self, run: str) -> dict[str, Any]:
        """Read run state through public inspection, never edit persisted files."""
        return json.loads(self.cli("runs", "show", run, "--format", "json").stdout)

    def wait(self, run: str, expected: str) -> dict[str, Any]:
        """Poll at most 30 seconds and fail promptly on unexpected terminal state."""
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            state = self.state(run)
            if state["status"] == expected:
                lease = self.cli("runs", "lease", "inspect", run, "--format", "json")
                if json.loads(lease.stdout) is None:
                    return state
                time.sleep(0.1)
                continue
            self.assertNotIn(state["status"], {"success", "failed", "canceled"}, state)
            time.sleep(0.1)
        self.fail(f"{run} did not reach {expected}: {self.state(run)}")

    def stop_active_runs(self) -> None:
        """Cancel failed-test leftovers before temporary homes are removed."""
        for run in self.runs:
            if self.state(run)["status"] not in {"success", "failed", "canceled"}:
                state = self.state(run)
                if state["status"] == "blocked":
                    self.wait(run, "blocked")
                    self.cli(
                        "runs",
                        "gate",
                        "reject",
                        run,
                        "--actor",
                        "tutorial-test",
                        "--reason",
                        "Clean up failed synthetic test",
                    )
                    self.wait(run, "failed")
                else:
                    self.cli("runs", "cancel", run)
                    self.wait(run, "canceled")
            self.wait(run, self.state(run)["status"])

    def install_flow(self, directory: Path, ref: str) -> None:
        """Exercise source checks/explanation and immutable standalone installation."""
        flow = str(directory / "workflow.yaml")
        inputs = str(directory / "inputs.json")
        self.cli("workflows", "check", "--path", flow)
        self.cli("workflows", "check", "--path", flow, "--inputs-file", inputs)
        explanation = self.cli(
            "workflows",
            "explain",
            "--path",
            flow,
            "--inputs-file",
            inputs,
            "--format",
            "json",
        )
        self.assertIsInstance(json.loads(explanation.stdout), dict)
        self.assertFalse(list((self.home / "runs").glob("run_*")))
        self.cli("workflows", "install", "--path", flow, "--approve")
        self.cli("workflows", "check", ref, "--inputs-file", inputs)

    def launch(self, ref: str, run: str, inputs: Path) -> None:
        """Launch only the synthetic tutorial and register its cleanup."""
        # Register first: a timeout/error may occur after detached runner birth.
        # Failed inspection during teardown retains the whole directory safely.
        self.runs.append(run)
        self.cli(
            "workflows", "run", ref, "--run-ref", run, "--inputs-file", str(inputs)
        )


class WorkflowTests(RuntimeTests):
    """Prove output handoff and approval ordering using the bundled shell action."""

    def test_review_before_delivery(self) -> None:
        """Final action is pending until approval, then consumes the real draft."""
        directory = EXAMPLES / "workflow"
        ref = "tutorial.reviewed_greeting@0.1.0"
        self.cli(
            "plugins", "install", "--path", str(ROOT / "plugins/shell/plugin.yaml")
        )
        self.install_flow(directory, ref)
        self.launch(ref, "review-demo", directory / "inputs.json")
        state = self.wait("review-demo", "blocked")
        self.assertEqual(
            state["steps"]["draft"]["last_result"]["output"]["stdout"], "Hello, Ada"
        )
        self.assertEqual(state["steps"]["deliver"]["status"], "pending")
        self.cli(
            "runs",
            "gate",
            "approve",
            "review-demo",
            "greeting-review",
            "--actor",
            "Tutorial Reviewer <reviewer@example.com>",
            "--reason",
            "Reviewed the greeting; allow the final local echo",
        )
        state = self.wait("review-demo", "success")
        self.assertEqual(
            state["steps"]["deliver"]["last_result"]["output"]["stdout"],
            "Approved: Hello, Ada",
        )

    def test_wrong_type_creates_no_run(self) -> None:
        """A numeric name fails the bound source check without dispatch."""
        self.cli(
            "plugins", "install", "--path", str(ROOT / "plugins/shell/plugin.yaml")
        )
        inputs = self.work / "invalid.json"
        inputs.write_text('{"name":42}\n')
        self.cli(
            "workflows",
            "check",
            "--path",
            str(EXAMPLES / "workflow/workflow.yaml"),
            "--inputs-file",
            str(inputs),
            ok=False,
        )
        self.assertFalse(list((self.home / "runs").glob("run_*")))


class PluginTests(RuntimeTests):
    """Install a real package, validate its action, and test immutable iteration."""

    def setUp(self) -> None:
        """Copy the tutorial before changing bytes; install exactly selected files."""
        super().setUp()
        self.directory = self.work / "project"
        shutil.copytree(EXAMPLES / "plugin", self.directory)
        self.entrypoint = self.directory / "greeting/plugin.py"
        self.entrypoint.chmod(0o755)
        self.manifest = self.directory / "greeting/plugin.yaml"
        self.ref = "tutorial.plugin_greeting@0.1.0"
        self.cli("plugins", "install", "--path", str(self.manifest))
        self.cli("plugins", "list")
        self.cli("plugins", "inspect", "tutorial.greeting")
        installed = self.home / "plugins/tutorial.greeting/0.1.0"
        self.assertEqual(
            {
                p.relative_to(installed).as_posix()
                for p in installed.rglob("*")
                if p.is_file()
            },
            {
                "plugin.yaml",
                "plugin.py",
                "schemas/greet.input.json",
                "schemas/greet.output.json",
            },
        )
        self.install_flow(self.directory, self.ref)

    def test_greeting_and_action_failure(self) -> None:
        """Verify successful output and a safe plugin-level validation failure."""
        self.launch(self.ref, "plugin-demo", self.directory / "inputs.json")
        state = self.wait("plugin-demo", "success")
        self.assertEqual(state["steps"]["greet"]["status"], "succeeded")
        self.assertEqual(
            state["steps"]["greet"]["last_result"]["output"], {"message": "Hello, Ada!"}
        )
        inputs = self.work / "blank.json"
        inputs.write_text('{"name":"   "}\n')
        self.launch(self.ref, "plugin-blank", inputs)
        state = self.wait("plugin-blank", "failed")
        self.assertEqual(
            state["steps"]["greet"]["last_result"]["error"]["code"], "validation_error"
        )

    def test_collision_and_fresh_home_iteration(self) -> None:
        """Changed source cannot alter an installed version; fresh home sees edit."""
        self.entrypoint.write_text(
            self.entrypoint.read_text().replace(
                'f"Hello, {name}!"', 'f"Welcome, {name}!"'
            )
        )
        collision = self.cli(
            "plugins", "install", "--path", str(self.manifest), ok=False
        )
        self.assertIn("different content", collision.stdout + collision.stderr)
        self.launch(self.ref, "old-bytes", self.directory / "inputs.json")
        state = self.wait("old-bytes", "success")
        self.assertEqual(
            state["steps"]["greet"]["last_result"]["output"]["message"], "Hello, Ada!"
        )
        self.runs.clear()  # Old run is terminal; retain its original home until teardown.
        self.home = self.work / "home-edited"
        self.home.mkdir()
        self.cli("home", "init", "--mode", "standalone")
        self.cli("plugins", "install", "--path", str(self.manifest))
        self.install_flow(self.directory, self.ref)
        self.launch(self.ref, "edited", self.directory / "inputs.json")
        state = self.wait("edited", "success")
        self.assertEqual(
            state["steps"]["greet"]["last_result"]["output"]["message"], "Welcome, Ada!"
        )

    def test_invalid_successful_output_is_rejected(self) -> None:
        """The adapter rejects a successful frame whose action output is invalid."""
        self.entrypoint.write_text(
            self.entrypoint.read_text().replace(
                '{"message": f"Hello, {name}!"}', '{"message": 42}'
            )
        )
        self.home = self.work / "home-invalid-output"
        self.home.mkdir()
        self.cli("home", "init", "--mode", "standalone")
        self.cli("plugins", "install", "--path", str(self.manifest))
        self.install_flow(self.directory, self.ref)
        self.launch(self.ref, "invalid-output", self.directory / "inputs.json")
        state = self.wait("invalid-output", "failed")
        result = state["steps"]["greet"]["last_result"]
        self.assertEqual(result["error"]["code"], "validation_error")
        self.assertNotIn("message", result["output"])


class ProtocolTests(unittest.TestCase):
    """Exercise APP framing and safe failures directly without a runtime home."""

    def exchange(
        self, tail: str = "", handshake: str | None = None
    ) -> tuple[subprocess.CompletedProcess[str], list[dict[str, Any]]]:
        """Send NDJSON to the selected entrypoint and require prompt clean exit."""
        first = (
            handshake
            if handshake is not None
            else json.dumps(
                {
                    "type": "app.handshake.request.v1",
                    "protocol_version": "app.v1",
                }
            )
            + "\n"
        )
        result = subprocess.run(
            [sys.executable, str(EXAMPLES / "plugin/greeting/plugin.py")],
            input=first + tail,
            capture_output=True,
            text=True,
            timeout=5,
        )
        return result, [json.loads(line) for line in result.stdout.splitlines()]

    def test_handshake_only_probe(self) -> None:
        """EOF after handshake succeeds and advertises only implemented features."""
        result, frames = self.exchange()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(
            frames,
            [
                {
                    "type": "app.handshake.response.v1",
                    "protocol_version": "app.v1",
                    "plugin": {"id": "tutorial.greeting", "version": "0.1.0"},
                    "adapter_kind": "process",
                    "capabilities": {
                        "actions": ["greet"],
                        "supports_cancel": False,
                        "supports_health": False,
                        "supports_artifact_hints": False,
                        "supports_evidence_hints": False,
                    },
                }
            ],
        )

    def test_correlated_success_and_failure(self) -> None:
        """Result correlation survives both valid and invalid action data."""
        for action, data, expected in (
            ("greet", {"name": "Ada"}, "success"),
            ("greet", {"name": " "}, "failed"),
            ("unknown", {"name": "Ada"}, "failed"),
            ("greet", {"name": 42}, "failed"),
        ):
            with self.subTest(action=action, data=data):
                request = {
                    "type": "app.invoke.request.v1",
                    "protocol_version": "app.v1",
                    "invoke_id": "example-1",
                    "action": action,
                    "input": data,
                }
                result, frames = self.exchange(json.dumps(request) + "\n")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")
                self.assertEqual(len(frames), 2)
                self.assertEqual(frames[1]["invoke_id"], "example-1")
                self.assertEqual(frames[1]["status"], expected)
                if expected == "success":
                    self.assertEqual(frames[1]["output"], {"message": "Hello, Ada!"})
                else:
                    self.assertEqual(frames[1]["error"]["code"], "validation_error")
                    self.assertEqual(frames[1]["output"], {})

    def test_malformed_frames_fail_without_echoing_input(self) -> None:
        """Malformed JSON, wrong versions/types and absent correlation fail closed."""
        for malformed in (
            "not-json-sensitive-value\n",
            "[]\n",
            '{"protocol_version":"app.v99"}\n',
            '{"protocol_version":"app.v1","type":"wrong"}\n',
            '{"protocol_version":"app.v1","type":"app.invoke.request.v1"}\n',
        ):
            with self.subTest(frame=malformed):
                result, frames = self.exchange(malformed)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(frames), 1)
                self.assertEqual(
                    result.stderr, "Invalid APP request or package metadata\n"
                )
        result, frames = self.exchange(handshake="not-json-sensitive-value\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(frames, [])
        self.assertNotIn("sensitive-value", result.stderr)


def main() -> int:
    """Select focused groups; return nonzero for any error, failure or missing CLI."""
    global BINARY
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--group", choices=("docs", "workflow", "plugin", "all"), default="all"
    )
    parser.add_argument("--transist-bin", type=Path, default=BINARY)
    args = parser.parse_args()
    BINARY = args.transist_bin.resolve()
    groups = {
        "docs": [DocumentationTests],
        "workflow": [WorkflowTests],
        "plugin": [ProtocolTests, PluginTests],
    }
    selected = groups.values() if args.group == "all" else [groups[args.group]]
    suite = unittest.TestSuite(
        unittest.defaultTestLoader.loadTestsFromTestCase(case)
        for cases in selected
        for case in cases
    )
    return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
