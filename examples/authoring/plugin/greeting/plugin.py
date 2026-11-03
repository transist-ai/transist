#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""One-shot local greeting action over APP NDJSON; no network or file writes."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any


def emit(frame: dict[str, Any]) -> None:
    """Write and flush one APP frame to stdout; reserve stderr for diagnostics."""
    print(json.dumps(frame), flush=True)


def read_frame() -> dict[str, Any] | None:
    """Read one APP object, returning None on EOF and rejecting malformed frames."""
    line = sys.stdin.readline()
    if not line:
        return None
    frame = json.loads(line)
    if not isinstance(frame, dict) or frame.get("protocol_version") != "app.v1":
        raise ValueError("Invalid APP object or version")
    return frame


def plugin_version() -> str:
    """Read the package's simple top-level version without a YAML dependency."""
    manifest = Path(__file__).with_name("plugin.yaml").read_text(encoding="utf-8")
    return next(
        line.removeprefix("version: ")
        for line in manifest.splitlines()
        if line.startswith("version: ")
    )


def invoke(request: dict[str, Any]) -> dict[str, Any]:
    """Return a correlated terminal result; reject blank names without logging input."""
    result: dict[str, Any] = {
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": request["invoke_id"],
        "status": "success",
        "output": {},
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {},
    }
    data = request.get("input")
    name = data.get("name") if isinstance(data, dict) else None
    if (
        request.get("action") != "greet"
        or not isinstance(name, str)
        or not name.strip()
        or len(name) > 80
        or set(data) != {"name"}
    ):
        result["status"] = "failed"
        result["error"] = {
            "code": "validation_error",
            "class": "validation",
            "message": "greet requires one non-blank name of at most 80 characters",
            "retryability": "none",
        }
    else:
        result["output"] = {"message": f"Hello, {name}!"}
    return result


def main() -> int:
    """Handshake, serve at most one invocation, then exit; EOF supports probes."""
    try:
        request = read_frame()
        if request is None or request.get("type") != "app.handshake.request.v1":
            raise ValueError("Expected APP handshake")
        emit(
            {
                "type": "app.handshake.response.v1",
                "protocol_version": "app.v1",
                "plugin": {"id": "tutorial.greeting", "version": plugin_version()},
                "adapter_kind": "process",
                "capabilities": {
                    "actions": ["greet"],
                    "supports_cancel": False,
                    "supports_health": False,
                    "supports_artifact_hints": False,
                    "supports_evidence_hints": False,
                },
            }
        )
        request = read_frame()
        if request is None:
            return 0
        if (
            request.get("type") != "app.invoke.request.v1"
            or not isinstance(request.get("invoke_id"), str)
            or not request["invoke_id"]
        ):
            raise ValueError("Expected correlated APP invocation")
        emit(invoke(request))
        return 0
    except (ValueError, KeyError, OSError, StopIteration):
        print("Invalid APP request or package metadata", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
