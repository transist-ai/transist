#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Focused fixtures for candidate policy and latest workflow selection."""

import copy
import base64
import json
import os
import tempfile
import unittest
from unittest.mock import patch

import verify_candidate_policy as policy
import verify_release_receipt as receipt_verifier
import verify_release_refs as refs
from test_release_candidates import FakeAPI, candidate, receipt
from release_candidates import Candidates

SHA = "1" * 40


class CandidatePolicyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.runs = {}
        for index, name in enumerate(("main-ci.yml", "security.yml"), 1):
            self.runs[name] = [{"id": index, "run_number": 1, "run_attempt": 1,
                "head_sha": SHA, "path": f".github/workflows/{name}", "head_branch": "main",
                "event": "push", "repository": {"id": 5}, "head_repository": {"id": 5},
                "run_started_at": "2026-01-01T10:00:00Z",
                "status": "completed", "conclusion": "success"}]
        self.publish = {"can_admins_bypass": False,
            "deployment_branch_policy": {"protected_branches": False, "custom_branch_policies": True},
            "protection_rules": [{"type": "required_reviewers", "prevent_self_review": False,
                "reviewers": [{"type": "User", "reviewer": {"login": "implicitly-awesome"}}]},
                {"id": 101, "node_id": "ENPR_branch", "type": "branch_policy"}]}
        self.build = copy.deepcopy(self.publish)
        self.build["protection_rules"] = [copy.deepcopy(self.publish["protection_rules"][1])]

    def api(self, path: str, paginate: bool = False) -> object:
        if path == "repos/o/r":
            return {"id": 5}
        if "/actions/workflows/" in path:
            workflow = path.split("/workflows/")[1].split("/")[0]
            return [{"workflow_runs": self.runs[workflow]}]
        if "/attempts/" in path and not path.endswith("/jobs?per_page=100"):
            run_id = int(path.split("/runs/")[1].split("/")[0])
            return next(run for runs in self.runs.values() for run in runs if run["id"] == run_id)
        if "/attempts/" in path:
            run_id = int(path.split("/runs/")[1].split("/")[0])
            gate = "main-gate" if run_id in (1, 3) else "security-gate"
            return [{"jobs": [{"name": gate, "run_id": run_id, "status": "completed", "conclusion": "success",
                "check_run_url": f"https://api.github.com/repos/o/r/check-runs/{run_id}"}]}]
        if "/check-runs/" in path:
            check_id = int(path.rsplit("/", 1)[1])
            return {"id": check_id, "name": "main-gate" if check_id in (1, 3) else "security-gate",
                "head_sha": SHA, "status": "completed", "conclusion": "success",
                "app": {"id": 15368, "slug": "github-actions"}}
        if "deployment-branch-policies" in path:
            return [{"branch_policies": [{"name": "main", "type": "branch"}]}]
        if path.endswith("release-build"):
            return self.build
        if path.endswith("release-publish"):
            return self.publish
        raise ValueError(path)

    def verify(self) -> None:
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "o/r"}), patch.object(policy, "api", self.api):
            policy.verify(SHA)

    def test_valid(self) -> None:
        self.verify()

    def test_latest_failed_or_unfinished_cannot_hide_behind_success(self) -> None:
        for status, conclusion in (("completed", "failure"), ("in_progress", None)):
            newer = dict(self.runs["main-ci.yml"][0], id=3, run_number=2, status=status, conclusion=conclusion,
                         run_started_at="2026-01-01T11:00:00Z")
            self.runs["main-ci.yml"].append(newer)
            with self.assertRaises(ValueError):
                self.verify()
            self.runs["main-ci.yml"].pop()

    def test_wrong_workflow_or_source_fails(self) -> None:
        for field, value in (("path", ".github/workflows/other.yml"), ("head_sha", "2" * 40),
                             ("event", "pull_request"), ("head_repository", {"id": 7})):
            run = self.runs["main-ci.yml"][0]
            previous = run[field]
            run[field] = value
            with self.assertRaises(ValueError):
                self.verify()
            run[field] = previous

    def test_publication_protection_fails_closed(self) -> None:
        self.publish["protection_rules"][0]["prevent_self_review"] = True
        with self.assertRaises(ValueError):
            self.verify()
        self.publish["protection_rules"][0]["prevent_self_review"] = False
        self.publish["can_admins_bypass"] = True
        with self.assertRaises(ValueError):
            self.verify()

    def test_latest_wrong_source_or_rerun_failure_is_not_ignored(self) -> None:
        for change in ({"event": "pull_request"}, {"run_attempt": 2, "conclusion": "failure"}):
            self.runs["main-ci.yml"].append(dict(self.runs["main-ci.yml"][0], id=3, run_number=2,
                                                  run_started_at="2026-01-01T11:00:00Z", **change))
            with self.assertRaises(ValueError):
                self.verify()
            self.runs["main-ci.yml"].pop()

    def test_latest_manual_mainline_run_is_verified_in_both_modes(self) -> None:
        latest = dict(self.runs["main-ci.yml"][0], id=3, run_number=2,
                      event="workflow_dispatch", run_started_at="2026-01-01T11:00:00Z")
        self.runs["main-ci.yml"].append(latest)
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "o/r"}), patch.object(policy, "api", self.api):
            for verify in (policy.verify, policy.verify_gates):
                with self.subTest(mode=verify.__name__):
                    verify(SHA)
                    for field, value in (("conclusion", "failure"), ("status", "in_progress"),
                                         ("head_branch", "other"), ("head_repository", {"id": 7})):
                        original = latest[field]
                        latest[field] = value
                        with self.assertRaises(ValueError):
                            verify(SHA)
                        latest[field] = original

    def test_later_rerun_of_older_run_invalidates_newer_success_in_both_modes(self) -> None:
        old = self.runs["main-ci.yml"][0]
        newer = dict(old, id=3, run_number=2, run_started_at="2026-01-01T11:00:00Z")
        self.runs["main-ci.yml"].append(newer)
        old.update(run_attempt=2, run_started_at="2026-01-01T12:00:00Z", conclusion="failure")
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "o/r"}), patch.object(policy, "api", self.api):
            for verify in (policy.verify, policy.verify_gates):
                with self.subTest(mode=verify.__name__), self.assertRaises(ValueError):
                    verify(SHA)

    def test_security_manual_and_scheduled_runs_are_eligible(self) -> None:
        for event in ("schedule", "workflow_dispatch"):
            self.runs["security.yml"][0]["event"] = event
            self.verify()

    def test_same_timestamp_is_ambiguous(self) -> None:
        self.runs["main-ci.yml"].append(dict(self.runs["main-ci.yml"][0], id=3, run_number=2))
        with self.assertRaises(ValueError):
            self.verify()


class ReleaseRefsTest(unittest.TestCase):
    def test_uses_only_selected_repository_and_rejects_wrong_tag_target(self) -> None:
        calls = []
        tag_object = "a" * 40
        target = [SHA]
        def fake_api(path: str) -> object:
            calls.append(path)
            if "/compare/" in path:
                return {"base_commit": {"sha": SHA}, "status": "ahead"}
            if "/git/ref/" in path:
                return {"ref": "refs/tags/v1.2.3", "object": {"type": "tag", "sha": tag_object}}
            return {"sha": tag_object, "tag": "v1.2.3", "object": {"type": "commit", "sha": target[0]}}
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "selected/repo"}), patch.object(refs, "api", fake_api):
            refs.verify_main(SHA)
            refs.verify_tag("v1.2.3", SHA, tag_object)
            target[0] = "b" * 40
            with self.assertRaises(ValueError):
                refs.verify_tag("v1.2.3", SHA, tag_object)
        self.assertTrue(all(path.startswith("repos/selected/repo/") for path in calls))


class ReceiptTest(unittest.TestCase):
    def test_receipt_bound_to_live_state_and_source(self) -> None:
        api = FakeAPI()
        candidates = Candidates(api, 10)
        ident = candidate("a")
        candidates.claim(ident)
        candidates.bind_release(ident["candidate_id"], 123)
        evidence = receipt(ident, 123)
        candidates.ready(ident["candidate_id"], evidence)
        def gh(command: list[str], text: bool) -> str:
            if command[-1] == "repos/o/r":
                return json.dumps({"id": 10})
            return json.dumps({"encoding": "base64", "content": base64.b64encode(json.dumps(api.state).encode()).decode()})
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8") as file:
            json.dump(evidence, file)
            file.flush()
            with patch.dict(os.environ, {"GITHUB_REPOSITORY": "o/r"}), patch.object(receipt_verifier.subprocess, "check_output", gh):
                receipt_verifier.verify(file.name, ident["source_sha"], ident["tree_sha"], ident["final_tag"])
                for sha, tree, tag in (("b" * 40, ident["tree_sha"], ident["final_tag"]),
                                       (ident["source_sha"], "e" * 40, ident["final_tag"]),
                                       (ident["source_sha"], ident["tree_sha"], "v9.9.9")):
                    with self.assertRaises(ValueError):
                        receipt_verifier.verify(file.name, sha, tree, tag)
                api.state["candidates"][ident["candidate_id"]]["state"] = "superseded"
                with self.assertRaises(ValueError):
                    receipt_verifier.verify(file.name, ident["source_sha"], ident["tree_sha"], ident["final_tag"])


if __name__ == "__main__":
    unittest.main()
