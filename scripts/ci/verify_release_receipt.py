#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Bind a publication receipt to the current repository's durable candidate state."""

import base64
import json
import os
import subprocess
import sys

from release_candidates import CandidateError, Candidates


def verify(path: str, sha: str, tree: str, tag: str) -> None:
    """Reject receipts not identical to a ready or reserved candidate in state."""
    repo = os.environ["GITHUB_REPOSITORY"]
    def api(endpoint: str) -> object:
        return json.loads(subprocess.check_output(["gh", "api", endpoint], text=True))
    repo_id = api(f"repos/{repo}")["id"]
    content = api(f"repos/{repo}/contents/state.json?ref=release-state")
    if content["encoding"] != "base64":
        raise ValueError("invalid state encoding")
    state = json.loads(base64.b64decode("".join(content["content"].split()), validate=True))
    Candidates(None, repo_id)._validate_state(state)
    with open(path, encoding="utf-8") as file:
        receipt = json.load(file)
    ident = receipt["identity"]
    record = state["candidates"][ident["candidate_id"]]
    if (record["state"] not in ("ready", "publishing") or record["receipt"] != receipt
            or ident["repository_id"] != repo_id or ident["source_sha"] != sha
            or ident["tree_sha"] != tree or ident["final_tag"] != tag):
        raise ValueError("receipt does not match current candidate")


if __name__ == "__main__":
    try:
        verify(*sys.argv[1:])
    except (CandidateError, KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"FAIL  receipt: {exc}", file=sys.stderr)
        sys.exit(1)
