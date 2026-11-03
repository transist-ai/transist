#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

facade="src/lib.rs"
if [[ ! -f "$facade" ]]; then
  echo "Root facade boundary violation: missing $facade." >&2
  exit 1
fi

if ! python3 - "$facade" <<'PY'
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
source = path.read_text(encoding="utf-8")
exports = re.compile(r"\bpub\s+use\b(?P<body>.*?);", re.DOTALL)

checks = (
    (
        re.compile(r"\bplugin_dir_sha256\b"),
        "plugin_dir_sha256 must be imported from transist_plugins by its callers, not re-exported by the root facade.",
    ),
    (
        re.compile(r"(?<![A-Za-z0-9_])Local[A-Z][A-Za-z0-9_]*\b"),
        "concrete Local* implementation types must not be re-exported by the root facade.",
    ),
    (
        re.compile(r"\btransist_compiler\s*::"),
        "compiler implementation helpers must not be re-exported by the root facade.",
    ),
    (
        re.compile(r"\btransist_storage_local\s*::"),
        "storage-local helpers must not be re-exported by the root facade.",
    ),
    (
        re.compile(
            r"\btransist_adapters\s*::|\b(?:AdapterRouter|ProcessAdapter|HttpAdapter|McpAdapter|with_builtin_adapters)\b"
        ),
        "adapter implementations and constructors must not be re-exported by the root facade.",
    ),
)

violations = []
for match in exports.finditer(source):
    body = match.group("body")
    line = source.count("\n", 0, match.start()) + 1
    for pattern, message in checks:
        if pattern.search(body):
            statement = " ".join(body.split())
            violations.append(f"{path}:{line}: pub use{statement};\n{message}")

if violations:
    print("\n".join(violations), file=sys.stderr)
    sys.exit(1)
PY
then
  exit 1
fi

echo "Root facade boundary check passed."
