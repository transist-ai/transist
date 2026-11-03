#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Tests for check_workspace_dependency_policy.py.

Creates temporary broken manifests and verifies the checker catches each
violation. Also verifies the real workspace passes.
"""

from __future__ import annotations

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ci" / "check_workspace_dependency_policy.py"


def load_checker():
    spec = importlib.util.spec_from_file_location("policy", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def run_real_check() -> int:
    result = subprocess.run(
        [sys.executable, str(SCRIPT)],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    return result.returncode


def run_check_against(workdir: Path) -> int:
    """Run the checker logic against a fake workspace rooted at workdir."""
    # We simulate by monkey-patching ROOT and WORKSPACE_TOML in the module.
    mod = load_checker()
    mod.ROOT = workdir
    mod.WORKSPACE_TOML = workdir / "Cargo.toml"

    # Patch derive_members to avoid calling cargo metadata on the fake dir.
    original_derive = mod.derive_members

    def fake_derive():
        members = []
        for d in sorted((workdir).iterdir()):
            manifest = d / "Cargo.toml"
            if manifest.exists():
                toml = mod.load_toml(manifest)
                pkg = toml.get("package", {}).get("name", d.name)
                members.append((pkg, d, toml))
        # Also check root
        root_manifest = workdir / "Cargo.toml"
        if root_manifest.exists():
            toml = mod.load_toml(root_manifest)
            members.append(("root-crate", workdir, toml))
        return members

    mod.derive_members = fake_derive
    try:
        return mod.main()
    finally:
        mod.derive_members = original_derive


def make_workspace(tmp: Path, members: dict[str, str]) -> None:
    """Create a fake workspace. `members` maps crate-name -> Cargo.toml content."""
    member_list = ["."] + list(members.keys())
    root_toml = (
        "[workspace]\n"
        f'members = {member_list}\n\n'
        "[workspace.dependencies]\n"
        'serde = "1.0"\n'
    )
    (tmp / "Cargo.toml").write_text(root_toml)
    for name, content in members.items():
        crate_dir = tmp / name
        crate_dir.mkdir(parents=True, exist_ok=True)
        (crate_dir / "Cargo.toml").write_text(content)


def test_inline_version_rejected():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'serde = "1.0"\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 1, "inline version for centralised dep must fail"
    print("PASS: inline version rejected")


def test_target_specific_inline_rejected():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[target.'cfg(unix)'.dependencies]\n"
                    'serde = "1.0"\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 1, "target-specific inline version must fail"
    print("PASS: target-specific inline rejected")


def test_repeated_non_centralised_rejected():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'anyhow = "1.0"\n'
                ),
                "crate-b": (
                    '[package]\nname = "crate-b"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'anyhow = "1.0"\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 1, "repeated non-centralised dep must fail"
    print("PASS: repeated non-centralised rejected")


def test_renamed_dep_detected():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'renamed = { version = "1.0", package = "anyhow" }\n'
                ),
                "crate-b": (
                    '[package]\nname = "crate-b"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'anyhow = "1.0"\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 1, "renamed dep must be detected as repeated"
    print("PASS: renamed dep detected")


def test_renamed_centralised_dep_rejected():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'renamed = { version = "1.0", package = "serde" }\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 1, "renamed centralised dep must use workspace = true"
    print("PASS: renamed centralised dep rejected")


def test_single_path_only_dep_allowed():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'local-crate = { path = "../local-crate" }\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 0, "path-only dep must be allowed"
    print("PASS: path-only dep allowed")


def test_repeated_path_only_dep_rejected():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'local-crate = { path = "../local-crate" }\n'
                ),
                "crate-b": (
                    '[package]\nname = "crate-b"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'local-crate = { path = "../local-crate" }\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 1, "repeated path-only dep must be centralised"
    print("PASS: repeated path-only dep rejected")


def test_workspace_true_allowed():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        make_workspace(
            tmp,
            {
                "crate-a": (
                    '[package]\nname = "crate-a"\nversion = "0.1.0"\n'
                    "[dependencies]\n"
                    'serde = { workspace = true }\n'
                ),
            },
        )
        rc = run_check_against(tmp)
        assert rc == 0, "workspace = true must pass"
    print("PASS: workspace=true allowed")


def test_real_workspace_passes():
    rc = run_real_check()
    assert rc == 0, f"real workspace must pass the policy check, got {rc}"
    print("PASS: real workspace passes")


if __name__ == "__main__":
    os.chdir(ROOT)
    test_inline_version_rejected()
    test_target_specific_inline_rejected()
    test_repeated_non_centralised_rejected()
    test_renamed_dep_detected()
    test_renamed_centralised_dep_rejected()
    test_single_path_only_dep_allowed()
    test_repeated_path_only_dep_rejected()
    test_workspace_true_allowed()
    test_real_workspace_passes()
    print("\nAll dependency-policy tests passed.")
