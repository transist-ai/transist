#!/usr/bin/env python3
from __future__ import annotations

import sys
from datetime import datetime, timezone

from blob_utils import runtime_paths, write_json_blob, write_text_blob


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit("usage: prepare_workspace.py <session_name> <checkpoint_number>")

    session_name = sys.argv[1]
    checkpoint_number = int(sys.argv[2])
    paths = runtime_paths()
    operator_dir = paths.run_root / "operator"
    operator_dir.mkdir(parents=True, exist_ok=True)

    manifest = {
        "session_name": session_name,
        "checkpoint_number": checkpoint_number,
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "workspace_dir": str(paths.workspace_dir),
        "run_root": str(paths.run_root),
        "artifacts_dir": str(paths.artifacts_dir),
        "evidence_dir": str(paths.evidence_dir),
        "operator_dir": str(operator_dir),
    }
    write_json_blob("artifacts", "000_session_manifest.json", manifest)

    notes = "\n".join(
        [
            f"session: {session_name}",
            f"checkpoint_number: {checkpoint_number}",
            f"operator_dir: {operator_dir}",
            "request-changes will regenerate the review candidate with the latest feedback.",
            "resume demo expects you to create operator/resume.ok before running runs resume.",
        ]
    )
    write_text_blob("artifacts", "001_operator_notes.txt", notes + "\n")

    trace = {
        "script": "prepare_workspace.py",
        "operator_dir_exists": operator_dir.exists(),
        "operator_dir": str(operator_dir),
        "session_name": session_name,
        "checkpoint_number": checkpoint_number,
    }
    write_json_blob("evidence", "000_prepare_trace.json", trace)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
