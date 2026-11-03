#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

fixture="$(mktemp -d "${TMPDIR:-/tmp}/transist-kernel-boundary.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT

mkdir -p "$fixture/crates/transist-core/src" "$fixture/crates/transist-plugins" "$fixture/src" "$fixture/scripts/ci"
cp scripts/ci/check_kernel_boundary.sh "$fixture/scripts/ci/check_kernel_boundary.sh"
printf '%s\n' 'AdapterRouter::with_builtin_adapters();' > "$fixture/src/composition.rs"

if (cd "$fixture" && bash scripts/ci/check_kernel_boundary.sh); then
  :
else
  echo "baseline kernel boundary check unexpectedly failed" >&2
  exit 1
fi

printf '%s\n' 'use crate::infra::atomic_io::write_replace_file;' \
  > "$fixture/crates/transist-core/src/forbidden.rs"
if (cd "$fixture" && bash scripts/ci/check_kernel_boundary.sh); then
  echo "kernel boundary fixture was accepted" >&2
  exit 1
fi

printf '%s\n' 'fn read_result() { let _ = std::fs::read("result"); let _ = std::fs::canonicalize("result"); }' \
  > "$fixture/crates/transist-core/src/kernel-run_execution.rs"
mkdir -p "$fixture/crates/transist-core/src/kernel"
mv "$fixture/crates/transist-core/src/kernel-run_execution.rs" \
  "$fixture/crates/transist-core/src/kernel/run_execution.rs"
if (cd "$fixture" && bash scripts/ci/check_kernel_boundary.sh); then
  echo "kernel boundary accepted result-file filesystem access" >&2
  exit 1
fi
rm "$fixture/crates/transist-core/src/kernel/run_execution.rs"

printf '%s\n' 'fn build_paths() { let _ = root.join("artifacts"); }' \
  > "$fixture/crates/transist-core/src/dispatch.rs"
if (cd "$fixture" && bash scripts/ci/check_kernel_boundary.sh); then
  echo "kernel boundary accepted invocation path construction" >&2
  exit 1
fi

echo "Kernel boundary negative fixture passed."
