#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# shellcheck disable=SC1091
source scripts/ci/tool-versions.env

fail=0
report_failure() {
  echo "FAIL: $1" >&2
  fail=1
}

while IFS= read -r action_ref; do
  [[ -n "$action_ref" ]] || continue
  sha="${action_ref##*@}"
  if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    report_failure "mutable or malformed Action reference: $action_ref"
  fi
done < <(grep -RhoE 'uses:[[:space:]]*[^[:space:]]+@[A-Za-z0-9._/-]+' .github/workflows)

for variable in \
  CARGO_DENY_VERSION \
  CARGO_ABOUT_VERSION \
  CARGO_CYCLONEDX_VERSION \
  CARGO_LLVM_COV_VERSION \
  LYCHEE_VERSION \
  LYCHEE_LINUX_X64_SHA256 \
  GITLEAKS_VERSION \
  GITLEAKS_LINUX_X64_SHA256 \
  GITLEAKS_MACOS_ARM64_SHA256 \
  RIPGREP_VERSION \
  RIPGREP_LINUX_X64_SHA256 \
  RIPGREP_MACOS_ARM64_SHA256 \
  ACTIONLINT_VERSION \
  ACTIONLINT_LINUX_X64_SHA256 \
  ACTIONLINT_MACOS_ARM64_SHA256 \
  MINISIGN_VERSION \
  MINISIGN_LINUX_X64_SHA256 \
  GH_VERSION \
  GH_LINUX_X64_SHA256 \
  GH_MACOS_ARM64_SHA256 \
  REDOCLY_CLI_VERSION \
  MARKDOWNLINT_VERSION \
  NODE_VERSION \
  NODE_LINUX_X64_URL \
  NODE_LINUX_X64_SHA256 \
  NODE_MACOS_ARM64_SHA256 \
  CI_CONTAINER_IMAGE \
  CI_ALPINE_CONTAINER_IMAGE \
  LINUX_RUNNER_IMAGE \
  MACOS_RUNNER_IMAGE \
  LINUX_ARM_RUNNER_IMAGE \
  MACOS_INTEL_RUNNER_IMAGE \
  APT_CURL_PACKAGE \
  APT_UNZIP_PACKAGE \
  APT_NODEJS_PACKAGE \
  APT_NPM_PACKAGE \
  APT_BUBBLEWRAP_PACKAGE \
  APT_RIPGREP_PACKAGE \
  APT_GIT_PACKAGE \
  APT_JQ_PACKAGE \
  APT_PYTHON3_PACKAGE \
  APT_PYTHON3_PIP_PACKAGE \
  ALPINE_BASH_PACKAGE \
  ALPINE_GIT_PACKAGE \
  ALPINE_TAR_PACKAGE \
  ALPINE_MUSL_DEV_PACKAGE \
  ALPINE_FILE_PACKAGE \
  ALPINE_JQ_PACKAGE \
  ALPINE_PYTHON3_PACKAGE \
  ALPINE_PY3_PIP_PACKAGE \
  ALPINE_BUBBLEWRAP_PACKAGE \
  ALPINE_CURL_PACKAGE \
  ALPINE_RIPGREP_PACKAGE; do
  if [[ -z "${!variable:-}" ]]; then
    report_failure "$variable is not declared in scripts/ci/tool-versions.env"
  fi
done

# Every released archive target must be represented so the Alpine musl archive
# cannot silently drift out of the release matrix.
if ! python3 - <<'PY' >/dev/null
import json, re, sys
with open("scripts/ci/release-archive-inventory.json", encoding="utf-8") as f:
    inventory = json.load(f)
triples = {e["target_triple"] for e in inventory["released_archives"]}
with open("scripts/ci/tool-versions.env", encoding="utf-8") as f:
    env = f.read()
assert "x86_64-unknown-linux-musl" in triples, "musl target missing from release inventory"
assert "CI_ALPINE_CONTAINER_IMAGE=" in env
assert "ALPINE_BUBBLEWRAP_PACKAGE=" in env
assert "ALPINE_TAR_PACKAGE=" in env
PY
then
  report_failure "Alpine/musl must be present in release inventory and tool-versions.env"
fi

if grep -RInE --exclude=check_supply_chain_pins.sh --exclude-dir=node_modules \
  'cargo install (cargo-deny|cargo-about|cargo-cyclonedx)( |$)' \
  .github/workflows scripts/ci >/dev/null; then
  report_failure "Cargo CI tools must be installed through install_ci_tools.sh with exact versions"
fi
if ! grep -Fq "install_cargo_tool cargo-llvm-cov \"\$CARGO_LLVM_COV_VERSION\"" scripts/ci/install_ci_tools.sh; then
  report_failure "cargo-llvm-cov must be installed through the pinned coverage tool path"
fi
if ! grep -Fq "verify_cargo_tool cargo-llvm-cov \"\$CARGO_LLVM_COV_VERSION\"" scripts/ci/install_ci_tools.sh; then
  report_failure "cargo-llvm-cov installation must verify the exact pinned version"
fi
if ! python3 - <<'PY' >/dev/null
import re
from pathlib import Path

text = Path("scripts/ci/install_ci_tools.sh").read_text(encoding="utf-8")
match = re.search(r'^install_release_tools\(\) \{(?P<body>.*?)(?=^}\n)', text, re.M | re.S)
assert match, "install_release_tools() is missing"
assert "install_coverage_tools" in match.group("body"), (
    "release tool installation must include the verified coverage tool"
)
PY
then
  report_failure "release tool installation does not include verified cargo-llvm-cov"
fi
if ! grep -Fq 'lychee-x86_64-unknown-linux-gnu.tar.gz' scripts/ci/install_ci_tools.sh \
  || ! grep -Fq "\"\$LYCHEE_LINUX_X64_SHA256\"" scripts/ci/install_ci_tools.sh; then
  report_failure "Lychee must be installed from its pinned release asset with SHA-256 verification"
fi
if ! grep -Fq "ripgrep-\${RIPGREP_VERSION}-x86_64-unknown-linux-musl.tar.gz" scripts/ci/install_ci_tools.sh \
  || ! grep -Fq "\"\$RIPGREP_LINUX_X64_SHA256\"" scripts/ci/install_ci_tools.sh; then
  report_failure "Linux ripgrep must be installed from its checksum-pinned release asset"
fi

# CI contract: a clean source build must require only documented tools.
# The repository must not set a mandatory rustc-wrapper (e.g. sccache) in
# .cargo/config.toml because Bookworm and macOS CI jobs do not install one.
# Caching is optional; the build must succeed without any wrapper.
if grep -q 'rustc-wrapper\|rustc-wrapper' .cargo/config.toml 2>/dev/null; then
  report_failure ".cargo/config.toml must not set a mandatory rustc-wrapper (caching is optional)"
fi

if grep -RInE --exclude=check_supply_chain_pins.sh --exclude-dir=node_modules \
  'curl .*\|[[:space:]]*(tar|sh)|wget .*\|[[:space:]]*(tar|sh)' \
  .github/workflows scripts/ci >/dev/null; then
  report_failure "downloaded executables must be saved and SHA-256 verified before extraction"
fi

# System packages must be installed through the pinned installers
# (install_debian_tools.sh / install_alpine_tools.sh) inside digest-pinned
# containers, never by an unpinned `apt-get install` / `apk add` in a workflow.
# The installers read exact pins from tool-versions.env, so a direct install
# would bypass the pin set and drift onto an un-versioned package.
if grep -RInE --exclude=check_supply_chain_pins.sh --exclude-dir=node_modules \
  --exclude=install_debian_tools.sh --exclude=install_alpine_tools.sh \
  'apt-get install|apk add' \
  .github/workflows scripts/ci >/dev/null; then
  report_failure "apt-get/apk installs must go through the pinned install_debian_tools.sh / install_alpine_tools.sh"
fi

# Runner labels and container images must equal the values declared in
# tool-versions.env (LINUX_RUNNER_IMAGE, MACOS_RUNNER_IMAGE,
# LINUX_ARM_RUNNER_IMAGE, MACOS_INTEL_RUNNER_IMAGE, CI_CONTAINER_IMAGE,
# CI_ALPINE_CONTAINER_IMAGE), never a floating `-latest` label or an undeclared
# container. This proves the workflows consume tool-versions.env and cannot
# drift onto an un-versioned runner or container.
#
# The scan covers job-level `runs-on:` / `image:` values, strategy.matrix
# include values (`runner:`, `os:`, `container_image:`), and `${{ matrix.* }}`
# expressions (whose referenced matrix key must exist and be validated through
# its include literals). Both `.yml` and `.yaml` workflow files are scanned.
#
# Package installs (apt-get/apk) are permitted only inside the digest-pinned
# containers validated below; a pinned container base is the accepted source of
# system tooling, and no job installs tools on a floating or unpinned base.
if ! python3 - <<'PY' >/dev/null
import re, sys
from pathlib import Path

env = {}
for m in re.finditer(r'^(\w+)=(\S+)', Path("scripts/ci/tool-versions.env").read_text(encoding="utf-8"), re.M):
    env[m.group(1)] = m.group(2)

# Allowed runner labels and container images come only from tool-versions.env.
allowed_runners = {
    env["LINUX_RUNNER_IMAGE"],
    env["MACOS_RUNNER_IMAGE"],
    env["LINUX_ARM_RUNNER_IMAGE"],
    env["MACOS_INTEL_RUNNER_IMAGE"],
}
allowed_images = {
    env["CI_CONTAINER_IMAGE"],
    env["CI_ALPINE_CONTAINER_IMAGE"],
}
# The declared container images must themselves be digest-pinned.
for image in allowed_images:
    assert re.match(r"^[^@\s]+@sha256:[0-9a-f]{64}$", image), (
        f"declared container image is not digest-pinned: {image}"
    )

runner_key_re = re.compile(r"^-?\s*(runs-on|runner|os):\s*(\S+)\s*$")
image_key_re = re.compile(r"^-?\s*(image|container_image):\s*(\S+)\s*$")
matrix_ref_re = re.compile(r"matrix\.(\w+)")

workflow_files = sorted(Path(".github/workflows").glob("*.yml")) + sorted(
    Path(".github/workflows").glob("*.yaml")
)
bad_runners = []
bad_images = []
unresolved_refs = []
for wf in workflow_files:
    text = wf.read_text(encoding="utf-8")
    # A matrix key is declared when a `runner:`/`os:`/`container_image:` include
    # literal appears in the workflow (the include values are validated above).
    declared_matrix_keys = set()
    for line in text.splitlines():
        stripped = line.strip()
        m = runner_key_re.match(stripped)
        if m:
            key, value = m.group(1), m.group(2).strip("'\"")
            if key != "runs-on":
                declared_matrix_keys.add(key)
            if value.startswith("${{"):
                continue
            if value not in allowed_runners:
                bad_runners.append(f"{wf.name}: {stripped}")
            continue
        m = image_key_re.match(stripped)
        if m:
            key, value = m.group(1), m.group(2).strip("'\"")
            if key != "image":
                declared_matrix_keys.add(key)
            # Empty image means "no container" (macOS jobs); allowed but only
            # together with an allowed runner label.
            if value.startswith("${{") or value == "":
                continue
            if value not in allowed_images:
                bad_images.append(f"{wf.name}: {stripped}")
            continue
    # Validate every `${{ matrix.<key> }}` expression references a matrix key
    # that was declared as a literal include key (and therefore validated above).
    for m in matrix_ref_re.finditer(text):
        key = m.group(1)
        if key in {"runner", "os", "container_image", "image"} and key not in declared_matrix_keys:
            unresolved_refs.append(f"{wf.name}: references undeclared matrix key '{key}'")

for label in bad_runners:
    print(f"  undeclared runner label: {label}", file=sys.stderr)
assert not bad_runners, "every runner label (job-level or matrix) must equal a declared tool-versions.env runner pin"
for image in bad_images:
    print(f"  undeclared container image: {image}", file=sys.stderr)
assert not bad_images, "every container image (job-level or matrix) must equal a digest-pinned CI_CONTAINER_IMAGE/CI_ALPINE_CONTAINER_IMAGE"
for ref in unresolved_refs:
    print(f"  unresolved matrix expression: {ref}", file=sys.stderr)
assert not unresolved_refs, "every ${{ matrix.* }} expression must reference a declared matrix include key"
PY
then
  report_failure "workflow runner/container/matrix pins drifted from tool-versions.env"
fi

# Workflow shell contract. GitHub container jobs default to `sh`; Bash-authored
# `run` blocks (set -euo pipefail, [[ ... ]], arrays, process substitution) must
# resolve to Bash. Every workflow therefore declares workflow-level
# defaults.run.shell: bash, and every Alpine bootstrap step (the only step that
# runs before the pinned Bash package is installed) invokes
# `sh scripts/ci/install_alpine_tools.sh` with `shell: sh` explicitly. No other
# container `run` block relies on GitHub's default `sh` once the workflow-level
# Bash default is in place.
if ! python3 - <<'PY' >/dev/null
import re, sys
from pathlib import Path

required = {"pr-ci.yml", "main-ci.yml", "security.yml", "release.yml"}
workflows = sorted(Path(".github/workflows").glob("*.yml"))
found = {wf.name for wf in workflows}
missing = required - found
assert not missing, f"missing required workflow files: {missing}"

errors = []
for wf in workflows:
    text = wf.read_text(encoding="utf-8")
    # Top-level (column 0) defaults.run.shell: bash.
    if not re.search(r'^defaults:\s*\n  run:\s*\n    shell:\s*bash\s*$', text, re.M):
        errors.append(f"{wf.name}: missing workflow-level defaults.run.shell: bash")
    # Each Alpine bootstrap step must use shell: sh and invoke the installer
    # through sh explicitly. Step chunks are split at the 6-space step boundary.
    for chunk in re.split(r'\n(?=      - )', text):
        if re.search(r'^      - name: Install Alpine runtime and sandbox tools', chunk, re.M):
            if not re.search(r'shell:\s*sh\b', chunk):
                errors.append(f"{wf.name}: Alpine bootstrap step missing 'shell: sh'")
            if not re.search(r'run:\s*sh scripts/ci/install_alpine_tools\.sh', chunk):
                errors.append(
                    f"{wf.name}: Alpine bootstrap step must invoke "
                    f"'sh scripts/ci/install_alpine_tools.sh'"
                )

for e in errors:
    print(f"  shell contract: {e}", file=sys.stderr)
assert not errors, "workflow shell contract violated"
PY
then
  report_failure "workflow shell contract violated (workflow-level Bash or Alpine bootstrap shell: sh)"
fi

if ! grep -Fq 'download_verified' scripts/ci/install_ci_tools.sh; then
  report_failure "install_ci_tools.sh does not route downloads through SHA-256 verification"
fi

if ! grep -Rql 'install_ci_tools.sh' .github/workflows; then
  report_failure "workflows do not consume the pinned CI tool installer"
fi

# Workflows must install system packages through the pinned installers (Debian
# and Alpine), never a bare `apt-get install`/`apk add`. The installers are the
# single consumers of the APT_*/ALPINE_* pins in tool-versions.env.
if ! grep -Rql 'install_debian_tools.sh' .github/workflows; then
  report_failure "workflows do not consume the pinned Debian package installer"
fi
if ! grep -Rql 'install_alpine_tools.sh' .github/workflows; then
  report_failure "workflows do not consume the pinned Alpine package installer"
fi
if ! grep -Fq "\"\$ALPINE_TAR_PACKAGE\"" scripts/ci/install_alpine_tools.sh; then
  report_failure "install_alpine_tools.sh does not install the pinned GNU tar package"
fi
if ! grep -Fq "grep -q '^tar (GNU tar)'" scripts/ci/install_alpine_tools.sh; then
  report_failure "install_alpine_tools.sh does not verify that tar is GNU tar"
fi

# Alpine jsonschema must be installed into an isolated target directory, never
# into the system site-packages (PEP 668). The installer must use --target,
# verify the import, and propagate PYTHONPATH via GITHUB_ENV for later GitHub
# Actions steps.
if grep -qE 'python3 -m pip install' scripts/ci/install_alpine_tools.sh \
  && ! grep -Fq -- '--target' scripts/ci/install_alpine_tools.sh; then
  report_failure "install_alpine_tools.sh must install jsonschema with --target, not globally"
fi
if ! grep -Fq -- '--target' scripts/ci/install_alpine_tools.sh; then
  report_failure "install_alpine_tools.sh is missing the --target flag for isolated jsonschema install"
fi
if ! grep -Fq "python3 -c 'import jsonschema'" scripts/ci/install_alpine_tools.sh; then
  report_failure "install_alpine_tools.sh must verify jsonschema imports from the isolated target"
fi
if ! grep -Fq 'GITHUB_ENV' scripts/ci/install_alpine_tools.sh; then
  report_failure "install_alpine_tools.sh must propagate PYTHONPATH via GITHUB_ENV for later steps"
fi

# GitHub environment files store literal values; shell parameter expansion must
# happen before writing PYTHONPATH or later steps cannot import the isolated
# jsonschema installation.
if ! python3 - <<'PY' >/dev/null
from pathlib import Path

expected = {
    "scripts/ci/install_alpine_tools.sh": (
        "printf 'PYTHONPATH=%s\\n' \"$pythonlib${PYTHONPATH:+:$PYTHONPATH}\" >> \"$GITHUB_ENV\""
    ),
    "scripts/ci/install_ci_tools.sh": (
        "printf 'PYTHONPATH=%s\\n' \"$tool_root/pythonlib${PYTHONPATH:+:$PYTHONPATH}\" >> \"$GITHUB_ENV\""
    ),
}
malformed = "printf 'PYTHONPATH=%s${PYTHONPATH:+:$PYTHONPATH}\\n'"
for path, required in expected.items():
    text = Path(path).read_text(encoding="utf-8")
    assert required in text, f"{path} must expand PYTHONPATH before writing GITHUB_ENV"
    assert malformed not in text, f"{path} writes an unexpanded shell expression to GITHUB_ENV"
PY
then
  report_failure "isolated Python paths must be expanded before writing GITHUB_ENV"
fi

# The Debian installer must map the pip3 tool name to the pinned
# APT_PYTHON3_PIP_PACKAGE so Bookworm jobs can install pip explicitly instead of
# relying on the base image or a transitive package.
if ! grep -Fq 'pip3) echo "APT_PYTHON3_PIP_PACKAGE"' scripts/ci/install_debian_tools.sh; then
  report_failure "install_debian_tools.sh does not map pip3 to APT_PYTHON3_PIP_PACKAGE"
fi

# Each Bookworm container job must install its complete Python/pip/jq
# requirements explicitly through install_debian_tools.sh before any step that
# uses them. No Bookworm job may depend on python3, pip3, or jq supplied by the
# base image or a transitive package.
if ! python3 - <<'PY' >/dev/null
import re, sys
from pathlib import Path

# Map each workflow/job to the set of Debian tools that must appear in the
# job's install_debian_tools.sh call(s). Jobs are identified by their job key
# in the workflow file. A job is a "Bookworm container job" when its container
# image is the digest-pinned CI_CONTAINER_IMAGE (rust:1.88.0-bookworm).
env = {}
for m in re.finditer(r'^(\w+)=(\S+)', Path("scripts/ci/tool-versions.env").read_text(encoding="utf-8"), re.M):
    env[m.group(1)] = m.group(2)
bookworm_image = env["CI_CONTAINER_IMAGE"]

# Required tools per job class. Every Bookworm container job that runs audit,
# E2E, shipped-workflow, schema, archive, SBOM, regression, provenance, or
# packaging steps must have jq, python3, and pip3 available (pip3 is required
# for install_ci_tools.sh audit/release which installs pinned jsonschema).
job_requirements = {
    ("pr-ci.yml", "validate-full"): {"bubblewrap", "ripgrep", "git", "jq", "python3", "pip3"},
    ("pr-ci.yml", "schema-api-contracts"): {"jq", "python3", "pip3"},
    ("pr-ci.yml", "cargo-deny"): {"jq", "python3", "pip3"},
    ("pr-ci.yml", "coverage"): {"bubblewrap", "git", "jq", "python3"},
    ("main-ci.yml", "platform-smoke"): {"bubblewrap", "git", "python3"},
    ("main-ci.yml", "coverage"): {"bubblewrap", "git", "jq", "python3"},
    ("security.yml", "security-audit"): {"python3", "pip3"},
    ("release.yml", "build"): {"jq", "python3"},
    ("release.yml", "package"): {"jq", "python3"},
    ("release.yml", "archive-smoke"): {"jq", "python3"},
    ("release.yml", "stage-and-sign"): {"curl", "git", "jq", "pip3", "python3", "unzip"},
}
bookworm_required = {("release.yml", "stage-and-sign")}

errors = []
for (wf_name, job_key), required in sorted(job_requirements.items()):
    wf_path = Path(".github/workflows") / wf_name
    text = wf_path.read_text(encoding="utf-8")
    # Extract the job block (from "  <job-key>:" at column 2 to the next job
    # or end of file).
    job_block_match = re.search(
        rf'^  {re.escape(job_key)}:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)',
        text, re.M | re.S,
    )
    if not job_block_match:
        errors.append(f"{wf_name}: job '{job_key}' not found")
        continue
    job_body = job_block_match.group("body")
    if (wf_name, job_key) in bookworm_required and not re.search(
        rf'container:\s*\n\s+image:\s*{re.escape(bookworm_image)}\s*$', job_body, re.M
    ):
        errors.append(f"{wf_name}: job '{job_key}' must use the pinned Bookworm container")
    # Skip if the job is not a Bookworm container job (e.g. a matrix job where
    # the GNU entry uses the bookworm image). For matrix jobs, we check that
    # the Debian install step is present but not which matrix entry triggers
    # it; the workflow-level shell and the install_debian_tools.sh call are
    # the contract.
    # Collect all tools passed to install_debian_tools.sh in this job.
    installed = set()
    for m in re.finditer(r'install_debian_tools\.sh\s+([^\n]+)', job_body):
        tools = m.group(1).strip().rstrip('\\').strip().strip('"').strip("'")
        for t in tools.split():
            installed.add(t)
    missing = required - installed
    if missing:
        errors.append(
            f"{wf_name} job '{job_key}': missing required Debian tools: "
            f"{', '.join(sorted(missing))} (installed: {', '.join(sorted(installed)) or 'none'})"
        )

for e in errors:
    print(f"  debian bootstrap: {e}", file=sys.stderr)
assert not errors, "Bookworm container jobs are missing required pinned Debian tool installs"
PY
then
  report_failure "Bookworm container job bootstrap sets are incomplete (missing jq/python3/pip3)"
fi

# Redocly is installed from a committed lockfile, never a global or live
# `npm install -g`. The lockfile is the supply-chain integrity source.
if grep -RInE --exclude=check_supply_chain_pins.sh --exclude-dir=node_modules \
  'npm install -g.*@redocly/cli|npm install -g.*redocly' \
  .github/workflows scripts/ci >/dev/null; then
  report_failure "Redocly must be installed from the committed lockfile via npm ci, not npm install -g"
fi
if [[ ! -f scripts/ci/redocly/package-lock.json ]]; then
  report_failure "committed Redocly lockfile scripts/ci/redocly/package-lock.json is missing"
fi
if ! grep -q '"@redocly/cli":' scripts/ci/redocly/package.json; then
  report_failure "scripts/ci/redocly/package.json does not pin @redocly/cli"
fi
if ! grep -q '"node_modules/@redocly/cli"' scripts/ci/redocly/package-lock.json; then
  report_failure "scripts/ci/redocly/package-lock.json does not resolve @redocly/cli"
fi

# The locked npm tool tree must remain covered by Dependabot and by a blocking
# High/Critical audit in the security-audit job. Keep the install script-free
# so dependency lifecycle hooks cannot execute before the audit runs.
if ! python3 - <<'PY' >/dev/null
import json
import re
from pathlib import Path

dependabot = Path(".github/dependabot.yml").read_text(encoding="utf-8")
security = Path(".github/workflows/security.yml").read_text(encoding="utf-8")
versions = dict(re.findall(r"^(\w+)=(\S+)", Path("scripts/ci/tool-versions.env").read_text(), re.M))
assert re.fullmatch(r"22\.\d+\.\d+", versions["NODE_VERSION"]), "Node must be pinned to an exact Node 22 release"
assert versions["NODE_LINUX_X64_URL"] == (
    f'https://nodejs.org/dist/v{versions["NODE_VERSION"]}/node-v{versions["NODE_VERSION"]}-linux-x64.tar.gz'
), "Node URL must match the pinned official release"
for name in ("NODE_LINUX_X64_SHA256", "NODE_MACOS_ARM64_SHA256"):
    assert re.fullmatch(r"[0-9a-f]{64}", versions[name]), f"invalid Node checksum: {name}"
installer = Path("scripts/ci/install_ci_tools.sh").read_text(encoding="utf-8")
node_body = re.search(r"^install_node\(\) \{(.*?)^}", installer, re.M | re.S).group(1)
assert 'download_verified "$url" "$checksum" "$archive_path"' in node_body
assert 'checksum="$NODE_LINUX_X64_SHA256"' in node_body
for function in ("install_release_tools", "install_redocly_cli", "install_markdown_tools"):
    body = re.search(rf"^{function}\(\) \{{(.*?)^}}", installer, re.M | re.S).group(1)
    assert "install_node" in body, f"{function} must provision Node before npm"
    assert "npm ci" not in body or body.index("install_node") < body.index("npm ci")

manifest = json.loads(Path("scripts/ci/markdown/package.json").read_text())
lock = json.loads(Path("scripts/ci/markdown/package-lock.json").read_text())
version = versions["MARKDOWNLINT_VERSION"]
assert manifest["dependencies"]["markdownlint"] == version
assert lock["packages"][""]["dependencies"] == manifest["dependencies"]
assert lock["packages"]["node_modules/markdownlint"]["version"] == version
assert lock["packages"]["node_modules/markdownlint"]["integrity"]
for dependency, pin in manifest["dependencies"].items():
    assert re.fullmatch(r"\d+\.\d+\.\d+", pin), f"Markdown dependency must be exact: {dependency}"
    assert lock["packages"][f"node_modules/{dependency}"]["version"] == pin
for name, package in lock["packages"].items():
    if name:
        assert package.get("integrity"), f"Markdown dependency missing integrity: {name}"
        assert package["resolved"].startswith("https://registry.npmjs.org/"), name
assert not any(
    name.rsplit("node_modules/", 1)[-1] in {"braces", "micromatch", "markdownlint-cli2"}
    for name in lock["packages"]
), "Markdown tools must not reintroduce the unpatched CLI2 glob tree"
audit_job_match = re.search(
    r"^  security-audit:\s*\n(?P<body>.*?)(?=^  [\w-]+:\s*\n|\Z)",
    security,
    re.M | re.S,
)
assert audit_job_match, "security-audit job is missing"
audit_job = audit_job_match.group("body")

assert re.search(
    r"install_debian_tools\.sh[^\n]*\bnodejs\b[^\n]*\bnpm\b",
    audit_job,
), "security-audit must install pinned nodejs and npm through install_debian_tools.sh"
assert not re.search(r"^    continue-on-error:", audit_job, re.M), "security-audit must remain blocking"
steps = re.split(r"^      - ", audit_job, flags=re.M)[1:]
for tool in ("redocly", "markdown"):
    directory = f"scripts/ci/{tool}"
    assert re.search(
        rf"package-ecosystem:\s*npm\s+directory:\s*[\"']?/{directory}[\"']?\s",
        dependabot,
    ), f"Dependabot npm coverage for /{directory} is missing"
    indices = []
    for command in ("npm ci --ignore-scripts --no-audit", "npm audit --audit-level=high"):
        matches = [
            (index, step) for index, step in enumerate(steps)
            if re.search(rf"^        working-directory: {directory}$", step, re.M)
            and re.search(rf"^        run: {re.escape(command)}$", step, re.M)
        ]
        assert len(matches) == 1, f"missing single blocking {command} for {directory}"
        index, step = matches[0]
        assert not re.search(r"^        (if|continue-on-error):", step, re.M), (
            f"npm safety step must not skip/ignore failure: {directory}/{command}"
        )
        indices.append(index)
    assert indices[0] < indices[1], f"install the locked {tool} tree before auditing it"
PY
then
  report_failure "npm dependency safety enforcement is missing or malformed"
fi

# Mutation coverage: no CI script or workflow may run a lifecycle-enabled
# npm install (npm ci without --ignore-scripts). The common Redocly installer
# (install_ci_tools.sh) and the security-audit job must both disable scripts.
# This catches a regression where the common installer is changed back to a
# bare `npm ci` that runs dependency install hooks before the audit.
if ! python3 - <<'PY' >/dev/null
import re, sys
from pathlib import Path

# install_ci_tools.sh must use --ignore-scripts --no-audit for Redocly.
installer = Path("scripts/ci/install_ci_tools.sh").read_text(encoding="utf-8")
# Find the npm ci call in install_redocly_cli.
npm_ci_calls = re.findall(r'npm ci[^\n]*', installer)
for call in npm_ci_calls:
    if "--ignore-scripts" not in call:
        print(f"  install_ci_tools.sh: npm ci without --ignore-scripts: {call.strip()}", file=sys.stderr)
        sys.exit(1)
    if "--no-audit" not in call:
        print(f"  install_ci_tools.sh: npm ci without --no-audit: {call.strip()}", file=sys.stderr)
        sys.exit(1)

# Scan all workflow files and CI scripts for bare `npm ci` without
# --ignore-scripts (excluding check_supply_chain_pins.sh itself and the
# security-audit job which is already validated above).
scan_files = list(Path("scripts/ci").glob("*.sh")) + list(Path(".github/workflows").glob("*.yml"))
for f in scan_files:
    if f.name == "check_supply_chain_pins.sh":
        continue
    text = f.read_text(encoding="utf-8")
    for m in re.finditer(r'npm ci(?!\s+--ignore-scripts)[^\n]*', text):
        line = text[:m.start()].count("\n") + 1
        print(f"  {f}:{line}: npm ci without --ignore-scripts: {m.group().strip()}", file=sys.stderr)
        sys.exit(1)
PY
then
  report_failure "npm lifecycle scripts are not disabled in all CI npm ci invocations"
fi

if command -v actionlint >/dev/null 2>&1; then
  actionlint
else
  report_failure "actionlint is required to validate workflow syntax"
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "Supply-chain pin checks passed."
