#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Focused, offline release candidate state-machine tests."""

import base64
import copy
import io
import json
import hashlib
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

import release_candidates as rc
from test_release_metadata import ReleaseMetadataTests  # noqa: F401
import candidate_assets
import release_eligibility as eligibility
import verify_candidate_policy as policy
from test_prepare_candidate import PreparationTests, RealPayloadTests  # noqa: F401
from test_publish_candidate import PublicationTests  # noqa: F401
from test_setup_release_pipeline import SetupTests  # noqa: F401


class FakeAPI:
    def __init__(self):
        self.state = {"repository_id": 10, "candidates": {}}
        self.revision = 0
        self.writes = 0
        self.conflicts = 0
        self.on_conflict = None
        self.order = {}

    def read(self):
        return copy.deepcopy(self.state), str(self.revision)

    def write(self, state, previous_sha):
        self.writes += 1
        if self.conflicts:
            self.conflicts -= 1
            if self.on_conflict:
                self.on_conflict()
            self.revision += 1
            raise rc.ConflictError("simulated race")
        if previous_sha != str(self.revision):
            raise rc.ConflictError("stale")
        self.state = copy.deepcopy(state)
        self.revision += 1

    def compare(self, base, head):
        return self.order.get((base, head), "diverged")


def candidate(char, run=1, version="1.2.3"):
    return rc.identity(10, version, char * 40, "f" * 40, "prepare.yml", run, 1)


def receipt(ident, release_id):
    assets = {f"asset-{i}": {"sha256": "a" * 64, "size": i} for i in range(28)}
    assets["release-manifest.txt"] = {"sha256": "b" * 64, "size": 12}
    return {"identity": copy.deepcopy(ident), "release_id": release_id,
            "assets": assets, "manifest_digest": "b" * 64,
            "check_run_ids": [41, 42], "attestations": ["attestation:123"]}


class CandidateTests(unittest.TestCase):
    def setUp(self):
        self.api = FakeAPI()
        self.candidates = rc.Candidates(self.api, 10)
        self.old, self.new = candidate("a"), candidate("b", 2)
        self.api.order[(self.old["source_sha"], self.new["source_sha"])] = "ahead"
        self.api.order[(self.new["source_sha"], self.old["source_sha"])] = "behind"

    def claim(self, ident, release_id):
        self.candidates.claim(ident)
        self.candidates.bind_release(ident["candidate_id"], release_id)
        return receipt(ident, release_id)

    def assert_rejected(self, func, *args, **kwargs):
        before = copy.deepcopy(self.api.state)
        writes = self.api.writes
        with self.assertRaises(rc.CandidateError):
            func(*args, **kwargs)
        self.assertEqual(before, self.api.state)
        self.assertEqual(writes, self.api.writes)

    def test_identity_and_workspace_version(self):
        self.assertEqual(self.old["candidate_id"], "a" * 40 + "-1-1")
        self.assertEqual(self.old["draft_tag"], "candidate-" + self.old["candidate_id"])
        self.assertEqual(self.old["draft_title"], "v1.2.3 candidate aaaaaaa")
        self.assertEqual(self.old["final_tag"], "v1.2.3")
        self.assertTrue(rc.workspace_version(Path("Cargo.toml")))
        self.assertEqual(candidate("a", version="1.2.3-rc.1+build.2")["final_tag"], "v1.2.3-rc.1+build.2")
        for version in ("v1.2.3", "01.2.3", "1.2.3-rc.01", "1.2"):
            with self.subTest(version=version), self.assertRaises(rc.CandidateError):
                candidate("a", version=version)

    def test_duplicate_event_reuses_active_identity_and_checks_metadata(self):
        self.candidates.claim(self.old)
        existing = self.candidates.claim(candidate("a", 7))
        self.assertEqual(existing["identity"], self.old)
        self.assertEqual(self.api.writes, 1)
        for change in ({"tree_sha": "e" * 40}, {"version": "1.2.4"}, {"repository_id": 11}):
            altered = dict(self.old, **change)
            if "version" in change:
                altered["final_tag"] = "v1.2.4"
                altered["draft_title"] = "v1.2.4 candidate aaaaaaa"
            self.assert_rejected(self.candidates.claim, altered)

    def test_receipt_mismatches_and_bad_evidence_never_write(self):
        good = self.claim(self.old, 12)
        mutations = []
        for field, value in (("repository_id", 11), ("source_sha", "c" * 40),
                             ("tree_sha", "d" * 40), ("version", "9.9.9")):
            bad = copy.deepcopy(good)
            bad["identity"][field] = value
            mutations.append(bad)
        for field, value in (("release_id", 13), ("manifest_digest", "c" * 64),
                             ("check_run_ids", []), ("attestations", [])):
            bad = copy.deepcopy(good)
            bad[field] = value
            mutations.append(bad)
        bad = copy.deepcopy(good)
        bad["assets"]["../unsafe"] = bad["assets"].pop("asset-1")
        mutations.append(bad)
        bad = copy.deepcopy(good)
        bad["assets"].pop("asset-1")
        mutations.append(bad)
        for bad in mutations:
            with self.subTest(receipt=bad):
                self.assert_rejected(self.candidates.ready, self.old["candidate_id"], bad)

    def test_supersession_and_out_of_order(self):
        old_receipt = self.claim(self.old, 1)
        new_receipt = self.claim(self.new, 2)
        self.candidates.ready(self.old["candidate_id"], old_receipt)
        self.candidates.ready(self.new["candidate_id"], new_receipt)
        self.assertEqual(self.api.state["candidates"][self.old["candidate_id"]]["state"], "superseded")
        self.assert_rejected(self.candidates.reserve, self.old["candidate_id"], old_receipt)

        api = FakeAPI()
        api.order = self.api.order
        machine = rc.Candidates(api, 10)
        for ident, rid in ((self.old, 1), (self.new, 2)):
            machine.claim(ident)
            machine.bind_release(ident["candidate_id"], rid)
        machine.ready(self.new["candidate_id"], new_receipt)
        with self.assertRaises(rc.CandidateError):
            machine.ready(self.old["candidate_id"], old_receipt)
        self.assertEqual(api.state["candidates"][self.new["candidate_id"]]["state"], "ready")

    def test_receipt_predecessor_is_bound_in_promotion_cas(self):
        old_receipt = self.claim(self.old, 1)
        new_receipt = self.claim(self.new, 2)
        self.candidates.ready(self.old["candidate_id"], old_receipt)
        new_receipt["supersedes"] = None
        self.assert_rejected(self.candidates.ready, self.new["candidate_id"], new_receipt)
        new_receipt["supersedes"] = self.old["candidate_id"]
        self.api.conflicts = 1
        self.candidates.ready(self.new["candidate_id"], new_receipt)
        self.assertEqual(self.api.state["candidates"][self.new["candidate_id"]]["receipt"], new_receipt)

    def test_predecessor_is_rechecked_when_cas_conflicts_with_reservation(self):
        old_receipt = self.claim(self.old, 1)
        new_receipt = self.claim(self.new, 2)
        self.candidates.ready(self.old["candidate_id"], old_receipt)
        new_receipt["supersedes"] = self.old["candidate_id"]
        self.api.conflicts = 1

        def reserve_during_conflict():
            self.api.state["candidates"][self.old["candidate_id"]]["state"] = "publishing"

        self.api.on_conflict = reserve_during_conflict
        with self.assertRaisesRegex(rc.CandidateError, "predecessor changed"):
            self.candidates.ready(self.new["candidate_id"], new_receipt)
        self.assertEqual(self.api.state["candidates"][self.new["candidate_id"]]["state"], "preparing")
        self.assertEqual(self.api.state["candidates"][self.old["candidate_id"]]["state"], "publishing")

    def test_failed_preparation_and_explicit_recovery(self):
        r = self.claim(self.old, 1)
        self.candidates.ready(self.old["candidate_id"], r)
        self.claim(self.new, 2)
        self.candidates.fail(self.new["candidate_id"])
        self.assertEqual(self.api.state["candidates"][self.old["candidate_id"]]["state"], "ready")
        retry = candidate("b", 3)
        self.assert_rejected(self.candidates.claim, retry)
        self.assert_rejected(self.candidates.claim, self.new, recovery=True)
        self.candidates.claim(retry, recovery=True)
        self.assertEqual(self.api.state["candidates"][retry["candidate_id"]]["state"], "preparing")

    def test_reservation_and_publication_with_concurrent_preparation(self):
        old_receipt = self.claim(self.old, 1)
        self.candidates.ready(self.old["candidate_id"], old_receipt)
        self.candidates.reserve(self.old["candidate_id"], old_receipt)
        self.candidates.reserve(self.old["candidate_id"], old_receipt)
        new_receipt = self.claim(self.new, 2)
        self.assert_rejected(self.candidates.ready, self.new["candidate_id"], new_receipt)
        tampered = copy.deepcopy(old_receipt)
        tampered["assets"]["asset-1"]["size"] += 1
        self.assert_rejected(self.candidates.published, self.old["candidate_id"], tampered)
        self.candidates.published(self.old["candidate_id"], old_receipt)
        self.candidates.published(self.old["candidate_id"], old_receipt)
        self.assert_rejected(self.candidates.ready, self.new["candidate_id"], new_receipt)
        self.assert_rejected(self.candidates.claim, candidate("c", 3))

    def test_incomparable_commits_and_release_id_reuse(self):
        r = self.claim(self.old, 1)
        self.candidates.ready(self.old["candidate_id"], r)
        other = candidate("c", 3)
        self.candidates.claim(other)
        self.assert_rejected(self.candidates.bind_release, other["candidate_id"], 1)
        self.candidates.bind_release(other["candidate_id"], 3)
        self.assert_rejected(self.candidates.bind_release, other["candidate_id"], 4)
        self.assert_rejected(self.candidates.ready, other["candidate_id"], receipt(other, 3))

    def test_conflicts_retry_and_reevaluate(self):
        self.api.conflicts = 1
        self.candidates.claim(self.old)
        self.assertEqual(self.api.writes, 2)
        newer = candidate("a", 8)
        self.api.conflicts = 1
        def concurrent_claim():
            self.api.state["candidates"][newer["candidate_id"]] = {
                "identity": newer, "state": "preparing", "release_id": None, "receipt": None}
        # A competing valid claim causes reevaluation to reuse that claim.
        self.api.state["candidates"].clear()
        self.api.on_conflict = concurrent_claim
        reused = self.candidates.claim(self.old)
        self.assertEqual(reused["identity"], newer)

    def test_conflict_exhaustion_does_not_write_state(self):
        self.api.conflicts = 5
        with self.assertRaises(rc.ConflictError):
            self.candidates.claim(self.old)
        self.assertEqual(self.api.writes, 5)
        self.assertFalse(self.api.state["candidates"])

    def test_invalid_state_fails_closed(self):
        self.api.state["repository_id"] = 99
        self.assert_rejected(self.candidates.claim, self.old)


class CandidateAssetTests(unittest.TestCase):
    def setUp(self):
        self.names = candidate_assets.expected_assets(
            "v1.2.3", Path("scripts/ci/release-archive-inventory.json"))
        self.assets = [{"name": name, "id": index + 1, "size": len(name.encode()),
                        "state": "uploaded"} for index, name in enumerate(sorted(self.names))]

    def test_paginated_listing_rejects_incomplete_duplicate_and_unsafe_assets(self):
        pages = [self.assets[:10], self.assets[10:20], self.assets[20:]]
        self.assertEqual(set(candidate_assets.validate_listing(pages, self.names)), self.names)
        variations = [self.assets[:-1], self.assets + [self.assets[0]],
                      self.assets + [{"name": "../escape", "id": 99, "size": 0, "state": "uploaded"}],
                      self.assets + [{"name": "extra", "id": 99, "size": 0, "state": "uploaded"}]]
        duplicate_id = copy.deepcopy(self.assets)
        duplicate_id[1]["id"] = duplicate_id[0]["id"]
        variations.append(duplicate_id)
        pending = copy.deepcopy(self.assets)
        pending[0]["state"] = "starter"
        variations.append(pending)
        for entries in variations:
            with self.subTest(entries=entries), self.assertRaises(ValueError):
                candidate_assets.validate_listing([entries], self.names)

    def test_downloaded_bytes_and_recorded_receipt_must_match(self):
        listing = candidate_assets.validate_listing([self.assets], self.names)
        expected = {name: {"sha256": hashlib.sha256(name.encode()).hexdigest(),
                           "size": len(name.encode())} for name in self.names}
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            for name in self.names:
                (directory / name).write_bytes(name.encode())
            self.assertEqual(candidate_assets.verify_downloads(directory, listing, expected), expected)
            name = sorted(self.names)[0]
            (directory / name).write_bytes(b"x" * len(name))
            with self.assertRaises(ValueError):
                candidate_assets.verify_downloads(directory, listing, expected)
            (directory / name).write_bytes(name.encode())
            tampered = copy.deepcopy(expected)
            tampered[name]["sha256"] = "0" * 64
            with self.assertRaises(ValueError):
                candidate_assets.verify_downloads(directory, listing, tampered)
            (directory / "unexpected").touch()
            with self.assertRaises(ValueError):
                candidate_assets.verify_downloads(directory, listing, expected)

    def test_listing_ignores_download_counts_but_binds_server_digest(self):
        first = copy.deepcopy(self.assets)
        later = copy.deepcopy(self.assets)
        for asset in first:
            asset["download_count"] = 0
        for asset in later:
            asset["download_count"] = 1
        self.assertEqual(candidate_assets.validate_listing([first], self.names),
                         candidate_assets.validate_listing([later], self.names))
        later[0]["digest"] = "sha256:" + "0" * 64
        listing = candidate_assets.validate_listing([later], self.names)
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            for name in self.names:
                (directory / name).write_bytes(name.encode())
            expected = candidate_assets.asset_digests(directory, self.names)
            with self.assertRaisesRegex(ValueError, "bytes differ"):
                candidate_assets.verify_downloads(directory, listing, expected)

    def test_release_id_download_rechecks_pages_identity_and_bytes(self):
        ident = candidate("a")
        expected = {name: {"sha256": hashlib.sha256(name.encode()).hexdigest(),
                           "size": len(name.encode())} for name in self.names}
        release = {"id": 42, "draft": True, "tag_name": ident["draft_tag"],
                   "target_commitish": ident["source_sha"]}
        ids = {asset["id"]: asset["name"] for asset in self.assets}
        listing_count = 0
        mode = "valid"

        def api(command, text):
            nonlocal listing_count
            self.assertTrue(text)
            self.assertEqual(command[:2], ["gh", "api"])
            if "/assets?" in command[2]:
                self.assertEqual(command[3:], ["--paginate", "--slurp"])
                listing_count += 1
                pages = [self.assets[:10], self.assets[10:]]
                if mode == "changed-listing" and listing_count == 2:
                    pages = [self.assets[:-1]]
                return json.dumps(pages)
            self.assertEqual(command[2], "repos/owner/repo/releases/42")
            return json.dumps(dict(release, target_commitish="b" * 40) if mode == "wrong-source" else release)

        def download(command, stdout, check):
            self.assertTrue(check)
            asset_id = int(command[2].rsplit("/", 1)[1])
            data = ids[asset_id].encode()
            stdout.write(b"x" * len(data) if mode == "changed-bytes" else data)

        for mode in ("valid", "wrong-source", "changed-bytes", "changed-listing"):
            listing_count = 0
            inventory = Path("scripts/ci/release-archive-inventory.json")
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as root, \
                    patch.object(candidate_assets.subprocess, "check_output", side_effect=api), \
                    patch.object(candidate_assets.subprocess, "run", side_effect=download):
                if mode == "valid":
                    self.assertEqual(candidate_assets.download_draft("owner/repo", 42, ident, Path(root), expected, inventory), expected)
                else:
                    with self.assertRaises(ValueError):
                        candidate_assets.download_draft("owner/repo", 42, ident, Path(root), expected, inventory)


class GitHubAPITests(unittest.TestCase):
    def setUp(self):
        self.api = rc.GitHubStateAPI("owner/repo", 10, "test-token")

    def test_contents_requests_and_compare(self):
        state = {"repository_id": 10, "candidates": {}}
        blob = "a" * 40
        def respond(request, timeout):
            self.assertEqual(timeout, 30)
            self.assertEqual(request.get_header("Authorization"), "Bearer test-token")
            url = request.full_url
            if "/compare/" in url:
                payload = {"status": "ahead"}
            elif "/contents/" in url and request.method == "PUT":
                body = json.loads(request.data)
                self.assertEqual((body["branch"], body["sha"]), ("release-state", blob))
                self.assertEqual(json.loads(base64.b64decode(body["content"])), state)
                payload = {}
            elif "/contents/" in url:
                self.assertTrue(url.endswith("?ref=release-state"))
                payload = {"sha": blob, "encoding": "base64", "content": base64.b64encode(json.dumps(state).encode()).decode()}
            else:
                payload = {"id": 10}
            return io.BytesIO(json.dumps(payload).encode())
        with patch("urllib.request.urlopen", side_effect=respond):
            self.assertEqual(self.api.read(), (state, blob))
            self.api.write(state, blob)
            self.assertEqual(self.api.compare("a" * 40, "b" * 40), "ahead")

    def test_http_conflict_requires_changed_blob(self):
        error = urllib.error.HTTPError("url", 422, "invalid", {}, None)
        with patch.object(self.api, "_check_repo"), patch.object(self.api, "_request", side_effect=error), patch.object(self.api, "read", return_value=({}, "b" * 40)):
            with self.assertRaises(rc.ConflictError):
                self.api.write({}, "a" * 40)
        with patch.object(self.api, "_check_repo"), patch.object(self.api, "_request", side_effect=error), patch.object(self.api, "read", return_value=({}, "a" * 40)):
            with self.assertRaises(urllib.error.HTTPError):
                self.api.write({}, "a" * 40)

    def test_repository_mismatch_fails(self):
        with patch.object(self.api, "_request", return_value={"id": 11}):
            with self.assertRaises(rc.CandidateError):
                self.api.read()


class EligibilityTests(unittest.TestCase):
    def setUp(self):
        self.sha = "a" * 40
        self.run = {"id": 11, "run_attempt": 1, "name": "Mainline Validation",
                    "path": ".github/workflows/main-ci.yml", "event": "push",
                    "head_branch": "main", "head_sha": self.sha, "status": "completed",
                    "conclusion": "success", "repository": {"id": 10}, "head_repository": {"id": 10},
                    "run_started_at": "2026-01-01T00:00:00Z"}
        self.security = dict(self.run, id=12, name="Security Scan", path=".github/workflows/security.yml")
        self.event = {"action": "completed", "repository": {"id": 10, "full_name": "owner/repo"},
                      "workflow_run": copy.deepcopy(self.run)}
        self.runs = {"main-ci.yml": [self.run], "security.yml": [self.security]}

    def api(self, path, paginate=False):
        if path == "repos/owner/repo":
            return {"id": 10}
        for workflow, runs in self.runs.items():
            if f"/workflows/{workflow}/runs?" in path:
                return [{"workflow_runs": runs}]
            for run in runs:
                prefix = f"/actions/runs/{run['id']}/attempts/{run['run_attempt']}"
                if path.endswith(prefix):
                    return run
                if prefix + "/jobs?" in path:
                    gate = "main-gate" if workflow == "main-ci.yml" else "security-gate"
                    return [{"jobs": [{"name": gate, "status": "completed", "conclusion": "success",
                                       "run_id": run["id"], "check_run_url": f"https://api.github.com/repos/owner/repo/check-runs/{run['id']}"}]}]
                if path.endswith(f"/check-runs/{run['id']}"):
                    return {"id": run["id"], "name": "main-gate" if workflow == "main-ci.yml" else "security-gate",
                            "head_sha": self.sha, "status": "completed", "conclusion": "success",
                            "app": {"id": 15368, "slug": "github-actions"}}
        raise AssertionError(path)

    def validate(self):
        with patch.dict("os.environ", {"GITHUB_REPOSITORY": "owner/repo"}), patch.object(policy, "api", side_effect=self.api):
            return eligibility.validate(self.event, "owner/repo", 10)

    def test_both_completion_events_and_duplicates(self):
        self.assertEqual(self.validate(), self.sha)
        self.assertEqual(self.validate(), self.sha)
        self.event["workflow_run"] = copy.deepcopy(self.security)
        self.assertEqual(self.validate(), self.sha)

    def test_reject_wrong_completion_identity(self):
        for field, value in (("event", "workflow_dispatch"), ("head_branch", "other"),
                             ("head_repository", {"id": 11}), ("repository", {"id": 11}),
                             ("path", ".github/workflows/other.yml"), ("conclusion", "failure")):
            with self.subTest(field=field):
                event = copy.deepcopy(self.event)
                event["workflow_run"][field] = value
                with self.assertRaises(ValueError):
                    eligibility.completion(event, "owner/repo", 10)
        self.event["repository"]["id"] = 11
        with self.assertRaises(ValueError):
            self.validate()

    def test_partial_and_stale_runs_fail_closed(self):
        self.runs["security.yml"] = []
        with self.assertRaises(ValueError):
            self.validate()
        newer = dict(self.security, id=13, run_started_at="2026-01-02T00:00:00Z", conclusion="failure")
        self.runs["security.yml"] = [self.security, newer]
        with self.assertRaises(ValueError):
            self.validate()
        newer = dict(self.run, run_attempt=2, run_started_at="2026-01-02T00:00:00Z")
        self.runs["security.yml"] = [self.security]
        self.runs["main-ci.yml"] = [newer]
        with self.assertRaises(ValueError):
            self.validate()

    def test_latest_scan_and_latest_push_must_both_pass(self):
        for event in ("schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                later = dict(self.security, id=13, event=event,
                             run_started_at="2026-01-02T00:00:00Z")
                self.runs["security.yml"] = [self.security, later]
                self.assertEqual(self.validate(), self.sha)
                later["conclusion"] = "failure"
                with self.assertRaises(ValueError):
                    self.validate()
                later["conclusion"] = None
                later["status"] = "in_progress"
                with self.assertRaises(ValueError):
                    self.validate()
                later["conclusion"] = "success"
                later["status"] = "completed"
                self.security["conclusion"] = "failure"
                with self.assertRaises(ValueError):
                    self.validate()
                self.security["conclusion"] = "success"
                self.runs["security.yml"] = [later]
                with self.assertRaises(ValueError):
                    self.validate()
                event_payload = copy.deepcopy(self.event)
                event_payload["workflow_run"] = later
                with self.assertRaises(ValueError):
                    eligibility.completion(event_payload, "owner/repo", 10)

    def test_receipt_check_ids_are_latest_verified_ids_and_fail_atomically(self):
        later = dict(self.security, id=13, event="schedule", run_started_at="2026-01-02T00:00:00Z")
        self.runs["security.yml"] = [self.security, later]
        checks = []
        with patch.dict("os.environ", {"GITHUB_REPOSITORY": "owner/repo"}), patch.object(policy, "api", side_effect=self.api):
            policy.verify_gates(self.sha, require_push=True, check_run_ids=checks)
            self.assertEqual(checks, [11, 13, 12])
            later["conclusion"] = "failure"
            checks[:] = [999]
            with self.assertRaises(ValueError):
                policy.verify_gates(self.sha, require_push=True, check_run_ids=checks)
            self.assertEqual(checks, [999])

    def test_durable_claim_deduplicates_and_rejects_bad_evidence(self):
        state = FakeAPI()
        def git(command, **kwargs):
            if command[1] == "rev-parse":
                return "f" * 40 + "\n"
            return b'[workspace.package]\nversion = "1.2.3"\n'
        def do_claim(expected_sha, run=20):
            with patch.dict("os.environ", {"GITHUB_REPOSITORY": "owner/repo"}), \
                 patch.object(policy, "api", side_effect=self.api), \
                 patch.object(eligibility.subprocess, "check_output", side_effect=git):
                return eligibility.claim(self.event, "owner/repo", 10, "token", run, 1, state, expected_sha)
        with self.assertRaises(ValueError):
            do_claim("b" * 40)
        self.assertEqual(state.writes, 0)
        first = do_claim(self.sha)
        self.assertEqual(first["identity"]["source_sha"], self.sha)
        self.assertEqual(first["identity"]["tree_sha"], "f" * 40)
        self.assertEqual(first["identity"]["version"], "1.2.3")
        self.assertEqual(first["state"], "preparing")
        self.assertEqual(do_claim(self.sha, 21), first)
        self.assertEqual(state.writes, 1)
        self.security["conclusion"] = "failure"
        with self.assertRaises(ValueError):
            do_claim(self.sha, 22)
        self.assertEqual(state.writes, 1)

    def test_workflow_contract(self):
        workflow = Path(".github/workflows/release-eligibility.yml").read_text()
        security = Path(".github/workflows/security.yml").read_text()
        self.assertIn("vars.RELEASE_PIPELINE_ENABLED == 'true'", workflow)
        self.assertIn("workflows: [Mainline Validation, Security Scan]", workflow)
        self.assertIn("ref: ${{ needs.gates.outputs.source_sha }}", workflow)
        self.assertIn('test "$(git rev-parse HEAD)" = "$SOURCE_SHA"', workflow)
        self.assertLess(workflow.index('git config --global --add safe.directory "$GITHUB_WORKSPACE"'),
                        workflow.index('test "$(git rev-parse HEAD)"'))
        self.assertIn("needs: [gates, readiness]", workflow)
        self.assertIn("bash scripts/ci/check_release_readiness.sh", workflow)
        self.assertIn("python3 scripts/ci/release_eligibility.py --claim", workflow)
        self.assertIn("candidate_id: ${{ steps.claim.outputs.candidate_id }}", workflow)
        self.assertNotIn("secrets.", workflow.split("  prepare:\n", 1)[0])
        self.assertNotIn("secrets.", workflow.split("  publish:\n", 1)[1])
        self.assertEqual(workflow.count("secrets."), 1)
        readiness = workflow.split("  readiness:", 1)[1].split("  eligible:", 1)[0]
        self.assertNotIn("contents: write", readiness)
        self.assertNotIn("GH_TOKEN", readiness)
        self.assertIn("  push:\n    branches:\n      - main", security)
        self.assertIn("  schedule:", security)
        self.assertIn("  workflow_dispatch:", security)


if __name__ == "__main__":
    unittest.main()
