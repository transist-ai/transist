#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck disable=SC1091
source "$ROOT_DIR/scripts/ci/tool-versions.env"

mode="${1:-all}"
case "$mode" in
  audit|coverage|links|static|node|markdown|preflight|release|staging|verification|gitleaks|all) ;;
  *)
    echo "Usage: $0 [audit|coverage|links|static|node|markdown|preflight|release|staging|verification|gitleaks|all]" >&2
    exit 2
    ;;
esac

tool_names_for_mode() {
  local selected_mode="$1"
  case "$selected_mode" in
    audit)
      printf '%s\n' node npm cargo-deny cargo-about actionlint rg redocly jsonschema gitleaks
      ;;
    coverage)
      printf '%s\n' cargo-llvm-cov
      ;;
    links)
      printf '%s\n' lychee
      ;;
    static)
      printf '%s\n' actionlint
      ;;
    node)
      printf '%s\n' node npm
      ;;
    markdown)
      printf '%s\n' node npm markdownlint
      ;;
    preflight)
      printf '%s\n' gh rg
      ;;
    release)
      printf '%s\n' node npm cargo-deny cargo-about cargo-cyclonedx cargo-llvm-cov gh actionlint rg jsonschema minisign
      ;;
    staging)
      printf '%s\n' cargo-cyclonedx gh jsonschema minisign
      ;;
    verification)
      printf '%s\n' gh jsonschema minisign
      ;;
    gitleaks)
      printf '%s\n' gitleaks
      ;;
    *)
      echo "unknown CI tool mode: $selected_mode" >&2
      return 2
      ;;
  esac
}

list_tools_for_mode() {
  local selected_mode="$1"
  local -a modes
  local -a seen_tools=()
  local tool
  if [[ "$selected_mode" == "all" ]]; then
    modes=(audit coverage links release gitleaks static node markdown)
  else
    modes=("$selected_mode")
  fi
  for selected_mode in "${modes[@]}"; do
    while IFS= read -r tool; do
      if [[ " ${seen_tools[*]-} " != *" $tool "* ]]; then
        printf '%s\n' "$tool"
        seen_tools+=("$tool")
      fi
    done < <(tool_names_for_mode "$selected_mode")
  done
}

tool_root="${CI_TOOL_ROOT:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/transist-ci-tools}"
# Cargo installs use this same isolated root. Only this process can change its
# PATH locally; callers must export CI_TOOL_ROOT/bin themselves. GITHUB_PATH
# publishes it to subsequent hosted steps, even on a verified cache hit.
bin_dir="$tool_root/bin"
download_dir="$tool_root/downloads"
mkdir -p "$bin_dir" "$download_dir"

if [[ -n "${GITHUB_PATH:-}" ]]; then
  printf '%s\n' "$bin_dir" >> "$GITHUB_PATH"
fi
export PATH="$bin_dir:$PATH"

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | cut -d' ' -f1
  else
    shasum -a 256 "$path" | cut -d' ' -f1
  fi
}

download_verified() {
  local url="$1"
  local expected_sha256="$2"
  local output_path="$3"
  curl --fail --silent --show-error --location --retry 3 --output "$output_path" "$url"
  local actual_sha256
  actual_sha256="$(sha256_file "$output_path")"
  if [[ "$actual_sha256" != "$expected_sha256" ]]; then
    echo "SHA-256 mismatch for $url" >&2
    echo "expected: $expected_sha256" >&2
    echo "actual:   $actual_sha256" >&2
    exit 1
  fi
}

install_cargo_tool() {
  local package="$1"
  local version="$2"
  shift 2
  # Cached binaries are only an optimization: check the exact executable version
  # on every invocation, then validate again after a missing/mismatched install.
  if cargo_tool_version_matches "$package" "$version"; then
    verify_cargo_tool_dispatch "$package"
    echo "Using pinned $package $version"
    return
  fi
  cargo +1.88.0 install "$package" --version "$version" --locked --force --root "$tool_root" "$@"
  verify_cargo_tool "$package" "$version"
  verify_cargo_tool_dispatch "$package"
}

# Probe only the executable installed under this tool root, never ambient PATH.
# Cargo wrappers need their subcommand argument even when invoked by exact path.
cargo_tool_version() {
  local executable="$1"
  local actual
  local path="$bin_dir/$executable"
  [[ -x "$path" ]] || return 1
  # These wrappers require the Cargo subcommand argument before --version;
  # invoking the executable with only --version is not their version surface.
  if [[ "$executable" == "cargo-llvm-cov" || "$executable" == "cargo-cyclonedx" ]]; then
    actual="$("$path" "${executable#cargo-}" --version 2>&1)" || return 1
    # Retain support for progress/diagnostic lines before the version line.
    actual="${actual##*$'\n'}"
  else
    actual="$("$path" --version 2>&1)" || return 1
  fi
  # CycloneDX's nested Clap command repeats its name in the version banner,
  # including when dispatched by real Cargo. Normalize only that known banner;
  # the caller still checks the exact pinned version and rejects failed probes.
  if [[ "$executable" == "cargo-cyclonedx" && "$actual" == "cargo-cyclonedx-cyclonedx "* ]]; then
    actual="cargo-cyclonedx ${actual#cargo-cyclonedx-cyclonedx }"
  fi
  printf '%s\n' "$actual"
}

# Cargo prepends CARGO_HOME/bin when that directory is absent from PATH. Fail
# explicitly if it would shadow the root-bound tool after a successful install.
# Real-Cargo fixtures cover both this case and explicit PATH-order precedence.
verify_cargo_tool_dispatch() {
  local executable="$1"
  local home_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
  local entry
  local -a path_entries
  [[ -x "$home_bin/$executable" ]] || return 0
  IFS=: read -r -a path_entries <<< "$PATH"
  for entry in "${path_entries[@]}"; do
    if [[ "$entry" == "$home_bin" || "$entry" == "$home_bin/" ]]; then
      return 0
    fi
  done
  echo "FAIL: $home_bin/$executable would shadow $bin_dir/$executable in Cargo dispatch." >&2
  echo "Cargo prioritizes CARGO_HOME/bin when it is absent from PATH." >&2
  echo "Export PATH=\"$bin_dir:$home_bin:\$PATH\" before rerunning the installer and CI commands." >&2
  return 1
}

# Return success only for a successful probe with the exact requested version.
cargo_tool_version_matches() {
  local executable="$1"
  local version="$2"
  local actual
  actual="$(cargo_tool_version "$executable")" || return 1
  [[ "$actual" == "$executable $version" || "$actual" == "$executable $version "* ]]
}

verify_cargo_tool() {
  local executable="$1"
  local version="$2"
  local actual
  actual="$(cargo_tool_version "$executable")" || {
    echo "FAIL: pinned Cargo tool is unavailable or its version probe failed: $executable" >&2
    exit 1
  }
  if [[ "$actual" != "$executable $version" && "$actual" != "$executable $version "* ]]; then
    echo "FAIL: pinned Cargo tool version mismatch for $executable" >&2
    echo "expected prefix: $executable $version" >&2
    echo "actual: $actual" >&2
    exit 1
  fi
}

install_linux_archive_binary() {
  local name="$1"
  local url="$2"
  local expected_sha256="$3"
  local archive_name="$4"
  local member="$5"
  local archive_path="$download_dir/$archive_name"
  local extract_dir="$download_dir/${name}-extract"

  download_verified "$url" "$expected_sha256" "$archive_path"
  rm -rf "$extract_dir"
  mkdir -p "$extract_dir"
  tar -xzf "$archive_path" -C "$extract_dir"
  install -m 0755 "$extract_dir/$member" "$bin_dir/$name"
}

install_github_cli() {
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
      install_linux_archive_binary \
        gh \
        "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_amd64.tar.gz" \
        "$GH_LINUX_X64_SHA256" \
        "gh-linux.tar.gz" \
        "gh_${GH_VERSION}_linux_amd64/bin/gh"
      ;;
    Darwin:arm64)
      local archive_path="$download_dir/gh-macos.zip"
      local extract_dir="$download_dir/gh-macos-extract"
      download_verified \
        "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_macOS_arm64.zip" \
        "$GH_MACOS_ARM64_SHA256" \
        "$archive_path"
      rm -rf "$extract_dir"
      mkdir -p "$extract_dir"
      unzip -q "$archive_path" -d "$extract_dir"
      install -m 0755 "$extract_dir/gh_${GH_VERSION}_macOS_arm64/bin/gh" "$bin_dir/gh"
      ;;
    *)
      echo "Unsupported platform for pinned GitHub CLI: $(uname -s):$(uname -m)" >&2
      exit 1
      ;;
  esac
}

install_actionlint() {
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
      install_linux_archive_binary \
        actionlint \
        "https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_linux_amd64.tar.gz" \
        "$ACTIONLINT_LINUX_X64_SHA256" \
        "actionlint.tar.gz" \
        actionlint
      ;;
    Darwin:arm64)
      install_linux_archive_binary \
        actionlint \
        "https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_darwin_arm64.tar.gz" \
        "$ACTIONLINT_MACOS_ARM64_SHA256" \
        "actionlint.tar.gz" \
        actionlint
      ;;
    *)
      echo "Unsupported platform for pinned actionlint: $(uname -s):$(uname -m)" >&2
      exit 1
      ;;
  esac
}

# Install an official checksum-pinned Node/npm distribution under CI_TOOL_ROOT.
# Local bootstrap already exports bin_dir before invoking this child installer;
# hosted callers receive that same PATH through GITHUB_PATH.
install_node() {
  local actual
  if [[ -x "$bin_dir/node" && -x "$bin_dir/npm" ]] \
    && actual="$("$bin_dir/node" --version 2>/dev/null)" \
    && [[ "$actual" == "v$NODE_VERSION" ]]; then
    echo "Using pinned Node $NODE_VERSION"
    return
  fi
  local platform url checksum
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
      platform=linux-x64
      url="$NODE_LINUX_X64_URL"
      checksum="$NODE_LINUX_X64_SHA256"
      ;;
    Darwin:arm64)
      platform=darwin-arm64
      url="https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-${platform}.tar.gz"
      checksum="$NODE_MACOS_ARM64_SHA256"
      ;;
    *)
      echo "Unsupported platform for pinned Node: $(uname -s):$(uname -m)" >&2
      exit 1
      ;;
  esac
  local archive_path="$download_dir/node.tar.gz"
  local extract_dir="$download_dir/node-extract"
  download_verified "$url" "$checksum" "$archive_path"
  rm -rf "$extract_dir"
  mkdir -p "$extract_dir"
  tar -xzf "$archive_path" -C "$extract_dir"
  rm -rf "$tool_root/node"
  mv "$extract_dir/node-v${NODE_VERSION}-${platform}" "$tool_root/node"
  local executable
  for executable in node npm npx; do
    ln -sf "../node/bin/$executable" "$bin_dir/$executable"
  done
  actual="$("$bin_dir/node" --version)"
  if [[ "$actual" != "v$NODE_VERSION" || ! -x "$bin_dir/npm" ]]; then
    echo "FAIL: pinned Node/npm installation did not verify (Node: $actual)" >&2
    exit 1
  fi
}

install_py_jsonschema() {
  # Pin the Python jsonschema used for SBOM semantic validation. Install into
  # the isolated tool-root so the package never leaks into the system
  # site-packages and the version stays deterministic.
  if ! command -v python3 >/dev/null 2>&1; then
    echo "FAIL: python3 is required to install pinned jsonschema" >&2
    exit 1
  fi
  if ! python3 -m pip --version >/dev/null 2>&1; then
    # Some minimal CI bases ship python3 without pip; bootstrap it so the
    # pinned dependency can still be installed deterministically.
    python3 -m ensurepip --user >/dev/null 2>&1 || true
  fi
  if ! python3 -m pip --version >/dev/null 2>&1; then
    echo "FAIL: cannot install pinned jsonschema (pip unavailable)" >&2
    exit 1
  fi
  rm -rf "$tool_root/pythonlib"
  python3 -m pip install --quiet --disable-pip-version-check \
    --target "$tool_root/pythonlib" "jsonschema==${PY_JSONSCHEMA_VERSION}"
  if [[ -n "${GITHUB_ENV:-}" ]]; then
    printf 'PYTHONPATH=%s\n' "$tool_root/pythonlib${PYTHONPATH:+:$PYTHONPATH}" >> "$GITHUB_ENV"
  elif [[ -z "${PYTHONPATH:-}" || ":${PYTHONPATH}:" != *":${tool_root}/pythonlib:"* ]]; then
    export PYTHONPATH="$tool_root/pythonlib${PYTHONPATH:+:$PYTHONPATH}"
  fi
}

install_ripgrep() {
  case "$(uname -s):$(uname -m)" in
    Darwin:arm64)
      install_linux_archive_binary \
        rg \
        "https://github.com/BurntSushi/ripgrep/releases/download/${RIPGREP_VERSION}/ripgrep-${RIPGREP_VERSION}-aarch64-apple-darwin.tar.gz" \
        "$RIPGREP_MACOS_ARM64_SHA256" \
        "ripgrep.tar.gz" \
        "ripgrep-${RIPGREP_VERSION}-aarch64-apple-darwin/rg"
      ;;
    Linux:x86_64)
      install_linux_archive_binary \
        rg \
        "https://github.com/BurntSushi/ripgrep/releases/download/${RIPGREP_VERSION}/ripgrep-${RIPGREP_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
        "$RIPGREP_LINUX_X64_SHA256" \
        "ripgrep-linux.tar.gz" \
        "ripgrep-${RIPGREP_VERSION}-x86_64-unknown-linux-musl/rg"
      ;;
    *)
      echo "Unsupported platform for pinned ripgrep: $(uname -s):$(uname -m)" >&2
      exit 1
      ;;
  esac
}

install_gitleaks() {
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
      install_linux_archive_binary \
        gitleaks \
        "https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/gitleaks_${GITLEAKS_VERSION}_linux_x64.tar.gz" \
        "$GITLEAKS_LINUX_X64_SHA256" \
        "gitleaks.tar.gz" \
        gitleaks
      ;;
    Darwin:arm64)
      install_linux_archive_binary \
        gitleaks \
        "https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/gitleaks_${GITLEAKS_VERSION}_darwin_arm64.tar.gz" \
        "$GITLEAKS_MACOS_ARM64_SHA256" \
        "gitleaks.tar.gz" \
        gitleaks
      ;;
    *)
      echo "Gitleaks is pinned for Linux x86-64 and macOS Apple Silicon jobs only." >&2
      ;;
  esac
}

install_audit_tools() {
  install_cargo_tool cargo-deny "$CARGO_DENY_VERSION"
  install_cargo_tool cargo-about "$CARGO_ABOUT_VERSION" --features cli
  install_actionlint
  install_ripgrep
  install_redocly_cli
  install_py_jsonschema
  install_gitleaks
}

install_coverage_tools() {
  install_cargo_tool cargo-llvm-cov "$CARGO_LLVM_COV_VERSION"
  verify_cargo_tool cargo-llvm-cov "$CARGO_LLVM_COV_VERSION"
}

install_lychee() {
  install_linux_archive_binary \
    lychee \
    "https://github.com/lycheeverse/lychee/releases/download/lychee-v${LYCHEE_VERSION}/lychee-x86_64-unknown-linux-gnu.tar.gz" \
    "$LYCHEE_LINUX_X64_SHA256" \
    "lychee.tar.gz" \
    lychee
}

install_redocly_cli() {
  install_node
  # Install from the committed lockfile (scripts/ci/redocly/package-lock.json),
  # never from a live registry, so the resolved tree is supply-chain pinned.
  # Disable lifecycle scripts and audit so dependency install hooks cannot
  # execute before the security-audit job runs its own script-disabled install
  # and High/Critical audit.
  npm ci --ignore-scripts --no-audit --prefix "$ROOT_DIR/scripts/ci/redocly"
}

# Install the locked native Markdown CLI; npm cache location remains caller-owned.
install_markdown_tools() {
  install_node
  npm ci --ignore-scripts --no-audit --prefix "$ROOT_DIR/scripts/ci/markdown"
}

install_release_tools() {
  install_node
  install_cargo_tool cargo-deny "$CARGO_DENY_VERSION"
  install_cargo_tool cargo-about "$CARGO_ABOUT_VERSION" --features cli
  install_staging_tools
  install_coverage_tools
  install_actionlint
  install_ripgrep
  install_py_jsonschema
}

install_preflight_tools() {
  install_github_cli
  install_ripgrep
}

install_staging_tools() {
  install_cargo_tool cargo-cyclonedx "$CARGO_CYCLONEDX_VERSION"
  install_github_cli
  install_py_jsonschema
  install_minisign
}

install_verification_tools() {
  install_github_cli
  install_py_jsonschema
  install_minisign
}

install_minisign() {
  if [[ "$(uname -s):$(uname -m)" == "Linux:x86_64" ]]; then
    install_linux_archive_binary \
      minisign \
      "https://github.com/jedisct1/minisign/releases/download/${MINISIGN_VERSION}/minisign-${MINISIGN_VERSION}-linux.tar.gz" \
      "$MINISIGN_LINUX_X64_SHA256" \
      "minisign.tar.gz" \
      "minisign-linux/x86_64/minisign"
  elif [[ "$(uname -s):$(uname -m)" == "Darwin:arm64" ]]; then
    local archive_path="$download_dir/minisign-macos.zip"
    download_verified \
      "https://github.com/jedisct1/minisign/releases/download/${MINISIGN_VERSION}/minisign-${MINISIGN_VERSION}-macos.zip" \
      "$MINISIGN_MACOS_ARM64_SHA256" \
      "$archive_path"
    local extract_dir="$download_dir/minisign-macos-extract"
    rm -rf "$extract_dir"
    mkdir -p "$extract_dir"
    unzip -q "$archive_path" -d "$extract_dir"
    install -m 0755 "$extract_dir/minisign" "$bin_dir/minisign"
  fi
}

if [[ "${CI_TOOLS_LIST_ONLY:-0}" == "1" ]]; then
  list_tools_for_mode "$mode"
  exit 0
fi

case "$mode" in
  audit) install_audit_tools ;;
  coverage) install_coverage_tools ;;
  links) install_lychee ;;
  static) install_actionlint ;;
  node) install_node ;;
  markdown) install_markdown_tools ;;
  preflight) install_preflight_tools ;;
  release) install_release_tools ;;
  staging) install_staging_tools ;;
  verification) install_verification_tools ;;
  gitleaks) install_gitleaks ;;
  all)
    install_audit_tools
    install_release_tools
    install_lychee
    install_markdown_tools
    ;;
esac

echo "Pinned CI tools installed in $bin_dir"
