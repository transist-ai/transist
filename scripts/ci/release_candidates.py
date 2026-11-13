#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Candidate identity and atomic release-state transitions.

The state branch and state.json must already exist. Callers must independently
verify source metadata, checks, assets and attestations before supplying them;
this module validates and binds that evidence, it does not perform those checks.
"""

from __future__ import annotations

import base64
import copy
import json
import re
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Callable, Protocol


class CandidateError(ValueError):
    """An invalid identity, receipt, or lifecycle transition."""


class ConflictError(RuntimeError):
    """The state blob changed during a compare-and-swap update."""


class StateAPI(Protocol):
    """Minimal repository-bound API needed by the candidate state machine."""

    def read(self) -> tuple[dict[str, Any], str]: ...

    def write(self, state: dict[str, Any], previous_sha: str) -> None: ...

    def compare(self, base: str, head: str) -> str: ...


def _positive(value: Any) -> bool:
    return type(value) is int and value > 0


def _sha(value: Any, length: int) -> bool:
    return isinstance(value, str) and re.fullmatch(rf"[0-9a-f]{{{length}}}", value) is not None


def workspace_version(path: Path) -> str:
    """Read and validate the version in a source checkout's Cargo.toml."""
    with path.open("rb") as file:
        version = tomllib.load(file)["workspace"]["package"]["version"]
    _version(version)
    return version


def _version(version: Any) -> None:
    if not isinstance(version, str) or not re.fullmatch(
        r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
        r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
        r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?", version
    ):
        raise CandidateError("version must be valid semver")
    prerelease = version.split("+", 1)[0].partition("-")[2]
    if any(part.isdigit() and len(part) > 1 and part.startswith("0") for part in prerelease.split(".")):
        raise CandidateError("numeric prerelease identifiers cannot have leading zeroes")


def identity(repository_id: int, version: str, source_sha: str, tree_sha: str,
             workflow: str, run_id: int, attempt: int) -> dict[str, Any]:
    """Build an immutable identity from independently verified source metadata."""
    _version(version)
    if not _positive(repository_id) or not _positive(run_id) or not _positive(attempt):
        raise CandidateError("repository, run and attempt IDs must be positive integers")
    if not _sha(source_sha, 40) or not _sha(tree_sha, 40):
        raise CandidateError("source and tree must be full lowercase Git SHA-1s")
    if not isinstance(workflow, str) or not workflow.strip():
        raise CandidateError("preparation workflow is required")
    candidate_id = f"{source_sha}-{run_id}-{attempt}"
    return {"repository_id": repository_id, "candidate_id": candidate_id,
            "version": version, "final_tag": f"v{version}",
            "source_sha": source_sha, "tree_sha": tree_sha,
            "workflow": workflow, "run_id": run_id, "attempt": attempt,
            "draft_tag": f"candidate-{candidate_id}",
            "draft_title": f"v{version} candidate {source_sha[:7]}"}


def _validate_identity(value: dict[str, Any]) -> None:
    try:
        expected = identity(*(value[k] for k in ("repository_id", "version", "source_sha",
                            "tree_sha", "workflow", "run_id", "attempt")))
    except (KeyError, TypeError) as exc:
        raise CandidateError("malformed candidate identity") from exc
    if value != expected:
        raise CandidateError("candidate identity does not match its derived fields")


def _validate_receipt(receipt: Any, candidate: dict[str, Any], release_id: int) -> None:
    required = {"identity", "release_id", "assets", "manifest_digest", "check_run_ids", "attestations"}
    if not isinstance(receipt, dict) or set(receipt) not in (required, required | {"supersedes"}):
        raise CandidateError("malformed receipt")
    prior = receipt.get("supersedes")
    if prior is not None and (not isinstance(prior, str) or not re.fullmatch(
            r"[0-9a-f]{40}-[1-9][0-9]*-[1-9][0-9]*", prior)):
        raise CandidateError("invalid superseded predecessor")
    if receipt["identity"] != candidate or receipt["release_id"] != release_id or not _positive(release_id):
        raise CandidateError("receipt identity or release ID mismatch")
    assets = receipt["assets"]
    if not isinstance(assets, dict) or len(assets) != 29:
        raise CandidateError("receipt must contain exactly 29 distinct assets")
    for name, metadata in assets.items():
        if (not isinstance(name, str) or not name or name in (".", "..") or
                "/" in name or "\\" in name or any(ord(c) < 32 for c in name) or
                not isinstance(metadata, dict) or set(metadata) != {"sha256", "size"} or
                not _sha(metadata["sha256"], 64) or type(metadata["size"]) is not int or
                metadata["size"] < 0):
            raise CandidateError("invalid asset metadata")
    if ("release-manifest.txt" not in assets or
            receipt["manifest_digest"] != assets["release-manifest.txt"]["sha256"]):
        raise CandidateError("manifest digest mismatch")
    checks = receipt["check_run_ids"]
    if not isinstance(checks, list) or not checks or any(not _positive(x) for x in checks) or len(set(checks)) != len(checks):
        raise CandidateError("invalid check-run evidence")
    attestations = receipt["attestations"]
    if not isinstance(attestations, list) or not attestations or any(not isinstance(x, str) or not x.strip() for x in attestations):
        raise CandidateError("attestation evidence is required")


class GitHubStateAPI:
    """Read/write state.json on release-state via GitHub's Contents API.

    The repository ID is checked on every operation. Only a changed blob is
    considered a conflict; other API errors are never retried as conflicts.
    """

    def __init__(self, repository: str, repository_id: int, token: str,
                 api_url: str = "https://api.github.com") -> None:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) or not _positive(repository_id) or not token:
            raise CandidateError("invalid repository configuration")
        self.repository = repository
        self.repository_id = repository_id
        self.token = token
        self.api_url = api_url.rstrip("/")

    def _request(self, path: str, data: dict[str, Any] | None = None) -> Any:
        request = urllib.request.Request(
            f"{self.api_url}/repos/{self.repository}{path}",
            data=None if data is None else json.dumps(data).encode(),
            headers={"Authorization": f"Bearer {self.token}", "Accept": "application/vnd.github+json",
                     "X-GitHub-Api-Version": "2022-11-28"},
            method="GET" if data is None else "PUT")
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)

    def _check_repo(self) -> None:
        if self._request("").get("id") != self.repository_id:
            raise CandidateError("GitHub repository ID mismatch")

    def read(self) -> tuple[dict[str, Any], str]:
        """Read the existing state and its Git blob SHA; missing state fails closed."""
        self._check_repo()
        result = self._request("/contents/state.json?ref=release-state")
        if result.get("encoding") != "base64" or not _sha(result.get("sha"), 40):
            raise CandidateError("invalid GitHub Contents response")
        try:
            content = "".join(result["content"].split())
            state = json.loads(base64.b64decode(content, validate=True))
        except (KeyError, ValueError, TypeError) as exc:
            raise CandidateError("invalid state content") from exc
        return state, result["sha"]

    def write(self, state: dict[str, Any], previous_sha: str) -> None:
        """Write state using the previously read blob SHA as a precondition."""
        self._check_repo()
        data = {"message": "Update release candidate state", "branch": "release-state",
                "sha": previous_sha, "content": base64.b64encode(
                    (json.dumps(state, sort_keys=True, indent=2) + "\n").encode()).decode()}
        try:
            self._request("/contents/state.json", data)
        except urllib.error.HTTPError as exc:
            if exc.code in (409, 422) and self.read()[1] != previous_sha:
                raise ConflictError("state changed") from exc
            raise

    def compare(self, base: str, head: str) -> str:
        """Return GitHub's ancestry comparison status for two full commits."""
        if not _sha(base, 40) or not _sha(head, 40):
            raise CandidateError("invalid comparison SHA")
        self._check_repo()
        status = self._request(f"/compare/{base}...{head}").get("status")
        if status not in ("ahead", "behind", "identical", "diverged"):
            raise CandidateError("unknown ancestry comparison")
        return status


class Candidates:
    """Atomic operations on repository-bound candidate lifecycle state."""

    def __init__(self, api: StateAPI, repository_id: int) -> None:
        if not _positive(repository_id):
            raise CandidateError("invalid repository ID")
        self.api, self.repository_id = api, repository_id

    def _update(self, operation: Callable[[dict[str, Any]], Any]) -> Any:
        for _ in range(5):
            state, sha = self.api.read()
            self._validate_state(state)
            updated = copy.deepcopy(state)
            result = operation(updated)
            self._validate_state(updated)
            if updated == state:
                return copy.deepcopy(result)
            try:
                self.api.write(updated, sha)
                return copy.deepcopy(result)
            except ConflictError:
                continue
        raise ConflictError("state update exhausted five attempts")

    def _validate_state(self, state: Any) -> None:
        if not isinstance(state, dict) or set(state) != {"repository_id", "candidates"} or state["repository_id"] != self.repository_id or not isinstance(state["candidates"], dict):
            raise CandidateError("invalid state or repository ID")
        active_sources: set[str] = set()
        current = 0
        published_versions: set[str] = set()
        release_ids: set[int] = set()
        for key, record in state["candidates"].items():
            if not isinstance(record, dict) or set(record) != {"identity", "state", "release_id", "receipt"}:
                raise CandidateError("malformed candidate record")
            ident = record["identity"]
            if not isinstance(ident, dict):
                raise CandidateError("malformed identity")
            _validate_identity(ident)
            status = record["state"]
            if key != ident["candidate_id"] or ident["repository_id"] != self.repository_id or status not in ("preparing", "failed", "ready", "superseded", "publishing", "published"):
                raise CandidateError("invalid candidate record")
            if status in ("preparing", "ready", "publishing"):
                if ident["source_sha"] in active_sources:
                    raise CandidateError("duplicate active source")
                active_sources.add(ident["source_sha"])
            if status in ("ready", "publishing"):
                current += 1
            if status == "published":
                if ident["version"] in published_versions:
                    raise CandidateError("duplicate published version")
                published_versions.add(ident["version"])
            if status in ("ready", "superseded", "publishing", "published"):
                _validate_receipt(record["receipt"], ident, record["release_id"])
            elif record["receipt"] is not None or (record["release_id"] is not None and not _positive(record["release_id"])):
                raise CandidateError("invalid preparation record")
            if record["release_id"] is not None:
                if record["release_id"] in release_ids:
                    raise CandidateError("duplicate release ID")
                release_ids.add(record["release_id"])
        if current > 1:
            raise CandidateError("multiple current candidates")
        for record in state["candidates"].values():
            prior = (record["receipt"] or {}).get("supersedes")
            if prior is not None:
                predecessor = state["candidates"].get(prior)
                if predecessor is None or predecessor is record or predecessor["state"] != "superseded":
                    raise CandidateError("receipt predecessor is not superseded")
        if any(r["identity"]["version"] in published_versions and r["state"] in ("ready", "publishing") for r in state["candidates"].values()):
            raise CandidateError("active candidate for published version")

    def claim(self, candidate: dict[str, Any], *, recovery: bool = False) -> dict[str, Any]:
        """Claim a source, reusing an active candidate or explicitly recovering failure."""
        _validate_identity(candidate)
        if candidate["repository_id"] != self.repository_id or type(recovery) is not bool:
            raise CandidateError("repository mismatch or invalid recovery flag")
        def apply(state: dict[str, Any]) -> dict[str, Any]:
            records = state["candidates"]
            if any(r["state"] == "published" and r["identity"]["version"] == candidate["version"] for r in records.values()):
                raise CandidateError("version already published")
            same = [r for r in records.values() if r["identity"]["source_sha"] == candidate["source_sha"]]
            for r in same:
                if any(r["identity"][k] != candidate[k] for k in ("repository_id", "version", "tree_sha")):
                    raise CandidateError("source metadata mismatch")
                if r["state"] in ("preparing", "ready", "publishing"):
                    return r
            if candidate["candidate_id"] in records:
                raise CandidateError("candidate ID already used")
            if any(r["state"] == "failed" for r in same) and not recovery:
                raise CandidateError("failed preparation requires explicit recovery")
            if any(r["state"] in ("superseded", "published") for r in same):
                raise CandidateError("source is no longer eligible")
            record = {"identity": copy.deepcopy(candidate), "state": "preparing", "release_id": None, "receipt": None}
            records[candidate["candidate_id"]] = record
            return record
        return self._update(apply)

    def bind_release(self, candidate_id: str, release_id: int) -> None:
        """Bind the draft release ID once, without permitting replacement."""
        if not _positive(release_id):
            raise CandidateError("invalid release ID")
        def apply(state: dict[str, Any]) -> None:
            r = self._record(state, candidate_id, "preparing")
            if r["release_id"] not in (None, release_id) or any(k != candidate_id and v["release_id"] == release_id for k, v in state["candidates"].items()):
                raise CandidateError("release ID already bound")
            r["release_id"] = release_id
        self._update(apply)

    def fail(self, candidate_id: str) -> None:
        """Mark a preparing candidate failed; recovery needs a new identity."""
        def apply(state: dict[str, Any]) -> None:
            self._record(state, candidate_id, "preparing")["state"] = "failed"
        self._update(apply)

    def ready(self, candidate_id: str, receipt: dict[str, Any]) -> None:
        """Promote a verified candidate, superseding only an older ready one."""
        def apply(state: dict[str, Any]) -> None:
            r = self._record(state, candidate_id, "preparing")
            _validate_receipt(receipt, r["identity"], r["release_id"])
            current = next((key for key, other in state["candidates"].items()
                            if other["state"] == "ready"), None)
            if "supersedes" in receipt and receipt["supersedes"] != current:
                raise CandidateError("promotion predecessor changed")
            for other in state["candidates"].values():
                if other["state"] == "published" and other["identity"]["version"] == r["identity"]["version"]:
                    raise CandidateError("version already published")
                if other["state"] in ("ready", "publishing", "published"):
                    ordering = self.api.compare(other["identity"]["source_sha"], r["identity"]["source_sha"])
                    if ordering != "ahead":
                        raise CandidateError("candidate is not a descendant of current release")
                    if other["state"] == "publishing":
                        raise CandidateError("publication is reserved")
                    if other["state"] == "ready":
                        other["state"] = "superseded"
            r["receipt"] = copy.deepcopy(receipt)
            r["state"] = "ready"
        self._update(apply)

    def reserve(self, candidate_id: str, receipt: dict[str, Any]) -> None:
        """Reserve a ready candidate for publication, binding the exact receipt."""
        def apply(state: dict[str, Any]) -> None:
            r = self._record(state, candidate_id, "ready", "publishing")
            if r["receipt"] != receipt:
                raise CandidateError("receipt mismatch")
            r["state"] = "publishing"
        self._update(apply)

    def published(self, candidate_id: str, receipt: dict[str, Any]) -> None:
        """Record successful publication of the reserved, unchanged receipt."""
        def apply(state: dict[str, Any]) -> None:
            r = self._record(state, candidate_id, "publishing", "published")
            if r["receipt"] != receipt:
                raise CandidateError("receipt mismatch")
            if any(v is not r and v["identity"]["version"] == r["identity"]["version"] and v["state"] in ("ready", "publishing", "published") for v in state["candidates"].values()):
                raise CandidateError("another candidate uses this version")
            r["state"] = "published"
        self._update(apply)

    @staticmethod
    def _record(state: dict[str, Any], candidate_id: str, *allowed: str) -> dict[str, Any]:
        r = state["candidates"].get(candidate_id)
        if r is None or r["state"] not in allowed:
            raise CandidateError("candidate does not permit this transition")
        return r
