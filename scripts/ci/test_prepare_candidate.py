#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Offline production-helper tests: fake remote API, real local payload verification."""

import base64
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import candidate_assets
import prepare_candidate as prep
import release_candidates as rc
from release_inventory import expected_members

GITHUB_TRANSPORT = prep.github
RUN_COMMAND = subprocess.run


class MemoryState:
    def __init__(self):
        self.value = {"repository_id": 10, "candidates": {}}
        self.revision = 0
        self.conflicts = 0

    def read(self):
        return copy.deepcopy(self.value), str(self.revision)

    def write(self, value, previous_sha):
        if self.conflicts:
            self.conflicts -= 1
            raise rc.ConflictError("fixture race")
        if previous_sha != str(self.revision):
            raise rc.ConflictError("stale")
        self.value = copy.deepcopy(value)
        self.revision += 1

    def compare(self, base, head):
        return "ahead" if base == "b" * 40 and head == "a" * 40 else "diverged"


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.downloaded = self.root / "downloaded"
        self.ident = rc.identity(10, "1.2.3", "a" * 40, "f" * 40,
                                 ".github/workflows/release-eligibility.yml", 20, 1)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
        self.api = MemoryState()
        self.state = rc.Candidates(self.api, 10)
        self.state.claim(self.ident)
        self.helper = prep.Preparation("owner/repo", 10, self.state, self.root)
        self.names = candidate_assets.expected_assets("v1.2.3", prep.INVENTORY)
        for name in self.names:
            (self.dist / name).write_bytes(name.encode())
        self.remote = {}
        self.blobs = {}
        self.calls = []
        self.upload_failure = None
        self.fail_attestation = False
        self.next_id = 42
        self.addCleanup(patch.stopall)
        patch.dict(os.environ, {"GITHUB_RUN_ID": "20", "GITHUB_RUN_ATTEMPT": "1",
                                "GITHUB_REF": "refs/heads/main", "GITHUB_REPOSITORY": "owner/repo"}).start()
        patch.object(prep, "github", side_effect=self.github).start()
        patch.object(subprocess, "check_output", side_effect=self.check_output).start()
        patch.object(subprocess, "run", side_effect=self.run_command).start()

    def github(self, path, method="GET", data=None, paginate=False):
        self.calls.append((path, method, copy.deepcopy(data), paginate))
        if path == "repos/owner/repo":
            return {"id": 10}
        if "/actions/runs/" in path:
            return {"id": 20, "run_attempt": 1, "path": ".github/workflows/release-eligibility.yml",
                    "head_sha": "c" * 40, "head_branch": "main", "event": "workflow_run",
                    "repository": {"id": 10}, "head_repository": {"id": 10}}
        if "/git/commits/" in path:
            return {"sha": self.ident["source_sha"], "tree": {"sha": self.ident["tree_sha"]}}
        if "/contents/.github/workflows?" in path:
            return [{"path": ".github/workflows/release.yml", "sha": "d" * 40, "type": "file"}]
        if path.endswith("/releases?per_page=100"):
            self.assertTrue(paginate)
            return [list(self.remote.values())]
        if method == "POST":
            self.assertEqual(path, "repos/owner/repo/releases")
            self.assertTrue(data["draft"])
            self.assertEqual(data["make_latest"], "false")
            self.assertEqual(data["target_commitish"], self.ident["source_sha"])
            draft = dict(data, id=self.next_id,
                         html_url=f"https://github.com/owner/repo/releases/untagged-{self.next_id}")
            self.remote[self.next_id] = draft
            self.next_id += 1
            return copy.deepcopy(draft)
        rid = int(path.split("/releases/", 1)[1].split("/")[0])
        if "/assets?" in path:
            self.assertTrue(paginate)
            entries = [dict(name=name, id=blob["id"], size=len(blob["bytes"]), state="uploaded")
                       for name, blob in self.blobs.get(rid, {}).items()]
            return [entries[:10], entries[10:]] if entries else [[]]
        if method == "PATCH":
            self.remote[rid].update(data)
        return copy.deepcopy(self.remote[rid])

    def check_output(self, command, **kwargs):
        if command[:2] == ["git", "rev-parse"]:
            return (self.ident["source_sha"] if command[2] == "HEAD" else self.ident["tree_sha"]) + "\n"
        self.assertEqual(command[:2], ["gh", "api"])
        if "/assets?" in command[2]:
            self.assertEqual(command[3:], ["--paginate", "--slurp"])
            return json.dumps(self.github(command[2], paginate=True))
        return json.dumps(self.github(command[2]))

    def run_command(self, command, **kwargs):
        self.calls.append((command, kwargs))
        self.assertTrue(kwargs.get("check"))
        if command[:3] == ["gh", "attestation", "verify"]:
            if self.fail_attestation:
                raise subprocess.CalledProcessError(1, command)
            self.assertIn("--signer-workflow", command)
            self.assertIn("owner/repo/.github/workflows/release.yml", command)
            self.assertIn("--source-ref", command)
            self.assertIn("refs/heads/main", command)
            return subprocess.CompletedProcess(command, 0)
        if command[:2] == ["gh", "api"] and "uploads.github.com" in command[2]:
            name = command[2].split("?name=", 1)[1]
            rid = int(command[2].split("/releases/", 1)[1].split("/")[0])
            blobs = self.blobs.setdefault(rid, {})
            if self.upload_failure is not None and len(blobs) == self.upload_failure:
                raise subprocess.CalledProcessError(1, command)
            self.assertNotIn(name, blobs)
            blobs[name] = {"id": 100 + len(blobs), "bytes": Path(command[-1]).read_bytes()}
        elif command[:2] == ["gh", "api"]:
            asset_id = int(command[2].rsplit("/", 1)[1])
            matches = [b for b in self.blobs[42].values() if b["id"] == asset_id]
            self.assertEqual(len(matches), 1)
            kwargs["stdout"].write(matches[0]["bytes"])
        else:
            self.fail("unexpected command: " + repr(command))
        return subprocess.CompletedProcess(command, 0)

    def execute(self, command, directory=None, bundle=None):
        self.helper.execute(command, self.ident["candidate_id"], directory or self.dist, bundle)

    def record(self):
        return self.api.value["candidates"][self.ident["candidate_id"]]

    def upload_download(self):
        self.execute("upload")
        self.execute("download", self.downloaded)

    def bundle(self):
        digests = candidate_assets.asset_digests(self.downloaded, self.names)
        statement = {"_type": "https://in-toto.io/Statement/v1",
                     "predicateType": "https://slsa.dev/provenance/v1",
                     "subject": [{"name": n, "digest": {"sha256": m["sha256"]}}
                                 for n, m in sorted(digests.items()) if n.endswith(".tar.gz")]}
        value = {"dsseEnvelope": {"payloadType": "application/vnd.in-toto+json",
                                  "payload": base64.b64encode(json.dumps(statement).encode()).decode(),
                                  "signatures": [{"sig": "fixture-verified-by-mocked-gh"}]}}
        path = self.root / "attestation.json"
        path.write_text(json.dumps(value))
        return path

    def checks(self, sha, **kwargs):
        self.assertEqual(sha, self.ident["source_sha"])
        self.assertTrue(kwargs["require_push"])
        kwargs["check_run_ids"][:] = [123, 456]

    def ready(self, bundle=None):
        with patch.object(self.helper, "verify_payload"), patch.object(prep.policy, "verify_gates", side_effect=self.checks):
            self.execute("ready", self.downloaded, bundle or self.bundle())

    def test_upload_download_ready_exact_inventory_and_receipt(self):
        self.api.conflicts = 1
        self.upload_download()
        self.assertEqual(len(self.blobs[42]), 29)
        self.assertFalse((self.dist / ".candidate-assets.json").exists())
        self.ready()
        self.assertEqual(self.record()["state"], "ready")
        receipt = self.record()["receipt"]
        self.assertEqual(receipt, json.loads((self.root / ".candidate-receipt.json").read_text()))
        self.assertEqual(receipt["check_run_ids"], [123, 456])
        self.assertEqual(receipt["manifest_digest"], receipt["assets"]["release-manifest.txt"]["sha256"])
        self.assertEqual(len(json.loads(receipt["attestations"][0])["subjects"]), 7)
        self.assertEqual(self.remote[42]["tag_name"], self.ident["draft_tag"])
        self.assertTrue(self.remote[42]["draft"])
        self.assertFalse(any("git/tags" in str(c) or "git/refs" in str(c) for c in self.calls))

    def test_partial_upload_records_failure_and_never_snapshots(self):
        self.upload_failure = 4
        with self.assertRaises(subprocess.CalledProcessError):
            self.execute("upload")
        self.assertEqual(self.record()["state"], "failed")
        self.assertEqual(len(self.blobs[42]), 4)
        self.assertFalse(self.helper.snapshot_path.exists())
        self.assertIsNone(self.record()["receipt"])

    def test_existing_assets_are_never_overwritten(self):
        self.execute("upload")
        self.helper.snapshot_path.unlink()
        before = copy.deepcopy(self.blobs)
        with self.assertRaisesRegex(ValueError, "existing assets"):
            self.execute("upload")
        self.assertEqual(self.blobs, before)
        self.assertEqual(self.record()["state"], "failed")

    def test_orphan_draft_is_not_adopted(self):
        self.remote[99] = {"tag_name": self.ident["draft_tag"]}
        with self.assertRaisesRegex(ValueError, "without binding"):
            self.execute("upload")
        self.assertIsNone(self.record()["release_id"])
        self.assertEqual(self.record()["state"], "failed")

    def test_changed_remote_bytes_fail_download(self):
        self.execute("upload")
        blob = next(iter(self.blobs[42].values()))
        blob["bytes"] = b"x" * len(blob["bytes"])
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            self.execute("download", self.downloaded)
        self.assertEqual(self.record()["state"], "failed")

    def test_extra_remote_asset_fails_before_download(self):
        self.execute("upload")
        self.blobs[42]["unexpected"] = {"id": 999, "bytes": b""}
        with self.assertRaises(ValueError):
            self.execute("download", self.downloaded)
        self.assertFalse(self.downloaded.exists())
        self.assertEqual(self.record()["state"], "failed")

    def test_wrong_release_source_is_not_ready(self):
        self.upload_download()
        self.remote[42]["target_commitish"] = "b" * 40
        with self.assertRaisesRegex(ValueError, "identity mismatch"):
            self.ready()
        self.assertEqual(self.record()["state"], "failed")

    def test_modified_download_fails_before_verification(self):
        self.upload_download()
        (self.downloaded / "release-manifest.txt").write_text("tamper")
        with self.assertRaises(ValueError):
            self.ready()
        self.assertEqual(self.record()["state"], "failed")

    def test_failed_cryptographic_attestation_never_readies(self):
        self.upload_download()
        self.fail_attestation = True
        with self.assertRaises(subprocess.CalledProcessError):
            self.ready()
        self.assertEqual(self.record()["state"], "failed")
        self.assertFalse((self.root / ".candidate-receipt.json").exists())

    def test_attestation_subject_mutations_and_missing_bundle_rejected(self):
        self.upload_download()
        path = self.bundle()
        original = json.loads(path.read_text())
        digests = candidate_assets.asset_digests(self.downloaded, self.names)
        for mode in ("changed-digest", "missing-subject", "extra-subject", "duplicate", "wrong-name", "unsigned", "wrong-predicate"):
            with self.subTest(mode=mode):
                value = copy.deepcopy(original)
                env = value["dsseEnvelope"]
                statement = json.loads(base64.b64decode(env["payload"]))
                if mode == "changed-digest":
                    statement["subject"][0]["digest"]["sha256"] = "0" * 64
                elif mode == "missing-subject":
                    statement["subject"].pop()
                elif mode == "extra-subject":
                    statement["subject"].append({"name": "extra", "digest": {"sha256": "0" * 64}})
                elif mode == "duplicate":
                    statement["subject"].append(statement["subject"][0])
                elif mode == "wrong-name":
                    statement["subject"][0]["name"] = "../escape.tar.gz"
                elif mode == "unsigned":
                    env["signatures"] = []
                elif mode == "wrong-predicate":
                    statement["predicateType"] = "other"
                env["payload"] = base64.b64encode(json.dumps(statement).encode()).decode()
                path.write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    self.helper.attestations(self.record(), self.downloaded, path, digests)
        path.unlink()
        with self.assertRaises(ValueError):
            self.ready(path)
        self.assertEqual(self.record()["state"], "failed")

    def test_snapshot_identity_digest_listing_and_receipt_tampering(self):
        self.upload_download()
        original = json.loads(self.helper.snapshot_path.read_text())
        for field in ("identity", "release_id", "assets", "listing"):
            with self.subTest(field=field):
                value = copy.deepcopy(original)
                if field == "identity":
                    value[field]["tree_sha"] = "0" * 40
                elif field == "release_id":
                    value[field] = 999
                elif field == "assets":
                    value[field]["release-manifest.txt"]["sha256"] = "0" * 64
                else:
                    next(iter(value[field].values()))["id"] = 999
                self.helper.snapshot_path.write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    snapshot = self.helper.snapshot(self.record())
                    listing = self.helper.check_snapshot(self.record(), snapshot)
                    candidate_assets.verify_downloads(self.downloaded, listing, snapshot["assets"])
        self.helper.snapshot_path.write_text(json.dumps(original))
        (self.root / ".candidate-receipt.json").write_text('{"tampered": true}')
        with self.assertRaisesRegex(ValueError, "local receipt"):
            self.ready()
        self.assertEqual(self.record()["state"], "failed")

    def test_late_failed_gates_and_verifier_failures_do_not_promote(self):
        self.upload_download()
        with patch.object(self.helper, "verify_payload", side_effect=ValueError("wrong provenance")):
            with self.assertRaisesRegex(ValueError, "wrong provenance"):
                self.execute("ready", self.downloaded, self.bundle())
        self.assertEqual(self.record()["state"], "failed")

    def test_latest_failed_gate_blocks_ready(self):
        self.upload_download()
        with patch.object(self.helper, "verify_payload"), patch.object(prep.policy, "verify_gates", side_effect=ValueError("latest failed")):
            with self.assertRaisesRegex(ValueError, "latest failed"):
                self.execute("ready", self.downloaded, self.bundle())
        self.assertEqual(self.record()["state"], "failed")

    def test_supersession_only_after_successful_promotion(self):
        old = rc.identity(10, "1.2.3", "b" * 40, "e" * 40, "prepare.yml", 19, 1)
        self.state.claim(old)
        self.state.bind_release(old["candidate_id"], 41)
        old_receipt = {"identity": old, "release_id": 41,
                       "assets": candidate_assets.asset_digests(self.dist, self.names),
                       "manifest_digest": hashlib.sha256(b"release-manifest.txt").hexdigest(),
                       "check_run_ids": [1], "attestations": ["old evidence"]}
        self.state.ready(old["candidate_id"], old_receipt)
        self.remote[41] = {"id": 41, "draft": True, "tag_name": old["draft_tag"],
                           "target_commitish": old["source_sha"], "body": "inspection evidence"}
        self.upload_download()
        self.assertEqual(self.api.value["candidates"][old["candidate_id"]]["state"], "ready")
        self.ready()
        self.assertEqual(self.api.value["candidates"][old["candidate_id"]]["state"], "superseded")
        self.assertIn("UNPUBLISHABLE", self.remote[41]["name"])
        self.assertIn("inspection evidence", self.remote[41]["body"])
        self.assertTrue(self.remote[41]["draft"])

    def test_publication_reservation_is_preserved(self):
        old = rc.identity(10, "1.2.3", "b" * 40, "e" * 40, "prepare.yml", 19, 1)
        self.state.claim(old)
        self.state.bind_release(old["candidate_id"], 41)
        evidence = {"identity": old, "release_id": 41,
                    "assets": candidate_assets.asset_digests(self.dist, self.names),
                    "manifest_digest": hashlib.sha256(b"release-manifest.txt").hexdigest(),
                    "check_run_ids": [1], "attestations": ["old evidence"]}
        self.state.ready(old["candidate_id"], evidence)
        self.state.reserve(old["candidate_id"], evidence)
        self.upload_download()
        with self.assertRaisesRegex(rc.CandidateError, "reserved"):
            self.ready()
        self.assertEqual(self.record()["state"], "failed")
        self.assertEqual(self.api.value["candidates"][old["candidate_id"]]["state"], "publishing")

    def test_wrong_owner_cannot_fail_another_candidate(self):
        with patch.dict(os.environ, {"GITHUB_RUN_ATTEMPT": "2"}):
            with self.assertRaisesRegex(ValueError, "does not own"):
                self.execute("upload")
        self.assertEqual(self.record()["state"], "preparing")
        self.assertFalse(self.remote)

    def test_checkout_mismatch_records_owned_preparation_failed(self):
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "9.9.9"\n')
        with self.assertRaisesRegex(ValueError, "checkout source"):
            self.execute("upload")
        self.assertEqual(self.record()["state"], "failed")
        self.assertFalse(self.remote)

    def test_replaced_asset_id_and_partial_listing_rejected(self):
        self.execute("upload")
        original = copy.deepcopy(self.blobs)
        blob = next(iter(self.blobs[42].values()))
        blob["id"] = 999
        with self.assertRaisesRegex(ValueError, "listing changed"):
            self.helper.download(self.record(), self.downloaded)
        self.blobs = original
        self.blobs[42].pop(next(iter(self.blobs[42])))
        with self.assertRaisesRegex(ValueError, "incomplete"):
            self.execute("download", self.downloaded)
        self.assertEqual(self.record()["state"], "failed")

    def test_symlinks_and_local_extra_assets_rejected(self):
        for mode in ("extra", "symlink", "missing"):
            with self.subTest(mode=mode):
                path = self.dist / "release-manifest.txt"
                original = path.read_bytes()
                extra = self.dist / "unexpected"
                if mode == "extra":
                    extra.touch()
                elif mode == "symlink":
                    path.unlink()
                    path.symlink_to(self.root / "Cargo.toml")
                else:
                    path.unlink()
                with self.assertRaises(ValueError):
                    self.helper.upload(self.record(), self.dist)
                if path.is_symlink():
                    path.unlink()
                path.write_bytes(original)
                extra.unlink(missing_ok=True)
        self.assertFalse(self.remote)

    def test_failed_attestation_does_not_supersede_old_ready(self):
        old = rc.identity(10, "1.2.3", "b" * 40, "e" * 40, "prepare.yml", 19, 1)
        self.state.claim(old)
        self.state.bind_release(old["candidate_id"], 41)
        evidence = {"identity": old, "release_id": 41,
                    "assets": candidate_assets.asset_digests(self.dist, self.names),
                    "manifest_digest": hashlib.sha256(b"release-manifest.txt").hexdigest(),
                    "check_run_ids": [1], "attestations": ["old evidence"]}
        self.state.ready(old["candidate_id"], evidence)
        self.upload_download()
        self.fail_attestation = True
        with self.assertRaises(subprocess.CalledProcessError):
            self.ready()
        self.assertEqual(self.api.value["candidates"][old["candidate_id"]]["state"], "ready")
        self.assertFalse(any(len(call) == 4 and call[1] == "PATCH" for call in self.calls))

    def test_lost_promotion_response_does_not_demote_ready_candidate(self):
        self.upload_download()
        original = self.state.ready

        def lost_response(*args):
            original(*args)
            raise OSError("response lost after commit")

        with patch.object(self.state, "ready", side_effect=lost_response):
            with self.assertRaisesRegex(OSError, "response lost"):
                self.ready()
        self.assertEqual(self.record()["state"], "ready")
        self.assertIsNotNone(self.record()["receipt"])
        self.ready()
        self.assertEqual(self.record()["state"], "ready")

    def old_ready(self):
        old = rc.identity(10, "1.2.3", "b" * 40, "e" * 40, "prepare.yml", 19, 1)
        self.state.claim(old)
        self.state.bind_release(old["candidate_id"], 41)
        evidence = {"identity": old, "release_id": 41,
                    "assets": candidate_assets.asset_digests(self.dist, self.names),
                    "manifest_digest": hashlib.sha256(b"release-manifest.txt").hexdigest(),
                    "check_run_ids": [1], "attestations": ["old evidence"]}
        self.state.ready(old["candidate_id"], evidence)
        self.remote[41] = {"id": 41, "draft": True, "tag_name": old["draft_tag"],
                           "target_commitish": old["source_sha"], "body": "inspection evidence"}
        return old, evidence

    def test_workflow_finalizer_fails_owned_claim_without_checkout_or_assets(self):
        (self.root / "Cargo.toml").unlink()
        for failure in ("preflight", "build", "package", "archive-smoke", "signing", "attestation"):
            with self.subTest(failure=failure):
                self.api.value["candidates"][self.ident["candidate_id"]]["state"] = "preparing"
                self.helper.execute("fail", self.ident["candidate_id"])
                self.assertEqual(self.record()["state"], "failed")
                self.helper.execute("fail", self.ident["candidate_id"])
                self.assertEqual(self.record()["state"], "failed")
        self.assertFalse(self.remote)

    def test_failure_finalizer_never_demotes_ready_reserved_or_published(self):
        self.upload_download()
        self.ready()
        for status in ("ready", "publishing", "published", "superseded"):
            with self.subTest(status=status):
                self.api.value["candidates"][self.ident["candidate_id"]]["state"] = status
                before = copy.deepcopy(self.api.value)
                self.helper.execute("fail", self.ident["candidate_id"])
                self.assertEqual(self.api.value, before)

    def test_failure_finalizer_rejects_wrong_run_attempt_repository_and_ref(self):
        for field, bad in (("GITHUB_RUN_ID", "21"), ("GITHUB_RUN_ATTEMPT", "2"),
                           ("GITHUB_REF", "refs/heads/other")):
            with self.subTest(field=field), patch.dict(os.environ, {field: bad}):
                with self.assertRaisesRegex(ValueError, "does not own"):
                    self.helper.execute("fail", self.ident["candidate_id"])
                self.assertEqual(self.record()["state"], "preparing")
        with patch.object(prep, "github", return_value={"id": 99}):
            with self.assertRaisesRegex(ValueError, "repository identity"):
                self.helper.execute("fail", self.ident["candidate_id"])
        self.assertEqual(self.record()["state"], "preparing")

    def test_failure_finalizer_promotion_race_leaves_ready(self):
        self.upload_download()
        snapshot = self.helper.snapshot(self.record())
        receipt = {"identity": self.ident, "release_id": 42, "assets": snapshot["assets"],
                   "manifest_digest": snapshot["assets"]["release-manifest.txt"]["sha256"],
                   "check_run_ids": [123], "attestations": ["verified"], "supersedes": None}

        def race(candidate_id):
            self.state.ready(candidate_id, receipt)
            raise rc.CandidateError("candidate no longer preparing")

        with patch.object(self.state, "fail", side_effect=race):
            self.helper.execute("fail", self.ident["candidate_id"])
        self.assertEqual(self.record()["state"], "ready")

    def test_label_failure_after_promotion_is_retryable_and_retains_exact_receipt(self):
        old, _ = self.old_ready()
        self.upload_download()
        bundle = self.bundle()
        original = self.github

        def fail_label(path, method="GET", data=None, paginate=False):
            if method == "PATCH":
                raise OSError("label endpoint unavailable")
            return original(path, method, data, paginate)

        with patch.object(prep, "github", side_effect=fail_label):
            with self.assertRaisesRegex(OSError, "label endpoint"):
                self.ready(bundle)
        self.assertEqual(self.record()["state"], "ready")
        receipt = copy.deepcopy(self.record()["receipt"])
        self.assertEqual(receipt["supersedes"], old["candidate_id"])
        self.assertEqual(json.loads((self.root / ".candidate-receipt.json").read_text()), receipt)
        self.ready(bundle)
        self.ready(bundle)
        self.assertEqual(self.record()["receipt"], receipt)
        self.assertEqual(self.remote[41]["body"].count("This candidate is superseded"), 1)
        writes = [c for c in self.calls if len(c) == 4 and c[1] == "PATCH"]
        self.assertEqual(len(writes), 1)

    def test_label_response_loss_is_idempotent(self):
        self.old_ready()
        self.upload_download()
        original = self.github

        def lost_response(path, method="GET", data=None, paginate=False):
            value = original(path, method, data, paginate)
            if method == "PATCH":
                raise OSError("label response lost")
            return value

        with patch.object(prep, "github", side_effect=lost_response):
            with self.assertRaisesRegex(OSError, "response lost"):
                self.ready()
        self.ready()
        self.assertEqual(self.remote[41]["body"].count("This candidate is superseded"), 1)
        self.assertEqual(len([c for c in self.calls if len(c) == 4 and c[1] == "PATCH"]), 1)

    def test_historical_missing_draft_is_not_read_or_relabelled(self):
        old, evidence = self.old_ready()
        historical = rc.identity(10, "1.2.2", "c" * 40, "d" * 40, "prepare.yml", 18, 1)
        self.api.value["candidates"][historical["candidate_id"]] = {
            "identity": historical, "state": "superseded", "release_id": 40,
            "receipt": dict(evidence, identity=historical, release_id=40)}
        self.upload_download()
        self.ready()
        self.ready()
        self.assertEqual(self.record()["receipt"]["supersedes"], old["candidate_id"])
        self.assertFalse(any("/releases/40" in str(c) for c in self.calls))

    def test_ready_retry_rejects_tampered_receipt_snapshot_and_bundle(self):
        self.upload_download()
        bundle = self.bundle()
        self.ready(bundle)
        receipt_path = self.root / ".candidate-receipt.json"
        original = receipt_path.read_bytes()
        altered = json.loads(original)
        altered["check_run_ids"] = [999]
        receipt_path.write_text(json.dumps(altered))
        with self.assertRaisesRegex(ValueError, "local receipt"):
            self.ready(bundle)
        receipt_path.write_bytes(original)
        snapshot_path = self.helper.snapshot_path
        original_snapshot = snapshot_path.read_bytes()
        altered_snapshot = json.loads(original_snapshot)
        altered_snapshot["assets"]["release-manifest.txt"]["sha256"] = "0" * 64
        snapshot_path.write_text(json.dumps(altered_snapshot))
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            self.ready(bundle)
        snapshot_path.write_bytes(original_snapshot)
        value = json.loads(bundle.read_text())
        value["unused-metadata"] = "different evidence"
        bundle.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "retry evidence"):
            self.ready(bundle)
        self.assertEqual(self.record()["state"], "ready")

    def test_ready_retry_does_not_replace_original_gate_evidence(self):
        self.upload_download()
        bundle = self.bundle()
        self.ready(bundle)
        original = copy.deepcopy(self.record()["receipt"])

        def newer_success(sha, **kwargs):
            kwargs["check_run_ids"][:] = [789, 790]

        with patch.object(self.helper, "verify_payload"), patch.object(prep.policy, "verify_gates", side_effect=newer_success):
            self.execute("ready", self.downloaded, bundle)
        self.assertEqual(self.record()["receipt"], original)

    def test_ready_retry_can_restore_missing_local_receipt_from_verified_state(self):
        self.upload_download()
        self.ready()
        original = copy.deepcopy(self.record()["receipt"])
        (self.root / ".candidate-receipt.json").unlink()
        self.ready()
        self.assertEqual(json.loads((self.root / ".candidate-receipt.json").read_text()), original)

    def test_reused_ready_claim_reconciles_after_exhausted_label_attempts(self):
        old, _ = self.old_ready()
        self.upload_download()
        bundle = self.bundle()
        original = self.github

        def unavailable(path, method="GET", data=None, paginate=False):
            if method == "PATCH":
                raise OSError("label unavailable")
            return original(path, method, data, paginate)

        with patch.object(prep, "github", side_effect=unavailable):
            for _ in range(3):
                with self.assertRaisesRegex(OSError, "label unavailable"):
                    self.ready(bundle)
        before, revision = self.api.read()
        blobs = copy.deepcopy(self.blobs)
        # No private evidence or source checkout from the first run is available.
        self.helper.snapshot_path.unlink()
        (self.root / ".candidate-receipt.json").unlink()
        (self.root / "Cargo.toml").unlink()
        duplicate = dict(self.ident, run_id=22, candidate_id="a" * 40 + "-22-1",
                         draft_tag="candidate-" + "a" * 40 + "-22-1")
        reused = self.state.claim(duplicate)
        self.assertEqual(reused["state"], "ready")
        self.assertEqual(reused["identity"], self.ident)
        with patch.dict(os.environ, {"GITHUB_RUN_ID": "22", "GITHUB_RUN_ATTEMPT": "4"}), \
                patch.object(self.helper, "verify_checkout", side_effect=AssertionError("no source checkout")), \
                patch.object(self.state, "ready", side_effect=AssertionError("no promotion")):
            self.helper.execute("reconcile", reused["identity"]["candidate_id"])
            self.helper.execute("reconcile", reused["identity"]["candidate_id"])
        self.assertEqual(self.api.read(), (before, revision))
        self.assertEqual(self.blobs, blobs)
        self.assertFalse(self.helper.snapshot_path.exists())
        patches = [call for call in self.calls if len(call) == 4 and call[1] == "PATCH"]
        self.assertEqual(len(patches), 1)
        self.assertEqual(patches[0][0], "repos/owner/repo/releases/41")
        self.assertEqual(set(patches[0][2]), {"name", "body", "draft", "make_latest"})
        self.assertTrue(patches[0][2]["draft"])
        self.assertEqual(self.record()["receipt"]["supersedes"], old["candidate_id"])

    def test_reconcile_rejects_nonready_states_without_mutation(self):
        self.upload_download()
        self.ready()
        for status in ("preparing", "failed", "superseded", "publishing", "published"):
            with self.subTest(status=status):
                record = self.record()
                record["state"] = status
                if status in ("preparing", "failed"):
                    record["receipt"] = None
                before = self.api.read()
                with self.assertRaises(ValueError):
                    self.helper.execute("reconcile", self.ident["candidate_id"])
                self.assertEqual(self.api.read(), before)
                # Restore a valid receipt for the following lifecycle fixtures.
                record["receipt"] = json.loads((self.root / ".candidate-receipt.json").read_text())

    def test_reconcile_rejects_wrong_repository_ref_and_ready_draft_identity(self):
        self.old_ready()
        self.upload_download()
        self.ready()
        before = self.api.read()
        with patch.dict(os.environ, {"GITHUB_REF": "refs/heads/untrusted"}):
            with self.assertRaisesRegex(ValueError, "main"):
                self.helper.execute("reconcile", self.ident["candidate_id"])
        with patch.object(prep, "github", return_value={"id": 99}):
            with self.assertRaisesRegex(ValueError, "repository identity"):
                self.helper.execute("reconcile", self.ident["candidate_id"])
        for key, value in (("id", 99), ("draft", False), ("target_commitish", "0" * 40), ("tag_name", "v1.2.3")):
            with self.subTest(key=key):
                original = self.remote[42][key]
                self.remote[42][key] = value
                with self.assertRaisesRegex(ValueError, "draft release identity"):
                    self.helper.execute("reconcile", self.ident["candidate_id"])
                self.remote[42][key] = original
        self.assertEqual(self.api.read(), before)

    def test_reconcile_rejects_tampered_identity_and_incomplete_receipt(self):
        self.upload_download()
        self.ready()
        original = copy.deepcopy(self.record())
        for mode in ("identity", "release-id", "manifest", "inventory", "predecessor"):
            with self.subTest(mode=mode):
                record = self.record()
                if mode == "identity":
                    record["receipt"]["identity"]["source_sha"] = "0" * 40
                elif mode == "release-id":
                    record["receipt"]["release_id"] = 99
                elif mode == "manifest":
                    record["receipt"]["manifest_digest"] = "0" * 64
                elif mode == "inventory":
                    record["receipt"]["assets"]["unrelated"] = record["receipt"]["assets"].pop("packaging-provenance.json")
                else:
                    record["receipt"].pop("supersedes")
                before = self.api.read()
                with self.assertRaises(ValueError):
                    self.helper.execute("reconcile", self.ident["candidate_id"])
                self.assertEqual(self.api.read(), before)
                self.api.value["candidates"][self.ident["candidate_id"]] = copy.deepcopy(original)

    def test_reconcile_errors_preserve_ready_and_can_be_retried(self):
        self.old_ready()
        self.upload_download()
        with patch.object(self.helper, "label_predecessor", side_effect=OSError("label unavailable")):
            with self.assertRaises(OSError):
                self.ready()
        before = self.api.read()
        with patch.object(self.helper, "label_predecessor", side_effect=OSError("still unavailable")):
            with self.assertRaises(OSError):
                self.helper.execute("reconcile", self.ident["candidate_id"])
        self.assertEqual(self.api.read(), before)
        self.helper.execute("reconcile", self.ident["candidate_id"])
        self.assertEqual(self.api.read(), before)

    def test_duplicate_ready_reconciliation_workflow_contract(self):
        workflow = (prep.ROOT / ".github/workflows/release-eligibility.yml").read_text()
        job = workflow.split("  reconcile:\n", 1)[1].split("  prepare:\n", 1)[0]
        self.assertIn("needs: [eligible]", job)
        self.assertIn("if: ${{ needs.eligible.outputs.candidate_state == 'ready' }}", job)
        self.assertIn("ref: main", job)
        self.assertIn("persist-credentials: false", job)
        self.assertIn("CANDIDATE_ID: ${{ needs.eligible.outputs.candidate_id }}", job)
        self.assertIn('prepare_candidate.py reconcile --candidate-id "$CANDIDATE_ID"', job)
        for forbidden in ("--directory", "--attestation-path", "environment:", "secrets.", "id-token:", "attestations:"):
            self.assertNotIn(forbidden, job)

    def test_workflow_failure_and_evidence_contract(self):
        workflow = (prep.ROOT / ".github/workflows/release.yml").read_text()
        finalizer = workflow.split("  preparation-failure:\n", 1)[1]
        self.assertIn("needs: [validate-input, release-preflight, build, package, archive-smoke, stage-and-sign]", finalizer)
        jobs = set(re.findall(r"^  ([a-z][a-z-]+):\n", workflow.split("\njobs:\n", 1)[1], re.M))
        dependencies = re.search(r"needs: \[([^\]]+)\]", finalizer).group(1).split(", ")
        self.assertEqual(set(dependencies), jobs - {"preparation-failure"})
        self.assertIn("always()", finalizer)
        for status in ("failure", "cancelled", "skipped"):
            self.assertIn(f"contains(needs.*.result, '{status}')", finalizer)
        self.assertIn('prepare_candidate.py fail --candidate-id "$CANDIDATE_ID"', finalizer)
        self.assertIn("ref: ${{ github.workflow_sha }}", finalizer)
        self.assertNotIn("environment:", finalizer)
        self.assertNotIn("secrets.", finalizer)
        evidence = workflow.split("      - name: Retain candidate receipt", 1)[1].split("  preparation-failure:", 1)[0]
        self.assertIn("if: ${{ always() }}", evidence)
        self.assertIn("include-hidden-files: true", evidence)
        self.assertIn("if-no-files-found: warn", evidence)

    def test_workflow_ready_retries_are_bounded_and_preserve_failure(self):
        workflow = (prep.ROOT / ".github/workflows/release.yml").read_text()
        ready_step = workflow.split("      - name: Verify attestation evidence and atomically mark candidate ready\n", 1)[1]
        script = ready_step.split("        run: |\n", 1)[1].split("      - name:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        bundle = self.root / "bundle"
        bundle.write_text("test bundle")
        for success_after in (2, 99):
            with self.subTest(success_after=success_after):
                calls = self.root / "ready-calls"
                calls.unlink(missing_ok=True)
                function = ('count=0; python3() { count=$((count + 1)); '
                            'printf "%s\\n" "$count" >> "$CALL_LOG"; '
                            'test "$count" -ge "$SUCCESS_AFTER"; };\n')
                result = RUN_COMMAND(["bash", "-c", function + script], capture_output=True, text=True,
                                     env=dict(os.environ, ATTESTATION_PATH=str(bundle), CANDIDATE_ID=self.ident["candidate_id"],
                                              CALL_LOG=str(calls), SUCCESS_AFTER=str(success_after)))
                self.assertEqual(result.returncode, 0 if success_after == 2 else 1, result.stderr)
                self.assertEqual(len(calls.read_text().splitlines()), min(success_after, 3))

    def test_api_transport_sends_json_booleans_and_paginated_reads(self):
        # Exercise the production transport, separately from the remote fake.
        with patch.object(subprocess, "check_output", return_value="{}") as call:
            GITHUB_TRANSPORT("repos/owner/repo/releases", "POST", {"draft": True, "make_latest": "false"})
            command = call.call_args.args[0]
            self.assertIn("--input", command)
            self.assertIn("POST", command)
            self.assertIs(json.loads(call.call_args.kwargs["input"])["draft"], True)
            GITHUB_TRANSPORT("repos/owner/repo/releases/42", "PATCH", {
                "draft": False, "prerelease": False, "make_latest": "true"})
            self.assertIn("PATCH", call.call_args.args[0])
            self.assertNotIn("-f", call.call_args.args[0])
            payload = json.loads(call.call_args.kwargs["input"])
            self.assertIs(payload["draft"], False)
            self.assertIs(payload["prerelease"], False)
            self.assertEqual(payload["make_latest"], "true")
            GITHUB_TRANSPORT("repos/owner/repo/releases/42/assets?per_page=100", paginate=True)
            self.assertEqual(call.call_args.args[0][-2:], ["--paginate", "--slurp"])


class RealPayloadTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.temp.name)
        cls.dist = cls.root / "downloaded"
        cls.dist.mkdir()
        (cls.root / "scripts").symlink_to(prep.ROOT / "scripts", target_is_directory=True)
        version = rc.workspace_version(prep.ROOT / "Cargo.toml")
        cls.ident = rc.identity(10, version, "a" * 40, "f" * 40, "prepare.yml", 20, 1)
        cls.record = {"identity": cls.ident, "release_id": 42, "state": "preparing", "receipt": None}
        cls.helper = prep.Preparation("owner/repo", 10, rc.Candidates(MemoryState(), 10), cls.root)
        cls.secret = cls.root / "ephemeral.key"
        cls.key = cls.root / "transist-release.minisign.pub"
        cls.command(["minisign", "-G", "-W", "-p", str(cls.key), "-s", str(cls.secret)])
        (cls.dist / cls.key.name).write_bytes(cls.key.read_bytes())
        packaging = {"kind": "final-packaging", "source": {"commit_sha": "a" * 40, "tree_sha": "f" * 40},
                     "packaging": {"container_image": "synthetic-fixture"}}
        (cls.dist / "packaging-provenance.json").write_text(json.dumps(packaging))
        targets = [t["target_triple"] for t in json.loads(prep.INVENTORY.read_text())["released_archives"]]
        for target in targets:
            package = f"transist-{cls.ident['final_tag']}-{target}"
            names = expected_members(str(prep.INVENTORY), str(prep.ROOT), package, target)
            archive_path = cls.dist / (package + ".tar.gz")
            binaries = {}
            with tarfile.open(archive_path, "w:gz") as archive:
                for name in names:
                    info = tarfile.TarInfo(name)
                    info.mode = 0o755 if any(other.startswith(name + "/") for other in names) else 0o644
                    if any(other.startswith(name + "/") for other in names):
                        info.type = tarfile.DIRTYPE
                        archive.addfile(info)
                    else:
                        data = json.dumps(packaging).encode() if name.endswith("/packaging-provenance.json") else b"synthetic fixture\n"
                        if "/bin/" in name:
                            binaries[name.rsplit("/", 1)[1]] = hashlib.sha256(data).hexdigest()
                        info.size = len(data)
                        archive.addfile(info, io.BytesIO(data))
            digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            Path(str(archive_path) + ".sha256").write_text(f"{digest}  {archive_path.name}\n")
            cls.sign(archive_path)
            provenance = {"kind": "native-build", "target": target, "source": packaging["source"],
                          "binaries": [{"name": n, "sha256": d} for n, d in binaries.items()]}
            (cls.dist / f"build-provenance-{target}.json").write_text(json.dumps(provenance))
        with tarfile.open(cls.dist / f"transist-{cls.ident['final_tag']}-sbom.tar.gz", "w:gz") as archive:
            data = json.dumps({"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
                               "components": [{"type": "application", "name": "transist", "version": cls.ident["version"]}]}).encode()
            info = tarfile.TarInfo("fixture.cdx.json")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
        cls.manifest()

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    @staticmethod
    def command(command):
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        if result.returncode:
            raise AssertionError(f"{command}: {result.stdout}")

    @classmethod
    def sign(cls, path):
        cls.command(["minisign", "-S", "-q", "-s", str(cls.secret), "-m", str(path), "-x", str(path) + ".minisig"])

    @classmethod
    def manifest(cls):
        cls.command(["bash", str(prep.ROOT / "scripts/ci/release_manifest.sh"), "generate", cls.ident["final_tag"], str(cls.dist)])
        cls.sign(cls.dist / "release-manifest.txt")

    def verify(self):
        # Capture subprocess output in the test runner's per-check log.
        self.helper.verify_payload(self.record, self.dist)

    def test_real_signed_six_archive_fixture_passes_existing_verifier(self):
        self.verify()

    def test_real_wrong_packaging_provenance_fails_even_when_resigned(self):
        path = self.dist / "packaging-provenance.json"
        original = path.read_bytes()
        try:
            value = json.loads(original)
            value["source"]["tree_sha"] = "0" * 40
            path.write_text(json.dumps(value))
            self.manifest()
            with self.assertRaises(subprocess.CalledProcessError):
                self.verify()
        finally:
            path.write_bytes(original)
            self.manifest()

    def test_real_wrong_native_binary_digest_fails(self):
        path = next(self.dist.glob("build-provenance-*.json"))
        original = path.read_bytes()
        try:
            value = json.loads(original)
            value["binaries"][0]["sha256"] = "0" * 64
            path.write_text(json.dumps(value))
            self.manifest()
            with self.assertRaises(subprocess.CalledProcessError):
                self.verify()
        finally:
            path.write_bytes(original)
            self.manifest()

    def test_real_mismatched_key_and_invalid_signature_rejected(self):
        path = self.dist / self.key.name
        original = path.read_bytes()
        path.write_bytes(b"different key")
        try:
            with self.assertRaisesRegex(ValueError, "committed key"):
                self.verify()
        finally:
            path.write_bytes(original)
        path = self.dist / "release-manifest.txt.minisig"
        original = path.read_bytes()
        path.write_text("invalid signature")
        try:
            with self.assertRaises(subprocess.CalledProcessError):
                self.verify()
        finally:
            path.write_bytes(original)


if __name__ == "__main__":
    unittest.main()
