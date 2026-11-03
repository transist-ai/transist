#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Check the public source identity against a reviewed, committed launch profile.

Scans tracked and non-ignored new files, using working-tree bytes (including
deletions). This is a source gate, not history, signature, archive or live-site
qualification. Profile changes require release/compatibility review; environment
variables cannot silently select another release. Third-party versions are not
product versions. Synthetic release-ordering inputs remain untouched.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

from check_catalog_versions import check_catalog

ROOT = Path(__file__).resolve().parents[2]
PROFILE = "scripts/ci/public-release-profile.json"
SEMVER = r"\d+\.\d+\.\d+"
RULES = {
    "private-repository": r"transist-ai[/:]transist\.oss\b",
    "obsolete-domain": r"\btransist\.io\b",
    "disabled-discussions": (
        r"github\.com/[^\s/]+/[^\s/)]+/discussions\b|\b[D]iscussions\b"
    ),
    "product-placeholder": r"\bvX\.Y\.Z\b",
    "migration-reference": r"migration-\d+\.\d+-to-\d+\.\d+",
    "stale-target-count": (
        r"\b(?:all\s+)?(?:three|3)[ -]+(?:release[ -]+)?"
        r"(?:targets|platforms|archives)\b"
    ),
    "unsupported-contract": (
        r"\b(?:transist|app|tep)\.(?:[\w.-]+\.)?v(\d+)\b|/api/v(\d+)\b"
    ),
}

# Exact path + complete stripped line, not a test-directory exemption. These
# inputs exercise fail-closed readers. A changed line requires fresh review.
# Construct identifiers here so the exception inventory itself is not a claim
# of supported contracts. The checks below still scan this checker and its tests.
NEGATIVE_CONTRACT_LINES = {
    "scripts/ci/test_authoring_tutorials.py": {
        '\'{"protocol_version":"app.' + "v99\"}\\n',",
    },
    "tests/support/process_lifecycle.rs": {
        "result['protocol_version'] = 'app." + "v999'",
    },
    "src/application/runs/execution.rs": {
        'json["schema"] = serde_json::json!("transist.run_provenance.' + 'v99");',
    },
    "tests/schema_contract.rs": {
        '"schema": "transist.run_state.' + 'v2",',
    },
    "crates/transist-core/src/kernel/gates.rs": {
        'bad_schema.schema = "transist.gate_decision.' + 'v99".to_string();',
    },
    "crates/transist-core/src/kernel/provenance.rs": {
        '"schema": "transist.run_provenance.' + 'v99",',
    },
    "scripts/ci/test_catalog_versions.py": {
        '"plugins/new/plugin.yaml", "schema: transist.plugin.'
        + 'v2\\nversion: 0.1.0\\n"',
    },
}

NEGATIVE_NUMERIC_LINES = {
    "crates/transist-core/tests/kernel_runtime.rs": {
        "record.asset_lock_version = " + "3;",
    },
}


def numeric_contract_inventory(root: Path, profile: dict) -> list[str]:
    """Check real schema bounds and fail-closed Rust guards, not test assertions.

    This source inventory deliberately requires review when guards move or change
    shape. Runtime tests remain the behavioral proof; textual presence alone is
    not a proof that every control-flow path invokes a validator.
    """
    failures = []
    contracts = profile["numeric_runtime_contracts"]
    if set(contracts) != {"asset_lock_version", "capability_authority_version"}:
        return [f"{PROFILE}: incomplete numeric-contract inventory"]
    for field, contract in contracts.items():
        if contract["minimum"] != 0 or contract["maximum"] != 1:
            failures.append(f"{PROFILE}: numeric-contract {field} requires bounds 0..1")
        schema_path = contract["schema"]
        if schema_path != "schemas/runtime/run-state.v1.schema.json":
            failures.append(f"{PROFILE}: numeric-contract {field} schema changed")
        schema = json.loads((root / schema_path).read_text())
        property_schema = schema.get("properties", {}).get(field, {})
        for key, expected in (("type", "integer"), ("minimum", 0), ("maximum", 1)):
            if property_schema.get(key) != expected:
                failures.append(
                    f"{schema_path}: numeric-contract {field} {key} must be {expected}"
                )
        guards = contract["guards"]
        expected_guards = {"crates/transist-core/src/types.rs": f"self.{field}"}
        if field == "asset_lock_version":
            expected_guards["crates/transist-core/src/dispatch.rs"] = field
        if guards != expected_guards:
            failures.append(
                f"{PROFILE}: numeric-contract {field} guard inventory changed"
            )
        for source, expression in expected_guards.items():
            text = (root / source).read_text()
            # Do not accept a commented-out guard or an assertion in its place.
            text = re.sub(r"/\*.*?\*/|//[^\n]*", "", text, flags=re.S)
            guard = (
                rf"\bif\s+{re.escape(expression)}\s*>\s*1\s*\{{\s*"
                rf"(?:anyhow::)?bail!\s*\("
            )
            if not re.search(guard, text):
                failures.append(
                    f"{source}: numeric-contract {field} rejection guard missing"
                )
    return failures


def numeric_line_violation(line: str, profile: dict) -> bool:
    """Reject later literal revisions and open-ended authority selection anywhere."""
    fields = "|".join(map(re.escape, profile["numeric_runtime_contracts"]))
    pattern = (
        rf'\b({fields})\b["\x27`\]]*\s*(>=|<=|==|!=|>|<|:|=)\s*'
        rf"(?:serde_json::json!\s*\(\s*)?(\d+)\b"
    )
    for match in re.finditer(pattern, line):
        field, operator, value = match.groups()
        maximum = profile["numeric_runtime_contracts"][field]["maximum"]
        if int(value) > maximum or (operator == ">=" and int(value) == maximum):
            return True
    return False


def source_paths(root: Path) -> list[Path]:
    """Inventory Git-visible working files without scanning ignored build output."""
    result = subprocess.run(
        [
            "git",
            "-C",
            str(root),
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        check=True,
        capture_output=True,
    )
    return [
        root / name for name in sorted(set(result.stdout.decode().split("\0"))) if name
    ]


def load_profile(root: Path) -> dict:
    """Read and validate the reviewed release identity and explicit note history."""
    profile = json.loads((root / PROFILE).read_text())
    for key in ("runtime_version", "catalog_version"):
        if not re.fullmatch(SEMVER, profile[key]):
            raise ValueError(f"invalid {key}")
    notes = profile["approved_release_notes"]
    if (
        profile["contract_version"] != 1
        or not isinstance(notes, list)
        or not notes
        or any(not isinstance(v, str) or not re.fullmatch(SEMVER, v) for v in notes)
        or len(set(notes)) != len(notes)
        or profile["runtime_version"] not in notes
    ):
        raise ValueError(
            "profile needs v1 contracts and explicit approved release-note history"
        )
    return profile


def owned_metadata(root: Path, paths: list[Path], profile: dict) -> list[str]:
    """Validate workspace manifests/lock identities without renaming dependencies."""
    failures = []
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    workspace = manifest["workspace"]["package"]
    expected = profile["runtime_version"]
    if workspace.get("version") != expected:
        failures.append(f"Cargo.toml: workspace release must be {expected}")
    names = set()
    for path in paths:
        if path.name != "Cargo.toml" or not path.is_file():
            continue
        data = tomllib.loads(path.read_text())
        package = data.get("package", {})
        name = package.get("name", "")
        if name != "transist" and not name.startswith("transist-"):
            continue
        names.add(name)
        version = package.get("version")
        if version not in (expected, {"workspace": True}):
            failures.append(
                f"{path.relative_to(root)}: owned release must be {expected}"
            )
    lock = root / "Cargo.lock"
    if lock.exists():
        for package in tomllib.loads(lock.read_text()).get("package", []):
            if (
                package["name"] in names
                and "source" not in package
                and package["version"] != expected
            ):
                failures.append(
                    f"Cargo.lock: {package['name']} release must be {expected}"
                )
    # Catalog policy is deliberately independent from runtime patch releases.
    if profile["catalog_version"] != "0.1.0":
        failures.append(
            f"{PROFILE}: catalog policy requires coordinated checker review"
        )
    failures.extend(check_catalog(root))
    return failures


def json_owned_versions(value: object, expected: str) -> list[str]:
    """Inspect nested SBOM/component records while preserving third-party versions."""
    failures = []
    if isinstance(value, dict):
        name = value.get("name", "")
        if isinstance(name, str) and (
            name == "transist" or name.startswith("transist-")
        ):
            if "version" in value and value["version"] != expected:
                failures.append(f"owned release for {name} must be {expected}")
        for child in value.values():
            failures.extend(json_owned_versions(child, expected))
    elif isinstance(value, list):
        for child in value:
            failures.extend(json_owned_versions(child, expected))
    return failures


def check_tree(root: Path) -> list[str]:
    """Return location-labelled violations in the current public candidate tree."""
    profile = load_profile(root)
    paths = source_paths(root)
    failures = owned_metadata(root, paths, profile)
    failures.extend(numeric_contract_inventory(root, profile))
    notes = {f"docs/public/releases/{v}.md" for v in profile["approved_release_notes"]}
    existing = {str(p.relative_to(root)) for p in paths if p.is_file()}
    for missing in sorted(notes - existing):
        failures.append(f"{missing}: missing approved release notes")
    compiled = {
        name: re.compile(pattern, re.I if name != "disabled-discussions" else 0)
        for name, pattern in RULES.items()
    }
    owned_version = re.compile(
        rf"\btransist(?:-(?:admin|server))?[- @]+v?({SEMVER})\b"
        rf"|\bTransist(?: runtime| release| version)?\s+`?v?({SEMVER})\b"
        rf"|\b[Rr]untime/admin crates[^\n]{{0,40}}?`({SEMVER})`"
        rf'|"name"\s*:\s*"transist(?:-[\w-]+)?"[^\n]*?"version"\s*:\s*"({SEMVER})"'
        rf'|\btransist_version["\x27]?\s*[:=]\s*["\x27]({SEMVER})["\x27]'
        rf"|^\s*(?:tag|release_tag)=[\"\x27]v({SEMVER})[\"\x27]",
    )
    narrative_bad = re.compile(
        r"\b(?-i:[0-9a-f]{40})\b|\bcommit\s+`?[0-9a-f]{7,40}\b|\b(?:PR|issue)\s*#\d+|"
        r"\b(?:Step[- ]\d+|remediation|private release|private baseline|failed release|"
        r"gate (?:passed|failed|outcomes?)|(?:checks|gates) passed)\b",
        re.I,
    )
    for path in paths:
        relative = str(path.relative_to(root))
        if not path.exists():
            continue  # A tracked deletion is absent from the candidate.
        if path.is_symlink():
            continue  # Do not follow links outside the candidate.
        if not path.is_file():
            continue
        content = path.read_bytes()
        if b"\0" in content:
            continue
        try:
            text = content.decode("utf-8")
        except UnicodeDecodeError:
            failures.append(f"{relative}: non-UTF-8 text cannot be inspected")
            continue
        if relative.startswith("docs/public/releases/") and relative not in notes:
            failures.append(f"{relative}: unapproved release-note file")
        if relative.startswith("schemas/"):
            for revision in re.findall(r"\.v(\d+)\.", path.name):
                if int(revision) > profile["contract_version"]:
                    failures.append(f"{relative}: unsupported-contract filename")
        if path.suffix == ".json":
            try:
                value = json.loads(text)
            except ValueError:
                pass  # Invalid JSON rejection fixtures are not metadata records.
            else:
                failures.extend(
                    f"{relative}: {f}"
                    for f in json_owned_versions(value, profile["runtime_version"])
                )
        if re.search(RULES["migration-reference"], relative):
            failures.append(f"{relative}: private migration file")
        narrative = relative in {
            "README.md",
            "CHANGELOG.md",
            "ROADMAP.md",
        } or relative.startswith("docs/public/releases/")
        for number, line in enumerate(text.splitlines(), 1):
            where = f"{relative}:{number}"
            if numeric_line_violation(
                line, profile
            ) and line.strip() not in NEGATIVE_NUMERIC_LINES.get(relative, set()):
                failures.append(f"{where}: unsupported-numeric-contract")
            for rule, pattern in compiled.items():
                matches = list(pattern.finditer(line))
                if rule == "unsupported-contract":
                    matches = [
                        m
                        for m in matches
                        if int(next(g for g in m.groups() if g))
                        > profile["contract_version"]
                    ]
                    # External API URLs do not declare the companion API contract.
                    external_urls = [
                        m.span()
                        for m in re.finditer(
                            r"https?://(?!localhost\b|127\.0\.0\.1\b|\[::1\])"
                            r"[^\s\"'<>]+",
                            line,
                        )
                    ]
                    matches = [
                        m
                        for m in matches
                        if not (
                            m.group().startswith("/api/")
                            and any(
                                start <= m.start() < end for start, end in external_urls
                            )
                        )
                    ]
                    if line.strip() in NEGATIVE_CONTRACT_LINES.get(relative, set()):
                        matches = []
                if matches:
                    failures.append(f"{where}: {rule}")
            for match in owned_version.finditer(line):
                version = next(g for g in match.groups() if g is not None)
                accepted = (
                    profile["approved_release_notes"]
                    if narrative
                    else [profile["runtime_version"]]
                )
                if version not in accepted:
                    failures.append(
                        f"{where}: owned release {version} != "
                        f"{profile['runtime_version']}"
                    )
            if path.suffix == ".md":
                for match in re.finditer(
                    rf"\b(?:shell|quickstart\.demo)@({SEMVER})", line
                ):
                    if match.group(1) != profile["catalog_version"]:
                        failures.append(
                            f"{where}: owned catalog pin must be "
                            f"{profile['catalog_version']}"
                        )
            if narrative and narrative_bad.search(line):
                failures.append(f"{where}: private development narrative")
            if narrative:
                for match in re.finditer(
                    rf"(?:^#+\s+\[?v?|\brelease\s+`?v?)({SEMVER})", line, re.I
                ):
                    if match.group(1) not in profile["approved_release_notes"]:
                        failures.append(f"{where}: unapproved release narrative")
    return failures


def main() -> int:
    """Run a read-only source check; --root supports isolated regression trees."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        failures = check_tree(args.root.resolve())
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        subprocess.CalledProcessError,
    ) as error:
        print(f"Public release identity check failed: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("Public release source identity matches the reviewed launch profile.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
