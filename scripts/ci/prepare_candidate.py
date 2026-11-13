#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Prepare immutable draft assets and promote only independently verified candidates.

Operational snapshots and receipts live in the checkout root, never in dist.
No command creates a final tag or publishes a release. All remote writes use
numeric release IDs and lifecycle writes use the existing bounded CAS protocol.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
from typing import Any
from urllib.parse import quote

import candidate_assets as assets
import release_candidates as rc
import verify_candidate_policy as policy

ROOT = Path(__file__).resolve().parents[2]
INVENTORY = ROOT / "scripts/ci/release-archive-inventory.json"


def github(path: str, method: str = "GET", data: dict[str, Any] | None = None,
           paginate: bool = False) -> Any:
    """Call the authenticated gh API; propagate all API and decoding failures."""
    command = ["gh", "api", path, "--method", method]
    if paginate:
        command += ["--paginate", "--slurp"]
    if data is not None:
        command += ["--input", "-"]
    return json.loads(subprocess.check_output(
        command, input=None if data is None else json.dumps(data), text=True))


def write_evidence(path: Path, value: dict[str, Any]) -> None:
    """Create private evidence once; never overwrite a previous snapshot/receipt."""
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write("\n")


class Preparation:
    """Repository-bound production preparation commands with conservative failures."""

    def __init__(self, repository: str, repository_id: int, state: rc.Candidates,
                 root: Path = ROOT) -> None:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
            raise ValueError("invalid repository")
        self.repository, self.repository_id, self.state, self.root = repository, repository_id, state, root
        self.endpoint = f"repos/{repository}/releases"
        self.snapshot_path = root / ".candidate-assets.json"

    def record(self, candidate_id: str, *allowed: str) -> dict[str, Any]:
        """Read and validate authoritative state, repository and preparation ownership."""
        value, _ = self.state.api.read()
        self.state._validate_state(value)
        record = value["candidates"].get(candidate_id)
        if record is None or record["state"] not in (allowed or ("preparing",)):
            raise ValueError("candidate does not permit this preparation command")
        ident = record["identity"]
        if github(f"repos/{self.repository}")["id"] != self.repository_id:
            raise ValueError("repository identity mismatch")
        if (str(ident["run_id"]) != os.environ["GITHUB_RUN_ID"]
                or str(ident["attempt"]) != os.environ["GITHUB_RUN_ATTEMPT"]
                or os.environ["GITHUB_REF"] != "refs/heads/main"):
            raise ValueError("preparation run does not own candidate")
        return record

    def fail(self, candidate_id: str) -> None:
        """Fail only this run/attempt's preparing claim; leave all terminal/reserved states intact.

        Used by the always-run workflow finalizer, including before a draft or
        checkout exists. A concurrent promotion is reread rather than demoted.
        """
        allowed = ("preparing", "failed", "ready", "superseded", "publishing", "published")
        record = self.record(candidate_id, *allowed)
        if record["state"] == "preparing":
            try:
                self.state.fail(candidate_id)
            except rc.CandidateError:
                if self.record(candidate_id, *allowed)["state"] == "preparing":
                    raise

    def verify_checkout(self, record: dict[str, Any]) -> None:
        """Require the checked-out source/tree/version to match the owned claim."""
        ident = record["identity"]
        sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
        tree = subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=self.root, text=True).strip()
        if (sha != ident["source_sha"] or tree != ident["tree_sha"]
                or rc.workspace_version(self.root / "Cargo.toml") != ident["version"]):
            raise ValueError("checkout source, tree or version mismatch")

    def release(self, record: dict[str, Any]) -> dict[str, Any]:
        """Require a private draft at the bound release ID and exact source SHA."""
        rid, ident = record["release_id"], record["identity"]
        if type(rid) is not int or rid <= 0:
            raise ValueError("missing bound draft release ID")
        value = github(f"{self.endpoint}/{rid}")
        if (value.get("id") != rid or value.get("draft") is not True
                or value.get("tag_name") != ident["draft_tag"]
                or value.get("target_commitish") != ident["source_sha"]):
            raise ValueError("draft release identity mismatch")
        return value

    def listing(self, release_id: int) -> list[list[dict[str, Any]]]:
        """Read every release asset page, without interpreting filenames."""
        return github(f"{self.endpoint}/{release_id}/assets?per_page=100", paginate=True)

    def snapshot(self, record: dict[str, Any]) -> dict[str, Any]:
        """Load private upload evidence and reject identity or inventory tampering."""
        if self.snapshot_path.is_symlink():
            raise ValueError("snapshot cannot be a symlink")
        value = json.loads(self.snapshot_path.read_text())
        if (set(value) != {"identity", "release_id", "assets", "listing"}
                or value["identity"] != record["identity"] or value["release_id"] != record["release_id"]):
            raise ValueError("snapshot identity mismatch")
        names = assets.expected_assets(record["identity"]["final_tag"], INVENTORY)
        if set(value["assets"]) != names:
            raise ValueError("snapshot inventory mismatch")
        assets.validate_listing([list(value["listing"].values())], names)
        if any(name != metadata["name"] for name, metadata in value["listing"].items()):
            raise ValueError("snapshot listing keys mismatch")
        for metadata in value["assets"].values():
            if (set(metadata) != {"sha256", "size"}
                    or not re.fullmatch(r"[0-9a-f]{64}", metadata["sha256"])
                    or type(metadata["size"]) is not int or metadata["size"] < 0):
                raise ValueError("invalid snapshot digest")
        return value

    def check_snapshot(self, record: dict[str, Any], snapshot: dict[str, Any]) -> dict[str, Any]:
        """Revalidate private draft identity and immutable uploaded asset IDs/metadata."""
        self.release(record)
        listing = assets.validate_listing(self.listing(record["release_id"]), set(snapshot["assets"]))
        if listing != snapshot["listing"]:
            raise ValueError("uploaded draft asset listing changed")
        return listing

    def upload(self, record: dict[str, Any], directory: Path) -> None:
        """Create/bind a draft once and upload the exact 29 assets without overwrites."""
        ident = record["identity"]
        names = assets.expected_assets(ident["final_tag"], INVENTORY)
        digests = assets.asset_digests(directory, names)
        if self.snapshot_path.exists() or self.snapshot_path.is_symlink():
            raise ValueError("upload snapshot already exists")
        if record["release_id"] is None:
            # An orphan draft from an interrupted create is not silently adopted.
            pages = github(f"{self.endpoint}?per_page=100", paginate=True)
            if any(r["tag_name"] == ident["draft_tag"] for page in pages for r in page):
                raise ValueError("candidate draft already exists without binding")
            draft = github(self.endpoint, "POST", {
                "tag_name": ident["draft_tag"], "target_commitish": ident["source_sha"],
                "name": ident["draft_title"], "draft": True, "prerelease": False,
                "make_latest": "false", "body": "Preparation in progress; unpublishable until verified."})
            rid = draft["id"]
            if type(rid) is not int or rid <= 0:
                raise ValueError("invalid created release ID")
            self.state.bind_release(ident["candidate_id"], rid)
            record = dict(record, release_id=rid)
        self.release(record)
        if self.listing(record["release_id"]) != [[]]:
            raise ValueError("refusing upload to a draft with existing assets")
        for name in sorted(names):
            # Upload via the fixed GitHub upload host, never a response-provided URL.
            subprocess.run([
                "gh", "api", f"https://uploads.github.com/{self.endpoint}/{record['release_id']}/assets?name={quote(name)}",
                "--method", "POST", "-H", "Content-Type: application/octet-stream",
                "--input", str(directory / name)], check=True, stdout=subprocess.DEVNULL)
        self.release(record)
        listing = assets.validate_listing(self.listing(record["release_id"]), names)
        if assets.asset_digests(directory, names) != digests or any(
                listing[name]["size"] != digests[name]["size"] for name in names):
            raise ValueError("upload bytes or sizes changed")
        write_evidence(self.snapshot_path, {"identity": ident, "release_id": record["release_id"],
                                          "assets": digests, "listing": listing})

    def download(self, record: dict[str, Any], directory: Path) -> None:
        """Download by release/asset ID and compare all bytes with the upload snapshot."""
        snapshot = self.snapshot(record)
        self.check_snapshot(record, snapshot)
        directory.mkdir(exist_ok=True)
        if directory.is_symlink():
            raise ValueError("download directory cannot be a symlink")
        assets.download_draft(self.repository, record["release_id"], record["identity"],
                              directory, snapshot["assets"], INVENTORY)
        self.check_snapshot(record, snapshot)

    def verify_payload(self, record: dict[str, Any], directory: Path) -> None:
        """Authenticate key, manifest, archive signatures, provenance and all archive members."""
        ident = record["identity"]
        key = self.root / "transist-release.minisign.pub"
        if (directory / key.name).read_bytes() != key.read_bytes():
            raise ValueError("downloaded verification key differs from committed key")
        signed = ["release-manifest.txt"] + sorted(
            n for n in assets.expected_assets(ident["final_tag"], INVENTORY)
            if n.endswith(".tar.gz") and not n.endswith("-sbom.tar.gz"))
        for name in signed:
            subprocess.run(["minisign", "-Vq", "-p", str(key), "-m", str(directory / name),
                            "-x", str(directory / (name + ".minisig"))], check=True)
        for operation in ("verify", "compare-draft"):
            subprocess.run(["bash", str(self.root / "scripts/ci/release_manifest.sh"),
                            operation, ident["final_tag"], str(directory)], check=True)
        # The legacy verifier extracts the SBOM with tar. Check its members
        # before handing it authenticated but still untrusted archive contents.
        with tarfile.open(directory / f"transist-{ident['final_tag']}-sbom.tar.gz", "r:gz") as sbom:
            seen = set()
            for member in sbom.getmembers():
                name = member.name.removeprefix("./").rstrip("/")
                if name == "." and member.isdir():
                    continue
                if (not name or name.startswith("/") or "\\" in name
                        or any(part in ("", ".", "..") for part in name.split("/"))
                        or not (member.isfile() or member.isdir()) or name in seen):
                    raise ValueError("unsafe SBOM archive member")
                seen.add(name)
        subprocess.run(["bash", str(self.root / "scripts/ci/verify_downloaded_draft.sh"),
                        ident["final_tag"], str(directory), "--expected-commit", ident["source_sha"],
                        "--expected-tree", ident["tree_sha"]], check=True)

    def attestations(self, record: dict[str, Any], directory: Path, path: Path,
                     digests: dict[str, Any]) -> list[str]:
        """Verify signed GitHub provenance for every exact archive digest; retain bundle evidence."""
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError("missing successful attestation bundle")
        raw = path.read_bytes()
        try:
            bundles = json.loads(raw)
        except json.JSONDecodeError:
            bundles = [json.loads(line) for line in raw.splitlines() if line.strip()]
        if not isinstance(bundles, list):
            bundles = [bundles]
        expected = {n: {"sha256": m["sha256"]} for n, m in digests.items() if n.endswith(".tar.gz")}
        subjects = {}
        for bundle in bundles:
            envelope = bundle["dsseEnvelope"]
            if not envelope["signatures"] or envelope["payloadType"] != "application/vnd.in-toto+json":
                raise ValueError("unsigned attestation")
            statement = json.loads(base64.b64decode(envelope["payload"], validate=True))
            if (statement["_type"] != "https://in-toto.io/Statement/v1"
                    or statement["predicateType"] != "https://slsa.dev/provenance/v1"):
                raise ValueError("unexpected attestation statement")
            for subject in statement["subject"]:
                name = subject["name"]
                if name in subjects:
                    raise ValueError("duplicate attestation subject")
                subjects[name] = subject["digest"]
        if subjects != expected:
            raise ValueError("attestation subjects do not bind exact archive digests")
        workflow_sha = self.workflow_source(record)
        for name in sorted(expected):
            # workflow_run's OIDC source SHA is the orchestration commit, which
            # can be newer than the checked-out candidate. Authenticate the
            # reusable signer on main; the signed manifest/provenance above
            # binds these exact subject bytes to the candidate SHA and tree.
            subprocess.run(["gh", "attestation", "verify", str(directory / name),
                            "--bundle", str(path), "--repo", self.repository,
                            "--signer-workflow", f"{self.repository}/.github/workflows/release.yml",
                             "--source-ref", "refs/heads/main",
                             "--source-digest", workflow_sha,
                             "--signer-digest", workflow_sha,
                            "--deny-self-hosted-runners"], check=True)
        if path.read_bytes() != raw:
            raise ValueError("attestation bundle changed during verification")
        return [json.dumps({"bundle_sha256": hashlib.sha256(raw).hexdigest(),
                            "subjects": subjects, "source_sha": record["identity"]["source_sha"],
                            "workflow_sha": workflow_sha}, sort_keys=True)]

    def workflow_source(self, record: dict[str, Any]) -> str:
        """Bind preparation's main workflow revision to the candidate workflow tree.

        workflow_run can execute a newer main revision than the built source.
        Permit that only when the complete workflow directory is unchanged.
        """
        ident = record["identity"]
        run = github(f"repos/{self.repository}/actions/runs/{ident['run_id']}/attempts/{ident['attempt']}")
        if (run["id"] != ident["run_id"] or run["run_attempt"] != ident["attempt"]
                or run["path"] != ".github/workflows/" + ident["workflow"].removeprefix(".github/workflows/")
                or run["head_branch"] != "main" or run["event"] not in ("workflow_run", "workflow_dispatch")
                or run["repository"]["id"] != self.repository_id
                or run["head_repository"]["id"] != self.repository_id
                or not re.fullmatch(r"[0-9a-f]{40}", run["head_sha"])):
            raise ValueError("preparation workflow identity mismatch")
        commit = github(f"repos/{self.repository}/git/commits/{ident['source_sha']}")
        if commit["sha"] != ident["source_sha"] or commit["tree"]["sha"] != ident["tree_sha"]:
            raise ValueError("source commit/tree mismatch")
        trees = []
        for sha in (ident["source_sha"], run["head_sha"]):
            entries = github(f"repos/{self.repository}/contents/.github/workflows?ref={sha}")
            if not entries or any(e["type"] != "file" for e in entries):
                raise ValueError("invalid workflow tree")
            trees.append({e["path"]: e["sha"] for e in entries})
        if trees[0] != trees[1]:
            raise ValueError("candidate and preparation workflow trees differ")
        return run["head_sha"]

    def ready(self, record: dict[str, Any], directory: Path, attestation_path: Path) -> None:
        """Verify all evidence, atomically promote, then label superseded drafts unpublishable."""
        snapshot = self.snapshot(record)
        listing = self.check_snapshot(record, snapshot)
        digests = assets.verify_downloads(directory, listing, snapshot["assets"])
        self.verify_payload(record, directory)
        attestations = self.attestations(record, directory, attestation_path, digests)
        checks: list[int] = []
        policy.verify_gates(record["identity"]["source_sha"], require_push=True, check_run_ids=checks)
        self.check_snapshot(record, snapshot)
        assets.verify_downloads(directory, listing, digests)
        value, _ = self.state.api.read()
        self.state._validate_state(value)
        if record["state"] == "ready":
            # Reverify bytes and bundle, but never replace the immutable receipt
            # with a newer set of gate IDs or a different predecessor on retry.
            receipt = record["receipt"]
            if (receipt["identity"] != record["identity"] or receipt["release_id"] != record["release_id"]
                    or receipt["assets"] != digests or receipt["attestations"] != attestations
                    or receipt["manifest_digest"] != digests["release-manifest.txt"]["sha256"]
                    or "supersedes" not in receipt
                    or value["candidates"][record["identity"]["candidate_id"]] != record):
                raise ValueError("ready retry evidence differs from persisted receipt")
        else:
            prior = next((key for key, old in value["candidates"].items() if old["state"] == "ready"), None)
            receipt = {"identity": record["identity"], "release_id": record["release_id"],
                       "assets": digests, "manifest_digest": digests["release-manifest.txt"]["sha256"],
                       "check_run_ids": checks, "attestations": attestations, "supersedes": prior}
        path = self.root / ".candidate-receipt.json"
        if path.is_symlink() or (path.exists() and json.loads(path.read_text()) != receipt):
            raise ValueError("local receipt differs from verified receipt")
        if not path.exists():
            write_evidence(path, receipt)
        if record["state"] == "preparing":
            # CAS checks supersedes against the actual prior current candidate.
            self.state.ready(record["identity"]["candidate_id"], receipt)
        self.label_predecessor(record["identity"]["candidate_id"], receipt)

    def label_predecessor(self, candidate_id: str, receipt: dict[str, Any]) -> None:
        """Idempotently label only the predecessor bound by the successful promotion CAS."""
        value, _ = self.state.api.read()
        self.state._validate_state(value)
        current = value["candidates"][candidate_id]
        if current["state"] != "ready" or current["receipt"] != receipt:
            raise ValueError("candidate is no longer ready with this receipt")
        prior = receipt["supersedes"]
        if prior is None:
            return
        old = value["candidates"][prior]
        draft = self.release(old)
        title = "SUPERSEDED — UNPUBLISHABLE — " + old["identity"]["draft_title"]
        marker = "This candidate is superseded and cannot be published."
        body = draft.get("body") or ""
        if draft.get("name") == title and body.startswith(marker):
            return
        if not body.startswith(marker):
            body = marker + "\n\n" + body
        github(f"{self.endpoint}/{old['release_id']}", "PATCH", {
            "name": title, "body": body, "draft": True, "make_latest": "false"})

    def reconcile(self, candidate_id: str) -> None:
        """Reconcile only the recorded predecessor's label from a new trusted main run.

        Unlike preparation, this does not require the original run/attempt or
        local artifacts. Authoritative ready state and its repository-bound
        receipt supply identity and the exact predecessor. No assets, receipt,
        lifecycle state, tags, or publication settings of the current draft change.
        """
        if os.environ.get("GITHUB_REF") != "refs/heads/main":
            raise ValueError("reconciliation must run from main")
        if github(f"repos/{self.repository}")["id"] != self.repository_id:
            raise ValueError("repository identity mismatch")
        value, _ = self.state.api.read()
        self.state._validate_state(value)
        record = value["candidates"].get(candidate_id)
        if record is None or record["state"] != "ready":
            raise ValueError("reconciliation requires a ready candidate")
        receipt = record["receipt"]
        if ("supersedes" not in receipt or set(receipt["assets"]) != assets.expected_assets(
                record["identity"]["final_tag"], INVENTORY)):
            raise ValueError("reconciliation requires a complete promotion receipt")
        self.release(record)
        self.label_predecessor(candidate_id, receipt)

    def execute(self, command: str, candidate_id: str, directory: Path | None = None,
                attestation_path: Path | None = None) -> None:
        """Execute one preparation operation; only owned preparing candidates may fail."""
        if command == "fail":
            self.fail(candidate_id)
            return
        if command == "reconcile":
            # Reconciliation errors must not use preparation's failure path.
            self.reconcile(candidate_id)
            return
        record = self.record(candidate_id, "preparing", "ready") if command == "ready" else self.record(candidate_id)
        try:
            self.verify_checkout(record)
            if directory is None:
                raise ValueError("asset directory is required")
            if directory.is_symlink():
                raise ValueError("asset directory cannot be a symlink")
            directory = directory.resolve()
            if directory == self.root or directory in self.snapshot_path.parents:
                raise ValueError("public assets must not contain private evidence")
            if command == "ready":
                if attestation_path is None:
                    raise ValueError("ready requires --attestation-path")
                self.ready(record, directory, attestation_path.resolve())
            elif command == "upload":
                self.upload(record, directory)
            elif command == "download":
                self.download(record, directory)
            else:
                raise ValueError("unknown preparation command")
        except Exception:
            # A promotion may have committed even if its response was lost.
            # fail() itself is CAS and refuses to demote ready/reserved candidates.
            try:
                self.fail(candidate_id)
            except Exception as failure:
                print(f"Could not record preparation failure: {failure}", file=sys.stderr)
            raise


def main() -> None:
    """Dispatch the workflow CLI using repository-bound state and job credentials."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("upload", "download", "ready", "fail", "reconcile"))
    parser.add_argument("--candidate-id", required=True)
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--attestation-path", type=Path)
    args = parser.parse_args()
    repository, rid = os.environ["GITHUB_REPOSITORY"], int(os.environ["GITHUB_REPOSITORY_ID"])
    api = rc.GitHubStateAPI(repository, rid, os.environ.get("GH_TOKEN") or os.environ["GITHUB_TOKEN"])
    Preparation(repository, rid, rc.Candidates(api, rid)).execute(
        args.command, args.candidate_id, args.directory, args.attestation_path)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"FAIL  candidate preparation: {exc}", file=sys.stderr)
        sys.exit(1)
