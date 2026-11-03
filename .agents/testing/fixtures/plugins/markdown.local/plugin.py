#!/usr/bin/env python3
"""markdown.local — local Markdown formatting and file-writing.

Two actions:
- `format`: light-touch normalisation (heading style, optional wrap, trailing
  newline). No LLM involvement.
- `write_file`: write a Markdown file to disk inside the run workspace.
"""

import json
import os
import re
import sys
import textwrap

APP_PROTOCOL = "app.v1"
PLUGIN_ID = "markdown.local"
PLUGIN_VERSION = "0.1.0"
ACTIONS = ["format", "write_file"]

CANONICAL_ERRORS = {
    "validation_error": {"class": "validation", "retryability": "none"},
    "tool_execution_error": {"class": "tool", "retryability": "safe"},
    "adapter_internal_error": {"class": "internal", "retryability": "operator"},
}


def emit(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def read_frame():
    line = sys.stdin.readline()
    return json.loads(line) if line else None


def protocol_error(code, message):
    code = code if code in CANONICAL_ERRORS else "adapter_internal_error"
    details = CANONICAL_ERRORS[code]
    return {
        "code": code,
        "class": details["class"],
        "message": str(message),
        "retryability": details["retryability"],
    }


def invoke_result(invoke_id, status, output=None, artifact_hints=None, error=None):
    return {
        "type": "app.invoke.result.v1",
        "protocol_version": APP_PROTOCOL,
        "invoke_id": invoke_id,
        "status": status,
        "output": output or {},
        "artifact_hints": artifact_hints or [],
        "evidence_hints": [],
        "metrics": {},
        "usage": None,
        "error": error,
    }


def failed(invoke_id, code, message, output=None):
    return invoke_result(invoke_id, "failed", output=output, error=protocol_error(code, message))


def hint(name, contents):
    return {"name": name, "contents": contents}


def workspace_path(request, raw_path):
    candidate = str(raw_path).strip()
    if not candidate:
        return ""
    if os.path.isabs(candidate):
        return candidate
    return os.path.join(request["paths"]["workspace_dir"], candidate)


def resolve_text(payload, request, key_text="text", key_file="text_file"):
    """Return text from inline `text` or a file path. Empty string is allowed."""
    text = payload.get(key_text)
    text_file = payload.get(key_file)
    t_set = isinstance(text, str)
    f_set = isinstance(text_file, str) and bool(text_file.strip())
    if t_set and f_set:
        raise ValueError(f"only one of {key_text} or {key_file} may be provided")
    if t_set:
        return text
    if f_set:
        resolved = workspace_path(request, text_file)
        with open(resolved, "r", encoding="utf-8") as handle:
            return handle.read()
    return ""


def normalize_headings(text):
    """Convert Setext (=== / ---) headings to ATX (# / ##)."""
    lines = text.splitlines()
    out = []
    i = 0
    while i < len(lines):
        line = lines[i]
        next_line = lines[i + 1] if i + 1 < len(lines) else ""
        if line.strip() and re.fullmatch(r"=+", next_line.strip() or ""):
            out.append(f"# {line.strip()}")
            i += 2
            continue
        if line.strip() and re.fullmatch(r"-+", next_line.strip() or "") and len(next_line.strip()) >= 2:
            out.append(f"## {line.strip()}")
            i += 2
            continue
        out.append(line)
        i += 1
    return "\n".join(out)


def wrap_text(text, wrap_at):
    if wrap_at <= 0:
        return text
    wrapped = []
    in_code = False
    for line in text.splitlines():
        if line.startswith("```"):
            in_code = not in_code
            wrapped.append(line)
            continue
        if in_code or not line.strip() or line.lstrip().startswith(("#", "-", "*", ">", "|")):
            wrapped.append(line)
            continue
        wrapped.extend(textwrap.wrap(line, width=wrap_at) or [""])
    return "\n".join(wrapped)


def handle_format(request):
    invoke_id = request["invoke_id"]
    payload = request.get("input") or {}
    try:
        text = resolve_text(payload, request)
    except (OSError, ValueError) as err:
        return failed(invoke_id, "validation_error", str(err))

    if payload.get("normalize_headings"):
        text = normalize_headings(text)
    wrap_at = int(payload.get("wrap_at") or 0)
    if wrap_at:
        text = wrap_text(text, wrap_at)
    if payload.get("ensure_trailing_newline", True) and text and not text.endswith("\n"):
        text = text + "\n"

    output = {
        "formatted_text": text,
        "byte_length": len(text.encode("utf-8")),
        "line_count": text.count("\n"),
    }
    artifact_hints = [hint("formatted.md", text)]

    raw_output_path = str(payload.get("output_path") or "").strip()
    if raw_output_path:
        target = workspace_path(request, raw_output_path)
        parent = os.path.dirname(target)
        if parent and not os.path.isdir(parent):
            try:
                os.makedirs(parent, exist_ok=True)
            except OSError as err:
                return failed(invoke_id, "tool_execution_error", f"failed creating parent: {err}")
        try:
            with open(target, "w", encoding="utf-8") as handle:
                handle.write(text)
        except OSError as err:
            return failed(invoke_id, "tool_execution_error", f"failed writing output_path: {err}")
        output["output_path"] = target
        artifact_hints = [hint(os.path.basename(target), text)]

    return invoke_result(
        invoke_id,
        "success",
        output=output,
        artifact_hints=artifact_hints,
    )


def handle_write_file(request):
    invoke_id = request["invoke_id"]
    payload = request.get("input") or {}
    raw_path = str(payload.get("path") or "").strip()
    if not raw_path:
        return failed(invoke_id, "validation_error", "path is required")
    try:
        text = resolve_text(payload, request)
    except (OSError, ValueError) as err:
        return failed(invoke_id, "validation_error", str(err))

    target = workspace_path(request, raw_path)
    parent = os.path.dirname(target)
    if parent and not os.path.isdir(parent):
        if payload.get("create_parents", True):
            try:
                os.makedirs(parent, exist_ok=True)
            except OSError as err:
                return failed(invoke_id, "tool_execution_error", f"failed creating parent: {err}")
        else:
            return failed(invoke_id, "tool_execution_error", f"parent directory does not exist: {parent}")

    existed = os.path.exists(target)
    if existed and not payload.get("overwrite", True):
        return failed(invoke_id, "tool_execution_error", f"path exists and overwrite=false: {target}")

    try:
        with open(target, "w", encoding="utf-8") as handle:
            handle.write(text)
            if text and not text.endswith("\n"):
                handle.write("\n")
    except OSError as err:
        return failed(invoke_id, "tool_execution_error", f"failed writing file: {err}")

    return invoke_result(
        invoke_id,
        "success",
        output={
            "path": target,
            "byte_length": len(text.encode("utf-8")),
            "created": not existed,
        },
        artifact_hints=[hint(os.path.basename(target), text)],
    )


def handle_request(request):
    try:
        action = request.get("action")
        if action == "format":
            return handle_format(request)
        if action == "write_file":
            return handle_write_file(request)
        return failed(request["invoke_id"], "validation_error", f"unsupported action {action}")
    except Exception as err:
        return failed(
            request["invoke_id"],
            "adapter_internal_error",
            f"unexpected markdown.local error: {type(err).__name__}: {err}",
        )


def main():
    if read_frame() is None:
        return 1
    emit({
        "type": "app.handshake.response.v1",
        "protocol_version": APP_PROTOCOL,
        "plugin": {"id": PLUGIN_ID, "version": PLUGIN_VERSION},
        "adapter_kind": "process",
        "capabilities": {
            "actions": ACTIONS,
            "supports_cancel": False,
            "supports_health": False,
            "supports_artifact_hints": True,
            "supports_evidence_hints": False,
        },
    })
    request = read_frame()
    if request is None:
        return 1
    emit(handle_request(request))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
