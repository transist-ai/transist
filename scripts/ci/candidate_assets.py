#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Fail-closed inventory and byte checks for candidate draft assets.

These helpers do not authenticate assets or promote candidates. Callers must
also verify signatures, provenance, attestations, and the candidate receipt.
"""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
from pathlib import Path
from typing import Any


def asset_digests(directory: Path, names: set[str]) -> dict[str, dict[str, Any]]:
    """Hash an exact inventory of regular files, rejecting symlinks and extras."""
    if directory.is_symlink() or not directory.is_dir() or {p.name for p in directory.iterdir()} != names:
        raise ValueError("asset directory inventory mismatch")
    result = {}
    for name in sorted(names):
        if not re.fullmatch(r"[A-Za-z0-9_.+-]+", name) or name in (".", ".."):
            raise ValueError("unsafe asset name")
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise ValueError("asset is not a regular file")
        digest = hashlib.sha256()
        size = 0
        with path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
        result[name] = {"sha256": digest.hexdigest(), "size": size}
    return result


def expected_assets(tag: str, inventory_path: Path) -> set[str]:
    """Derive the exact release asset names for a final tag from the inventory."""
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag):
        raise ValueError("invalid final release tag")
    inventory = json.loads(inventory_path.read_text(encoding="utf-8"))
    targets = [entry["target_triple"] for entry in inventory["released_archives"]]
    if (len(targets) != 6 or len(set(targets)) != 6 or
            any(not re.fullmatch(r"[a-z0-9_-]+", target) for target in targets)):
        raise ValueError("invalid release target inventory")
    names = {"packaging-provenance.json", f"transist-{tag}-sbom.tar.gz",
             "transist-release.minisign.pub", "release-manifest.txt",
             "release-manifest.txt.minisig"}
    for target in targets:
        archive = f"transist-{tag}-{target}.tar.gz"
        names.update((archive, archive + ".sha256", archive + ".minisig",
                      f"build-provenance-{target}.json"))
    if len(names) != 29:
        raise ValueError("invalid release asset inventory")
    return names


def validate_listing(pages: list[list[dict[str, Any]]], expected: set[str]) -> dict[str, dict[str, Any]]:
    """Validate all API listing pages before any filename is used locally.

    Returns metadata keyed by name; partial, duplicate, unsafe, non-uploaded,
    or unexpected entries and duplicate asset IDs are rejected.
    """
    result: dict[str, dict[str, Any]] = {}
    ids: set[int] = set()
    for page in pages:
        if not isinstance(page, list):
            raise ValueError("invalid asset listing page")
        for asset in page:
            if not isinstance(asset, dict):
                raise ValueError("invalid asset listing entry")
            name, asset_id, size = asset.get("name"), asset.get("id"), asset.get("size")
            if (not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_.+-]+", name)
                    or name in (".", "..") or name not in expected or name in result
                    or type(asset_id) is not int or asset_id <= 0 or asset_id in ids
                    or type(size) is not int or size < 0 or asset.get("state") != "uploaded"):
                raise ValueError("invalid, duplicate, or unexpected release asset")
            ids.add(asset_id)
            # download_count and other presentation fields can change merely
            # by reading a draft. Compare only immutable asset identity/bytes.
            result[name] = {key: asset[key] for key in ("name", "id", "size", "state")}
            if asset.get("digest") is not None:
                if not isinstance(asset["digest"], str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", asset["digest"]):
                    raise ValueError("invalid API asset digest")
                result[name]["digest"] = asset["digest"]
    if set(result) != expected:
        raise ValueError("incomplete release asset set")
    return result


def verify_downloads(directory: Path, listing: dict[str, dict[str, Any]],
                     expected: dict[str, dict[str, Any]]) -> dict[str, dict[str, Any]]:
    """Compare downloaded regular files with API sizes and independently recorded digests.

    Returns the computed digest and size map. This is a byte comparison, not
    signature, provenance, or attestation verification.
    """
    if set(listing) != set(expected) or {p.name for p in directory.iterdir()} != set(expected):
        raise ValueError("downloaded asset inventory mismatch")
    observed: dict[str, dict[str, Any]] = {}
    for name, metadata in expected.items():
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_.+-]+", name) or name in (".", ".."):
            raise ValueError("unsafe asset name")
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise ValueError("download is not a regular file")
        digest = hashlib.sha256()
        size = 0
        with path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
        observed[name] = {"sha256": digest.hexdigest(), "size": size}
        if (observed[name] != metadata or size != listing[name]["size"]
                or (listing[name].get("digest") is not None
                    and listing[name]["digest"] != "sha256:" + observed[name]["sha256"])):
            raise ValueError("downloaded asset bytes differ")
    return observed


def download_draft(repository: str, release_id: int, identity: dict[str, Any],
                   directory: Path, expected: dict[str, dict[str, Any]],
                   inventory_path: Path) -> dict[str, dict[str, Any]]:
    """Download and byte-compare a draft by release ID using the authenticated gh CLI.

    The destination must already exist and be empty. Validates draft identity,
    all asset-listing pages and the independent expected digest map; downloads
    by numeric asset ID and checks the listing again afterward. Raises on any
    mismatch or API error. This does not verify signatures or attestations.
    """
    if (not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository)
            or type(release_id) is not int or release_id <= 0
            or not re.fullmatch(r"[0-9a-f]{40}", identity.get("source_sha", ""))
            or not re.fullmatch(r"candidate-[0-9a-f]{40}-[1-9][0-9]*-[1-9][0-9]*", identity.get("draft_tag", ""))
            or not identity["draft_tag"].startswith("candidate-" + identity["source_sha"] + "-")):
        raise ValueError("invalid draft identity")
    if any(directory.iterdir()):
        raise ValueError("download destination must be empty")
    if set(expected) != expected_assets(identity["final_tag"], inventory_path):
        raise ValueError("expected digests do not match release inventory")
    root = f"repos/{repository}/releases"

    def check_release() -> None:
        release = json.loads(subprocess.check_output(["gh", "api", f"{root}/{release_id}"], text=True))
        if (release.get("id") != release_id or release.get("draft") is not True
                or release.get("tag_name") != identity["draft_tag"]
                or release.get("target_commitish") != identity["source_sha"]):
            raise ValueError("draft release identity mismatch")

    def listing() -> dict[str, dict[str, Any]]:
        pages = json.loads(subprocess.check_output(
            ["gh", "api", f"{root}/{release_id}/assets?per_page=100", "--paginate", "--slurp"], text=True))
        return validate_listing(pages, set(expected))

    check_release()
    before = listing()
    for name, asset in before.items():
        with (directory / name).open("xb") as output:
            subprocess.run(["gh", "api", f"{root}/assets/{asset['id']}",
                            "-H", "Accept: application/octet-stream"],
                           stdout=output, check=True)
    observed = verify_downloads(directory, before, expected)
    check_release()
    if listing() != before:
        raise ValueError("draft asset listing changed during download")
    return observed
