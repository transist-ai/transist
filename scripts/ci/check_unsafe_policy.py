#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Unsafe-code policy gate.

Enforces:
  1. ``unsafe_code = "deny"`` is set in ``[workspace.lints.rust]``.
  2. Any ``allow(unsafe_code)`` attribute (file-level, item-level, or via
     ``cfg_attr``) appears only in an approved allowlist of OS-integration
     modules and test helpers.
   3. Every ``unsafe {`` / ``unsafe fn`` / ``unsafe impl`` / ``unsafe trait``
      / ``unsafe extern`` has one immediately preceding, contiguous, non-empty
      ``// SAFETY:`` explanation (block-specific justification).

Exit code 0 on success, 1 on policy violation.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    print("FAIL: this check requires Python 3.11+ (tomllib).", file=sys.stderr)
    sys.exit(1)

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_TOML = ROOT / "Cargo.toml"

# Approved files that may contain ``allow(unsafe_code)``.
# Production OS-integration modules with heavy libc usage.
APPROVED_ALLOW_FILES = {
    "crates/transist-sandbox-launcher/src/main.rs",
    "crates/transist-adapters/src/os_process.rs",
    "crates/transist-adapters/src/preflight/mod.rs",
    "crates/transist-adapters/src/process_runtime.rs",
    "crates/transist-adapters/src/filesystem_sandbox.rs",
    "crates/transist-cli/src/runner.rs",
    "crates/transist-cli/src/terminal.rs",
    "crates/transist-cli/src/ui/fullscreen/screens.rs",
    "crates/transist-storage-local/src/atomic_io.rs",
    "crates/transist-storage-local/src/invocation_staging.rs",
    "crates/transist-storage-local/src/run_paths.rs",
    "crates/transist-inspection/src/secure_file.rs",
    "crates/transist-plugins/src/staged.rs",
    "crates/transist-compiler/src/digest.rs",
    "src/composition/prompt_files.rs",
    "transist-admin/src/package.rs",
    # Narrow function-level allows for single-syscall wrappers.
    "crates/transist-cli/src/commands/dispatch.rs",
    "crates/transist-cli/src/execution/cleanup_inspection.rs",
    "crates/transist-cli/src/main.rs",
    "crates/transist-cli/src/pager.rs",
    "crates/transist-storage-local/src/lease_store.rs",
    "crates/transist-storage-local/src/util.rs",
    # Test modules with env mutation.
    "src/home/mod.rs",
    "src/application/runs/gates.rs",
}

# Test files that may use ``allow(unsafe_code)``.
APPROVED_TEST_FILES = {
    "crates/transist-adapters/tests/active_cancel.rs",
    "crates/transist-adapters/tests/host_adapters.rs",
    "crates/transist-cli/tests/support/mod.rs",
    "tests/e2e_plugin_runtime.rs",
    "tests/secret_persistence_guardrails.rs",
}

# Scan paths for unsafe usage.
SCAN_DIRS = [
    "crates",
    "src",
    "tests",
    "transist-admin",
]

# Matches all unsafe constructs.
UNSAFE_RE = re.compile(r"\bunsafe\s+(\{|\bfn\b|\bimpl\b|\btrait\b|\bextern\b)")
SAFETY_MARKER_RE = re.compile(r"^\s*//\s*SAFETY:(.*)$")
SAFETY_WORD_RE = re.compile(r"^\s*//+\s*SAFETY\b", re.IGNORECASE)


def skip_line_comment(source: str, i: int) -> int:
    """Return index after the line comment starting at i (source[i] == '/' and
    source[i+1] == '/')."""
    nl = source.find("\n", i)
    return len(source) if nl == -1 else nl + 1


def skip_block_comment(source: str, i: int) -> int:
    """Return index after the block comment starting at i (source[i:i+2] == '/*')."""
    end = source.find("*/", i + 2)
    return len(source) if end == -1 else end + 2


def skip_string(source: str, i: int) -> int:
    """Return index after a `"..."` string literal starting at i (source[i] == '"')."""
    j = i + 1
    n = len(source)
    while j < n:
        if source[j] == "\\":
            j += 2
            continue
        if source[j] == '"':
            return j + 1
        j += 1
    return n


def skip_raw_string(source: str, i: int, hashes: int) -> int:
    """Return index after a raw string literal whose opening quote is at i and
    which carries `hashes` leading `#` markers."""
    closing = '"' + ("#" * hashes)
    idx = source.find(closing, i + 1)
    return len(source) if idx == -1 else idx + len(closing)


def try_prefix_string(source: str, i: int) -> int:
    """If index i starts a byte/raw/char string prefix (b, r, br, c), return the
    index just past the literal; otherwise return i unchanged."""
    n = len(source)
    if source[i] == "r" and i + 1 < n and source[i + 1] == '"':
        return skip_raw_string(source, i + 1, 0)
    if source[i] == "r" and i + 1 < n and source[i + 1] == "#":
        hashes = 0
        j = i + 1
        while j < n and source[j] == "#":
            hashes += 1
            j += 1
        if j < n and source[j] == '"':
            return skip_raw_string(source, j, hashes)
        return i
    if source[i] in ("b", "c") and i + 1 < n and source[i + 1] == '"':
        return skip_string(source, i + 1)
    if (
        source[i] == "b"
        and i + 1 < n
        and source[i + 1] == "r"
        and i + 2 < n
        and source[i + 2] == '"'
    ):
        return skip_raw_string(source, i + 2, 0)
    return i


def skip_char_literal(source: str, i: int) -> int:
    """Skip a simple Rust char literal (e.g. 'a', '\\n', '#') starting at i
    (source[i] == \"'\"). Lifetimes ('a) are left untouched."""
    n = len(source)
    j = i + 1
    if j >= n:
        return i + 1
    if source[j] == "\\":
        j += 2
    else:
        j += 1
    if j < n and source[j] == "'":
        return j + 1
    return i + 1


def find_attribute_end(source: str, i: int) -> int | None:
    """Return index of the closing `]` of the attribute whose body starts at i,
    respecting nested brackets, parens, strings, and comments."""
    depth = 0
    j = i
    n = len(source)
    while j < n:
        c = source[j]
        if c == "/":
            if j + 1 < n and source[j + 1] == "/":
                j = skip_line_comment(source, j)
                continue
            if j + 1 < n and source[j + 1] == "*":
                j = skip_block_comment(source, j)
                continue
        elif c == '"':
            j = skip_string(source, j)
            continue
        elif c in "([{":
            depth += 1
        elif c == "]":
            if depth == 0:
                return j
            depth -= 1
        elif c in ")}":
            depth -= 1
        j += 1
    return None


def iter_attributes(source: str):
    """Yield the normalized body text of every `#[...]` and `#![...]` attribute
    in Rust source, skipping comments and string literals."""
    i = 0
    n = len(source)
    while i < n:
        c = source[i]
        if c == "/":
            if i + 1 < n and source[i + 1] == "/":
                i = skip_line_comment(source, i)
                continue
            if i + 1 < n and source[i + 1] == "*":
                i = skip_block_comment(source, i)
                continue
            i += 1
            continue
        if c == '"':
            i = skip_string(source, i)
            continue
        if c in ("r", "b", "c"):
            nxt = try_prefix_string(source, i)
            if nxt != i:
                i = nxt
                continue
            i += 1
            continue
        if c == "'":
            i = skip_char_literal(source, i)
            continue
        if c == "#" and i + 1 < n:
            start = None
            if source[i + 1] == "[":
                start = i + 2
            elif source[i + 1] == "!" and i + 2 < n and source[i + 2] == "[":
                start = i + 3
            if start is not None:
                end = find_attribute_end(source, start)
                if end is None:
                    i = n
                    continue
                yield source[start:end]
                i = end + 1
                continue
        i += 1


def match_paren(s: str, i: int) -> int | None:
    """Return index of the closing `)` matching the opening `(` at i."""
    depth = 0
    j = i
    while j < len(s):
        c = s[j]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return j
        j += 1
    return None


def split_top_level(s: str) -> list[str]:
    """Split on commas at bracket-depth 0."""
    parts = []
    depth = 0
    cur: list[str] = []
    for c in s:
        if c in "([{":
            depth += 1
            cur.append(c)
        elif c in ")]}":
            depth -= 1
            cur.append(c)
        elif c == "," and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(c)
    if cur:
        parts.append("".join(cur))
    return parts


def strip_strings(s: str) -> str:
    """Replace string literals in an attribute body with spaces."""
    out = []
    i = 0
    n = len(s)
    while i < n:
        if s[i] == '"':
            end = skip_string(s, i)
            out.append(" " * (end - i))
            i = end
            continue
        out.append(s[i])
        i += 1
    return "".join(out)


def contains_allow_unsafe(attr: str) -> bool:
    """True if the attribute body contains an `allow(...)` whose argument list
    names `unsafe_code`, including nested inside `cfg_attr(...)` and multiline
    or multi-lint forms."""
    body = strip_strings(attr)
    i = 0
    n = len(body)
    while i < n:
        m = re.search(r"\ballow\b", body[i:])
        if not m:
            return False
        pos = i + m.start()
        j = i + m.end()
        while j < n and body[j].isspace():
            j += 1
        if j >= n or body[j] != "(":
            i = pos + 1
            continue
        end = match_paren(body, j)
        if end is None:
            return False
        inner = body[j + 1 : end]
        for arg in split_top_level(inner):
            if arg.strip() == "unsafe_code":
                return True
        i = end + 1
    return False


def load_workspace_lints() -> dict:
    with open(WORKSPACE_TOML, "rb") as fh:
        return tomllib.load(fh)


def check_workspace_deny(violations: list[str]) -> None:
    ws = load_workspace_lints()
    rust_lints = ws.get("workspace", {}).get("lints", {}).get("rust", {})
    if rust_lints.get("unsafe_code") != "deny":
        violations.append('[workspace.lints.rust] must set unsafe_code = "deny"')


def collect_rust_files() -> list[Path]:
    files = []
    for d in SCAN_DIRS:
        base = ROOT / d
        if not base.exists():
            continue
        for p in base.rglob("*.rs"):
            if "target" in p.parts:
                continue
            files.append(p)
    return sorted(files)


def strip_code(line: str) -> str:
    """Remove string literals and trailing comments from a code line."""
    out = []
    i = 0
    n = len(line)
    while i < n:
        if line[i] == '"':
            end = skip_string(line, i)
            out.append(" " * (end - i))
            i = end
            continue
        out.append(line[i])
        i += 1
    stripped = "".join(out)
    idx = stripped.find("//")
    if idx >= 0:
        stripped = stripped[:idx]
    return stripped


def check_allowlist(violations: list[str]) -> None:
    """Reject any ``allow(unsafe_code)`` outside the approved set."""
    approved = APPROVED_ALLOW_FILES | APPROVED_TEST_FILES
    for rs in collect_rust_files():
        rel = rs.relative_to(ROOT).as_posix()
        try:
            content = rs.read_text()
        except Exception as e:
            violations.append(f"cannot read {rel}: {e}")
            continue
        for attr in iter_attributes(content):
            if contains_allow_unsafe(attr) and rel not in approved:
                violations.append(
                    f"{rel}: allow(unsafe_code) attribute is not in the "
                    f"approved unsafe-module allowlist"
                )
                break


def check_safety_comments(violations: list[str]) -> None:
    """Require one contiguous, non-empty SAFETY explanation per construct."""
    for rs in collect_rust_files():
        rel = rs.relative_to(ROOT).as_posix()
        try:
            lines = rs.read_text().split("\n")
        except Exception as e:
            violations.append(f"cannot read {rel}: {e}")
            continue

        in_block_comment = False
        used_marker_lines: set[int] = set()
        for i, raw_line in enumerate(lines):
            stripped = raw_line.strip()

            # Track block comments (simple).
            if "/*" in stripped and "*/" not in stripped:
                in_block_comment = True
                continue
            if in_block_comment:
                if "*/" in stripped:
                    in_block_comment = False
                continue

            # Skip pure comments and blank lines.
            if not stripped or stripped.startswith("//"):
                continue

            # Strip strings and trailing comments for matching.
            code = strip_code(raw_line)

            # Find every unsafe construct on this line. Counting them matters:
            # one marker must not be reused for two constructs on one line.
            constructs = list(UNSAFE_RE.finditer(code))
            if not constructs:
                continue

            # A SAFETY explanation may span contiguous // comment lines, but
            # the block must touch the code and contain exactly one approved
            # marker. Blank lines, code, empty markers, and malformed markers
            # therefore cannot silently satisfy the policy.
            marker_lines: list[tuple[int, re.Match[str]]] = []
            unapproved_marker_lines: list[int] = []
            j = i - 1
            while j >= 0 and lines[j].strip().startswith("//"):
                marker = SAFETY_MARKER_RE.match(lines[j])
                if marker:
                    marker_lines.append((j, marker))
                elif SAFETY_WORD_RE.match(lines[j]):
                    unapproved_marker_lines.append(j)
                j -= 1

            if unapproved_marker_lines or len(marker_lines) != 1:
                if unapproved_marker_lines:
                    reason = "unapproved or malformed SAFETY marker"
                elif marker_lines:
                    reason = "multiple // SAFETY: markers are reused"
                elif i > 0 and SAFETY_WORD_RE.match(lines[i - 1]):
                    reason = "unapproved or malformed SAFETY marker"
                else:
                    reason = "missing immediately preceding // SAFETY: comment"
                violations.append(
                    f"{rel}:{i + 1}: unsafe construct has {reason}: "
                    f"{stripped[:80]}"
                )
                continue

            marker_line, marker = marker_lines[0]
            for construct_index, _construct in enumerate(constructs):
                if not marker.group(1).strip():
                    violations.append(
                        f"{rel}:{i + 1}: // SAFETY: explanation is empty: "
                        f"{stripped[:80]}"
                    )
                    continue

                if marker_line in used_marker_lines or construct_index > 0:
                    violations.append(
                        f"{rel}:{i + 1}: // SAFETY: marker is reused for multiple "
                        f"unsafe constructs: {stripped[:80]}"
                    )
                    continue
                used_marker_lines.add(marker_line)


def main() -> int:
    violations: list[str] = []

    check_workspace_deny(violations)
    check_allowlist(violations)
    check_safety_comments(violations)

    if violations:
        print("Unsafe-code policy violations:", file=sys.stderr)
        for v in violations:
            print(f"  - {v}", file=sys.stderr)
        return 1

    approved_count = len(APPROVED_ALLOW_FILES | APPROVED_TEST_FILES)
    print(
        f"Unsafe-code policy passed: workspace deny enforced, "
        f"{approved_count} approved modules, all unsafe constructs have "
        f"SAFETY comments."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
