"""Offline APP regression tests for the bundled shell plugin."""

import importlib.util
import json
import os
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock


PLUGIN_DIR = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "bundled_shell_plugin", PLUGIN_DIR / "plugin.py"
)
PLUGIN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PLUGIN)


class ShellPluginTests(unittest.TestCase):
    def test_terminal_follows_finalization_and_heartbeat(self):
        request = {
            "action": "exec",
            "input": {"command": "tool"},
            "invoke_id": "test-invoke",
            "paths": {"workspace_dir": "/fixture"},
        }
        frames = []
        finalized = False

        def run(command, **kwargs):
            nonlocal finalized
            self.assertIsNone(kwargs["timeout"])
            self.assertNotIn("capture_output", kwargs)
            self.assertNotIn("text", kwargs)
            with mock.patch.object(PLUGIN.time, "monotonic", return_value=100):
                kwargs["on_tick"]()
            finalized = True
            return subprocess.CompletedProcess(
                command, 0, '{"value": 42, "ok": false}', ""
            )

        def emit(frame):
            if frame["type"] == "app.invoke.result.v1":
                self.assertTrue(finalized)
            frames.append(frame)

        with mock.patch.object(
            PLUGIN, "read_frame", side_effect=[{}, request]
        ), mock.patch.object(PLUGIN, "run_process", side_effect=run), mock.patch.object(
            PLUGIN, "emit", side_effect=emit
        ), mock.patch.object(
            PLUGIN.time, "monotonic", return_value=0
        ), mock.patch.object(
            PLUGIN, "collect_evidence_hints", return_value=[]
        ):
            self.assertEqual(PLUGIN.main(), 0)
        self.assertEqual(frames[1]["line_preview"], "Command is still running...")
        self.assertEqual(frames[-1]["output"]["value"], 42)
        self.assertTrue(frames[-1]["output"]["ok"])

    def invoke(
        self, action: str, payload: dict, workspace: str, error=None
    ) -> tuple[dict, mock.Mock]:
        """Invoke the APP entrypoint with a mocked child and return its result."""
        request = {
            "action": action,
            "input": payload,
            "invoke_id": "test-invoke",
            "paths": {"workspace_dir": workspace, "run_root": workspace},
        }
        child = mock.Mock(
            return_value=subprocess.CompletedProcess([], 0, "{}", ""), side_effect=error
        )
        frames = []
        with mock.patch.object(
            PLUGIN, "read_frame", side_effect=[{}, request]
        ), mock.patch.object(
            PLUGIN, "emit", side_effect=frames.append
        ), mock.patch.object(
            PLUGIN, "run_process", child
        ), mock.patch.object(
            PLUGIN, "collect_evidence_hints", return_value=[]
        ):
            self.assertEqual(PLUGIN.main(), 0)
        self.assertEqual(frames[0]["plugin"]["version"], "0.1.0")
        return frames[1], child

    def test_exec_forwards_gate_and_typed_args(self):
        decision = {
            "schema": "transist.gate_decision.v1",
            "run_id": "run-1",
            "gate_id": "review",
            "step_id": "review",
            "decision": "approved",
            "decided_at_ms": 42,
        }
        with tempfile.TemporaryDirectory() as workspace:
            result, child = self.invoke(
                "exec",
                {
                    "command": "printf",
                    "args": ["literal", 12, 1.5, True, False],
                    "gate_decision": decision,
                },
                workspace,
            )
        self.assertEqual(
            child.call_args.args[0], ["printf", "literal", "12", "1.5", "true", "false"]
        )
        self.assertEqual(
            json.loads(child.call_args.kwargs["env"]["TRANSIST_GATE_DECISION_JSON"]),
            decision,
        )
        self.assertEqual(result["output"]["ok"], True)

    def test_exec_removes_inherited_stale_decision(self):
        with tempfile.TemporaryDirectory() as workspace, mock.patch.dict(
            os.environ, {"TRANSIST_GATE_DECISION_JSON": "stale"}
        ):
            _, child = self.invoke("exec", {"command": "true"}, workspace)
        self.assertNotIn("TRANSIST_GATE_DECISION_JSON", child.call_args.kwargs["env"])

    def test_echo_formats_typed_args(self):
        with tempfile.TemporaryDirectory() as workspace:
            result, child = self.invoke(
                "echo", {"args": ["one", 2, True, False]}, workspace
            )
        child.assert_not_called()
        self.assertEqual(result["output"]["stdout"], "one 2 true false")

    def test_source_path_still_rejects_escape(self):
        with tempfile.TemporaryDirectory() as workspace:
            source = pathlib.Path(workspace) / "source"
            source.mkdir()
            (pathlib.Path(workspace) / "outside.py").write_text(
                "pass", encoding="utf-8"
            )
            request = {
                "metadata": {"recipe_source_dir": str(source)},
                "paths": {"workspace_dir": workspace},
            }
            with self.assertRaisesRegex(
                ValueError, "escapes the staged source directory"
            ):
                PLUGIN.resolve_arg(request, "source://../outside.py")

    def test_cleanup_failure_is_nonretryable_and_preserves_output(self):
        for action in ("exec", "exec_with_secret_env"):
            with self.subTest(
                action=action
            ), tempfile.TemporaryDirectory() as workspace:
                result, _ = self.invoke(
                    action,
                    {"command": "tool"},
                    workspace,
                    PLUGIN.CleanupUnconfirmedError("partial", "stderr"),
                )
            self.assertEqual(result["error"]["code"], "protocol_error")
            self.assertEqual(result["error"]["retryability"], "none")
            self.assertEqual(
                result["error"]["message"],
                "Command process cleanup could not be confirmed",
            )
            self.assertEqual(result["artifact_hints"][0]["contents"], "partial")

    def test_unexpected_runner_failure_is_safe_internal(self):
        with tempfile.TemporaryDirectory() as workspace:
            result, _ = self.invoke(
                "exec",
                {"command": "tool"},
                workspace,
                RuntimeError("private token/argv"),
            )
        self.assertEqual(result["error"]["code"], "adapter_internal_error")
        self.assertEqual(result["error"]["message"], "Command runner failed")
        self.assertNotIn("private token/argv", str(result))


if __name__ == "__main__":
    unittest.main()
