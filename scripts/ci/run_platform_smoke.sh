#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Shared mainline build/smoke contract. Usage: linux|macos|alpine [--probe-only].
# Probe mode checks the expected native host/backend without invoking Cargo.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"
profile="${1:-}"
case "$profile" in
  linux) expected=x86_64-unknown-linux-gnu ;;
  macos) expected=aarch64-apple-darwin ;;
  alpine) expected=x86_64-unknown-linux-musl ;;
  *) echo "Usage: $0 linux|macos|alpine [--probe-only]" >&2; exit 2 ;;
esac
if [[ $# -gt 2 || ( $# -eq 2 && "$2" != --probe-only ) ]]; then
  echo "Unexpected smoke argument" >&2
  exit 2
fi
host="$(rustc -vV | sed -n 's/^host: //p')"
[[ "$host" == "$expected" ]] || {
  echo "FAIL: expected native Rust host $expected, got $host" >&2; exit 1;
}
if [[ "$profile" != macos ]]; then
  command -v bwrap >/dev/null || { echo "FAIL: bubblewrap is required" >&2; exit 1; }
  # Fail before compiling when Docker/host security prevents real namespaces.
  # Mirror the runtime's actual user/network namespaces, not unrelated PID/IPC
  # isolation. Docker can forbid nested PID proc mounts while supporting both.
  bwrap --die-with-parent --unshare-user --unshare-net --ro-bind / / --proc /proc --dev /dev -- /bin/true || {
    echo "FAIL: bubblewrap isolation is unavailable" >&2; exit 1;
  }
fi
[[ "${2:-}" != --probe-only ]] || exit 0
packages=(-p transist-cli -p transist-server -p transist-admin)
if [[ "$profile" != macos ]]; then
  packages+=(-p transist-sandbox-launcher)
fi
if [[ "$profile" == alpine ]]; then
  probe_dir="$(mktemp -d)"
  trap 'rm -rf "$probe_dir"' EXIT
  printf 'fn main(){}\n' > "$probe_dir/h.rs"
  rustc --target x86_64-unknown-linux-musl --crate-type bin -o "$probe_dir/h.bin" "$probe_dir/h.rs"
  # Read bytes directly: strings/grep pipelines under pipefail can mask matches
  # via SIGPIPE, and an absent strings executable must never imply success.
  python3 - "$probe_dir/h.bin" <<'PY'
from pathlib import Path
import sys

data = Path(sys.argv[1]).read_bytes()
if b"ld-linux" in data or b"libc.so" in data:
    sys.exit("FAIL: musl output links against glibc")
print("PASS: musl output is glibc-independent")
PY
  cargo build --locked --release --target x86_64-unknown-linux-musl "${packages[@]}"
  TRANSIST_SANDBOX_LAUNCHER=target/x86_64-unknown-linux-musl/release/transist-sandbox-launcher \
    cargo test --locked --target x86_64-unknown-linux-musl --test e2e_plugin_runtime runtime_temp_
  binary=target/x86_64-unknown-linux-musl/release/transist
else
  cargo build --locked --release "${packages[@]}"
  binary=target/release/transist
fi
bash scripts/ci/run_quickstart_smoke.sh --require-isolation --transist-bin "$binary"
