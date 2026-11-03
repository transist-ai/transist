#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

fail=0

check_forbidden() {
  local scope="$1"
  local pattern="$2"
  local message="$3"
  if [[ ! -e "$scope" ]]; then
    return
  fi
  if rg -n --hidden -g '!target/**' -e "$pattern" "$scope" >/dev/null; then
    rg -n --hidden -g '!target/**' -e "$pattern" "$scope" || true
    echo "$message" >&2
    fail=1
  fi
}

check_forbidden \
  "crates/transist-core/src" \
  'std::process::Command|TcpStream|axum::|clap::' \
  "Boundary violation: transist-core must stay free of host adapter and CLI/server dependencies."

check_forbidden \
  "crates/transist-core/src" \
  'fault_injection|atomic_io|create_dir_all_durable|append_jsonl_durable|write_replace_file|remove_file_anchored|rename_dir_anchored' \
  "Boundary violation: transist-core must not contain concrete local persistence infrastructure."

if ! python3 - <<'PY'
import pathlib
import re
import sys

pattern = re.compile(
    r"write_json_atomic|write_json_atomic_create_new|LocalControlRequestStore|"
    r"\.request_cancel\(|update_run_state\(|store_step_state\(|"
    r"store_step_execution\(|fs::create_dir_all|fs::write\("
)
violations = []
for path in pathlib.Path("src/application/runs").rglob("*.rs"):
    production = path.read_text().split("#[cfg(test)]", 1)[0]
    for line_no, line in enumerate(production.splitlines(), 1):
        if pattern.search(line):
            violations.append(f"{path}:{line_no}: {line}")
if violations:
    print("\n".join(violations), file=sys.stderr)
    print(
        "Boundary violation: application run operations must not write protected runtime state directly; use kernel use cases and ports.",
        file=sys.stderr,
    )
    sys.exit(1)
PY
then
  fail=1
fi

# Keep asset ownership checks focused on production code. Core unit tests may
# create fixture trees, but the runtime implementation must not know the
# plugin manifest format or local prompt-file storage layout.
if ! python3 - <<'PY'
import pathlib
import re
import sys

patterns = (
    re.compile(r"plugin\.yaml|load_manifest|join\(\s*['\"]plugins['\"]\s*\)"),
    re.compile(r"(?is)(?:prompt|prompt_file).{0,300}(?:fs::(?:read|read_to_string)|File::open)|(?:fs::(?:read|read_to_string)|File::open).{0,300}(?:prompt|prompt_file)"),
)
violations = []
for path in pathlib.Path("crates/transist-core/src").rglob("*.rs"):
    source = path.read_text()
    # Test modules are allowed to construct fixtures. Blank their exact
    # brace-balanced spans without truncating production items that follow.
    production = source
    starts = list(re.finditer(r"(?m)^\s*#\[cfg\(test\)\]\s*\n\s*mod\s+\w+\s*\{", source))
    for start in reversed(starts):
        depth = 0
        end = start.end()
        for index in range(start.end() - 1, len(source)):
            if source[index] == "{":
                depth += 1
            elif source[index] == "}":
                depth -= 1
                if depth == 0:
                    end = index + 1
                    break
        production = production[:start.start()] + ("\n" * production[start.start():end].count("\n")) + production[end:]
    for pattern in patterns:
        for match in pattern.finditer(production):
            line_no = production.count("\n", 0, match.start()) + 1
            line = production.splitlines()[line_no - 1]
            violations.append(f"{path}:{line_no}: {line}")
if violations:
    print("\n".join(violations), file=sys.stderr)
    print("Boundary violation: transist-core must not read plugin manifests, prompt files, or construct installed-plugin paths.", file=sys.stderr)
    sys.exit(1)
PY
then
  fail=1
fi

# Result-file verification and invocation-path layout are storage concerns.
# Scan only production portions because unit tests intentionally use temporary
# files and hand-built path fixtures.
if ! python3 - <<'PY'
import pathlib
import re
import sys

def production_source(path):
    source = path.read_text()
    starts = list(re.finditer(r"(?m)^\s*#\[cfg\(test\)\]\s*\n\s*mod\s+\w+\s*\{", source))
    for start in reversed(starts):
        depth = 0
        end = start.end()
        for index in range(start.end() - 1, len(source)):
            if source[index] == "{":
                depth += 1
            elif source[index] == "}":
                depth -= 1
                if depth == 0:
                    end = index + 1
                    break
        source = source[:start.start()] + ("\n" * source[start.start():end].count("\n")) + source[end:]
    return source

checks = {
    pathlib.Path("crates/transist-core/src/kernel/run_execution.rs"): (
        re.compile(r"(?:std::fs|fs)::read\s*\("),
        re.compile(r"(?:std::fs|fs)::canonicalize\s*\("),
    ),
    pathlib.Path("crates/transist-core/src/dispatch.rs"): (
        re.compile(r"\.join\(\s*['\"]artifacts['\"]\s*\)"),
        re.compile(r"\.join\(\s*['\"]evidence['\"]\s*\)"),
        re.compile(r"execution_\{"),
    ),
}
violations = []
for path, patterns in checks.items():
    if not path.exists():
        continue
    source = production_source(path)
    for pattern in patterns:
        for match in pattern.finditer(source):
            line_no = source.count("\n", 0, match.start()) + 1
            line = source.splitlines()[line_no - 1]
            violations.append(f"{path}:{line_no}: {line}")
if violations:
    print("\n".join(violations), file=sys.stderr)
    print(
        "Boundary violation: result-file I/O and invocation path layout must remain behind RunStorePort.",
        file=sys.stderr,
    )
    sys.exit(1)
PY
then
  fail=1
fi

for forbidden_path in \
  crates/transist-core/src/infra/atomic_io.rs \
  crates/transist-core/src/infra/staging.rs \
  crates/transist-core/src/infra/locator.rs \
  crates/transist-core/src/infra/paths.rs; do
  if [[ -e "$forbidden_path" ]]; then
    echo "Boundary violation: concrete persistence file returned to transist-core: $forbidden_path" >&2
    fail=1
  fi
done

if ! rg -n --hidden -g '!target/**' -e 'AdapterRouter::with_builtin_adapters\(' src >/dev/null; then
  echo "Boundary violation: root runtime is not composed through AdapterRouter." >&2
  fail=1
fi

if [[ ! -d crates/transist-plugins ]]; then
  echo "Boundary violation: plugin-management crate is missing." >&2
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "Kernel boundary check passed."
