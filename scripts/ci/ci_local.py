#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Serial local CI orchestration; checks never install tools without --bootstrap.

Docker mounts a host-filtered source snapshot read-only and reconstructs Git
history from a separate bundle. Dirty source/index bytes survive; host runtime
state, credentials, Git config and executable Cargo shims are never mounted.
"""

import argparse
from collections.abc import Callable
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from typing import TypeVar

ROOT = Path(__file__).resolve().parents[2]
YAML_VERSION = "5.5.0"
T = TypeVar("T")
SECURITY_OPTIONS = [
    "--cap-add=SYS_ADMIN",
    "--cap-add=NET_ADMIN",
    "--security-opt",
    "apparmor=unconfined",
    "--security-opt",
    "seccomp=unconfined",
]
EXCLUDED = {
    ".git",
    "target",
    "target-linux",
    ".transist",
    ".transist-dev",
    ".transist-tmp",
    ".tmp_transist_home",
    "dist",
    "coverage",
    ".tmp",
    "tmp",
    "trust-input",
    "trust-output",
    "secrets",
    "run:",
    ".DS_Store",
}
PRIVATE_NAMES = {
    ".git",
    ".transist",
    ".aws",
    ".ssh",
    ".gnupg",
    ".gitconfig",
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "id_rsa",
    "id_ed25519",
    "secrets",
}


def execute(
    args: list[str],
    *,
    root: Path = ROOT,
    env: dict[str, str] | None = None,
    capture: bool = False,
    trim: bool = True,
) -> str:
    """Run an argv without a shell; propagate failures and optionally return stdout."""
    result = subprocess.run(
        args,
        cwd=root,
        env=env,
        check=True,
        text=True,
        errors="surrogateescape",
        stdout=subprocess.PIPE if capture else None,
    )
    return (result.stdout.strip() if trim else result.stdout) if capture else ""


def require_tools(names: list[str]) -> None:
    """Fail with an actionable diagnostic before invoking any absent executable."""
    missing = [name for name in names if shutil.which(name) is None]
    if missing:
        raise ValueError(
            "Missing prerequisites: "
            + ", ".join(missing)
            + ". Install host tools or explicitly use --bootstrap."
        )


def node_prerequisite(root: Path) -> None:
    """Require Node 22+ before using locked npm tools; never install it implicitly."""
    require_tools(["node", "npm"])
    version = execute(["node", "--version"], root=root, capture=True)
    match = re.fullmatch(r"v(\d+)\.\d+\.\d+", version)
    if match is None or int(match.group(1)) < 22:
        raise ValueError(
            f"Node >=22 is required; use the pinned Node 22 tooling (got {version!r})"
        )


def resolve_refs(root: Path, base: str | None) -> tuple[str, str]:
    """Resolve a complete, connected BASE/HEAD history locally; never fetch refs."""
    if not base:
        raise ValueError("--base REF is required for pr and debian")
    head = execute(
        ["git", "rev-parse", "--verify", "HEAD^{commit}"], root=root, capture=True
    )
    resolved = execute(
        ["git", "rev-parse", "--verify", "--end-of-options", base + "^{commit}"],
        root=root,
        capture=True,
    )
    if (
        execute(
            ["git", "rev-parse", "--is-shallow-repository"], root=root, capture=True
        )
        != "false"
    ):
        raise ValueError("Complete Git history is required; fetch it explicitly first")
    execute(["git", "merge-base", resolved, head], root=root, capture=True)
    return resolved, head


def bootstrap(root: Path) -> None:
    """Explicitly install pinned Rust components and locked/pinned CI tools."""
    require_tools(["rustup", "cargo", "curl", "python3"])
    execute(
        [
            "rustup",
            "toolchain",
            "install",
            "1.88.0",
            "--profile",
            "minimal",
            "--component",
            "rustfmt",
            "--component",
            "clippy",
            "--component",
            "llvm-tools-preview",
            "--no-self-update",
        ],
        root=root,
    )
    execute(["bash", "scripts/ci/install_ci_tools.sh", "release"], root=root)
    node_prerequisite(root)
    for package in ("redocly", "markdown"):
        execute(
            [
                "npm",
                "ci",
                "--ignore-scripts",
                "--no-audit",
                "--prefix",
                f"scripts/ci/{package}",
            ],
            root=root,
        )


def native_prerequisites(root: Path) -> None:
    """Check installed tooling/components without rustup auto-install or compilation."""
    require_tools(
        [
            "bash",
            "git",
            "rustup",
            "rustc",
            "cargo",
            "node",
            "npm",
            "jq",
            "rg",
            "curl",
            "tar",
            "actionlint",
            "cargo-deny",
            "cargo-llvm-cov",
            "cargo-about",
            "cargo-cyclonedx",
            "minisign",
        ]
    )
    node_prerequisite(root)
    execute(["rustup", "run", "1.88.0", "rustc", "--version"], root=root)
    components = execute(
        ["rustup", "component", "list", "--installed", "--toolchain", "1.88.0"],
        root=root,
        capture=True,
    )
    for component in ("rustfmt", "clippy", "llvm-tools"):
        if not any(
            line.startswith(component + "-") for line in components.splitlines()
        ):
            raise ValueError(f"Missing Rust component {component}; use --bootstrap")
    for relative in (
        "scripts/ci/check_markdown.sh",
        "scripts/ci/check_gate_results.sh",
        "scripts/ci/markdown/node_modules/markdownlint/lib/exports.mjs",
        "scripts/ci/redocly/node_modules/.bin/redocly",
    ):
        if not (root / relative).is_file():
            raise ValueError(
                f"Missing {relative}; prepare locked tools with --bootstrap"
            )


def isolation_probe(root: Path) -> None:
    """Fail before compilation if the native sandbox backend cannot create isolation."""
    if sys.platform == "linux":
        require_tools(["bwrap"])
        execute(
            [
                "bwrap",
                "--die-with-parent",
                "--unshare-user",
                "--unshare-net",
                "--ro-bind",
                "/",
                "/",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--",
                "/bin/true",
            ],
            root=root,
        )
    elif sys.platform == "darwin":
        require_tools(["sandbox-exec"])
        execute(
            [
                "sandbox-exec",
                "-p",
                "(version 1)(allow default)(deny network*)",
                "/usr/bin/true",
            ],
            root=root,
        )
    else:
        raise ValueError("Native PR profile requires Linux or macOS")


class Stages:
    """Run fail-fast stages and retain timings/status, including the failing stage."""

    def __init__(self) -> None:
        """Create an empty per-invocation summary."""
        self.results: list[tuple[str, str, float]] = []

    def run(self, name: str, operation: Callable[[], T]) -> T:
        """Return one operation's value; record failures before propagating the exception."""
        start = time.monotonic()
        status = "FAIL"
        print(f">>> {name}", flush=True)
        try:
            result = operation()
            status = "PASS"
            return result
        finally:
            self.results.append((name, status, time.monotonic() - start))

    def summary(self) -> None:
        """Print the completed/failed stages; unexecuted work is never called passed."""
        print("\n=== local CI summary ===", flush=True)
        for name, status, elapsed in self.results:
            print(f"{name:24} {status:4} {elapsed:8.2f}s", flush=True)
        print(f"TOTAL {sum(row[2] for row in self.results):.2f}s", flush=True)


def coverage(root: Path) -> None:
    """Reuse an ABI/profile-specific LLVM target and retain unique evidence per run."""
    target = Path(os.environ.get("CARGO_TARGET_DIR", str(root / "target")))
    if not target.is_absolute():
        target = root / target
    compiler = execute(["rustc", "-vV"], root=root, capture=True)
    hosts = re.findall(r"^host: ([\w.-]+)$", compiler, re.M)
    if len(hosts) != 1:
        raise ValueError("Coverage requires one explicit Rust host target")
    build_target = os.environ.get("CARGO_BUILD_TARGET", "")
    settings = {
        key: os.environ.get(key, "")
        for key in (
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTDOCFLAGS",
            "CARGO_BUILD_TARGET",
        )
    }
    identity = hashlib.sha256(
        json.dumps(
            [str(root.resolve()), compiler, "pr-default", settings], sort_keys=True
        ).encode()
    ).hexdigest()[:20]
    output_parent = target / "ci-local"
    output_parent.mkdir(parents=True, exist_ok=True)
    output = Path(tempfile.mkdtemp(prefix="coverage-", dir=output_parent))
    abi = re.sub(r"[^\w.-]", "_", build_target or hosts[0])
    llvm_target = output_parent / f"llvm-pr-default-{abi}-{identity}"
    launcher_target = target / build_target if build_target else target
    env = dict(
        os.environ,
        CARGO_LLVM_COV_TARGET_DIR=str(llvm_target),
        TRANSIST_SANDBOX_LAUNCHER=str(
            launcher_target / "debug/transist-sandbox-launcher"
        ),
    )
    print(f"Coverage evidence: {output / 'reports'}", flush=True)
    execute(
        ["bash", "scripts/ci/run_coverage.sh", str(output / "reports")],
        root=root,
        env=env,
    )


def run_native(root: Path, base: str, stages: Stages, *, install: bool = False) -> None:
    """Run the complete PR profile serially, with cheap failures before compilation."""
    # Resolve history even before an explicitly requested expensive tool bootstrap.
    stages.run("base refs", lambda: resolve_refs(root, base))
    if install:
        stages.run("bootstrap", lambda: bootstrap(root))
    stages.run("prerequisites", lambda: native_prerequisites(root))
    commands = [
        ("static", ["bash", "scripts/ci/check_ci_static.sh"]),
        ("DCO", ["bash", "scripts/ci/check_commit_signoffs.sh", base, "HEAD"]),
        ("markdown", ["bash", "scripts/ci/check_markdown.sh"]),
        ("cargo deny", ["cargo", "deny", "check"]),
    ]
    for name, command in commands:
        stages.run(name, lambda command=command: execute(command, root=root))
    stages.run("isolation", lambda: isolation_probe(root))
    stages.run(
        "schema",
        lambda: execute(
            ["bash", "scripts/ci/check_schema_api_contracts.sh"], root=root
        ),
    )
    stages.run(
        "full PR readiness",
        lambda: execute(["bash", "scripts/ci/check_pr_ready.sh"], root=root),
    )
    stages.run("coverage", lambda: coverage(root))
    print(
        "Local evidence only: hosted dependency-review and remote gates are not evaluated."
    )


def load_workflows(root: Path, *, tools: Path | None = None) -> list[dict]:
    """Parse all workflow YAML with the exact locked js-yaml, rejecting duplicate keys."""
    tool = tools or root / "scripts/ci/redocly"
    lock = json.loads(
        (root / "scripts/ci/redocly/package-lock.json").read_text(encoding="utf-8")
    )
    pinned = lock["packages"].get("node_modules/js-yaml", {})
    if pinned.get("version") != YAML_VERSION or not pinned.get("integrity"):
        raise ValueError(
            f"Locked js-yaml@{YAML_VERSION} is required in scripts/ci/redocly"
        )
    node_prerequisite(root)
    # js-yaml's default loader rejects duplicates (json:true would weaken this).
    program = """
const fs = require('fs');
const path = require('path');
const tool = process.argv[1];
const yaml = require(path.join(tool, 'node_modules/js-yaml'));
if (require(path.join(tool, 'node_modules/js-yaml/package.json')).version !== process.argv[2])
  throw new Error('YAML parser version mismatch; run explicit bootstrap');
const docs = process.argv.slice(3).map(file => ({
  file: path.basename(file),
  document: yaml.load(fs.readFileSync(file, 'utf8'), {json: false})
}));
process.stdout.write(JSON.stringify(docs));
"""
    paths = sorted((root / ".github/workflows").glob("*.yml"))
    paths += sorted((root / ".github/workflows").glob("*.yaml"))
    return json.loads(
        execute(
            ["node", "-e", program, str(tool), YAML_VERSION, *map(str, paths)],
            root=root,
            capture=True,
        )
    )


def select_image(documents: list[dict], profile: str) -> str:
    """Select the workflow image, rejecting any Debian/musl disagreement or ambiguity."""
    families: dict[str, set[str]] = {"debian": set(), "alpine": set()}
    main_images: dict[str, set[str]] = {"debian": set(), "alpine": set()}
    for entry in documents:
        jobs = entry["document"]["jobs"]
        for job in jobs.values():
            container = job.get("container", {})
            image = (
                container.get("image", "") if isinstance(container, dict) else container
            )
            images = [image]
            matrix = job.get("strategy", {}).get("matrix", {})
            if image == "${{ matrix.container_image }}":
                if not matrix.get("include") or any(
                    "container_image" not in row for row in matrix["include"]
                ):
                    raise ValueError(
                        "Container matrix must resolve every image explicitly"
                    )
            elif isinstance(image, str) and "${{" in image:
                raise ValueError(f"Unresolved workflow container expression: {image}")
            for row in matrix.get("include", []):
                images.append(row.get("container_image", ""))
            for value in images:
                if not isinstance(value, str):
                    raise ValueError("Non-literal workflow container image")
                if not value or value == "${{ matrix.container_image }}":
                    continue
                family = (
                    "debian"
                    if "bookworm" in value
                    else "alpine" if "alpine" in value else None
                )
                if family is None:
                    raise ValueError(f"Unsupported workflow container image: {value}")
                if family:
                    if not re.fullmatch(r"[^@\s]+@sha256:[0-9a-f]{64}", value):
                        raise ValueError(
                            f"Container image must be digest-pinned: {value}"
                        )
                    families[family].add(value)
                    if entry["file"] == "main-ci.yml":
                        main_images[family].add(value)
            if entry["file"] == "main-ci.yml" and image:
                if image == "${{ matrix.container_image }}":
                    options = [
                        row.get("container_options", "")
                        for row in matrix["include"]
                        if row["container_image"]
                    ]
                else:
                    options = [container.get("options", "")]
                if any(shlex.split(value) != SECURITY_OPTIONS for value in options):
                    raise ValueError(
                        "Main workflow container security options disagree with local runner"
                    )
    for family, values in families.items():
        if len(values) != 1 or values != main_images[family]:
            raise ValueError(
                f"Missing or disagreeing {family} workflow images: {sorted(values)}"
            )
    main_docs = [
        entry["document"] for entry in documents if entry["file"] == "main-ci.yml"
    ]
    if len(main_docs) != 1:
        raise ValueError("Exactly one main-ci.yml workflow is required")
    main_jobs = main_docs[0]["jobs"]
    linux_rows = [
        row
        for row in main_jobs["platform-smoke"]["strategy"]["matrix"]["include"]
        if row.get("platform") == "Linux"
    ]
    if (
        len(linux_rows) != 1
        or linux_rows[0]["container_image"] not in families["debian"]
        or main_jobs["musl-smoke"]["container"]["image"] not in families["alpine"]
        or main_jobs["coverage"]["container"]["image"] not in families["debian"]
    ):
        raise ValueError(
            "Main Linux/musl/coverage jobs disagree with their selected ABI images"
        )
    return next(iter(families[profile]))


def excluded(relative: Path) -> bool:
    """Omit generated/runtime paths and conventional local credential/env files."""
    if not relative.parts:
        return False
    # Cargo build configuration is a release input, not a generated cache.
    # Keep it while excluding credentials, installed tools and registry state.
    if relative.parts[0] == ".cargo" and len(relative.parts) > 1:
        return len(relative.parts) != 2 or relative.parts[1] not in {
            "config",
            "config.toml",
        }
    return (
        relative.parts[0] in EXCLUDED
        or relative.parts[0].startswith((".tmp-", ".transist-"))
        or any(
            part in PRIVATE_NAMES | {"node_modules", "__pycache__"}
            or part.startswith((".env", ".transist-"))
            or part.endswith((".key", ".p12", ".pfx"))
            for part in relative.parts
        )
    )


def copy_checkout(source: Path, destination: Path, *, copy: bool = True) -> None:
    """Validate/copy source without following symlinks or including runtime/caches.

    Relative internal links are retained. Absolute links cannot be relocated
    byte-for-byte safely, so they, escaping links and special files are rejected.
    Git metadata is supplied separately, including for Git worktrees.
    """
    for directory, dirs, files in os.walk(source, followlinks=False):
        current = Path(directory)
        dirs[:] = [
            name for name in dirs if not excluded((current / name).relative_to(source))
        ]
        for name in [*dirs, *files]:
            path = current / name
            relative = path.relative_to(source)
            if excluded(relative):
                continue
            output = destination / relative
            if path.is_symlink():
                resolved = path.resolve()
                if not resolved.is_relative_to(source.resolve()):
                    raise ValueError(f"Snapshot symlink escapes checkout: {relative}")
                if excluded(resolved.relative_to(source.resolve())):
                    continue
                if Path(os.readlink(path)).is_absolute():
                    raise ValueError(
                        f"Snapshot absolute symlink cannot be safely relocated: {relative}"
                    )
                if copy:
                    output.parent.mkdir(parents=True, exist_ok=True)
                    output.symlink_to(os.readlink(path))
            elif path.is_dir():
                if copy:
                    output.mkdir(parents=True, exist_ok=True)
            elif path.is_file():
                if copy:
                    output.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(path, output)
            else:
                raise ValueError(f"Snapshot contains special file: {relative}")
        # Do not leave symlink directories in the walk's traversal list, even
        # when they point inside the checkout. Their links were handled above.
        dirs[:] = [name for name in dirs if not (current / name).is_symlink()]


# /bin/sh is intentional: Alpine has no Bash/Python until explicit bootstrap.
# The host filters/rejects source links before bootstrap. This script sees only
# that filtered snapshot; Python revalidates the disposable worktree copy.
CONTAINER_ENTRY = """set -eu
profile="$1"; bootstrap="$2"; base="$3"; head="$4"; probe_only="$5"
if [ "$bootstrap" = 1 ]; then
  if [ "$profile" = alpine ]; then
    sh /checkout/scripts/ci/install_alpine_tools.sh
  elif [ "$probe_only" = 1 ]; then
    bash /checkout/scripts/ci/install_debian_tools.sh bubblewrap git python3
  else
    bash /checkout/scripts/ci/install_debian_tools.sh bubblewrap ripgrep git jq python3 curl unzip nodejs npm pip3
  fi
fi
for tool in python3 git bash; do
  command -v "$tool" >/dev/null || { echo "Missing $tool; explicitly use --bootstrap" >&2; exit 1; }
done
command -v bwrap >/dev/null || { echo "Missing bubblewrap; explicitly use --bootstrap" >&2; exit 1; }
# Match the runtime's user/network namespace boundary. --unshare-all also
# requests a PID namespace whose proc mount is unsupported in these containers.
bwrap --die-with-parent --unshare-user --unshare-net --ro-bind / / --proc /proc --dev /dev -- /bin/true || {
  echo "FAIL: bubblewrap isolation is unavailable" >&2; exit 1;
}
mkdir -p /work
python3 -c 'import sys; sys.path.insert(0,"/checkout/scripts/ci"); from ci_local import copy_checkout; from pathlib import Path; copy_checkout(Path("/checkout"), Path("/work"))'
git clone --no-checkout /history/history.bundle /tmp/history
mv /tmp/history/.git /work/.git
cd /work
git reset --mixed "$head"
# Recreate the staged index/blobs as well as dirty working bytes. This keeps
# staged additions visible to tracked-file checks without modifying the host.
if [ -s /history/index.patch ]; then git apply --cached /history/index.patch; fi
if [ "$probe_only" = 1 ]; then
  if [ "$bootstrap" = 1 ]; then
    rustup toolchain install 1.88.0 --profile minimal --no-self-update
  fi
  if [ "$profile" = alpine ]; then smoke_profile=alpine; else smoke_profile=linux; fi
  exec bash scripts/ci/run_platform_smoke.sh "$smoke_profile" --probe-only
fi
if [ "$profile" = alpine ]; then
  if [ "$bootstrap" = 1 ]; then
    rustup toolchain install 1.88.0 --profile minimal --no-self-update
    rustup target add --toolchain 1.88.0 x86_64-unknown-linux-musl
  fi
  exec bash scripts/ci/run_platform_smoke.sh alpine
fi
if [ "$bootstrap" = 1 ]; then
  bash scripts/ci/check_ci_local.sh --profile pr --base "$base" --bootstrap
else
  bash scripts/ci/check_ci_local.sh --profile pr --base "$base"
fi
exec bash scripts/ci/run_platform_smoke.sh linux
"""


def repository_identity(root: Path) -> str:
    """Identify repository lineage/origin locally; hash callers must not emit raw origin data."""
    lineage = execute(
        ["git", "rev-list", "--max-parents=0", "HEAD"], root=root, capture=True
    )
    try:
        origin = execute(
            ["git", "config", "--local", "--get", "remote.origin.url"],
            root=root,
            capture=True,
        )
    except subprocess.CalledProcessError as error:
        if (
            error.returncode != 1
        ):  # An absent origin is valid for a local-only repository.
            raise
        origin = ""
    return hashlib.sha256(json.dumps([lineage, origin]).encode()).hexdigest()


def cache_scope(
    root: Path, snapshot: Path, profile: str, image: str, *, identity: str | None = None
) -> str:
    """Key all caches by checkout identity, ABI/image and exact installer/version inputs."""
    digest = hashlib.sha256()
    identity = repository_identity(root) if identity is None else identity
    digest.update(
        json.dumps(
            [str(root.resolve()), identity, profile, "linux/amd64", image]
        ).encode()
    )
    paths = {
        Path("rust-toolchain.toml"),
        Path("Cargo.lock"),
        Path("scripts/ci/tool-versions.env"),
        Path("scripts/ci/ci_local.py"),
    }
    for pattern in ("scripts/ci/install*.sh", "scripts/ci/*/package*.json"):
        paths.update(path.relative_to(snapshot) for path in snapshot.glob(pattern))
    for relative in sorted(paths):
        path = snapshot / relative
        digest.update(str(relative).encode() + b"\0")
        digest.update(path.read_bytes() if path.is_file() else b"<missing>")
        digest.update(b"\0")
    return digest.hexdigest()[:32]


def docker_command(
    snapshot: Path,
    history: Path,
    profile: str,
    image: str,
    base: str,
    head: str,
    install: bool,
    *,
    scope: str,
    probe_only: bool = False,
) -> list[str]:
    """Build exact amd64 Docker argv with read-only inputs and profile-specific volumes."""
    if any("," in str(path) for path in (snapshot, history)):
        raise ValueError("Docker --mount source paths may not contain commas")
    if not re.fullmatch(r"[0-9a-f]{32}", scope):
        raise ValueError(
            "Docker caches require an explicit repository/image/tooling scope"
        )
    prefix = f"transist-ci-{profile}-amd64-{scope}"
    command = [
        "docker",
        "run",
        "--rm",
        "--pull=never",
        "--platform",
        "linux/amd64",
        *SECURITY_OPTIONS,
        "--mount",
        f"type=bind,source={snapshot},target=/checkout,readonly",
        "--mount",
        f"type=bind,source={history},target=/history,readonly",
        "--mount",
        f"type=volume,source={prefix}-registry,target=/usr/local/cargo/registry",
        "--mount",
        f"type=volume,source={prefix}-git,target=/usr/local/cargo/git",
        "--mount",
        f"type=volume,source={prefix}-target,target=/work/target",
        "--mount",
        f"type=volume,source={prefix}-tools,target=/ci-tools",
    ]
    for key, value in {
        "CI_TOOL_ROOT": "/ci-tools",
        "PATH": "/ci-tools/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        "PYTHONPATH": "/ci-tools/pythonlib",
        "TRANSIST_CI_PYTHONLIB": "/ci-tools/pythonlib",
        "CARGO_INCREMENTAL": "0",
        "CARGO_NET_RETRY": "10",
        "CARGO_TERM_COLOR": "always",
        "RUST_BACKTRACE": "short",
        "RUSTUP_AUTO_INSTALL": "0",
        "RUSTUP_TOOLCHAIN": "1.88.0",
        "GIT_NO_LAZY_FETCH": "1",
    }.items():
        command += ["--env", f"{key}={value}"]
    return command + [
        image,
        "sh",
        "-c",
        CONTAINER_ENTRY,
        "ci-local",
        profile,
        "1" if install else "0",
        base,
        head,
        "1" if probe_only else "0",
    ]


def write_history(root: Path, history: Path) -> None:
    """Bundle complete Git history and a byte-exact staged patch without copying host config."""
    paths = execute(
        [
            "git",
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
        ],
        root=root,
        capture=True,
        trim=False,
    )
    for path in paths.split("\0"):
        if path and excluded(Path(path)):
            raise ValueError(f"Cannot snapshot staged sensitive/generated path: {path}")
    execute(
        ["git", "bundle", "create", str(history / "history.bundle"), "--all", "HEAD"],
        root=root,
    )
    staged = execute(
        [
            "git",
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
        ],
        root=root,
        capture=True,
        trim=False,
    )
    (history / "index.patch").write_bytes(
        staged.encode("utf-8", errors="surrogateescape")
    )


def docker_inputs_parent(root: Path) -> Path:
    """Create a nonsymlink repo-local temp parent shared with Docker alongside the checkout."""
    checkout = root.resolve(strict=True)
    parent = checkout
    for name in (".tmp", "ci-inputs"):
        parent = parent / name
        if parent.is_symlink():
            raise ValueError(f"Docker input directory must not be a symlink: {parent}")
        parent.mkdir(mode=0o700, exist_ok=True)
        if not parent.is_dir() or not parent.resolve(strict=True).is_relative_to(
            checkout
        ):
            raise ValueError(
                f"Docker input directory must stay inside the checkout: {parent}"
            )
    return parent


def run_container(command: list[str], root: Path, inputs: Path) -> None:
    """Run Docker without daemon repair; explain bind-sharing failures with the exact input parent."""
    try:
        execute(command, root=root)
    except subprocess.CalledProcessError as error:
        if error.returncode == 125:
            print(
                f"Docker could not start the container. If a bind source is unavailable, "
                f"ensure Docker/Colima shares the checkout containing {inputs}. "
                "Inspect the original Docker diagnostic above.",
                file=sys.stderr,
                flush=True,
            )
        raise


def run_docker(
    root: Path,
    profile: str,
    base: str | None,
    install: bool,
    stages: Stages,
    *,
    probe_only: bool = False,
) -> None:
    """Validate local inputs and run one disposable container, never concurrent Cargo."""

    def refs() -> tuple[str, str]:
        """Resolve local history before any bootstrap/download/container operations."""
        require_tools(["docker", "git"])
        if profile == "debian" and (not probe_only or base):
            return resolve_refs(root, base)
        if (
            execute(
                ["git", "rev-parse", "--is-shallow-repository"], root=root, capture=True
            )
            != "false"
        ):
            raise ValueError("Docker profiles require complete Git history")
        return "", execute(
            ["git", "rev-parse", "--verify", "HEAD^{commit}"], root=root, capture=True
        )

    resolved, head = stages.run("base refs", refs)
    parent = stages.run("Docker input directory", lambda: docker_inputs_parent(root))
    # Colima commonly shares the checkout but not the host's /tmp. An explicit
    # repo-local parent also prevents TMPDIR from redirecting Docker bind inputs.
    with tempfile.TemporaryDirectory(
        prefix="transist-ci-inputs-", dir=parent
    ) as directory:
        inputs = Path(directory)
        snapshot, history = inputs / "checkout", inputs / "history"
        snapshot.mkdir()
        history.mkdir()
        # Do this before ANY npm/bootstrap call. Only these filtered bytes and
        # a sanitized history payload will ever become Docker bind mounts.
        stages.run("filtered host snapshot", lambda: copy_checkout(root, snapshot))
        stages.run("Git history snapshot", lambda: write_history(root, history))
        tools = root / "scripts/ci/redocly"
        if install:
            tools = inputs / "host-yaml-tools"
            tools.mkdir()
            for name in ("package.json", "package-lock.json"):
                shutil.copy2(snapshot / "scripts/ci/redocly" / name, tools / name)
            stages.run("Node prerequisite", lambda: node_prerequisite(snapshot))
            stages.run(
                "YAML tools bootstrap",
                lambda: execute(
                    [
                        "npm",
                        "ci",
                        "--ignore-scripts",
                        "--no-audit",
                        "--prefix",
                        str(tools),
                    ],
                    root=snapshot,
                ),
            )

        def prerequisites() -> str:
            """Parse snapshot workflows on the host and require a working Docker daemon."""
            selected = select_image(load_workflows(snapshot, tools=tools), profile)
            execute(["docker", "info"], root=root, capture=True)
            return selected

        image = stages.run("Docker prerequisites", prerequisites)
        if install:
            stages.run(
                "pull pinned image",
                lambda: execute(
                    ["docker", "pull", "--platform", "linux/amd64", image], root=root
                ),
            )
        else:
            stages.run(
                "pinned image present",
                lambda: execute(
                    ["docker", "image", "inspect", image], root=root, capture=True
                ),
            )
        print(f"Docker profile {profile}: {image}", flush=True)
        stages.run(
            profile,
            lambda: run_container(
                docker_command(
                    snapshot,
                    history,
                    profile,
                    image,
                    resolved,
                    head,
                    install,
                    scope=cache_scope(root, snapshot, profile, image),
                    probe_only=probe_only,
                ),
                root,
                parent,
            ),
        )


def main(argv: list[str] | None = None) -> int:
    """Parse the public CLI, run one serial profile, and always summarize outcomes."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--profile",
        required=True,
        choices=("pr", "debian", "alpine"),
        help="pr: native full PR + coverage; debian: amd64 PR + Linux smoke; alpine: amd64 musl smoke",
    )
    parser.add_argument(
        "--base",
        help=(
            "existing local Git ref; required for full pr/debian profiles, not Docker probes (no fetch)"
        ),
    )
    parser.add_argument(
        "--bootstrap",
        action="store_true",
        help=(
            "explicitly install pinned/locked tools; Docker system tools are disposable, "
            "so fresh base containers generally need this flag on every run"
        ),
    )
    parser.add_argument(
        "--probe-only",
        action="store_true",
        help=(
            "Docker profiles only: check isolation and native Rust host without running the suite"
        ),
    )
    args = parser.parse_args(argv)
    if args.profile == "pr" and args.probe_only:
        parser.error("--probe-only is only supported by Docker profiles")
    if args.profile != "alpine" and not args.base and not args.probe_only:
        parser.error("--base REF is required for pr and debian")
    if args.profile == "alpine" and args.base:
        parser.error("--base is not used by alpine")
    stages = Stages()
    try:
        if args.profile == "pr":
            run_native(ROOT, args.base, stages, install=args.bootstrap)
        else:
            run_docker(
                ROOT,
                args.profile,
                args.base,
                args.bootstrap,
                stages,
                probe_only=args.probe_only,
            )
        return 0
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        print(f"FAIL: {error}", file=sys.stderr, flush=True)
        return 1
    finally:
        stages.summary()


if __name__ == "__main__":
    sys.exit(main())
