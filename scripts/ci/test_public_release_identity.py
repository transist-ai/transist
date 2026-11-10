#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Isolated source-tree regressions; no builds, networking or signing."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from check_public_release_identity import (
    NEGATIVE_CONTRACT_LINES,
    NEGATIVE_NUMERIC_LINES,
    PROFILE,
    ROOT,
    check_tree,
    source_paths,
)

BAD_VERSION = "0.9.0"


class PublicIdentityTests(unittest.TestCase):
    """Exercise prohibited identities without creating repositories or commits."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="public-identity-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        inventory = patch(
            "check_public_release_identity.source_paths",
            side_effect=lambda root: sorted(root.rglob("*")),
        )
        inventory.start()
        self.addCleanup(inventory.stop)
        self.profile = json.loads((ROOT / PROFILE).read_text())
        self.runtime_version = self.profile["runtime_version"]
        self.write(PROFILE, json.dumps(self.profile))
        self.write(
            "Cargo.toml",
            f'[workspace.package]\nversion = "{self.runtime_version}"\n'
            '[package]\nname = "transist"\nversion.workspace = true\n',
        )
        self.write(
            "Cargo.lock",
            f'[[package]]\nname = "transist"\nversion = "{self.runtime_version}"\n',
        )
        for version in self.profile["approved_release_notes"]:
            self.write(f"docs/public/releases/{version}.md", "# Public release\n")
        for name in (
            "schemas/runtime/run-state.v1.schema.json",
            "crates/transist-core/src/types.rs",
            "crates/transist-core/src/dispatch.rs",
        ):
            self.write(name, (ROOT / name).read_text())

    def write(self, name: str, text: str) -> None:
        """Write a candidate file inside the disposable tree."""
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_prohibited_identities_on_all_surfaces(self) -> None:
        """Hidden sources, scripts and docs all reject each prohibited identity."""
        cases = {
            "private-repository": "https://github.com/transist-ai/"
            + "transist.oss/issues",
            "obsolete-domain": "https://transist." + "io/docs",
            "disabled-discussions": "https://github.com/transist-ai/transist/"
            + "discussions",
            "product-placeholder": "transist-vX." + "Y.Z-linux.tar.gz",
            "migration-reference": "migration-0." + "12-to-0.13.md",
            "stale-target-count": "All three " + "targets",
            "unsupported-contract": "transist.workflow_install." + "v2",
        }
        for path in (
            "README.md",
            ".agents/skills/new.md",
            "scripts/ci/new.sh",
            "src/new.rs",
        ):
            for rule, payload in cases.items():
                with self.subTest(path=path, rule=rule):
                    self.write(path, payload)
                    self.assertTrue(
                        any(rule in failure for failure in check_tree(self.root))
                    )
            (self.root / path).unlink()
        self.assertEqual(check_tree(self.root), [])

    def test_exact_rejection_fixture_exceptions(self) -> None:
        """Only reviewed path/line pairs pass, not nearby or copied claims."""
        for path, lines in NEGATIVE_CONTRACT_LINES.items():
            for line in lines:
                with self.subTest(path=path):
                    self.write(path, line)
                    self.assertEqual(check_tree(self.root), [])
                    self.write(path, line + " // supported")
                    self.assertTrue(
                        any("unsupported-contract" in f for f in check_tree(self.root))
                    )
                    (self.root / path).unlink()
                    self.write("src/copied.rs", line)
                    self.assertTrue(
                        any("unsupported-contract" in f for f in check_tree(self.root))
                    )
                    (self.root / "src/copied.rs").unlink()

    def test_contract_api_and_large_versions(self) -> None:
        """All later runtime revisions fail; external API revisions remain valid."""
        for token in (
            "transist.storage.v",
            "app.v",
            "app.handshake.v",
            "tep.v",
            "tep.request.v",
            "/api/v",
        ):
            for version in (2, 9, 10, 99, 100):
                self.write("schemas/new.json", token + str(version))
                self.assertTrue(
                    any("unsupported-contract" in f for f in check_tree(self.root))
                )
        (self.root / "schemas/new.json").unlink()
        self.write("schemas/runtime/new.v2.schema.json", "{}")
        self.assertTrue(
            any("unsupported-contract filename" in f for f in check_tree(self.root))
        )

    def test_private_narratives(self) -> None:
        """Public notes and roadmap cannot retain development audit history."""
        for text in (
            "Commit abcdef1234567",
            "PR #98",
            "Step 12",
            "remediation complete",
            "private release",
            "failed release",
            "checks passed",
            "gate passed",
        ):
            with self.subTest(text=text):
                self.write("ROADMAP.md", text)
                self.assertTrue(
                    any(
                        "private development narrative" in f
                        for f in check_tree(self.root)
                    )
                )

    def test_owned_release_versions(self) -> None:
        """Artifact names, product prose and SBOM identities use the profile."""
        for text in (
            "transist-v" + "0.9.0-linux.tar.gz",
            "Transist " + "0.9.0",
            f"runtime/admin crates use `{BAD_VERSION}`",
            'tag="v0.9.0"',
            f'"name": "transist-cli", "version": "{BAD_VERSION}"',
            f'transist_version: "{BAD_VERSION}".to_string(),',
            f'"transist_version": "{BAD_VERSION}"',
        ):
            self.write("scripts/ci/fixture.sh", text)
            self.assertTrue(any("owned release" in f for f in check_tree(self.root)))

    def test_workspace_and_lock_versions(self) -> None:
        """Workspace versions, explicit owned crates and lock entries are checked."""
        self.write(
            "Cargo.toml",
            '[workspace.package]\nversion="0.9.0"\n'
            '[package]\nname="transist"\nversion="0.9.0"\n',
        )
        self.write("Cargo.lock", '[[package]]\nname="transist"\nversion="0.9.0"\n')
        failures = check_tree(self.root)
        self.assertTrue(any("workspace release" in f for f in failures))
        self.assertTrue(any("owned release" in f for f in failures))
        self.assertTrue(any("Cargo.lock" in f for f in failures))

    def test_multiline_sbom_and_documented_pin(self) -> None:
        """Generated nested component identities and maintained pins cannot drift."""
        self.write(
            "sbom.json",
            json.dumps(
                {
                    "components": [
                        {"name": "serde", "version": "1.0.200"},
                        {"name": "transist", "version": BAD_VERSION},
                    ]
                },
                indent=2,
            ),
        )
        self.write("docs/pins.md", "shell@" + "1.4.3")
        failures = check_tree(self.root)
        self.assertTrue(any("sbom.json: owned release" in f for f in failures))
        self.assertTrue(any("owned catalog pin" in f for f in failures))

    def test_catalog_versions_and_pins(self) -> None:
        """A newly introduced owned catalog package is inventoried too."""
        self.write(
            "plugins/new/plugin.yaml", "schema: transist.plugin.v1\nversion: 0.9.0\n"
        )
        self.write(
            "workflows/new.yaml",
            "schema: transist.flow.v1\nversion: 0.1.0\nplugins:\n  shell: 0.9.0\n",
        )
        failures = check_tree(self.root)
        self.assertTrue(any("release version" in f for f in failures))
        self.assertTrue(any("must pin" in f for f in failures))

    def test_approved_patch_notes(self) -> None:
        """Patch notes require explicit approval, not merely a version-shaped name."""
        major, minor, patch_version = map(int, self.runtime_version.split("."))
        next_version = f"{major}.{minor}.{patch_version + 1}"
        self.write(f"docs/public/releases/{next_version}.md", "# Public patch\n")
        self.assertTrue(any("unapproved" in f for f in check_tree(self.root)))
        self.profile["approved_release_notes"].append(next_version)
        self.profile["runtime_version"] = next_version
        self.write("docs/public/releases/0.1.0.md", "# Transist " + "0.1.0\n")
        self.write(PROFILE, json.dumps(self.profile))
        for path in ("Cargo.toml", "Cargo.lock"):
            self.write(
                path,
                (self.root / path).read_text().replace(self.runtime_version, next_version),
            )
        self.assertEqual(check_tree(self.root), [])
        self.write("docs/public/releases/unreviewed.md", "Unexpected notes")
        self.assertTrue(any("unapproved" in f for f in check_tree(self.root)))

    def test_release_narrative_and_missing_notes(self) -> None:
        """Private release headings and missing approved notes fail independently."""
        for heading in ("## [0.13.3]", "# Release 0.12.0"):
            self.write("CHANGELOG.md", heading)
            self.assertTrue(
                any("unapproved release narrative" in f for f in check_tree(self.root))
            )
        (self.root / "CHANGELOG.md").unlink()
        (self.root / "docs/public/releases/0.1.0.md").unlink()
        self.assertTrue(
            any("missing approved release notes" in f for f in check_tree(self.root))
        )

    def test_migration_filename(self) -> None:
        """An empty obsolete guide is rejected by its path, not just its content."""
        self.write("docs/public/reference/migration-0." + "12-to-0.13.md", "")
        self.assertTrue(
            any("private migration file" in f for f in check_tree(self.root))
        )

    def test_dependencies_standards_and_synthetic_ordering(self) -> None:
        """Unowned versions and algorithm-ordering values are not launch claims."""
        self.write(
            "tools.txt",
            "Python 3.14.0\nOpenAPI 3.1.0\nCycloneDX 1.5\n"
            "https://external.example/v2/chat\nactions/checkout@v4\n"
            "https://external.example/api/v2/chat\n"
            "ALPINE_BUBBLEWRAP_PACKAGE=bubblewrap=0.12.0-r0\n"
            "fixture-owner/fixture-repo v0.10.16\n"
            "transist-<tag>-<target>.tar.gz\ntransist.flow.v1\n/api/v1/runs\n"
            "app.v1\napp.invoke.request.v1\ntep.v1\ntep.invoke.v1\n",
        )
        self.write(
            "README.md",
            f"transist-v{self.runtime_version}-x86_64-unknown-linux-gnu.tar.gz\n"
            "Key ID `9B91C4C6F8E61392`\nPython 3.14.0\n",
        )
        with (self.root / "Cargo.lock").open("a") as stream:
            stream.write('[[package]]\nname="serde"\nversion="1.0.200"\n')
        self.assertEqual(check_tree(self.root), [])

    def test_git_inventory(self) -> None:
        """Tracked ignored files are checked; deletions and ignored outputs are not."""
        result = subprocess.CompletedProcess([], 0, b"tracked\0deleted\0new\0tracked\0")
        with patch(
            "check_public_release_identity.subprocess.run", return_value=result
        ) as git:
            self.assertEqual(
                source_paths(self.root),
                [self.root / name for name in ("deleted", "new", "tracked")],
            )
            self.assertEqual(
                git.call_args.args[0][-5:],
                ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            )

    def test_shell_entrypoint(self) -> None:
        """The shell gate propagates success and missing-profile failure."""
        command = [
            "bash",
            str(ROOT / "scripts/ci/check_public_release_identity.sh"),
            "--root",
            str(ROOT),
        ]
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
        command[-1] = str(self.root / "missing")
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("Public release identity check failed", result.stderr)

    def test_numeric_schema_bounds(self) -> None:
        """Removing or widening either real schema bound fails the inventory."""
        name = "schemas/runtime/run-state.v1.schema.json"
        original = json.loads((self.root / name).read_text())
        for field in self.profile["numeric_runtime_contracts"]:
            for maximum in (None, 2, 99):
                schema = json.loads(json.dumps(original))
                if maximum is None:
                    del schema["properties"][field]["maximum"]
                else:
                    schema["properties"][field]["maximum"] = maximum
                self.write(name, json.dumps(schema))
                self.assertTrue(
                    any("numeric-contract" in f for f in check_tree(self.root))
                )
        self.write(name, json.dumps(original))
        self.assertEqual(check_tree(self.root), [])

    def test_numeric_runtime_guards(self) -> None:
        """Widened, removed, or commented guards fail despite unchanged tests."""
        for contract in self.profile["numeric_runtime_contracts"].values():
            for name, expression in contract["guards"].items():
                original = (self.root / name).read_text()
                guard = f"if {expression} > 1 {{"
                for replacement in (
                    f"if {expression} > 2 {{",
                    "if false {",
                    f"// {guard}\nif false {{",
                ):
                    self.write(name, original.replace(guard, replacement))
                    self.assertTrue(
                        any(
                            "rejection guard missing" in f
                            for f in check_tree(self.root)
                        )
                    )
                self.write(name, original)

    def test_numeric_inventory_cannot_omit_contracts(self) -> None:
        """Profile removal cannot silently exclude a numeric runtime contract."""
        del self.profile["numeric_runtime_contracts"]["asset_lock_version"]
        self.write(PROFILE, json.dumps(self.profile))
        self.assertTrue(
            any(
                "incomplete numeric-contract inventory" in f
                for f in check_tree(self.root)
            )
        )

    def test_numeric_claims_and_exact_exceptions(self) -> None:
        """Numeric claims are scanned in tests too; exceptions are exact path/line."""
        for field in self.profile["numeric_runtime_contracts"]:
            for value in (2, 3, 99):
                for name in ("docs/claim.md", "tests/new.rs", "src/new.rs"):
                    for claim in (
                        f"record.{field} = {value};",
                        f'json["{field}"] = serde_json::json!({value});',
                    ):
                        self.write(name, claim)
                        self.assertTrue(
                            any(
                                "unsupported-numeric-contract" in f
                                for f in check_tree(self.root)
                            )
                        )
                    (self.root / name).unlink()
        for name, lines in NEGATIVE_NUMERIC_LINES.items():
            for line in lines:
                self.write(name, line)
                self.assertEqual(check_tree(self.root), [])
                self.write(name, line + " // supported")
                self.assertTrue(
                    any(
                        "unsupported-numeric-contract" in f
                        for f in check_tree(self.root)
                    )
                )
                (self.root / name).unlink()
                self.write("tests/copied.rs", line)
                self.assertTrue(
                    any(
                        "unsupported-numeric-contract" in f
                        for f in check_tree(self.root)
                    )
                )
                (self.root / "tests/copied.rs").unlink()


if __name__ == "__main__":
    unittest.main()
