#!/usr/bin/env python3
"""remote.echo — minimal remote-provider plugin for local-only deny tests."""
import json
import sys

APP_PROTOCOL = "app.v1"

def main():
    payload = json.loads(sys.stdin.readline())
    if payload.get("type") == "app.handshake.request.v1":
        print(json.dumps({
            "type": "app.handshake.response.v1",
            "protocol_version": APP_PROTOCOL,
            "plugin": {"id": "remote.echo", "version": "0.1.0"},
            "adapter_kind": "process",
            "capabilities": {
                "actions": ["echo"],
                "supports_cancel": False,
                "supports_health": False,
                "supports_artifact_hints": False,
                "supports_evidence_hints": False
            }
        }), flush=True)
        invoke = json.loads(sys.stdin.readline())
        invoke_id = invoke.get("invoke_id", "invoke_1")
        print(json.dumps({
            "type": "app.invoke.result.v1",
            "protocol_version": APP_PROTOCOL,
            "invoke_id": invoke_id,
            "status": "success",
            "output": {"stdout": "remote-echo", "stderr": "", "exit_code": 0},
            "artifact_hints": [],
            "evidence_hints": [],
            "metrics": {},
            "usage": None,
            "error": None
        }), flush=True)

if __name__ == "__main__":
    main()
