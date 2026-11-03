#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Tests for check_unsafe_policy.py.

Creates temporary Rust files with violations and verifies the checker
catches each one. Also verifies the real workspace passes.
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ci" / "check_unsafe_policy.py"


def load_checker():
    spec = importlib.util.spec_from_file_location("unsafe_policy", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def run_real_check() -> int:
    import subprocess
    result = subprocess.run(
        [sys.executable, str(SCRIPT)],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    return result.returncode


def assert_allowlist_catches(source: str, label: str) -> None:
    """Create a temp file outside the allowlist and assert the checker flags it."""
    mod = load_checker()
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        fake_dir = tmp / "crates" / "fake-crate" / "src"
        fake_dir.mkdir(parents=True)
        fake = fake_dir / "lib.rs"
        fake.write_text(source)

        original_collect = mod.collect_rust_files
        original_root = mod.ROOT
        original_deny = mod.check_workspace_deny

        def fake_collect():
            return [fake]

        mod.collect_rust_files = fake_collect
        mod.ROOT = tmp
        mod.WORKSPACE_TOML = tmp / "Cargo.toml"
        mod.check_workspace_deny = lambda v: None
        try:
            violations = []
            mod.check_allowlist(violations)
            assert violations, f"{label}: allow(unsafe_code) outside allowlist must be caught"
        finally:
            mod.collect_rust_files = original_collect
            mod.ROOT = original_root
            mod.check_workspace_deny = original_deny
    print(f"PASS: {label}")


def safety_violations(source: str) -> list[str]:
    """Run the safety-comment check against one temporary Rust source file."""
    mod = load_checker()
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        fake_file = tmp / "lib.rs"
        fake_file.write_text(source)
        original_collect = mod.collect_rust_files
        original_root = mod.ROOT

        def fake_collect():
            return [fake_file]

        mod.collect_rust_files = fake_collect
        mod.ROOT = tmp
        try:
            violations: list[str] = []
            mod.check_safety_comments(violations)
            return violations
        finally:
            mod.collect_rust_files = original_collect
            mod.ROOT = original_root


def assert_safety_catches(source: str, label: str) -> None:
    """Assert that a temporary Rust source violates the safety-comment policy."""
    violations = safety_violations(source)
    assert violations, f"{label}: safety policy violation must be caught"
    print(f"PASS: {label}")


def test_scoped_allow_rejected():
    """A scoped #[allow(unsafe_code)] outside the allowlist must be caught."""
    assert_allowlist_catches(
        "#[allow(unsafe_code)]\nfn bad() { unsafe { } }\n",
        "scoped allow rejected",
    )


def test_multi_lint_allow_rejected():
    """#[allow(unsafe_code, dead_code)] must be caught."""
    assert_allowlist_catches(
        "#[allow(unsafe_code, dead_code)]\nfn bad() { unsafe { } }\n",
        "multi-lint allow rejected",
    )


def test_multiline_allow_rejected():
    """A multiline #[allow(...)] listing unsafe_code must be caught."""
    assert_allowlist_catches(
        "#[allow(\n    dead_code,\n    unsafe_code,\n)]\nfn bad() { unsafe { } }\n",
        "multiline allow rejected",
    )


def test_inner_multiline_allow_rejected():
    """A crate-level #![allow(...)] listing unsafe_code must be caught."""
    assert_allowlist_catches(
        "#![allow(\n    unsafe_code,\n    dead_code,\n)]\nfn main() {}\n",
        "inner multiline allow rejected",
    )


def test_cfg_attr_allow_rejected():
    """#[cfg_attr(unix, allow(unsafe_code))] must be caught."""
    assert_allowlist_catches(
        "#[cfg_attr(unix, allow(unsafe_code))]\nfn bad() { unsafe { } }\n",
        "cfg_attr allow rejected",
    )


def test_cfg_attr_multiline_allow_rejected():
    """A multiline cfg_attr(... allow(dead_code, unsafe_code)) must be caught."""
    assert_allowlist_catches(
        "#[cfg_attr(\n    unix,\n    allow(dead_code, unsafe_code),\n)]\nfn bad() { unsafe { } }\n",
        "cfg_attr multiline allow rejected",
    )


def test_allow_in_string_not_matched():
    """The attribute text inside a doc string must not count."""
    mod = load_checker()
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        fake = tmp / "lib.rs"
        fake.write_text('//! doc: #[allow(unsafe_code)] is documented here\nfn ok() {}\n')
        original_collect = mod.collect_rust_files
        original_root = mod.ROOT

        def fake_collect():
            return [fake]

        mod.collect_rust_files = fake_collect
        mod.ROOT = tmp
        try:
            violations = []
            mod.check_allowlist(violations)
            assert not violations, "allow(unsafe_code) inside a comment must not count"
        finally:
            mod.collect_rust_files = original_collect
            mod.ROOT = original_root
    print("PASS: allow in comment not matched")


def test_unsafe_trait_detected():
    """unsafe trait must be recognized as an unsafe construct."""
    mod = load_checker()
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        fake_file = tmp / "lib.rs"
        fake_file.write_text("unsafe trait Bad {}\n")
        # Patch to scan only this file.
        original_collect = mod.collect_rust_files

        def fake_collect():
            return [fake_file]

        mod.collect_rust_files = fake_collect
        original_root = mod.ROOT
        mod.ROOT = tmp
        try:
            violations = []
            mod.check_safety_comments(violations)
            assert violations, "unsafe trait without SAFETY comment must be caught"
        finally:
            mod.collect_rust_files = original_collect
            mod.ROOT = original_root
    print("PASS: unsafe trait detected")


def test_safety_comment_distance_limit():
    """A SAFETY comment more than 10 lines away must not count."""
    mod = load_checker()
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        fake_file = tmp / "lib.rs"
        # SAFETY comment 15 lines before the unsafe block — too far.
        lines = ["// SAFETY: valid\n"]
        lines += ["\n"] * 14
        lines += ["unsafe { }\n"]
        fake_file.write_text("".join(lines))
        original_collect = mod.collect_rust_files

        def fake_collect():
            return [fake_file]

        mod.collect_rust_files = fake_collect
        original_root = mod.ROOT
        mod.ROOT = tmp
        try:
            violations = []
            mod.check_safety_comments(violations)
            assert violations, "distant SAFETY comment must not count"
        finally:
            mod.collect_rust_files = original_collect
            mod.ROOT = original_root
    print("PASS: SAFETY distance limit enforced")


def test_missing_safety_comment_rejected():
    assert_safety_catches("unsafe { }\n", "missing SAFETY comment rejected")


def test_empty_safety_comment_rejected():
    assert_safety_catches(
        "// SAFETY:\nunsafe { }\n",
        "empty SAFETY comment rejected",
    )


def test_separated_safety_comment_rejected():
    assert_safety_catches(
        "// SAFETY: explanation\n\nunsafe { }\n",
        "separated SAFETY comment rejected",
    )


def test_reused_safety_comment_rejected():
    assert_safety_catches(
        "// SAFETY: one explanation\nlet _ = (unsafe { }, unsafe { });\n",
        "reused SAFETY comment rejected",
    )


def test_unapproved_safety_marker_rejected():
    assert_safety_catches(
        "// SAFETY explanation without the approved marker\nunsafe { }\n",
        "unapproved SAFETY marker rejected",
    )
    assert_safety_catches(
        "// SAFETY: approved explanation\n// SAFETY malformed\nunsafe { }\n",
        "mixed SAFETY markers rejected",
    )


def test_string_literal_not_matched():
    """The word 'unsafe' inside a string literal must not trigger."""
    mod = load_checker()
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        fake_file = tmp / "lib.rs"
        fake_file.write_text('let s = "unsafe { not real }";\n')
        original_collect = mod.collect_rust_files

        def fake_collect():
            return [fake_file]

        mod.collect_rust_files = fake_collect
        original_root = mod.ROOT
        mod.ROOT = tmp
        try:
            violations = []
            mod.check_safety_comments(violations)
            assert not violations, "string literal must not match unsafe pattern"
        finally:
            mod.collect_rust_files = original_collect
            mod.ROOT = original_root
    print("PASS: string literal not matched")


def test_real_workspace_passes():
    rc = run_real_check()
    assert rc == 0, f"real workspace must pass the unsafe policy check, got {rc}"
    print("PASS: real workspace passes")


if __name__ == "__main__":
    test_scoped_allow_rejected()
    test_multi_lint_allow_rejected()
    test_multiline_allow_rejected()
    test_inner_multiline_allow_rejected()
    test_cfg_attr_allow_rejected()
    test_cfg_attr_multiline_allow_rejected()
    test_allow_in_string_not_matched()
    test_unsafe_trait_detected()
    test_safety_comment_distance_limit()
    test_missing_safety_comment_rejected()
    test_empty_safety_comment_rejected()
    test_separated_safety_comment_rejected()
    test_reused_safety_comment_rejected()
    test_unapproved_safety_marker_rejected()
    test_string_literal_not_matched()
    test_real_workspace_passes()
    print("\nAll unsafe-policy tests passed.")
