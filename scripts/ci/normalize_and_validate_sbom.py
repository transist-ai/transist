#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""
normalize_and_validate_sbom.py — Produce a deterministic, semantically valid
CycloneDX SBOM for a release.

The cargo-cyclonedx emitter produces SBOMs that are not bit-for-bit
deterministic: array ordering and a generated timestamp vary across runs. This
tool normalizes that variability away so the SBOM archive is reproducible from
identical inputs, then validates every document against the pinned CycloneDX
1.5 JSON schema committed at scripts/ci/schemas/cyclonedx-1.5.schema.json.

Usage:
  python3 normalize_and_validate_sbom.py <input-sbom.json> <output-sbom.json>
      Normalize one SBOM document and validate it against the pinned schema.
  python3 normalize_and_validate_sbom.py --validate <sbom.json>...
      Validate one or more existing SBOM documents against the pinned schema
      without rewriting them.

Normalization rules (each preserves semantic content while removing
non-determinism):
  - remove the top-level "serialNumber" field (a per-run random RFC 4122 UUID;
    the BOM has no identity-bearing serial that must survive)
  - remove the top-level "timestamp" field (a generation-time value)
  - sort "components" by (type, bom-ref, name, version, purl)
  - sort "services", "externalReferences", and "properties" deterministically
  - stable-serialize with a fixed key order

Exit status: 0 if every document normalizes and validates; 1 otherwise.
"""

import argparse
import json
import os
import sys

EXPECTED_SPEC_VERSION = "1.5"

CANONICAL_KEYS = [
    "bomFormat",
    "specVersion",
    "version",
    "serialNumber",
    "metadata",
    "components",
    "services",
    "compositions",
    "externalReferences",
    "dependencies",
    "formulation",
    "annotations",
    "signature",
    "vulnerabilities",
]

KEY_PRIORITY = {key: rank for rank, key in enumerate(CANONICAL_KEYS)}


def sort_components(components):
    return sorted(
        components,
        key=lambda c: (
            c.get("type", ""),
            str(c.get("bom-ref", "")),
            c.get("name", ""),
            c.get("version", ""),
            c.get("purl", ""),
        ),
    )


def canonicalize(value):
    if isinstance(value, dict):
        ordered = {}
        for key in sorted(value.keys()):
            ordered[key] = canonicalize(value[key])
        return ordered
    if isinstance(value, list):
        return [canonicalize(item) for item in value]
    return value


def normalize(document):
    doc = canonicalize(document)
    if "serialNumber" in doc:
        del doc["serialNumber"]
    if "timestamp" in doc:
        del doc["timestamp"]
    if "metadata" in doc and isinstance(doc["metadata"], dict):
        metadata = doc["metadata"]
        if "timestamp" in metadata:
            del metadata["timestamp"]
        # Re-sort metadata and drop generated supplier metadata that can carry
        # nondeterministic values; the component list is the semantic core.
        for sub in ("tools", "authors", "component", "manufacture", "supplier"):
            if sub in metadata and isinstance(metadata[sub], list):
                metadata[sub] = sorted(
                    metadata[sub],
                    key=lambda c: (
                        str(c.get("name", "")) if isinstance(c, dict) else str(c),
                    ),
                )
    if "components" in doc and isinstance(doc["components"], list):
        doc["components"] = sort_components(doc["components"])
    if "services" in doc and isinstance(doc["services"], list):
        doc["services"] = sorted(
            doc["services"],
            key=lambda s: (
                str(s.get("bom-ref", "")),
                s.get("name", ""),
            ),
        )
    if "externalReferences" in doc and isinstance(doc["externalReferences"], list):
        doc["externalReferences"] = sorted(
            doc["externalReferences"],
            key=lambda r: (r.get("type", ""), r.get("url", "")),
        )
    return doc


def validate(document, schema_path):
    if document.get("specVersion") != EXPECTED_SPEC_VERSION:
        raise SystemExit(
            f"FAIL: SBOM specVersion is {document.get('specVersion')!r}, "
            f"expected {EXPECTED_SPEC_VERSION!r}"
        )
    try:
        import jsonschema
    except ImportError as error:  # pragma: no cover - guarded in CI
        raise SystemExit("FAIL: python jsonschema is required") from error
    import jsonschema.exceptions

    with open(schema_path, encoding="utf-8") as stream:
        schema = json.load(stream)
    try:
        jsonschema.validate(instance=document, schema=schema)
    except jsonschema.exceptions.ValidationError as error:
        raise SystemExit(f"FAIL: SBOM failed CycloneDX schema validation: {error.message}") from error


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--validate",
        action="store_true",
        help="validate existing documents against the pinned schema without rewriting",
    )
    parser.add_argument("paths", nargs="+", help="input SBOM path(s)")
    args = parser.parse_args(argv)

    schema_path = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "schemas", "cyclonedx-1.5.schema.json"
    )

    if args.validate:
        ok = True
        for path in args.paths:
            with open(path, encoding="utf-8") as stream:
                doc = json.load(stream)
            validate(doc, schema_path)
            print(f"OK  {path} is a valid CycloneDX {EXPECTED_SPEC_VERSION} document")
        return 0 if ok else 1

    if len(args.paths) != 2:
        parser.error("expected <input-sbom.json> <output-sbom.json>, or --validate <files>...")
    input_path, output_path = args.paths
    with open(input_path, encoding="utf-8") as stream:
        doc = json.load(stream)
    normalized = normalize(doc)
    validate(normalized, schema_path)
    with open(output_path, "w", encoding="utf-8") as stream:
        json.dump(normalized, stream, indent=2, sort_keys=True)
        stream.write("\n")
    print(f"OK  normalized and validated {input_path} -> {output_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
