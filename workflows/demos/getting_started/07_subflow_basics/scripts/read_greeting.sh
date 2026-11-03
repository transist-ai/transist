#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
# Read the module's workspace artifact at $1 and print it for root-step inspection.
set -eu

test -f "$1" || { echo "module did not produce greeting.txt"; exit 1; }
printf 'root sees: %s\n' "$(cat "$1")"
