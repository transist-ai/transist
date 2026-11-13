#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Run every shipped workflow to its declared terminal result.

This is the automatic execution coverage contract for everything shipped under
``<root>/workflows``. It:

1. Discovers every ``transist.flow.v1`` workflow file under ``<root>/workflows``,
    by schema detection: both ``.yaml`` and ``.yml`` files are
   scanned for the flow schema identifier in plain or quoted form, so a workflow
   cannot hide behind a different extension or quoting style.
2. Requires each discovered workflow to be declared in ``WORKFLOWS`` with a
   hermetic plan, an expected terminal status, and the fixture inputs / artifact
   checks the plan needs. A shipped workflow without coverage fails the gate, so
   a new workflow cannot ship silently.
3. For each declared workflow it creates an isolated Transist home and workspace,
    installs the bundled ``shell`` plugin from the same checkout/archive, installs
    the workflow, executes it to its declared terminal result, and verifies the
   declared artifacts/evidence. Every command runs from a consistent workspace
   directory so the runtime's persisted workspace capability projection matches.
   Each plan asserts the workflow's declared ``terminal`` status rather than a
   hardcoded expectation.

Only locally committed fixtures (the workflow/plugin sources and the bundled
``inputs.example.json`` files) and locally created temp homes/workspaces are
used: no sibling repository, network service, host file (e.g.
``/etc/gitconfig``), or mutable external fixture. Release-archive extraction is
validated member-by-member so no archive can write outside its extraction root.

Source mode:
    python3 scripts/ci/run_shipped_workflows.py \
        [--root DIR] [--transist-bin BIN]

Release-archive mode (runs the same inventory from an extracted release
archive, using ``bin/transist`` and the workflows/plugins shipped inside it):
    python3 scripts/ci/run_shipped_workflows.py \
        --archive transist-<tag>-<target>.tar.gz

Exit status: 0 if every shipped workflow reached its declared terminal result
with its artifact checks satisfied; 1 otherwise.
"""

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
from typing import Any

TERMINAL_STATUSES = {"success", "failed", "canceled"}

# Every shipped workflow, declared by path relative to the workflow root
# (the repository checkout in source mode, the extracted package root in
# archive mode). ``plan`` selects the hermetic execution plan; ``id_ref`` is the
# ``<id>@<version>`` workflow reference; ``inputs_file`` / ``inputs`` provide the
# hermetic fixture inputs; ``checks`` verify declared results/artifacts.
WORKFLOWS = [
    {
        "path": "workflows/demos/getting_started/01_hello_world/workflow.yaml",
        "id_ref": "demo_01_hello_world@0.1.0",
        "plan": "success",
        "terminal": "success",
        "reason": "smallest deterministic workflow",
    },
    {
        "path": "workflows/demos/getting_started/02_review_with_manual_gate/workflow.yaml",
        "id_ref": "demo_02_review_with_manual_gate@0.1.0",
        "plan": "success",
        "terminal": "success",
        "inputs_file": "workflows/demos/getting_started/02_review_with_manual_gate/inputs.example.json",
        "checks": [
            {
                "kind": "state_step_succeeded",
                "step": "draft_summary",
                "label": "review demo summary step succeeded",
            },
            {
                "kind": "workspace_contains",
                "path": "review-context.txt",
                "label": "review context file written to the workspace",
                "contains": "request=TASK-123",
            },
        ],
        "reason": "multi-step run with inputs, artifacts, and evidence",
    },
    {
        "path": "workflows/demos/getting_started/03_resume_after_failure/workflow.yaml",
        "id_ref": "demo_03_resume_after_failure@0.1.0",
        "plan": "resume",
        "terminal": "success",
        "checks": [
            {
                "kind": "state_step_executions_at_least",
                "step": "wait_for_fix",
                "minimum": 2,
                "label": "resume preserved prior execution history",
            },
        ],
        "reason": "failed-run recovery path through runs resume",
    },
    {
        "path": "workflows/demos/getting_started/04_replay/workflow.yaml",
        "id_ref": "demo_04_replay@0.1.0",
        "plan": "replay",
        "terminal": "success",
        "inputs": {"marker": "demo-v1-replay"},
        "checks": [
            {
                "kind": "workspace_line_count",
                "path": "replay-history.log",
                "expected": 2,
                "label": "replay appended the same marker as the source run",
            },
        ],
        "reason": "full-run replay with persisted-input reuse",
    },
    {
        "path": "workflows/demos/getting_started/05_artifacts_and_evidence/workflow.yaml",
        "id_ref": "demo_05_artifacts_and_evidence@0.1.0",
        "plan": "success",
        "terminal": "success",
        "checks": [
            {
                "kind": "captured_source_assets",
                "workflow_dir": "workflows/demos/getting_started/05_artifacts_and_evidence",
                "paths": [
                    "source/scripts/write_outputs.sh",
                    "source/scripts/read_outputs.sh",
                ],
                "label": "artifact demo captured both source scripts exactly",
            },
            {
                "kind": "state_step_stdout_contains",
                "step": "read_outputs",
                "needle": "artifact says: artifact body\n",
                "label": "artifact demo root reader printed the workspace artifact",
            },
            {
                "kind": "state_step_evidence_content",
                "step": "write_outputs",
                "path": "000_evidence.txt",
                "expected": "evidence body\n",
                "label": "writer evidence persisted in its execution directory",
            },
            {
                "kind": "state_step_evidence_content",
                "step": "read_outputs",
                "path": "000_read-confirmation.txt",
                "expected": "read_outputs ran ok\n",
                "label": "reader evidence persisted in its execution directory",
            },
            {
                "kind": "workspace_contains",
                "path": "artifact.txt",
                "label": "workspace artifact written by the first step",
                "contains": "artifact body",
            },
        ],
        "reason": "workspace artifacts vs run-scoped evidence",
    },
    {
        "path": "workflows/demos/getting_started/06_loop_until_condition/workflow.yaml",
        "id_ref": "demo_06_loop_until_condition@0.1.0",
        "plan": "success",
        "terminal": "success",
        "checks": [
            {
                "kind": "workspace_content",
                "path": ".loop-counter",
                "expected": "3",
                "label": "loop body ran exactly three times",
            },
        ],
        "reason": "control.loop with body/until/max_iters",
    },
    {
        "path": "workflows/demos/getting_started/07_subflow_basics/workflow.yaml",
        "id_ref": "demo_07_subflow_basics@0.1.0",
        "plan": "success",
        "terminal": "success",
        "inputs_file": "workflows/demos/getting_started/07_subflow_basics/inputs.example.json",
        "checks": [
            {
                "kind": "captured_source_assets",
                "workflow_dir": "workflows/demos/getting_started/07_subflow_basics",
                "paths": ["source/scripts/read_greeting.sh"],
                "label": "subflow demo captured the root reader exactly",
            },
            {
                "kind": "captured_source_assets",
                "workflow_dir": "workflows/demos/getting_started/07_subflow_basics/modules",
                "child_step": "greet_via_subflow",
                "paths": ["source/scripts/write_greeting.sh"],
                "label": "subflow demo captured the module writer exactly",
            },
            {
                "kind": "state_step_stdout_contains",
                "step": "read_subflow_artifact",
                "needle": "root sees: hello, transist\n",
                "label": "root reader printed the module greeting",
            },
            {
                "kind": "workspace_content",
                "path": "greeting.txt",
                "expected": "hello, transist\n",
                "label": "root step read the subflow-produced greeting",
            },
        ],
        "reason": "subflow invocation with input passing and shared workspace artifacts",
    },
    {
        "path": "workflows/demos/getting_started/08_manual_gate/workflow.yaml",
        "id_ref": "demo_08_manual_gate@0.1.0",
        "plan": "gate",
        "terminal": "success",
        "checks": [
            {
                "kind": "state_step_succeeded",
                "step": "post_gate",
                "label": "run continued to the post-gate step after approval",
            },
        ],
        "reason": "manual gate approval and continuation",
    },
    {
        "path": "workflows/demos/getting_started/09_cleanup_branches/workflow.yaml",
        "id_ref": "demo_09_cleanup_branches@0.1.0",
        "plan": "cleanup",
        "terminal": "success",
        "reason": "success, canceled, and failed cleanup branches",
    },
    {
        "path": "workflows/demos/getting_started/10_secret_env/workflow.yaml",
        "id_ref": "demo_10_secret_env@0.1.0",
        "plan": "secret",
        "terminal": "success",
        "checks": [
            {
                "kind": "state_step_stdout_contains",
                "step": "receive_secret",
                "needle": "received secret of length",
                "label": "secret delivered into the step without leaking the value",
            },
        ],
        "reason": "end-to-end secret delivery into a shell step",
    },
    {
        "path": "workflows/demos/getting_started/11_filesystem_policy/workflow.yaml",
        "id_ref": "demo_11_filesystem_policy@0.1.0",
        "plan": "filesystem_policy",
        "terminal": "success",
        "inputs": {"repo_path": "{workspace}"},
        "checks": [
            {
                "kind": "state_step_succeeded",
                "step": "verify_workspace_intact",
                "label": "filesystem policy probes completed",
            },
        ],
        "reason": "workspace-scoped filesystem policy and external path grants",
    },
    {
        "path": "workflows/inspection_e2e/failure_demo/workflow.yaml",
        "id_ref": "failure_demo@0.1.0",
        "plan": "expect_failed",
        "terminal": "failed",
        "source_only": True,
        "reason": "inspection demo built to fail deterministically; tested from the source tree only, never packaged",
    },
]


class WorkflowError(Exception):
    """Raised when a shipped workflow does not reach its declared result."""


def fail(label, detail):
    print(f"FAIL: {label}{(' | ' + detail) if detail else ''}", flush=True)
    raise WorkflowError(label)


def ok(label):
    print(f"OK: {label}", flush=True)


def assert_rejected_resume_state_unchanged(
    before: dict[str, Any], after: dict[str, Any]
) -> None:
    """Assert refused recovery preserves state, allowing only trusted audit times.

    Inputs are decoded persisted state dictionaries and are never mutated. The
    before-state's ``trusted_execution_mode is True`` selects trusted/governed
    re-entry; untrusted standalone requires complete equality. Trusted re-entry
    permits only ``trust_decision.decided_at_ms`` and top-level ``updated_at_ms``
    to advance (or remain equal), each a required u64 Unix-millisecond integer.
    Every other field, including trust authority and execution/cleanup counters,
    must be identical. Raises WorkflowError on malformed times or any other drift;
    returns None on success. This checks state, not external history files.
    """
    expected = dict(before)
    if before.get("trusted_execution_mode") is True:
        old_trust = before.get("trust_decision")
        new_trust = after.get("trust_decision")
        if not isinstance(old_trust, dict) or not isinstance(new_trust, dict):
            raise WorkflowError(
                "rejected resume has missing or malformed trust decision"
            )
        for label, old, new in (
            ("updated_at_ms", before.get("updated_at_ms"), after.get("updated_at_ms")),
            (
                "trust_decision.decided_at_ms",
                old_trust.get("decided_at_ms"),
                new_trust.get("decided_at_ms"),
            ),
        ):
            if any(
                type(value) is not int or not 0 <= value < 2**64 for value in (old, new)
            ):
                raise WorkflowError(f"rejected resume has invalid {label}")
            if new < old:
                raise WorkflowError(f"rejected resume regressed {label}")
        expected["updated_at_ms"] = after["updated_at_ms"]
        expected["trust_decision"] = {
            **old_trust,
            "decided_at_ms": new_trust["decided_at_ms"],
        }
    # JSON comparison retains type distinctions that Python equality erases
    # (for example True == 1), without removing fields or mutating either input.
    if json.dumps(expected, sort_keys=True) != json.dumps(after, sort_keys=True):
        raise WorkflowError("rejected resume mutated non-audit run state")


def recovery_history_snapshot(run_root: pathlib.Path) -> dict[str, bytes]:
    """Read run-relative file bytes, excluding projections and runner logging.

    Used around refused recovery after runner finalization. Includes events,
    immutable execution records, artifacts/evidence and recipe; reads only.
    Top-level state.json is checked separately; summary.json is its derived cache
    (including updated_at_ms), not history. Neither exclusion applies to children.
    Runner start/finish logging is not execution history and can change on refusal.
    """
    return {
        path.relative_to(run_root).as_posix(): path.read_bytes()
        for path in run_root.rglob("*")
        if path.is_file()
        and path != run_root / "state.json"
        and path != run_root / "summary.json"
        and path != run_root / "runner.log"
    }


def session_parent() -> pathlib.Path:
    """Create and return source-side generated scratch, never archive payload."""
    parent = pathlib.Path(__file__).resolve().parents[2] / ".tmp" / "shipped-workflows"
    parent.mkdir(parents=True, exist_ok=True)
    return parent


class Session:
    """One isolated home + workspace used to execute a single workflow plan."""

    def __init__(self, root, transist_bin, ci):
        self.root = pathlib.Path(root)
        self.transist_bin = pathlib.Path(transist_bin)
        self.ci = ci
        # Keep generated state outside the immutable release payload. Exact
        # user-data grants may live below /tmp, but never cover its private roots.
        self.temp_dir = pathlib.Path(
            tempfile.mkdtemp(prefix="transist-shipped-wf-", dir=session_parent())
        ).resolve()
        self.home = self.temp_dir / "home"
        self.fake_home = self.temp_dir / "fake-home"
        self.workspace = self.temp_dir / "workspace"
        self.fake_home.mkdir(parents=True)
        self.workspace.mkdir(parents=True)

    def close(self):
        shutil.rmtree(self.temp_dir, ignore_errors=True)

    def abs_path(self, rel):
        return self.root / rel

    def state_path(self, run_id):
        return self.home / "runs" / f"run_{run_id}" / "state.json"

    def state(self, run_id):
        return json.loads(self.state_path(run_id).read_text(encoding="utf-8"))

    def env(self, extra=None):
        env = dict(os.environ)
        env["HOME"] = str(self.fake_home)
        env.pop("TRANSIST_HOME", None)
        if self.ci:
            env["TRANSIST_E2E_CI"] = "1"
        if extra:
            env.update(extra)
        return env

    def cli(self, args, run_id=None, cwd=None, env=None, must=True):
        cmd = [str(self.transist_bin), "--home", str(self.home)] + args
        result = subprocess.run(
            cmd,
            cwd=str(cwd if cwd is not None else self.workspace),
            env=self.env(extra=env),
            capture_output=True,
            text=True,
        )
        if must and result.returncode != 0:
            detail = (result.stdout + result.stderr).strip().splitlines()
            raise WorkflowError(
                f"command failed ({result.returncode}): {' '.join(cmd)}"
                + (f": {'; '.join(detail[-4:])}" if detail else "")
            )
        return result

    def wait_status(self, run_id, predicate, timeout=240, poll=0.5):
        path = self.state_path(run_id)
        start = time.time()
        last = None
        while time.time() - start < timeout:
            try:
                last = json.loads(path.read_text(encoding="utf-8"))["status"]
            except (OSError, ValueError):
                last = None
            if last is not None and predicate(last):
                return last
            if last is None:
                time.sleep(poll)
                continue
            time.sleep(poll)
        return last

    def wait_terminal(self, run_id, expected, timeout=240):
        status = self.wait_status(
            run_id, lambda st: st in TERMINAL_STATUSES, timeout=timeout
        )
        if status is None:
            fail(f"run {run_id}", f"never reached a terminal status within {timeout}s")
        if status != expected:
            fail(
                f"run {run_id}",
                f"expected terminal status {expected!r}, got {status!r}",
            )
        return status

    def wait_runner_finished(self, run_id: str, timeout: float = 30) -> None:
        """Wait for this run's latest runner completion before another mutation.

        Terminal state precedes lease finalization. Reads trusted runner events
        and sleeps until completion; never retries or bypasses a runtime command.
        """
        path = self.home / "runs" / f"run_{run_id}" / "runner.log"
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                lines = path.read_text(encoding="utf-8").splitlines()
            except OSError:
                lines = []
            for line in reversed(lines):
                try:
                    event = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(event, dict):
                    continue
                if (
                    event.get("schema") == "transist.runner_event.v1"
                    and event.get("run_id") == run_id
                    and event.get("event") in ("started", "finished")
                ):
                    if event["event"] == "finished":
                        return
                    break
            time.sleep(0.05)
        raise WorkflowError(f"runner for {run_id!r} did not finish within {timeout}s")

    def workflow_input_args(self, wf):
        """Return the prospective input arguments shared by check and run."""
        args = []
        if wf.get("inputs_file"):
            args += ["--inputs-file", str(self.abs_path(wf["inputs_file"]))]
        for key, value in (wf.get("inputs") or {}).items():
            if value == "{workspace}":
                value = str(self.workspace)
            args += ["--input", f"{key}={value}"]
        return args

    def check(self, wf, extra_env=None):
        """Verify one installed workflow with the exact inputs used for its run."""
        args = ["workflows", "check", wf["id_ref"], "--format", "json"]
        args += self.workflow_input_args(wf)
        self.cli(args, cwd=self.workspace, env=extra_env)

    def run(self, run_id, wf, extra_env=None):
        args = ["workflows", "run", wf["id_ref"], "--run-ref", run_id]
        args += self.workflow_input_args(wf)
        self.cli(args, cwd=self.workspace, env=extra_env)

    def install(self, wf):
        """Install workflow code without binding prospective run inputs."""
        if wf.get("plan") == "filesystem_policy":
            self.fake_home.joinpath(".gitconfig").write_text("", encoding="utf-8")
            self.fake_home.joinpath(".ssh").mkdir(parents=True, exist_ok=True)
            shutil.copyfile(
                self.abs_path(
                    "workflows/demos/getting_started/11_filesystem_policy/settings.example.yaml"
                ),
                self.home / "settings.yaml",
            )
        args = ["workflows", "install", "--path", str(self.abs_path(wf["path"]))]
        args.append("--approve")
        self.cli(args, cwd=self.workspace)

    def install_shell_plugin(self):
        self.cli(
            ["plugins", "install", "--path", str(self.abs_path("plugins/shell/plugin.yaml"))],
            cwd=self.workspace,
        )

    def init_home(self):
        self.cli(["home", "init", "--mode", "standalone"], cwd=self.workspace)

    def run_checks(self, wf):
        if not wf.get("checks"):
            return
        for check in wf["checks"]:
            self._run_check(check)

    def _run_check(self, check):
        kind = check["kind"]
        if kind == "captured_source_assets":
            self._check_captured_source_assets(check)
        elif kind == "state_step_evidence_content":
            self._check_step_evidence_content(check)
        elif kind == "state_step_succeeded":
            state = self.state(check["run_id"])
            step = state["steps"].get(check["step"]) or {}
            if step.get("status") != "succeeded":
                fail(check["label"], f"step {check['step']} status={step.get('status')!r}")
            ok(check["label"])
        elif kind == "state_step_executions_at_least":
            state = self.state(check["run_id"])
            step = state["steps"].get(check["step"]) or {}
            if int(step.get("executions", 0)) < int(check["minimum"]):
                fail(check["label"], f"executions={step.get('executions')}")
            ok(check["label"])
        elif kind == "state_step_stdout_contains":
            state = self.state(check["run_id"])
            step = state["steps"].get(check["step"]) or {}
            stdout = (step.get("last_result") or {}).get("output", {}).get("stdout", "")
            if check["needle"] not in stdout:
                fail(check["label"], f"stdout missing {check['needle']!r}")
            ok(check["label"])
        elif kind == "workspace_contains":
            path = self.workspace / check["path"]
            if not path.is_file():
                fail(check["label"], f"missing workspace file {check['path']}")
            if "contains" in check and check["contains"] not in path.read_text(
                encoding="utf-8", errors="replace"
            ):
                fail(check["label"], f"workspace file {check['path']} missing expected content")
            ok(check["label"])
        elif kind == "workspace_content":
            path = self.workspace / check["path"]
            if not path.is_file():
                fail(check["label"], f"missing workspace file {check['path']}")
            actual = path.read_text(encoding="utf-8", errors="replace")
            if actual != check["expected"]:
                fail(
                    check["label"],
                    f"workspace file {check['path']} content={actual!r}, expected {check['expected']!r}",
                )
            ok(check["label"])
        elif kind == "workspace_line_count":
            path = self.workspace / check["path"]
            if not path.is_file():
                fail(check["label"], f"missing workspace file {check['path']}")
            count = len(path.read_text(encoding="utf-8", errors="replace").splitlines())
            if count != int(check["expected"]):
                fail(check["label"], f"line count={count}, expected {check['expected']}")
            ok(check["label"])
        else:
            fail("coverage plan", f"unknown check kind {kind!r}")

    def _check_captured_source_assets(self, check: dict) -> None:
        """Compare declared source paths, manifest digests and blobs to shipped bytes."""
        run_root = self.state_path(check["run_id"]).parent
        if check.get("child_step"):
            step = self.state(check["run_id"])["steps"][check["child_step"]]
            child_id = step["last_result"]["output"]["child_run_id"]
            run_root = run_root / "contained_runs" / f"run_{child_id}"
        assets = run_root / "workflow_assets"
        manifest = json.loads((assets / "manifest.json").read_text(encoding="utf-8"))
        expected = {
            path: hashlib.sha256(
                (
                    self.root / check["workflow_dir"] / path.removeprefix("source/")
                ).read_bytes()
            ).hexdigest()
            for path in check["paths"]
        }
        entries = [
            entry
            for entry in manifest["entries"]
            if entry["path"].startswith("source/")
        ]
        actual = {entry["path"]: entry["sha256"] for entry in entries}
        if actual != expected or len(entries) != len(expected):
            fail(check["label"], f"source captures={actual!r}, expected {expected!r}")
        for path, digest in expected.items():
            source = self.root / check["workflow_dir"] / path.removeprefix("source/")
            if (assets / "blobs" / digest).read_bytes() != source.read_bytes():
                fail(
                    check["label"], f"captured blob differs from shipped bytes: {path}"
                )
        ok(check["label"])

    def _check_step_evidence_content(self, check: dict) -> None:
        """Read expected evidence bytes through the persisted step request paths."""
        run_root = self.state_path(check["run_id"]).parent
        step = self.state(check["run_id"])["steps"][check["step"]]
        directory = pathlib.Path(step["last_request"]["paths"]["evidence_dir"])
        expected_dir = (
            run_root / "artifacts" / check["step"] / "execution_001" / "evidence"
        )
        if directory != expected_dir:
            fail(
                check["label"],
                f"evidence directory={directory}, expected {expected_dir}",
            )
        path = directory / check["path"]
        body = check["expected"].encode("utf-8")
        refs = [
            ref for ref in step["last_result"]["evidence"] if ref["path"] == str(path)
        ]
        if (
            len(refs) != 1
            or refs[0]["sha256"] != hashlib.sha256(body).hexdigest()
            or refs[0]["bytes"] != len(body)
            or not path.is_file()
            or path.read_bytes() != body
        ):
            fail(check["label"], f"missing or incorrect evidence: {path}")
        ok(check["label"])


def sandbox_backend_available():
    if sys.platform == "darwin":
        return pathlib.Path("/usr/bin/sandbox-exec").is_file()
    return shutil.which("bwrap") is not None


def run_plan_success(session, wf):
    run_id = "run"
    session.run(run_id, wf)
    session.wait_terminal(run_id, wf["terminal"])
    for check in wf.get("checks") or []:
        check["run_id"] = run_id
    session.run_checks(wf)
    ok(f"{wf['id_ref']} reached {wf['terminal']}")


def run_plan_resume(session, wf):
    run_id = "resume-run"
    session.run(run_id, wf)
    failed = session.wait_status(run_id, lambda st: st == "failed", timeout=120)
    if failed != "failed":
        raise WorkflowError(f"resume source run did not fail as designed (status={failed!r})")
    session.wait_runner_finished(run_id)
    (session.workspace / ".retry-ok").touch()
    session.cli(["runs", "resume", run_id], cwd=session.workspace)
    session.wait_terminal(run_id, wf["terminal"])
    for check in wf.get("checks") or []:
        check["run_id"] = run_id
    session.run_checks(wf)
    ok("resume demo reached success after runs resume")


def run_plan_replay(session, wf):
    source_run = "replay-source"
    target_run = "replay-target"
    session.run(source_run, wf)
    session.wait_terminal(source_run, "success")
    session.wait_runner_finished(source_run)
    session.cli(["runs", "replay", source_run, "--run-ref", target_run], cwd=session.workspace)
    session.wait_terminal(target_run, wf["terminal"])
    for check in wf.get("checks") or []:
        check["run_id"] = target_run
    session.run_checks(wf)
    ok("replay demo replayed the source run to success")


def run_plan_gate(session, wf):
    run_id = "gate-run"
    session.run(run_id, wf)
    blocked = session.wait_status(run_id, lambda st: st == "blocked", timeout=120)
    if blocked != "blocked":
        raise WorkflowError(f"gate run did not block as designed (status={blocked!r})")
    approval_args = ["runs", "gate", "approve", run_id, "--reason", "covered-approval"]
    approval = None
    for _ in range(20):
        approval = session.cli(
            approval_args,
            cwd=session.workspace,
            env={"TRANSIST_ACTOR": "shipped-workflows@test.local"},
            must=False,
        )
        if approval.returncode == 0:
            break
        time.sleep(0.25)
    if approval is None or approval.returncode != 0:
        session.cli(
            approval_args,
            cwd=session.workspace,
            env={"TRANSIST_ACTOR": "shipped-workflows@test.local"},
        )
    session.wait_terminal(run_id, wf["terminal"])
    for check in wf.get("checks") or []:
        check["run_id"] = run_id
    session.run_checks(wf)
    ok("manual gate demo reached success after approval")


def run_plan_cleanup(session, wf):
    # Separate workspaces prevent earlier markers from satisfying later checks.
    workspace = session.workspace
    try:
        for outcome, inputs in (
            (wf["terminal"], {"fail": "false", "slow": "false"}),
            ("failed", {"fail": "true", "slow": "false"}),
            ("canceled", {"fail": "false", "slow": "true"}),
        ):
            run_id = f"cleanup-{outcome}"
            session.workspace = workspace / outcome
            session.workspace.mkdir()
            session.run(run_id, {**wf, "inputs": inputs})
            if outcome == "canceled":
                _wait_for_cleanup_demo_invocation(session, run_id)
                report = json.loads(session.cli(["runs", "cancel", run_id]).stdout)
                if not (
                    report.get("run_id") == run_id
                    and report.get("root_run_id") == run_id
                    and report.get("cancellation_requested") is True
                    and report.get("settled") is True
                    and report.get("final_status") == "canceled"
                    and report.get("cleanup_state") == "succeeded"
                    and report.get("error") is None
                    and report.get("active_execution_cancellation_error") is None
                ):
                    raise WorkflowError(f"cleanup cancel did not settle: {report!r}")
            session.wait_terminal(run_id, outcome, timeout=120)
            _wait_for_cleanup(session, run_id, outcome)
            session.wait_runner_finished(run_id)
            state = session.state(run_id)
            step = state["cleanup"].get("steps", {}).get(f"on_{outcome}", {})
            if step.get("status") != "succeeded" or step.get("executions") != 1:
                raise WorkflowError(f"{run_id}: expected one successful cleanup: {step!r}")
            marker = session.workspace / f"cleanup-{outcome}.txt"
            if not marker.is_file() or marker.read_text(encoding="utf-8") != (
                f"cleanup-{outcome} ran\n"
            ):
                raise WorkflowError(f"{run_id}: missing or incorrect automatic cleanup marker")
            for other in TERMINAL_STATUSES - {outcome}:
                if (session.workspace / f"cleanup-{other}.txt").exists():
                    raise WorkflowError(f"{run_id}: nonmatching {other} cleanup ran")
            if outcome == "failed":
                if state.get("recovery_state") != "cleanup_completed":
                    raise WorkflowError("automatic failed cleanup did not complete recovery")
                run_root = session.state_path(run_id).parent
                history = recovery_history_snapshot(run_root)
                resume = session.cli(["runs", "resume", run_id], must=False)
                if resume.returncode == 0 or "cleanup has begun" not in (
                    resume.stdout + resume.stderr
                ).lower():
                    raise WorkflowError("resume was not rejected after automatic failed teardown")
                assert_rejected_resume_state_unchanged(state, session.state(run_id))
                if recovery_history_snapshot(run_root) != history:
                    raise WorkflowError("rejected resume mutated execution/history evidence")
            ok(f"cleanup demo {outcome} branch automatically settled cleanup and marker")
    finally:
        session.workspace = workspace
    ok("cleanup demo all three branches reached their declared results")


def run_plan_secret(session, wf):
    run_id = "secret-run"
    session.run(run_id, wf, extra_env={"MY_API_TOKEN": "local-shipped-workflow-secret"})
    session.wait_terminal(run_id, wf["terminal"])
    for check in wf.get("checks") or []:
        check["run_id"] = run_id
    session.run_checks(wf)
    ok("secret env demo delivered the secret and succeeded")


def run_plan_filesystem_policy(session, wf):
    if not sandbox_backend_available():
        raise WorkflowError(
            "filesystem-policy demo requires a sandbox backend (bwrap on Linux or "
            "sandbox-exec on macOS) to prove write/read enforcement; none is available"
        )
    # Hermetic fixtures: the global read grant and the step-level ~/.ssh grant
    # must resolve to real paths at run birth, so materialize them under the
    # isolated fake home before running. Both are committed fixtures, so the
    # probe never depends on a host file such as /etc/gitconfig.
    (session.fake_home / ".gitconfig").write_text("", encoding="utf-8")
    (session.fake_home / ".ssh").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(
        session.abs_path(
            "workflows/demos/getting_started/11_filesystem_policy/settings.example.yaml"
        ),
        session.home / "settings.yaml",
    )
    run_id = "fs-policy-run"
    run_wf = {
        **wf,
        "inputs": {**(wf.get("inputs") or {}), "repo_path": str(session.workspace)},
    }
    session.run(run_id, run_wf)
    session.wait_terminal(run_id, wf["terminal"], timeout=240)
    for check in wf.get("checks") or []:
        check["run_id"] = run_id
    session.run_checks(wf)
    ok("filesystem policy demo verified sandboxed read/write enforcement")


def run_plan_subflow(session, wf):
    session.run("subflow-run", wf)
    session.wait_terminal("subflow-run", wf["terminal"])
    for check in wf.get("checks") or []:
        check["run_id"] = "subflow-run"
    session.run_checks(wf)
    ok("shipped subflow executed standalone")


def run_plan_expect_failed(session, wf):
    run_id = "failure-demo"
    session.run(run_id, wf)
    session.wait_terminal(run_id, "failed")
    ok("failure demo reached its declared terminal status 'failed'")


def _wait_for_cleanup(
    session: Session, run_id: str, outcome: str, timeout: float = 60
) -> dict:
    """Read persisted automatic cleanup until success; never dispatch recovery."""
    deadline = time.monotonic() + timeout
    state = {}
    while time.monotonic() < deadline:
        state = session.state(run_id)
        cleanup = state.get("cleanup") or {}
        if (
            state.get("status") == outcome
            and cleanup.get("trigger") == outcome
            and cleanup.get("state") == "succeeded"
        ):
            return state
        if cleanup.get("state") == "failed":
            raise WorkflowError(f"{run_id}: automatic cleanup failed: {cleanup!r}")
        time.sleep(0.1)
    raise WorkflowError(f"{run_id}: automatic cleanup never succeeded: {state!r}")


def _wait_for_cleanup_demo_invocation(
    session: Session, run_id: str, timeout: float = 30
) -> None:
    """Wait for the demo's persisted process invocation before requesting cancel."""
    records = session.state_path(run_id).parent / "active_invocations"
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        state = session.state(run_id)
        request = state.get("steps", {}).get("maybe_fail_or_slow", {}).get("last_request") or {}
        paths = list(records.glob("*.json"))
        paths += list(
            (session.home / ".transist-staging").glob("invoke-*/active_invocations/*.json")
        )
        for path in paths:
            try:
                record = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue
            if (
                record.get("run_id") == run_id
                and record.get("step_id") == "maybe_fail_or_slow"
                and record.get("adapter_kind") == "process"
                and record.get("invoke_id") == request.get("invoke_id")
            ):
                return
        time.sleep(0.05)
    raise WorkflowError(f"{run_id}: slow step never published its active invocation")


PLANS = {
    "success": run_plan_success,
    "resume": run_plan_resume,
    "replay": run_plan_replay,
    "gate": run_plan_gate,
    "cleanup": run_plan_cleanup,
    "secret": run_plan_secret,
    "filesystem_policy": run_plan_filesystem_policy,
    "subflow": run_plan_subflow,
    "expect_failed": run_plan_expect_failed,
}


def discover_workflows(root):
    """Return {relpath: pathlib.Path} for every transist.flow.v1 file under workflows/.

    Detection is compile-based: it keys on the same top-level ``schema``
    identifier the compiler reads (``transist.flow.v1``), so discovery matches
    what the runtime will actually accept. Every ``.yaml`` and ``.yml`` file is
    scanned for that identifier in plain, single-quoted, or double-quoted form,
    so a workflow cannot hide behind a different extension or quoting style.
    """
    discovered = {}
    workflows_dir = pathlib.Path(root) / "workflows"
    schema_re = re.compile(
        r"^\s*schema:\s*['\"]?transist\.flow\.v1['\"]?\s*$", re.MULTILINE
    )
    for path in sorted(workflows_dir.rglob("*")):
        if not path.is_file() or path.suffix not in (".yaml", ".yml"):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if schema_re.search(text):
            discovered[path.relative_to(root).as_posix()] = path
    return discovered


def safe_extract(tar, dest):
    """Extract an archive safely, rejecting traversal and external members.

    Before writing anything, every member is validated so nothing resolves
    outside ``dest``: absolute paths, NUL bytes, parent-relative (``..``)
    components, and symlink/hardlink targets that escape ``dest`` are rejected.
    Extraction only proceeds once the whole member list is validated.
    """
    dest = pathlib.Path(dest).resolve()
    for member in tar.getmembers():
        name = member.name
        if name.startswith("/") or "\x00" in name or "\x00" in member.linkname:
            raise WorkflowError(f"archive member has unsafe path: {name!r}")
        target = (dest / name).resolve()
        if not target.is_relative_to(dest):
            raise WorkflowError(f"archive member escapes extraction root: {name!r}")
        if member.islnk() or member.issym():
            link_dest = (dest / name).parent / member.linkname
            if not link_dest.resolve().is_relative_to(dest):
                raise WorkflowError(f"archive link escapes extraction root: {name!r}")
    tar.extractall(dest)


def check_coverage(root, discovered, archive=False):
    """Fail the gate when a shipped workflow lacks coverage or coverage is stale.

    ``archive`` limits the required inventory to workflows that are packaged in
    a release archive. Source-only workflows (``source_only: True``) are still
    covered from the repository checkout but are not required inside an
    archive, because ``workflows/inspection_e2e`` is excluded from packaged
    content.
    """
    known = {entry["path"] for entry in WORKFLOWS}
    problems = []
    for rel in sorted(discovered):
        if rel not in known:
            problems.append(
                f"shipped workflow {rel} has no hermetic coverage; add a plan to "
                f"scripts/ci/run_shipped_workflows.py WORKFLOWS"
            )
    for entry in WORKFLOWS:
        rel = entry["path"]
        if rel not in discovered:
            if archive and entry.get("source_only"):
                continue
            problems.append(f"coverage plan references missing shipped workflow {rel}")
            continue
        inputs_file = entry.get("inputs_file")
        if inputs_file and not (pathlib.Path(root) / inputs_file).is_file():
            problems.append(
                f"coverage plan for {rel} references missing fixture {inputs_file}"
            )
        plan = entry.get("plan")
        if plan not in PLANS:
            problems.append(f"coverage plan for {rel} references unknown plan {plan!r}")
        terminal = entry.get("terminal")
        if terminal not in TERMINAL_STATUSES:
            problems.append(
                f"coverage plan for {rel} declares invalid terminal {terminal!r}"
            )
    for problem in problems:
        print(f"FAIL: workflow coverage: {problem}", flush=True)
    if problems:
        raise WorkflowError("workflow coverage gate failed")
    print(f"OK: all {len(discovered)} shipped workflows have hermetic coverage", flush=True)


def run_workflow(session, entry):
    session.init_home()
    session.install_shell_plugin()
    session.install(entry)
    secret_env = (
        {"MY_API_TOKEN": "local-shipped-workflow-secret"}
        if entry["plan"] == "secret"
        else None
    )
    if entry["plan"] == "secret":
        # Validation and execution must evaluate the same delivery policy.
        (pathlib.Path(session.home) / "config.yaml").write_text(
            "version: v1\nruntime:\n  mode: standalone\npolicy:\n  profile: permissive\n"
            "  remote_secret_delivery:\n    process: true\n    http: false\n    mcp: false\n",
            encoding="utf-8",
        )
    session.check(entry, extra_env=secret_env)
    plan = PLANS[entry["plan"]]
    plan(session, entry)


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Execute every shipped workflow to its declared terminal result."
    )
    parser.add_argument(
        "--root",
        default=None,
        help="Workflow root (repository or extracted archive). Defaults to the repo root.",
    )
    parser.add_argument(
        "--transist-bin",
        default=None,
        help="Path to the transist binary. Defaults to target/debug then target/release.",
    )
    parser.add_argument(
        "--archive",
        default=None,
        help="Release archive to extract and run the same inventory from.",
    )
    parser.add_argument(
        "--no-ci",
        action="store_true",
        help="Do not set TRANSIST_E2E_CI for child commands.",
    )
    args = parser.parse_args(argv)

    script_dir = pathlib.Path(__file__).resolve().parent.parent.parent

    root = None
    archive_tmp = None
    archive_root = None
    if args.archive:
        if not pathlib.Path(args.archive).is_file():
            print(f"FAIL: release archive not found: {args.archive}", file=sys.stderr)
            return 1
        archive_tmp = pathlib.Path(tempfile.mkdtemp(prefix="transist-wf-archive-"))
        try:
            with tarfile.open(args.archive, "r:gz") as tar:
                safe_extract(tar, archive_tmp)
        except Exception:
            shutil.rmtree(archive_tmp, ignore_errors=True)
            raise
        pkg_dirs = [p for p in archive_tmp.iterdir() if p.is_dir()]
        if len(pkg_dirs) != 1:
            print("FAIL: archive must contain exactly one top-level package directory", file=sys.stderr)
            shutil.rmtree(archive_tmp, ignore_errors=True)
            return 1
        archive_root = pkg_dirs[0]
        root = archive_root
        transist_bin = archive_root / "bin" / "transist"
    else:
        root = pathlib.Path(args.root) if args.root else script_dir
        if args.transist_bin:
            transist_bin = pathlib.Path(args.transist_bin).resolve()
        else:
            debug_bin = root / "target" / "debug" / "transist"
            release_bin = root / "target" / "release" / "transist"
            if debug_bin.is_file():
                transist_bin = debug_bin
            elif release_bin.is_file():
                transist_bin = release_bin
            else:
                print(
                    f"FAIL: no transist binary found under {root}/target; build it or pass --transist-bin",
                    file=sys.stderr,
                )
                return 1

    if not transist_bin.is_file() or not os.access(transist_bin, os.X_OK):
        print(f"FAIL: transist binary not executable: {transist_bin}", file=sys.stderr)
        shutil.rmtree(archive_tmp, ignore_errors=True) if archive_root else None
        return 1

    ci = not args.no_ci
    archive_mode = archive_root is not None

    try:
        discovered = discover_workflows(root)
        try:
            check_coverage(root, discovered, archive=archive_mode)
        except WorkflowError as error:
            print(f"FAIL: {error}", file=sys.stderr)
            return 1

        failures = []
        for entry in WORKFLOWS:
            if archive_mode and entry.get("source_only"):
                print(f"\n=== {entry['path']} ({entry['plan']}) — SKIP: source-only, not packaged ===", flush=True)
                continue
            label = f"{entry['path']} ({entry['plan']})"
            print(f"\n=== {label} ===", flush=True)
            session = Session(root, transist_bin, ci)
            try:
                run_workflow(session, entry)
            except WorkflowError as error:
                print(f"FAIL: {label}: {error}", flush=True)
                failures.append(label)
            except Exception as error:  # noqa: BLE001 - surface any plan defect
                print(f"FAIL: {label}: unexpected error: {error}", flush=True)
                failures.append(label)
            finally:
                session.close()

        print("")
        if failures:
            print(f"FAIL: {len(failures)} shipped workflow(s) did not reach their declared result:", file=sys.stderr)
            for label in failures:
                print(f"  - {label}", file=sys.stderr)
            return 1

        if archive_mode:
            print(f"PASS: all {len(WORKFLOWS)} declared workflows handled (source-only entries skipped in archive mode).")
        else:
            print(f"PASS: all {len(WORKFLOWS)} shipped workflows reached their declared terminal results.")
        return 0
    finally:
        if archive_root:
            shutil.rmtree(archive_tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
