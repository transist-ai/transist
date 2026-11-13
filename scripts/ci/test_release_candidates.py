#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Focused, offline release candidate state-machine tests."""

import base64
import copy
import io
import json
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

import release_candidates as rc


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


if __name__ == "__main__":
    unittest.main()
