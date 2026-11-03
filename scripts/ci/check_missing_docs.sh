#!/usr/bin/env bash
set -euo pipefail

# Missing-docs policy:
# - every checked workspace crate must compile cleanly under `-D missing_docs`

echo "Checking missing_docs policy..."
echo "Output format:"
echo "  <crate> missing_docs=0 policy=deny"

has_policy_attr() {
  local root_file="$1"
  local expected="$2"

  grep -Eq "^#!\\[$expected\\(missing_docs\\)\\]$|^#!\\[cfg_attr\\(not\\(clippy\\), $expected\\(missing_docs\\)\\)\\]$" "$root_file"
}

run_deny_check() {
  local crate="$1"
  local root_file="$2"
  shift 2

  if ! has_policy_attr "$root_file" "deny"; then
    echo "deny(missing_docs) policy missing from $root_file" >&2
    exit 1
  fi

  cargo rustc "$@" -- -D missing_docs >/dev/null
  echo "$crate missing_docs=0 policy=deny"
}

run_deny_check transist src/lib.rs --package transist --lib
run_deny_check transist-protocol crates/transist-protocol/src/lib.rs -p transist-protocol --lib
run_deny_check transist-core crates/transist-core/src/lib.rs -p transist-core --lib
run_deny_check transist-compiler crates/transist-compiler/src/lib.rs -p transist-compiler --lib
run_deny_check transist-plugins crates/transist-plugins/src/lib.rs -p transist-plugins --lib
run_deny_check transist-prerequisites crates/transist-prerequisites/src/lib.rs -p transist-prerequisites --lib

run_deny_check transist-policy crates/transist-policy/src/lib.rs -p transist-policy --lib
run_deny_check transist-storage-local crates/transist-storage-local/src/lib.rs -p transist-storage-local --lib
run_deny_check transist-adapters crates/transist-adapters/src/lib.rs -p transist-adapters --lib
run_deny_check transist-cli crates/transist-cli/src/main.rs -p transist-cli --bin transist
run_deny_check transist-cli-lib crates/transist-cli/src/lib.rs -p transist-cli --lib
run_deny_check transist-server crates/transist-server/src/lib.rs -p transist-server --lib
run_deny_check transist-server-bin crates/transist-server/src/main.rs -p transist-server --bin transist-server
run_deny_check transist-trust-digest crates/transist-trust-digest/src/lib.rs -p transist-trust-digest --lib
run_deny_check transist-metrics crates/transist-metrics/src/lib.rs -p transist-metrics --lib
run_deny_check transist-inspection crates/transist-inspection/src/lib.rs -p transist-inspection --lib
run_deny_check transist-admin transist-admin/src/lib.rs -p transist-admin --lib
run_deny_check transist-admin-bin transist-admin/src/main.rs -p transist-admin --bin transist-admin
run_deny_check transist-sandbox-launcher crates/transist-sandbox-launcher/src/main.rs -p transist-sandbox-launcher --bin transist-sandbox-launcher
echo "missing_docs policy checks passed."
