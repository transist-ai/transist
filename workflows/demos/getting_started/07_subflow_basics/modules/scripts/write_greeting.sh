#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
# Write a greeting for topic $1 to workspace artifact $2; report completion.
set -eu

printf 'hello, %s\n' "$1" > "$2"
printf 'wrote greeting for %s\n' "$1"
