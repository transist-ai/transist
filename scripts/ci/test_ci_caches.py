#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Offline cache-contract mutation tests; no YAML package, downloads or builds.

Like test_ci_contracts.py, this inspects the repository's block-job/step format.
Actionlint remains the general YAML/Actions validator. Workflow hashes bind the
actual container digests, including matrix images, without unavailable job fields.
"""

import hashlib
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
JOB = re.compile(r"^  ([\w-]+):[^\S\n]*\n(.*?)(?=^  [\w-]+:|\Z)", re.M | re.S)
TOOLS = {"cargo-deny", "cargo-about", "cargo-cyclonedx", "cargo-llvm-cov"}


def field(step: str, name: str) -> str:
    """Return one scalar action input at the checked-in with-block indentation."""
    match = re.search(rf"^          {re.escape(name)}: (.+)$", step, re.M)
    return match.group(1).strip("'\"") if match else ""


def hashed_files(key: str) -> set[str]:
    """Return literal inputs actually passed to hashFiles, rather than key labels."""
    return {
        path
        for call in re.findall(r"hashFiles\(([^)]*)\)", key)
        for path in re.findall(r"['\"]([^'\"]+)['\"]", call)
    }


def cache_errors(text: str, workflow: str) -> list[str]:
    """Report missing caches, unsafe paths, weak keys and validation bypasses."""
    scope = "pr-ci" if workflow == "pr-ci.yml" else "main-ci"
    required = (
        {"validate-full", "schema-api-contracts", "coverage"}
        if scope == "pr-ci"
        else {"platform-smoke", "musl-smoke", "coverage"}
    )
    required_tools = (
        {"validate-full", "schema-api-contracts", "cargo-deny", "coverage"}
        if scope == "pr-ci"
        else {"coverage"}
    )
    errors: list[str] = []
    builds: set[str] = set()
    tool_jobs: set[str] = set()
    identities: set[str] = set()
    image_input = f".github/workflows/{workflow}"
    for job, body in JOB.findall(text.partition("\njobs:\n")[2]):
        for step in re.split(r"^      - ", body, flags=re.M)[1:]:
            if "cache-hit" in step:
                errors.append(f"validation must run on cache hits: {job}")
            action = re.search(r"^        uses: (\S+)", step, re.M)
            if not action:
                continue
            action = action.group(1)
            build = action.startswith("Swatinem/rust-cache@")
            tool = action.startswith("actions/cache@")
            if not (build or tool):
                continue
            if not re.fullmatch(r"[^@]+@[0-9a-f]{40}", action):
                errors.append(f"cache action must use a commit pin: {job}")
            key = field(step, "prefix-key" if build else "key")
            inputs = hashed_files(key)
            if not key.startswith(scope + "-"):
                errors.append(f"cache crosses workflow trust scope: {job}")
            if image_input not in inputs:
                errors.append(f"cache must hash actual workflow/container image: {job}")
            if "scripts/ci/tool-versions.env" not in inputs:
                errors.append(f"cache must hash tool/image pins: {job}")
            if build:
                builds.add(job)
                mode = "coverage" if job == "coverage" else "noncoverage"
                if not key.startswith(f"{scope}-{mode}-"):
                    errors.append(f"build instrumentation namespace is wrong: {job}")
                if field(step, "cache-bin") != "false":
                    errors.append(f"whole Cargo bin caching is forbidden: {job}")
                if field(step, "cache-directories"):
                    errors.append(
                        f"extra runtime/cache directories are forbidden: {job}"
                    )
                identity = field(step, "shared-key")
                expected = (
                    "${{ matrix.cache_key }}"
                    if job == "platform-smoke"
                    else f"{scope}-{job}"
                )
                if identity != expected or identity in identities:
                    errors.append(f"build job cache identity is missing/shared: {job}")
                identities.add(identity)
            else:
                tool_jobs.add(job)
                mode = {"validate-full": "release", "coverage": "coverage"}.get(
                    job, "audit"
                )
                if not key.startswith(f"{scope}-tools-{mode}-"):
                    errors.append(f"tool job/mode cache identity is wrong: {job}")
                for pin in ("rust-toolchain.toml", "scripts/ci/install_ci_tools.sh"):
                    if pin not in inputs:
                        errors.append(f"tool cache must hash {pin}: {job}")
                if not all(f"runner.{value}" in key for value in ("os", "arch")):
                    errors.append(f"tool cache must bind OS and architecture: {job}")
                if re.search(r"^          restore-keys:", step, re.M):
                    errors.append(f"tool cache fallback is forbidden: {job}")
                paths = field(step, "path")
                if paths == "|":
                    paths = "\n".join(re.findall(r"^            (.+)$", step, re.M))
                allowed = {"${{ env.CI_TOOL_ROOT }}/bin/" + name for name in TOOLS}
                expected_names = {
                    "coverage": {"cargo-llvm-cov"},
                    "audit": {"cargo-deny", "cargo-about"},
                    "release": TOOLS,
                }[mode]
                expected_paths = {
                    "${{ env.CI_TOOL_ROOT }}/bin/" + name for name in expected_names
                }
                actual_paths = set(paths.splitlines())
                if not actual_paths <= allowed or actual_paths != expected_paths:
                    errors.append(
                        f"tool cache must contain only selected Cargo executables: {job}"
                    )
    if builds != required:
        errors.append("required build cache job set changed")
    if tool_jobs != required_tools:
        errors.append("required tool cache job set changed")
    if not re.search(r"^  CI_TOOL_ROOT: /tmp/transist-ci-tools$", text, re.M):
        errors.append("installer/cache tool root must be explicit and identical")
    if scope == "pr-ci":
        node = next(
            (
                step
                for step in re.split(r"^      - ", text, flags=re.M)
                if "uses: actions/setup-node@" in step
            ),
            "",
        )
        if field(node, "cache") != "npm" or field(node, "cache-dependency-path") != (
            "scripts/ci/markdown/package-lock.json"
        ):
            errors.append("Markdown npm cache must use its committed lockfile")
    return errors


class CacheContracts(unittest.TestCase):
    """Exercise the real workflow contracts and rejected unsafe mutations."""

    def test_current_workflows(self) -> None:
        """All required caches use pinned, isolated, image-aware namespaces."""
        for workflow in ("pr-ci.yml", "main-ci.yml"):
            with self.subTest(workflow=workflow):
                text = (ROOT / ".github/workflows" / workflow).read_text()
                self.assertEqual(cache_errors(text, workflow), [])

    def test_unsafe_cache_mutations(self) -> None:
        """Reject fallback, weak pins, broad binaries, sharing and lost npm caching."""
        text = (ROOT / ".github/workflows/pr-ci.yml").read_text()
        changes = [
            ("'rust-toolchain.toml', ", ""),
            (", 'scripts/ci/install_ci_tools.sh'", ""),
            (", 'scripts/ci/tool-versions.env'", ""),
            ("hashFiles('.github/workflows/pr-ci.yml', ", "hashFiles("),
            (
                "          path: |",
                "          restore-keys: pr-ci-tools-\n          path: |",
            ),
            ("${{ env.CI_TOOL_ROOT }}/bin/cargo-deny", "~/.cargo/bin"),
            ("cache-bin: false", "cache-bin: true"),
            (
                "cache-bin: false",
                "cache-bin: false\n          cache-directories: .transist",
            ),
            ("pr-ci-tools-release-", "main-ci-tools-release-"),
            ("pr-ci-tools-coverage-", "pr-ci-tools-release-"),
            ("pr-ci-coverage-", "pr-ci-noncoverage-"),
            (
                "shared-key: pr-ci-schema-api-contracts",
                "shared-key: pr-ci-validate-full",
            ),
            ("uses: actions/cache@", "uses: actions/not-a-cache@"),
            (
                "cache-dependency-path: scripts/ci/markdown/package-lock.json",
                "cache-dependency-path: Cargo.lock",
            ),
            (
                "run: bash scripts/ci/run_coverage.sh coverage",
                "if: steps.cache.outputs.cache-hit != 'true'\n"
                "        run: bash scripts/ci/run_coverage.sh coverage",
            ),
        ]
        for old, new in changes:
            with self.subTest(mutation=old):
                self.assertIn(old, text)
                self.assertTrue(cache_errors(text.replace(old, new), "pr-ci.yml"))

    def test_image_update_invalidates_key_material(self) -> None:
        """Changing a literal container digest changes an input hashed by every cache."""
        for workflow in ("pr-ci.yml", "main-ci.yml"):
            text = (ROOT / ".github/workflows" / workflow).read_text()
            mutated = re.sub(r"@sha256:[0-9a-f]{64}", "@sha256:" + "0" * 64, text)
            self.assertNotEqual(
                hashlib.sha256(text.encode()).digest(),
                hashlib.sha256(mutated.encode()).digest(),
            )
            self.assertEqual(cache_errors(mutated, workflow), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
