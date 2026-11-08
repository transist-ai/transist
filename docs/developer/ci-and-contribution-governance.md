# CI and Contribution Governance

This document is the contributor-facing companion to
[CONTRIBUTING.md](../../CONTRIBUTING.md). It describes the CI tiers, coverage
mapping, validation commands, test categorization, and the PR guard security
design. It is written for external participants, trusted contributors, and
maintainers.

## How External Proposals Are Evaluated

External participation happens through public **Issues**
(bug reports, reproducible test cases, feature proposals, architectural
discussion, security reports via the private channel). Maintainers evaluate a
proposal on:

- alignment with [VISION.md](../../VISION.md) and the kernel contract;
- whether it belongs in the kernel vs. an extension (the
  [Extension-First Rule](../../AGENTS.md));
- security and trust-boundary implications;
- long-term maintenance and compatibility cost.

If a proposal is accepted, a maintainer or trusted contributor implements it
and opens a pull request through the normal, validated flow.

## Changes That Require Architectural Discussion

Large architectural work should be discussed before implementation. Examples
include changes affecting:

- workflow execution semantics;
- persistence;
- plugin loading;
- plugin isolation;
- trust keys;
- artifact signing;
- signature verification;
- workflow distribution;
- remote control;
- runtime APIs;
- sandboxing;
- execution permissions;
- network access;
- compatibility guarantees.

Maintainers should be able to reject a proposal before implementation work
begins, reducing wasted contributor effort and review cost.

## CI Tiers And Their Cost / Security Boundaries

CI is split into several cost/trust tiers. Each tier has a clearly defined
purpose, and expensive or privileged validation runs only in trusted contexts.

| Tier | Workflow | Trigger | What runs | Runner / boundary |
|---|---|---|---|---|
| **1. PR CI** | `pr-ci.yml` | `pull_request` (opened, synchronize, reopened, ready_for_review) on `main`; validation jobs require ready-for-review state | CI static contracts, DCO, Markdown lint, cargo-deny and optional dependency review feed `preflight-gate`; only then run full `check_pr_ready.sh`, schema/OpenAPI and coverage | Linux glibc containers and a clean Linux runner, `contents: read`. Draft validation jobs are skipped. `validate-full` retains release build, archive reproducibility, quickstart smoke, E2E and shipped-workflow coverage. Duplicate hosted `validate-fast` is removed. |
| **2. Default-branch CI** | `main-ci.yml` | `push` to `main` + input-free `workflow_dispatch` | Linux x86-64/glibc + macOS Apple Silicon + Alpine/musl release build and quickstart smoke, focused musl runtime-temp regression, plus coverage-instrumented default-feature tests | Trusted `main` context, low merge volume. No full PR-readiness, clippy, E2E harness, static, or security gate. Coverage is required by `main-gate`. |
| **3. Release CI** | `release.yml` | `workflow_dispatch` (stage / publish), dispatched from the exact protected tag ref | Exact-SHA preflight requires existing successful `main-gate` and `security-gate` checks. Six-target native builds and archive smoke, central packaging, reproducibility, shipped-workflow coverage, checksums, signing, SBOM, verification, attestation, publication. No fresh `release-security` job. A guard requires `refs/tags/<release_tag>`. | Trusted, maintainer-triggered, isolated release credentials. |
| **Security Scan** | `security.yml` | weekly `schedule` + `workflow_dispatch` + reusable `workflow_call` | Always-required security audit: regressions, cargo-deny, npm audit, full-history secret scan. Public-only CodeQL. | Gate requires audit success and CodeQL success when public, exactly skipped when private (audit-only evidence). Trusted contexts only; no PR or per-push trigger. Release CD consumes an existing exact-SHA result and does not call this workflow. |
| **Coverage** | `pr-ci.yml` / `main-ci.yml` / local release readiness | ready PR + every push to `main` + local pre-tag gate | Pinned `cargo-llvm-cov`; HTML, LCOV, and text reports | Generation is required by `pr-gate`, `main-gate`, and the local canonical release gate; the percentage remains informational with no threshold. |
| **Markdown lint** | `pr-ci.yml` | ready PR | Locked `markdownlint` library gate using existing JSONC rules and file-selection semantics | Required by `pr-gate`; no runtime or release behavior. |
| **External links** | `link-check.yml` | ready PR + weekly `schedule` + manual | SHA-256-verified Lychee release asset over Markdown links | Informational. Network and link failures are reported and never block unrelated PR or mainline gates. |

### Coverage Mapping (mainline exclusions -> trusted home)

The ready-PR tier runs the full local PR-readiness gate. The mainline tier is
intentionally narrower and requires its separately generated coverage result.
Checks outside the mainline gate have these homes:

| Check outside the mainline gate | Trusted home |
|---|---|
| Unit tests, clippy, rustdoc, full release tail, E2E suites, shipped-workflow coverage | Ready PR: `pr-ci.yml` `validate-full`; local: `bash scripts/ci/check_pr_ready.sh` (full gate) before merging to `main` |
| Supply-chain pins, legal notices, full-history secrets, public-tree, schema contracts, release invariants | Local: `bash scripts/ci/check_release_readiness.sh` before tagging |
| Linux x86-64/glibc release build + quickstart smoke | `main-ci.yml` `platform-smoke` (Linux) per push to `main` |
| macOS Apple Silicon release build + quickstart smoke | `main-ci.yml` `platform-smoke` (macOS) per push to `main` |
| Alpine / musl release build + quickstart smoke | `main-ci.yml` `musl-smoke` per push to `main` |
| Admin sign/verify conformance | Local: `check_release_readiness.sh` (admin sign/verify conformance step) |
| Security regression suite | Local `check_release_readiness.sh` and `run_security_regression_suite.sh`; hosted `security.yml` |
| Public-only CodeQL / always-required security audit | `security.yml` (weekly cron or manual dispatch); private runs require skipped CodeQL, public runs require successful CodeQL; release preflight requires successful exact-SHA `security-gate` |

Mainline smoke covers three hosts; release staging defines all six supported
targets. ARM64 musl uses native Bookworm with `musl-tools` for build and archive
smoke, and one central pinned Linux job packages all six archives. See
[Supported Platforms](../public/reference/supported-platforms.md) and
[Release Environment Configuration](release-environment.md) for the three
environment-gated signing, attestation, and publication jobs.

### How To Run Full Validation Manually

- **Complete PR tier locally:** `bash scripts/ci/check_ci_local.sh --profile pr --base origin/main`.
  See [Local CI Preflight](local-ci-preflight.md) for explicit bootstrap and
  pinned Debian/Alpine parity. `check_pr_ready.sh --fast` remains an inner-loop aid.
- **Default-branch tier on demand:** trigger
  `main-ci.yml` via `workflow_dispatch` on `main`.
- **Security scan on demand:** trigger
  `security.yml` via `workflow_dispatch`.
- **Coverage locally:** install the pinned `cargo-llvm-cov` tool and run
  `bash scripts/ci/run_coverage.sh coverage`. Downloaded CI artifacts contain
  `html/`, `lcov.info`, and `summary.txt`; no coverage threshold is enforced.
  Coverage uses the default feature set, matching the readiness test command.
  The all-feature test surface is intentionally excluded from coverage
  instrumentation; focused `dev-version-bypass` behavior runs in the security
  regression gate, while all-feature workspace tests, clippy, and rustdoc
  remain release-readiness checks.
- **Benchmarks locally:** run `cargo +1.88.0 bench --bench tui_load_benchmark`.
  The fixture is generated in a temporary directory and is independent of user
  state. See [Benchmarks](benchmarks.md) for interpretation guidance.
- **Full release readiness:** `bash scripts/ci/check_release_readiness.sh` (covers
  the pinned compile/test/docs/security regression and coverage signals,
  candidate cleanliness and exact-SHA dry run, supply-chain pins, legal notices,
  full-history secret scanning, schema/OpenAPI contracts, release archive
  inventory, reproducibility, smoke, E2E, admin sign/verify conformance, SBOM,
  signature/provenance, and release invariants). It runs the workspace test
  suite under both the default and the all-feature feature sets on the local
  qualification host. Release CD consumes existing hosted gate results and
  runs native archive checks; it does not rerun this aggregate suite.
- **Security regression:** `bash scripts/ci/run_security_regression_suite.sh`
  (required in addition to `check_pr_ready.sh` for dependency, trust, secret,
  or local-only changes).

### Developer Gate: Validate Before Merging To `main`

Trusted contributors must run the applicable local validation after the final
code change and **before merging to `main`**. The hosted `PR Gate / pr-gate`
runs the full local gate plus hosted contracts, but it does not replace the
required contributor-run local validation.

Mandatory local commands by change type:

| Change type | Required local commands |
|---|---|
| Documentation-only | `bash scripts/ci/validate_docs_and_skills.sh` + `bash scripts/ci/check_markdown.sh` |
| Any code change | `bash scripts/ci/check_pr_ready.sh` |
| Dependency, trust, secret, or local-only | `bash scripts/ci/check_pr_ready.sh` + `bash scripts/ci/run_security_regression_suite.sh` |
| Kernel or public contract | the above plus the matching E2E suite under `.agents/testing/e2e_suites/` |
| Release engineering | `bash scripts/ci/check_release_readiness.sh` |

The contributor records every command, its result, and any skipped check with
justification in the pull request. A merge requires every applicable command to
have passed, unless a maintainer records explicit approval and rationale.

Intermediate local plan steps run only their specified focused tests/validation;
they do not rerun workspace or release suites and are not PR-ready evidence.
Run the complete applicable suites after final integration, before review/merge.
Keep step branches local until that boundary to avoid repeated hosted validation.

### How Hardware / Privileged / Release Tests Are Triggered

Hardware-dependent, privileged, and release validation runs only from trusted
repository-controlled events:

- `release.yml` `workflow_dispatch` (`stage` /
  `publish`) on a protected `v*` tag, guarded by protected tags and exact-SHA
  release-candidate checks.
- Persistent Transist-owned infrastructure (self-hosted runners, dedicated
  devices, machines with sensitive credentials) never executes arbitrary code
  from external pull requests. See the Trust Boundary section of
  [AGENTS.md](../../AGENTS.md).

## `pr-guard.yml` Security Review Note

`pr-guard.yml` is the enforcement mechanism for the trusted-contributor policy.
GitHub has no permanent native setting to disable non-collaborator PRs on a
public repository, so the workflow is the primary enforcement and native
settings are defense-in-depth.

The workflow uses `pull_request_target` (the narrowly-scoped, security-reviewed
exception of requirement §16.3) for one reason only: `pull_request` runs with a
read-only token and no PR-write permission, so it cannot close a PR or comment.
`pull_request_target` runs with repository-level privileges, so the guard **never
checks out or executes PR content** — it has no `actions/checkout` step and only
reads GitHub-provided PR metadata and verifies current author permissions when
the association is not trusted. Its permissions are limited
to `contents: read`, `pull-requests: write`, and `issues: write` (the minimum
needed to comment and close). All used actions are SHA-pinned and reviewed
(requirement §16.4).

Same-repository Dependabot updates are exempt from automatic closure only when
the PR author matches GitHub's account ID `49699333`, login `dependabot[bot]`,
and account type `Bot`, and the source repository ID matches the target ID.
Display names, commit authors, and the actor reopening a PR grant no exception.
This permits review only: CI, security checks, and maintainer merge policy
remain unchanged. The guard still executes no PR code.

> **Operational note:** the guard closes non-collaborator PRs on `opened`,
> `reopened`, and `ready_for_review`. It does not prevent GitHub from already
> queueing a `pull_request` job, and `pr-ci.yml` has no author-association
> filter. A fork PR can therefore run read-only validation before the guard
> closes it, or after a later `synchronize` event if it remains open. Fork jobs
> receive no repository secrets and use the read-only `contents` permission.
> Enable *Actions > General > Fork pull request workflows > Require approval for
> all external contributors* to require maintainer approval before those jobs
> run.

## Test Categorization

Tests are explicitly categorized so each category has a documented execution
policy covering when it runs, which runner it uses, whether external resources
are required, whether secrets are required, and its expected cost.

| Category | When it runs | Runner | External resources | Secrets | Cost / notes |
|---|---|---|---|---|---|
| **lint** | PR (`validate-fast` + `markdownlint`) + local full gate | Linux glibc | none | none | Cheap: `cargo fmt --check`, clippy `--all-features`, and tracked-Markdown lint. |
| **unit** | PR (`validate-full`) + local full gate | Linux glibc | none | none | `cargo test --workspace --all-targets`. |
| **integration-light** | PR (`validate-full`) + local full gate | Linux glibc | sandbox launcher (local) | none | Deterministic in-process/launcher integration. |
| **integration-full** | PR (`validate-full`) + local pre-merge / release readiness | Linux glibc + macOS | filesystem sandbox (bwrap / macOS profile) | none | E2E suites and shipped-workflow coverage; release staging runs shipped-workflow coverage from all six archives. Not run on every `main` push. |
| **cross-platform** | main smoke + release staging | Main: Linux x86-64 glibc/musl + macOS ARM64; release: all six target triples | none | none | Mainline runs release build + quickstart smoke on its three-host subset; release adds native archive smoke for all targets. ARM64 musl uses Bookworm/musl-tools. Archive reproducibility is local/release. |
| **hardware** | release / maintainer-triggered only | protected refs; never untrusted | Transist Station / hardware | none in CI | Not run on PR or ordinary mainline. |
| **external-service** | release / maintainer-triggered | protected refs | live external services | yes, isolated | Paid/live APIs excluded from PR and default-branch CI. |
| **benchmark** | maintainer-triggered | protected refs | none | none | Long-running; not run per PR. |
| **release** | release CI | protected `v*` tag | none | release signing, isolated | Build matrix, packaging, signing, SBOM, attestation. |
