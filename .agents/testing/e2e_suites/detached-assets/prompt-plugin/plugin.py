#!/usr/bin/env python3
import json
import sys


PLUGIN_ID = "detached.prompt"
PLUGIN_VERSION = "1.0.0"


def emit(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def result(invoke_id, ok, message):
    return {
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": invoke_id,
        "plugin": {"id": PLUGIN_ID, "version": PLUGIN_VERSION},
        "status": "success" if ok else "failed",
        "output": {
            "ok": ok,
            "prompt_seen": message,
            "stdout": message,
            "stderr": "" if ok else message,
            "exit_code": 0 if ok else 1,
        },
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {},
        "usage": None,
        "error": None if ok else {
            "code": "prompt_not_available",
            "category": "validation",
            "message": message,
            "retryability": "safe",
        },
    }


def main():
    handshake = json.loads(sys.stdin.readline())
    if handshake.get("type") != "app.handshake.request.v1":
        return 1
    emit({
        "type": "app.handshake.response.v1",
        "protocol_version": "app.v1",
        "plugin": {"id": PLUGIN_ID, "version": PLUGIN_VERSION},
        "adapter_kind": "process",
        "capabilities": {
            "actions": ["run"],
            "supports_cancel": False,
            "supports_health": False,
            "supports_artifact_hints": False,
            "supports_evidence_hints": False,
        },
    })
    request = json.loads(sys.stdin.readline())
    invoke_id = request.get("invoke_id", "fixture")
    content = (request.get("input") or {}).get("prompt", "")
    ok = "detached prompt" in content
    emit(result(invoke_id, ok, "detached prompt consumed" if ok else "unexpected prompt content"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
