#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Check (default) or explicitly provision release environments and orphan state.

Uses authenticated gh; never activates the pipeline, dispatches work, exports
secrets, removes secrets, or checks out/executes the operational state branch.
"""

import argparse
import base64
import json
import os
import re
import stat
import subprocess
import sys
from contextlib import ExitStack
from typing import BinaryIO

from release_candidates import Candidates
from release_environment_policy import load_policy, validate_environment

KEY = "MINISIGN_SECRET_KEY"
ENVIRONMENTS = ("release-build", "release-publish")


def api(
    path: str,
    method: str = "GET",
    data: dict | None = None,
    *,
    missing: bool = False,
    pages: bool = False,
) -> object:
    """Call gh with JSON input, distinguishing an allowed 404 from other failures."""
    command = ["gh", "api", path, "--method", method]
    if pages:
        command += ["--paginate", "--slurp"]
    if data is not None:
        command += ["--input", "-"]
    result = subprocess.run(
        command,
        input=None if data is None else json.dumps(data),
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        if missing and "(HTTP 404)" in result.stderr:
            return None
        raise ValueError(f"GitHub {method} failed: {path}")
    return json.loads(result.stdout) if result.stdout.strip() else None


def policies(root: str, name: str) -> list[dict]:
    """Read every selected deployment policy page."""
    return [
        p
        for page in api(
            f"{root}/environments/{name}/deployment-branch-policies?per_page=100",
            pages=True,
        )
        for p in page["branch_policies"]
    ]


def secret_names(root: str, name: str) -> set[str]:
    """Read secret names only; GitHub does not expose their values."""
    return {
        s["name"]
        for page in api(f"{root}/environments/{name}/secrets?per_page=100", pages=True)
        for s in page["secrets"]
    }


def validate_state(root: str, repository_id: int) -> None:
    """Inspect only a single state.json blob and validate existing lifecycle history."""
    ref = api(f"{root}/git/ref/heads/release-state")
    commit = api(f"{root}/git/commits/{ref['object']['sha']}")
    tree = api(f"{root}/git/trees/{commit['tree']['sha']}")
    entries = tree["tree"]
    if (
        tree.get("truncated")
        or len(entries) != 1
        or (entries[0]["path"], entries[0]["type"], entries[0]["mode"])
        != ("state.json", "blob", "100644")
    ):
        raise ValueError(
            "release-state must contain only a regular state.json; refusing to overwrite"
        )
    blob = api(f"{root}/git/blobs/{entries[0]['sha']}")
    if blob["encoding"] != "base64":
        raise ValueError("invalid state blob encoding")
    state = json.loads(
        base64.b64decode("".join(blob["content"].split()), validate=True)
    )
    Candidates(None, repository_id)._validate_state(state)


def create_state(root: str, repository_id: int) -> None:
    """Create a parentless data-only commit, then a non-forced branch reference."""
    blob = api(
        f"{root}/git/blobs",
        "POST",
        {
            "content": json.dumps(
                {"repository_id": repository_id, "candidates": {}}, indent=2
            )
            + "\n",
            "encoding": "utf-8",
        },
    )
    tree = api(
        f"{root}/git/trees",
        "POST",
        {
            "tree": [
                {
                    "path": "state.json",
                    "mode": "100644",
                    "type": "blob",
                    "sha": blob["sha"],
                },
            ]
        },
    )
    commit = api(
        f"{root}/git/commits",
        "POST",
        {
            "message": "Initialize release candidate state",
            "tree": tree["sha"],
            "parents": [],
        },
    )
    # Never force/reset an existing branch, including one created concurrently.
    api(
        f"{root}/git/refs",
        "POST",
        {"ref": "refs/heads/release-state", "sha": commit["sha"]},
    )
    validate_state(root, repository_id)


def open_key(path: str) -> BinaryIO:
    """Open an owner-only, nonempty regular key file without reading its values."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    stream = os.fdopen(fd, "rb")
    info = os.fstat(fd)
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_uid != os.getuid()
        or info.st_mode & 0o077
        or info.st_size == 0
        or info.st_nlink != 1
    ):
        stream.close()
        raise ValueError(
            "signing-key source must be nonempty, owner-owned, owner-only regular file (no links)"
        )
    return stream


def provision_key(repository: str, stream: BinaryIO) -> None:
    """Stream the owner-supplied file directly to gh; suppress all secret output."""
    environment = {k: v for k, v in os.environ.items() if k != "GH_DEBUG"}
    result = subprocess.run(
        ["gh", "secret", "set", KEY, "--repo", repository, "--env", "release-build"],
        stdin=stream,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=environment,
        check=False,
    )
    if result.returncode:
        raise ValueError(
            "secure signing-key provisioning failed; existing secrets remain untouched"
        )


def setup(repository: str, apply: bool, key_path: str | None) -> None:
    """Check or reconcile policy/state, provisioning only an explicitly supplied key."""
    reviewers, branches = load_policy()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("--repo owner/name is required")
    if key_path and not apply:
        raise ValueError("--signing-key-file is allowed only with --apply")
    root = f"repos/{repository}"
    with ExitStack() as stack:
        key = stack.enter_context(open_key(key_path)) if key_path else None
        metadata = api(root)
        repository_id = metadata["id"]
        if (
            type(repository_id) is not int
            or repository_id <= 0
            or metadata["default_branch"] != "main"
        ):
            raise ValueError(
                "repository must have a positive ID and main default branch"
            )
        variable = api(
            f"{root}/actions/variables/RELEASE_PIPELINE_ENABLED", missing=True
        )
        if variable and variable["value"].lower() == "true":
            raise ValueError(
                "disable RELEASE_PIPELINE_ENABLED before setup; setup never changes it"
            )
        user = api(f"users/{reviewers[0]}")
        if (
            user["login"] != reviewers[0]
            or user["type"] != "User"
            or type(user["id"]) is not int
            or user["id"] <= 0
        ):
            raise ValueError("publication reviewer identity invalid")
        environments = {
            name: api(f"{root}/environments/{name}", missing=True)
            for name in ENVIRONMENTS
        }
        names = {
            name: secret_names(root, name) if environments[name] else set()
            for name in ENVIRONMENTS
        }
        if KEY in names["release-publish"]:
            raise ValueError(
                "release-publish has a signing key; owner must remove it explicitly"
            )
        if KEY not in names["release-build"] and key is None:
            raise ValueError(
                "release-build signing key missing; supply --apply --signing-key-file from owner secure source; secrets cannot be exported"
            )
        ref = api(f"{root}/git/ref/heads/release-state", missing=True)
        if ref:
            validate_state(root, repository_id)
        elif not apply:
            raise ValueError("release-state missing")
        # Read all configuration before the first write. Fail closed on unreadable state/policies.
        current = {
            name: policies(root, name) if environments[name] else []
            for name in ENVIRONMENTS
        }
        if not apply:
            for name in ENVIRONMENTS:
                if not environments[name]:
                    raise ValueError(f"{name} missing")
                validate_environment(name, environments[name], current[name])
        else:
            for name in ENVIRONMENTS:
                desired = {
                    "wait_timer": 0,
                    "prevent_self_review": False,
                    "can_admins_bypass": False,
                    "reviewers": []
                    if name == "release-build"
                    else [{"type": "User", "id": user["id"]}],
                    "deployment_branch_policy": {
                        "protected_branches": False,
                        "custom_branch_policies": True,
                    },
                }
                try:
                    validate_environment(name, environments[name], current[name])
                except (KeyError, TypeError, ValueError):
                    api(f"{root}/environments/{name}", "PUT", desired)
                if not any(
                    {"name": p["name"], "type": p["type"]} == branches[0]
                    for p in current[name]
                ):
                    api(
                        f"{root}/environments/{name}/deployment-branch-policies",
                        "POST",
                        branches[0],
                    )
            if key is not None:
                provision_key(repository, key)
            if KEY not in secret_names(root, "release-build"):
                raise ValueError(
                    "signing key metadata missing after provisioning; no policies removed"
                )
            if not ref:
                create_state(root, repository_id)
            # Validate provisioning before narrowly removing obsolete deployment selections.
            for name in ENVIRONMENTS:
                live = policies(root, name)
                if (
                    sum(
                        {"name": p["name"], "type": p["type"]} == branches[0]
                        for p in live
                    )
                    != 1
                ):
                    raise ValueError(
                        "main deployment policy not provisioned uniquely; no policies removed"
                    )
                env = api(f"{root}/environments/{name}")
                validate_environment(
                    name,
                    env,
                    [
                        p
                        for p in live
                        if {"name": p["name"], "type": p["type"]} == branches[0]
                    ],
                )
            for name in ENVIRONMENTS:
                for p in policies(root, name):
                    if {"name": p["name"], "type": p["type"]} != branches[0]:
                        api(
                            f"{root}/environments/{name}/deployment-branch-policies/{p['id']}",
                            "DELETE",
                        )
                validate_environment(
                    name, api(f"{root}/environments/{name}"), policies(root, name)
                )
        print("PASS  release environments, signing-key presence and data-only state")
        print(
            "Key values cannot be inspected/exported. Step 7 must validate signing against the committed public key before explicit repository-secret removal. Activation is separate."
        )


def main() -> int:
    """Parse explicit setup authority and report sanitized errors without key values."""
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--check", action="store_true", help="read-only (default)")
    modes.add_argument(
        "--apply", action="store_true", help="provision configuration; Step 7 only"
    )
    parser.add_argument("--repo", required=True, help="explicit owner/name target")
    parser.add_argument(
        "--signing-key-file",
        help="owner-supplied secure file, streamed only to gh secret set",
    )
    args = parser.parse_args()
    try:
        setup(args.repo, args.apply, args.signing_key_file)
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        # Do not include subprocess output, file contents, or file paths in errors.
        message = (
            str(exc)
            if isinstance(exc, ValueError) and not isinstance(exc, json.JSONDecodeError)
            else "unreadable or malformed setup input/response"
        )
        print(f"FAIL  {message}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
