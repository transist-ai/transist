#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Reject retired invocation and subflow fields in authored workflow sources."""

from __future__ import annotations

import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]
FLOW_SCHEMA = re.compile(r"^\s*schema:\s*['\"]?transist\.flow\.v1['\"]?\s*$", re.MULTILINE)
REMOVED_FIELD = re.compile(
    r"^(?P<indent>\s*)(?P<field>adapter|plugin|plugin_version|action|execution_kind|subflow):",
    re.MULTILINE,
)

def main() -> int:
    failures: list[str] = []
    for path in sorted(ROOT.rglob("*.yaml")) + sorted(ROOT.rglob("*.yml")):
        relative = path.relative_to(ROOT)
        text = path.read_text(encoding="utf-8")
        if not FLOW_SCHEMA.search(text):
            continue
        for match in REMOVED_FIELD.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            failures.append(f"{relative}:{line}: removed authored field '{match.group('field')}'")

    if failures:
        print("Retired authored fields are not allowed in maintained transist.flow.v1 sources:", file=sys.stderr)
        print("\n".join(failures), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
