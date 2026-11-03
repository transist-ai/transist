#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

installer="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/install_ci_tools.sh"
test_dir="$(mktemp -d /tmp/transist-ci-tools-test.XXXXXX)"
trap 'rm -rf "$test_dir"' EXIT

list_tools() {
  CI_TOOL_ROOT="$test_dir/tools" GITHUB_PATH="" CI_TOOLS_LIST_ONLY=1 bash "$installer" "$1"
}

list_tools audit > "$test_dir/audit"
list_tools coverage > "$test_dir/coverage"
list_tools links > "$test_dir/links"
list_tools static > "$test_dir/static"
list_tools node > "$test_dir/node"
list_tools markdown > "$test_dir/markdown"
list_tools preflight > "$test_dir/preflight"
list_tools release > "$test_dir/release"
list_tools staging > "$test_dir/staging"
list_tools verification > "$test_dir/verification"
list_tools gitleaks > "$test_dir/gitleaks"
list_tools all > "$test_dir/all"

cat "$test_dir/audit" "$test_dir/coverage" "$test_dir/links" \
  "$test_dir/release" "$test_dir/gitleaks" "$test_dir/static" "$test_dir/markdown" | sort -u > "$test_dir/expected"
sort "$test_dir/all" > "$test_dir/actual"
if ! cmp -s "$test_dir/expected" "$test_dir/actual"; then
  echo "FAIL: all CI tools are not the union of the individual modes" >&2
  diff -u "$test_dir/expected" "$test_dir/actual" >&2 || true
  exit 1
fi

if [[ "$(cat "$test_dir/preflight")" != $'gh\nrg' ]]; then
  echo "FAIL: preflight mode must install exactly gh and rg" >&2
  exit 1
fi

if [[ "$(cat "$test_dir/static")" != actionlint || "$(cat "$test_dir/node")" != $'node\nnpm' \
  || "$(cat "$test_dir/markdown")" != $'node\nnpm\nmarkdownlint' ]]; then
  echo "FAIL: static/node/markdown mode tool sets drifted" >&2
  exit 1
fi

if [[ "$(cat "$test_dir/staging")" != $'cargo-cyclonedx\ngh\njsonschema\nminisign' ]]; then
  echo "FAIL: staging mode must remain limited to release assembly tools" >&2
  exit 1
fi

if [[ "$(cat "$test_dir/verification")" != $'gh\njsonschema\nminisign' ]]; then
  echo "FAIL: verification mode must remain limited to draft inspection tools" >&2
  exit 1
fi

mock_bin="$test_dir/mock-bin"
mkdir -p "$mock_bin"
cat > "$test_dir/cargo-tool-fixture" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
executable="${0##*/}"
case "$executable" in
  cargo-llvm-cov|cargo-cyclonedx)
    [[ "$#" == 2 && "$1" == "${executable#cargo-}" && "$2" == --version ]]
    ;;
  *) [[ "$#" == 1 && "$1" == --version ]] ;;
esac
IFS= read -r version < "$TEST_DIR/$executable.version"
banner="$executable"
# Match the real pinned nested Clap banner, not a simplified mock-only output.
[[ "$executable" != cargo-cyclonedx ]] || banner=cargo-cyclonedx-cyclonedx
printf '%s\n' "info: fixture progress" "$banner $version"
[[ ! -f "$TEST_DIR/persistent-probe-failure" ]] || exit 1
if [[ -f "$TEST_DIR/probe-fails" ]]; then
  rm "$TEST_DIR/probe-fails"
  exit 1
fi
EOF
# Direct executables print only their version, unlike the wrapped tools above.
cat > "$test_dir/cargo-direct-fixture" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 1 && "$1" == --version ]]
executable="${0##*/}"
IFS= read -r version < "$TEST_DIR/$executable.version"
printf '%s\n' "$executable $version"
EOF
cat > "$mock_bin/cargo-llvm-cov" <<'EOF'
#!/usr/bin/env bash
touch "$TEST_DIR/ambient-probed"
echo 'cargo-llvm-cov 0.9.0'
EOF
cat > "$mock_bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "+1.88.0" && "$2" == "install" ]]; then
  printf '%s\n' "$*" >> "$TEST_DIR/installs"
  package="$3"
  version="$5"
  [[ " $* " == *" --version $version --locked --force --root $CI_TOOL_ROOT "* ]]
  [[ "${INSTALL_FAIL:-0}" == 0 ]] || exit 1
  [[ "${INSTALL_MISSING_ROOT:-0}" == 0 ]] || exit 0
  printf '%s\n' "${INSTALLED_VERSION:-$version}" > "$TEST_DIR/$package.version"
  case "$package" in
    cargo-llvm-cov|cargo-cyclonedx) fixture=cargo-tool-fixture ;;
    *) fixture=cargo-direct-fixture ;;
  esac
  cp "$TEST_DIR/$fixture" "$CI_TOOL_ROOT/bin/$package"
  chmod +x "$CI_TOOL_ROOT/bin/$package"
  if [[ "${POST_INSTALL_PROBE_FAIL:-0}" == 1 ]]; then
    touch "$TEST_DIR/persistent-probe-failure"
  fi
  exit 0
fi

echo "unexpected cargo invocation: $*" >&2
exit 1
EOF
chmod +x "$mock_bin/cargo" "$mock_bin/cargo-llvm-cov"
export TEST_DIR="$test_dir" CI_TOOL_ROOT="$test_dir/tools" PATH="$mock_bin:$PATH"
export GITHUB_PATH="$test_dir/github-path"
export CARGO_HOME="$test_dir/cargo-home"

# Assert the number of real installs, keeping every version check active.
assert_installs() {
  local count
  count="$(wc -l < "$test_dir/installs" | tr -d ' ')"
  [[ "$count" == "$1" ]] || { echo "FAIL: expected $1 installs, got $count" >&2; exit 1; }
}

bash "$installer" coverage
assert_installs 1
[[ -x "$CI_TOOL_ROOT/bin/cargo-llvm-cov" && ! -e "$test_dir/ambient-probed" ]]
bash "$installer" coverage
assert_installs 1
printf '0.8.0\n' > "$test_dir/cargo-llvm-cov.version"
bash "$installer" coverage
assert_installs 2
printf '0.9.01\n' > "$test_dir/cargo-llvm-cov.version"
bash "$installer" coverage
assert_installs 3
touch "$test_dir/probe-fails"
bash "$installer" coverage
assert_installs 4
printf '0.8.0\n' > "$test_dir/cargo-llvm-cov.version"
if INSTALLED_VERSION=0.9.01 bash "$installer" coverage > "$test_dir/wrong-version.log" 2>&1; then
  echo "FAIL: installer accepted an incorrect post-install version" >&2
  exit 1
fi
assert_installs 5
if INSTALL_FAIL=1 bash "$installer" coverage > "$test_dir/failed-install.log" 2>&1; then
  echo "FAIL: installer hid a failed Cargo install" >&2
  exit 1
fi
assert_installs 6
if POST_INSTALL_PROBE_FAIL=1 bash "$installer" coverage > "$test_dir/failed-probe.log" 2>&1; then
  echo "FAIL: installer accepted version text from a failed post-install probe" >&2
  exit 1
fi
assert_installs 7
rm "$test_dir/persistent-probe-failure"
if CI_TOOL_ROOT="$test_dir/missing-root" GITHUB_PATH="" INSTALL_MISSING_ROOT=1 bash "$installer" coverage > "$test_dir/missing-root.log" 2>&1; then
  echo "FAIL: ambient exact version hid a missing post-install root executable" >&2
  exit 1
fi
assert_installs 8
bash -c 'source "$1" coverage; install_cargo_tool cargo-cyclonedx "$CARGO_CYCLONEDX_VERSION"; install_cargo_tool cargo-deny "$CARGO_DENY_VERSION"' _ "$installer"
assert_installs 10
bash -c 'source "$1" coverage; install_cargo_tool cargo-cyclonedx "$CARGO_CYCLONEDX_VERSION"; install_cargo_tool cargo-deny "$CARGO_DENY_VERSION"' _ "$installer"
assert_installs 10
printf '0.5.90\n' > "$test_dir/cargo-cyclonedx.version"
if INSTALLED_VERSION=0.5.90 bash -c 'source "$1" coverage; install_cargo_tool cargo-cyclonedx "$CARGO_CYCLONEDX_VERSION"' _ "$installer" > "$test_dir/wrong-cyclonedx.log" 2>&1; then
  echo "FAIL: CycloneDX banner normalization accepted an incorrect version" >&2
  exit 1
fi
assert_installs 11
bash -c 'source "$1" coverage; install_cargo_tool cargo-cyclonedx "$CARGO_CYCLONEDX_VERSION"' _ "$installer"
assert_installs 12
# The exact root probe must not be confused by a hidden CARGO_HOME executable.
mkdir -p "$CARGO_HOME/bin"
cp "$mock_bin/cargo-llvm-cov" "$CARGO_HOME/bin/cargo-llvm-cov"
if bash "$installer" coverage > "$test_dir/cargo-home-conflict.log" 2>&1; then
  echo "FAIL: hidden CARGO_HOME/bin shadowing was not reported" >&2
  exit 1
fi
grep -Fq 'before rerunning the installer and CI commands' "$test_dir/cargo-home-conflict.log"
PATH="$mock_bin:$CARGO_HOME/bin:$PATH" bash "$installer" coverage
assert_installs 12
# Restore the fixture without the conflicting executable for the Node tests.
rm "$CARGO_HOME/bin/cargo-llvm-cov"
if [[ "$(sort -u "$GITHUB_PATH")" != "$CI_TOOL_ROOT/bin" ]]; then
  echo "FAIL: installer did not publish the explicit CI_TOOL_ROOT/bin through GITHUB_PATH" >&2
  exit 1
fi

# Tiny download/unpack fixtures prove Node provisioning without network or npm.
# shellcheck disable=SC1091
source "$(dirname "$installer")/tool-versions.env"
export NODE_FIXTURE_VERSION="$NODE_VERSION" NODE_FIXTURE_SHA256="$NODE_LINUX_X64_SHA256"
export NODE_FIXTURE_URL="$NODE_LINUX_X64_URL"
cat > "$mock_bin/uname" <<'EOF'
#!/usr/bin/env bash
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) exit 1 ;; esac
EOF
cat > "$mock_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$TEST_DIR/downloads"
[[ " $* " == *" $NODE_FIXTURE_URL " ]]
while [[ "$1" != --output ]]; do shift; done
printf 'fixture archive\n' > "$2"
EOF
cat > "$mock_bin/sha256sum" <<'EOF'
#!/usr/bin/env bash
printf '%s  %s\n' "${NODE_FIXTURE_BAD_SHA:-$NODE_FIXTURE_SHA256}" "$1"
EOF
cat > "$mock_bin/tar" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == -xzf && "$3" == -C ]]
printf 'unpack\n' >> "$TEST_DIR/unpacks"
root="$4/node-v${NODE_FIXTURE_VERSION}-linux-x64"
mkdir -p "$root/bin"
cat > "$root/bin/node" <<'NODE'
#!/usr/bin/env bash
[[ "$1" == --version ]] || exit 1
printf 'v%s\n' "${NODE_INSTALLED_VERSION:-$NODE_FIXTURE_VERSION}"
[[ "${NODE_PROBE_FAIL:-0}" == 0 ]]
NODE
cat > "$root/bin/npm" <<'NPM'
#!/usr/bin/env bash
set -euo pipefail
[[ "$(command -v node)" == "$CI_TOOL_ROOT/bin/node" ]]
[[ "$(node --version)" == "v$NODE_FIXTURE_VERSION" ]]
[[ " $* " == *" ci --ignore-scripts --no-audit --prefix "* ]]
printf '%s\n' "$*" >> "$TEST_DIR/npm-calls"
NPM
cp "$root/bin/npm" "$root/bin/npx"
chmod +x "$root/bin/"*
EOF
chmod +x "$mock_bin/"{uname,curl,sha256sum,tar}
bash "$installer" node
bash "$installer" node
[[ "$(wc -l < "$test_dir/downloads" | tr -d ' ')" == 1 ]]
[[ "$(wc -l < "$test_dir/unpacks" | tr -d ' ')" == 1 ]]
printf '#!/usr/bin/env bash\nprintf "v18.0.0\\n"\n' > "$CI_TOOL_ROOT/node/bin/node"
bash "$installer" node
[[ "$(wc -l < "$test_dir/downloads" | tr -d ' ')" == 2 ]]
bash "$installer" markdown
[[ "$(wc -l < "$test_dir/npm-calls" | tr -d ' ')" == 1 ]]
rm "$CI_TOOL_ROOT/bin/node"
if NODE_FIXTURE_BAD_SHA=bad bash "$installer" node > "$test_dir/bad-node-sha.log" 2>&1; then
  echo "FAIL: Node archive with the wrong checksum was accepted" >&2
  exit 1
fi
[[ "$(wc -l < "$test_dir/unpacks" | tr -d ' ')" == 2 ]]
if NODE_INSTALLED_VERSION=18.0.0 bash "$installer" node > "$test_dir/bad-node-version.log" 2>&1; then
  echo "FAIL: incorrect installed Node version was accepted" >&2
  exit 1
fi
if NODE_PROBE_FAIL=1 bash "$installer" node > "$test_dir/bad-node-probe.log" 2>&1; then
  echo "FAIL: failed Node version probe was accepted" >&2
  exit 1
fi
echo "CI tool modes, exact-version/exit-status checks, PATH and verified Node/npm fixtures passed without downloads."
