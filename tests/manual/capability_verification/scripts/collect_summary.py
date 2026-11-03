#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
from pathlib import Path

from blob_utils import runtime_paths, write_json_blob, write_text_blob


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: collect_summary.py <session_name>")

    session_name = sys.argv[1]
    paths = runtime_paths()
    state_path = paths.run_root / "state.json"
    state = json.loads(state_path.read_text()) if state_path.exists() else {}
    run_inputs = _load_run_inputs(paths.run_root)

    review_run_root = _find_review_run_root(paths.run_root, run_inputs)
    review_root = review_run_root or paths.run_root

    render_revision_executions = _execution_count(review_root, "render_revision")
    resume_executions = _execution_count(paths.run_root, "resume_checkpoint")
    gate_records = _load_gate_records(review_root / "gates")
    latest_gate = gate_records[-1] if gate_records else {}
    artifacts_root = paths.run_root / "artifacts"

    summary = {
        "session_name": session_name,
        "run_id": state.get("run_id"),
        "workflow_id": state.get("workflow_id"),
        "workflow_version": state.get("workflow_version"),
        "run_status": state.get("status"),
        "optional_branch_step_status": _optional_step_status(state, run_inputs),
        "render_revision_executions": render_revision_executions,
        "gate_decision_count": len(gate_records),
        "latest_gate_decision": latest_gate.get("decision"),
        "latest_gate_reason": latest_gate.get("reason"),
        "resume_checkpoint_ran": resume_executions > 0,
        "resume_checkpoint_executions": resume_executions,
        "review_run_root": str(review_root),
        "review_run_id": review_root.name.removeprefix("run_"),
        "paths": {
            "recipe_exists": (paths.run_root / "recipe.yaml").exists(),
            "run_inputs_exists": (paths.run_root / "context" / "run_inputs.json").exists(),
            "events_exists": (paths.run_root / "events.jsonl").exists(),
            "step_executions_exists": (paths.run_root / "step_executions").exists(),
            "gates_exists": (paths.run_root / "gates").exists(),
            "review_gates_exists": (review_root / "gates").exists(),
            "artifacts_root_exists": artifacts_root.exists(),
            "evidence_tree_exists": _evidence_tree_exists(artifacts_root),
        },
    }
    write_json_blob("artifacts", "000_verification_summary.json", summary)

    inspection_lines = [
        f"run_root={paths.run_root}",
        f"state_json={paths.run_root / 'state.json'}",
        f"recipe_yaml={paths.run_root / 'recipe.yaml'}",
        f"run_inputs_json={paths.run_root / 'context' / 'run_inputs.json'}",
        f"events_jsonl={paths.run_root / 'events.jsonl'}",
        f"step_executions_dir={paths.run_root / 'step_executions'}",
        f"gates_dir={paths.run_root / 'gates'}",
        f"artifacts_root={artifacts_root}",
    ]
    write_text_blob("evidence", "000_inspection_paths.txt", "\n".join(inspection_lines) + "\n")
    return 0


def _execution_count(run_root: Path, step_id: str) -> int:
    executions_dir = run_root / "step_executions" / step_id
    if not executions_dir.exists():
        return 0
    return len(list(executions_dir.glob("execution_*.json")))


def _step_status(state: dict, step_id: str) -> str | None:
    steps = state.get("steps") or {}
    step = steps.get(step_id) or {}
    return step.get("status")


def _optional_step_status(state: dict, run_inputs: dict) -> str | None:
    status = _step_status(state, "optional_echo")
    if status and status != "pending":
        return status
    return "succeeded" if run_inputs.get("include_optional_branch") else "skipped"


def _load_run_inputs(run_root: Path) -> dict:
    record_path = run_root / "context" / "run_inputs.json"
    if not record_path.exists():
        return {}
    payload = json.loads(record_path.read_text())
    return payload.get("values") or {}


def _find_review_run_root(run_root: Path, run_inputs: dict) -> Path | None:
    session_name = str(run_inputs.get("session_name") or "").strip()
    if not session_name:
        return None

    home_dir = run_root.parent.parent
    runs_dir = home_dir / "runs"
    if not runs_dir.exists():
        return None

    candidates: list[tuple[int, Path]] = []
    for state_path in runs_dir.glob("run_*/state.json"):
        try:
            payload = json.loads(state_path.read_text())
        except json.JSONDecodeError:
            continue
        if payload.get("workflow_id") != "review_loop":
            continue
        workflow_inputs = payload.get("workflow_inputs") or {}
        if (workflow_inputs.get("session_name") or "").strip() != session_name:
            continue
        candidates.append((int(payload.get("updated_at_ms") or 0), state_path.parent))

    if not candidates:
        return None
    candidates.sort(key=lambda item: item[0])
    return candidates[-1][1]


def _load_gate_records(gates_dir: Path) -> list[dict]:
    if not gates_dir.exists():
        return []

    records: list[dict] = []
    seen: set[str] = set()

    for path in sorted(gates_dir.glob("*.json")):
        raw = path.read_text()
        if raw in seen:
            continue
        seen.add(raw)
        try:
            records.append(json.loads(raw))
        except json.JSONDecodeError:
            continue

    history_dir = gates_dir / "history"
    if history_dir.exists():
        for path in sorted(history_dir.glob("*.json")):
            raw = path.read_text()
            if raw in seen:
                continue
            seen.add(raw)
            try:
                records.append(json.loads(raw))
            except json.JSONDecodeError:
                continue

    records.sort(key=lambda item: (item.get("decided_at_ms") or 0, item.get("decision") or ""))
    return records


def _evidence_tree_exists(artifacts_root: Path) -> bool:
    if not artifacts_root.exists():
        return False
    return any(path.is_dir() and path.name == "evidence" for path in artifacts_root.rglob("evidence"))


if __name__ == "__main__":
    raise SystemExit(main())
