#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Validate workflow completion events and exact-SHA release eligibility."""

import json
import os
import re
import subprocess
import sys
import tomllib
from typing import Any

import release_candidates as candidates
import verify_candidate_policy as policy


def completion(event: dict[str, Any], repository: str, repository_id: int) -> tuple[str, tuple[str, int, int]]:
    """Validate a completion payload and return its SHA and triggering attempt identity."""
    run = event["workflow_run"]
    paths = {"Mainline Validation": "main-ci.yml", "Security Scan": "security.yml"}
    workflow = paths.get(run["name"])
    policy.require(event["action"] == "completed" and workflow is not None, "unexpected completion")
    policy.require(event["repository"]["id"] == repository_id and event["repository"]["full_name"] == repository,
                   "event repository mismatch")
    policy.require(run["repository"]["id"] == repository_id and run["head_repository"]["id"] == repository_id,
                   "workflow repository mismatch")
    policy.require(run["path"] == f".github/workflows/{workflow}" and run["event"] == "push"
                   and run["head_branch"] == "main" and run["status"] == "completed"
                   and run["conclusion"] == "success", "workflow source or result mismatch")
    sha = run["head_sha"]
    policy.require(isinstance(sha, str) and re.fullmatch(r"[0-9a-f]{40}", sha) is not None, "invalid source SHA")
    policy.require(type(run["id"]) is int and run["id"] > 0 and type(run["run_attempt"]) is int
                   and run["run_attempt"] > 0, "invalid workflow attempt")
    return sha, (workflow, run["id"], run["run_attempt"])


def validate(event: dict[str, Any], repository: str, repository_id: int) -> str:
    """Verify the trigger and latest successful push gates; return the exact source SHA."""
    sha, trigger = completion(event, repository, repository_id)
    policy.require(policy.api(f"repos/{repository}")["id"] == repository_id, "API repository mismatch")
    policy.verify_gates(sha, require_push=True, trigger=trigger)
    return sha


def claim(event: dict[str, Any], repository: str, repository_id: int, token: str,
          run_id: int, attempt: int, api: candidates.StateAPI, expected_sha: str) -> dict[str, Any]:
    """Revalidate gates and atomically claim a candidate from committed Git metadata.

    The caller must have completed aggregate readiness on the same event SHA.
    Reads Git objects locally and may update the repository's release-state branch.
    Returns the existing or newly claimed candidate record.
    """
    sha = validate(event, repository, repository_id)
    policy.require(sha == expected_sha, "readiness source mismatch")
    policy.require(bool(token), "GitHub token is required")
    tree = subprocess.check_output(["git", "rev-parse", "--verify", f"{sha}^{{tree}}"], text=True).strip()
    manifest = subprocess.check_output(["git", "show", f"{sha}:Cargo.toml"])
    version = tomllib.loads(manifest.decode("utf-8"))["workspace"]["package"]["version"]
    ident = candidates.identity(repository_id, version, sha, tree, "release-eligibility.yml", run_id, attempt)
    return candidates.Candidates(api, repository_id).claim(ident)


if __name__ == "__main__":
    try:
        with open(os.environ["GITHUB_EVENT_PATH"], encoding="utf-8") as file:
            payload = json.load(file)
        repository = os.environ["GITHUB_REPOSITORY"]
        repository_id = int(os.environ["GITHUB_REPOSITORY_ID"])
        if sys.argv[1:] == ["--claim"]:
            token = os.environ["GH_TOKEN"]
            api = candidates.GitHubStateAPI(repository, repository_id, token)
            record = claim(payload, repository, repository_id, token, int(os.environ["GITHUB_RUN_ID"]),
                           int(os.environ["GITHUB_RUN_ATTEMPT"]), api, os.environ["SOURCE_SHA"])
            source = record["identity"]["source_sha"]
        elif not sys.argv[1:]:
            source = validate(payload, repository, repository_id)
            record = None
        else:
            raise ValueError("expected no arguments or --claim")
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as file:
            file.write(f"source_sha={source}\n")
            if record is not None:
                file.write(f"candidate_id={record['identity']['candidate_id']}\n")
                file.write(f"candidate_state={record['state']}\n")
        print(f"PASS  eligibility gates for {source}")
    except (KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"FAIL  release eligibility: {exc}", file=sys.stderr)
        sys.exit(1)
