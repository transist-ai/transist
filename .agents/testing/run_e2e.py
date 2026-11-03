#!/usr/bin/env python3
"""Run hermetic E2E test suites from committed run.sh scripts.

Each suite lives under ``.agents/testing/e2e_suites/<name>/run.sh`` and is a
self-contained executable bash script that resolves binaries from this
workspace (``target/debug/{transist,transist-server,transist-admin}``) and
inputs from repository paths.

Usage:
    python3 .agents/testing/run_e2e.py             # run all required suites
    python3 .agents/testing/run_e2e.py --ci        # CI mode: fail on skip/warn
    python3 .agents/testing/run_e2e.py --suite NAME # run a single suite
    python3 .agents/testing/run_e2e.py --json       # machine-readable output

Required production-feature suites (run in --ci):
    plugin-lifecycle, resume-replay, run-cancellation, manual-gates, secrets-and-policy,
    run-cleanup, snapshot-export, filesystem-policy, detached-assets, workflow-authorization,
    managed-documents
"""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
SUITES_DIR = REPO_ROOT / ".agents" / "testing" / "e2e_suites"

REQUIRED_SUITES = [
    "plugin-lifecycle",
    "resume-replay",
    "run-cancellation",
    "manual-gates",
    "secrets-and-policy",
    "run-cleanup",
    "snapshot-export",
    "filesystem-policy",
    "detached-assets",
    "workflow-authorization",
    "managed-documents",
]

DEFAULT_TIMEOUT_S = 600
LEAK_QUIESCENCE_S = 2.0
LEAK_POLL_S = 0.1


def discover_suites(ci_mode: bool) -> list[str]:
    """Return the list of suite names to run."""
    if ci_mode:
        return list(REQUIRED_SUITES)
    return sorted(
        d.name
        for d in SUITES_DIR.iterdir()
        if d.is_dir() and (d / "run.sh").exists()
    )


def _matching_leaked_processes(suite_name: str, suite_pid: int) -> list[int]:
    """Return detached processes whose command line still references a suite home."""
    home_patterns = {
        re.escape(f"/tmp/.transist-{suite_name}-"),
        re.escape(os.path.realpath(f"/tmp/.transist-{suite_name}-")),
    }
    leaked: list[int] = []
    try:
        for home_pattern in home_patterns:
            result = subprocess.run(
                ["pgrep", "-f", home_pattern],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if result.returncode == 0:
                for line in result.stdout.strip().split("\n"):
                    line = line.strip()
                    if line and line not in {str(os.getpid()), str(suite_pid)}:
                        pid = int(line)
                        if pid not in leaked:
                            leaked.append(pid)
    except (FileNotFoundError, subprocess.TimeoutExpired, ValueError):
        pass
    return leaked


def _detect_leaked_processes(
    suite_name: str,
    suite_pid: int,
    quiescence_s: float = LEAK_QUIESCENCE_S,
    poll_s: float = LEAK_POLL_S,
) -> list[int]:
    """Detect processes that escaped the suite's process group via setsid.

    Suites create a physical temporary ``TRANSIST_HOME`` whose template includes
    the suite name. After the suite exits, any matching process is a leak.
    """
    deadline = time.monotonic() + max(quiescence_s, 0)
    while True:
        matching = _matching_leaked_processes(suite_name, suite_pid)
        if not matching or time.monotonic() >= deadline:
            return matching
        time.sleep(min(max(poll_s, 0), max(0, deadline - time.monotonic())))


def _kill_leaked_processes(pids: list[int]) -> None:
    """Attempt graceful then forceful termination of leaked processes."""
    for pid in pids:
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.kill(pid, sig)
                time.sleep(1)
            except (ProcessLookupError, PermissionError):
                break


def run_suite(
    suite_name: str, ci_mode: bool, timeout_s: int = DEFAULT_TIMEOUT_S
) -> dict:
    """Run a single suite's run.sh in its own process group.

    Returns a result dict with keys:
        suite, status, detail, duration, output, ok_count, fail_count,
        warn_count, skip_count, leaked_pids
    """
    run_sh = SUITES_DIR / suite_name / "run.sh"
    if not run_sh.exists():
        return {
            "suite": suite_name,
            "status": "SKIP",
            "detail": f"no run.sh found at {run_sh}",
            "duration": 0,
            "output": "",
            "ok_count": 0,
            "fail_count": 0,
            "warn_count": 0,
            "skip_count": 0,
            "leaked_pids": [],
        }

    env = os.environ.copy()
    env["TRANSIST_E2E_CI"] = "1" if ci_mode else "0"
    env["REPO_ROOT"] = str(REPO_ROOT)

    start_time = time.time()
    leaked_pids: list[int] = []

    try:
        proc = subprocess.Popen(
            ["bash", str(run_sh)],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            cwd=str(REPO_ROOT),
            start_new_session=True,
        )
        try:
            output = proc.communicate(timeout=timeout_s)[0]
        except subprocess.TimeoutExpired:
            _kill_process_group(proc.pid)
            try:
                output = proc.communicate(timeout=10)[0]
            except Exception:
                output = ""
            duration = round(time.time() - start_time, 2)
            leaked_pids = _detect_leaked_processes(suite_name, proc.pid)
            if leaked_pids:
                _kill_leaked_processes(leaked_pids)
            return {
                "suite": suite_name,
                "status": "TIMEOUT",
                "detail": f"script timed out after {timeout_s}s",
                "duration": duration,
                "output": output,
                "ok_count": 0,
                "fail_count": 0,
                "warn_count": 0,
                "skip_count": 0,
                "leaked_pids": leaked_pids,
            }
    except Exception as e:
        return {
            "suite": suite_name,
            "status": "ERROR",
            "detail": str(e),
            "duration": round(time.time() - start_time, 2),
            "output": "",
            "ok_count": 0,
            "fail_count": 0,
            "warn_count": 0,
            "skip_count": 0,
            "leaked_pids": leaked_pids,
        }

    duration = round(time.time() - start_time, 2)

    # Detect processes that escaped the suite's process group (e.g.
    # detached Transist runners spawned via setsid). The suite PID is
    # available as proc.pid even after communicate() returns.
    leaked_pids = _detect_leaked_processes(suite_name, proc.pid)
    if leaked_pids:
        _kill_leaked_processes(leaked_pids)

    ok_count = sum(1 for l in output.split("\n") if l.startswith("OK:"))
    fail_count = sum(1 for l in output.split("\n") if l.startswith("FAIL:"))
    warn_count = sum(1 for l in output.split("\n") if l.startswith("WARN:"))
    skip_count = sum(1 for l in output.split("\n") if l.startswith("SKIP:"))

    pass_line = any(l.startswith("PASS:") for l in output.split("\n"))

    if fail_count > 0:
        status = "FAIL"
    elif skip_count > 0:
        status = "SKIP"
    elif warn_count > 0:
        status = "WARN"
    elif proc.returncode != 0 and not pass_line:
        status = "ERROR"
    elif proc.returncode == 0 and pass_line:
        status = "PASS"
    elif proc.returncode != 0:
        status = "FAIL"
    else:
        # Zero exit but no PASS: marker — the script did not explicitly
        # signal completion. Treat as ERROR so incomplete suites cannot pass.
        status = "ERROR"

    summary_lines = [
        l for l in output.split("\n")
        if l.startswith(("OK:", "FAIL:", "WARN:", "SKIP:", "PASS:"))
    ]
    detail = "; ".join(summary_lines[-15:]) if summary_lines else output.strip()[-300:]

    return {
        "suite": suite_name,
        "status": status,
        "detail": detail,
        "duration": duration,
        "output": output,
        "ok_count": ok_count,
        "fail_count": fail_count,
        "warn_count": warn_count,
        "skip_count": skip_count,
        "leaked_pids": leaked_pids,
    }


def _kill_process_group(pid: int) -> None:
    """Kill the entire process group for the given PID.

    Captures the PGID once before signaling so a leader exit between
    SIGTERM and SIGKILL does not prevent escalation.
    """
    try:
        pgid = os.getpgid(pid)
    except (ProcessLookupError, PermissionError):
        return
    try:
        os.killpg(pgid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return
    time.sleep(2)
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def main() -> int:
    parser = argparse.ArgumentParser(description="Run hermetic E2E test suites.")
    parser.add_argument(
        "--ci",
        action="store_true",
        help="CI mode: run only required suites; fail on SKIP, WARN, or TIMEOUT.",
    )
    parser.add_argument(
        "--suite",
        type=str,
        help="Run a single named suite (bypasses required-suites filter).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON results instead of a table.",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_TIMEOUT_S,
        help=f"Per-suite timeout in seconds (default: {DEFAULT_TIMEOUT_S}).",
    )
    args = parser.parse_args()

    # --ci implies machine-readable JSON output so CI systems can parse results.
    if args.ci:
        args.json = True

    if args.suite:
        suites = [args.suite]
    else:
        suites = discover_suites(args.ci)

    results = []
    for suite in suites:
        if not args.json:
            print(f"  [{suite}] ", end="", flush=True)
        result = run_suite(suite, args.ci, timeout_s=args.timeout)
        results.append(result)
        if not args.json:
            icons = {
                "PASS": "PASS",
                "FAIL": "FAIL",
                "SKIP": "SKIP",
                "WARN": "WARN",
                "TIMEOUT": "TIME",
                "ERROR": "ERR",
            }
            print(f"{icons.get(result['status'], '?'):>5} ({result['duration']}s)")
            if result["status"] not in ("PASS",):
                for line in result.get("output", "").split("\n")[:8]:
                    s = line.strip()
                    if s and not s.startswith("OK:"):
                        print(f"      {s[:120]}")

    # Compute summary counts independently of output mode so JSON and
    # table paths both have access.
    counts: dict[str, int] = {}
    for r in results:
        s = r["status"]
        counts[s] = counts.get(s, 0) + 1

    if args.json:
        print(json.dumps(results, indent=2))
    else:
        print(f"\n{'=' * 58}")
        print(f"  E2E Test Suite Results  ({time.strftime('%Y-%m-%d %H:%M:%S')})")
        print(f"{'=' * 58}")
        print(f"\n{'Suite':<35} {'Result':>8} {'Duration':>10}")
        print("-" * 55)
        for r in results:
            s = r["status"]
            print(f"{r['suite']:<35} {s:>8} {r['duration']:>8.1f}s")
        print("-" * 55)
        print(f"{'TOTAL':<35} {len(results):>8}")
        for k in ("PASS", "FAIL", "WARN", "TIMEOUT", "ERROR", "SKIP"):
            if counts.get(k, 0):
                print(f"{k:<35} {counts[k]:>8}")

    if args.ci:
        failing = [r for r in results if r["status"] != "PASS"]
        # Leaked processes are always a CI failure, even if the suite passed.
        leaking = [r for r in results if r.get("leaked_pids")]
        if failing or leaking:
            if not args.json:
                if failing:
                    print(f"\nCI mode: {len(failing)} suite(s) did not pass:", file=sys.stderr)
                    for r in failing:
                        print(f"  {r['suite']}: {r['status']}", file=sys.stderr)
                if leaking:
                    print(f"\nCI mode: {len(leaking)} suite(s) leaked processes:", file=sys.stderr)
                    for r in leaking:
                        print(f"  {r['suite']}: pids {r['leaked_pids']}", file=sys.stderr)
            return 1

    non_pass = counts.get("FAIL", 0) + counts.get("ERROR", 0) + counts.get("TIMEOUT", 0)
    return 1 if non_pass > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
