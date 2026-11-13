#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Verify release source and annotated tag in the API-selected repository."""

import os
import re
import subprocess
import sys

from verify_candidate_policy import api, require


def root() -> str:
    """Return a validated GitHub repository API path."""
    repo = os.environ["GITHUB_REPOSITORY"]
    require(bool(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo)), "invalid repository")
    return f"repos/{repo}"


def verify_main(sha: str) -> None:
    """Require the candidate commit to be an ancestor of the repository's main."""
    comparison = api(f"{root()}/compare/{sha}...main")
    require(comparison["base_commit"]["sha"] == sha and comparison["status"] in ("ahead", "identical"),
            "candidate is not an ancestor of repository main")


def verify_tag(tag: str, sha: str, local_tag: str) -> None:
    """Require the remote annotated tag object and its target to match local evidence."""
    require(bool(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag)), "invalid tag")
    reference = api(f"{root()}/git/ref/tags/{tag}")
    require(reference["ref"] == f"refs/tags/{tag}" and reference["object"]["type"] == "tag"
            and reference["object"]["sha"] == local_tag, "remote tag object mismatch")
    obj = api(f"{root()}/git/tags/{local_tag}")
    require(obj["sha"] == local_tag and obj["tag"] == tag and obj["object"]["type"] == "commit"
            and obj["object"]["sha"] == sha, "remote tag target mismatch")


if __name__ == "__main__":
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "main":
            verify_main(sys.argv[2])
        elif len(sys.argv) == 5 and sys.argv[1] == "tag":
            verify_tag(*sys.argv[2:])
        else:
            raise ValueError("expected main SHA or tag TAG SHA TAG_OBJECT")
    except (KeyError, IndexError, TypeError, ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"FAIL  release refs: {exc}", file=sys.stderr)
        sys.exit(1)
