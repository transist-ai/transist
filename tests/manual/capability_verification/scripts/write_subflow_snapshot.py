#!/usr/bin/env python3
from __future__ import annotations

import sys
from datetime import datetime, timezone

from blob_utils import runtime_paths, write_json_blob, write_text_blob


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: write_subflow_snapshot.py <session_name>")

    session_name = sys.argv[1]
    paths = runtime_paths()

    note = "\n".join(
        [
            f"session: {session_name}",
            "subflow step: record_subflow_snapshot",
            f"subflow_run_root: {paths.run_root}",
            "This artifact is emitted from the child subflow run.",
        ]
    )
    write_text_blob("artifacts", "000_subflow_note.txt", note + "\n")

    trace = {
        "script": "write_subflow_snapshot.py",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "session_name": session_name,
        "subflow_run_root": str(paths.run_root),
        "workspace_dir": str(paths.workspace_dir),
    }
    write_json_blob("evidence", "000_subflow_trace.json", trace)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
