#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise setup through a stateful fake gh executable, without network access."""

import copy
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from release_candidates import identity
from release_environment_policy import ROOT, load_policy, validate_environment


class SetupTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.gh = self.directory / "gh"
        self.gh.write_bytes((ROOT / "tests/fixtures/release/setup_gh.py").read_bytes())
        self.gh.chmod(0o700)
        self.store = self.directory / "github.json"
        self.key = self.directory / "owner.key"
        self.key.write_text("synthetic-secret-do-not-log")
        self.key.chmod(0o600)
        self.state = {
            "calls": [],
            "envs": {},
            "policies": {},
            "secrets": {},
            "ref": False,
        }
        self.save()

    def save(self) -> None:
        self.store.write_text(json.dumps(self.state))

    def run_setup(
        self, *args: str, success: bool = True
    ) -> subprocess.CompletedProcess:
        result = subprocess.run(
            [
                "python3",
                str(ROOT / "scripts/ci/setup_release_pipeline.py"),
                "--repo",
                "o/r",
                *args,
            ],
            env=dict(
                os.environ,
                PATH=f"{self.directory}:{os.environ['PATH']}",
                SETUP_GH_STATE=str(self.store),
            ),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(
            result.returncode, 0 if success else 1, result.stdout + result.stderr
        )
        self.assertNotIn("synthetic-secret-do-not-log", result.stdout + result.stderr)
        self.state = json.loads(self.store.read_text())
        return result

    def writes(self) -> list:
        return [c for c in self.state["calls"] if c[0] != "GET"]

    def provision(self) -> None:
        self.run_setup("--apply", "--signing-key-file", str(self.key))

    def clear_calls(self) -> None:
        self.state["calls"] = []
        self.save()

    def test_provision_orphan_exact_policies_then_idempotent_apply_and_default_check(
        self,
    ) -> None:
        self.provision()
        self.assertEqual(self.state["state"], {"repository_id": 10, "candidates": {}})
        commits = [c[2] for c in self.writes() if c[1].endswith("/git/commits")]
        self.assertEqual(commits[0]["parents"], [])
        for name in ("release-build", "release-publish"):
            validate_environment(
                name, self.state["envs"][name], self.state["policies"][name]
            )
        for args in ((), ("--check",), ("--apply",)):
            self.clear_calls()
            self.run_setup(*args)
            self.assertEqual(self.writes(), [])

    def test_missing_key_blocks_before_any_mutation_no_secret_export(self) -> None:
        for args in ((), ("--apply",)):
            self.run_setup(*args, success=False)
            self.assertEqual(self.writes(), [])
        self.assertTrue(all("/secrets/" not in c[1] for c in self.state["calls"]))

    def test_check_never_opens_supplied_key_or_writes(self) -> None:
        self.run_setup("--check", "--signing-key-file", "/missing", success=False)
        self.assertEqual(self.state["calls"], [])

    def test_insecure_missing_empty_linked_key_block_before_api(self) -> None:
        self.key.chmod(0o644)
        self.run_setup("--apply", "--signing-key-file", str(self.key), success=False)
        self.key.chmod(0o600)
        link = self.directory / "link"
        link.symlink_to(self.key)
        self.run_setup("--apply", "--signing-key-file", str(link), success=False)
        self.key.write_bytes(b"")
        self.run_setup("--apply", "--signing-key-file", str(self.key), success=False)
        self.assertEqual(self.state["calls"], [])

    def test_enabled_pipeline_and_forbidden_api_fail_without_writes(self) -> None:
        for change in (
            {"enabled": True},
            {"fail_path": "repos/o/r/git/ref/heads/release-state"},
        ):
            self.state.update(change)
            self.save()
            self.run_setup(
                "--apply", "--signing-key-file", str(self.key), success=False
            )
            self.assertEqual(self.writes(), [])
            for field in change:
                self.state.pop(field)

    def test_secret_failure_does_not_remove_policy_or_create_state(self) -> None:
        self.state["fail_secret"] = True
        self.state["policies"] = {
            "release-build": [{"name": "v*", "type": "tag", "id": 1}]
        }
        self.save()
        self.run_setup("--apply", "--signing-key-file", str(self.key), success=False)
        self.assertFalse(
            any(c[0] == "DELETE" or "/git/" in c[1] for c in self.writes())
        )
        self.assertFalse(self.state["ref"])

    def test_extra_policies_removed_only_after_provisioning_no_secrets_or_variable_deleted(
        self,
    ) -> None:
        self.state["policies"] = {
            "release-build": [{"name": "v*", "type": "tag", "id": 1}]
        }
        self.save()
        self.provision()
        writes = self.writes()
        deletion = next(i for i, c in enumerate(writes) if c[0] == "DELETE")
        self.assertGreater(
            deletion, next(i for i, c in enumerate(writes) if c[0] == "SECRET")
        )
        self.assertGreater(
            deletion,
            next(i for i, c in enumerate(writes) if c[1].endswith("/git/refs")),
        )
        self.assertTrue(
            all(
                "/deployment-branch-policies/" in c[1]
                for c in writes
                if c[0] == "DELETE"
            )
        )
        self.assertFalse(any("/actions/variables" in c[1] for c in writes))

    def test_existing_state_preserved_and_unsafe_or_wrong_repo_state_rejected(
        self,
    ) -> None:
        self.provision()
        good = copy.deepcopy(self.state)
        for mutate in (
            lambda s: s["state"].update(repository_id=99),
            lambda s: s["tree"].append(
                {
                    "path": ".github/workflows/pwn.yml",
                    "type": "blob",
                    "mode": "100644",
                    "sha": "bad",
                }
            ),
            lambda s: s["tree"][0].update(mode="120000"),
        ):
            self.state = copy.deepcopy(good)
            self.state["calls"] = []
            mutate(self.state)
            self.save()
            self.run_setup("--apply", success=False)
            self.assertEqual(self.writes(), [])

    def test_publish_secret_blocks_and_does_not_delete_it(self) -> None:
        self.provision()
        self.state["secrets"]["release-publish"] = [{"name": "MINISIGN_SECRET_KEY"}]
        self.clear_calls()
        self.run_setup("--apply", success=False)
        self.assertEqual(self.writes(), [])

    def test_policy_file_drift_rejected(self) -> None:
        load_policy()
        for reviewers, branches in (
            ([], [{"name": "main", "type": "branch"}]),
            (["implicitly-awesome"], [{"name": "v*", "type": "tag"}]),
        ):
            (self.directory / ".release_reviewers").write_text(json.dumps(reviewers))
            (self.directory / ".branch_policies").write_text(json.dumps(branches))
            with self.assertRaises(ValueError):
                load_policy(self.directory)

    def test_policy_drift_and_additional_protection_fail_read_only(self) -> None:
        self.provision()
        good = copy.deepcopy(self.state)
        for name in ("release-build", "release-publish"):
            for change in (
                "bypass",
                "tag",
                "wildcard",
                "duplicate",
                "protection",
                "self-review",
                "reviewer",
            ):
                self.state = copy.deepcopy(good)
                self.state["calls"] = []
                environment = self.state["envs"][name]
                if change == "bypass":
                    environment["can_admins_bypass"] = True
                elif change == "tag":
                    self.state["policies"][name][0]["type"] = "tag"
                elif change == "wildcard":
                    self.state["policies"][name][0]["name"] = "*"
                elif change == "duplicate":
                    self.state["policies"][name] *= 2
                elif change == "protection":
                    environment["protection_rules"].append(
                        {"type": "wait_timer", "wait_timer": 10}
                    )
                elif name == "release-build":
                    continue
                elif change == "self-review":
                    environment["protection_rules"][0]["prevent_self_review"] = True
                else:
                    environment["protection_rules"][0]["reviewers"][0]["reviewer"][
                        "login"
                    ] = "other"
                self.save()
                self.run_setup(success=False)
                self.assertEqual(self.writes(), [])

    def test_builtin_branch_rule_does_not_widen_policy_or_hide_unknown_rules(
        self,
    ) -> None:
        self.provision()
        for name in ("release-build", "release-publish"):
            environment = self.state["envs"][name]
            policies = self.state["policies"][name]
            branch = next(
                r
                for r in environment["protection_rules"]
                if r["type"] == "branch_policy"
            )
            self.assertEqual(
                branch, {"id": 101, "node_id": "ENPR_branch", "type": "branch_policy"}
            )
            reordered = copy.deepcopy(environment)
            reordered["protection_rules"].reverse()
            validate_environment(name, reordered, policies)
            for wrong_policies in (
                [],
                [{"name": "*", "type": "branch"}],
                [{"name": "main", "type": "tag"}],
                policies * 2,
            ):
                with self.assertRaises(ValueError):
                    validate_environment(name, environment, wrong_policies)
            for additional in (
                branch,
                {"type": "unknown"},
                {"type": "wait_timer", "wait_timer": 0},
            ):
                altered = copy.deepcopy(environment)
                altered["protection_rules"].append(copy.deepcopy(additional))
                with self.assertRaises(ValueError):
                    validate_environment(name, altered, policies)
            for mutation in ({"id": False}, {"node_id": None}, {"reviewers": []}):
                altered = copy.deepcopy(environment)
                next(
                    r
                    for r in altered["protection_rules"]
                    if r["type"] == "branch_policy"
                ).update(mutation)
                with self.assertRaises(ValueError):
                    validate_environment(name, altered, policies)

    def test_failed_secret_provision_is_recoverable_and_keeps_existing_history(
        self,
    ) -> None:
        self.state["fail_secret"] = True
        self.save()
        self.run_setup("--apply", "--signing-key-file", str(self.key), success=False)
        self.state.pop("fail_secret")
        self.save()
        self.provision()
        ident = identity(10, "1.2.3", "a" * 40, "b" * 40, "release.yml", 1, 1)
        self.state["state"]["candidates"][ident["candidate_id"]] = {
            "identity": ident,
            "state": "failed",
            "release_id": 123,
            "receipt": None,
        }
        original = copy.deepcopy(self.state["state"])
        self.clear_calls()
        self.run_setup("--apply")
        self.assertEqual(self.state["state"], original)
        self.assertEqual(self.writes(), [])


if __name__ == "__main__":
    unittest.main()
