#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
# Read workspace artifact $1 and write independent evidence for this invocation.
set -eu

# Workspace artifacts are shared across steps.
test -f "$1" || { echo "missing artifact"; exit 1; }
printf 'artifact says: %s\n' "$(cat "$1")"

# Each step gets its own evidence directory, not the preceding step's files.
# Inspect the writer's retained evidence with `transist runs show` instead.
mkdir -p "$TRANSIST_EVIDENCE_DIR"
printf 'read_outputs ran ok\n' > "$TRANSIST_EVIDENCE_DIR/read-confirmation.txt"
printf 'wrote read-confirmation evidence to %s\n' "$TRANSIST_EVIDENCE_DIR"
