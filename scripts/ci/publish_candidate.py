#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Publish only a reserved, approved candidate; never produce or replace assets."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any

import candidate_assets as assets
import prepare_candidate as prep
import release_candidates as rc
import verify_candidate_policy as policy
import verify_release_refs as refs


class Publication(prep.Preparation):
    """Production publication/recovery using immutable receipts and bounded state CAS."""

    def record(self, candidate_id: str, *allowed: str) -> dict[str, Any]:
        """Read a repository-bound candidate from a main-only invocation."""
        if (os.environ.get("GITHUB_REF") != "refs/heads/main"
                or not re.fullmatch(r"[0-9a-f]{40}-[1-9][0-9]*-[1-9][0-9]*", candidate_id)):
            raise ValueError("publication/recovery requires main and an exact candidate ID")
        value, _ = self.state.api.read()
        self.state._validate_state(value)
        if prep.github(f"repos/{self.repository}")["id"] != self.repository_id:
            raise ValueError("repository identity mismatch")
        return self.state._record(value, candidate_id, *(allowed or ("ready", "publishing", "published")))

    def release(self, record: dict[str, Any]) -> dict[str, Any]:
        """Accept the exact private candidate/final draft or matching public final release."""
        ident, rid = record["identity"], record["release_id"]
        value = prep.github(f"{self.endpoint}/{rid}")
        if (value.get("id") != rid or value.get("target_commitish") != ident["source_sha"]
                or type(value.get("draft")) is not bool or value.get("prerelease") is not False
                or value.get("tag_name") not in (ident["draft_tag"], ident["final_tag"])
                or (value["draft"] is False and (value["tag_name"] != ident["final_tag"]
                    or record["state"] not in ("publishing", "published")))
                or (record["state"] == "published" and value["draft"] is not False)
                or (record["state"] == "ready" and value["tag_name"] != ident["draft_tag"])):
            raise ValueError("publication release identity mismatch")
        return value

    def summary(self, candidate_id: str) -> None:
        """Write review evidence before the environment-protected job is scheduled."""
        record = self.record(candidate_id)
        value = self.release(record)
        ident, receipt = record["identity"], record["receipt"]
        url = value.get("html_url", "")
        if not re.fullmatch(r"https://github\.com/" + re.escape(self.repository) + r"/releases/[A-Za-z0-9_./+-]+", url):
            raise ValueError("invalid candidate draft URL")
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as stream:
            stream.write(f"## Release publication review\n\nCandidate: `{candidate_id}`\n\n"
                         f"Source: `{ident['source_sha']}`\n\nTree: `{ident['tree_sha']}`\n\n"
                         f"Version: `{ident['version']}`\n\n"
                         f"[Download candidate]({url})\n\n"
                         f"Manifest SHA-256: `{receipt['manifest_digest']}`\n\n"
                         f"Preparation: verified 29 assets, signatures, source provenance, seven archive attestations, "
                         f"six native archive smoke jobs and aggregate readiness.\n\n"
                         f"[Preparation run](https://github.com/{self.repository}/actions/runs/{ident['run_id']}/attempts/{ident['attempt']})\n\n"
                         f"Recorded gate check IDs: `{receipt['check_run_ids']}`. State: `{record['state']}`.\n")

    def revalidate(self, record: dict[str, Any]) -> None:
        """Recheck receipt and mutable eligibility before new public effects."""
        self.authenticate_receipt(record)
        ident = record["identity"]
        refs.verify_main(ident["source_sha"])
        subprocess.run(["bash", str(self.root / "scripts/ci/check_release_policy.sh"),
                        "--candidate", ident["source_sha"]], check=True, cwd=self.root)
        policy.verify_gates(ident["source_sha"], require_push=True)

    def authenticate_receipt(self, record: dict[str, Any]) -> None:
        """Bind immutable receipt evidence to exact source and preparation workflow.

        This authentication also applies to public recovery, independently of
        later changes to gates, main ancestry or publication policy.
        """
        self.verify_checkout(record)
        ident = record["identity"]
        if f"## [{ident['version']}]" not in (self.root / "CHANGELOG.md").read_text():
            raise ValueError("candidate changelog version missing")
        if set(record["receipt"]["assets"]) != assets.expected_assets(ident["final_tag"], prep.INVENTORY):
            raise ValueError("receipt asset inventory mismatch")
        workflow_sha = self.workflow_source(record)
        evidence = record["receipt"]["attestations"]
        expected = {n: {"sha256": m["sha256"]} for n, m in record["receipt"]["assets"].items()
                    if n.endswith(".tar.gz")}
        if len(evidence) != 1:
            raise ValueError("ambiguous attestation evidence")
        value = json.loads(evidence[0])
        if (value.get("subjects") != expected or value.get("source_sha") != ident["source_sha"]
                or value.get("workflow_sha") != workflow_sha
                or not re.fullmatch(r"[0-9a-f]{64}", value.get("bundle_sha256", ""))):
            raise ValueError("receipt attestation identity mismatch")

    def download_verified(self, record: dict[str, Any], directory: Path) -> dict[str, Any]:
        """Download immutable assets by numeric ID and authenticate their recorded bytes."""
        self.release(record)
        expected = record["receipt"]["assets"]
        listing = assets.validate_listing(self.listing(record["release_id"]), set(expected))
        for name, metadata in listing.items():
            with (directory / name).open("xb") as output:
                subprocess.run(["gh", "api", f"{self.endpoint}/assets/{metadata['id']}",
                                "-H", "Accept: application/octet-stream"], stdout=output, check=True)
        assets.verify_downloads(directory, listing, expected)
        self.verify_payload(record, directory)
        evidence = json.loads(record["receipt"]["attestations"][0])
        for name in sorted(evidence["subjects"]):
            subprocess.run(["gh", "attestation", "verify", str(directory / name),
                            "--repo", self.repository,
                            "--signer-workflow", f"{self.repository}/.github/workflows/release.yml",
                            "--source-ref", "refs/heads/main", "--source-digest", evidence["workflow_sha"],
                            "--signer-digest", evidence["workflow_sha"],
                            "--deny-self-hosted-runners"], check=True)
        self.check_assets(record, listing)
        return listing

    def check_assets(self, record: dict[str, Any], listing: dict[str, Any]) -> None:
        """Recheck release identity and asset IDs/digests before/after each effect."""
        self.release(record)
        if assets.validate_listing(self.listing(record["release_id"]), set(listing)) != listing:
            raise ValueError("publication asset listing changed")

    def final_tag(self, record: dict[str, Any], create: bool = False) -> None:
        """Create an annotated final ref once; accept only an exact annotated commit tag.

        Read absence with the matching-refs API so authentication/server failures
        cannot masquerade as a missing tag. Ref creation is never forced.
        """
        ident = record["identity"]
        root, tag = f"repos/{self.repository}", ident["final_tag"]

        def existing() -> dict[str, Any] | None:
            values = prep.github(f"{root}/git/matching-refs/tags/{tag}")
            matches = [v for v in values if v["ref"] == f"refs/tags/{tag}"]
            if len(matches) > 1:
                raise ValueError("duplicate final tag")
            return matches[0] if matches else None

        reference = existing()
        if reference is None and create:
            obj = prep.github(f"{root}/git/tags", "POST", {
                "tag": tag, "message": f"Transist {tag}\nCandidate {ident['candidate_id']}\n"
                f"Manifest SHA-256 {record['receipt']['manifest_digest']}\n",
                "object": ident["source_sha"], "type": "commit"})
            try:
                prep.github(f"{root}/git/refs", "POST", {"ref": f"refs/tags/{tag}", "sha": obj["sha"]})
            except subprocess.CalledProcessError:
                # A competing same-candidate publisher or lost response may
                # have created it. Validate actual state; never move the ref.
                if existing() is None:
                    raise
            reference = existing()
        if reference is None or reference["object"]["type"] != "tag":
            raise ValueError("final tag is missing or is not annotated")
        obj = prep.github(f"{root}/git/tags/{reference['object']['sha']}")
        if (obj["sha"] != reference["object"]["sha"] or obj["tag"] != tag
                or obj["object"]["type"] != "commit" or obj["object"]["sha"] != ident["source_sha"]):
            raise ValueError("annotated final tag target mismatch")

    def publish(self, candidate_id: str) -> None:
        """Publish after fresh approval, reserving before any tag/release side effects.

        Failures retain the publication reservation. Retrying an already public
        release still authenticates all bytes and the exact final annotated tag,
        but later mutable eligibility cannot block recording an existing effect.
        """
        record = self.record(candidate_id)
        was_draft = self.release(record)["draft"]
        if was_draft:
            self.revalidate(record)
            self.state.reserve(candidate_id, record["receipt"])
        else:
            self.authenticate_receipt(record)
        record = self.record(candidate_id, "publishing", "published")
        with tempfile.TemporaryDirectory(prefix="candidate-publication-") as temp:
            listing = self.download_verified(record, Path(temp))
            release = self.release(record)
            did_publish = release["draft"]
            if did_publish and not was_draft:
                raise ValueError("public release became a draft during recovery")
            if did_publish:
                self.final_tag(record, create=True)
                self.check_assets(record, listing)
                if release["tag_name"] != record["identity"]["final_tag"]:
                    prep.github(f"{self.endpoint}/{record['release_id']}", "PATCH", {
                        "tag_name": record["identity"]["final_tag"], "target_commitish": record["identity"]["source_sha"],
                        "prerelease": False})
                self.check_assets(record, listing)
                if self.release(record)["tag_name"] != record["identity"]["final_tag"]:
                    raise ValueError("draft final tag update did not persist")
                self.final_tag(record)
                # Recheck checks/policies immediately before the irreversible effect.
                self.revalidate(record)
                prep.github(f"{self.endpoint}/{record['release_id']}", "PATCH", {
                    "draft": False, "prerelease": False, "make_latest": "true"})
            self.final_tag(record)
            self.check_assets(record, listing)
            if self.release(record)["draft"] is not False:
                raise ValueError("release did not become public")
            if did_publish and prep.github(f"{self.endpoint}/latest").get("id") != record["release_id"]:
                raise ValueError("published release is not latest")
            self.state.published(candidate_id, record["receipt"])


def main() -> None:
    """Run summary or approved publication using this job's repository credentials."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("summary", "publish"))
    parser.add_argument("--candidate-id", required=True)
    parser.add_argument("--source-directory", type=Path, default=prep.ROOT)
    args = parser.parse_args()
    repo, rid = os.environ["GITHUB_REPOSITORY"], int(os.environ["GITHUB_REPOSITORY_ID"])
    state = rc.Candidates(rc.GitHubStateAPI(repo, rid, os.environ["GH_TOKEN"]), rid)
    helper = Publication(repo, rid, state, args.source_directory.resolve())
    getattr(helper, args.command)(args.candidate_id)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"FAIL  candidate publication: {exc}", file=sys.stderr)
        sys.exit(1)
