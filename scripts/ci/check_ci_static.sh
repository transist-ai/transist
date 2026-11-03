#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Cheap, installation-free CI syntax, pin, and orchestration checks. No Cargo.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"
if [[ -n "${CI_TOOL_ROOT:-}" ]]; then
  export PATH="$CI_TOOL_ROOT/bin:$PATH"
fi
python3 - <<'PY'
import ast
from pathlib import Path
import subprocess

paths = subprocess.check_output(["git", "ls-files", "-z", "*.sh", "*.py"]).split(b"\0")
for raw in paths:
    if not raw:
        continue
    path = Path(raw.decode("utf-8"))
    if path.suffix == ".py":
        ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    else:
        subprocess.run(["bash", "-n", str(path)], check=True)
PY
bash scripts/ci/check_supply_chain_pins.sh
python3 scripts/ci/test_ci_contracts.py
python3 scripts/ci/test_ci_caches.py
python3 scripts/ci/test_candidate_worktree.py
python3 scripts/ci/test_commit_signoffs.py
python3 scripts/ci/test_ci_local.py
echo "CI static checks passed."
