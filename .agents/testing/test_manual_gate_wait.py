#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Focused regressions for asynchronous manual-gate history consumption."""

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from manual_gate_wait import wait_for_decision


class TestManualGateWait(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.run_dir = Path(self.tmp.name) / "run_test"
        self.history = self.run_dir / "gates" / "history"
        self.history.mkdir(parents=True)
        self.record = {
            "run_id": "test", "gate_id": "review-gate", "step_id": "review_gate",
            "decision": "approved", "reason": "lgtm",
        }

    def publish(self) -> None:
        (self.history / "decision.json").write_text(json.dumps(self.record))

    def wait(self, **kwargs: object) -> None:
        wait_for_decision(
            self.run_dir, "review-gate", "review_gate",
            self.record["decision"], "lgtm", **kwargs,
        )

    def test_delayed_history_after_active_request_removed(self) -> None:
        active = self.run_dir / "gates" / "review-gate.json"
        active.write_text("{}")

        def consume(_: float) -> None:
            active.unlink()
            self.publish()

        with patch("manual_gate_wait.time.sleep", side_effect=consume) as poll:
            self.wait()
        poll.assert_called_once()
        self.assertFalse(active.exists())

    def test_approval_waits_for_success_after_history(self) -> None:
        self.publish()
        state = self.run_dir / "state.json"
        state.write_text('{"status": "blocked"}')
        with patch("manual_gate_wait.time.sleep", side_effect=lambda _: state.write_text(
            '{"status": "success"}'
        )) as poll:
            self.wait(expected_status="success")
        poll.assert_called_once()

    def test_reject_and_request_changes_wait_for_history(self) -> None:
        for decision in ("rejected", "request_changes"):
            with self.subTest(decision=decision):
                self.record["decision"] = decision
                for path in self.history.glob("*.json"):
                    path.unlink()
                with patch("manual_gate_wait.time.sleep", side_effect=lambda _: self.publish()) as poll:
                    self.wait()
                poll.assert_called_once()

    def test_missing_history_times_out(self) -> None:
        with self.assertRaisesRegex(TimeoutError, "durable gate history"):
            self.wait(timeout_s=0)

    def test_history_does_not_substitute_for_success(self) -> None:
        self.publish()
        with self.assertRaisesRegex(TimeoutError, "run status 'success'"):
            self.wait(expected_status="success", timeout_s=0)

    def test_malformed_history_fails_without_polling(self) -> None:
        for raw in ("{", "[]"):
            with self.subTest(raw=raw):
                (self.history / "decision.json").write_text(raw)
                with patch("manual_gate_wait.time.sleep") as poll:
                    with self.assertRaises(ValueError):
                        self.wait()
                poll.assert_not_called()

    def test_identity_decision_and_reason_mismatches_fail_immediately(self) -> None:
        for key in self.record:
            with self.subTest(key=key):
                self.publish()
                path = self.history / "decision.json"
                record = json.loads(path.read_text())
                record[key] = "wrong"
                path.write_text(json.dumps(record))
                with patch("manual_gate_wait.time.sleep") as poll:
                    with self.assertRaisesRegex(ValueError, f"history mismatch.*{key}"):
                        self.wait()
                poll.assert_not_called()

    def test_failed_run_is_mismatch_not_timeout(self) -> None:
        self.publish()
        (self.run_dir / "state.json").write_text('{"status": "failed"}')
        with self.assertRaisesRegex(ValueError, "run status mismatch"):
            self.wait(expected_status="success")


if __name__ == "__main__":
    unittest.main()
