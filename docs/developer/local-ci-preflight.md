# Local CI Preflight

Validate locally before publishing a ready PR. All profiles fail on missing tools
or unavailable isolation; none silently substitutes another platform. Intermediate
plan steps use their focused tests only; final integration uses the full profiles.

## Prerequisites

- Complete Git history and an existing base ref, such as `origin/main`.
- Native macOS Apple Silicon: Bash, Git, Python 3.11+, Rustup, curl and unzip.
- Docker profiles: a running Docker daemon supporting `linux/amd64`, namespaces
  and the workflow's capabilities/security options; Node/npm for the YAML parser.
- Network access for explicit bootstrap, dependency downloads, and advisory data.
- Spare disk for debug, release, LLVM coverage, and separate GNU/musl build caches.
- A canonical, nonsymlink temporary directory outside the repository workspace.
  On macOS, `/tmp` is a symlink and is rejected by runtime no-follow writes;
  putting temporary homes inside the workspace also violates runtime policy.

Commands below use an isolated tool directory. Bootstrap installs the repository's
pinned tools, Node runtime and locked npm trees; without it checks install nothing.
Do not run these commands against untrusted source with access to valuable caches.

## Commands

```bash
export CI_TOOL_ROOT="$PWD/.tmp/ci-tools"
export PATH="$CI_TOOL_ROOT/bin:$PATH"
# Select an existing canonical temporary directory OUTSIDE this workspace.
export TMPDIR="/absolute/path/to/external-temporary-directory"

# Cheap checks; install the pinned actionlint explicitly once.
bash scripts/ci/install_ci_tools.sh static
bash scripts/ci/check_ci_static.sh

# Complete native PR checks, including full readiness and coverage.
bash scripts/ci/check_ci_local.sh --profile pr --base origin/main --bootstrap

# Bootstrap + isolation/architecture probes, without Cargo builds/tests.
bash scripts/ci/check_ci_local.sh --profile debian --bootstrap --probe-only
bash scripts/ci/check_ci_local.sh --profile alpine --bootstrap --probe-only

# Full GNU PR + mainline smoke, then musl mainline parity; run serially.
bash scripts/ci/check_ci_local.sh --profile debian --base origin/main --bootstrap
bash scripts/ci/check_ci_local.sh --profile alpine --bootstrap

# Final release engineering gate: clean committed candidate required.
bash scripts/ci/check_release_readiness.sh
```

`--bootstrap` also executes the selected checks. Omit it on a prepared native
host. Disposable Docker base images generally need it each run for OS packages;
tool and Cargo caches persist. No production keys or provider credentials are needed.

## Evidence and Boundaries

- Static, DCO, Markdown and dependency checks precede schema/full builds/coverage.
- DCO checks committed `BASE..HEAD`; source checks include dirty working bytes.
- Docker uses workflow digest pins, read-only filtered source, sanitized Git
  history, and separate cache volumes. It does not mount the real checkout,
  runtime homes, Docker socket, host credentials or host environment wholesale.
  Disposable bind inputs live under `.tmp/ci-inputs/` so Docker Desktop/Colima
  can use the same shared checkout mount; the directory must not be a symlink.
- Native coverage reports live in unique `target/ci-local/coverage-*` directories;
  instrumented build outputs have a separate reusable target. Capture console
  output outside the tracked tree and retain its exit status.
- Docker output/coverage remain in the profile's named target volume. Inspect
  volumes with `docker volume ls --filter name=transist-ci-`.
- Local success does not attest to GitHub dependency review, CodeQL uploads,
  permissions, branch protections, remote artifacts, or hosted runner kernels.
- Static contract tests execute the real security gate with simulated results:
  audit success is always required, with CodeQL success for public repositories
  and exactly `skipped` for private repositories. This checks policy wiring, not
  a CodeQL scan. Private hosted success is audit-only evidence; maintainers enable
  GitHub security features when making the repository public.
- Final qualification requires green PR, Mainline Validation and Security Scan
  gates on the same final commit. Do not stage/publish a release to test CI.

## Cache Policy and Cleanup

Hosted build caches separate PR/main and instrumented/noninstrumented outputs.
Pinned Cargo-tool caches contain an explicit binary allowlist, never all Cargo
launchers. Versions are checked on every invocation; cache hits do not skip tests.
Markdown uses a lockfile-keyed npm download cache and still performs `npm ci`.

Docker volume identities include repository, profile/ABI, image and tool inputs.
Only registry/Git downloads, isolated tools and build targets persist; image
Cargo/Rustup launchers remain authoritative. To reclaim space, stop validation,
list the `transist-ci-` volumes, then pass only the selected names to
`docker volume rm NAME ...`. Delete local `target/ci-local/` reports and the
chosen `CI_TOOL_ROOT` separately when no check is using them.

Docker emulation on Apple Silicon is slower than native Linux and may not support
the required isolation. A failed probe is a blocker, not permission to disable
the sandbox. No local profile replaces final hosted verification.
