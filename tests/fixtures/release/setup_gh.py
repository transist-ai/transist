#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Stateful fake gh for setup tests; no network or secret-value persistence."""

import base64
import json
import os
import sys
from pathlib import Path

store = Path(os.environ["SETUP_GH_STATE"])
state = json.loads(store.read_text())
args = sys.argv[1:]
if args[:2] == ["secret", "set"]:
    # Consume synthetic input only, and record presence, never bytes.
    assert args == [
        "secret",
        "set",
        "MINISIGN_SECRET_KEY",
        "--repo",
        "o/r",
        "--env",
        "release-build",
    ]
    assert sys.stdin.buffer.read()
    state["calls"].append(["SECRET", "release-build"])
    if not state.get("fail_secret"):
        state["secrets"]["release-build"] = [{"name": "MINISIGN_SECRET_KEY"}]
    store.write_text(json.dumps(state))
    # Even a noisy failing gh cannot leak values through setup output.
    print("synthetic-secret-do-not-log", file=sys.stderr)
    sys.exit(1 if state.get("fail_secret") else 0)
assert args[0] == "api"
path = args[1]
method = args[args.index("--method") + 1]
data = json.load(sys.stdin) if "--input" in args else None
state["calls"].append([method, path, data])
out = None
missing = False
if state.get("fail_path") == path:
    store.write_text(json.dumps(state))
    print("gh: forbidden (HTTP 403)", file=sys.stderr)
    sys.exit(1)
if path == "repos/o/r":
    out = {"id": 10, "default_branch": "main"}
elif path == "users/implicitly-awesome":
    out = {"id": 42, "login": "implicitly-awesome", "type": "User"}
elif path.endswith("/actions/variables/RELEASE_PIPELINE_ENABLED"):
    out = {"value": "true"} if state.get("enabled") else None
    missing = out is None
elif "/environments/" in path:
    suffix = path.split("/environments/")[1]
    name = suffix.split("/")[0]
    if "/secrets?" in suffix:
        out = [{"secrets": state["secrets"].get(name, [])}]
    elif "/deployment-branch-policies" in suffix:
        items = state["policies"].setdefault(name, [])
        if method == "GET":
            # Split pages to exercise complete listing rather than first-page matches.
            out = [{"branch_policies": items[:1]}, {"branch_policies": items[1:]}]
        elif method == "POST":
            items.append(dict(data, id=99))
            out = items[-1]
        elif method == "DELETE":
            state["policies"][name] = [
                p for p in items if str(p["id"]) != suffix.rsplit("/", 1)[1]
            ]
    elif method == "GET":
        out = state["envs"].get(name)
        missing = out is None
    elif method == "PUT":
        out = {
            "can_admins_bypass": data["can_admins_bypass"],
            "deployment_branch_policy": data["deployment_branch_policy"],
            "protection_rules": [
                {"id": 101, "node_id": "ENPR_branch", "type": "branch_policy"}
            ],
        }
        if data["reviewers"]:
            out["protection_rules"] = [
                {
                    "type": "required_reviewers",
                    "prevent_self_review": data["prevent_self_review"],
                    "reviewers": [
                        {"type": "User", "reviewer": {"login": "implicitly-awesome"}}
                    ],
                }
            ] + out["protection_rules"]
        state["envs"][name] = out
elif path.endswith("/git/ref/heads/release-state"):
    missing = not state.get("ref")
    out = {"object": {"sha": "commit"}} if not missing else None
elif path.endswith("/git/refs"):
    assert data["ref"] == "refs/heads/release-state" and not state.get("ref")
    state["ref"] = True
    out = data
elif path.endswith("/git/commits/commit"):
    out = {"tree": {"sha": "tree"}}
elif path.endswith("/git/trees/tree"):
    out = {"tree": state["tree"]}
elif path.endswith("/git/blobs/blob"):
    out = {
        "encoding": "base64",
        "content": base64.b64encode(json.dumps(state["state"]).encode()).decode(),
    }
elif path.endswith("/git/blobs"):
    state["state"] = json.loads(data["content"])
    out = {"sha": "blob"}
elif path.endswith("/git/trees"):
    assert set(data) == {"tree"}  # No base tree: never copy source/workflows.
    state["tree"] = data["tree"]
    out = {"sha": "tree"}
elif path.endswith("/git/commits"):
    assert data["parents"] == []  # Genuine orphan, no main history.
    out = {"sha": "commit"}
else:
    raise AssertionError((method, path, data))
store.write_text(json.dumps(state))
if missing:
    print("gh: not found (HTTP 404)", file=sys.stderr)
    sys.exit(1)
if out is not None:
    print(json.dumps(out))
