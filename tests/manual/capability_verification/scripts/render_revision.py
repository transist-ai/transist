#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
from pathlib import Path

from blob_utils import runtime_paths, write_json_blob, write_text_blob


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: render_revision.py <session_name>")

    session_name = sys.argv[1]
    paths = runtime_paths()
    decisions = _load_gate_history(paths.run_root / "gates")
    revision_requests = [
        record
        for record in decisions
        if record.get("gate_id") == "verification-review"
        and record.get("decision") == "request_changes"
    ]

    revision_number = len(revision_requests) + 1
    latest_feedback = ""
    if revision_requests:
        latest_feedback = (revision_requests[-1].get("reason") or "").strip()

    lines = [
        "# Verification Candidate",
        "",
        f"- Session: `{session_name}`",
        f"- Revision: `{revision_number}`",
        f"- Run root: `{paths.run_root}`",
        "",
    ]
    if latest_feedback:
        lines.extend(
            [
                "## Latest Review Feedback",
                "",
                latest_feedback,
                "",
            ]
        )
    else:
        lines.extend(
            [
                "## Latest Review Feedback",
                "",
                "Initial draft. No `request-changes` decision has been recorded yet.",
                "",
            ]
        )
    lines.extend(
        [
            "## Checks Exercised",
            "",
            "- invoke step execution",
            "- guarded branch behavior",
            "- subflow execution",
            "- manual gate review loop",
            "- run artifact and evidence inspection",
            "",
            "Approve to continue, request changes to trigger another render execution, or reject to terminate the run.",
            "",
        ]
    )
    write_text_blob("artifacts", "000_review_candidate.md", "\n".join(lines))

    trace = {
        "script": "render_revision.py",
        "session_name": session_name,
        "revision_number": revision_number,
        "request_changes_count": len(revision_requests),
        "latest_feedback": latest_feedback,
        "history_files": _history_files(paths.run_root / "gates" / "history"),
    }
    write_json_blob("evidence", "000_review_trace.json", trace)
    return 0


def _load_gate_history(gates_dir: Path) -> list[dict]:
    if not gates_dir.exists():
        return []

    records: list[dict] = []
    history_dir = gates_dir / "history"
    for path in sorted(history_dir.glob("*.json")) if history_dir.exists() else []:
        try:
            payload = json.loads(path.read_text())
        except json.JSONDecodeError:
            continue
        payload["_history_path"] = str(path)
        records.append(payload)
    records.sort(key=lambda item: (item.get("decided_at_ms") or 0, item.get("_history_path", "")))
    return records


def _history_files(history_dir: Path) -> list[str]:
    if not history_dir.exists():
        return []
    return [str(path) for path in sorted(history_dir.glob("*.json"))]


if __name__ == "__main__":
    raise SystemExit(main())
