#!/usr/bin/env python3
import json
import sys


APP_PROTOCOL = "app.v1"
PLUGIN_ID = "goose.cli"
PLUGIN_VERSION = "0.1.0"
ACTION = "run_task"


def emit(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def failed_result(invoke_id, code, message):
    return {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": invoke_id,
        "plugin": {"id": PLUGIN_ID, "version": PLUGIN_VERSION},
        "status": "failed",
        "output": {
            "ok": False,
            "response_text": "",
            "response_file": "",
            "stdout": "",
            "stderr": message,
            "exit_code": 1,
        },
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {},
        "usage": None,
        "error": {
            "code": code,
            "category": "validation",
            "message": message,
            "retryability": "safe",
        },
    }


def success_result(invoke_id):
    return {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": invoke_id,
        "plugin": {"id": PLUGIN_ID, "version": PLUGIN_VERSION},
        "status": "success",
        "output": {
            "ok": True,
            "response_text": "",
            "response_file": "",
            "stdout": "",
            "stderr": "",
            "exit_code": 0,
        },
        "artifact_hints": [],
        "evidence_hints": [],
        "metrics": {},
        "usage": None,
        "error": None,
    }


def main():
    request = json.load(sys.stdin)
    invoke_id = request.get("invoke_id", "fixture")
    if request.get("action") != ACTION:
        emit(failed_result(invoke_id, "unknown_action", f"unsupported action: {request.get('action')}"))
        return
    emit(success_result(invoke_id))


if __name__ == "__main__":
    main()
