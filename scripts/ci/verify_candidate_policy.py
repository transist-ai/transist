#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Verify candidate environments and exact-SHA GitHub Actions workflow attempts."""

import json
import os
import re
import subprocess
import sys
from datetime import datetime


def api(path: str, paginate: bool = False) -> object:
    """Read a GitHub API endpoint, collecting every page when requested."""
    command = ["gh", "api", path]
    if paginate:
        command += ["--paginate", "--slurp"]
    return json.loads(subprocess.check_output(command, text=True))


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def verify_gates(sha: str, *, require_push: bool = False, trigger: tuple[str, int, int] | None = None) -> str:
    """Verify latest attempts and, optionally, latest push attempts for the source SHA."""
    repo = os.environ["GITHUB_REPOSITORY"]
    require(bool(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo)), "invalid repository")
    root = f"repos/{repo}"
    repository_id = api(root)["id"]
    for workflow, gate in (("main-ci.yml", "main-gate"), ("security.yml", "security-gate")):
        pages = api(f"{root}/actions/workflows/{workflow}/runs?head_sha={sha}&per_page=100", True)
        runs = [run for page in pages for run in page["workflow_runs"] if run["head_sha"] == sha]
        require(bool(runs), f"missing exact-source workflow {workflow}")
        attempts = []
        for listed in runs:
            number = listed["run_attempt"]
            require(type(number) is int and number > 0, "invalid workflow attempt")
            attempt = api(f"{root}/actions/runs/{listed['id']}/attempts/{number}")
            require(attempt["id"] == listed["id"] and attempt["run_attempt"] == number
                    and attempt["head_sha"] == sha, f"{workflow} attempt identity mismatch")
            started = datetime.strptime(attempt["run_started_at"], "%Y-%m-%dT%H:%M:%SZ")
            attempts.append((started, attempt))
        def latest_run(options: list) -> dict:
            require(bool(options), f"missing eligible {workflow} workflow attempt")
            latest = max(started for started, _ in options)
            newest = [run for started, run in options if started == latest]
            require(len(newest) == 1, f"ambiguous latest {workflow} attempt timestamp")
            return newest[0]

        latest = latest_run(attempts)
        selected = [latest]
        if require_push:
            push = latest_run([(started, run) for started, run in attempts if run["event"] == "push"])
            if trigger is not None and trigger[0] == workflow:
                require((push["id"], push["run_attempt"]) == trigger[1:], f"stale {workflow} completion")
            if push is not latest:
                selected.append(push)
        for run in selected:
            require(run["path"] == f".github/workflows/{workflow}" and run["head_branch"] == "main"
                    and run["event"] in (("push", "workflow_dispatch") if workflow == "main-ci.yml" else ("push", "schedule", "workflow_dispatch"))
                    and run["repository"]["id"] == repository_id
                    and run["head_repository"]["id"] == repository_id, f"{workflow} source identity mismatch")
            require(run["status"] == "completed" and run["conclusion"] == "success", f"latest {workflow} run did not succeed")
            attempt = run["run_attempt"]
            require(type(attempt) is int and attempt > 0, "invalid workflow attempt")
            pages = api(f"{root}/actions/runs/{run['id']}/attempts/{attempt}/jobs?per_page=100", True)
            jobs = [job for page in pages for job in page["jobs"] if job["name"] == gate]
            require(len(jobs) == 1, f"missing or duplicate {gate} job")
            job = jobs[0]
            require(job["status"] == "completed" and job["conclusion"] == "success", f"{gate} did not succeed")
            match = re.fullmatch(r"https://api\.github\.com/repos/" + re.escape(repo) + r"/check-runs/([0-9]+)", job["check_run_url"])
            require(match is not None and job["run_id"] == run["id"], f"{gate} job identity mismatch")
            check_id = int(match.group(1))
            check = api(f"{root}/check-runs/{check_id}")
            require(check["id"] == check_id and check["name"] == gate and check["head_sha"] == sha
                    and check["status"] == "completed" and check["conclusion"] == "success"
                    and check["app"]["id"] == 15368 and check["app"]["slug"] == "github-actions",
                    f"{gate} check identity mismatch")
    print("PASS  exact-SHA latest workflow attempts and GitHub Actions checks")
    return root


def verify(sha: str) -> None:
    """Verify both release environments and the required hosted gates."""
    root = verify_gates(sha)
    for name in ("release-build", "release-publish"):
        env = api(f"{root}/environments/{name}")
        require(env["can_admins_bypass"] is False, f"{name} allows admin bypass")
        require(env["deployment_branch_policy"] == {"protected_branches": False, "custom_branch_policies": True}, f"{name} deployment policy invalid")
        pages = api(f"{root}/environments/{name}/deployment-branch-policies?per_page=100", True)
        policies = [(p["name"], p["type"]) for page in pages for p in page["branch_policies"]]
        require(policies == [("main", "branch")], f"{name} must allow only main")
        rules = [r for r in env["protection_rules"] if r["type"] == "required_reviewers"]
        if name == "release-build":
            require(not rules or (len(rules) == 1 and rules[0]["reviewers"] == []), "release-build requires reviewers")
        else:
            require(len(rules) == 1 and rules[0]["prevent_self_review"] is False, "publication self-review policy invalid")
            require(len(rules[0]["reviewers"]) == 1 and rules[0]["reviewers"][0]["type"] == "User"
                    and rules[0]["reviewers"][0]["reviewer"]["login"] == "implicitly-awesome", "publication reviewer invalid")
    print("PASS  candidate environments and exact-SHA latest workflow attempts")


if __name__ == "__main__":
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "--gates-only":
            verify_gates(sys.argv[2])
        elif len(sys.argv) == 2:
            verify(sys.argv[1])
        else:
            raise ValueError("expected [--gates-only] SHA")
    except (KeyError, IndexError, TypeError, ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"FAIL  candidate policy: {exc}", file=sys.stderr)
        sys.exit(1)
