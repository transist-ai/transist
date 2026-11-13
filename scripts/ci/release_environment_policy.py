#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Shared committed environment contract for checking and provisioning releases."""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load_policy(root: Path = ROOT) -> tuple[list[str], list[dict]]:
    """Load and validate the fixed reviewer and main-only deployment contract."""
    reviewers = json.loads((root / ".release_reviewers").read_text())
    branches = json.loads((root / ".branch_policies").read_text())
    if reviewers != ["implicitly-awesome"] or branches != [
        {"name": "main", "type": "branch"}
    ]:
        raise ValueError(
            "committed release policy must require implicitly-awesome and main branch only"
        )
    return reviewers, branches


def validate_environment(name: str, environment: dict, policies: list[dict]) -> None:
    """Reject live policy drift, including additional deployment protection rules."""
    reviewers, branches = load_policy()
    if environment["can_admins_bypass"] is not False or environment[
        "deployment_branch_policy"
    ] != {
        "protected_branches": False,
        "custom_branch_policies": True,
    }:
        raise ValueError(f"{name} bypass or deployment policy invalid")
    actual = [{"name": p["name"], "type": p["type"]} for p in policies]
    if actual != branches:
        raise ValueError(f"{name} must allow only committed main branch policy")
    rules = environment["protection_rules"]
    if not isinstance(rules, list) or any(not isinstance(rule, dict) for rule in rules):
        raise ValueError(f"{name} protection rules malformed")
    # GitHub also represents selected deployment branches as a built-in rule.
    # Its metadata grants nothing: the separately fetched policies above must
    # still match main/branch exactly. Do not filter arbitrary unknown rules.
    branch_rules = [rule for rule in rules if rule.get("type") == "branch_policy"]
    if len(branch_rules) > 1 or any(
        set(rule) - {"type", "id", "node_id"}
        or ("id" in rule and (type(rule["id"]) is not int or rule["id"] <= 0))
        or (
            "node_id" in rule
            and (not isinstance(rule["node_id"], str) or not rule["node_id"])
        )
        for rule in branch_rules
    ):
        raise ValueError(f"{name} built-in branch_policy rule malformed or duplicated")
    rules = [rule for rule in rules if rule.get("type") != "branch_policy"]
    if name == "release-build":
        if rules:
            raise ValueError(
                "release-build permits only the built-in branch_policy rule"
            )
    elif name == "release-publish":
        if (
            len(rules) != 1
            or rules[0]["type"] != "required_reviewers"
            or rules[0]["prevent_self_review"] is not False
            or len(rules[0]["reviewers"]) != len(reviewers)
            or [(r["type"], r["reviewer"]["login"]) for r in rules[0]["reviewers"]]
            != [("User", login) for login in reviewers]
        ):
            raise ValueError("publication reviewer or self-review policy invalid")
    else:
        raise ValueError("unknown release environment")
