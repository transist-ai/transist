#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Wait for durable manual-gate evidence, never the consumed active request."""

import argparse
import json
import sys
import time
from pathlib import Path


def _read_object(path: Path) -> dict:
    """Read one durable JSON object, rejecting malformed evidence immediately."""
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ValueError(f"malformed record at {path}: expected JSON object")
    return value


def wait_for_decision(
    run_dir: Path,
    gate_id: str,
    step_id: str,
    decision: str,
    reason: str,
    expected_status: str | None = None,
    timeout_s: float = 30,
) -> None:
    """Validate history identity/decision/reason and optionally await run status.

    Reads only durable history and state under run_dir. Missing evidence is
    polled until timeout; malformed or mismatched evidence fails immediately.
    """
    deadline = time.monotonic() + timeout_s
    expected = {
        "run_id": run_dir.name.removeprefix("run_"),
        "gate_id": gate_id,
        "step_id": step_id,
        "decision": decision,
        "reason": reason,
    }
    pending = "durable gate history"
    while True:
        records = sorted((run_dir / "gates" / "history").glob("*.json"))
        for path in records:
            record = _read_object(path)
            for key, value in expected.items():
                if record.get(key) != value:
                    raise ValueError(
                        f"history mismatch at {path}: {key} expected {value!r}, "
                        f"got {record.get(key)!r}"
                    )
        if records:
            if expected_status is None:
                return
            pending = f"run status {expected_status!r}"
            try:
                state = _read_object(run_dir / "state.json")
            except FileNotFoundError:
                state = None
            if state is not None:
                status = state.get("status")
                if not isinstance(status, str):
                    raise ValueError("malformed run state: missing/string status required")
                if status == expected_status:
                    return
                if status in {"success", "failed", "cancelled"}:
                    raise ValueError(
                        f"run status mismatch: expected {expected_status!r}, got {status!r}"
                    )
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"timed out waiting for {pending} in {run_dir}")
        time.sleep(min(0.1, remaining))


def main() -> int:
    """Parse suite arguments and report timeout separately from invalid evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path)
    parser.add_argument("decision")
    parser.add_argument("reason")
    parser.add_argument("--expected-status")
    args = parser.parse_args()
    try:
        wait_for_decision(
            args.run_dir, "review-gate", "review_gate", args.decision,
            args.reason, args.expected_status,
        )
    except TimeoutError as error:
        print(f"FAIL: timeout: {error}", file=sys.stderr)
        return 1
    except (OSError, ValueError) as error:
        print(f"FAIL: invalid gate evidence: {error}", file=sys.stderr)
        return 1
    print(f"OK: durable {args.decision} history identity and reason validated")
    if args.expected_status:
        print(f"OK: run reached {args.expected_status}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
