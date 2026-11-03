#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
# Write workspace artifact $1 and evidence in this invocation's runtime directory.
set -eu

mkdir -p "$TRANSIST_EVIDENCE_DIR"
printf 'artifact body\n' > "$1"
printf 'evidence body\n' > "$TRANSIST_EVIDENCE_DIR/evidence.txt"
printf 'wrote %s and evidence.txt\n' "$1"
