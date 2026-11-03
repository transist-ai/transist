#!/usr/bin/env python3
from __future__ import annotations

import json
import sys

from blob_utils import runtime_paths, write_json_blob, write_text_blob


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: check_resume_marker.py <session_name>")

    session_name = sys.argv[1]
    paths = runtime_paths()
    marker_path = paths.run_root / "operator" / "resume.ok"
    exercise_resume_demo = _exercise_resume_demo(paths.run_root)

    write_text_blob("artifacts", "000_resume_marker_path.txt", f"{marker_path}\n")

    marker_exists = marker_path.exists()
    marker_body = marker_path.read_text().strip() if marker_exists else ""
    write_json_blob(
        "evidence",
        "000_resume_check.json",
        {
            "script": "check_resume_marker.py",
            "session_name": session_name,
            "exercise_resume_demo": exercise_resume_demo,
            "marker_path": str(marker_path),
            "marker_exists": marker_exists,
            "marker_contents": marker_body,
        },
    )

    if not exercise_resume_demo:
        acknowledgement = "\n".join(
            [
                f"session: {session_name}",
                "resume demo disabled for this run",
                f"marker path: {marker_path}",
            ]
        )
        write_text_blob("artifacts", "001_resume_acknowledged.txt", acknowledgement + "\n")
        return 0

    if not marker_exists:
        message = (
            f"resume marker missing: create {marker_path} and run "
            f"'cargo run -p transist-cli -- runs resume <run-id>'"
        )
        print(message, file=sys.stderr)
        return 1

    acknowledgement = "\n".join(
        [
            f"session: {session_name}",
            f"resume marker detected at: {marker_path}",
            f"marker contents: {marker_body or '(empty)'}",
        ]
    )
    write_text_blob("artifacts", "001_resume_acknowledged.txt", acknowledgement + "\n")
    return 0


def _exercise_resume_demo(run_root):
    record_path = run_root / "context" / "run_inputs.json"
    if not record_path.exists():
        return False
    payload = json.loads(record_path.read_text())
    values = payload.get("values") or {}
    return bool(values.get("exercise_resume_demo"))


if __name__ == "__main__":
    raise SystemExit(main())
