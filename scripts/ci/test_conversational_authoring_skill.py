#!/usr/bin/env python3
"""Focused regressions for the checked-out conversational authoring instructions."""

import json
from pathlib import Path
import re
import unittest

from check_catalog_versions import runtime_version

ROOT = Path(__file__).resolve().parents[2]
SKILL = ROOT / ".agents/skills/transist-workflow-authoring"


class ConversationalSkillTests(unittest.TestCase):
    """Protect the practical sequence and the execution/integration boundaries."""

    def test_seven_stages_in_order(self) -> None:
        """Clarification precedes contracts; explanation precedes execution consent."""
        source = (SKILL / "SKILL.md").read_text()
        stages = re.findall(r"^(\d)\. \*\*([^*]+)\*\*", source, re.M)
        self.assertEqual(
            stages,
            [
                ("1", "Capture intent."),
                ("2", "Clarify only gaps."),
                ("3", "Inspect real contracts."),
                ("4", "Generate the package."),
                ("5", "Validate read-only source."),
                ("6", "Explain deterministically."),
                ("7", "Present prerequisites and execution boundary separately."),
            ],
        )
        self.assertIn("Do not ask users to choose temporary", source)
        self.assertIn("Report missing integrations", source)
        self.assertIn("unsupported integration inside unrestricted shell", source)
        self.assertIn("An authoring request is not execution consent", source)
        self.assertIn("install, authorize, sign or dispatch", source)
        self.assertIn("not real services or future runs", source)

    def test_current_binding_and_version_contracts(self) -> None:
        """The whole current bundle avoids stale storage/version advice."""
        text = "\n".join(path.read_text() for path in SKILL.rglob("*.md"))
        for required in (
            "deferred_to_dispatch",
            "JSON types",
            "module-call",
            "reviewed_by",
            "gate step ID",
            "max_bytes",
            "max_files",
            "access: update",
            "HTTP/MCP",
            "fresh home",
            "Signatures bind bytes",
            runtime_version(ROOT),
            "--compare-to",
            "compatibility-and-stability.md",
            "--inputs-file",
            "synthetic",
            "prompt_file",
        ):
            with self.subTest(required=required):
                self.assertIn(required, text)
        for stale in (
            "requires `version: v1`",
            "require a bumped, signed",
            "Use action-supported workspace-relative files for large",
            "default to repository-local",
            "Existing ticket workflows migrate separately",
        ):
            with self.subTest(stale=stale):
                self.assertNotIn(stale, text)

    def test_references_and_public_entry_points(self) -> None:
        """Keep skill links and public entry points reachable."""
        for path in SKILL.rglob("*.md"):
            for link in re.findall(r"\[[^\]]+\]\(([^)]+)\)", path.read_text()):
                if not link.startswith(("https://", "http://", "#")):
                    target = (path.parent / link.split("#")[0]).resolve()
                    self.assertTrue(target.is_relative_to(SKILL))
                    self.assertTrue(target.is_file(), link)
        guide = (
            ROOT
            / "docs/public/guides/authoring/ai-assisted-authoring-with-repo-skills.md"
        )
        text = guide.read_text()
        for required in (
            "transist-workflow-authoring/SKILL.md",
            "../../../../workflows",
            "../../../../plugins/shell/plugin.yaml",
            "scripts/ci/run_shipped_workflows.sh",
            "Execution is a separate decision",
            "--compare-to",
            runtime_version(ROOT),
            "compatibility-and-stability.md",
        ):
            self.assertIn(required, text)
        for link in re.findall(r"\[[^\]]+\]\(([^)]+)\)", text):
            if not link.startswith(("https://", "http://", "#")):
                target = (guide.parent / link.split("#")[0]).resolve()
                self.assertTrue(target.is_relative_to(ROOT))
                self.assertTrue(target.exists(), link)
        self.assertIn(
            "Conversational Authoring", (ROOT / "docs/GLOSSARY.md").read_text()
        )
        self.assertIn(
            "Managed Documents",
            (
                ROOT / "docs/public/handbook/02-everyday-usage/08-authoring-patterns.md"
            ).read_text(),
        )

    def test_related_skills_do_not_restore_legacy_delivery_handoffs(self) -> None:
        """Setup/plugin guidance must not contradict managed-document authoring."""
        for name in ("transist-plugin-authoring", "transist-workflow-setup"):
            with self.subTest(skill=name):
                source = (ROOT / ".agents/skills" / name / "SKILL.md").read_text()
                normalized = " ".join(source.lower().split())
                self.assertIn("managed documents", normalized)
                self.assertNotIn("<repo_path>/.transist/<workflow>/<ticket>/", source)
                self.assertNotIn("default to repository-local", source)
                self.assertIn("0.1.0", source)
                self.assertIn(runtime_version(ROOT), source)

    def test_authoring_examples_use_bundled_contracts(self) -> None:
        """Keep implicit unbundled action dependencies out of active guidance."""
        guide = (
            ROOT
            / "docs/public/guides/authoring/ai-assisted-authoring-with-repo-skills.md"
        )
        for path in [guide, *SKILL.rglob("*.md")]:
            source = path.read_text()
            for action in (
                "openai.compatible/chat_complete",
                "github.cli/pr_create",
                "shell/read_file",
            ):
                with self.subTest(path=path, action=action):
                    self.assertNotIn(action, source)

        reference = (SKILL / "references/conversational-authoring.md").read_text()
        for required in (
            "plugins/shell/plugin.yaml",
            ".agents/testing/e2e_suites/managed-documents/plugin/plugin.yaml",
            "process:fixture.documents/call",
            "not a production",
            "report it as missing",
        ):
            self.assertIn(required, reference)
        for manifest, action in (
            (ROOT / "plugins/shell/plugin.yaml", "echo"),
            (
                ROOT
                / ".agents/testing/e2e_suites/managed-documents/plugin/plugin.yaml",
                "call",
            ),
        ):
            self.assertIn(f"- name: {action}\n", manifest.read_text())
        fixture = ROOT / ".agents/testing/e2e_suites/managed-documents/plugin"
        schema = json.loads((fixture / "input.json").read_text())
        self.assertEqual(
            schema["properties"]["ops"]["items"]["required"], ["path", "op"]
        )

    def test_active_runtime_policy_and_examples_match_workspace(self) -> None:
        """Match active guidance to the workspace without sweeping old evidence."""
        version = runtime_version(ROOT)
        reference = ROOT / "docs/public/reference"
        self.assertIn(
            f"Transist `{version}`",
            (reference / "compatibility-and-stability.md").read_text(),
        )
        compatibility = (reference / "compatibility-and-stability.md").read_text()
        for requirement in (
            "Start evaluation in a fresh runtime home",
            "Never overwrite",
            "pinned payloads",
            "Resume retains committed documents",
            "replay creates a fresh document namespace",
        ):
            self.assertIn(requirement, compatibility)
        self.assertIn(
            f'"runtime": {{"version": "{version}"}}',
            (reference / "app.md").read_text(),
        )
        lifecycle = (
            ROOT
            / ".agents/skills/transist-workflow-setup/references"
            / "workflow-setup-lifecycle.md"
        )
        self.assertIn(version, lifecycle.read_text())


if __name__ == "__main__":
    unittest.main()
