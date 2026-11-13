#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Check shipped-workflow assertions, isolation and local shell helpers."""

import hashlib
from copy import deepcopy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, call, patch

import run_shipped_workflows as driver


class RejectedResumeStateTests(unittest.TestCase):
    """Only two typed audit timestamps may change in persisted trusted mode."""

    def state(self, trusted: bool = True) -> dict:
        """Return a representative state with nested authority and history data."""
        subject = {
            "id": "demo",
            "version": "0.1.0",
            "digest": "abc",
            "signature_verified": True,
            "signer_key_id": "fixture",
            "trust_decision": "verified",
        }
        return {
            "trusted_execution_mode": trusted,
            "updated_at_ms": 100,
            "trust_decision": (
                {
                    "schema": "transist.trust_decision.v1",
                    "run_id": "demo",
                    "trusted_execution_mode": True,
                    "decision": "allowed",
                    "workflow": subject,
                    "plugins": [deepcopy(subject)],
                    "decided_at_ms": 90,
                }
                if trusted
                else None
            ),
            "status": "failed",
            "recovery_state": "cleanup_completed",
            "resume_count": 0,
            "executions": {"primary": 1},
            "runtime_mode": "governed" if trusted else "standalone",
            "policy_profile": "allowlist",
            "capability_enforcement": "enforced",
            "cleanup": {
                "state": "succeeded",
                "steps": {
                    "teardown": {"executions": 1, "status": "succeeded"},
                },
            },
            "steps": {
                "primary": {
                    "executions": 1,
                    "last_result": {
                        "evidence": [{"sha256": "abc", "bytes": 1}],
                    },
                }
            },
            "capability_authority_version": 1,
            "asset_locks": [{"kind": "plugin", "ref_id": "shell", "sha256": "abc"}],
            "recipe_snapshot": {"sha256": "abc"},
            "created_at_ms": 80,
            "workspace": {"path": "/fixture", "access": "full"},
        }

    def test_accepts_only_audit_timestamps_without_mutating_inputs(self) -> None:
        """Both timestamps independently allow equality and forward movement."""
        for updated in (100, 101):
            for decided in (90, 91):
                before = self.state()
                after = deepcopy(before)
                after["updated_at_ms"] = updated
                after["trust_decision"]["decided_at_ms"] = decided
                saved = deepcopy((before, after))
                driver.assert_rejected_resume_state_unchanged(before, after)
                self.assertEqual((before, after), saved)

    def test_rejects_every_other_leaf_and_added_or_removed_fields(self) -> None:
        """Recursive mutation matrix covers authority, signer/digest and counters."""
        before = self.state()

        def visit(value: dict | list, path: tuple = ()) -> None:
            """Mutate each persisted field individually, including container shape."""
            keys = range(len(value)) if isinstance(value, list) else value.keys()
            for key in keys:
                leaf_path = (*path, key)
                if leaf_path in (
                    ("updated_at_ms",),
                    ("trust_decision", "decided_at_ms"),
                ):
                    continue
                for operation in ("replace", "remove"):
                    after = deepcopy(before)
                    target = after
                    for part in path:
                        target = target[part]
                    if operation == "remove":
                        del target[key]
                    else:
                        target[key] = "changed"
                    with self.subTest(path=leaf_path, operation=operation):
                        saved = deepcopy((before, after))
                        with self.assertRaises(driver.WorkflowError):
                            driver.assert_rejected_resume_state_unchanged(before, after)
                        self.assertEqual((before, after), saved)
                if isinstance(value[key], (dict, list)):
                    visit(value[key], leaf_path)
            if isinstance(value, dict):
                after = deepcopy(before)
                target = after
                for part in path:
                    target = target[part]
                target["unexpected"] = 1
                with self.subTest(path=path, operation="add"):
                    with self.assertRaises(driver.WorkflowError):
                        driver.assert_rejected_resume_state_unchanged(before, after)

        visit(before)
        after = deepcopy(before)
        after["steps"]["primary"]["executions"] = True
        with self.assertRaises(driver.WorkflowError):
            driver.assert_rejected_resume_state_unchanged(before, after)

    def test_rejects_missing_malformed_or_regressing_audit_timestamps(self) -> None:
        """Both snapshots require u64 times; booleans/floats are not integers."""
        for path in (("updated_at_ms",), ("trust_decision", "decided_at_ms")):
            for side in (0, 1):
                for invalid in (None, True, False, "100", 100.0, -1, 2**64, [], {}):
                    states = [self.state(), self.state()]
                    target = states[side]
                    for key in path[:-1]:
                        target = target[key]
                    target[path[-1]] = invalid
                    with self.subTest(path=path, side=side, invalid=invalid):
                        with self.assertRaises(driver.WorkflowError):
                            driver.assert_rejected_resume_state_unchanged(*states)
                states = [self.state(), self.state()]
                target = states[side]
                for key in path[:-1]:
                    target = target[key]
                del target[path[-1]]
                with self.assertRaises(driver.WorkflowError):
                    driver.assert_rejected_resume_state_unchanged(*states)
            before = self.state()
            after = deepcopy(before)
            target = after if len(path) == 1 else after[path[0]]
            target[path[-1]] -= 1
            with self.assertRaises(driver.WorkflowError):
                driver.assert_rejected_resume_state_unchanged(before, after)

    def test_only_persisted_true_enables_allowance(self) -> None:
        """A decision object or a mode-looking field alone grants no exemption."""
        for flag in (False, None, 1, "true", "governed", "missing"):
            before = self.state()
            before["trusted_execution_mode"] = flag
            if flag == "missing":
                del before["trusted_execution_mode"]
            driver.assert_rejected_resume_state_unchanged(before, deepcopy(before))
            for path in (("updated_at_ms",), ("trust_decision", "decided_at_ms")):
                after = deepcopy(before)
                target = after if len(path) == 1 else after[path[0]]
                target[path[-1]] += 1
                with self.subTest(flag=flag, path=path):
                    with self.assertRaises(driver.WorkflowError):
                        driver.assert_rejected_resume_state_unchanged(before, after)
        before = self.state(False)
        driver.assert_rejected_resume_state_unchanged(before, deepcopy(before))
        for invalid in (None, [], "decision", {}):
            before = self.state()
            before["trust_decision"] = invalid
            with self.assertRaises(driver.WorkflowError):
                driver.assert_rejected_resume_state_unchanged(before, deepcopy(before))


class RecoveryHistoryTests(unittest.TestCase):
    """Snapshot all history bytes, exempting only exact top-level projections."""

    def test_snapshot_detects_changed_added_and_removed_history(self) -> None:
        """Nested names cannot inherit exclusions; every history file is compared."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = (
                "events.jsonl",
                "failures.jsonl",
                "recipe.yaml",
                "artifacts/step/execution_001/evidence/proof.txt",
                "executions/step/execution_001.json",
                "contained_runs/run_child/state.json",
                "contained_runs/run_child/summary.json",
                "contained_runs/run_child/runner.log",
            )
            for name in (*paths, "state.json", "summary.json", "runner.log"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"original")
            before = driver.recovery_history_snapshot(root)
            self.assertEqual(set(before), set(paths))
            for name in ("state.json", "summary.json", "runner.log"):
                (root / name).write_bytes(b"audit projection")
            self.assertEqual(driver.recovery_history_snapshot(root), before)
            for name in paths:
                with self.subTest(path=name):
                    path = root / name
                    path.write_bytes(b"changed")
                    self.assertNotEqual(driver.recovery_history_snapshot(root), before)
                    path.unlink()
                    self.assertNotEqual(driver.recovery_history_snapshot(root), before)
                    path.write_bytes(b"original")
            (root / "unexpected-execution.json").write_bytes(b"new")
            self.assertNotEqual(driver.recovery_history_snapshot(root), before)


class CapturedDemoTests(unittest.TestCase):
    """Exercise capture integrity and evidence checks against persisted layouts."""

    def test_source_capture_rejects_wrong_paths_digests_and_blob_bytes(self) -> None:
        """Correct content under a stale path or digest must not satisfy capture."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            session = driver.Session.__new__(driver.Session)
            session.root = root
            session.home = root / "home"
            source = root / "demo/scripts/write_outputs.sh"
            source.parent.mkdir(parents=True)
            body = b"#!/bin/sh\r\nprintf 'captured bytes\\n'\r\n"
            source.write_bytes(body)
            digest = hashlib.sha256(body).hexdigest()
            assets = session.state_path("demo").parent / "workflow_assets"
            (assets / "blobs").mkdir(parents=True)
            blob = assets / "blobs" / digest
            blob.write_bytes(body)
            entry = {"path": "source/scripts/write_outputs.sh", "sha256": digest}
            check = {
                "kind": "captured_source_assets",
                "run_id": "demo",
                "workflow_dir": "demo",
                "paths": [entry["path"]],
                "label": "source capture",
            }
            for entries, captured, valid in (
                ([entry], body, True),
                ([], body, False),
                ([{**entry, "path": "source/write_outputs.sh"}], body, False),
                ([{**entry, "sha256": "0" * 64}], body, False),
                ([entry, entry], body, False),
                ([entry, {**entry, "path": "source/scripts/stale.sh"}], body, False),
                ([entry], body.replace(b"\r\n", b"\n"), False),
            ):
                with self.subTest(entries=entries, captured=captured):
                    (assets / "manifest.json").write_text(
                        json.dumps({"entries": entries}), encoding="utf-8"
                    )
                    blob.write_bytes(captured)
                    if valid:
                        session._run_check(check)
                    else:
                        with self.assertRaises(driver.WorkflowError):
                            session._run_check(check)

    def test_module_capture_uses_persisted_child_id_and_module_relative_path(
        self,
    ) -> None:
        """A contained module's manifest has its own source-relative namespace."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            session = driver.Session.__new__(driver.Session)
            session.root = root
            session.home = root / "home"
            source = root / "demo/modules/scripts/write_greeting.sh"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"printf 'hello\\n'\n")
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            run_root = session.state_path("demo").parent
            assets = run_root / "contained_runs/run_actual-child/workflow_assets"
            (assets / "blobs").mkdir(parents=True)
            (assets / "blobs" / digest).write_bytes(source.read_bytes())
            (assets / "manifest.json").write_text(
                json.dumps(
                    {
                        "entries": [
                            {
                                "path": "source/scripts/write_greeting.sh",
                                "sha256": digest,
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )
            session.state_path("demo").write_text(
                json.dumps(
                    {
                        "steps": {
                            "greet_via_subflow": {
                                "last_result": {
                                    "output": {"child_run_id": "actual-child"},
                                }
                            }
                        }
                    }
                ),
                encoding="utf-8",
            )
            session._run_check(
                {
                    "kind": "captured_source_assets",
                    "run_id": "demo",
                    "workflow_dir": "demo/modules",
                    "child_step": "greet_via_subflow",
                    "paths": ["source/scripts/write_greeting.sh"],
                    "label": "module capture",
                }
            )

    def test_demo05_evidence_is_read_from_each_persisted_step_directory(self) -> None:
        """Missing, changed or cross-step evidence fails despite a workspace copy."""
        workflow = next(
            w for w in driver.WORKFLOWS if w["id_ref"].startswith("demo_05")
        )
        checks = [
            c for c in workflow["checks"] if c["kind"] == "state_step_evidence_content"
        ]
        self.assertEqual({c["step"] for c in checks}, {"write_outputs", "read_outputs"})
        with tempfile.TemporaryDirectory() as directory:
            session = driver.Session.__new__(driver.Session)
            session.home = Path(directory) / "home"
            session.workspace = Path(directory) / "workspace"
            session.workspace.mkdir()
            run_root = session.state_path("demo").parent
            steps = {}
            for check in checks:
                evidence = (
                    run_root / "artifacts" / check["step"] / "execution_001/evidence"
                )
                evidence.mkdir(parents=True)
                (evidence / check["path"]).write_text(
                    check["expected"], encoding="utf-8"
                )
                steps[check["step"]] = {
                    "last_request": {"paths": {"evidence_dir": str(evidence)}},
                    "last_result": {
                        "evidence": [
                            {
                                "path": str(evidence / check["path"]),
                                "sha256": hashlib.sha256(
                                    check["expected"].encode("utf-8")
                                ).hexdigest(),
                                "bytes": len(check["expected"].encode("utf-8")),
                            }
                        ]
                    },
                }
            state = session.state_path("demo")
            state.write_text(json.dumps({"steps": steps}), encoding="utf-8")
            for check in checks:
                check = {**check, "run_id": "demo"}
                session._run_check(check)
                evidence = Path(
                    steps[check["step"]]["last_request"]["paths"]["evidence_dir"]
                )
                path = evidence / check["path"]
                (session.workspace / check["path"]).write_bytes(path.read_bytes())
                for body in (None, b"wrong evidence\n"):
                    with self.subTest(step=check["step"], body=body):
                        if body is None:
                            path.unlink()
                        else:
                            path.write_bytes(body)
                        with self.assertRaises(driver.WorkflowError):
                            session._run_check(check)
                path.write_text(check["expected"], encoding="utf-8")
                refs = steps[check["step"]]["last_result"]["evidence"]
                for invalid in (
                    [],
                    [{**refs[0], "sha256": "0" * 64}],
                    [{**refs[0], "bytes": 0}],
                ):
                    with self.subTest(step=check["step"], refs=invalid):
                        steps[check["step"]]["last_result"]["evidence"] = invalid
                        state.write_text(json.dumps({"steps": steps}), encoding="utf-8")
                        with self.assertRaises(driver.WorkflowError):
                            session._run_check(check)
                steps[check["step"]]["last_result"]["evidence"] = refs
                state.write_text(json.dumps({"steps": steps}), encoding="utf-8")
            steps["read_outputs"]["last_request"]["paths"]["evidence_dir"] = str(
                run_root / "artifacts/write_outputs/execution_001/evidence"
            )
            state.write_text(json.dumps({"steps": steps}), encoding="utf-8")
            with self.assertRaises(driver.WorkflowError):
                session._run_check({**checks[1], "run_id": "demo"})

    def test_root_stdout_checks_require_the_reader_output(self) -> None:
        """A successful writer or workspace file cannot replace root reader stdout."""
        for prefix in ("demo_05", "demo_07"):
            workflow = next(
                w for w in driver.WORKFLOWS if w["id_ref"].startswith(prefix)
            )
            check = next(
                c
                for c in workflow["checks"]
                if c["kind"] == "state_step_stdout_contains"
            )
            session = driver.Session.__new__(driver.Session)
            for stdout, valid in ((check["needle"], True), ("wrote greeting\n", False)):
                with self.subTest(demo=prefix, stdout=stdout):
                    with patch.object(
                        session,
                        "state",
                        return_value={
                            "steps": {
                                check["step"]: {
                                    "last_result": {"output": {"stdout": stdout}}
                                },
                            }
                        },
                    ):
                        if valid:
                            session._run_check({**check, "run_id": "demo"})
                        else:
                            with self.assertRaises(driver.WorkflowError):
                                session._run_check({**check, "run_id": "demo"})


class GreetingHelperTests(unittest.TestCase):
    """Run shipped greeting scripts locally with literal arguments only."""

    def test_greeting_helpers_preserve_spaces_and_shell_metacharacters(self) -> None:
        """Paths and topics are data, including quotes, substitutions and globs."""
        demo = (
            Path(__file__).resolve().parents[2]
            / "workflows/demos/getting_started/07_subflow_basics"
        )
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            artifact = workspace / "greeting ' ; $(touch injected) & *.txt"
            topic = "transist ' ; $(touch injected) `touch injected` & * $HOME"
            writer = subprocess.run(
                [
                    "sh",
                    str(demo / "modules/scripts/write_greeting.sh"),
                    topic,
                    str(artifact),
                ],
                cwd=workspace,
                capture_output=True,
                text=True,
                check=True,
            )
            reader = subprocess.run(
                ["sh", str(demo / "scripts/read_greeting.sh"), str(artifact)],
                cwd=workspace,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(artifact.read_text(encoding="utf-8"), f"hello, {topic}\n")
            self.assertEqual(writer.stdout, f"wrote greeting for {topic}\n")
            self.assertEqual(reader.stdout, f"root sees: hello, {topic}\n")
            self.assertEqual(list(workspace.iterdir()), [artifact])

    def test_greeting_reader_fails_when_artifact_is_missing(self) -> None:
        """The root reader reports a missing artifact and exits unsuccessfully."""
        demo = (
            Path(__file__).resolve().parents[2]
            / "workflows/demos/getting_started/07_subflow_basics"
        )
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                ["sh", str(demo / "scripts/read_greeting.sh"), "missing greeting.txt"],
                cwd=directory,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("module did not produce greeting.txt", result.stdout)


class SessionIsolationTests(unittest.TestCase):
    """Keep generated homes disjoint from payload and mutations correctly ordered."""

    def test_runner_completion_waits_for_latest_matching_event(self) -> None:
        """Old/unrelated completion cannot hide a newer still-active runner."""
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            log = home / "runs" / "run_demo" / "runner.log"
            log.parent.mkdir(parents=True)
            events = [
                {
                    "schema": "transist.runner_event.v1",
                    "event": "finished",
                    "run_id": "demo",
                },
                {
                    "schema": "transist.runner_event.v1",
                    "event": "started",
                    "run_id": "demo",
                },
                {
                    "schema": "transist.runner_event.v1",
                    "event": "finished",
                    "run_id": "other",
                },
            ]
            log.write_text(
                "\n".join(map(json.dumps, events)) + "\npartial", encoding="utf-8"
            )

            def complete(_: float) -> None:
                """Publish the completion only after the waiter observes startup."""
                log.write_text(json.dumps(events[0]) + "\n", encoding="utf-8")

            with patch.object(driver.time, "sleep", side_effect=complete) as sleep:
                driver.Session.wait_runner_finished(Mock(home=home), "demo")
            sleep.assert_called_once_with(0.05)

    def test_runner_completion_timeout_is_an_error(self) -> None:
        """Missing completion fails the fixture instead of attempting resume."""
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(driver.time, "monotonic", side_effect=[0, 0, 31]):
                with patch.object(driver.time, "sleep"):
                    with self.assertRaises(driver.WorkflowError):
                        driver.Session.wait_runner_finished(
                            Mock(home=Path(directory)), "demo"
                        )

    def test_resume_waits_for_runner_before_marker_and_command(self) -> None:
        """Resume uses the actual finalization checkpoint before any mutation."""
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            session = Mock(workspace=workspace)
            session.wait_status.return_value = "failed"

            def finished(_: str) -> None:
                """Verify the retry signal is not published ahead of completion."""
                self.assertFalse((workspace / ".retry-ok").exists())

            session.wait_runner_finished.side_effect = finished
            driver.run_plan_resume(session, {"terminal": "success", "checks": []})
            session.wait_runner_finished.assert_called_once_with("resume-run")
            session.cli.assert_called_once_with(
                ["runs", "resume", "resume-run"], cwd=workspace
            )
            session.assert_has_calls(
                [
                    call.wait_runner_finished("resume-run"),
                    call.cli(["runs", "resume", "resume-run"], cwd=workspace),
                ]
            )
            self.assertTrue((workspace / ".retry-ok").exists())

    def test_replay_waits_for_source_runner_before_command(self) -> None:
        """Terminal status alone must not authorize replay ahead of finalization."""
        session = Mock(workspace=Path("/fixture"))
        workflow = {"terminal": "success", "checks": []}
        driver.run_plan_replay(session, workflow)
        self.assertEqual(
            session.mock_calls,
            [
                call.run("replay-source", workflow),
                call.wait_terminal("replay-source", "success"),
                call.wait_runner_finished("replay-source"),
                call.cli(
                    ["runs", "replay", "replay-source", "--run-ref", "replay-target"],
                    cwd=Path("/fixture"),
                ),
                call.wait_terminal("replay-target", "success"),
                call.run_checks(workflow),
            ],
        )

    def test_replay_does_not_dispatch_when_source_runner_does_not_finish(self) -> None:
        """A failed completion wait must propagate without retrying replay."""
        session = Mock(workspace=Path("/fixture"))
        session.wait_runner_finished.side_effect = driver.WorkflowError("runner still active")
        with self.assertRaisesRegex(driver.WorkflowError, "runner still active"):
            driver.run_plan_replay(session, {"terminal": "success", "checks": []})
        session.cli.assert_not_called()
        session.run_checks.assert_not_called()

    def test_secret_delivery_policy_is_configured_before_check(self) -> None:
        """Checking and running the secret demo share its explicit delivery grant."""
        with tempfile.TemporaryDirectory() as directory:
            session = Mock(home=Path(directory))
            entry = {"plan": "secret"}
            checked_policy = []
            session.check.side_effect = lambda *args, **kwargs: checked_policy.append(
                (session.home / "config.yaml").read_text(encoding="utf-8")
            )
            plan = Mock()
            with patch.dict(driver.PLANS, {"secret": plan}):
                driver.run_workflow(session, entry)
            self.assertEqual(len(checked_policy), 1)
            self.assertIn("process: true", checked_policy[0])
            self.assertIn("http: false", checked_policy[0])
            self.assertIn("mcp: false", checked_policy[0])
            session.check.assert_called_once_with(
                entry, extra_env={"MY_API_TOKEN": "local-shipped-workflow-secret"}
            )
            plan.assert_called_once_with(session, entry)
            self.assertEqual(
                (session.home / "config.yaml").read_text(encoding="utf-8"),
                checked_policy[0],
            )

    def test_source_side_parent_is_independent_of_tmpdir(self) -> None:
        """The source tree selects scratch, not the runtime temp environment."""
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve() / "source"
            script = source / "scripts" / "ci" / "run_shipped_workflows.py"
            with patch.object(driver, "__file__", str(script)):
                with patch.dict(
                    driver.os.environ, {"TMPDIR": "/unrelated/runtime-temp"}
                ):
                    parent = driver.session_parent()
            self.assertEqual(parent, source / ".tmp" / "shipped-workflows")
            self.assertTrue(parent.is_dir())

    def test_archive_payload_is_unchanged_and_cleanup_is_session_scoped(self) -> None:
        """Archive sessions create fake homes outside payload and retain peers."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            release = root / "release"
            release.mkdir()
            marker = release / "immutable-payload"
            marker.write_bytes(b"preserve")
            parent = root / "source" / ".tmp" / "shipped-workflows"
            parent.mkdir(parents=True)
            with patch.object(driver, "session_parent", return_value=parent):
                first = driver.Session(release, release / "bin/transist", False)
                second = driver.Session(release, release / "bin/transist", False)
            self.assertEqual(first.temp_dir.parent, parent)
            self.assertEqual(Path(first.env()["HOME"]), first.fake_home)
            self.assertTrue(first.fake_home.is_dir())
            self.assertFalse(first.temp_dir.is_relative_to(release))
            first.close()
            self.assertFalse(first.temp_dir.exists())
            self.assertTrue(second.temp_dir.exists())
            self.assertEqual(marker.read_bytes(), b"preserve")
            self.assertEqual(list(release.iterdir()), [marker])
            second.close()


if __name__ == "__main__":
    unittest.main()
