#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import sys
import time
from typing import Callable

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from process_runner import CleanupUnconfirmedError, run_process  # noqa: E402

APP_PROTOCOL = "app.v1"


def command_heartbeat(request: dict) -> Callable[[], None]:
    """Return a main-thread callback emitting safe status once per second."""
    last = time.monotonic()

    def tick() -> None:
        nonlocal last
        now = time.monotonic()
        if now - last >= 1:
            last = now
            emit(
                {
                    "type": "app.invoke.progress.v1",
                    "protocol_version": APP_PROTOCOL,
                    "invoke_id": request["invoke_id"],
                    "stream": "status",
                    "delta_lines": 0,
                    "line_preview": "Command is still running...",
                }
            )

    return tick


def runner_failure(request: dict, code: str, message: str, hints: list) -> dict:
    """Build a safe terminal failure after command finalization."""
    return {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": request["invoke_id"],
        "status": "failed",
        "output": {},
        "artifact_hints": hints,
        "evidence_hints": [],
        "metrics": {},
        "error": {
            "code": code,
            "class": "protocol" if code == "protocol_error" else "internal",
            "message": message,
            "retryability": "none" if code == "protocol_error" else "operator",
        },
    }


def load_plugin_version():
    manifest_path = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "plugin.yaml"
    )
    try:
        with open(manifest_path, "r", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("version: "):
                    version = line.split(": ", 1)[1].strip().strip("'\"")
                    if version:
                        return version
    except OSError:
        pass
    return "0.0.0"


PLUGIN_VERSION = load_plugin_version()


def emit(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def read_frame():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


def artifact_hint(name, contents):
    return {"name": name, "contents": contents}


MAX_EVIDENCE_HINTS = 16
MAX_EVIDENCE_FILE_BYTES = 256 * 1024


def collect_evidence_hints():
    """Promote regular UTF-8 text files the command wrote to
    $TRANSIST_EVIDENCE_DIR as evidence hints. The runtime redacts known
    secrets before persisting them. Non-regular, symlinked, and non-UTF-8
    files are skipped so the quickstart's authored evidence is honest.
    At most MAX_EVIDENCE_HINTS files are promoted, each capped at
    MAX_EVIDENCE_FILE_BYTES; larger files are skipped."""
    evidence_dir = os.environ.get("TRANSIST_EVIDENCE_DIR") or ""
    if not evidence_dir or not os.path.isdir(evidence_dir):
        return []
    hints = []
    try:
        names = sorted(os.listdir(evidence_dir))
    except OSError:
        return []
    for name in names:
        if len(hints) >= MAX_EVIDENCE_HINTS:
            break
        full = os.path.join(evidence_dir, name)
        if not os.path.isfile(full) or os.path.islink(full):
            continue
        try:
            if os.path.getsize(full) > MAX_EVIDENCE_FILE_BYTES:
                continue
            with open(full, "r", encoding="utf-8") as handle:
                contents = handle.read()
        except (OSError, UnicodeDecodeError):
            continue
        hints.append({"name": name, "contents": contents})
    return hints


def root_run_dir(request):
    run_root = os.path.normpath(request["paths"]["run_root"])
    while os.path.basename(os.path.dirname(run_root)) == "contained_runs":
        run_root = os.path.dirname(os.path.dirname(run_root))
    run_name = os.path.basename(run_root)
    parent = os.path.dirname(run_root)
    if run_name.startswith("run_"):
        root_run_id = run_name[len("run_") :].split("__", 1)[0]
        return os.path.join(parent, f"run_{root_run_id}")
    return run_root


def resolve_repo_root(request, input_payload, require=False):
    raw_path = str((input_payload or {}).get("repo_path") or "").strip()
    if not raw_path:
        if require:
            raise ValueError("repo_path is required when using repo:// artifact paths")
        return ""
    if os.path.isabs(raw_path):
        candidate = raw_path
    else:
        candidate = os.path.join(request["paths"]["workspace_dir"], raw_path)
    resolved = os.path.normpath(candidate)
    if not os.path.isdir(resolved):
        raise ValueError(
            f"repo_path '{raw_path}' did not resolve to an existing directory"
        )
    return resolved


def resolve_recipe_source_dir(request, require=False):
    metadata = request.get("metadata") or {}
    metadata_source_dir = str(metadata.get("recipe_source_dir") or "").strip()
    if metadata_source_dir:
        if os.path.isabs(metadata_source_dir):
            return os.path.normpath(metadata_source_dir)
        return os.path.normpath(
            os.path.join(request["paths"]["workspace_dir"], metadata_source_dir)
        )

    recipe_path = os.path.join(request["paths"]["run_root"], "recipe.yaml")
    try:
        with open(recipe_path, "r", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("source_dir: "):
                    source_dir = line.split(": ", 1)[1].strip().strip("'\"")
                    if not source_dir:
                        break
                    if os.path.isabs(source_dir):
                        return os.path.normpath(source_dir)
                    return os.path.normpath(
                        os.path.join(request["paths"]["workspace_dir"], source_dir)
                    )
    except OSError:
        pass
    if require:
        raise ValueError(
            "recipe source_dir is required when using source:// artifact paths"
        )
    return ""


def resolve_source_path(request, raw_path):
    source_dir = resolve_recipe_source_dir(request, require=True)
    root = os.path.realpath(source_dir)
    candidate = os.path.realpath(os.path.join(root, str(raw_path).lstrip("/")))
    try:
        contained = os.path.commonpath([root, candidate]) == root
    except ValueError:
        contained = False
    if not contained:
        raise ValueError(
            f"source:// path '{raw_path}' escapes the staged source directory"
        )
    if not os.path.isfile(candidate):
        raise ValueError(f"source:// path '{raw_path}' did not resolve to a file")
    return candidate


def stringify_arg(raw_arg: str | int | float | bool) -> str:
    """Convert a supported argument to command-line form, preserving strings."""
    if isinstance(raw_arg, bool):
        return "true" if raw_arg else "false"
    if isinstance(raw_arg, (int, float)):
        return json.dumps(raw_arg, allow_nan=False)
    return raw_arg


def resolve_arg(request, raw_arg, input_payload=None):
    candidate = stringify_arg(raw_arg)
    stripped = candidate.strip()
    if stripped.startswith("run://"):
        return os.path.join(
            root_run_dir(request), stripped[len("run://") :].lstrip("/")
        )
    if stripped.startswith("repo://"):
        repo_root = resolve_repo_root(request, input_payload, require=True)
        return os.path.join(
            repo_root, ".transist", stripped[len("repo://") :].lstrip("/")
        )
    if stripped.startswith("source://"):
        return resolve_source_path(request, stripped[len("source://") :])
    return candidate


def resolve_args(request, input_payload):
    return [
        resolve_arg(request, arg, input_payload)
        for arg in (input_payload.get("args") or [])
    ]


def command_environment(input_payload: dict) -> dict[str, str]:
    """Forward the validated gate decision to a child, never a stale inherited one."""
    environment = os.environ.copy()
    environment.pop("TRANSIST_GATE_DECISION_JSON", None)
    gate_decision = input_payload.get("gate_decision")
    if isinstance(gate_decision, dict):
        environment["TRANSIST_GATE_DECISION_JSON"] = json.dumps(
            gate_decision, separators=(",", ":"), sort_keys=True
        )
    return environment


def main():
    handshake = read_frame()
    if handshake is None:
        return 1
    emit(
        {
            "type": "app.handshake.response.v1",
            "protocol_version": APP_PROTOCOL,
            "plugin": {"id": "shell", "version": PLUGIN_VERSION},
            "adapter_kind": "process",
            "capabilities": {
                "actions": ["exec", "exec_with_secret_env", "echo"],
                "supports_cancel": True,
                "supports_health": False,
                "supports_artifact_hints": True,
                "supports_evidence_hints": True,
            },
        }
    )

    request = read_frame()
    if request is None:
        return 1

    action = request.get("action")
    input_payload = request.get("input") or {}
    if action == "echo":
        args = input_payload.get("args") or []
        stdout = " ".join(stringify_arg(arg) for arg in args)
        emit(
            {
                "type": "app.invoke.result.v1",
                "protocol_version": APP_PROTOCOL,
                "invoke_id": request["invoke_id"],
                "status": "success",
                "output": {
                    "ok": True,
                    "stdout": stdout,
                    "stderr": "",
                    "exit_code": 0,
                },
                "artifact_hints": [
                    artifact_hint("stdout.txt", stdout),
                    artifact_hint("stderr.txt", ""),
                ],
                "evidence_hints": [],
                "metrics": {},
                "usage": None,
                "error": None,
            }
        )
        return 0

    if action in {"exec", "exec_with_secret_env"}:
        command = input_payload.get("command")
        if not command:
            emit(
                {
                    "type": "app.invoke.result.v1",
                    "protocol_version": APP_PROTOCOL,
                    "invoke_id": request["invoke_id"],
                    "status": "failed",
                    "output": {},
                    "artifact_hints": [],
                    "evidence_hints": [],
                    "metrics": {},
                    "usage": None,
                    "error": {
                        "code": "validation_error",
                        "class": "validation",
                        "message": "command is required",
                        "retryability": "none",
                    },
                }
            )
            return 0

        try:
            command = resolve_arg(request, command, input_payload)
            args = resolve_args(request, input_payload)
        except ValueError as err:
            emit(
                {
                    "type": "app.invoke.result.v1",
                    "protocol_version": APP_PROTOCOL,
                    "invoke_id": request["invoke_id"],
                    "status": "failed",
                    "output": {},
                    "artifact_hints": [],
                    "evidence_hints": [],
                    "metrics": {},
                    "usage": None,
                    "error": {
                        "code": "validation_error",
                        "class": "validation",
                        "message": str(err),
                        "retryability": "none",
                    },
                }
            )
            return 0
        try:
            completed = run_process(
                [command, *args],
                cwd=request["paths"]["workspace_dir"],
                env=command_environment(input_payload),
                timeout=None,
                on_tick=command_heartbeat(request),
            )
        except CleanupUnconfirmedError as err:
            hints = [
                artifact_hint(name, text)
                for name, text in (
                    ("stdout.txt", err.stdout),
                    ("stderr.txt", err.stderr),
                )
                if isinstance(text, str) and len(text.encode("utf-8")) <= 256 * 1024
            ]
            emit(
                runner_failure(
                    request,
                    "protocol_error",
                    "Command process cleanup could not be confirmed",
                    hints,
                )
            )
            return 0
        except Exception:
            emit(
                runner_failure(
                    request,
                    "adapter_internal_error",
                    "Command runner failed",
                    [],
                )
            )
            return 0
        status = "success" if completed.returncode == 0 else "failed"
        error = None
        if completed.returncode != 0:
            error = {
                "code": "tool_execution_error",
                "class": "tool",
                "message": f"command exited with code {completed.returncode}",
                "retryability": "safe",
            }
        # If stdout is a JSON object, promote its fields into the output so
        # workflow guards can read them directly as out.<field>.  The fixed
        # fields (ok, stdout, stderr, exit_code) always take precedence.
        stdout_fields: dict = {}
        try:
            parsed = json.loads(completed.stdout.strip())
            if isinstance(parsed, dict):
                stdout_fields = parsed
        except (json.JSONDecodeError, ValueError):
            pass
        emit(
            {
                "type": "app.invoke.result.v1",
                "protocol_version": APP_PROTOCOL,
                "invoke_id": request["invoke_id"],
                "status": status,
                "output": {
                    **stdout_fields,
                    "ok": completed.returncode == 0,
                    "stdout": completed.stdout,
                    "stderr": completed.stderr,
                    "exit_code": completed.returncode,
                },
                "artifact_hints": [
                    artifact_hint("stdout.txt", completed.stdout),
                    artifact_hint("stderr.txt", completed.stderr),
                ],
                "evidence_hints": collect_evidence_hints(),
                "metrics": {},
                "usage": None,
                "error": error,
            }
        )
        return 0

    emit(
        {
            "type": "app.invoke.result.v1",
            "protocol_version": APP_PROTOCOL,
            "invoke_id": request["invoke_id"],
            "status": "failed",
            "output": {},
            "artifact_hints": [],
            "evidence_hints": [],
            "metrics": {},
            "usage": None,
            "error": {
                "code": "validation_error",
                "class": "validation",
                "message": f"unsupported action {action}",
                "retryability": "none",
            },
        }
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
