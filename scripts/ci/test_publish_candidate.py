#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Offline production publication tests with immutable downloaded fixture bytes."""

import copy
import json
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

import prepare_candidate as prep
import publish_candidate as pub
import recover_candidate as recovery
import release_candidates as rc
from test_prepare_candidate import PreparationTests


class PublicationTests(unittest.TestCase):
    def setUp(self):
        fixture = PreparationTests("test_upload_download_ready_exact_inventory_and_receipt")
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        fixture.upload_download()
        fixture.ready()
        self.f = fixture
        self.ident, self.cid = fixture.ident, fixture.ident["candidate_id"]
        self.helper = pub.Publication("owner/repo", 10, fixture.state, fixture.root)
        (fixture.root / "CHANGELOG.md").write_text("## [1.2.3]\n")
        self.original_bytes = copy.deepcopy(fixture.blobs)
        self.tag_ref = None
        self.tag_obj = None
        self.effects = []
        self.fail_at = None
        self.after = False
        self.calls = []
        self.addCleanup(patch.stopall)
        patch.object(prep, "github", side_effect=self.github).start()
        patch.object(subprocess, "run", side_effect=self.run_command).start()
        patch.object(pub.refs, "verify_main").start()
        patch.object(pub.policy, "verify_gates").start()
        patch.object(self.helper, "verify_payload").start()

    def github(self, path, method="GET", data=None, paginate=False):
        self.calls.append((path, method, copy.deepcopy(data)))
        effect = None
        if method != "GET":
            effect = ("tag-object" if path.endswith("/git/tags") else
                      "tag-ref" if path.endswith("/git/refs") else
                      "publish" if data.get("draft") is False else "retag")
            self.assertEqual(self.f.record()["state"], "publishing")
            self.effects.append(effect)
            if self.fail_at == effect and not self.after:
                raise subprocess.CalledProcessError(1, ["fixture", effect])
        if "/git/matching-refs/" in path:
            result = [] if self.tag_ref is None else [copy.deepcopy(self.tag_ref)]
        elif path.endswith("/git/tags") and method == "POST":
            self.tag_obj = {"sha": "e" * 40, "tag": data["tag"],
                            "object": {"type": data["type"], "sha": data["object"]}}
            result = copy.deepcopy(self.tag_obj)
        elif path.endswith("/git/refs") and method == "POST":
            if self.tag_ref is not None:
                raise subprocess.CalledProcessError(1, ["fixture", "ref already exists"])
            self.tag_ref = {"ref": data["ref"], "object": {"type": "tag", "sha": data["sha"]}}
            result = copy.deepcopy(self.tag_ref)
        elif "/git/tags/" in path:
            result = copy.deepcopy(self.tag_obj)
        elif path.endswith("/releases/latest"):
            result = copy.deepcopy(self.f.remote[42])
        else:
            result = self.f.github(path, method, data, paginate)
        if effect is not None and effect == self.fail_at and self.after:
            raise subprocess.CalledProcessError(1, ["fixture", effect])
        return result

    def run_command(self, command, **kwargs):
        if command[0] == "bash":
            self.assertIn("check_release_policy.sh", command[1])
            self.assertIn("--candidate", command)
            return subprocess.CompletedProcess(command, 0)
        if command[:3] == ["gh", "attestation", "verify"]:
            self.assertEqual(command[command.index("--source-digest") + 1], "c" * 40)
        return self.f.run_command(command, **kwargs)

    def publish(self):
        self.helper.publish(self.cid)
        self.assertEqual(self.f.record()["state"], "published")
        self.assertEqual(self.f.blobs, self.original_bytes)
        self.assertFalse(self.f.remote[42]["draft"])
        self.assertEqual(self.f.remote[42]["tag_name"], self.ident["final_tag"])

    def test_full_publication_boolean_api_payload_and_idempotence(self):
        self.publish()
        self.assertEqual(self.effects, ["tag-object", "tag-ref", "retag", "publish"])
        payload = [data for _, method, data in self.calls if method == "PATCH" and data.get("draft") is False]
        self.assertEqual(payload, [{"draft": False, "prerelease": False, "make_latest": "true"}])
        self.effects.clear()
        self.publish()
        self.assertEqual(self.effects, [])

    def test_every_remote_effect_failure_before_and_after_recovers_same_bytes(self):
        for boundary in ("tag-object", "tag-ref", "retag", "publish"):
            for after in (False, True):
                with self.subTest(boundary=boundary, after=after):
                    self.set_remote_ready()
                    self.fail_at, self.after = boundary, after
                    try:
                        self.helper.publish(self.cid)
                    except subprocess.CalledProcessError:
                        pass
                    recovered_ref = boundary == "tag-ref" and after
                    self.assertEqual(self.f.record()["state"], "published" if recovered_ref else "publishing")
                    self.assertEqual(self.f.remote[42]["draft"], not ((boundary == "publish" and after) or recovered_ref))
                    self.assertEqual(self.f.blobs, self.original_bytes)
                    self.fail_at = None
                    self.publish()

    def set_remote_ready(self):
        self.f.api.value["candidates"][self.cid]["state"] = "ready"
        self.f.remote[42].update(draft=True, tag_name=self.ident["draft_tag"])
        self.tag_ref = self.tag_obj = None
        self.effects.clear()

    def test_failure_recording_public_state_recovers_without_any_effect(self):
        with patch.object(self.f.state, "published", side_effect=rc.ConflictError("five conflicts")):
            with self.assertRaises(rc.ConflictError):
                self.helper.publish(self.cid)
        self.assertFalse(self.f.remote[42]["draft"])
        self.assertEqual(self.f.record()["state"], "publishing")
        self.effects.clear()
        self.publish()
        self.assertEqual(self.effects, [])

    def test_public_recovery_after_state_write_failure_ignores_later_failed_gate(self):
        with patch.object(self.f.state, "published", side_effect=rc.ConflictError("state write failed")):
            with self.assertRaises(rc.ConflictError):
                self.helper.publish(self.cid)
        self.assertFalse(self.f.remote[42]["draft"])
        self.assertEqual(self.f.record()["state"], "publishing")
        self.effects.clear()
        self.helper.verify_payload.reset_mock()
        with patch.object(pub.policy, "verify_gates", side_effect=ValueError("newer gate failed")) as gates, \
                patch.object(self.helper, "revalidate", side_effect=AssertionError("mutable checks on public recovery")), \
                patch.object(self.helper, "authenticate_receipt", wraps=self.helper.authenticate_receipt) as receipt, \
                patch.object(self.helper, "download_verified", wraps=self.helper.download_verified) as download, \
                patch.object(self.helper, "final_tag", wraps=self.helper.final_tag) as tag:
            self.publish()
            receipt.assert_called_once()
            download.assert_called_once()
            tag.assert_called_once()
            self.helper.verify_payload.assert_called_once()
            gates.assert_not_called()
            self.assertEqual(self.effects, [])
            # The published terminal state uses the same authenticated no-effect path.
            self.publish()
            gates.assert_not_called()
            self.assertEqual(self.effects, [])

    def test_public_recovery_with_failed_gate_still_rejects_wrong_public_evidence(self):
        with patch.object(self.f.state, "published", side_effect=rc.ConflictError("state write failed")):
            with self.assertRaises(rc.ConflictError):
                self.helper.publish(self.cid)
        public = copy.deepcopy(self.f.remote[42])
        receipt = copy.deepcopy(self.f.record()["receipt"])
        tag = copy.deepcopy(self.tag_obj)
        for wrong in ("id", "source", "release-tag", "prerelease", "receipt", "bytes", "provenance", "attestation", "tag-target"):
            with self.subTest(wrong=wrong):
                self.f.remote[42] = copy.deepcopy(public)
                self.f.api.value["candidates"][self.cid]["receipt"] = copy.deepcopy(receipt)
                self.f.blobs = copy.deepcopy(self.original_bytes)
                self.tag_obj = copy.deepcopy(tag)
                self.f.fail_attestation = wrong == "attestation"
                self.effects.clear()
                if wrong == "id":
                    self.f.remote[42]["id"] = 99
                elif wrong == "source":
                    self.f.remote[42]["target_commitish"] = "b" * 40
                elif wrong == "release-tag":
                    self.f.remote[42]["tag_name"] = "v9.9.9"
                elif wrong == "prerelease":
                    self.f.remote[42]["prerelease"] = True
                elif wrong == "receipt":
                    evidence = json.loads(receipt["attestations"][0])
                    evidence["workflow_sha"] = "b" * 40
                    self.f.api.value["candidates"][self.cid]["receipt"]["attestations"] = [json.dumps(evidence)]
                elif wrong == "bytes":
                    self.f.blobs[42]["release-manifest.txt"]["bytes"] += b"tampered"
                elif wrong == "tag-target":
                    self.tag_obj["object"]["sha"] = "b" * 40
                with patch.object(pub.policy, "verify_gates", side_effect=ValueError("newer gate failed")) as gates, \
                        patch.object(self.helper, "verify_payload", side_effect=ValueError("wrong provenance") if wrong == "provenance" else None):
                    with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                        self.helper.publish(self.cid)
                    gates.assert_not_called()
                self.assertEqual(self.f.record()["state"], "publishing")
                self.assertEqual(self.effects, [])

    def test_each_api_read_failure_is_retryable_without_replacing_bytes(self):
        self.calls.clear()
        self.publish()
        read_boundaries = [i for i, (_, method, _) in enumerate(self.calls, 1) if method == "GET"]
        for boundary in read_boundaries:
            with self.subTest(api_call=boundary):
                self.set_remote_ready()
                count = 0
                original = self.github

                def fail_once(*args, **kwargs):
                    nonlocal count
                    count += 1
                    if count == boundary:
                        raise subprocess.CalledProcessError(1, ["fixture", "API unavailable"])
                    return original(*args, **kwargs)

                with patch.object(prep, "github", side_effect=fail_once):
                    try:
                        self.helper.publish(self.cid)
                    except subprocess.CalledProcessError:
                        pass
                if "publish" not in self.effects:
                    self.assertTrue(self.f.remote[42]["draft"])
                self.publish()

    def test_each_download_and_attestation_failure_retains_reservation_without_tag(self):
        for boundary in range(1, 37):  # 29 asset downloads + seven archive attestations
            with self.subTest(command=boundary):
                self.set_remote_ready()
                count = 0

                def fail_once(command, **kwargs):
                    nonlocal count
                    if command[0] == "gh":
                        count += 1
                        if count == boundary:
                            raise subprocess.CalledProcessError(1, command)
                    return self.run_command(command, **kwargs)

                with patch.object(subprocess, "run", side_effect=fail_once):
                    with self.assertRaises(subprocess.CalledProcessError):
                        self.helper.publish(self.cid)
                self.assertEqual(self.effects, [])
                self.assertTrue(self.f.remote[42]["draft"])
                self.assertEqual(self.f.record()["state"], "publishing")
                self.publish()

    def test_lost_reservation_and_published_state_write_responses_recover(self):
        original = self.f.api.write
        for status in ("publishing", "published"):
            with self.subTest(status=status):
                self.set_remote_ready()

                def lost_response(value, previous):
                    original(value, previous)
                    if value["candidates"][self.cid]["state"] == status:
                        raise RuntimeError("state response lost")

                with patch.object(self.f.api, "write", side_effect=lost_response):
                    with self.assertRaises(RuntimeError):
                        self.helper.publish(self.cid)
                self.assertEqual(self.f.record()["state"], status)
                self.publish()

    def test_asset_listing_mutation_after_retag_prevents_publication(self):
        original = self.github

        def mutate(path, method="GET", data=None, paginate=False):
            value = original(path, method, data, paginate)
            if method == "PATCH" and "tag_name" in data:
                self.f.blobs[42]["release-manifest.txt"]["id"] += 1000
            return value

        with patch.object(prep, "github", side_effect=mutate):
            with self.assertRaisesRegex(ValueError, "listing changed"):
                self.helper.publish(self.cid)
        self.assertTrue(self.f.remote[42]["draft"])
        self.assertNotIn("publish", self.effects)

    def test_reservation_failure_prevents_download_tag_and_release_effects(self):
        with patch.object(self.f.state, "reserve", side_effect=rc.ConflictError("exhausted")), \
                patch.object(self.helper, "download_verified") as download:
            with self.assertRaises(rc.ConflictError):
                self.helper.publish(self.cid)
            download.assert_not_called()
        self.assertEqual(self.effects, [])
        self.assertTrue(self.f.remote[42]["draft"])

    def test_supersession_race_during_reservation_cannot_publish(self):
        def competing(value, previous):
            self.f.api.value["candidates"][self.cid]["state"] = "superseded"
            raise rc.ConflictError("superseded during CAS")

        with patch.object(self.f.api, "write", side_effect=competing):
            with self.assertRaises(rc.CandidateError):
                self.helper.publish(self.cid)
        self.assertEqual(self.effects, [])
        self.assertTrue(self.f.remote[42]["draft"])

    def test_environment_policy_failure_cannot_reserve_or_create_tag(self):
        def reject_policy(command, **kwargs):
            if command[0] == "bash":
                raise subprocess.CalledProcessError(1, command)
            return self.run_command(command, **kwargs)

        with patch.object(subprocess, "run", side_effect=reject_policy):
            with self.assertRaises(subprocess.CalledProcessError):
                self.helper.publish(self.cid)
        self.assertEqual(self.f.record()["state"], "ready")
        self.assertEqual(self.effects, [])

    def test_racing_final_ref_creation_accepts_only_correct_annotated_target(self):
        original = self.github
        for correct in (True, False):
            with self.subTest(correct=correct):
                self.set_remote_ready()

                def race(path, method="GET", data=None, paginate=False):
                    if method == "POST" and path.endswith("/git/refs"):
                        self.tag_ref = {"ref": data["ref"], "object": {"type": "tag", "sha": data["sha"]}}
                        if not correct:
                            self.tag_obj["object"]["sha"] = "b" * 40
                        raise subprocess.CalledProcessError(1, ["fixture", "competing tag creation"])
                    return original(path, method, data, paginate)

                with patch.object(prep, "github", side_effect=race):
                    if correct:
                        self.publish()
                    else:
                        with self.assertRaisesRegex(ValueError, "target mismatch"):
                            self.helper.publish(self.cid)
                        self.assertTrue(self.f.remote[42]["draft"])
                        self.assertEqual(self.f.record()["state"], "publishing")

    def test_revalidation_and_download_boundaries_fail_closed(self):
        for method in ("verify_checkout", "workflow_source", "verify_payload", "download_verified", "check_assets"):
            with self.subTest(method=method):
                self.set_remote_ready()
                with patch.object(self.helper, method, side_effect=ValueError(method)):
                    with self.assertRaises(ValueError):
                        self.helper.publish(self.cid)
                self.assertTrue(self.f.remote[42]["draft"])
                self.assertEqual(self.effects, [])
        for target, method in ((pub.refs, "verify_main"), (pub.policy, "verify_gates")):
            self.set_remote_ready()
            with patch.object(target, method, side_effect=ValueError("latest/source failed")):
                with self.assertRaises(ValueError):
                    self.helper.publish(self.cid)
            self.assertEqual(self.effects, [])

    def test_policy_failure_immediately_before_publish_leaves_private_final_draft(self):
        original = self.helper.revalidate
        count = 0

        def revalidate(record):
            nonlocal count
            count += 1
            if count == 2:
                raise ValueError("new failed gate")
            original(record)

        with patch.object(self.helper, "revalidate", side_effect=revalidate):
            with self.assertRaises(ValueError):
                self.helper.publish(self.cid)
        self.assertEqual(self.effects, ["tag-object", "tag-ref", "retag"])
        self.assertTrue(self.f.remote[42]["draft"])
        self.assertEqual(self.f.record()["state"], "publishing")

    def test_wrong_or_lightweight_existing_tag_rejected_correct_annotated_accepted(self):
        self.f.state.reserve(self.cid, self.f.record()["receipt"])
        record = self.f.record()
        self.helper.final_tag(record, create=True)
        valid_ref, valid_obj = copy.deepcopy(self.tag_ref), copy.deepcopy(self.tag_obj)
        for change in ("lightweight", "wrong-sha", "wrong-name", "nested"):
            self.tag_ref, self.tag_obj = copy.deepcopy(valid_ref), copy.deepcopy(valid_obj)
            if change == "lightweight":
                self.tag_ref["object"]["type"] = "commit"
            elif change == "wrong-sha":
                self.tag_obj["object"]["sha"] = "b" * 40
            elif change == "wrong-name":
                self.tag_obj["tag"] = "v9.9.9"
            else:
                self.tag_obj["object"]["type"] = "tag"
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.helper.publish(self.cid)
            self.assertTrue(self.f.remote[42]["draft"])
        self.tag_ref, self.tag_obj = valid_ref, valid_obj
        self.effects.clear()
        self.publish()
        self.assertNotIn("tag-ref", self.effects)

    def test_asset_tampering_or_attestation_failure_never_tags(self):
        self.f.blobs[42]["release-manifest.txt"]["bytes"] += b"tamper"
        with self.assertRaises(ValueError):
            self.helper.publish(self.cid)
        self.assertEqual(self.effects, [])
        self.f.blobs = copy.deepcopy(self.original_bytes)
        self.f.fail_attestation = True
        with self.assertRaises(subprocess.CalledProcessError):
            self.helper.publish(self.cid)
        self.assertEqual(self.effects, [])

    def test_public_mismatch_hard_failure_and_no_redraft(self):
        self.publish()
        for key, value in (("target_commitish", "b" * 40), ("tag_name", "v9.9.9"), ("prerelease", True), ("id", 99)):
            saved = copy.deepcopy(self.f.remote[42])
            self.f.remote[42][key] = value
            self.effects.clear()
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.helper.publish(self.cid)
            self.assertEqual(self.effects, [])
            self.f.remote[42] = saved

    def test_receipt_attestation_source_and_workflow_tree_tampering(self):
        original = copy.deepcopy(self.f.record()["receipt"])
        for key, value in (("source_sha", "b" * 40), ("workflow_sha", "b" * 40), ("subjects", {})):
            evidence = json.loads(original["attestations"][0])
            evidence[key] = value
            self.f.api.value["candidates"][self.cid]["receipt"]["attestations"] = [json.dumps(evidence)]
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.helper.publish(self.cid)
            self.assertEqual(self.effects, [])
        self.f.api.value["candidates"][self.cid]["receipt"] = original
        original_api = self.github

        def changed(path, *args, **kwargs):
            value = original_api(path, *args, **kwargs)
            if "/contents/.github/workflows?ref=" + "c" * 40 in path:
                value[0]["sha"] = "b" * 40
            return value

        with patch.object(prep, "github", side_effect=changed):
            with self.assertRaisesRegex(ValueError, "workflow trees differ"):
                self.helper.publish(self.cid)
        self.assertEqual(self.effects, [])

    def test_summary_is_preapproval_and_contains_review_evidence(self):
        path = self.f.root / "summary.md"
        with patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(path)}):
            self.helper.summary(self.cid)
        text = path.read_text()
        for value in (self.ident["source_sha"], "1.2.3", "/releases/untagged-42", self.f.record()["receipt"]["manifest_digest"], "29 assets", "Preparation run"):
            self.assertIn(value, text)
        self.assertEqual(self.effects, [])
        self.assertEqual(self.f.record()["state"], "ready")

    def test_recovery_failed_new_identity_and_exact_source_only(self):
        self.f.api.value["candidates"][self.cid].update(state="failed", receipt=None)
        with patch.dict(os.environ, {"GITHUB_RUN_ID": "30", "GITHUB_RUN_ATTEMPT": "2"}):
            result = recovery.recover(self.helper, self.cid, claim=True)
        self.assertEqual(result["identity"]["candidate_id"], self.ident["source_sha"] + "-30-2")
        self.assertEqual(result["identity"]["tree_sha"], self.ident["tree_sha"])
        self.assertEqual(result["identity"]["workflow"], "release-recovery.yml")
        self.assertEqual(result["state"], "preparing")
        self.assertIsNone(result["release_id"])
        self.assertEqual(self.effects, [])
        with self.assertRaisesRegex(ValueError, "another recovery"):
            recovery.recover(self.helper, self.cid, claim=True)

    def test_recovery_ready_reserved_published_never_claims_or_prepares(self):
        for status in ("ready", "publishing", "published"):
            self.f.api.value["candidates"][self.cid]["state"] = status
            result = recovery.recover(self.helper, self.cid)
            self.assertEqual(result["identity"]["candidate_id"], self.cid)
            with self.assertRaises(ValueError):
                recovery.recover(self.helper, self.cid, claim=True)
        for bad in ("main", "v1.2.3", "", "a" * 40):
            with self.assertRaises(ValueError):
                recovery.recover(self.helper, bad)
        with patch.dict(os.environ, {"GITHUB_REF": "refs/heads/other"}):
            with self.assertRaises(ValueError):
                recovery.recover(self.helper, self.cid)

    def test_workflows_approval_order_permissions_and_candidate_only_recovery(self):
        workflow = Path(".github/workflows/release-publication.yml").read_text()
        approved = workflow.split("  publish-release:", 1)[1]
        self.assertIn("needs: review-summary", approved)
        self.assertIn("environment: release-publish", approved)
        self.assertNotIn("environment:", workflow.split("  publish-release:")[0])
        self.assertIn("group: release-publication-effects", approved)
        for forbidden in ("secrets.", "id-token:", "attestations: write", "upload-artifact", "cargo ", "minisign -S"):
            self.assertNotIn(forbidden, workflow)
        self.assertIn("ref: ${{ github.workflow_sha }}", workflow)
        self.assertIn("ref: ${{ inputs.source_sha }}", workflow)
        recovery_workflow = Path(".github/workflows/release-recovery.yml").read_text()
        dispatch = recovery_workflow.split("permissions:", 1)[0]
        self.assertIn("candidate_id:", dispatch)
        self.assertNotIn("source_sha:", dispatch)
        self.assertNotIn("release_tag:", dispatch)
        self.assertIn("github.ref == 'refs/heads/main'", recovery_workflow)
        self.assertIn("uses: ./.github/workflows/release-publication.yml", recovery_workflow)
        automatic = Path(".github/workflows/release-eligibility.yml").read_text()
        self.assertIn("needs: [gates, eligible, prepare]", automatic)
        self.assertIn("uses: ./.github/workflows/release-publication.yml", automatic)
