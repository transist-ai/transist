#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Resolve recovery solely from durable candidate identity; failed builds get a new attempt."""

import argparse
import os
from pathlib import Path
from typing import Any

import prepare_candidate as prep
import publish_candidate as publication
import release_candidates as rc
import verify_candidate_policy as policy


def recover(helper: publication.Publication, candidate_id: str, claim: bool = False) -> dict[str, Any]:
    """Resolve an existing candidate, or claim a new failed-preparation attempt after readiness."""
    record = helper.record(candidate_id, "failed", "ready", "publishing", "published")
    if record["state"] != "failed":
        if claim:
            raise ValueError("only failed preparation may be claimed again")
        return record
    if not claim:
        return record
    helper.verify_checkout(record)
    policy.verify_gates(record["identity"]["source_sha"], require_push=True)
    ident = record["identity"]
    retry = rc.identity(helper.repository_id, ident["version"], ident["source_sha"], ident["tree_sha"],
                        "release-recovery.yml", int(os.environ["GITHUB_RUN_ID"]), int(os.environ["GITHUB_RUN_ATTEMPT"]))
    result = helper.state.claim(retry, recovery=True)
    # Two recovery dispatches may race. Only the new attempt's owner can prepare.
    if result["identity"] != retry:
        raise ValueError("another recovery already owns this source")
    return result


def main() -> None:
    """Emit source and operation outputs from state, never from operator-provided refs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-id", required=True)
    parser.add_argument("--claim", action="store_true")
    parser.add_argument("--source-directory", type=Path, default=prep.ROOT)
    args = parser.parse_args()
    repo, rid = os.environ["GITHUB_REPOSITORY"], int(os.environ["GITHUB_REPOSITORY_ID"])
    helper = publication.Publication(repo, rid, rc.Candidates(
        rc.GitHubStateAPI(repo, rid, os.environ["GH_TOKEN"]), rid), args.source_directory.resolve())
    record = recover(helper, args.candidate_id, args.claim)
    ident = record["identity"]
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"candidate_id={ident['candidate_id']}\nsource_sha={ident['source_sha']}\n"
                     f"release_tag={ident['final_tag']}\n"
                     f"operation={'prepare' if record['state'] in ('failed', 'preparing') else 'publish'}\n")


if __name__ == "__main__":
    main()
