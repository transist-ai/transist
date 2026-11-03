#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Workspace dependency centralisation policy gate.

Rejects:
  1. Any dependency declared with an inline ``version``, ``git``,
     ``registry``, or ``path`` override when its canonical package is
     offered by ``[workspace.dependencies]``.
  2. Any dependency that appears (by canonical package name)
     in two or more member manifests but is **not** declared in
     ``[workspace.dependencies]``.

Members are derived from ``cargo metadata --no-deps`` so new workspace
members are always covered.

Exit code 0 on success, 1 on policy violation.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    print("FAIL: this check requires Python 3.11+ (tomllib).", file=sys.stderr)
    sys.exit(1)

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_TOML = ROOT / "Cargo.toml"

# Dependency tables to inspect, including target-specific ones.
DEP_TABLE_NAMES = ["dependencies", "dev-dependencies", "build-dependencies"]


def load_toml(path: Path) -> dict:
    with open(path, "rb") as fh:
        return tomllib.load(fh)


def iter_dep_tables(toml: dict):
    """Yield (table_label, deps_dict) for normal and target-specific dep tables."""
    for name in DEP_TABLE_NAMES:
        deps = toml.get(name, {})
        if deps:
            yield name, deps
    # Target-specific: [target.'cfg(...)'.dependencies] etc.
    targets = toml.get("target", {})
    if isinstance(targets, dict):
        for target_key, target_val in targets.items():
            if not isinstance(target_val, dict):
                continue
            for dep_kind in DEP_TABLE_NAMES:
                deps = target_val.get(dep_kind, {})
                if deps:
                    yield f"target.{target_key}.{dep_kind}", deps


def canonical_name(name: str, spec: object) -> str:
    """Resolve the canonical package name, handling renamed deps via `package`."""
    if isinstance(spec, dict) and "package" in spec:
        return str(spec["package"])
    return name


def is_inline_override(spec: object) -> bool:
    """True when a dependency spec carries its own version/git/registry/path
    instead of ``workspace = true``."""
    if isinstance(spec, str):
        return True
    if isinstance(spec, dict):
        if spec.get("workspace") is True:
            return False
        # Any of these keys means it's not a pure workspace reference.
        return any(
            k in spec
            for k in ("version", "git", "registry", "path")
            if spec.get(k) is not None
        ) or "workspace" not in spec
    return False


def derive_members() -> list[tuple[str, Path, dict]]:
    """Use cargo metadata to get the authoritative member list."""
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    if result.returncode != 0:
        print("FAIL: cargo metadata failed:", file=sys.stderr)
        print(result.stderr, file=sys.stderr)
        sys.exit(1)

    data = json.loads(result.stdout)
    members = []
    for pkg in sorted(data.get("packages", []), key=lambda p: p["name"]):
        manifest_path = Path(pkg["manifest_path"])
        toml = load_toml(manifest_path)
        members.append((pkg["name"], manifest_path.parent, toml))
    return members


def main() -> int:
    workspace = load_toml(WORKSPACE_TOML)
    ws_deps: dict[str, object] = workspace.get("workspace", {}).get(
        "dependencies", {}
    )
    ws_dep_names = set(ws_deps.keys())
    ws_canonical_names = {
        canonical_name(name, spec) for name, spec in ws_deps.items()
    }

    member_packages = derive_members()
    if not member_packages:
        print("FAIL: cargo metadata returned no packages", file=sys.stderr)
        return 1

    violations: list[str] = []

    # Rule 1: centralised dep referenced with inline override.
    for pkg_name, _dir, toml in member_packages:
        for table_label, deps in iter_dep_tables(toml):
            for name, spec in deps.items():
                canon = canonical_name(name, spec)
                if canon in ws_canonical_names and is_inline_override(spec):
                    violations.append(
                        f"{pkg_name}: dependency '{name}' ({canon}) is centralised in "
                        f"[workspace.dependencies] but declared with an inline "
                        f"override in [{table_label}] — use `workspace = true`."
                    )

    # Rule 2: external dep repeated across 2+ members but not centralised.
    occurrence: dict[str, set[str]] = {}
    for pkg_name, _dir, toml in member_packages:
        for _table_label, deps in iter_dep_tables(toml):
            for name, spec in deps.items():
                canon = canonical_name(name, spec)
                if canon in ws_canonical_names:
                    continue
                occurrence.setdefault(canon, set()).add(pkg_name)

    for canon, pkgs in sorted(occurrence.items()):
        if len(pkgs) >= 2:
            violations.append(
                f"dependency '{canon}' appears in {len(pkgs)} members "
                f"({', '.join(sorted(pkgs))}) but is not declared in "
                f"[workspace.dependencies] — centralise it."
            )

    if violations:
        print("Workspace dependency centralisation policy violations:", file=sys.stderr)
        for v in violations:
            print(f"  - {v}", file=sys.stderr)
        return 1

    print(
        f"Workspace dependency centralisation policy passed: "
        f"{len(ws_dep_names)} centralised dependencies, "
        f"{len(member_packages)} members checked."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
