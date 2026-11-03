#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

# Schema and HTTP API contract gate for the companion server.
#
# Verifies that:
# - every committed *.schema.json file declares a $schema key and parses as
#   valid JSON; public schemas under schemas/ declare Draft 2020-12;
# - unversioned schema aliases are pure $refs to their canonical versioned schema;
# - the companion API route inventory, the registered router, and the published
#   OpenAPI contract agree bidirectionally and expose only GET methods;
# - representative payloads serialized from transist-protocol validate against
#   the matching schemas; and the OpenAPI document passes semantic validation.
#
# The deeper schema compilation and serialized-payload validation runs in the
# Rust schema_contract test; the route/OpenAPI parity runs in the
# transist-server api_contract_parity test. This script gates that those
# tests are wired and that the committed files are structurally sound. Redocly
# CLI is pinned (see tool-versions.env + install_ci_tools.sh audit) because
# parsing YAML alone does not validate OpenAPI semantics.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# shellcheck disable=SC1091
source scripts/ci/tool-versions.env

fail=0

require_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "FAIL: missing required contract file '$path'" >&2
    fail=1
  fi
}

require_file "schemas/http/companion-api.v1.openapi.yaml"
require_file "schemas/runtime/workflow-reports.v1.schema.json"
require_file "schemas/runtime/gate-review.v1.schema.json"
require_file "schemas/tep/invoke.v1.schema.json"
require_file "schemas/tep/invoke.schema.json"
require_file "schemas/tep/result.v1.schema.json"
require_file "schemas/tep/result.schema.json"
require_file "schemas/tep/error.schema.json"
require_file "schemas/tep/capabilities.schema.json"
require_file "schemas/tep/handshake.request.v1.schema.json"
require_file "schemas/tep/handshake.response.v1.schema.json"
require_file "schemas/app/handshake.request.v1.schema.json"
require_file "schemas/app/handshake.response.v1.schema.json"
require_file "schemas/app/invoke.request.v1.schema.json"
require_file "schemas/app/invoke.result.v1.schema.json"
require_file "schemas/app/invoke.progress.v1.schema.json"
require_file "schemas/app/cancel.request.v1.schema.json"
require_file "schemas/app/cancel.result.v1.schema.json"

if ! command -v python3 >/dev/null; then
  echo "FAIL: python3 is required to validate the schema and API contracts" >&2
  exit 1
fi

# Use the pinned Redocly CLI installed by install_ci_tools.sh audit from the
# committed lockfile. Never resolve via npx or a global install at runtime
# (that would bypass the supply-chain pin).
redocly_bin="scripts/ci/redocly/node_modules/.bin/redocly"
if [[ ! -x "$redocly_bin" ]]; then
  echo "FAIL: redocly CLI is required for semantic OpenAPI validation." >&2
  echo "      Install it: bash scripts/ci/install_ci_tools.sh audit" >&2
  exit 1
fi

actual_version="$("$redocly_bin" --version 2>/dev/null | tr -d '[:space:]')"
if [[ "$actual_version" != "$REDOCLY_CLI_VERSION" ]]; then
  echo "FAIL: redocly version mismatch: expected ${REDOCLY_CLI_VERSION}, got '${actual_version}'" >&2
  fail=1
fi

"$redocly_bin" lint schemas/http/companion-api.v1.openapi.yaml

python3 - <<'PY'
import json
import os
import sys

root = os.getcwd()
fail = 0

# Auto-discover every committed *.schema.json file. A schema filename without a
# top-level $schema key is a coverage failure, rather than ordinary JSON that
# silently escapes this gate. CycloneDX is intentionally Draft 7; public
# schemas under schemas/ are required to use Draft 2020-12.
schema_files = []
for dirpath, _dirs, files in os.walk(root):
    if os.path.relpath(dirpath, root).startswith(".git"):
        continue
    for name in sorted(files):
        if name.endswith(".schema.json"):
            schema_files.append(os.path.join(dirpath, name))

for path in sorted(schema_files):
    try:
        with open(path) as fh:
            doc = json.load(fh)
    except Exception as exc:
        print(f"FAIL: {path} is not valid JSON: {exc}", file=sys.stderr)
        fail = 1
        continue
    rel = os.path.relpath(path, root)
    if not isinstance(doc, dict) or "$schema" not in doc:
        print(f"FAIL: {rel} must declare a top-level $schema", file=sys.stderr)
        fail = 1
        continue
    if rel.startswith("schemas/") and doc["$schema"] != "https://json-schema.org/draft/2020-12/schema":
        print(f"FAIL: {rel} must declare Draft 2020-12 $schema", file=sys.stderr)
        fail = 1

def keys(obj):
    return set(obj.keys()) if isinstance(obj, dict) else set()

# Unversioned TEP aliases must contain only a $ref to the canonical versioned
# schema (plus metadata keys), with no duplicated definitions.
alias_pairs = {
    "schemas/tep/invoke.schema.json": "schemas/tep/invoke.v1.schema.json",
    "schemas/tep/result.schema.json": "schemas/tep/result.v1.schema.json",
}
for alias, canonical in alias_pairs.items():
    with open(os.path.join(root, alias)) as fh:
        doc = json.load(fh)
    allowed = {"$schema", "$id", "title", "$ref"}
    extra = set(doc.keys()) - allowed
    if extra:
        print(f"FAIL: {alias} must contain only $ref (plus metadata); found extra keys: {sorted(extra)}", file=sys.stderr)
        fail = 1
    ref = doc.get("$ref")
    if ref != os.path.basename(canonical):
        print(f"FAIL: {alias} $ref must point to {os.path.basename(canonical)}; found {ref!r}", file=sys.stderr)
        fail = 1
    if "definitions" in doc or "$defs" in doc:
        print(f"FAIL: {alias} must not duplicate definitions", file=sys.stderr)
        fail = 1

# The companion OpenAPI contract must exist; its semantic validity is checked
# above, while route parity and GET-only enforcement are validated by the Rust
# api_contract_parity test, which parses the YAML via yaml_serde.
openapi_path = os.path.join(root, "schemas/http/companion-api.v1.openapi.yaml")
if not os.path.isfile(openapi_path):
    print("FAIL: companion OpenAPI contract is missing", file=sys.stderr)
    fail = 1

sys.exit(1 if fail else 0)
PY
python_ok=$?

# Run the Rust schema contract and route/OpenAPI parity tests.
cargo test --locked --test schema_contract
cargo test --locked -p transist-server --test api_contract_parity
cargo test --locked -p transist-server --test server_integration every_companion_route_response_matches_its_openapi_contract
cargo test --locked -p transist-server --test server_integration document_reviews

if [[ "$fail" -ne 0 || "$python_ok" -ne 0 ]]; then
  echo "Schema and API contract checks failed." >&2
  exit 1
fi

echo "Schema and API contract checks passed."
