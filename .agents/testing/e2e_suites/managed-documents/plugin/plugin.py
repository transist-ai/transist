#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Finite offline APP fixture: exact files, trees, boundary failures and path echoes."""

import base64
import json
import os
from pathlib import Path
import sys


def emit(value: dict) -> None:
    """Write one APP response to stdout."""
    print(json.dumps(value), flush=True)


def denied(operation) -> None:
    """Require the sandbox to deny an attempted mutation/read."""
    try:
        operation()
    except OSError:
        return
    raise AssertionError("sandbox unexpectedly allowed protected operation")


try:
    with (Path.cwd() / "startups").open("a") as startup_log:
        startup_log.write("started\n")
except OSError:
    pass
json.loads(sys.stdin.readline())
emit(
    {
        "type": "app.handshake.response.v1",
        "protocol_version": "app.v1",
        "plugin": {"id": "fixture.documents", "version": "0.1.0"},
        "adapter_kind": "process",
        "capabilities": {
            "actions": ["call", "strict", "typed"],
            "supports_cancel": False,
            "supports_health": False,
            "supports_artifact_hints": True,
            "supports_evidence_hints": False,
        },
    }
)
line = sys.stdin.readline()
if not line:
    sys.exit(0)
request = json.loads(line)
data = request.get("input", {})
output = {"ok": True, "paths": [], "seen": [], "structured": data.get("structured")}
for item in data.get("ops", []):
    path = Path(item["path"])
    op = item["op"]
    content = item.get("content", "business\n").encode()
    if op == "write":
        assert not path.exists()
        path.write_bytes(content)
    elif op == "update":
        output["seen"].append(path.read_text())
        path.write_bytes(path.read_bytes() + content)
    elif op == "runtime_metadata":
        metadata = Path(os.environ["TRANSIST_RUNTIME_METADATA_FILE"]).read_bytes()
        assert json.loads(metadata)["run"] == {"id": request["run_id"]}
        path.write_bytes(metadata)
        output["value"] = metadata.decode()
    elif op in ("tree", "tree_update"):
        path.mkdir(exist_ok=op == "tree_update")
        (path / "empty").mkdir(exist_ok=True)
        (path / "sub").mkdir(exist_ok=True)
        (path / "sub" / "member.txt").write_bytes(content)
    elif op in ("read", "tree_read"):
        leaf = path / "sub" / "member.txt" if op == "tree_read" else path
        assert leaf.read_bytes() == content
        output["seen"].append(leaf.read_text())
        denied(lambda: leaf.write_bytes(b"mutated"))
        denied(lambda: leaf.chmod(0o600))
        denied(leaf.unlink)
        denied(lambda: path.rename(path.with_name("moved")))
        denied(lambda: os.link(leaf, path.parent.parent / "write" / "alias"))
        documents_root = path.parent.parent
        denied(
            lambda: documents_root.rename(documents_root.with_name("moved-documents"))
        )
        temporary_root = documents_root.parent
        if data.get("workspace_alias_probe"):
            host_alias = (
                Path.cwd()
                / "tmp"
                / temporary_root.name
                / "documents"
                / "read"
                / path.name
            )
            assert (
                not host_alias.exists()
            ), "read copy exposed through workspace TMPDIR alias"
        denied(
            lambda: temporary_root.rename(
                temporary_root.with_name(temporary_root.name + "-moved")
            )
        )
        if op == "tree_read":
            denied(lambda: (path / "new").write_bytes(b"mutated"))
    elif op == "symlink":
        path.symlink_to(Path.cwd() / "source.txt")
    elif op == "tree_symlink":
        path.mkdir()
        (path / "escape").symlink_to(Path.cwd())
    elif op == "oversized":
        path.write_bytes(b"x" * 4097)
    elif op == "too_many":
        path.mkdir()
        for index in range(4):
            (path / str(index)).write_bytes(b"x")
    elif op == "secret":
        secret = os.environ["DOCUMENT_TOKEN"].encode()
        if item.get("encoded"):
            secret = base64.urlsafe_b64encode(secret).rstrip(b"=")
        path.write_bytes(secret)
    elif op == "manifest_secret":
        path.mkdir()
        (path / "empty.txt").write_bytes(b"")
    elif op != "missing":
        raise AssertionError("unknown fixture operation")
    output["paths"].append(
        {"exact": str(path), "nested": ["prefix=" + str(path) + "/child suffix"]}
    )
if data.get("home_probe"):
    denied(lambda: Path(data["home_probe"]).read_bytes())
if data.get("workspace_read_only"):
    denied(lambda: (Path.cwd() / "forbidden").write_bytes(b"mutated"))
if data.get("large"):
    output["padding"] = "x" * 262144
    artifact = Path(os.environ["TRANSIST_ARTIFACTS_DIR"]) / "metadata.txt"
    artifact.write_text(json.dumps(output))
    artifacts = [{"path": str(artifact)}]
else:
    artifacts = []
if data.get("invalid_result"):
    output["ok"] = False
if "return_value" in data:
    output["value"] = data["return_value"]
if data.get("smuggle"):
    output["value"] = {"$document": "file", "access": "read"}
status = data.get("status", "success")
if data.get("fail_first") and request["step"]["execution"] == 1:
    status = "failed"
emit(
    {
        "type": "app.invoke.result.v1",
        "protocol_version": "app.v1",
        "invoke_id": request["invoke_id"],
        "status": status,
        "output": output,
        "artifact_hints": artifacts,
        "evidence_hints": [],
        "metrics": {},
        "usage": None,
        "error": None,
    }
)
