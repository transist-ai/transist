# Contributing to Transist

Transist is a governed local execution runtime for explicit AI and automation
workflows running on devices, machines, and edge infrastructure. Most changes
extend workflows, plugins, adapters, documentation, or examples and do not
touch the protected kernel. This guide explains how to participate and how
changes enter the official upstream project.

## Curated Upstream Development Model

Transist is publicly available under [MPL-2.0](LICENSE) and uses a
**curated upstream development model**. Anyone may use, inspect, fork, and
modify Transist under the license. Only trusted contributors and maintainers
may open implementation pull requests in the official upstream repository,
and only maintainers may merge them.

- **Public participation is welcome.** Bug reports, reproducible test cases,
  feature proposals, architecture and design discussion, documentation
  feedback, compatibility reports, and plugin or integration proposals belong
  in [Issues](https://github.com/transist-ai/transist/issues).
- **Security vulnerabilities** use the private reporting path in
  [SECURITY.md](SECURITY.md) — never a public issue.
- **Unsolicited external pull requests are not accepted.** If you are not a
  trusted contributor, use an Issue first. `pr-guard.yml`
  automatically closes pull requests from accounts without repository
  collaborator access, except verified same-repository Dependabot updates.
  That exception permits review only; CI and maintainer merge requirements remain.
- **Discuss substantial changes before implementing them.** Open an issue
  first; implementation should normally follow architectural
  agreement rather than precede it.
- **You remain free to fork and modify Transist** under the terms of its
  license, and to publish compatible plugins or integrations.
- **Maintainers may implement accepted proposals or invite participants to
  become trusted contributors.** See
  [MAINTAINERS.md](MAINTAINERS.md) and
  [CI and Contribution Governance](docs/developer/ci-and-contribution-governance.md).

The project is open for use and public design input; official upstream write
and merge access is deliberately maintainer-controlled. Open source does not
require the project to accept every implementation pull request.

## Roles

These project roles align with the maintainer authority defined in
[MAINTAINERS.md](MAINTAINERS.md):

### External participant

May open Issues, participate in design discussion, report bugs or security issues,
propose designs, maintain external workflows/plugins/integrations, and fork the
project under the license. This role has no upstream pull-request or merge
rights by default.

### Trusted contributor

An invited collaborator who may open upstream implementation pull requests in
approved areas and is responsible for the required local validation and DCO
sign-off. Trusted-contributor status does not grant merge, release, governance,
or licensing authority.

### Maintainer

A person appointed under [MAINTAINERS.md](MAINTAINERS.md) who may approve
architecture-sensitive changes and merge within their defined area.
Maintainers collectively control project governance. Only maintainers with
explicit release authority may approve and publish releases. Changes to
licensing claims, compatibility policy, and trust-boundary contracts require
maintainer approval.

## Why Core Development Is Restricted

Transist is execution and security infrastructure. Changes to workflow
semantics, package verification, capability enforcement, trust policy,
persistence, recovery, and execution evidence can change the boundary between
automation and a host or device.

The project is at a stage where architectural consistency matters more than
maximizing contribution volume. Restricting official implementation work keeps
that boundary reviewable; it is not a judgment about the safety or ability of
external participants.

## First Contribution Path

Start with the smallest useful contribution and use the matching public
channel. Redact secrets, tokens, customer data, private paths, and run artifacts
from every report.

1. **Documentation report:** open an issue describing the incorrect page,
   expected wording, and a proposed correction.
2. **Reproduction:** open an issue with the exact command, pinned platform,
   minimal input, and complete redacted error output.
3. **Fixture:** attach a minimal committed-style fixture to the issue
   when it demonstrates a parser, policy, storage, or workflow case.
4. **Workflow:** propose a `workflows/` or `examples/` change in an issue,
   including inputs, expected terminal state, and hermetic coverage.
5. **External plugin:** publish a versioned `transist.plugin.v1` package in your
   own repository and link it from an issue; do not add provider logic to
   the kernel.

A maintainer may carry an accepted proposal forward or invite its author to
become a trusted contributor. External participants should not open an
implementation pull request unless invited.

## License and Sign-Off

Contributions are accepted under the Developer Certificate of Origin (DCO).
By submitting, you certify that you wrote the contribution or have the right to
submit it under the DCO, and you agree it may be distributed under the project
license, [MPL-2.0](LICENSE). The full DCO text is published at
<https://developercertificate.org/>.

Every commit must include a `Signed-off-by:` line:

```bash
git commit -s
```

Trusted-contributor or maintainer status does not replace DCO sign-off or the
project's licensing requirements.

## Where Contributions Fit

Under the curated upstream model, the primary public contribution path is
through **Issues**. Trusted contributors may open pull
requests in approved areas. A maintainer may carry an accepted proposal
forward or invite a participant to the trusted-contributor group.

Welcomed contribution surfaces, based on what exists in this repository:

- **Workflows and examples** under `workflows/` and `examples/`
- **Plugins** under `plugins/` and in external `transist.plugin.v1` packages
- **Adapters** in `crates/transist-adapters` (process, HTTP, MCP)
- **Host capability integrations** through the relevant extension surfaces,
  and operator tooling in `crates/transist-cli` and `crates/transist-server`
- **Local inspection and metrics** in `crates/transist-inspection` and
  `crates/transist-metrics`
- **Documentation** under `docs/`, plus `README.md`, `ARCHITECTURE.md`, and
  guides
- **Platform support** and reliability hardening
- **Tests** under `tests/` and the e2e suites under
  `.agents/testing/e2e_suites/`
- **Security hardening** that preserves the trust boundary in `SECURITY.md`

External workflows, plugins, adapters, integrations, examples, and tooling can
be developed and published in independent repositories or packages. They do
not need to be accepted into this monorepo to participate in the extension
ecosystem.

Kernel and public-contract changes are possible but require additional
justification because they affect execution semantics and compatibility. See
the next section.

## Required Reading

### For every contribution

- [README.md](README.md)
- [VISION.md](VISION.md)
- Relevant area documentation under `docs/`
- [SECURITY.md](SECURITY.md) when handling security-sensitive work

### For kernel, trust-boundary, or public-contract changes

Additionally required:

- [AGENTS.md](AGENTS.md)
- [.agents/CORE_CHANGE_CHECKLIST.md](.agents/CORE_CHANGE_CHECKLIST.md)
- [Kernel Contract](docs/developer/architecture/kernel-contract.md)
- [Compatibility and Stability](docs/public/reference/compatibility-and-stability.md)
- [Threat Model](docs/public/security/threat-model.md) when the change touches
  trust, secrets, or local-only behavior

## AI Coding Agents

Automated coding tools and their operators must follow the same contribution
rules as any other contributor. When an AI coding agent changes protected core
surfaces, it must read [AGENTS.md](AGENTS.md) and
[.agents/CORE_CHANGE_CHECKLIST.md](.agents/CORE_CHANGE_CHECKLIST.md) before
proposing the change. Repo-local skills under `.agents/skills/` track the
current checked-out behavior and should be preferred over general knowledge.

## Working Rules

- Keep changes scoped to one topic.
- Update relevant documentation in the same change when behavior or contracts
  move.
- Do not introduce kernel changes without explicit rationale and evidence; use
  the core-change checklist.
- Keep the trust boundary intact: `transist` may verify and enforce trust, but
  key generation and signing belong in `transist-admin`.
- Do not change support, stability, compatibility, roadmap, or licensing claims
  without maintainer approval.
- Do not add provider-specific, tool-specific, or product-specific behavior to
  the kernel; push it to a workflow, plugin, adapter, or host capability
  integration.

## Local Setup

This repository is self-contained and self-sufficient for source development,
testing and release packaging. Workflows, plugins, skills and fixtures needed by
CI/CD are committed here; no sibling catalog checkout is required. Keep new
validation and release inputs repository-local. Standard toolchain, package
registry and pinned CI-tool downloads remain prerequisites; this is not a fully
vendored offline build.

`python3 scripts/ci/check_repository_self_contained.py` rejects external catalog
references while preserving the internal plugin crate. It runs in the
maintainability gate and release-candidate preflight.

Rust `1.88.0` is the repository build pin and minimum supported Rust version
(MSRV). It remains authoritative until an explicit policy change; see
[Compatibility & Stability](docs/public/reference/compatibility-and-stability.md)
and the [Dependency Update Policy](docs/public/reference/dependency-update-policy.md)
for enforcement. The checked-in `rust-toolchain.toml` selects this exact
toolchain for local builds and CI.

The maintainability gate uses Python scripts that require **Python 3.11+**
(the `tomllib` standard-library module). CI images and `install_debian_tools.sh`
install `python3` from a recent Debian package; ensure the same or newer is on
`PATH` locally (`python3 --version`).

For a clean supported source-development machine, install Git, Rustup, and the
host tools used by the validation suites. Linux contributors also need
`bubblewrap`; macOS contributors need Apple Silicon. On Debian or Ubuntu:

```bash
sudo apt-get update
sudo apt-get install -y build-essential bubblewrap curl git jq nodejs npm python3 python3-pip ripgrep
```

Install the pinned Rust toolchain and coverage component:

```bash
rustup toolchain install 1.88.0 --profile minimal \
  --component rustfmt --component clippy --component llvm-tools-preview
rustup default 1.88.0
cargo +1.88.0 install cargo-llvm-cov --version 0.9.0 --locked
```

The package names above are examples for Debian/Ubuntu. Fedora, Arch, Alpine,
and macOS contributors should install the equivalent `git`, `curl`, `jq`,
Python 3.11+, Node.js/npm, `ripgrep`, and (on Linux) `bubblewrap` packages.

Install the repository-pinned CI executables into an isolated directory, then
put that directory on `PATH` for subsequent commands. Use this exact command
and setup rather than globally installing unpinned CI tools:

```bash
export CI_TOOL_ROOT="$HOME/.local/share/transist-ci-tools"
bash scripts/ci/install_ci_tools.sh all
export PATH="$CI_TOOL_ROOT/bin:$PATH"
```

The installer adds its tool directory to `PATH` only while it runs, so the
final export is required in a local shell. In GitHub Actions it also appends
the directory to `GITHUB_PATH`, which makes it available to later steps.

The standard first-run path is the
[Quickstart Guide](docs/public/guides/getting-started/quickstart.md).

```bash
cargo +1.88.0 run -p transist-cli -- init --mode standalone
cargo +1.88.0 run -p transist-cli -- plugins install --path plugins/shell/plugin.yaml
```

## Validation

Trusted contributors must run the applicable local validation after the final
code change and **before merging to `main`**. The hosted `PR Gate / pr-gate`
runs the full Linux PR-readiness gate plus schema/OpenAPI and cargo-deny policy
checks, but it does not replace local validation while iterating or release
readiness validation before tagging.

For intermediate local development-plan steps, run only the focused tests and
validation listed by that step. Record their results; do not interpret them as
PR readiness. Run the complete applicable gates once after integration, before
requesting final review or merging. CI, signing, and release changes require the
full release-readiness gate at that final boundary, not after every local step.

Run the complete local PR profile (replace `origin/main` with the intended local
base ref; the runner never fetches):

```bash
bash scripts/ci/check_ci_local.sh --profile pr --base origin/main
```

This adds CI syntax/contracts, DCO, Markdown lint, dependency policy,
schema/OpenAPI checks, and coverage to the existing PR-readiness gate. First-time
tool installation is explicit: add `--bootstrap`. See
[Local CI Preflight](docs/developer/local-ci-preflight.md) for pinned Docker
Debian/Alpine profiles, prerequisites, cache cleanup, and hosted-only limitations.

Run the core full PR-readiness gate separately when debugging:

```bash
bash scripts/ci/check_pr_ready.sh
```

The canonical aggregate gate for full release readiness is:

```bash
bash scripts/ci/check_release_readiness.sh
```

Coverage is an evidence artifact, not a threshold gate. Generate the same HTML,
LCOV, and textual reports used by PR and mainline CI with:

```bash
bash scripts/ci/run_coverage.sh coverage
```

The reports are written to `coverage/html/`, `coverage/lcov.info`, and
`coverage/summary.txt`. CI uploads the directory for every ready PR and every
main build. See [Benchmarks](docs/developer/benchmarks.md) for the deterministic
TUI benchmark and its interpretation.

`check_pr_ready.sh` runs the core local checks (format, clippy,
maintainability, workspace tests, release build, archive reproducibility,
quickstart smoke, and E2E suites). `check_release_readiness.sh` runs every
check in `check_pr_ready.sh` plus supply-chain pins, legal notices,
full-git-history secret scanning, schema/OpenAPI contracts, release archive
inventory, admin sign/verify conformance, and release invariants. Run
`check_release_readiness.sh` before tagging a release.

`check_pr_ready.sh` supports flags for faster inner-loop iteration. The
default (no flags) is the full local gate and is what must pass before merging
to `main`:

- `--fast` runs only workspace tests, format, clippy, rustdoc, and maintainability
  checks (in that order), skipping the release build, archive reproducibility,
  quickstart smoke, and E2E suites. Use it to iterate quickly on code changes;
  it is not a substitute for the full gate before merging to `main`.
- `--skip-archive`, `--skip-smoke`, and `--skip-e2e` skip individual
  release-tail stages for targeted debugging.
- `--parallel` runs the pure-shell maintainability checks concurrently with
  the debug compile phase. Cargo build stages are not run concurrently
  (cargo holds a lock on the target directory). Output interleaves.
- `--fail-fast` stops the gate at the first failing stage. cargo test already
  stops at the first failing test binary by default, so no extra flag is
  forwarded to the test runner. This is the default in local runs.
- `--no-fail-fast` runs every scheduled stage (and passes `--no-fail-fast` to
  `cargo test` so every test runs) instead of stopping at the first
  failure, then reports all failed stages and exits non-zero. Use it to
  harvest the full failure list in one pass. This is the default in CI
  environments (auto-detected, e.g. GitHub Actions/GitLab CI): no-fail-fast in
  CI ensures one stage's failure cannot hide unrelated failures from the same
  run. Locally the default is fail-fast. Either flag overrides the
  environment-derived default.
- `--help` prints the full flag usage. Any reduced-mode run (using `--fast` or
  a `--skip-*` flag) prints a reminder that the full gate (no flags) is still
  required before merging to `main`.

CI is split into three cost/trust tiers:

- **PR CI (`PR Gate / pr-gate`)** runs the full `check_pr_ready.sh` gate on a
  Linux glibc runner, plus schema/OpenAPI contract checks and full
  `cargo deny check`, Markdown lint, and coverage, for ready trusted-contributor
  PRs. Cheap static, sign-off, Markdown, dependency-policy and optional dependency
  review checks must succeed before full validation, schema tests, and coverage
  start. There is no additional hosted `validate-fast` job; local `--fast` remains.
  Draft PRs skip validation. A fork or non-collaborator PR may queue read-only
  validation before `pr-guard.yml` closes it, or on a later `synchronize`
  event if it remains open. These jobs receive no repository secrets; repository
  settings should require maintainer approval for all external contributors.
- **Mainline CI (`Mainline Validation / main-gate`)** runs, per push to
  `main`, a cross-platform release smoke: a fat-LTO release build and the
  quickstart smoke suites on Linux x86-64/glibc, macOS Apple Silicon, and
  Alpine/musl, plus a focused musl runtime-temp regression and required
  coverage-instrumented default-feature tests. Its smoke contract covers that
  three-host subset. Release staging defines native archive checks for all six
  release targets; workspace tests run in the Linux PR gate, mainline coverage
  job, and local default- and all-feature release-readiness gates, not anew in
  hosted Release CD.
- **Target naming:** release artifacts use Rust target triples. A GitHub runner
  or pinned Debian/Alpine container is validation evidence, not an
  Ubuntu-, Debian-, or Alpine-specific support claim. Candidate target checks
  stay manual and non-blocking until a target is formally promoted.
- **Security Scan (`Security Scan / security-gate`)** requires the security audit
  (regressions, cargo-deny, npm audit, and full-history secret scanning) on a weekly
  schedule and on demand. CodeQL is public-only: the gate requires success for a
  public repository and exactly `skipped` for a private repository. Private
  success is audit-only evidence. A successful exact-SHA gate is required before
  release staging; see [Security Policy](SECURITY.md#ci-security-scanning).

Release staging requires the exact tagged SHA to pass `Mainline Validation /
main-gate` and `Security Scan / security-gate`. Release preflight verifies
existing check results; Release CD does not run a fresh `release-security` job
or repeat the local aggregate readiness suite. Obtain the exact-SHA security
result before staging. Release CD must be dispatched from
the exact release tag ref (`github.ref == refs/tags/<tag>`); the workflow
rejects any other ref so every check attaches to the tag SHA. Sandbox-dependent
tests must fail when the expected platform backend is unavailable; do not skip
them to make a platform green.

All six archives are centrally packaged; ARM64 musl uses native Bookworm with
`musl-tools` for build and archive smoke. See
[Supported Platforms](docs/public/reference/supported-platforms.md) and
[Release Environment Configuration](docs/developer/release-environment.md) for
the environment-gated `stage-and-sign`, `attest-public-assets`, and
`publish-release` jobs.

The hosted gates are not a substitute for local validation. The full local gates
below remain the mandatory pre-merge (and pre-release) evidence for a
contribution.

Mandatory local validation by change type — run after the final code change and
before merging to `main`:

The full local gate runs formatting, clippy, maintainability checks, inspection
snapshot contract tests, and workspace tests. It also includes a boundary guard
that rejects runtime signing symbols such as `SigningKey`, private-key
encoding, and keygen helpers in `src/` and `crates/`.

- **Documentation-only changes**: run
  `bash scripts/ci/validate_docs_and_skills.sh`, which checks Markdown links,
  root guardrail patterns, and skill structure, plus
  `npx --yes markdownlint-cli2@0.19.0 --config .markdownlint-cli2.jsonc '**/*.md' '**/*.markdown'`.
  Run `check_pr_ready.sh` when
  the change affects examples, commands, public behavior, contracts, or
  executable workflow definitions. Report what you ran and any limitations.
- **Code changes**: run `bash scripts/ci/check_pr_ready.sh`.
- **Dependency, trust, secret, or local-only changes**: run
  `bash scripts/ci/check_pr_ready.sh` and
  `bash scripts/ci/run_security_regression_suite.sh` (below).
- **Kernel or public-contract changes**: run `bash scripts/ci/check_pr_ready.sh`,
  `bash scripts/ci/run_security_regression_suite.sh` (below), and the matching
  e2e suite under `.agents/testing/e2e_suites/` as required by the core-change
  checklist.
- **Release engineering changes**: run `bash scripts/ci/check_release_readiness.sh`
  (the canonical aggregate gate).

Record every command you ran, its result, and any skipped check with a
justification in the pull request. Merging to `main` while an applicable command
failed, or was not run, is not allowed unless a maintainer records explicit
approval and rationale in the pull request.

When a change updates dependencies, release workflow actions, or anything that
touches trust, local-only, or secret-scoping behavior, also run:

```bash
bash scripts/ci/run_security_regression_suite.sh
```

Changes to workflow actions, CI tools, release executables, or third-party
notices must also pass the supply-chain and license checks:

```bash
bash scripts/ci/check_supply_chain_pins.sh
bash scripts/ci/check_licenses.sh
```

This suite is the focused governed-runtime regression gate. It covers trust
verification and trust-store lifecycle, local-only and policy enforcement,
plugin package hardening, schema contract drift, secret scoping and redaction,
and snapshot export. CI runs it under the Rust `1.88.0` toolchain.

## E2E Test Suites

The hermetic E2E suites live under `.agents/testing/e2e_suites/`. The eleven
required production-feature suites each have a committed `run.sh` that resolves
binaries from `target/debug/` and uses repository-local inputs. No sibling
checkout or external service is required.

Required production-feature suites:

- `plugin-lifecycle`, `resume-replay`, `manual-gates`, `secrets-and-policy`
- `run-cleanup`, `snapshot-export`, `filesystem-policy`, `detached-assets`
- `run-cancellation`, `workflow-authorization`, `managed-documents`

Run the harness unit tests and all required suites:

```bash
python3 .agents/testing/test_run_e2e.py
python3 .agents/testing/run_e2e.py --ci
```

Run a single suite:

```bash
python3 .agents/testing/run_e2e.py --suite filesystem-policy
```

Machine-readable JSON output:

```bash
python3 .agents/testing/run_e2e.py --json
```

The eleven required suites run locally through `check_pr_ready.sh` (full gate)
and are part of `check_release_readiness.sh`; they are not executed on every
`main` push. The mainline workflow runs cross-platform release build and
quickstart smoke, coverage, and the focused musl runtime-temp regression.
In `--ci` mode, any SKIP, WARN, TIMEOUT, or leaked process
causes failure.

## Shipped Workflow Coverage

Every workflow shipped under `workflows/` must have hermetic execution
coverage. The driver at `scripts/ci/run_shipped_workflows.py` discovers every
`transist.flow.v1` file under `workflows/` by compile-based schema detection
(both `.yaml` and `.yml`, plain or quoted schema) and requires each to be
declared with a plan, fixture inputs, and an expected terminal result that the
plan asserts. A new workflow with a new workflow file but no coverage fails CI,
so a workflow cannot ship silently. The driver uses only committed fixtures and
locally created temp homes and workspaces — no sibling repository, network
service, host file (e.g. `/etc/gitconfig`), or mutable external fixture.
Release-archive extraction validates every archive member, so no archive can
write outside its extraction root.

Run coverage from a source checkout:

```bash
bash scripts/ci/run_shipped_workflows.sh
```

The same inventory also runs against an extracted release archive:

```bash
bash scripts/ci/run_shipped_workflows.sh --archive transist-<tag>-<target>.tar.gz
```

Add a new shipped workflow by declaring it in the `WORKFLOWS` list in
`scripts/ci/run_shipped_workflows.py` with a plan that reaches its declared
terminal result. If a workflow requires an inaccessible repository, a mutable
service, or an undocumented dependency, remove it from distribution instead of
adding coverage exceptions.

## Pull Requests

Pull requests to the official repository are **limited to trusted
contributors and maintainers** (see the
[Curated Upstream Development Model](#curated-upstream-development-model)
above). If you have not been invited and granted collaborator access, start
with an Issue rather than a pull request.

Trusted contributors and maintainers opening a pull request should:

- Use the repository pull request template at
  `.github/pull_request_template.md`.
- Call out compatibility and product-surface impact.
- Prefer small, reviewable changes.
- Update [Compatibility and Stability](docs/public/reference/compatibility-and-stability.md)
  when a change alters a documented public contract.
- Update `CHANGELOG.md` for operator-visible behavior changes.

## License

Transist is licensed under [MPL-2.0](LICENSE). See
[MAINTAINERS.md](MAINTAINERS.md) for maintainer governance and the
[Code of Conduct](CODE_OF_CONDUCT.md) for community standards.
