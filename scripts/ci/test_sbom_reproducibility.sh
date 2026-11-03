#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# test_sbom_reproducibility.sh — Verify the SBOM normalization is byte-for-byte
# deterministic across independent generations.
#
# cargo-cyclonedx emits a per-run RFC 4122 serialNumber and a generation
# timestamp, and does not guarantee component ordering. The
# normalize_and_validate_sbom.py step (run in release staging) strips that
# non-determinism so a release SBOM is reproducible from identical inputs. This
# test proves the guarantee holds:
#
#   - normalizing the same raw document twice yields byte-identical output;
#   - two raw documents that differ ONLY in serialNumber, timestamp, or
#     component/service order normalize to byte-identical output;
#   - a serialNumber/timestamp present in the raw input never survives.
#
# It runs the normalizer exactly as release staging does (including pinned
# CycloneDX 1.5 schema validation), so it needs the pinned python jsonschema
# (installed by install_ci_tools.sh audit/release).
#
# Exit status: 0 (all checks pass) or 1 (any check fails).

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

script="scripts/ci/normalize_and_validate_sbom.py"
command -v python3 >/dev/null 2>&1 || { echo "FAIL: python3 is required" >&2; exit 1; }

failures=0
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A raw SBOM-like document carrying the non-deterministic fields that
# cargo-cyclonedx emits. It exercises serialNumber, timestamp (top-level and in
# metadata), and unordered components/services.
make_raw() {
  local serial="$1"
  local stamp="$2"
  cat <<EOF
{
  "bomFormat": "CycloneDX",
  "specVersion": "1.5",
  "version": 1,
  "serialNumber": "$serial",
  "timestamp": "$stamp",
  "metadata": {
    "timestamp": "$stamp",
    "tools": [{"name": "cargo-cyclonedx", "version": "0.5.9"}],
    "component": {"type": "application", "name": "transist", "version": "0.1.0"}
  },
  "components": [
    {"type": "library", "name": "zeta", "version": "2.0.0", "bom-ref": "pkg:cargo/zeta@2.0.0", "purl": "pkg:cargo/zeta@2.0.0"},
    {"type": "library", "name": "alpha", "version": "1.0.0", "bom-ref": "pkg:cargo/alpha@1.0.0", "purl": "pkg:cargo/alpha@1.0.0"}
  ],
  "services": [
    {"name": "service-b", "bom-ref": "svc-b"},
    {"name": "service-a", "bom-ref": "svc-a"}
  ],
  "dependencies": [{"ref": "pkg:cargo/alpha@1.0.0"}]
}
EOF
}

echo "--- Test 1: normalizing twice yields byte-identical output ---"
: > "$work/raw1.json"
make_raw "urn:uuid:11111111-1111-1111-1111-111111111111" "2026-08-13T00:00:00Z" > "$work/raw1.json"
python3 "$script" "$work/raw1.json" "$work/norm-a.json" >/dev/null
python3 "$script" "$work/raw1.json" "$work/norm-b.json" >/dev/null
if cmp -s "$work/norm-a.json" "$work/norm-b.json"; then
  echo "  PASS  repeated normalization identical"
else
  echo "  FAIL  repeated normalization differs" >&2
  failures=$((failures + 1))
fi

echo "--- Test 2: only-serialNumber/timestamp/order changes normalize identically ---"
# Second generation differs in serialNumber, both timestamps, and reverses the
# component/service order. After normalization the bytes must match Test 1.
make_raw "urn:uuid:22222222-2222-2222-2222-222222222222" "2026-08-13T01:30:00Z" > "$work/raw2.json"
python3 - "$work/raw2.json" <<'PY'
import json, sys
p = sys.argv[1]
with open(p, encoding="utf-8") as fh:
    d = json.load(fh)
d["components"] = list(reversed(d["components"]))
d["services"] = list(reversed(d["services"]))
with open(p, "w", encoding="utf-8") as fh:
    json.dump(d, fh, indent=2)
PY
python3 "$script" "$work/raw2.json" "$work/norm2.json" >/dev/null
if cmp -s "$work/norm-a.json" "$work/norm2.json"; then
  echo "  PASS  independent generations normalize to identical bytes"
else
  echo "  FAIL  independent generations normalized differently" >&2
  diff <(python3 -m json.tool "$work/norm-a.json") <(python3 -m json.tool "$work/norm2.json") >&2 || true
  failures=$((failures + 1))
fi

echo "--- Test 3: serialNumber and timestamp never survive normalization ---"
if grep -q '"serialNumber"' "$work/norm-a.json"; then
  echo "  FAIL  normalized SBOM still contains serialNumber" >&2
  failures=$((failures + 1))
else
  echo "  PASS  serialNumber removed"
fi
if grep -q '"timestamp"' "$work/norm-a.json"; then
  echo "  FAIL  normalized SBOM still contains timestamp" >&2
  failures=$((failures + 1))
else
  echo "  PASS  timestamp removed"
fi

echo "--- Test 4: normalized output validates against the pinned schema ---"
if python3 "$script" --validate "$work/norm-a.json"; then
  echo "  PASS  normalized SBOM is a valid CycloneDX 1.5 document"
else
  echo "  FAIL  normalized SBOM failed schema validation" >&2
  failures=$((failures + 1))
fi

echo ""
echo "============================================================"
if [[ "$failures" -eq 0 ]]; then
  echo "  RESULT: All SBOM reproducibility checks passed"
else
  echo "  RESULT: $failures SBOM reproducibility check(s) failed"
fi
echo "============================================================"
exit "$failures"
