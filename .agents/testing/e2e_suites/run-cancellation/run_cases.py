#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Synchronize real CLI cancellation races on persisted invocation evidence."""

import json
import pathlib
import subprocess
import sys
import time
from collections.abc import Callable
from typing import TypeVar

T = TypeVar("T")


def wait_for(label: str, read: Callable[[], T], timeout: float = 30) -> T:
    """Poll bounded evidence reads until truthy, returning the matching evidence."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = read()
        except (OSError, ValueError):
            value = None
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {label}")


def read_json(path: pathlib.Path) -> dict:
    """Read a persisted runtime object without modifying it."""
    return json.loads(path.read_text(encoding="utf-8"))


def active_record(home: pathlib.Path, run_dir: pathlib.Path, step_id: str) -> dict | None:
    """Match live logical/staged process records to persisted run/step/invoke identity."""
    state = read_json(run_dir / "state.json")
    step = state.get("steps", {}).get(step_id) or (
        (state.get("cleanup") or {}).get("steps", {}).get(step_id)
    ) or {}
    invoke_id = (step.get("last_request") or {}).get("invoke_id")
    if not invoke_id:
        return None
    paths = list((run_dir / "active_invocations").glob("*.json"))
    paths += list((home / ".transist-staging").glob("invoke-*/active_invocations/*.json"))
    for path in paths:
        try:
            record = read_json(path)
        except (OSError, ValueError):
            continue
        if (
            record.get("run_id") == state.get("run_id")
            and record.get("step_id") == step_id
            and record.get("invoke_id") == invoke_id
            and record.get("adapter_kind") == "process"
        ):
            return {**record, "_record_path": str(path)}
    return None


def runner_finished(run_dir: pathlib.Path) -> bool:
    """Read the latest runner lifecycle event, after lease finalization."""
    events = []
    for line in (run_dir / "runner.log").read_text(encoding="utf-8").splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if (
            isinstance(event, dict)
            and event.get("schema") == "transist.runner_event.v1"
            and event.get("event") in ("started", "finished")
        ):
            events.append(event)
    return bool(events) and events[-1]["event"] == "finished"


def run_case(binary: str, home: pathlib.Path, workspace: pathlib.Path, nested: bool) -> None:
    """Cancel primary work, repeat cancel during gated cleanup, and assert root-only teardown."""
    kind = "nested" if nested else "direct"
    run_id = f"{kind}-cancel"
    workspace.mkdir()
    root = home / "runs" / f"run_{run_id}"
    child_id = f"{run_id}__child"
    child = root / "contained_runs" / f"run_{child_id}"
    primary = child if nested else root
    command = [binary, "--home", str(home)]
    callers: list[tuple[str, subprocess.Popen]] = []

    def cli(args: list[str], must: bool = True) -> subprocess.CompletedProcess:
        """Run one bounded foreground CLI command in this case's exact workspace."""
        result = subprocess.run(
            command + args, cwd=workspace, capture_output=True, text=True, timeout=25
        )
        if must and result.returncode != 0:
            raise AssertionError(f"{args}: {result.stdout}\n{result.stderr}")
        return result

    def cleanup_running() -> dict | None:
        """Require both persisted cleanup dispatch and its live process evidence."""
        state = read_json(root / "state.json")
        cleanup = state.get("cleanup") or {}
        if (
            state.get("status") == "canceled"
            and cleanup.get("state") == "running"
            and cleanup.get("trigger") == "canceled"
            and (workspace / "cleanup-started").is_file()
        ):
            return active_record(home, root, "teardown")
        return None

    def start_cancel(target: str) -> None:
        """Start and track a concurrent cancellation observer without waiting for it."""
        process = subprocess.Popen(
            command + ["runs", "cancel", target],
            cwd=workspace,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        callers.append((target, process))

    def assert_callers_waiting(invocation: dict) -> None:
        """Check callers remain pending while the same gated cleanup is still live."""
        for target, process in callers:
            if process.poll() is not None:
                stdout, stderr = process.communicate(timeout=5)
                raise AssertionError(
                    f"cancel {target} returned before cleanup release "
                    f"(rc={process.returncode}): {stdout}\n{stderr}"
                )
        assert cleanup_running() == invocation, (
            "cancel replaced or interrupted cleanup invocation"
        )
        assert not (workspace / "cleanup-signaled").exists(), "cleanup received a signal"
        assert not list((root / "active_invocations").glob("*.cancel")), (
            "cleanup was signaled on disk"
        )
        assert not pathlib.Path(invocation["_record_path"]).with_suffix(".cancel").exists(), (
            "staged cleanup invocation was signaled on disk"
        )
        assert not (workspace / "cleanup-canceled.txt").exists(), (
            "cleanup bypassed release gate"
        )

    try:
        cli(["workflows", "run", f"e2e_cancel_{kind}@0.1.0", "--run-ref", run_id])
        primary_invocation = wait_for(
            f"{kind} primary invoke",
            lambda: (workspace / "main-started").is_file()
            and active_record(home, primary, "wait"),
        )
        print(f"OK: {kind} primary invocation ready: {primary_invocation['_record_path']}", flush=True)
        # Keep the original cancellation command independent of the test driver:
        # cleanup must start even while that command is settling the root outcome.
        start_cancel(run_id)
        invocation = wait_for(f"{kind} automatic canceled cleanup invoke", cleanup_running)
        print(f"OK: {kind} cleanup invocation ready: {invocation['_record_path']}", flush=True)
        assert_callers_waiting(invocation)

        # Every caller observes the same live cancellation, including callers
        # addressing a contained run. None may return until teardown and lease
        # release complete, so keep every command concurrent with the gate.
        for _ in range(3):
            for target in ([run_id, child_id] if nested else [run_id]):
                start_cancel(target)
            # A foreground inspection provides an independent runtime round trip
            # while cancellation observers run; do not guess startup with sleeps.
            observed = json.loads(cli(["runs", "show", run_id]).stdout)
            assert observed["status"] == "canceled", observed
            assert observed["cleanup"]["state"] == "running", observed
            assert_callers_waiting(invocation)

        (workspace / "cleanup-release").touch()
        deadline = time.monotonic() + 30
        for index, (target, process) in enumerate(callers):
            stdout, stderr = process.communicate(
                timeout=max(0.1, deadline - time.monotonic())
            )
            assert process.returncode == 0, (target, stdout, stderr)
            report = json.loads(stdout)
            assert report["run_id"] == target and report["root_run_id"] == run_id, report
            assert report["cancellation_requested"] is True, report
            assert report["settled"] is True and report["final_status"] == "canceled", report
            assert report["cleanup_state"] == "succeeded" and report.get("error") is None, report
            assert report.get("active_execution_cancellation_error") is None, report
            if index > 0:
                assert report.get("active_execution_cancellation") is None, report
                assert report.get("recovery_runner_pid") is None, report
        wait_for(
            f"{kind} cleanup succeeded",
            lambda: (read_json(root / "state.json").get("cleanup") or {}).get("state")
            == "succeeded",
        )
        wait_for(f"{kind} runner lease finalization", lambda: runner_finished(root))
        state = read_json(root / "state.json")
        assert state["status"] == "canceled", state
        cleanup = state["cleanup"]
        assert (
            cleanup["trigger"] == "canceled" and cleanup.get("finished_at_ms") is not None
        ), cleanup
        step = cleanup["steps"]["teardown"]
        assert step["status"] == "succeeded" and step["executions"] == 1, step
        assert step["last_result"]["status"] == "success", step
        assert (workspace / "cleanup-attempts.txt").read_text() == "attempt\n"
        assert (workspace / "cleanup-canceled.txt").read_text() == "cleanup-canceled ran\n"
        for unexpected in ("main-completed", "cleanup-signaled", "child-cleanup-ran"):
            assert not (workspace / unexpected).exists(), unexpected
        assert not list(root.rglob("active_invocations/*.json")), "invocations leaked after cleanup"
        assert not pathlib.Path(invocation["_record_path"]).exists(), "staged cleanup invocation leaked"
        assert not pathlib.Path(primary_invocation["_record_path"]).exists(), "staged primary invocation leaked"
        assert read_json(primary / "state.json")["steps"]["wait"]["status"] == "canceled"
        if nested:
            child_state = read_json(child / "state.json")
            assert child_state["status"] == "canceled", child_state
            assert child_state.get("cleanup") is None, "contained run dispatched its own cleanup"
        print(f"OK: {kind} repeated cancel preserved one root-only automatic cleanup", flush=True)
    except Exception:
        for path in root.rglob("state.json"):
            print(f"Evidence {path}:\n{path.read_text()}", file=sys.stderr)
        for path in (home / ".transist-staging").glob("invoke-*/active_invocations/*.json"):
            try:
                print(f"Active invocation {path}:\n{path.read_text()}", file=sys.stderr)
            except OSError:
                pass
        raise
    finally:
        # Release fixture gates even on assertion failure, then await the detached
        # runner before the shell removes its home. Never edit runtime state.
        (workspace / "main-release").touch()
        (workspace / "cleanup-release").touch()
        deadline = time.monotonic() + 30
        pending = []
        for target, process in callers:
            try:
                process.communicate(timeout=max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pending.append((target, process))
        for _, process in pending:
            process.kill()
        unreaped = []
        for target, process in pending:
            try:
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                unreaped.append((target, process.pid))
        if root.exists():
            wait_for(f"{kind} runner shutdown", lambda: runner_finished(root))
        assert not unreaped, f"cancellation callers did not exit after kill: {unreaped}"


def main() -> None:
    """Execute direct and nested cases with sibling home/workspace directories."""
    binary, home_arg, root_arg = sys.argv[1:]
    home = pathlib.Path(home_arg).resolve()
    root = pathlib.Path(root_arg).resolve()
    assert not home.is_relative_to(root) and not root.is_relative_to(home)
    for nested in (False, True):
        run_case(binary, home, root / ("nested" if nested else "direct"), nested)


if __name__ == "__main__":
    main()
