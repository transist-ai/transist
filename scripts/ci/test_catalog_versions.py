#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Focused regression tests for the maintained catalog release policy."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_catalog_versions import ROOT, check_catalog, runtime_version


class CatalogVersionTests(unittest.TestCase):
    """Reject catalog drift while preserving independent schemas and fixtures."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, path: str, text: str) -> None:
        """Create one temporary catalog or exempt fixture document."""
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def test_current_catalog(self) -> None:
        self.assertEqual(check_catalog(ROOT), [])

    def test_release_is_independent_of_schema_and_config_versions(self) -> None:
        for kind, path in (
            ("plugin", "plugins/new/plugin.yaml"),
            ("flow", "workflows/demo/workflow.yml"),
            ("module", "workflows/demo/modules/child.yaml"),
            ("flow", "examples/new/workflow.yaml"),
            ("flow", "tests/manual/demo/workflow.yaml"),
            ("plugin", "tests/fixtures/plugins/new/plugin.yaml"),
            ("plugin", ".agents/testing/fixtures/plugins/new/plugin.yaml"),
        ):
            self.write(
                path, f"schema: 'transist.{kind}.v1'\nversion: \"0.1.0\" # release\n"
            )
        self.write("examples/new/config.yaml", "version: v1\n")
        self.assertEqual(check_catalog(self.root), [])

    def test_catalog_release_drift_and_missing_release_are_rejected(self) -> None:
        for version in (
            "v1",
            "v0",
            "1.0.0",
            "0.2.0",
            "0.1.0-beta.1",
            "",
        ):
            with self.subTest(version=version):
                self.write(
                    "workflows/demo/module.yaml",
                    f"schema: transist.module.v1\nversion: {version}\n",
                )
                self.assertTrue(check_catalog(self.root))
        self.write("workflows/demo/module.yaml", "schema: transist.module.v1\n")
        self.assertTrue(check_catalog(self.root))

    def test_every_pin_is_exact_including_unused_dependencies(self) -> None:
        for pin in (
            "1.0.0",
            "v1",
            "^0.1.0",
            "0.1.1",
            "0.2.0",
            "0.1.0-beta.1",
        ):
            with self.subTest(pin=pin):
                self.write(
                    "workflows/demo/workflow.yaml",
                    "schema: transist.flow.v1\nversion: 0.1.0\nplugins:\n"
                    f"  shell: 0.1.0\n  other: {pin}\nsteps: []\n",
                )
                self.assertTrue(check_catalog(self.root))
        self.write(
            "workflows/demo/workflow.yaml",
            "schema: transist.flow.v1\nversion: 0.1.0\nplugins:\n"
            "  shell: '0.1.0' # exact\nsteps: []\n",
        )
        self.assertEqual(check_catalog(self.root), [])

    def test_runtime_release_changes_do_not_change_catalog_policy(self) -> None:
        """Matching or different runtime releases do not change catalog policy."""
        for version in ("0.1.0", "0.13.0", "0.14.0"):
            with self.subTest(runtime=version):
                self.write(
                    "Cargo.toml", f'[workspace.package]\nversion = "{version}"\n'
                )
                self.assertEqual(runtime_version(self.root), version)
                self.write(
                    "workflows/demo/workflow.yaml",
                    "schema: transist.flow.v1\nversion: 0.1.0\nplugins:\n"
                    "  shell: 0.1.0\n",
                )
                self.assertEqual(check_catalog(self.root), [])
                self.write(
                    "workflows/demo/workflow.yaml",
                    "schema: transist.flow.v1\nversion: 0.2.0\nplugins:\n"
                    "  shell: 0.2.0\n",
                )
                failures = check_catalog(self.root)
                self.assertTrue(
                    any("release version must be 0.1.0" in item for item in failures)
                )
                self.assertTrue(
                    any("plugin shell must pin 0.1.0" in item for item in failures)
                )

    def test_inline_pins_do_not_escape_validation(self) -> None:
        self.write(
            "workflows/demo/workflow.yaml",
            "schema: transist.flow.v1\nversion: 0.1.0\nplugins: {shell: 1.0.0}\n",
        )
        self.assertTrue(check_catalog(self.root))

    def test_schema_upgrade_is_not_a_release_sweep(self) -> None:
        self.write(
            "plugins/new/plugin.yaml", "schema: transist.plugin.v2\nversion: 0.1.0\n"
        )
        self.assertTrue(check_catalog(self.root))

    def test_quoted_schema_keys_are_rejected_for_catalog_documents(self) -> None:
        for kind in ("flow", "module", "plugin"):
            for quote in ("'", '"'):
                with self.subTest(kind=kind, quote=quote):
                    self.write(
                        "workflows/demo/workflow.yaml",
                        f"{quote}schema{quote}: transist.{kind}.v1\n"
                        "version: 1.0.0\n",
                    )
                    self.assertTrue(check_catalog(self.root))

    def test_quoted_plugins_keys_cannot_hide_dependency_pins(self) -> None:
        for schema_quote in ("", "'", '"'):
            for plugins_quote in ("'", '"'):
                with self.subTest(schema=schema_quote, plugins=plugins_quote):
                    self.write(
                        "workflows/demo/workflow.yaml",
                        f"{schema_quote}schema{schema_quote}: transist.flow.v1\n"
                        "version: 0.1.0\n"
                        f"{plugins_quote}plugins{plugins_quote}:\n  shell: 1.0.0\n",
                    )
                    self.assertTrue(check_catalog(self.root))

    def test_quoted_catalog_keys_are_rejected_even_with_correct_release(self) -> None:
        for key in ("schema", "version", "plugins"):
            for quote in ("'", '"'):
                with self.subTest(key=key, quote=quote):
                    text = (
                        "schema: transist.flow.v1\nversion: 0.1.0\n"
                        "plugins:\n  shell: 0.1.0\n"
                    ).replace(f"{key}:", f"{quote}{key}{quote}:", 1)
                    self.write("workflows/demo/workflow.yaml", text)
                    failures = check_catalog(self.root)
                    self.assertTrue(
                        any("unquoted catalog keys" in failure for failure in failures)
                    )

    def test_arbitrary_negative_and_historical_fixtures_are_exempt(self) -> None:
        for path in (
            "tests/fixtures/admin_tooling/demo.workflow.yaml",
            "tests/fixtures/yaml_compatibility/workflow.yaml",
            ".agents/testing/fixtures/workflows/negative.yaml",
            "crates/transist-core/tests/fixtures/provenance.yaml",
            "docs/public/releases/historical.yaml",
        ):
            self.write(
                path,
                "schema: transist.flow.v1\nversion: v1\nplugins:\n  shell: 1.0.0\n",
            )
        self.assertEqual(check_catalog(self.root), [])


if __name__ == "__main__":
    unittest.main()
