#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class RuntimePaths:
    artifacts_dir: Path
    evidence_dir: Path
    workspace_dir: Path


def runtime_paths() -> RuntimePaths:
    return RuntimePaths(
        artifacts_dir=_required_path("TRANSIST_ARTIFACTS_DIR"),
        evidence_dir=_required_path("TRANSIST_EVIDENCE_DIR"),
        workspace_dir=_required_path("TRANSIST_WORKSPACE_DIR"),
    )


def write_text_blob(
    family: str,
    file_name: str,
    text: str,
    *,
    status: str = "success",
    invoke_id: str | None = None,
) -> Path:
    return _write_blob(
        family=family,
        file_name=file_name,
        payload=text.encode("utf-8"),
        status=status,
        invoke_id=invoke_id,
    )


def write_json_blob(
    family: str,
    file_name: str,
    payload: Any,
    *,
    status: str = "success",
    invoke_id: str | None = None,
) -> Path:
    body = json.dumps(payload, indent=2, sort_keys=True).encode("utf-8") + b"\n"
    return _write_blob(
        family=family,
        file_name=file_name,
        payload=body,
        status=status,
        invoke_id=invoke_id,
    )


def _write_blob(
    *,
    family: str,
    file_name: str,
    payload: bytes,
    status: str,
    invoke_id: str | None,
) -> Path:
    paths = runtime_paths()
    directory = _family_dir(paths, family)
    directory.mkdir(parents=True, exist_ok=True)

    target = directory / file_name
    target.write_bytes(payload)

    descriptor = {
        "kind": family,
        "source_path": str(target),
        "sha256": hashlib.sha256(payload).hexdigest(),
        "bytes": len(payload),
        "status": status,
    }
    if invoke_id:
        descriptor["invoke_id"] = invoke_id

    descriptor_path = target.with_name(f"{target.stem}.descriptor.json")
    descriptor_path.write_text(json.dumps(descriptor, indent=2, sort_keys=True) + "\n")
    return target


def _family_dir(paths: RuntimePaths, family: str) -> Path:
    if family == "artifacts":
        return paths.artifacts_dir
    if family == "evidence":
        return paths.evidence_dir
    raise ValueError(f"unsupported blob family: {family}")


def _required_path(name: str) -> Path:
    raw = os.environ.get(name, "").strip()
    if not raw:
        raise RuntimeError(f"{name} is required")
    return Path(raw)
